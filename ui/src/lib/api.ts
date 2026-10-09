// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

// Types mirror crates/portal-ipc (local daemon API, version 1) and crates/portal-config.

export interface PeerInfo {
  machine: string
  name: string
}

export interface StatusInfo {
  machine: string
  is_host: boolean
  active: string
  locked: boolean
  peers: PeerInfo[]
}

export interface DiscoveredInfo {
  machine: string
  name: string
  addresses: string[]
  port: number
  is_host: boolean
  paired: boolean
}

export type ModifierKey = 'shift' | 'control' | 'alt' | 'meta'

export interface SwitchSettings {
  delay_ms: number
  double_tap_ms: number
  tap_zone_px: number
  corner_px: number
  dead_top_left: boolean
  dead_top_right: boolean
  dead_bottom_left: boolean
  dead_bottom_right: boolean
  required_modifiers: ModifierKey[]
}

export interface ScreenConfig {
  machine_id: string
  x: number
  y: number
  width: number
  height: number
}

export interface Config {
  version: number
  machine_name: string
  is_host: boolean
  listen_port: number
  layout: ScreenConfig[]
  switch: SwitchSettings
  peers: { address: string }[]
}

export type DaemonEvent =
  | { event: 'pairing_requested'; data: { id: number; remote_name: string; code: string } }
  | { event: 'peer_connected'; data: { machine: string; name: string } }
  | { event: 'peer_disconnected'; data: { machine: string } }
  | { event: 'active_changed'; data: { machine: string } }
  | { event: 'dial_failed'; data: { address: string; reason: string } }
  | { event: 'peer_discovered'; data: { machine: string; name: string; addresses: string[]; port: number } }
  | { event: 'peer_lost'; data: { machine: string } }

/** Everything the UI needs from the daemon. Implemented over Tauri and by a mock for development. */
export interface Backend {
  readonly kind: 'tauri' | 'mock'
  status(): Promise<StatusInfo>
  paired(): Promise<PeerInfo[]>
  discovered(): Promise<DiscoveredInfo[]>
  connect(address: string): Promise<void>
  jump(machine: string): Promise<void>
  setLocked(locked: boolean): Promise<void>
  answerPairing(id: number, accept: boolean): Promise<void>
  revoke(machine: string): Promise<void>
  getConfig(): Promise<Config>
  /** Returns whether a restart is needed for the change to take full effect. */
  setConfig(config: Config): Promise<{ restart_required: boolean }>
  onEvent(handler: (event: DaemonEvent) => void): () => void
  version(): Promise<string>
}

export const DONATE_URL = 'https://buymeacoffee.com/portalswitch'
export const SPONSORS_URL = 'https://github.com/sponsors/Ander-tsx'
export const REPO_URL = 'https://github.com/Ander-tsx/PORTAL-SWITCH'
