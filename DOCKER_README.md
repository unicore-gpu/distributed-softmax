# Vector Service - Docker Deployment and Testing Guide

## 🚀 CUDA Accelerated Version - Quick Start (7 Steps)

**Most Important Steps - Build, Run and Test CUDA Accelerated Vector Service:**

### 1. Clean Up Old Containers
```bash
docker rm -f vector-service || true
```

### 2. Rebuild Image (Including CUDA Compilation)
```bash
docker compose build --no-cache
```

### 3. Start Services (Including GPU Support)
```bash
docker compose up -d
```

### 4. Check Status and Logs
```bash
docker compose ps
docker logs vector-service --tail=200
```

### 5. Enter Container
```bash
docker exec -it vector-service bash
```

### 6. Verify CUDA Library Exists
```bash
ls -l /app/worker/cuda/libsoftmax_cuda.so
```

### 7. Test CUDA Accelerated Softmax (GPU-Only Processing)
```bash
# Test via gRPC (end-to-end with result polling)
python3 -c "
import grpc
import time
import vector_service_pb2 as pb
import vector_service_pb2_grpc as stubs

channel = grpc.insecure_channel('localhost:50051')
stub = stubs.VectorServiceStub(channel)

job_id = 'test-softmax-001'
resp = stub.SubmitTask(pb.TaskRequest(
    job_id=job_id,
    task='softmax',
    vector=[1.0, 2.0, 3.0, 4.0, 5.0]
))
print('✅ Task submitted:', resp.message)

# Poll for GPU-processed results
for i in range(10):
    result_resp = stub.GetResult(pb.ResultRequest(job_id=job_id))
    print(f'[{i+1:2d}] Status: {result_resp.status:8s} | Progress: {result_resp.completed_slices}/{result_resp.total_slices}')
    
    if result_resp.status == 'ready':
        print('🎉 GPU Result:', list(result_resp.result))
        print('📊 Sum verification:', f'{sum(result_resp.result):.6f}', '(should be ~1.0)')
        break
    elif result_resp.status == 'failed':
        print('❌ Task failed:', result_resp.message)
        break
    time.sleep(1)
else:
    print('⏰ Timeout waiting for results')
"

# Test different CUDA methods directly
python3 -c "from worker.softmax import softmax; print('Auto (NCCL→Basic→NumPy):', softmax([1.0,2.0,3.0]))"
python3 -c "from worker.softmax import softmax; print('NCCL method:', softmax([1.0,2.0,3.0], 'nccl'))"
python3 -c "from worker.softmax import softmax; print('Basic CUDA:', softmax([1.0,2.0,3.0], 'basic'))"
python3 -c "from worker.softmax import softmax; print('NumPy fallback:', softmax([1.0,2.0,3.0], 'numpy'))"

# Verify GPU access
nvidia-smi
```

**CUDA Support Notes:**
- **NCCL Method**: Uses optimized warp reduction, best performance
- **Basic CUDA**: Basic CUDA implementation, good compatibility
- **NumPy Fallback**: CPU implementation, automatically used when no GPU
- **Auto Selection**: NCCL → Basic CUDA → NumPy intelligent fallback
- All methods produce identical results, with different performance

---

## Overview

This guide explains how to use Docker to containerize and deploy the gRPC Vector Service, including build, run, and testing procedures.

## Architecture Description

All service components run inside the Docker container:
- **Redis**: Result caching and storage
- **NATS Server**: Message queue
- **Gateway Server (C++)**: gRPC API entry point (task submission and result polling)
- **Python Worker**: GPU-based vector computation processor (CUDA acceleration)

**Processing Flow:**
1. Client submits task via `SubmitTask` → Gateway returns "OK - Processing with GPU workers"
2. Gateway distributes work to GPU workers via NATS
3. Workers process vectors using CUDA acceleration
4. Client polls for results via `GetResult` until status is "ready"

All services run automatically in the background when the container starts, no need to manually manage multiple processes.

---

## Prerequisites

### System Requirements
- Ubuntu 22.04 LTS (ARM64 or AMD64)
- Docker installed
- At least 4GB available memory
- At least 10GB available disk space

### Check Docker Installation
```bash
docker --version
docker info
```

