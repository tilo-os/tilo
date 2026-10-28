//! Source-held handoff for an installation prepared on another computer.
//!
//! The medium carries the ordinary encrypted preparation package and a small
//! public descriptor. The descriptor names one opaque peer ticket, the package
//! digest and the package's person identity; it contains no path, profile
//! name, credential or content key. The preparation computer stays available
//! and, after the destination has a real device identity, approves that exact
//! device and delivers the package over the authenticated peer transport.
//!
//! The package keeps its existing password envelope and the existing
//! first-boot consumer. The password is generated for this operation and
//! sealed with HPKE to an ephemeral X25519 key. The destination proves that
//! key and its certified device key together, in a signature bound to the
//! operation, the descriptor, the package and the person identity. The source
//! releases no key before a person approves that exact request. The source
//! reports the package as imported only after the destination's canonical
//! importer returns a receipt for the exact package digest; that is a package
//! receipt, not a claim that Home or application migration has finished.
//!
//! This crate contains that transport and the native preflight for the
//! prepared setup's public TPM sidecar. The installer and the first-boot
//! importer own the package and its canonical adoption; the shell does not
//! link the whole installer backend to receive one package, and the installer's
//! extractor verifies the sidecar here without depending on that backend.

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

/// The endpoint-bound rendezvous the preparation computer publishes before the
/// medium is written. It is portable metadata; only a Unix-hosted source can
/// bind and serve the endpoint.
#[derive(Clone, Debug)]
pub struct HandoffIdentity {
    pub operation_id: String,
    pub ticket: String,
    /// The person identity the prepared package carries. The destination
    /// refuses a descriptor for any other already-active identity.
    pub identity_public_key: [u8; 32],
    pub expires_at_ns: u64,
}

/// What the preparing computer tells its person while it serves the package.
/// This is portable presentation state; only a Unix-hosted source can bind and
/// serve the endpoint that produces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceState {
    Waiting,
    Requested {
        label: String,
        code: String,
        approval: String,
    },
    /// A fresh destination asked to join this person before it can receive
    /// the package. The person compares this code with the destination.
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
    /// The destination's canonical importer confirmed this exact package.
    /// Home and application migration can still be finishing there.
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
/// One frame is chunk data plus its envelope. The package itself is bounded by
/// the existing prepared-setup limit.
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

/// The endpoint-bound rendezvous published on the medium. It is not a bearer
/// capability: the source authenticates and a person approves every receiver.
/// Every field is public metadata; it names no source path, credential, token
/// or content key.
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
    /// Bind the reviewed descriptor to the exact delivered ciphertext.
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

    /// A descriptor is not usable after its operation window closes.
    pub fn valid_at(&self, now_ns: u64) -> bool {
        self.valid() && now_ns < self.expires_at_ns
    }

    /// Decode the exact package digest published in this descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error when the descriptor's digest is not 32 hex-encoded
    /// bytes.
    pub fn digest(&self) -> io::Result<[u8; 32]> {
        let bytes =
            hex::decode(&self.package_sha256).map_err(|_| invalid("Invalid package digest"))?;
        bytes
            .try_into()
            .map_err(|_| invalid("Invalid package digest"))
    }

    /// The descriptor as it is published on the medium. It has no private
    /// source path, profile name, token or content key.
    /// Encode the descriptor as published on the medium.
    ///
    /// # Errors
    ///
    /// Refuses an invalid descriptor or a value that cannot be encoded.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        if !self.valid() {
            return Err(invalid("Invalid other-computer handoff"));
        }
        serde_json::to_vec(self).map_err(|_| invalid("Invalid other-computer handoff"))
    }

    /// Decode a bounded descriptor from the medium.
    ///
    /// # Errors
    ///
    /// Refuses an oversized, malformed or invalid descriptor.
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
    // An iroh endpoint ticket is JSON naming the endpoint id and its relay and
    // direct addresses; relay URLs contain `/`. It is opaque public routing
    // data, never a filesystem path, so accept every graphic ASCII byte and
    // let the peer endpoint reject a ticket that does not parse.
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

/// The exact package, operation and person a sealed package password belongs
/// to. HPKE already binds its recipient key; this binds the plaintext to the
/// delivery the person approved.
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

/// A short human-comparable authenticator. Source and destination derive the
/// same value from the operation, the certified device and its ephemeral key,
/// the exact package and the person identity; the person compares the two
/// screens before approving.
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

/// Native verification of the prepared setup's public TPM sidecar.
#[cfg(unix)]
pub mod prepared;

#[cfg(all(test, feature = "peer"))]
mod tests;
