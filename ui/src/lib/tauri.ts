// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import type { Backend, Config, DaemonEvent, DiscoveredInfo, PeerInfo, StatusInfo } from './api'

interface TauriGlobal {
  core: { invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> }
  event: { listen<T>(name: string, cb: (e: { payload: T }) => void): Promise<() => void> }
}

declare global {
  interface Window {
    __TAURI__?: TauriGlobal
  }
}

export function hasTauri(): boolean {
  return typeof window !== 'undefined' && !!window.__TAURI__
}

/** The shell exposes one `ipc_call` command that forwards a request to the daemon. */
export function tauriBackend(): Backend {
  const tauri = window.__TAURI__!
  const call = <T>(method: string, params?: unknown) =>
    tauri.core.invoke<T>('ipc_call', { method, params: params ?? null })
  return {
    kind: 'tauri',
    status: () => call<StatusInfo>('status'),
    paired: () => call<PeerInfo[]>('paired'),
    discovered: () => call<DiscoveredInfo[]>('discovered'),
    connect: (address) => call('connect', { address }),
    jump: (machine) => call('jump', { machine }),
    setLocked: (locked) => call('set_locked', { locked }),
    answerPairing: (id, accept) => call('answer_pairing', { id, accept }),
    revoke: (machine) => call('revoke', { machine }),
    getConfig: () => call<Config>('get_config'),
    setConfig: (config) => call('set_config', { config }),
    version: () => tauri.core.invoke<string>('app_version'),
    onEvent(handler) {
      let stop: (() => void) | undefined
      let cancelled = false
      void tauri.event.listen<DaemonEvent>('daemon-event', (e) => handler(e.payload)).then((un) => {
        if (cancelled) un()
        else stop = un
      })
      return () => {
        cancelled = true
        stop?.()
      }
    },
  }
}
