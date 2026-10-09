# Architecture

This document describes how PortalSwitch is put together. Parts marked *planned* do not
exist in code yet; they describe the design the code is being built towards.

## Processes

```
┌──────────────────────┐   typed local IPC    ┌─────────────────────────────┐
│  UI app (Tauri 2)    │ ◄──────────────────► │  portald (daemon)           │
│  layout editor, tray │  socket / named pipe │  network, topology, input,  │
│  onboarding, status  │   user-only ACL      │  clipboard, files, config   │
└──────────────────────┘                      └──────────────┬──────────────┘
                                                             │ TLS 1.3, mutual auth
                                                             ▼
                                                    other machines' portald
```

- **portald** owns everything that matters at runtime: networking, the screen topology,
  input capture and injection, clipboard, file transfer and configuration. It runs at login
  (macOS: login item inside the signed app bundle; Windows: service plus a user-session
  helper — *planned*).
- **UI app** is a thin client. It can quit or crash without interrupting input sharing.
  It talks to the daemon over a versioned, typed IPC API (request/response plus an event
  stream). The UI never parses logs to learn state.
- **No server/client roles.** The machine that owns the physical keyboard and mouse is the
  host for the session. The layout is edited once and replicated to peers.

## Crates

| Crate | Kind | Responsibility |
|---|---|---|
| `portal-core` | lib | Platform-independent logic: screen layout and cursor routing, switching conditions (delay, double-tap, corners, modifiers), the host routing state machine and the receiver-side stuck-input guard. No OS code, no I/O. |
| `portal-proto` | lib | Wire protocol v1: message types and a sans-IO frame codec, checked against independent test vectors and fuzzed in tests. |
| `portal-net` | lib | Machine identity, pinned trust store, mutual TLS 1.3, pairing codes, handshake, links (liveness, coalescing control queue), dual-TCP transport and mDNS discovery. |
| `portal-config` | lib | Versioned JSON configuration model and file locations, shared by the daemon and tools. |
| `portal-ipc` | lib | Local API between the daemon and the UI or tools: JSON lines over a Unix socket / named pipe with a per-user token. |
| `portal-platform` | lib | Platform traits (`InputCapture`, `InputInjector`, `Screens`) plus an in-process fake backend for headless tests. |
| `portald` | lib + bin | The daemon: wires configuration, the TLS endpoint, the router, discovery, the local API and a platform backend. |

*Planned* crates (added as work starts on them):

| Crate | Responsibility |
|---|---|
| `portal-platform-macos` | macOS implementations of the platform traits. |
| `portal-platform-windows` | Windows implementations of the platform traits. |
| `portal-ui` | Tauri application. |

Dependency direction: `portald` → `portal-net`/`portal-ipc`/`portal-platform-*` →
`portal-proto`/`portal-platform` → `portal-core`. Core crates never depend on platform crates;
platform crates are selected with `cfg(target_os)` in the daemon.

## Network protocol

- Own binary protocol over TLS 1.3 (`rustls`), always encrypted, mutually authenticated with
  pinned per-machine keys established through code-based pairing.
- **Separate channels for input and bulk data.** Input events travel on a latency-critical
  channel with `TCP_NODELAY`; clipboard payloads and file transfers use their own flow-
  controlled channel(s) so they can never delay input. The current transport uses two TCP + TLS
  connections per link behind a `Link` abstraction; QUIC remains a drop-in alternative pending
  real-network measurements.
- Length-prefixed frames with explicit message types and hard size limits; unknown optional
  messages are ignored; versions and capabilities are negotiated in the handshake.

## Input model

- Capture uses the OS's native per-event relative motion; the cursor is not re-centred to
  measure movement.
- Every key event carries the physical key (HID usage), the logical key/character and the
  modifier state, so the receiver can inject by character, by physical key, or as an exact
  shortcut depending on policy.
- The receiver tracks every synthetic press and releases all of them on leave, disconnect or
  timeout. No stuck keys or buttons is a tested invariant.

## Testing strategy

- Pure logic (topology, routing, protocol) lives in platform-independent crates and is unit-
  and property-tested.
- A fake platform backend lets whole sessions (pairing, switching, input relay) run headless
  in CI.
- Platform code is kept thin and exercised by manual and on-device test plans.
