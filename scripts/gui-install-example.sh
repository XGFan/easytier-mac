#!/bin/bash
#
# gui-install-example.sh - 演示 GUI 未来如何"一次密码"完成安装:
# 以当前登录用户身份运行,通过 osascript 的
# `do shell script ... with administrator privileges` 触发一次系统授权对话框,
# 由授权后的 root 身份调用同目录下的 install.sh。
#
# 用法(普通用户终端运行,不要加 sudo):
#   ./gui-install-example.sh --supervisor-bin <path> --core-bin <path>
#
# 预期效果:
#   - 弹出一次 macOS 密码/Touch ID 授权对话框;
#   - 授权通过后以 root 执行:
#       install.sh --supervisor-bin <path> --core-bin <path> --owner-uid <当前用户 uid>
#   - owner-uid 自动取自 `id -u`,不需要用户手动输入。
#
# 转义结构说明(osascript 是本脚本最容易出错的地方,分三层,自查时按层核对):
#   第 1 层 RAW_CMD:实际要在 root shell(/bin/sh)里执行的命令行。用 bash
#     内建 `printf '%q'` 对每个参数(含可能带空格的路径)分别做 shell 转义,
#     再拼接成一整句合法命令,保证含空格路径安全。
#   第 2 层 AS_ESCAPED_CMD:把 RAW_CMD 里的反斜杠和双引号再转义一层
#     (先 \ -> \\,再 " -> \"),使其可以整体塞进 AppleScript 的双引号字符串
#     字面量里而不会提前把字符串截断。两步顺序不能颠倒:必须先转义反斜杠,
#     否则第二步产生的转义反斜杠会被第一步误伤。
#   第 3 层 osascript -e "do shell script \"${AS_ESCAPED_CMD}\" ...":这层
#     本身是 bash 的双引号字符串,bash 只会展开 $AS_ESCAPED_CMD 的值(原样代入,
#     不会对其内容再做一次转义),然后把整个结果作为单个 argv 传给 osascript
#     可执行文件——不会有第二次 shell 重新解析这个字符串,因此不存在“转义
#     被吃掉”的风险。
#
# 契约来源:DESIGN.md §1-§3。本脚本仅为示例/联调用,
# 不在生产 GUI 中直接调用,也不会被本次任务实际执行。

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
INSTALL_SH="${SCRIPT_DIR}/install.sh"

log() { echo "[gui-install-example] $*"; }
err() { echo "[gui-install-example] ERROR: $*" >&2; }

SUPERVISOR_BIN=""
CORE_BIN=""

while [ $# -gt 0 ]; do
  case "$1" in
    --supervisor-bin)
      SUPERVISOR_BIN="${2:-}"
      shift 2
      ;;
    --core-bin)
      CORE_BIN="${2:-}"
      shift 2
      ;;
    *)
      err "未知参数: $1"
      exit 1
      ;;
  esac
done

if [ "$(id -u)" -eq 0 ]; then
  err "此脚本应以普通用户身份运行(权限提升由 osascript 弹窗处理),不要用 sudo 运行。"
  exit 1
fi

if [ -z "$SUPERVISOR_BIN" ] || [ -z "$CORE_BIN" ]; then
  err "用法: $0 --supervisor-bin <path> --core-bin <path>"
  exit 1
fi

if [ ! -f "$INSTALL_SH" ]; then
  err "找不到 install.sh: $INSTALL_SH"
  exit 1
fi

# 弹密码框前先校验二进制路径,避免为明显错误的路径浪费一次系统凭据授权
if [ ! -f "$SUPERVISOR_BIN" ] || [ ! -x "$SUPERVISOR_BIN" ]; then
  err "supervisor 二进制不存在或不可执行: $SUPERVISOR_BIN"
  exit 1
fi

if [ ! -f "$CORE_BIN" ] || [ ! -x "$CORE_BIN" ]; then
  err "core 二进制不存在或不可执行: $CORE_BIN"
  exit 1
fi

OWNER_UID="$(id -u)"
log "当前用户 uid = ${OWNER_UID}"

# ---- 第 1 层:构造 root shell 里实际执行的命令行(逐参数 shell 转义) ----
# 注意:`printf '%q'` 对空格等常见字符输出反斜杠转义(POSIX sh 也认得),
# 但遇到控制字符等异常输入时可能改为输出 bash 专属的 $'...' ANSI-C 引法,
# 这不是标准 POSIX sh 语法。这条链路能工作是因为 macOS `do shell script`
# 实际调用的 /bin/sh 就是 bash 以 POSIX 兼容模式运行,恰好认得 $'...'；
# 换成别的系统/别的 sh 实现(dash 等)不保证成立。仅示例/联调用途可接受,
# 生产 GUI 若要复用这条链路,应改用不依赖 bash 专属引法的转义方式。
q() { printf '%q' "$1"; }

RAW_CMD="$(q "$INSTALL_SH") --supervisor-bin $(q "$SUPERVISOR_BIN") --core-bin $(q "$CORE_BIN") --owner-uid $(q "$OWNER_UID")"

# ---- 第 2 层:为 AppleScript 双引号字符串字面量转义(先反斜杠,后双引号) ----
AS_ESCAPED_CMD="$(printf '%s' "$RAW_CMD" | sed -e 's/\\/\\\\/g' -e 's/"/\\"/g')"

log "即将请求管理员权限执行安装,命令(转义前)为:"
log "  $RAW_CMD"

# ---- 第 3 层:整体作为单个 argv 传给 osascript,不再经过 shell 二次解析 ----
osascript -e "do shell script \"${AS_ESCAPED_CMD}\" with administrator privileges"

log "安装请求已完成(若用户取消授权,osascript 会以非零状态退出并在上面报错)。"
