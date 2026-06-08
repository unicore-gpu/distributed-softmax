#pragma once
#include <sw/redis++/redis++.h>
#include <nlohmann/json.hpp>
#include <optional>
#include <chrono>
#include "redis_config.h"
#include <cstdlib>


using namespace sw::redis;

struct JobMetadata {
    std::string job_id;
    std::string task_type;
    size_t total_slices;
    std::time_t created_at;
    std::string status;
    
    // Convert to JSON
    nlohmann::json to_json() const {
        return nlohmann::json{
            {"job_id", job_id},
            {"task_type", task_type},
            {"total_slices", total_slices},
            {"created_at", created_at},
            {"status", status}
        };
    }
    
    // Create from JSON
    static JobMetadata from_json(const nlohmann::json& j) {
        return JobMetadata{
            j["job_id"],
            j["task_type"],
            j["total_slices"],
            j["created_at"],
            j.value("status", "submitted")
        };
    }
};

class RedisManager {
private:
    Redis redis_;

    // Build typed ConnectionOptions from environment variables.
    // Shared by constructor and SliceNotifier.
    static ConnectionOptions buildBaseConnectionOptions() {
        ConnectionOptions opts;
        const char* h = std::getenv("REDIS_HOST");
        const char* p = std::getenv("REDIS_PORT");
        const char* pw = std::getenv("REDIS_PASSWORD");
        opts.host = h ? h : "127.0.0.1";
        opts.port = p ? std::stoi(p) : 6379;
        if (pw) opts.password = pw;
        return opts;
    }

    // Pool size: REDIS_POOL_SIZE env var, default 32.
    // Prevents "Connection reset by peer" under high concurrency (conc ≥ 16).
    static ConnectionPoolOptions buildPoolOptions() {
        ConnectionPoolOptions pool;
        const char* e = std::getenv("REDIS_POOL_SIZE");
        pool.size = (e && std::atoi(e) > 0) ? std::atoi(e) : 32;
        pool.wait_timeout = std::chrono::milliseconds(200);
        return pool;
    }

public:
    // Build typed ConnectionOptions — used by SliceNotifier for pub/sub.
    static ConnectionOptions buildConnectionOptions() {
        return buildBaseConnectionOptions();
    }

    RedisManager()
        : redis_(buildBaseConnectionOptions(), buildPoolOptions()) {
        auto opts = buildBaseConnectionOptions();
        auto pool = buildPoolOptions();
        std::cout << "🔗 Redis: tcp://" << opts.host << ":" << opts.port
                  << "  pool=" << pool.size << std::endl;
    }

    RedisManager(const std::string& connection_string)
        : redis_(connection_string) {}
    
    // TTL-aware storage methods
    void setWithTTL(const std::string& key, const std::string& value, int ttl_seconds) {
        try {
            redis_.setex(key, std::chrono::seconds(ttl_seconds), value);
            std::cout << "✅ Stored " << key << " with TTL " << ttl_seconds << "s" << std::endl;
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to store " << key << " with TTL: " << e.what() << std::endl;
        }
    }
    
    // Standard get operation
    std::optional<std::string> get(const std::string& key) {
        try {
            return redis_.get(key);
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to get " << key << ": " << e.what() << std::endl;
            return std::nullopt;
        }
    }
    
    // Check if key exists
    bool exists(const std::string& key) {
        try {
            return redis_.exists(key) > 0;
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to check existence of " << key << ": " << e.what() << std::endl;
            return false;
        }
    }
    
    // Get TTL of a key - Fixed for Redis++ API
    int getTTL(const std::string& key) {
        try {
            // Redis++ ttl() returns long long directly, not a duration
            long long ttl_value = redis_.ttl(key);
            return static_cast<int>(ttl_value);
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to get TTL for " << key << ": " << e.what() << std::endl;
            return -1;
        }
    }
    
    // Job metadata operations
    void setJobMetadata(const std::string& job_id, const JobMetadata& metadata) {
        std::string key = RedisConfig::makeMetadataKey(job_id);
        std::string value = metadata.to_json().dump();
        setWithTTL(key, value, RedisConfig::getMetadataTTL());
    }
    
    std::optional<JobMetadata> getJobMetadata(const std::string& job_id) {
        std::string key = RedisConfig::makeMetadataKey(job_id);
        auto value = get(key);
        
        if (!value) {
            return std::nullopt;
        }
        
        try {
            nlohmann::json j = nlohmann::json::parse(*value);
            return JobMetadata::from_json(j);
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to parse metadata for " << job_id << ": " << e.what() << std::endl;
            return std::nullopt;
        }
    }
    
    // Store final result with TTL
    void storeFinalResult(const std::string& job_id, const std::string& result_json) {
        std::string key = RedisConfig::makeResultKey(job_id);
        setWithTTL(key, result_json, RedisConfig::getResultTTL());
        
        // Also update progress tracking
        updateProgress(job_id, "completed");
    }
    
    // Update progress tracking
    void updateProgress(const std::string& job_id, const std::string& status) {
        std::string key = RedisConfig::makeProgressKey(job_id);
        nlohmann::json progress = {
            {"job_id", job_id},
            {"status", status},
            {"updated_at", std::time(nullptr)}
        };
        setWithTTL(key, progress.dump(), RedisConfig::getProgressTTL());
    }
    
    // Get slice completion count
    int getCompletedSliceCount(const std::string& job_id, size_t total_slices) {
        int completed = 0;
        
        for (size_t i = 0; i < total_slices; ++i) {
            std::string slice_key = RedisConfig::makeSliceKey(job_id, i);
            if (exists(slice_key)) {
                completed++;
            }
        }
        
        return completed;
    }
    
    // Cleanup expired keys (manual cleanup)
    void cleanupExpiredKeys() {
        // Note: Redis automatically removes expired keys, but we can do manual cleanup
        // for monitoring purposes
        std::cout << "🧹 Running manual cleanup check..." << std::endl;
        
        // This is mainly for logging/monitoring
        // Redis handles TTL expiration automatically
    }
    
    // Get all keys matching a pattern (for debugging)
    std::vector<std::string> getKeysByPattern(const std::string& pattern) {
        std::vector<std::string> keys;
        try {
            redis_.keys(pattern, std::back_inserter(keys));
        } catch (const std::exception& e) {
            std::cerr << "❌ Failed to get keys by pattern " << pattern << ": " << e.what() << std::endl;
        }
        return keys;
    }
};