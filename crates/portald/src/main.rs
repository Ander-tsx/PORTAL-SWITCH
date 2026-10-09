// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! PortalSwitch daemon entry point.

use std::io::Write as _;
use std::process::ExitCode;

const HELP: &str = "\
portald — PortalSwitch background daemon

USAGE:
    portald [--version | --help]
    portald run [--config-dir <dir>]

Real input capture and injection backends for this operating system are not wired in yet;
`run` currently reports that and exits.
";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut out = std::io::stdout().lock();
    let written = match args.next().as_deref() {
        Some("--version" | "-V") | None => writeln!(out, "portald {}", portal_core::VERSION),
        Some("--help" | "-h") => write!(out, "{HELP}"),
        Some("run") => {
            drop(out);
            return run();
        }
        Some(other) => {
            drop(out);
            return fail(&format!("unknown command {other:?}; try --help"));
        }
    };
    if written.is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn fail(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "portald: {message}");
    ExitCode::FAILURE
}

fn run() -> ExitCode {
    fail("no input backend is available for this operating system yet")
}
