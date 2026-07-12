//
//  WindowManager.swift
//  EasyTier
//
//  Menu-bar-app window lifecycle (DESIGN §8/§9): closing the window hides it to
//  the menu bar and drops the app to `.accessory` (no Dock icon); showing it
//  switches to `.regular` and focuses. The SwiftUI window is never truly closed —
//  `windowShouldClose` returns false and orders it out — so the same NSWindow is
//  reused across show/hide.
//

import AppKit

@MainActor
final class WindowManager: NSObject, NSWindowDelegate, ObservableObject {
    private weak var window: NSWindow?

    /// Adopt the underlying NSWindow once SwiftUI has created it. Idempotent.
    func adopt(_ window: NSWindow) {
        guard self.window !== window else { return }
        self.window = window
        window.delegate = self
        window.isReleasedWhenClosed = false
    }

    /// Show + focus the main window; app becomes `.regular` (Dock icon appears).
    func show() {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        window?.makeKeyAndOrderFront(nil)
    }

    /// Hide the window to the menu bar; app becomes `.accessory`.
    func hide() {
        window?.orderOut(nil)
        NSApp.setActivationPolicy(.accessory)
    }

    // Intercept the red close button: hide instead of close.
    func windowShouldClose(_ sender: NSWindow) -> Bool {
        hide()
        return false
    }
}
