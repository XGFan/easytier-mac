//
//  BridgeClient.swift
//  EasyTier
//
//  Swift wrapper around the Bridge C API (easytier_bridge.h). Responsibilities:
//    - own the EtbHandle (created in init, released in shutdown);
//    - marshal all handle calls onto a dedicated serial queue and expose them as
//      `async` methods (header rule 4: handle calls are blocking → never on the
//      main thread);
//    - turn the C event callback into an `AsyncStream<BridgeEvent>` (header rule
//      5: the callback copies the JSON immediately and does no other work);
//    - free every returned `char*` exactly once via `etb_free_string`
//      (header ownership rule 1).
//
//  `@unchecked Sendable`: the handle is internally serialized by the Rust tokio
//  runtime (header rule 3) and every Swift-side call funnels through `queue`.
//

import Foundation

/// Holds the event-stream continuation so the `@convention(c)` trampoline can
/// reach it through the opaque `ctx` pointer passed to `etb_init`.
private final class EventSink: @unchecked Sendable {
    let continuation: AsyncStream<BridgeEvent>.Continuation
    init(_ continuation: AsyncStream<BridgeEvent>.Continuation) {
        self.continuation = continuation
    }
}

/// C event callback trampoline. Runs on a Rust runtime thread; copies the JSON
/// (via `String(cString:)`) and yields — no `etb_*` calls, no blocking work.
private func bridgeEventTrampoline(
    _ eventJSON: UnsafePointer<CChar>?,
    _ ctx: UnsafeMutableRawPointer?
) {
    guard let ctx, let eventJSON else { return }
    let sink = Unmanaged<EventSink>.fromOpaque(ctx).takeUnretainedValue()
    let json = String(cString: eventJSON)
    sink.continuation.yield(BridgeEvent(jsonString: json))
}

final class BridgeClient: @unchecked Sendable {
    /// Opaque `EtbHandle *`; nil only if `etb_init` failed (rare: runtime setup).
    private let handle: OpaquePointer?
    /// Serializes handle calls off the main thread.
    private let queue = DispatchQueue(label: "com.easytier.mac.bridge")
    /// Retained event sink; released after `etb_shutdown`.
    private let sinkRetained: Unmanaged<EventSink>?
    /// Delivered supervisor/core events. Iterate exactly once.
    let events: AsyncStream<BridgeEvent>

    /// Shared snake_case-aware decoder for result payloads.
    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()

    init() {
        var continuation: AsyncStream<BridgeEvent>.Continuation!
        self.events = AsyncStream(bufferingPolicy: .unbounded) { continuation = $0 }
        let sink = EventSink(continuation)
        let retained = Unmanaged.passRetained(sink)
        self.sinkRetained = retained
        self.handle = etb_init(bridgeEventTrampoline, retained.toOpaque())
        if handle == nil {
            continuation.yield(.error(code: "init_failed", message: "etb_init returned NULL"))
        }
    }

    /// Copy a Rust-owned `char*` into a Swift String and free it. NULL → nil.
    private static func takeString(_ ptr: UnsafeMutablePointer<CChar>?) -> String? {
        guard let ptr else { return nil }
        defer { etb_free_string(ptr) }
        return String(cString: ptr)
    }

    // MARK: - Handle calls (async, off the main thread)

    /// Connect: ensure core running + run the network instance. nil = success,
    /// otherwise the error message.
    func connect(toml: String) async -> String? {
        await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            queue.async {
                guard let handle = self.handle else {
                    cont.resume(returning: "Bridge 未初始化")
                    return
                }
                let result = toml.withCString { etb_connect(handle, $0) }
                cont.resume(returning: BridgeClient.takeString(result))
            }
        }
    }

    /// Disconnect: delete the instance and stop the core. nil = success.
    func disconnect() async -> String? {
        await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            queue.async {
                guard let handle = self.handle else {
                    cont.resume(returning: nil)
                    return
                }
                cont.resume(returning: BridgeClient.takeString(etb_disconnect(handle)))
            }
        }
    }

    /// Peer/self status snapshot; nil if the JSON was missing or undecodable.
    func status() async -> StatusEnvelope? {
        await withCheckedContinuation { (cont: CheckedContinuation<StatusEnvelope?, Never>) in
            queue.async {
                guard let handle = self.handle else {
                    cont.resume(returning: nil)
                    return
                }
                let json = BridgeClient.takeString(etb_status(handle))
                cont.resume(returning: BridgeClient.decode(StatusEnvelope.self, json))
            }
        }
    }

    /// supervisor/core/install status snapshot.
    func supervisorStatus() async -> SupervisorStatus? {
        await withCheckedContinuation { (cont: CheckedContinuation<SupervisorStatus?, Never>) in
            queue.async {
                guard let handle = self.handle else {
                    cont.resume(returning: nil)
                    return
                }
                let json = BridgeClient.takeString(etb_supervisor_status(handle))
                cont.resume(returning: BridgeClient.decode(SupervisorStatus.self, json))
            }
        }
    }

    /// Request takeover of another instance's owner lease (after user confirm).
    func takeover() {
        queue.async {
            guard let handle = self.handle else { return }
            etb_takeover(handle)
        }
    }

    // MARK: - Handle-free calls

    /// Validate config text (TomlConfigLoader + NetworkConfig). Handle-free but
    /// run off-main to stay uniform.
    func validate(toml: String) async -> ValidateResult {
        await withCheckedContinuation { (cont: CheckedContinuation<ValidateResult, Never>) in
            queue.async {
                let json = toml.withCString { BridgeClient.takeString(etb_validate($0)) }
                let result = BridgeClient.decode(ValidateResult.self, json)
                    ?? ValidateResult(ok: false, error: "校验返回无法解析")
                cont.resume(returning: result)
            }
        }
    }

    /// Install the privileged supervisor (osascript prompt). Passing nil uses the
    /// bridge's default paths. nil = success.
    func install() async -> String? {
        await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            queue.async {
                cont.resume(returning: BridgeClient.takeString(etb_install(nil, nil)))
            }
        }
    }

    /// Uninstall the privileged supervisor. nil = success.
    func uninstall() async -> String? {
        await withCheckedContinuation { (cont: CheckedContinuation<String?, Never>) in
            queue.async {
                cont.resume(returning: BridgeClient.takeString(etb_uninstall()))
            }
        }
    }

    // MARK: - Teardown

    /// Graceful shutdown: stop the control connection + tokio, then release the
    /// event sink. Call once from `applicationWillTerminate`.
    func shutdown() {
        queue.sync {
            if let handle = self.handle {
                etb_shutdown(handle)
            }
        }
        sinkRetained?.release()
    }

    private static func decode<T: Decodable>(_ type: T.Type, _ json: String?) -> T? {
        guard let json, let data = json.data(using: .utf8) else { return nil }
        return try? decoder.decode(type, from: data)
    }
}
