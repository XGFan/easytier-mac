#!/bin/bash
#
# uninstall.sh - 卸载 EasyTier macOS supervisor:注销 launchd、终止受管进程、
# 删除 plist/控制 socket/整个安装目录。
#
# 用法:
#   sudo ./uninstall.sh
#
# 预期效果:
#   - launchctl bootout 停止并注销 supervisor;
#   - 按完整路径精确终止残留的受管 easytier-core(绝不按进程名模糊匹配,
#     用户机器上可能自己跑着同名的 ~/.bin/easytier-core);
#   - 删除 plist、控制 socket 文件;
#   - 删除安装目录下除 hooks/ 以外的内容(bin/、supervisor.toml、logs/,
#     包括 logs/hooks.log);hooks/ 目录及其中的用户脚本(up.sh/down.sh 等)
#     予以保留 —— 那是用户资产(DNS 切换等自定义逻辑),卸载 supervisor
#     不应该连带销毁用户配置好的 hook 脚本,便于重装后免于重新部署;
#   - 每一步结果逐项回显,便于排查。
#
# 契约来源:DESIGN.md §1。

set -euo pipefail

readonly LABEL="com.easytier.supervisor"

log() { echo "[uninstall] $*"; }
err() { echo "[uninstall] ERROR: $*" >&2; }

if [ "$(id -u)" -ne 0 ]; then
  err "必须以 root 运行(sudo $0)"
  exit 1
fi

# ---- 1. 从 launchd 注销(会先停止 supervisor) ----
log "注销 launchd daemon (${LABEL})..."
if launchctl bootout "system/${LABEL}" 2>/dev/null; then
  log "  -> 已注销。"
else
  log "  -> 未注册或已注销,跳过。"
fi

# ---- 2. 精确终止受管 easytier-core(完整路径匹配,不按进程名模糊杀) ----
if pgrep -f '^/Library/Application Support/EasyTier/bin/easytier-core' >/dev/null 2>&1; then
  log "发现受管 easytier-core 残留进程,按完整路径精确终止..."
  pkill -f '^/Library/Application Support/EasyTier/bin/easytier-core' || true
  log "  -> 已发送终止信号。"
else
  log "未发现受管 easytier-core 残留进程。"
fi

# ---- 3. 精确终止孤儿 easytier-supervisor(完整路径匹配,与 core 处理对称) ----
# 正常情况下 bootout 已经停止了 launchd 管理的 supervisor;这里兜底处理
# plist 缺失/损坏、或此前 bootout 半途失败等场景下可能残留的孤儿进程。
if pgrep -f '^/Library/Application Support/EasyTier/bin/easytier-supervisor' >/dev/null 2>&1; then
  log "发现孤儿 easytier-supervisor 残留进程,按完整路径精确终止..."
  pkill -f '^/Library/Application Support/EasyTier/bin/easytier-supervisor' || true
  log "  -> 已发送终止信号。"
else
  log "未发现孤儿 easytier-supervisor 残留进程。"
fi

# ---- 4. 删除 plist ----
log "删除 plist: /Library/LaunchDaemons/com.easytier.supervisor.plist"
if [ -f "/Library/LaunchDaemons/com.easytier.supervisor.plist" ]; then
  rm -f "/Library/LaunchDaemons/com.easytier.supervisor.plist"
  log "  -> 已删除。"
else
  log "  -> 不存在,跳过。"
fi

# ---- 5. 删除控制 socket 文件 ----
log "删除控制 socket: /var/run/easytier.supervisor.sock"
if [ -e "/var/run/easytier.supervisor.sock" ]; then
  rm -f "/var/run/easytier.supervisor.sock"
  log "  -> 已删除。"
else
  log "  -> 不存在,跳过。"
fi

# ---- 6. 清理安装目录(路径写死字面量,防止误删);保留 hooks/ ----
# hooks/ 目录及其中的用户脚本(up.sh/down.sh 等)故意不删 —— 那是用户
# 资产(DNS 切换等自定义逻辑),卸载 supervisor 不该连带销毁,重装后可直接生效。
log "清理安装目录: /Library/Application Support/EasyTier(保留 hooks/ 及其中脚本)"
if [ -d "/Library/Application Support/EasyTier" ]; then
  rm -rf "/Library/Application Support/EasyTier/bin"
  rm -f "/Library/Application Support/EasyTier/supervisor.toml"
  rm -rf "/Library/Application Support/EasyTier/logs"
  log "  -> 已删除 bin/、supervisor.toml、logs/(含 hooks.log)。"
  if [ -d "/Library/Application Support/EasyTier/hooks" ]; then
    log "  -> hooks/ 及其中的用户脚本予以保留(不删除)。"
  fi
else
  log "  -> 不存在,跳过。"
fi

log "卸载完成。"
