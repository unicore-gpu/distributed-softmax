# 快速开始指南

## 🚀 一键部署

### 方法 1: 使用自动化脚本（推荐）

```bash
# 确保 Docker 正在运行
docker ps

# 执行自动部署脚本
./deploy.sh
```

脚本会自动完成：
1. ✅ 构建优化的 Docker 镜像
2. ✅ 导出镜像文件
3. ✅ 传输到虚拟机 (ben@100.100.48.93)
4. ✅ 在虚拟机上加载并运行
5. ✅ 测试服务连接

**预计总时间**: 15-25 分钟

---

### 方法 2: 手动部署

#### 步骤 1: 构建镜像（本地）

```bash
docker build -t distributed-softmax:latest .
```

#### 步骤 2: 导出镜像（本地）

```bash
docker save distributed-softmax:latest | gzip > distributed-softmax.tar.gz
```

#### 步骤 3: 传输到虚拟机（本地）

```bash
rsync -avz --progress distributed-softmax.tar.gz ben@100.100.48.93:~/
```

#### 步骤 4: 部署到虚拟机

```bash
# SSH 到虚拟机
ssh ben@100.100.48.93

# 加载镜像
docker load < distributed-softmax.tar.gz

# 停止旧容器（如果存在）
docker stop distributed-softmax 2>/dev/null || true
docker rm distributed-softmax 2>/dev/null || true

# 创建数据卷
docker volume create nats-data
docker volume create redis-data
docker volume create logs

# 运行容器
docker run -d \
  --name distributed-softmax \
  --gpus all \
  --restart unless-stopped \
  -p 50051:50051 \
  -p 4222:4222 \
  -p 8222:8222 \
  -p 6379:6379 \
  -p 8000:8000 \
  -v nats-data:/var/lib/nats \
  -v redis-data:/var/lib/redis \
  -v logs:/var/log \
  -e NUM_WORKERS=8 \
  -e WORKER_CONCURRENCY=200 \
  distributed-softmax:latest

# 查看日志
docker logs -f distributed-softmax
```

---

## 🧪 测试部署

### 从本地测试虚拟机服务

```bash
# 测试虚拟机上的服务
./test_deployment.py --host 100.100.48.93

# 或手动测试
python3 test_deployment.py --host 100.100.48.93 --port 50051
```

### 在虚拟机上测试

```bash
ssh ben@100.100.48.93

# 方法 1: 在容器内测试
docker exec -it distributed-softmax python3 /app/client/client.py

# 方法 2: 快速测试
docker exec distributed-softmax redis-cli ping
curl http://localhost:8222/varz
```

---

## 📊 监控服务

### 查看运行状态

```bash
ssh ben@100.100.48.93

# 容器状态
docker ps | grep distributed-softmax

# 实时日志
docker logs -f distributed-softmax

# 资源使用
docker stats distributed-softmax

# GPU 使用
docker exec distributed-softmax nvidia-smi
```

### 服务端点

| 服务 | 地址 | 说明 |
|------|------|------|
| gRPC Gateway | `100.100.48.93:50051` | 主要 API 入口 |
| NATS Server | `100.100.48.93:4222` | 消息队列 |
| NATS Monitor | `http://100.100.48.93:8222` | NATS 监控面板 |
| Redis | `100.100.48.93:6379` | 缓存服务 |
| Prometheus | `http://100.100.48.93:8000/metrics` | 性能指标 |

### 查看指标

```bash
# NATS 统计
curl -s http://100.100.48.93:8222/varz | python3 -m json.tool

# Prometheus 指标
curl http://100.100.48.93:8000/metrics

# Redis 信息
ssh ben@100.100.48.93 "docker exec distributed-softmax redis-cli INFO stats"
```

---

## ⚙️ 配置调优

### 调整 Worker 数量

根据虚拟机的 CPU 核心数调整：

```bash
# 停止容器
ssh ben@100.100.48.93 "docker stop distributed-softmax && docker rm distributed-softmax"

# 使用更多 workers 重启（例如 16 个）
ssh ben@100.100.48.93 << 'EOF'
docker run -d \
  --name distributed-softmax \
  --gpus all \
  --restart unless-stopped \
  -p 50051:50051 -p 4222:4222 -p 8222:8222 -p 6379:6379 -p 8000:8000 \
  -v nats-data:/var/lib/nats -v redis-data:/var/lib/redis -v logs:/var/log \
  -e NUM_WORKERS=16 \
  -e WORKER_CONCURRENCY=500 \
  -e SLICE_SIZE=20 \
  distributed-softmax:latest
EOF
```

