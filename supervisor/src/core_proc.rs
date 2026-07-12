//! easytier-core process lifecycle: spawn, stop-with-escalation, and a monitor
//! thread that reaps the child and reports unexpected exits (DESIGN §5).
//!
//! ## pid-reuse safety invariant
//!
//! The monitor thread is the **sole reaper** (the only caller of
//! `Child::try_wait`) **and the sole signaller** (the only place `kill(2)` is
//! sent to the core). Both happen on that one thread, and it stops signalling
//! the instant `try_wait` reports the child reaped. Therefore a signal can
//! never be delivered to a pid that has already been reaped (and possibly
//! recycled by the OS). `stop()` never signals — it only sets a flag and waits.

use std::fs::OpenOptions;
use std::io;
use std::net::TcpListener;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::hooks::{self, HookEvent, HookHandle, HookReason};
use crate::proto::Event;
use crate::server::Shared;

/// SIGTERM -> SIGKILL escalation delay, and the monitor's poll cadence
/// (DESIGN §5: "SIGTERM -> 100ms 轮询共 5s -> SIGKILL").
const TERM_GRACE: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Core process state tracked under `Shared::st`.
pub enum CoreProc {
    Stopped,
    Running(Running),
}

pub struct Running {
    pub pid: i32,
    pub rpc_port: u16,
    /// Set by `stop()` so the monitor escalates (SIGTERM/SIGKILL) and knows this
    /// exit was requested (stays quiet: no `core_exited` push, no crash sweep).
    pub stop_requested: bool,
}

/// How the core process terminated, as reported to the owner.
pub struct ExitInfo {
    pub code: Option<i32>,
    pub signal: Option<i32>,
}

impl ExitInfo {
    fn from_status(status: ExitStatus) -> ExitInfo {
        ExitInfo {
            code: status.code(),
            signal: status.signal(),
        }
    }
}

/// Probe a free loopback TCP port by binding `127.0.0.1:0` and releasing it.
///
/// There is an inherent race between release and the core re-binding it; the
/// window is accepted for M0 (DESIGN §5).
pub fn pick_free_port() -> io::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

fn build_command(config: &Config, port: u16) -> io::Result<Command> {
    let log_path = Path::new(&config.log_dir).join("core.out.log");
    let out = OpenOptions::new().create(true).append(true).open(&log_path)?;
    let err = out.try_clone()?;

    let mut cmd = Command::new(&config.core_path);
    // DESIGN §5 argv. The default --rpc-portal-whitelist is already loopback
    // only (127.0.0.0/8, ::1/128; easytier/src/instance/instance.rs:205), and
    // we bind rpc-portal to 127.0.0.1, so we deliberately omit the whitelist
    // flag rather than risk mis-spelling it.
    cmd.arg("--daemon")
        .arg("--rpc-portal")
        .arg(format!("127.0.0.1:{port}"));
    // The GUI/launchd environment must not leak ET_* knobs into the core;
    // start from an empty env and re-add only a minimal PATH.
    cmd.env_clear();
    cmd.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    cmd.current_dir(config.install_root());
    // argv[0] must be exactly core_path so a future janitor pass can recognize
    // an orphan of ours (see janitor::argv0_matches).
    cmd.arg0(&config.core_path);
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::from(out));
    cmd.stderr(Stdio::from(err));
    Ok(cmd)
}

