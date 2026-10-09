// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import type { Backend, Config, DaemonEvent, DiscoveredInfo, PeerInfo, StatusInfo } from './api'
import { detectLang, translator, type Lang } from './i18n'
import { mockBackend } from './mock'
import { hasTauri, tauriBackend } from './tauri'

export interface PairingPrompt {
  id: number
  name: string
  code: string
}

/** Reactive application state shared by all views. */
class AppState {
  backend: Backend
  lang = $state<Lang>(detectLang())
  online = $state(false)
  status = $state<StatusInfo | undefined>()
  paired = $state<PeerInfo[]>([])
  discovered = $state<DiscoveredInfo[]>([])
  config = $state<Config | undefined>()
  prompts = $state<PairingPrompt[]>([])
  error = $state<string | undefined>()
  notice = $state<string | undefined>()
  version = $state('')

  t = $derived(translator(this.lang))

  constructor() {
    this.backend = hasTauri() ? tauriBackend() : mockBackend()
  }

  /** Name of any known machine, falling back to a short ID. */
  nameOf(machine: string): string {
    if (machine === this.status?.machine) return this.config?.machine_name ?? this.t('thisComputer')
    const known =
      this.status?.peers.find((p) => p.machine === machine) ??
      this.paired.find((p) => p.machine === machine) ??
      this.discovered.find((d) => d.machine === machine)
    return known?.name ?? machine.slice(0, 8)
  }

  async start() {
    this.backend.onEvent((e) => this.onEvent(e))
    this.version = await this.backend.version().catch(() => '')
    await this.refresh()
    setInterval(() => void this.refresh(), 5000)
  }

  async refresh() {
    try {
      const [status, paired, discovered, config] = await Promise.all([
        this.backend.status(),
        this.backend.paired(),
        this.backend.discovered(),
        this.backend.getConfig(),
      ])
      this.status = status
      this.paired = paired
      this.discovered = discovered
      this.config ??= config
      this.online = true
    } catch {
      this.online = false
    }
  }

  private onEvent(e: DaemonEvent) {
    switch (e.event) {
      case 'pairing_requested':
        this.prompts = [...this.prompts, { id: e.data.id, name: e.data.remote_name, code: e.data.code }]
        break
      case 'dial_failed':
        this.error = this.t('pairFailed', { name: e.data.address })
        break
      case 'peer_connected':
        void this.reloadConfig()
        void this.refresh()
        break
      default:
        void this.refresh()
    }
  }

  async reloadConfig() {
    this.config = await this.backend.getConfig()
  }

  async answer(prompt: PairingPrompt, accept: boolean) {
    this.prompts = this.prompts.filter((p) => p.id !== prompt.id)
    await this.run(() => this.backend.answerPairing(prompt.id, accept))
  }

  /** Run an action; failures become a visible error instead of a silent no-op. */
  async run<T>(action: () => Promise<T>): Promise<T | undefined> {
    try {
      const result = await action()
      await this.refresh()
      return result
    } catch (e) {
      this.error = e instanceof Error ? e.message : String(e)
      return undefined
    }
  }

  flash(message: string) {
    this.notice = message
    setTimeout(() => {
      if (this.notice === message) this.notice = undefined
    }, 2500)
  }
}

export const app = new AppState()
