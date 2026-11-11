#![allow(
    clippy::missing_errors_doc,
    clippy::significant_drop_tightening,
    reason = "migration stack lint debt is scoped to this target"
)]

pub use tilo_identity_client::{
    AsyncBrokerSigningIdentity, BrokerMetadata, BrokerSigningIdentity, IDENTITY_SERVICE,
    IdentityMetadata, IdentitySummary, KeyProtection, MAX_PHOTO_BYTES, MAX_PHOTO_DIMENSION,
    NotAvailable, ProfilePhoto, bound_profile_photo, collaboration_join_signing_bytes,
    load_bound_metadata, wire,
};

#[cfg(test)]
#[path = "test_wire.rs"]
mod client;

#[cfg(unix)]
pub mod connected;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::*;

/// Outside Unix (the browser) no identity service is ever handed to an app,
/// so there is nothing to load; callers fall back as they do outside Tilo.
///
/// # Errors
///
/// Always [`NotAvailable::ServiceFdMissing`].
#[cfg(not(unix))]
pub fn load() -> Result<BrokerSigningIdentity, NotAvailable> {
    Err(NotAvailable::ServiceFdMissing)
}

/// Outside Unix no identity service is handed to an app and no record is
/// kept on disk.
#[cfg(not(unix))]
pub use tilo_identity_client::{load_metadata, load_metadata_from_path, resolve_active_identity};
