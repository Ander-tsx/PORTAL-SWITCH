// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Host-side routing state machine: which machine is active, where the cursor is, and where
//! every captured event goes (spec 01 and 03).
//!
//! The router is *sans-IO*. The daemon feeds it [`Event`]s with a monotonic timestamp and
//! executes the [`Command`]s it appends to the output vector. The return value of
//! [`Router::handle`] tells the platform capture layer whether the original input event must be
//! swallowed (not delivered to host applications).
//!
//! Safety invariant (property-tested in `tests/no_stuck_input.rs`): no key or button is ever
//! left pressed on any machine, including the host's own OS, after a switch, disconnect or
//! timeout.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::gate::{Decision, Micros, SwitchConfig, SwitchGate};
use crate::input::{InputEvent, KeyAction, KeyEvent};
use crate::layout::{Edge, Layout, MachineId, Point, Route};

/// How long the host waits for `LeaveAck` before releasing the peer forcibly.
pub const LEAVE_ACK_TIMEOUT_US: Micros = 250_000;

/// Something the daemon must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Start or stop capturing: while `swallow`, the host cursor is hidden/parked and pointer
    /// and keyboard events are withheld from host applications.
    Capture {
        swallow: bool,
    },
    /// Move the host's real cursor.
    WarpHost(Point),
    /// Synthesize presses on the host OS for physically held modifiers (it never saw them go down).
    HostPress {
        keys: Vec<KeyEvent>,
    },
    /// Synthesize releases on the host OS for inputs it saw go down but whose release was swallowed.
    HostRelease {
        keys: Vec<KeyEvent>,
        buttons: Vec<u8>,
    },
    Enter {
        to: MachineId,
        enter_seq: u32,
        pos: Point,
        toggles: u8,
        modifiers: u32,
    },
    Leave {
        to: MachineId,
        enter_seq: u32,
    },
    ReleaseAll {
        to: MachineId,
        enter_seq: u32,
    },
    Motion {
        to: MachineId,
        enter_seq: u32,
        dx: i32,
        dy: i32,
    },
    Button {
        to: MachineId,
        enter_seq: u32,
        button: u8,
        down: bool,
    },
    Wheel {
        to: MachineId,
        enter_seq: u32,
        dx: i16,
        dy: i16,
        flags: u8,
    },
    Key {
        to: MachineId,
        enter_seq: u32,
        event: KeyEvent,
    },
    /// The active machine changed (for the UI / tray).
    ActiveChanged {
        machine: MachineId,
    },
}

/// Input to the router.
#[derive(Debug, Clone)]
pub enum Event {
    Input(InputEvent),
    PeerConnected(MachineId),
    PeerDisconnected(MachineId),
    LeaveAck {
        from: MachineId,
        enter_seq: u32,
        ok: bool,
    },
    /// Time passed; call at least at [`Router::deadline`].
    Tick,
    /// Hotkey: jump straight to a machine, restoring its last cursor position.
    Jump(MachineId),
    /// Lock the cursor to the current screen.
    SetLocked(bool),
    SetLayout(Layout),
    SetConfig(SwitchConfig),
    /// Host lock-key state (bit 0 Caps, 1 Num, 2 Scroll).
    Toggles(u8),
}

#[derive(Debug, Clone, Copy)]
struct Pending {
    from: MachineId,
    from_seq: u32,
    to: MachineId,
    to_pos: Point,
    deadline: Micros,
}

/// Host routing state.
#[derive(Debug)]
pub struct Router {
    host: MachineId,
    layout: Layout,
    cfg: SwitchConfig,
    gate: SwitchGate,
    active: MachineId,
    cursor: Point,
    /// Counter handing out `enter_seq` values (strictly increasing).
    seq: u32,
    /// `enter_seq` of the entry into the active peer (meaningless while the host is active).
    active_seq: u32,
    locked: bool,
    toggles: u8,
    modifiers: u32,
    last_edge: Option<Edge>,
    last_pos: HashMap<MachineId, Point>,
    pending: Option<Pending>,

    /// Every key physically held right now, as last reported.
    physical: BTreeMap<u32, KeyEvent>,
    /// Keys the host OS saw go down and has not seen released.
    host_os_keys: BTreeMap<u32, KeyEvent>,
    /// Keys whose press was swallowed; their release must be swallowed too.
    swallowed_keys: BTreeSet<u32>,
    /// Keys pressed on the active peer through us.
    forwarded_keys: BTreeSet<u32>,
    /// Releases the host OS is owed (down seen, up swallowed).
    owed_keys: BTreeMap<u32, KeyEvent>,
    host_os_buttons: BTreeSet<u8>,
    swallowed_buttons: BTreeSet<u8>,
    forwarded_buttons: BTreeSet<u8>,
    owed_buttons: BTreeSet<u8>,
}

