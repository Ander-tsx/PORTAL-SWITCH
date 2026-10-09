// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! TLS 1.3 configuration with mutual authentication.
//!
//! Both sides present self-signed certificates. The handshake proves possession of the
//! certificate's key (signatures are verified) but does **not** decide trust: whether the peer
//! is pinned is checked right after the handshake against the [`crate::TrustStore`], because an
//! unknown peer must still be able to connect in order to pair (protocol §9).

use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};

use crate::Error;
use crate::identity::Identity;
use crate::spki::spki_sha256;

/// ALPN of the control (and QUIC) connection.
pub const ALPN_CONTROL: &[u8] = b"portalswitch/1";
/// ALPN of the bulk connection in the dual-TCP mapping.
pub const ALPN_BULK: &[u8] = b"portalswitch/1-bulk";
/// Name used for the (ignored) server-name check.
pub const SERVER_NAME: &str = "portalswitch";
/// Label of the TLS exporter that seeds pairing codes.
pub const PAIRING_EXPORTER_LABEL: &[u8] = b"EXPORTER-PortalSwitch-pairing";

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

#[derive(Debug)]
struct AcceptAnyPeer(Arc<CryptoProvider>);

impl AcceptAnyPeer {
    fn schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }

    fn verify_tls13(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }
}

impl ServerCertVerifier for AcceptAnyPeer {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        spki_sha256(end_entity)
            .map(|_| ServerCertVerified::assertion())
            .ok_or_else(|| rustls::Error::General("certificate has no public key".into()))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

impl ClientCertVerifier for AcceptAnyPeer {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, rustls::Error> {
        spki_sha256(end_entity)
            .map(|_| ClientCertVerified::assertion())
            .ok_or_else(|| rustls::Error::General("certificate has no public key".into()))
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("TLS 1.2 is not supported".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        self.verify_tls13(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.schemes()
    }
}

fn identity_parts(identity: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(identity.cert_der().to_vec())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(identity.key_pkcs8().to_vec())),
    )
}

/// Server-side (acceptor) configuration; the client must present a certificate.
///
/// # Errors
/// The identity's key or certificate was rejected by rustls.
pub fn server_config(
    identity: &Identity,
    alpns: &[&[u8]],
) -> Result<Arc<rustls::ServerConfig>, Error> {
    let provider = provider();
    let (certs, key) = identity_parts(identity);
    let mut config = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(AcceptAnyPeer(provider)))
        .with_single_cert(certs, key)?;
    config.alpn_protocols = alpns.iter().map(|a| a.to_vec()).collect();
    Ok(Arc::new(config))
}

/// Client-side (initiator) configuration.
///
/// # Errors
/// The identity's key or certificate was rejected by rustls.
pub fn client_config(identity: &Identity, alpn: &[u8]) -> Result<Arc<rustls::ClientConfig>, Error> {
    let provider = provider();
    let (certs, key) = identity_parts(identity);
    let mut config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AcceptAnyPeer(provider)))
        .with_client_auth_cert(certs, key)?;
    config.alpn_protocols = vec![alpn.to_vec()];
    Ok(Arc::new(config))
}

/// The pin of the peer on the other end of an established connection.
#[must_use]
pub fn peer_spki_sha256(peer_certificates: Option<&[CertificateDer<'_>]>) -> Option<[u8; 32]> {
    spki_sha256(peer_certificates?.first()?)
}
