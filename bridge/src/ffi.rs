//! C ABI surface for the EasyTier macOS native GUI bridge (M2).
//!
//! The authoritative contract is `include/easytier_bridge.h` (hand-maintained);
//! this module implements it. Rules enforced here:
//!   * every `extern "C"` body is wrapped in `catch_unwind` so a panic never
//!     crosses the FFI boundary (it degrades to an error string / safe default);
//!   * incoming `const char*` is copied into an owned `String` immediately;
//!   * outgoing `char*` is a `CString::into_raw()` freed only by `etb_free_string`
//!     (NULL is a valid "success / no content" sentinel);
//!   * blocking work runs on the handle's built-in multi-threaded tokio runtime.

use std::ffi::{CStr, CString, c_char, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Arc, Mutex};

use serde_json::json;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use uuid::Uuid;

use crate::rpc::RpcClient;
use crate::supervisor_client::{SupervisorClient, SupervisorConfig, SupervisorEvent};

/// C event callback: `void (*)(const char *event_json, void *ctx)`.
pub type EtbEventCb = Option<extern "C" fn(event_json: *const c_char, ctx: *mut c_void)>;

/// Opaque session handle: built-in tokio runtime + supervisor driver + RPC client.
pub struct EtbHandle {
    runtime: Runtime,
    supervisor: SupervisorClient,
    rpc: Arc<RpcClient>,
    instance_id: Mutex<Option<Uuid>>,
}

/// Bundles the C callback pointer + opaque `ctx` so it can be moved into the
/// event-forwarding task. The Swift caller guarantees `ctx` outlives the handle
/// and that the callback is thread-safe (it copies the JSON and hops to the main
/// actor), so hand-implementing `Send`/`Sync` for the raw pointer is sound.
struct EventSink {
    cb: EtbEventCb,
    ctx: *mut c_void,
}

unsafe impl Send for EventSink {}
unsafe impl Sync for EventSink {}

impl EventSink {
    fn emit(&self, json: &str) {
        let Some(cb) = self.cb else { return };
        // Interior NUL can't appear in our JSON; guard anyway.
        let Ok(cstr) = CString::new(json) else { return };
        cb(cstr.as_ptr(), self.ctx);
    }
}

// ---------------------------------------------------------------------------
// Event forwarding
// ---------------------------------------------------------------------------

/// Drain supervisor events: keep the RPC port in sync with core lifecycle
/// (mechanism; policy stays in Swift, DESIGN §9), then push each as JSON.
async fn forward_events(
    mut rx: UnboundedReceiver<SupervisorEvent>,
    rpc: Arc<RpcClient>,
    sink: Arc<EventSink>,
) {
    while let Some(ev) = rx.recv().await {
        match &ev {
            SupervisorEvent::Connected {
                rpc_port: Some(p), ..
            } => rpc.set_port(Some(*p)),
            SupervisorEvent::CoreStarted { rpc_port, .. } => rpc.set_port(Some(*rpc_port)),
            SupervisorEvent::CoreStopped { .. } | SupervisorEvent::CoreExited { .. } => {
                rpc.set_port(None)
            }
            _ => {}
        }
        sink.emit(&event_to_json(&ev).to_string());
    }
}

/// Serialize a supervisor event to the JSON schema in `easytier_bridge.h`.
fn event_to_json(ev: &SupervisorEvent) -> serde_json::Value {
    match ev {
        SupervisorEvent::Connected {
            version,
            core,
            rpc_port,
        } => json!({"type": "connected", "version": version, "core": core, "rpc_port": rpc_port}),
        SupervisorEvent::Disconnected => json!({"type": "disconnected"}),
        SupervisorEvent::CoreStarted { pid, rpc_port } => {
            json!({"type": "core_started", "pid": pid, "rpc_port": rpc_port})
        }
        SupervisorEvent::CoreStopped { reason } => {
            json!({"type": "core_stopped", "reason": reason})
        }
        SupervisorEvent::CoreExited { code, signal } => {
            json!({"type": "core_exited", "code": code, "signal": signal})
        }
        SupervisorEvent::Busy { owner } => json!({"type": "busy", "owner": owner}),
        SupervisorEvent::Kicked => json!({"type": "kicked"}),
        SupervisorEvent::Error { code, msg } => {
            json!({"type": "error", "code": code, "msg": msg})
        }
    }
}

