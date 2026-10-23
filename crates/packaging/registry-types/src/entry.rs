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
//!
//! App entries keep the shape and signing bytes every released client
//! reads: the entry's `kind` and a version's bot artifact are serialized only
//! when present, so an app entry encodes exactly as it did before bots
//! existed. A bot entry signs under its own domain ([`BOT_ENTRY_DOMAIN`]),
//! so an app package and a bot program can never be passed off as each
//! other.

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::Deserialize;
use serde::Serialize;
use thiserror::Error;

pub use crate::target::TILO_OS_PLATFORM;

/// Domain-separation prefix for a registry-entry publisher signature.
pub const ENTRY_DOMAIN: &[u8] = b"tilo-registry-entry-v1\0";

/// Domain-separation prefix for a bot entry's publisher signature. Clients
/// that predate bots never verify one: snapshots carry bot entries apart
/// from app entries (see [`crate::snapshot`]).
pub const BOT_ENTRY_DOMAIN: &[u8] = b"tilo-registry-bot-entry-v1\0";

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
        "versions must contain complete artifact bindings for an x86_64/aarch64 target of a \
         known platform"
    )]
    InvalidVersionTarget,
    #[error("every version's artifact must match the entry's kind")]
    ArtifactKindMismatch,
    #[error("bot program artifact is invalid: {reason}")]
    InvalidBotArtifact { reason: &'static str },
}

/// One listed version of a registry entry, named by the registry whose owner
/// key signs the catalogue. A bot installed from a directory keeps this so
/// its room can tell which listing it runs and offer that listing's updates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DirectoryRef {
    /// Lowercase hex of the registry owner's public key.
    pub registry_owner: String,
    /// The entry's package id.
    pub entry: String,
    pub version: String,
}

/// Most bytes a bot program file may have.
pub const MAX_BOT_PROGRAM_BYTES: u64 = 262_144;

/// What a registry entry publishes: an app package or a bot program.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EntryKind {
    #[default]
    App,
    Bot,
}

impl EntryKind {
    /// Whether this is an app entry, whose `kind` is left out of its
    /// encoding so the entry reads as every older client expects.
    #[must_use]
    pub fn is_app(&self) -> bool {
        *self == Self::App
    }

    /// The domain the publisher's signature over an entry of this kind uses.
    #[must_use]
    pub fn signing_domain(self) -> &'static [u8] {
        match self {
            Self::App => ENTRY_DOMAIN,
            Self::Bot => BOT_ENTRY_DOMAIN,
        }
    }
}

/// One released version: its number, its date and the artifact it binds.
///
/// A version binding a `.tapp` encodes with the fields every released client
/// reads (`version`, `tapp_url`, `tapp_root_hash_hex`, `tapp_size_bytes`,
/// `released_at_unix`, `tapp_blake3_hex`, `target`, in that order, which the
/// signing form depends on); a bot version encodes as `version`,
/// `released_at_unix` and `bot`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegistryVersion {
    /// Semver string, or `CalVer` YYYY.MM as some publishers use; treated
    /// as opaque text — client picks a "latest".
    pub version: String,
    /// Unix seconds when the publisher released this version.
    pub released_at_unix: u64,
    pub artifact: VersionArtifact,
}

#[derive(Serialize)]
#[serde(rename = "RegistryVersion")]
struct TappVersionOut<'a> {
    version: &'a str,
    tapp_url: &'a str,
    tapp_root_hash_hex: &'a str,
    tapp_size_bytes: u64,
    released_at_unix: u64,
    tapp_blake3_hex: &'a str,
    target: &'a RegistryTarget,
}

#[derive(Serialize)]
#[serde(rename = "RegistryVersion")]
struct BotVersionOut<'a> {
    version: &'a str,
    released_at_unix: u64,
    bot: &'a BotArtifact,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum VersionIn {
    Bot {
        version: String,
        released_at_unix: u64,
        bot: BotArtifact,
    },
    Tapp {
        version: String,
        tapp_url: String,
        tapp_root_hash_hex: String,
        tapp_size_bytes: u64,
        released_at_unix: u64,
        tapp_blake3_hex: String,
        target: RegistryTarget,
    },
}

