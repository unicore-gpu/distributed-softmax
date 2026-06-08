#include <grpcpp/grpcpp.h>
#include "vector_service.grpc.pb.h"
#include <cmath>
#include <cstring>
#include <numeric>
#include <algorithm>
#include <iostream>
#include <vector>
#include <thread>
#include <chrono>
#include <mutex>
#include <map>
#include <csignal>
#include <nlohmann/json.hpp>
#include <sw/redis++/redis++.h>
#include "redis_manager.h"
#include "redis_config.h"
#include "cache_manager.h"
#include "slice_notifier.h"

#include "publisher_interface.h"
#include "nats_publisher.h"
#include "zmq_publisher.h"
#include "zmq_multi_publisher.h"
#ifdef USE_RABBIT
  #include "rabbit_publisher.h"
#endif

// Selected at startup via TRANSPORT env var: "zmq" | "zmq_nccl" | "nats" | "rabbit"
static std::unique_ptr<IPublisher> bus;
static std::string                 transport_mode;

using grpc::Server;
using grpc::ServerBuilder;
using grpc::ServerContext;
using grpc::Status;
using vector::TaskRequest;
using vector::TaskResponse;
using vector::ResultRequest;
using vector::ResultResponse;
using vector::VectorService;
using namespace sw::redis;

namespace {
// Configuration constants
// NUM_SLICES env var controls how many slices a vector is split into.
// Default: 4 (one per GPU for a 4-GPU machine).
// The slice size is then vector_size / NUM_SLICES (rounded up).
static size_t getNumSlices() {
    const char* e = std::getenv("NUM_SLICES");
    if (e) {
        int v = std::atoi(e);
        if (v > 0) return static_cast<size_t>(v);
    }
    return 4;
}
// Slice collection timeout: SLICE_TIMEOUT_MS env var (default 30 000 ms = 30 s).
// Single-machine ZMQ: slices arrive in <5 ms — 30 s is plenty.
// Multi-machine NATS via SSH tunnel: also fine at 30 s.
constexpr int POLL_INTERVAL_MS = 100;  // 100 ms polling granularity
static int getSliceTimeoutMs() {
    const char* e = std::getenv("SLICE_TIMEOUT_MS");
    return (e && std::atoi(e) > 0) ? std::atoi(e) : 30000;  // default 30 s
}
}

// Partial softmax statistics returned by each worker slice.
//
// Standard mode (0x01 magic):
//   Workers compute exp(x - local_max); gateway does two-pass global reduction.
//
// NCCL mode (0x02 magic):
//   Workers already performed AllReduce via NVLink; values are final probabilities.
//   Gateway just concatenates — no aggregation math needed.
struct SliceStats {
    std::vector<double> exp_values;  // exp(x_i - local_max), or final probs in NCCL mode
    double local_max     = 0.0;
    double partial_sum   = 1.0;
    bool   is_normalized = false;    // true → NCCL mode, values are final probabilities
};

class ResultAggregator {
private:
    RedisManager redis_manager_;

public:
    ResultAggregator() = default;

