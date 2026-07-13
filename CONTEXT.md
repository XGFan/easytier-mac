# easytier-mac

macOS 原生 EasyTier 客户端：菜单栏 GUI + 特权 supervisor，架构类似 tailscale 的 GUI/tailscaled 分离。本文件是术语表，只记录概念，不记录实现。

## Language

**Supervisor**：
root 权限、launchd 按需激活的守护进程，负责 spawn/kill Core，生命周期跟随 Owner 连接。
_Avoid_: daemon（泛指）、helper

**Core**：
受 Supervisor 托管的官方 `easytier-core` 进程，GUI 经其 RPC portal 管理。
_Avoid_: 节点进程、后端

**Owner**：
当前持有 Supervisor 控制连接的 GUI 实例；Owner 断连即全停。
_Avoid_: client、session

**配置文件（Config）**：
用户拥有的唯一网络配置 TOML，路径固定。GUI 对它只读（仅缺失时写入一次模板），永不改写用户内容。
_Avoid_: profile（多配置时代的旧词）

**校验（Validate）**：
对配置文件做完整解析检查（TOML 语法 + 网络配置语义），只报告结果，不修改文件。
_Avoid_: 修复、格式化

**连接 / 断开（Connect / Disconnect）**：
GUI 对唯一网络的启停：连接 = 拉起 Core 并运行配置对应的网络实例；断开 = 删除实例并停掉 Core（零常驻）。
_Avoid_: 启动/停止网络实例（多实例时代的旧词）

**面板（Panel）**：
菜单栏图标点开的唯一 GUI 界面，承载主页面（状态/速率/节点）与二级设置页；应用没有主窗口。
_Avoid_: 主窗口（已废除的旧形态）、托盘菜单

**Bridge**：
Rust cdylib，把 Supervisor 控制协议与 Core RPC 封装成小 C API 供 Swift 层调用；只提供机制（协议、RPC、校验、事件），不含策略（重启、恢复、设置归 Swift 层）。
_Avoid_: FFI 层（泛指）、sidecar
