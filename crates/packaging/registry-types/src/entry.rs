//! `RegistryEntry` and the canonical signing form.
//!
//! The publisher signs `blake3(ENTRY_DOMAIN ||
//! postcard(entry_signing_payload))`, where `entry_signing_payload`
//! mirrors [`RegistryEntry`] *without* `signature_hex` — excluding it
//! avoids a fixed-point loop (the signature must not cover itself).
//!
//! Verifiers reconstruct the same bytes by peeling `signature_hex` off
//! the received entry, re-encoding, and re-hashing; since every other
//! field is included, any tamper (a swapped `tapp_url`, a changed root
//! hash, an added category) flips the digest and fails verification.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

pub use crate::target::TILO_OS_PLATFORM;

/// Domain-separation prefix for a registry-entry publisher signature.
pub const ENTRY_DOMAIN: &[u8] = b"tilo-registry-entry-v1\0";

/// Domain-separation prefix for a registry-owner index signature; not
/// used in this crate but exposed for federation clients snapshotting registry state.
pub const INDEX_DOMAIN: &[u8] = b"tilo-registry-index-v1\0";

/// Most versions one entry lists. Publishers keep the newest; an older
/// version leaves the entry once a newer one for its target supersedes it.
pub const MAX_ENTRY_VERSIONS: usize = 128;

/// Localized text whose signed representation is a canonical, ordered map.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalizedText(pub BTreeMap<String, String>);

impl LocalizedText {
    pub fn english(value: impl Into<String>) -> Self {
        Self(BTreeMap::from([("en".into(), value.into())]))
    }

    pub fn resolve(&self, language: &str) -> Option<&str> {
        self.0
            .get(language)
            .or_else(|| {
                language
                    .split_once('-')
                    .and_then(|(base, _)| self.0.get(base))
            })
            .or_else(|| self.0.get("en"))
            .map(String::as_str)
    }
}

impl From<&str> for LocalizedText {
    fn from(value: &str) -> Self {
        Self::english(value)
    }
}

