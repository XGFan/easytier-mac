//! EasyTier macOS menu-bar GUI (M1) — Tauri backend.
//!
//! Wiring (DESIGN §8): a background [`supervisor_client`] drives the launchd
//! supervisor over its control socket; [`rpc`] talks to the managed core's RPC
//! portal; [`profiles`] persists profiles and GUI state. This module exposes the
//! Tauri command surface, forwards supervisor events to the webview, owns the
//! menu-bar tray + window lifecycle, and implements install/conflict helpers.

pub mod conflict;
pub mod install;
pub mod profiles;
pub mod proto;
pub mod rpc;
pub mod supervisor_client;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use easytier::common::config::{ConfigLoader, TomlConfigLoader};
use serde::Serialize;
use tokio::sync::mpsc::{self, UnboundedReceiver};
use uuid::Uuid;

use profiles::{AppState, ProfileMeta, ProfileRecord, ProfileStore};
use rpc::{NetworkStatus, RpcClient};
use supervisor_client::{SupervisorClient, SupervisorConfig, SupervisorEvent};

#[cfg(not(target_os = "macos"))]
compile_error!("easytier-mac-gui targets macOS only");

use tauri::{AppHandle, Emitter, Manager, State};
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri_plugin_autostart::ManagerExt;

const MAX_AUTO_RESTART: u32 = 3;

/// Shared application state managed by Tauri and the background tasks.
pub struct AppInner {
    supervisor: SupervisorClient,
    rpc: RpcClient,
    store: ProfileStore,
    /// Live set of running profile ids (== instance ids).
    running: Mutex<HashSet<String>>,
    /// Profiles to (re)start once the supervisor first connects.
    pending_restore: Mutex<Vec<String>>,
    restored: AtomicBool,
    auto_restart: AtomicBool,
    restart_failures: AtomicU32,
}

impl AppInner {
    fn persist_running(&self) {
        let running: Vec<String> = self.running.lock().unwrap().iter().cloned().collect();
        let auto_restart = self.auto_restart.load(Ordering::SeqCst);
        // Atomic read-modify-write under the store's lock.
        let _ = self.store.update_state(|state| {
            state.running = running;
            state.auto_restart = auto_restart;
        });
    }
}

// ---------------------------------------------------------------------------
// Command payloads
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct SupervisorStatus {
    connected: bool,
    core_running: bool,
    rpc_port: Option<u16>,
    installed: bool,
}

// ---------------------------------------------------------------------------
// Profile commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn list_profiles(state: State<'_, Arc<AppInner>>) -> Result<Vec<ProfileMeta>, String> {
    state.store.list_profiles()
}

#[tauri::command]
fn get_profile(state: State<'_, Arc<AppInner>>, id: String) -> Result<ProfileRecord, String> {
    state.store.read_profile(&id)
}

/// Validate + normalise TOML, embed a stable `instance_id`, and persist. The
/// stored id equals the config's instance id so it maps to the runtime instance.
#[tauri::command]
fn save_profile(
    state: State<'_, Arc<AppInner>>,
    id: Option<String>,
    toml: String,
) -> Result<ProfileMeta, String> {
    let loader =
        TomlConfigLoader::new_from_str(&toml).map_err(|e| format!("invalid config: {e}"))?;
    let id = match id {
        Some(s) if !s.trim().is_empty() => {
            Uuid::parse_str(s.trim()).map_err(|e| format!("invalid id: {e}"))?
        }
        _ => loader.get_id(),
    };
    loader.set_id(id);
    let normalized = loader.dump();
    state.store.write_profile(&id.to_string(), &normalized)
}

#[tauri::command]
fn validate_toml(toml: String) -> Result<(), String> {
    TomlConfigLoader::new_from_str(&toml)
        .map(|_| ())
        .map_err(|e| format!("{e}"))
}

