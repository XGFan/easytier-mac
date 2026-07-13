#!/bin/bash
#
# install.sh - 安装 EasyTier macOS supervisor 为系统级 launchd daemon(按需激活)。
#
# 用法:
#   sudo ./install.sh --supervisor-bin <path> --core-bin <path> --owner-uid <uid>
#
# 预期效果:
#   - 在 /Library/Application Support/EasyTier/ 下建立 bin/、logs/ 目录树,
#     拷贝 easytier-supervisor、easytier-core 两个二进制(root:wheel 0755);
#   - 写入 <安装根>/supervisor.toml(root:wheel 0644);
#   - 写入 /Library/LaunchDaemons/com.easytier.supervisor.plist 并向 launchd 注册
#     (RunAtLoad=false + socket 激活,注册后不应有进程常驻,这是预期);
#   - 幂等:可重复执行,会先卸载旧 launchd 注册、温和终止受管旧进程、覆盖旧文件。
#
# 契约来源:DESIGN.md §1-§3,改动路径/键值前请先同步该文档。

set -euo pipefail

# ---- 常量(严格对齐 DESIGN.md §1-§3,不接受外部覆盖) ----
readonly LABEL="com.easytier.supervisor"
readonly PLIST_PATH="/Library/LaunchDaemons/com.easytier.supervisor.plist"
readonly SOCK_PATH="/var/run/easytier.supervisor.sock"
readonly INSTALL_ROOT="/Library/Application Support/EasyTier"
readonly BIN_DIR="${INSTALL_ROOT}/bin"
readonly LOG_DIR="${INSTALL_ROOT}/logs"
readonly HOOKS_DIR="${INSTALL_ROOT}/hooks"
readonly SUPERVISOR_TOML="${INSTALL_ROOT}/supervisor.toml"
readonly SUPERVISOR_DST="${BIN_DIR}/easytier-supervisor"
readonly CORE_DST="${BIN_DIR}/easytier-core"
# 精确匹配受管 core 的完整路径,字面量写死,不由变量拼接而来,防止误杀
readonly MANAGED_CORE_PATTERN='^/Library/Application Support/EasyTier/bin/easytier-core'

log() { echo "[install] $*"; }
err() { echo "[install] ERROR: $*" >&2; }

# ---- 参数解析(手写 while/case,兼容 bash 3.2,不用 getopts 长选项/关联数组) ----
SUPERVISOR_BIN=""
CORE_BIN=""
OWNER_UID=""

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
    --owner-uid)
      OWNER_UID="${2:-}"
      shift 2
      ;;
    *)
      err "未知参数: $1"
      exit 1
      ;;
  esac
done

# ---- 校验 ----
if [ "$(id -u)" -ne 0 ]; then
  err "必须以 root 运行(sudo $0 ...)"
  exit 1
fi

if [ -z "$SUPERVISOR_BIN" ] || [ -z "$CORE_BIN" ] || [ -z "$OWNER_UID" ]; then
  err "缺少必填参数。用法: sudo $0 --supervisor-bin <path> --core-bin <path> --owner-uid <uid>"
  exit 1
fi

if [ ! -f "$SUPERVISOR_BIN" ] || [ ! -x "$SUPERVISOR_BIN" ]; then
  err "supervisor 二进制不存在或不可执行: $SUPERVISOR_BIN"
  exit 1
fi

if [ ! -f "$CORE_BIN" ] || [ ! -x "$CORE_BIN" ]; then
  err "core 二进制不存在或不可执行: $CORE_BIN"
  exit 1
fi

case "$OWNER_UID" in
  ''|*[!0-9]*)
    err "--owner-uid 必须为非负整数: $OWNER_UID"
    exit 1
    ;;
esac

log "参数校验通过: supervisor-bin=$SUPERVISOR_BIN core-bin=$CORE_BIN owner-uid=$OWNER_UID"

# ---- 预清理:幂等 + 温和处理正在运行的旧实例 ----
# bootout 会停止并注销旧的 supervisor(若存在);找不到注册目标时返回非零,忽略即可
log "卸载旧 launchd 注册(若存在)..."
launchctl bootout "system/${LABEL}" 2>/dev/null || true

# 托管 core 用完整路径精确匹配后 kill,绝不按进程名模糊杀
# (用户机器上可能自己跑着同名的 ~/.bin/easytier-core)
if pgrep -f "$MANAGED_CORE_PATTERN" >/dev/null 2>&1; then
  log "发现受管 easytier-core 残留进程,按完整路径精确终止..."
  pkill -f "$MANAGED_CORE_PATTERN" || true
else
  log "未发现受管 easytier-core 残留进程。"
fi

