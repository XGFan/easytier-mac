#!/bin/bash
#
# app-install.sh - 构建原生 EasyTier.app(SwiftUI + Rust bridge)并安装到 /Applications。
#
# 用法(普通用户终端运行,不要加 sudo):
#   ./app-install.sh                # debug 构建 + 安装(日常开发默认)
#   ./app-install.sh --release     # release 构建 + 安装
#   ./app-install.sh --skip-build  # 跳过构建,直接安装已有产物
#   ./app-install.sh --quit        # 旧实例在运行时,先请它优雅退出
#
# 预期效果:
#   - cargo 构建 bridge 静态库(easytier-mac-bridge)→ xcodegen 生成工程 →
#     xcodebuild 构建 EasyTier.app(ad-hoc 签名);
#   - ditto 覆盖安装到 /Applications/EasyTier.app;
#   - 幂等:可重复执行,旧 .app 被整体替换。
#
# 依赖:Xcode(xcodebuild)、xcodegen(brew)、rust 工具链。
# 注意:GUI 退出会连带终止受管 core(DESIGN.md 生命周期契约),检测到旧实例
# 运行时默认报错退出,加 --quit 才代为退出。Tauri 版(gui-install.sh)与本
# 脚本安装到同一路径/同一 bundle id,二者互斥,装谁谁生效。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
APP_DIR="${REPO_ROOT}/app"
DEST_APP="/Applications/EasyTier.app"
readonly BUNDLE_ID="com.easytier.mac"
# 同时覆盖原生与 Tauri 两代可执行名,防护逻辑对两者一致生效
readonly INSTALLED_GUI_PATTERN='^/Applications/EasyTier\.app/Contents/MacOS/'

log() { echo "[app-install] $*"; }
err() { echo "[app-install] ERROR: $*" >&2; }

PROFILE="debug"
SKIP_BUILD=0
QUIT_RUNNING=0

while [ $# -gt 0 ]; do
  case "$1" in
    --release)
      PROFILE="release"
      shift
      ;;
    --skip-build)
      SKIP_BUILD=1
      shift
      ;;
    --quit)
      QUIT_RUNNING=1
      shift
      ;;
    *)
      err "未知参数: $1(可用: --release --skip-build --quit)"
      exit 1
      ;;
  esac
done

if [ "$(id -u)" -eq 0 ]; then
  err "不要用 sudo 运行:root 会污染 cargo/DerivedData 缓存属主。"
  exit 1
fi

# xcodebuild 的 configuration 与 cargo profile 对应
if [ "$PROFILE" = "release" ]; then
  XCODE_CONFIG="Release"
else
  XCODE_CONFIG="Debug"
fi
BUILD_DIR="${APP_DIR}/build"
BUILT_APP="${BUILD_DIR}/Build/Products/${XCODE_CONFIG}/EasyTier.app"

# ---- 构建 ----
if [ "$SKIP_BUILD" -eq 1 ]; then
  log "跳过构建(--skip-build),直接使用已有产物: $BUILT_APP"
else
  for tool in xcodebuild xcodegen cargo; do
    if ! command -v "$tool" >/dev/null 2>&1; then
      err "找不到 ${tool},无法构建。"
      exit 1
    fi
  done

  log "构建 bridge 静态库(cargo ${PROFILE})..."
  CARGO_ARGS=(build -p easytier-mac-bridge)
  if [ "$PROFILE" = "release" ]; then
    CARGO_ARGS+=(--release)
  fi
  (cd "$REPO_ROOT" && cargo "${CARGO_ARGS[@]}")

  log "生成 Xcode 工程(xcodegen)..."
  (cd "$APP_DIR" && xcodegen generate)

  log "构建 EasyTier.app(xcodebuild ${XCODE_CONFIG})..."
  (cd "$APP_DIR" && xcodebuild \
      -project EasyTier.xcodeproj \
      -scheme EasyTier \
      -configuration "$XCODE_CONFIG" \
      -derivedDataPath "$BUILD_DIR" \
      RUST_PROFILE_DIR="${REPO_ROOT}/target/${PROFILE}" \
      build | tail -5)
fi

if [ ! -d "$BUILT_APP" ] || [ ! -x "${BUILT_APP}/Contents/MacOS/EasyTier" ]; then
  err "构建产物不存在或不完整: $BUILT_APP"
  exit 1
fi

# ---- 替换前处理正在运行的旧实例(原生或 Tauri 版均适用) ----
if pgrep -f "$INSTALLED_GUI_PATTERN" >/dev/null 2>&1; then
  if [ "$QUIT_RUNNING" -ne 1 ]; then
    err "检测到 ${DEST_APP} 正在运行(退出 GUI 会连带停掉受管 core)。"
    err "请先手动退出,或加 --quit 由脚本代为优雅退出。"
    exit 1
  fi
  log "请求正在运行的 EasyTier 退出..."
  osascript -e "tell application id \"${BUNDLE_ID}\" to quit" || true
  WAITED=0
  while pgrep -f "$INSTALLED_GUI_PATTERN" >/dev/null 2>&1; do
    if [ "$WAITED" -ge 10 ]; then
      err "等待 10s 后 GUI 仍未退出,安装终止;请手动退出后重试。"
      exit 1
    fi
    sleep 1
    WAITED=$((WAITED + 1))
  done
  log "旧实例已退出。"
fi

# ---- 安装 ----
log "安装到 ${DEST_APP} ..."
rm -rf "$DEST_APP"
ditto "$BUILT_APP" "$DEST_APP"

# ---- 验证 ----
if [ ! -x "${DEST_APP}/Contents/MacOS/EasyTier" ]; then
  err "安装后校验失败: ${DEST_APP}/Contents/MacOS/EasyTier 不存在或不可执行。"
  exit 1
fi
codesign -dv "$DEST_APP" >/dev/null 2>&1 || {
  err "安装后校验失败: codesign 校验未通过。"
  exit 1
}

log "安装完成(${PROFILE} 构建): ${DEST_APP}"
log "启动: open ${DEST_APP}"