impl Serialize for RegistryVersion {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match &self.artifact {
            VersionArtifact::Tapp(tapp) => TappVersionOut {
                version: &self.version,
                tapp_url: &tapp.tapp_url,
                tapp_root_hash_hex: &tapp.tapp_root_hash_hex,
                tapp_size_bytes: tapp.tapp_size_bytes,
                released_at_unix: self.released_at_unix,
                tapp_blake3_hex: &tapp.tapp_blake3_hex,
                target: &tapp.target,
            }
            .serialize(serializer),
            VersionArtifact::Bot(bot) => BotVersionOut {
                version: &self.version,
                released_at_unix: self.released_at_unix,
                bot,
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for RegistryVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match VersionIn::deserialize(deserializer)? {
            VersionIn::Bot {
                version,
                released_at_unix,
                bot,
            } => Self {
                version,
                released_at_unix,
                artifact: VersionArtifact::Bot(bot),
            },
            VersionIn::Tapp {
                version,
                tapp_url,
                tapp_root_hash_hex,
                tapp_size_bytes,
                released_at_unix,
                tapp_blake3_hex,
                target,
            } => Self {
                version,
                released_at_unix,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url,
                    tapp_root_hash_hex,
                    tapp_size_bytes,
                    tapp_blake3_hex,
                    target,
                }),
            },
        })
    }
}

/// The file a version binds. The variant must match the entry's kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VersionArtifact {
    Tapp(TappArtifact),
    Bot(BotArtifact),
}

/// A `.tapp` with the download URL and the BLAKE3 root the client verifies after fetching.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TappArtifact {
    /// HTTPS URL of the `.tapp` file.
    pub tapp_url: String,
    /// BLAKE3 hash of the .tapp's signed root (same hash
    /// `tilo_app_package::PackageId` derives); clients recompute on download — a
    /// mismatch means bytes were tampered in transit.
    pub tapp_root_hash_hex: String,
    /// Byte size of the .tapp, for progress bars and integrity pre-flight.
    pub tapp_size_bytes: u64,
    /// BLAKE3 of the complete `.tapp` byte stream. The package root above
    /// authenticates the logical package; this additionally pins the exact
    /// transport artifact fetched by clients and reviewed by moderators.
    pub tapp_blake3_hex: String,
    /// Execution target authenticated by the publisher.
    pub target: RegistryTarget,
}

/// A bot program: a Shortcut's canonical JSON, stored by the registry as a
/// content-addressed blob, so what a room installs is exactly what was signed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotArtifact {
    /// `<registry>/blobs/<program_blake3_hex>`; see [`bot_program_url`].
    pub program_url: String,
    pub program_blake3_hex: String,
    /// At most [`MAX_BOT_PROGRAM_BYTES`].
    pub program_size_bytes: u64,
    pub requires: BotRequirements,
}

/// What a bot needs from the room and its host, checked against the program
/// by the registry and shown on the listing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotRequirements {
    /// Connector packages the room's host must have installed and configured,
    /// sorted by package id.
    pub connectors: Vec<PackageRequirement>,
    /// Apps whose actions the steps invoke or whose changes the triggers
    /// watch, sorted. A connector is listed only under `connectors`.
    pub apps: Vec<String>,
    /// Whether the program posts under other names and avatars.
    pub appearance: bool,
    /// Trigger kinds the program reacts to, sorted, for the listing.
    pub triggers: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageRequirement {
    pub package_id: String,
    /// Semver of the oldest version that works.
    pub min_version: String,
}

/// Which versions supersede each other: tapps per platform and architecture,
/// bots all together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VersionGroup<'a> {
    Target {
        platform: &'a str,
        architecture: &'a str,
    },
    Bot,
}

impl RegistryVersion {
    #[must_use]
    pub fn tapp(&self) -> Option<&TappArtifact> {
        match &self.artifact {
            VersionArtifact::Tapp(tapp) => Some(tapp),
            VersionArtifact::Bot(_) => None,
        }
    }

    #[must_use]
    pub fn tapp_mut(&mut self) -> Option<&mut TappArtifact> {
        match &mut self.artifact {
            VersionArtifact::Tapp(tapp) => Some(tapp),
            VersionArtifact::Bot(_) => None,
        }
    }

