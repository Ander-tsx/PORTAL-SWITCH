// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Locating the `SubjectPublicKeyInfo` inside an X.509 certificate.
//!
//! Peers are pinned by the SHA-256 of their SPKI (protocol §9). Only the structure we need is
//! parsed: `Certificate ::= SEQUENCE { tbsCertificate SEQUENCE { [0] version?, serial,
//! signature, issuer, validity, subject, subjectPublicKeyInfo, … }, … }`. All reads are bounds
//! checked; malformed input yields `None`.

/// A DER element: tag, content and total encoded length.
struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
    total: usize,
}

fn read_tlv(input: &[u8]) -> Option<Tlv<'_>> {
    let tag = *input.first()?;
    let first = *input.get(1)?;
    let (len, header) = if first < 0x80 {
        (usize::from(first), 2)
    } else {
        let n = usize::from(first & 0x7f);
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for byte in input.get(2..2 + n)? {
            len = (len << 8) | usize::from(*byte);
        }
        (len, 2 + n)
    };
    let total = header.checked_add(len)?;
    Some(Tlv {
        tag,
        content: input.get(header..total)?,
        total,
    })
}

/// The complete DER encoding (tag, length, content) of the certificate's SPKI.
#[must_use]
pub fn spki_der(cert_der: &[u8]) -> Option<&[u8]> {
    let cert = read_tlv(cert_der)?;
    if cert.tag != 0x30 {
        return None;
    }
    let tbs = read_tlv(cert.content)?;
    if tbs.tag != 0x30 {
        return None;
    }
    let mut rest = tbs.content;
    // Optional [0] EXPLICIT version.
    if let Some(first) = read_tlv(rest)
        && first.tag == 0xa0
    {
        rest = &rest[first.total..];
    }
    // serial, signature, issuer, validity, subject
    for _ in 0..5 {
        let element = read_tlv(rest)?;
        rest = &rest[element.total..];
    }
    let spki = read_tlv(rest)?;
    (spki.tag == 0x30).then(|| &rest[..spki.total])
}

/// SHA-256 of the certificate's SPKI: the pin for that machine.
#[must_use]
pub fn spki_sha256(cert_der: &[u8]) -> Option<[u8; 32]> {
    let spki = spki_der(cert_der)?;
    let digest = ring::digest::digest(&ring::digest::SHA256, spki);
    let mut out = [0u8; 32];
    out.copy_from_slice(digest.as_ref());
    Some(out)
}

#[cfg(test)]
mod tests {
    use rcgen::{CertificateParams, KeyPair, PKCS_ED25519, PublicKeyData as _};

    use super::*;

    #[test]
    fn finds_the_public_key_of_a_generated_certificate() {
        let key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let cert = CertificateParams::new(vec!["x".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let spki = spki_der(cert.der()).unwrap();
        assert_eq!(spki, key.subject_public_key_info().as_slice());
    }

    #[test]
    fn malformed_input_never_panics() {
        let key = KeyPair::generate_for(&PKCS_ED25519).unwrap();
        let cert = CertificateParams::new(vec!["x".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        let der = cert.der().to_vec();
        // Every truncation and a sweep of single-byte corruptions.
        for cut in 0..der.len() {
            let _ = spki_der(&der[..cut]);
        }
        for i in 0..der.len() {
            for value in [0x00, 0x30, 0x80, 0x84, 0xff] {
                let mut copy = der.clone();
                copy[i] = value;
                let _ = spki_der(&copy);
            }
        }
        assert!(spki_der(&[]).is_none());
        assert!(spki_der(&[0x30, 0x84, 0xff, 0xff, 0xff, 0xff]).is_none());
    }
}