/// Spawn the core if not already running; returns `(pid, rpc_port)`.
///
/// Idempotent: if a core is already running its existing `(pid, rpc_port)` is
/// returned without spawning a second one.
///
/// The spawn happens **while holding `st`** so state and reality never diverge:
/// a core process exists iff `st.core` is `Running`. The crash-path sweep relies
/// on this — observing `Stopped` under the lock means no core process exists.
pub fn start(shared: &Arc<Shared>) -> Result<(i32, u16), String> {
    let mut st = shared.st.lock().unwrap();
    if let CoreProc::Running(r) = &st.core {
        return Ok((r.pid, r.rpc_port));
    }

    let port = pick_free_port().map_err(|e| format!("could not pick a free port: {e}"))?;
    let mut cmd = build_command(&shared.config, port).map_err(|e| format!("open core log: {e}"))?;
    let child = cmd
        .spawn()
        .map_err(|e| format!("spawn {}: {e}", shared.config.core_path))?;
    let pid = child.id() as i32;
    st.core = CoreProc::Running(Running {
        pid,
        rpc_port: port,
        stop_requested: false,
    });
    drop(st);

    spawn_monitor(shared.clone(), child, pid);

    // Core process now exists: fire the `up` hook (detached — it must not block
    // the state machine). Semantics are "core process started", which may be
    // before the TUN is ready; scripts that need the interface retry (DESIGN).
    let _ = hooks::run_hook(&shared.config, shared.auth, HookEvent::Up, None);
    Ok((pid, port))
}

fn send_signal(pid: i32, sig: libc::c_int) {
    // SAFETY: kill(2) targeting our own child pid; only ever called by the
    // monitor thread while `try_wait` has confirmed the child is not yet reaped.
    unsafe {
        libc::kill(pid, sig);
    }
}

/// Reaper/watchdog for one core generation (see the module-level invariant).
///
/// `easytier-core --daemon` does NOT fork/detach: it only registers a
/// `DaemonGuard` (easytier/src/core.rs:1375). So the child we spawn IS the core
/// process and remains our direct child, which is what makes pid tracking and
/// reaping valid here.
fn spawn_monitor(shared: Arc<Shared>, mut child: Child, pid: i32) {
    let spawn_result = thread::Builder::new()
        .name("core-monitor".into())
        .spawn(move || {
            let mut term_deadline: Option<Instant> = None;
            let mut kill_sent = false;

            loop {
                // Sole reaper. Non-blocking so we can also drive escalation. Once
                // this yields Some, the pid is reaped and we return without ever
                // signalling again (pid-reuse safety).
                match child.try_wait() {
                    Ok(Some(status)) => {
                        handle_core_exit(&shared, pid, ExitInfo::from_status(status));
                        return;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("core-monitor: try_wait failed for pid={pid}: {e}");
                        handle_core_exit(
                            &shared,
                            pid,
                            ExitInfo {
                                code: None,
                                signal: None,
                            },
                        );
                        return;
                    }
                }

                // Sole signaller. Escalate only when a stop was requested for our
                // generation. The `try_wait` above returned `None` in this same
                // iteration, so the child is confirmed un-reaped right now.
                let stop_requested = {
                    let st = shared.st.lock().unwrap();
                    matches!(&st.core, CoreProc::Running(r) if r.pid == pid && r.stop_requested)
                };
                if stop_requested {
                    match term_deadline {
                        None => {
                            send_signal(pid, libc::SIGTERM);
                            term_deadline = Some(Instant::now() + TERM_GRACE);
                        }
                        Some(deadline) if !kill_sent && Instant::now() >= deadline => {
                            send_signal(pid, libc::SIGKILL);
                            kill_sent = true;
                        }
                        _ => {}
                    }
                }

                // Sleep until a stop request wakes us or the poll interval lapses.
                let st = shared.st.lock().unwrap();
                let _ = shared.cv.wait_timeout(st, POLL_INTERVAL).unwrap();
            }
        });

    // A failed thread spawn (only under severe resource exhaustion) would leave
    // an unmonitored, unreaped core; we accept that extreme case and fail loud.
    spawn_result.expect("spawn core-monitor thread");
}

