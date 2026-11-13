//! The fleet of devices one person's identity has certified: the certificates it issued and the
//! revocations it signed, and what they say of a device.
//!
//! A host keeps it in the identity directory ([`store`]); every host holds the same document and
//! the same rules over it.

use serde::{Deserialize, Serialize};
use tilo_user_identity::{DeviceCert, RevocationEntry, verify_device_cert_at};

#[cfg(not(target_arch = "wasm32"))]
mod store;
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) use store::loads_on_this_thread;
#[cfg(not(target_arch = "wasm32"))]
pub use store::{DeviceFleetStore, FleetStamps};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use store::{FileStamp, SETTLE};

pub(crate) const FLEET_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFleet {
    pub schema_version: u32,
    pub identity_id: String,
    pub certificates: Vec<DeviceCert>,
    pub revocations: Vec<RevocationEntry>,
    /// Enrollment nonces an older build recorded here. Redeemed nonces now
    /// live in [`crate::enrollment::NonceStore`], which the nearby-pairing
    /// broker shares; the next redemption moves these there and saves the
    /// fleet without them. Omitted while empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) consumed_enrollments: Vec<ConsumedEnrollment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConsumedEnrollment {
    pub(crate) nonce: [u8; 16],
}

impl DeviceFleet {
    /// Check that this is a fleet of `identity_id` a verifier takes: of the
    /// current schema, of that identity, and with every certificate and
    /// revocation in it verifying under it.
    ///
    /// # Errors
    ///
    /// Returns an error when the schema version or identity id is another,
    /// or a stored record does not verify.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn check(&self, identity_id: &str) -> std::io::Result<()> {
        if self.schema_version != FLEET_SCHEMA_VERSION || self.identity_id != identity_id {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "device fleet identity/schema mismatch",
            ));
        }
        for cert in &self.certificates {
            tilo_user_identity::verify_device_cert(cert)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        }
        for entry in &self.revocations {
            tilo_user_identity::verify_revocation_entry(entry)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        }
        Ok(())
    }

    /// An empty fleet of `identity_id`: no certificates, revocations or
    /// consumed enrollments.
    #[must_use]
    pub fn new(identity_id: String) -> Self {
        Self {
            schema_version: FLEET_SCHEMA_VERSION,
            identity_id,
            certificates: Vec::new(),
            revocations: Vec::new(),
            consumed_enrollments: Vec::new(),
        }
    }

    pub fn is_revoked(&self, device_public_key: &[u8; 32]) -> bool {
        self.revocations
            .iter()
            .any(|entry| &entry.revoked_device_public_key == device_public_key)
    }

    /// Whether this fleet's revocations refuse `cert`: its device's, or its
    /// stand-in issuer's from before it issued it
    /// ([`tilo_user_identity::revocation::cert_refused`]).
    #[must_use]
    pub fn refuses(&self, cert: &tilo_user_identity::DeviceCert) -> bool {
        tilo_user_identity::revocation::cert_refused(cert, None, |root, key| {
            self.revocations
                .iter()
                .filter(|entry| {
                    (&entry.user_public_key, &entry.revoked_device_public_key) == (root, key)
                })
                .map(|entry| entry.revoked_at_ns)
                .min()
        })
    }

    /// The person's own devices this fleet certifies at `now_ns`: personal
    /// devices ([`DeviceCapabilities::is_personal_device`]) of `person`,
    /// unexpired and not revoked.
    #[must_use]
    pub fn personal_devices(&self, person: &[u8; 32], now_ns: u64) -> Vec<[u8; 32]> {
        self.certificates
            .iter()
            .filter(|cert| {
                &cert.user_public_key == person
                    && cert.capabilities.is_personal_device()
                    && verify_device_cert_at(cert, now_ns).is_ok()
                    && !self.refuses(cert)
            })
            .map(|cert| cert.device_public_key)
            .collect()
    }
}
