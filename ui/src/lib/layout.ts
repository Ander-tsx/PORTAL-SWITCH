// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

export interface Box {
  x: number
  y: number
  width: number
  height: number
}

export const MAX_SCREENS = 5

export function overlaps(a: Box, b: Box): boolean {
  return a.x < b.x + b.width && b.x < a.x + a.width && a.y < b.y + b.height && b.y < a.y + a.height
}

/**
 * Snap `moving` (already at its dragged position) against the other screens: edges that are within
 * `threshold` canvas pixels glue together, and screens that touch also line up on a shared edge or
 * centre. Returns the adjusted position, or `null` if the result would overlap another screen.
 */
export function snap(moving: Box, others: Box[], threshold: number): { x: number; y: number } | null {
  let { x, y } = moving
  let bestDx = threshold + 1
  let bestDy = threshold + 1
  let dx = 0
  let dy = 0

  for (const o of others) {
    // Horizontal gluing: my left to their right, my right to their left.
    for (const delta of [o.x + o.width - x, o.x - (x + moving.width)]) {
      if (Math.abs(delta) < bestDx && verticalReach(moving, o, y, threshold)) {
        bestDx = Math.abs(delta)
        dx = delta
      }
    }
    // Vertical gluing.
    for (const delta of [o.y + o.height - y, o.y - (y + moving.height)]) {
      if (Math.abs(delta) < bestDy && horizontalReach(moving, o, x, threshold)) {
        bestDy = Math.abs(delta)
        dy = delta
      }
    }
  }
  if (bestDx <= threshold) x += dx
  if (bestDy <= threshold) y += dy

  // Once glued on one axis, align the other axis to edges / centres of the screen it touches.
  for (const o of others) {
    const placed = { ...moving, x, y }
    if (touchesHorizontally(placed, o)) {
      const candidates = [o.y - y, o.y + o.height - (y + moving.height), o.y + o.height / 2 - (y + moving.height / 2)]
      const best = candidates.reduce((a, b) => (Math.abs(b) < Math.abs(a) ? b : a))
      if (Math.abs(best) <= threshold) y += best
    } else if (touchesVertically(placed, o)) {
      const candidates = [o.x - x, o.x + o.width - (x + moving.width), o.x + o.width / 2 - (x + moving.width / 2)]
      const best = candidates.reduce((a, b) => (Math.abs(b) < Math.abs(a) ? b : a))
      if (Math.abs(best) <= threshold) x += best
    }
  }

  const result = { ...moving, x: Math.round(x), y: Math.round(y) }
  return others.some((o) => overlaps(result, o)) ? null : { x: result.x, y: result.y }
}

function verticalReach(a: Box, b: Box, y: number, slack: number): boolean {
  return y < b.y + b.height + slack && b.y < y + a.height + slack
}
function horizontalReach(a: Box, b: Box, x: number, slack: number): boolean {
  return x < b.x + b.width + slack && b.x < x + a.width + slack
}
function touchesHorizontally(a: Box, b: Box): boolean {
  return (a.x === b.x + b.width || a.x + a.width === b.x) && a.y < b.y + b.height && b.y < a.y + a.height
}
function touchesVertically(a: Box, b: Box): boolean {
  return (a.y === b.y + b.height || a.y + a.height === b.y) && a.x < b.x + b.width && b.x < a.x + a.width
}

/** Does every screen connect (by touching edges) to the others? Disconnected layouts cannot be crossed. */
export function isConnected(boxes: Box[]): boolean {
  if (boxes.length <= 1) return true
  const seen = new Set([0])
  const queue = [0]
  while (queue.length) {
    const i = queue.pop()!
    boxes.forEach((b, j) => {
      if (!seen.has(j) && (touchesHorizontally(boxes[i], b) || touchesVertically(boxes[i], b))) {
        seen.add(j)
        queue.push(j)
      }
    })
  }
  return seen.size === boxes.length
}

/** Bounding box of all screens. */
export function bounds(boxes: Box[]): Box {
  if (!boxes.length) return { x: 0, y: 0, width: 1, height: 1 }
  const left = Math.min(...boxes.map((b) => b.x))
  const top = Math.min(...boxes.map((b) => b.y))
  const right = Math.max(...boxes.map((b) => b.x + b.width))
  const bottom = Math.max(...boxes.map((b) => b.y + b.height))
  return { x: left, y: top, width: right - left, height: bottom - top }
}
