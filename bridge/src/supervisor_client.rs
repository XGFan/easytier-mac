//! Unix-socket JSON-lines client for the EasyTier supervisor (DESIGN §4/§8).
//!
//! A background driver task owns the connection. It connects to the control
//! socket (triggering launchd socket activation), performs the `hello`
//! handshake, then services request/reply commands (`start`/`stop`/`status`)
//! while forwarding unsolicited pushes (`core_exited`/`kicked`/`busy`) to a
//! [`SupervisorEvent`] channel. On any disconnect it reconnects with capped
//! exponential backoff (initial 1s, max 30s), which is compatible with the
//! launchd `ThrottleInterval=5` activation throttle.
//!
//! Only one command may be in flight at a time; the GUI issues commands
//! sequentially so this keeps reply correlation trivial (the protocol carries
//! no request ids).
//!
//! Migrated verbatim from `gui/src-tauri/src/supervisor_client.rs` (the Tauri GUI
//! stays the source of truth until native parity). A few methods of the public
//! surface (`request_reconnect`, `status`) are unused by the bridge FFI today but
//! kept intact so the module does not diverge; hence the module-level allow.
#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

use crate::proto::{Cmd, CoreState, Event, PROTO_VERSION, decode_event, encode_cmd};

/// Default control socket path (DESIGN §1). Overridden by `ET_SUPERVISOR_SOCKET`.
pub const DEFAULT_SOCKET_PATH: &str = "/var/run/easytier.supervisor.sock";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// Resolve the control socket path, honouring the `ET_SUPERVISOR_SOCKET` override
/// used for dev/integration against a `--dev-listen` supervisor.
pub fn resolve_socket_path() -> PathBuf {
    std::env::var_os("ET_SUPERVISOR_SOCKET")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET_PATH))
}

/// Reply payload for a successful `start` (or idempotent re-`start`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StartInfo {
    pub pid: i32,
    pub rpc_port: u16,
}

/// Reply payload for `status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct StatusInfo {
    pub core: CoreState,
    pub pid: Option<i32>,
    pub rpc_port: Option<u16>,
}

/// Events surfaced from the driver to the app (forwarded to Tauri as events and
/// used to drive RPC setup / auto-restart).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorEvent {
    Connected {
        version: String,
        core: CoreState,
        rpc_port: Option<u16>,
    },
    Disconnected,
    CoreStarted {
        pid: i32,
        rpc_port: u16,
    },
    CoreStopped {
        reason: String,
    },
    CoreExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Kicked,
    Busy {
        owner: bool,
    },
    Error {
        code: String,
        msg: String,
    },
}

/// Driver configuration.
#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub socket_path: PathBuf,
    pub takeover: bool,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl Default for SupervisorConfig {
    fn default() -> Self {
        Self {
            socket_path: resolve_socket_path(),
            takeover: false,
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
        }
    }
}

enum Request {
    Start(oneshot::Sender<Result<StartInfo, String>>),
    Stop(oneshot::Sender<Result<(), String>>),
    Status(oneshot::Sender<Result<StatusInfo, String>>),
}

impl Request {
    fn cmd(&self) -> Cmd {
        match self {
            Request::Start(_) => Cmd::Start,
            Request::Stop(_) => Cmd::Stop,
            Request::Status(_) => Cmd::Status,
        }
    }

    /// Fail this pending request with `msg` (used on write errors / protocol
    /// mismatches / supervisor `error` replies).
    fn fail(self, msg: String) {
        match self {
            Request::Start(tx) => {
                let _ = tx.send(Err(msg));
            }
            Request::Stop(tx) => {
                let _ = tx.send(Err(msg));
            }
            Request::Status(tx) => {
                let _ = tx.send(Err(msg));
            }
        }
    }
}

/// Signals the driver reads besides the command channel.
struct Control {
    connected: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
    takeover: Arc<Notify>,
    reconnect: Arc<Notify>,
}

