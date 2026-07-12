// Typed wrappers over the Tauri command surface + event bus.
//
// Command contract mirrors easytier-mac/gui/src-tauri/src/lib.rs. Arg objects
// use camelCase keys; Tauri v2 maps them to the snake_case Rust parameter
// names automatically. All commands reject with a plain string on error.

import { invoke } from '@tauri-apps/api/core'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'

// ---------------------------------------------------------------------------
// Types (fields mirror the Rust serde structs exactly)
// ---------------------------------------------------------------------------

export interface ProfileMeta {
  id: string
  name: string
}

export interface ProfileRecord {
  id: string
  name: string
  toml: string
}

export interface PeerRow {
  peer_id: number
  hostname: string
  ipv4: string
  cost: string
  latency_ms: number
  loss_rate: number
  rx_bytes: number
  tx_bytes: number
  nat_type: string
  version: string
  is_local: boolean
}

export interface NetworkStatus {
  instance_id: string
  rows: PeerRow[]
}

export interface SupervisorStatus {
  connected: boolean
  core_running: boolean
  rpc_port: number | null
  installed: boolean
}

export interface InstallationStatus {
  plist_exists: boolean
  supervisor_bin_exists: boolean
  core_bin_exists: boolean
  installed: boolean
}

export interface Conflicts {
  unmanaged_core: boolean
  unmanaged_core_cmds: string[]
  tun_vpn: boolean
  tun_vpn_cmds: string[]
}

export interface Settings {
  autostart: boolean
  auto_restart: boolean
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

export function listProfiles(): Promise<ProfileMeta[]> {
  return invoke('list_profiles')
}

export function getProfile(id: string): Promise<ProfileRecord> {
  return invoke('get_profile', { id })
}

export function saveProfile(id: string | null, toml: string): Promise<ProfileMeta> {
  return invoke('save_profile', { id, toml })
}

export function validateToml(toml: string): Promise<void> {
  return invoke('validate_toml', { toml })
}

export function deleteProfile(id: string): Promise<void> {
  return invoke('delete_profile', { id })
}

export function runningIds(): Promise<string[]> {
  return invoke('running_ids')
}

export function startNetwork(id: string): Promise<void> {
  return invoke('start_network', { id })
}

export function stopNetwork(id: string): Promise<void> {
  return invoke('stop_network', { id })
}

export function networkStatus(id: string): Promise<NetworkStatus> {
  return invoke('network_status', { id })
}

export function supervisorStatus(): Promise<SupervisorStatus> {
  return invoke('supervisor_status')
}

export function installationStatus(): Promise<InstallationStatus> {
  return invoke('installation_status')
}

export function detectConflicts(): Promise<Conflicts> {
  return invoke('detect_conflicts')
}

export function getSettings(): Promise<Settings> {
  return invoke('get_settings')
}

export function setAutoRestart(enabled: boolean): Promise<void> {
  return invoke('set_auto_restart', { enabled })
}

export function setAutostart(enabled: boolean): Promise<void> {
  return invoke('set_autostart', { enabled })
}

export function installPrivileged(supervisorBin?: string, coreBin?: string): Promise<void> {
  return invoke('install_privileged', { supervisorBin, coreBin })
}

export function uninstallPrivileged(): Promise<void> {
  return invoke('uninstall_privileged')
}

/** Take over an existing supervisor owner lease after the user confirms. */
export function takeoverSupervisor(): Promise<void> {
  return invoke('takeover_supervisor')
}

export function quitApp(): Promise<void> {
  return invoke('quit_app')
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/** Subscribe to a Tauri event; returns the unlisten function. */
export function onEvent<T = unknown>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  return listen<T>(event, (e) => handler(e.payload))
}
