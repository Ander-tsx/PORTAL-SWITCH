// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

use std::io;

/// Errors from identity, trust and TLS setup.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("certificate generation failed: {0}")]
    Cert(#[from] rcgen::Error),
    #[error("TLS configuration error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("the system random number generator failed")]
    Random,
    #[error("certificate has no readable public key")]
    BadCertificate,
    #[error("identity file is corrupt")]
    BadIdentityFile,
    #[error("unsupported file version {0}")]
    UnsupportedFile(u32),
}