#[tauri::command]
async fn delete_profile(state: State<'_, Arc<AppInner>>, id: String) -> Result<(), String> {
    // Stop it first if running, then remove the file.
    let inner = state.inner().clone();
    do_stop_network(&inner, &id).await;
    inner.store.delete_profile(&id)
}

// ---------------------------------------------------------------------------
// Network lifecycle commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn running_ids(state: State<'_, Arc<AppInner>>) -> Vec<String> {
    state.running.lock().unwrap().iter().cloned().collect()
}

#[tauri::command]
async fn start_network(
    state: State<'_, Arc<AppInner>>,
    app: AppHandle,
    id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    do_start_network(&inner, &id).await?;
    inner.restart_failures.store(0, Ordering::SeqCst);
    rebuild_tray(&app);
    Ok(())
}

async fn do_start_network(inner: &Arc<AppInner>, id: &str) -> Result<(), String> {
    let rec = inner.store.read_profile(id)?;
    // Ensure the core is running (triggers launchd activation if needed).
    let info = inner.supervisor.start().await?;
    inner.rpc.set_port(Some(info.rpc_port));
    inner.rpc.run_network_instance(&rec.toml).await?;
    inner.running.lock().unwrap().insert(id.to_string());
    inner.persist_running();
    Ok(())
}

#[tauri::command]
async fn stop_network(
    state: State<'_, Arc<AppInner>>,
    id: String,
) -> Result<(), String> {
    let inner = state.inner().clone();
    do_stop_network(&inner, &id).await;
    Ok(())
}

async fn do_stop_network(inner: &Arc<AppInner>, id: &str) {
    if inner.rpc.port().is_some() {
        if let Ok(uuid) = Uuid::parse_str(id) {
            // Best effort: the instance may already be gone.
            let _ = inner.rpc.delete_network_instance(uuid).await;
        }
    }
    let now_empty = {
        let mut running = inner.running.lock().unwrap();
        running.remove(id);
        running.is_empty()
    };
    inner.persist_running();
    // When nothing is left running, ask the supervisor to stop the core so the
    // on-demand process does not idle.
    if now_empty {
        let _ = inner.supervisor.stop().await;
        inner.rpc.set_port(None);
    }
}

#[tauri::command]
async fn network_status(
    state: State<'_, Arc<AppInner>>,
    id: String,
) -> Result<NetworkStatus, String> {
    let uuid = Uuid::parse_str(&id).map_err(|e| format!("invalid id: {e}"))?;
    state.rpc.network_status(uuid).await
}

// ---------------------------------------------------------------------------
// Supervisor / settings / install commands
// ---------------------------------------------------------------------------

#[tauri::command]
fn supervisor_status(state: State<'_, Arc<AppInner>>) -> SupervisorStatus {
    SupervisorStatus {
        connected: state.supervisor.is_connected(),
        core_running: state.rpc.port().is_some(),
        rpc_port: state.rpc.port(),
        installed: install::installation_status().installed,
    }
}

#[tauri::command]
fn installation_status() -> install::InstallationStatus {
    install::installation_status()
}

#[tauri::command]
fn detect_conflicts() -> conflict::Conflicts {
    conflict::detect()
}

#[derive(Serialize)]
pub struct Settings {
    autostart: bool,
    auto_restart: bool,
}

#[tauri::command]
fn get_settings(state: State<'_, Arc<AppInner>>, app: AppHandle) -> Settings {
    let autostart = app.autolaunch().is_enabled().unwrap_or(false);
    Settings {
        autostart,
        auto_restart: state.auto_restart.load(Ordering::SeqCst),
    }
}

