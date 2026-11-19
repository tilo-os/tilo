#![allow(
    clippy::missing_errors_doc,
    clippy::significant_drop_tightening,
    reason = "migration stack lint debt is scoped to this target"
)]

pub use tilo_identity_client::{
    AsyncBrokerSigningIdentity, BrokerMetadata, BrokerSigningIdentity, IDENTITY_SERVICE,
    IdentityMetadata, IdentitySummary, KeyProtection, MAX_PHOTO_BYTES, MAX_PHOTO_DIMENSION,
    NotAvailable, ProfilePhoto, bound_profile_photo, collaboration_join_digest, load,
    load_bound_metadata, load_metadata, load_metadata_from_path, resolve_active_identity, wire,
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
