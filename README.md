# Loadforge

单机高性能压测引擎。**Python 做控制层，Rust 做执行层**：Python 只负责描述“测什么”，Rust 负责“怎么测”。压测过程中没有一行 Python 代码在热路径上运行。

## 架构一句话

```
Python 测试计划(dict)
   → loadforge.run(plan)
   → PyO3
   → Rust Engine (Tokio)
   → HTTP/1.1 / HTTP/2 / HTTPS / WebSocket / SSE
   → 目标服务
   → Metrics
   → Python 结果(dict)
```

## 安装

```bash
pip install loadforge
```

或从源码构建（需要 Rust 工具链 + Python 3.10+）：

```bash
maturin develop --release
```

## 快速开始

```python
import loadforge

result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 100,          # 虚拟用户数
    "duration": 10,     # 压测时长（秒）
    "endpoints": [
        {"method": "GET", "path": "/api/test", "weight": 1},
    ],
})

print(result)
```

## API

`loadforge.run(plan)` 是唯一的入口。`plan` 是一个 Python `dict`。

### plan 顶层字段

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `base_url` | str | 按工作负载 | HTTP/SSE 目标地址（`http://` 或 `https://`，形如 `http://host[:port]`）；`ws` 工作负载下可省略 |
| `vu` | int | ✅ | 虚拟用户数 |
| `duration` | int | 三选一 | 压测时长（秒） |
| `iterations` | int | 三选一 | 固定总迭代次数（跑完即停；`endpoints`=请求数，`flow`=流程轮数） |
| `stages` | list | 三选一 | 分阶段负载曲线：`[{duration, target}]`，VU 从 0 按每阶段线性爬/降到 `target` |
| `insecure` | bool | ❌ | 跳过 TLS 证书校验（自签名/内网），默认 `False` |
| `http2` | bool | ❌ | 使用 HTTP/2（`endpoints`/`flow` 时），默认 `False` |
| `ramp_up` | float | ❌ | VU 从 0 线性爬到 `vu` 的时长（秒），默认 `0`（瞬时全量） |
| `rps` | float | ❌ | 全局请求到达率上限（req/s），仅 HTTP 工作负载；不设则不限速 |
| `endpoints` | list | 四选一 | 单个请求模式（加权随机） |
| `flow` | list | 四选一 | 业务流程模式（顺序步骤） |
| `sse` | dict | 四选一 | Server-Sent Events 事件流模式 |
| `ws` | dict | 四选一 | WebSocket 模式 |

> 若同时传多个工作负载，优先级为 `flow` > `endpoints` > `sse` > `ws`。

### 单个请求模式 `endpoints`

每个元素描述一个请求，VU 每次迭代按 `weight` 加权随机选一个发送：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `method` | str | ❌ | HTTP 方法，默认 `GET` |
| `path` | str | ✅ | 请求路径，如 `/api/users` |
| `weight` | int | ❌ | 权重，默认 `1` |
| `body` | str | ❌ | 请求体（字符串，JSON 请自行 `json.dumps`） |
| `headers` | dict | ❌ | 请求头，`{k: v}` 均为字符串 |
| `expect` | dict | ❌ | 逐请求断言：`status` / `body_contains` / `json` |

### 业务流程模式 `flow`

每个元素是一个步骤，VU 一次迭代按顺序跑完整条流程。支持变量提取与替换：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `method` | str | ❌ | HTTP 方法，默认 `GET` |
| `path` | str | ✅ | 请求路径，可含 `${变量}` |
| `body` | str | ❌ | 请求体，可含 `${变量}` |
| `headers` | dict | ❌ | 请求头，值可含 `${变量}` |
| `extract` | dict | ❌ | `{变量名: JSON路径}`，从响应中提取变量 |
| `expect` | dict | ❌ | 逐请求断言：`status` / `body_contains` / `json` |

变量提取路径支持：`data.token`、`$.data.token`、`items.0.id` 等简单 JSON 点路径。变量是 **VU 局部** 的，不同 VU 之间互不共享。

### SSE 模式 `sse`

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `path` | str | ✅ | SSE 流路径，如 `/events` |
| `headers` | dict | ❌ | 额外请求头（自动附带 `Accept: text/event-stream`） |
| `expect` | dict | ❌ | 逐事件断言：对每个事件的 `data` 内容做 `status`/`body_contains`/`json` 校验 |

### WebSocket 模式 `ws`

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `url` | str | ✅ | `ws://` 或 `wss://` 地址，如 `ws://host:port/ws` |
| `message` | str | ❌ | 发送的文本消息，默认空串（`recv` 模式下作为一次性订阅消息） |
| `mode` | str | ❌ | `echo`（默认，发一收一测 RTT）/ `send`（只发不收，测发送吞吐）/ `recv`（只收，测推送吞吐） |
| `send_interval` | float | ❌ | `send` 模式下的发送间隔（秒），默认 `0`（全速） |

