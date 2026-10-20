//! Trusted Tilo release catalog and verified medium download/cache.
//!
//! The catalog is embedded into each platform-signed Imager package at build
//! time. In a genuinely signed distribution, changing catalog bytes requires
//! replacing that signed application. Local development embedding alone is not
//! cryptographic release evidence.  Both the compressed transport and
//! expanded disk image are SHA-256 checked before the image becomes selectable.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::model::{Source, TiloRelease};

const BUNDLED_CATALOG: &str = include_str!("../release-catalog.json");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    tag: String,
    releases: Vec<CatalogRelease>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum CatalogRelease {
    Media(MediaRelease),
    TargetPolicy(TargetPolicyMetadata),
    Internal(InternalMetadata),
}

#[derive(Debug, Deserialize)]
enum MediaPurpose {
    #[serde(rename = "removable-media")]
    RemovableMedia,
}
#[derive(Debug, Deserialize)]
enum MediaLayout {
    #[serde(rename = "whole-disk-image")]
    WholeDiskImage,
}
#[derive(Debug, Deserialize)]
enum InternalPurpose {
    #[serde(rename = "tilo-internal-installer")]
    InternalInstaller,
}
#[derive(Debug, Deserialize)]
enum InternalLayout {
    #[serde(rename = "internal-bundle-v2")]
    BundleV2,
}

#[derive(Debug, Deserialize)]
enum TargetPolicyPurpose {
    #[serde(rename = "tilo-target-policy")]
    TargetPolicy,
}

#[derive(Debug, Deserialize)]
enum TargetPolicyLayout {
    #[serde(rename = "target-policy-bundle-v1")]
    BundleV1,
}

/// A small, signed bundle of the exact target UKI and its generation-bound
/// PCR prediction. It lets a preparation host review the destination's boot
/// policy without mounting the live installer image.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetPolicyMetadata {
    purpose: TargetPolicyPurpose,
    layout: TargetPolicyLayout,
    release_id: String,
    arch: String,
    generation: u64,
    target_uki_sha256: String,
    url: String,
    archive_sha256: String,
    archive_size: u64,
}

fn present_tag<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MediaRelease {
    #[serde(default, deserialize_with = "present_tag")]
    purpose: Option<MediaPurpose>,
    #[serde(default, deserialize_with = "present_tag")]
    layout: Option<MediaLayout>,
    name: String,
    version: String,
    channel: String,
    arch: String,
    #[serde(default)]
    hardware_profile: Option<String>,
    #[serde(default)]
    provenance: Option<MediaProvenance>,
    url: String,
    archive_sha256: String,
    archive_size: u64,
    image_sha256: String,
    image_size: u64,
    #[serde(default)]
    account_custody: Option<crate::setup::account_admission::ConsumerCapability>,
    #[serde(default)]
    home_archive: Option<crate::setup::home_admission::ConsumerCapability>,
    #[serde(default)]
    applications: Option<crate::setup::applications_admission::ConsumerCapability>,
    #[serde(default)]
    original_source: Option<crate::setup::original_admission::ConsumerCapability>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct InternalMetadata {
    purpose: InternalPurpose,
    layout: InternalLayout,
    release_id: String,
    arch: String,
    manifest_url: String,
    manifest_sha256: String,
    manifest_size: u64,
}

#[derive(Debug, Deserialize)]
struct MediaProvenance {
    source_image_id: String,
    input_qcow2_sha256: String,
    hardware_profile_sha256: Option<String>,
    target_update_ref: Option<String>,
}

impl MediaProvenance {
    fn validate(&self) -> Result<(), String> {
        let source_digest = self
            .source_image_id
            .strip_prefix("sha256:")
            .ok_or_else(|| {
                "The release catalog contains an invalid source image identity.".to_string()
            })?;
        validate_digest(source_digest)?;
        validate_digest(&self.input_qcow2_sha256)?;
        let profile_digest = self.hardware_profile_sha256.as_deref().ok_or_else(|| {
            "The release catalog is missing a hardware profile digest.".to_string()
        })?;
        validate_digest(profile_digest)?;
        let origin = self.target_update_ref.as_deref().ok_or_else(|| {
            "The release catalog is missing a hardware update origin.".to_string()
        })?;
        // Board images carry private firmware and follow the forge's private
        // repository; every other image follows the public one.
        let reference = ["registry.tilo.so/tilo/tilo", "code.tilo.so/tilo/tilo"]
            .iter()
            .find_map(|repository| origin.strip_prefix(repository));
        let valid_origin =
            if let Some(digest) = reference.and_then(|rest| rest.strip_prefix("@sha256:")) {
                validate_digest(digest).is_ok()
            } else if let Some(tag) = reference.and_then(|rest| rest.strip_prefix(':')) {
                !tag.is_empty()
                    && tag.len() <= 128
                    && tag
                        .bytes()
                        .next()
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                    && tag.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-')
                    })
            } else {
                false
            };
        if !valid_origin {
            return Err("The release catalog contains an untrusted hardware update origin.".into());
        }
        Ok(())
    }
}

/// A local, non-replicated update from the verified release fetch worker.
#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Downloading { received: u64, total: u64 },
    Verifying,
    Expanding { written: u64, total: u64 },
}

/// Cancellation narrows the authority of this process-local network job. The
/// partial archive remains in the cache so a later attempt resumes safely.
#[derive(Clone, Debug, Default)]
pub struct DownloadControl(Arc<AtomicBool>);

impl DownloadControl {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A selection obtained only from this build's embedded catalog. Authentication
/// depends on a genuinely signed build; development embedding is not a signature.
/// This type cannot become a whole-disk `Source` or a media download request.
#[derive(Debug)]
pub struct InternalRelease {
    metadata: InternalMetadata,
}

impl InternalRelease {
    pub fn release_id(&self) -> &str {
        &self.metadata.release_id
    }
    pub fn architecture(&self) -> &str {
        &self.metadata.arch
    }
    pub fn manifest_url(&self) -> &str {
        &self.metadata.manifest_url
    }
    pub fn manifest_sha256(&self) -> &str {
        &self.metadata.manifest_sha256
    }

    #[cfg(all(test, feature = "installation"))]
    pub(crate) fn transport_fixture(bytes: &[u8], url: &str) -> Self {
        Self {
            metadata: InternalMetadata {
                purpose: InternalPurpose::InternalInstaller,
                layout: InternalLayout::BundleV2,
                release_id: "fixture-only".into(),
                arch: "x86_64".into(),
                manifest_url: url.into(),
                manifest_sha256: hex::encode(Sha256::digest(bytes)),
                manifest_size: bytes.len() as u64,
            },
        }
    }

    pub fn manifest_size(&self) -> u64 {
        self.metadata.manifest_size
    }

    /// Bind the exact received bytes to the embedded selection before parsing.
    /// A later downloader must retain existing origin/length rules; this method
    /// performs no transport, staging, execution or device access.
    /// # Errors
    /// Rejects byte length/digest mismatch, invalid schema or selection mismatch.
    pub fn verify_manifest(
        &self,
        bytes: &[u8],
    ) -> Result<crate::setup::internal_bundle::InternalBundle, String> {
        let expected = &self.metadata;
        if bytes.len() as u64 != expected.manifest_size
            || hex::encode(Sha256::digest(bytes)) != expected.manifest_sha256
        {
            return Err("Internal manifest does not match the embedded catalog bytes.".into());
        }
        let bundle = crate::setup::internal_bundle::validate_internal_bundle(bytes)?;
        let (InternalPurpose::InternalInstaller, InternalLayout::BundleV2) =
            (&expected.purpose, &expected.layout);
        if bundle.release_id() != expected.release_id
            || bundle.architecture() != expected.arch
            || bundle.purpose() != crate::setup::internal_bundle::PURPOSE
        {
            return Err("Internal manifest does not match the selected release.".into());
        }
        Ok(bundle)
    }
}

fn trusted_catalog_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Invalid catalog URL.".to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some("code.tilo.so")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("The release catalog contains an untrusted download origin.".into());
    }
    Ok(())
}

