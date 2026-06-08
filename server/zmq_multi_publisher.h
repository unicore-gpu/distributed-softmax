#pragma once
#include "publisher_interface.h"
#include <zmq.h>
#include <cerrno>
#include <cstdlib>
#include <iostream>
#include <mutex>
#include <string>
#include <vector>

// ---------------------------------------------------------------------------
// ZmqMultiPublisher
//
// Binds one ZMQ PUSH socket per worker rank (0 … world_size-1).
// Used in NCCL dispatch mode so slice i is always sent to GPU i,
// guaranteeing all workers process jobs in the same order — required by NCCL.
//
// ── Single-machine (IPC) mode ────────────────────────────────────────────────
//   ZMQ_BASE_ENDPOINT=ipc:///tmp/softmax  (default)
//   Gateway binds:  ipc:///tmp/softmax_0 … ipc:///tmp/softmax_3
//   Workers connect: ipc:///tmp/softmax_0 … ipc:///tmp/softmax_3
//
// ── Multi-machine (TCP) mode ─────────────────────────────────────────────────
//   ZMQ_BASE_ENDPOINT=tcp  (or anything not starting with "ipc")
//   ZMQ_BASE_PORT=5560     (default; gateway binds 5560, 5561, 5562, 5563)
//   Gateway binds:  tcp://0.0.0.0:5560 … tcp://0.0.0.0:5563
//   Workers connect: tcp://<GATEWAY_HOST>:5560 … tcp://<GATEWAY_HOST>:5563
//     where GATEWAY_HOST = ZMQ_GATEWAY_ADDR env var on the worker side
//
// publish(subject, payload): subject is the rank as a decimal string ("0".."N-1")
//
// Thread safety: each socket has its own mutex — safe for the 256-thread
// gRPC pool (P0) to call publish() concurrently.
// ---------------------------------------------------------------------------
class ZmqMultiPublisher : public IPublisher {
public:
    explicit ZmqMultiPublisher(int world_size = 4)
        : ctx_(nullptr), connected_(false), mutexes_(world_size) {

        const char* base_env = std::getenv("ZMQ_BASE_ENDPOINT");
        std::string base     = base_env ? base_env : "ipc:///tmp/softmax";

        // ── TCP mode: bind one port per rank ─────────────────────────────────
        const bool tcp_mode = (base.find("tcp") != std::string::npos ||
                                base.find("TCP") != std::string::npos);
        int base_port = 5560;
        if (tcp_mode) {
            const char* port_env = std::getenv("ZMQ_BASE_PORT");
            if (port_env && std::atoi(port_env) > 0)
                base_port = std::atoi(port_env);
        }

        ctx_ = zmq_ctx_new();
        if (!ctx_) {
            std::cerr << "❌ ZmqMultiPublisher: failed to create context" << std::endl;
            return;
        }

        sockets_.resize(world_size, nullptr);
        endpoints_.resize(world_size);
        for (int i = 0; i < world_size; ++i) {
            if (tcp_mode)
                endpoints_[i] = "tcp://0.0.0.0:" + std::to_string(base_port + i);
            else
                endpoints_[i] = base + "_" + std::to_string(i);

            sockets_[i] = zmq_socket(ctx_, ZMQ_PUSH);
            if (!sockets_[i]) {
                std::cerr << "❌ ZmqMultiPublisher: socket failed for rank " << i << std::endl;
                return;
            }
            int linger = 0;
            zmq_setsockopt(sockets_[i], ZMQ_LINGER, &linger, sizeof(linger));
            int hwm = 100000;
            zmq_setsockopt(sockets_[i], ZMQ_SNDHWM, &hwm, sizeof(hwm));

            if (zmq_bind(sockets_[i], endpoints_[i].c_str()) != 0) {
                std::cerr << "❌ ZmqMultiPublisher: bind failed on "
                          << endpoints_[i] << ": " << zmq_strerror(errno) << std::endl;
                return;
            }
            std::cout << "✅ ZMQ PUSH bound to " << endpoints_[i] << std::endl;
        }
        if (tcp_mode)
            std::cout << "🌐 Multi-machine TCP mode: workers connect to "
                      << "<GATEWAY_HOST>:" << base_port << " … "
                      << "<GATEWAY_HOST>:" << base_port + world_size - 1 << std::endl;
        connected_ = true;
    }

    ~ZmqMultiPublisher() override {
        for (auto* s : sockets_) if (s) zmq_close(s);
        if (ctx_) zmq_ctx_destroy(ctx_);
    }

    // subject = rank as string ("0", "1", …)
    void publish(const std::string& subject, const std::string& payload) override {
        int rank = std::atoi(subject.c_str());
        if (rank < 0 || rank >= static_cast<int>(sockets_.size())) {
            std::cerr << "❌ ZmqMultiPublisher: invalid rank '" << subject << "'" << std::endl;
            return;
        }
        std::lock_guard<std::mutex> lock(mutexes_[rank]);
        if (zmq_send(sockets_[rank], payload.data(), payload.size(), ZMQ_DONTWAIT) < 0) {
            if (errno == EAGAIN)
                zmq_send(sockets_[rank], payload.data(), payload.size(), 0);
        }
    }

    bool isConnected() const override { return connected_; }

private:
    void*                    ctx_;
    std::vector<void*>       sockets_;
    std::vector<std::string> endpoints_;
    std::vector<std::mutex>  mutexes_;
    bool                     connected_;
};
