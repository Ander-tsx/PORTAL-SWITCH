// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Conditions under which an edge crossing is allowed (spec 01, "Switch conditions").
//!
//! The gate is a pure state machine: time is passed in as monotonic microseconds, so tests are
//! deterministic and the daemon decides how to schedule wake-ups ([`SwitchGate::deadline`]).

use crate::layout::{Edge, Point};

/// Monotonic time in microseconds.
pub type Micros = u64;

/// Modifier groups, independent of left/right side. Bit layout matches protocol §8.1 pairs.
pub mod group {
    pub const SHIFT: u32 = 0b11;
    pub const CONTROL: u32 = 0b11 << 2;
    pub const ALT: u32 = 0b11 << 4;
    pub const META: u32 = 0b11 << 6;
}

/// Screen corners that can be "dead" (switching suppressed near them).
#[allow(clippy::struct_excessive_bools)] // one flag per physical corner
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Corners {
    pub top_left: bool,
    pub top_right: bool,
    pub bottom_left: bool,
    pub bottom_right: bool,
}

/// User-facing switching options.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SwitchConfig {
    /// Time the cursor must stay pushed against the edge; 0 = immediate.
    pub delay_us: Micros,
    /// Window for the second edge hit; 0 = double-tap disabled.
    pub double_tap_us: Micros,
    /// Distance the cursor must move inward between the two taps.
    pub tap_zone_px: u32,
    /// Size of a dead corner, measured along both edges.
    pub corner_px: u32,
    pub dead_corners: Corners,
    /// Modifier groups ([`group`]) that must all be held to switch; 0 = none required.
    pub required_modifiers: u32,
}

/// Outcome of a crossing attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny,
    /// Keep pushing; the switch fires at this time (see [`SwitchGate::poll`]).
    Pending {
        until: Micros,
    },
}