// ---------------------------------------------------------------------------
// Boundary helpers
// ---------------------------------------------------------------------------

/// Copy an owned Rust string into a freshly allocated C string owned by the caller.
fn to_cstr(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(_) => CString::new("bridge: string contained interior nul")
            .unwrap()
            .into_raw(),
    }
}

/// NULL on success, an owned error C string otherwise.
fn result_to_cstr(r: Result<(), String>) -> *mut c_char {
    match r {
        Ok(()) => std::ptr::null_mut(),
        Err(e) => to_cstr(e),
    }
}

/// Borrow a C string as an owned Rust `String` (copied at the boundary); NULL → None.
///
/// # Safety
/// `p` must be NULL or a valid NUL-terminated C string that stays valid for the
/// duration of the call.
unsafe fn opt_string(p: *const c_char) -> Option<String> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned())
    }
}

/// Borrow a handle pointer as a shared reference; NULL → None.
///
/// # Safety
/// `h` must be NULL or a pointer returned by `etb_init` that has not been freed.
unsafe fn handle_ref<'a>(h: *mut EtbHandle) -> Option<&'a EtbHandle> {
    if h.is_null() {
        None
    } else {
        Some(unsafe { &*h })
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Create a session and start the supervisor driver. NULL on failure (runtime
/// creation only). `ET_SUPERVISOR_SOCKET` overrides the socket path (DESIGN §8).
#[unsafe(no_mangle)]
pub extern "C" fn etb_init(event_cb: EtbEventCb, ctx: *mut c_void) -> *mut EtbHandle {
    let built = panic::catch_unwind(AssertUnwindSafe(|| {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .ok()?;
        let (ev_tx, ev_rx) = mpsc::unbounded_channel::<SupervisorEvent>();
        // SupervisorClient::spawn needs a tokio context.
        let supervisor =
            runtime.block_on(async { SupervisorClient::spawn(SupervisorConfig::default(), ev_tx) });
        let rpc = Arc::new(RpcClient::new());
        let sink = Arc::new(EventSink { cb: event_cb, ctx });
        runtime.spawn(forward_events(ev_rx, rpc.clone(), sink));
        Some(Box::new(EtbHandle {
            runtime,
            supervisor,
            rpc,
            instance_id: Mutex::new(None),
        }))
    }));
    match built {
        Ok(Some(h)) => Box::into_raw(h),
        _ => std::ptr::null_mut(),
    }
}

/// Gracefully stop and free the handle. Dropping the owner control connection is
/// `stop` semantics (DESIGN §4), so the supervisor tears the core down and exits.
/// The handle is invalid after this call.
#[unsafe(no_mangle)]
pub extern "C" fn etb_shutdown(handle: *mut EtbHandle) {
    if handle.is_null() {
        return;
    }
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        // Reclaim ownership; dropping the box drops the runtime last.
        let h = unsafe { Box::from_raw(handle) };
        h.runtime.block_on(async {
            h.supervisor.shutdown().await;
        });
        drop(h);
    }));
}

// ---------------------------------------------------------------------------
// Network lifecycle
// ---------------------------------------------------------------------------

