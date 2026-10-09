<!-- SPDX-License-Identifier: Apache-2.0 -->
<!-- Copyright 2026 The PortalSwitch Authors -->
<script lang="ts">
  import type { Config, ScreenConfig } from '../lib/api'
  import { type Box, bounds, isConnected, snap } from '../lib/layout'
  import { app } from '../lib/store.svelte'

  const t = $derived(app.t)
  let screens = $state<ScreenConfig[]>(app.config ? structuredClone($state.snapshot(app.config.layout)) : [])
  let canvas: HTMLDivElement | undefined = $state()
  let canvasWidth = $state(760)
  let drag = $state<{ index: number; startX: number; startY: number; origX: number; origY: number } | undefined>()
  let dirty = $state(false)

  const PADDING = 1200 // canvas units of free space around the screens, so there is room to drag
  const boxes = $derived(screens.map((s): Box => ({ x: s.x, y: s.y, width: s.width, height: s.height })))
  const view = $derived.by(() => {
    const b = bounds(boxes)
    return { x: b.x - PADDING, y: b.y - PADDING * 0.6, width: b.width + PADDING * 2, height: b.height + PADDING * 1.2 }
  })
  const scale = $derived(canvasWidth / view.width)
  const connected = $derived(isConnected(boxes))

  function down(e: PointerEvent, index: number) {
    ;(e.currentTarget as HTMLElement).setPointerCapture(e.pointerId)
    drag = { index, startX: e.clientX, startY: e.clientY, origX: screens[index].x, origY: screens[index].y }
  }

  function move(e: PointerEvent) {
    if (!drag) return
    const me = screens[drag.index]
    const wanted: Box = {
      x: drag.origX + (e.clientX - drag.startX) / scale,
      y: drag.origY + (e.clientY - drag.startY) / scale,
      width: me.width,
      height: me.height,
    }
    const others = boxes.filter((_, i) => i !== drag!.index)
    const placed = snap(wanted, others, 40 / scale)
    if (placed) {
      me.x = placed.x
      me.y = placed.y
      dirty = true
    }
  }

  async function save() {
    if (!app.config) return
    const next: Config = { ...$state.snapshot(app.config), layout: $state.snapshot(screens) as ScreenConfig[] }
    const result = await app.run(() => app.backend.setConfig(next))
    if (result) {
      await app.reloadConfig()
      dirty = false
      app.flash(t('layoutSaved'))
    }
  }
</script>

<section class="card stack">
  <div>
    <h2>{t('layoutTitle')}</h2>
    <p class="muted">{t('layoutHelp')}</p>
  </div>

  <div class="canvas" bind:this={canvas} bind:clientWidth={canvasWidth} style:height="{view.height * scale}px">
    {#each screens as screen, i (screen.machine_id)}
      <div
        class="screen"
        class:me={screen.machine_id === app.status?.machine}
        class:active={screen.machine_id === app.status?.active}
        role="button"
        tabindex="0"
        aria-label={app.nameOf(screen.machine_id)}
        style:left="{(screen.x - view.x) * scale}px"
        style:top="{(screen.y - view.y) * scale}px"
        style:width="{screen.width * scale}px"
        style:height="{screen.height * scale}px"
        onpointerdown={(e) => down(e, i)}
        onpointermove={move}
        onpointerup={() => (drag = undefined)}
        onpointercancel={() => (drag = undefined)}
      >
        <strong>{app.nameOf(screen.machine_id)}</strong>
        <span class="size">{screen.width}×{screen.height}</span>
        {#if screen.machine_id === app.status?.machine}<span class="badge accent">{t('thisComputer')}</span>{/if}
      </div>
    {/each}
  </div>

  {#if !connected}<p class="badge" style="color: var(--warn)">{t('layoutDisconnected')}</p>{/if}
  <div><button class="primary" disabled={!dirty || !connected} onclick={save}>{t('layoutSave')}</button></div>
</section>

<style>
  .canvas {
    position: relative;
    width: 100%;
    background-image: radial-gradient(var(--line) 1px, transparent 1px);
    background-size: 22px 22px;
    border: 1px dashed var(--line);
    border-radius: 12px;
    overflow: hidden;
    touch-action: none;
  }
  .screen {
    position: absolute;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.25rem;
    text-align: center;
    border-radius: 8px;
    border: 2px solid var(--line);
    background: var(--surface);
    box-shadow: var(--shadow);
    cursor: grab;
    user-select: none;
    overflow: hidden;
    padding: 0.25rem;
  }
  .screen:active {
    cursor: grabbing;
  }
  .screen.me {
    border-color: var(--accent);
  }
  .screen.active {
    background: color-mix(in srgb, var(--accent) 10%, var(--surface));
  }
  .size {
    color: var(--muted);
    font-size: 0.75rem;
  }
</style>
