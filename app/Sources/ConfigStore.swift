//
//  ConfigStore.swift
//  EasyTier
//
//  The single, user-owned config file (DESIGN §9, ADR 0002):
//    ~/Library/Application Support/EasyTier/config.toml
//
//  The GUI is read-only over this file. It writes a commented template ONLY when
//  the file is absent, and never rewrites existing content.
//

import Foundation

/// Template written on first launch when config.toml is missing. It doubles as
/// the format documentation for the user (ADR 0002: the GUI never edits the
/// file, so this is the only place format knowledge reaches the user).
/// Field names verified against easytier/src/common/config.rs.
private let TEMPLATE_TOML = """
# EasyTier 配置文件
# 本文件由 EasyTier.app 首次启动时生成,此后完全归你所有:GUI 只读、校验、连接,永不改写。
# 修改后切回 EasyTier 窗口会自动重新校验;连接中修改的内容在重新连接后生效。
# 完整字段参考仓库 easytier/src/common/config.rs,或 https://easytier.cn 文档。

# 本机在虚拟网络中的主机名(其他节点看到的名字)
hostname = "my-mac"

# 虚拟网卡 IPv4;或删除本行并改 dhcp = true 由网络自动分配
ipv4 = "10.0.0.1"
dhcp = false

[network_identity]
# 同一网络的所有节点必须一致
network_name = "your-network-name"
network_secret = "your-network-secret"

# 要主动连接的节点(可多条,支持 tcp/udp/ws/wss/quic)
[[peer]]
uri = "tcp://public.easytier.cn:11010"

# [[peer]]
# uri = "udp://your-server:11010"

# 希望其他节点能主动连入本机时,配置监听地址
# listeners = ["tcp://0.0.0.0:11010", "udp://0.0.0.0:11010"]

# 手动路由(全隧道示例:配合 exit_nodes 把 0.0.0.0/0 走出口节点)
# routes = ["0.0.0.0/0"]
# exit_nodes = ["10.0.0.2"]

# 子网代理:把本机可达的局域网段共享给整个网络
# [[proxy_network]]
# cidr = "192.168.2.0/24"

[flags]
# 常用可选开关:
# latency_first = true       # 延迟优先选路
# enable_kcp_proxy = true    # KCP 代理加速 TCP 流
# no_tun = true              # 不创建虚拟网卡(纯中继/调试用)
"""

/// Result of validating the config text (thin wrapper over `ValidateResult`).
struct ConfigValidation: Sendable {
    let ok: Bool
    let error: String?

    static let unknown = ConfigValidation(ok: false, error: nil)
    static func from(_ r: ValidateResult) -> ConfigValidation {
        ConfigValidation(ok: r.ok, error: r.error)
    }
}

/// Fixed-path config file accessor. Value type; all methods are pure filesystem
/// operations safe to call from any thread.
struct ConfigStore: Sendable {
    /// ~/Library/Application Support/EasyTier
    let directory: URL
    /// ~/Library/Application Support/EasyTier/config.toml
    let fileURL: URL

    init() {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        self.directory = base.appendingPathComponent("EasyTier", isDirectory: true)
        self.fileURL = directory.appendingPathComponent("config.toml", isDirectory: false)
    }

    /// True if the config file exists on disk.
    var exists: Bool {
        FileManager.default.fileExists(atPath: fileURL.path)
    }

    /// Write the template iff the file is absent. Never overwrites existing
    /// content. Creates the containing directory as needed.
    func ensureTemplate() {
        guard !exists else { return }
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? TEMPLATE_TOML.write(to: fileURL, atomically: true, encoding: .utf8)
    }

    /// Read the current file contents. Throws if the file is missing/unreadable.
    func read() throws -> String {
        try String(contentsOf: fileURL, encoding: .utf8)
    }
}