### Install Docker (if not installed)
```bash
# Install Docker
curl -fsSL https://get.docker.com -o get-docker.sh
sudo sh get-docker.sh

# Add current user to docker group
sudo usermod -aG docker $USER

# Re-login or run
newgrp docker
```

---

## Quick Start

### 1. Prepare Project Files

Ensure the project directory structure is as follows:
```
distributed-softmax/
├── Dockerfile
├── docker-compose.yml
├── .dockerignore
├── proto/
│   └── vector_service.proto
├── server/
│   ├── gateway_server.cc
│   └── CMakeLists.txt
├── worker/
│   ├── main.py
│   ├── softmax.py
│   ├── handler.py
│   └── cuda/
│       ├── softmax_kernel.cu
│       ├── softmax_kernel.cuh
│       └── Makefile
└── client/
    └── client.py
```

### 2. Build and Start (Recommended: Docker Compose)

#### Method A: Using Docker Compose (Recommended)

```bash
cd ~/workspace/ben/distributed-softmax

# Clean up old containers (if any)
docker rm -f vector-service || true

# Build image (first build takes about 10-20 minutes, includes CUDA compilation)
docker compose build --no-cache

# Start services (including GPU support)
docker compose up -d

# Check service status
docker compose ps

# View logs
docker logs vector-service --tail=200
```

#### Method B: Manual Build and Run

```bash
cd ~/workspace/ben/distributed-softmax

# Build image
docker build -t distributed-softmax .

# Start container (with GPU support)
docker run -d \
  --name vector-service \
  --gpus all \
  -p 50051:50051 \
  -p 4222:4222 \
  -p 8222:8222 \
  -p 6379:6379 \
  -p 8000:8000 \
  distributed-softmax
```

**Port Mapping Description:**
| Container Port | Host Port | Service |
|----------------|-----------|---------|
| 50051 | 50051 | gRPC Gateway API |
| 4222 | 4222 | NATS Message Queue |
| 8222 | 8222 | NATS Monitoring Interface |
| 6379 | 6379 | Redis |
| 8000 | 8000 | Prometheus Metrics |

### 3. Check Container Status

```bash
# View running containers
docker ps

# View container logs
docker logs vector-service

# Real-time log tracking
docker logs -f vector-service
```

**Successful startup logs should contain:**
```
Services started!
```

### 4. Verify CUDA Support (Optional)

```bash
# Enter container
docker exec -it vector-service bash

# Check if CUDA library exists
ls -l /app/worker/cuda/libsoftmax_cuda.so

# Test GPU access
nvidia-smi

# Exit container
exit
```

---

## Testing Services

### Understanding Client and Server

Before testing, it's important to understand the following concepts:

**Container External Testing (Host as Client):**
```
Host (Client)  ──Network Connection→  Container (Server)
                Port 50051
```
- Need to generate protobuf files on host
- Need to install Python packages like `grpcio`
- Access services inside container through port mapping

**Container Internal Testing (Container is both Client and Server):**
```
Inside Container:
  Client Code ──localhost→ Gateway Server (same container)
```
- No need to install anything on host
- Protobuf files already generated during build
- Faster, no cross-network boundary

**Why doesn't the client need other components?**

The client only needs two things:
1. **Generated protobuf files** - for data serialization/deserialization
2. **Network connection** - connect to service via gRPC port

The client doesn't need Redis, NATS, Gateway Server, Worker, etc. These are all server components that process requests inside the container.

### Test 1: Health Check

Check if various services are running normally:

```bash
# Test NATS monitoring endpoint
curl http://localhost:8222/
# Expected: Returns JSON format NATS server information

# Test Redis (need to install redis-cli)
redis-cli -h localhost ping
# Expected: PONG

# Test Prometheus metrics
curl http://localhost:8000/metrics
# Expected: Returns Prometheus format metrics data
```

### Test 2: gRPC Service - Softmax (CUDA Acceleration Support)

#### Method A: Test Inside Container (Recommended, Simplest)

