#pragma once
#include <nats/nats.h>
#include <cstdlib>
#include "publisher_interface.h"

class NatsPublisher : public IPublisher {
public:
    NatsPublisher() : conn_(nullptr), connected_(false) {
        // Read NATS URL from environment variable, default to localhost
        const char* nats_url_env = std::getenv("NATS_URL");
        std::string nats_url = nats_url_env ? nats_url_env : "nats://localhost:4222";

        std::cout << "🔧 Initializing NATS Publisher..." << std::endl;
        std::cout << "📡 Connecting to: " << nats_url << std::endl;
        
        // Initialize NATS library
        natsStatus s = nats_Open(-1);
        if (s != NATS_OK) {
            std::cerr << "❌ Failed to initialize NATS library: " << natsStatus_GetText(s) << std::endl;
            return;
        }

        // Create connection options
        natsOptions *opts = nullptr;
        s = natsOptions_Create(&opts);
        if (s != NATS_OK) {
            std::cerr << "❌ Failed to create NATS options: " << natsStatus_GetText(s) << std::endl;
            nats_Close();
            return;
        }

        // Set NATS URL in options
        s = natsOptions_SetURL(opts, nats_url.c_str());
        if (s != NATS_OK) {
            std::cerr << "❌ Failed to set NATS URL: " << natsStatus_GetText(s) << std::endl;
            natsOptions_Destroy(opts);
            nats_Close();
            return;
        }

        // Connect to NATS server
        s = natsConnection_Connect(&conn_, opts);
        if (s != NATS_OK) {
            std::cerr << "❌ Failed to connect to NATS: " << natsStatus_GetText(s) << std::endl;
            natsOptions_Destroy(opts);
            nats_Close();
            return;
        }

        natsOptions_Destroy(opts);
        connected_ = true;
        std::cout << "✅ NATS Publisher connected successfully" << std::endl;
    }

    void publish(const std::string& subj, const std::string& payload) override {
        if (!connected_ || !conn_) {
            std::cerr << "❌ NATS not connected, cannot publish to " << subj << std::endl;
            return;
        }

        std::cout << "📤 Publishing to " << subj << " (size: " << payload.length() << " bytes)" << std::endl;
        
        natsStatus s = natsConnection_PublishString(conn_, subj.c_str(), payload.c_str());
        if (s != NATS_OK) {
            std::cerr << "❌ NATS publish failed to " << subj << ": " << natsStatus_GetText(s) << std::endl;
            return;
        }

        // Flush to ensure delivery
        s = natsConnection_Flush(conn_);
        if (s != NATS_OK) {
            std::cerr << "⚠️  NATS flush failed: " << natsStatus_GetText(s) << std::endl;
        } else {
            std::cout << "✅ Message published and flushed to " << subj << std::endl;
        }
    }

    ~NatsPublisher() {
        if (conn_) {
            std::cout << "🔌 Closing NATS connection..." << std::endl;
            natsConnection_Destroy(conn_);
        }
        if (connected_) {
            nats_Close();
        }
    }

    bool isConnected() const override {
        return connected_;
    }

private:
    natsConnection *conn_;
    bool connected_;
};