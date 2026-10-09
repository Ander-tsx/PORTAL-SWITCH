// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Screen layout: where machines sit on the canvas and how the cursor travels between them.
//!
//! Each machine is one rectangle (its virtual-desktop bounding box) on a shared canvas. Links
//! are *derived*: two rectangles that touch along an edge are neighbours over their overlap.
//! The canvas coordinate is continuous across a crossing, which is the same as mapping the
//! fractional position along the shared edge (spec 01).

use std::fmt;

/// Stable identity of a machine (`UUIDv4` bytes, `Hello::machine_id`).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MachineId(pub [u8; 16]);

impl fmt::Debug for MachineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MachineId(")?;
        for b in &self.0[..4] {
            write!(f, "{b:02x}")?;
        }
        write!(f, "…)")
    }
}

/// Direction of travel / side of a screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The edge a cursor arrives through when it leaves another screen through `self`.
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

/// A point in a machine's local pixel space (or on the canvas, by context).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// An axis-aligned rectangle; `width` and `height` are always > 0 in a valid layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// One past the last column, as `i64` to avoid overflow at the extremes.
    #[must_use]
    pub const fn right(&self) -> i64 {
        self.x as i64 + self.width as i64
    }

    /// One past the last row.
    #[must_use]
    pub const fn bottom(&self) -> i64 {
        self.y as i64 + self.height as i64
    }

    fn overlaps(&self, other: &Self) -> bool {
        i64::from(self.x) < other.right()
            && i64::from(other.x) < self.right()
            && i64::from(self.y) < other.bottom()
            && i64::from(other.y) < self.bottom()
    }
}

/// One machine on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Screen {
    pub id: MachineId,
    pub rect: Rect,
    /// Whether the machine is currently linked. Absent machines are skipped over.
    pub connected: bool,
}

/// Why a layout was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// A screen has zero width or height.
    EmptyScreen(MachineId),
    /// Two screens occupy the same canvas area.
    Overlap(MachineId, MachineId),
    /// The same machine appears twice.
    Duplicate(MachineId),
    /// More machines than the product limit.
    TooManyScreens,
}

/// Product limit on machines in one layout.
pub const MAX_SCREENS: usize = 5;

/// Result of moving the cursor, see [`Layout::route`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Still inside the starting screen.
    Stay(Point),
    /// Crossed onto another (connected) screen; `to` is in that screen's local coordinates.
    Cross {
        machine: MachineId,
        to: Point,
        edge: Edge,
    },
    /// Nowhere to go in that direction; the position was clamped into the starting screen.
    Blocked { clamped: Point, edge: Edge },
}

/// An immutable, validated arrangement of screens.
#[derive(Debug, Clone, Default)]
pub struct Layout {
    screens: Vec<Screen>,
}

impl Layout {
    /// Validate and build a layout.
    ///
    /// # Errors
    /// See [`LayoutError`].
    pub fn new(screens: Vec<Screen>) -> Result<Self, LayoutError> {
        if screens.len() > MAX_SCREENS {
            return Err(LayoutError::TooManyScreens);
        }
        for (i, a) in screens.iter().enumerate() {
            if a.rect.width == 0 || a.rect.height == 0 {
                return Err(LayoutError::EmptyScreen(a.id));
            }
            for b in &screens[i + 1..] {
                if a.id == b.id {
                    return Err(LayoutError::Duplicate(a.id));
                }
                if a.rect.overlaps(&b.rect) {
                    return Err(LayoutError::Overlap(a.id, b.id));
                }
            }
        }
        Ok(Self { screens })
    }

    #[must_use]
    pub fn screens(&self) -> &[Screen] {
        &self.screens
    }

    #[must_use]
    pub fn screen(&self, id: MachineId) -> Option<&Screen> {
        self.screens.iter().find(|s| s.id == id)
    }

    /// Copy of the layout with a machine marked connected or absent.
    #[must_use]
    pub fn with_connected(&self, id: MachineId, connected: bool) -> Self {
        let mut next = self.clone();
        if let Some(s) = next.screens.iter_mut().find(|s| s.id == id) {
            s.connected = connected;
        }
        next
    }

