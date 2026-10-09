// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Randomised proof that no key or button is ever left pressed, on any machine, whatever mix of
//! typing, mouse movement, switching, disconnects, lost acknowledgements and timeouts occurs.
//!
//! The harness wires a host [`Router`] to simulated peers ([`PeerInput`]) and a simulated host
//! OS, then checks invariants after every step and full release at the end of every run.

#![allow(
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::collections::{BTreeSet, HashMap, VecDeque};

use portal_core::gate::{Corners, Micros, SwitchConfig};
use portal_core::input::{InputEvent, KeyAction, KeyEvent};
use portal_core::layout::{Layout, MachineId, Point, Rect, Screen};
use portal_core::peer::{Inject, PeerInput};
use portal_core::router::{Command, Event, Router};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

fn id(n: u8) -> MachineId {
    MachineId([n; 16])
}

fn layout() -> Layout {
    Layout::new(vec![
        Screen {
            id: id(1),
            rect: Rect::new(0, 0, 1920, 1080),
            connected: true,
        },
        Screen {
            id: id(2),
            rect: Rect::new(1920, 0, 1280, 720),
            connected: true,
        },
        Screen {
            id: id(3),
            rect: Rect::new(3200, 0, 1920, 1080),
            connected: true,
        },
    ])
    .unwrap()
}

fn key(key_id: u32, action: KeyAction) -> KeyEvent {
    // Key ids 1..=3 are modifiers (Shift, Control, Meta), the rest are letters.
    let (named_key, hid_usage) = match key_id {
        1 => (50, 0x0007_00E1),
        2 => (51, 0x0007_00E0),
        3 => (53, 0x0007_00E3),
        n => (0, 0x0007_0000 + n),
    };
    KeyEvent {
        action,
        repeat_count: 0,
        key_id,
        hid_usage,
        codepoint: 0,
        named_key,
        modifiers: 0,
        flags: 0,
    }
}

#[derive(Default)]
struct PeerSim {
    input: PeerInput,
    /// What the peer OS believes is pressed, derived only from `Inject` actions.
    os_keys: BTreeSet<u32>,
    os_buttons: BTreeSet<u8>,
    connected: bool,
}

impl PeerSim {
    fn apply(&mut self, injects: &[Inject]) {
        for inject in injects {
            match inject {
                Inject::KeyDown(k) => {
                    self.os_keys.insert(k.key_id);
                }
                Inject::KeyUp(k) => {
                    assert!(
                        self.os_keys.remove(&k.key_id),
                        "released a key that was not down"
                    );
                }
                Inject::Button { button, down: true } => {
                    self.os_buttons.insert(*button);
                }
                Inject::Button {
                    button,
                    down: false,
                } => {
                    assert!(
                        self.os_buttons.remove(button),
                        "released a button that was not down"
                    );
                }
                _ => {}
            }
        }
    }
}

struct World {
    rng: Rng,
    now: Micros,
    router: Router,
    host: MachineId,
    peers: HashMap<MachineId, PeerSim>,
    /// Host OS view of what is pressed (what applications on the host would see).
    host_keys: BTreeSet<u32>,
    host_buttons: BTreeSet<u8>,
    host_pos: Point,
    phys_keys: BTreeSet<u32>,
    phys_buttons: BTreeSet<u8>,
    /// Acks in flight: `(deliver_at, from, seq, ok)`.
    acks: VecDeque<(Micros, MachineId, u32, bool)>,
    drop_acks: bool,
}

impl World {
    fn new(seed: u64, cfg: SwitchConfig, drop_acks: bool) -> Self {
        let host = id(1);
        let mut peers = HashMap::new();
        for n in [2, 3] {
            peers.insert(
                id(n),
                PeerSim {
                    connected: true,
                    ..PeerSim::default()
                },
            );
        }
        Self {
            rng: Rng(seed | 1),
            now: 1_000,
            router: Router::new(host, layout(), cfg),
            host,
            peers,
            host_keys: BTreeSet::new(),
            host_buttons: BTreeSet::new(),
            host_pos: Point { x: 960, y: 540 },
            phys_keys: BTreeSet::new(),
            phys_buttons: BTreeSet::new(),
            acks: VecDeque::new(),
            drop_acks,
        }
    }

    fn send(&mut self, event: Event) -> bool {
        let mut out = Vec::new();
        let swallow = self.router.handle(self.now, event, &mut out);
        for command in out {
            self.execute(command);
        }
        swallow
    }

