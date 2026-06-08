#pragma once
#include <zmq.h>
#include <cerrno>
#include <cstdlib>
#include <cstring>
#include <iostream>
#include <mutex>
#include <string>
#include "publisher_interface.h"

// ZMQ PUSH publisher — binds a PUSH socket so workers (PULL) can connect.
//
// Topology:  Gateway BINDS PUSH  ←→  Workers CONNECT PULL
//
// Gateway is the stable endpoint; workers are ephemeral.  ZMQ distributes
// messages round-robin across all connected workers automatically — no queue
// group configuration needed.
//
// Transport selection:
//   Same machine  →  ZMQ_PUSH_ENDPOINT=ipc:///tmp/softmax_tasks   (default)
//   Cross-machine →  ZMQ_PUSH_ENDPOINT=tcp://0.0.0.0:5555
//
// Workers must set ZMQ_PULL_ENDPOINT to the matching address.
class ZmqPublisher : public IPublisher {
public:
    ZmqPublisher() : ctx_(nullptr), sock_(nullptr), connected_(false) {
        const char* ep_env = std::getenv("ZMQ_PUSH_ENDPOINT");
        endpoint_ = ep_env ? ep_env : "ipc:///tmp/softmax_tasks";

        ctx_ = zmq_ctx_new();
        if (!ctx_) {
            std::cerr << "❌ ZMQ: failed to create context" << std::endl;
            return;
        }

        sock_ = zmq_socket(ctx_, ZMQ_PUSH);
        if (!sock_) {
            std::cerr << "❌ ZMQ: failed to create PUSH socket" << std::endl;
            zmq_ctx_destroy(ctx_);
            ctx_ = nullptr;
            return;
        }

        // Backpressure: block sender when HWM is reached rather than dropping.
        int hwm = 50000;
        zmq_setsockopt(sock_, ZMQ_SNDHWM, &hwm, sizeof(hwm));

        // Linger: on close, wait up to 1 s for pending messages to drain.
        int linger_ms = 1000;
        zmq_setsockopt(sock_, ZMQ_LINGER, &linger_ms, sizeof(linger_ms));

        if (zmq_bind(sock_, endpoint_.c_str()) != 0) {
            std::cerr << "❌ ZMQ: failed to bind to " << endpoint_
                      << ": " << zmq_strerror(errno) << std::endl;
            zmq_close(sock_);
            zmq_ctx_destroy(ctx_);
            sock_ = nullptr;
            ctx_ = nullptr;
            return;
        }

        connected_ = true;
        std::cout << "✅ ZMQ Publisher bound to " << endpoint_ << std::endl;
    }

    // subject is ignored for ZMQ (the socket topology already routes correctly).
    // Mutex guard: ZMQ sockets are NOT thread-safe; multiple concurrent gRPC
    // threads (P0 enlarged pool) call publish() simultaneously.
    void publish(const std::string& /*subject*/, const std::string& payload) override {
        if (!connected_) {
            std::cerr << "❌ ZMQ not connected, dropping message" << std::endl;
            return;
        }
        std::lock_guard<std::mutex> lock(send_mutex_);
        if (zmq_send(sock_, payload.c_str(), payload.size(), ZMQ_DONTWAIT) < 0) {
            if (errno == EAGAIN) {
                zmq_send(sock_, payload.c_str(), payload.size(), 0);
            } else {
                std::cerr << "❌ ZMQ send failed: " << zmq_strerror(errno) << std::endl;
            }
        }
    }

    bool isConnected() const override { return connected_; }

    ~ZmqPublisher() override {
        if (sock_) {
            zmq_close(sock_);
            sock_ = nullptr;
        }
        if (ctx_) {
            zmq_ctx_destroy(ctx_);
            ctx_ = nullptr;
        }
    }

private:
    void*       ctx_;
    void*       sock_;
    bool        connected_;
    std::string endpoint_;
    std::mutex  send_mutex_;
};