impl From<String> for LocalizedText {
    fn from(value: String) -> Self {
        Self::english(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryImageKind {
    Icon,
    Screenshot,
    Banner,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryImage {
    pub kind: RegistryImageKind,
    pub url: String,
    pub alt: String,
}

/// Locale-specific image sets, allowing screenshots with translated UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LocalizedImages(pub BTreeMap<String, Vec<RegistryImage>>);

impl LocalizedImages {
    /// A complete English artwork set for a simple product card or a test
    /// fixture. Publishers may instead provide locale-aware artwork or no
    /// artwork at all when their app is headless.
    pub fn english_icon(url: impl Into<String>, alt: impl Into<String>) -> Self {
        Self(BTreeMap::from([(
            "en".into(),
            vec![RegistryImage {
                kind: RegistryImageKind::Icon,
                url: url.into(),
                alt: alt.into(),
            }],
        )]))
    }

    pub fn resolve(&self, language: &str) -> &[RegistryImage] {
        self.0
            .get(language)
            .or_else(|| {
                language
                    .split_once('-')
                    .and_then(|(base, _)| self.0.get(base))
            })
            .or_else(|| self.0.get("en"))
            .map_or(&[], Vec::as_slice)
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum EntryValidationError {
    #[error("package id must be lowercase reverse DNS with at least three labels")]
    InvalidPackageId,
    #[error("{field} must contain an English fallback")]
    MissingEnglishFallback { field: &'static str },
    #[error("{field} contains invalid language tag {tag:?}")]
    InvalidLanguageTag { field: &'static str, tag: String },
    #[error("{field} contains an empty or oversized value")]
    InvalidLocalizedValue { field: &'static str },
    #[error("supported_languages must be non-empty, canonical, sorted, and unique")]
    InvalidSupportedLanguages,
    #[error("images contain invalid metadata")]
    InvalidImages,
    #[error(
        "versions must contain complete artifact bindings for a Tilo OS x86_64/aarch64 or \
         web wasm32 target"
    )]
    InvalidVersionTarget,
}

/// One version of a .tapp, with the download URL and the BLAKE3 root the client verifies after fetching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryVersion {
    /// Semver string, or `CalVer` YYYY.MM as some publishers use; treated
    /// as opaque text — client picks a "latest".
    pub version: String,
    /// HTTPS URL of the `.tapp` file.
    pub tapp_url: String,
    /// BLAKE3 hash of the .tapp's signed root (same hash
    /// `tilo_app_package::PackageId` derives); clients recompute on download — a
    /// mismatch means bytes were tampered in transit.
    pub tapp_root_hash_hex: String,
    /// Byte size of the .tapp, for progress bars and integrity pre-flight.
    pub tapp_size_bytes: u64,
    /// Unix seconds when the publisher released this version.
    pub released_at_unix: u64,
    /// BLAKE3 of the complete `.tapp` byte stream. The package root above
    /// authenticates the logical package; this additionally pins the exact
    /// transport artifact fetched by clients and reviewed by moderators.
    pub tapp_blake3_hex: String,
    /// Execution target authenticated by the publisher.
    pub target: RegistryTarget,
}

/// The machine/runtime contract for one package artifact. Accepted
/// spellings are defined in [`crate::target`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryTarget {
    /// Extensible platform-family wire name: `tilo-os` or `web`.
    pub platform: String,
    /// Architecture wire name.
    pub architecture: String,
    /// Minimum ISA level, e.g. `x86_64-v3`, or a web feature set such as
    /// `wasm32-mvp+bulk-memory+sign-ext`.
    pub isa: String,
    /// Runtime ABI required by the package: optional for `tilo-os`,
    /// `tilo-runtime/<version>+wasm.<32 hex>` for `web`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
}

/// One package entry inside a registry — everything the client needs to render a store card and start a download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Reverse-DNS id from `tilo_app_package::Manifest.package.id`.
    pub package_id: String,
    /// Localized display name.
    pub name: LocalizedText,
    /// Localized product description.
    pub description: LocalizedText,
    /// Localized short card tagline.
    pub tagline: LocalizedText,
    /// Locale-specific store artwork.
    pub images: LocalizedImages,
    /// Canonical language tags supported by the app itself.
    pub supported_languages: Vec<String>,
    /// Free-form category tags — the UI groups on these.
    pub categories: Vec<String>,
    /// Optional source-code URL.
    pub source_url: Option<String>,
    /// Optional project homepage.
    pub homepage: Option<String>,
    /// Ed25519 identity of the publisher (32 bytes, hex); matches
    /// `tilo_app_package::Manifest.publisher.identity` on every `tapp_url` this entry advertises.
    pub publisher_pubkey_hex: String,
    /// All versions the publisher has released. Order-preserving.
    pub versions: Vec<RegistryVersion>,
    /// Unix seconds of the last publisher change — signed alongside the
    /// rest of the entry; used to order concurrent updates (POST takes the newer value).
    pub updated_at_unix: u64,
    /// Publisher's Ed25519 signature over `blake3(ENTRY_DOMAIN ||
    /// postcard(signing_payload))`, hex-encoded 64 bytes.
    pub signature_hex: String,
}

/// The canonical signing payload for a [`RegistryEntry`] — every
/// field *except* the signature itself.
///
/// A borrowed view (avoids cloning the entry). Postcard walks fields
/// in declaration order, so keep this shape aligned with
/// `RegistryEntry`'s fields (minus `signature_hex`).
#[derive(Serialize)]
struct EntrySigningPayload<'a> {
    package_id: &'a str,
    name: &'a LocalizedText,
    description: &'a LocalizedText,
    tagline: &'a LocalizedText,
    images: &'a LocalizedImages,
    supported_languages: &'a [String],
    categories: &'a [String],
    source_url: &'a Option<String>,
    homepage: &'a Option<String>,
    publisher_pubkey_hex: &'a str,
    versions: &'a [RegistryVersion],
    updated_at_unix: u64,
}

impl RegistryEntry {
    /// The 32-byte BLAKE3 digest the publisher signs.
    pub fn signing_message(&self) -> [u8; 32] {
        entry_signing_message(self)
    }

    /// Detach the publisher signature, returning an entry with an empty
    /// `signature_hex` — for preparing an entry to sign or re-signing
    /// after a moderator edit. Verification uses `signing_message`
    /// directly and doesn't need this.
    #[must_use]
    pub fn without_signature(mut self) -> Self {
        self.signature_hex.clear();
        self
    }

    /// Validate the entry's identifiers, localized metadata, package targets,
    /// and artifact bindings.
    ///
    /// # Errors
    ///
    /// Returns an [`EntryValidationError`] identifying the first invalid field
    /// or collection encountered.
    pub fn validate(&self) -> Result<(), EntryValidationError> {
        if !crate::namespace::valid_package_id(&self.package_id) {
            return Err(EntryValidationError::InvalidPackageId);
        }
        validate_localized_text("name", &self.name, 256)?;
        validate_localized_text("description", &self.description, 8_192)?;
        validate_localized_text("tagline", &self.tagline, 512)?;
        validate_images(&self.images)?;
        if self.supported_languages.is_empty()
            || self.supported_languages.len() > 128
            || self
                .supported_languages
                .iter()
                .any(|tag| !valid_language_tag(tag))
            || self.supported_languages.windows(2).any(|pair| {
                pair.first()
                    .zip(pair.get(1))
                    .is_some_and(|(left, right)| left >= right)
            })
        {
            return Err(EntryValidationError::InvalidSupportedLanguages);
        }
        if self.versions.is_empty()
            || self.versions.len() > MAX_ENTRY_VERSIONS
            || self.versions.iter().any(|v| {
                v.tapp_url.is_empty()
                    || v.tapp_url.len() > 4_096
                    || !valid_artifact_url(&v.tapp_url)
                    || v.tapp_root_hash_hex.len() != 64
                    || !v.tapp_root_hash_hex.bytes().all(|b| b.is_ascii_hexdigit())
                    || v.tapp_blake3_hex.len() != 64
                    || !v.tapp_blake3_hex.bytes().all(|b| b.is_ascii_hexdigit())
                    || v.tapp_size_bytes == 0
                    || v.target.validate().is_err()
            })
        {
            return Err(EntryValidationError::InvalidVersionTarget);
        }
        Ok(())
    }
}

fn valid_artifact_url(value: &str) -> bool {
    value.starts_with("https://")
        || value.starts_with("http://127.0.0.1:")
        || value.starts_with("http://localhost:")
        || value.starts_with("http://[::1]:")
}

fn validate_localized_text(
    field: &'static str,
    value: &LocalizedText,
    max_value_len: usize,
) -> Result<(), EntryValidationError> {
    if value.0.is_empty() || !value.0.contains_key("en") {
        return Err(EntryValidationError::MissingEnglishFallback { field });
    }
    for (tag, text) in &value.0 {
        if !valid_language_tag(tag) {
            return Err(EntryValidationError::InvalidLanguageTag {
                field,
                tag: tag.clone(),
            });
        }
        if text.trim().is_empty() || text.len() > max_value_len {
            return Err(EntryValidationError::InvalidLocalizedValue { field });
        }
    }
    Ok(())
}

fn validate_images(images: &LocalizedImages) -> Result<(), EntryValidationError> {
    if images.0.is_empty() {
        return Ok(());
    }
    if !images.0.contains_key("en") || images.0.len() > 32 {
        return Err(EntryValidationError::MissingEnglishFallback { field: "images" });
    }
    for (tag, set) in &images.0 {
        if !valid_language_tag(tag)
            || set.len() > 32
            || set.iter().any(|image| {
                !image.url.starts_with("https://")
                    || image.url.len() > 4_096
                    || image.alt.trim().is_empty()
                    || image.alt.len() > 512
            })
        {
            return Err(EntryValidationError::InvalidImages);
        }
    }
    Ok(())
}

/// Conservative canonical BCP-47 validation for app/store metadata.
pub fn valid_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(language) = parts.next() else {
        return false;
    };
    if !(2..=3).contains(&language.len()) || !language.bytes().all(|b| b.is_ascii_lowercase()) {
        return false;
    }
    let mut saw_script = false;
    let mut saw_region = false;
    for part in parts {
        if part.is_empty() {
            return false;
        }
        let mut script_bytes = part.bytes();
        let is_canonical_script = part.len() == 4
            && script_bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_uppercase())
            && script_bytes.all(|byte| byte.is_ascii_lowercase());
        if !saw_script && is_canonical_script {
            saw_script = true;
        } else if !saw_region
            && ((part.len() == 2 && part.bytes().all(|b| b.is_ascii_uppercase()))
                || (part.len() == 3 && part.bytes().all(|b| b.is_ascii_digit())))
        {
            saw_region = true;
        } else if !(4..=8).contains(&part.len())
            || !part
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        {
            return false;
        }
    }
    true
}

/// Standalone form of [`RegistryEntry::signing_message`], as a free
/// function so verifier code can compute the digest without
/// constructing/mutating an entry.
///
/// # Panics
///
/// Panics if `postcard::to_stdvec` fails to encode `payload`. In
/// practice this is unreachable: every field is either an owned
/// `String`/`Vec`/`Option` with the standard derived `Serialize` impl
/// (no custom impl that can error) or a `u64`, and postcard only
/// errors on a `Serialize` impl returning an error or a sequence
/// length that overflows its varint encoding — neither can happen here.
#[allow(
    clippy::expect_used,
    reason = "postcard::to_stdvec cannot fail for this owned, derive(Serialize) payload — see \
              the # Panics note above; propagating a phantom Result would push an unreachable \
              error case onto every signer/verifier in this crate for no real safety gain"
)]
pub fn entry_signing_message(entry: &RegistryEntry) -> [u8; 32] {
    let payload = EntrySigningPayload {
        package_id: &entry.package_id,
        name: &entry.name,
        description: &entry.description,
        tagline: &entry.tagline,
        images: &entry.images,
        supported_languages: &entry.supported_languages,
        categories: &entry.categories,
        source_url: &entry.source_url,
        homepage: &entry.homepage,
        publisher_pubkey_hex: &entry.publisher_pubkey_hex,
        versions: &entry.versions,
        updated_at_unix: entry.updated_at_unix,
    };
    // Postcard's to_stdvec never fails for owned inputs of this shape — unwrap is safe.
    let body = postcard::to_stdvec(&payload).expect("postcard encode entry payload");
    let mut hasher = blake3::Hasher::new();
    hasher.update(ENTRY_DOMAIN);
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Domain-separated index signing message, exposed for federation
/// clients snapshotting registry state for the owner to sign.
///
/// # Panics
///
/// Panics if `postcard::to_stdvec` fails to encode `index_manifest`.
/// Unreachable for any `T` built from owned, derive(Serialize) data —
/// see [`entry_signing_message`]'s `# Panics` note for why.
#[allow(
    clippy::expect_used,
    reason = "postcard::to_stdvec cannot fail for a derive(Serialize) manifest — see the \
              # Panics note above; propagating a phantom Result buys no real safety here"
)]
pub fn index_signing_message<T: Serialize>(index_manifest: &T) -> [u8; 32] {
    let body = postcard::to_stdvec(index_manifest).expect("postcard encode index manifest");
    let mut hasher = blake3::Hasher::new();
    hasher.update(INDEX_DOMAIN);
    hasher.update(&body);
    *hasher.finalize().as_bytes()
}