    std::string aggregateResults(const std::string& job_id, const std::string& task_type,
                                 size_t total_slices, const std::vector<double>& /*original_vector*/) {
        std::vector<SliceStats> stats;
        if (!waitForAllSlices(job_id, total_slices, stats)) {
            return "";
        }

        std::vector<double> final_result = aggregateSoftmax(stats);

        nlohmann::json result_json = final_result;
        std::string result_str = result_json.dump();

        try {
            redis_manager_.storeFinalResult(job_id, result_str);
            std::cout << "📊 Aggregated result stored for " << job_id << std::endl;
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to store final result: " << e.what() << std::endl;
            return "";
        }

        return result_str;
    }

private:
    // Wait for all slices using the global SliceNotifier (pub/sub + condvar).
    // Slices already written to Redis before this call are pre-counted to
    // handle the subscribe-before-publish race condition.
    bool waitForAllSlices(const std::string& job_id, size_t total_slices,
                          std::vector<SliceStats>& stats) {
        stats.resize(total_slices);

        auto& notifier = SliceNotifier::instance();
        // Register before pre-scanning Redis so notifier counters start at 0.
        notifier.registerJob(job_id, total_slices);

        // Pre-read slices already in Redis (workers that finished before we
        // subscribed); bump the notifier counter for each so condvar math works.
        for (size_t i = 0; i < total_slices; ++i) {
            if (readSlice(job_id, i, stats[i])) {
                notifier.notifySlice(job_id);
            }
        }

        auto timeout = std::chrono::milliseconds(getSliceTimeoutMs());
        bool ok = notifier.waitForAllSlices(job_id, total_slices, timeout);
        notifier.unregisterJob(job_id);

        if (!ok) {
            std::cerr << "⏰ Timeout waiting for slices for job " << job_id << std::endl;
            return false;
        }

        // Read any slices that arrived purely via pub/sub (not pre-read above).
        for (size_t i = 0; i < total_slices; ++i) {
            if (stats[i].exp_values.empty()) {
                if (!readSlice(job_id, i, stats[i])) {
                    std::cerr << "❌ Failed to read slice " << i
                              << " for job " << job_id << std::endl;
                    return false;
                }
            }
        }

        std::cout << "✅ All " << total_slices << " slices ready for job " << job_id << std::endl;
        return true;
    }

    // Slice binary formats:
    //   0x01 — partial stats (standard two-pass, P2):
    //     [0x01][f64 local_max][f64 partial_sum][u32 n][f32×n exp_values]
    //   0x02 — pre-normalized (NCCL AllReduce mode):
    //     [0x02][u32 n][f32×n probabilities]  — gateway only concatenates
    //   Fallback: JSON {"local_max":…,"partial_sum":…,"exp_values":[…]}

private:
    bool readSlice(const std::string& job_id, size_t slice_index, SliceStats& out) {
        std::string key = RedisConfig::makeSliceKey(job_id, slice_index);
        try {
            auto raw = redis_manager_.get(key);
            if (!raw) return false;
            const std::string& s = *raw;
            if (s.empty()) return false;

            unsigned char magic = static_cast<unsigned char>(s[0]);

            // ── 0x01: partial stats (standard two-pass) ───────────────────────
            if (magic == 0x01) {
                static constexpr size_t HDR = 1 + 8 + 8 + 4;
                if (s.size() < HDR) return false;
                const char* p = s.data() + 1;
                double local_max, partial_sum; uint32_t n;
                std::memcpy(&local_max,   p,      8);
                std::memcpy(&partial_sum, p +  8, 8);
                std::memcpy(&n,           p + 16, 4);
                if (s.size() != HDR + static_cast<size_t>(n) * 4) return false;
                out.local_max    = local_max;
                out.partial_sum  = partial_sum;
                out.is_normalized = false;
                out.exp_values.resize(n);
                const float* fp = reinterpret_cast<const float*>(p + 20);
                for (uint32_t i = 0; i < n; ++i) out.exp_values[i] = fp[i];
                return true;
            }

            // ── 0x02: pre-normalized (NCCL AllReduce) ────────────────────────
            if (magic == 0x02) {
                static constexpr size_t HDR2 = 1 + 4;
                if (s.size() < HDR2) return false;
                const char* p = s.data() + 1;
                uint32_t n;
                std::memcpy(&n, p, 4);
                if (s.size() != HDR2 + static_cast<size_t>(n) * 4) return false;
                out.is_normalized = true;
                out.local_max    = 0.0;
                out.partial_sum  = 1.0;
                out.exp_values.resize(n);
                const float* fp = reinterpret_cast<const float*>(p + 4);
                for (uint32_t i = 0; i < n; ++i) out.exp_values[i] = fp[i];
                return true;
            }

            // ── JSON fallback (legacy workers / debug) ────────────────────────
            nlohmann::json j = nlohmann::json::parse(s);
            if (!j.is_object() || !j.contains("exp_values") ||
                !j.contains("local_max") || !j.contains("partial_sum"))
                return false;
            out.local_max    = j["local_max"].get<double>();
            out.partial_sum  = j["partial_sum"].get<double>();
            out.is_normalized = false;
            out.exp_values.clear();
            for (auto& v : j["exp_values"]) out.exp_values.push_back(v.get<double>());
            return true;

        } catch (const std::exception& e) {
            std::cerr << "❌ Error reading slice " << slice_index
                      << ": " << e.what() << std::endl;
            return false;
        }
    }

private:
    // Aggregate slice results into the final softmax probability vector.
    //
    // NCCL mode (is_normalized == true):
    //   Workers already performed AllReduce via NVLink — just concatenate.
    //
    // Standard two-pass mode:
    //   global_max  = max(local_max_i)
    //   adjust_i    = exp(local_max_i - global_max)
    //   global_sum  = sum_i(partial_sum_i * adjust_i)
    //   result[i,j] = exp_values[i][j] * adjust_i / global_sum
    std::vector<double> aggregateSoftmax(const std::vector<SliceStats>& stats) {
        if (stats.empty()) return {};

        // ── NCCL mode: workers already normalized, just concatenate ──────────
        if (stats[0].is_normalized) {
            std::vector<double> result;
            for (const auto& s : stats)
                for (double v : s.exp_values)
                    result.push_back(v);
            return result;
        }

        // ── Standard two-pass mode ────────────────────────────────────────────
        double global_max = -std::numeric_limits<double>::infinity();
        for (const auto& s : stats) global_max = std::max(global_max, s.local_max);

        double global_sum = 0.0;
        std::vector<double> adjust(stats.size());
        for (size_t i = 0; i < stats.size(); ++i) {
            adjust[i] = std::exp(stats[i].local_max - global_max);
            global_sum += stats[i].partial_sum * adjust[i];
        }

        std::vector<double> result;
        result.reserve([&] {
            size_t n = 0; for (const auto& s : stats) n += s.exp_values.size(); return n;
        }());
        for (size_t i = 0; i < stats.size(); ++i)
            for (double ev : stats[i].exp_values)
                result.push_back(ev * adjust[i] / global_sum);
        return result;
    }
};

