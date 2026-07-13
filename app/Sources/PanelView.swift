//
//  PanelView.swift
//  EasyTier
//
//  The menu-bar panel (DESIGN §9): main page = status pill + message lines +
//  rate chart + peer list; the settings page is in-panel navigation. When the
//  supervisor is not installed the content area becomes an install card.
//

import SwiftUI
import Charts

struct PanelView: View {
    @EnvironmentObject private var model: AppModel
    @State private var showSettings = false

    var body: some View {
        Group {
            if showSettings {
                PanelSettingsView(back: { showSettings = false })
            } else {
                MainPage(openSettings: { showSettings = true })
            }
        }
        .frame(width: 430)
        .onAppear {
            // 面板打开时重读重校(DESIGN §9)
            model.onBecomeActive()
        }
    }
}

// MARK: - Main page

private struct MainPage: View {
    @EnvironmentObject private var model: AppModel
    let openSettings: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HeaderBar(openSettings: openSettings)

            if let message = model.statusMessage {
                Text(message)
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
            if model.configChanged {
                Label("配置已修改，重新连接后生效", systemImage: "exclamationmark.triangle.fill")
                    .font(.callout)
                    .foregroundStyle(.orange)
            }

            if let s = model.supervisorStatus, !s.installed {
                InstallGuideCard()
            } else {
                RateSection()
                PeerList()
            }
        }
        .padding(EdgeInsets(top: 12, leading: 14, bottom: 14, trailing: 14))
    }
}

// MARK: - Header (status pill + settings/quit)

private struct HeaderBar: View {
    @EnvironmentObject private var model: AppModel
    let openSettings: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            statusPill

            if !model.validation.ok {
                Label("校验不通过", systemImage: "exclamationmark.circle")
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.red)
                    .help(model.validation.error ?? "配置校验失败")
            }

            Spacer()

            iconButton("gearshape", help: "全部设置", tint: .secondary, action: openSettings)
            iconButton("rectangle.portrait.and.arrow.right", help: "退出 EasyTier", tint: .red) {
                NSApp.terminate(nil)
            }
        }
    }

    /// 状态即按钮:点击 pill 连接/断开(过渡态、校验失败、未安装 supervisor 时禁点)。
    private var statusPill: some View {
        Button(action: { model.toggleConnection() }) {
            HStack(spacing: 7) {
                Circle()
                    .fill(dotColor)
                    .frame(width: 9, height: 9)
                Text(statusLabel)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(textColor)
            }
            .padding(EdgeInsets(top: 5, leading: 10, bottom: 5, trailing: 13))
            .background(RoundedRectangle(cornerRadius: 8).fill(dotColor.opacity(0.12)))
            .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(dotColor.opacity(0.35)))
            .contentShape(RoundedRectangle(cornerRadius: 8))
        }
        .buttonStyle(.plain)
        .disabled(pillDisabled)
        .help(actionHelp)
    }

    private var statusLabel: String {
        switch model.connectionState {
        case .disconnected: return "已断开"
        case .connecting: return "连接中…"
        case .connected: return "已连接"
        case .disconnecting: return "断开中…"
        }
    }

    private var dotColor: Color {
        switch model.connectionState {
        case .connected: return .green
        case .connecting, .disconnecting: return .orange
        case .disconnected: return .gray
        }
    }

    private var textColor: Color {
        model.connectionState == .connected ? .green : .secondary
    }

    private var pillDisabled: Bool {
        if model.connectionState.isTransitioning { return true }
        if model.connectionState == .disconnected && !model.validation.ok { return true }
        if model.supervisorStatus?.installed == false { return true }
        return false
    }

    private var actionHelp: String {
        model.connectionState == .connected ? "断开" : "连接"
    }

    private func iconButton(
        _ systemImage: String, help: String, tint: Color, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Image(systemName: systemImage)
                .font(.system(size: 14))
                .foregroundStyle(tint)
                .frame(width: 28, height: 28)
                .contentShape(RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .help(help)
    }
}

// MARK: - Rate chart

private struct RateSection: View {
    @EnvironmentObject private var model: AppModel

    private var currentRx: Double { model.rateSamples.last?.rx ?? 0 }
    private var currentTx: Double { model.rateSamples.last?.tx ?? 0 }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack {
                Text("速率")
                Spacer()
                Text(Fmt.rate(currentRx + currentTx))
            }
            .font(.system(size: 10))
            .foregroundStyle(.tertiary)

            if model.connectionState == .connected {
                if model.rateSamples.isEmpty {
                    placeholder("正在采集速率数据…")
                } else {
                    chart
                }
            } else {
                placeholder("未连接")
            }

            HStack(spacing: 16) {
                legend(color: .blue, text: "下载 \(Fmt.rate(currentRx))")
                legend(color: .green, text: "上传 \(Fmt.rate(currentTx))")
            }
        }
    }

    private var chart: some View {
        Chart {
            ForEach(model.rateSamples) { sample in
                AreaMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.rx),
                    stacking: .unstacked
                )
                .foregroundStyle(by: .value("方向", "下载"))
                .opacity(0.12)

                AreaMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.tx),
                    stacking: .unstacked
                )
                .foregroundStyle(by: .value("方向", "上传"))
                .opacity(0.12)

                LineMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.rx),
                    series: .value("方向", "下载")
                )
                .foregroundStyle(by: .value("方向", "下载"))

                LineMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.tx),
                    series: .value("方向", "上传")
                )
                .foregroundStyle(by: .value("方向", "上传"))
            }
        }
        .chartForegroundStyleScale(["下载": Color.blue, "上传": Color.green])
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .chartLegend(.hidden)
        .frame(height: 78)
    }

    private func legend(color: Color, text: String) -> some View {
        HStack(spacing: 4) {
            Circle().fill(color).frame(width: 6, height: 6)
            Text(text).font(.system(size: 11)).foregroundStyle(color)
        }
    }

    private func placeholder(_ text: String) -> some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(Color.gray.opacity(0.08))
            .frame(height: 78)
            .overlay(Text(text).font(.caption).foregroundStyle(.secondary))
    }
}

