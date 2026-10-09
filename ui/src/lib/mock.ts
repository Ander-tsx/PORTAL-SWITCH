// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import type { Backend, Config, DaemonEvent, DiscoveredInfo, PeerInfo, StatusInfo } from './api'

const ME = 'a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1'
const LAPTOP = 'b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2b2'
const PC = 'c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3'

/** In-memory daemon used by `npm run dev` and the tests; behaves like the real one closely enough. */
export function mockBackend(): Backend {
  const handlers = new Set<(e: DaemonEvent) => void>()
  const emit = (e: DaemonEvent) => handlers.forEach((h) => h(e))

  const names: Record<string, string> = { [ME]: 'Studio Mac', [LAPTOP]: 'Laptop', [PC]: 'Living-room PC' }
  let peers: PeerInfo[] = [{ machine: LAPTOP, name: 'Laptop' }]
  let paired: PeerInfo[] = [{ machine: LAPTOP, name: 'Laptop' }]
  let active = ME
  let locked = false
  let pendingPair: { id: number; machine: string } | undefined
  let config: Config = {
    version: 1,
    machine_name: 'Studio Mac',
    is_host: true,
    listen_port: 24850,
    layout: [
      { machine_id: ME, x: 0, y: 0, width: 1920, height: 1080 },
      { machine_id: LAPTOP, x: 1920, y: 180, width: 1440, height: 900 },
    ],
    switch: {
      delay_ms: 0,
      double_tap_ms: 0,
      tap_zone_px: 40,
      corner_px: 0,
      dead_top_left: false,
      dead_top_right: false,
      dead_bottom_left: false,
      dead_bottom_right: false,
      required_modifiers: [],
    },
    peers: [],
  }
  const discovered = (): DiscoveredInfo[] => [
    { machine: LAPTOP, name: 'Laptop', addresses: ['192.168.1.20'], port: 24850, is_host: false, paired: true },
    ...(peers.some((p) => p.machine === PC)
      ? []
      : [{ machine: PC, name: 'Living-room PC', addresses: ['192.168.1.31'], port: 24850, is_host: false, paired: false }]),
  ]

  return {
    kind: 'mock',
    async status(): Promise<StatusInfo> {
      return { machine: ME, is_host: config.is_host, active, locked, peers }
    },
    async paired() {
      return paired
    },
    async discovered() {
      return discovered()
    },
    async connect(address) {
      if (!/^[\d.]+:\d+$/.test(address)) throw new Error('Address must look like 192.168.1.5:24850')
      const machine = address.startsWith('192.168.1.31') ? PC : LAPTOP
      pendingPair = { id: Date.now(), machine }
      setTimeout(
        () =>
          emit({
            event: 'pairing_requested',
            data: { id: pendingPair!.id, remote_name: names[machine], code: '482 913'.replace(' ', '') },
          }),
        500,
      )
    },
    async jump(machine) {
      if (config.is_host) {
        active = machine
        emit({ event: 'active_changed', data: { machine } })
      }
    },
    async setLocked(value) {
      locked = value
    },
    async answerPairing(id, accept) {
      if (pendingPair?.id !== id) return
      const { machine } = pendingPair
      pendingPair = undefined
      if (!accept) {
        emit({ event: 'dial_failed', data: { address: machine, reason: 'pairing was declined' } })
        return
      }
      const peer = { machine, name: names[machine] }
      paired = [...paired.filter((p) => p.machine !== machine), peer]
      peers = [...peers.filter((p) => p.machine !== machine), peer]
      if (!config.layout.some((s) => s.machine_id === machine)) {
        const right = Math.max(...config.layout.map((s) => s.x + s.width))
        config = { ...config, layout: [...config.layout, { machine_id: machine, x: right, y: 0, width: 1920, height: 1080 }] }
      }
      emit({ event: 'peer_connected', data: { machine, name: peer.name } })
    },
    async revoke(machine) {
      paired = paired.filter((p) => p.machine !== machine)
      peers = peers.filter((p) => p.machine !== machine)
      emit({ event: 'peer_disconnected', data: { machine } })
    },
    async getConfig() {
      return structuredClone(config)
    },
    async setConfig(next) {
      const restart = next.is_host !== config.is_host || next.listen_port !== config.listen_port
      config = structuredClone(next)
      return { restart_required: restart }
    },
    onEvent(handler) {
      handlers.add(handler)
      return () => handlers.delete(handler)
    },
    async version() {
      return '0.0.0 (demo)'
    },
  }
}