```bash
# Enter container
docker exec -it vector-service bash

# Test Softmax (uses GPU workers for distributed processing)
python3 -c "
import grpc
import time
import vector_service_pb2 as pb
import vector_service_pb2_grpc as stubs

channel = grpc.insecure_channel('localhost:50051')
stub = stubs.VectorServiceStub(channel)

job_id = 'test-softmax-001'
resp = stub.SubmitTask(pb.TaskRequest(
    job_id=job_id,
    task='softmax',
    vector=[1.0, 2.0, 3.0]
))
print('✅ Task submitted:', resp.message)

# Poll for results
for i in range(10):
    result_resp = stub.GetResult(pb.ResultRequest(job_id=job_id))
    if result_resp.status == 'ready':
        print('🎉 GPU Result:', list(result_resp.result))
        break
    time.sleep(1)
"

# Directly test Python softmax function (verify CUDA/NumPy switching)
python3 -c "from worker.softmax import softmax; print('Direct softmax:', softmax([1.0,2.0,3.0]))"

# Exit container
exit
```

**CUDA Support Notes:**
- **NCCL Method**: Uses optimized warp reduction, best performance
- **Basic CUDA**: Basic CUDA implementation, good compatibility
- **NumPy Fallback**: CPU implementation, automatically used when no GPU
- **Auto Selection**: NCCL → Basic CUDA → NumPy intelligent fallback
- All methods produce identical results, with different performance

#### Method B: Test on Host

**Prerequisites (required for first time):**

```bash
cd ~/projects/gRPC-test  # or ~/workspace/ben/gRPC-test

# 1. Install Python dependencies
pip install grpcio grpcio-tools protobuf

# 2. Generate protobuf files
python3 -m grpc_tools.protoc \
  --proto_path=proto \
  --python_out=client \
  --grpc_python_out=client \
  proto/vector_service.proto

# 3. Verify generated files
ls -la client/vector_service_pb2*.py
```

**Test Code:**

```bash
# Test Softmax normalization
python3 -c "
import grpc
import time
import sys
sys.path.insert(0, 'client')
import vector_service_pb2 as pb
import vector_service_pb2_grpc as stubs

channel = grpc.insecure_channel('localhost:50051')
stub = stubs.VectorServiceStub(channel)

job_id = 'test-softmax-001'
resp = stub.SubmitTask(pb.TaskRequest(
    job_id=job_id,
    task='softmax',
    vector=[1.0, 2.0, 3.0, 4.0, 5.0]
))
print('✅ Task submitted:', resp.message)

# Poll for GPU-processed results
for i in range(10):
    result_resp = stub.GetResult(pb.ResultRequest(job_id=job_id))
    if result_resp.status == 'ready':
        print('🎉 GPU Result:', list(result_resp.result))
        print('📊 Sum verification:', f'{sum(result_resp.result):.6f}', '(should be ~1.0)')
        break
    time.sleep(1)
"
```

**Expected Output:**
```
✅ Task submitted: OK - Processing with GPU workers
[ 1] Status: pending   | Progress: 0/1
[ 2] Status: ready     | Progress: 1/1
🎉 GPU Result: [0.0116562, 0.0316849, 0.0861285, 0.2341217, 0.6364086]
📊 Sum verification: 1.000000 (should be ~1.0)
```

Result explanation: Output is normalized probability distribution, all values sum to 1.0. The system now uses GPU-only processing with asynchronous result polling.

#### Method C: Copy protobuf files from container to host

```bash
# If you don't want to regenerate on host, you can copy from container
docker cp vector-service:/app/client/vector_service_pb2.py client/
docker cp vector-service:/app/client/vector_service_pb2_grpc.py client/

# Then you can run tests on host
```

### Test 3: Run Complete Test Client (Host Only)

```bash
# Ensure prerequisites from Test 2 are completed
cd ~/projects/gRPC-test

# Run complete client test
python3 client/client.py
```

### Test Method Comparison

| Feature | Container Internal Testing | Container External Testing (Host) |
|---------|---------------------------|-----------------------------------|
| **Prerequisites** | No preparation needed | Need to install Python packages and generate protobuf |
| **Environment Dependencies** | None | Need grpcio, grpcio-tools, protobuf |
| **Protobuf Files** | Generated during build | Need manual generation or copy from container |
| **Network Communication** | Container internal localhost | Through port mapping |
| **Speed** | Faster (no network boundary) | Slightly slower (cross-network) |
| **Use Cases** | Quick verification, debugging | Development testing, CI/CD |
| **Commands** | `docker exec -it vector-service ...` | `python3 client/...` |

