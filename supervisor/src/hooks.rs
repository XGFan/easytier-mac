//! Lifecycle hooks: run a fixed-path, root-owned user script at core up/down
//! transitions (plan §3.2, DESIGN §hooks).
//!
//! The supervisor is the one privileged execution point we already trust, so we
//! reuse it to run `<install_root>/hooks/up.sh` after a core spawns and
//! `<install_root>/hooks/down.sh` after it dies. This exists so a user can, for
//! example, switch DNS on connect and restore it on disconnect without any new
//! IPC surface: the event and reason are supervisor-side enums — no client data
//! ever reaches the hook's arguments or environment.
//!
//! ## Safety
//!
//! Running a user script as root is a local privilege-escalation surface, so the
//! script path is derived from config (never client-supplied) and every run is
//! gated by [`check_hook_security`] (a pure predicate, hence unit-testable). In
//! production (launchd auth) the script must be a root:wheel regular file that is
//! owner-executable and not group/world writable — the sudoers/cron discipline.
//! Dev mode (`--dev-listen`) relaxes ownership to the process euid so the
//! integration tests can run unprivileged, mirroring `auth.rs`.
//!
//! ## Scheduling
//!
//! A hook runs on its own thread and never blocks the state machine. Each run is
//! bounded: if the script outlives `hook_timeout_secs` it is SIGKILLed and
//! reaped. [`run_hook`] returns a joinable [`HookHandle`] so the owner-disconnect
//! exit path can await an in-flight `down` hook (bounded by the same timeout)
//! before the process exits; every other caller detaches it by dropping.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::auth::AuthMode;
use crate::config::Config;

/// Minimal PATH handed to hook scripts (matches the core spawn env in
/// `core_proc::build_command`).
const HOOK_PATH: &str = "/usr/bin:/bin:/usr/sbin:/sbin";

/// Poll cadence while waiting for a hook child to exit (also the timeout
/// granularity).
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Which lifecycle transition a hook fires on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    Up,
    Down,
}

impl HookEvent {
    /// `EASYTIER_EVENT` value.
    pub fn as_str(self) -> &'static str {
        match self {
            HookEvent::Up => "up",
            HookEvent::Down => "down",
        }
    }

    /// Script filename inside `hooks_dir`.
    fn script_name(self) -> &'static str {
        match self {
            HookEvent::Up => "up.sh",
            HookEvent::Down => "down.sh",
        }
    }
}

/// Why a `down` hook fired (`EASYTIER_REASON`). Kept in lock-step with the
/// `core_stopped` / `core_exited` wire reasons so logs read consistently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookReason {
    /// Client sent `stop`.
    Requested,
    /// Owner control connection dropped (== stop semantics + supervisor exit).
    OwnerDrop,
    /// Core died on its own (crash / external kill).
    CoreExited,
    /// Janitor SIGKILLed a leftover core (backstop for a crashed predecessor
    /// that never ran its own `down`).
    Janitor,
}

impl HookReason {
    pub fn as_str(self) -> &'static str {
        match self {
            HookReason::Requested => "requested",
            HookReason::OwnerDrop => "owner_drop",
            HookReason::CoreExited => "core_exited",
            HookReason::Janitor => "janitor",
        }
    }
}

/// A spawned hook thread. Drop to detach (the script keeps running in the
/// background, self-bounded by the timeout); [`HookHandle::join`] to await it.
pub struct HookHandle {
    join: thread::JoinHandle<()>,
}

impl HookHandle {
    /// Wait for the hook to finish. Bounded in practice: the hook self-kills at
    /// the configured timeout, so this cannot block longer than that.
    pub fn join(self) {
        let _ = self.join.join();
    }
}

/// The file facts [`check_hook_security`] needs; split out so the predicate is
/// pure and unit-testable without touching the filesystem.
struct FileFacts {
    is_file: bool,
    uid: u32,
    gid: u32,
    mode: u32,
}

