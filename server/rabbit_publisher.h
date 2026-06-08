#pragma once
#include <SimpleAmqpClient/SimpleAmqpClient.h>
#include <string>

class RabbitPublisher {
public:
    RabbitPublisher(const std::string& host = "localhost")
    {
        chan_ = AmqpClient::Channel::Create(host);
        chan_->DeclareQueue("task_queue", /*passive*/false,
                            /*durable*/true, /*exclusive*/false,
                            /*auto_delete*/false);
    }

    // publish persistent message; properties can hold headers later
    void publish(const std::string& payload)
    {
        auto msg = AmqpClient::BasicMessage::Create(payload);
        msg->DeliveryMode(AmqpClient::BasicMessage::dm_persistent);
        chan_->BasicPublish("", "task_queue", msg, /*mandatory*/false, /*immediate*/false);
    }

private:
    AmqpClient::Channel::ptr_t chan_;
};

