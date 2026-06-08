# Docker 部署与测试规程详细说明

本文档解析了系统的容器化定义，说明了在生产环境中如何配置资源与启动服务。

## 1. `docker-compose.yml` 配置深度解析

该文件编排了四个关键组件。

### A. GPU 资源预留 (Reservations)

```yaml
# 41-47行: 关键 GPU 挂载配置
deploy:
  resources:
    reservations:
      devices:
        - driver: nvidia # 指定使用 NVIDIA 驱动
          count: 1       # 允许访问的 GPU 数量
          capabilities: [gpu] # 启用 GPU 能力
```

### B. 性能调优参数：多进程软件 Worker

这里需要区分“软件 Worker 进程”与“硬件 GPU 资源”：

```yaml
# 28-31行: 并发优化
NUM_WORKERS=8          # 启动 8 个并行 Python 进程（软件 Worker）
WORKER_CONCURRENCY=200 # 每个进程内部允许的异步任务上限
```

### C. GPU 扩展性 (GPU Scaling)

**为什么需要 8 个 Python Worker？**
- **消除 I/O 等待**: 每个 Worker 在处理 NATS 消息或存取 Redis 时是有网络延迟的。如果只有一个 Worker，GPU 会在网络交互时处于闲置状态。
- **充分压榨 GPU**: 通过启动 8 个进程，系统可以确保在任何时刻都有任务准备好送入 GPU 进行计算。
- **多卡扩展性**: 在多 GPU 环境下（例如 `count: 2`），这些软件 Worker 会通过底层 CUDA 库自动分配或共享显卡资源。

---

## 2. `Dockerfile` 构建流程关键点

采用 **多阶段构建 (Multi-stage Build)**，第一阶段 (builder) 包含 2GB+ 的编译工具，第二阶段 (runtime) 仅包含 800MB 左右的运行环境。

### A. 编译第三方库

```dockerfile
# 53-66行: 核心库编译
# 编译 redis-plus-plus 以便 C++ 网关存取数据
# 编译 nats.c 以便 C++ 网关发布任务
cmake .. -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=/usr/local
make -j$(nproc) # 使用所有核心并行编译
```

### B. 容器启动脚本 (`start.sh`)

这是容器内部的 init 进程，确保了各组件的启动顺序。

```bash
# 186-216行: 内部启动顺序
1. 启动 Redis (带优化参数 --tcp-backlog 511)
2. 启动 NATS Server (配置文件带有 10MB 负载上限设置)
3. 启动 C++ Gateway (网关)
4. 运行一个 for 循环按 NUM_WORKERS 环境变量启动多个 Worker
```

---

## 3. 系统运行流程：以 `test_deployment.py` 为核心的逻辑追踪

通过追踪 `test_deployment.py` 的执行路径，可以清晰地观察到系统中各个模块是如何协同工作的。

### 🚀 执行流程分析：全链路逻辑追踪

*   **Step 1: 模块加载与契约实例化**
    *   **脚本行为**: `test_deployment.py` (15-23行) 导入 `pb` 和 `stubs` 模块。
    *   **技术链路**: 映射至 **[2_proto]** 章节 2 及 1-B。
    *   **核心逻辑**: 调用基于 `vector_service.proto` 编译出的 Python 类。这确保了客户端发送的 `TaskRequest` 数据结构与 C++ 服务器定义的内存布局完全一致。

*   **Step 2: 通道建立与服务绑定**
    *   **脚本行为**: `test_deployment.py` (56-62行) 连接 50051 端口。
    *   **技术链路**: 映射至 **[3_server]** 章节 3 及 **[6_deploy]** 章节 2-B。
    *   **核心逻辑**: 连接到 C++ 容器内部由 `ServerBuilder` 启动的监听地址。此时容器内的 `start.sh` 已确保网关进程和各底层服务（Redis/NATS）处于就绪状态。

*   **Step 3: 任务分发与异步扇出**
    *   **脚本行为**: `test_deployment.py` (79-83行) 调用 `SubmitTask` 接口。
    *   **技术链路**: 映射至 **[3_server]** 章节 1-A 及 **[5_worker]** 章节 2。
    *   **核心逻辑**: 网关触发 `gateway_server.cc` 中的切片逻辑，将单一庞大请求拆分为多个 JSON 消息，通过 NATS 队列广播给所有在线 Worker。

*   **Step 4: 计算执行与算子加速**
    *   **脚本行为**: Worker 进程被 NATS 消息触发执行计算。
    *   **技术链路**: 映射至 **[5_worker]** 章节 3 及 章节 5。
    *   **核心逻辑**: Worker 进程利用 `ctypes` 加载 `libsoftmax_cuda.so`。若环境具备 GPU，则执行高性能 CUDA 内核；否则自动回退至 CPU 端的 `softmax_numpy`，计算结果存入 Redis。

*   **Step 5: 状态查询与进度映射**
    *   **脚本行为**: `test_deployment.py` (93-102行) 循环轮询 `GetResult` 接口。
    *   **技术链路**: 映射至 **[1_client]** 章节 B。
    *   **核心逻辑**: 客户端的每轮查询都对应网关对 Redis 元数据的实时检索。文档 1 中描述的 `pending -> running` 转变正是通过 `completed_slices` 的动态增加实现的。

*   **Step 6: 数据归口与结果返回**
    *   **脚本行为**: `test_deployment.py` (94-95行) 识别到 `ready` 状态。
    *   **技术链路**: 映射至 **[3_server]** 章节 1-B 及 章节 2。
    *   **核心逻辑**: 当网关后台进程 `waitForAllSlices` 检测到 Redis 中所有分片结果均已集齐，它会将结果合成并通过 gRPC 回传。这体现了 `redis_manager.h` 中数据生命周期的终点。

*   **Step 7: 数学正确性与一致性校验**
    *   **脚本行为**: `test_deployment.py` (113-125行) 验证返回值总和。
    *   **技术链路**: 映射至 **[4_tests]** 章节 4。
    *   **核心逻辑**: 验证经过 分片->传输->计算->聚合 后的最终浮点数序列是否符合 Softmax 概率分布（总和应为 1.0），从而验证全链路计算的精确性。

---

## 4. 测试验证 (Test Procedure)

### 第一阶段：容器健康检查
```bash
# 查看健康状态（配置文件 49-54 行定义）
# 该检查会运行 redis-cli ping 并访问 NATS 监控接口
docker ps --filter "name=vector-service"
```

### 第二阶段：功能性验收测试
执行 `test_deployment.py`。该脚本由三部分组成：
1. **gRPC 通信校验**: `VectorServiceStub` 能否连接。
2. **切片完整性校验**: 提交足够大的向量（如 100 元素），确认网关能否拆分成 10 个切片并正确收集所有反馈。
3. **结果数学精度**: 验证 Softmax 结果是否满足 $\sum e_i = 1$ 的特性。

### 第三阶段：GPU 活性观察
在容器运行期间执行：
```bash
docker exec vector-service nvidia-smi
```
如果看到 `python3` 进程出现在显存占用列表中，说明 `libsoftmax_cuda.so` 已被正确加载并利用 GPU 资源。
