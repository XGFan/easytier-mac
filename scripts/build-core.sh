#!/usr/bin/env bash
#
# build-core.sh - 在 vendor/EasyTier submodule 里构建 easytier-core 二进制。
#
# 用法:
#   scripts/build-core.sh             # debug 构建(日常开发/m0 验证脚本默认)
#   scripts/build-core.sh --release   # release 构建(安装 supervisor 用)
#
# 产物:
#   vendor/EasyTier/target/<profile>/easytier-core
#   (submodule 有自己的 workspace,target 目录也在 submodule 内,
#    与本仓库根的 target/ 互不干扰。)
#
# 说明:core 二进制必须从 vendor/EasyTier(XGFan fork,带 macOS 全隧道修复)
# 构建,与 bridge 链接的 easytier 库同一 commit,保证 RPC 协议匹配;
# 不要用上游发布的官方二进制替代。构建依赖 protoc(brew install protobuf)。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_MANIFEST="${REPO_ROOT}/vendor/EasyTier/Cargo.toml"

if [ ! -f "$VENDOR_MANIFEST" ]; then
  echo "[build-core] ERROR: vendor/EasyTier 为空,先执行: git submodule update --init" >&2
  exit 1
fi

CARGO_ARGS=(build --manifest-path "$VENDOR_MANIFEST" -p easytier --bin easytier-core)
PROFILE="debug"
if [ "${1:-}" = "--release" ]; then
  CARGO_ARGS+=(--release)
  PROFILE="release"
elif [ -n "${1:-}" ]; then
  echo "[build-core] ERROR: 未知参数: $1(可用: --release)" >&2
  exit 1
fi

echo "[build-core] cargo ${CARGO_ARGS[*]}"
cargo "${CARGO_ARGS[@]}"
echo "[build-core] 产物: ${REPO_ROOT}/vendor/EasyTier/target/${PROFILE}/easytier-core"
