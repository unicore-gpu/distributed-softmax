#pragma once
#include <thread>
#include <chrono>
#include <atomic>
#include <vector>
#include <iostream>
#include "redis_manager.h"
#include "redis_config.h"

class CacheManager {
private:
    RedisManager& redis_manager_;
    std::atomic<bool> running_;
    std::thread cleanup_thread_;
    std::chrono::minutes cleanup_interval_;
    
public:
    CacheManager(RedisManager& redis_manager, int cleanup_interval_minutes = 15)
        : redis_manager_(redis_manager), running_(false), 
          cleanup_interval_(cleanup_interval_minutes) {}
    
    ~CacheManager() {
        stop();
    }
    
    void start() {
        if (running_.load()) {
            std::cout << "⚠️  Cache manager already running" << std::endl;
            return;
        }
        
        running_ = true;
        cleanup_thread_ = std::thread(&CacheManager::cleanupLoop, this);
        std::cout << "🧹 Cache manager started (cleanup every " << cleanup_interval_.count() << " minutes)" << std::endl;
    }
    
    void stop() {
        if (!running_.load()) {
            return;
        }
        
        running_ = false;
        if (cleanup_thread_.joinable()) {
            cleanup_thread_.join();
        }
        std::cout << "🛑 Cache manager stopped" << std::endl;
    }
    
    void performCleanup() {
        std::cout << "🧹 Starting cache cleanup..." << std::endl;
        
        try {
            // Get statistics before cleanup
            auto result_keys = redis_manager_.getKeysByPattern("result:*");
            auto metadata_keys = redis_manager_.getKeysByPattern("metadata:*");
            auto progress_keys = redis_manager_.getKeysByPattern("progress:*");
            
            int total_before = result_keys.size() + metadata_keys.size() + progress_keys.size();
            int expired_count = 0;
            
            // Check each key's TTL
            for (const auto& key : result_keys) {
                int ttl = redis_manager_.getTTL(key);
                if (ttl == -2) {  // Key expired/doesn't exist
                    expired_count++;
                }
            }
            
            for (const auto& key : metadata_keys) {
                int ttl = redis_manager_.getTTL(key);
                if (ttl == -2) {
                    expired_count++;
                }
            }
            
            for (const auto& key : progress_keys) {
                int ttl = redis_manager_.getTTL(key);
                if (ttl == -2) {
                    expired_count++;
                }
            }
            
            std::cout << "📊 Cleanup stats:" << std::endl;
            std::cout << "   Total keys checked: " << total_before << std::endl;
            std::cout << "   Result keys: " << result_keys.size() << std::endl;
            std::cout << "   Metadata keys: " << metadata_keys.size() << std::endl;
            std::cout << "   Progress keys: " << progress_keys.size() << std::endl;
            std::cout << "   Expired/cleaned: " << expired_count << std::endl;
            
        } catch (const std::exception& e) {
            std::cerr << "❌ Cleanup error: " << e.what() << std::endl;
        }
        
        std::cout << "✅ Cache cleanup completed" << std::endl;
    }
    
    std::vector<std::string> listActiveJobs() {
        std::vector<std::string> active_jobs;
        
        try {
            auto metadata_keys = redis_manager_.getKeysByPattern("metadata:*");
            
            for (const auto& key : metadata_keys) {
                // Extract job_id from "metadata:job_id"
                if (key.length() > 9) {  // "metadata:" = 9 chars
                    std::string job_id = key.substr(9);
                    
                    // Check if metadata still exists (not expired)
                    if (redis_manager_.exists(key)) {
                        active_jobs.push_back(job_id);
                    }
                }
            }
            
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to list active jobs: " << e.what() << std::endl;
        }
        
        return active_jobs;
    }
    
    void extendJobTTL(const std::string& job_id, int additional_seconds) {
        try {
            // Get current metadata
            auto metadata = redis_manager_.getJobMetadata(job_id);
            if (!metadata) {
                std::cout << "⚠️  Job " << job_id << " not found for TTL extension" << std::endl;
                return;
            }
            
            // Extend final result TTL if it exists
            std::string result_key = RedisConfig::makeResultKey(job_id);
            if (redis_manager_.exists(result_key)) {
                auto result_value = redis_manager_.get(result_key);
                if (result_value) {
                    int new_ttl = RedisConfig::getResultTTL() + additional_seconds;
                    redis_manager_.setWithTTL(result_key, *result_value, new_ttl);
                    std::cout << "🔄 Extended result TTL for " << job_id << " by " << additional_seconds << "s" << std::endl;
                }
            }
            
            // Extend metadata TTL
            std::string metadata_key = RedisConfig::makeMetadataKey(job_id);
            int new_metadata_ttl = RedisConfig::getMetadataTTL() + additional_seconds;
            redis_manager_.setWithTTL(metadata_key, metadata->to_json().dump(), new_metadata_ttl);
            
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to extend TTL for job " << job_id << ": " << e.what() << std::endl;
        }
    }
    
    void printCacheStats() {
        try {
            auto active_jobs = listActiveJobs();
            auto result_keys = redis_manager_.getKeysByPattern("result:*");
            
            std::cout << "📈 Cache Statistics:" << std::endl;
            std::cout << "   Active jobs: " << active_jobs.size() << std::endl;
            std::cout << "   Total result keys: " << result_keys.size() << std::endl;
            
            if (!active_jobs.empty()) {
                std::cout << "   Recent jobs:" << std::endl;
                for (size_t i = 0; i < std::min(active_jobs.size(), size_t(5)); ++i) {
                    std::string job_id = active_jobs[i];
                    std::string result_key = RedisConfig::makeResultKey(job_id);
                    int ttl = redis_manager_.getTTL(result_key);
                    
                    std::string status = redis_manager_.exists(result_key) ? "completed" : "pending";
                    std::cout << "     " << job_id << " (" << status << ", TTL: " << ttl << "s)" << std::endl;
                }
            }
            
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to print cache stats: " << e.what() << std::endl;
        }
    }

private:
    void cleanupLoop() {
        while (running_.load()) {
            try {
                // Perform cleanup
                performCleanup();
                
                // Print stats periodically
                printCacheStats();
                
                // Wait for next cleanup cycle
                std::this_thread::sleep_for(cleanup_interval_);
                
            } catch (const std::exception& e) {
                std::cerr << "❌ Cleanup loop error: " << e.what() << std::endl;
                std::this_thread::sleep_for(std::chrono::minutes(1)); // Brief pause on error
            }
        }
    }
};
