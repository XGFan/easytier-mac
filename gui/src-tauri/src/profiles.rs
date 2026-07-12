//! Profile + app-state persistence (DESIGN §8).
//!
//! Layout under `~/Library/Application Support/EasyTier/`:
//!   - `profiles/<id>.toml` — one network profile each, in the `TomlConfigLoader`
//!     format the core consumes. `<id>` is the config's instance id, so it maps
//!     directly to the runtime instance the RPC layer reports.
//!   - `state.json` — the set of profiles that were running last, plus GUI
//!     settings (autostart, auto-restart).
//!
//! This module treats profile TOML as opaque text (a display name is derived
//! from a few well-known fields) so it stays decoupled from the core config
//! types and unit-testable against a tempdir.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Metadata for the network list (no full TOML body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMeta {
    pub id: String,
    pub name: String,
}

/// A full profile including its TOML body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileRecord {
    pub id: String,
    pub name: String,
    pub toml: String,
}

fn default_true() -> bool {
    true
}

/// Persisted GUI state (DESIGN §8: last-running set, autostart, settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    /// Profile ids that were running at last persist; restored on launch when
    /// the supervisor is reachable.
    #[serde(default)]
    pub running: Vec<String>,
    /// Login-item autostart toggle (mirrors the OS login item).
    #[serde(default)]
    pub autostart: bool,
    /// Auto-restart core after an unexpected `core_exited` (capped retries).
    #[serde(default = "default_true")]
    pub auto_restart: bool,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            running: Vec::new(),
            autostart: false,
            auto_restart: true,
        }
    }
}

/// File-backed profile + state store rooted at a directory.
pub struct ProfileStore {
    root: PathBuf,
    /// Serializes state.json read-modify-write cycles within this process.
    state_lock: std::sync::Mutex<()>,
}

impl ProfileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            state_lock: std::sync::Mutex::new(()),
        }
    }

    /// Default per-user root: `~/Library/Application Support/EasyTier`.
    pub fn default_root() -> PathBuf {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        home.join("Library")
            .join("Application Support")
            .join("EasyTier")
    }

    pub fn profiles_dir(&self) -> PathBuf {
        self.root.join("profiles")
    }

    pub fn state_path(&self) -> PathBuf {
        self.root.join("state.json")
    }

    fn profile_path(&self, id: &str) -> Result<PathBuf, String> {
        validate_id(id)?;
        Ok(self.profiles_dir().join(format!("{id}.toml")))
    }

    fn ensure_dirs(&self) -> Result<(), String> {
        std::fs::create_dir_all(self.profiles_dir()).map_err(|e| e.to_string())
    }

    /// List all profiles, deriving each display name from its TOML.
    pub fn list_profiles(&self) -> Result<Vec<ProfileMeta>, String> {
        let dir = self.profiles_dir();
        let mut out = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e.to_string()),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let toml_text = std::fs::read_to_string(&path).unwrap_or_default();
            out.push(ProfileMeta {
                id: id.to_string(),
                name: derive_name(id, &toml_text),
            });
        }
        out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(out)
    }

    pub fn read_profile(&self, id: &str) -> Result<ProfileRecord, String> {
        let path = self.profile_path(id)?;
        let toml_text = std::fs::read_to_string(&path)
            .map_err(|e| format!("read profile {id}: {e}"))?;
        Ok(ProfileRecord {
            id: id.to_string(),
            name: derive_name(id, &toml_text),
            toml: toml_text,
        })
    }

    pub fn profile_exists(&self, id: &str) -> bool {
        self.profile_path(id).map(|p| p.exists()).unwrap_or(false)
    }

    /// Write (create or overwrite) a profile's TOML body.
    pub fn write_profile(&self, id: &str, toml_text: &str) -> Result<ProfileMeta, String> {
        self.ensure_dirs()?;
        let path = self.profile_path(id)?;
        std::fs::write(&path, toml_text).map_err(|e| format!("write profile {id}: {e}"))?;
        Ok(ProfileMeta {
            id: id.to_string(),
            name: derive_name(id, toml_text),
        })
    }

    pub fn delete_profile(&self, id: &str) -> Result<(), String> {
        let path = self.profile_path(id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("delete profile {id}: {e}")),
        }
    }

    /// Load persisted state, returning defaults when absent or corrupt.
    pub fn load_state(&self) -> AppState {
        let path = self.state_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return AppState::default();
        };
        serde_json::from_str(&text).unwrap_or_default()
    }

    /// Persist state atomically: write a sibling temp file, then rename over the
    /// target so a crash mid-write cannot leave a truncated state.json.
    pub fn save_state(&self, state: &AppState) -> Result<(), String> {
        self.ensure_dirs()?;
        let text = serde_json::to_string_pretty(state).map_err(|e| e.to_string())?;
        let tmp_path = self.root.join("state.json.tmp");
        std::fs::write(&tmp_path, text).map_err(|e| format!("write state.json.tmp: {e}"))?;
        std::fs::rename(&tmp_path, self.state_path())
            .map_err(|e| format!("rename state.json: {e}"))
    }

    /// Atomic read-modify-write of the persisted state, serialized against other
    /// callers in this process by `state_lock`.
    pub fn update_state(&self, f: impl FnOnce(&mut AppState)) -> Result<(), String> {
        let _guard = self.state_lock.lock().unwrap();
        let mut state = self.load_state();
        f(&mut state);
        self.save_state(&state)
    }
}