**Recommendations:**
- Quick testing and verification: Use container internal testing
- Development and integration: Use container external testing

---

## Container Internal Testing

Sometimes you need to debug or test inside the container. The container already contains all necessary tools and generated files.

### Basic Operations

```bash
# Enter container
docker exec -it vector-service bash

# View running processes
ps aux | grep -E 'redis|nats|gateway|python'

# View directory structure
ls -la /app/

# View generated protobuf files
ls -la /app/client/vector_service_pb2*.py

# Exit container
exit
```

### Service Check

```bash
# Check various services inside container
docker exec -it vector-service bash

# Test Redis
redis-cli ping

# View NATS logs
cat /var/log/nats/nats-server.log

# Test NATS HTTP interface
curl http://localhost:8222/

# Exit
exit
```

### Complete Container Internal Testing Example

```bash
# Enter container
docker exec -it vector-service bash

# Test softmax processing
echo "Testing Softmax..."
python3 -c "
import grpc
import time
import vector_service_pb2 as pb
import vector_service_pb2_grpc as stubs

channel = grpc.insecure_channel('localhost:50051')
stub = stubs.VectorServiceStub(channel)

job_id = 'test-softmax-001'
resp = stub.SubmitTask(pb.TaskRequest(
    job_id=job_id,
    task='softmax',
    vector=[1.0, 2.0, 3.0]
))
print('✅ Task submitted:', resp.message)

# Poll for results
for i in range(10):
    result_resp = stub.GetResult(pb.ResultRequest(job_id=job_id))
    if result_resp.status == 'ready':
        print('🎉 GPU Result:', list(result_resp.result))
        break
    time.sleep(1)
"

# Exit container
exit
```

### Container Internal vs External Working Principles

**Container External Testing (Host as Client):**
```
┌─────────────────┐           ┌──────────────────────────┐
│   Host          │           │        Container          │
│                 │           │                          │
│  Client Code    │  gRPC     │  Gateway Server          │
│  + protobuf     │ ───────→  │  ├─ NATS                │
│  + grpcio lib   │  :50051   │  ├─ Worker               │
│                 │           │  └─ Redis                │
└─────────────────┘           └──────────────────────────┘
```

**Container Internal Testing (Container Self-Test):**
```
┌──────────────────────────────────────────┐
│                Container                  │
│                                          │
│  Client Code  ──localhost→  Gateway Server │
│  (Test Script)               ├─ NATS       │
│                              ├─ Worker     │
│                              └─ Redis      │
└──────────────────────────────────────────┘
```

**Key Differences:**
- Container Internal: All communication is inside container, via localhost
- Container External: Need to communicate across network boundary via port mapping (50051)

---

## Container Management

### View Container Information

```bash
# View running containers
docker ps

# View all containers (including stopped)
docker ps -a

# View container detailed information
docker inspect vector-service

# View container resource usage
docker stats vector-service --no-stream
```

### Log Management

```bash
# View all logs
docker logs vector-service

# View recent 100 lines
docker logs --tail 100 vector-service

# Real-time log tracking
docker logs -f vector-service

# View logs from last 10 minutes
docker logs --since 10m vector-service

# Export logs to file
docker logs vector-service > service.log 2>&1
```

### Start/Stop/Restart

```bash
# Stop container
docker stop vector-service

# Start stopped container
docker start vector-service

# Restart container
docker restart vector-service

# Force delete container
docker rm -f vector-service
```

### Clean Up Resources

```bash
# Delete container
docker rm vector-service

# Delete image
docker rmi vector-service:simple

# Clean up unused resources
docker system prune -a

# View disk usage
docker system df
```

---

## Troubleshooting

### Problem 1: Container Cannot Start

**Symptoms:** Container exits immediately after `docker run`

**Troubleshooting Steps:**
```bash
# View container exit reason
docker logs vector-service

# Check if port is already in use
netstat -tulpn | grep :50051

# Check Docker daemon status
sudo systemctl status docker

# Check available disk space
df -h
```

**Common Solutions:**
- Port conflict: Change port mapping or stop conflicting service
- Insufficient resources: Increase memory/disk space
- Docker daemon not running: `sudo systemctl start docker`

### Problem 2: gRPC Connection Failed

