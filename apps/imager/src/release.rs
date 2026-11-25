//! The trusted Tilo release catalogue, and downloading a medium whose bytes it verified.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;
use sha2::{Digest, Sha256};
use tilo_net::http::fetch;

use crate::model::{Source, TiloRelease};

const BUNDLED_CATALOG: &str = include_str!(concat!(env!("OUT_DIR"), "/release-catalog.json"));

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
    hardware_profile: String,
    #[serde(default)]
    provenance: Option<MediaProvenance>,
    url: String,
    archive_sha256: String,
    archive_size: u64,
    image_sha256: String,
    image_size: u64,
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

#[derive(Clone, Debug)]
pub enum DownloadEvent {
    Downloading { received: u64, total: u64 },
    Verifying,
    Expanding { written: u64, total: u64 },
}

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
        let valid = shape.is_object()
            && shape
                .get("releases")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|entries| {
                    entries.iter().all(|entry| {
                        entry.is_object()
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
    if catalog.schema != 2 {
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
                let profile = media.hardware_profile.as_str();
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
        if unqualified_architecture(arch).is_some() {
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
                hardware_profile: release.hardware_profile,
                local_path: String::new(),
                url: release.url,
                archive_sha256: release.archive_sha256,
                archive_size: release.archive_size,
                image_sha256: release.image_sha256,
            })
        })
        .collect()
}

pub const SUPPORTED_ARCHITECTURES: [&str; 2] = ["x86_64", "aarch64"];

pub const ARCHITECTURE_UNSUPPORTED: &str = "installer-preview-target-architecture-unsupported";

type ArchitectureBoundary = (&'static str, fn(&str) -> bool);

const UNQUALIFIED: [ArchitectureBoundary; 3] = [
    (
        "installer-preview-target-architecture-unsupported-processor-size",
        is_thirty_two_bit,
    ),
    (
        "installer-preview-target-architecture-unsupported-byte-order",
        is_big_endian,
    ),
    (
        "installer-preview-target-architecture-unsupported-graphics",
        is_sparc,
    ),
];

#[must_use]
pub fn unqualified_architecture(architecture: &str) -> Option<&'static str> {
    UNQUALIFIED
        .iter()
        .find(|(_, recognizes)| recognizes(architecture))
        .map(|(key, _)| *key)
}

fn is_thirty_two_bit(architecture: &str) -> bool {
    matches!(
        architecture,
        "i386" | "i486" | "i586" | "i686" | "x86" | "armv6l" | "armv7l" | "armv7hl" | "armhf"
    )
}

fn is_big_endian(architecture: &str) -> bool {
    matches!(
        architecture,
        "s390" | "s390x" | "ppc" | "powerpc" | "ppc64" | "mips" | "mips64"
    )
}

fn is_sparc(architecture: &str) -> bool {
    matches!(architecture, "sparc" | "sparc64")
}

pub fn bundled_releases() -> Result<Vec<TiloRelease>, String> {
    releases_for_host(BUNDLED_CATALOG, crate::host::architecture())
}

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
    if unqualified_architecture(architecture).is_some() {
        return Err("Unsupported release architecture.".into());
    }
    let catalog = parse_catalog(catalog_json)?;
    Ok(media_releases(catalog, architecture))
}

pub fn bundled_internal_releases() -> Result<Vec<InternalRelease>, String> {
    bundled_internal_releases_for_arch(std::env::consts::ARCH)
}

