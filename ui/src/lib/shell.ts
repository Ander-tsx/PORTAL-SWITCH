// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import type { Backend, Config, DaemonEvent, DiscoveredInfo, PeerInfo, StatusInfo } from './api'

declare global {
  interface Window {
    __PORTAL_SHELL__?: boolean
    ipc?: { postMessage(message: string): void }
    __portalReply?: (id: number, ok: boolean, payload: unknown) => void
    __portalEvent?: (event: DaemonEvent) => void
  }
}

export function hasShell(): boolean {
  return typeof window !== 'undefined' && !!window.__PORTAL_SHELL__ && !!window.ipc
}

/** Backend for the native window: calls go through `window.ipc` to the daemon's local API. */
export function shellBackend(): Backend {
  let next = 1
  const pending = new Map<number, { resolve: (v: unknown) => void; reject: (e: Error) => void }>()
  const handlers = new Set<(e: DaemonEvent) => void>()

  window.__portalReply = (id, ok, payload) => {
    const waiter = pending.get(id)
    if (!waiter) return
    pending.delete(id)
    if (ok) waiter.resolve(payload)
    else waiter.reject(new Error(String(payload)))
  }
  window.__portalEvent = (event) => handlers.forEach((h) => h(event))

  const call = <T>(method: string, params?: unknown) =>
    new Promise<T>((resolve, reject) => {
      const id = next++
      pending.set(id, { resolve: resolve as (v: unknown) => void, reject })
      window.ipc!.postMessage(JSON.stringify({ id, method, params: params ?? null }))
    })

  return {
    kind: 'shell',
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
    version: () => call<string>('app_version'),
    onEvent(handler) {
      handlers.add(handler)
      return () => handlers.delete(handler)
    },
  }
}