    /// Resolve a cursor position `p` (local coordinates of `from`, possibly outside it).
    ///
    /// Crossing is resolved one axis at a time (horizontal first). Absent machines in the way are
    /// skipped; a jump larger than a whole screen continues across the following screens. The
    /// result is always inside a connected screen.
    #[must_use]
    pub fn route(&self, from: MachineId, p: Point) -> Route {
        let Some(start) = self.screen(from) else {
            return Route::Stay(p);
        };
        let (mut current, mut pos) = (*start, p);
        let mut first_edge = None;
        for _ in 0..=MAX_SCREENS * 2 {
            let Some(edge) = exit_edge(&current.rect, pos) else {
                return match first_edge {
                    None => Route::Stay(pos),
                    Some(edge) => Route::Cross {
                        machine: current.id,
                        to: pos,
                        edge,
                    },
                };
            };
            first_edge.get_or_insert(edge);
            if let Some((next, landed)) = self.step(&current, pos, edge) {
                (current, pos) = (next, landed);
                continue;
            }
            // Blocked: nothing (connected) beyond. Clamp inside the screen we are in.
            let clamped = clamp(&current.rect, pos);
            return match first_edge {
                Some(e) if current.id != from => Route::Cross {
                    machine: current.id,
                    to: clamped,
                    edge: e,
                },
                _ => Route::Blocked { clamped, edge },
            };
        }
        Route::Blocked {
            clamped: clamp(&start.rect, p),
            edge: first_edge.unwrap_or(Edge::Right),
        }
    }

    /// One hop from `current` through `edge`: the next connected screen and the position in it.
    fn step(&self, current: &Screen, pos: Point, edge: Edge) -> Option<(Screen, Point)> {
        let r = current.rect;
        // Canvas coordinate along the crossed edge.
        let (along_canvas, mut boundary) = match edge {
            Edge::Left | Edge::Right => (
                i64::from(r.y) + i64::from(pos.y).clamp(0, i64::from(r.height) - 1),
                if edge == Edge::Left {
                    i64::from(r.x)
                } else {
                    r.right()
                },
            ),
            Edge::Top | Edge::Bottom => (
                i64::from(r.x) + i64::from(pos.x).clamp(0, i64::from(r.width) - 1),
                if edge == Edge::Top {
                    i64::from(r.y)
                } else {
                    r.bottom()
                },
            ),
        };
        // Overshoot beyond the edge, in pixels (≥ 0).
        let overshoot = match edge {
            Edge::Left => -i64::from(pos.x) - 1,
            Edge::Right => i64::from(pos.x) - i64::from(r.width),
            Edge::Top => -i64::from(pos.y) - 1,
            Edge::Bottom => i64::from(pos.y) - i64::from(r.height),
        }
        .max(0);

        for _ in 0..=self.screens.len() {
            let next = self
                .screens
                .iter()
                .find(|s| touches(&s.rect, edge, boundary, along_canvas))?;
            if next.connected {
                let landed = land(&next.rect, edge, along_canvas, overshoot);
                return Some((*next, landed));
            }
            // Absent: look past it, same position along the edge.
            boundary = match edge {
                Edge::Left => i64::from(next.rect.x),
                Edge::Right => next.rect.right(),
                Edge::Top => i64::from(next.rect.y),
                Edge::Bottom => next.rect.bottom(),
            };
        }
        None
    }
}

/// Which edge of `rect` the local position `p` has passed, horizontal first.
fn exit_edge(rect: &Rect, p: Point) -> Option<Edge> {
    if p.x < 0 {
        Some(Edge::Left)
    } else if i64::from(p.x) >= i64::from(rect.width) {
        Some(Edge::Right)
    } else if p.y < 0 {
        Some(Edge::Top)
    } else if i64::from(p.y) >= i64::from(rect.height) {
        Some(Edge::Bottom)
    } else {
        None
    }
}

fn clamp(rect: &Rect, p: Point) -> Point {
    // Dimensions fit i32 for any realistic screen; saturate defensively.
    let max_x = i32::try_from(i64::from(rect.width) - 1).unwrap_or(i32::MAX);
    let max_y = i32::try_from(i64::from(rect.height) - 1).unwrap_or(i32::MAX);
    Point {
        x: p.x.clamp(0, max_x),
        y: p.y.clamp(0, max_y),
    }
}

