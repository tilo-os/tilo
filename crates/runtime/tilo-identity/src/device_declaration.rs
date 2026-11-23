//! The public facts that name one enrolled device, for every host to keep and compare: what an
//! installer declares about the device it carries, and the receipt of a sealed transport of it.
//! Neither admits an identity; the transport itself ([`crate::enrolled_transport`]) runs where
//! a root installer does.

use serde::{Deserialize, Serialize};

/// Public comparison facts; deserializing these never admits an identity.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentDeviceDeclaration {
    pub(crate) schema: u32,
    pub(crate) user_public_key: [u8; 32],
    pub(crate) device_public_key: [u8; 32],
    pub(crate) record_name: String,
    pub(crate) record_sha256: [u8; 32],
    pub(crate) certificate_sha256: [u8; 32],
    pub(crate) fleet_sha256: [u8; 32],
}

impl CurrentDeviceDeclaration {
    #[must_use]
    pub const fn public_key(&self) -> [u8; 32] {
        self.user_public_key
    }
    #[must_use]
    pub const fn device_public_key(&self) -> [u8; 32] {
        self.device_public_key
    }
    #[must_use]
    pub fn record_name(&self) -> &str {
        &self.record_name
    }
}

/// Non-secret proof identifying the one device carried by an authenticated artifact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportReceipt {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub sealed_bytes: u64,
}