/// A delayed switch that became due.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueSwitch {
    pub edge: Edge,
    /// Position on the edge when the wait started.
    pub at: Point,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Tap {
    #[default]
    None,
    /// Second tap accepted for `edge`; stays until the switch happens or the cursor backs off.
    Accepted { edge: Edge },
    /// First edge hit seen at `since`; waiting for inward motion and a second hit.
    First {
        edge: Edge,
        since: Micros,
        armed: bool,
        last_dist: u32,
        run: u8,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Push {
    edge: Edge,
    since: Micros,
    at: Point,
}

/// Per-host switching state (delay and double-tap progress).
#[derive(Debug, Clone, Copy, Default)]
pub struct SwitchGate {
    tap: Tap,
    push: Option<Push>,
}

impl SwitchGate {
    /// Forget all pending delay / double-tap progress (target edge changed, screen switched, …).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The cursor tried to leave through `edge` at `at` (a point on the edge, local coordinates
    /// of a `size` screen) while `modifiers` are held and the cursor is not locked.
    pub fn attempt(
        &mut self,
        cfg: &SwitchConfig,
        now: Micros,
        edge: Edge,
        at: Point,
        size: (u32, u32),
        modifiers: u32,
    ) -> Decision {
        if cfg.required_modifiers != 0 && !has_all_groups(modifiers, cfg.required_modifiers) {
            self.reset();
            return Decision::Deny;
        }
        if in_dead_corner(cfg, at, size) {
            self.reset();
            return Decision::Deny;
        }

        // A different target edge starts everything over.
        if self.push.is_some_and(|p| p.edge != edge) {
            self.push = None;
        }
        if matches!(self.tap, Tap::First { edge: e, .. } | Tap::Accepted { edge: e } if e != edge) {
            self.tap = Tap::None;
        }

        // Double-tap is checked first.
        if cfg.double_tap_us > 0 {
            match self.tap {
                Tap::First {
                    since, armed: true, ..
                } if now.saturating_sub(since) <= cfg.double_tap_us => {
                    self.tap = Tap::Accepted { edge }; // fall through to the delay check
                }
                Tap::Accepted { .. } => {}
                Tap::First { since, .. } if now.saturating_sub(since) <= cfg.double_tap_us => {
                    return Decision::Deny; // still the first tap (not yet moved inward)
                }
                _ => {
                    self.tap = Tap::First {
                        edge,
                        since: now,
                        armed: false,
                        last_dist: 0,
                        run: 0,
                    };
                    return Decision::Deny;
                }
            }
        }

        if cfg.delay_us == 0 {
            self.push = None;
            self.tap = Tap::None;
            return Decision::Allow;
        }
        let push = *self.push.get_or_insert(Push {
            edge,
            since: now,
            at,
        });
        let due = push.since.saturating_add(cfg.delay_us);
        if now >= due {
            self.push = None;
            self.tap = Tap::None;
            Decision::Allow
        } else {
            Decision::Pending { until: due }
        }
    }

    /// The cursor moved without trying to cross; `dist_from_edge` is its distance from the edge
    /// it last pushed against, if any. Cancels a delayed switch and arms the second tap.
    pub fn moved_inside(&mut self, cfg: &SwitchConfig, now: Micros, dist_from_edge: u32) {
        self.push = None;
        if let Tap::First {
            edge,
            since,
            armed,
            last_dist,
            run,
        } = self.tap
        {
            if cfg.double_tap_us == 0 || now.saturating_sub(since) > cfg.double_tap_us {
                self.tap = Tap::None;
                return;
            }
            // Only sustained inward motion arms the tap: filters the spurious reverse motion
            // some systems emit at screen borders.
            let run = if dist_from_edge > last_dist {
                run.saturating_add(1)
            } else {
                0
            };
            let armed = armed || (dist_from_edge >= cfg.tap_zone_px && run >= 2);
            self.tap = Tap::First {
                edge,
                since,
                armed,
                last_dist: dist_from_edge,
                run,
            };
        }
    }

    /// When a delayed switch will become due, if one is pending.
    #[must_use]
    pub fn deadline(&self, cfg: &SwitchConfig) -> Option<Micros> {
        self.push.map(|p| p.since.saturating_add(cfg.delay_us))
    }

    /// Fire a delayed switch whose time has come, using the position where the wait started.
    pub fn poll(&mut self, cfg: &SwitchConfig, now: Micros) -> Option<DueSwitch> {
        let push = self.push?;
        if now >= push.since.saturating_add(cfg.delay_us) {
            self.push = None;
            self.tap = Tap::None;
            Some(DueSwitch {
                edge: push.edge,
                at: push.at,
            })
        } else {
            None
        }
    }
}

fn has_all_groups(modifiers: u32, required: u32) -> bool {
    [group::SHIFT, group::CONTROL, group::ALT, group::META]
        .into_iter()
        .filter(|g| required & g != 0)
        .all(|g| modifiers & g != 0)
}

fn in_dead_corner(cfg: &SwitchConfig, at: Point, size: (u32, u32)) -> bool {
    let reach = i64::from(cfg.corner_px);
    if reach == 0 {
        return false;
    }
    let (px, py) = (i64::from(at.x), i64::from(at.y));
    let (width, height) = (i64::from(size.0), i64::from(size.1));
    let near_left = px < reach;
    let near_right = px >= width - reach;
    let near_top = py < reach;
    let near_bottom = py >= height - reach;
    let dead = &cfg.dead_corners;
    (dead.top_left && near_left && near_top)
        || (dead.top_right && near_right && near_top)
        || (dead.bottom_left && near_left && near_bottom)
        || (dead.bottom_right && near_right && near_bottom)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: (u32, u32) = (1920, 1080);
    const MID: Point = Point { x: 1919, y: 540 };

    fn attempt(g: &mut SwitchGate, cfg: &SwitchConfig, now: Micros) -> Decision {
        g.attempt(cfg, now, Edge::Right, MID, SIZE, 0)
    }

    #[test]
    fn default_config_allows_immediately() {
        let mut g = SwitchGate::default();
        assert_eq!(
            attempt(&mut g, &SwitchConfig::default(), 0),
            Decision::Allow
        );
    }

    #[test]
    fn dead_corner_denies_only_inside_the_corner() {
        let cfg = SwitchConfig {
            corner_px: 50,
            dead_corners: Corners {
                top_right: true,
                ..Corners::default()
            },
            ..SwitchConfig::default()
        };
        let mut g = SwitchGate::default();
        let corner = Point { x: 1919, y: 10 };
        let outside = Point { x: 1919, y: 60 };
        assert_eq!(
            g.attempt(&cfg, 0, Edge::Right, corner, SIZE, 0),
            Decision::Deny
        );
        assert_eq!(
            g.attempt(&cfg, 0, Edge::Right, outside, SIZE, 0),
            Decision::Allow
        );
        // A corner that is not dead still switches.
        let other = Point { x: 1919, y: 1070 };
        assert_eq!(
            g.attempt(&cfg, 0, Edge::Right, other, SIZE, 0),
            Decision::Allow
        );
    }

    #[test]
    fn required_modifiers_must_all_be_held() {
        let cfg = SwitchConfig {
            required_modifiers: group::SHIFT | group::CONTROL,
            ..SwitchConfig::default()
        };
        let mut g = SwitchGate::default();
        assert_eq!(
            g.attempt(&cfg, 0, Edge::Right, MID, SIZE, 0b1),
            Decision::Deny
        );
        // Right Shift (bit 1) + left Control (bit 2) satisfy both groups.
        assert_eq!(
            g.attempt(&cfg, 0, Edge::Right, MID, SIZE, 0b110),
            Decision::Allow
        );
    }

    #[test]
    fn delay_waits_then_allows_and_cancels_when_leaving_the_edge() {
        let cfg = SwitchConfig {
            delay_us: 500_000,
            ..SwitchConfig::default()
        };
        let mut g = SwitchGate::default();
        assert_eq!(
            attempt(&mut g, &cfg, 0),
            Decision::Pending { until: 500_000 }
        );
        assert_eq!(
            attempt(&mut g, &cfg, 200_000),
            Decision::Pending { until: 500_000 }
        );
        g.moved_inside(&cfg, 300_000, 30); // backs off: wait cancelled
        assert_eq!(
            attempt(&mut g, &cfg, 400_000),
            Decision::Pending { until: 900_000 }
        );
        assert_eq!(attempt(&mut g, &cfg, 900_000), Decision::Allow);
    }

    #[test]
    fn delay_fires_on_poll_at_the_original_position() {
        let cfg = SwitchConfig {
            delay_us: 300_000,
            ..SwitchConfig::default()
        };
        let mut g = SwitchGate::default();
        assert!(matches!(
            attempt(&mut g, &cfg, 1_000),
            Decision::Pending { .. }
        ));
        assert_eq!(g.deadline(&cfg), Some(301_000));
        assert_eq!(g.poll(&cfg, 300_999), None);
        assert_eq!(
            g.poll(&cfg, 301_000),
            Some(DueSwitch {
                edge: Edge::Right,
                at: MID
            })
        );
        assert_eq!(g.deadline(&cfg), None);
    }

    #[test]
    fn changing_edge_resets_the_delay() {
        let cfg = SwitchConfig {
            delay_us: 500_000,
            ..SwitchConfig::default()
        };
        let mut g = SwitchGate::default();
        assert!(matches!(attempt(&mut g, &cfg, 0), Decision::Pending { .. }));
        let d = g.attempt(&cfg, 400_000, Edge::Left, Point { x: 0, y: 5 }, SIZE, 0);
        assert_eq!(d, Decision::Pending { until: 900_000 });
    }

    fn double_tap_cfg() -> SwitchConfig {
        SwitchConfig {
            double_tap_us: 300_000,
            tap_zone_px: 30,
            ..SwitchConfig::default()
        }
    }

    fn move_in(g: &mut SwitchGate, cfg: &SwitchConfig, now: Micros) {
        for (i, d) in [10, 20, 35, 50].into_iter().enumerate() {
            g.moved_inside(cfg, now + i as u64 * 1_000, d);
        }
    }

    #[test]
    fn double_tap_within_window_switches() {
        let cfg = double_tap_cfg();
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        move_in(&mut g, &cfg, 50_000);
        assert_eq!(attempt(&mut g, &cfg, 200_000), Decision::Allow);
    }

    #[test]
    fn slow_second_tap_does_not_switch() {
        let cfg = double_tap_cfg();
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        move_in(&mut g, &cfg, 50_000);
        assert_eq!(attempt(&mut g, &cfg, 400_000), Decision::Deny); // restarts as a first tap
        move_in(&mut g, &cfg, 410_000);
        assert_eq!(attempt(&mut g, &cfg, 500_000), Decision::Allow);
    }

    #[test]
    fn second_tap_without_moving_inward_does_not_switch() {
        let cfg = double_tap_cfg();
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        assert_eq!(attempt(&mut g, &cfg, 100_000), Decision::Deny);
    }

    #[test]
    fn single_spurious_reverse_sample_does_not_arm_the_tap() {
        let cfg = double_tap_cfg();
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        g.moved_inside(&cfg, 1_000, 40); // one jump past the zone, not sustained
        assert_eq!(attempt(&mut g, &cfg, 100_000), Decision::Deny);
    }

    #[test]
    fn direction_change_resets_double_tap() {
        let cfg = double_tap_cfg();
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        move_in(&mut g, &cfg, 10_000);
        let left = Point { x: 0, y: 500 };
        assert_eq!(
            g.attempt(&cfg, 100_000, Edge::Left, left, SIZE, 0),
            Decision::Deny
        );
    }

    #[test]
    fn double_tap_then_delay_combine() {
        let cfg = SwitchConfig {
            delay_us: 100_000,
            ..double_tap_cfg()
        };
        let mut g = SwitchGate::default();
        assert_eq!(attempt(&mut g, &cfg, 0), Decision::Deny);
        move_in(&mut g, &cfg, 10_000);
        assert_eq!(
            attempt(&mut g, &cfg, 150_000),
            Decision::Pending { until: 250_000 }
        );
        assert_eq!(attempt(&mut g, &cfg, 250_000), Decision::Allow);
    }
}
