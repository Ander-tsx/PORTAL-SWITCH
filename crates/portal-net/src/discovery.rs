// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! LAN discovery with mDNS / DNS-SD (`_portalswitch._tcp`).
//!
//! Discovery only *suggests* machines to the user; trust still comes from pairing. The TXT
//! record carries the machine ID and display name, never anything secret.

use std::collections::HashMap;
use std::net::IpAddr;

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use portal_core::layout::MachineId;
use tokio::sync::mpsc;

use crate::identity::{hex, unhex};

/// DNS-SD service type.
pub const SERVICE_TYPE: &str = "_portalswitch._tcp.local.";
const INSTANCE_PREFIX: &str = "portalswitch-";

/// What this machine announces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertisement {
    pub machine: MachineId,
    pub name: String,
    pub port: u16,
    pub is_host: bool,
}

/// A machine found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    pub machine: MachineId,
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub is_host: bool,
}

/// Changes to the set of visible machines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscoveryEvent {
    Found(Discovered),
    Lost(MachineId),
}

/// Discovery failed to start.
#[derive(Debug, thiserror::Error)]
#[error("LAN discovery is unavailable: {0}")]
pub struct DiscoveryError(String);

impl From<mdns_sd::Error> for DiscoveryError {
    fn from(e: mdns_sd::Error) -> Self {
        Self(e.to_string())
    }
}

/// A running advertisement and browser. Dropping it withdraws the advertisement.
pub struct Discovery {
    daemon: ServiceDaemon,
    fullname: String,
}

impl std::fmt::Debug for Discovery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Discovery")
            .field("service", &self.fullname)
            .finish_non_exhaustive()
    }
}

impl Discovery {
    /// Advertise this machine and start browsing for others.
    ///
    /// # Errors
    /// mDNS could not be started (no suitable network interface, sockets blocked, …).
    pub fn start(
        ad: &Advertisement,
    ) -> Result<(Self, mpsc::Receiver<DiscoveryEvent>), DiscoveryError> {
        let daemon = ServiceDaemon::new()?;
        let instance = format!("{INSTANCE_PREFIX}{}", hex(&ad.machine.0));
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host_label(&ad.name),
            "",
            ad.port,
            txt_for(ad),
        )?
        .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info)?;

        let browse = daemon.browse(SERVICE_TYPE)?;
        let (tx, rx) = mpsc::channel(64);
        let own = ad.machine;
        std::thread::Builder::new()
            .name("mdns-browse".into())
            .spawn(move || {
                while let Ok(event) = browse.recv() {
                    let out = match event {
                        ServiceEvent::ServiceResolved(service) => parse(&service)
                            .filter(|d| d.machine != own)
                            .map(DiscoveryEvent::Found),
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            machine_from_fullname(&fullname).map(DiscoveryEvent::Lost)
                        }
                        _ => None,
                    };
                    if let Some(out) = out
                        && tx.blocking_send(out).is_err()
                    {
                        return;
                    }
                }
            })
            .map_err(|e| DiscoveryError(e.to_string()))?;
        Ok((Self { daemon, fullname }, rx))
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

fn txt_for(ad: &Advertisement) -> HashMap<String, String> {
    HashMap::from([
        ("v".to_owned(), "1".to_owned()),
        ("id".to_owned(), hex(&ad.machine.0)),
        ("name".to_owned(), ad.name.chars().take(63).collect()),
        (
            "host".to_owned(),
            if ad.is_host { "1" } else { "0" }.to_owned(),
        ),
    ])
}

/// A DNS-safe host label derived from the display name.
fn host_label(name: &str) -> String {
    let label: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let label = label.trim_matches('-');
    format!(
        "{}.local.",
        if label.is_empty() {
            "portalswitch"
        } else {
            label
        }
    )
}

fn machine_from_fullname(fullname: &str) -> Option<MachineId> {
    let instance = fullname.strip_prefix(INSTANCE_PREFIX)?.split('.').next()?;
    Some(MachineId(unhex(instance).ok()?.try_into().ok()?))
}

fn parse(service: &ResolvedService) -> Option<Discovered> {
    let machine = MachineId(
        unhex(service.get_property_val_str("id")?)
            .ok()?
            .try_into()
            .ok()?,
    );
    let name = service
        .get_property_val_str("name")
        .unwrap_or("Unnamed computer");
    let mut addresses: Vec<IpAddr> = service
        .get_addresses()
        .iter()
        .map(mdns_sd::ScopedIp::to_ip_addr)
        .collect();
    addresses.sort();
    if addresses.is_empty() || service.port == 0 {
        return None;
    }
    Some(Discovered {
        machine,
        name: name.chars().take(64).collect(),
        addresses,
        port: service.port,
        is_host: service.get_property_val_str("host") == Some("1"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_labels_are_dns_safe() {
        assert_eq!(host_label("Ana's MacBook Pro"), "ana-s-macbook-pro.local.");
        assert_eq!(host_label("***"), "portalswitch.local.");
        assert_eq!(host_label("Studio"), "studio.local.");
    }

    #[test]
    fn instance_names_round_trip_to_machine_ids() {
        let id = MachineId([0xab; 16]);
        let fullname = format!("{INSTANCE_PREFIX}{}.{SERVICE_TYPE}", hex(&id.0));
        assert_eq!(machine_from_fullname(&fullname), Some(id));
        assert_eq!(
            machine_from_fullname("someone-else._http._tcp.local."),
            None
        );
        assert_eq!(
            machine_from_fullname("portalswitch-zz._portalswitch._tcp.local."),
            None
        );
    }

    #[test]
    fn advertisement_text_records_carry_the_machine() {
        let ad = Advertisement {
            machine: MachineId([1; 16]),
            name: "Studio".into(),
            port: 24850,
            is_host: true,
        };
        let txt = txt_for(&ad);
        assert_eq!(txt["id"], hex(&[1; 16]));
        assert_eq!(txt["host"], "1");
        assert_eq!(txt["v"], "1");
    }
}
