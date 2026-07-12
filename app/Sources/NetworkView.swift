//
//  NetworkView.swift
//  EasyTier
//
//  网络 tab: connection bar + throughput chart + peer table (DESIGN §9 layout).
//  When the supervisor is not installed the whole page becomes an install card.
//

import SwiftUI
import Charts

struct NetworkView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        Group {
            if let s = model.supervisorStatus, !s.installed {
                InstallGuideCard()
            } else {
                content
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onAppear { Task { await model.refreshSupervisorStatus() } }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 16) {
            ConnectionBar()
            RateChart()
            PeerTable()
        }
    }
}

// MARK: - Connection bar

private struct ConnectionBar: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                Circle()
                    .fill(statusColor)
                    .frame(width: 10, height: 10)
                Text(model.config.fileURL.lastPathComponent)
                    .font(.headline)

                validationBadge

                Spacer()

                Button("打开配置") { model.openConfigInEditor() }
                Button("在 Finder 中显示") { model.revealConfigInFinder() }
                Button(connectTitle) { model.toggleConnection() }
                    .keyboardShortcut(.defaultAction)
                    .disabled(connectDisabled)
            }

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
        }
        .padding(12)
        .background(RoundedRectangle(cornerRadius: 10).fill(Color(nsColor: .controlBackgroundColor)))
    }

    private var statusColor: Color {
        switch model.connectionState {
        case .connected: return .green
        case .connecting, .disconnecting: return .orange
        case .disconnected: return .secondary
        }
    }

    @ViewBuilder
    private var validationBadge: some View {
        if model.validation.ok {
            Label("校验通过", systemImage: "checkmark.circle.fill")
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.green)
                .font(.callout)
        } else {
            // Bridge 保证错误是单行摘要(friendly_config_error);行数限制只是兜底,
            // 完整信息保留在悬停 tooltip 里。
            Label(model.validation.error ?? "校验失败", systemImage: "xmark.circle.fill")
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.red)
                .font(.callout)
                .lineLimit(2)
                .truncationMode(.tail)
                .help(model.validation.error ?? "")
        }
    }

    private var connectTitle: String {
        switch model.connectionState {
        case .disconnected: return "连接"
        case .connecting: return "连接中…"
        case .connected: return "断开"
        case .disconnecting: return "断开中…"
        }
    }

    private var connectDisabled: Bool {
        if model.connectionState.isTransitioning { return true }
        if model.connectionState == .disconnected && !model.validation.ok { return true }
        return false
    }
}

// MARK: - Throughput chart

private struct RateChart: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("速率").font(.subheadline).foregroundStyle(.secondary)
            if model.connectionState == .connected {
                if model.rateSamples.isEmpty {
                    placeholder("正在采集速率数据…")
                } else {
                    chart
                }
            } else {
                placeholder("未连接")
            }
        }
    }

    private var chart: some View {
        Chart {
            ForEach(model.rateSamples) { sample in
                LineMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.rx),
                    series: .value("方向", "下载")
                )
                .foregroundStyle(by: .value("方向", "下载"))
            }
            ForEach(model.rateSamples) { sample in
                LineMark(
                    x: .value("时间", sample.time),
                    y: .value("速率", sample.tx),
                    series: .value("方向", "上传")
                )
                .foregroundStyle(by: .value("方向", "上传"))
            }
        }
        .chartForegroundStyleScale(["下载": Color.blue, "上传": Color.green])
        .chartYAxis {
            AxisMarks { value in
                AxisGridLine()
                AxisValueLabel {
                    if let bytes = value.as(Double.self) {
                        Text(Fmt.rate(bytes))
                    }
                }
            }
        }
        .chartXAxis {
            AxisMarks(values: .automatic(desiredCount: 4)) { _ in
                AxisGridLine()
            }
        }
        .frame(height: 160)
    }

    private func placeholder(_ text: String) -> some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(Color(nsColor: .controlBackgroundColor))
            .frame(height: 160)
            .overlay(Text(text).foregroundStyle(.secondary))
    }
}