## 示例

### 1. 单个 HTTP 请求

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 100, "duration": 10,
    "endpoints": [{"method": "GET", "path": "/api/health", "weight": 1}],
})
```

### 2. 单个 HTTPS 请求（自签名证书）

```python
result = loadforge.run({
    "base_url": "https://127.0.0.1:8443",
    "vu": 100, "duration": 10,
    "insecure": True,
    "endpoints": [{"method": "GET", "path": "/api/health", "weight": 1}],
})
```

### 3. 加权多端点（GET / POST 混合）

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 200, "duration": 30,
    "endpoints": [
        {"method": "GET",  "path": "/api/users",    "weight": 5},
        {"method": "GET",  "path": "/api/products", "weight": 3},
        {"method": "POST", "path": "/api/orders",   "weight": 2,
         "body": '{"product_id": "p-001", "amount": 100}',
         "headers": {"Content-Type": "application/json"}},
    ],
})
```

### 4. 业务流程（登录拿 token → 鉴权请求）

```python
flow = [
    {"method": "GET", "path": "/api/login",
     "extract": {"token": "data.token", "uid": "data.user_id"}},
    {"method": "GET", "path": "/api/users/${uid}",
     "headers": {"Authorization": "Bearer ${token}"}},
    {"method": "POST", "path": "/api/order",
     "headers": {"Authorization": "Bearer ${token}"},
     "body": '{"user_id": ${uid}}'},
]

result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 100, "duration": 30,
    "flow": flow,
})
```

### 5. VU 爬坡 + 到达率控制

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 500, "duration": 60,
    "ramp_up": 15,      # 前 15 秒 VU 从 0 线性爬到 500
    "rps": 2000,        # 全程最多 2000 req/s
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})
```

### 6. HTTP/2

```python
# h2 over TLS（需服务端支持 ALPN h2）
result = loadforge.run({
    "base_url": "https://127.0.0.1:8443",
    "vu": 100, "duration": 10,
    "http2": True, "insecure": True,
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})

# h2c（明文 prior knowledge）
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 100, "duration": 10,
    "http2": True,
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})
```

### 7. SSE 事件流

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 50, "duration": 10,
    "sse": {"path": "/events",
            "expect": {"body_contains": "hello"}},   # 可选：逐事件内容断言
})
# result["total"] = 收到的 SSE 事件数；断言结果在 checks_passed / checks_failed
```

### 8. WebSocket 回环

```python
# 回环（默认）：发一条 → 等一条回包 → 记 RTT
result = loadforge.run({
    "vu": 100, "duration": 10,
    "ws": {"url": "ws://127.0.0.1:8080/ws", "message": "ping"},
})

# 只发不收（fire-and-forget）
result = loadforge.run({
    "vu": 100, "duration": 10,
    "ws": {"url": "ws://127.0.0.1:8080/ws", "message": "ping", "mode": "send"},
})

# 只收（服务器推送）
result = loadforge.run({
    "vu": 100, "duration": 10,
    "ws": {"url": "ws://127.0.0.1:8080/ws", "mode": "recv"},
})
# result["total"] = 消息数；echo 模式延迟 = RTT，send/recv 模式延迟为 0
```

### 9. 逐请求断言 `expect`

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 20, "duration": 10,
    "endpoints": [{
        "method": "GET", "path": "/api/me", "weight": 1,
        "expect": {
            "status": 200,                 # 期望状态码
            "body_contains": "alice",      # body 子串
            "json": {"name": "alice"},     # body JSON 字段（部分匹配，仅顶层）
        },
    }],
})
```

### 10. 固定次数 `iterations`

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 10, "iterations": 1000,          # 所有 VU 合计跑 1000 次
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})
```

### 11. 分阶段负载曲线 `stages`

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "stages": [
        {"duration": 30, "target": 100},   # 30s 内从 0 爬升到 100 VU
        {"duration": 60, "target": 100},   # 保持 100 VU 60s
        {"duration": 30, "target": 0},     # 30s 内降到 0
    ],
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})
```

### 12. 每秒时序数据 `time_series`

```python
result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 100, "duration": 10,
    "endpoints": [{"method": "GET", "path": "/api/test", "weight": 1}],
})

for p in result["time_series"]:
    print(p["t"], p["requests"], p["failed"], p["avg_ms"])
    # t=0s, 1s, 2s, ...  每秒请求数 / 失败数 / 平均延迟
