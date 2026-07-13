#!/usr/bin/env bash
#
# fetch-official-core.sh - 下载官方 EasyTier release 的 easytier-core 二进制。
#
# 适用场景:配置不涉及全隧道(0.0.0.0/0 拆分路由 / exit node)时,官方
# 同版本二进制与 fork 构建的 core 行为一致——fork 的 macOS 修复不改
# RPC/配置面,且只在全隧道场景生效——可以省去本地编译。
# 全隧道场景必须用 scripts/build-core.sh 构建 fork 版,
# 见 README「对 EasyTier 的依赖」。
#
# 用法:
#   scripts/fetch-official-core.sh            # 版本默认取 vendor pin(easytier crate 版本)
#   scripts/fetch-official-core.sh 2.6.4      # 显式指定版本
#
# 产物:target/official/easytier-core
#
# 替换已安装的 core(GUI 先断开连接,重连后生效):
#   sudo install -o root -g wheel -m 0755 target/official/easytier-core \
#     "/Library/Application Support/EasyTier/bin/easytier-core"
# 或全新安装时直接把 --core-bin 指到本产物(scripts/install.sh)。

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

log() { echo "[fetch-official-core] $*"; }
err() { echo "[fetch-official-core] ERROR: $*" >&2; }

VERSION="${1:-}"
if [ -z "$VERSION" ]; then
  VENDOR_MANIFEST="${REPO_ROOT}/vendor/EasyTier/easytier/Cargo.toml"
  if [ ! -f "$VENDOR_MANIFEST" ]; then
    err "vendor/EasyTier 为空且未指定版本;先 git submodule update --init,或显式传版本号。"
    exit 1
  fi
  VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' "$VENDOR_MANIFEST" | head -1)"
  if [ -z "$VERSION" ]; then
    err "无法从 ${VENDOR_MANIFEST} 解析版本号,请显式传版本号。"
    exit 1
  fi
  log "版本取自 vendor pin: v${VERSION}"
fi

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64)        ARCH="x86_64" ;;
  *) err "不支持的架构: $(uname -m)"; exit 1 ;;
esac

# 官方 release 资产命名:easytier-macos-<arch>-v<version>.zip,
# 解包后二进制在 easytier-macos-<arch>/ 子目录下。
URL="https://github.com/EasyTier/EasyTier/releases/download/v${VERSION}/easytier-macos-${ARCH}-v${VERSION}.zip"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

log "下载 ${URL}"
curl -fL --retry 2 -o "${TMP}/core.zip" "$URL"
unzip -oq "${TMP}/core.zip" -d "$TMP"

SRC="${TMP}/easytier-macos-${ARCH}/easytier-core"
if [ ! -f "$SRC" ]; then
  # 命名布局变动时兜底搜一次
  SRC="$(find "$TMP" -type f -name easytier-core | head -1)"
fi
if [ -z "$SRC" ] || [ ! -f "$SRC" ]; then
  err "压缩包里找不到 easytier-core。"
  exit 1
fi

DEST_DIR="${REPO_ROOT}/target/official"
mkdir -p "$DEST_DIR"
install -m 0755 "$SRC" "${DEST_DIR}/easytier-core"

# 冒烟:能执行且报出版本
REPORTED="$("${DEST_DIR}/easytier-core" --version 2>&1 | head -1 || true)"
log "产物: ${DEST_DIR}/easytier-core(${REPORTED})"
log "替换已安装的 core(GUI 先断开连接):"
log "  sudo install -o root -g wheel -m 0755 '${DEST_DIR}/easytier-core' '/Library/Application Support/EasyTier/bin/easytier-core'"
