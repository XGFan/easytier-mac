//
//  AppModel.swift
//  EasyTier
//
//  Policy layer (DESIGN §9: "策略归 Swift"). Owns the connection state machine
//  and the policies the Bridge deliberately does not: auto-restart on unexpected
//  core exit, startup restore, focus revalidation, settings persistence, and the
//  busy→takeover confirmation. Mechanism (supervisor protocol, RPC, validate,
//  events) lives in the Rust Bridge behind `BridgeClient`.
//

import SwiftUI
import AppKit

/// Connection lifecycle for the single network (CONTEXT.md: 连接/断开).
enum ConnectionState: Sendable {
    case disconnected
    case connecting
    case connected
    case disconnecting

    /// A transient state where the connect/disconnect control should be disabled.
    var isTransitioning: Bool { self == .connecting || self == .disconnecting }
}

/// One rx/tx rate sample for the throughput chart.
struct RateSample: Identifiable, Sendable {
    let id = UUID()
    let time: Date
    /// Download (received) bytes per second.
    let rx: Double
    /// Upload (transmitted) bytes per second.
    let tx: Double
}

@MainActor
final class AppModel: ObservableObject {
    /// Give up auto-restart after this many consecutive failures.
    private static let maxAutoRestart = 3
    /// Retained throughput samples (~3 min at 1 Hz).
    private static let maxRateSamples = 180

    let bridge: BridgeClient
    let config: ConfigStore

    // MARK: Published UI state

    @Published private(set) var connectionState: ConnectionState = .disconnected
    @Published private(set) var validation: ConfigValidation = .unknown
    @Published private(set) var rows: [PeerRow] = []
    @Published private(set) var supervisorStatus: SupervisorStatus?
    @Published private(set) var rateSamples: [RateSample] = []
    /// Set when the on-disk config differs from the running config (DESIGN §9:
    /// "配置已修改，重新连接后生效").
    @Published private(set) var configChanged = false
    /// Human-readable status/error line shown in the network page.
    @Published var statusMessage: String?
    @Published private(set) var installing = false
    @Published private(set) var uninstalling = false

    /// Auto-restart toggle (persisted). Mechanism default is on (DESIGN §8).
    @Published var autoRestartEnabled: Bool {
        didSet { defaults.set(autoRestartEnabled, forKey: Keys.autoRestart) }
    }

    /// 启动 app 时自动连接。与「开机自启」(登录项)相互独立:两者都开 = 开机即连;
    /// 只开本项 = 手动启动 app 后自动连。
    @Published var autoConnectOnLaunch: Bool {
        didSet { defaults.set(autoConnectOnLaunch, forKey: Keys.autoConnectOnLaunch) }
    }

    // MARK: Internal state

    private let defaults = UserDefaults.standard
    /// The TOML currently running, cached to diff against on-disk changes.
    private var runningToml: String?
    private var restartFailures = 0
    private var didAttemptRestore = false

    // Rate-diff baseline.
    private var lastRxTotal: Double?
    private var lastTxTotal: Double?
    private var lastSampleTime: Date?

    private var eventTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    private var restartTask: Task<Void, Never>?

    private enum Keys {
        static let autoRestart = "autoRestart"
        static let autoConnectOnLaunch = "autoConnectOnLaunch"
    }

    // MARK: Init

    init(bridge: BridgeClient, config: ConfigStore) {
        self.bridge = bridge
        self.config = config
        defaults.register(defaults: [Keys.autoRestart: true, Keys.autoConnectOnLaunch: true])
        self.autoRestartEnabled = defaults.bool(forKey: Keys.autoRestart)
        self.autoConnectOnLaunch = defaults.bool(forKey: Keys.autoConnectOnLaunch)
        // 旧版隐式恢复开关,已被 autoConnectOnLaunch 显式开关取代
        defaults.removeObject(forKey: "wasConnected")

        config.ensureTemplate()

        eventTask = Task { [weak self] in
            guard let events = self?.bridge.events else { return }
            for await event in events {
                self?.handle(event)
            }
        }

        Task { await self.revalidate() }
        Task { await self.refreshSupervisorStatus() }
    }

