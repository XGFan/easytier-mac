//! Integration tests for the supervisor client against a fake supervisor that
//! speaks the DESIGN §4 JSON-lines protocol over a Unix socket. No root and no
//! real supervisor binary required.

use std::time::Duration;

use easytier_mac_gui_lib::proto::CoreState;
use easytier_mac_gui_lib::supervisor_client::{
    StartInfo, SupervisorClient, SupervisorConfig, SupervisorEvent,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc::{self, UnboundedReceiver};

const HELLO: &[u8] =
    b"{\"event\":\"hello\",\"proto\":1,\"version\":\"9.9\",\"core\":\"stopped\",\"rpc_port\":null}\n";

fn fast_config(sock: std::path::PathBuf) -> SupervisorConfig {
    SupervisorConfig {
        socket_path: sock,
        takeover: false,
        initial_backoff: Duration::from_millis(30),
        max_backoff: Duration::from_millis(120),
    }
}

async fn next_event(rx: &mut UnboundedReceiver<SupervisorEvent>) -> SupervisorEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for a supervisor event")
        .expect("event channel closed unexpectedly")
}

#[tokio::test]
async fn hello_start_status_stop_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("sup.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();

        reader.read_line(&mut line).await.unwrap();
        assert!(line.contains("\"cmd\":\"hello\""), "got: {line}");
        w.write_all(HELLO).await.unwrap();

        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap() == 0 {
                break;
            }
            if line.contains("\"cmd\":\"start\"") {
                w.write_all(b"{\"event\":\"core_started\",\"pid\":4321,\"rpc_port\":50777}\n")
                    .await
                    .unwrap();
            } else if line.contains("\"cmd\":\"status\"") {
                w.write_all(
                    b"{\"event\":\"status\",\"core\":\"running\",\"pid\":4321,\"rpc_port\":50777}\n",
                )
                .await
                .unwrap();
            } else if line.contains("\"cmd\":\"stop\"") {
                w.write_all(b"{\"event\":\"core_stopped\",\"reason\":\"requested\"}\n")
                    .await
                    .unwrap();
            }
        }
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = SupervisorClient::spawn(fast_config(sock), tx);

    match next_event(&mut rx).await {
        SupervisorEvent::Connected { version, core, .. } => {
            assert_eq!(version, "9.9");
            assert_eq!(core, CoreState::Stopped);
        }
        e => panic!("expected Connected, got {e:?}"),
    }

    let info = client.start().await.unwrap();
    assert_eq!(
        info,
        StartInfo {
            pid: 4321,
            rpc_port: 50777
        }
    );
    // start is also surfaced as an event (DESIGN §8).
    match next_event(&mut rx).await {
        SupervisorEvent::CoreStarted { pid, rpc_port } => {
            assert_eq!(pid, 4321);
            assert_eq!(rpc_port, 50777);
        }
        e => panic!("expected CoreStarted, got {e:?}"),
    }

    let status = client.status().await.unwrap();
    assert_eq!(status.core, CoreState::Running);
    assert_eq!(status.rpc_port, Some(50777));

    client.stop().await.unwrap();
    match next_event(&mut rx).await {
        SupervisorEvent::CoreStopped { reason } => assert_eq!(reason, "requested"),
        e => panic!("expected CoreStopped, got {e:?}"),
    }

    client.shutdown().await;
    let _ = server.await;
}

#[tokio::test]
async fn reconnects_with_backoff_after_disconnect() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("sup.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    // Accept twice: greet with hello, then drop the connection each time.
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            let (stream, _) = listener.accept().await.unwrap();
            let (r, mut w) = stream.into_split();
            let mut reader = BufReader::new(r);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            w.write_all(HELLO).await.unwrap();
            // Drop immediately to force the client to reconnect.
        }
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = SupervisorClient::spawn(fast_config(sock), tx);

    assert!(matches!(
        next_event(&mut rx).await,
        SupervisorEvent::Connected { .. }
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        SupervisorEvent::Disconnected
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        SupervisorEvent::Connected { .. }
    ));

    client.shutdown().await;
    let _ = server.await;
}

#[tokio::test]
async fn forwards_unsolicited_core_exited() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("sup.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        w.write_all(HELLO).await.unwrap();
        // Push an unsolicited crash notification.
        w.write_all(b"{\"event\":\"core_exited\",\"code\":null,\"signal\":9}\n")
            .await
            .unwrap();
        // Keep the connection open briefly so the push is delivered first.
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = SupervisorClient::spawn(fast_config(sock), tx);

    assert!(matches!(
        next_event(&mut rx).await,
        SupervisorEvent::Connected { .. }
    ));
    match next_event(&mut rx).await {
        SupervisorEvent::CoreExited { code, signal } => {
            assert_eq!(code, None);
            assert_eq!(signal, Some(9));
        }
        e => panic!("expected CoreExited, got {e:?}"),
    }

    client.shutdown().await;
    let _ = server.await;
}

#[tokio::test]
async fn busy_pauses_reconnect_until_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("sup.sock");
    let listener = UnixListener::bind(&sock).unwrap();

    let server = tokio::spawn(async move {
        // First connection: reject as busy. The initial hello must NOT take over.
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.contains("\"takeover\":false"),
            "initial hello must not take over: {line}"
        );
        w.write_all(b"{\"event\":\"busy\",\"owner\":true}\n")
            .await
            .unwrap();
        drop((reader, w));

        // A second connection must only happen after an explicit takeover, and
        // its hello must request takeover=true.
        let (stream, _) = listener.accept().await.unwrap();
        let (r, mut w) = stream.into_split();
        let mut reader = BufReader::new(r);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.contains("\"takeover\":true"),
            "takeover hello must set takeover=true: {line}"
        );
        w.write_all(HELLO).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
    });

    let (tx, mut rx) = mpsc::unbounded_channel();
    let client = SupervisorClient::spawn(fast_config(sock), tx);

    match next_event(&mut rx).await {
        SupervisorEvent::Busy { owner } => assert!(owner),
        e => panic!("expected Busy, got {e:?}"),
    }

    // While busy, the driver must stay paused (no busy/reconnect storm): no
    // further event arrives until we explicitly request a takeover.
    let quiet = tokio::time::timeout(Duration::from_millis(300), rx.recv()).await;
    assert!(quiet.is_err(), "driver reconnected while busy: {quiet:?}");

    client.request_takeover();
    assert!(matches!(
        next_event(&mut rx).await,
        SupervisorEvent::Connected { .. }
    ));

    client.shutdown().await;
    let _ = server.await;
}