# ---- 建目录树(属主权限按 DESIGN §1) ----
log "创建目录树..."
mkdir -p "$BIN_DIR"
mkdir -p "$LOG_DIR"
mkdir -p "$HOOKS_DIR"
chown root:wheel "$INSTALL_ROOT" "$BIN_DIR" "$LOG_DIR" "$HOOKS_DIR"
chmod 0755 "$INSTALL_ROOT" "$BIN_DIR" "$LOG_DIR" "$HOOKS_DIR"

# 预创建日志文件并锁定权限为 0644,保证普通用户可读(DESIGN §1);
# 若不预创建,launchd/daemon 首次写入时可能按更严格的 umask 创建文件
touch "${LOG_DIR}/supervisor.err.log" "${LOG_DIR}/core.out.log" "${LOG_DIR}/hooks.log"
chown root:wheel "${LOG_DIR}/supervisor.err.log" "${LOG_DIR}/core.out.log" "${LOG_DIR}/hooks.log"
chmod 0644 "${LOG_DIR}/supervisor.err.log" "${LOG_DIR}/core.out.log" "${LOG_DIR}/hooks.log"

# ---- 拷贝二进制(root:wheel 0755) ----
log "安装二进制..."
install -o root -g wheel -m 0755 "$SUPERVISOR_BIN" "$SUPERVISOR_DST"
install -o root -g wheel -m 0755 "$CORE_BIN" "$CORE_DST"

# ---- 写 supervisor.toml(root:wheel 0644,DESIGN §3) ----
log "写入 supervisor.toml..."
cat > "$SUPERVISOR_TOML" <<EOF
proto = 1
owner_uid = ${OWNER_UID}            # 安装时的用户 uid,连接鉴权用
core_path = "${CORE_DST}"
log_dir = "${LOG_DIR}"
EOF
chown root:wheel "$SUPERVISOR_TOML"
chmod 0644 "$SUPERVISOR_TOML"

# ---- 写 plist(键值严格按 DESIGN §2,SockPathMode=438 即八进制 0666) ----
log "写入 launchd plist..."
cat > "$PLIST_PATH" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>${LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>${SUPERVISOR_DST}</string>
    </array>
    <key>Sockets</key>
    <dict>
        <key>Listeners</key>
        <dict>
            <key>SockPathName</key>
            <string>${SOCK_PATH}</string>
            <key>SockPathMode</key>
            <integer>438</integer>
        </dict>
    </dict>
    <key>RunAtLoad</key>
    <false/>
    <key>KeepAlive</key>
    <false/>
    <key>ThrottleInterval</key>
    <integer>5</integer>
    <key>StandardErrorPath</key>
    <string>${LOG_DIR}/supervisor.err.log</string>
</dict>
</plist>
EOF
chown root:wheel "$PLIST_PATH"
chmod 0644 "$PLIST_PATH"

# ---- 注册到 launchd ----
# 有界重试:紧接着上面的 bootout,launchd 有时还没拆卸完旧注册,
# 此时 bootstrap 会报 "Bootstrap failed: 5: Input/output error"(瞬时竞态)。
# 重试 3 次、间隔 1s 自愈;仍失败则保留非零退出码,不吞错误。
BOOTSTRAP_OK=0
BOOTSTRAP_ATTEMPT=1
while [ "$BOOTSTRAP_ATTEMPT" -le 3 ]; do
  log "注册 launchd daemon(第 ${BOOTSTRAP_ATTEMPT}/3 次)..."
  if launchctl bootstrap system "$PLIST_PATH"; then
    BOOTSTRAP_OK=1
    break
  fi
  BOOTSTRAP_ATTEMPT=$((BOOTSTRAP_ATTEMPT + 1))
  if [ "$BOOTSTRAP_ATTEMPT" -le 3 ]; then
    log "bootstrap 失败,可能是 bootout 拆卸未完成的瞬时竞态,1s 后重试..."
    sleep 1
  fi
done

if [ "$BOOTSTRAP_OK" -ne 1 ]; then
  err "launchctl bootstrap 重试 3 次后仍失败,安装终止。"
  exit 1
fi

log "验证注册..."
PRINT_OUTPUT="$(launchctl print "system/${LABEL}" 2>&1)" || {
  err "launchctl print 失败,注册可能未成功:"
  echo "$PRINT_OUTPUT" >&2
  exit 1
}
echo "$PRINT_OUTPUT"

# RunAtLoad=false + socket 激活:注册成功后不应有进程在跑,这是预期。
# 若检测到 pid 行说明已有连接触发了激活(例如并发测试),给出提示但不视为失败。
if echo "$PRINT_OUTPUT" | grep -qE '^[[:space:]]*pid = [0-9]+'; then
  log "提示: 注册后检测到进程已在运行(可能是有连接触发了按需激活),请核实是否符合预期。"
else
  log "确认: 注册成功且当前无常驻进程(符合按需激活预期)。"
fi

log "安装完成。"
