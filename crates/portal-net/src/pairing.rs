// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Pairing codes (protocol §9) and attempt rate limiting.
//!
//! Both machines derive the same six digits from the TLS session and their two public keys. A
//! man in the middle terminates two different TLS sessions, so the codes shown differ.

use std::collections::HashMap;
use std::net::IpAddr;

use portal_core::gate::Micros;

use crate::tls::PAIRING_EXPORTER_LABEL;

/// Context passed to the TLS exporter: the two SPKI hashes in byte order.
#[must_use]
pub fn exporter_context(a: &[u8; 32], b: &[u8; 32]) -> [u8; 64] {
    let (low, high) = if a <= b { (a, b) } else { (b, a) };
    let mut out = [0u8; 64];
    out[..32].copy_from_slice(low);
    out[32..].copy_from_slice(high);
    out
}

/// Exporter label used to seed the code.
#[must_use]
pub const fn exporter_label() -> &'static [u8] {
    PAIRING_EXPORTER_LABEL
}

/// Turn the 8 exported bytes into the six digit code shown to the user.
#[must_use]
pub fn code_from_exported(exported: [u8; 8]) -> String {
    format!("{:06}", u64::from_le_bytes(exported) % 1_000_000)
}

/// Failed pairing attempts allowed per address before a lockout.
pub const MAX_FAILURES: usize = 3;
/// Length of the lockout and of the window failures are counted in.
pub const LOCKOUT_US: Micros = 60_000_000;

/// "3 failures per remote address → 60 s lockout".
#[derive(Debug, Default)]
pub struct PairingLimiter {
    failures: HashMap<IpAddr, Vec<Micros>>,
}

impl PairingLimiter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// May `addr` start a pairing attempt now?
    #[must_use]
    pub fn allowed(&mut self, addr: IpAddr, now: Micros) -> bool {
        self.prune(addr, now);
        self.failures
            .get(&addr)
            .is_none_or(|f| f.len() < MAX_FAILURES)
    }

    /// Record a rejected or mismatched attempt.
    pub fn record_failure(&mut self, addr: IpAddr, now: Micros) {
        self.prune(addr, now);
        self.failures.entry(addr).or_default().push(now);
    }

    /// A successful pairing clears the address's history.
    pub fn record_success(&mut self, addr: IpAddr) {
        self.failures.remove(&addr);
    }

    fn prune(&mut self, addr: IpAddr, now: Micros) {
        if let Some(list) = self.failures.get_mut(&addr) {
            list.retain(|t| now.saturating_sub(*t) < LOCKOUT_US);
            if list.is_empty() {
                self.failures.remove(&addr);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use super::*;

    const A: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
    const B: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 11));

    #[test]
    fn code_is_six_digits_with_leading_zeros() {
        assert_eq!(code_from_exported(1u64.to_le_bytes()), "000001");
        assert_eq!(code_from_exported(1_234_567u64.to_le_bytes()), "234567");
        assert_eq!(code_from_exported([0; 8]).len(), 6);
    }

    #[test]
    fn context_is_order_independent() {
        let (a, b) = ([1u8; 32], [2u8; 32]);
        assert_eq!(exporter_context(&a, &b), exporter_context(&b, &a));
    }

    #[test]
    fn three_failures_lock_out_for_a_minute() {
        let mut limiter = PairingLimiter::new();
        for t in 0..3 {
            assert!(limiter.allowed(A, t));
            limiter.record_failure(A, t);
        }
        assert!(!limiter.allowed(A, 1_000));
        assert!(limiter.allowed(B, 1_000), "other addresses are unaffected");
        assert!(limiter.allowed(A, LOCKOUT_US + 3));
    }

    #[test]
    fn success_clears_history() {
        let mut limiter = PairingLimiter::new();
        limiter.record_failure(A, 0);
        limiter.record_failure(A, 1);
        limiter.record_success(A);
        limiter.record_failure(A, 2);
        assert!(limiter.allowed(A, 3));
    }
}