impl Router {
    /// A router with the host active.
    ///
    /// # Panics
    /// Never in practice; the host must be present in `layout` (checked with `debug_assert`).
    #[must_use]
    pub fn new(host: MachineId, layout: Layout, cfg: SwitchConfig) -> Self {
        debug_assert!(layout.screen(host).is_some(), "host missing from layout");
        let cursor = layout.screen(host).map_or(Point::default(), |s| Point {
            x: i32::try_from(s.rect.width / 2).unwrap_or(0),
            y: i32::try_from(s.rect.height / 2).unwrap_or(0),
        });
        Self {
            host,
            layout,
            cfg,
            gate: SwitchGate::default(),
            active: host,
            cursor,
            seq: 0,
            active_seq: 0,
            locked: false,
            toggles: 0,
            modifiers: 0,
            last_edge: None,
            last_pos: HashMap::new(),
            pending: None,
            physical: BTreeMap::new(),
            host_os_keys: BTreeMap::new(),
            swallowed_keys: BTreeSet::new(),
            forwarded_keys: BTreeSet::new(),
            owed_keys: BTreeMap::new(),
            host_os_buttons: BTreeSet::new(),
            swallowed_buttons: BTreeSet::new(),
            forwarded_buttons: BTreeSet::new(),
            owed_buttons: BTreeSet::new(),
        }
    }

    #[must_use]
    pub const fn active(&self) -> MachineId {
        self.active
    }

    #[must_use]
    pub const fn cursor(&self) -> Point {
        self.cursor
    }

    #[must_use]
    pub const fn is_locked(&self) -> bool {
        self.locked
    }

    /// Whether a switch is waiting for a peer's `LeaveAck`.
    #[must_use]
    pub const fn is_switching(&self) -> bool {
        self.pending.is_some()
    }

