# 渐进式迁移路线图

## 📍 当前架构（起点）

### 现状：单节点单容器架构

当前系统运行在**单台机器上的单个 Docker 容器**中，所有组件都在同一个容器内：

```
┌─────────────────────────────────────────┐
│  单台机器 / 单个容器                      │
│  ┌──────────┐  ┌──────────┐            │
│  │ Gateway  │  │   NATS   │            │
│  │ (Coord)  │  │ (Coord)  │            │
│  └──────────┘  └──────────┘            │
│  ┌──────────┐  ┌──────────┐            │
│  │  Redis   │  │ Workers  │            │
│  │ (Coord)  │  │ (多个)   │            │
│  └──────────┘  └──────────┘            │
│      所有组件都在同一台机器               │
└─────────────────────────────────────────┘
```

### 当前架构特点

- ✅ **单容器部署**：所有服务（Gateway, NATS, Redis, Workers）在一个容器内
- ✅ **本地通信**：所有组件通过 `localhost` 通信
- ✅ **硬编码配置**：NATS 和 Redis 地址硬编码为 `localhost`
- ✅ **简单部署**：一个 `docker-compose.yml` 或 `docker run` 命令即可启动
- ❌ **无法扩展**：无法利用多节点 GPU 资源
- ❌ **资源竞争**：所有组件共享同一台机器的资源

### 当前启动方式

```bash
# 单个容器启动所有服务
docker run -d \
  --name distributed-softmax \
  --gpus all \
  -p 50051:50051 -p 4222:4222 -p 6379:6379 \
  distributed-softmax:latest
# 容器内自动启动：Redis + NATS + Gateway + Workers
```

### 迁移路径总览

```
┌─────────────────────────────────────────┐
│  阶段0: 当前架构（起点）                  │
│  单台机器 / 单个容器                      │
│  ┌──────────┐  ┌──────────┐            │
│  │ Gateway  │  │   NATS   │            │
│  └──────────┘  └──────────┘            │
│  ┌──────────┐  ┌──────────┐            │
│  │  Redis   │  │ Workers  │            │
│  └──────────┘  └──────────┘            │
└─────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────┐
│  阶段1: 代码修改                         │
│  支持环境变量配置                        │
│  - 保持向后兼容（默认 localhost）        │
│  - 支持多节点配置（环境变量）            │
└─────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────┐
│  阶段2: 方案A（简化分离）                 │
│  Coordinator 节点:                       │
│  └─> 1个容器 (Gateway + NATS + Redis)   │
│                                          │
│  Worker 节点:                           │
│  └─> 每个节点 1个容器 (Worker)          │
└─────────────────────────────────────────┘
              ↓
┌─────────────────────────────────────────┐
│  阶段3: 方案B（完全分离）                 │
│  Coordinator 节点:                       │
│  ├─> Gateway 容器                       │
│  ├─> NATS 容器                          │
│  └─> Redis 容器                          │
│                                          │
│  Worker 节点:                           │
│  └─> Worker 容器                        │
└─────────────────────────────────────────┘
```

---

## 🎯 迁移策略：方案A → 方案B

### 目标架构

**阶段2（方案A）**：简化分离（第一步）
```
Coordinator 节点:
  └─> 1个容器 (Gateway + NATS + Redis)

Worker 节点:
  └─> 每个节点 1个容器 (Worker)
```

**阶段3（方案B）**：完全分离（第二步，生产级）
```
Coordinator 节点:
  ├─> Gateway 容器
  ├─> NATS 容器
  └─> Redis 容器

Worker 节点:
  └─> Worker 容器
```

### 为什么这个策略好？

1. ✅ **低风险**：先验证多节点架构，再优化
2. ✅ **平滑过渡**：方案A的代码可以直接用于方案B
3. ✅ **快速上线**：方案A可以快速部署，方案B可以后续优化
4. ✅ **学习曲线**：逐步理解分布式架构

---

## 📋 迁移路线图

### 阶段 1: 代码修改（支持两种方案）⭐

**目标**: 修改代码支持环境变量，兼容单节点和多节点

**修改内容**:
- ✅ `worker/main.py` - 环境变量 + Queue Group
- ✅ `server/nats_publisher.h` - 环境变量
- ✅ `server/redis_manager.h` - 环境变量
- ✅ `server/gateway_server.cc` - 环境变量

**结果**: 
- 代码支持环境变量配置
- 默认值保持 `localhost`（向后兼容）
- **可以同时支持方案A和方案B**

