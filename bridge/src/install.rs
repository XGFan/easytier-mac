//! Install-guide helpers (DESIGN §8): detect whether the privileged supervisor
//! is installed, and drive `scripts/install.sh` / `uninstall.sh` through a single
//! `osascript` admin prompt (mirroring `scripts/gui-install-example.sh`).
//!
//! The install/uninstall commands are wired but intentionally not executed by
//! the automated build; end-to-end privileged install is a manual acceptance
//! step.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

/// launchd plist path (DESIGN §1).
pub const PLIST_PATH: &str = "/Library/LaunchDaemons/com.easytier.supervisor.plist";
/// Installed supervisor binary (DESIGN §1).
pub const SUPERVISOR_DST: &str = "/Library/Application Support/EasyTier/bin/easytier-supervisor";
/// Installed core binary (DESIGN §1).
pub const CORE_DST: &str = "/Library/Application Support/EasyTier/bin/easytier-core";

#[derive(Debug, Clone, Serialize)]
pub struct InstallationStatus {
    pub plist_exists: bool,
    pub supervisor_bin_exists: bool,
    pub core_bin_exists: bool,
    /// True only when all three managed artifacts are present.
    pub installed: bool,
}

pub fn installation_status() -> InstallationStatus {
    let plist_exists = Path::new(PLIST_PATH).exists();
    let supervisor_bin_exists = Path::new(SUPERVISOR_DST).exists();
    let core_bin_exists = Path::new(CORE_DST).exists();
    InstallationStatus {
        plist_exists,
        supervisor_bin_exists,
        core_bin_exists,
        installed: plist_exists && supervisor_bin_exists && core_bin_exists,
    }
}

/// Repo root, resolved from the crate manifest dir at compile time. Works for
/// dev runs (`cargo`/`target`); a bundled app would ship the scripts as
/// resources (M2). This crate lives at `easytier-mac/bridge`, so the repo root is
/// two levels up (the Tauri GUI at `easytier-mac/gui/src-tauri` was three).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

fn scripts_dir() -> PathBuf {
    repo_root().join("easytier-mac").join("scripts")
}

fn dev_bin(name: &str) -> PathBuf {
    repo_root().join("target").join("debug").join(name)
}

/// Run `install.sh` with an admin prompt. Paths default to the workspace dev
/// binaries; callers may override.
pub fn run_install(
    supervisor_bin: Option<String>,
    core_bin: Option<String>,
) -> Result<(), String> {
    let supervisor_bin = supervisor_bin
        .map(PathBuf::from)
        .unwrap_or_else(|| dev_bin("easytier-supervisor"));
    let core_bin = core_bin
        .map(PathBuf::from)
        .unwrap_or_else(|| dev_bin("easytier-core"));
    let script = scripts_dir().join("install.sh");

    if !script.exists() {
        return Err(format!("install script not found: {}", script.display()));
    }
    if !supervisor_bin.exists() {
        return Err(format!(
            "supervisor binary not found: {} (build it first)",
            supervisor_bin.display()
        ));
    }
    if !core_bin.exists() {
        return Err(format!(
            "core binary not found: {} (build it first)",
            core_bin.display()
        ));
    }

    let uid = current_uid();
    let raw = format!(
        "{} --supervisor-bin {} --core-bin {} --owner-uid {}",
        shell_quote(&script.to_string_lossy()),
        shell_quote(&supervisor_bin.to_string_lossy()),
        shell_quote(&core_bin.to_string_lossy()),
        uid
    );
    run_osascript_admin(&raw)
}

/// Run `uninstall.sh` with an admin prompt.
pub fn run_uninstall() -> Result<(), String> {
    let script = scripts_dir().join("uninstall.sh");
    if !script.exists() {
        return Err(format!("uninstall script not found: {}", script.display()));
    }
    let raw = shell_quote(&script.to_string_lossy());
    run_osascript_admin(&raw)
}

fn current_uid() -> u32 {
    // Safe: getuid never fails and has no memory effects.
    unsafe { libc::getuid() }
}

/// POSIX single-quote a string so it survives `/bin/sh` word splitting inside
/// `do shell script`.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Escape a command for embedding inside an AppleScript double-quoted string
/// literal (backslash first, then double quote — order matters).
fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Execute `osascript -e 'do shell script "<cmd>" with administrator privileges'`.
/// The AppleScript is passed as a single argv, so no extra shell layer re-parses
/// it (see `scripts/gui-install-example.sh`).
fn run_osascript_admin(raw_cmd: &str) -> Result<(), String> {
    let apple_script = format!(
        "do shell script \"{}\" with administrator privileges",
        applescript_escape(raw_cmd)
    );
    let output = Command::new("osascript")
        .arg("-e")
        .arg(&apple_script)
        .output()
        .map_err(|e| format!("failed to launch osascript: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!("privileged command failed: {}", stderr.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_wraps_and_escapes_single_quotes() {
        assert_eq!(shell_quote("/a/b c"), "'/a/b c'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn applescript_escape_order() {
        // Backslash escaped before quote so an escaped quote's backslash is not doubled.
        assert_eq!(applescript_escape(r#"a\b"c"#), "a\\\\b\\\"c");
    }
}
