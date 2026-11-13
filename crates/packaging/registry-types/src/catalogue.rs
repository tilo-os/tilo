//! What `app list --json` prints: the applications the configured registries offer that are not
//! installed here. The catalogue binary produces it; the compositor reads it without linking the
//! HTTP client.

use serde::{Deserialize, Serialize};

/// One application a registry offers that is not installed here: metadata a signed entry carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueApp {
    pub package_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tagline: String,
    #[serde(default)]
    pub images: Vec<crate::RegistryImage>,
    #[serde(default)]
    pub supported_languages: Vec<String>,
    /// The first declared category; an installed app has one.
    #[serde(default)]
    pub category: Option<String>,
    pub version: String,
    pub registry: String,
    pub download_url: String,
    /// The signed package root, covered by the registry signature.
    #[serde(default)]
    pub tapp_root_hash_hex: String,
    /// Covered by the registry signature, like the root below.
    #[serde(default)]
    pub tapp_size_bytes: u64,
    /// BLAKE3 of the exact `.tapp` bytes.
    #[serde(default)]
    pub tapp_blake3_hex: String,
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub isa: String,
    #[serde(default)]
    pub runtime: Option<String>,
    /// The key the downloaded `.tapp` must be signed by.
    #[serde(default)]
    pub publisher_pubkey_hex: String,
    /// `"publisher-signed"` means the publisher's and the pinned owner's snapshot proofs both
    /// verified. Untrusted entries never become a `CatalogueApp`.
    pub trust: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    /// Registries configured but unreadable, with the reason, so a caller can tell "nothing
    /// available" from "could not find out".
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
}