fn parse_catalog(raw: &str) -> Result<Catalog, String> {
    if raw.len() > 1024 * 1024 {
        return Err("Release catalog exceeds size limit.".into());
    }
    // Derived record/enum deserializers admit sequence records and object
    // unit-enum tags. Constrain the published JSON shapes before decoding the
    // ORIGINAL bytes, preserving duplicate-key checks and authenticated input.
    {
        let shape: serde_json::Value =
            serde_json::from_str(raw).map_err(|error| error.to_string())?;
        let valid =
            shape.is_object()
                && shape
                    .get("releases")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|entries| {
                        entries.iter().all(|entry| {
                            entry.is_object()
                            && entry
                                .get("home_archive")
                                .is_none_or(serde_json::Value::is_object)
                            && entry.get("applications").is_none_or(serde_json::Value::is_object)
                            && entry.get("original_source").is_none_or(
                                crate::setup::original_admission::ConsumerCapability::wire_shape,
                            )
                            && ["purpose", "layout"]
                                .iter()
                                .all(|key| entry.get(*key).is_none_or(serde_json::Value::is_string))
                        })
                    });
        if !valid {
            return Err(
                "Catalog records must be JSON objects with string purpose/layout tags.".into(),
            );
        }
    }
    let catalog: Catalog = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    if !matches!(catalog.schema, 1 | 2) {
        return Err("Unsupported release catalog schema.".into());
    }
    if catalog.releases.is_empty() && catalog.tag == "unreleased" {
        return Ok(catalog);
    }
    if !catalog.tag.starts_with("imager-v") || catalog.releases.len() > 256 {
        return Err("Invalid release catalog tag or entry count.".into());
    }
    let mut identities = std::collections::HashSet::new();
    let mut target_policies = std::collections::HashSet::new();
    let mut internal_ids = std::collections::HashSet::new();
    // Validate every entry before any architecture filtering.
    for entry in &catalog.releases {
        let arch = match entry {
            CatalogRelease::Media(media) => {
                let profile =
                    match (catalog.schema, media.hardware_profile.as_deref()) {
                        (1, None | Some("generic")) => "generic",
                        (2, Some(profile)) => profile,
                        _ => return Err(
                            "The release catalog has an invalid hardware profile for its schema."
                                .into(),
                        ),
                    };
                if !valid_hardware_profile(profile)
                    || !identities.insert((media.arch.as_str(), profile))
                {
                    return Err("Invalid or duplicate architecture and hardware profile.".into());
                }
                if profile != "generic" {
                    media
                        .provenance
                        .as_ref()
                        .ok_or_else(|| {
                            "The release catalog is missing hardware media provenance.".to_string()
                        })?
                        .validate()?;
                }
                if media.purpose.is_some() != media.layout.is_some() {
                    return Err("Incomplete media purpose/layout.".into());
                }
                if let Some(proof) = &media.account_custody {
                    proof
                        .validate()
                        .map_err(|_| "Invalid media account consumer proof.".to_string())?;
                }
                if let Some(proof) = &media.applications {
                    proof
                        .validate()
                        .map_err(|_| "Application consumer proof is invalid".to_string())?;
                }
                if let Some(proof) = &media.home_archive {
                    proof
                        .validate()
                        .map_err(|_| "Invalid media Home consumer proof.".to_string())?;
                }
                if let Some(proof) = &media.original_source {
                    proof
                        .validate()
                        .map_err(|_| "Invalid media original-source consumer proof.".to_string())?;
                    if proof.capture_pins.arch != media.arch {
                        return Err(
                            "Original-source proof architecture differs from selected image."
                                .into(),
                        );
                    }
                }
                validate_digest(&media.archive_sha256)?;
                validate_digest(&media.image_sha256)?;
                if media.archive_size == 0 || media.image_size == 0 {
                    return Err("Invalid media size.".into());
                }
                trusted_catalog_url(&media.url)?;
                &media.arch
            }
            CatalogRelease::TargetPolicy(policy) => {
                use crate::setup::internal_bundle::{canonical_digest, identifier};
                if !matches!(&policy.purpose, TargetPolicyPurpose::TargetPolicy)
                    || !matches!(&policy.layout, TargetPolicyLayout::BundleV1)
                    || !identifier(&policy.release_id)
                    || policy.release_id != catalog.tag
                    || !canonical_digest(&policy.target_uki_sha256, 64)
                    || policy.generation == 0
                    || policy.archive_size == 0
                    || !target_policies.insert(policy.arch.clone())
                {
                    return Err("Invalid target policy metadata.".into());
                }
                validate_digest(&policy.archive_sha256)?;
                trusted_catalog_url(&policy.url)?;
                &policy.arch
            }
            CatalogRelease::Internal(internal) => {
                use crate::setup::internal_bundle::{
                    MAX_MANIFEST_BYTES, canonical_digest, identifier,
                };
                if !identifier(&internal.release_id)
                    || !canonical_digest(&internal.manifest_sha256, 64)
                    || internal.manifest_size == 0
                    || internal.manifest_size > MAX_MANIFEST_BYTES as u64
                    || !internal_ids.insert((&internal.release_id, &internal.arch))
                {
                    return Err("Invalid internal bundle metadata.".into());
                }
                trusted_catalog_url(&internal.manifest_url)?;
                &internal.arch
            }
        };
        if !matches!(arch.as_str(), "x86_64" | "aarch64") {
            return Err("Unsupported release architecture.".into());
        }
    }
    Ok(catalog)
}

fn media_releases(catalog: Catalog, arch: &str) -> Vec<TiloRelease> {
    catalog
        .releases
        .into_iter()
        .filter_map(|entry| {
            let CatalogRelease::Media(release) = entry else {
                return None;
            };
            (release.arch == arch).then(|| TiloRelease {
                name: release.name,
                version: release.version,
                size: release.image_size,
                channel: release.channel,
                architecture: release.arch,
                hardware_profile: release.hardware_profile.unwrap_or_else(|| "generic".into()),
                local_path: String::new(),
                url: release.url,
                archive_sha256: release.archive_sha256,
                archive_size: release.archive_size,
                image_sha256: release.image_sha256,
                account_custody: release.account_custody,
                home_archive: release.home_archive,
                applications: release.applications,
                original_source: release.original_source,
            })
        })
        .collect()
}

/// The architectures the release catalog may describe.
pub const SUPPORTED_ARCHITECTURES: [&str; 2] = ["x86_64", "aarch64"];

/// The selected target policy for `architecture`, if the embedded catalog
/// publishes one. An empty selection is honest: source preparation then
/// defers sensitive capture instead of claiming TPM protection it cannot prove.
/// The download and the bundle review belong to the platform backend.
/// # Errors
/// Rejects malformed catalog entries or more than one policy for an architecture.
pub fn bundled_target_policy_for_arch(
    architecture: &str,
) -> Result<Option<staging_handoff_model::prepared::TargetPolicyBundle>, String> {
    if !SUPPORTED_ARCHITECTURES.contains(&architecture) {
        return Err("Unsupported release architecture.".into());
    }
    let catalog = parse_catalog(&internal_catalog_text()?)?;
    let mut selected = catalog.releases.into_iter().filter_map(|entry| {
        let CatalogRelease::TargetPolicy(policy) = entry else {
            return None;
        };
        (policy.arch == architecture).then_some(policy)
    });
    let Some(policy) = selected.next() else {
        return Ok(None);
    };
    if selected.next().is_some() {
        return Err(
            "The release catalog publishes more than one target policy for this architecture."
                .into(),
        );
    }
    let digest = hex::decode(&policy.target_uki_sha256)
        .map_err(|_| "The target policy has an invalid UKI digest.".to_string())?;
    let target_uki_sha256: [u8; 32] = digest
        .try_into()
        .map_err(|_| "The target policy has an invalid UKI digest.".to_string())?;
    Ok(Some(staging_handoff_model::prepared::TargetPolicyBundle {
        generation: policy.generation,
        target_uki_sha256,
        archive_sha256: policy.archive_sha256,
        archive_size: policy.archive_size,
        url: policy.url,
    }))
}

/// Media selections for the computer this build runs on.
///
/// Authentication depends on the signature of the build. A medium prepared for
/// another computer selects that computer's architecture through
/// [`bundled_releases_for_arch`]; the host architecture is not a constraint on
/// what the verified catalog may offer.
/// # Errors
/// Rejects invalid metadata in any entry, including foreign architectures.
pub fn bundled_releases() -> Result<Vec<TiloRelease>, String> {
    releases_for_host(BUNDLED_CATALOG, crate::host::architecture())
}

/// Media selections for `architecture` from the embedded, verified catalog.
///
/// This is the selection a medium prepared for another computer uses. It never
/// falls back to the preparation host's architecture: an architecture without
/// a verified payload yields an empty selection, so the interface can say so
/// instead of writing a medium that cannot start there.
/// # Errors
/// Rejects an unsupported architecture and invalid metadata in any entry.
pub fn bundled_releases_for_arch(architecture: &str) -> Result<Vec<TiloRelease>, String> {
    releases_for_arch(BUNDLED_CATALOG, architecture)
}

fn releases_for_host(
    catalog_json: &str,
    host_arch: Result<Option<&str>, String>,
) -> Result<Vec<TiloRelease>, String> {
    if let Some(architecture) = host_arch? {
        releases_for_arch(catalog_json, architecture)
    } else {
        parse_catalog(catalog_json)?;
        Ok(Vec::new())
    }
}

fn releases_for_arch(catalog_json: &str, architecture: &str) -> Result<Vec<TiloRelease>, String> {
    if !SUPPORTED_ARCHITECTURES.contains(&architecture) {
        return Err("Unsupported release architecture.".into());
    }
    let catalog = parse_catalog(catalog_json)?;
    Ok(media_releases(catalog, architecture))
}

/// Internal selections stay separate from the ordinary media downloader.
/// No released entries exist until actual reviewed artifacts are published.
/// # Errors
/// Rejects any invalid embedded catalog entry before host filtering.
pub fn bundled_internal_releases() -> Result<Vec<InternalRelease>, String> {
    bundled_internal_releases_for_arch(std::env::consts::ARCH)
}

