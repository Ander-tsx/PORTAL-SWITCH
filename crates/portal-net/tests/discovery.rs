// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Real multicast DNS on the local machine. Ignored by default because CI runners and sandboxes
//! often block multicast; run with `cargo test -p portal-net --test discovery -- --ignored`.

#![allow(clippy::unwrap_used)]

use std::time::Duration;

use portal_core::layout::MachineId;
use portal_net::discovery::{Advertisement, Discovery, DiscoveryEvent};

#[tokio::test]
#[ignore = "needs working multicast on the local network"]
async fn two_instances_find_each_other() {
    let a = Advertisement {
        machine: MachineId([0xa1; 16]),
        name: "Studio".into(),
        port: 24850,
        is_host: true,
    };
    let b = Advertisement {
        machine: MachineId([0xb2; 16]),
        name: "Laptop".into(),
        port: 24851,
        is_host: false,
    };
    let (_da, events_a) = Discovery::start(&a).unwrap();
    let (_db, events_b) = Discovery::start(&b).unwrap();

    let find = |mut rx: tokio::sync::mpsc::Receiver<DiscoveryEvent>, wanted: MachineId| async move {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let Some(DiscoveryEvent::Found(d)) = rx.recv().await
                    && d.machine == wanted
                {
                    return d;
                }
            }
        })
        .await
        .expect("peer not discovered")
    };
    let seen_by_a = find(events_a, b.machine).await;
    let seen_by_b = find(events_b, a.machine).await;
    assert_eq!(
        (seen_by_a.name.as_str(), seen_by_a.port, seen_by_a.is_host),
        ("Laptop", 24851, false)
    );
    assert_eq!(
        (seen_by_b.name.as_str(), seen_by_b.port, seen_by_b.is_host),
        ("Studio", 24850, true)
    );
    assert_ne!(seen_by_a.addresses, Vec::<std::net::IpAddr>::new());
}
