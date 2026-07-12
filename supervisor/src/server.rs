//! Connection handling, the single-owner lease, and the shared supervisor
//! state (DESIGN §4).

use std::io::{self, BufRead, BufReader, Read};
use std::net::Shutdown;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use crate::auth::{self, AuthMode};
use crate::config::Config;
use crate::core_proc::{self, CoreProc};
use crate::proto::{self, Cmd, CoreState, Event, PROTO_VERSION};

/// Hard cap on a single control line. Legit commands are ~100 bytes; this bounds
/// memory against a peer that never sends a newline.
const MAX_LINE: usize = 64 * 1024;

/// A connection that has not completed `hello` gets this long to do so, then the
/// read fails and we close it (bounds slow-loris / idle pre-auth sockets).
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on the courtesy `kicked` write to a displaced owner, so a full
/// old-owner socket cannot block the new owner's takeover.
const KICK_WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Everything shared between the accept loop, owner thread, and core monitor.
pub struct Shared {
    pub config: Config,
    pub auth: AuthMode,
    pub st: Mutex<State>,
    pub cv: Condvar,
    next_id: AtomicU64,
}

pub struct State {
    pub owner: Option<OwnerSlot>,
    pub core: CoreProc,
}

/// The current owner connection.
///
/// - `writer` is the shared write half used for all events to this owner.
/// - `shutdown` is an independent dup of the socket used to unblock the owner's
///   reader on takeover WITHOUT taking `writer`'s mutex (so a stuck writer can
///   never wedge a takeover).
/// - `kicked` tells the owner thread it was replaced (not genuinely
///   disconnected), so it must not tear the core down.
pub struct OwnerSlot {
    pub id: u64,
    pub writer: Arc<Mutex<UnixStream>>,
    pub shutdown: UnixStream,
    pub kicked: Arc<AtomicBool>,
}

impl Shared {
    pub fn new(config: Config, auth: AuthMode) -> Shared {
        Shared {
            config,
            auth,
            st: Mutex::new(State {
                owner: None,
                core: CoreProc::Stopped,
            }),
            cv: Condvar::new(),
            next_id: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Snapshot the core state for `hello`/`status` replies.
    pub fn core_snapshot(&self) -> (CoreState, Option<i32>, Option<u16>) {
        match &self.st.lock().unwrap().core {
            CoreProc::Stopped => (CoreState::Stopped, None, None),
            CoreProc::Running(r) => (CoreState::Running, Some(r.pid), Some(r.rpc_port)),
        }
    }

    /// Push an async event to whoever currently owns the connection (used by
    /// the core monitor for `core_exited`). No-op if there is no owner.
    pub fn write_to_owner(&self, ev: &Event) {
        let writer = self.st.lock().unwrap().owner.as_ref().map(|o| o.writer.clone());
        if let Some(w) = writer {
            if let Ok(mut s) = w.lock() {
                let _ = proto::write_event(&mut *s, ev);
            }
        }
    }

    /// True while there is no authenticated owner; used by the 30s no-owner
    /// self-exit timer (DESIGN §4).
    pub fn has_owner(&self) -> bool {
        self.st.lock().unwrap().owner.is_some()
    }
}

/// Lease outcome for a `hello` given whether an owner already exists.
#[derive(Debug, PartialEq, Eq)]
pub enum Lease {
    Accept,
    Busy,
    Takeover,
}

pub fn lease_decision(has_owner: bool, takeover: bool) -> Lease {
    match (has_owner, takeover) {
        (false, _) => Lease::Accept,
        (true, true) => Lease::Takeover,
        (true, false) => Lease::Busy,
    }
}

fn write_locked(writer: &Arc<Mutex<UnixStream>>, ev: &Event) -> io::Result<()> {
    let mut s = writer
        .lock()
        .map_err(|_| io::Error::new(io::ErrorKind::Other, "writer mutex poisoned"))?;
    proto::write_event(&mut *s, ev)
}

/// Read one JSON line capped at `max` bytes. Returns `Ok(None)` on clean EOF,
/// `Ok(Some(line))` (newline included) otherwise, and `Err` on I/O error,
/// over-limit (no newline within `max`), or invalid UTF-8 — the caller closes
/// the connection on `Err`.
fn read_line_limited<R: BufRead>(reader: &mut R, max: usize) -> io::Result<Option<String>> {
    let mut buf = Vec::new();
    let n = reader.by_ref().take(max as u64).read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&b'\n') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "control line exceeds limit or missing newline",
        ));
    }
    String::from_utf8(buf)
        .map(Some)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "control line is not valid UTF-8"))
}