**Symptoms:** `grpc.RpcError: <_InactiveRpcError of RPC that terminated with: status = StatusCode.UNAVAILABLE>`

**Troubleshooting Steps:**
```bash
# Check if container is running
docker ps | grep vector-service

# Check if port is accessible
telnet localhost 50051

# Check container logs
docker logs vector-service --tail=50

# Test from inside container
docker exec -it vector-service bash
python3 -c "import grpc; print('gRPC available')"
```

**Common Solutions:**
- Container not started: `docker start vector-service`
- Port mapping issue: Check docker run command
- Firewall blocking: Check firewall settings

### Problem 3: CUDA Not Working

**Symptoms:** CUDA methods fail, falling back to NumPy

**Troubleshooting Steps:**
```bash
# Check if GPU is accessible in container
docker exec -it vector-service nvidia-smi

# Check CUDA library
docker exec -it vector-service ls -l /app/worker/cuda/libsoftmax_cuda.so

# Test CUDA directly
docker exec -it vector-service python3 -c "
from worker.softmax import softmax
print('CUDA test:', softmax([1.0,2.0,3.0], 'basic'))
"
```

**Common Solutions:**
- GPU not accessible: Add `--gpus all` to docker run
- CUDA library missing: Rebuild image with CUDA support
- Driver issues: Update NVIDIA drivers

### Problem 4: NATS Connection Failed

**Symptoms:** Worker cannot connect to NATS

**Troubleshooting Steps:**
```bash
# Check NATS server status
docker exec -it vector-service ps aux | grep nats

# Test NATS connection
docker exec -it vector-service curl http://localhost:8222/

# Check NATS logs
docker exec -it vector-service cat /var/log/nats/nats-server.log
```

**Common Solutions:**
- NATS not started: Restart container
- Port conflict: Check port 4222 availability
- Configuration issue: Check NATS config file

### Problem 5: Redis Connection Failed

**Symptoms:** Cannot store/retrieve results

**Troubleshooting Steps:**
```bash
# Check Redis status
docker exec -it vector-service redis-cli ping

# Check Redis logs
docker exec -it vector-service tail -f /var/log/redis/redis-server.log

# Test Redis operations
docker exec -it vector-service redis-cli set test "hello"
docker exec -it vector-service redis-cli get test
```

**Common Solutions:**
- Redis not started: Restart container
- Memory issues: Check available memory
- Disk space: Check available disk space

---

## Performance Optimization

### GPU Optimization

```bash
# Monitor GPU usage
docker exec -it vector-service nvidia-smi -l 1

# Test different CUDA methods
docker exec -it vector-service python3 -c "
import time
from worker.softmax import softmax
import numpy as np

# Generate large vector
large_vector = np.random.rand(10000).tolist()

# Test performance
methods = ['numpy', 'basic', 'nccl']
for method in methods:
    start = time.time()
    result = softmax(large_vector, method)
    print(f'{method}: {time.time() - start:.4f}s')
"
```

### Memory Optimization

```bash
# Monitor container memory usage
docker stats vector-service

# Check Redis memory usage
docker exec -it vector-service redis-cli info memory

# Monitor system resources
docker exec -it vector-service top
```

### Network Optimization

```bash
# Test network latency
docker exec -it vector-service ping -c 5 localhost

# Monitor network connections
docker exec -it vector-service netstat -tulpn

# Check gRPC connection pool
docker exec -it vector-service python3 -c "
import grpc
channel = grpc.insecure_channel('localhost:50051')
print('Channel state:', channel.get_state())
"
```

---

## Advanced Configuration

### Custom CUDA Configuration

```bash
# Set CUDA device
docker run -d \
  --name vector-service \
  --gpus '"device=0"' \
  -e CUDA_VISIBLE_DEVICES=0 \
  -p 50051:50051 \
  distributed-softmax
```

### Custom Redis Configuration

```bash
# Mount custom Redis config
docker run -d \
  --name vector-service \
  -v /path/to/redis.conf:/etc/redis/redis.conf \
  -p 6379:6379 \
  distributed-softmax
```

### Custom NATS Configuration

```bash
# Mount custom NATS config
docker run -d \
  --name vector-service \
  -v /path/to/nats.conf:/etc/nats/nats-server.conf \
  -p 4222:4222 \
  distributed-softmax
```