/// Run the hook for `event`/`reason` if its script exists and passes the
/// security gate.
///
/// Returns `Some(handle)` when a hook thread was spawned, `None` when the script
/// is absent (silent skip — not an error) or was rejected (logged to stderr ->
/// supervisor.err.log, not executed).
pub fn run_hook(
    config: &Config,
    auth: AuthMode,
    event: HookEvent,
    reason: Option<HookReason>,
) -> Option<HookHandle> {
    let script = config.hooks_dir().join(event.script_name());

    let facts = match file_facts(&script) {
        Ok(Some(f)) => f,
        // Missing script == feature not configured: skip silently (plan §1).
        Ok(None) => return None,
        Err(e) => {
            eprintln!("hook: cannot stat {}: {e}", script.display());
            return None;
        }
    };
    if let Err(why) = check_hook_security(&facts, auth) {
        eprintln!("hook: refusing to run {}: {why}", script.display());
        return None;
    }

    let log_path = Path::new(&config.log_dir).join("hooks.log");
    let cwd = config.install_root();
    let timeout = config.hook_timeout();

    let join = thread::Builder::new()
        .name("hook".into())
        .spawn(move || run_child(&script, &log_path, &cwd, timeout, event, reason))
        .expect("spawn hook thread");
    Some(HookHandle { join })
}

