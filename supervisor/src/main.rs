//! easytier-supervisor: privileged on-demand supervisor for the EasyTier macOS
//! GUI. Contract: easytier-mac/DESIGN.md (M0).
//!
//! Zero-residency model: launchd activates us on the first socket connection;
//! we serve exactly one owner (the GUI), spawn/reap a single easytier-core, and
//! exit(0) as soon as the owner disconnects.

mod activation;
mod auth;
mod config;
mod core_proc;
mod hooks;
mod janitor;
mod proto;
mod server;

use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use auth::AuthMode;
use config::{Config, DEFAULT_CONFIG_PATH};
use server::Shared;

/// Seconds we wait for an authenticated owner after activation before
/// self-exiting (DESIGN §4).
const NO_OWNER_TIMEOUT_SECS: u64 = 30;

struct Args {
    config_path: PathBuf,
    dev_listen: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut config_path = PathBuf::from(DEFAULT_CONFIG_PATH);
    let mut dev_listen = None;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                config_path = it
                    .next()
                    .ok_or_else(|| "--config requires a path".to_string())?
                    .into();
            }
            "--dev-listen" => {
                dev_listen = Some(
                    it.next()
                        .ok_or_else(|| "--dev-listen requires a path".to_string())?
                        .into(),
                );
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }
    Ok(Args {
        config_path,
        dev_listen,
    })
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("easytier-supervisor: {e}");
            exit(2);
        }
    };

    let config = match Config::load(&args.config_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "easytier-supervisor: failed to load config {}: {e}",
                args.config_path.display()
            );
            exit(1);
        }
    };

    let auth = match &args.dev_listen {
        // SAFETY: geteuid() is always successful.
        Some(_) => AuthMode::Dev {
            euid: unsafe { libc::geteuid() },
        },
        None => AuthMode::Launchd {
            owner_uid: config.owner_uid,
        },
    };

    // Activation-time orphan reconciliation (DESIGN §5): kill any leftover core
    // from a crashed previous supervisor before we start serving. exclude=None
    // is safe here — this is a fresh process that has not spawned a core yet, so
    // nothing of ours is running. A kill here also fires the `down` (janitor)
    // hook: the backstop for a predecessor that crashed without running `down`
    // (e.g. leaving DNS overridden). `auth` selects the hook security mode.
    janitor::sweep_orphans(&config, auth, None);

    let listeners = match obtain_listeners(&args) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("easytier-supervisor: no control socket: {e}");
            exit(1);
        }
    };

    let shared = Arc::new(Shared::new(config, auth));

    start_no_owner_timer(shared.clone());

    // One accept loop per listener (launchd normally hands us a single socket).
    let mut handles = Vec::new();
    for listener in listeners {
        let sh = shared.clone();
        handles.push(thread::spawn(move || accept_loop(sh, listener)));
    }
    for h in handles {
        let _ = h.join();
    }
}

fn obtain_listeners(args: &Args) -> std::io::Result<Vec<UnixListener>> {
    if let Some(path) = &args.dev_listen {
        // Best-effort unlink of a stale socket before bind (DESIGN §7).
        let _ = std::fs::remove_file(path);
        Ok(vec![UnixListener::bind(path)?])
    } else {
        activation::activate_listeners("Listeners")
    }
}

/// Exit if no owner has connected within the timeout (DESIGN §4). Harmless once
/// an owner exists: an owner disconnect already exits the process directly.
fn start_no_owner_timer(shared: Arc<Shared>) {
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(NO_OWNER_TIMEOUT_SECS));
        if !shared.has_owner() {
            eprintln!("easytier-supervisor: no owner within {NO_OWNER_TIMEOUT_SECS}s, exiting");
            exit(0);
        }
    });
}

fn accept_loop(shared: Arc<Shared>, listener: UnixListener) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let sh = shared.clone();
                thread::spawn(move || server::handle_connection(sh, stream));
            }
            Err(e) => {
                eprintln!("easytier-supervisor: accept failed: {e}");
                break;
            }
        }
    }
}
