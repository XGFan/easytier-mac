#!/bin/bash
#
# down.sh - EasyTier supervisor "down" hook 示例:core 进程停止/退出后,
# 把系统 DNS 恢复为 DHCP 自动获取(撤销 up.sh 做的修改)。
#
# ---- 安装方法 ----
#   sudo cp down.sh "/Library/Application Support/EasyTier/hooks/down.sh"
#   sudo chown root:wheel "/Library/Application Support/EasyTier/hooks/down.sh"
#   sudo chmod 755 "/Library/Application Support/EasyTier/hooks/down.sh"
# 同目录下的 up.sh 按同样方法部署,负责连接时切换。
#
# supervisor 执行前会校验:属主 root:wheel、常规文件、属主可执行、
# 非 group/world 可写;任一不满足会拒绝执行并记 <安装根>/logs/supervisor.err.log。
# 部署后请务必确认权限(chown/chmod 如上),否则 hook 会被静默拒绝执行。
#
# ---- 触发时机与环境变量(详见 DESIGN.md「Hooks」一节) ----
#   EASYTIER_EVENT=down
#   EASYTIER_REASON 取以下四种之一:
#     requested   —— 用户主动断开(GUI 点断开 / cli stop)
#     owner_drop  —— GUI 控制连接断开(含崩溃),supervisor 收尾
#     core_exited —— core 进程意外退出(如被 kill -9)
#     janitor     —— supervisor 启动时兜底清理上一代残留的 core
#   本示例对四种 reason 一视同仁(统一恢复 DHCP),如需区分行为可读
#   $EASYTIER_REASON 自行分支,例如仅在 requested 时才提示用户。
#
# ---- 幂等要求 ----
# supervisor 可能因崩溃恢复、janitor 兜底等原因重复调用本脚本(甚至在
# DNS 已经是 DHCP 状态时调用),必须可安全重复执行 ——
# networksetup -setdnsservers <service> empty 在已是空的情况下重复执行
# 也不会报错,本脚本天然幂等。

set -euo pipefail

# 与 up.sh 对称:枚举所有当前启用的网络服务并逐一恢复为 DHCP。
# 'empty' 是 networksetup 的关键字参数,不是字面意义上的空字符串,
# 表示"清空手动 DNS,改回从 DHCP/网络配置自动获取"。
networksetup -listallnetworkservices | tail -n +2 | while IFS= read -r service; do
  case "$service" in
    '*'*)
      continue
      ;;
  esac
  networksetup -setdnsservers "$service" empty || true
done

# 服务名固定的场景可省去循环直接写死,与 up.sh 保持一致,例如:
#   networksetup -setdnsservers "Wi-Fi" empty
#   networksetup -setdnsservers "USB 10/100/1G/2.5G LAN" empty

exit 0