### 环境变量说明

| 变量 | 默认值 | 推荐范围 | 说明 |
|------|--------|----------|------|
| `NUM_WORKERS` | 4 | 4-32 | Python worker 进程数（建议=CPU核心数） |
| `WORKER_CONCURRENCY` | 100 | 100-1000 | 每个 worker 的并发任务数 |
| `SLICE_SIZE` | 10 | 10-100 | 向量切片大小 |
| `NCCL_DEBUG` | WARN | INFO/WARN/ERROR | GPU 通信日志级别 |

---

## 🔧 常用命令

### 日常维护

```bash
# 重启服务
ssh ben@100.100.48.93 "docker restart distributed-softmax"

# 查看最近 100 行日志
ssh ben@100.100.48.93 "docker logs --tail 100 distributed-softmax"

# 进入容器调试
ssh ben@100.100.48.93 "docker exec -it distributed-softmax bash"

# 查看所有容器
ssh ben@100.100.48.93 "docker ps -a"
```

### 更新服务

当代码更新后：

```bash
# 重新运行部署脚本
./deploy.sh
```

### 备份数据

```bash
ssh ben@100.100.48.93 << 'EOF'
# 备份 NATS 数据
docker run --rm \
  -v nats-data:/data \
  -v $(pwd):/backup \
  ubuntu tar czf /backup/nats-backup-$(date +%Y%m%d).tar.gz /data

# 备份 Redis 数据
docker run --rm \
  -v redis-data:/data \
  -v $(pwd):/backup \
  ubuntu tar czf /backup/redis-backup-$(date +%Y%m%d).tar.gz /data
EOF
```

### 清理资源

```bash
ssh ben@100.100.48.93 << 'EOF'
# 停止并删除容器
docker stop distributed-softmax
docker rm distributed-softmax

# 删除镜像
docker rmi distributed-softmax:latest

# 清理未使用的资源
docker system prune -a
EOF
```

---

## 🐛 故障排除

### 问题 1: 构建失败

```bash
# 检查 Docker 是否运行
docker ps

# 清理 Docker 缓存后重试
docker builder prune -a
docker build --no-cache -t distributed-softmax:latest .
```

### 问题 2: 容器无法启动

```bash
# 查看详细错误
ssh ben@100.100.48.93 "docker logs distributed-softmax"

# 检查端口占用
ssh ben@100.100.48.93 "sudo netstat -tulpn | grep -E '50051|4222|6379'"
```

### 问题 3: GPU 不可用

```bash
# 检查 GPU
ssh ben@100.100.48.93 "nvidia-smi"

# 检查 Docker GPU 支持
ssh ben@100.100.48.93 "docker run --rm --gpus all nvidia/cuda:12.6.1-base-ubuntu22.04 nvidia-smi"

# 重启 Docker
ssh ben@100.100.48.93 "sudo systemctl restart docker"
```

### 问题 4: 性能不佳

```bash
# 增加 worker 数量
# 编辑 deploy.sh，修改 NUM_WORKERS=16
./deploy.sh

# 或手动调整（见"配置调优"部分）
```

---

## 📚 相关文档

- [DEPLOYMENT_GUIDE.md](DEPLOYMENT_GUIDE.md) - 完整部署文档
- [README.md](README.md) - 项目说明
- [DOCKER_README.md](DOCKER_README.md) - Docker 使用说明

---

## 🆘 快速帮助

```bash
# 查看所有服务状态
ssh ben@100.100.48.93 "docker ps && docker stats distributed-softmax --no-stream"

# 测试服务是否正常
./test_deployment.py --host 100.100.48.93

# 查看实时日志
ssh ben@100.100.48.93 "docker logs -f distributed-softmax"
```

---

## ✅ 检查清单

部署前确认：
- [ ] Docker 已安装并运行
- [ ] 可以 SSH 到虚拟机 `ben@100.100.48.93`
- [ ] 虚拟机已安装 Docker 和 NVIDIA Container Toolkit
- [ ] 虚拟机有足够磁盘空间（至少 10GB）

部署后验证：
- [ ] 容器正在运行 (`docker ps`)
- [ ] 日志无错误 (`docker logs`)
- [ ] 测试通过 (`./test_deployment.py --host 100.100.48.93`)
- [ ] GPU 可访问 (`nvidia-smi`)
- [ ] 所有端口正常监听

---

**需要帮助？** 查看详细日志：
```bash
ssh ben@100.100.48.93 "docker logs -f distributed-softmax"
```
