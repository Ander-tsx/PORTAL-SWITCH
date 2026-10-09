# Third-party notices

PortalSwitch is built with permissively licensed components only (MIT, Apache-2.0, BSD,
ISC, Unicode-3.0). This file lists components whose notices must be reproduced and is
updated whenever such a component is vendored or linked into a release.

No third-party code is vendored in the repository yet. Dependency licenses are checked in CI
with `cargo deny check` (see `deny.toml`); release builds ship the generated list of every
linked crate and its license text.