class VectorServiceImpl final : public VectorService::Service {
private:
    ResultAggregator aggregator_;
    RedisManager& redis_manager_;
    
public:
    VectorServiceImpl(RedisManager& redis_manager) : redis_manager_(redis_manager) {}
    Status GetResult(ServerContext*, const ResultRequest* req, ResultResponse* res) override {
        std::string job_id = req->job_id();
        res->set_job_id(job_id);
        
        try {
            // Check if job metadata exists (indicates job was submitted)
            auto metadata = redis_manager_.getJobMetadata(job_id);
            if (!metadata) {
                res->set_status("not_found");
                res->set_message("Job not found or expired");
                return Status::OK;
            }
            
            size_t total_slices = metadata->total_slices;
            std::string task_type = metadata->task_type;
            
            // Check for final aggregated result first
            auto final_result = redis_manager_.get(RedisConfig::makeResultKey(job_id));
            
            if (final_result) {
                // Final result is ready
                res->set_status("ready");
                res->set_completed_slices(total_slices);
                res->set_total_slices(total_slices);
                
                // Get TTL information
                int ttl = redis_manager_.getTTL(RedisConfig::makeResultKey(job_id));
                if (ttl > 0) {
                    res->set_message("Result ready (expires in " + std::to_string(ttl) + " seconds)");
                } else {
                    res->set_message("Result ready");
                }
                
                try {
                    nlohmann::json result_json = nlohmann::json::parse(*final_result);
                    for (auto& val : result_json) {
                        res->add_result(static_cast<float>(val.get<double>()));
                    }
                } catch (const std::exception& e) {
                    res->set_status("failed");
                    res->set_message("Failed to parse result: " + std::string(e.what()));
                }
                
                return Status::OK;
            }
            
            // Check slice completion progress
            int completed_slices = redis_manager_.getCompletedSliceCount(job_id, total_slices);
            
            res->set_completed_slices(completed_slices);
            res->set_total_slices(total_slices);
            
            if (completed_slices == 0) {
                res->set_status("pending");
                res->set_message("Job submitted, waiting for processing to start");
            } else if (completed_slices < static_cast<int>(total_slices)) {
                res->set_status("running");
                res->set_message("Processing in progress: " + std::to_string(completed_slices) + 
                               "/" + std::to_string(total_slices) + " slices completed");
            } else {
                res->set_status("running");
                res->set_message("All slices completed, aggregating results...");
            }
            
        } catch (const std::exception& e) {
            res->set_status("failed");
            res->set_message("Internal error: " + std::string(e.what()));
        }
        
        return Status::OK;
    }
    Status SubmitTask(ServerContext*, const TaskRequest* req, TaskResponse* res) override {
        std::vector<double> input_vector(req->vector().begin(), req->vector().end());
        
        // Validate task type (no immediate computation)
        if (req->task() != "softmax") {
            return Status(grpc::StatusCode::INVALID_ARGUMENT, "only softmax task supported");
        }
        
        // Async processing: publish slices for distributed worker processing
        size_t vector_size = req->vector_size();
        size_t num_slices  = getNumSlices();
        // Each slice gets ceil(vector_size / num_slices) elements; last slice may be smaller.
        size_t slice_size  = (vector_size + num_slices - 1) / num_slices;
        size_t total_slices = (vector_size + slice_size - 1) / slice_size;

        // Store job metadata in Redis with TTL
        JobMetadata metadata{
            req->job_id(),
            req->task(),
            total_slices,
            std::time(nullptr)
        };
        redis_manager_.setJobMetadata(req->job_id(), metadata);


        int slice_id = 0;
        // In zmq_nccl mode the subject is the rank string ("0","1",…) so
        // slice i is always delivered to GPU i — required for NCCL ordering.
        // All other transports ignore the subject (ZMQ round-robin / NATS subject).
        const bool nccl_dispatch = (transport_mode == "zmq_nccl");

        for (size_t offset = 0; offset < vector_size; offset += slice_size, ++slice_id) {
            nlohmann::json slice_message{
                {"job_id",  req->job_id()},
                {"slice_id", slice_id},
                {"task",    req->task()}
            };
            for (size_t idx = offset; idx < std::min(offset + slice_size, vector_size); ++idx)
                slice_message["data"].push_back(req->vector(idx));

            std::string subject = nccl_dispatch ? std::to_string(slice_id) : "task_queue";
            bus->publish(subject, slice_message.dump());
        }
        
        // P1: Synchronous aggregation — blocks this gRPC thread until all slices
        // are collected and the result is computed.  With the enlarged thread pool
        // (P0: ResourceQuota::SetMaxThreads) up to 256 requests can run in parallel.
        std::string result_str = aggregator_.aggregateResults(
            req->job_id(), req->task(), total_slices, input_vector);

        if (result_str.empty()) {
            return Status(grpc::StatusCode::INTERNAL,
                          "Aggregation failed or timed out for job " + req->job_id());
        }

        try {
            auto j = nlohmann::json::parse(result_str);
            for (auto& v : j)
                res->add_result(static_cast<float>(v.get<double>()));
            res->set_message("OK");
        } catch (const std::exception& e) {
            return Status(grpc::StatusCode::INTERNAL,
                          std::string("Failed to parse result: ") + e.what());
        }
        return Status::OK;
    }
};

