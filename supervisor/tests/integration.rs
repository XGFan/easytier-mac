//! Integration tests for easytier-supervisor (DESIGN §8).
//!
//! These drive the actual compiled binary in `--dev-listen` mode (non-root),
//! using shell-script "fake cores". Auth degrades to same-euid, so the test
//! process is allowed to connect. We never assert on internal types — only on
//! wire behavior and OS process state.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::OpenOptionsExt;
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

    /// Spawn the supervisor and wait until its control socket accepts a client.
    fn start_supervisor(&self) -> Supervisor {
        let child = Command::new(BIN)
            .arg("--config")
            .arg(&self.config)
            .arg("--dev-listen")
            .arg(&self.sock)
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