/// Handle to the background supervisor driver.
pub struct SupervisorClient {
    cmd_tx: mpsc::Sender<Request>,
    connected: Arc<AtomicBool>,
    shutdown: Arc<Notify>,
    takeover: Arc<Notify>,
    reconnect: Arc<Notify>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl SupervisorClient {
    /// Spawn the background driver. Requires a Tokio runtime.
    pub fn spawn(config: SupervisorConfig, event_tx: UnboundedSender<SupervisorEvent>) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel::<Request>(16);
        let connected = Arc::new(AtomicBool::new(false));
        let shutdown = Arc::new(Notify::new());
        let takeover = Arc::new(Notify::new());
        let reconnect = Arc::new(Notify::new());

        let task = tokio::spawn(run_driver(
            config,
            cmd_rx,
            event_tx,
            Control {
                connected: connected.clone(),
                shutdown: shutdown.clone(),
                takeover: takeover.clone(),
                reconnect: reconnect.clone(),
            },
        ));

        Self {
            cmd_tx,
            connected,
            shutdown,
            takeover,
            reconnect,
            task: Mutex::new(Some(task)),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    pub async fn start(&self) -> Result<StartInfo, String> {
        let (tx, rx) = oneshot::channel();
        self.dispatch(Request::Start(tx), rx).await
    }

    pub async fn stop(&self) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        self.dispatch(Request::Stop(tx), rx).await
    }

    pub async fn status(&self) -> Result<StatusInfo, String> {
        let (tx, rx) = oneshot::channel();
        self.dispatch(Request::Status(tx), rx).await
    }

    async fn dispatch<T>(
        &self,
        req: Request,
        rx: oneshot::Receiver<Result<T, String>>,
    ) -> Result<T, String> {
        self.cmd_tx
            .send(req)
            .await
            .map_err(|_| "supervisor client stopped".to_string())?;
        match tokio::time::timeout(REQUEST_TIMEOUT, rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err("supervisor disconnected before reply".to_string()),
            Err(_) => Err("supervisor request timed out".to_string()),
        }
    }

    /// Request a one-shot reconnect that takes over an existing owner lease,
    /// used after the user confirms takeover of a `busy` supervisor (DESIGN §8).
    /// While paused on `busy` the driver reconnects only in response to this.
    pub fn request_takeover(&self) {
        self.takeover.notify_one();
    }

    /// Nudge the driver to reconnect immediately instead of waiting out the
    /// current backoff (e.g. right after a successful install).
    pub fn request_reconnect(&self) {
        self.reconnect.notify_one();
    }

    /// Signal the driver to stop and drop the control connection. Dropping the
    /// owner connection is a `stop` per DESIGN §4, so the supervisor tears the
    /// core down and exits; we intentionally do not send an explicit `stop`.
    pub async fn shutdown(&self) {
        self.shutdown.notify_one();
        let handle = self.task.lock().unwrap().take();
        if let Some(handle) = handle {
            let _ = handle.await;
        }
    }
}

struct Session {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    write: OwnedWriteHalf,
}

enum SessionEnd {
    Disconnected,
    Shutdown,
}

/// Why a connect attempt failed. `Busy` means another owner holds the lease and
/// we must not auto-reconnect; `Other` is a transient error eligible for backoff.
enum ConnectErr {
    Busy,
    Other,
}

async fn run_driver(
    config: SupervisorConfig,
    mut cmd_rx: mpsc::Receiver<Request>,
    event_tx: UnboundedSender<SupervisorEvent>,
    control: Control,
) {
    let Control {
        connected,
        shutdown,
        takeover,
        reconnect,
    } = control;
    let mut backoff = config.initial_backoff;
    // Whether the next connect attempt should request takeover of an existing
    // owner lease. Only set after the user confirms a takeover.
    let mut next_takeover = config.takeover;

    loop {
        let attempt = tokio::select! {
            _ = shutdown.notified() => break,
            s = connect_and_hello(&config, next_takeover, &event_tx) => s,
        };
        next_takeover = false;

        match attempt {
            Ok(session) => {
                backoff = config.initial_backoff;
                connected.store(true, Ordering::SeqCst);
                let end = run_session(session, &mut cmd_rx, &event_tx, &shutdown).await;
                connected.store(false, Ordering::SeqCst);
                let _ = event_tx.send(SupervisorEvent::Disconnected);
                if matches!(end, SessionEnd::Shutdown) {
                    break;
                }
                // Normal disconnect: fall through to backoff, then reconnect.
            }
            Err(ConnectErr::Busy) => {
                // Another owner holds the lease. Stop auto-reconnecting (avoids a
                // busy/reconnect storm) and wait for an explicit, user-confirmed
                // takeover request (DESIGN §8).
                tokio::select! {
                    _ = shutdown.notified() => break,
                    _ = takeover.notified() => {
                        next_takeover = true;
                        continue; // reconnect immediately, taking over
                    }
                }
            }
            Err(ConnectErr::Other) => {
                // Transient connect/handshake failure: fall through to backoff.
            }
        }

        // Back off before the next attempt; wake early on shutdown or an
        // explicit reconnect request (e.g. right after install).
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = reconnect.notified() => {}
            _ = tokio::time::sleep(backoff) => {}
        }
        backoff = (backoff * 2).min(config.max_backoff);
    }
    connected.store(false, Ordering::SeqCst);
}

