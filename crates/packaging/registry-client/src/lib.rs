//! A publisher's side of the Tilo registry: keys, signed entries, and the
//! registry's HTTP API for uploads, entries and publisher applications.
//!
//! `tilo-app publish`, `tilo-app apply` and the App Publisher app use this;
//! so can any CI that would rather not shell out.

pub mod client;
pub mod entry;
pub mod key;
pub mod publish;

pub use client::{DEFAULT_REGISTRY, REGISTRY_ENV, RegistryClient};
pub use entry::{EntryMetadata, Platform, build_entry, check_package};
pub use publish::{Published, publish};

use anyhow::Result;
use ed25519_dalek::SigningKey;
use registry_types::application::{NamespaceClaim, PublisherApplication};

/// Sign an application for `claims` and send it. Answers its status.
///
/// # Errors
///
/// When the registry refuses it; the message names any missing proof.
pub fn apply(
    client: &RegistryClient,
    key: &SigningKey,
    claims: Vec<NamespaceClaim>,
    contact: String,
    note: String,
) -> Result<serde_json::Value> {
    let mut application = PublisherApplication {
        pubkey_hex: String::new(),
        claims,
        contact,
        note,
        submitted_at_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
        signature_hex: String::new(),
    };
    application.sign(key);
    client.apply(&application)
}
