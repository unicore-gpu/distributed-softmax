# 分布式 Softmax 服务部署指南

## 🚀 快速启动 (虚拟机直接构建)

这种方案适用于你已经在虚拟机上同步了代码的情况，是最推荐的部署方式。

### 1. 启动服务

进入项目根目录，使用 Docker Compose 一键构建并启动：

```bash
# 进入项目目录
cd ~/workspace/ben/new-distributed-softmax

# 停止并清理旧容器（如有）
docker compose down

# 构建并后台运行
docker compose up -d --build
```

### 2. 验证部署结果

使用容器内置的专业测试工具进行功能验证：

```bash
# 运行完整测试用例（包括小、中、大向量测试）
docker exec vector-service python3 /app/test_deployment.py
```

**期望输出**:
```text
🧪 测试分布式 Softmax 服务
✅ 连接成功
[1/3] 小向量测试 ... ✅ 测试通过
[2/3] 中等向量测试 ... ✅ 测试通过
[3/3] 大向量测试 ... ✅ 测试通过
✅ 所有测试通过！
```

### 3. 查看 GPU 运行实况

确认计算是否确实在 GPU 上完成：

```bash
# 查看实时日志
docker compose logs -f
```

**重点观察**:
寻找到带有 `🚀 [GPU]` 的日志行。
- `🚀 [GPU] Computation successful using NCCL method` (高性能分布式模式)
- `⚡ [GPU] NCCL failed, using Basic CUDA method` (基础 GPU 模式)
- `🐢 [CPU] GPU methods failed, falling back to NumPy` (CPU 兜底模式)

---

## � 常用维护命令

| 任务 | 命令 |
| :--- | :--- |
| **查看服务状态** | `docker compose ps` |
| **查看实时日志** | `docker compose logs -f` |
| **进入容器调试** | `docker exec -it vector-service bash` |
| **重启所有服务** | `docker compose restart` |
| **查看 GPU 显存占用** | `docker exec vector-service nvidia-smi` |
| **清理全部资源** | `docker compose down` |

---

## 📦 离线部署方案 (本地构建 + 传输)

适用于虚拟机无法连接外网，或需要在多台机器间快速分发镜像的情况。

### 步骤 1: 本地导出
在**开发机**执行：
```bash
# 构建镜像
docker build -t distributed-softmax:latest .
# 导出镜像包
docker save distributed-softmax:latest | gzip > distributed-softmax.tar.gz
# 发送到虚拟机
rsync -avz --progress distributed-softmax.tar.gz ben@100.100.48.93:~/
```

### 步骤 2: 虚拟机加载
在**虚拟机**执行：
```bash
# 加载镜像
docker load < ~/distributed-softmax.tar.gz
# 使用 Compose 启动（它会自动识别已加载的镜像）
cd ~/workspace/ben/new-distributed-softmax
docker compose up -d
```

---

## 🚨 故障排除

### 1. 端口占用
如果报错 `port is already allocated`，执行：
```bash
sudo fuser -k 50051/tcp  # 杀死占用 gRPC 端口的进程
```

### 2. GPU 不可用
如果显卡没被识别，确认 NVIDIA 驱动已安装：
```bash
nvidia-smi
# 如果报错，请参考 nvidia-container-toolkit 安装文档
```

---

## 🔥 性能调优

你可以根据机器配置修改 `docker-compose.yml` 中的环境变量：
- `NUM_WORKERS`: Python Worker 数量（建议 = CPU 核心数）
- `WORKER_CONCURRENCY`: 每个 Worker 的并发处理能力
- `SLICE_SIZE`: 向量分片大小