/// Handle one accepted connection start-to-finish (runs on its own thread).
pub fn handle_connection(shared: Arc<Shared>, stream: UnixStream) {
    // 1. Peer credential gate (DESIGN §6).
    let peer_uid = match auth::peer_uid(stream.as_raw_fd()) {
        Ok(u) => u,
        Err(e) => {
            eprintln!("auth: could not read peer uid: {e}");
            return;
        }
    };
    if !shared.auth.allows(peer_uid) {
        eprintln!("auth: rejected connection from uid={peer_uid}");
        return;
    }

    let reader_stream = match stream.try_clone() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("conn: try_clone(reader) failed: {e}");
            return;
        }
    };
    // Bound the pre-hello phase; cleared once the owner is established.
    let _ = reader_stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let writer = match stream.try_clone() {
        Ok(s) => Arc::new(Mutex::new(s)),
        Err(e) => {
            eprintln!("conn: try_clone(writer) failed: {e}");
            return;
        }
    };
    let mut reader = BufReader::new(reader_stream);

    // 2. First line must be a valid hello.
    let takeover = match read_hello(&mut reader, &writer) {
        Some(t) => t,
        None => return,
    };

    // 3. Acquire the single-owner lease. Any I/O to a displaced owner is deferred
    //    until AFTER the lock is released (see below).
    let id = shared.next_id();
    let kicked = Arc::new(AtomicBool::new(false));
    let displaced: Option<OwnerSlot> = {
        let mut st = shared.st.lock().unwrap();
        match lease_decision(st.owner.is_some(), takeover) {
            Lease::Busy => {
                drop(st);
                let _ = write_locked(&writer, &Event::Busy { owner: true });
                return;
            }
            Lease::Takeover => {
                let old = st.owner.take();
                if let Some(o) = &old {
                    o.kicked.store(true, Ordering::SeqCst);
                }
                st.owner = Some(OwnerSlot {
                    id,
                    writer: writer.clone(),
                    shutdown: stream,
                    kicked: kicked.clone(),
                });
                old
            }
            Lease::Accept => {
                st.owner = Some(OwnerSlot {
                    id,
                    writer: writer.clone(),
                    shutdown: stream,
                    kicked: kicked.clone(),
                });
                None
            }
        }
    };

    // Notify + disconnect the displaced owner OUTSIDE the state lock: a stuck or
    // full old socket must never hold the state mutex and wedge the supervisor.
    // `kicked` was already set under the lock; this is best-effort.
    if let Some(old) = displaced {
        if let Ok(mut s) = old.writer.try_lock() {
            // Bound the courtesy write: a full old-owner socket must not stall
            // this (the new owner's) thread and its pending hello reply.
            let _ = s.set_write_timeout(Some(KICK_WRITE_TIMEOUT));
            let _ = proto::write_event(&mut *s, &Event::Kicked);
        }
        // Unblocks the old owner's reader regardless of writer-lock state.
        let _ = old.shutdown.shutdown(Shutdown::Both);
    }

    // Established: drop the pre-hello timeout so the command loop blocks for
    // owner commands indefinitely.
    let _ = reader.get_ref().set_read_timeout(None);

    // 4. hello reply reflects current core state.
    let (core, _pid, rpc_port) = shared.core_snapshot();
    let _ = write_locked(
        &writer,
        &Event::Hello {
            proto: PROTO_VERSION,
            version: env!("CARGO_PKG_VERSION").to_string(),
            core,
            rpc_port,
        },
    );

    // 5. Command loop.
    run_command_loop(&shared, &mut reader, &writer);

    // 6. Connection ended.
    if kicked.load(Ordering::SeqCst) {
        // We were taken over; the new owner keeps the core running.
        return;
    }

    // Genuine owner disconnect == stop + supervisor exit (DESIGN §4).
    {
        let mut st = shared.st.lock().unwrap();
        if st.owner.as_ref().map(|o| o.id) == Some(id) {
            st.owner = None;
        }
    }
    core_proc::stop(&shared);
    // Sweeping with exclude=None is safe here: our core (if any) was just reaped
    // by stop(), no owner remains, and this single supervisor is about to exit.
    crate::janitor::sweep_orphans(&shared.config, None);
    crate::janitor::cleanup_routes(&shared.config);
    std::process::exit(0);
}

