//! PortalSwitch daemon entry point.
//!
//! Placeholder until the daemon is implemented; it only reports its version.

use std::io::Write as _;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut out = std::io::stdout().lock();
    match writeln!(out, "portald {}", portal_core::VERSION) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