/// Ensure the core is running (triggers launchd activation) and run the network
/// instance described by `toml_text`. NULL on success, else an error string.
#[unsafe(no_mangle)]
pub extern "C" fn etb_connect(handle: *mut EtbHandle, toml_text: *const c_char) -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let Some(h) = (unsafe { handle_ref(handle) }) else {
            return Err("bridge: null handle".to_string());
        };
        let Some(toml) = (unsafe { opt_string(toml_text) }) else {
            return Err("bridge: null config".to_string());
        };
        h.runtime.block_on(async {
            let info = h.supervisor.start().await?;
            h.rpc.set_port(Some(info.rpc_port));
            let id = h.rpc.run_network_instance(&toml).await?;
            *h.instance_id.lock().unwrap() = Some(id);
            Ok::<(), String>(())
        })
    }));
    match r {
        Ok(res) => result_to_cstr(res),
        Err(_) => to_cstr("bridge: panic in etb_connect".to_string()),
    }
}

/// Delete the current instance and stop the core (zero-residency). Idempotent;
/// every step is best-effort. NULL on success.
#[unsafe(no_mangle)]
pub extern "C" fn etb_disconnect(handle: *mut EtbHandle) -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let Some(h) = (unsafe { handle_ref(handle) }) else {
            return Err("bridge: null handle".to_string());
        };
        h.runtime.block_on(async {
            let id = h.instance_id.lock().unwrap().take();
            if let Some(id) = id {
                if h.rpc.port().is_some() {
                    let _ = h.rpc.delete_network_instance(id).await;
                }
            }
            let _ = h.supervisor.stop().await;
            h.rpc.set_port(None);
        });
        Ok::<(), String>(())
    }));
    match r {
        Ok(res) => result_to_cstr(res),
        Err(_) => to_cstr("bridge: panic in etb_disconnect".to_string()),
    }
}

/// Node status snapshot for the current instance (schema in the header). Always
/// returns non-NULL JSON: `{"ok":{...}}` or `{"err":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn etb_status(handle: *mut EtbHandle) -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let Some(h) = (unsafe { handle_ref(handle) }) else {
            return json!({"err": "bridge: null handle"}).to_string();
        };
        let id = *h.instance_id.lock().unwrap();
        let Some(id) = id else {
            return json!({"err": "not connected"}).to_string();
        };
        if h.rpc.port().is_none() {
            return json!({"err": "core is not running"}).to_string();
        }
        match h.runtime.block_on(h.rpc.network_status(id)) {
            Ok(status) => json!({"ok": status}).to_string(),
            Err(e) => json!({"err": e}).to_string(),
        }
    }));
    match r {
        Ok(s) => to_cstr(s),
        Err(_) => to_cstr(json!({"err": "bridge: panic in etb_status"}).to_string()),
    }
}

/// Supervisor/core/install status (schema in the header). Always non-NULL JSON.
#[unsafe(no_mangle)]
pub extern "C" fn etb_supervisor_status(handle: *mut EtbHandle) -> *mut c_char {
    let disconnected = || {
        json!({
            "connected": false,
            "core_running": false,
            "rpc_port": serde_json::Value::Null,
            "installed": crate::install::installation_status().installed,
        })
        .to_string()
    };
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let Some(h) = (unsafe { handle_ref(handle) }) else {
            return disconnected();
        };
        let port = h.rpc.port();
        json!({
            "connected": h.supervisor.is_connected(),
            "core_running": port.is_some(),
            "rpc_port": port,
            "installed": crate::install::installation_status().installed,
        })
        .to_string()
    }));
    match r {
        Ok(s) => to_cstr(s),
        Err(_) => to_cstr(disconnected()),
    }
}

/// Request takeover of another instance's owner lease (after user confirmation).
#[unsafe(no_mangle)]
pub extern "C" fn etb_takeover(handle: *mut EtbHandle) {
    let _ = panic::catch_unwind(AssertUnwindSafe(|| {
        if let Some(h) = unsafe { handle_ref(handle) } {
            h.supervisor.request_takeover();
        }
    }));
}

// ---------------------------------------------------------------------------
// Handle-free helpers
// ---------------------------------------------------------------------------

