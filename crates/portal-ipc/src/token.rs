// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! The per-user token that authenticates local clients.
//!
//! The daemon writes a random token into a file only the user can read; a client proves it can
//! read the file by sending the token in its hello. That works the same on every OS and does
//! not depend on socket or pipe permissions alone.

use std::fs;
use std::io;
use std::path::Path;

use ring::rand::{SecureRandom, SystemRandom};

/// Read the token, creating a new random one if the file does not exist.
///
/// # Errors
/// I/O failures or an unusable random number generator.
pub fn load_or_create(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(token) if token.trim().len() >= 32 => return Ok(token.trim().to_owned()),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| io::Error::other("no randomness"))?;
    let token = bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
        s
    });
    write_private(path, token.as_bytes())?;
    Ok(token)
}

/// Read an existing token (client side).
///
/// # Errors
/// The file is missing or unreadable.
pub fn read(path: &Path) -> io::Result<String> {
    Ok(fs::read_to_string(path)?.trim().to_owned())
}

/// Constant-time comparison.
#[must_use]
pub fn matches(expected: &str, offered: &str) -> bool {
    // The length of a token is not secret; its content must not leak through timing.
    expected.len() == offered.len()
        && expected
            .bytes()
            .zip(offered.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    use std::io::Write as _;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_is_created_once_and_reused() {
        let dir = std::env::temp_dir().join(format!("portalswitch-token-{}", std::process::id()));
        let path = dir.join("ipc.token");
        let first = load_or_create(&path).unwrap();
        assert_eq!(first.len(), 64);
        assert_eq!(load_or_create(&path).unwrap(), first);
        assert_eq!(read(&path).unwrap(), first);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o077, 0);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn comparison_rejects_differences() {
        assert!(matches("abc", "abc"));
        assert!(!matches("abc", "abd"));
        assert!(!matches("abc", "ab"));
    }
}
