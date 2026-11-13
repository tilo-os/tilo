//! Source-held handoff for an installation prepared on another computer.
//!
//! The medium carries the ordinary encrypted preparation package and a small
//! public descriptor naming one opaque peer ticket, the package digest and the
//! person identity, and no path, profile name, credential or content key. After
//! the destination has a real device identity, the preparing computer approves
//! that exact device and delivers the package over the authenticated peer
//! transport.
//!
//! The password is generated for this operation and sealed with HPKE to an
//! ephemeral X25519 key, so it leaves only to the device the person approved.

use serde::{Deserialize, Serialize};
use std::io;

#[cfg(all(unix, feature = "peer"))]
mod bootstrap;
#[cfg(all(unix, feature = "peer"))]
pub mod receiver;
#[cfg(all(unix, feature = "peer"))]
pub mod transport;

#[cfg(all(unix, feature = "peer"))]
pub use bootstrap::{SourceIdentity, public_identity_record};
#[cfg(all(unix, feature = "peer"))]
pub use receiver::{Received, Receiver, ReceiverIdentity};
#[cfg(all(unix, feature = "peer"))]
pub use transport::Source;

/// The endpoint-bound rendezvous the preparing computer publishes before the medium
/// is written: portable metadata, which only a Unix-hosted source can bind and
/// serve.
#[derive(Clone, Debug)]
pub struct HandoffIdentity {
    pub operation_id: String,
    pub ticket: String,
    pub identity_public_key: [u8; 32],
    pub expires_at_ns: u64,
}

/// What the preparing computer tells its person while it serves the package.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    Waiting,
    Requested {
        label: String,
        code: String,
        approval: String,
    },
    JoinRequested {
        label: String,
        code: String,
        approval: String,
    },
    Joining {
        label: String,
    },
    Sending {
        label: String,
        completed: u64,
        total: u64,
    },
    AwaitingReceipt {
        label: String,
    },
    PackageImported {
        label: String,
    },
    Rejected,
    Failed,
    Cancelled,
    Expired,
}

pub const SCHEMA: u32 = 2;
pub const DIRECTORY: &str = ".tilo-installer";
pub const DESCRIPTOR_FILE: &str = "other-device.json";
pub const ALPN: &[u8] = b"tilo-migration-handoff/2";
pub const MAX_FRAME: usize = 2 * 1024 * 1024;
pub const CHUNK: usize = 512 * 1024;
pub const MAX_PACKAGE: u64 = 32 * 1024 * 1024 + 16 * 1024;
pub const MAX_TICKET: usize = 4096;
pub const MAX_DESCRIPTOR: u64 = 16 * 1024;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn valid_operation(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicHandoff {
    pub schema: u32,
    pub operation_id: String,
    pub ticket: String,
    pub identity_public_key: [u8; 32],
    pub expires_at_ns: u64,
    pub package_sha256: String,
    pub package_bytes: u64,
}

impl PublicHandoff {
    pub fn from_identity(
        identity: &HandoffIdentity,
        package_sha256: impl Into<String>,
        package_bytes: u64,
    ) -> Self {
        Self {
            schema: SCHEMA,
            operation_id: identity.operation_id.clone(),
            ticket: identity.ticket.clone(),
            identity_public_key: identity.identity_public_key,
            expires_at_ns: identity.expires_at_ns,
            package_sha256: package_sha256.into(),
            package_bytes,
        }
    }

    pub fn valid(&self) -> bool {
        self.schema == SCHEMA
            && valid_operation(&self.operation_id)
            && valid_ticket(&self.ticket)
            && self.identity_public_key != [0; 32]
            && self.expires_at_ns != 0
            && self.package_sha256.len() == 64
            && self
                .package_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
            && (48..=MAX_PACKAGE).contains(&self.package_bytes)
    }

    pub fn valid_at(&self, now_ns: u64) -> bool {
        self.valid() && now_ns < self.expires_at_ns
    }

    /// Decode the exact package digest published in this descriptor.
    ///
    /// # Errors
    ///
    /// When the descriptor's digest is not 32 hex-encoded bytes.
    pub fn digest(&self) -> io::Result<[u8; 32]> {
        let bytes =
            hex::decode(&self.package_sha256).map_err(|_| invalid("Invalid package digest"))?;
        bytes
            .try_into()
            .map_err(|_| invalid("Invalid package digest"))
    }

    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(invalid("Invalid other-computer handoff"));
        }
        serde_json::to_vec(self).map_err(|_| invalid("Invalid other-computer handoff"))
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() as u64 > MAX_DESCRIPTOR {
            return Err(invalid("Other-computer handoff is too large"));
        }
        let value: Self =
            serde_json::from_slice(bytes).map_err(|_| invalid("Invalid other-computer handoff"))?;
        value
            .valid()
            .then_some(value)
            .ok_or_else(|| invalid("Invalid other-computer handoff"))
    }
}

pub(crate) fn valid_ticket(ticket: &str) -> bool {
    // An iroh endpoint ticket is JSON naming the endpoint id and its relay and direct
    // addresses, and relay URLs contain `/`; it is opaque public routing data, never a
    // filesystem path, so the peer endpoint rejects one that does not parse.
    !ticket.is_empty()
        && ticket.len() <= MAX_TICKET
        && ticket.bytes().all(|byte| byte.is_ascii_graphic())
}

/// SHA-256 of one frame's bytes, used to reject a torn or reordered chunk.
#[cfg(feature = "peer")]
pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(bytes).into()
}

#[cfg(feature = "peer")]
pub(crate) fn handoff_aad(
    operation_id: &str,
    package_sha256: &[u8; 32],
    identity_public_key: &[u8; 32],
) -> Vec<u8> {
    let mut aad = b"tilo-migration-handoff-hpke-v2\0".to_vec();
    aad.extend_from_slice(operation_id.as_bytes());
    aad.extend_from_slice(package_sha256);
    aad.extend_from_slice(identity_public_key);
    aad
}

#[cfg(feature = "peer")]
pub(crate) fn verification_code(
    operation_id: &str,
    device_public_key: &[u8; 32],
    ephemeral_public_key: &[u8; 32],
    package_sha256: &[u8; 32],
    identity_public_key: &[u8; 32],
) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"tilo-migration-handoff-code-v2\0");
    hasher.update(operation_id.as_bytes());
    hasher.update(device_public_key);
    hasher.update(ephemeral_public_key);
    hasher.update(package_sha256);
    hasher.update(identity_public_key);
    let bytes = <[u8; 32]>::from(hasher.finalize());
    let value = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    format!("{:06}", value % 1_000_000)
}

#[cfg(unix)]
mod descriptor;
#[cfg(unix)]
pub use descriptor::read_descriptor;

#[cfg(unix)]
pub mod prepared;

#[cfg(all(test, feature = "peer"))]
mod tests;
