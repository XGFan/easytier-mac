//
//  SettingsView.swift
//  EasyTier
//
//  设置 tab: launch-at-login (SMAppService), auto-restart toggle, config file
//  shortcuts, supervisor uninstall, and the version string.
//

import SwiftUI
import ServiceManagement

struct SettingsView: View {
    @EnvironmentObject private var model: AppModel
    @State private var launchAtLogin = false
    @State private var showUninstallConfirm = false

    var body: some View {
        Form {
            Section("通用") {
                Toggle("开机自启", isOn: $launchAtLogin)
                    .onChange(of: launchAtLogin) { _, newValue in
                        setLaunchAtLogin(newValue)
                    }
                Toggle("启动时自动连接", isOn: $model.autoConnectOnLaunch)
                Toggle("连接中断后自动重启", isOn: $model.autoRestartEnabled)
            }

            Section("配置文件") {
                HStack {
                    Text(model.config.fileURL.path)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Spacer()
                    Button("打开") { model.openConfigInEditor() }
                    Button("在 Finder 中显示") { model.revealConfigInFinder() }
                }
            }

            Section("特权组件") {
                HStack {
                    Text(installedText)
                    Spacer()
                    Button("卸载 Supervisor", role: .destructive) {
                        showUninstallConfirm = true
                    }
                    .disabled(model.uninstalling || model.supervisorStatus?.installed == false)
                }
            }

            Section("关于") {
                LabeledContent("版本", value: appVersion)
            }
        }
        .formStyle(.grouped)
        .padding(16)
        .onAppear {
            launchAtLogin = (SMAppService.mainApp.status == .enabled)
            Task { await model.refreshSupervisorStatus() }
        }
        .confirmationDialog(
            "确定卸载特权组件？",
            isPresented: $showUninstallConfirm,
            titleVisibility: .visible
        ) {
            Button("卸载", role: .destructive) {
                Task { await model.uninstallSupervisor() }
            }
            Button("取消", role: .cancel) {}
        } message: {
            Text("卸载后将无法连接网络，需要重新安装（一次管理员密码）。")
        }
    }

    private var installedText: String {
        switch model.supervisorStatus?.installed {
        case .some(true): return "已安装"
        case .some(false): return "未安装"
        case .none: return "检测中…"
        }
    }

    private var appVersion: String {
        let short = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
        let build = Bundle.main.infoDictionary?["CFBundleVersion"] as? String ?? "?"
        return "\(short) (\(build))"
    }

    private func setLaunchAtLogin(_ enabled: Bool) {
        do {
            if enabled {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
        } catch {
            // Revert the toggle to reflect the real state on failure.
            launchAtLogin = (SMAppService.mainApp.status == .enabled)
            model.statusMessage = "开机自启设置失败：\(error.localizedDescription)"
        }
    }
}
