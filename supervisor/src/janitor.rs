//! Orphan reconciliation (DESIGN §5).
//!
//! macOS has no `PR_SET_PDEATHSIG`: if a previous supervisor crashed it may
//! leave a root-owned `easytier-core` orphan behind. On activation (and after
//! an unexpected core exit) we scan for cores that we launched — matched by
//! their argv[0] being exactly `config.core_path` — and SIGKILL them.
//!
//! We deliberately match argv[0] and NOT the process name: this machine may run
//! the user's own `~/.bin/easytier-core`, which is none of our business.

use std::process::Command;

use crate::auth::AuthMode;
use crate::config::Config;
use crate::hooks::{self, HookEvent, HookReason};

/// Kill leftover cores whose argv[0] is exactly `config.core_path`.
///
/// `exclude_pid` (and our own pid) are never signalled. Returns the number of
/// processes killed. Logs each kill to stderr (-> supervisor.err.log).
///
/// If anything was killed we fire the `down` (janitor) hook — a backstop so a
/// crashed predecessor that never ran its own `down` still gets cleaned up (plan
/// §1). This never double-fires for a core that stop/exit already handled: those
/// paths reap their core before sweeping, so it is gone from `ps` and not
/// counted here — only a genuinely orphaned process makes `killed > 0`.
pub fn sweep_orphans(config: &Config, auth: AuthMode, exclude_pid: Option<i32>) -> usize {
    let self_pid = std::process::id() as i32;
    let mut killed = 0;

    // `-ww` disables the default args truncation; `pid=,args=` drops headers.
    let output = match Command::new("ps").args(["-axww", "-o", "pid=,args="]).output() {
        Ok(o) => o,
        Err(e) => {
            eprintln!("janitor: failed to run ps: {e}");
            return 0;
        }
    };
    let text = String::from_utf8_lossy(&output.stdout);

    for line in text.lines() {
        let line = line.trim_start();
        // `<pid> <full argv joined by spaces>`.
        let mut parts = line.splitn(2, char::is_whitespace);
        let pid: i32 = match parts.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let args = parts.next().unwrap_or("").trim_start();

        if pid == self_pid || Some(pid) == exclude_pid {
            continue;
        }
        if !argv0_matches(args, &config.core_path) {
            continue;
        }

        // SAFETY: kill(2) with a pid we parsed from ps; SIGKILL is unconditional.
        let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
        if rc == 0 {
            eprintln!("janitor: killed orphan core pid={pid} args={args:?}");
            killed += 1;
        } else {
            let err = std::io::Error::last_os_error();
            // ESRCH: it already exited between ps and kill; not an error for us.
            if err.raw_os_error() != Some(libc::ESRCH) {
                eprintln!("janitor: kill pid={pid} failed: {err}");
            }
        }
    }

    if killed > 0 {
        // Detached: activation continues serving; the post-death sweeps that pass
        // through here have already run their own `down`, so this only adds up for
        // truly orphaned processes.
        let _ = hooks::run_hook(config, auth, HookEvent::Down, Some(HookReason::Janitor));
    }
    killed
}

/// True when `args` (a full command line) has argv[0] exactly equal to
/// `core_path`.
///
/// We match a full-path prefix rather than whitespace-tokenizing because the
/// production `core_path` contains a space ("Application Support"), so the first
/// whitespace token is not argv[0]. Requiring the path to be followed by a
/// space (or be the whole line) avoids matching siblings like
/// `easytier-core-extra`.
fn argv0_matches(args: &str, core_path: &str) -> bool {
    args == core_path || args.starts_with(&format!("{core_path} "))
}

/// Route / utun residue cleanup after a core exit.
///
/// v0 is process-scoped only; route reconciliation is deferred until the
/// scripts/m0a signal-residue experiment tells us what actually leaks. Left as a
/// reserved seam (DESIGN §5).
pub fn cleanup_routes(_config: &Config) {
    // TODO(m0a): reconcile leftover routes / utun devices once measured.
}

#[cfg(test)]
mod tests {
    use super::argv0_matches;

    #[test]
    fn matches_exact_and_with_args() {
        let cp = "/Library/Application Support/EasyTier/bin/easytier-core";
        assert!(argv0_matches(cp, cp));
        assert!(argv0_matches(
            &format!("{cp} --daemon --rpc-portal 127.0.0.1:5000"),
            cp
        ));
    }

    #[test]
    fn does_not_match_sibling_or_substring() {
        let cp = "/tmp/et/easytier-core";
        assert!(!argv0_matches("/tmp/et/easytier-core-extra --daemon", cp));
        assert!(!argv0_matches("/usr/bin/easytier-core --daemon", cp));
        assert!(!argv0_matches("/bin/sh /tmp/et/easytier-core --daemon", cp));
    }
}