// MARK: - Peer table

private struct PeerTable: View {
    @EnvironmentObject private var model: AppModel
    /// 列显隐/顺序配置。表头右键可勾选展示列;持久化到 UserDefaults 跨重启记住。
    @State private var columnCustomization = PeerTable.loadCustomization()

    private static let customizationKey = "peerTableColumns"

    private static func loadCustomization() -> TableColumnCustomization<PeerRow> {
        guard let data = UserDefaults.standard.data(forKey: customizationKey),
              let saved = try? JSONDecoder().decode(
                  TableColumnCustomization<PeerRow>.self, from: data)
        else { return TableColumnCustomization<PeerRow>() }
        return saved
    }

    private static func saveCustomization(_ value: TableColumnCustomization<PeerRow>) {
        if let data = try? JSONEncoder().encode(value) {
            UserDefaults.standard.set(data, forKey: customizationKey)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("节点").font(.subheadline).foregroundStyle(.secondary)
            if model.connectionState == .connected && !model.rows.isEmpty {
                table
            } else {
                placeholder(model.connectionState == .connected ? "暂无节点" : "未连接")
            }
        }
    }

    private var table: some View {
        // 「虚拟 IPv4」不带 customizationID = 锚点列,永不可隐藏,避免全空表格。
        Table(model.rows, columnCustomization: $columnCustomization) {
            TableColumn("虚拟 IPv4") { row in cell(row.ipv4, row) }
            TableColumn("主机名") { row in
                // 本机行靠颜色区分(蓝色),不加文字标注
                cell(row.hostname, row)
            }
            .customizationID("hostname")
            TableColumn("路由") { row in cell(row.cost, row) }
                .customizationID("cost")
            TableColumn("协议") { row in cell(row.protos.joined(separator: ", "), row) }
                .customizationID("protos")
            TableColumn("延迟") { row in cell(Fmt.latency(row.latencyMs), row) }
                .customizationID("latency")
            TableColumn("上传") { row in cell(Fmt.bytes(row.txBytes), row) }
                .customizationID("tx")
            TableColumn("下载") { row in cell(Fmt.bytes(row.rxBytes), row) }
                .customizationID("rx")
            TableColumn("丢包") { row in cell(Fmt.loss(row.lossRate), row) }
                .customizationID("loss")
            TableColumn("NAT") { row in cell(row.natType, row) }
                .customizationID("nat")
            TableColumn("版本") { row in cell(row.version, row) }
                .customizationID("version")
        }
        .onChange(of: columnCustomization) { _, newValue in
            Self.saveCustomization(newValue)
        }
        // 内容未超出可视区域时不允许滚动/回弹(双指拖动不产生位移)
        .scrollBounceBehavior(.basedOnSize, axes: [.vertical, .horizontal])
        .frame(minHeight: 180)
    }

    private func cell(_ text: String, _ row: PeerRow) -> some View {
        Text(text.isEmpty ? "-" : text)
            .fontWeight(row.isLocal ? .semibold : .regular)
            .foregroundStyle(row.isLocal ? Color.accentColor : Color.primary)
    }

    private func placeholder(_ text: String) -> some View {
        RoundedRectangle(cornerRadius: 8)
            .fill(Color(nsColor: .controlBackgroundColor))
            .frame(minHeight: 180)
            .overlay(Text(text).foregroundStyle(.secondary))
    }
}

// MARK: - Install guide

private struct InstallGuideCard: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "shield.lefthalf.filled")
                .font(.system(size: 48))
                .foregroundStyle(.secondary)
            Text("尚未安装特权组件")
                .font(.title2).bold()
            Text("EasyTier 需要一个具有管理员权限的后台组件（Supervisor）来创建网络接口与路由。安装时会请求一次管理员密码。")
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)

            if model.installing {
                ProgressView().controlSize(.small)
            } else {
                Button("安装") { Task { await model.installSupervisor() } }
                    .keyboardShortcut(.defaultAction)
            }

            if let message = model.statusMessage {
                Text(message).font(.callout).foregroundStyle(.red)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}
