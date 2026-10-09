// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! PortalSwitch daemon entry point.

use std::io::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use portal_ipc::{Endpoint, token};
use portal_net::link::LinkConfig;
use portal_net::{Identity, TrustStore};
use portald::config::{Config, Paths};
use portald::daemon::{DaemonOptions, IpcOptions, PairingPolicy, Platform, start};

const HELP: &str = "\
portald — PortalSwitch background daemon

USAGE:
    portald --version | --help
    portald run [--config-dir <dir>] [--role host|peer]

`run` starts the daemon in the foreground. The machine that has the physical keyboard and mouse
runs as `host`; every other machine runs as `peer`. The role is saved in the configuration.
Control it with `portalctl`.
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut out = std::io::stdout().lock();
    let written = match args.first().map(String::as_str) {
        Some("--version" | "-V") | None => writeln!(out, "portald {}", portal_core::VERSION),
        Some("--help" | "-h") => write!(out, "{HELP}"),
        Some("run") => {
            drop(out);
            return run(&args[1..]);
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

struct RunArgs {
    paths: Paths,
    role: Option<bool>,
}

fn parse(args: &[String]) -> Result<RunArgs, String> {
    let mut paths = Paths::default_for_user();
    let mut role = None;
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config-dir" => {
                paths = Paths {
                    dir: PathBuf::from(it.next().ok_or("--config-dir needs a value")?),
                };
            }
            "--role" => {
                role = Some(match it.next().map(String::as_str) {
                    Some("host") => true,
                    Some("peer") => false,
                    _ => return Err("--role must be `host` or `peer`".into()),
                });
            }
            other => return Err(format!("unknown option {other:?}")),
        }
    }
    Ok(RunArgs { paths, role })
}

fn run(args: &[String]) -> ExitCode {
    let args = match parse(args) {
        Ok(a) => a,
        Err(e) => return fail(&e),
    };
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return fail(&format!("cannot start the async runtime: {e}")),
    };
    match runtime.block_on(run_async(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

/// The computer's own name, used the first time as the display name.
fn computer_name() -> String {
    ["COMPUTERNAME", "HOSTNAME", "HOST"]
        .iter()
        .find_map(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
        .or_else(|| {
            std::process::Command::new("hostname")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "This computer".to_owned())
}

async fn run_async(args: RunArgs) -> Result<(), String> {
    let paths = args.paths;
    let mut config = Config::load(&paths.config()).map_err(|e| e.to_string())?;
    let mut changed = false;
    if config.machine_name == Config::default().machine_name {
        config.machine_name = computer_name();
        changed = true;
    }
    if let Some(is_host) = args.role {
        changed |= config.is_host != is_host;
        config.is_host = is_host;
    }
    if changed || !paths.config().exists() {
        config.save(&paths.config()).map_err(|e| e.to_string())?;
    }

    let identity = Identity::load_or_create(&paths.identity()).map_err(|e| e.to_string())?;
    let trust = TrustStore::open(&paths.trust()).map_err(|e| e.to_string())?;
    let ipc_token =
        token::load_or_create(&paths.dir.join("ipc.token")).map_err(|e| e.to_string())?;
    let platform = platform(config.is_host)?;

    let options = DaemonOptions {
        bind: std::net::SocketAddr::from(([0, 0, 0, 0], config.listen_port)),
        config,
        config_path: Some(paths.config()),
        identity,
        trust,
        link: LinkConfig::default(),
        pairing: PairingPolicy::Ask,
        ipc: Some(IpcOptions {
            endpoint: Endpoint::in_dir(&paths.dir),
            token: ipc_token,
        }),
        discovery: true,
    };
    let handle = start(options, platform).await.map_err(|e| e.to_string())?;
    tracing::info!(addr = %handle.local_addr(), "ready; stop with Ctrl-C");
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    tracing::info!("shutting down");
    handle.shutdown();
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform(is_host: bool) -> Result<Platform, String> {
    use portal_platform_macos::{
        MacCapture, MacInjector, MacScreens, permissions, request_permissions,
    };

    let perms = permissions();
    if !perms.all_granted() {
        request_permissions();
        tracing::warn!(
            accessibility = perms.accessibility,
            input_monitoring = perms.input_monitoring,
            "macOS permissions are missing. Grant Accessibility and Input Monitoring to this \
             program in System Settings → Privacy & Security, then start it again."
        );
        if is_host {
            return Err("missing macOS permissions (see the message above)".into());
        }
    }
    Ok(Platform {
        capture: Box::new(MacCapture::new()),
        injector: Box::new(MacInjector::new().map_err(|e| e.to_string())?),
        screens: Box::new(MacScreens),
    })
}

#[cfg(windows)]
fn platform(_is_host: bool) -> Result<Platform, String> {
    use portal_platform_windows::{WinCapture, WinInjector, WinScreens, enable_dpi_awareness};

    enable_dpi_awareness();
    Ok(Platform {
        capture: Box::new(WinCapture::new()),
        injector: Box::new(WinInjector::new()),
        screens: Box::new(WinScreens),
    })
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform(_is_host: bool) -> Result<Platform, String> {
    Err("no input backend is available for this operating system yet".into())
}