/// Convenience: sign an entry in place, filling `signature_hex`; caller is responsible for the publisher's key.
pub fn sign_entry(entry: &mut RegistryEntry, signing_key: &SigningKey) {
    // Ensure the signing_message is computed over the payload
    // WITHOUT the signature field, so we clear it first.
    entry.signature_hex.clear();
    let msg = entry_signing_message(entry);
    let sig: Signature = signing_key.sign(&msg);
    entry.signature_hex = hex::encode(sig.to_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::target::{WASM32_ARCHITECTURE, WEB_PLATFORM};
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            name: "Editor".into(),
            description: "CRDT-native text editor".into(),
            tagline: "Edit together".into(),
            images: LocalizedImages(BTreeMap::from([(
                "en".into(),
                vec![RegistryImage {
                    kind: RegistryImageKind::Icon,
                    url: "https://apps.tilo.so/editor/icon.png".into(),
                    alt: "Editor icon".into(),
                }],
            )])),
            supported_languages: vec!["de".into(), "en".into()],
            categories: vec!["Productivity".into(), "Text".into()],
            source_url: Some("https://code.tilo.so/tilo/editor".into()),
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![RegistryVersion {
                version: "2026.06".into(),
                tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                tapp_root_hash_hex: "cd".repeat(32),
                tapp_size_bytes: 12_345_678,
                released_at_unix: 1_800_000_000,
                tapp_blake3_hex: "ef".repeat(32),
                target: RegistryTarget {
                    platform: TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: 1_800_000_100,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn entry_signing_message_covers_every_signed_field() {
        // phase: entry_json_roundtrip
        {
            let e = sample_entry();
            let s = serde_json::to_string(&e).expect("encode");
            let back: RegistryEntry = serde_json::from_str(&s).expect("decode");
            assert_eq!(back, e);
        }
        // phase: signing_message_is_independent_of_signature_field
        {
            let mut e = sample_entry();
            let m1 = e.signing_message();
            e.signature_hex = "deadbeef".into();
            let m2 = e.signing_message();
            assert_eq!(m1, m2, "signature_hex must not affect signing digest");
        }
        // phase: signing_message_changes_when_any_signed_field_changes
        {
            let base = sample_entry();
            let base_msg = base.signing_message();

            {
                let mut alt = sample_entry();
                alt.package_id = "so.tilo.other".into();
                assert_ne!(alt.signing_message(), base_msg);
            }
            {
                let mut alt = sample_entry();
                alt.versions[0].tapp_root_hash_hex = "ff".repeat(32);
                assert_ne!(alt.signing_message(), base_msg);
            }
            {
                let mut bound = sample_entry();
                bound.versions[0].tapp_blake3_hex = "ee".repeat(32);
                bound.versions[0].target = RegistryTarget {
                    platform: TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: Some("tilo-runtime/2026.07".into()),
                };
                let bound_msg = bound.signing_message();
                assert_ne!(bound_msg, base_msg);
                bound.versions[0].tapp_blake3_hex = "fe".repeat(32);
                assert_ne!(bound.signing_message(), bound_msg);
            }
            {
                let mut alt = sample_entry();
                alt.updated_at_unix += 1;
                assert_ne!(alt.signing_message(), base_msg);
            }
            {
                let mut alt = sample_entry();
                alt.categories.push("Extra".into());
                assert_ne!(alt.signing_message(), base_msg);
            }
        }
        // phase: sign_entry_fills_signature_field
        {
            let mut e = sample_entry();
            let key = SigningKey::generate(&mut UnwrapErr(SysRng));
            sign_entry(&mut e, &key);
            assert_eq!(e.signature_hex.len(), 128, "64-byte hex-encoded sig");
            // Idempotent under re-sign with the same key + same payload.
            let sig1 = e.signature_hex.clone();
            sign_entry(&mut e, &key);
            assert_eq!(e.signature_hex, sig1);
        }
        // phase: index_signing_message_is_domain_separated
        {
            let entry = sample_entry();
            let entry_msg = entry_signing_message(&entry);
            // Feeding the same postcard body through the *index*
            // domain must yield a different digest.
            let index_msg = index_signing_message(&entry);
            assert_ne!(entry_msg, index_msg);
        }
    }

    #[test]
    fn entry_validation_rejects_bad_locales_platforms_and_architectures() {
        // phase: localized_metadata_requires_canonical_tags_and_english_fallback
        {
            let mut entry = sample_entry();
            assert_eq!(entry.validate(), Ok(()));

            entry.tagline.0 = BTreeMap::from([("de".into(), "Gemeinsam bearbeiten".into())]);
            assert!(matches!(
                entry.validate(),
                Err(EntryValidationError::MissingEnglishFallback { field: "tagline" })
            ));

            entry = sample_entry();
            entry
                .description
                .0
                .insert("en_US".into(), "Invalid tag".into());
            assert!(matches!(
                entry.validate(),
                Err(EntryValidationError::InvalidLanguageTag {
                    field: "description",
                    ..
                })
            ));

            entry = sample_entry();
            entry.name.0.insert("de".into(), String::new());
            assert!(matches!(
                entry.validate(),
                Err(EntryValidationError::InvalidLocalizedValue { field: "name" })
            ));

            entry = sample_entry();
            entry.images = LocalizedImages::english_icon(
                "https://cdn.example.invalid/icon.svg",
                "Third-party icon",
            );
            entry.source_url = Some("https://example.invalid/tilo".into());
            entry.homepage = Some("https://another.example.invalid/product".into());
            assert_eq!(entry.validate(), Ok(()));

            entry = sample_entry();
            entry.images = LocalizedImages::default();
            assert_eq!(entry.validate(), Ok(()));
        }
        // phase: registry_rejects_non_tilo_platforms_and_unsupported_architectures
        {
            let mut entry = sample_entry();
            entry.versions[0].target.platform = "freebsd".into();
            assert_eq!(
                entry.validate(),
                Err(EntryValidationError::InvalidVersionTarget)
            );
            entry.versions[0].target.platform = TILO_OS_PLATFORM.into();
            entry.versions[0].target.architecture = "riscv64".into();
            assert_eq!(
                entry.validate(),
                Err(EntryValidationError::InvalidVersionTarget)
            );
        }
    }

    #[test]
    fn web_versions_sit_beside_native_versions_of_one_entry() {
        let mut entry = sample_entry();
        let mut web = entry.versions[0].clone();
        web.tapp_url = "https://apps.tilo.so/editor/2026.06-web.tapp".into();
        web.target = RegistryTarget {
            platform: WEB_PLATFORM.into(),
            architecture: WASM32_ARCHITECTURE.into(),
            isa: "wasm32-mvp+bulk-memory+sign-ext".into(),
            runtime: Some("tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff".into()),
        };
        entry.versions.push(web);
        assert_eq!(entry.validate(), Ok(()));

        entry.versions[1].target.runtime = Some("tilo-runtime/2026.07".into());
        assert_eq!(
            entry.validate(),
            Err(EntryValidationError::InvalidVersionTarget)
        );
        entry.versions[1].target.runtime = None;
        assert_eq!(
            entry.validate(),
            Err(EntryValidationError::InvalidVersionTarget)
        );
    }
}
