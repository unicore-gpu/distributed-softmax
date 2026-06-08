# Worker 目录详细文档

Worker 是系统的计算引擎。通过 Python 实现，它结合了高性能的 CUDA 算子与易于扩展的 NATS 消费者逻辑。

## 1. `softmax.py` - 高性能计算核心

这是整个项目最复杂的部分，实现了三种不同级别的计算降级方案。

### A. GPU 方法探测与加载

```python
# 19-65行: 使用 ctypes 加载动态库 (libsoftmax_cuda.so)
def _load_cuda_lib():
    for lib_path in CUDA_LIB_PATHS:
        if os.path.isfile(lib_path):
            lib = ctypes.CDLL(lib_path)
            # 配置 C 接口的参数类型 (input, output, size)
            lib.run_softmax_basic.argtypes = [
                ctypes.POINTER(ctypes.c_float), 
                ctypes.POINTER(ctypes.c_float), 
                ctypes.c_int
            ]
            return lib
```

### B. 阶梯式计算策略 (Fall-through)

```python
# 99-112行: 智能选择计算模式
if method == "auto":
    try:
        # 1. 优先尝试 NCCL (支持多卡并行的最新算子)
        return _run_cuda_method(array, "nccl")
    except Exception:
        try:
            # 2. 如果 NCCL 报错，切回 Basic CUDA 
            return _run_cuda_method(array, "basic")
        except Exception:
            # 3. 如果没 GPU 或 CUDA 环境损坏，使用 Python NumPy 兜底
            return _softmax_numpy(array).tolist()
```

---

## 2. `handler.py` - 消息分发与任务调度

Worker 并不直接对接客户端，而是从消息队列中领取分片任务。`handler.py` 在此充当了核心的逻辑调度层。

### A. 设计模式解析：什么是 Handler？

在软件架构中，**Handler（处理器）** 承担着“调度员”或“中介者”的角色。它的主要职责是实现 **计算逻辑** 与 **网络/存储逻辑** 的解耦。

*   **职责范围**：
    1.  **解析 (Parsing)**：解码来自 NATS 队列的原始 JSON 字节流。
    2.  **分发 (Dispatching)**：识别任务类型并调用对应的核心算子（如 `softmax.py`）。
    3.  **持久化 (Persistence)**：负责将分片计算结果写入 Redis。
    4.  **监控 (Monitoring)**：触发 Prometheus 耗时统计与计数。

*   **为什么要专门建立 handler.py？**
    通过独立出 Handler 层，底层的数学计算库（如 `softmax.py`）可以保持纯净，它不关心消息是从 NATS 还是其他渠道来的，也不关心结果去往哪里，这极大提高了代码的可维护性和测试性。

### B. 核心代码行为分析

```python
# 8-37行: 处理 NATS 解析后的消息
async def handle_task_message(data_bytes, redis):
    message = json.loads(data_bytes.decode())
    job_id = message["job_id"]
    slice_id = message["slice_id"]
    vector_data = message["data"] # 这里只包含 10 个左右的浮点数

    # 计算该分片的局部 Softmax (或中间 exp 值)
    result = softmax(vector_data)

    # 结果存回 Redis，Key 格式非常重要，网关指望这个 Key 进行聚合
    # Key: result:job-{id}:{slice_id}
    result_key = f"result:{job_id}:{slice_id}"
    await redis.set(result_key, json.dumps(result))
```

---

## 3. `main.py` - Worker 并发模型

Worker 采用了异步协程模型，可以同时开启多个实例提高吞吐。

```python
# 11-17行: NATS 订阅逻辑
async def main():
    nc = NATS()
    await nc.connect("nats://localhost:4222")

    async def message_handler(msg):
        # 消息一到，立即调用 handler 进行计算
        await handle_task_message(msg.data, redis)

    # 订阅 task_queue 频道
    await nc.subscribe("task_queue", cb=message_handler)
```

---

## 4. `metrics.py` - 监控采集

Worker 暴露了 Prometheus 指标，你可以通过 `localhost:8000/metrics` 查看。

- `task_counter`: 记录处理成功和失败的任务数。
- `task_duration`: 记录 GPU/CPU 计算实际耗时。

---

## 5. 多进程与 GPU 资源分配 (Scaling)

在实际部署中，你会发现系统启动了多个 Worker 进程（由 `NUM_WORKERS` 决定）。

### 为什么不只用一个进程？
1.  **并发处理能力**: 一个 Python 进程受到 GIL (全局解释器锁) 的限制，且在执行 Redis/NATS 等 I/O 操作时会阻塞。多进程可以并行处理不同的任务分片。
2.  **GPU 饱和度**: GPU 的计算速度远快于数据传输速度。多进程可以保持 GPU 任务队列始终处于“填满”状态，避免计算资源的浪费。
3.  **多卡利用**: 当宿主机有多个 GPU 时，不同的 Worker 进程可以被分配到不同的显卡上（或通过 CUDA 的负载均衡共同使用某几张卡）。
