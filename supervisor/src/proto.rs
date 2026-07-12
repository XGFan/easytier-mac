//! Control protocol v1 (JSON Lines, UTF-8, one object per line).
//!
//! Contract: `easytier-mac/DESIGN.md` §4. Client sends `cmd` objects,
//! supervisor replies/pushes `event` objects. The first client message on a
//! connection must be `hello`; any other `cmd` before `hello` is rejected.

use std::io::{self, Write};

use serde::{Deserialize, Serialize};

/// Protocol version spoken by this supervisor (DESIGN §4).
pub const PROTO_VERSION: u32 = 1;

/// Coarse core lifecycle state as reported on the wire (`"stopped"`/`"running"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreState {
    Stopped,
    Running,
}

/// Client -> supervisor requests. Internally tagged by `cmd`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    Hello {
        proto: u32,
        #[serde(default)]
        takeover: bool,
    },
    Start,
    Status,
    Stop,
}

/// Supervisor -> client replies and pushes. Internally tagged by `event`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Hello {
        proto: u32,
        version: String,
        core: CoreState,
        rpc_port: Option<u16>,
    },
    CoreStarted {
        pid: i32,
        rpc_port: u16,
    },
    Status {
        core: CoreState,
        pid: Option<i32>,
        rpc_port: Option<u16>,
    },
    CoreStopped {
        reason: String,
    },
    /// Core exited on its own (crash/external kill). Supervisor cleans up but
    /// does NOT restart; the restart decision lives in the client (DESIGN §4).
    CoreExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Error {
        code: String,
        msg: String,
    },
    /// A second connection tried to take ownership while an owner already
    /// exists and did not request takeover.
    Busy {
        owner: bool,
    },
    /// This connection's ownership was taken over by a `hello.takeover=true`
    /// connection.
    Kicked,
}

impl Event {
    pub fn error(code: &str, msg: impl Into<String>) -> Event {
        Event::Error {
            code: code.to_string(),
            msg: msg.into(),
        }
    }
}

/// Parse one JSON Lines request. Whitespace around the object is tolerated.
pub fn decode_cmd(line: &str) -> Result<Cmd, serde_json::Error> {
    serde_json::from_str(line.trim())
}

/// Encode an event as a single `\n`-terminated JSON line.
pub fn encode_event(ev: &Event) -> String {
    let mut s = serde_json::to_string(ev).expect("event serialization is infallible");
    s.push('\n');
    s
}

/// Write one event to a stream as a JSON line and flush.
pub fn write_event<W: Write>(w: &mut W, ev: &Event) -> io::Result<()> {
    w.write_all(encode_event(ev).as_bytes())?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hello_with_takeover() {
        let c = decode_cmd(r#"{"cmd":"hello","proto":1,"takeover":true}"#).unwrap();
        assert_eq!(
            c,
            Cmd::Hello {
                proto: 1,
                takeover: true
            }
        );
    }

    #[test]
    fn decode_hello_defaults_takeover_false() {
        let c = decode_cmd(r#"{"cmd":"hello","proto":1}"#).unwrap();
        assert_eq!(
            c,
            Cmd::Hello {
                proto: 1,
                takeover: false
            }
        );
    }

    #[test]
    fn decode_unit_variants() {
        assert_eq!(decode_cmd(r#"{"cmd":"start"}"#).unwrap(), Cmd::Start);
        assert_eq!(decode_cmd(r#"{"cmd":"status"}"#).unwrap(), Cmd::Status);
        assert_eq!(decode_cmd(r#"{"cmd":"stop"}"#).unwrap(), Cmd::Stop);
    }

    #[test]
    fn decode_tolerates_surrounding_whitespace() {
        let c = decode_cmd("  {\"cmd\":\"stop\"}  \n").unwrap();
        assert_eq!(c, Cmd::Stop);
    }

    #[test]
    fn decode_rejects_unknown_cmd() {
        assert!(decode_cmd(r#"{"cmd":"frobnicate"}"#).is_err());
    }

    #[test]
    fn encode_hello_event_shape() {
        let ev = Event::Hello {
            proto: 1,
            version: "0.1.0".into(),
            core: CoreState::Stopped,
            rpc_port: None,
        };
        assert_eq!(
            encode_event(&ev),
            "{\"event\":\"hello\",\"proto\":1,\"version\":\"0.1.0\",\"core\":\"stopped\",\"rpc_port\":null}\n"
        );
    }

    #[test]
    fn encode_core_started_shape() {
        let ev = Event::CoreStarted {
            pid: 12345,
            rpc_port: 50321,
        };
        assert_eq!(
            encode_event(&ev),
            "{\"event\":\"core_started\",\"pid\":12345,\"rpc_port\":50321}\n"
        );
    }

    #[test]
    fn encode_status_running_shape() {
        let ev = Event::Status {
            core: CoreState::Running,
            pid: Some(12345),
            rpc_port: Some(50321),
        };
        assert_eq!(
            encode_event(&ev),
            "{\"event\":\"status\",\"core\":\"running\",\"pid\":12345,\"rpc_port\":50321}\n"
        );
    }

    #[test]
    fn encode_busy_and_kicked_shape() {
        assert_eq!(
            encode_event(&Event::Busy { owner: true }),
            "{\"event\":\"busy\",\"owner\":true}\n"
        );
        assert_eq!(
            encode_event(&Event::Kicked),
            "{\"event\":\"kicked\"}\n"
        );
    }

    #[test]
    fn encode_core_exited_shape() {
        assert_eq!(
            encode_event(&Event::CoreExited {
                code: None,
                signal: Some(9)
            }),
            "{\"event\":\"core_exited\",\"code\":null,\"signal\":9}\n"
        );
    }

    #[test]
    fn event_round_trips_through_a_line() {
        // Events are what a client parses; make sure each encodes to exactly one line.
        let ev = Event::CoreStopped {
            reason: "requested".into(),
        };
        let line = encode_event(&ev);
        assert!(line.ends_with('\n'));
        assert_eq!(line.matches('\n').count(), 1);
    }
}
