# 方案 A：多节点分布式 Docker 部署指南

本指南介绍如何通过 Docker 容器化方式实施 **方案 A**，将系统的 **Coordinator (调度中心)** 和 **Workers (计算节点)** 部署在不同的机器上。

## 1. 架构说明

-   **Coordinator 节点**: 运行 Redis, NATS 和 Gateway。
-   **Worker 节点**: 运行多个 Python 计算实例，连接远程 Coordinator。

---

## 2. 部署步骤

### 第一步：启动 Coordinator (调度机)

1.  在调度机上，使用以下命令启动服务：
    ```bash
    docker-compose -f docker-compose.coordinator.yml up -d
    ```
2.  **获取 IP**：记录这台调度机的内网 IP（例如 `192.168.1.100`）。

### 第二步：启动 Workers (计算机)

1.  在每一台拥有 GPU 的计算机上，修改 `docker-compose.worker.yml`。
2.  将环境变量中的 `COORDINATOR_IP` 替换为第一步获取的 IP：
    ```yaml
    environment:
      - NATS_URL=nats://192.168.1.100:4222
      - REDIS_HOST=192.168.1.100
      - NUM_WORKERS=8  # 设置该机器要启动的 Worker 进程数
    ```
3.  运行启动命令：
    ```bash
    docker-compose -f docker-compose.worker.yml up -d
    ```

---

## 3. 测试与验证

### 3.1 检查服务日志
在执行测试前，请确保服务已正常启动：
-   **调度中心**：`docker logs -f softmax-coordinator`
-   **计算节点**：`docker logs -f softmax-worker`

### 3.2 运行功能测试脚本
你可以选择在宿主机上运行，或者在已有的 Docker 容器内运行测试。

#### 方法 A：在宿主机运行（推荐）
确保你的宿主机安装了 `grpcio` 和 `protobuf`：
```bash
# 修改 IP 指向你的调度机
python3 test_deployment.py --host 192.168.1.100
```

#### 方法 B：在容器内运行
直接利用已经搭建好的容器环境进行测试：
```bash
# 进入调度机容器并执行
docker exec -it softmax-coordinator python3 /app/test_deployment.py --host localhost
```

---

## 4. 常见问题 (FAQ)

**Q: 为什么容器能运行我的脚本？**
A: `docker-compose` 内部通过 `command` 字段调用了容器内的 `/app/scripts/start-coordinator.sh` 和 `/app/scripts/start-worker.sh`。

**Q: 如何动态增加节点？**
A: 在新的 GPU 机器上重复“第二步”即可。NATS 会通过 Queue Group 自动将任务负载均衡到新节点。
