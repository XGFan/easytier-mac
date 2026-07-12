//! Conflict detection (DESIGN §8): warn about unmanaged `easytier-core`
//! processes (distinct from the supervisor-managed binary path) and about a
//! coexisting TUN VPN (e.g. mihomo/clash) that may own the default route.

use std::process::Command;

use serde::Serialize;

/// The supervisor-managed core path (DESIGN §1); anything else is "unmanaged".
const MANAGED_CORE: &str = "/Library/Application Support/EasyTier/bin/easytier-core";

#[derive(Debug, Clone, Default, Serialize)]
pub struct Conflicts {
    /// An `easytier-core` is running from a path other than the managed one.
    pub unmanaged_core: bool,
    pub unmanaged_core_cmds: Vec<String>,
    /// A known TUN VPN process is running (may own the default route).
    pub tun_vpn: bool,
    pub tun_vpn_cmds: Vec<String>,
}

pub fn detect() -> Conflicts {
    match process_lines() {
        Some(lines) => classify(&lines),
        None => Conflicts::default(),
    }
}

/// Enumerate processes as `<pid> <args...>` lines.
fn process_lines() -> Option<Vec<String>> {
    let output = Command::new("ps")
        .args(["-axww", "-o", "pid=,args="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(|l| l.to_string())
            .collect(),
    )
}

fn classify(lines: &[String]) -> Conflicts {
    let mut c = Conflicts::default();
    for line in lines {
        let trimmed = line.trim_start();
        // Split pid from args.
        let Some((_pid, args)) = trimmed.split_once(char::is_whitespace) else {
            continue;
        };
        let args = args.trim();
        let argv0 = args.split_whitespace().next().unwrap_or("");

        if is_core_argv0(argv0) && !is_managed_core(args) {
            c.unmanaged_core = true;
            c.unmanaged_core_cmds.push(args.to_string());
        }

        if is_tun_vpn(argv0) {
            c.tun_vpn = true;
            c.tun_vpn_cmds.push(args.to_string());
        }
    }
    c
}

/// argv[0] names the core binary (managed path has spaces, so its argv[0] token
/// is not `.../easytier-core`; only genuinely unmanaged invocations match here).
fn is_core_argv0(argv0: &str) -> bool {
    argv0 == "easytier-core" || argv0.ends_with("/easytier-core")
}

fn is_managed_core(args: &str) -> bool {
    args == MANAGED_CORE || args.starts_with(&format!("{MANAGED_CORE} "))
}

fn is_tun_vpn(argv0: &str) -> bool {
    let base = argv0.rsplit('/').next().unwrap_or(argv0);
    matches!(
        base,
        "mihomo" | "clash" | "clash-meta" | "clashx" | "ClashX" | "sing-box" | "sing_box"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_unmanaged_core_but_not_managed() {
        let lines = vec![
            "  501 /Users/me/.bin/easytier-core --daemon".to_string(),
            "    1 /Library/Application Support/EasyTier/bin/easytier-core --daemon --rpc-portal 127.0.0.1:1".to_string(),
        ];
        let c = classify(&lines);
        assert!(c.unmanaged_core);
        assert_eq!(c.unmanaged_core_cmds.len(), 1);
        assert!(c.unmanaged_core_cmds[0].contains("/Users/me/.bin/easytier-core"));
    }

    #[test]
    fn managed_core_alone_is_not_a_conflict() {
        let lines = vec![
            "    1 /Library/Application Support/EasyTier/bin/easytier-core --daemon".to_string(),
        ];
        let c = classify(&lines);
        assert!(!c.unmanaged_core);
    }

    #[test]
    fn detects_tun_vpn() {
        let lines = vec!["  777 /opt/homebrew/bin/mihomo -d /etc/mihomo".to_string()];
        let c = classify(&lines);
        assert!(c.tun_vpn);
        assert_eq!(c.tun_vpn_cmds.len(), 1);
    }

    #[test]
    fn ignores_unrelated_processes() {
        let lines = vec!["  123 /usr/sbin/mDNSResponder".to_string()];
        let c = classify(&lines);
        assert!(!c.unmanaged_core);
        assert!(!c.tun_vpn);
    }
}
