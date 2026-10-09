// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 The PortalSwitch Authors

//! Platform transport: a Unix domain socket on macOS/Linux, a named pipe on Windows.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncRead, AsyncWrite};

/// Any bidirectional byte stream.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

/// A boxed stream.
pub type BoxIo = Box<dyn Io>;

/// Where the daemon listens.
#[derive(Debug, Clone)]
pub struct Endpoint {
    /// Unix socket path, or the full `\\.\pipe\…` name on Windows.
    pub path: PathBuf,
}

impl Endpoint {
    /// The default endpoint for the user whose data lives in `dir`.
    #[must_use]
    pub fn in_dir(dir: &Path) -> Self {
        #[cfg(windows)]
        {
            // Pipe names live in a global namespace: include the directory so two users (or two
            // test instances) never collide.
            let tag: String = dir
                .to_string_lossy()
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect();
            Self {
                path: PathBuf::from(format!(r"\\.\pipe\portalswitch-{tag}")),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                path: dir.join("portald.sock"),
            }
        }
    }
}

#[cfg(unix)]
mod imp {
    use std::os::unix::fs::PermissionsExt as _;

    use tokio::net::{UnixListener, UnixStream};

    use super::{BoxIo, Endpoint, io};

    /// Accepts local connections.
    #[derive(Debug)]
    pub struct Listener {
        inner: UnixListener,
    }

    impl Listener {
        /// Bind, replacing a stale socket file but refusing to steal a live one.
        ///
        /// # Errors
        /// Another daemon is listening, or the socket cannot be created.
        pub async fn bind(endpoint: &Endpoint) -> io::Result<Self> {
            if let Some(dir) = endpoint.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            if endpoint.path.exists() {
                if UnixStream::connect(&endpoint.path).await.is_ok() {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        "another PortalSwitch daemon is already running",
                    ));
                }
                std::fs::remove_file(&endpoint.path)?;
            }
            let inner = UnixListener::bind(&endpoint.path)?;
            std::fs::set_permissions(&endpoint.path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self { inner })
        }

        /// Wait for the next client.
        ///
        /// # Errors
        /// The listener failed.
        pub async fn accept(&mut self) -> io::Result<BoxIo> {
            Ok(Box::new(self.inner.accept().await?.0))
        }
    }

    /// Connect to a running daemon.
    ///
    /// # Errors
    /// The daemon is not running.
    pub async fn connect(endpoint: &Endpoint) -> io::Result<BoxIo> {
        Ok(Box::new(UnixStream::connect(&endpoint.path).await?))
    }
}

#[cfg(windows)]
mod imp {
    use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeServer, ServerOptions};

    use super::{BoxIo, Endpoint, io};

    /// Accepts local connections.
    #[derive(Debug)]
    pub struct Listener {
        name: std::path::PathBuf,
        next: NamedPipeServer,
    }

    fn create(name: &std::path::Path, first: bool) -> io::Result<NamedPipeServer> {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(name)
    }

    impl Listener {
        /// Create the pipe.
        ///
        /// # Errors
        /// Another daemon owns the pipe, or it cannot be created.
        pub async fn bind(endpoint: &Endpoint) -> io::Result<Self> {
            let next = create(&endpoint.path, true)?;
            Ok(Self {
                name: endpoint.path.clone(),
                next,
            })
        }

        /// Wait for the next client.
        ///
        /// # Errors
        /// The listener failed.
        pub async fn accept(&mut self) -> io::Result<BoxIo> {
            self.next.connect().await?;
            let replacement = create(&self.name, false)?;
            Ok(Box::new(std::mem::replace(&mut self.next, replacement)))
        }
    }

    /// Connect to a running daemon.
    ///
    /// # Errors
    /// The daemon is not running.
    pub async fn connect(endpoint: &Endpoint) -> io::Result<BoxIo> {
        Ok(Box::new(ClientOptions::new().open(&endpoint.path)?))
    }
}

pub use imp::{Listener, connect};