    #[must_use]
    pub fn bot(&self) -> Option<&BotArtifact> {
        match &self.artifact {
            VersionArtifact::Bot(bot) => Some(bot),
            VersionArtifact::Tapp(_) => None,
        }
    }

    #[must_use]
    pub fn target(&self) -> Option<&RegistryTarget> {
        self.tapp().map(|tapp| &tapp.target)
    }

    #[must_use]
    pub fn group(&self) -> VersionGroup<'_> {
        match &self.artifact {
            VersionArtifact::Tapp(tapp) => VersionGroup::Target {
                platform: &tapp.target.platform,
                architecture: &tapp.target.architecture,
            },
            VersionArtifact::Bot(_) => VersionGroup::Bot,
        }
    }
}

/// The URL a registry at `base` serves the program `program_blake3_hex` at.
#[must_use]
pub fn bot_program_url(base: &str, program_blake3_hex: &str) -> String {
    format!(
        "{}/blobs/{}",
        base.trim_end_matches('/'),
        program_blake3_hex
    )
}

/// The machine/runtime contract for one package artifact. Accepted
/// spellings are defined in [`crate::target`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryTarget {
    /// Extensible platform-family wire name, e.g. `tilo-os` or `linux`.
    pub platform: String,
    /// Architecture wire name.
    pub architecture: String,
    /// Minimum ISA level, e.g. `x86_64-v3`.
    pub isa: String,
    /// Runtime the package links: for `tilo-os`, the exact build
    /// (`tilo-runtime/<version>+native.<32 hex>`); optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// System interfaces the build requires, `<name>@<version>`, sorted:
    /// the required services of its manifest. A device offers the version
    /// only when its OS serves every one; see [`crate::interfaces`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interfaces: Vec<String>,
}

/// One package entry inside a registry — everything the client needs to render a store card and start a download.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistryEntry {
    /// Reverse-DNS id from `tilo_app_package::Manifest.package.id`.
    pub package_id: String,
    /// App package or bot program; every version's artifact has this kind.
    /// Left out for apps, so an app entry encodes as it always has.
    #[serde(default, skip_serializing_if = "EntryKind::is_app")]
    pub kind: EntryKind,
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

    /// Validate the entry's identifiers, localized metadata, package targets,
    /// and artifact bindings.
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
        if self.versions.is_empty() || self.versions.len() > MAX_ENTRY_VERSIONS {
            return Err(EntryValidationError::InvalidVersionTarget);
        }
        let language_pack = crate::target::is_language_pack_package_id(&self.package_id);
        for version in &self.versions {
            match (self.kind, &version.artifact) {
                // A language pack is data for every architecture, and only a
                // language pack is.
                (EntryKind::App, VersionArtifact::Tapp(tapp))
                    if tapp.target.is_any_architecture() != language_pack =>
                {
                    return Err(EntryValidationError::InvalidVersionTarget);
                }
                (EntryKind::App, VersionArtifact::Tapp(tapp)) => validate_tapp(tapp)?,
                (EntryKind::Bot, VersionArtifact::Bot(bot)) => validate_bot(bot)?,
                _ => return Err(EntryValidationError::ArtifactKindMismatch),
            }
        }
        Ok(())
    }
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn is_hex_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn validate_tapp(v: &TappArtifact) -> Result<(), EntryValidationError> {
    let ok = !v.tapp_url.is_empty()
        && v.tapp_url.len() <= 4_096
        && valid_artifact_url(&v.tapp_url)
        && is_hex_64(&v.tapp_root_hash_hex)
        && is_hex_64(&v.tapp_blake3_hex)
        && v.tapp_size_bytes != 0
        && v.target.validate().is_ok();
    if ok {
        Ok(())
    } else {
        Err(EntryValidationError::InvalidVersionTarget)
    }
}

fn sorted_unique(ids: &[&str]) -> bool {
    ids.windows(2)
        .all(|pair| pair.first().zip(pair.get(1)).is_some_and(|(l, r)| l < r))
}

