//! Portable untrusted catalog metadata. Parsing grants no selection authority.
use serde::Deserialize;

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalog {
    pub schema: u32,
    pub tag: String,
    pub releases: Vec<CatalogRelease>,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum CatalogRelease {
    Media(MediaRelease),
    Internal(InternalMetadata),
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub enum MediaPurpose {
    #[serde(rename = "removable-media")]
    RemovableMedia,
}
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub enum MediaLayout {
    #[serde(rename = "whole-disk-image")]
    WholeDiskImage,
}
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub enum InternalPurpose {
    #[serde(rename = "qos-internal-installer")]
    InternalInstaller,
}
#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub enum InternalLayout {
    #[serde(rename = "internal-bundle-v1")]
    BundleV1,
}

fn present_tag<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaRelease {
    #[serde(default, deserialize_with = "present_tag")]
    pub purpose: Option<MediaPurpose>,
    #[serde(default, deserialize_with = "present_tag")]
    pub layout: Option<MediaLayout>,
    pub name: String,
    pub version: String,
    pub channel: String,
    pub arch: String,
    #[serde(default)]
    pub hardware_profile: Option<String>,
    #[serde(default)]
    pub provenance: Option<MediaProvenance>,
    pub url: String,
    pub archive_sha256: String,
    pub archive_size: u64,
    pub image_sha256: String,
    pub image_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InternalMetadata {
    pub purpose: InternalPurpose,
    pub layout: InternalLayout,
    pub release_id: String,
    pub arch: String,
    pub manifest_url: String,
    pub manifest_sha256: String,
    pub manifest_size: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Deserialize)]
pub struct MediaProvenance {
    pub source_image_id: String,
    pub input_qcow2_sha256: String,
    pub hardware_profile_sha256: Option<String>,
    pub target_update_ref: Option<String>,
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
        let valid_origin =
            if let Some(digest) = origin.strip_prefix("code.podesta.ai/qos/tilo@sha256:") {
                validate_digest(digest).is_ok()
            } else if let Some(tag) = origin.strip_prefix("code.podesta.ai/qos/tilo:") {
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

fn trusted_catalog_url(value: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Invalid catalog URL.".to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some("code.podesta.ai")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("The release catalog contains an untrusted download origin.".into());
    }
    Ok(())
}

pub fn parse_catalog(raw: &str) -> Result<Catalog, String> {
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
                validate_digest(&media.archive_sha256)?;
                validate_digest(&media.image_sha256)?;
                if media.archive_size == 0 || media.image_size == 0 {
                    return Err("Invalid media size.".into());
                }
                trusted_catalog_url(&media.url)?;
                &media.arch
            }
            CatalogRelease::Internal(internal) => {
                use crate::bundle::{MAX_MANIFEST_BYTES, canonical_digest, identifier};
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
