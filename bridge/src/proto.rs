//! Client-side view of the supervisor control protocol v1
//! (JSON Lines, UTF-8, one object per line).
//!
//! Authoritative contract: `easytier-mac/DESIGN.md` §4. The supervisor crate owns
//! the server-side `proto.rs`; this module is an independent client-side mirror so
//! the GUI does not need to depend on the supervisor binary crate. The GUI sends
//! `Cmd` objects and receives `Event` objects.

use serde::{Deserialize, Serialize};

/// Protocol version spoken by this client (DESIGN §4).
pub const PROTO_VERSION: u32 = 1;

/// Coarse core lifecycle state as reported on the wire (`"stopped"`/`"running"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreState {
    Stopped,
    Running,
}

/// Client -> supervisor requests. Internally tagged by `cmd`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Cmd {
    Hello { proto: u32, takeover: bool },
    Start,
    Status,
    Stop,
}

/// Supervisor -> client replies and pushes. Internally tagged by `event`.
///
/// An `Unknown` catch-all keeps the client forward-compatible with supervisor
/// versions that add new event kinds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Hello {
        proto: u32,
        #[serde(default)]
        version: String,
        core: CoreState,
        #[serde(default)]
        rpc_port: Option<u16>,
    },
    CoreStarted {
        pid: i32,
        rpc_port: u16,
    },
    Status {
        core: CoreState,
        #[serde(default)]
        pid: Option<i32>,
        #[serde(default)]
        rpc_port: Option<u16>,
    },
    CoreStopped {
        #[serde(default)]
        reason: String,
    },
    CoreExited {
        #[serde(default)]
        code: Option<i32>,
        #[serde(default)]
        signal: Option<i32>,
    },
    Error {
        #[serde(default)]
        code: String,
        #[serde(default)]
        msg: String,
    },
    Busy {
        #[serde(default)]
        owner: bool,
    },
    Kicked,
    #[serde(other)]
    Unknown,
}

/// Encode a command as a single `\n`-terminated JSON line.
pub fn encode_cmd(cmd: &Cmd) -> String {
    let mut s = serde_json::to_string(cmd).expect("cmd serialization is infallible");
    s.push('\n');
    s
}

/// Parse one JSON Lines event. Whitespace around the object is tolerated.
pub fn decode_event(line: &str) -> Result<Event, serde_json::Error> {
    serde_json::from_str(line.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_hello_cmd_shape() {
        let s = encode_cmd(&Cmd::Hello {
            proto: 1,
            takeover: false,
        });
        assert_eq!(s, "{\"cmd\":\"hello\",\"proto\":1,\"takeover\":false}\n");
    }

    #[test]
    fn encode_unit_cmds() {
        assert_eq!(encode_cmd(&Cmd::Start), "{\"cmd\":\"start\"}\n");
        assert_eq!(encode_cmd(&Cmd::Status), "{\"cmd\":\"status\"}\n");
        assert_eq!(encode_cmd(&Cmd::Stop), "{\"cmd\":\"stop\"}\n");
    }

    #[test]
    fn decode_hello_event() {
        let ev = decode_event(
            r#"{"event":"hello","proto":1,"version":"0.1.0","core":"stopped","rpc_port":null}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            Event::Hello {
                proto: 1,
                version: "0.1.0".into(),
                core: CoreState::Stopped,
                rpc_port: None
            }
        );
    }

    #[test]
    fn decode_core_started_and_status() {
        assert_eq!(
            decode_event(r#"{"event":"core_started","pid":123,"rpc_port":50321}"#).unwrap(),
            Event::CoreStarted {
                pid: 123,
                rpc_port: 50321
            }
        );
        assert_eq!(
            decode_event(r#"{"event":"status","core":"running","pid":123,"rpc_port":50321}"#)
                .unwrap(),
            Event::Status {
                core: CoreState::Running,
                pid: Some(123),
                rpc_port: Some(50321)
            }
        );
    }

    #[test]
    fn decode_pushes() {
        assert_eq!(
            decode_event(r#"{"event":"core_exited","code":null,"signal":9}"#).unwrap(),
            Event::CoreExited {
                code: None,
                signal: Some(9)
            }
        );
        assert_eq!(
            decode_event(r#"{"event":"busy","owner":true}"#).unwrap(),
            Event::Busy { owner: true }
        );
        assert_eq!(
            decode_event(r#"{"event":"kicked"}"#).unwrap(),
            Event::Kicked
        );
    }

    #[test]
    fn decode_tolerates_whitespace_and_unknown_events() {
        assert_eq!(
            decode_event("  {\"event\":\"kicked\"}  \n").unwrap(),
            Event::Kicked
        );
        // Forward-compatible: an unrecognised event decodes to Unknown, not an error.
        assert_eq!(
            decode_event(r#"{"event":"future_thing","x":1}"#).unwrap(),
            Event::Unknown
        );
    }

    #[test]
    fn decode_error_event() {
        assert_eq!(
            decode_event(r#"{"event":"error","code":"not_owner","msg":"nope"}"#).unwrap(),
            Event::Error {
                code: "not_owner".into(),
                msg: "nope".into()
            }
        );
    }
}