fn validate_bot(v: &BotArtifact) -> Result<(), EntryValidationError> {
    let bad = |reason| Err(EntryValidationError::InvalidBotArtifact { reason });
    if !is_lower_hex_64(&v.program_blake3_hex) {
        return bad("program_blake3_hex must be 64 lowercase hex digits");
    }
    if v.program_size_bytes == 0 || v.program_size_bytes > MAX_BOT_PROGRAM_BYTES {
        return bad("program_size_bytes must be between 1 and 262144");
    }
    // The URL must name exactly this hash under some registry's /blobs/, so
    // the bytes a client fetches are the ones the publisher signed.
    let Some(base) = v
        .program_url
        .strip_suffix(v.program_blake3_hex.as_str())
        .and_then(|rest| rest.strip_suffix("/blobs/"))
    else {
        return bad("program_url must end in /blobs/<program_blake3_hex>");
    };
    if v.program_url.len() > 4_096
        || !valid_artifact_url(&v.program_url)
        || base.contains(['?', '#', '@'])
        || base
            .split("://")
            .nth(1)
            .is_none_or(|authority| authority.is_empty() || authority.contains('/'))
    {
        return bad("program_url must be <https origin>/blobs/<hash>");
    }
    let r = &v.requires;
    if r.connectors.len() > 32 || r.apps.len() > 64 || r.triggers.len() > 32 {
        return bad("too many requirements");
    }
    let connectors: Vec<&str> = r.connectors.iter().map(|c| c.package_id.as_str()).collect();
    let apps: Vec<&str> = r.apps.iter().map(String::as_str).collect();
    if !sorted_unique(&connectors) || !sorted_unique(&apps) {
        return bad("requirement ids must be sorted and unique");
    }
    if connectors
        .iter()
        .chain(&apps)
        .any(|id| !crate::namespace::valid_package_id(id))
    {
        return bad("requirement ids must be package ids");
    }
    if apps.iter().any(|app| connectors.contains(app)) {
        return bad("a package is either an app or a connector requirement");
    }
    if r.connectors
        .iter()
        .any(|c| semver::Version::parse(&c.min_version).is_err())
    {
        return bad("connector min_version must be semver");
    }
    let triggers: Vec<&str> = r.triggers.iter().map(String::as_str).collect();
    if !sorted_unique(&triggers)
        || triggers.iter().any(|t| {
            t.is_empty() || t.len() > 64 || !t.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
    {
        return bad("triggers must be sorted, unique snake_case kinds");
    }
    Ok(())
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
    hasher.update(entry.kind.signing_domain());
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
    use ed25519_dalek::SigningKey;
    use getrandom::{SysRng, rand_core::UnwrapErr};

    fn tilo_target() -> RegistryTarget {
        RegistryTarget {
            platform: TILO_OS_PLATFORM.into(),
            architecture: "x86_64".into(),
            isa: "x86_64-v3".into(),
            runtime: None,
            interfaces: Vec::new(),
        }
    }

    fn sample_entry() -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            kind: EntryKind::App,
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
                released_at_unix: 1_800_000_000,
                artifact: VersionArtifact::Tapp(TappArtifact {
                    tapp_url: "https://apps.tilo.so/editor/2026.06.tapp".into(),
                    tapp_root_hash_hex: "cd".repeat(32),
                    tapp_size_bytes: 12_345_678,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: tilo_target(),
                }),
            }],
            updated_at_unix: 1_800_000_100,
            signature_hex: String::new(),
        }
    }

    fn sample_bot() -> RegistryEntry {
        let hash = "ab".repeat(32);
        RegistryEntry {
            package_id: "so.tilo.bots.discord-bridge".into(),
            kind: EntryKind::Bot,
            versions: vec![RegistryVersion {
                version: "0.1.0".into(),
                released_at_unix: 1_800_000_000,
                artifact: VersionArtifact::Bot(BotArtifact {
                    program_url: bot_program_url("https://apps.tilo.so", &hash),
                    program_blake3_hex: hash,
                    program_size_bytes: 4_096,
                    requires: BotRequirements {
                        connectors: vec![PackageRequirement {
                            package_id: "so.tilo.discord".into(),
                            min_version: "0.1.0".into(),
                        }],
                        apps: vec!["so.tilo.chat".into()],
                        appearance: true,
                        triggers: vec!["connector".into(), "record_changed".into()],
                    },
                }),
            }],
            ..sample_entry()
        }
    }

    fn bot_artifact(entry: &mut RegistryEntry) -> &mut BotArtifact {
        match &mut entry.versions[0].artifact {
            VersionArtifact::Bot(artifact) => artifact,
            VersionArtifact::Tapp(_) => unreachable!("fixture is a bot"),
        }
    }

    #[test]
    fn app_entries_sign_and_encode_exactly_as_before_bots() {
        // The shape every released client decodes and re-encodes to check a
        // publisher's signature, declared here as it was before bots.
        #[derive(Serialize)]
        struct V1Version<'a> {
            version: &'a str,
            tapp_url: &'a str,
            tapp_root_hash_hex: &'a str,
            tapp_size_bytes: u64,
            released_at_unix: u64,
            tapp_blake3_hex: &'a str,
            target: &'a RegistryTarget,
        }
        #[derive(Serialize)]
        struct V1Payload<'a> {
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
            versions: Vec<V1Version<'a>>,
            updated_at_unix: u64,
        }
        let app = sample_entry();
        let versions = app
            .versions
            .iter()
            .map(|v| {
                let tapp = v.tapp().unwrap();
                V1Version {
                    version: &v.version,
                    tapp_url: &tapp.tapp_url,
                    tapp_root_hash_hex: &tapp.tapp_root_hash_hex,
                    tapp_size_bytes: tapp.tapp_size_bytes,
                    released_at_unix: v.released_at_unix,
                    tapp_blake3_hex: &tapp.tapp_blake3_hex,
                    target: &tapp.target,
                }
            })
            .collect();
        let body = postcard::to_stdvec(&V1Payload {
            package_id: &app.package_id,
            name: &app.name,
            description: &app.description,
            tagline: &app.tagline,
            images: &app.images,
            supported_languages: &app.supported_languages,
            categories: &app.categories,
            source_url: &app.source_url,
            homepage: &app.homepage,
            publisher_pubkey_hex: &app.publisher_pubkey_hex,
            versions,
            updated_at_unix: app.updated_at_unix,
        })
        .unwrap();
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"tilo-registry-entry-v1\0");
        hasher.update(&body);
        assert_eq!(app.signing_message(), *hasher.finalize().as_bytes());

        let json = serde_json::to_value(&app).unwrap();
        assert!(json.get("kind").is_none(), "{json}");
        let version = json["versions"][0].as_object().unwrap();
        assert_eq!(
            version.len(),
            7,
            "an app version keeps exactly its released fields: {version:?}"
        );
        assert!(version["tapp_url"].is_string());

        let mut relabelled = sample_entry();
        relabelled.kind = EntryKind::Bot;
        assert_ne!(app.signing_message(), relabelled.signing_message());
    }

    #[test]
    fn versions_round_trip_through_json() {
        let bot = sample_bot();
        let json = serde_json::to_value(&bot).unwrap();
        assert_eq!(json["kind"], "bot");
        assert!(json["versions"][0]["bot"]["program_url"].is_string());
        assert!(json["versions"][0].get("tapp_url").is_none());
        assert_eq!(serde_json::from_value::<RegistryEntry>(json).unwrap(), bot);
        let app = sample_entry();
        let json = serde_json::to_value(&app).unwrap();
        assert_eq!(serde_json::from_value::<RegistryEntry>(json).unwrap(), app);
    }

    #[test]
    fn bot_entries_validate_and_kind_must_match_every_artifact() {
        assert_eq!(sample_bot().validate(), Ok(()));
        let mut mixed = sample_bot();
        mixed.versions.push(sample_entry().versions[0].clone());
        assert_eq!(
            mixed.validate(),
            Err(EntryValidationError::ArtifactKindMismatch)
        );
        let mut app_with_bot = sample_entry();
        app_with_bot.versions = sample_bot().versions;
        assert_eq!(
            app_with_bot.validate(),
            Err(EntryValidationError::ArtifactKindMismatch)
        );
    }

    #[test]
    fn bot_artifact_url_must_be_this_hash_under_blobs() {
        let other = "cd".repeat(32);
        let hash = "ab".repeat(32);
        for url in [
            format!("https://apps.tilo.so/blobs/{other}"),
            format!("https://apps.tilo.so/artifacts/{hash}"),
            format!("https://apps.tilo.so/blobs/{hash}?x=1"),
            format!("http://apps.tilo.so/blobs/{hash}"),
            format!("https://apps.tilo.so/x/blobs/{hash}"),
            format!("https://user@apps.tilo.so/blobs/{hash}"),
        ] {
            let mut bot = sample_bot();
            bot_artifact(&mut bot).program_url = url.clone();
            assert!(
                matches!(
                    bot.validate(),
                    Err(EntryValidationError::InvalidBotArtifact { .. })
                ),
                "{url} must be refused"
            );
        }
        // Another HTTPS origin is a well-formed entry: which hosts a registry
        // serves programs from is its own policy, checked at submission.
        let mut foreign = sample_bot();
        bot_artifact(&mut foreign).program_url = bot_program_url("https://evil.example", &hash);
        assert_eq!(foreign.validate(), Ok(()));
        let mut local = sample_bot();
        bot_artifact(&mut local).program_url = bot_program_url("http://127.0.0.1:8787", &hash);
        assert_eq!(local.validate(), Ok(()));
    }

    #[test]
    fn bot_artifact_bounds_and_requirement_lists_are_checked() {
        let edit = |f: &dyn Fn(&mut BotArtifact)| {
            let mut bot = sample_bot();
            f(bot_artifact(&mut bot));
            bot.validate()
        };
        assert!(edit(&|a| a.program_size_bytes = 0).is_err());
        assert!(edit(&|a| a.program_size_bytes = MAX_BOT_PROGRAM_BYTES + 1).is_err());
        assert_eq!(
            edit(&|a| a.program_size_bytes = MAX_BOT_PROGRAM_BYTES),
            Ok(())
        );
        assert!(edit(&|a| a.program_blake3_hex = "AB".repeat(32)).is_err());
        assert!(
            edit(&|a| a.requires.apps = vec!["so.tilo.chat".into(), "so.tilo.chat".into()])
                .is_err()
        );
        assert!(
            edit(&|a| a.requires.apps = vec!["so.tilo.notes".into(), "so.tilo.chat".into()])
                .is_err()
        );
        assert!(edit(&|a| a.requires.apps = vec!["Chat".into()]).is_err());
        assert!(edit(&|a| a.requires.connectors[0].min_version = "latest".into()).is_err());
        assert!(
            edit(&|a| a.requires.apps = vec!["so.tilo.discord".into()]).is_err(),
            "an id is an app or a connector, not both"
        );
        assert!(edit(&|a| a.requires.triggers = vec!["Record Changed".into()]).is_err());
    }

    #[test]
    fn versions_group_by_target_and_bots_form_one_group() {
        let app = sample_entry();
        assert_eq!(
            app.versions[0].group(),
            VersionGroup::Target {
                platform: TILO_OS_PLATFORM,
                architecture: "x86_64"
            }
        );
        assert_eq!(sample_bot().versions[0].group(), VersionGroup::Bot);
        assert!(sample_bot().versions[0].target().is_none());
        assert!(sample_bot().versions[0].tapp().is_none());
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
                alt.versions[0].tapp_mut().unwrap().tapp_root_hash_hex = "ff".repeat(32);
                assert_ne!(alt.signing_message(), base_msg);
            }
            {
                let mut bound = sample_entry();
                bound.versions[0].tapp_mut().unwrap().tapp_blake3_hex = "ee".repeat(32);
                bound.versions[0].tapp_mut().unwrap().target = RegistryTarget {
                    platform: TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: Some("tilo-runtime/2026.07".into()),
                    interfaces: Vec::new(),
                };
                let bound_msg = bound.signing_message();
                assert_ne!(bound_msg, base_msg);
                bound.versions[0].tapp_mut().unwrap().tapp_blake3_hex = "fe".repeat(32);
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
            entry.versions[0].tapp_mut().unwrap().target.platform = "freebsd".into();
            assert_eq!(
                entry.validate(),
                Err(EntryValidationError::InvalidVersionTarget)
            );
            entry.versions[0].tapp_mut().unwrap().target.platform = TILO_OS_PLATFORM.into();
            entry.versions[0].tapp_mut().unwrap().target.architecture = "riscv64".into();
            assert_eq!(
                entry.validate(),
                Err(EntryValidationError::InvalidVersionTarget)
            );
        }
    }
}