/// Transition to `Stopped` for this generation and, if the exit was NOT
/// requested, report it to the owner and best-effort clean residue.
fn handle_core_exit(shared: &Arc<Shared>, pid: i32, info: ExitInfo) {
    let expected = {
        let mut st = shared.st.lock().unwrap();
        let expected = match &st.core {
            CoreProc::Running(r) if r.pid == pid => r.stop_requested,
            // Not our generation (or already cleared): stay quiet.
            _ => true,
        };
        if matches!(&st.core, CoreProc::Running(r) if r.pid == pid) {
            st.core = CoreProc::Stopped;
        }
        shared.cv.notify_all();
        expected
    };

    if expected {
        // Requested stop: `stop()` reports `core_stopped`; nothing to push here.
        return;
    }

    // Unexpected exit (crash / external kill): report to the owner (DESIGN §4).
    // We clean up but do NOT restart — the restart decision belongs to the
    // client.
    shared.write_to_owner(&Event::CoreExited {
        code: info.code,
        signal: info.signal,
    });

    // Mirror that transition to the `down` hook. Detached: nothing here exits, so
    // there is no need to await it. This is mutually exclusive with the `stop`
    // path's `down` — a requested stop takes the early `return` above and never
    // reaches this branch, so a given core death fires exactly one `down`.
    let _ = hooks::run_hook(
        &shared.config,
        shared.auth,
        HookEvent::Down,
        Some(HookReason::CoreExited),
    );

    // Guarded orphan sweep. Because `start()` spawns under `st`, observing
    // `Stopped` under the lock means no core process exists; holding the lock
    // across the sweep stops a concurrent (idempotent) `start()` from racing in
    // a fresh core that this sweep would then SIGKILL. If the owner already
    // restarted, `st.core` is `Running` and we leave that core untouched.
    let st = shared.st.lock().unwrap();
    if matches!(st.core, CoreProc::Stopped) {
        crate::janitor::sweep_orphans(&shared.config, shared.auth, None);
        crate::janitor::cleanup_routes(&shared.config);
    }
    drop(st);
}

/// Result of a `stop()`: whether a core was actually reaped, and (if so) the
/// `down` hook it fired.
pub struct StopOutcome {
    /// True if a core was running (and is now stopped), false if none was.
    pub was_running: bool,
    /// The `down` hook spawned for this stop, if a core was reaped and a hook
    /// script was present. The owner-disconnect exit path joins this (bounded by
    /// the hook timeout) so a `down` script is not cut short by `exit(0)`; every
    /// other caller drops it to detach.
    pub hook: Option<HookHandle>,
}

/// Request the core stop and block until the monitor has reaped it.
///
/// Sets `stop_requested`, wakes the monitor (which sends SIGTERM, escalates to
/// SIGKILL after `TERM_GRACE`, and reaps), then waits for `Stopped`. `stop()`
/// itself never signals — see the module-level pid-reuse invariant.
///
/// Once the core is confirmed reaped, fires the `down` hook with `reason`
/// (`requested` for a `stop` command, `owner_drop` for a disconnect). When no
/// core was running there is nothing to tear down, so no hook fires.
pub fn stop(shared: &Arc<Shared>, reason: HookReason) -> StopOutcome {
    let mut st = shared.st.lock().unwrap();
    match &mut st.core {
        CoreProc::Running(r) => r.stop_requested = true,
        CoreProc::Stopped => {
            return StopOutcome {
                was_running: false,
                hook: None,
            }
        }
    }
    shared.cv.notify_all();

    loop {
        if matches!(st.core, CoreProc::Stopped) {
            break;
        }
        // Bounded wait so a missed notify can never wedge us permanently.
        let (guard, _) = shared.cv.wait_timeout(st, Duration::from_millis(200)).unwrap();
        st = guard;
    }
    drop(st);

    // Core is reaped (waitpid confirmed via the monitor's transition to
    // `Stopped`): now safe to run the `down` hook.
    let hook = hooks::run_hook(&shared.config, shared.auth, HookEvent::Down, Some(reason));
    StopOutcome {
        was_running: true,
        hook,
    }
}