/// Does `rect` sit on the far side of `boundary` (when leaving through `edge`) and span `along`?
fn touches(rect: &Rect, edge: Edge, boundary: i64, along: i64) -> bool {
    match edge {
        // Leaving left: the neighbour's right side meets the boundary.
        Edge::Left => {
            rect.right() == boundary && i64::from(rect.y) <= along && along < rect.bottom()
        }
        Edge::Right => {
            i64::from(rect.x) == boundary && i64::from(rect.y) <= along && along < rect.bottom()
        }
        Edge::Top => {
            rect.bottom() == boundary && i64::from(rect.x) <= along && along < rect.right()
        }
        Edge::Bottom => {
            i64::from(rect.y) == boundary && i64::from(rect.x) <= along && along < rect.right()
        }
    }
}

/// Local position in `rect` for a cursor arriving through the side opposite to `edge`.
fn land(rect: &Rect, edge: Edge, along_canvas: i64, overshoot: i64) -> Point {
    let lx = |v: i64| i32::try_from(v).unwrap_or(i32::MAX);
    match edge {
        Edge::Right => Point {
            x: lx(overshoot),
            y: lx(along_canvas - i64::from(rect.y)),
        },
        Edge::Left => Point {
            x: lx(i64::from(rect.width) - 1 - overshoot),
            y: lx(along_canvas - i64::from(rect.y)),
        },
        Edge::Bottom => Point {
            x: lx(along_canvas - i64::from(rect.x)),
            y: lx(overshoot),
        },
        Edge::Top => Point {
            x: lx(along_canvas - i64::from(rect.x)),
            y: lx(i64::from(rect.height) - 1 - overshoot),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u8) -> MachineId {
        MachineId([n; 16])
    }

    fn screen(n: u8, rect: (i32, i32, u32, u32), connected: bool) -> Screen {
        Screen {
            id: id(n),
            rect: Rect::new(rect.0, rect.1, rect.2, rect.3),
            connected,
        }
    }

    fn row(connected: [bool; 3]) -> Layout {
        // A(1920x1080) | B(1280x720, top-aligned) | C(1920x1080)
        Layout::new(vec![
            screen(1, (0, 0, 1920, 1080), connected[0]),
            screen(2, (1920, 0, 1280, 720), connected[1]),
            screen(3, (3200, 0, 1920, 1080), connected[2]),
        ])
        .unwrap()
    }

    #[test]
    fn stays_inside() {
        let l = row([true; 3]);
        assert_eq!(
            l.route(id(1), Point { x: 10, y: 10 }),
            Route::Stay(Point { x: 10, y: 10 })
        );
    }

    #[test]
    fn crosses_right_keeping_canvas_row() {
        let l = row([true; 3]);
        let r = l.route(id(1), Point { x: 1920, y: 300 });
        assert_eq!(
            r,
            Route::Cross {
                machine: id(2),
                to: Point { x: 0, y: 300 },
                edge: Edge::Right
            }
        );
    }

    #[test]
    fn partial_edge_blocks_where_no_neighbour() {
        // y=900 is below B (height 720): no screen to the right at that row.
        let l = row([true; 3]);
        let r = l.route(id(1), Point { x: 1920, y: 900 });
        assert_eq!(
            r,
            Route::Blocked {
                clamped: Point { x: 1919, y: 900 },
                edge: Edge::Right
            }
        );
    }

    #[test]
    fn absent_middle_machine_is_skipped_keeping_row() {
        let l = row([true, false, true]);
        let r = l.route(id(1), Point { x: 1920, y: 500 });
        assert_eq!(
            r,
            Route::Cross {
                machine: id(3),
                to: Point { x: 0, y: 500 },
                edge: Edge::Right
            }
        );
    }

    #[test]
    fn skipping_needs_a_screen_at_that_row() {
        // B absent and row 900 only exists on A and C, but B's gap breaks the chain at row 900.
        let l = row([true, false, true]);
        assert!(matches!(
            l.route(id(1), Point { x: 1920, y: 900 }),
            Route::Blocked { .. }
        ));
    }

    #[test]
    fn large_jump_crosses_several_screens() {
        let l = row([true; 3]);
        // 1920 + 1280 = 3200 → overshoot 1280+5 past A's right edge lands 5px into C.
        let r = l.route(
            id(1),
            Point {
                x: 1920 + 1280 + 5,
                y: 100,
            },
        );
        assert_eq!(
            r,
            Route::Cross {
                machine: id(3),
                to: Point { x: 5, y: 100 },
                edge: Edge::Right
            }
        );
    }

    #[test]
    fn leftmost_edge_is_blocked() {
        let l = row([true; 3]);
        assert_eq!(
            l.route(id(1), Point { x: -3, y: 40 }),
            Route::Blocked {
                clamped: Point { x: 0, y: 40 },
                edge: Edge::Left
            }
        );
    }

    #[test]
    fn vertical_neighbours() {
        let l = Layout::new(vec![
            screen(1, (0, 0, 1000, 800), true),
            screen(2, (0, 800, 1000, 600), true),
        ])
        .unwrap();
        assert_eq!(
            l.route(id(1), Point { x: 250, y: 800 }),
            Route::Cross {
                machine: id(2),
                to: Point { x: 250, y: 0 },
                edge: Edge::Bottom
            }
        );
        assert_eq!(
            l.route(id(2), Point { x: 250, y: -1 }),
            Route::Cross {
                machine: id(1),
                to: Point { x: 250, y: 799 },
                edge: Edge::Top
            }
        );
    }

    #[test]
    fn crossing_left_into_a_smaller_screen_lands_inside() {
        let l = row([true; 3]);
        let r = l.route(id(3), Point { x: -1, y: 719 });
        assert_eq!(
            r,
            Route::Cross {
                machine: id(2),
                to: Point { x: 1279, y: 719 },
                edge: Edge::Left
            }
        );
    }

    #[test]
    fn rejects_overlap_empty_duplicate_and_too_many() {
        assert_eq!(
            Layout::new(vec![
                screen(1, (0, 0, 100, 100), true),
                screen(2, (50, 50, 100, 100), true)
            ])
            .unwrap_err(),
            LayoutError::Overlap(id(1), id(2))
        );
        assert_eq!(
            Layout::new(vec![screen(1, (0, 0, 0, 100), true)]).unwrap_err(),
            LayoutError::EmptyScreen(id(1))
        );
        assert_eq!(
            Layout::new(vec![
                screen(1, (0, 0, 10, 10), true),
                screen(1, (20, 0, 10, 10), true)
            ])
            .unwrap_err(),
            LayoutError::Duplicate(id(1))
        );
        let many = (0..6)
            .map(|i| screen(i, (i32::from(i) * 100, 0, 100, 100), true))
            .collect();
        assert_eq!(Layout::new(many).unwrap_err(), LayoutError::TooManyScreens);
    }

    #[test]
    fn route_result_is_always_inside_a_connected_screen() {
        let layouts = [
            row([true; 3]),
            row([true, false, true]),
            row([true, true, false]),
        ];
        let mut state = 12345_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for layout in &layouts {
            for _ in 0..20_000 {
                let from = id(1 + (next() % 3) as u8);
                if !layout.screen(from).unwrap().connected {
                    continue;
                }
                let p = Point {
                    x: (next() % 14_000) as i32 - 4_000,
                    y: (next() % 4_000) as i32 - 1_500,
                };
                let (machine, pt) = match layout.route(from, p) {
                    Route::Stay(pt) => (from, pt),
                    Route::Cross { machine, to, .. } => (machine, to),
                    Route::Blocked { clamped, .. } => (from, clamped),
                };
                let s = layout.screen(machine).unwrap();
                assert!(s.connected, "landed on an absent screen");
                assert!(
                    pt.x >= 0 && i64::from(pt.x) < i64::from(s.rect.width),
                    "{p:?} -> {pt:?}"
                );
                assert!(
                    pt.y >= 0 && i64::from(pt.y) < i64::from(s.rect.height),
                    "{p:?} -> {pt:?}"
                );
            }
        }
    }
}
