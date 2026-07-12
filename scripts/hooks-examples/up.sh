#!/bin/bash
#
# up.sh - EasyTier supervisor "up" hook 示例:core 进程启动后,把系统 DNS
# 切到目标网络的私有 DNS 服务器(例如 EasyTier 网络自带的 Magic DNS)。
#
# ---- 安装方法 ----
#   sudo cp up.sh "/Library/Application Support/EasyTier/hooks/up.sh"
#   sudo chown root:wheel "/Library/Application Support/EasyTier/hooks/up.sh"
#   sudo chmod 755 "/Library/Application Support/EasyTier/hooks/up.sh"
# 同目录下的 down.sh 按同样方法部署,负责断开时恢复。
#
# supervisor 执行前会校验:属主 root:wheel、常规文件、属主可执行、
# 非 group/world 可写;任一不满足会拒绝执行并记 <安装根>/logs/supervisor.err.log。
# 部署后请务必确认权限(chown/chmod 如上),否则 hook 会被静默拒绝执行。
#
# ---- 触发时机与环境变量(详见 easytier-mac/DESIGN.md「Hooks」一节) ----
#   EASYTIER_EVENT=up   —— core 进程已被 supervisor spawn
#   注意:这个时机只保证 core 进程已启动,不保证虚拟网卡(TUN)/路由已
#   配置完成。多数 DNS 切换场景不受影响;但如果你的脚本要等网络真正
#   连通(例如先 ping 通对端网关再动作),需要自行重试,见文末示例。
#
# ---- 幂等要求 ----
# supervisor 可能因崩溃恢复、janitor 兜底等原因重复调用本脚本,必须可
# 安全重复执行。本脚本每次都是"设置为目标值"而非"追加/切换",天然幂等。
#
# ---- 修改指南 ----
# 下面的 DNS 服务器地址 10.126.126.1 是占位符,改成你自己 EasyTier
# 网络里的实际 DNS / Magic DNS 地址。

set -euo pipefail

readonly TARGET_DNS="10.126.126.1"   # 改成你自己的 DNS 服务器地址

# 用 networksetup -listallnetworkservices 枚举所有网络服务并逐一设置,
# 而不是写死 "Wi-Fi" / "USB 10/100/1G/2.5G LAN" 这类具体服务名 ——
# 有线网卡的服务名因机型、驱动、转接器品牌而异,同一台机器换个 USB-C
# 转接头服务名都可能变。循环处理所有"当前启用"的服务更稳,新增/更换
# 网卡也不用改脚本。
#
# 输出第一行是提示语(An asterisk (*) denotes that a network service is
# disabled),用 tail -n +2 跳过;被禁用的服务名前会带 "*" 前缀,也要
# 跳过。IFS= + read -r 保留服务名里的空格(如上面提到的有线服务名)。
networksetup -listallnetworkservices | tail -n +2 | while IFS= read -r service; do
  case "$service" in
    '*'*)
      continue
      ;;
  esac
  networksetup -setdnsservers "$service" "$TARGET_DNS" || true
done

# 若你的服务名固定且明确知道有哪些(例如只用 Wi-Fi,不接有线),也可以
# 省去上面的循环直接写死,例如:
#   networksetup -setdnsservers "Wi-Fi" "$TARGET_DNS"
#   networksetup -setdnsservers "USB 10/100/1G/2.5G LAN" "$TARGET_DNS"
# 但如前所述,有线服务名因机器而异,不推荐硬编码,上面的循环写法更稳。

# 若需要等 TUN /网络就绪后再动作(本示例的 DNS 切换不需要),可参考:
#   for i in $(seq 1 10); do
#     ping -c1 -t1 <网关或对端IP> >/dev/null 2>&1 && break
#     sleep 1
#   done

exit 0
