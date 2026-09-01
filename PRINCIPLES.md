# Loadforge 核心原则

本文档是 Loadforge 项目的最高设计准则，所有开发决策必须以此为准。违背任何一条原则的代码，不得合入。

---

## 第一原则

> **在单机有限资源下，实现最大规模的性能测试。**

一切设计、架构、选型，都服务于这一个目标。
当灵活性与性能冲突时，性能优先。
当代码优雅与性能冲突时，性能优先。

---

## 架构原则

### 1. Python 是控制层，Rust 是执行层

```
Python ──低频控制──→ Rust
        ←──返回结果──
```

Python 只负责：测试定义、参数配置、启动测试、获取结果。
Rust 负责：并发调度、VU 管理、HTTP 执行、连接管理、请求计时、Metrics。

### 2. Python 不进入高频执行路径

Python 描述"测什么"，Rust 决定"怎么测"。

执行过程中，没有一行 Python 代码运行。

Python 与 Rust 的交互只发生在两个时刻：
- 测试开始前：传递测试配置
- 测试结束后：接收执行结果

### 3. VU 是轻量状态机

VU 不等于 OS Thread、Python Thread、Python Coroutine 或重量级对象。

VU 是 Rust 内部的轻量异步任务，目标是最低的内存占用、调度开销和 Context Switching。

---

## 执行原则

### 4. Rust Engine 是项目核心

Rust Engine 承载所有高频逻辑：
- 异步运行时（Tokio）
- 并发调度
- VU 生命周期管理
- HTTP 请求执行
- TCP/TLS 连接管理
- Connection Pool
- 请求计时
- 基础 Metrics
- 高并发状态管理

Rust Engine 的核心目标：低 CPU、低内存、高并发。

### 5. 系统资源是第一瓶颈

文件描述符、临时端口、内核 TCP 缓冲区、网卡带宽，这些才是真正的天花板。

引擎必须主动检测并优化系统参数，而不是假装语言性能可以绕过它们。

### 6. 每个设计决策回答一个问题

> "这个方案，能不能让同一个用户、同一台机器，测出更大的规模？"

能 → 采用。
不能 → 排除。
有争议 → 用 Benchmark 说话。

---

## 范围原则

### 7. MVP 只实现最本质的功能

**第一阶段只做：**
- Python API
- PyO3 桥接
- Rust Async Engine（Tokio）
- HTTP/1.1
- 并发执行
- 基础 VU
- 基础 RPS
- Duration 控制
- 基础 Metrics（requests / success / failed / latency）

**第一阶段不做：**
- CLI / Web UI / Dashboard / Grafana
- 分布式压测 / Agent
- HTTP/2 / WebSocket / SSE
- 插件系统 / 复杂报告
- 云端控制 / 集群管理 / 复杂 DSL

### 8. 先做单机，再考虑分布式

### 9. 先做 HTTP/1.1，再扩展其他协议

### 10. 先把轮子做对，再给轮子加功能

---

## 用户体验原则

### 11. Rust 对用户透明

用户只需要：

```bash
pip install loadforge
```

```python
from loadforge import LoadTest
```

用户不需要了解 Rust、Tokio、PyO3、FFI、Cargo 或任何 Rust 内部实现。

### 12. 性能优势必须通过 Benchmark 证明

不假设性能优势，用数据说话。

相同机器、相同目标、相同场景下，与 k6 比较：
- CPU 占用
- 内存占用
- RPS
- P95 / P99 延迟
- 最大 VU
- 单机最大并发

---

## 质量原则

### 13. 先验证核心链路

MVP 只验证这一条链路：

```
Python Test 定义
    → Python API
    → PyO3
    → Rust Engine
    → Tokio
    → HTTP/1.1
    → Target API
    → Metrics
    → Python Result
```

### 14. MVP 成功标准

MVP 不以功能数量作为成功标准，只验证三个问题：

1. **能不能跑** — Python 定义测试，成功启动 Rust Engine，完成压测
2. **能不能高并发** — 至少验证 1K / 10K / 100K VU
3. **是否比 k6 更高效** — 用 Benchmark 证明资源效率优势

---

## 编码原则

### 15. 简单优先

能用简单方案解决的，不用复杂方案。
能在一个文件里写清楚的，不拆成五个文件。
MVP 阶段不过度工程化。

### 16. 代码不是为了展示，是为了执行

每一行代码都必须服务于"多发一个请求"或"少用一 byte 内存"。
不允许为了代码优雅而引入不必要的抽象开销。
