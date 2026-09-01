use std::fmt;
use std::net::SocketAddr;
use std::time::Duration;

use thiserror::Error;

use crate::{Catalog, CatalogError, LicenseState};

/// A secret whose formatter never exposes its contents and whose allocation is
/// cleared before release. It is deliberately not `Clone`.
pub struct Secret(Vec<u8>);

impl Secret {
    #[must_use]
    pub fn new(bytes: impl Into<Vec<u8>>) -> Self {
        Self(bytes.into())
    }

    #[must_use]
    pub fn expose(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentHealth {
    pub protocol_major: u16,
    pub guest_boot_id: String,
    pub catalog_generation: u64,
    pub license: LicenseState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GuestSession {
    /// Opaque session identifier issued by the mutually authenticated bridge.
    pub id: String,
    pub health: AgentHealth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RdpEndpoint {
    pub address: SocketAddr,
    pub username: String,
    /// SHA-256 certificate fingerprint as 64 hexadecimal characters.
    pub certificate_sha256: String,
}

/// One single-use `RemoteApp` launch prepared by the guest bridge.
#[derive(Debug)]
pub struct GuestLaunch {
    pub endpoint: RdpEndpoint,
    pub password: Secret,
    pub remote_app_alias: String,
    pub launch_nonce: String,
}

/// Authenticated bridge to the qOS guest service. Implementations are expected
/// to use `AF_VSOCK` or an equivalently host-confined transport; the public RDP
/// service is never trusted to publish the catalogue.
pub trait GuestAgent: Send + Sync {
    fn wait_ready(&self, timeout: Duration) -> Result<GuestSession, AgentError>;

    fn catalog(&self, session: &GuestSession) -> Result<Catalog, AgentError>;

    fn prepare_launch(
        &self,
        session: &GuestSession,
        application_id: &str,
    ) -> Result<GuestLaunch, AgentError>;
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("the authenticated Windows guest bridge was not available before the deadline")]
    Unavailable,
    #[error("guest bridge protocol {actual} is incompatible with host protocol {expected}")]
    ProtocolMismatch { expected: u16, actual: u16 },
    #[error("guest bridge authentication failed")]
    Authentication,
    #[error("guest bridge returned an invalid catalogue: {0}")]
    InvalidCatalog(#[from] CatalogError),
    #[error("guest bridge refused the application publication {0:?}")]
    NotPublished(String),
    #[error("guest bridge returned an invalid RemoteApp endpoint")]
    InvalidEndpoint,
    #[error("guest bridge transport failed: {0}")]
    Transport(String),
}
