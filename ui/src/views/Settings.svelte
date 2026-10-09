<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import type { Config, ModifierKey } from '../lib/api'
  import { app } from '../lib/store.svelte'

  const t = $derived(app.t)
  let draft = $state<Config | undefined>(app.config ? structuredClone($state.snapshot(app.config)) : undefined)
  let restart = $state(false)

  const mods: ModifierKey[] = ['shift', 'control', 'alt', 'meta']
  const corners = [
    ['dead_top_left', 'topLeft'],
    ['dead_top_right', 'topRight'],
    ['dead_bottom_left', 'bottomLeft'],
    ['dead_bottom_right', 'bottomRight'],
  ] as const

  function toggle(key: ModifierKey) {
    if (!draft) return
    const set = new Set(draft.switch.required_modifiers)
    if (!set.delete(key)) set.add(key)
    draft.switch.required_modifiers = [...set]
  }

  async function save() {
    if (!draft) return
    const result = await app.run(() => app.backend.setConfig($state.snapshot(draft!) as Config))
    if (result) {
      restart = result.restart_required
      await app.reloadConfig()
      app.flash(t('saved'))
    }
  }
</script>

{#if draft}
  <form class="stack" onsubmit={(e) => { e.preventDefault(); void save() }}>
    <section class="card stack">
      <h2>{t('settingsTitle')}</h2>

      <div class="field">
        <label for="name">{t('computerName')}</label>
        <input id="name" type="text" bind:value={draft.machine_name} maxlength="64" />
      </div>

      <fieldset class="field">
        <legend class="label">{t('role')}</legend>
        <label class="row small"><input type="radio" name="role" checked={draft.is_host} onchange={() => (draft!.is_host = true)} /> {t('roleHost')}</label>
        <label class="row small"><input type="radio" name="role" checked={!draft.is_host} onchange={() => (draft!.is_host = false)} /> {t('rolePeer')}</label>
      </fieldset>

      <div class="field">
        <label for="delay">{t('delay')}</label>
        <div class="row"><input id="delay" type="number" min="0" max="5000" step="50" bind:value={draft.switch.delay_ms} /><span class="muted">{t('ms')}</span></div>
        <span class="muted small">{t('delayHelp')}</span>
      </div>

      <div class="field">
        <label for="tap">{t('doubleTap')}</label>
        <div class="row"><input id="tap" type="number" min="0" max="2000" step="50" bind:value={draft.switch.double_tap_ms} /><span class="muted">{t('ms')}</span></div>
        <span class="muted small">{t('doubleTapHelp')}</span>
      </div>

      <fieldset class="field">
        <legend class="label">{t('corners')}</legend>
        <span class="muted small">{t('cornersHelp')}</span>
        <div class="row wrap">
          {#each corners as [key, label] (key)}
            <label class="row small"><input type="checkbox" bind:checked={draft.switch[key]} /> {t(label)}</label>
          {/each}
        </div>
        <div class="row"><input type="number" min="0" max="400" step="5" bind:value={draft.switch.corner_px} aria-label={t('cornersHelp')} /><span class="muted">{t('px')}</span></div>
      </fieldset>

      <fieldset class="field">
        <legend class="label">{t('modifiers')}</legend>
        <span class="muted small">{t('modifiersHelp')}</span>
        <div class="row wrap">
          {#each mods as key (key)}
            <label class="row small"><input type="checkbox" checked={draft.switch.required_modifiers.includes(key)} onchange={() => toggle(key)} /> {t(key)}</label>
          {/each}
        </div>
      </fieldset>

      {#if restart}<p class="badge accent">{t('restartNeeded')}</p>{/if}
      <div><button class="primary" type="submit">{t('save')}</button></div>
    </section>
  </form>
{/if}
