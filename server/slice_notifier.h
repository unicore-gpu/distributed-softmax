#pragma once
#include <sw/redis++/redis++.h>
#include <atomic>
#include <chrono>
#include <condition_variable>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <unordered_map>
#include "redis_manager.h"

using namespace sw::redis;

// SliceNotifier is a process-wide singleton that maintains a single Redis
// pub/sub connection and fans out "slice done" notifications to whichever
// ResultAggregator thread is waiting for that job.
//
// This replaces the per-job busy-poll loop: instead of spinning on Redis GET
// every 100 ms per job (O(N_jobs) polling threads), we have one subscriber
// thread and per-job condition variables.
class SliceNotifier {
public:
    static SliceNotifier& instance() {
        static SliceNotifier inst;
        return inst;
    }

    // Register a job before submitting its slices to NATS so we never miss
    // a notification that arrives before waitForAllSlices() is called.
    void registerJob(const std::string& job_id, size_t total_slices) {
        std::lock_guard<std::mutex> lk(mu_);
        jobs_[job_id] = {0, total_slices};
    }

    void unregisterJob(const std::string& job_id) {
        std::lock_guard<std::mutex> lk(mu_);
        jobs_.erase(job_id);
        cv_.notify_all();
    }

    // Bump the received counter for a job (called by subscriber thread OR
    // by the aggregator after it pre-reads already-present slices from Redis).
    void notifySlice(const std::string& job_id) {
        {
            std::lock_guard<std::mutex> lk(mu_);
            auto it = jobs_.find(job_id);
            if (it != jobs_.end()) {
                it->second.received++;
            }
        }
        cv_.notify_all();
    }

    // Block until all slices for job_id have been notified, or timeout.
    // Returns true on success, false on timeout.
    bool waitForAllSlices(const std::string& job_id, size_t total_slices,
                          std::chrono::milliseconds timeout) {
        auto deadline = std::chrono::steady_clock::now() + timeout;
        std::unique_lock<std::mutex> lk(mu_);
        return cv_.wait_until(lk, deadline, [&] {
            auto it = jobs_.find(job_id);
            return it != jobs_.end() && it->second.received >= total_slices;
        });
    }

private:
    struct JobState {
        size_t received = 0;
        size_t total    = 0;
    };

    std::mutex mu_;
    std::condition_variable cv_;
    std::unordered_map<std::string, JobState> jobs_;

    std::unique_ptr<Redis> sub_redis_;
    std::thread sub_thread_;
    std::atomic<bool> running_{false};

    SliceNotifier() {
        auto opts = RedisManager::buildConnectionOptions();
        // socket_timeout makes consume() return periodically so the loop can
        // check running_ and not block forever on shutdown.
        opts.socket_timeout = std::chrono::milliseconds(500);
        sub_redis_ = std::make_unique<Redis>(opts);

        running_ = true;
        sub_thread_ = std::thread(&SliceNotifier::subscriberLoop, this);
        std::cout << "🔔 SliceNotifier subscriber thread started" << std::endl;
    }

    ~SliceNotifier() {
        running_ = false;
        if (sub_thread_.joinable()) sub_thread_.join();
    }

    void subscriberLoop() {
        auto sub = sub_redis_->subscriber();

        sub.on_pmessage([this](std::string /*pattern*/,
                               std::string channel,
                               std::string /*payload*/) {
            // channel = "slice_done:{job_id}"
            constexpr std::string_view prefix = "slice_done:";
            if (channel.rfind(prefix.data(), 0) == 0) {
                notifySlice(channel.substr(prefix.size()));
            }
        });

        sub.psubscribe("slice_done:*");

        while (running_) {
            try {
                sub.consume();
            } catch (const TimeoutError&) {
                // Expected: socket_timeout fired, loop around and check running_
            } catch (const std::exception& e) {
                if (running_) {
                    std::cerr << "⚠️  SliceNotifier error: " << e.what()
                              << " — retrying in 1s" << std::endl;
                    std::this_thread::sleep_for(std::chrono::seconds(1));
                }
            }
        }
    }

    SliceNotifier(const SliceNotifier&)            = delete;
    SliceNotifier& operator=(const SliceNotifier&) = delete;
};
