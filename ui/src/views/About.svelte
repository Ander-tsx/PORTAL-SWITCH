<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import { DONATE_URL, REPO_URL, SPONSORS_URL } from '../lib/api'
  import { app } from '../lib/store.svelte'

  const t = $derived(app.t)
</script>

<div class="stack">
  <section class="card">
    <h2>{t('aboutTitle')}</h2>
    <p>{t('aboutBody')}</p>
    <p class="muted small">{t('version')} {app.version} · {t('license')}</p>
    <p class="small"><a href={REPO_URL} target="_blank" rel="noopener noreferrer">{t('source')}</a></p>
  </section>

  <!-- Deliberately a quiet card: no pop-ups, no nagging. -->
  <section class="card support">
    <h2>{t('supportTitle')}</h2>
    <p class="muted">{t('supportBody')}</p>
    <div class="row wrap">
      <a class="button coffee" href={DONATE_URL} target="_blank" rel="noopener noreferrer">☕ {t('coffee')}</a>
      <a class="button" href={SPONSORS_URL} target="_blank" rel="noopener noreferrer">{t('sponsors')}</a>
    </div>
  </section>
</div>

<style>
  .button {
    display: inline-block;
    padding: 0.55rem 1rem;
    border-radius: 10px;
    border: 1px solid var(--line);
    background: var(--surface);
    color: var(--fg);
    text-decoration: none;
  }
  .button.coffee {
    background: var(--coffee);
    color: var(--coffee-fg);
    border: 0;
    font-weight: 600;
  }
  a {
    color: var(--accent);
  }
  .button:hover {
    border-color: var(--accent);
  }
</style>
