// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Pinned peers: which machines are trusted and which key they must present.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use portal_core::layout::MachineId;
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::identity::{hex, unhex};

/// A paired machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustedPeer {
    pub name: String,
    /// SHA-256 of the peer's SPKI, hex.
    pub spki_sha256: String,
}

/// The set of pinned peers, persisted as JSON.
#[derive(Debug, Default)]
pub struct TrustStore {
    path: Option<PathBuf>,
    peers: BTreeMap<String, TrustedPeer>, // keyed by hex machine id
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    peers: BTreeMap<String, TrustedPeer>,
}

/// How a presented key relates to the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustState {
    /// Machine and key both match a pin.
    Trusted,
    /// Never paired.
    Unknown,
    /// The machine ID is pinned but with a different key: reject, never silently re-pin.
    KeyMismatch,
}

impl TrustStore {
    /// An in-memory store (tests, first run).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Load from `path`, or start empty if it does not exist. Changes are saved back there.
    ///
    /// # Errors
    /// I/O failure or an unreadable file.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let peers = match fs::read_to_string(path) {
            Ok(text) => {
                let file: File = serde_json::from_str(&text)?;
                if file.version != 1 {
                    return Err(Error::UnsupportedFile(file.version));
                }
                file.peers
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            path: Some(path.to_owned()),
            peers,
        })
    }

    /// Classify a connecting machine.
    #[must_use]
    pub fn check(&self, machine: MachineId, spki_sha256: &[u8; 32]) -> TrustState {
        match self.peers.get(&hex(&machine.0)) {
            None => TrustState::Unknown,
            Some(p) if p.spki_sha256 == hex(spki_sha256) => TrustState::Trusted,
            Some(_) => TrustState::KeyMismatch,
        }
    }

    /// Whether any pinned peer uses this key.
    #[must_use]
    pub fn is_key_pinned(&self, spki_sha256: &[u8; 32]) -> bool {
        let wanted = hex(spki_sha256);
        self.peers.values().any(|p| p.spki_sha256 == wanted)
    }

    /// Pin a machine after the user confirmed matching pairing codes.
    ///
    /// # Errors
    /// Persisting failed (the in-memory pin is rolled back).
    pub fn pin(
        &mut self,
        machine: MachineId,
        name: &str,
        spki_sha256: &[u8; 32],
    ) -> Result<(), Error> {
        let key = hex(&machine.0);
        let previous = self.peers.insert(
            key.clone(),
            TrustedPeer {
                name: name.to_owned(),
                spki_sha256: hex(spki_sha256),
            },
        );
        if let Err(e) = self.save() {
            match previous {
                Some(p) => self.peers.insert(key, p),
                None => self.peers.remove(&key),
            };
            return Err(e);
        }
        Ok(())
    }

    /// Forget a peer (revocation). Returns whether it was pinned.
    ///
    /// # Errors
    /// Persisting failed (the peer stays pinned in memory too).
    pub fn revoke(&mut self, machine: MachineId) -> Result<bool, Error> {
        let key = hex(&machine.0);
        let Some(previous) = self.peers.remove(&key) else {
            return Ok(false);
        };
        if let Err(e) = self.save() {
            self.peers.insert(key, previous);
            return Err(e);
        }
        Ok(true)
    }

    /// Paired machines as `(id, peer)`.
    ///
    /// # Panics
    /// Never: keys are produced by this module as 32 hex digits.
    pub fn peers(&self) -> impl Iterator<Item = (MachineId, &TrustedPeer)> {
        self.peers.iter().filter_map(|(k, v)| {
            let bytes: [u8; 16] = unhex(k).ok()?.try_into().ok()?;
            Some((MachineId(bytes), v))
        })
    }

    fn save(&self) -> Result<(), Error> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let file = File {
            version: 1,
            peers: self.peers.clone(),
        };
        crate::fsutil::write_private(path, serde_json::to_string_pretty(&file)?.as_bytes())?;
        Ok(())
    }
}