#[tauri::command]
fn set_auto_restart(state: State<'_, Arc<AppInner>>, enabled: bool) {
    state.auto_restart.store(enabled, Ordering::SeqCst);
    state.persist_running();
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    if enabled {
        mgr.enable().map_err(|e| e.to_string())?;
    } else {
        mgr.disable().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Install the privileged supervisor via `osascript` (single admin prompt).
/// Paths default to the workspace dev binaries; callers may override. On success
/// nudge the driver to reconnect immediately rather than wait out the backoff.
#[tauri::command]
fn install_privileged(
    state: State<'_, Arc<AppInner>>,
    supervisor_bin: Option<String>,
    core_bin: Option<String>,
) -> Result<(), String> {
    install::run_install(supervisor_bin, core_bin)?;
    state.supervisor.request_reconnect();
    Ok(())
}

/// Take over an existing supervisor owner lease (after user confirmation of a
/// `busy` event). Triggers one takeover reconnect; if it fails the driver
/// returns to normal backoff.
#[tauri::command]
fn takeover_supervisor(state: State<'_, Arc<AppInner>>) {
    state.supervisor.request_takeover();
}

#[tauri::command]
fn uninstall_privileged() -> Result<(), String> {
    install::run_uninstall()
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    // The supervisor tears the core down when the owner connection drops. Process
    // exit closes the control socket (= disconnect); we also fire a best-effort
    // explicit shutdown first (DESIGN §8).
    if let Some(inner) = app.try_state::<Arc<AppInner>>() {
        let inner = inner.inner().clone();
        tauri::async_runtime::spawn(async move {
            inner.supervisor.shutdown().await;
        });
    }
    app.exit(0);
}

// ---------------------------------------------------------------------------
// Window / tray helpers
// ---------------------------------------------------------------------------

fn set_dock_visible(app: &AppHandle, visible: bool) {
    use tauri::ActivationPolicy;
    let _ = app.set_activation_policy(if visible {
        ActivationPolicy::Regular
    } else {
        ActivationPolicy::Accessory
    });
}

fn show_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
        set_dock_visible(app, true);
    }
}

fn hide_main_window(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
        set_dock_visible(app, false);
    }
}

/// Rebuild the tray menu from the current profiles + running set: a Show item,
/// one toggle per network, then Quit.
fn rebuild_tray(app: &AppHandle) {
    let Some(tray) = app.tray_by_id("main") else {
        return;
    };
    let menu = match build_tray_menu(app) {
        Ok(m) => m,
        Err(_) => return,
    };
    let _ = tray.set_menu(Some(menu));
}

