#pragma once
#include <string>

// Common interface for all message transport backends.
// Lets gateway_server.cc select NATS vs ZMQ at runtime via the
// TRANSPORT environment variable without recompiling.
class IPublisher {
public:
    virtual void publish(const std::string& subject, const std::string& payload) = 0;
    virtual bool isConnected() const = 0;
    virtual ~IPublisher() = default;
};
