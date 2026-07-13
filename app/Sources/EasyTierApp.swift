//
//  EasyTierApp.swift
//  EasyTier
//
//  App entry point: a pure menu-bar (LSUIElement) app with no main window
//  (DESIGN §9). All UI lives in the MenuBarExtra window-style panel; the
//  menu-bar glyph reflects the connection state (solid = connected).
//

import SwiftUI
import AppKit

/// Single shared policy model, created on the main actor at launch. Shared by the
/// scene and the app delegate (terminate → graceful shutdown).
@MainActor
extension AppModel {
    static let shared = AppModel(bridge: BridgeClient(), config: ConfigStore())
}

@main
struct EasyTierApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var appDelegate
    @StateObject private var model = AppModel.shared

    var body: some Scene {
        MenuBarExtra {
            PanelView()
                .environmentObject(model)
        } label: {
            Image(nsImage: MenuBarIcon.image(connected: model.connectionState == .connected))
        }
        .menuBarExtraStyle(.window)
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        AppModel.shared.shutdown()
    }
}
