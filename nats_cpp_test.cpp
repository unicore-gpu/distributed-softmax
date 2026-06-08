#include <iostream>
#include <string>
#include <nats/nats.h>
#include <unistd.h>

int main() {
    natsConnection *conn = nullptr;
    natsStatus s;
    
    std::cout << "🔧 Testing C++ NATS Publisher..." << std::endl;
    
    // Initialize NATS library
    s = nats_Open(-1);
    if (s != NATS_OK) {
        std::cerr << "❌ Failed to initialize NATS: " << natsStatus_GetText(s) << std::endl;
        return 1;
    }
    
    // Connect to NATS
    std::cout << "🔌 Connecting to NATS..." << std::endl;
    s = natsConnection_ConnectTo(&conn, "nats://localhost:4222");
    if (s != NATS_OK) {
        std::cerr << "❌ Failed to connect to NATS: " << natsStatus_GetText(s) << std::endl;
        nats_Close();
        return 1;
    }
    
    std::cout << "✅ Connected to NATS server" << std::endl;
    
    // Test message (same format as your gateway should send)
    std::string test_message = R"({
    "job_id": "cpp-test-12345",
    "slice_id": 0,
    "task": "softmax",
    "data": [1.0, 2.0, 3.0, 4.0, 5.0]
})";
    
    std::cout << "📤 Publishing test message..." << std::endl;
    std::cout << "   Subject: task_queue" << std::endl;
    std::cout << "   Message: " << test_message << std::endl;
    
    // Publish message
    s = natsConnection_PublishString(conn, "task_queue", test_message.c_str());
    if (s != NATS_OK) {
        std::cerr << "❌ Failed to publish: " << natsStatus_GetText(s) << std::endl;
        natsConnection_Destroy(conn);
        nats_Close();
        return 1;
    }
    
    std::cout << "✅ Message published successfully" << std::endl;
    
    // Flush to ensure delivery
    std::cout << "🔄 Flushing connection..." << std::endl;
    s = natsConnection_Flush(conn);
    if (s != NATS_OK) {
        std::cerr << "⚠️  Flush failed: " << natsStatus_GetText(s) << std::endl;
    } else {
        std::cout << "✅ Connection flushed" << std::endl;
    }
    
    // Wait a moment to ensure delivery
    std::cout << "⏰ Waiting for delivery..." << std::endl;
    sleep(1);
    
    std::cout << "🎯 Test completed!" << std::endl;
    std::cout << "   Check your debug_worker.py - it should have received the message" << std::endl;
    
    // Clean up
    natsConnection_Destroy(conn);
    nats_Close();
    
    return 0;
}