/// `lstat` the script: `Ok(None)` if it does not exist (skip), `Ok(Some)` with
/// its facts otherwise. `symlink_metadata` does not follow symlinks, so a
/// symlink reports `is_file == false` and is rejected rather than followed.
fn file_facts(path: &Path) -> io::Result<Option<FileFacts>> {
    match std::fs::symlink_metadata(path) {
        Ok(md) => Ok(Some(FileFacts {
            is_file: md.is_file(),
            uid: md.uid(),
            gid: md.gid(),
            mode: md.mode(),
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Pure security predicate (unit-testable): may we execute this hook script?
///
/// Always required: a regular file, owner-executable, not group/world writable.
/// Ownership depends on the auth mode — production demands root:wheel; dev
/// (`--dev-listen`) demands the process's own euid (gid unchecked), so the tests
/// need no root.
fn check_hook_security(f: &FileFacts, auth: AuthMode) -> Result<(), String> {
    if !f.is_file {
        return Err("hook must be a regular file (symlink or special file rejected)".into());
    }
    if f.mode & 0o022 != 0 {
        return Err("hook must not be group/world writable".into());
    }
    if f.mode & 0o100 == 0 {
        return Err("hook must be executable by its owner".into());
    }
    match auth {
        AuthMode::Launchd { .. } => {
            if f.uid != 0 {
                return Err(format!("hook must be owned by root, is uid {}", f.uid));
            }
            if f.gid != 0 {
                return Err(format!("hook must be group wheel (gid 0), is gid {}", f.gid));
            }
        }
        AuthMode::Dev { euid } => {
            if f.uid != euid {
                return Err(format!(
                    "hook (dev mode) must be owned by uid {euid}, is uid {}",
                    f.uid
                ));
            }
        }
    }
    Ok(())
}

/// Body of the hook thread: run the script with a scrubbed environment, tee its
/// output to `hooks.log`, and enforce the timeout with SIGKILL.
fn run_child(
    script: &Path,
    log_path: &Path,
    cwd: &Path,
    timeout: Duration,
    event: HookEvent,
    reason: Option<HookReason>,
) {
    let mut log = match OpenOptions::new().create(true).append(true).open(log_path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("hook: open {}: {e}", log_path.display());
            return;
        }
    };
    let reason_str = reason.map(HookReason::as_str).unwrap_or("");
    let _ = writeln!(
        log,
        "[{}] {} start (reason={})",
        epoch_secs(),
        event.as_str(),
        reason_str
    );

    // Both stdout and stderr append to the same log fd.
    let (out, err) = match (log.try_clone(), log.try_clone()) {
        (Ok(o), Ok(e)) => (o, e),
        _ => {
            let _ = writeln!(log, "[{}] {} clone log fd failed", epoch_secs(), event.as_str());
            return;
        }
    };

    let mut cmd = Command::new(script);
    // Scrub the (launchd/GUI) environment; inject only PATH + the event context.
    cmd.env_clear();
    cmd.env("PATH", HOOK_PATH);
    cmd.env("EASYTIER_EVENT", event.as_str());
    if let Some(r) = reason {
        cmd.env("EASYTIER_REASON", r.as_str());
    }
    cmd.current_dir(cwd);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(out));
    cmd.stderr(Stdio::from(err));

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(
                log,
                "[{}] {} spawn failed: {e}",
                epoch_secs(),
                event.as_str()
            );
            return;
        }
    };

    // We are the sole waiter for this child, so a SIGKILL after the timeout can
    // never hit a reused pid.
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = writeln!(log, "[{}] {} exit ({status})", epoch_secs(), event.as_str());
                return;
            }
            Ok(None) => {}
            Err(e) => {
                let _ = writeln!(log, "[{}] {} wait failed: {e}", epoch_secs(), event.as_str());
                return;
            }
        }
        if Instant::now() >= deadline {
            // SAFETY: kill(2) on our own un-reaped child pid.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGKILL);
            }
            let _ = child.wait();
            let _ = writeln!(
                log,
                "[{}] {} timed out after {:?}, killed",
                epoch_secs(),
                event.as_str(),
                timeout
            );
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Seconds since the Unix epoch, for log lines (avoids a chrono dependency).
fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn facts(is_file: bool, uid: u32, gid: u32, mode: u32) -> FileFacts {
        FileFacts {
            is_file,
            uid,
            gid,
            mode,
        }
    }

    const PROD: AuthMode = AuthMode::Launchd { owner_uid: 501 };
    const DEV: AuthMode = AuthMode::Dev { euid: 501 };

    #[test]
    fn prod_accepts_root_wheel_executable() {
        assert!(check_hook_security(&facts(true, 0, 0, 0o755), PROD).is_ok());
        assert!(check_hook_security(&facts(true, 0, 0, 0o700), PROD).is_ok());
    }

    #[test]
    fn prod_rejects_non_root_owner() {
        let e = check_hook_security(&facts(true, 501, 0, 0o755), PROD).unwrap_err();
        assert!(e.contains("owned by root"), "{e}");
    }

    #[test]
    fn prod_rejects_non_wheel_group() {
        let e = check_hook_security(&facts(true, 0, 20, 0o755), PROD).unwrap_err();
        assert!(e.contains("gid"), "{e}");
    }

    #[test]
    fn rejects_group_or_world_writable() {
        // group-writable (0o020) and world-writable (0o002) both refused.
        let g = check_hook_security(&facts(true, 0, 0, 0o775), PROD).unwrap_err();
        assert!(g.contains("writable"), "{g}");
        let w = check_hook_security(&facts(true, 0, 0, 0o757), PROD).unwrap_err();
        assert!(w.contains("writable"), "{w}");
    }

    #[test]
    fn rejects_non_executable() {
        let e = check_hook_security(&facts(true, 0, 0, 0o644), PROD).unwrap_err();
        assert!(e.contains("executable"), "{e}");
    }

    #[test]
    fn rejects_non_regular_file() {
        // e.g. a symlink: symlink_metadata reports is_file == false.
        let e = check_hook_security(&facts(false, 0, 0, 0o755), PROD).unwrap_err();
        assert!(e.contains("regular file"), "{e}");
    }

    #[test]
    fn dev_accepts_process_owner_and_rejects_others() {
        assert!(check_hook_security(&facts(true, 501, 20, 0o755), DEV).is_ok());
        let e = check_hook_security(&facts(true, 0, 0, 0o755), DEV).unwrap_err();
        assert!(e.contains("uid 501"), "{e}");
    }

    #[test]
    fn event_and_reason_strings() {
        assert_eq!(HookEvent::Up.as_str(), "up");
        assert_eq!(HookEvent::Down.as_str(), "down");
        assert_eq!(HookEvent::Up.script_name(), "up.sh");
        assert_eq!(HookEvent::Down.script_name(), "down.sh");
        assert_eq!(HookReason::Requested.as_str(), "requested");
        assert_eq!(HookReason::OwnerDrop.as_str(), "owner_drop");
        assert_eq!(HookReason::CoreExited.as_str(), "core_exited");
        assert_eq!(HookReason::Janitor.as_str(), "janitor");
    }

    #[test]
    fn missing_script_is_skipped_silently() {
        // hooks_dir points at an empty temp dir: no up.sh -> None, no thread.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("logs")).unwrap();
        let cfg = Config {
            proto: 1,
            owner_uid: 501,
            core_path: root.join("easytier-core").to_string_lossy().into_owned(),
            log_dir: root.join("logs").to_string_lossy().into_owned(),
            hooks_dir: Some(root.join("hooks").to_string_lossy().into_owned()),
            hook_timeout_secs: Some(30),
        };
        assert!(run_hook(&cfg, DEV, HookEvent::Up, None).is_none());
    }

    #[test]
    fn hooks_dir_and_timeout_defaults() {
        // No hooks_dir/hook_timeout keys -> derived from install_root and 30s.
        let cfg = Config {
            proto: 1,
            owner_uid: 501,
            core_path: "/opt/et/bin/easytier-core".into(),
            log_dir: "/opt/et/logs".into(),
            hooks_dir: None,
            hook_timeout_secs: None,
        };
        assert_eq!(cfg.hooks_dir(), PathBuf::from("/opt/et/hooks"));
        assert_eq!(cfg.hook_timeout(), Duration::from_secs(30));
    }
}