    /// Earliest time at which [`Event::Tick`] must be delivered, if anything is pending.
    #[must_use]
    pub fn deadline(&self) -> Option<Micros> {
        [
            self.pending.map(|p| p.deadline),
            self.gate.deadline(&self.cfg),
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Process one event. Returns `true` when an [`Event::Input`] must be withheld from the
    /// host's applications.
    pub fn handle(&mut self, now: Micros, event: Event, out: &mut Vec<Command>) -> bool {
        match event {
            Event::Input(input) => return self.input(now, input, out),
            Event::Tick => self.tick(now, out),
            Event::PeerConnected(id) => self.layout = self.layout.with_connected(id, true),
            Event::PeerDisconnected(id) => self.peer_gone(id, out),
            Event::LeaveAck {
                from,
                enter_seq,
                ok,
            } => self.leave_ack(from, enter_seq, ok, out),
            Event::Jump(target) => self.jump(now, target, out),
            Event::SetLocked(locked) => {
                self.locked = locked;
                self.gate.reset();
            }
            Event::SetLayout(layout) => self.set_layout(layout, out),
            Event::SetConfig(cfg) => {
                self.cfg = cfg;
                self.gate.reset();
            }
            Event::Toggles(toggles) => self.toggles = toggles,
        }
        false
    }

    // ---- input ---------------------------------------------------------------------------

    fn input(&mut self, now: Micros, input: InputEvent, out: &mut Vec<Command>) -> bool {
        match input {
            InputEvent::Motion { dx, dy, host_pos } => self.motion(now, dx, dy, host_pos, out),
            InputEvent::Button { button, down } => self.button(button, down, out),
            InputEvent::Wheel { dx, dy, flags } => {
                if let Some((to, enter_seq)) = self.target() {
                    out.push(Command::Wheel {
                        to,
                        enter_seq,
                        dx,
                        dy,
                        flags,
                    });
                }
                self.active != self.host || self.pending.is_some()
            }
            InputEvent::Key(event) => self.key(event, out),
        }
    }

    /// The peer currently receiving input, if any (none while the host is active or mid-switch).
    fn target(&self) -> Option<(MachineId, u32)> {
        (self.active != self.host && self.pending.is_none())
            .then_some((self.active, self.active_seq))
    }

    fn motion(
        &mut self,
        now: Micros,
        dx: i32,
        dy: i32,
        host_pos: Point,
        out: &mut Vec<Command>,
    ) -> bool {
        if self.pending.is_some() {
            return true; // mid-switch: the pointer belongs to nobody
        }
        let on_host = self.active == self.host;
        let Some(screen) = self.layout.screen(self.active).copied() else {
            return !on_host;
        };
        let size = (screen.rect.width, screen.rect.height);
        let candidate = Point {
            x: self.cursor.x.saturating_add(dx),
            y: self.cursor.y.saturating_add(dy),
        };

        match self.layout.route(self.active, candidate) {
            Route::Stay(p) => {
                self.cursor = if on_host { host_pos } else { p };
                if let Some(edge) = self.last_edge {
                    let dist = edge_distance(self.cursor, size, edge);
                    self.gate.moved_inside(&self.cfg, now, dist);
                    if dist > 200 {
                        self.last_edge = None;
                    }
                }
            }
            Route::Blocked { clamped, .. } => {
                self.cursor = clamped; // nothing beyond this edge: no gate state, just stop
            }
            Route::Cross { machine, to, edge } => {
                let at = clamp_to_edge(candidate, size, edge);
                self.last_edge = Some(edge);
                let decision = if self.locked {
                    Decision::Deny
                } else {
                    self.gate
                        .attempt(&self.cfg, now, edge, at, size, self.modifiers)
                };
                if decision == Decision::Allow {
                    self.switch_to(now, machine, to, out);
                    return true;
                }
                self.cursor = at;
            }
        }

        if on_host {
            false
        } else {
            if let Some((to, enter_seq)) = self.target() {
                out.push(Command::Motion {
                    to,
                    enter_seq,
                    dx,
                    dy,
                });
            }
            true
        }
    }

    fn button(&mut self, button: u8, down: bool, out: &mut Vec<Command>) -> bool {
        let target = self.target();
        let on_host = self.active == self.host && self.pending.is_none();
        if down {
            if on_host {
                self.host_os_buttons.insert(button);
                return false;
            }
            self.swallowed_buttons.insert(button);
            if let Some((to, enter_seq)) = target {
                self.forwarded_buttons.insert(button);
                out.push(Command::Button {
                    to,
                    enter_seq,
                    button,
                    down: true,
                });
            }
            return true;
        }
        if self.forwarded_buttons.remove(&button)
            && let Some((to, enter_seq)) = target
        {
            out.push(Command::Button {
                to,
                enter_seq,
                button,
                down: false,
            });
        }
        if self.host_os_buttons.contains(&button) {
            if on_host {
                self.host_os_buttons.remove(&button);
                return false;
            }
            self.host_os_buttons.remove(&button);
            self.owed_buttons.insert(button);
            return true;
        }
        if self.swallowed_buttons.remove(&button) {
            return true;
        }
        !on_host
    }

    fn key(&mut self, event: KeyEvent, out: &mut Vec<Command>) -> bool {
        self.modifiers = event.modifiers;
        let target = self.target();
        let on_host = self.active == self.host && self.pending.is_none();
        let id = event.key_id;
        match event.action {
            KeyAction::Down => {
                self.physical.insert(id, event);
                if on_host {
                    self.host_os_keys.insert(id, event);
                    return false;
                }
                self.swallowed_keys.insert(id);
                if let Some((to, enter_seq)) = target {
                    self.forwarded_keys.insert(id);
                    out.push(Command::Key {
                        to,
                        enter_seq,
                        event,
                    });
                }
                true
            }
            KeyAction::Repeat => {
                if on_host {
                    return self.swallowed_keys.contains(&id);
                }
                if self.forwarded_keys.contains(&id)
                    && let Some((to, enter_seq)) = target
                {
                    out.push(Command::Key {
                        to,
                        enter_seq,
                        event,
                    });
                }
                true
            }
            KeyAction::Up => {
                self.physical.remove(&id);
                if self.forwarded_keys.remove(&id)
                    && let Some((to, enter_seq)) = target
                {
                    out.push(Command::Key {
                        to,
                        enter_seq,
                        event,
                    });
                }
                if let Some(down) = self.host_os_keys.remove(&id) {
                    if on_host {
                        return false;
                    }
                    self.owed_keys.insert(id, down);
                    return true;
                }
                if self.swallowed_keys.remove(&id) {
                    return true;
                }
                !on_host
            }
        }
    }

    // ---- switching -----------------------------------------------------------------------

    fn switch_to(&mut self, now: Micros, to: MachineId, to_pos: Point, out: &mut Vec<Command>) {
        self.gate.reset();
        self.last_edge = None;
        if to == self.active {
            self.cursor = to_pos;
            if self.active == self.host {
                out.push(Command::WarpHost(to_pos));
            }
            return;
        }
        self.last_pos.insert(self.active, self.cursor);
        if self.active == self.host {
            out.push(Command::Capture { swallow: true });
            self.enter(to, to_pos, out);
            return;
        }
        // Leaving a peer: it must release its input first (or time out).
        let from = self.active;
        out.push(Command::Leave {
            to: from,
            enter_seq: self.active_seq,
        });
        self.forwarded_keys.clear();
        self.forwarded_buttons.clear();
        self.pending = Some(Pending {
            from,
            from_seq: self.active_seq,
            to,
            to_pos,
            deadline: now.saturating_add(LEAVE_ACK_TIMEOUT_US),
        });
    }

    /// Make `to` the active machine. All bookkeeping for the previous one is already done.
    fn enter(&mut self, to: MachineId, pos: Point, out: &mut Vec<Command>) {
        self.active = to;
        self.cursor = pos;
        out.push(Command::ActiveChanged { machine: to });
        if to == self.host {
            out.push(Command::Capture { swallow: false });
            out.push(Command::WarpHost(pos));
            // Physically held modifiers the host OS never saw go down.
            let held: Vec<KeyEvent> = self
                .physical
                .values()
                .filter(|k| k.is_modifier() && self.swallowed_keys.contains(&k.key_id))
                .copied()
                .collect();
            if !held.is_empty() {
                for key in &held {
                    self.swallowed_keys.remove(&key.key_id);
                    self.host_os_keys.insert(key.key_id, *key);
                }
                out.push(Command::HostPress { keys: held });
            }
            if !self.owed_keys.is_empty() || !self.owed_buttons.is_empty() {
                let keys = std::mem::take(&mut self.owed_keys).into_values().collect();
                let buttons = std::mem::take(&mut self.owed_buttons).into_iter().collect();
                out.push(Command::HostRelease { keys, buttons });
            }
            return;
        }
        self.seq += 1;
        self.active_seq = self.seq;
        out.push(Command::Enter {
            to,
            enter_seq: self.active_seq,
            pos,
            toggles: self.toggles,
            modifiers: self.modifiers,
        });
        // Held modifiers travel with the pointer; other held keys must be pressed again.
        let held: Vec<KeyEvent> = self
            .physical
            .values()
            .filter(|k| k.is_modifier())
            .copied()
            .collect();
        for key in held {
            self.swallowed_keys.insert(key.key_id);
            self.forwarded_keys.insert(key.key_id);
            out.push(Command::Key {
                to,
                enter_seq: self.active_seq,
                event: key.with_action(KeyAction::Down),
            });
        }
    }

    fn leave_ack(&mut self, from: MachineId, enter_seq: u32, ok: bool, out: &mut Vec<Command>) {
        let Some(p) = self.pending else { return };
        if p.from != from || p.from_seq != enter_seq {
            return;
        }
        self.pending = None;
        if ok {
            self.finish_switch(p, out);
        } else {
            // The peer refused to let go: stay where we were, and make sure it holds nothing.
            out.push(Command::ReleaseAll {
                to: p.from,
                enter_seq: p.from_seq,
            });
        }
    }

    fn finish_switch(&mut self, p: Pending, out: &mut Vec<Command>) {
        if p.to != self.host && self.layout.screen(p.to).is_none_or(|s| !s.connected) {
            // Target vanished while we waited; go home instead.
            let home = self.last_pos.get(&self.host).copied().unwrap_or(p.to_pos);
            self.enter(self.host, home, out);
        } else {
            self.enter(p.to, p.to_pos, out);
        }
    }

    fn tick(&mut self, now: Micros, out: &mut Vec<Command>) {
        if let Some(p) = self.pending {
            if now >= p.deadline {
                self.pending = None;
                out.push(Command::ReleaseAll {
                    to: p.from,
                    enter_seq: p.from_seq,
                });
                self.finish_switch(p, out);
            }
            return;
        }
        if let Some(due) = self.gate.poll(&self.cfg, now) {
            let outside = match due.edge {
                Edge::Left => Point {
                    x: due.at.x - 1,
                    ..due.at
                },
                Edge::Right => Point {
                    x: due.at.x + 1,
                    ..due.at
                },
                Edge::Top => Point {
                    y: due.at.y - 1,
                    ..due.at
                },
                Edge::Bottom => Point {
                    y: due.at.y + 1,
                    ..due.at
                },
            };
            if let Route::Cross { machine, to, .. } = self.layout.route(self.active, outside) {
                self.switch_to(now, machine, to, out);
            }
        }
    }

    fn jump(&mut self, now: Micros, target: MachineId, out: &mut Vec<Command>) {
        if self.pending.is_some() || self.locked {
            return;
        }
        let Some(screen) = self.layout.screen(target) else {
            return;
        };
        if !screen.connected {
            return;
        }
        let pos = self.last_pos.get(&target).copied().unwrap_or(Point {
            x: i32::try_from(screen.rect.width / 2).unwrap_or(0),
            y: i32::try_from(screen.rect.height / 2).unwrap_or(0),
        });
        self.switch_to(now, target, pos, out);
    }

    fn peer_gone(&mut self, id: MachineId, out: &mut Vec<Command>) {
        self.layout = self.layout.with_connected(id, false);
        self.last_pos.remove(&id);
        if let Some(p) = self.pending {
            if p.from == id {
                // The peer we were leaving is gone: nothing to release, finish the switch.
                self.pending = None;
                self.finish_switch(p, out);
            } else if p.to == id {
                // Target vanished: finish at home (the sender already left).
                self.pending = None;
                self.finish_switch(p, out);
            }
            return;
        }
        if self.active == id {
            // The peer vanished under the pointer: take it back, no ack to wait for.
            self.forwarded_keys.clear();
            self.forwarded_buttons.clear();
            self.gate.reset();
            let home = self.last_pos.get(&self.host).copied().unwrap_or_else(|| {
                self.layout
                    .screen(self.host)
                    .map_or(Point::default(), |s| Point {
                        x: i32::try_from(s.rect.width / 2).unwrap_or(0),
                        y: i32::try_from(s.rect.height / 2).unwrap_or(0),
                    })
            });
            self.enter(self.host, home, out);
        }
    }

    fn set_layout(&mut self, layout: Layout, out: &mut Vec<Command>) {
        self.layout = layout;
        self.gate.reset();
        let active_ok = self
            .layout
            .screen(self.active)
            .is_some_and(|s| s.connected || s.id == self.host);
        if !active_ok && self.pending.is_none() {
            let id = self.active;
            self.peer_gone(id, out);
        }
        if let Some(screen) = self.layout.screen(self.active) {
            // The screen may have changed shape: keep the cursor inside it.
            let max_x = i32::try_from(screen.rect.width).unwrap_or(i32::MAX) - 1;
            let max_y = i32::try_from(screen.rect.height).unwrap_or(i32::MAX) - 1;
            self.cursor = Point {
                x: self.cursor.x.clamp(0, max_x),
                y: self.cursor.y.clamp(0, max_y),
            };
        }
    }
}

/// Distance of `p` from `edge` of a `size` screen.
fn edge_distance(p: Point, size: (u32, u32), edge: Edge) -> u32 {
    let dist = match edge {
        Edge::Left => i64::from(p.x),
        Edge::Right => i64::from(size.0) - 1 - i64::from(p.x),
        Edge::Top => i64::from(p.y),
        Edge::Bottom => i64::from(size.1) - 1 - i64::from(p.y),
    };
    u32::try_from(dist.max(0)).unwrap_or(u32::MAX)
}

/// `p` pushed back onto `edge` (and inside the screen along the other axis).
fn clamp_to_edge(p: Point, size: (u32, u32), edge: Edge) -> Point {
    let max_x = i32::try_from(size.0).unwrap_or(i32::MAX) - 1;
    let max_y = i32::try_from(size.1).unwrap_or(i32::MAX) - 1;
    match edge {
        Edge::Left => Point {
            x: 0,
            y: p.y.clamp(0, max_y),
        },
        Edge::Right => Point {
            x: max_x,
            y: p.y.clamp(0, max_y),
        },
        Edge::Top => Point {
            x: p.x.clamp(0, max_x),
            y: 0,
        },
        Edge::Bottom => Point {
            x: p.x.clamp(0, max_x),
            y: max_y,
        },
    }
}
