cmake_minimum_required(VERSION 3.22)
project(vector_gateway CXX)

set(CMAKE_CXX_STANDARD 17)

# Use relative path for protobuf files
set(PROTO_DIR ${CMAKE_SOURCE_DIR}/../generated)

# Ensure proto directory exists
if(NOT EXISTS ${PROTO_DIR})
    message(FATAL_ERROR "Generated protobuf directory not found: ${PROTO_DIR}")
endif()

# 1. COMMON DEPENDENCIES - Use pkg-config for gRPC and protobuf
find_package(PkgConfig REQUIRED)

# Find gRPC and protobuf using pkg-config
pkg_check_modules(GRPC REQUIRED grpc++)
pkg_check_modules(PROTOBUF REQUIRED protobuf)

include_directories(${PROTO_DIR})
include_directories(${GRPC_INCLUDE_DIRS})
include_directories(${PROTOBUF_INCLUDE_DIRS})

# 2. BUILD-TIME SWITCHES
option(USE_RABBIT "Publish via RabbitMQ (ON) or via NATS (OFF)" OFF)
# ZMQ is always compiled in when the library is available; the transport is
# selected at *runtime* via the TRANSPORT env var, not at compile time.
option(USE_ZMQ "Compile ZMQ transport support (runtime: TRANSPORT=zmq)" ON)

if (USE_RABBIT)
    add_definitions(-DUSE_RABBIT)
endif()

# 3. MQ DEPENDENCIES - Use standard find methods
if (USE_RABBIT)
    # ----- RabbitMQ (SimpleAmqpClient) -----
    find_library(SIMPLEAMQPCLIENT_LIB NAMES SimpleAmqpClient)
    find_path(SIMPLEAMQPCLIENT_INCLUDE NAMES SimpleAmqpClient/SimpleAmqpClient.h)

    if (NOT SIMPLEAMQPCLIENT_LIB OR NOT SIMPLEAMQPCLIENT_INCLUDE)
        message(FATAL_ERROR "SimpleAmqpClient not found — install it or turn OFF USE_RABBIT")
    endif()

    include_directories(${SIMPLEAMQPCLIENT_INCLUDE})
    set(MQ_LIB ${SIMPLEAMQPCLIENT_LIB})

else()
    # ----- NATS (cnats) -----
    find_library(CNATS_LIB NAMES nats PATHS /usr/local/lib NO_DEFAULT_PATH)
    find_library(CNATS_LIB NAMES nats)
    find_path(CNATS_INCLUDE NAMES nats/nats.h PATHS /usr/local/include NO_DEFAULT_PATH)
    find_path(CNATS_INCLUDE NAMES nats/nats.h)

    if (NOT CNATS_LIB OR NOT CNATS_INCLUDE)
        message(FATAL_ERROR "cnats library not found — install it or turn ON USE_RABBIT")
    endif()

    message(STATUS "Found NATS library: ${CNATS_LIB}")
    message(STATUS "Found NATS include: ${CNATS_INCLUDE}")
    
    include_directories(${CNATS_INCLUDE})
    set(MQ_LIB ${CNATS_LIB})
endif()

# 3b. ZMQ (optional, runtime-selectable transport)
set(ZMQ_LIBS "")
if (USE_ZMQ)
    find_library(ZMQ_LIB NAMES zmq PATHS /usr/local/lib NO_DEFAULT_PATH)
    find_library(ZMQ_LIB NAMES zmq)
    find_path(ZMQ_INCLUDE NAMES zmq.h PATHS /usr/local/include NO_DEFAULT_PATH)
    find_path(ZMQ_INCLUDE NAMES zmq.h)

    if (ZMQ_LIB AND ZMQ_INCLUDE)
        message(STATUS "Found ZMQ: lib=${ZMQ_LIB}  inc=${ZMQ_INCLUDE}")
        include_directories(${ZMQ_INCLUDE})
        set(ZMQ_LIBS ${ZMQ_LIB})
    else()
        message(WARNING "ZMQ not found — building without ZMQ support (TRANSPORT=zmq will fail at runtime)")
        set(USE_ZMQ OFF)
    endif()
endif()

# Redis++ dependency - portable finding
find_library(REDISPLUSPLUS_LIB NAMES redis++)
find_path(REDISPLUSPLUS_INCLUDE NAMES sw/redis++/redis++.h)

if (NOT REDISPLUSPLUS_LIB OR NOT REDISPLUSPLUS_INCLUDE)
    message(FATAL_ERROR "redis++ library not found — install it first")
endif()

# Hiredis dependency - portable finding
find_library(HIREDIS_LIB NAMES hiredis)
find_path(HIREDIS_INCLUDE NAMES hiredis/hiredis.h)

if (NOT HIREDIS_LIB OR NOT HIREDIS_INCLUDE)
    message(FATAL_ERROR "hiredis library not found — install it first")
endif()

include_directories(${REDISPLUSPLUS_INCLUDE})
include_directories(${HIREDIS_INCLUDE})

# 4. TARGET
add_executable(gateway_server
  ${PROTO_DIR}/vector_service.pb.cc
  ${PROTO_DIR}/vector_service.grpc.pb.cc
  gateway_server.cc)

target_link_libraries(gateway_server
  ${GRPC_LIBRARIES}
  ${PROTOBUF_LIBRARIES}
  grpc++_reflection
  ${REDISPLUSPLUS_LIB}
  ${HIREDIS_LIB}
  ${MQ_LIB}
  ${ZMQ_LIBS}
)

# 5. HELPER OUTPUT
message(STATUS "Project root: ${CMAKE_SOURCE_DIR}")
message(STATUS "Build directory: ${CMAKE_BINARY_DIR}")
message(STATUS "Proto directory: ${PROTO_DIR}")
message(STATUS "Build mode: ${CMAKE_BUILD_TYPE}")
message(STATUS "MQ backend: $<IF:$<BOOL:${USE_RABBIT}>,RabbitMQ,NATS>")
message(STATUS "Redis libs: redis++=${REDISPLUSPLUS_LIB}, hiredis=${HIREDIS_LIB}")
message(STATUS "gRPC libs: ${GRPC_LIBRARIES}")
message(STATUS "Protobuf libs: ${PROTOBUF_LIBRARIES}")