/// Validate config text (`TomlConfigLoader` + `NetworkConfig`, no disk/state).
/// Always non-NULL JSON: `{"ok":true}` or `{"ok":false,"error":"..."}`.
#[unsafe(no_mangle)]
pub extern "C" fn etb_validate(toml_text: *const c_char) -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let Some(toml) = (unsafe { opt_string(toml_text) }) else {
            return json!({"ok": false, "error": "bridge: null config"}).to_string();
        };
        match crate::rpc::validate_config(&toml) {
            Ok(()) => json!({"ok": true}).to_string(),
            Err(e) => json!({"ok": false, "error": e}).to_string(),
        }
    }));
    match r {
        Ok(s) => to_cstr(s),
        Err(_) => to_cstr(json!({"ok": false, "error": "bridge: panic in etb_validate"}).to_string()),
    }
}

/// Install the privileged supervisor (single `osascript` admin prompt). NULL paths
/// use workspace dev binaries. NULL on success.
#[unsafe(no_mangle)]
pub extern "C" fn etb_install(
    supervisor_bin: *const c_char,
    core_bin: *const c_char,
) -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(|| {
        let sup = unsafe { opt_string(supervisor_bin) };
        let core = unsafe { opt_string(core_bin) };
        crate::install::run_install(sup, core)
    }));
    match r {
        Ok(res) => result_to_cstr(res),
        Err(_) => to_cstr("bridge: panic in etb_install".to_string()),
    }
}

/// Uninstall the privileged supervisor. NULL on success.
#[unsafe(no_mangle)]
pub extern "C" fn etb_uninstall() -> *mut c_char {
    let r = panic::catch_unwind(AssertUnwindSafe(crate::install::run_uninstall));
    match r {
        Ok(res) => result_to_cstr(res),
        Err(_) => to_cstr("bridge: panic in etb_uninstall".to_string()),
    }
}

/// Conflict detection (`Conflicts` serialization from `conflict.rs`). Non-NULL JSON.
#[unsafe(no_mangle)]
pub extern "C" fn etb_detect_conflicts() -> *mut c_char {
    let r = panic::catch_unwind(|| {
        serde_json::to_string(&crate::conflict::detect()).unwrap_or_else(|_| "{}".to_string())
    });
    to_cstr(r.unwrap_or_else(|_| "{}".to_string()))
}