/// Internal installer selections for `architecture`, not for the preparing
/// host. A medium prepared for another computer resolves its own target
/// architecture here; no fallback to the host architecture exists.
/// # Errors
/// Rejects an unsupported architecture or an invalid embedded catalog.
pub fn bundled_internal_releases_for_arch(
    architecture: &str,
) -> Result<Vec<InternalRelease>, String> {
    if !SUPPORTED_ARCHITECTURES.contains(&architecture) {
        return Err("Unsupported release architecture.".into());
    }
    let catalog = internal_catalog_text()?;
    Ok(parse_catalog(&catalog)?
        .releases
        .into_iter()
        .filter_map(|entry| match entry {
            CatalogRelease::Internal(metadata) if metadata.arch == architecture => {
                Some(InternalRelease { metadata })
            }
            _ => None,
        })
        .collect())
}

/// The catalog internal releases resolve against: the embedded one, or, in
/// builds that allow staging test overrides (debug, or compiled with
/// `TILO_IMAGER_STAGING_TEST_OVERRIDES=1`), the file named by
/// `TILO_STAGING_TEST_CATALOG`. The loop-device harness uses it to name its
/// synthetic bundle; release builds never read it, and the file passes the
/// same `parse_catalog` validation as the embedded catalog.
fn internal_catalog_text() -> Result<std::borrow::Cow<'static, str>, String> {
    if crate::setup::staging::helper::test_overrides_allowed() {
        if let Some(path) = std::env::var_os("TILO_STAGING_TEST_CATALOG") {
            let text = std::fs::read_to_string(&path)
                .map_err(|e| format!("test catalog {}: {e}", path.to_string_lossy()))?;
            return Ok(text.into());
        }
    }
    Ok(BUNDLED_CATALOG.into())
}

fn valid_hardware_profile(profile: &str) -> bool {
    !profile.is_empty()
        && profile.len() <= 64
        && profile
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && profile
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn validate_digest(digest: &str) -> Result<(), String> {
    if digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err("The release catalog contains an invalid SHA-256 digest.".into())
    }
}

fn cache_root() -> PathBuf {
    tilo_paths::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("tilo-imager")
}

