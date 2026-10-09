<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import type { PairingPrompt } from '../lib/store.svelte'
  import { app } from '../lib/store.svelte'

  let { prompt }: { prompt: PairingPrompt } = $props()
  const t = $derived(app.t)
  const spaced = $derived(`${prompt.code.slice(0, 3)} ${prompt.code.slice(3)}`)
</script>

<div class="backdrop">
  <div class="card dialog" role="dialog" aria-modal="true" aria-labelledby="pair-title">
    <h2 id="pair-title">{t('pairingTitle', { name: prompt.name })}</h2>
    <p class="muted">{t('pairingBody')}</p>
    <div class="code" aria-label={spaced}>{spaced}</div>
    <div class="row actions">
      <button onclick={() => app.answer(prompt, false)}>{t('pairingDecline')}</button>
      <button class="primary" onclick={() => app.answer(prompt, true)}>{t('pairingAccept')}</button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    display: grid;
    place-items: center;
    background: rgb(8 10 24 / 0.55);
    padding: 1rem;
  }
  .dialog {
    width: min(26rem, 100%);
    text-align: center;
  }
  .code {
    font-size: 2.6rem;
    font-weight: 700;
    letter-spacing: 0.18em;
    margin: 1rem 0 1.25rem;
    font-variant-numeric: tabular-nums;
    background: linear-gradient(90deg, var(--accent), var(--accent-2));
    -webkit-background-clip: text;
    background-clip: text;
    color: transparent;
  }
  .actions {
    justify-content: center;
  }
</style>