---

## Production Deployment

### Docker Compose for Production

```yaml
version: '3.8'
services:
  vector-service:
    build: .
    ports:
      - "50051:50051"
      - "8000:8000"
    environment:
      - REDIS_TTL_HOURS=24
      - REDIS_TTL_SLICE_HOURS=48
    deploy:
      resources:
        limits:
          memory: 4G
        reservations:
          memory: 2G
    restart: unless-stopped
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8000/metrics"]
      interval: 30s
      timeout: 10s
      retries: 3
```

### Kubernetes Deployment

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: vector-service
spec:
  replicas: 3
  selector:
    matchLabels:
      app: vector-service
  template:
    metadata:
      labels:
        app: vector-service
    spec:
      containers:
      - name: vector-service
        image: distributed-softmax:latest
        ports:
        - containerPort: 50051
        - containerPort: 8000
        resources:
          limits:
            memory: "4Gi"
            nvidia.com/gpu: 1
          requests:
            memory: "2Gi"
            nvidia.com/gpu: 1
        env:
        - name: REDIS_TTL_HOURS
          value: "24"
```

---

## Monitoring and Observability

### Prometheus Metrics

```bash
# View available metrics
curl http://localhost:8000/metrics

# Key metrics to monitor:
# - worker_tasks_total: Total tasks processed
# - worker_duration_seconds: Task processing duration
# - redis_operations: Redis operation counts
```

### Log Aggregation

```bash
# Export logs for analysis
docker logs vector-service > vector-service.log 2>&1

# Real-time log monitoring
docker logs -f vector-service | grep -E "(ERROR|WARN|INFO)"

# Log rotation setup
docker run -d \
  --name vector-service \
  --log-driver json-file \
  --log-opt max-size=100m \
  --log-opt max-file=3 \
  distributed-softmax
```

### Health Checks

```bash
# Custom health check script
#!/bin/bash
# health-check.sh

# Check gRPC service
grpcurl -plaintext localhost:50051 list

# Check Redis
redis-cli -h localhost ping

# Check NATS
curl -f http://localhost:8222/ > /dev/null

# Check Prometheus metrics
curl -f http://localhost:8000/metrics > /dev/null

echo "All services healthy"
```

---

## Security Considerations

### Network Security

```bash
# Use TLS for gRPC
docker run -d \
  --name vector-service \
  -v /path/to/certs:/app/certs \
  -e GRPC_TLS_CERT=/app/certs/server.crt \
  -e GRPC_TLS_KEY=/app/certs/server.key \
  distributed-softmax
```

### Container Security

```bash
# Run as non-root user
docker run -d \
  --name vector-service \
  --user 1000:1000 \
  distributed-softmax

# Read-only filesystem
docker run -d \
  --name vector-service \
  --read-only \
  --tmpfs /tmp \
  distributed-softmax
```

### Resource Limits

```bash
# Set resource limits
docker run -d \
  --name vector-service \
  --memory=4g \
  --cpus=2 \
  --ulimit nofile=65536:65536 \
  distributed-softmax
```

---

## Backup and Recovery

### Data Backup

```bash
# Backup Redis data
docker exec vector-service redis-cli BGSAVE
docker cp vector-service:/var/lib/redis/dump.rdb ./backup/

# Backup configuration
docker cp vector-service:/app/config/ ./backup/
```

### Container Backup

```bash
# Export container as image
docker commit vector-service vector-service:backup

# Save image to file
docker save vector-service:backup > vector-service-backup.tar

# Load image from file
docker load < vector-service-backup.tar
```

---

## Conclusion

This Docker deployment guide provides comprehensive instructions for:

✅ **Quick Start**: 7-step process to get CUDA-accelerated Vector Service running
✅ **Testing**: Multiple testing methods (container internal/external)
✅ **Troubleshooting**: Common problems and solutions
✅ **Optimization**: Performance tuning and monitoring
✅ **Production**: Deployment best practices
✅ **Security**: Security considerations and hardening
✅ **Maintenance**: Backup, recovery, and monitoring

The Vector Service is now ready for distributed softmax processing with CUDA acceleration in a containerized environment.

For additional support or questions, refer to the main README.md or create an issue in the project repository.