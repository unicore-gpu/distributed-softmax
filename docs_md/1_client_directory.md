# Client 目录详细文档

该目录包含与分布式 Softmax 服务交互的 Python 客户端实现，主要用于任务提交、状态监控和结果获取。

## 1. `client.py` 核心解析

该脚本实现了完整的 gRPC 客户端流程，包括异步任务提交和状态机轮询。

### A. 任务提交逻辑

```python
# 10-25行: 构造并发送任务
def submit_and_track_job(stub, input_vector, task_type="softmax"):
    # 生成唯一作业 ID，前缀为 job- 加上 8 位随机十六进制字符串
    job_id = f"job-{uuid.uuid4().hex[:8]}"
    
    # 构造 gRPC TaskRequest 消息
    submit_request = pb.TaskRequest(
        job_id=job_id,      # 唯一标识，后续查询结果必须提供
        task=task_type,    # 任务类型，当前服务端仅处理 "softmax"
        vector=input_vector # 原始浮点数向量
    )
    
    # 调用 Stub 发送请求到网关
    submit_response = stub.SubmitTask(submit_request)
```

**参数说明**:
- `stub`: 已连接到网关 (localhost:50051) 的 gRPC 存根对象。
- `input_vector`: 一个包含浮点数的 Python 列表。
- `job_id`: 字符串，系统追踪该任务的唯一凭证。

---

### B. 结果轮询与状态机

由于任务是异步处理的（分片后发往 NATS），客户端需要循环查询结果。

```python
# 43-75行: 轮询 GetResult 接口
result_request = pb.ResultRequest(job_id=job_id)
max_attempts = 30 # 最多轮询 30 次（约 30 秒）

for attempt in range(max_attempts):
    result_response = stub.GetResult(result_request)
    status = result_response.status # 获取当前状态
    
    # 进度信息：已完成切片数 / 总切片数
    progress = f"{result_response.completed_slices}/{result_response.total_slices}"
    
    if status == "ready":
        # 任务完成，result_response.result 包含最终聚合后的向量
        print(f"Final result: {result_response.result[:10]}...")
        return result_response
        
    elif status == "failed":
        print(f"❌ Job failed: {result_response.message}")
        return None
    
    time.sleep(1) # 每秒轮询一次
```

**状态说明 (`status`)**:
- `pending`: 任务已在 Redis 中注册元数据，但尚未有 Worker 开始处理或切片尚未入库。
- `running`: 至少有一个切片已在 Redis 中完成计算并被网关识别。
- `ready`: 网关已成功聚合所有切片，并将最终结果存入 Redis。
- `failed`: 聚合过程或 Worker 计算过程中发生错误。
- `not_found`: 任务 ID 无效或因为 TTL 到期已被 Redis 自动删除。

---

### C. 主流程控制

```python
# 144-164行: 程序入口
if __name__ == "__main__":
    # 1. 建立非安全连接通道
    with grpc.insecure_channel("localhost:50051") as channel:
        # 2. 等待通道就绪 (Ready)，超时 5 秒
        future = grpc.channel_ready_future(channel)
        future.result(timeout=5)
        
        # 3. 运行测试套件
        main()
```