fn sha256_controlled(path: &Path, control: &DownloadControl) -> Result<String, String> {
    let mut file =
        File::open(path).map_err(|error| format!("Could not open downloaded data: {error}"))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    loop {
        if control.is_cancelled() {
            return Err("Tilo installer download cancelled.".into());
        }
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not verify downloaded data: {error}"))?;
        if read == 0 {
            break;
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound downloaded data while verifying it.".to_string())?;
        hash.update(chunk);
    }
    Ok(hex::encode(hash.finalize()))
}

fn same_origin(actual: &reqwest::Url, trusted: &reqwest::Url) -> bool {
    actual.scheme() == trusted.scheme()
        && actual.host_str() == trusted.host_str()
        && actual.port_or_known_default() == trusted.port_or_known_default()
}

fn content_range(value: &str) -> Option<(u64, u64, u64)> {
    let value = value.strip_prefix("bytes ")?;
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    Some((start.parse().ok()?, end.parse().ok()?, total.parse().ok()?))
}

fn copy_download_observed(
    response: reqwest::blocking::Response,
    archive: &mut File,
    limit: u64,
    already_received: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<u64, String> {
    let mut input = response.take(limit.saturating_add(1));
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    let mut downloaded = 0_u64;
    let result = loop {
        if control.is_cancelled() {
            break Err("Tilo installer download cancelled.".to_string());
        }
        let read = input
            .read(&mut buffer)
            .map_err(|error| format!("The Tilo installer download was interrupted: {error}"))?;
        if read == 0 {
            break Ok(downloaded);
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound the Tilo installer response.".to_string())?;
        archive
            .write_all(chunk)
            .map_err(|error| format!("Could not save the Tilo installer: {error}"))?;
        downloaded = downloaded.saturating_add(read as u64);
        notify(DownloadEvent::Downloading {
            received: already_received.saturating_add(downloaded),
            total: already_received.saturating_add(limit),
        });
    };
    // Make successfully received chunks durable even when the connection ends
    // early, so the next attempt can ask only for the missing suffix.
    archive
        .flush()
        .map_err(|error| format!("Could not save the Tilo installer: {error}"))?;
    result
}

fn append_range_observed(
    response: reqwest::blocking::Response,
    archive_part: &Path,
    partial_size: u64,
    expected_size: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<bool, String> {
    let parsed_range = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .and_then(content_range);
    let correct_range = parsed_range.is_some_and(|(start, end, total)| {
        start == partial_size && end >= start && end < expected_size && total == expected_size
    });
    let response_size = parsed_range.map_or(0, |(start, end, _)| {
        end.saturating_sub(start).saturating_add(1)
    });
    let correct_length = response
        .content_length()
        .is_none_or(|length| length == response_size);
    if !correct_range || !correct_length {
        fs::remove_file(archive_part)
            .map_err(|error| format!("Could not restart a mismatched partial download: {error}"))?;
        return Ok(false);
    }

    let mut archive = OpenOptions::new()
        .append(true)
        .open(archive_part)
        .map_err(|error| format!("Could not resume the download cache: {error}"))?;
    let downloaded = copy_download_observed(
        response,
        &mut archive,
        response_size,
        partial_size,
        control,
        notify,
    )?;
    let total = archive
        .metadata()
        .map_or(u64::MAX, |metadata| metadata.len());
    if downloaded == response_size && total == expected_size {
        return Ok(true);
    }
    if downloaded > response_size || total > expected_size {
        let _ = fs::remove_file(archive_part);
        return Err("The Tilo installer range response exceeded its declared boundary.".into());
    }
    Err("The Tilo installer download was interrupted before it completed.".into())
}

#[cfg(test)]
fn download_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    trusted_origin: &reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
) -> Result<(), String> {
    download_archive_observed(
        client,
        url,
        trusted_origin,
        archive_part,
        expected_size,
        &DownloadControl::default(),
        &mut |_| {},
    )
}

fn download_archive_observed(
    client: &reqwest::blocking::Client,
    url: &str,
    trusted_origin: &reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<(), String> {
    let mut partial_size = archive_part.metadata().map_or(0, |metadata| metadata.len());
    if partial_size > expected_size {
        fs::remove_file(archive_part)
            .map_err(|error| format!("Could not replace an oversized partial download: {error}"))?;
        partial_size = 0;
    }
    if partial_size == expected_size {
        return Ok(());
    }

    // One ranged attempt may be followed by one clean full request when the
    // server ignores Range or returns a response that cannot safely be joined.
    for _ in 0..2 {
        let resume = partial_size > 0;
        let mut request = client.get(url);
        if resume {
            request = request.header(reqwest::header::RANGE, format!("bytes={partial_size}-"));
        }
        let response = request
            .send()
            .map_err(|error| format!("Could not download the Tilo installer: {error}"))?;
        if !same_origin(response.url(), trusted_origin) {
            return Err("The release download redirected to an untrusted origin.".into());
        }

        if resume && response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
            if append_range_observed(
                response,
                archive_part,
                partial_size,
                expected_size,
                control,
                notify,
            )? {
                return Ok(());
            }
            partial_size = 0;
            continue;
        }

        if resume && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
            // The remote object no longer agrees with the cached offset. Retry
            // once from zero; the final digest still authenticates the result.
            fs::remove_file(archive_part)
                .map_err(|error| format!("Could not restart the partial download: {error}"))?;
            partial_size = 0;
            continue;
        }

        if resume && response.status() != reqwest::StatusCode::OK {
            // Preserve a valid partial across transient server errors. No bytes
            // from an unrecognized response are ever appended to it.
            return response
                .error_for_status()
                .map(|_| ())
                .map_err(|error| format!("Could not download the Tilo installer: {error}"));
        }

        let response = response
            .error_for_status()
            .map_err(|error| format!("Could not download the Tilo installer: {error}"))?;
        if response.status() != reqwest::StatusCode::OK {
            return Err("The Tilo installer server returned an invalid full response.".into());
        }
        if response
            .content_length()
            .is_some_and(|length| length != expected_size)
        {
            if resume {
                let _ = fs::remove_file(archive_part);
            }
            return Err("The Tilo installer download has an unexpected size.".into());
        }
        // A 200 response to a ranged request means Range was ignored. Truncate
        // first, then safely consume that response as the new complete file.
        let mut archive = File::create(archive_part)
            .map_err(|error| format!("Could not create the download cache: {error}"))?;
        let downloaded =
            copy_download_observed(response, &mut archive, expected_size, 0, control, notify)?;
        let total = archive
            .metadata()
            .map_or(u64::MAX, |metadata| metadata.len());
        if downloaded == expected_size && total == expected_size {
            return Ok(());
        }
        if total > expected_size {
            let _ = fs::remove_file(archive_part);
        }
        return Err("The Tilo installer download was interrupted before it completed.".into());
    }

    Err("The Tilo installer server did not honor a safe download restart.".into())
}

/// Download, verify and expand `release`, telling `notify` how far it is.
/// Blocks; run it off the UI thread. `control` stops it between chunks.
pub fn verified_image_observed(
    release: &TiloRelease,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<Source, String> {
    validate_digest(&release.archive_sha256)?;
    validate_digest(&release.image_sha256)?;
    if release.archive_size == 0 || release.size == 0 {
        return Err("The Tilo installer has invalid download metadata.".into());
    }
    let root = cache_root().join(&release.image_sha256);
    fs::create_dir_all(&root)
        .map_err(|error| format!("Could not create the download cache: {error}"))?;
    let image = root.join("tilo-installer.img");
    if image.is_file() {
        notify(DownloadEvent::Verifying);
        if sha256_controlled(&image, control)? == release.image_sha256 {
            return source_for(release, &image);
        }
        fs::remove_file(&image)
            .map_err(|error| format!("Could not replace an invalid cached installer: {error}"))?;
    }

    let archive_part = root.join("tilo-installer.img.zst.part");
    let image_part = root.join("tilo-installer.img.part");
    let client = tilo_net::http::blocking_client()
        .user_agent("Tilo-Imager/1")
        .timeout(std::time::Duration::from_secs(60 * 60))
        .build()
        .map_err(|error| format!("Could not initialize the downloader: {error}"))?;
    let trusted_origin = reqwest::Url::parse("https://code.tilo.so/")
        .map_err(|error| format!("Could not initialize the trusted release origin: {error}"))?;
    notify(DownloadEvent::Downloading {
        received: archive_part.metadata().map_or(0, |metadata| metadata.len()),
        total: release.archive_size,
    });
    download_archive_observed(
        &client,
        &release.url,
        &trusted_origin,
        &archive_part,
        release.archive_size,
        control,
        notify,
    )?;
    notify(DownloadEvent::Verifying);
    if sha256_controlled(&archive_part, control)? != release.archive_sha256 {
        let _ = fs::remove_file(&archive_part);
        return Err("The downloaded Tilo installer did not match its signed catalog.".into());
    }

    let compressed = File::open(&archive_part)
        .map_err(|error| format!("Could not open the verified installer: {error}"))?;
    let mut decoder = ZstdFrames::new(BufReader::new(compressed));
    let mut expanded = File::create(&image_part)
        .map_err(|error| format!("Could not create the expanded installer: {error}"))?;
    let mut expanded_size = 0_u64;
    let mut buffer = vec![0_u8; 1024 * 1024].into_boxed_slice();
    while expanded_size <= release.size {
        if control.is_cancelled() {
            let _ = fs::remove_file(&image_part);
            return Err("Tilo installer download cancelled.".into());
        }
        let allowed = release
            .size
            .saturating_add(1)
            .saturating_sub(expanded_size)
            .min(buffer.len() as u64) as usize;
        if allowed == 0 {
            break;
        }
        let writable = buffer
            .get_mut(..allowed)
            .ok_or_else(|| "Could not bound the expanded Tilo installer.".to_string())?;
        let read = decoder
            .read(writable)
            .map_err(|error| format!("Could not unpack the Tilo installer: {error}"))?;
        if read == 0 {
            break;
        }
        let chunk = buffer
            .get(..read)
            .ok_or_else(|| "Could not bound the expanded Tilo installer.".to_string())?;
        expanded
            .write_all(chunk)
            .map_err(|error| format!("Could not unpack the Tilo installer: {error}"))?;
        expanded_size = expanded_size.saturating_add(read as u64);
        notify(DownloadEvent::Expanding {
            written: expanded_size,
            total: release.size,
        });
    }
    expanded
        .flush()
        .map_err(|error| format!("Could not save the expanded installer: {error}"))?;
    notify(DownloadEvent::Verifying);
    if expanded_size != release.size
        || expanded.metadata().map_or(0, |meta| meta.len()) != release.size
        || sha256_controlled(&image_part, control)? != release.image_sha256
    {
        let _ = fs::remove_file(&image_part);
        return Err("The expanded Tilo installer did not match its signed catalog.".into());
    }
    fs::rename(&image_part, &image)
        .map_err(|error| format!("Could not publish the verified installer cache: {error}"))?;
    let _ = fs::remove_file(&archive_part);
    source_for(release, &image)
}

fn source_for(release: &TiloRelease, image: &Path) -> Result<Source, String> {
    let file = File::open(image)
        .map_err(|error| format!("Could not open the verified installer: {error}"))?;
    let mut source = release.to_source();
    source.path = image.to_string_lossy().into_owned();
    source.identity = crate::model::SourceIdentity::from_file(&file)
        .map_err(|error| format!("Could not identify the verified installer: {error}"))?;
    Ok(source)
}

/// Zstandard frames back to back, decoded as `zstd --decompress` reads them.
struct ZstdFrames<R> {
    source: R,
    frame: ruzstd::decoding::FrameDecoder,
    open: bool,
}

impl<R: BufRead> ZstdFrames<R> {
    fn new(source: R) -> Self {
        Self {
            source,
            frame: ruzstd::decoding::FrameDecoder::new(),
            open: false,
        }
    }
}

impl<R: BufRead> Read for ZstdFrames<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if !self.open {
                if self.source.fill_buf()?.is_empty() {
                    return Ok(0);
                }
                self.frame
                    .reset(&mut self.source)
                    .map_err(io::Error::other)?;
                self.open = true;
            }
            while self.frame.can_collect() < buf.len() && !self.frame.is_finished() {
                let wanted = buf.len() - self.frame.can_collect();
                self.frame
                    .decode_blocks(
                        &mut self.source,
                        ruzstd::decoding::BlockDecodingStrategy::UptoBytes(wanted),
                    )
                    .map_err(io::Error::other)?;
            }
            let read = self.frame.read(buf)?;
            if read > 0 {
                return Ok(read);
            }
            self.open = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};
    use std::sync::{Arc, Mutex};
    use std::thread;

    struct HttpResponse {
        status: &'static str,
        headers: Vec<String>,
        body: Vec<u8>,
        advertised_size: Option<usize>,
    }

    fn serve(
        responses: Vec<HttpResponse>,
    ) -> (
        String,
        reqwest::Url,
        Arc<Mutex<Vec<String>>>,
        thread::JoinHandle<()>,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let handle = thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let request = read_request(&mut stream);
                seen.lock().unwrap().push(request);
                let size = response.advertised_size.unwrap_or(response.body.len());
                let mut head = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {size}\r\nConnection: close\r\n",
                    response.status
                );
                for header in response.headers {
                    head.push_str(&header);
                    head.push_str("\r\n");
                }
                head.push_str("\r\n");
                stream.write_all(head.as_bytes()).unwrap();
                stream.write_all(&response.body).unwrap();
            }
        });
        (
            format!("http://{address}/installer.img.zst"),
            reqwest::Url::parse(&format!("http://{address}/")).unwrap(),
            requests,
            handle,
        )
    }

    fn read_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
        }
        String::from_utf8(bytes).unwrap()
    }

    fn test_client() -> reqwest::blocking::Client {
        tilo_net::http::blocking_client().build().unwrap()
    }

    /// Architecture-matched media selection.
    #[test]
    fn media_selection_matches_the_native_host_architecture_only() {
        // Phase `development_catalog_is_well_formed`.
        {
            assert!(bundled_releases().is_ok());
        }

        // Phase `native_arm64_host_selects_only_arm64_media_regardless_of_imager_build`.
        {
            let releases =
                releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
            assert_eq!(releases.len(), 1);
            assert_eq!(releases[0].name, "Tilo aarch64");
            assert!(releases[0].url.ends_with("tilo-aarch64.img.zst"));
            assert_eq!(releases[0].image_sha256, "b".repeat(64));
        }

        // Phase `native_x86_64_host_selects_only_x86_64_media`.
        {
            let releases =
                releases_for_host(&catalog_with_both_architectures(), Ok(Some("x86_64"))).unwrap();
            assert_eq!(releases.len(), 1);
            assert_eq!(releases[0].name, "Tilo x86_64");
        }

        // Phase `native_arm64_host_does_not_fall_back_when_only_x86_media_is_published`.
        {
            let mut catalog: serde_json::Value =
                serde_json::from_str(&catalog_with_both_architectures()).unwrap();
            catalog["releases"]
                .as_array_mut()
                .unwrap()
                .retain(|release| release["arch"] == "x86_64");
            assert!(
                releases_for_host(&catalog.to_string(), Ok(Some("aarch64")))
                    .unwrap()
                    .is_empty()
            );
        }

        // Phase `unknown_or_failed_native_detection_never_offers_media`.
        {
            let catalog = catalog_with_both_architectures();
            assert!(releases_for_host(&catalog, Ok(None)).unwrap().is_empty());
            assert_eq!(
                releases_for_host(&catalog, Err("host detection failed".into())).unwrap_err(),
                "host detection failed"
            );
        }

        // Phase `arm64_selection_preserves_catalog_metadata_validation`.
        {
            let catalog = catalog_with_both_architectures();
            for invalid in [
                catalog.replace("code.tilo.so", "untrusted.example"),
                catalog.replace(&"a".repeat(64), "invalid-digest"),
                catalog.replace("\"image_size\":64", "\"image_size\":0"),
            ] {
                assert!(releases_for_host(&invalid, Ok(Some("aarch64"))).is_err());
            }
        }
    }

    /// A medium prepared for another computer selects that computer's
    /// architecture from the verified catalog, never the preparation host's.
    #[test]
    fn another_computers_architecture_selects_its_own_verified_payload() {
        let catalog = catalog_with_both_architectures();
        let arm = releases_for_arch(&catalog, "aarch64").unwrap();
        assert_eq!(arm.len(), 1);
        assert_eq!(arm[0].architecture, "aarch64");
        assert_eq!(arm[0].image_sha256, "b".repeat(64));
        let x86 = releases_for_arch(&catalog, "x86_64").unwrap();
        assert_eq!(x86.len(), 1);
        assert_eq!(x86[0].architecture, "x86_64");

        // The embedded catalog is validated for the target architecture even
        // when this build runs elsewhere.
        assert!(bundled_releases_for_arch("aarch64").is_ok());
        assert!(bundled_releases_for_arch("x86_64").is_ok());
        assert!(bundled_releases_for_arch("riscv64").is_err());

        // A missing target payload stays empty rather than falling back to
        // the preparation host's architecture.
        let mut only_x86: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        only_x86["releases"]
            .as_array_mut()
            .unwrap()
            .retain(|release| release["arch"] == "x86_64");
        assert!(
            releases_for_arch(&only_x86.to_string(), "aarch64")
                .unwrap()
                .is_empty()
        );

        // Every entry is validated before the target filter, including a
        // corrupt payload for another architecture.
        let mut corrupt: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        corrupt["releases"][1]["archive_sha256"] = serde_json::json!("not-a-digest");
        assert!(releases_for_arch(&corrupt.to_string(), "x86_64").is_err());
    }

    /// The receiving helper resolves an internal installer bundle for the
    /// target architecture the medium names, never for the preparing host.
    #[test]
    fn internal_selection_follows_the_target_architecture() {
        let entries = ["x86_64", "aarch64"].map(|arch| {
            serde_json::json!({
                "purpose": "tilo-internal-installer",
                "layout": "internal-bundle-v2",
                "release_id": format!("release-{arch}"),
                "arch": arch,
                "manifest_url": format!("https://code.tilo.so/tilo/releases/download/{arch}/manifest.json"),
                "manifest_sha256": "a".repeat(64),
                "manifest_size": 128,
            })
        });
        let catalog = serde_json::json!({
            "schema": 1,
            "tag": "imager-v1.0.0",
            "releases": entries,
        })
        .to_string();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("internal-catalog.json");
        std::fs::write(&path, catalog).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_STAGING_TEST_CATALOG", &path);

        let arm = bundled_internal_releases_for_arch("aarch64").unwrap();
        assert_eq!(arm.len(), 1);
        assert_eq!(arm[0].architecture(), "aarch64");
        assert_eq!(arm[0].release_id(), "release-aarch64");
        let x86 = bundled_internal_releases_for_arch("x86_64").unwrap();
        assert_eq!(x86.len(), 1);
        assert_eq!(x86[0].architecture(), "x86_64");
        assert!(bundled_internal_releases_for_arch("riscv64").is_err());
        assert!(
            bundled_internal_releases_for_arch(if std::env::consts::ARCH == "aarch64" {
                "x86_64"
            } else {
                "aarch64"
            })
            .is_ok()
        );
    }

    #[test]
    fn target_policy_selection_is_architecture_bound_and_absent_until_published() {
        assert_eq!(bundled_target_policy_for_arch("x86_64").unwrap(), None);
        assert!(bundled_target_policy_for_arch("riscv64").is_err());
        let entry = |arch: &str| {
            serde_json::json!({
                "purpose": "tilo-target-policy",
                "layout": "target-policy-bundle-v1",
                "release_id": "imager-v1.0.0",
                "arch": arch,
                "generation": 11,
                "target_uki_sha256": "b".repeat(64),
                "url": format!("https://code.tilo.so/releases/{arch}/target-policy.tar"),
                "archive_sha256": "c".repeat(64),
                "archive_size": 4096,
            })
        };
        let catalog = serde_json::json!({
            "schema": 1,
            "tag": "imager-v1.0.0",
            "releases": [entry("x86_64"), entry("aarch64")],
        })
        .to_string();
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("target-policy-catalog.json");
        std::fs::write(&path, &catalog).unwrap();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_STAGING_TEST_CATALOG", &path);

        let x86 = bundled_target_policy_for_arch("x86_64").unwrap().unwrap();
        assert_eq!(x86.generation, 11);
        assert_eq!(x86.target_uki_sha256, [0xbb; 32]);
        assert_eq!(
            bundled_target_policy_for_arch("aarch64")
                .unwrap()
                .unwrap()
                .target_uki_sha256,
            [0xbb; 32]
        );

        let mut duplicate: serde_json::Value = serde_json::from_str(&catalog).unwrap();
        duplicate["releases"]
            .as_array_mut()
            .unwrap()
            .push(entry("x86_64"));
        std::fs::write(&path, duplicate.to_string()).unwrap();
        assert!(
            bundled_target_policy_for_arch("x86_64").is_err(),
            "two target policies for one architecture are ambiguous"
        );
    }

    fn catalog_with_both_architectures() -> String {
        let releases = ["x86_64", "aarch64"].map(|arch| {
            serde_json::json!({
                "name": format!("Tilo {arch}"),
                "version": "1.0.0",
                "channel": "stable",
                "arch": arch,
                "url": format!("https://code.tilo.so/tilo/releases/releases/download/imager-v1.0.0/tilo-{arch}.img.zst"),
                "archive_sha256": "a".repeat(64),
                "archive_size": 32,
                "image_sha256": "b".repeat(64),
                "image_size": 64,
            })
        });
        serde_json::json!({
            "schema": 1,
            "tag": "imager-v1.0.0",
            "releases": releases,
        })
        .to_string()
    }

    fn catalog_with_hardware_variant() -> serde_json::Value {
        let mut catalog: serde_json::Value =
            serde_json::from_str(&catalog_with_both_architectures()).unwrap();
        catalog["schema"] = 2.into();
        for release in catalog["releases"].as_array_mut().unwrap() {
            release["hardware_profile"] = "generic".into();
        }
        let mut board = catalog["releases"][1].clone();
        board["hardware_profile"] = "lenovo-83ed".into();
        board["image_sha256"] = "c".repeat(64).into();
        board["provenance"] = serde_json::json!({
            "source_image_id": format!("sha256:{}", "d".repeat(64)),
            "input_qcow2_sha256": "e".repeat(64),
            "hardware_profile_sha256": "f".repeat(64),
            "target_update_ref": "code.tilo.so/tilo/tilo:board-fixture",
        });
        catalog["releases"].as_array_mut().unwrap().push(board);
        catalog
    }

    /// Hardware variants, legacy catalogs and v2 profiles.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn hardware_profiles_are_explicit_data_driven_and_provenance_bound() {
        // Phase `hardware_variants_keep_distinct_authenticated_selection_and_source_labels`.
        {
            let releases = releases_for_host(
                &catalog_with_hardware_variant().to_string(),
                Ok(Some("aarch64")),
            )
            .unwrap();
            assert_eq!(releases.len(), 2);
            assert_eq!(releases[0].hardware_profile, "generic");
            assert_eq!(releases[1].hardware_profile, "lenovo-83ed");
            assert_ne!(releases[0].selection_id(), releases[1].selection_id());
            let board = releases[1].to_source();
            assert!(board.name.contains("lenovo-83ed only"));
            assert!(board.label.contains("lenovo-83ed only"));
            assert_eq!(
                board.expected_sha256.as_deref(),
                Some("c".repeat(64).as_str())
            );
            assert!(!releases[0].matches_source(&board));
            assert!(releases[1].matches_source(&board));
        }

        // Phase `legacy_catalogs_mean_generic_and_cannot_silently_claim_a_board`.
        {
            let legacy =
                releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
            assert_eq!(legacy[0].hardware_profile, "generic");
            let mut catalog = catalog_with_hardware_variant();
            catalog["schema"] = 1.into();
            assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
        }

        // Phase `generic_media_provenance_does_not_require_a_board_profile_payload`.
        {
            let mut catalog: serde_json::Value =
                serde_json::from_str(&catalog_with_both_architectures()).unwrap();
            catalog["releases"][1]["provenance"] = serde_json::json!({
                "source_image_id": format!("sha256:{}", "d".repeat(64)),
                "input_qcow2_sha256": "e".repeat(64),
                "hardware_profile_sha256": null,
                "target_update_ref": null,
            });
            let releases = releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).unwrap();
            assert_eq!(releases[0].hardware_profile, "generic");
        }

        // Phase `v2_requires_safe_explicit_unique_profiles_and_bound_provenance`.
        {
            for invalid in [
                serde_json::Value::Null,
                "".into(),
                "../board".into(),
                "Board".into(),
            ] {
                let mut catalog = catalog_with_hardware_variant();
                catalog["releases"][2]["hardware_profile"] = invalid;
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
            }
            for field in ["hardware_profile", "provenance"] {
                let mut catalog = catalog_with_hardware_variant();
                catalog["releases"][2]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
            }
            for field in [
                "source_image_id",
                "hardware_profile_sha256",
                "input_qcow2_sha256",
                "target_update_ref",
            ] {
                let mut catalog = catalog_with_hardware_variant();
                catalog["releases"][2]["provenance"][field] = "untrusted".into();
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
            }
            for origin in [
                "registry.tilo.so/tilo/tilo:gen7".to_owned(),
                format!("code.tilo.so/tilo/tilo@sha256:{}", "a".repeat(64)),
            ] {
                let mut catalog = catalog_with_hardware_variant();
                catalog["releases"][2]["provenance"]["target_update_ref"] = origin.into();
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_ok());
            }
            for origin in [
                "registry.tilo.so/tilo/tilo-vm:gen7",
                "registry.tilo.so/tilo/tilo:",
                "code.tilo.so/tilo/tilo@sha256:short",
                "example.org/tilo/tilo:gen7",
            ] {
                let mut catalog = catalog_with_hardware_variant();
                catalog["releases"][2]["provenance"]["target_update_ref"] = origin.into();
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
            }
            let mut catalog = catalog_with_hardware_variant();
            let duplicate = catalog["releases"][2].clone();
            catalog["releases"].as_array_mut().unwrap().push(duplicate);
            assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
        }

        // Phase `hardware_profile_ids_are_data_driven_not_a_model_allowlist`.
        {
            let mut catalog = catalog_with_hardware_variant();
            catalog["releases"][2]["hardware_profile"] = "another-reviewed-board".into();
            let releases = releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).unwrap();
            assert!(
                releases[1]
                    .to_source()
                    .name
                    .contains("another-reviewed-board only")
            );
        }
    }

    /// Digest validation, verified source identity and redirect checks.
    #[test]
    fn release_sources_keep_strict_digests_identity_and_trusted_redirects() {
        // Phase `digest_validation_is_strict`.
        {
            assert!(validate_digest(&"a".repeat(64)).is_ok());
            assert!(validate_digest(&"z".repeat(64)).is_err());
            assert!(validate_digest("abc").is_err());
        }

        // Phase `verified_release_source_keeps_catalog_digest_and_file_identity`.
        {
            let image = tempfile::NamedTempFile::new().unwrap();
            let release = TiloRelease {
                name: "Tilo".into(),
                version: "1.0".into(),
                size: 1,
                channel: "stable".into(),
                architecture: "aarch64".into(),
                hardware_profile: "generic".into(),
                local_path: String::new(),
                url:
                    "https://code.tilo.so/tilo/releases/releases/download/imager-v1.0.0/tilo.img.zst"
                        .into(),
                archive_sha256: "a".repeat(64),
                archive_size: 1,
                image_sha256: "b".repeat(64),
                account_custody: None,
                home_archive: None,
            applications: None,
                original_source: None,
            };

            let source = source_for(&release, image.path()).unwrap();
            let expected = "b".repeat(64);
            assert_eq!(source.expected_sha256.as_deref(), Some(expected.as_str()));
            assert!(source.identity.is_some());
            assert_eq!(
                source.identity,
                crate::model::SourceIdentity::from_file(image.as_file()).unwrap()
            );
            let missing = image.path().with_extension("missing");
            assert!(source_for(&release, &missing).is_err());
        }

        // Phase `trusted_redirect_check_includes_scheme_host_and_port`.
        {
            let trusted = reqwest::Url::parse("https://code.tilo.so/").unwrap();
            assert!(same_origin(
                &reqwest::Url::parse("https://code.tilo.so/releases/file").unwrap(),
                &trusted
            ));
            assert!(!same_origin(
                &reqwest::Url::parse("https://code.tilo.so:444/releases/file").unwrap(),
                &trusted
            ));
            assert!(!same_origin(
                &reqwest::Url::parse("http://code.tilo.so/releases/file").unwrap(),
                &trusted
            ));
        }
    }

    /// Range resume, restart and oversized partials.
    #[test]
    fn release_download_resumes_only_valid_ranges() {
        // Phase `interrupted_download_keeps_bytes_and_resumes_with_a_valid_range`.
        {
            let payload = b"signed installer archive bytes";
            let split = 9;
            let (url, origin, requests, server) = serve(vec![
                HttpResponse {
                    status: "200 OK",
                    headers: vec![],
                    body: payload[..split].to_vec(),
                    advertised_size: Some(payload.len()),
                },
                HttpResponse {
                    status: "206 Partial Content",
                    headers: vec![format!(
                        "Content-Range: bytes {split}-{}/{}",
                        payload.len() - 1,
                        payload.len()
                    )],
                    body: payload[split..].to_vec(),
                    advertised_size: None,
                },
            ]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            let client = test_client();

            assert!(download_archive(&client, &url, &origin, &part, payload.len() as u64).is_err());
            assert_eq!(fs::read(&part).unwrap(), payload[..split]);
            download_archive(&client, &url, &origin, &part, payload.len() as u64).unwrap();
            assert_eq!(fs::read(&part).unwrap(), payload);
            server.join().unwrap();
            let requests = requests.lock().unwrap();
            assert!(!requests[0].contains("Range:"));
            assert!(requests[1].contains(&format!("range: bytes={split}-")));
            drop(requests);
        }

        // Phase `ignored_range_restarts_without_duplicating_the_partial`.
        {
            let payload = b"complete archive from a range-ignorant server";
            let (url, origin, requests, server) = serve(vec![HttpResponse {
                status: "200 OK",
                headers: vec![],
                body: payload.to_vec(),
                advertised_size: None,
            }]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            fs::write(&part, &payload[..8]).unwrap();

            download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
            assert_eq!(fs::read(&part).unwrap(), payload);
            server.join().unwrap();
            assert!(requests.lock().unwrap()[0].contains("range: bytes=8-"));
        }

        // Phase `mismatched_content_range_is_discarded_before_a_clean_restart`.
        {
            let payload = b"archive with authenticated final digest";
            let split = 7;
            let (url, origin, requests, server) = serve(vec![
                HttpResponse {
                    status: "206 Partial Content",
                    headers: vec![format!(
                        "Content-Range: bytes {}-{}/{}",
                        split + 1,
                        payload.len() - 1,
                        payload.len()
                    )],
                    body: payload[split..].to_vec(),
                    advertised_size: None,
                },
                HttpResponse {
                    status: "200 OK",
                    headers: vec![],
                    body: payload.to_vec(),
                    advertised_size: None,
                },
            ]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            fs::write(&part, &payload[..split]).unwrap();

            download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
            assert_eq!(fs::read(&part).unwrap(), payload);
            server.join().unwrap();
            let requests = requests.lock().unwrap();
            assert!(requests[0].contains(&format!("range: bytes={split}-")));
            assert!(!requests[1].contains("Range:"));
            assert!(!requests[1].contains("range:"));
            drop(requests);
        }

        // Phase `oversized_partial_is_removed_before_requesting_the_full_archive`.
        {
            let payload = b"small complete archive";
            let (url, origin, requests, server) = serve(vec![HttpResponse {
                status: "200 OK",
                headers: vec![],
                body: payload.to_vec(),
                advertised_size: None,
            }]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            fs::write(&part, vec![b'x'; payload.len() + 1]).unwrap();

            download_archive(&test_client(), &url, &origin, &part, payload.len() as u64).unwrap();
            assert_eq!(fs::read(&part).unwrap(), payload);
            server.join().unwrap();
            assert!(!requests.lock().unwrap()[0].contains("range:"));
        }
    }

    /// Transient failures, progress reporting and cancellation.
    #[test]
    fn release_download_failures_and_cancellation_keep_the_partial() {
        // Phase `transient_http_failure_does_not_destroy_a_partial_archive`.
        {
            let payload = b"partial archive survives";
            let (url, origin, requests, server) = serve(vec![HttpResponse {
                status: "503 Service Unavailable",
                headers: vec![],
                body: b"later".to_vec(),
                advertised_size: None,
            }]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            fs::write(&part, &payload[..6]).unwrap();

            assert!(
                download_archive(&test_client(), &url, &origin, &part, payload.len() as u64)
                    .is_err()
            );
            assert_eq!(fs::read(&part).unwrap(), payload[..6]);
            server.join().unwrap();
            assert!(requests.lock().unwrap()[0].contains("range: bytes=6-"));
        }

        // Phase `observed_download_reports_bounded_byte_progress`.
        {
            let payload = b"installer bytes with progress";
            let (url, origin, _requests, server) = serve(vec![HttpResponse {
                status: "200 OK",
                headers: vec![],
                body: payload.to_vec(),
                advertised_size: None,
            }]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            let mut events = Vec::new();

            download_archive_observed(
                &test_client(),
                &url,
                &origin,
                &part,
                payload.len() as u64,
                &DownloadControl::default(),
                &mut |event| events.push(event),
            )
            .unwrap();

            server.join().unwrap();
            assert!(events.iter().any(|event| matches!(
                event,
                DownloadEvent::Downloading { received, total }
                    if *received == payload.len() as u64 && *total == payload.len() as u64
            )));
        }

        // Phase `cancellation_preserves_the_resumable_partial_archive`.
        {
            let payload = b"more authenticated installer bytes";
            let split = 8;
            let (url, origin, _requests, server) = serve(vec![HttpResponse {
                status: "206 Partial Content",
                headers: vec![format!(
                    "Content-Range: bytes {split}-{}/{}",
                    payload.len() - 1,
                    payload.len()
                )],
                body: payload[split..].to_vec(),
                advertised_size: None,
            }]);
            let cache = tempfile::tempdir().unwrap();
            let part = cache.path().join("installer.part");
            fs::write(&part, &payload[..split]).unwrap();
            let control = DownloadControl::default();
            control.cancel();

            let error = download_archive_observed(
                &test_client(),
                &url,
                &origin,
                &part,
                payload.len() as u64,
                &control,
                &mut |_| {},
            )
            .unwrap_err();

            server.join().unwrap();
            assert!(error.contains("cancelled"));
            assert_eq!(fs::read(&part).unwrap(), payload[..split]);
        }
    }
}

#[cfg(test)]
mod internal_catalog_tests {
    use super::*;
    use serde_json::{Value, json};
    fn media() -> Value {
        json!({"name":"Tilo","version":"test","channel":"test","arch":"x86_64",
            "url":"https://code.tilo.so/test.img.zst","archive_sha256":"a".repeat(64),
            "archive_size":42,"image_sha256":"b".repeat(64),"image_size":84})
    }
    fn catalog(entry: Value) -> String {
        json!({"schema":1,"tag":"imager-vtest","releases":[entry]}).to_string()
    }
    /// Legacy media, internal purpose, unknown fields and published packages.
    #[test]
    fn internal_catalog_entries_parse_strictly_and_separately_from_media() {
        // Phase `legacy_media_still_parses`.
        {
            assert!(parse_catalog(&catalog(media())).is_ok());
        }

        // Phase `explicit_internal_purpose_never_falls_through_as_media`.
        {
            let mut value = media();
            value["purpose"] = json!("tilo-internal-installer");
            value["layout"] = json!("internal-bundle-v2");
            assert!(parse_catalog(&catalog(value)).is_err());
        }

        // Phase `catalog_rejects_unknown_fields_and_bad_foreign_entries`.
        {
            let mut value = media();
            value["unexpected"] = json!(true);
            assert!(parse_catalog(&catalog(value)).is_err());
            for (field, bad) in [
                ("archive_sha256", json!("invalid")),
                ("url", json!("https://evil.example/file")),
                ("image_size", json!(0)),
            ] {
                let mut value = media();
                value["arch"] = json!("aarch64");
                value[field] = bad;
                assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
            }
        }

        // Phase `loop_harness_catalog_shape_is_a_valid_catalog`.
        // The catalog `os/tests/staging_helper_loop.sh` writes for its synthetic
        // bundle (via `TILO_STAGING_TEST_CATALOG`) must pass the real parser.
        {
            let mut entry = internal(b"{}");
            entry["release_id"] = json!("loop-test");
            entry["manifest_url"] = json!("https://code.tilo.so/tilo/loop-test/manifest.json");
            let text =
                json!({"schema":1,"tag":"imager-v0.0.0-loop-test","releases":[entry]}).to_string();
            let parsed = parse_catalog(&text).unwrap();
            assert!(matches!(
                parsed.releases.as_slice(),
                [CatalogRelease::Internal(m)] if m.release_id == "loop-test"
            ));
        }

        // Phase `published_package_entries_parse_as_internal_releases`.
        {
            // The exact shape build-release-catalog.sh emits for a Forgejo package.
            let mut value = crate::setup::internal_bundle::tests::fixture();
            value["release_id"] = json!("imager-v1.2.3");
            let bytes = crate::setup::internal_bundle::tests::bytes(&value);
            let mut entry = internal(&bytes);
            entry["release_id"] = json!("imager-v1.2.3");
            entry["manifest_url"] = json!(
                "https://code.tilo.so/api/packages/tilo/generic/tilo-internal-installer/imager-v1.2.3/tilo-internal-installer-x86_64.json"
            );
            let raw =
                json!({"schema":1,"tag":"imager-v1.2.3","releases":[media(), entry]}).to_string();
            let catalog = parse_catalog(&raw).unwrap();
            assert_eq!(
                media_releases(parse_catalog(&raw).unwrap(), "x86_64").len(),
                1
            );
            let release = catalog
                .releases
                .into_iter()
                .find_map(|entry| match entry {
                    CatalogRelease::Internal(metadata) => Some(InternalRelease { metadata }),
                    CatalogRelease::Media(_) | CatalogRelease::TargetPolicy(_) => None,
                })
                .unwrap();
            assert_eq!(release.release_id(), "imager-v1.2.3");
            assert!(release.verify_manifest(&bytes).is_ok());
        }
    }
    #[test]
    fn media_catalog_retains_strict_account_consumer_proof_bound_to_image() {
        let mut value = media();
        value["account_custody"] = json!({"schema":1,"protected_grant_protocol":1,
            "credential_helper_sha256":"1".repeat(64),"runtime_root_hash":"2".repeat(64),
            "runtime_probe_sha256":"3".repeat(64),"runtime_library_sha256":"4".repeat(64),
            "runtime_version":"test-1","runtime_library":"lib/libruntime_dylib.so"});
        let selected = media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64");
        assert_eq!(selected.len(), 1);
        assert_eq!(
            selected[0].image_sha256,
            value["image_sha256"].as_str().unwrap()
        );
        assert_eq!(
            selected[0]
                .account_custody
                .as_ref()
                .unwrap()
                .protected_grant_protocol,
            1
        );
        for (key, invalid) in [
            ("schema", json!(2)),
            ("protected_grant_protocol", json!(2)),
            ("runtime_library", json!("../libruntime_dylib.so")),
            ("unexpected", json!(true)),
        ] {
            let mut altered = value.clone();
            altered["account_custody"][key] = invalid;
            assert!(parse_catalog(&catalog(altered)).is_err());
        }
        assert!(
            media_releases(parse_catalog(&catalog(media())).unwrap(), "x86_64")[0]
                .account_custody
                .is_none()
        );
    }

    fn internal(manifest: &[u8]) -> Value {
        json!({"purpose":"tilo-internal-installer","layout":"internal-bundle-v2",
            "release_id":"fixture-only","arch":"x86_64",
            "manifest_url":"https://code.tilo.so/fixture/manifest.json",
            "manifest_sha256":hex::encode(Sha256::digest(manifest)),"manifest_size":manifest.len()})
    }
    fn selection(value: Value) -> InternalRelease {
        match parse_catalog(&catalog(value))
            .unwrap()
            .releases
            .pop()
            .unwrap()
        {
            CatalogRelease::Internal(metadata) => InternalRelease { metadata },
            _ => panic!("internal entry became media"),
        }
    }
    /// Internal entry binding, selection mismatches and strict profiles.
    #[test]
    fn internal_entries_bind_manifest_bytes_release_and_architecture() {
        // Phase `internal_entries_remain_separate_and_bind_exact_manifest_bytes`.
        {
            let bytes = crate::setup::internal_bundle::tests::bytes(
                &crate::setup::internal_bundle::tests::fixture(),
            );
            let value = internal(&bytes);
            assert!(
                media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64")
                    .is_empty()
            );
            let release = selection(value);
            assert!(release.verify_manifest(&bytes).is_ok());
            let mut longer = bytes.clone();
            longer.push(b' ');
            assert!(release.verify_manifest(&longer).is_err());
            let mut changed_manifest = crate::setup::internal_bundle::tests::fixture();
            changed_manifest["boot_binding"]["uki_sha256"] = json!("c".repeat(64));
            let changed = crate::setup::internal_bundle::tests::bytes(&changed_manifest);
            assert_eq!(changed.len(), bytes.len());
            let valid = crate::setup::internal_bundle::validate_internal_bundle(&changed).unwrap();
            assert_eq!(valid.release_id(), release.release_id());
            assert_eq!(valid.architecture(), release.architecture());
            assert!(release.verify_manifest(&changed).is_err());
        }

        // Phase `internal_selection_rejects_release_and_architecture_mismatch`.
        {
            let bytes = crate::setup::internal_bundle::tests::bytes(
                &crate::setup::internal_bundle::tests::fixture(),
            );
            for (key, value) in [
                ("release_id", json!("other-release")),
                ("arch", json!("aarch64")),
            ] {
                let mut entry = internal(&bytes);
                entry[key] = value;
                assert!(
                    selection(entry).verify_manifest(&bytes).is_err(),
                    "accepted {key}"
                );
            }
        }

        // Phase `explicit_media_pair_and_internal_catalog_profiles_are_strict`.
        {
            let mut tagged = media();
            tagged["purpose"] = json!("removable-media");
            tagged["layout"] = json!("whole-disk-image");
            assert_eq!(
                media_releases(parse_catalog(&catalog(tagged.clone())).unwrap(), "x86_64").len(),
                1
            );
            for bad in [Value::Null, json!("unknown"), json!(true)] {
                let mut value = tagged.clone();
                value["purpose"] = bad;
                assert!(parse_catalog(&catalog(value)).is_err());
            }
            tagged.as_object_mut().unwrap().remove("layout");
            assert!(parse_catalog(&catalog(tagged)).is_err());
            let bytes = crate::setup::internal_bundle::tests::bytes(
                &crate::setup::internal_bundle::tests::fixture(),
            );
            for (field, bad) in [
                ("layout", json!("whole-disk-image")),
                // Schema-1 manifests are retired; so are the catalog tags naming them.
                ("layout", json!("internal-bundle-v1")),
                // Only the exact purpose names an internal installer.
                ("purpose", json!("internal-installer")),
                ("manifest_size", json!(65537)),
                ("manifest_sha256", json!("A".repeat(64))),
                ("purpose", json!("removable-media")),
                ("manifest_url", json!("https://code.tilo.so:444/manifest")),
            ] {
                let mut value = internal(&bytes);
                value[field] = bad;
                assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
            }
            assert!(bundled_internal_releases().unwrap().is_empty());
            assert!(bundled_releases().unwrap().is_empty());
            let raw = catalog(media()).replacen("\"arch\":", "\"arch\":\"x86_64\",\"arch\":", 1);
            assert!(parse_catalog(&raw).is_err());
        }
    }
    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
    /// Catalog records keep their JSON object representation: a positional
    /// array, an object-shaped tag or a duplicated key never parses.
    #[test]
    fn catalog_representation_requires_objects_string_tags_and_unique_keys() {
        let bytes = crate::setup::internal_bundle::tests::bytes(
            &crate::setup::internal_bundle::tests::fixture(),
        );

        // The catalog itself must be an object.
        let raw = catalog(media());
        assert!(parse_catalog(&raw).is_ok());
        let value: Value = serde_json::from_str(&raw).unwrap();
        let array = positional_record(&value, "schema tag releases");
        assert!(parse_catalog(&array.to_string()).is_err());

        let mut tagged_media = media();
        tagged_media["purpose"] = json!("removable-media");
        tagged_media["layout"] = json!("whole-disk-image");
        for (case, entry, fields) in [
            (
                "media",
                tagged_media,
                "purpose layout name version channel arch url archive_sha256 archive_size image_sha256 image_size",
            ),
            (
                "internal",
                internal(&bytes),
                "purpose layout release_id arch manifest_url manifest_sha256 manifest_size",
            ),
        ] {
            assert!(
                parse_catalog(&catalog(entry.clone())).is_ok(),
                "case {case}"
            );
            // The entry must be an object, not a positional record.
            let array = positional_record(&entry, fields);
            assert!(parse_catalog(&catalog(array)).is_err(), "case {case}");
            // Purpose and layout must be strings, not object-shaped tags.
            let accepted: Vec<_> = ["purpose", "layout"]
                .into_iter()
                .filter(|field| {
                    let mut value = entry.clone();
                    value[*field] = json!({entry[*field].as_str().unwrap():null});
                    parse_catalog(&catalog(value)).is_ok()
                })
                .collect();
            assert!(
                accepted.is_empty(),
                "case {case}: accepted object-shaped tags: {accepted:?}"
            );
        }

        // Duplicate keys are not collapsed into one.
        for entry in [media(), internal(&bytes)] {
            let raw = catalog(entry);
            assert!(parse_catalog(&raw).is_ok());
            for (marker, replacement) in [
                ("\"schema\":1", "\"schema\":1,\"schema\":1"),
                (
                    "\"arch\":\"x86_64\"",
                    "\"arch\":\"x86_64\",\"arch\":\"x86_64\"",
                ),
            ] {
                let changed = raw.replacen(marker, replacement, 1);
                assert_ne!(changed, raw);
                assert!(parse_catalog(&changed).is_err());
            }
        }
    }

    #[test]
    fn zstd_frames_decode_back_to_back_frames_in_small_reads() {
        use ruzstd::encoding::{CompressionLevel, compress_to_vec};
        let first: Vec<u8> = (0..300_000_u32).map(|i| (i % 251) as u8).collect();
        let second = b"second frame".repeat(1000);
        let mut archive = compress_to_vec(first.as_slice(), CompressionLevel::Fastest);
        archive.extend(compress_to_vec(
            second.as_slice(),
            CompressionLevel::Fastest,
        ));
        let mut frames = ZstdFrames::new(BufReader::new(archive.as_slice()));
        let mut expanded = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = frames.read(&mut chunk).unwrap();
            if read == 0 {
                break;
            }
            expanded.extend_from_slice(&chunk[..read]);
        }
        assert_eq!(expanded.len(), first.len() + second.len());
        assert!(expanded.starts_with(&first) && expanded.ends_with(&second));
        assert!(
            ZstdFrames::new(BufReader::new(&b"not zstd"[..]))
                .read(&mut chunk)
                .is_err()
        );
    }
    #[test]
    fn media_catalog_home_proof_is_strict_and_bound_to_selected_image() {
        let mut value = media();
        let legacy = media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64");
        assert!(
            crate::setup::home_admission::require(legacy[0].home_archive.as_ref(), None).is_ok()
        );
        assert!(
            crate::setup::home_admission::require(legacy[0].home_archive.as_ref(), Some(2))
                .is_err()
        );
        value["home_archive"] = json!({"schema":1,"archive_protocol":2,"seed_protocol":1,
            "firstboot_user_sha256":"c".repeat(64),"seed_helper_sha256":"d".repeat(64),
            "prepared_helper_sha256":"a".repeat(64),"tar_sha256":"b".repeat(64)});
        let selected = media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64");
        assert_eq!(
            selected[0].image_sha256,
            value["image_sha256"].as_str().unwrap()
        );
        assert!(
            crate::setup::home_admission::require(selected[0].home_archive.as_ref(), Some(2))
                .is_ok()
        );
        for bad in [
            json!(null),
            json!([]),
            json!(true),
            json!([
                1,
                2,
                1,
                "c".repeat(64),
                "d".repeat(64),
                "a".repeat(64),
                "b".repeat(64)
            ]),
        ] {
            let mut altered = value.clone();
            altered["home_archive"] = bad;
            assert!(parse_catalog(&catalog(altered)).is_err());
        }
        for key in ["firstboot_user_sha256", "seed_helper_sha256"] {
            let mut altered = value.clone();
            altered["home_archive"][key] = json!("X".repeat(64));
            assert!(parse_catalog(&catalog(altered)).is_err());
        }
        let mut altered = value.clone();
        altered["home_archive"]["seed_protocol"] = json!(2);
        assert!(parse_catalog(&catalog(altered)).is_err());
        value["home_archive"]["archive_protocol"] = json!(1);
        assert!(parse_catalog(&catalog(value)).is_err());
    }
    #[test]
    fn media_catalog_applications_proof_is_closed_and_exact_selected_image_bound() {
        let mut value = media();
        let legacy = media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64");
        assert!(
            crate::setup::applications_admission::require(legacy[0].applications.as_ref(), None)
                .is_ok()
        );
        assert!(
            crate::setup::applications_admission::require(legacy[0].applications.as_ref(), Some(2))
                .is_err()
        );
        value["applications"] = json!({"schema":1,"subset_protocol":2,"prepared_helper_sha256":"a".repeat(64),"qualification_script_sha256":"b".repeat(64),"native_qualification_sha256":"c".repeat(64)});
        let selected = media_releases(parse_catalog(&catalog(value.clone())).unwrap(), "x86_64");
        assert!(
            crate::setup::applications_admission::require(
                selected[0].applications.as_ref(),
                Some(2)
            )
            .is_ok()
        );
        assert_eq!(
            selected[0].image_sha256,
            value["image_sha256"].as_str().unwrap()
        );
        for bad in [json!(null), json!([]), json!(true)] {
            let mut changed = value.clone();
            changed["applications"] = bad;
            assert!(parse_catalog(&catalog(changed)).is_err());
        }
        for field in [
            "prepared_helper_sha256",
            "qualification_script_sha256",
            "native_qualification_sha256",
        ] {
            let mut changed = value.clone();
            changed["applications"][field] = json!("X".repeat(64));
            assert!(parse_catalog(&catalog(changed)).is_err());
        }
        value["applications"]["subset_protocol"] = json!(1);
        assert!(parse_catalog(&catalog(value)).is_err());
    }

    #[test]
    fn media_catalog_original_syntax_is_closed_and_bound_to_selected_architecture() {
        let mut entry = media();
        let legacy = media_releases(parse_catalog(&catalog(entry.clone())).unwrap(), "x86_64");
        assert!(legacy[0].original_source.is_none());
        assert!(crate::setup::original_admission::require(None, Some(5)).is_err());
        let proof = crate::setup::original_admission::syntax_fixture();
        entry["original_source"] = serde_json::to_value(&proof).unwrap();
        let selected = media_releases(parse_catalog(&catalog(entry.clone())).unwrap(), "x86_64");
        assert_eq!(selected[0].original_source.as_ref(), Some(&proof));
        for bad in [json!(null), json!([]), json!(true)] {
            let mut changed = entry.clone();
            changed["original_source"] = bad;
            assert!(parse_catalog(&catalog(changed)).is_err());
        }
        let mut changed = entry.clone();
        changed["original_source"]["capture_pins"]["arch"] = json!("aarch64");
        assert!(parse_catalog(&catalog(changed)).is_err());
        entry["original_source"]["qualified"] = json!(true);
        assert!(parse_catalog(&catalog(entry)).is_err());
    }
}
