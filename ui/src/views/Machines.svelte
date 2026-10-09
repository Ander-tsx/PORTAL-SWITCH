<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import { app } from '../lib/store.svelte'

  const t = $derived(app.t)
  const status = $derived(app.status)
  const nearby = $derived(app.discovered.filter((d) => !d.paired || !status?.peers.some((p) => p.machine === d.machine)))
  let address = $state('')

  async function connect(target: string) {
    await app.run(() => app.backend.connect(target))
  }
</script>

<div class="stack">
  <section class="card">
    <div class="row wrap">
      <div class="grow">
        <h2>{app.config?.machine_name ?? t('thisComputer')}</h2>
        <span class="badge accent">{status?.is_host ? t('host') : t('peer')}</span>
        {#if status?.active === status?.machine}<span class="badge ok">{t('active')}</span>{/if}
      </div>
      {#if status?.is_host}
        <label class="row small">
          <input type="checkbox" checked={status.locked} onchange={(e) => app.run(() => app.backend.setLocked(e.currentTarget.checked))} />
          {t('lockCursor')}
        </label>
      {/if}
    </div>
  </section>

  {#each status?.peers ?? [] as peer (peer.machine)}
    <section class="card row wrap">
      <div class="grow">
        <h3>{peer.name}</h3>
        <span class="badge ok">{t('connected')}</span>
        {#if status?.active === peer.machine}<span class="badge accent">{t('active')}</span>{/if}
      </div>
      {#if status?.is_host && status.active !== peer.machine}
        <button onclick={() => app.run(() => app.backend.jump(peer.machine))}>{t('switchHere')}</button>
      {/if}
      <button class="danger ghost" onclick={() => app.run(() => app.backend.revoke(peer.machine))}>{t('forget')}</button>
    </section>
  {/each}

  <section class="stack">
    <h2>{t('nearby')}</h2>
    {#each nearby as machine (machine.machine)}
      <div class="card row wrap">
        <div class="grow">
          <h3>{machine.name}</h3>
          <span class="muted small">{machine.addresses[0]}:{machine.port}</span>
          {#if machine.paired}<span class="badge">{t('paired')}</span>{/if}
        </div>
        <button class="primary" onclick={() => connect(`${machine.addresses[0]}:${machine.port}`)}>{t('connect')}</button>
      </div>
    {:else}
      <p class="muted">{t('nearbyEmpty')}</p>
    {/each}
  </section>

  <section class="card">
    <form
      class="field"
      onsubmit={(e) => {
        e.preventDefault()
        if (address.trim()) void connect(address.trim()).then(() => (address = ''))
      }}
    >
      <label for="addr">{t('addByAddress')}</label>
      <div class="row">
        <input id="addr" type="text" bind:value={address} placeholder={t('addressPlaceholder')} inputmode="url" autocomplete="off" />
        <button class="primary" type="submit" disabled={!address.trim()}>{t('connect')}</button>
      </div>
    </form>
  </section>
</div>
