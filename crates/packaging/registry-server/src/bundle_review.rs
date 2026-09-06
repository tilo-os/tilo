//! Fail-closed inspection of package bytes behind a pending registry entry.
//!
//! A publisher signature authenticates metadata, but it does not prove that a
//! moderator reviewed the bytes at the advertised URL. The reviewer CLI uses
//! this module before approval. Redirects and non-public destinations are
//! refused so inspection cannot become an SSRF primitive.

use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use registry_types::{RegistryEntry, RegistryVersion};
use serde::Serialize;
use thiserror::Error;

#[derive(Clone, Copy, Debug)]
pub struct InspectionPolicy {
    pub max_package_bytes: u64,
    pub timeout: Duration,
    /// Development-only escape hatch for an explicitly local registry.
    pub allow_insecure_local: bool,
}

impl Default for InspectionPolicy {
    fn default() -> Self {
        Self {
            max_package_bytes: 512 * 1024 * 1024,
            timeout: Duration::from_secs(30),
            allow_insecure_local: false,
        }
    }
}

/// Security-relevant facts read from the verified, publisher-signed manifest.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InspectedVersion {
    pub version: String,
    pub package_id: String,
    pub size_bytes: u64,
    pub sandbox: serde_json::Value,
}

#[derive(Debug, Error)]
pub enum BundleReviewError {
    #[error("registry entry has no package versions to review")]
    NoVersions,
    #[error("invalid package URL {url:?}: {reason}")]
    Url { url: String, reason: String },
    #[error("package URL {0:?} resolves to a non-public address")]
    PrivateAddress(String),
    #[error("could not resolve package URL {url:?}: {reason}")]
    Resolve { url: String, reason: String },
    #[error("could not fetch package {url:?}: {reason}")]
    Fetch { url: String, reason: String },
    #[error("package {version} is too large: {size} bytes (limit {limit})")]
    TooLarge {
        version: String,
        size: u64,
        limit: u64,
    },
    #[error("package {version} size mismatch: registry says {expected}, response has {actual}")]
    SizeMismatch {
        version: String,
        expected: u64,
        actual: u64,
    },
    #[error("package {version} has an invalid publisher key: {reason}")]
    Publisher { version: String, reason: String },
    #[error("package {version} has an invalid root id: {reason}")]
    PackageId { version: String, reason: String },
    #[error("package {version} failed cryptographic verification: {reason}")]
    Verify { version: String, reason: String },
    #[error(
        "package {version} manifest names {actual_id}@{actual_version}, expected {expected_id}@{expected_version}"
    )]
    ManifestMismatch {
        version: String,
        actual_id: String,
        actual_version: String,
        expected_id: String,
        expected_version: String,
    },
}

/// Fetch and inspect every version. The caller approves the unchanged signed
/// submission only after this succeeds.
///
/// # Errors
///
/// Returns an error when an entry has no versions, a URL is unsafe or cannot
/// be fetched within policy, or the downloaded bytes fail verification.
pub fn inspect_entry(
    entry: &RegistryEntry,
    policy: InspectionPolicy,
) -> Result<Vec<InspectedVersion>, BundleReviewError> {
    if entry.versions.is_empty() {
        return Err(BundleReviewError::NoVersions);
    }
    entry
        .versions
        .iter()
        .map(|version| fetch_and_inspect(entry, version, policy))
        .collect()
}

