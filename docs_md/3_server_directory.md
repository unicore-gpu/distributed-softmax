# Server 目录详细文档

网关服务器是整个系统的中控，负责接收 gRPC 请求、维护 Redis 状态、向 NATS 分发任务切片，并聚合最终结果。

## 1. `gateway_server.cc` 逻辑核心

### A. 任务切片与 NATS 分发

在 `SubmitTask` 方法中，网关将巨大的向量切割成小块以实现分布式处理。

```cpp
// 246-278行: 切片逻辑
size_t vector_size = req->vector_size();
// 计算总切片数，DEFAULT_SLICE_SIZE = 10
size_t total_slices = (vector_size + DEFAULT_SLICE_SIZE - 1) / DEFAULT_SLICE_SIZE;

// 在 Redis 记录元数据
JobMetadata metadata{ req->job_id(), req->task(), total_slices, std::time(nullptr) };
redis_manager_.setJobMetadata(req->job_id(), metadata);

for (size_t offset = 0; offset < vector_size; offset += DEFAULT_SLICE_SIZE, ++slice_id) {
    nlohmann::json slice_message{
        {"job_id",  req->job_id()},
        {"slice_id", slice_id},
        {"task",    req->task()}
    };
    // 将一小段向量（1-10个元素）放入 slice_message["data"]
    for (size_t i = offset; i < std::min(offset + DEFAULT_SLICE_SIZE, vector_size); ++i)
        slice_message["data"].push_back(req->vector(i));
    
    // 向 NATS 的 "task_queue" 频道发布 JSON 数据
    bus.publish("task_queue", slice_message.dump());
}
```

**参数解释**:
- `DEFAULT_SLICE_SIZE`: 指定每个分片包含的浮点数数量。较小的尺寸增加了并行度，但增加了调度开销。

---

### B. 后台聚合逻辑

网关会开启一个独立线程来等待各分片计算完成。

```cpp
// 81-133行: 检查切片完成情况
bool waitForAllSlices(const std::string& job_id, size_t total_slices, 
                     std::vector<std::vector<double>>& slice_results) {
    for (int attempt = 0; attempt < MAX_POLL_ATTEMPTS; ++attempt) {
        bool all_complete = true;
        for (size_t slice_index = 0; slice_index < total_slices; ++slice_index) {
            std::string slice_key = RedisConfig::makeSliceKey(job_id, slice_index);
            auto slice_result = redis_manager_.get(slice_key);
            if (slice_result) {
                // 如果 Redis 里有这个 Key，说明该分片已被 Worker 处理并存入结果
                slice_results[slice_index] = parseJson(slice_result);
            } else {
                all_complete = false; break;
            }
        }
        if (all_complete) return true;
        std::this_thread::sleep_for(std::chrono::milliseconds(100)); // 轮询等待
    }
    return false;
}
```

---

## 2. `redis_manager.h` 通信管理

该文件封装了 Redis++ 库，通过设定不同生命周期 (TTL) 来管理内存占用。

```cpp
// 49-56行: 设定过期时间的存储
void setWithTTL(const std::string& key, const std::string& value, int ttl_seconds) {
    // 使用 redis_.setex 命令原子地设置值和过期时间
    redis_.setex(key, std::chrono::seconds(ttl_seconds), value);
}
```

---

## 3. `redis_config.h` 配置项

通过环境变量可以控制系统存储的持久性。

| 环境变量 | 默认值 (秒) | 描述 |
| :--- | :--- | :--- |
| `REDIS_TTL_HOURS` | 3600 | 最终计算结果保留 1 小时 |
| `REDIS_TTL_SLICE_HOURS` | 7200 | 中间切片结果保留 2 小时 |
| `REDIS_TTL_METADATA_HOURS` | 86400 | 任务元数据（总进度信息）保留 24 小时 |

---

## 4. `cache_manager.h` 后台清理

这是一个监控线程，每隔 15 分钟运行一次。

```cpp
// 179-196行: 清理循环
void cleanupLoop() {
    while (running_.load()) {
        performCleanup(); // 此处实际上是由 Redis 自动处理过期，但该函数用于打印系统状态指标
        printCacheStats(); // 输出当前活跃 Job 数和 Redis 内存统计
        std::this_thread::sleep_for(cleanup_interval_);
    }
}
```
