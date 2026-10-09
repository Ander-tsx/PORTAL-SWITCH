// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! `portalctl`: command-line control of a running `portald`.

use std::io::{BufRead as _, Write as _};
use std::path::PathBuf;
use std::process::ExitCode;

use portal_ipc::proto::{DiscoveredInfo, PeerInfo, Request};
use portal_ipc::{Client, Endpoint, Event, token};
use portald::config::Paths;

const HELP: &str = "\
portalctl — control a running portald

USAGE:
    portalctl [--config-dir <dir>] <command>

COMMANDS:
    status                 who is active, who is connected
    peers                  paired machines
    discovered             machines visible on the network
    connect <ip:port>      link to another machine (starts pairing the first time)
    jump <name|id>         move the pointer to a machine
    lock | unlock          keep the pointer on the current screen
    revoke <name|id>       forget a paired machine
    config                 print the configuration
    pair                   answer pairing requests (compare the code on both screens)
    watch                  print events as they happen
    shutdown               stop the daemon
";

fn say(text: &str) {
    let _ = writeln!(std::io::stdout(), "{text}");
}

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut paths = Paths::default_for_user();
    if args.first().map(String::as_str) == Some("--config-dir") && args.len() >= 2 {
        paths = Paths {
            dir: PathBuf::from(args.remove(1)),
        };
        args.remove(0);
    }
    if args.is_empty() || matches!(args[0].as_str(), "--help" | "-h") {
        say(HELP.trim_end());
        return ExitCode::SUCCESS;
    }
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => return fail(&e.to_string()),
    };
    match runtime.block_on(run(&paths, &args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => fail(&e),
    }
}

fn fail(message: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "portalctl: {message}");
    ExitCode::FAILURE
}

async fn run(paths: &Paths, args: &[String]) -> Result<(), String> {
    let token = token::read(&paths.dir.join("ipc.token")).map_err(|_| {
        "cannot read the access token; is portald running for this user?".to_owned()
    })?;
    let client = Client::connect(&Endpoint::in_dir(&paths.dir), &token)
        .await
        .map_err(|e| format!("cannot reach portald: {e}"))?;
    let arg = args.get(1).map(String::as_str);
    match (args[0].as_str(), arg) {
        ("status", _) => status(&client).await,
        ("peers", _) => {
            for p in client.paired().await.map_err(err)? {
                say(&format!("{}  {}", p.machine, p.name));
            }
            Ok(())
        }
        ("discovered", _) => {
            let list: Vec<DiscoveredInfo> =
                serde_json::from_value(client.call(Request::Discovered).await.map_err(err)?)
                    .map_err(|e| e.to_string())?;
            for d in list {
                let state = if d.paired { "paired" } else { "new" };
                say(&format!(
                    "{}  {:<20} {}:{}  [{state}]",
                    d.machine,
                    d.name,
                    d.addresses.join(","),
                    d.port
                ));
            }
            Ok(())
        }
        ("connect", Some(address)) => {
            say(
                "connecting… if this is the first time, compare the code with `portalctl pair` on both machines",
            );
            client
                .call(Request::Connect {
                    address: address.to_owned(),
                })
                .await
                .map_err(err)?;
            say("connected");
            Ok(())
        }
        ("jump", Some(who)) => {
            let machine = resolve(&client, who).await?;
            client.call(Request::Jump { machine }).await.map_err(err)?;
            Ok(())
        }
        ("lock", _) => client
            .call(Request::SetLocked { locked: true })
            .await
            .map(drop)
            .map_err(err),
        ("unlock", _) => client
            .call(Request::SetLocked { locked: false })
            .await
            .map(drop)
            .map_err(err),
        ("revoke", Some(who)) => {
            let machine = resolve(&client, who).await?;
            client
                .call(Request::Revoke { machine })
                .await
                .map_err(err)?;
            say("forgotten");
            Ok(())
        }
        ("config", _) => {
            let config = client.call(Request::GetConfig).await.map_err(err)?;
            say(&serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?);
            Ok(())
        }
        ("shutdown", _) => client.call(Request::Shutdown).await.map(drop).map_err(err),
        ("pair", _) => pair(&client).await,
        ("watch", _) => watch(&client).await,
        (other, _) => Err(format!(
            "unknown or incomplete command {other:?}; try --help"
        )),
    }
}

#[allow(clippy::needless_pass_by_value)] // used as a `map_err` function pointer
fn err(e: portal_ipc::ClientError) -> String {
    e.to_string()
}

async fn status(client: &Client) -> Result<(), String> {
    let s = client.status().await.map_err(err)?;
    say(&format!("machine   {}", s.machine));
    say(&format!(
        "role      {}",
        if s.is_host { "host" } else { "peer" }
    ));
    say(&format!("active    {}", s.active));
    say(&format!("locked    {}", s.locked));
    if s.peers.is_empty() {
        say("peers     none connected");
    }
    for p in s.peers {
        say(&format!("peer      {}  {}", p.machine, p.name));
    }
    Ok(())
}

/// A machine ID, or a name/ID prefix matched against connected and paired machines.
async fn resolve(client: &Client, who: &str) -> Result<String, String> {
    let mut known: Vec<PeerInfo> = client.status().await.map_err(err)?.peers;
    known.extend(client.paired().await.map_err(err)?);
    let wanted = who.to_lowercase();
    let matches: Vec<&PeerInfo> = known
        .iter()
        .filter(|p| p.name.to_lowercase() == wanted || p.machine.starts_with(&wanted))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one.machine.clone()),
        [] => Err(format!("no machine matches {who:?}")),
        _ => Err(format!("{who:?} is ambiguous")),
    }
}

async fn pair(client: &Client) -> Result<(), String> {
    let mut events = client.subscribe().await.map_err(err)?;
    say("waiting for pairing requests (Ctrl-C to stop)");
    while let Some(event) = events.recv().await {
        if let Event::PairingRequested {
            id,
            remote_name,
            code,
        } = event
        {
            say(&format!(
                "\n{remote_name} wants to pair. Code on its screen must be: {code}"
            ));
            let _ = write!(std::io::stdout(), "Same code? [y/N] ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .map_err(|e| e.to_string())?;
            let accept = line.trim().eq_ignore_ascii_case("y");
            client
                .call(Request::AnswerPairing { id, accept })
                .await
                .map_err(err)?;
            say(if accept { "accepted" } else { "declined" });
        }
    }
    Ok(())
}

async fn watch(client: &Client) -> Result<(), String> {
    let mut events = client.subscribe().await.map_err(err)?;
    while let Some(event) = events.recv().await {
        say(&serde_json::to_string(&event).map_err(|e| e.to_string())?);
    }
    Ok(())
}