async fn connect_and_hello(
    config: &SupervisorConfig,
    takeover: bool,
    event_tx: &UnboundedSender<SupervisorEvent>,
) -> Result<Session, ConnectErr> {
    let stream = match UnixStream::connect(&config.socket_path).await {
        Ok(s) => s,
        Err(_) => return Err(ConnectErr::Other),
    };
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);

    let hello = encode_cmd(&Cmd::Hello {
        proto: PROTO_VERSION,
        takeover,
    });
    if write.write_all(hello.as_bytes()).await.is_err() || write.flush().await.is_err() {
        return Err(ConnectErr::Other);
    }

    let mut line = String::new();
    match reader.read_line(&mut line).await {
        Ok(0) | Err(_) => return Err(ConnectErr::Other),
        Ok(_) => {}
    }

    match decode_event(&line) {
        Ok(Event::Hello {
            version,
            core,
            rpc_port,
            ..
        }) => {
            let _ = event_tx.send(SupervisorEvent::Connected {
                version,
                core,
                rpc_port,
            });
            Ok(Session { reader, write })
        }
        Ok(Event::Busy { owner }) => {
            let _ = event_tx.send(SupervisorEvent::Busy { owner });
            Err(ConnectErr::Busy)
        }
        Ok(Event::Error { code, msg }) => {
            let _ = event_tx.send(SupervisorEvent::Error { code, msg });
            Err(ConnectErr::Other)
        }
        _ => Err(ConnectErr::Other),
    }
}

async fn run_session(
    session: Session,
    cmd_rx: &mut mpsc::Receiver<Request>,
    event_tx: &UnboundedSender<SupervisorEvent>,
    shutdown: &Notify,
) -> SessionEnd {
    let Session { mut reader, mut write } = session;
    let mut pending: Option<Request> = None;
    let mut line = String::new();

    loop {
        line.clear();
        tokio::select! {
            _ = shutdown.notified() => return SessionEnd::Shutdown,
            maybe_req = cmd_rx.recv() => match maybe_req {
                None => return SessionEnd::Shutdown,
                Some(req) => {
                    if pending.is_some() {
                        req.fail("another request is already in flight".to_string());
                    } else {
                        let cmd = req.cmd();
                        if let Err(e) = write.write_all(encode_cmd(&cmd).as_bytes()).await {
                            req.fail(format!("write failed: {e}"));
                            return SessionEnd::Disconnected;
                        }
                        if let Err(e) = write.flush().await {
                            req.fail(format!("flush failed: {e}"));
                            return SessionEnd::Disconnected;
                        }
                        pending = Some(req);
                    }
                }
            },
            res = reader.read_line(&mut line) => match res {
                Ok(0) => return SessionEnd::Disconnected,
                Ok(_) => {
                    if let Ok(ev) = decode_event(&line) {
                        dispatch_event(ev, &mut pending, event_tx);
                    }
                }
                Err(_) => return SessionEnd::Disconnected,
            },
        }
    }
}

/// Route one decoded event: pushes go to the event channel, replies fulfil the
/// in-flight request. `core_started`/`core_stopped` are both surfaced as events
/// (per §8) and, when they answer a pending command, complete it.
fn dispatch_event(
    ev: Event,
    pending: &mut Option<Request>,
    event_tx: &UnboundedSender<SupervisorEvent>,
) {
    match ev {
        Event::CoreExited { code, signal } => {
            let _ = event_tx.send(SupervisorEvent::CoreExited { code, signal });
        }
        Event::Kicked => {
            let _ = event_tx.send(SupervisorEvent::Kicked);
        }
        Event::Busy { owner } => {
            let _ = event_tx.send(SupervisorEvent::Busy { owner });
        }
        Event::CoreStarted { pid, rpc_port } => {
            let _ = event_tx.send(SupervisorEvent::CoreStarted { pid, rpc_port });
            match pending.take() {
                Some(Request::Start(tx)) => {
                    let _ = tx.send(Ok(StartInfo { pid, rpc_port }));
                }
                Some(other) => other.fail("unexpected core_started reply".to_string()),
                None => {}
            }
        }
        Event::CoreStopped { reason } => {
            let _ = event_tx.send(SupervisorEvent::CoreStopped {
                reason: reason.clone(),
            });
            match pending.take() {
                Some(Request::Stop(tx)) => {
                    let _ = tx.send(Ok(()));
                }
                Some(other) => other.fail("unexpected core_stopped reply".to_string()),
                None => {}
            }
        }
        Event::Status { core, pid, rpc_port } => match pending.take() {
            Some(Request::Status(tx)) => {
                let _ = tx.send(Ok(StatusInfo {
                    core,
                    pid,
                    rpc_port,
                }));
            }
            Some(other) => other.fail("unexpected status reply".to_string()),
            None => {}
        },
        Event::Error { code, msg } => match pending.take() {
            Some(req) => req.fail(msg),
            None => {
                let _ = event_tx.send(SupervisorEvent::Error { code, msg });
            }
        },
        // A mid-session hello or unknown event is not expected; ignore it.
        Event::Hello { .. } | Event::Unknown => {}
    }
}