/// Free any `char*` returned by this library. NULL is a no-op.
#[unsafe(no_mangle)]
pub extern "C" fn etb_free_string(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    // Reclaim and drop the CString this library allocated.
    unsafe {
        let _ = CString::from_raw(s);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::CoreState;
    use crate::rpc::{NetworkStatus, PeerRow};

    /// Call `etb_validate` and parse its JSON result (freeing the C string).
    fn call_validate(toml: &str) -> serde_json::Value {
        let c = CString::new(toml).unwrap();
        let ptr = etb_validate(c.as_ptr());
        assert!(!ptr.is_null(), "etb_validate must never return NULL");
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        etb_free_string(ptr);
        serde_json::from_str(&s).unwrap()
    }

    #[test]
    fn validate_accepts_minimal_no_tun_config() {
        let v = call_validate(
            "hostname = \"unit-test\"\n\
             listeners = []\n\
             [network_identity]\n\
             network_name = \"unit-test-net\"\n\
             network_secret = \"unit-test-sec\"\n\
             [flags]\n\
             no_tun = true\n",
        );
        assert_eq!(v["ok"], true, "expected ok, got {v}");
    }

    #[test]
    fn validate_rejects_garbage() {
        let v = call_validate("this is = not = valid = toml ===");
        assert_eq!(v["ok"], false);
        assert!(
            v.get("error").and_then(|e| e.as_str()).is_some(),
            "error message must be present: {v}"
        );
    }

    #[test]
    fn validate_null_is_error_not_crash() {
        let ptr = etb_validate(std::ptr::null());
        let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
        etb_free_string(ptr);
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["ok"], false);
    }

    #[test]
    fn free_string_null_is_noop() {
        etb_free_string(std::ptr::null_mut());
    }

    #[test]
    fn free_string_roundtrip_does_not_leak_or_crash() {
        let p = to_cstr("hello bridge".to_string());
        assert!(!p.is_null());
        etb_free_string(p);
    }

    #[test]
    fn status_row_json_shape_includes_protos() {
        let status = NetworkStatus {
            instance_id: "11111111-1111-1111-1111-111111111111".to_string(),
            rows: vec![
                PeerRow {
                    peer_id: 1,
                    hostname: "local".to_string(),
                    ipv4: "10.0.0.1".to_string(),
                    cost: "local".to_string(),
                    latency_ms: 0.0,
                    loss_rate: 0.0,
                    rx_bytes: 0,
                    tx_bytes: 0,
                    nat_type: "Unknown".to_string(),
                    version: "0.1.0".to_string(),
                    is_local: true,
                    protos: Vec::new(),
                },
                PeerRow {
                    peer_id: 2,
                    hostname: "peer".to_string(),
                    ipv4: "10.0.0.2".to_string(),
                    cost: "direct".to_string(),
                    latency_ms: 5.5,
                    loss_rate: 0.0,
                    rx_bytes: 10,
                    tx_bytes: 20,
                    nat_type: "FullCone".to_string(),
                    version: "0.1.0".to_string(),
                    is_local: false,
                    protos: vec!["udp".to_string(), "tcp".to_string()],
                },
            ],
        };
        let v = json!({"ok": status});
        assert_eq!(v["ok"]["instance_id"], status.instance_id);
        let local = &v["ok"]["rows"][0];
        assert_eq!(local["is_local"], true);
        assert!(local["protos"].as_array().unwrap().is_empty());
        let peer = &v["ok"]["rows"][1];
        assert_eq!(peer["cost"], "direct");
        assert_eq!(peer["protos"][0], "udp");
        assert_eq!(peer["protos"][1], "tcp");
    }

    #[test]
    fn event_json_shapes_match_header_schema() {
        let connected = event_to_json(&SupervisorEvent::Connected {
            version: "0.1.0".to_string(),
            core: CoreState::Running,
            rpc_port: Some(50321),
        });
        assert_eq!(connected["type"], "connected");
        assert_eq!(connected["core"], "running");
        assert_eq!(connected["rpc_port"], 50321);
        assert_eq!(connected["version"], "0.1.0");

        let started = event_to_json(&SupervisorEvent::CoreStarted {
            pid: 4321,
            rpc_port: 50321,
        });
        assert_eq!(started["type"], "core_started");
        assert_eq!(started["pid"], 4321);
        assert_eq!(started["rpc_port"], 50321);

        let exited = event_to_json(&SupervisorEvent::CoreExited {
            code: None,
            signal: Some(9),
        });
        assert_eq!(exited["type"], "core_exited");
        assert!(exited["code"].is_null());
        assert_eq!(exited["signal"], 9);

        let stopped = event_to_json(&SupervisorEvent::CoreStopped {
            reason: "requested".to_string(),
        });
        assert_eq!(stopped["type"], "core_stopped");
        assert_eq!(stopped["reason"], "requested");

        let busy = event_to_json(&SupervisorEvent::Busy { owner: true });
        assert_eq!(busy["type"], "busy");
        assert_eq!(busy["owner"], true);

        assert_eq!(
            event_to_json(&SupervisorEvent::Kicked)["type"],
            "kicked"
        );
        assert_eq!(
            event_to_json(&SupervisorEvent::Disconnected)["type"],
            "disconnected"
        );

        let err = event_to_json(&SupervisorEvent::Error {
            code: "spawn_failed".to_string(),
            msg: "boom".to_string(),
        });
        assert_eq!(err["type"], "error");
        assert_eq!(err["code"], "spawn_failed");
        assert_eq!(err["msg"], "boom");
    }
}
