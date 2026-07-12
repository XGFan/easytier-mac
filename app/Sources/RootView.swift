//
//  RootView.swift
//  EasyTier
//
//  Two-tab window shell (网络 / 设置). Revalidates the config whenever the app
//  becomes active (DESIGN §9: 窗口激活时重读重校).
//

import SwiftUI
import AppKit

struct RootView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        TabView {
            NetworkView()
                .tabItem { Label("网络", systemImage: "network") }
            SettingsView()
                .tabItem { Label("设置", systemImage: "gearshape") }
        }
        .frame(minWidth: 720, minHeight: 540)
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            model.onBecomeActive()
        }
    }
}