    // MARK: - User actions

    /// Toggle connect/disconnect from a single control.
    func toggleConnection() {
        switch connectionState {
        case .disconnected:
            Task { await connect() }
        case .connected:
            Task { await disconnect() }
        case .connecting, .disconnecting:
            break
        }
    }

    /// Connect: read → validate → run the network instance.
    func connect() async {
        guard connectionState == .disconnected else { return }
        restartTask?.cancel()
        connectionState = .connecting
        statusMessage = nil

        guard let text = try? config.read() else {
            connectionState = .disconnected
            statusMessage = "配置文件读取失败"
            return
        }

        let v = await bridge.validate(toml: text)
        validation = .from(v)
        guard v.ok else {
            connectionState = .disconnected
            statusMessage = "配置校验失败：\(v.error ?? "未知错误")"
            return
        }

        if let err = await bridge.connect(toml: text) {
            connectionState = .disconnected
            statusMessage = "连接失败：\(err)"
            return
        }

        runningToml = text
        configChanged = false
        restartFailures = 0
        statusMessage = nil
        connectionState = .connected
        startPolling()
        Task { await refreshSupervisorStatus() }
    }

    /// Disconnect: delete the instance + stop the core (zero residency).
    func disconnect() async {
        guard connectionState == .connected || connectionState == .connecting else { return }
        restartTask?.cancel()
        connectionState = .disconnecting
        stopPolling()

        _ = await bridge.disconnect()

        runningToml = nil
        configChanged = false
        restartFailures = 0
        rows = []
        rateSamples = []
        resetRateBaseline()
        statusMessage = nil
        connectionState = .disconnected
    }

    /// Re-read + revalidate the config file (startup and on window activation).
    func revalidate() async {
        guard let text = try? config.read() else {
            validation = ConfigValidation(ok: false, error: "配置文件读取失败")
            return
        }
        let v = await bridge.validate(toml: text)
        validation = .from(v)
        if connectionState == .connected, let running = runningToml {
            configChanged = (text != running)
        }
    }

    /// Called from `NSApplication.didBecomeActiveNotification`.
    func onBecomeActive() {
        Task { await revalidate() }
        Task { await refreshSupervisorStatus() }
    }

    func openConfigInEditor() {
        NSWorkspace.shared.open(config.fileURL)
    }

    func revealConfigInFinder() {
        NSWorkspace.shared.activateFileViewerSelecting([config.fileURL])
    }

    // MARK: - Supervisor install/uninstall

    func refreshSupervisorStatus() async {
        if let s = await bridge.supervisorStatus() {
            supervisorStatus = s
        }
    }

    func installSupervisor() async {
        guard !installing else { return }
        installing = true
        statusMessage = nil
        let err = await bridge.install()
        installing = false
        if let err {
            statusMessage = "安装失败：\(err)"
        } else {
            await refreshSupervisorStatus()
        }
    }

    func uninstallSupervisor() async {
        guard !uninstalling else { return }
        uninstalling = true
        let err = await bridge.uninstall()
        uninstalling = false
        if let err {
            statusMessage = "卸载失败：\(err)"
        } else {
            await refreshSupervisorStatus()
        }
    }

    // MARK: - Teardown

    /// Graceful shutdown; call from `applicationWillTerminate`.
    func shutdown() {
        eventTask?.cancel()
        pollTask?.cancel()
        restartTask?.cancel()
        bridge.shutdown()
    }

    // MARK: - Event handling

    private func handle(_ event: BridgeEvent) {
        switch event {
        case .connected:
            Task { await refreshSupervisorStatus() }
            startupRestoreIfNeeded()
        case .disconnected:
            Task { await refreshSupervisorStatus() }
        case .coreStarted, .coreStopped:
            break
        case .coreExited:
            handleCoreExit()
        case .busy:
            promptTakeover()
        case .kicked:
            restartTask?.cancel()
            stopPolling()
            connectionState = .disconnected
            runningToml = nil
            rows = []
            rateSamples = []
            resetRateBaseline()
            statusMessage = "已被其他会话接管，连接已断开"
        case let .error(code, message):
            statusMessage = "错误（\(code)）：\(message)"
        case .unknown, .malformed:
            break
        }
    }

