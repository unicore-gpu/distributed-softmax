#pragma once
#include <string>
#include <cstdlib>

class RedisConfig {
public:
    // Default TTL values in seconds
    static constexpr int DEFAULT_RESULT_TTL = 3600;      // 1 hour for final results
    static constexpr int DEFAULT_SLICE_TTL = 7200;       // 2 hours for slice results  
    static constexpr int DEFAULT_METADATA_TTL = 86400;   // 24 hours for job metadata
    static constexpr int DEFAULT_PROGRESS_TTL = 3600;    // 1 hour for progress tracking
    
    // Get TTL values from environment or use defaults
    static int getResultTTL() {
        const char* env_val = std::getenv("REDIS_TTL_HOURS");
        return env_val ? std::stoi(env_val) * 3600 : DEFAULT_RESULT_TTL;
    }
    
    static int getSliceTTL() {
        const char* env_val = std::getenv("REDIS_TTL_SLICE_HOURS");
        return env_val ? std::stoi(env_val) * 3600 : DEFAULT_SLICE_TTL;
    }
    
    static int getMetadataTTL() {
        const char* env_val = std::getenv("REDIS_TTL_METADATA_HOURS");
        return env_val ? std::stoi(env_val) * 3600 : DEFAULT_METADATA_TTL;
    }
    
    static int getProgressTTL() {
        const char* env_val = std::getenv("REDIS_TTL_PROGRESS_HOURS");
        return env_val ? std::stoi(env_val) * 3600 : DEFAULT_PROGRESS_TTL;
    }
    
    // Redis key generators
    static std::string makeResultKey(const std::string& job_id) {
        return "result:" + job_id;
    }
    
    static std::string makeSliceKey(const std::string& job_id, int slice_id) {
        return "result:" + job_id + ":" + std::to_string(slice_id);
    }
    
    static std::string makeMetadataKey(const std::string& job_id) {
        return "metadata:" + job_id;
    }
    
    static std::string makeProgressKey(const std::string& job_id) {
        return "progress:" + job_id;
    }
};
