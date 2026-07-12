# GUI 转 native：SwiftUI 应用 + Rust bridge cdylib

M1 的 Tauri 2 + Vue GUI 已可用，但决定在「单配置文件」大改时整体转为原生 SwiftUI 菜单栏应用，UI 之下的逻辑（supervisor 控制协议、core RPC 客户端、安装/冲突检测）保留在 Rust，封装成一个小 C API 的 cdylib（Bridge）供 Swift 调用。

理由：本次大改把 UI 面缩到最小（单配置、只读、两个 tab、一张状态页），是重写成本的历史最低点——同一套 UI 不写两遍；托盘/开机自启/单实例在原生侧是一等公民，比 Tauri 插件模拟更稳；运行时甩掉 WKWebView 进程与 pnpm/vite 前端链。代价：工作量约为 Tauri 重构的 3-4 倍，工具链引入 Xcode/Swift。

否决的替代方案：Swift 重实现 easytier 自研 RPC 协议（跟上游成本不可接受）；shell 出 easytier-cli --output json（进程开销与错误面大，仅作兜底认知）。RPC 必须留在 Rust，这是 Bridge 存在的根本原因。
