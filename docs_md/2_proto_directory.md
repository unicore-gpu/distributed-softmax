# Proto 目录详细文档

该目录定义了系统的服务契约。通过协议缓冲区 (Protocol Buffers) 定义接口，确保了 C++ 网关与 Python 客户端/Worker 之间的数据一致性。

## 1. `vector_service.proto` 定义解析

### A. 服务接口 (Service)

```protobuf
service VectorService {
  // 提交一个新任务，网关分片后通过 NATS 转发
  rpc SubmitTask(TaskRequest) returns (TaskResponse);
  
  // 用于客户端轮询特定 Job 的执行进度和最终结果
  rpc GetResult(ResultRequest) returns (ResultResponse);
}
```

---

### B. 请求消息体

#### `TaskRequest` (客户端提交)
```protobuf
message TaskRequest {
  string job_id = 1;      // 字符串，客户端生成的 UUID
  string task = 2;        // 目前仅支持 "softmax"
  repeated float vector = 3; // 浮点数数组，即待处理的原始数据
}
```

#### `ResultRequest` (客户端查询)
```protobuf
message ResultRequest {
  string job_id = 1;      // 之前提交任务时使用的唯一 ID
}
```

---

### C. 响应消息体 (关键参数说明)

#### `ResultResponse`
```protobuf
message ResultResponse {
  string job_id = 1;
  string status = 2;         // 核心状态：pending, running, ready, failed
  repeated float result = 3;  // 当 status 为 "ready" 时，这里存放全量计算结果
  string message = 4;         // 包含 TTL 剩余时间或错误详情的辅助文本
  int32 completed_slices = 5; // 当前已在 Redis 完成计算的切片数量
  int32 total_slices = 6;     // 该任务被拆分出的切片总数（向量大小 / 10）
}
```

## 2. 编译与生成逻辑

在 `Dockerfile` 中，使用了以下命令生成对应语言的代码包：

```bash
# 生成 C++ 服务端代码 (放入 server 目录)
protoc --cpp_out=generated --grpc_out=generated \
       --plugin=protoc-gen-grpc=$(which grpc_cpp_plugin) \
       --proto_path=proto proto/vector_service.proto

# 生成 Python 客户端/Worker 代码 (放入 client 目录)
python3 -m grpc_tools.protoc --proto_path=proto \
       --python_out=client --grpc_python_out=client proto/vector_service.proto
```

**生成的关键文件**:
- `vector_service_pb2.py`: 包含 Python 数据模型类（如 `TaskRequest`）。
- `vector_service_pb2_grpc.py`: 包含 Python 服务类和存根类（如 `VectorServiceStub`）。
- `vector_service.pb.h/cc`: C++ 消息序列化类。
- `vector_service.grpc.pb.h/cc`: C++ gRPC 模板类。