/// Read and validate the mandatory first `hello`. Returns `Some(takeover)` on
/// success, or `None` (after sending an error / closing) otherwise.
fn read_hello<R: BufRead>(reader: &mut R, writer: &Arc<Mutex<UnixStream>>) -> Option<bool> {
    let line = match read_line_limited(reader, MAX_LINE) {
        Ok(Some(l)) => l,
        Ok(None) => return None, // closed before saying hello
        Err(e) => {
            // Includes the 10s hello timeout, over-limit, and invalid UTF-8.
            eprintln!("conn: read hello failed: {e}");
            return None;
        }
    };
    match proto::decode_cmd(&line) {
        Ok(Cmd::Hello { proto, takeover }) => {
            if proto != PROTO_VERSION {
                let _ = write_locked(
                    writer,
                    &Event::error("bad_proto", format!("unsupported proto {proto}")),
                );
                return None;
            }
            Some(takeover)
        }
        Ok(_) => {
            let _ = write_locked(writer, &Event::error("bad_proto", "expected hello first"));
            None
        }
        Err(e) => {
            let _ = write_locked(writer, &Event::error("bad_proto", format!("malformed hello: {e}")));
            None
        }
    }
}

fn run_command_loop<R: BufRead>(
    shared: &Arc<Shared>,
    reader: &mut R,
    writer: &Arc<Mutex<UnixStream>>,
) {
    loop {
        let line = match read_line_limited(reader, MAX_LINE) {
            Ok(Some(l)) => l,
            Ok(None) => break, // disconnect
            Err(e) => {
                eprintln!("conn: read command failed: {e}");
                break; // over-limit / invalid UTF-8 -> close
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        match proto::decode_cmd(&line) {
            Ok(Cmd::Hello { .. }) => {
                let _ = write_locked(writer, &Event::error("bad_proto", "duplicate hello"));
            }
            Ok(Cmd::Start) => match core_proc::start(shared) {
                Ok((pid, rpc_port)) => {
                    let _ = write_locked(writer, &Event::CoreStarted { pid, rpc_port });
                }
                Err(msg) => {
                    let _ = write_locked(writer, &Event::error("spawn_failed", msg));
                }
            },
            Ok(Cmd::Status) => {
                let (core, pid, rpc_port) = shared.core_snapshot();
                let _ = write_locked(writer, &Event::Status { core, pid, rpc_port });
            }
            Ok(Cmd::Stop) => {
                let was_running = core_proc::stop(shared);
                let reason = if was_running {
                    "requested"
                } else {
                    "already_stopped"
                };
                let _ = write_locked(
                    writer,
                    &Event::CoreStopped {
                        reason: reason.into(),
                    },
                );
            }
            Err(e) => {
                let _ = write_locked(writer, &Event::error("bad_request", format!("{e}")));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_no_owner_accepts() {
        assert_eq!(lease_decision(false, false), Lease::Accept);
        assert_eq!(lease_decision(false, true), Lease::Accept);
    }

    #[test]
    fn lease_with_owner_busy_unless_takeover() {
        assert_eq!(lease_decision(true, false), Lease::Busy);
        assert_eq!(lease_decision(true, true), Lease::Takeover);
    }

    #[test]
    fn read_line_limited_reads_and_trims_to_newline() {
        let data = b"{\"cmd\":\"stop\"}\nleftover";
        let mut r = std::io::BufReader::new(&data[..]);
        let line = read_line_limited(&mut r, MAX_LINE).unwrap().unwrap();
        assert_eq!(line, "{\"cmd\":\"stop\"}\n");
    }

    #[test]
    fn read_line_limited_eof_is_none() {
        let data = b"";
        let mut r = std::io::BufReader::new(&data[..]);
        assert!(read_line_limited(&mut r, MAX_LINE).unwrap().is_none());
    }

    #[test]
    fn read_line_limited_rejects_overlong_line() {
        // No newline within the limit -> error (caller closes the connection).
        let data = vec![b'x'; 100];
        let mut r = std::io::BufReader::new(&data[..]);
        let err = read_line_limited(&mut r, 16).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }
}
