# Loadforge 0.1.0 安装说明（macOS Apple Silicon）

本目录是 Loadforge 的源码发布包。由于 PyO3 扩展是平台相关的二进制，
需要在目标机器（macOS ARM64）上编译出 wheel。

## 前置条件

- Python 3.10+（请用原生 arm64 的 Python，勿用 Rosetta 的 x86_64 Python）
- Rust 工具链
- Xcode Command Line Tools（提供 C 编译器，`ring` 构建需要）

## 一次性安装依赖

```bash
# C 编译器（已装过可跳过）
xcode-select --install

# Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"

# maturin
pip install "maturin>=1,<2"
```

## 构建 + 安装

```bash
cd <本目录>
bash build_macos.sh
```

或手动：

```bash
maturin build --release
pip install target/wheels/loadforge-0.1.0-cpXXX-cpXXX-macosx_11_0_arm64.whl
```

## 验证

```python
import loadforge

result = loadforge.run({
    "base_url": "http://127.0.0.1:8080",
    "vu": 10, "duration": 2,
    "endpoints": [{"method": "GET", "path": "/", "weight": 1}],
})
print(result)
```

## 常见问题

- `pip install` 报“找不到匹配的 wheel”：先确认 Python 是 arm64：

  ```bash
  python -c "import platform; print(platform.machine())"
  ```

  应输出 `arm64`；若是 `x86_64` 说明用了 Rosetta 的 Python，请换成原生 arm64 Python。

- 提示缺 C 编译器：执行 `xcode-select --install`。
