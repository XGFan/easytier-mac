//! End-to-end integration test for the bridge FFI against a real `--dev-listen`
//! supervisor driving a real `easytier-core` (DESIGN §8, plan A2).
//!
//! Non-root: the core runs a no-tun instance (no TUN device, no listeners) so the
//! whole init → connect → status → disconnect → shutdown loop works without
//! privileges. It exercises the actual C ABI (`etb_*`) through the compiled
//! binaries rather than any internal type.
//!
//! If the supervisor/core binaries are missing (nobody built them), the test
//! prints a hint and returns — it never fails for that reason. Build them with:
//!   `cargo build -p easytier-supervisor -p easytier --bin easytier-core`

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use easytier_mac_bridge::ffi::{
    EtbHandle, etb_connect, etb_disconnect, etb_free_string, etb_init, etb_shutdown, etb_status,
    etb_validate,
};

/// Per-step wait budget (plan A2 requires every async step to have a timeout).
const STEP_TIMEOUT: Duration = Duration::from_secs(15);

// ---------------------------------------------------------------------------
// Event collection through the C callback
// ---------------------------------------------------------------------------

/// C callback: copy the event JSON and push it into the shared buffer behind `ctx`.
extern "C" fn collect_cb(json: *const c_char, ctx: *mut c_void) {
    if json.is_null() || ctx.is_null() {
        return;
    }
    let s = unsafe { CStr::from_ptr(json) }.to_string_lossy().into_owned();
    // SAFETY: `ctx` points at the `Mutex<Vec<String>>` kept alive by the test for
    // the whole handle lifetime (see `events` below).
    let events = unsafe { &*(ctx as *const Mutex<Vec<String>>) };
    if let Ok(mut v) = events.lock() {
        v.push(s);
    }
}

fn event_type_is(json: &str, ty: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(json)
        .ok()
        .and_then(|v| v.get("type").and_then(|t| t.as_str().map(|s| s == ty)))
        .unwrap_or(false)
}

fn wait_for_event(events: &Mutex<Vec<String>>, ty: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if events.lock().unwrap().iter().any(|s| event_type_is(s, ty)) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---------------------------------------------------------------------------
// FFI string helpers
// ---------------------------------------------------------------------------

/// Take ownership of a `char*` from the bridge: None if NULL, else the freed string.
fn take_cstr(ptr: *mut c_char) -> Option<String> {
    if ptr.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned();
    etb_free_string(ptr);
    Some(s)
}

// ---------------------------------------------------------------------------
// Binary discovery + supervisor fixture
// ---------------------------------------------------------------------------

fn target_debug() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        PathBuf::from(dir).join("debug")
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/debug")
    }
}

/// Kill-on-drop guard around the supervisor child.
struct Supervisor {
    child: Child,
}