fn fetch_and_inspect(
    entry: &RegistryEntry,
    version: &RegistryVersion,
    policy: InspectionPolicy,
) -> Result<InspectedVersion, BundleReviewError> {
    if version.qpkg_size_bytes > policy.max_package_bytes {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: version.qpkg_size_bytes,
            limit: policy.max_package_bytes,
        });
    }
    let url = reqwest::Url::parse(&version.qpkg_url).map_err(|error| BundleReviewError::Url {
        url: version.qpkg_url.clone(),
        reason: error.to_string(),
    })?;
    let scheme_allowed =
        url.scheme() == "https" || (policy.allow_insecure_local && url.scheme() == "http");
    if !scheme_allowed
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(BundleReviewError::Url {
            url: version.qpkg_url.clone(),
            reason: "expected HTTPS without credentials or a fragment".into(),
        });
    }
    let host = url.host_str().ok_or_else(|| BundleReviewError::Url {
        url: version.qpkg_url.clone(),
        reason: "host is missing".into(),
    })?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| BundleReviewError::Url {
            url: version.qpkg_url.clone(),
            reason: "port is missing".into(),
        })?;
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| BundleReviewError::Resolve {
            url: version.qpkg_url.clone(),
            reason: error.to_string(),
        })?
        .collect();
    let Some(address) = addresses.first().copied() else {
        return Err(BundleReviewError::PrivateAddress(version.qpkg_url.clone()));
    };
    if !policy.allow_insecure_local && addresses.iter().any(|addr| !public_ip(addr.ip())) {
        return Err(BundleReviewError::PrivateAddress(version.qpkg_url.clone()));
    }
    // Pin the already-checked address to prevent DNS rebinding.
    let client = reqwest::blocking::Client::builder()
        .timeout(policy.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, address)
        .build()
        .map_err(|error| fetch_error(version, &error))?;
    let mut response = client
        .get(url)
        .send()
        .map_err(|error| fetch_error(version, &error))?;
    if response.status().is_redirection() {
        return Err(BundleReviewError::Fetch {
            url: version.qpkg_url.clone(),
            reason: "redirects are not allowed during moderation review".into(),
        });
    }
    response = response
        .error_for_status()
        .map_err(|error| fetch_error(version, &error))?;
    if let Some(length) = response.content_length() {
        check_size(version, length, policy.max_package_bytes)?;
    }
    let read_limit = policy
        .max_package_bytes
        .min(version.qpkg_size_bytes)
        .saturating_add(1);
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| BundleReviewError::Fetch {
            url: version.qpkg_url.clone(),
            reason: error.to_string(),
        })?;
    check_size(version, bytes.len() as u64, policy.max_package_bytes)?;
    inspect_bytes(entry, version, &bytes)
}

fn fetch_error(version: &RegistryVersion, error: &reqwest::Error) -> BundleReviewError {
    BundleReviewError::Fetch {
        url: version.qpkg_url.clone(),
        reason: error.to_string(),
    }
}

fn check_size(version: &RegistryVersion, actual: u64, limit: u64) -> Result<(), BundleReviewError> {
    if actual > limit {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: actual,
            limit,
        });
    }
    if actual != version.qpkg_size_bytes {
        return Err(BundleReviewError::SizeMismatch {
            version: version.version.clone(),
            expected: version.qpkg_size_bytes,
            actual,
        });
    }
    Ok(())
}

/// Inspect already-fetched bytes for offline tooling and deterministic tests.
///
/// # Errors
///
/// Returns an error if the bytes differ from the advertised size, package ID,
/// publisher identity, manifest app ID, or manifest version.
pub fn inspect_bytes(
    entry: &RegistryEntry,
    version: &RegistryVersion,
    bytes: &[u8],
) -> Result<InspectedVersion, BundleReviewError> {
    check_size(version, bytes.len() as u64, u64::MAX)?;
    let publisher =
        parse_32(&entry.publisher_pubkey_hex).map_err(|reason| BundleReviewError::Publisher {
            version: version.version.clone(),
            reason,
        })?;
    let expected = format!("qpkg:{}", version.qpkg_root_hash_hex)
        .parse()
        .map_err(|error| BundleReviewError::PackageId {
            version: version.version.clone(),
            reason: format!("{error:?}"),
        })?;
    app_registry::verify_bytes(bytes, expected, publisher).map_err(|error| {
        BundleReviewError::Verify {
            version: version.version.clone(),
            reason: error.to_string(),
        }
    })?;
    let meta = app_registry::qpkg::load(bytes).map_err(|error| BundleReviewError::Verify {
        version: version.version.clone(),
        reason: error.to_string(),
    })?;
    if meta.manifest.package.id != entry.package_id
        || meta.manifest.package.version != version.version
    {
        return Err(BundleReviewError::ManifestMismatch {
            version: version.version.clone(),
            actual_id: meta.manifest.package.id,
            actual_version: meta.manifest.package.version,
            expected_id: entry.package_id.clone(),
            expected_version: version.version.clone(),
        });
    }
    Ok(InspectedVersion {
        version: version.version.clone(),
        package_id: meta.package_id.to_string(),
        size_bytes: bytes.len() as u64,
        sandbox: serde_json::to_value(meta.manifest.sandbox).unwrap_or(serde_json::Value::Null),
    })
}

fn parse_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(value).map_err(|error| error.to_string())?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected 32 bytes, got {}", bytes.len()))
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || ip.is_multicast()
        || a == 0
        || (a == 100 && (64..=127).contains(&b))
        || (a == 198 && (18..=19).contains(&b)))
}

