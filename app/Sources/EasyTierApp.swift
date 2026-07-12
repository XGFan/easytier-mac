//
//  EasyTierApp.swift
//  EasyTier
//
//  App entry point: a menu-bar (LSUIElement) app with a two-tab window. The
//  MenuBarExtra offers connect/disconnect, open window, and quit; the Window
//  hosts the 网络/设置 tabs and hides (not closes) on the red button.
//

import SwiftUI
import AppKit

/// Single shared policy model, created on the main actor at launch. Shared by the
/// scenes and the app delegate (terminate → graceful shutdown).
@MainActor
extension AppModel {
    static let shared = AppModel(bridge: BridgeClient(), config: ConfigStore())
}

@main
struct EasyTierApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel.shared
    @StateObject private var windowManager = WindowManager()

    var body: some Scene {
        Window("EasyTier", id: "main") {
            RootView()
                .environmentObject(model)
                .environmentObject(windowManager)
                .background(WindowAccessor { windowManager.adopt($0) })
                .onAppear { windowManager.show() }
        }
        .windowResizability(.contentSize)
        .defaultSize(width: 760, height: 580)

        MenuBarExtra {
            MenuBarContent(model: model, windowManager: windowManager)
        } label: {
            Image(systemName: "network")
        }
    }
}

/// Menu-bar dropdown: dynamic connect/disconnect + open window + quit.
private struct MenuBarContent: View {
    @ObservedObject var model: AppModel
    @ObservedObject var windowManager: WindowManager

    var body: some View {
        Button(model.connectionState == .connected ? "断开" : "连接") {
            model.toggleConnection()
        }
        .disabled(connectDisabled)

        Divider()

        Button("打开主窗口") { windowManager.show() }

        Divider()

        Button("退出 EasyTier") { NSApp.terminate(nil) }
    }

    private var connectDisabled: Bool {
        if model.connectionState.isTransitioning { return true }
        if model.connectionState == .disconnected && !model.validation.ok { return true }
        return false
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        AppModel.shared.shutdown()
    }

    // Menu-bar app: keep running when the window is closed/hidden.
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        false
    }
}

/// Resolves the hosting NSWindow and hands it to the WindowManager (idempotent).
struct WindowAccessor: NSViewRepresentable {
    let onResolve: (NSWindow) -> Void

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        DispatchQueue.main.async { [weak view] in
            if let window = view?.window { onResolve(window) }
        }
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        if let window = nsView.window { onResolve(window) }
    }
}