```

## 返回结果

`loadforge.run` 返回一个 `dict`：

| 字段 | 类型 | 说明 |
|---|---|---|
| `total` | int | 请求数 / SSE 事件数 / WS 消息数（视工作负载） |
| `success` | int | 成功数 |
| `failed` | int | 失败数（连接/传输层错误） |
| `elapsed_secs` | float | 实际耗时（秒） |
| `rps` | float | 平均每秒请求数（或事件数 / 消息数） |
| `bytes` | int | 响应/载荷字节总数 |
| `min_ms` / `avg_ms` / `max_ms` | float | 延迟最小/平均/最大（毫秒；SSE 为 0，不统计） |
| `p50_ms` / `p95_ms` / `p99_ms` | float | 延迟分位数（毫秒） |
| `percentiles` | dict | 百分位数组 `{50, 75, 90, 95, 99}`，值为毫秒 |
| `status_codes` | dict | HTTP 状态码分布；WS 为 `{101: n}`；连接失败记 `0` |
| `errors` | dict | 连接失败细分：`timeout` / `refused` / `reset` / `tls` / `dns` / `protocol` / `other` |
| `checks_passed` / `checks_failed` | int | 断言通过/失败数 |
| `check_failures` | list | 失败断言样本（`method`/`path`/`check`/`expected`/`actual`，最多 100 条） |
| `time_series` | list | 每秒采样：`{t, requests, success, failed, bytes, avg_ms}`，`t` 为秒 |

> `status_codes` 里的 `0` 表示连接/传输层失败（没有收到响应）。

## 性能对比（与 k6）

测试环境：同机（22 核）、相同 Node 目标服务器、相同场景、时长 5s。吞吐 = req/s（HTTP）或 msg/s（WS）。

| 工作负载 | VU | LF 吞吐 | k6 吞吐 | 吞吐比 | LF CPU | k6 CPU | LF 内存 | k6 内存 | 效率比 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| HTTP/1.1 | 100 | 58 171 | 44 770 | 1.30x | 21.1s | 39.6s | 20MB | 137MB | 2.44x |
| HTTP/1.1 | 1000 | 61 888 | 50 818 | 1.22x | 21.3s | 46.0s | 63MB | 354MB | 2.64x |
| HTTP/2 | 100 | 41 161 | 19 138 | 2.15x | 15.3s | 14.8s | 18MB | 93MB | 2.09x |
| HTTP/2 | 1000 | 56 697 | 20 056 | 2.83x | 18.5s | 43.1s | 31MB | 358MB | 6.53x |
| WebSocket | 300 | 42 383 | 37 312 | 1.14x | 12.0s | 14.7s | 14MB | 109MB | 1.39x |

> 效率比 = 每 CPU 秒产出的请求/消息数之比。Loadforge 在所有工作负载上内存省 4–12 倍、CPU 效率全面占优。

### 高 VU 扩展性（多进程服务器）

| 协议 | 1000 VU | 2000 VU | 5000 VU |
|---|---:|---:|---:|
| HTTP/1.1（每 VU 一连接） | 62k | 47k | 43k |
| HTTP/2（连接池多路复用） | 96k | 99k | 107k |
| WebSocket（每 VU 一连接） | 52k | 51k | — |

HTTP/2 多路复用让吞吐随 VU 持续上升；HTTP/1.1 / WebSocket 在更高 VU 下受“每 VU 一条连接”的开销限制。

### 断言校验开销

| 配置 | RPS | CPU | 内存 |
|---|---:|---:|---:|
| 无断言 | 56 145 | 16.7s | 39MB |
| 仅状态码 | 57 898 | 16.3s | 39MB |
| 状态码 + body + json | 55 997 | 17.8s | 39MB |

状态码断言几乎零开销；内容断言约 +6% CPU、RPS 基本不变。

## 当前支持 / 不支持

**支持**：HTTP/1.1、HTTP/2（h2 与 h2c，连接池多路复用）、HTTPS、WebSocket、SSE、加权单请求、业务流程（变量提取/替换）、逐请求断言（`expect`）、时长/固定次数/分阶段曲线（`stages`）三种模式、到达率控制、每秒时序采样（`time_series`）、连接失败细分（`errors`）、P50/P75/P90/P95/P99 及 min/avg/max 延迟统计。

**暂不支持（MVP 边界）**：分布式 / Agent、CLI / Web UI、cookie/session 管理、TLS 客户端证书、多目标主机、自定义断言、HTTP/3。

## 设计原则

本项目的最高设计准则见 [`PRINCIPLES.md`](PRINCIPLES.md)。核心：单机有限资源下实现最大规模压测；性能优先；Python 不进热路径；VU 是 Rust 内部的轻量异步任务。