    /// On first supervisor connect, auto-connect when the user enabled
    /// 「启动时自动连接」(explicit toggle, independent of the login item).
    private func startupRestoreIfNeeded() {
        guard !didAttemptRestore else { return }
        didAttemptRestore = true
        guard autoConnectOnLaunch, connectionState == .disconnected else { return }
        Task { await connect() }
    }

    /// Auto-restart after an unexpected `core_exited` (DESIGN §8/§9). Consecutive
    /// failures back off by N×1s; three in a row give up. A manual connect resets
    /// the counter.
    private func handleCoreExit() {
        guard connectionState == .connected, let toml = runningToml else { return }

        stopPolling()
        rows = []
        resetRateBaseline()

        guard autoRestartEnabled else {
            connectionState = .disconnected
            statusMessage = "Core 已退出（自动重启已关闭）"
            return
        }

        connectionState = .connecting
        restartTask?.cancel()
        restartTask = Task { [weak self] in
            guard let self else { return }
            while true {
                let attempt = self.restartFailures + 1
                if attempt > Self.maxAutoRestart {
                    self.connectionState = .disconnected
                    self.runningToml = nil
                    self.statusMessage = "自动重连失败，已停止（连续 \(Self.maxAutoRestart) 次）"
                    return
                }
                self.statusMessage = "Core 已退出，正在自动重连（第 \(attempt) 次）…"
                try? await Task.sleep(for: .seconds(Double(attempt)))
                if Task.isCancelled { return }

                let err = await self.bridge.connect(toml: toml)
                if Task.isCancelled { return }
                if err == nil {
                    self.restartFailures = 0
                    self.connectionState = .connected
                    self.statusMessage = "已自动重连"
                    self.startPolling()
                    return
                } else {
                    self.restartFailures += 1
                }
            }
        }
    }

    /// Modal confirmation before taking over another instance's owner lease.
    private func promptTakeover() {
        let alert = NSAlert()
        alert.messageText = "检测到另一个 EasyTier 会话"
        alert.informativeText = "另一个 EasyTier 会话正持有控制连接，是否接管？接管会断开对方。"
        alert.addButton(withTitle: "接管")
        alert.addButton(withTitle: "取消")
        alert.alertStyle = .warning
        if alert.runModal() == .alertFirstButtonReturn {
            bridge.takeover()
        }
    }

    // MARK: - Status polling + rate

    private func startPolling() {
        stopPolling()
        resetRateBaseline()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.poll()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    private func poll() async {
        guard connectionState == .connected else { return }
        let env = await bridge.status()
        guard connectionState == .connected else { return }

        if let ns = env?.ok {
            rows = ns.rows.sorted { lhs, rhs in
                if lhs.isLocal != rhs.isLocal { return lhs.isLocal }
                return lhs.peerId < rhs.peerId
            }
            recordRate(rows: ns.rows)
        } else {
            rows = []
        }
    }

    private func recordRate(rows: [PeerRow]) {
        let totalRx = rows.reduce(0.0) { $0 + Double($1.rxBytes) }
        let totalTx = rows.reduce(0.0) { $0 + Double($1.txBytes) }
        let now = Date()
        defer {
            lastRxTotal = totalRx
            lastTxTotal = totalTx
            lastSampleTime = now
        }
        guard let lastRx = lastRxTotal, let lastTx = lastTxTotal, let lastTime = lastSampleTime else {
            return // first sample establishes the baseline only
        }
        let dt = now.timeIntervalSince(lastTime)
        guard dt > 0 else { return }
        // Counters may reset on reconnect → clamp negatives to zero.
        let rxRate = max(0, totalRx - lastRx) / dt
        let txRate = max(0, totalTx - lastTx) / dt
        rateSamples.append(RateSample(time: now, rx: rxRate, tx: txRate))
        if rateSamples.count > Self.maxRateSamples {
            rateSamples.removeFirst(rateSamples.count - Self.maxRateSamples)
        }
    }

    private func resetRateBaseline() {
        lastRxTotal = nil
        lastTxTotal = nil
        lastSampleTime = nil
    }
}