fn build_tray_menu(app: &AppHandle) -> tauri::Result<Menu<tauri::Wry>> {
    let show = MenuItem::with_id(app, "show", "打开主窗口", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出 EasyTier", true, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;

    let menu = Menu::new(app)?;
    menu.append(&show)?;
    menu.append(&sep)?;

    if let Some(inner) = app.try_state::<Arc<AppInner>>() {
        let running = inner.running.lock().unwrap().clone();
        if let Ok(profiles) = inner.store.list_profiles() {
            for p in profiles {
                let is_running = running.contains(&p.id);
                let label = format!("{} {}", if is_running { "◉" } else { "○" }, p.name);
                let action = if is_running { "stop" } else { "start" };
                let item = MenuItem::with_id(
                    app,
                    format!("net:{action}:{}", p.id),
                    label,
                    true,
                    None::<&str>,
                )?;
                menu.append(&item)?;
            }
        }
    }

    let sep2 = PredefinedMenuItem::separator(app)?;
    menu.append(&sep2)?;
    menu.append(&quit)?;
    Ok(menu)
}

fn on_menu_event(app: &AppHandle, id: &str) {
    match id {
        "show" => show_main_window(app),
        "quit" => quit_app(app.clone()),
        other if other.starts_with("net:") => {
            // net:<start|stop>:<profile id>
            let mut parts = other.splitn(3, ':');
            let _ = parts.next();
            let action = parts.next().unwrap_or("").to_string();
            let pid = parts.next().unwrap_or("").to_string();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                if let Some(inner) = app.try_state::<Arc<AppInner>>() {
                    let inner = inner.inner().clone();
                    match action.as_str() {
                        "start" => {
                            let _ = do_start_network(&inner, &pid).await;
                        }
                        "stop" => do_stop_network(&inner, &pid).await,
                        _ => {}
                    }
                    rebuild_tray(&app);
                    let _ = app.emit("network://changed", &pid);
                }
            });
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Supervisor event forwarding + auto-restart
// ---------------------------------------------------------------------------

async fn forward_events(
    mut rx: UnboundedReceiver<SupervisorEvent>,
    inner: Arc<AppInner>,
    app: AppHandle,
) {
    while let Some(ev) = rx.recv().await {
        match ev {
            SupervisorEvent::Connected {
                version,
                core,
                rpc_port,
            } => {
                if let Some(p) = rpc_port {
                    inner.rpc.set_port(Some(p));
                }
                let _ = app.emit(
                    "supervisor://connected",
                    serde_json::json!({
                        "version": version,
                        "core": format!("{core:?}").to_lowercase(),
                        "rpc_port": rpc_port,
                    }),
                );
                maybe_restore(&inner, &app).await;
            }
            SupervisorEvent::Disconnected => {
                let _ = app.emit("supervisor://disconnected", ());
            }
            SupervisorEvent::CoreStarted { pid, rpc_port } => {
                inner.rpc.set_port(Some(rpc_port));
                let _ = app.emit(
                    "core://started",
                    serde_json::json!({ "pid": pid, "rpc_port": rpc_port }),
                );
            }
            SupervisorEvent::CoreStopped { reason } => {
                inner.rpc.set_port(None);
                let _ = app.emit("core://stopped", serde_json::json!({ "reason": reason }));
            }
            SupervisorEvent::CoreExited { code, signal } => {
                inner.rpc.set_port(None);
                let _ = app.emit(
                    "core://exited",
                    serde_json::json!({ "code": code, "signal": signal }),
                );
                handle_core_exit(&inner, &app).await;
            }
            SupervisorEvent::Kicked => {
                let _ = app.emit("supervisor://kicked", ());
            }
            SupervisorEvent::Busy { owner } => {
                let _ = app.emit("supervisor://busy", serde_json::json!({ "owner": owner }));
            }
            SupervisorEvent::Error { code, msg } => {
                let _ = app.emit(
                    "supervisor://error",
                    serde_json::json!({ "code": code, "msg": msg }),
                );
            }
        }
    }
}

/// On first successful connect, restart the profiles that were running last.
async fn maybe_restore(inner: &Arc<AppInner>, app: &AppHandle) {
    if inner.restored.swap(true, Ordering::SeqCst) {
        return;
    }
    let ids: Vec<String> = std::mem::take(&mut *inner.pending_restore.lock().unwrap());
    if ids.is_empty() {
        return;
    }
    for id in ids {
        if inner.store.profile_exists(&id) {
            let _ = do_start_network(inner, &id).await;
        }
    }
    rebuild_tray(app);
    let _ = app.emit("network://changed", "restore");
}

/// Auto-restart the core + previously running instances after an unexpected
/// `core_exited` (DESIGN §8). `restart_failures` counts *consecutive* failures:
/// it is reset to 0 on a fully successful restart (or a user-initiated start),
/// and only a run of `MAX_AUTO_RESTART` failures in a row gives up. A failed
/// `start` is itself a failed attempt and is retried within the same budget.
async fn handle_core_exit(inner: &Arc<AppInner>, app: &AppHandle) {
    let running: Vec<String> = inner.running.lock().unwrap().iter().cloned().collect();
    if running.is_empty() {
        return;
    }
    if !inner.auto_restart.load(Ordering::SeqCst) {
        let _ = app.emit(
            "network://restart_skipped",
            serde_json::json!({ "reason": "auto_restart_disabled" }),
        );
        return;
    }

    loop {
        let attempt = inner.restart_failures.load(Ordering::SeqCst) + 1;
        if attempt > MAX_AUTO_RESTART {
            let _ = app.emit(
                "network://restart_gaveup",
                serde_json::json!({ "attempts": MAX_AUTO_RESTART }),
            );
            return;
        }
        // Backoff grows with the consecutive-failure count.
        tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;

        match try_restart(inner, &running).await {
            Ok(()) => {
                inner.restart_failures.store(0, Ordering::SeqCst);
                let _ = app.emit(
                    "network://restarted",
                    serde_json::json!({ "attempt": attempt, "count": running.len() }),
                );
                rebuild_tray(app);
                return;
            }
            Err(e) => {
                inner.restart_failures.fetch_add(1, Ordering::SeqCst);
                let _ = app.emit(
                    "network://restart_failed",
                    serde_json::json!({ "error": e, "attempt": attempt }),
                );
                // Retry the next iteration until the budget is exhausted.
            }
        }
    }
}

/// One restart attempt: bring the core up and re-run every previously running
/// instance. Any failure (start or run) fails the whole attempt.
async fn try_restart(inner: &Arc<AppInner>, running: &[String]) -> Result<(), String> {
    let info = inner.supervisor.start().await?;
    inner.rpc.set_port(Some(info.rpc_port));
    for id in running {
        let rec = inner.store.read_profile(id)?;
        inner.rpc.run_network_instance(&rec.toml).await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// App entry point
// ---------------------------------------------------------------------------

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let launched_hidden = std::env::args().any(|a| a == "--hidden");

    let mut builder = tauri::Builder::default();

    builder = builder.plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
        show_main_window(app);
    }));

    builder = builder.plugin(tauri_plugin_autostart::init(
        tauri_plugin_autostart::MacosLauncher::LaunchAgent,
        Some(vec!["--hidden"]),
    ));

    builder
        .setup(move |app| {
            let handle = app.handle().clone();

            // --- Build shared state + background driver ---
            let store = ProfileStore::new(ProfileStore::default_root());
            let persisted: AppState = store.load_state();

            let (ev_tx, ev_rx) = mpsc::unbounded_channel::<SupervisorEvent>();
            let config = SupervisorConfig::default();
            // Spawning the supervisor driver requires an async (tokio) context.
            let supervisor =
                tauri::async_runtime::block_on(async { SupervisorClient::spawn(config, ev_tx) });

            let inner = Arc::new(AppInner {
                supervisor,
                rpc: RpcClient::new(),
                store,
                running: Mutex::new(HashSet::new()),
                pending_restore: Mutex::new(persisted.running.clone()),
                restored: AtomicBool::new(false),
                auto_restart: AtomicBool::new(persisted.auto_restart),
                restart_failures: AtomicU32::new(0),
            });
            app.manage(inner.clone());

            tauri::async_runtime::spawn(forward_events(ev_rx, inner, handle.clone()));

            // --- Tray ---
            let menu = build_tray_menu(&handle)?;
            let _tray = TrayIconBuilder::with_id("main")
                .icon(tauri::image::Image::from_bytes(include_bytes!(
                    "../icons/icon.png"
                ))?)
                .icon_as_template(true)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| on_menu_event(app, event.id.as_ref()))
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                })
                .build(app)?;

            // --- Window visibility ---
            if launched_hidden {
                set_dock_visible(&handle, false);
            } else {
                show_main_window(&handle);
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_profiles,
            get_profile,
            save_profile,
            validate_toml,
            delete_profile,
            running_ids,
            start_network,
            stop_network,
            network_status,
            supervisor_status,
            installation_status,
            detect_conflicts,
            get_settings,
            set_auto_restart,
            set_autostart,
            install_privileged,
            uninstall_privileged,
            takeover_supervisor,
            quit_app,
        ])
        .on_window_event(|win, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                // Closing hides to the menu bar instead of quitting (DESIGN §8).
                hide_main_window(win.app_handle());
                api.prevent_close();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running EasyTier tauri application");
}