pub fn bundled_internal_releases_for_arch(
    architecture: &str,
) -> Result<Vec<InternalRelease>, String> {
    if unqualified_architecture(architecture).is_some() {
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

fn download_archive(
    downloader: &tilo_net::http::fetch::Downloader,
    url: reqwest::Url,
    archive_part: &Path,
    expected_size: u64,
    expected_sha256: &str,
    control: &DownloadControl,
    notify: &mut impl FnMut(DownloadEvent),
) -> Result<(), String> {
    let held = archive_part.metadata().map_or(0, |metadata| metadata.len());
    if held > expected_size {
        fs::remove_file(archive_part)
            .map_err(|error| format!("Could not replace an oversized partial download: {error}"))?;
    }
    let mut archive = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(archive_part)
        .map_err(|error| format!("Could not create the download cache: {error}"))?;
    notify(DownloadEvent::Downloading {
        received: archive.metadata().map_or(0, |metadata| metadata.len()),
        total: expected_size,
    });
    let result = downloader.fetch(
        fetch::Request {
            url,
            size: expected_size,
            sha256: Some(expected_sha256),
            cancel: &control.0,
            pause: None,
        },
        &mut archive,
        &mut (),
        |update| match update.phase {
            fetch::Phase::Verifying => notify(DownloadEvent::Verifying),
            fetch::Phase::Requesting | fetch::Phase::Receiving | fetch::Phase::Resetting => {
                notify(DownloadEvent::Downloading {
                    received: update.received,
                    total: update.total,
                });
            }
            fetch::Phase::Retrying | fetch::Phase::Complete => {}
        },
    );
    // A partial download that is the wrong size or digest must not be resumed
    // from the next time this installer runs.
    if matches!(
        &result,
        Err(fetch::Error::Digest | fetch::Error::Size { .. })
    ) {
        let _ = fs::remove_file(archive_part);
    }
    result.map_err(archive_error)
}

/// What the person sees when a download did not produce the signed installer.
fn archive_error(error: fetch::Error) -> String {
    match error {
        fetch::Error::Cancelled => "Tilo installer download cancelled.".into(),
        fetch::Error::Digest => {
            "The downloaded Tilo installer did not match its signed catalog.".into()
        }
        fetch::Error::Size { .. } => "The Tilo installer download has an unexpected size.".into(),
        fetch::Error::Incomplete(_) | fetch::Error::Paused => {
            "The Tilo installer download was interrupted before it completed.".into()
        }
        fetch::Error::Io(error) => format!("Could not save the Tilo installer: {error}"),
        fetch::Error::Client(detail) => format!("Could not initialize the downloader: {detail}"),
        fetch::Error::Trusted(detail) | fetch::Error::Protocol(detail) => {
            format!("Could not download the Tilo installer: {detail}")
        }
        fetch::Error::Hook(detail) => format!("Could not save the Tilo installer: {detail}"),
    }
}

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
    let trusted_origin = reqwest::Url::parse("https://code.tilo.so/")
        .map_err(|error| format!("Could not initialize the trusted release origin: {error}"))?;
    let downloader = fetch::Downloader::new(
        trusted_origin,
        std::time::Duration::from_secs(60 * 60),
        Some("Tilo-Imager/1"),
    )
    .map_err(|error| format!("Could not initialize the downloader: {error}"))?;
    let url = reqwest::Url::parse(&release.url)
        .map_err(|error| format!("The Tilo installer has an invalid download URL: {error}"))?;
    download_archive(
        &downloader,
        url,
        &archive_part,
        release.archive_size,
        &release.archive_sha256,
        control,
        notify,
    )?;

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
    tilo_test_env::scenario! {
        fn a_release_is_selected_for_the_architecture_it_was_built_for;

        phase media_selection_matches_the_native_host_architecture_only;
        {
            {
                assert!(bundled_releases().is_ok());
            }

            {
                let releases =
                    releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
                assert_eq!(releases.len(), 1);
                assert_eq!(releases[0].name, "Tilo aarch64");
                assert!(releases[0].url.ends_with("tilo-aarch64.img.zst"));
                assert_eq!(releases[0].image_sha256, "b".repeat(64));
            }

            {
                let releases =
                    releases_for_host(&catalog_with_both_architectures(), Ok(Some("x86_64"))).unwrap();
                assert_eq!(releases.len(), 1);
                assert_eq!(releases[0].name, "Tilo x86_64");
            }

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

            {
                let catalog = catalog_with_both_architectures();
                assert!(releases_for_host(&catalog, Ok(None)).unwrap().is_empty());
                assert_eq!(
                    releases_for_host(&catalog, Err("host detection failed".into())).unwrap_err(),
                    "host detection failed"
                );
            }

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

        phase another_computers_architecture_selects_its_own_verified_payload;
        {
            let catalog = catalog_with_both_architectures();
            let arm = releases_for_arch(&catalog, "aarch64").unwrap();
            assert_eq!(arm.len(), 1);
            assert_eq!(arm[0].architecture, "aarch64");
            assert_eq!(arm[0].image_sha256, "b".repeat(64));
            let x86 = releases_for_arch(&catalog, "x86_64").unwrap();
            assert_eq!(x86.len(), 1);
            assert_eq!(x86[0].architecture, "x86_64");

            assert!(bundled_releases_for_arch("aarch64").is_ok());
            assert!(bundled_releases_for_arch("x86_64").is_ok());
            assert!(bundled_releases_for_arch("i686").is_err());
            assert!(bundled_releases_for_arch("riscv64").unwrap().is_empty());

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

            let mut corrupt: serde_json::Value = serde_json::from_str(&catalog).unwrap();
            corrupt["releases"][1]["archive_sha256"] = serde_json::json!("not-a-digest");
            assert!(releases_for_arch(&corrupt.to_string(), "x86_64").is_err());
        }

        phase internal_selection_follows_the_target_architecture;
        {
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
                "schema": 2,
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
            assert!(bundled_internal_releases_for_arch("i686").is_err());
            assert!(
                bundled_internal_releases_for_arch("riscv64")
                    .unwrap()
                    .is_empty()
            );
            assert!(
                bundled_internal_releases_for_arch(if std::env::consts::ARCH == "aarch64" {
                    "x86_64"
                } else {
                    "aarch64"
                })
                .is_ok()
            );
        }

        phase hardware_profiles_are_explicit_data_driven_and_provenance_bound;
        {
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
                assert!(board.label.contains("lenovo-83ed"));
                assert_eq!(
                    board.expected_sha256.as_deref(),
                    Some("c".repeat(64).as_str())
                );
                assert!(!releases[0].matches_source(&board));
                assert!(releases[1].matches_source(&board));
            }

            {
                let legacy =
                    releases_for_host(&catalog_with_both_architectures(), Ok(Some("aarch64"))).unwrap();
                assert_eq!(legacy[0].hardware_profile, "generic");
                let mut catalog = catalog_with_hardware_variant();
                catalog["schema"] = 1.into();
                assert!(releases_for_host(&catalog.to_string(), Ok(Some("aarch64"))).is_err());
            }

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
    }

    fn catalog_with_both_architectures() -> String {
        let releases = ["x86_64", "aarch64"].map(|arch| {
            serde_json::json!({
                "name": format!("Tilo {arch}"),
                "version": "1.0.0",
                "channel": "stable",
                "arch": arch,
                "hardware_profile": "generic",
                "url": format!("https://code.tilo.so/tilo/releases/releases/download/imager-v1.0.0/tilo-{arch}.img.zst"),
                "archive_sha256": "a".repeat(64),
                "archive_size": 32,
                "image_sha256": "b".repeat(64),
                "image_size": 64,
            })
        });
        serde_json::json!({
            "schema": 2,
            "tag": "imager-v1.0.0",
            "releases": releases,
        })
        .to_string()
    }

    fn catalog_with_hardware_variant() -> serde_json::Value {
        let mut catalog: serde_json::Value =
            serde_json::from_str(&catalog_with_both_architectures()).unwrap();
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

    tilo_test_env::scenario! {
        fn a_release_source_keeps_strict_digests_and_identity;

        phase release_sources_keep_strict_digests_and_identity;
        {
            {
                assert!(validate_digest(&"a".repeat(64)).is_ok());
                assert!(validate_digest(&"z".repeat(64)).is_err());
                assert!(validate_digest("abc").is_err());
            }

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
        }
    }
}

#[cfg(test)]
mod internal_catalog_tests {
    use super::*;
    use serde_json::{Value, json};
    fn media() -> Value {
        json!({"name":"Tilo","version":"test","channel":"test","arch":"x86_64",
            "hardware_profile":"generic",
            "url":"https://code.tilo.so/test.img.zst","archive_sha256":"a".repeat(64),
            "archive_size":42,"image_sha256":"b".repeat(64),"image_size":84})
    }
    fn catalog(entry: Value) -> String {
        json!({"schema":2,"tag":"imager-vtest","releases":[entry]}).to_string()
    }
    tilo_test_env::scenario! {
        fn the_internal_and_media_catalogs_parse_strictly;

        phase internal_catalog_entries_parse_strictly_and_separately_from_media;
        {
            {
                assert!(parse_catalog(&catalog(media())).is_ok());
            }

            {
                let mut value = media();
                value["purpose"] = json!("tilo-internal-installer");
                value["layout"] = json!("internal-bundle-v2");
                assert!(parse_catalog(&catalog(value)).is_err());
            }

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

            {
                let mut entry = internal(b"{}");
                entry["release_id"] = json!("loop-test");
                entry["manifest_url"] = json!("https://code.tilo.so/tilo/loop-test/manifest.json");
                let text =
                    json!({"schema":2,"tag":"imager-v0.0.0-loop-test","releases":[entry]}).to_string();
                let parsed = parse_catalog(&text).unwrap();
                assert!(matches!(
                    parsed.releases.as_slice(),
                    [CatalogRelease::Internal(m)] if m.release_id == "loop-test"
                ));
            }

            {
                let mut value = crate::setup::internal_bundle::tests::fixture();
                value["release_id"] = json!("imager-v1.2.3");
                let bytes = crate::setup::internal_bundle::tests::bytes(&value);
                let mut entry = internal(&bytes);
                entry["release_id"] = json!("imager-v1.2.3");
                entry["manifest_url"] = json!(
                    "https://code.tilo.so/api/packages/tilo/generic/tilo-internal-installer/imager-v1.2.3/tilo-internal-installer-x86_64.json"
                );
                let raw =
                    json!({"schema":2,"tag":"imager-v1.2.3","releases":[media(), entry]}).to_string();
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

        phase internal_entries_bind_manifest_bytes_release_and_architecture;
        {
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
                    ("layout", json!("internal-bundle-v1")),
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

        phase catalog_representation_requires_objects_string_tags_and_unique_keys;
        {
            let bytes = crate::setup::internal_bundle::tests::bytes(
                &crate::setup::internal_bundle::tests::fixture(),
            );

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
                    "purpose layout name version channel arch hardware_profile url archive_sha256 archive_size image_sha256 image_size",
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
                let array = positional_record(&entry, fields);
                assert!(parse_catalog(&catalog(array)).is_err(), "case {case}");
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

            for entry in [media(), internal(&bytes)] {
                let raw = catalog(entry);
                assert!(parse_catalog(&raw).is_ok());
                for (marker, replacement) in [
                    ("\"schema\":2", "\"schema\":2,\"schema\":2"),
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

        phase zstd_frames_decode_back_to_back_frames_in_small_reads;
        {
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
    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
}