    fn execute(&mut self, command: Command) {
        let mut injects = Vec::new();
        match command {
            Command::Capture { .. } | Command::WarpHost(_) | Command::ActiveChanged { .. } => {}
            Command::HostPress { keys } => {
                for k in keys {
                    self.host_keys.insert(k.key_id);
                }
            }
            Command::HostRelease { keys, buttons } => {
                for k in keys {
                    self.host_keys.remove(&k.key_id);
                }
                for b in buttons {
                    self.host_buttons.remove(&b);
                }
            }
            Command::Enter {
                to,
                enter_seq,
                pos,
                toggles,
                ..
            } => {
                let peer = self.peers.get_mut(&to).unwrap();
                assert!(peer.connected, "entered a disconnected peer");
                assert!(
                    peer.input.enter(enter_seq, pos, toggles, &mut injects),
                    "stale enter"
                );
                peer.apply(&injects);
            }
            Command::Leave { to, enter_seq } => {
                let peer = self.peers.get_mut(&to).unwrap();
                let ok = peer.input.leave(enter_seq, &mut injects);
                peer.apply(&injects);
                let delay = 1_000 + self.rng.below(400_000); // sometimes beyond the 250 ms timeout
                let lost = self.drop_acks && self.rng.chance(15);
                if !lost && peer.connected {
                    self.acks.push_back((self.now + delay, to, enter_seq, ok));
                }
            }
            Command::ReleaseAll { to, enter_seq } => {
                let peer = self.peers.get_mut(&to).unwrap();
                peer.input.release_all(enter_seq, &mut injects);
                peer.apply(&injects);
            }
            Command::Motion {
                to,
                enter_seq,
                dx,
                dy,
            } => {
                let peer = self.peers.get_mut(&to).unwrap();
                peer.input.mouse_motion(enter_seq, dx, dy, &mut injects);
            }
            Command::Button {
                to,
                enter_seq,
                button,
                down,
            } => {
                let peer = self.peers.get_mut(&to).unwrap();
                peer.input
                    .mouse_button(enter_seq, button, down, &mut injects);
                peer.apply(&injects);
            }
            Command::Wheel {
                to,
                enter_seq,
                dx,
                dy,
                flags,
            } => {
                let peer = self.peers.get_mut(&to).unwrap();
                peer.input.wheel(enter_seq, dx, dy, flags, &mut injects);
            }
            Command::Key {
                to,
                enter_seq,
                event,
            } => {
                let peer = self.peers.get_mut(&to).unwrap();
                peer.input.key(enter_seq, event, &mut injects);
                peer.apply(&injects);
            }
        }
    }

    fn deliver_due_acks(&mut self) {
        while let Some(i) = self.acks.iter().position(|a| a.0 <= self.now) {
            let (_, from, enter_seq, ok) = self.acks.remove(i).unwrap();
            self.send(Event::LeaveAck {
                from,
                enter_seq,
                ok,
            });
        }
    }

    /// One random step of "the world".
    fn step(&mut self) {
        self.now += 1 + self.rng.below(30_000);
        self.deliver_due_acks();
        match self.rng.below(100) {
            0..=24 => self.random_key(),
            25..=34 => self.random_button(),
            35..=74 => self.random_motion(),
            75..=79 => {
                self.deliver_input(InputEvent::Wheel {
                    dx: 0,
                    dy: 120,
                    flags: 0,
                });
            }
            80..=84 => {
                let target = id(1 + self.rng.below(3) as u8);
                self.send(Event::Jump(target));
            }
            85..=87 => {
                let peer = id(2 + self.rng.below(2) as u8);
                self.disconnect(peer);
            }
            88..=91 => {
                let peer = id(2 + self.rng.below(2) as u8);
                self.reconnect(peer);
            }
            92..=93 => {
                let locked = self.rng.chance(50);
                self.send(Event::SetLocked(locked));
            }
            _ => {
                self.send(Event::Tick);
            }
        }
        self.check_always();
    }

    fn random_key(&mut self) {
        let key_id = 1 + self.rng.below(8) as u32;
        let action = if !self.phys_keys.contains(&key_id) {
            KeyAction::Down
        } else if self.rng.chance(25) {
            KeyAction::Repeat
        } else {
            KeyAction::Up
        };
        match action {
            KeyAction::Down => {
                self.phys_keys.insert(key_id);
            }
            KeyAction::Up => {
                self.phys_keys.remove(&key_id);
            }
            KeyAction::Repeat => {}
        }
        self.deliver_input(InputEvent::Key(key(key_id, action)));
    }

    fn random_button(&mut self) {
        let button = 1 + self.rng.below(3) as u8;
        let down = !self.phys_buttons.contains(&button);
        if down {
            self.phys_buttons.insert(button);
        } else {
            self.phys_buttons.remove(&button);
        }
        self.deliver_input(InputEvent::Button { button, down });
    }

    fn random_motion(&mut self) {
        let scale = if self.rng.chance(10) { 3_000 } else { 120 };
        let dx = self.rng.below(scale * 2) as i32 - scale as i32;
        let dy = self.rng.below(scale) as i32 - (scale / 2) as i32;
        // The host OS moves its own cursor only when the event is not swallowed.
        let pos = Point {
            x: (self.host_pos.x + dx).clamp(0, 1919),
            y: (self.host_pos.y + dy).clamp(0, 1079),
        };
        let swallowed = self.deliver_input(InputEvent::Motion {
            dx,
            dy,
            host_pos: pos,
        });
        if !swallowed {
            self.host_pos = pos;
        }
    }

