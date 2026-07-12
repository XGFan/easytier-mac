//! Integration tests for easytier-supervisor (DESIGN §8).
//!
//! These drive the actual compiled binary in `--dev-listen` mode (non-root),
//! using shell-script "fake cores". Auth degrades to same-euid, so the test
//! process is allowed to connect. We never assert on internal types — only on
//! wire behavior and OS process state.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_easytier-supervisor");

/// Fake core that terminates on SIGTERM (default disposition).
const CORE_NORMAL: &str = "#!/bin/sh\nwhile true; do sleep 0.2; done\n";
/// Fake core that ignores SIGTERM, so only SIGKILL stops it. Uses perl because
/// macOS `/bin/sh` (bash) still dies on SIGTERM while blocked in a foreground
/// `sleep`, even with `trap '' TERM`; perl's `$SIG{TERM}='IGNORE'` is reliable.
///
/// It touches `core.ready` in its cwd (the install root) *after* the handler is
/// installed, so the test can wait for readiness before sending `stop` and
/// avoid racing SIGTERM against perl's startup.
const CORE_TRAP_TERM: &str = "#!/usr/bin/perl\n$SIG{TERM}='IGNORE';\nopen(my $f,'>','core.ready') and close($f);\nwhile(1){sleep 1}\n";
/// Fake core that exits on its own (cleanly) shortly after start, to exercise
/// the crash/self-exit path deterministically.
const CORE_SELF_EXIT: &str = "#!/bin/sh\nsleep 0.5\nexit 0\n";

/// `up.sh` that records its event into a marker in the cwd (== install root).
const HOOK_UP_MARKER: &str = "#!/bin/sh\nprintf 'event=%s\\n' \"$EASYTIER_EVENT\" > up.marker\n";
/// `down.sh` that records event + reason into a marker in the cwd.
const HOOK_DOWN_MARKER: &str =
    "#!/bin/sh\nprintf 'event=%s reason=%s\\n' \"$EASYTIER_EVENT\" \"$EASYTIER_REASON\" > down.marker\n";
/// `down.sh` that sleeps well past its (1s) timeout; the post-sleep marker write
/// only runs if it was NOT killed, so its absence proves the SIGKILL fired.
const HOOK_DOWN_SLOW: &str = "#!/bin/sh\nsleep 10\nprintf killed=no > down.marker\n";

/// Deadline for a hook side effect (marker / log line) to appear. Generous
/// because the hook forks a child and the suite runs many tests in parallel;
/// `wait_file_contains` returns as soon as the content shows, so a healthy run
/// pays only the real latency.
const HOOK_WAIT: Duration = Duration::from_secs(10);

struct Fixture {
    dir: tempfile::TempDir,
    sock: PathBuf,
    core_path: PathBuf,
    config: PathBuf,
}

impl Fixture {
    /// Build a temp dir with a config and (optionally) a fake-core script.
    fn new(core_script: Option<&str>) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Kept inside the tempdir (auto-cleaned); comfortably under the ~104
        // byte unix-socket path limit for the OS temp base.
        let sock = root.join("sup.sock");
        let core_path = root.join("easytier-core");
        let config = root.join("supervisor.toml");
        // DESIGN layout: log_dir = <install_root>/logs, so install_root (the
        // core cwd) is the tempdir root.
        let log_dir = root.join("logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        if let Some(script) = core_script {
            write_exec(&core_path, script);
        }

        std::fs::write(
            &config,
            format!(
                "proto = 1\nowner_uid = {}\ncore_path = {:?}\nlog_dir = {:?}\n",
                // SAFETY: geteuid never fails.
                unsafe { libc::geteuid() },
                core_path.to_str().unwrap(),
                log_dir.to_str().unwrap(),
            ),
        )
        .unwrap();

        Fixture {
            dir,
            sock,
            core_path,
            config,
        }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Path a hook writes its marker to (cwd == install root == tempdir root).
    fn marker(&self, name: &str) -> PathBuf {
        self.root().join(name)
    }

