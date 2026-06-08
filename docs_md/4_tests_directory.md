# Tests 目录详细文档

该目录包含了一系列验证系统稳定性（特别是缓存管理）的测试工具。

## 1. `ttl_tests.py` - TTL 全面验证

该脚本的核心在于通过 **环境变量钩子** 模拟高加速处理流程。

### A. 配置覆盖测试

```python
# 45-56行: 动态修改配置
# 通过环境变量设置极短的 TTL (0.01 小时 ≈ 36 秒)
os.environ['REDIS_TTL_HOURS'] = '0.01' 
custom_result_ttl_seconds = RedisConfig.get_result_ttl() # 程序会自动从环境变量读取并覆盖默认值
```

### B. 实时过期监控

```python
# 109-130行: 轮询 Redis 内部状态
for poll_attempt in range(20):
    # 下列命令直接查询 Redis 内部的生存时间计数器 (TTL)
    metadata_ttl_seconds = redis_client.ttl(metadata_key)
    result_ttl_seconds = redis_client.ttl(result_key)
    
    print(f"Meta TTL: {metadata_ttl_seconds}s | Result TTL: {result_ttl_seconds}s")
```

**参数说明**:
- `redis_client.ttl(key)`: 返回该 Key 距离过期删除还剩多少秒。若返回 -2，表示 Key 已不存在（已被成功自动清理）。

---

## 2. `debug_nats.py` - 通信链路测试

当 Worker 不干活时，通过此脚本确定 NATS 队列是否畅通。

```python
# 20-30行: 模拟发布原始切片数据
first_test_message = {
    "job_id": "test-123",
    "slice_id": 0,
    "task": "softmax",
    "data": [1.0, 2.0, 3.0]
}
await nats_client.publish("task_queue", json.dumps(first_test_message).encode())
```

---

## 3. `debug_worker.py` - Worker 逻辑单体测试

如果想脱离 gRPC 网关直接给 Worker 派活，可以使用此工具。

```python
# 62-67行: 模拟 Worker 将结果写入 Redis
# 构造符合网关规则的结果 Key: result:JOB_ID:SLICE_ID
result_key = f"result:{job_id}:{slice_id}"
dummy_result = [0.1, 0.2, 0.3] 
await redis_client.set(result_key, json.dumps(dummy_result))
```

---

## 4. `test_deployment.py` (根目录)

用于在 Docker 构建完成后执行自动化学验。

```python
# 74-106行: 执行多量级测试
def test_softmax_scale(stub, size, name):
    # 生成随机向量
    vector = [random.uniform(-10, 10) for _ in range(size)]
    # 提交并等待
    response = stub.SubmitTask(pb.TaskRequest(job_id=f"test-{size}", task="softmax", vector=vector))
    # 轮询获取结果...
    # 验证 Softmax 特性: 所有元素应大于 0 且总和为 1
    total_sum = sum(response.result)
    assert abs(total_sum - 1.0) < 1e-5
```
