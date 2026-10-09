# PortalSwitch

Use one keyboard and mouse across several computers. Move the pointer off the edge of one
screen and it continues on the next machine — keyboard, clipboard and files follow.

PortalSwitch targets macOS and Windows first (Linux later), up to five machines, with a focus
on being installable and usable by non-technical people:

- **One app, no roles.** Every machine runs the same app; arrange screens in a visual layout
  editor and the rest is derived.
- **Low-latency input.** Native per-event mouse deltas and a dedicated input channel that
  bulk traffic (clipboard, files) can never delay.
- **Correct keyboards across layouts.** Character, physical and shortcut modes, with a visual
  editor for modifier and key mappings between machines.
- **Clipboard and file drag & drop** between machines.
- **Secure by default.** Always encrypted (TLS 1.3), mutual authentication, pairing with a
  short code instead of fingerprints.

> **Status:** early development (foundations phase). Nothing usable yet.

PortalSwitch is free and open source ([Apache-2.0](LICENSE)). If it saves you a cable or a
second keyboard, you can support its development — see [Supporting the project](#supporting-the-project).

## Repository layout

| Path | Contents |
|---|---|
| `crates/` | Rust workspace crates (see [ARCHITECTURE.md](ARCHITECTURE.md)) |
| `website/` | Project landing page (static, published with GitHub Pages) |
| `dev/` | Developer tooling (e.g. macOS dev code signing) |
| `.github/workflows/` | CI |

## Building

Requirements: [rustup](https://rustup.rs). The toolchain version is pinned in
`rust-toolchain.toml` and installed automatically.

```sh
cargo build --workspace
cargo test --workspace
```

macOS developers: input capture/injection requires Accessibility and Input Monitoring
permissions, which macOS ties to the code-signing identity. Sign every local build with a
stable identity so grants survive rebuilds — see [CONTRIBUTING.md](CONTRIBUTING.md#macos-dev-signing).

## Supporting the project

PortalSwitch has no ads, no tracking and no paid tier. Donations are optional and go toward
signing certificates, test hardware and maintainer time:

- [Buy Me a Coffee](https://buymeacoffee.com/portalswitch)
- [GitHub Sponsors](https://github.com/sponsors/Ander-tsx)

The same links appear (unobtrusively) in the app's *About* page. Only these channels are
official; see [TRADEMARKS.md](TRADEMARKS.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Security issues: [SECURITY.md](SECURITY.md).

## License

Code: [Apache License 2.0](LICENSE) — see [NOTICE](NOTICE). The PortalSwitch name and logo
are not covered by the license: [TRADEMARKS.md](TRADEMARKS.md). Third-party components:
[THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
