// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Long-term machine identity: an Ed25519 key, a self-signed certificate and a stable ID.

use std::fs;
use std::io;
use std::path::Path;

use portal_core::layout::MachineId;
use rcgen::{CertificateParams, KeyPair, PKCS_ED25519};
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::Error;
use crate::spki::spki_sha256;

/// This machine's identity. Cloning is cheap enough for per-connection use.
#[derive(Clone)]
pub struct Identity {
    machine_id: MachineId,
    key_pkcs8: Vec<u8>,
    cert_der: Vec<u8>,
    spki_sha256: [u8; 32],
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        f.debug_struct("Identity")
            .field("machine_id", &self.machine_id)
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct Stored {
    version: u32,
    machine_id: String,
    key_pkcs8: String,
    cert_der: String,
}

impl Identity {
    /// Create a fresh identity (new key, new certificate, new random machine ID).
    ///
    /// # Errors
    /// Key or certificate generation failed.
    pub fn generate() -> Result<Self, Error> {
        let key = KeyPair::generate_for(&PKCS_ED25519)?;
        let params = CertificateParams::new(vec!["portalswitch".to_owned()])?;
        let cert = params.self_signed(&key)?;
        let mut id = [0u8; 16];
        SystemRandom::new()
            .fill(&mut id)
            .map_err(|_| Error::Random)?;
        // UUID version 4 / variant bits, so the ID is a valid UUIDv4.
        id[6] = (id[6] & 0x0f) | 0x40;
        id[8] = (id[8] & 0x3f) | 0x80;
        Self::from_parts(MachineId(id), key.serialize_der(), cert.der().to_vec())
    }

    fn from_parts(
        machine_id: MachineId,
        key_pkcs8: Vec<u8>,
        cert_der: Vec<u8>,
    ) -> Result<Self, Error> {
        let spki_sha256 = spki_sha256(&cert_der).ok_or(Error::BadCertificate)?;
        Ok(Self {
            machine_id,
            key_pkcs8,
            cert_der,
            spki_sha256,
        })
    }

    #[must_use]
    pub const fn machine_id(&self) -> MachineId {
        self.machine_id
    }

    /// Certificate in DER form.
    #[must_use]
    pub fn cert_der(&self) -> &[u8] {
        &self.cert_der
    }

    /// Private key in PKCS#8 DER form.
    pub(crate) fn key_pkcs8(&self) -> &[u8] {
        &self.key_pkcs8
    }

    /// The pin other machines store for this one.
    #[must_use]
    pub const fn spki_sha256(&self) -> [u8; 32] {
        self.spki_sha256
    }

    /// Load an identity, or create and persist one if the file does not exist.
    ///
    /// # Errors
    /// I/O failures, or a file that is not a valid identity (it is never overwritten).
    pub fn load_or_create(path: &Path) -> Result<Self, Error> {
        match fs::read_to_string(path) {
            Ok(text) => Self::from_json(&text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let identity = Self::generate()?;
                identity.save(path)?;
                Ok(identity)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn from_json(text: &str) -> Result<Self, Error> {
        let stored: Stored = serde_json::from_str(text)?;
        if stored.version != 1 {
            return Err(Error::UnsupportedFile(stored.version));
        }
        let id: [u8; 16] = unhex(&stored.machine_id)?
            .try_into()
            .map_err(|_| Error::BadIdentityFile)?;
        Self::from_parts(
            MachineId(id),
            unhex(&stored.key_pkcs8)?,
            unhex(&stored.cert_der)?,
        )
    }

    /// Persist atomically; on Unix the file is readable by the owner only.
    ///
    /// # Errors
    /// I/O failures.
    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let stored = Stored {
            version: 1,
            machine_id: hex(&self.machine_id.0),
            key_pkcs8: hex(&self.key_pkcs8),
            cert_der: hex(&self.cert_der),
        };
        crate::fsutil::write_private(path, serde_json::to_string_pretty(&stored)?.as_bytes())?;
        Ok(())
    }
}

/// Lower-case hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Decode lower- or upper-case hex.
///
/// # Errors
/// Odd length or a non-hex character.
pub fn unhex(text: &str) -> Result<Vec<u8>, Error> {
    if !text.len().is_multiple_of(2) || !text.is_ascii() {
        return Err(Error::BadIdentityFile);
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|_| Error::BadIdentityFile))
        .collect()
}
