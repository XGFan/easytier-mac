//! supervisor.toml loading. Written by the installer; see DESIGN.md §1/§3.

use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default hook timeout when `hook_timeout_secs` is absent (see `hooks.rs`).
const DEFAULT_HOOK_TIMEOUT_SECS: u64 = 30;

/// Default config path (DESIGN §1), overridable with `--config`.
pub const DEFAULT_CONFIG_PATH: &str = "/Library/Application Support/EasyTier/supervisor.toml";

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Config {
    /// Protocol version the installer expects (DESIGN §3). Advisory only.
    #[allow(dead_code)]
    pub proto: u32,
    /// uid of the installing user; connection auth allows {0, owner_uid}.
    pub owner_uid: u32,
    /// Absolute path to the easytier-core binary we spawn and reap.
    pub core_path: String,
    /// Directory for core.out.log (DESIGN §5).
    pub log_dir: String,
    /// Directory holding lifecycle hook scripts (`up.sh`/`down.sh`). Absent =>
    /// `<install_root>/hooks`. See `hooks.rs`.
    #[serde(default)]
    pub hooks_dir: Option<String>,
    /// Per-hook wall-clock budget before SIGKILL, in seconds. Absent => 30.
    #[serde(default)]
    pub hook_timeout_secs: Option<u64>,
}

impl Config {
    pub fn load(path: &Path) -> io::Result<Config> {
        // Defense in depth: when running privileged, refuse a config that isn't
        // a root-owned, non-group/world-writable regular file (a foothold for a
        // future `--config` injection). Dev mode runs unprivileged and skips it.
        // SAFETY: geteuid never fails.
        if unsafe { libc::geteuid() } == 0 {
            let md = std::fs::symlink_metadata(path)?;
            check_secure_ownership(md.is_file(), md.uid(), md.mode())
                .map_err(|msg| io::Error::new(io::ErrorKind::PermissionDenied, msg))?;
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Install root == parent of the log directory (`<root>/logs`), used as the
    /// core process cwd (DESIGN §5). Falls back to `/` if the log dir has no
    /// parent.
    pub fn install_root(&self) -> PathBuf {
        Path::new(&self.log_dir)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// Directory holding hook scripts: the configured `hooks_dir`, else
    /// `<install_root>/hooks`.
    pub fn hooks_dir(&self) -> PathBuf {
        match &self.hooks_dir {
            Some(d) => PathBuf::from(d),
            None => self.install_root().join("hooks"),
        }
    }

    /// Wall-clock budget for one hook run before SIGKILL.
    pub fn hook_timeout(&self) -> Duration {
        Duration::from_secs(self.hook_timeout_secs.unwrap_or(DEFAULT_HOOK_TIMEOUT_SECS))
    }
}

/// Root-mode config safety predicate (pure, so it is unit-testable): the config
/// must be a regular file, owned by root, and not group/world writable.
fn check_secure_ownership(is_file: bool, uid: u32, mode: u32) -> Result<(), String> {
    if !is_file {
        return Err("config must be a regular file (symlink or special file rejected)".into());
    }
    if uid != 0 {
        return Err(format!("config must be owned by root, is uid {uid}"));
    }
    if mode & 0o022 != 0 {
        return Err("config must not be group/world writable".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_design_example() {
        let cfg: Config = toml::from_str(
            r#"
proto = 1
owner_uid = 501
core_path = "/Library/Application Support/EasyTier/bin/easytier-core"
log_dir = "/Library/Application Support/EasyTier/logs"
"#,
        )
        .unwrap();
        assert_eq!(cfg.proto, 1);
        assert_eq!(cfg.owner_uid, 501);
        assert_eq!(
            cfg.install_root(),
            PathBuf::from("/Library/Application Support/EasyTier")
        );
    }

    #[test]
    fn secure_ownership_predicate() {
        // Accept: regular file, root-owned, 0644.
        assert!(check_secure_ownership(true, 0, 0o644).is_ok());
        // Reject: not a regular file (e.g. symlink).
        assert!(check_secure_ownership(false, 0, 0o644).is_err());
        // Reject: not owned by root.
        assert!(check_secure_ownership(true, 501, 0o644).is_err());
        // Reject: group- or world-writable.
        assert!(check_secure_ownership(true, 0, 0o664).is_err());
        assert!(check_secure_ownership(true, 0, 0o646).is_err());
    }
}
