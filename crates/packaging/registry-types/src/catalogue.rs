//! What `app list --json` prints: the applications the configured
//! registries offer that are not installed here. The catalogue binary
//! produces it and the compositor reads it without linking the HTTP client.

use serde::{Deserialize, Serialize};

/// One application a registry offers that is not installed here.
///
/// The wire shape `app list --json` prints and the compositor parses —
/// one definition, so the two sides cannot drift apart.
/// Everything on it is metadata a signed registry entry carried.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogueApp {
    /// Reverse-DNS package id — the same identity `tilo-install` records once this is installed.
    pub package_id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    /// Publisher's short localized tagline.
    #[serde(default)]
    pub tagline: String,
    /// Locale-selected store artwork.
    #[serde(default)]
    pub images: Vec<crate::RegistryImage>,
    /// Canonical language tags the app itself supports.
    #[serde(default)]
    pub supported_languages: Vec<String>,
    /// First declared category, when any; matches an installed app's single `category`.
    #[serde(default)]
    pub category: Option<String>,
    /// The newest version the registry advertises.
    pub version: String,
    /// Which configured registry offered it.
    pub registry: String,
    /// Where the `.tapp` is fetched from.
    pub download_url: String,
    /// Signed-root/package-id hash, covered by the registry signature.
    #[serde(default)]
    pub tapp_root_hash_hex: String,
    /// Expected download size, covered by the registry signature.
    #[serde(default)]
    pub tapp_size_bytes: u64,
    /// BLAKE3 of the exact downloaded `.tapp` bytes.
    #[serde(default)]
    pub tapp_blake3_hex: String,
    /// Signed execution target metadata.
    #[serde(default)]
    pub architecture: String,
    #[serde(default)]
    pub isa: String,
    #[serde(default)]
    pub runtime: Option<String>,
    /// Publisher key the downloaded tapp must be signed by.
    #[serde(default)]
    pub publisher_pubkey_hex: String,
    /// `"publisher-signed"` means publisher and pinned-owner snapshot proofs
    /// both verified. Untrusted entries never become a `CatalogueApp`.
    pub trust: String,
}

/// The JSON envelope `app list --json` prints.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CatalogueListing {
    #[serde(default)]
    pub apps: Vec<CatalogueApp>,
    /// Registries configured but unreadable, with the reason — reported
    /// rather than swallowed so a caller can tell "nothing available"
    /// from "we couldn't find out".
    #[serde(default)]
    pub unreachable: Vec<UnreachableRegistry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnreachableRegistry {
    pub name: String,
    pub detail: String,
}