int main() {
    const char* addr_env = std::getenv("GATEWAY_ADDR");
    std::string addr = addr_env ? addr_env : "0.0.0.0:50051";

    // Runtime transport selection — no recompile needed.
    //   TRANSPORT=zmq    → ZMQ PUSH/PULL (low-latency, same-machine or cross-machine TCP)
    //   TRANSPORT=nats   → NATS core publish (default, cross-machine friendly)
    //   TRANSPORT=rabbit → RabbitMQ (compile with -DUSE_RABBIT=ON)
    const char* transport_env = std::getenv("TRANSPORT");
    transport_mode = transport_env ? transport_env : "nats";
    const std::string& transport = transport_mode;

    if (transport == "zmq") {
        bus = std::make_unique<ZmqPublisher>();
    } else if (transport == "zmq_nccl") {
        // NCCL mode: one dedicated PUSH socket per GPU rank.
        // Slice i is always delivered to GPU i — required for NCCL AllReduce ordering.
        const char* ws_env = std::getenv("NCCL_WORLD_SIZE");
        int world_size = (ws_env && std::atoi(ws_env) > 0) ? std::atoi(ws_env) : 4;
        bus = std::make_unique<ZmqMultiPublisher>(world_size);
    }
#ifdef USE_RABBIT
    else if (transport == "rabbit") {
        bus = std::make_unique<RabbitPublisher>();
    }
#endif
    else {
        bus = std::make_unique<NatsPublisher>();
    }

    if (!bus->isConnected()) {
        std::cerr << "❌ Failed to connect transport '" << transport << "', exiting." << std::endl;
        return 1;
    }

    std::cout << "🚀 Gateway service starting..." << std::endl;
    std::cout << "📍 Address:   " << addr << std::endl;
    std::cout << "🚌 Transport: " << transport << std::endl;

    // Create shared Redis manager
    RedisManager redis_manager;
    
    // Create service with Redis manager
    VectorServiceImpl service(redis_manager);
    
    // Create and start cache manager
    CacheManager cache_manager(redis_manager);
    cache_manager.start();
    
    // P0: Enlarge gRPC server-side thread pool so many concurrent synchronous
    // RPCs (P1) can each hold a thread without starving each other.
    // GRPC_MAX_WORKERS env var allows runtime tuning (default: 256).
    const char* max_workers_env = std::getenv("GRPC_MAX_WORKERS");
    int max_workers = max_workers_env ? std::atoi(max_workers_env) : 256;

    grpc::ResourceQuota quota;
    quota.SetMaxThreads(max_workers);

    // Setup gRPC server
    ServerBuilder builder;
    builder.SetResourceQuota(quota);
    builder.AddChannelArgument(GRPC_ARG_MAX_CONCURRENT_STREAMS, max_workers);
    builder.AddListeningPort(addr, grpc::InsecureServerCredentials());
    builder.RegisterService(&service);
    std::unique_ptr<Server> server(builder.BuildAndStart());

    std::cout << "⚙️  gRPC thread pool: " << max_workers << " workers" << std::endl;
    
    std::cout << "🚀 Gateway listening on " << addr << " with Redis TTL management" << std::endl;
    std::cout << "⚙️  Configuration:" << std::endl;
    std::cout << "   Result TTL: " << RedisConfig::getResultTTL() << "s" << std::endl;
    std::cout << "   Slice TTL: " << RedisConfig::getSliceTTL() << "s" << std::endl;
    std::cout << "   Metadata TTL: " << RedisConfig::getMetadataTTL() << "s" << std::endl;
    
    
    // Install signal handler for graceful shutdown
    std::signal(SIGINT, [](int) {
        // Signal handler will be called from another thread
        static bool shutdown_requested = false;
        if (!shutdown_requested) {
            shutdown_requested = true;
            std::cout << "\n🛑 Shutdown requested..." << std::endl;
        }
    });
    
    server->Wait();
    
    // Cleanup
    cache_manager.stop();
    
    return 0;
}