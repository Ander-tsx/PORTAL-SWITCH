<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import { onMount } from 'svelte'
  import { app } from './lib/store.svelte'
  import About from './views/About.svelte'
  import LayoutEditor from './views/LayoutEditor.svelte'
  import Machines from './views/Machines.svelte'
  import PairingDialog from './views/PairingDialog.svelte'
  import Settings from './views/Settings.svelte'

  type Tab = 'machines' | 'layout' | 'settings' | 'about'
  let tab = $state<Tab>('machines')
  const t = $derived(app.t)

  onMount(() => void app.start())

  const tabs: { id: Tab; label: () => string }[] = [
    { id: 'machines', label: () => t('tabMachines') },
    { id: 'layout', label: () => t('tabLayout') },
    { id: 'settings', label: () => t('tabSettings') },
    { id: 'about', label: () => t('tabAbout') },
  ]
</script>

<div class="shell">
  <header>
    <div class="brand">
      <img src="/logo.svg" alt="" width="34" height="34" />
      <div>
        <strong>{t('appName')}</strong>
        <div class="muted small">{t('tagline')}</div>
      </div>
    </div>
    <nav aria-label="Main">
      {#each tabs as item (item.id)}
        <button class="tab" class:on={tab === item.id} aria-current={tab === item.id ? 'page' : undefined} onclick={() => (tab = item.id)}>
          {item.label()}
        </button>
      {/each}
    </nav>
  </header>

  {#if !app.online}
    <div class="card offline" role="status">
      <h3>{t('daemonDown')}</h3>
      <p class="muted">{t('daemonDownHelp')}</p>
    </div>
  {:else}
    <main>
      {#if tab === 'machines'}<Machines />{/if}
      {#if tab === 'layout'}<LayoutEditor />{/if}
      {#if tab === 'settings'}<Settings />{/if}
      {#if tab === 'about'}<About />{/if}
    </main>
  {/if}

  {#each app.prompts.slice(0, 1) as prompt (prompt.id)}
    <PairingDialog {prompt} />
  {/each}

  {#if app.error}
    <div class="toast error" role="alert">
      <strong>{t('errorTitle')}</strong>
      <span>{app.error}</span>
      <button class="ghost" onclick={() => (app.error = undefined)}>{t('dismiss')}</button>
    </div>
  {:else if app.notice}
    <div class="toast" role="status">{app.notice}</div>
  {/if}
</div>

<style>
  .shell {
    max-width: 880px;
    margin: 0 auto;
    padding: 1.25rem 1rem 3rem;
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 1rem;
    margin-bottom: 1.25rem;
  }
  .brand {
    display: flex;
    align-items: center;
    gap: 0.7rem;
  }
  nav {
    display: flex;
    gap: 0.25rem;
    background: var(--surface-2);
    padding: 0.25rem;
    border-radius: 12px;
  }
  .tab {
    border: 0;
    background: transparent;
    color: var(--muted);
    padding: 0.4rem 0.9rem;
  }
  .tab.on {
    background: var(--surface);
    color: var(--fg);
    box-shadow: var(--shadow);
    font-weight: 600;
  }
  .offline {
    text-align: center;
    padding: 2.5rem 1rem;
  }
  .toast {
    position: fixed;
    left: 50%;
    bottom: 1.25rem;
    transform: translateX(-50%);
    display: flex;
    align-items: center;
    gap: 0.75rem;
    background: var(--fg);
    color: var(--bg);
    padding: 0.6rem 1rem;
    border-radius: 12px;
    box-shadow: var(--shadow);
    max-width: calc(100vw - 2rem);
  }
  .toast.error {
    background: var(--danger);
    color: #fff;
  }
  .toast button {
    color: inherit;
    border-color: currentColor;
  }
</style>
