//! Spike: input capture/injection on Windows — latency, Raw Input under low-level hooks,
//! desktops (secure desktop / UAC) and the service + session agent model.
//!
//! Modes:
//!   inject <hz> <secs> [swallow]        SendInput -> WH_MOUSE_LL latency; `swallow` drops events
//!   rawinput <secs> [swallow] [selftest] Raw Input deltas, optionally while a LL hook swallows;
//!                                        `selftest` injects motion itself (no human needed)
//!   desktop                             session, elevation and input desktop of this process
//!   service install|uninstall|start|stop  manage the spike service (run as administrator)
//!   service-main                        entry point used by the service control manager
//!   agent                               session agent launched by the service
//!
//! Logs from the service and agent go to C:\ProgramData\PortalSwitchSpike\.

#[cfg(windows)]
mod desktop;
#[cfg(windows)]
mod latency;
#[cfg(windows)]
mod service;
#[cfg(windows)]
mod util;

#[cfg(windows)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let num = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let has = |f: &str| args.iter().any(|a| a == f);
    let result = match args.first().map(String::as_str) {
        Some("inject") => latency::inject(num(1, 1000), num(2, 10), has("swallow")),
        Some("rawinput") => latency::rawinput(num(1, 10), has("swallow"), has("selftest")),
        Some("desktop") => desktop::report(),
        Some("service") => service::manage(args.get(1).map_or("", String::as_str)),
        Some("service-main") => service::run_dispatcher(),
        Some("agent") => service::agent(),
        _ => {
            eprintln!(
                "usage: windows-input inject <hz> <secs> [swallow] | rawinput <secs> [swallow] [selftest] \
                 | desktop | service install|uninstall|start|stop"
            );
            Ok(String::new())
        }
    };
    match result {
        Ok(out) => {
            if !out.is_empty() {
                println!("{out}");
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("this spike only runs on Windows");
    std::process::exit(1);
}