fn public_v6(ip: Ipv6Addr) -> bool {
    if let Some(mapped) = ip.to_ipv4_mapped() {
        return public_v4(mapped);
    }
    let segments = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        || ip.is_multicast()
        || (segments[0] == 0x2001 && segments[1] == 0x0db8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pkg::pack::{SigningIdentity, pack};
    use pkg::{Manifest, MemoryBlobStore};

    #[test]
    fn review_fetch_refuses_local_and_credentialed_urls_before_network_io() {
        let mut entry = sample_entry("http://127.0.0.1/app.qpkg");
        assert!(matches!(
            inspect_entry(&entry, InspectionPolicy::default()),
            Err(BundleReviewError::Url { .. })
        ));
        entry.versions[0].qpkg_url = "https://user@example.com/app.qpkg".into();
        assert!(matches!(
            inspect_entry(&entry, InspectionPolicy::default()),
            Err(BundleReviewError::Url { .. })
        ));
        entry.versions[0].qpkg_url = "https://127.0.0.1/app.qpkg".into();
        assert!(matches!(
            inspect_entry(&entry, InspectionPolicy::default()),
            Err(BundleReviewError::PrivateAddress(_))
        ));
    }

    #[test]
    fn public_address_classification_covers_special_ranges() {
        assert!(!public_ip("127.0.0.1".parse().unwrap()));
        assert!(!public_ip("169.254.1.2".parse().unwrap()));
        assert!(!public_ip("100.64.0.1".parse().unwrap()));
        assert!(!public_ip("2001:db8::1".parse().unwrap()));
        assert!(public_ip("1.1.1.1".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
    }

    #[test]
    fn package_inspection_verifies_identity_manifest_and_size() {
        let identity = SigningIdentity::generate();
        let (bytes, package_id) = signed_qpkg(&identity);
        let mut entry = sample_entry("https://example.com/app.qpkg");
        entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
        entry.versions[0].qpkg_root_hash_hex = hex::encode(package_id.as_bytes());
        entry.versions[0].qpkg_size_bytes = bytes.len() as u64;

        let inspected = inspect_bytes(&entry, &entry.versions[0], &bytes).unwrap();
        assert_eq!(inspected.version, "1.0.0");
        assert_eq!(inspected.package_id, package_id.to_string());
        assert_eq!(inspected.sandbox["landlock"], "strict");

        let mut wrong_size = entry.versions[0].clone();
        wrong_size.qpkg_size_bytes += 1;
        assert!(matches!(
            inspect_bytes(&entry, &wrong_size, &bytes),
            Err(BundleReviewError::SizeMismatch { .. })
        ));

        let mut wrong_publisher = entry.clone();
        wrong_publisher.publisher_pubkey_hex = "aa".repeat(32);
        assert!(matches!(
            inspect_bytes(&wrong_publisher, &wrong_publisher.versions[0], &bytes),
            Err(BundleReviewError::Verify { .. })
        ));

        let mut wrong_manifest = entry.clone();
        wrong_manifest.package_id = "dev.qos.other".into();
        assert!(matches!(
            inspect_bytes(&wrong_manifest, &wrong_manifest.versions[0], &bytes),
            Err(BundleReviewError::ManifestMismatch { .. })
        ));
    }

    fn signed_qpkg(identity: &SigningIdentity) -> (Vec<u8>, pkg::PackageId) {
        let publisher = hex::encode(identity.public_key_bytes());
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/editor"), b"#!/bin/true\n").unwrap();
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "dev.qos.editor"
version = "1.0.0"
kind = "app"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "qos-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/editor"
type = "gui"
"#
        ))
        .unwrap();
        let mut store = MemoryBlobStore::new();
        let id = pack(&payload, &manifest, identity, &mut store).unwrap();
        (app_registry::qpkg::write_bytes(id, &store).unwrap(), id)
    }

    fn sample_entry(url: &str) -> RegistryEntry {
        RegistryEntry {
            package_id: "dev.qos.editor".into(),
            name: "Editor".into(),
            description: "Text editor".into(),
            categories: vec!["Productivity".into()],
            icon_url: None,
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                qpkg_url: url.into(),
                qpkg_root_hash_hex: "bb".repeat(32),
                qpkg_size_bytes: 10,
                released_at_unix: 1,
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        }
    }
}