// MARK: - Peer list

/// 固定 6 列(DESIGN §9):节点[主机名+虚拟IPv4]/延迟/下载/上传/丢包/NAT;
/// 路由/协议/版本收进行 hover tooltip。
private struct PeerList: View {
    @EnvironmentObject private var model: AppModel

    private enum Col {
        static let latency: CGFloat = 56
        static let bytes: CGFloat = 62
        static let loss: CGFloat = 42
        static let nat: CGFloat = 76
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            headerRow
            Divider()
            if model.connectionState == .connected && !model.rows.isEmpty {
                VStack(spacing: 0) {
                    ForEach(model.rows) { row in
                        PeerRowView(row: row)
                        Divider().opacity(0.5)
                    }
                }
            } else {
                RoundedRectangle(cornerRadius: 8)
                    .fill(Color.gray.opacity(0.08))
                    .frame(height: 120)
                    .overlay(
                        Text(model.connectionState == .connected ? "暂无节点" : "未连接")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    )
                    .padding(.top, 6)
            }
        }
    }

    private var headerRow: some View {
        HStack(spacing: 6) {
            Text("节点").frame(maxWidth: .infinity, alignment: .leading)
            Text("延迟").frame(width: Col.latency, alignment: .leading)
            Text("下载").frame(width: Col.bytes, alignment: .leading)
            Text("上传").frame(width: Col.bytes, alignment: .leading)
            Text("丢包").frame(width: Col.loss, alignment: .leading)
            Text("NAT").frame(width: Col.nat, alignment: .leading)
        }
        .font(.system(size: 10, weight: .semibold))
        .foregroundStyle(.tertiary)
        .padding(.bottom, 5)
    }

    private struct PeerRowView: View {
        let row: PeerRow

        var body: some View {
            HStack(spacing: 6) {
                VStack(alignment: .leading, spacing: 1) {
                    Text(row.hostname.isEmpty ? "-" : row.hostname)
                        .font(.system(size: 12, weight: .semibold))
                        .foregroundStyle(row.isLocal ? Color.accentColor : .primary)
                        .lineLimit(1)
                    Text(row.ipv4.isEmpty ? "-" : row.ipv4)
                        .font(.system(size: 9.5, design: .monospaced))
                        .foregroundStyle(.tertiary)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
                .frame(maxWidth: .infinity, alignment: .leading)

                Text(Fmt.latency(row.latencyMs))
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(latencyColor)
                    .frame(width: Col.latency, alignment: .leading)
                Text("↓\(Fmt.bytes(row.rxBytes))")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.blue)
                    .frame(width: Col.bytes, alignment: .leading)
                Text("↑\(Fmt.bytes(row.txBytes))")
                    .font(.system(size: 10.5))
                    .foregroundStyle(.green)
                    .frame(width: Col.bytes, alignment: .leading)
                Text(Fmt.loss(row.lossRate))
                    .font(.system(size: 10.5))
                    .foregroundStyle(.secondary)
                    .frame(width: Col.loss, alignment: .leading)
                natBadge
                    .frame(width: Col.nat, alignment: .leading)
            }
            .padding(.vertical, 6)
            .help(tooltip)
        }

        private var tooltip: String {
            let protos = row.protos.isEmpty ? "-" : row.protos.joined(separator: ", ")
            return "路由 \(row.cost) · 协议 \(protos) · 版本 \(row.version.isEmpty ? "-" : row.version)"
        }

        private var latencyColor: Color {
            if row.latencyMs <= 0 { return .secondary }
            if row.latencyMs < 60 { return .green }
            if row.latencyMs < 180 { return .primary }
            return .orange
        }

        private var natBadge: some View {
            Text(row.natType.isEmpty ? "-" : row.natType)
                .font(.system(size: 9.5, weight: .semibold))
                .lineLimit(1)
                .truncationMode(.tail)
                .padding(EdgeInsets(top: 2, leading: 5, bottom: 2, trailing: 5))
                .foregroundStyle(natTint)
                .background(RoundedRectangle(cornerRadius: 5).fill(natTint.opacity(0.14)))
        }

        private var natTint: Color {
            switch row.natType {
            case "OpenInternet", "FullCone": return .green
            case "NoPat", "Restricted", "PortRestricted": return .orange
            case let t where t.hasPrefix("Sym"): return .red
            default: return .secondary
            }
        }
    }
}

// MARK: - Install guide

private struct InstallGuideCard: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: "shield.lefthalf.filled")
                .font(.system(size: 36))
                .foregroundStyle(.secondary)
            Text("尚未安装特权组件")
                .font(.headline)
            Text("EasyTier 需要一个具有管理员权限的后台组件（Supervisor）来创建网络接口与路由。安装时会请求一次管理员密码。")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)

            if model.installing {
                ProgressView().controlSize(.small)
            } else {
                Button("安装") { Task { await model.installSupervisor() } }
                    .keyboardShortcut(.defaultAction)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 24)
    }
}
