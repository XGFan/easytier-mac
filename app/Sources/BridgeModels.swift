//
//  BridgeModels.swift
//  EasyTier
//
//  Codable models mirroring the JSON schemas in easytier_bridge.h (events +
//  result payloads). Field names align with the header; snake_case JSON keys are
//  mapped via a shared `.convertFromSnakeCase` decoder (see BridgeClient).
//

import Foundation

// MARK: - Events (event_cb JSON)

/// One decoded supervisor/core event delivered over the C event callback.
/// Schema: easytier_bridge.h "事件 JSON schema".
enum BridgeEvent: Sendable {
    case connected(version: String, core: String, rpcPort: UInt16?)
    case disconnected
    case coreStarted(pid: UInt32, rpcPort: UInt16)
    case coreStopped(reason: String)
    case coreExited(code: Int32?, signal: Int32?)
    case busy(owner: Bool)
    case kicked
    case error(code: String, message: String)
    /// A well-formed event whose `type` we do not model.
    case unknown(type: String)
    /// The callback delivered something that was not decodable JSON.
    case malformed(String)
}

extension BridgeEvent {
    /// Decode a raw event JSON string; never throws (unparseable → `.malformed`).
    init(jsonString: String) {
        guard let data = jsonString.data(using: .utf8) else {
            self = .malformed(jsonString)
            return
        }
        do {
            self = try JSONDecoder().decode(BridgeEvent.self, from: data)
        } catch {
            self = .malformed(jsonString)
        }
    }
}

extension BridgeEvent: Decodable {
    private enum CodingKeys: String, CodingKey {
        case type, version, core, pid, reason, code, signal, owner, msg
        case rpcPort = "rpc_port"
    }

    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let type = try c.decode(String.self, forKey: .type)
        switch type {
        case "connected":
            self = .connected(
                version: (try? c.decode(String.self, forKey: .version)) ?? "",
                core: (try? c.decode(String.self, forKey: .core)) ?? "",
                rpcPort: try c.decodeIfPresent(UInt16.self, forKey: .rpcPort)
            )
        case "disconnected":
            self = .disconnected
        case "core_started":
            self = .coreStarted(
                pid: (try? c.decode(UInt32.self, forKey: .pid)) ?? 0,
                rpcPort: (try? c.decode(UInt16.self, forKey: .rpcPort)) ?? 0
            )
        case "core_stopped":
            self = .coreStopped(reason: (try? c.decode(String.self, forKey: .reason)) ?? "")
        case "core_exited":
            self = .coreExited(
                code: try c.decodeIfPresent(Int32.self, forKey: .code),
                signal: try c.decodeIfPresent(Int32.self, forKey: .signal)
            )
        case "busy":
            self = .busy(owner: (try? c.decode(Bool.self, forKey: .owner)) ?? true)
        case "kicked":
            self = .kicked
        case "error":
            self = .error(
                code: (try? c.decode(String.self, forKey: .code)) ?? "",
                message: (try? c.decode(String.self, forKey: .msg)) ?? ""
            )
        default:
            self = .unknown(type: type)
        }
    }
}

// MARK: - Result payloads

/// `etb_validate` result: `{"ok":true} | {"ok":false,"error":"..."}`.
struct ValidateResult: Decodable, Sendable {
    let ok: Bool
    let error: String?
}

/// `etb_supervisor_status` result.
struct SupervisorStatus: Decodable, Sendable {
    let connected: Bool
    let coreRunning: Bool
    let rpcPort: UInt16?
    let installed: Bool
}

/// One row of `etb_status().ok.rows` — a peer/self entry (schema in the header).
struct PeerRow: Decodable, Sendable, Identifiable {
    let peerId: UInt32
    let hostname: String
    let ipv4: String
    /// "local" | "direct" | "relay(N)".
    let cost: String
    let latencyMs: Double
    /// Fraction 0.0–1.0.
    let lossRate: Double
    let rxBytes: UInt64
    let txBytes: UInt64
    let natType: String
    let version: String
    let isLocal: Bool
    let protos: [String]

    var id: UInt32 { peerId }
}

/// `etb_status().ok` — snapshot of the running instance.
struct NetworkStatus: Decodable, Sendable {
    let instanceId: String
    let rows: [PeerRow]
}

/// `etb_status` envelope: `{"ok":{...}} | {"err":"..."}`.
struct StatusEnvelope: Decodable, Sendable {
    let ok: NetworkStatus?
    let err: String?
}
