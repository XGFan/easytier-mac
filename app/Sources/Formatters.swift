//
//  Formatters.swift
//  EasyTier
//
//  Byte / rate / latency / loss formatting helpers (binary units, per DESIGN §9).
//

import Foundation

enum Fmt {
    private static let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"]

    /// Human-readable binary size, e.g. "1.5 MiB".
    static func bytes(_ value: Double) -> String {
        var v = max(value, 0)
        var i = 0
        while v >= 1024 && i < units.count - 1 {
            v /= 1024
            i += 1
        }
        return i == 0 ? String(format: "%.0f %@", v, units[i])
                      : String(format: "%.1f %@", v, units[i])
    }

    static func bytes(_ value: UInt64) -> String { bytes(Double(value)) }

    /// Per-second rate, e.g. "3.2 MiB/s".
    static func rate(_ bytesPerSecond: Double) -> String {
        bytes(bytesPerSecond) + "/s"
    }

    /// Latency in ms, one decimal; "-" when unknown (0).
    static func latency(_ ms: Double) -> String {
        ms <= 0 ? "-" : String(format: "%.1f ms", ms)
    }

    /// Loss fraction (0.0–1.0) as a percentage.
    static func loss(_ fraction: Double) -> String {
        String(format: "%.1f%%", max(fraction, 0) * 100)
    }
}
