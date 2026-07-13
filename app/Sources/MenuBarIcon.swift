//
//  MenuBarIcon.swift
//  EasyTier
//
//  Menu-bar status glyph: a three-node mesh triangle rendered as a template
//  image (DESIGN §9 — 单色模板,不用彩色). Connected = solid nodes; disconnected
//  = hollow nodes. Template rendering adapts to menu-bar appearance/highlight.
//

import AppKit

@MainActor
enum MenuBarIcon {
    private static let connectedImage = make(connected: true)
    private static let disconnectedImage = make(connected: false)

    static func image(connected: Bool) -> NSImage {
        connected ? connectedImage : disconnectedImage
    }

    private static func make(connected: Bool) -> NSImage {
        let side: CGFloat = 18
        let top = NSPoint(x: 9, y: 13.4)
        let left = NSPoint(x: 4.4, y: 4.9)
        let right = NSPoint(x: 13.6, y: 4.9)
        let radius: CGFloat = 2.4

        let image = NSImage(size: NSSize(width: side, height: side), flipped: false) { _ in
            NSColor.black.set()

            let edges = NSBezierPath()
            edges.lineWidth = 1.4
            for (a, b) in [(top, left), (top, right), (left, right)] {
                edges.move(to: a)
                edges.line(to: b)
            }
            edges.stroke()

            for center in [top, left, right] {
                let rect = NSRect(
                    x: center.x - radius, y: center.y - radius,
                    width: radius * 2, height: radius * 2)
                let node = NSBezierPath(ovalIn: rect)
                if connected {
                    node.fill()
                } else {
                    // Punch the edge lines out of the disc so the ring reads hollow.
                    if let cg = NSGraphicsContext.current?.cgContext {
                        cg.saveGState()
                        cg.setBlendMode(.clear)
                        node.fill()
                        cg.restoreGState()
                    }
                    node.lineWidth = 1.2
                    node.stroke()
                }
            }
            return true
        }
        image.isTemplate = true
        return image
    }
}