    /// Write a hook script into `<root>/hooks/<name>` with an explicit mode.
    /// `chmod` is applied after write so the mode survives the process umask (so
    /// e.g. a deliberately group-writable 0o775 is really group-writable).
    fn write_hook(&self, name: &str, body: &str, mode: u32) {
        let hooks = self.root().join("hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let path = hooks.join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Append a shorter hook timeout to the config (for the timeout test).
    fn set_hook_timeout(&self, secs: u64) {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&self.config)
            .unwrap();
        writeln!(f, "hook_timeout_secs = {secs}").unwrap();
    }

    /// Spawn the supervisor and wait until its control socket accepts a client.
    ///
    /// stderr is redirected to `<root>/logs/supervisor.err.log` so tests can
    /// assert on rejection diagnostics (and to keep test output quiet).
    fn start_supervisor(&self) -> Supervisor {
        let errlog = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.root().join("logs").join("supervisor.err.log"))
            .unwrap();
        let child = Command::new(BIN)
            .arg("--config")
            .arg(&self.config)
            .arg("--dev-listen")
            .arg(&self.sock)
            .stderr(std::process::Stdio::from(errlog))
            .spawn()
            .expect("spawn supervisor");
        Supervisor { child }
    }

    fn connect(&self) -> Client {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(stream) = UnixStream::connect(&self.sock) {
                stream
                    .set_read_timeout(Some(Duration::from_secs(12)))
                    .unwrap();
                let reader = BufReader::new(stream.try_clone().unwrap());
                return Client { stream, reader };
            }
            if Instant::now() >= deadline {
                panic!("supervisor socket never came up: {:?}", self.sock);
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct Supervisor {
    child: Child,
}

impl Supervisor {
    /// Wait up to `dur` for the supervisor process to exit; returns its code.
    fn wait_exit(&mut self, dur: Duration) -> Option<i32> {
        wait_child(&mut self.child, dur).map(|s| s.code().unwrap_or(-1))
    }
}

impl Drop for Supervisor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Client {
    fn send(&mut self, line: &str) {
        self.stream.write_all(line.as_bytes()).unwrap();
        self.stream.write_all(b"\n").unwrap();
        self.stream.flush().unwrap();
    }

    fn recv(&mut self) -> String {
        let mut line = String::new();
        let n = self.reader.read_line(&mut line).unwrap_or(0);
        assert!(n > 0, "expected an event line, got EOF");
        line.trim().to_string()
    }

    fn hello(&mut self, takeover: bool) -> String {
        self.send(&format!(
            r#"{{"cmd":"hello","proto":1,"takeover":{takeover}}}"#
        ));
        self.recv()
    }
}

fn write_exec(path: &Path, contents: &str) {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o755)
        .open(path)
        .unwrap();
    f.write_all(contents.as_bytes()).unwrap();
}

fn is_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes for the process' existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_dead(pid: i32, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if !is_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !is_alive(pid)
}

fn wait_file(path: &Path, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

/// Wait until `path` exists and contains `needle` (avoids racing a file that is
/// created empty a moment before its content is written).
fn wait_file_contains(path: &Path, needle: &str, dur: Duration) -> bool {
    let deadline = Instant::now() + dur;
    loop {
        if let Ok(s) = std::fs::read_to_string(path) {
            if s.contains(needle) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn wait_child(child: &mut Child, dur: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + dur;
    loop {
        match child.try_wait().unwrap() {
            Some(status) => return Some(status),
            None if Instant::now() >= deadline => return None,
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn extract_i64(line: &str, key: &str) -> Option<i64> {
    let pat = format!("\"{key}\":");
    let start = line.find(&pat)? + pat.len();
    let rest = line[start..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

fn has_event(line: &str, ev: &str) -> bool {
    line.contains(&format!("\"event\":\"{ev}\""))
}

#[test]
fn hello_start_status_stop() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    let hello = c.hello(false);
    assert!(has_event(&hello, "hello"), "hello reply: {hello}");
    assert!(hello.contains("\"core\":\"stopped\""), "hello: {hello}");

    c.send(r#"{"cmd":"start"}"#);
    let started = c.recv();
    assert!(has_event(&started, "core_started"), "start: {started}");
    let pid = extract_i64(&started, "pid").expect("pid") as i32;
    let port = extract_i64(&started, "rpc_port").expect("rpc_port");
    assert!(port > 0 && port <= 65535, "rpc_port out of range: {port}");
    assert!(is_alive(pid), "core should be running after start");

    c.send(r#"{"cmd":"status"}"#);
    let status = c.recv();
    assert!(has_event(&status, "status"), "status: {status}");
    assert!(status.contains("\"core\":\"running\""), "status: {status}");
    assert_eq!(extract_i64(&status, "pid"), Some(pid as i64));

    c.send(r#"{"cmd":"stop"}"#);
    let stopped = c.recv();
    assert!(has_event(&stopped, "core_stopped"), "stop: {stopped}");
    assert!(wait_dead(pid, Duration::from_secs(3)), "core should be dead after stop");

    drop(sup);
}

#[test]
fn disconnect_stops_core_and_exits() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    let mut sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let started = c.recv();
    let pid = extract_i64(&started, "pid").expect("pid") as i32;
    assert!(is_alive(pid));

    // Owner disconnects abruptly -> stop semantics + supervisor exit(0).
    drop(c);

    assert!(wait_dead(pid, Duration::from_secs(5)), "core killed on disconnect");
    assert_eq!(
        sup.wait_exit(Duration::from_secs(5)),
        Some(0),
        "supervisor should exit(0) after owner disconnect"
    );
}

#[test]
fn stop_escalates_to_sigkill() {
    let fx = Fixture::new(Some(CORE_TRAP_TERM));
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let started = c.recv();
    let pid = extract_i64(&started, "pid").expect("pid") as i32;
    assert!(is_alive(pid));

    // Wait until the core has actually installed its SIGTERM-ignore handler,
    // otherwise SIGTERM could race perl's startup and kill it immediately.
    assert!(
        wait_file(&fx.root().join("core.ready"), Duration::from_secs(5)),
        "core never signaled readiness"
    );

    // Core ignores SIGTERM; supervisor must escalate to SIGKILL after ~5s.
    let began = Instant::now();
    c.send(r#"{"cmd":"stop"}"#);
    let stopped = c.recv();
    let elapsed = began.elapsed();
    assert!(has_event(&stopped, "core_stopped"), "stop: {stopped}");
    assert!(
        elapsed >= Duration::from_secs(4),
        "SIGKILL escalation should take ~5s, took {elapsed:?}"
    );
    assert!(wait_dead(pid, Duration::from_secs(2)), "core dead after SIGKILL");

    drop(sup);
}

#[test]
fn second_owner_is_busy_then_takeover_kicks() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    let sup = fx.start_supervisor();

    let mut a = fx.connect();
    assert!(has_event(&a.hello(false), "hello"));

    // B without takeover -> busy, then closed.
    let mut b = fx.connect();
    let busy = b.hello(false);
    assert!(has_event(&busy, "busy"), "expected busy, got: {busy}");

    // C with takeover -> A receives kicked, C becomes owner.
    let mut cc = fx.connect();
    assert!(has_event(&cc.hello(true), "hello"), "C should own");

    let kicked = a.recv();
    assert!(has_event(&kicked, "kicked"), "A should be kicked, got: {kicked}");

    // New owner can drive the core.
    cc.send(r#"{"cmd":"status"}"#);
    assert!(has_event(&cc.recv(), "status"));

    drop(sup);
}

#[test]
fn takeover_preserves_running_core() {
    // DESIGN §4 takeover semantics: the new owner inherits the SAME running core.
    let fx = Fixture::new(Some(CORE_NORMAL));
    let sup = fx.start_supervisor();

    let mut a = fx.connect();
    assert!(has_event(&a.hello(false), "hello"));
    a.send(r#"{"cmd":"start"}"#);
    let started = a.recv();
    let pid = extract_i64(&started, "pid").expect("pid") as i32;
    assert!(is_alive(pid), "core should be running under owner A");

    // B takes over: A is kicked, B becomes owner.
    let mut b = fx.connect();
    let bhello = b.hello(true);
    assert!(has_event(&bhello, "hello"), "B hello: {bhello}");
    // B's hello reflects the inherited running core.
    assert!(bhello.contains("\"core\":\"running\""), "B hello: {bhello}");

    let kicked = a.recv();
    assert!(has_event(&kicked, "kicked"), "A should be kicked: {kicked}");

    // The core survived the ownership change...
    assert!(is_alive(pid), "core must survive takeover");
    // ...and B drives the very same pid.
    b.send(r#"{"cmd":"status"}"#);
    let status = b.recv();
    assert!(has_event(&status, "status"), "B status: {status}");
    assert!(status.contains("\"core\":\"running\""), "B status: {status}");
    assert_eq!(
        extract_i64(&status, "pid"),
        Some(pid as i64),
        "B must see the same core pid"
    );

    drop(sup);
}

#[test]
fn stop_after_natural_exit_is_clean() {
    // Deterministic facet of the reap/signal race: the core exits on its own,
    // the supervisor reaps it and pushes core_exited, and a subsequent stop must
    // NOT signal the (already reaped) pid — it reports already_stopped.
    let fx = Fixture::new(Some(CORE_SELF_EXIT));
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let started = c.recv();
    assert!(has_event(&started, "core_started"), "start: {started}");
    let port = extract_i64(&started, "rpc_port").expect("rpc_port");
    assert!(port > 0 && port <= 65535, "rpc_port out of range: {port}");

    // Core exits by itself (~0.5s) -> supervisor pushes core_exited{code:0}.
    let exited = c.recv();
    assert!(has_event(&exited, "core_exited"), "expected core_exited: {exited}");
    assert_eq!(extract_i64(&exited, "code"), Some(0), "exit code: {exited}");

    // Stop on an already-exited core is clean and honest about it.
    c.send(r#"{"cmd":"stop"}"#);
    let stopped = c.recv();
    assert!(has_event(&stopped, "core_stopped"), "stop: {stopped}");
    assert!(
        stopped.contains("already_stopped"),
        "stop on dead core should say already_stopped: {stopped}"
    );

    drop(sup);
}

#[test]
fn janitor_kills_orphan_core_on_activation() {
    // core_path need not exist as a file; the janitor matches argv[0] strings.
    let fx = Fixture::new(None);
    let core_path = fx.core_path.to_str().unwrap().to_string();

    // Orphan whose argv[0] is exactly core_path (as a real core would be). We
    // exec /bin/sh with arg0 overridden so `ps -o args` shows core_path first.
    let mut orphan = Command::new("/bin/sh")
        .arg0(&core_path)
        .arg("-c")
        .arg("while true; do sleep 0.2; done")
        .spawn()
        .expect("spawn orphan");
    let opid = orphan.id() as i32;
    std::thread::sleep(Duration::from_millis(300));
    assert!(is_alive(opid), "orphan should be running before supervisor starts");

    // Supervisor runs janitor at activation and should SIGKILL the orphan.
    let sup = fx.start_supervisor();

    let status = wait_child(&mut orphan, Duration::from_secs(6));
    // Reaped by us; confirm it died and was not still looping.
    assert!(status.is_some(), "janitor did not kill the orphan in time");
    assert!(!is_alive(opid), "orphan still alive after janitor sweep");

    drop(sup);
}

// ---------------------------------------------------------------------------
// Lifecycle hooks (plan §4 H2-H4). Fixtures run in --dev-listen mode, so hook
// auth degrades to same-euid and the scripts we write pass the security gate.
// ---------------------------------------------------------------------------

/// H2 (up + reason=requested): start fires `up.sh`; an explicit `stop` fires
/// `down.sh` with reason=requested.
#[test]
fn hook_up_then_down_requested() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    fx.write_hook("up.sh", HOOK_UP_MARKER, 0o755);
    fx.write_hook("down.sh", HOOK_DOWN_MARKER, 0o755);
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let started = c.recv();
    let pid = extract_i64(&started, "pid").expect("pid") as i32;
    assert!(
        wait_file_contains(&fx.marker("up.marker"), "event=up", HOOK_WAIT),
        "up hook should have run with EASYTIER_EVENT=up"
    );

    c.send(r#"{"cmd":"stop"}"#);
    assert!(has_event(&c.recv(), "core_stopped"));
    assert!(wait_dead(pid, Duration::from_secs(3)), "core dead after stop");
    assert!(
        wait_file_contains(
            &fx.marker("down.marker"),
            "event=down reason=requested",
            HOOK_WAIT
        ),
        "down hook should have run with reason=requested"
    );

    drop(sup);
}

/// H2 (reason=owner_drop): a genuine owner disconnect fires `down.sh` with
/// reason=owner_drop, and the supervisor awaits it before exit(0).
#[test]
fn hook_down_owner_drop() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    fx.write_hook("down.sh", HOOK_DOWN_MARKER, 0o755);
    let mut sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let pid = extract_i64(&c.recv(), "pid").expect("pid") as i32;
    assert!(is_alive(pid));

    // Owner disconnects -> stop(owner_drop) + join down hook + exit(0).
    drop(c);

    assert!(wait_dead(pid, Duration::from_secs(5)), "core dead on disconnect");
    assert!(
        wait_file_contains(
            &fx.marker("down.marker"),
            "event=down reason=owner_drop",
            HOOK_WAIT
        ),
        "down hook should have run with reason=owner_drop"
    );
    assert_eq!(
        sup.wait_exit(Duration::from_secs(5)),
        Some(0),
        "supervisor exits(0) after owner disconnect"
    );
}

/// H2 (reason=core_exited): an external SIGKILL of the core is an unexpected
/// exit and fires `down.sh` with reason=core_exited.
#[test]
fn hook_down_core_exited() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    fx.write_hook("down.sh", HOOK_DOWN_MARKER, 0o755);
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let pid = extract_i64(&c.recv(), "pid").expect("pid") as i32;
    assert!(is_alive(pid));

    // Kill the core out from under the supervisor -> unexpected exit path.
    // SAFETY: SIGKILL to the fake core pid we just started.
    unsafe { libc::kill(pid, libc::SIGKILL) };

    let exited = c.recv();
    assert!(has_event(&exited, "core_exited"), "expected core_exited: {exited}");
    assert!(
        wait_file_contains(
            &fx.marker("down.marker"),
            "event=down reason=core_exited",
            HOOK_WAIT,
        ),
        "down hook should have run with reason=core_exited"
    );

    drop(sup);
}

/// H2 (reason=janitor): activation-time orphan sweep that kills something fires
/// `down.sh` with reason=janitor (backstop for a crashed predecessor).
#[test]
fn hook_down_janitor() {
    let fx = Fixture::new(None);
    fx.write_hook("down.sh", HOOK_DOWN_MARKER, 0o755);
    let core_path = fx.core_path.to_str().unwrap().to_string();

    // Orphan whose argv[0] is exactly core_path, as a real leaked core would be.
    let mut orphan = Command::new("/bin/sh")
        .arg0(&core_path)
        .arg("-c")
        .arg("while true; do sleep 0.2; done")
        .spawn()
        .expect("spawn orphan");
    let opid = orphan.id() as i32;
    std::thread::sleep(Duration::from_millis(300));
    assert!(is_alive(opid), "orphan should be running before supervisor starts");

    // Activation sweep kills the orphan (killed>0) -> down(janitor).
    let sup = fx.start_supervisor();

    assert!(
        wait_child(&mut orphan, Duration::from_secs(6)).is_some(),
        "janitor did not kill the orphan in time"
    );
    assert!(
        wait_file_contains(
            &fx.marker("down.marker"),
            "event=down reason=janitor",
            HOOK_WAIT
        ),
        "down hook should have run with reason=janitor"
    );

    drop(sup);
}

/// H3: a hook that outlives the timeout is SIGKILLed, hooks.log records it, and
/// the state machine keeps working (a later start/stop still succeeds).
#[test]
fn hook_timeout_killed_but_state_machine_ok() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    fx.write_hook("down.sh", HOOK_DOWN_SLOW, 0o755);
    fx.set_hook_timeout(1);
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let pid = extract_i64(&c.recv(), "pid").expect("pid") as i32;

    // Stop spawns the slow down hook (detached) but must not block: core_stopped
    // comes back promptly even though the script sleeps for 3s.
    let began = Instant::now();
    c.send(r#"{"cmd":"stop"}"#);
    assert!(has_event(&c.recv(), "core_stopped"));
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "stop must not block on the slow hook, took {:?}",
        began.elapsed()
    );
    assert!(wait_dead(pid, Duration::from_secs(3)), "core dead after stop");

    // The hook is SIGKILLed at ~1s; hooks.log records the timeout and the
    // post-sleep marker write never happens.
    let hooks_log = fx.root().join("logs").join("hooks.log");
    assert!(
        wait_file_contains(&hooks_log, "timed out", HOOK_WAIT),
        "hooks.log should record the timeout"
    );
    assert!(
        !fx.marker("down.marker").exists(),
        "slow hook was killed before writing its marker"
    );

    // Skip the down hook for the next cycle so we prove the state machine
    // recovered without leaving another slow child behind.
    std::fs::remove_file(fx.root().join("hooks").join("down.sh")).unwrap();
    c.send(r#"{"cmd":"start"}"#);
    let pid2 = extract_i64(&c.recv(), "pid").expect("pid2") as i32;
    assert!(is_alive(pid2), "start works after a hook timeout");
    c.send(r#"{"cmd":"stop"}"#);
    assert!(has_event(&c.recv(), "core_stopped"));
    assert!(wait_dead(pid2, Duration::from_secs(3)), "second core dead after stop");

    drop(sup);
}

/// H4: a group-writable hook is refused; the core still starts/stops and the
/// refusal is logged to supervisor.err.log.
#[test]
fn hook_group_writable_is_rejected() {
    let fx = Fixture::new(Some(CORE_NORMAL));
    // 0o775 is group-writable -> security gate rejects it.
    fx.write_hook("up.sh", HOOK_UP_MARKER, 0o775);
    let sup = fx.start_supervisor();
    let mut c = fx.connect();

    c.hello(false);
    c.send(r#"{"cmd":"start"}"#);
    let pid = extract_i64(&c.recv(), "pid").expect("pid") as i32;
    assert!(is_alive(pid), "core still starts even though the up hook is rejected");

    let errlog = fx.root().join("logs").join("supervisor.err.log");
    assert!(
        wait_file_contains(&errlog, "refusing to run", HOOK_WAIT),
        "rejection should be logged to supervisor.err.log"
    );
    assert!(
        !fx.marker("up.marker").exists(),
        "rejected up hook must not have executed"
    );

    // State machine unaffected: status and stop still work.
    c.send(r#"{"cmd":"status"}"#);
    assert!(has_event(&c.recv(), "status"));
    c.send(r#"{"cmd":"stop"}"#);
    assert!(has_event(&c.recv(), "core_stopped"));
    assert!(wait_dead(pid, Duration::from_secs(3)), "core dead after stop");

    drop(sup);
}