    /// Deliver a captured event; mirror the router's verdict into the host OS model.
    fn deliver_input(&mut self, input: InputEvent) -> bool {
        let swallow = self.send(Event::Input(input));
        if !swallow {
            match input {
                InputEvent::Key(k) => match k.action {
                    KeyAction::Down => {
                        self.host_keys.insert(k.key_id);
                    }
                    KeyAction::Up => {
                        self.host_keys.remove(&k.key_id);
                    }
                    KeyAction::Repeat => {}
                },
                InputEvent::Button { button, down: true } => {
                    self.host_buttons.insert(button);
                }
                InputEvent::Button {
                    button,
                    down: false,
                } => {
                    self.host_buttons.remove(&button);
                }
                _ => {}
            }
        }
        swallow
    }

    fn disconnect(&mut self, peer: MachineId) {
        let sim = self.peers.get_mut(&peer).unwrap();
        if !sim.connected {
            return;
        }
        sim.connected = false;
        let mut injects = Vec::new();
        sim.input.disconnected(&mut injects);
        sim.apply(&injects);
        self.acks.retain(|a| a.1 != peer);
        self.send(Event::PeerDisconnected(peer));
    }

    fn reconnect(&mut self, peer: MachineId) {
        let sim = self.peers.get_mut(&peer).unwrap();
        if sim.connected {
            return;
        }
        sim.connected = true;
        self.send(Event::PeerConnected(peer));
    }

    /// Invariants that must hold after every single step.
    fn check_always(&self) {
        let active = self.router.active();
        for (peer_id, sim) in &self.peers {
            // The peer's own bookkeeping and the OS it drives agree.
            assert_eq!(
                sim.input.held_keys(),
                sim.os_keys.len(),
                "peer tracker vs OS"
            );
            assert_eq!(
                sim.input.held_buttons(),
                sim.os_buttons.len(),
                "peer tracker vs OS"
            );
            // Anything pressed on a peer is physically held, and only the active peer holds input.
            assert!(
                sim.os_keys.is_subset(&self.phys_keys),
                "{peer_id:?} holds a key nobody presses"
            );
            assert!(
                sim.os_buttons.is_subset(&self.phys_buttons),
                "{peer_id:?} holds a button nobody presses"
            );
            if *peer_id != active {
                assert!(
                    sim.os_keys.is_empty() && sim.os_buttons.is_empty(),
                    "{peer_id:?} idle but holding input"
                );
            }
        }
    }

    /// Let the user lift every finger, then settle the system and check nothing is stuck.
    fn finish(&mut self) {
        for key_id in self.phys_keys.clone() {
            self.phys_keys.remove(&key_id);
            self.deliver_input(InputEvent::Key(key(key_id, KeyAction::Up)));
        }
        for button in self.phys_buttons.clone() {
            self.phys_buttons.remove(&button);
            self.deliver_input(InputEvent::Button {
                button,
                down: false,
            });
        }
        self.send(Event::SetLocked(false));
        for _ in 0..20 {
            self.now += 300_000;
            self.deliver_due_acks();
            self.send(Event::Tick);
            if self.router.active() == self.host && !self.router.is_switching() {
                break;
            }
            self.send(Event::Jump(self.host));
        }
        assert_eq!(self.router.active(), self.host, "router never came home");
        assert!(!self.router.is_switching());
        assert!(
            self.host_keys.is_empty(),
            "host OS has stuck keys: {:?}",
            self.host_keys
        );
        assert!(
            self.host_buttons.is_empty(),
            "host OS has stuck buttons: {:?}",
            self.host_buttons
        );
        for (peer_id, sim) in &self.peers {
            assert!(
                sim.os_keys.is_empty(),
                "{peer_id:?} stuck keys {:?}",
                sim.os_keys
            );
            assert!(
                sim.os_buttons.is_empty(),
                "{peer_id:?} stuck buttons {:?}",
                sim.os_buttons
            );
        }
    }
}

fn run(seed: u64, cfg: SwitchConfig, drop_acks: bool, steps: usize) {
    let mut world = World::new(seed, cfg, drop_acks);
    for _ in 0..steps {
        world.step();
    }
    world.finish();
}

#[test]
fn default_switching() {
    for seed in 1..=400 {
        run(seed * 7919, SwitchConfig::default(), false, 400);
    }
}

#[test]
fn lost_acks_and_timeouts() {
    for seed in 1..=400 {
        run(seed * 104_729, SwitchConfig::default(), true, 400);
    }
}

#[test]
fn delay_double_tap_corners() {
    let cfg = SwitchConfig {
        delay_us: 40_000,
        double_tap_us: 200_000,
        tap_zone_px: 20,
        corner_px: 40,
        dead_corners: Corners {
            top_right: true,
            bottom_right: true,
            ..Corners::default()
        },
        required_modifiers: 0,
    };
    for seed in 1..=300 {
        run(seed * 15_485_863, cfg, true, 500);
    }
}