impl Supervisor {
    fn wait_exit(&mut self, dur: Duration) -> Option<i32> {
        let deadline = Instant::now() + dur;
        loop {
            match self.child.try_wait().unwrap() {
                Some(status) => return Some(status.code().unwrap_or(-1)),
                None if Instant::now() >= deadline => return None,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn wait_path(path: &Path, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    path.exists()
}

/// A no-tun instance config: no TUN device and no listeners, so a random port /
/// privileged interface is never touched. Self-checked with `etb_validate` below.
fn no_tun_config() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let unique = format!("{}-{}", std::process::id(), nanos);
    format!(
        "hostname = \"bridge-it-{unique}\"\n\
         listeners = []\n\
         [network_identity]\n\
         network_name = \"bridge-it-net\"\n\
         network_secret = \"bridge-it-sec\"\n\
         [flags]\n\
         no_tun = true\n"
    )
}

// ---------------------------------------------------------------------------
// The end-to-end flow (single test to avoid racing the process-global env var)
// ---------------------------------------------------------------------------

#[test]
fn init_connect_status_disconnect_shutdown() {
    let debug = target_debug();
    let sup_bin = debug.join("easytier-supervisor");
    let core_bin = debug.join("easytier-core");
    if !sup_bin.exists() || !core_bin.exists() {
        eprintln!(
            "SKIP dev_supervisor: missing binaries.\n  supervisor: {} ({})\n  core: {} ({})\n  build: cargo build -p easytier-supervisor -p easytier --bin easytier-core",
            sup_bin.display(),
            sup_bin.exists(),
            core_bin.display(),
            core_bin.exists(),
        );
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let sock = root.join("sup.sock");
    let log_dir = root.join("logs");
    let config = root.join("supervisor.toml");
    std::fs::create_dir_all(&log_dir).unwrap();
    std::fs::write(
        &config,
        format!(
            "proto = 1\nowner_uid = {}\ncore_path = {:?}\nlog_dir = {:?}\n",
            // SAFETY: geteuid never fails.
            unsafe { libc::geteuid() },
            core_bin.to_str().unwrap(),
            log_dir.to_str().unwrap(),
        ),
    )
    .unwrap();

    // Start the supervisor in dev-listen mode and wait for it to bind the socket.
    let mut sup = Supervisor {
        child: Command::new(&sup_bin)
            .arg("--config")
            .arg(&config)
            .arg("--dev-listen")
            .arg(&sock)
            .spawn()
            .expect("spawn supervisor"),
    };
    assert!(
        wait_path(&sock, Duration::from_secs(5)),
        "supervisor never bound its dev socket: {}",
        sock.display()
    );

    // The bridge resolves the socket from ET_SUPERVISOR_SOCKET at init time.
    // SAFETY: single-threaded setup before any other thread reads the env.
    unsafe {
        std::env::set_var("ET_SUPERVISOR_SOCKET", &sock);
    }

    // Event buffer kept alive for the whole handle lifetime (the callback holds a
    // raw pointer into it).
    let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let ctx = Arc::as_ptr(&events) as *mut c_void;

    let handle: *mut EtbHandle = etb_init(Some(collect_cb), ctx);
    assert!(!handle.is_null(), "etb_init returned NULL");

    // 1) Control connection established.
    assert!(
        wait_for_event(&events, "connected", STEP_TIMEOUT),
        "no 'connected' event; got: {:?}",
        events.lock().unwrap()
    );

    // Self-check the test config through the real validator before connecting.
    let cfg = no_tun_config();
    let cfg_c = CString::new(cfg.clone()).unwrap();
    let validate_json = take_cstr(etb_validate(cfg_c.as_ptr())).expect("validate JSON");
    let validate: serde_json::Value = serde_json::from_str(&validate_json).unwrap();
    assert_eq!(validate["ok"], true, "test config invalid: {validate_json}");

    // 2) Connect: ensure core is up + run the no-tun instance.
    if let Some(err) = take_cstr(etb_connect(handle, cfg_c.as_ptr())) {
        panic!("etb_connect failed: {err}");
    }
    assert!(
        wait_for_event(&events, "core_started", STEP_TIMEOUT),
        "no 'core_started' event; got: {:?}",
        events.lock().unwrap()
    );

    // 3) Status must eventually contain the local node row.
    let deadline = Instant::now() + STEP_TIMEOUT;
    let mut saw_local = false;
    let mut last_status = String::new();
    while Instant::now() < deadline {
        last_status = take_cstr(etb_status(handle)).expect("status JSON is never NULL");
        let v: serde_json::Value = serde_json::from_str(&last_status).unwrap();
        if let Some(rows) = v.get("ok").and_then(|ok| ok.get("rows")).and_then(|r| r.as_array()) {
            if rows.iter().any(|row| row.get("is_local") == Some(&serde_json::Value::Bool(true))) {
                // The protos field must be present on every row (plan A3).
                assert!(
                    rows.iter().all(|row| row.get("protos").map(|p| p.is_array()).unwrap_or(false)),
                    "a status row is missing the protos array: {last_status}"
                );
                saw_local = true;
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(saw_local, "status never reported the local node row: {last_status}");

    // 4) Disconnect: delete instance + stop core (zero-residency). Idempotent.
    if let Some(err) = take_cstr(etb_disconnect(handle)) {
        panic!("etb_disconnect failed: {err}");
    }
    assert!(
        wait_for_event(&events, "core_stopped", STEP_TIMEOUT),
        "no 'core_stopped' event; got: {:?}",
        events.lock().unwrap()
    );

    // A second disconnect is a clean no-op.
    assert!(
        take_cstr(etb_disconnect(handle)).is_none(),
        "second etb_disconnect should succeed idempotently"
    );

    // 5) Shutdown: dropping the owner connection makes the supervisor exit.
    etb_shutdown(handle);
    assert_eq!(
        sup.wait_exit(Duration::from_secs(10)),
        Some(0),
        "supervisor should exit(0) after the owner connection drops"
    );

    // Keep the event buffer alive until after shutdown returned.
    drop(events);
}