---

### 阶段 2: 实现方案A（简化分离）⭐

**目标**: Coordinator 1个容器，Worker 每个节点1个容器

**新增内容**:
- ✅ `scripts/start-coordinator.sh` - Coordinator 启动脚本
- ✅ `docker-compose.coordinator.yml` - Coordinator 部署配置
- ✅ `docker-compose.worker.yml` - Worker 部署配置（模板）
- ✅ 更新 `Dockerfile` - 支持不同启动命令

**架构**:
```
Coordinator 节点:
  └─> 1个容器 (Gateway + NATS + Redis)

Worker 节点:
  └─> 每个节点 1个容器 (Worker)
```

**验证**:
- ✅ 测试 Coordinator 和 Worker 分离部署
- ✅ 验证多节点通信
- ✅ 验证负载均衡

---

### 阶段 3: 升级到方案B（完全分离）⭐

**目标**: 每个组件独立容器

**新增内容**:
- ✅ `docker-compose.full.yml` - 完全分离配置
- ✅ 可选：分离 Dockerfile（Gateway 专用、Worker 专用）

**架构**:
```
Coordinator 节点:
  ├─> Gateway 容器
  ├─> NATS 容器
  └─> Redis 容器

Worker 节点:
  └─> Worker 容器
```

**优势**:
- ✅ 更好的资源隔离
- ✅ 独立扩展每个组件
- ✅ 生产环境最佳实践

---

## 🔄 从方案A升级到方案B的步骤

### 关键点：代码不需要修改！

因为我们在阶段1已经做了环境变量支持，所以：
- ✅ **方案A和方案B使用相同的代码**
- ✅ **只需要改变 Docker 部署配置**
- ✅ **平滑升级，无需代码改动**

### 升级步骤

#### 步骤 1: 停止方案A部署
```bash
# Coordinator 节点
docker stop coordinator
docker rm coordinator

# Worker 节点（保持不变，可以继续使用）
# Worker 容器不需要修改
```

#### 步骤 2: 部署方案B
```bash
# Coordinator 节点 - 使用新的 docker-compose
docker-compose -f docker-compose.full.yml up -d

# Worker 节点 - 保持不变
# 只需要更新环境变量指向新的服务名
```

#### 步骤 3: 更新 Worker 环境变量
```bash
# 方案A: 指向 Coordinator IP
NATS_URL=nats://coordinator-ip:4222
REDIS_HOST=coordinator-ip

# 方案B: 指向服务名（如果在同一 Docker 网络）
NATS_URL=nats://nats-service:4222
REDIS_HOST=redis-service

# 或者继续使用 IP（如果跨节点）
NATS_URL=nats://coordinator-ip:4222
REDIS_HOST=coordinator-ip
```

---

## 📝 具体实施计划

### 阶段 1: 代码修改（现在开始）

#### 1.1 修改 Worker (`worker/main.py`)
```python
import os

async def main():
    # 从环境变量读取，默认 localhost（向后兼容）
    redis_host = os.getenv("REDIS_HOST", "localhost")
    redis_port = int(os.getenv("REDIS_PORT", "6379"))
    nats_url = os.getenv("NATS_URL", "nats://localhost:4222")
    queue_name = os.getenv("NATS_QUEUE_NAME", "softmax-workers")
    
    redis = Redis(host=redis_host, port=redis_port)
    setup_metrics()

    nc = NATS()
    await nc.connect(nats_url)

    async def message_handler(msg):
        await handle_task_message(msg.data, redis)

    # Queue Group 支持多节点负载均衡
    await nc.subscribe("task_queue", queue=queue_name, cb=message_handler)
    
    print(f"🎯 Worker listening on task_queue (queue: {queue_name})")
    print(f"📡 NATS: {nats_url}")
    print(f"💾 Redis: {redis_host}:{redis_port}")

    try:
        while True:
            await asyncio.sleep(3600)
    except KeyboardInterrupt:
        print("🛑 Shutting down worker...")
        await nc.close()
```

#### 1.2 修改 NATS Publisher (`server/nats_publisher.h`)
```cpp
#include <cstdlib>
#include <string>

class NatsPublisher {
public:
    NatsPublisher() : conn_(nullptr), connected_(false) {
        // 从环境变量读取 NATS URL
        const char* nats_url_env = std::getenv("NATS_URL");
        std::string nats_url = nats_url_env ? nats_url_env : "nats://localhost:4222";
        
        std::cout << "🔧 Initializing NATS Publisher..." << std::endl;
        std::cout << "📡 Connecting to: " << nats_url << std::endl;
        
        // ... 使用 nats_url 连接
    }
};
```