/// Reject ids that could escape the profiles directory or collide with the
/// filesystem. Profile ids are config instance uuids in normal use.
fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains('\0')
    {
        return Err(format!("invalid profile id: {id:?}"));
    }
    Ok(())
}

/// Derive a human-readable profile name from its TOML, preferring
/// `instance_name`, then `[network_identity].network_name`, then `hostname`,
/// then falling back to the id.
fn derive_name(id: &str, toml_text: &str) -> String {
    if let Ok(value) = toml_text.parse::<toml::Value>() {
        for key in ["instance_name", "hostname"] {
            if let Some(s) = value.get(key).and_then(|v| v.as_str()) {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
        if let Some(s) = value
            .get("network_identity")
            .and_then(|n| n.get("network_name"))
            .and_then(|v| v.as_str())
        {
            if !s.trim().is_empty() {
                return s.to_string();
            }
        }
    }
    id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, ProfileStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(dir.path());
        (dir, store)
    }

    #[test]
    fn write_read_roundtrip_and_name_derivation() {
        let (_dir, store) = store();
        let toml = "instance_name = \"home-net\"\nhostname = \"mac\"\n";
        let meta = store.write_profile("id-1", toml).unwrap();
        assert_eq!(meta.name, "home-net");

        let rec = store.read_profile("id-1").unwrap();
        assert_eq!(rec.id, "id-1");
        assert_eq!(rec.name, "home-net");
        assert_eq!(rec.toml, toml);
    }

    #[test]
    fn name_falls_back_through_network_name_then_hostname_then_id() {
        let (_dir, store) = store();
        store
            .write_profile("a", "hostname = \"boxA\"\n")
            .unwrap();
        assert_eq!(store.read_profile("a").unwrap().name, "boxA");

        store
            .write_profile(
                "b",
                "[network_identity]\nnetwork_name = \"team\"\n",
            )
            .unwrap();
        assert_eq!(store.read_profile("b").unwrap().name, "team");

        store.write_profile("c", "ipv4 = \"10.0.0.1\"\n").unwrap();
        assert_eq!(store.read_profile("c").unwrap().name, "c");
    }

    #[test]
    fn list_is_sorted_and_delete_works() {
        let (_dir, store) = store();
        store
            .write_profile("z", "instance_name = \"Zeta\"\n")
            .unwrap();
        store
            .write_profile("a", "instance_name = \"alpha\"\n")
            .unwrap();
        let list = store.list_profiles().unwrap();
        assert_eq!(
            list.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "Zeta"]
        );

        store.delete_profile("a").unwrap();
        assert!(!store.profile_exists("a"));
        assert_eq!(store.list_profiles().unwrap().len(), 1);
        // Deleting a missing profile is a no-op.
        store.delete_profile("a").unwrap();
    }

    #[test]
    fn list_on_missing_dir_is_empty() {
        let (_dir, store) = store();
        assert!(store.list_profiles().unwrap().is_empty());
    }

    #[test]
    fn state_roundtrip_and_defaults() {
        let (_dir, store) = store();
        // Missing file → defaults (auto_restart defaults true).
        let def = store.load_state();
        assert!(def.auto_restart);
        assert!(!def.autostart);
        assert!(def.running.is_empty());

        let state = AppState {
            running: vec!["id-1".into(), "id-2".into()],
            autostart: true,
            auto_restart: false,
        };
        store.save_state(&state).unwrap();
        assert_eq!(store.load_state(), state);
    }

    #[test]
    fn partial_state_json_uses_field_defaults() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.profiles_dir()).unwrap();
        std::fs::write(store.state_path(), "{\"autostart\": true}").unwrap();
        let s = store.load_state();
        assert!(s.autostart);
        assert!(s.auto_restart); // defaulted true
        assert!(s.running.is_empty());
    }

    #[test]
    fn rejects_path_traversal_ids() {
        let (_dir, store) = store();
        assert!(store.write_profile("../evil", "x = 1").is_err());
        assert!(store.read_profile("a/b").is_err());
    }
}
