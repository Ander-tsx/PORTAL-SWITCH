# Contributing

## Setup

1. Install [rustup](https://rustup.rs). The pinned toolchain (`rust-toolchain.toml`) installs
   automatically on the first `cargo` command.
2. Optional tools used by CI:
   - [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) — dependency policy
   - [`shellcheck`](https://www.shellcheck.net) — shell scripts in `dev/`

## Before pushing

Run the same checks as CI:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo deny check
shellcheck dev/*.sh
```

## Code standards

- Formatting: `rustfmt` with the repository `rustfmt.toml`.
- Lints are configured once in the workspace `Cargo.toml` (`[workspace.lints]`); every crate
  opts in with `[lints] workspace = true`. CI treats warnings as errors.
- `unsafe` is denied by default. Platform crates that need FFI may allow it locally, with a
  `// SAFETY:` comment on every `unsafe` block.
- No `unwrap()` outside tests; return errors or use `expect()` with an invariant message.
- Never log clipboard contents, key events or file contents.

## Dependencies

Only permissive licenses are accepted: **MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause**.
No GPL, LGPL, AGPL or other copyleft licenses. `deny.toml` enforces this in CI; if a useful
crate needs another permissive license, open a discussion before adding it to the allow list.

Prefer few, well-maintained dependencies. Add shared versions under
`[workspace.dependencies]` and reference them with `dep.workspace = true`.

## Commits

[Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<scope>): <summary in imperative mood, ≤ 72 chars>

<optional body: what and why, wrapped at 72>
```

- Types: `feat`, `fix`, `perf`, `refactor`, `test`, `docs`, `build`, `ci`, `chore`.
- Scope: crate or area without the `portal-` prefix (`core`, `proto`, `net`, `macos`,
  `windows`, `ui`, `ci`, `dev`).
- One logical change per commit; the tree must build and pass tests at every commit.

## Branches and pull requests

- `main` is always green. Work on short-lived branches (`feat/…`, `fix/…`) and merge through
  pull requests once CI passes.
- PR descriptions state what changed, why, and how it was tested (including manual tests on
  real machines for platform code).

## macOS dev signing

macOS ties Accessibility and Input Monitoring grants to a binary's code-signing identity.
Ad-hoc signatures change on every build, so grants would be lost after each rebuild. Use the
stable local identity instead:

```sh
dev/macos-signing.sh setup                   # once per machine
dev/macos-signing.sh sign path/to/App.app    # after every build
dev/macos-signing.sh status path/to/App.app  # inspect the signature
```

The first signing may ask for your login keychain password — choose **Always Allow**.
Release builds use a Developer ID certificate and notarization instead.

## Windows dev builds

Unsigned builds are fine for development. Features that need the service (secure desktop,
elevated windows) require running the daemon as an administrator or installing the dev
service; instructions will be added with that code.