#### 1.3 修改 Redis Manager (`server/redis_manager.h`)
```cpp
#include <cstdlib>
#include <string>

class RedisManager {
private:
    static std::string getRedisConnectionString() {
        const char* redis_host = std::getenv("REDIS_HOST");
        const char* redis_port = std::getenv("REDIS_PORT");
        const char* redis_password = std::getenv("REDIS_PASSWORD");
        
        std::string host = redis_host ? redis_host : "127.0.0.1";
        std::string port = redis_port ? redis_port : "6379";
        
        std::string conn_str = "tcp://";
        if (redis_password) {
            conn_str += std::string(":") + redis_password + "@";
        }
        conn_str += host + ":" + port;
        
        return conn_str;
    }
    
public:
    RedisManager() : redis_(getRedisConnectionString()) {
        std::cout << "🔗 Redis: " << getRedisConnectionString() << std::endl;
    }
    
    RedisManager(const std::string& connection_string) 
        : redis_(connection_string) {}
    // ... 其余代码
};
```

---

### 阶段 2: 实现方案A（简化分离）

#### 2.1 创建 Coordinator 启动脚本

**文件**: `scripts/start-coordinator.sh`
```bash
#!/bin/bash
set -e

echo "🚀 Starting Coordinator services..."

# 启动 Redis
echo "📦 Starting Redis..."
redis-server --daemonize yes \
  --maxclients 10000 \
  --timeout 0 \
  --tcp-backlog 511 \
  --save ""

sleep 2

# 启动 NATS
echo "📨 Starting NATS..."
nats-server -c /etc/nats/nats-server.conf &

sleep 2

# 启动 Gateway
echo "🚪 Starting Gateway..."
/app/build/gateway_server &

echo "✅ Coordinator services started"
echo "📊 Gateway: 0.0.0.0:50051"
echo "📨 NATS: 0.0.0.0:4222"
echo "💾 Redis: 0.0.0.0:6379"

# 保持容器运行
wait
```

#### 2.2 更新 Dockerfile 支持不同启动方式

**修改**: `Dockerfile`
```dockerfile
# ... 现有构建步骤 ...

# 复制启动脚本
COPY scripts/ /app/scripts/
RUN chmod +x /app/scripts/*.sh

# 默认命令（保持向后兼容，单节点部署）
CMD ["/app/start.sh"]

# 但可以通过命令覆盖：
# docker run ... /app/scripts/start-coordinator.sh
# docker run ... python3 /app/worker/main.py
```

#### 2.3 创建方案A的 Docker Compose

**文件**: `docker-compose.coordinator.yml`
```yaml
version: '3.8'

services:
  coordinator:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: softmax-coordinator
    ports:
      - "50051:50051"  # gRPC Gateway
      - "4222:4222"    # NATS
      - "8222:8222"    # NATS Monitor
      - "6379:6379"    # Redis
    volumes:
      - nats-data:/var/lib/nats
      - redis-data:/var/lib/redis
    environment:
      - NATS_URL=nats://localhost:4222
      - REDIS_HOST=localhost
      - REDIS_PORT=6379
    command: ["/app/scripts/start-coordinator.sh"]
    restart: unless-stopped
    # 不需要 GPU

volumes:
  nats-data:
  redis-data:
```

**文件**: `docker-compose.worker.yml` (模板)
```yaml
version: '3.8'

services:
  worker:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: softmax-worker
    runtime: nvidia
    environment:
      - NATS_URL=nats://COORDINATOR_IP:4222  # 替换为实际 IP
      - REDIS_HOST=COORDINATOR_IP            # 替换为实际 IP
      - REDIS_PORT=6379
      - NATS_QUEUE_NAME=softmax-workers
    command: ["python3", "/app/worker/main.py"]
    deploy:
      resources:
        reservations:
          devices:
            - driver: nvidia
              count: 1
              capabilities: [gpu]
    restart: unless-stopped
```

#### 2.4 部署方案A

