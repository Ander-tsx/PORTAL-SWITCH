// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

import { describe, expect, it } from 'vitest'
import { bounds, isConnected, overlaps, snap, type Box } from './layout'

const mac: Box = { x: 0, y: 0, width: 1920, height: 1080 }

describe('snap', () => {
  it('glues to the right edge of a neighbour when close', () => {
    const moving: Box = { x: 1950, y: 40, width: 1440, height: 900 }
    expect(snap(moving, [mac], 60)).toEqual({ x: 1920, y: 0 }) // also aligns the top edge
  })

  it('leaves a far-away screen alone', () => {
    const moving: Box = { x: 2600, y: 700, width: 1000, height: 600 }
    expect(snap(moving, [mac], 60)).toEqual({ x: 2600, y: 700 })
  })

  it('glues below and centres horizontally', () => {
    const moving: Box = { x: 400, y: 1100, width: 1120, height: 700 }
    expect(snap(moving, [mac], 60)).toEqual({ x: 400, y: 1080 })
    const nearCentre: Box = { x: 770, y: 1095, width: 380, height: 300 }
    expect(snap(nearCentre, [mac], 60)).toEqual({ x: 770, y: 1080 })
    const centred: Box = { x: 765, y: 1085, width: 390, height: 300 }
    expect(snap(centred, [mac], 60)?.x).toBe(765) // already centred (960 - 195)
  })

  it('refuses positions that overlap another screen', () => {
    expect(snap({ x: 100, y: 100, width: 500, height: 500 }, [mac], 20)).toBeNull()
  })
})

describe('layout helpers', () => {
  it('detects overlap only for real area', () => {
    expect(overlaps(mac, { x: 1920, y: 0, width: 10, height: 10 })).toBe(false)
    expect(overlaps(mac, { x: 1919, y: 0, width: 10, height: 10 })).toBe(true)
  })

  it('knows whether screens connect', () => {
    const right: Box = { x: 1920, y: 100, width: 800, height: 600 }
    const far: Box = { x: 5000, y: 0, width: 800, height: 600 }
    expect(isConnected([mac, right])).toBe(true)
    expect(isConnected([mac, right, far])).toBe(false)
    expect(isConnected([mac])).toBe(true)
  })

  it('computes bounds', () => {
    expect(bounds([mac, { x: -500, y: 200, width: 500, height: 2000 }])).toEqual({
      x: -500,
      y: 0,
      width: 2420,
      height: 2200,
    })
  })
})
