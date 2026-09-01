#!/usr/bin/env bash
set -e

maturin build --release

echo "构建完成，wheel 文件："
ls target/wheels/*.whl

echo ""
echo "安装请执行："
echo "  pip install $(ls target/wheels/*.whl | head -n1)"