```bash
# Coordinator 节点
docker-compose -f docker-compose.coordinator.yml up -d

# Worker 节点 1
docker run -d \
  --name worker1 \
  --gpus all \
  -e NATS_URL=nats://COORDINATOR_IP:4222 \
  -e REDIS_HOST=COORDINATOR_IP \
  -e NATS_QUEUE_NAME=softmax-workers \
  distributed-softmax:latest \
  python3 /app/worker/main.py

# Worker 节点 2
docker run -d \
  --name worker2 \
  --gpus all \
  -e NATS_URL=nats://COORDINATOR_IP:4222 \
  -e REDIS_HOST=COORDINATOR_IP \
  -e NATS_QUEUE_NAME=softmax-workers \
  distributed-softmax:latest \
  python3 /app/worker/main.py
```

---

### 阶段 3: 升级到方案B（完全分离）

#### 3.1 创建完全分离的 Docker Compose

**文件**: `docker-compose.full.yml`
```yaml
version: '3.8'

services:
  # Gateway 独立容器
  gateway:
    build:
      context: .
      dockerfile: Dockerfile
    container_name: softmax-gateway
    ports:
      - "50051:50051"
    environment:
      - NATS_URL=nats://nats:4222
      - REDIS_HOST=redis
      - REDIS_PORT=6379
    command: ["/app/build/gateway_server"]
    depends_on:
      - nats
      - redis
    restart: unless-stopped

  # NATS 独立容器
  nats:
    image: nats:2.10.7
    container_name: softmax-nats
    ports:
      - "4222:4222"
      - "8222:8222"
    volumes:
      - nats-data:/var/lib/nats
    command: ["-js", "-sd", "/var/lib/nats"]
    restart: unless-stopped

  # Redis 独立容器
  redis:
    image: redis:7-alpine
    container_name: softmax-redis
    ports:
      - "6379:6379"
    volumes:
      - redis-data:/data
    command: redis-server --appendonly yes
    restart: unless-stopped

volumes:
  nats-data:
  redis-data:
```

#### 3.2 升级步骤

```bash
# 1. 停止方案A的 Coordinator
docker stop softmax-coordinator
docker rm softmax-coordinator

# 2. 启动方案B的 Coordinator 组件
docker-compose -f docker-compose.full.yml up -d

# 3. Worker 节点 - 更新环境变量（如果使用服务名）
# 如果在同一 Docker 网络，可以使用服务名
# 如果跨节点，继续使用 IP 地址

# 4. 验证
docker ps
curl http://localhost:8222/varz  # NATS
redis-cli -h localhost ping      # Redis
```

---

## ✅ 迁移检查清单

### 阶段 1: 代码修改
- [ ] 修改 `worker/main.py` 支持环境变量
- [ ] 修改 `server/nats_publisher.h` 支持环境变量
- [ ] 修改 `server/redis_manager.h` 支持环境变量
- [ ] 修改 `server/gateway_server.cc` 支持环境变量
- [ ] 测试：单节点部署仍然工作（向后兼容）

### 阶段 2: 方案A部署
- [ ] 创建 `scripts/start-coordinator.sh`
- [ ] 创建 `docker-compose.coordinator.yml`
- [ ] 创建 `docker-compose.worker.yml` (模板)
- [ ] 更新 `Dockerfile` 支持不同启动命令
- [ ] 部署 Coordinator 节点
- [ ] 部署 Worker 节点 1
- [ ] 部署 Worker 节点 2
- [ ] 测试：多节点通信和负载均衡

### 阶段 3: 升级到方案B
- [ ] 创建 `docker-compose.full.yml`
- [ ] 停止方案A Coordinator
- [ ] 启动方案B组件（Gateway, NATS, Redis）
- [ ] 更新 Worker 环境变量（如果需要）
- [ ] 测试：验证完全分离部署
- [ ] 性能测试：对比方案A和方案B

---

## 🎯 总结

### 迁移路径
```
当前架构 (单节点)
    ↓
阶段1: 代码修改（支持环境变量）
    ↓
阶段2: 方案A（简化分离）
    ↓
阶段3: 方案B（完全分离）
```

### 关键优势
1. ✅ **代码一次修改**：阶段1的代码修改同时支持方案A和方案B
2. ✅ **平滑升级**：从方案A到方案B只需要改变部署配置
3. ✅ **低风险**：每个阶段都可以独立测试和验证
4. ✅ **向后兼容**：每个阶段都保持向后兼容

### 下一步
1. **现在开始阶段1**：修改代码支持环境变量
2. **然后阶段2**：实现方案A部署（简化分离）
3. **最后阶段3**：升级到方案B（完全分离，可选）

需要我开始实施阶段1的代码修改吗？

