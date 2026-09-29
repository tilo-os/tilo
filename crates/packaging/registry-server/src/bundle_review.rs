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

/// Largest web runtime (toolkit core plus its fonts and assets) review will
/// download. Web apps still obey the application limit once decoded.
pub const MAX_WEB_RUNTIME_TAPP_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct InspectionPolicy {
    pub max_package_bytes: u64,
    /// Download ceiling for web versions, which may be the shared toolkit
    /// core rather than an app.
    pub max_web_runtime_bytes: u64,
    pub timeout: Duration,
    /// Development-only escape hatch for an explicitly local registry.
    pub allow_insecure_local: bool,
}

impl Default for InspectionPolicy {
    fn default() -> Self {
        Self {
            max_package_bytes: app_registry::MAX_APP_TAPP_BYTES,
            max_web_runtime_bytes: MAX_WEB_RUNTIME_TAPP_BYTES,
            timeout: Duration::from_secs(30),
            allow_insecure_local: false,
        }
    }
}

impl InspectionPolicy {
    fn download_limit(&self, version: &RegistryVersion) -> u64 {
        if version.target.is_web() {
            self.max_package_bytes.max(self.max_web_runtime_bytes)
        } else if registry_types::DESKTOP_PLATFORMS.contains(&version.target.platform.as_str()) {
            self.max_package_bytes
                .max(tilo_app_package::platform_size_limit(
                    &version.target.platform,
                ))
        } else {
            self.max_package_bytes
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
    #[error("package {version} final-byte BLAKE3 mismatch")]
    ContentHashMismatch { version: String },
    #[error("package {version} has no supported signed execution target")]
    UnsupportedTarget { version: String },
    #[error("package {version} target does not match its signed manifest")]
    TargetMismatch { version: String },
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
    #[error("reading uploaded package {version}: {reason}")]
    Storage { version: String, reason: String },
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
    Ok(fetch_entry_bytes(entry, policy)?
        .into_iter()
        .map(|(inspection, _bytes)| inspection)
        .collect())
}

/// Fetch every version and retain the exact verified bytes for a later
/// networkless signing step.
///
/// # Errors
/// Returns the same URL, fetch, size, identity, and package verification
/// failures as [`inspect_entry`].
pub fn fetch_entry_bytes(
    entry: &RegistryEntry,
    policy: InspectionPolicy,
) -> Result<Vec<(InspectedVersion, Vec<u8>)>, BundleReviewError> {
    if entry.versions.is_empty() {
        return Err(BundleReviewError::NoVersions);
    }
    entry
        .versions
        .iter()
        .map(|version| fetch_version_bytes(entry, version, policy))
        .collect()
}

/// [`fetch_entry_bytes`], reading each version a publisher uploaded to this
/// registry from `storage` (see [`review_version_bytes`]).
///
/// # Errors
/// As [`fetch_entry_bytes`], or a storage error reading an upload.
pub fn review_entry_bytes(
    storage: &crate::Storage,
    entry: &RegistryEntry,
    policy: InspectionPolicy,
) -> Result<Vec<(InspectedVersion, Vec<u8>)>, BundleReviewError> {
    if entry.versions.is_empty() {
        return Err(BundleReviewError::NoVersions);
    }
    entry
        .versions
        .iter()
        .map(|version| review_version_bytes(storage, entry, version, policy))
        .collect()
}

/// [`fetch_version_bytes`], except that a version whose URL is this
/// registry's `/artifacts/<blake3>.tapp` and whose bytes a publisher uploaded
/// (`PUT /uploads`) is read from `storage` instead: approval is what makes
/// that URL serve them. The bytes pass the same inspection either way.
///
/// # Errors
/// As [`fetch_version_bytes`], or a storage error reading the upload.
pub fn review_version_bytes(
    storage: &crate::Storage,
    entry: &RegistryEntry,
    version: &RegistryVersion,
    policy: InspectionPolicy,
) -> Result<(InspectedVersion, Vec<u8>), BundleReviewError> {
    let digest = version.tapp_blake3_hex.to_ascii_lowercase();
    if !version
        .tapp_url
        .ends_with(&format!("/artifacts/{digest}.tapp"))
    {
        return fetch_version_bytes(entry, version, policy);
    }
    let uploaded = storage
        .load_upload(&digest)
        .map_err(|error| BundleReviewError::Storage {
            version: version.version.clone(),
            reason: error.to_string(),
        })?;
    let Some(bytes) = uploaded else {
        return fetch_version_bytes(entry, version, policy);
    };
    check_size(version, bytes.len() as u64, policy.download_limit(version))?;
    let inspection = inspect_bytes(entry, version, &bytes)?;
    Ok((inspection, bytes))
}

/// Fetch and verify one signed version while retaining its exact bytes.
///
/// This single-version boundary lets callers durably consume each verified
/// artifact before fetching the next version instead of retaining an entry's
/// complete version history in memory.
///
/// # Errors
///
/// Returns the same URL, fetch, size, identity, and package verification
/// failures as [`fetch_entry_bytes`].
pub fn fetch_version_bytes(
    entry: &RegistryEntry,
    version: &RegistryVersion,
    policy: InspectionPolicy,
) -> Result<(InspectedVersion, Vec<u8>), BundleReviewError> {
    let limit = policy.download_limit(version);
    if version.tapp_size_bytes > limit {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: version.tapp_size_bytes,
            limit,
        });
    }
    let url = reqwest::Url::parse(&version.tapp_url).map_err(|error| BundleReviewError::Url {
        url: version.tapp_url.clone(),
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
            url: version.tapp_url.clone(),
            reason: "expected HTTPS without credentials or a fragment".into(),
        });
    }
    let host = url.host_str().ok_or_else(|| BundleReviewError::Url {
        url: version.tapp_url.clone(),
        reason: "host is missing".into(),
    })?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| BundleReviewError::Url {
            url: version.tapp_url.clone(),
            reason: "port is missing".into(),
        })?;
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| BundleReviewError::Resolve {
            url: version.tapp_url.clone(),
            reason: error.to_string(),
        })?
        .collect();
    let Some(address) = addresses.first().copied() else {
        return Err(BundleReviewError::PrivateAddress(version.tapp_url.clone()));
    };
    if !policy.allow_insecure_local && addresses.iter().any(|addr| !public_ip(addr.ip())) {
        return Err(BundleReviewError::PrivateAddress(version.tapp_url.clone()));
    }
    // Pin the already-checked address to prevent DNS rebinding.
    let client = tilo_net::http::blocking_client()
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
            url: version.tapp_url.clone(),
            reason: "redirects are not allowed during moderation review".into(),
        });
    }
    response = response
        .error_for_status()
        .map_err(|error| fetch_error(version, &error))?;
    if let Some(length) = response.content_length() {
        check_size(version, length, limit)?;
    }
    let read_limit = limit.min(version.tapp_size_bytes).saturating_add(1);
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| BundleReviewError::Fetch {
            url: version.tapp_url.clone(),
            reason: error.to_string(),
        })?;
    check_size(version, bytes.len() as u64, limit)?;
    let inspection = inspect_bytes(entry, version, &bytes)?;
    Ok((inspection, bytes))
}

fn fetch_error(version: &RegistryVersion, error: &reqwest::Error) -> BundleReviewError {
    let mut reason = format!(
        "{error} [connect={}, timeout={}, request={}, body={}]",
        error.is_connect(),
        error.is_timeout(),
        error.is_request(),
        error.is_body(),
    );
    let mut source = std::error::Error::source(error);
    for _ in 0..4 {
        let Some(cause) = source else { break };
        reason.push_str(": ");
        reason.push_str(&cause.to_string());
        source = cause.source();
    }
    BundleReviewError::Fetch {
        url: version.tapp_url.clone(),
        reason,
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
    if actual != version.tapp_size_bytes {
        return Err(BundleReviewError::SizeMismatch {
            version: version.version.clone(),
            expected: version.tapp_size_bytes,
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
    let actual_hash = blake3::hash(bytes).to_hex().to_string();
    if !version.tapp_blake3_hex.eq_ignore_ascii_case(&actual_hash) {
        return Err(BundleReviewError::ContentHashMismatch {
            version: version.version.clone(),
        });
    }
    let publisher =
        parse_32(&entry.publisher_pubkey_hex).map_err(|reason| BundleReviewError::Publisher {
            version: version.version.clone(),
            reason,
        })?;
    let expected = format!("tapp:{}", version.tapp_root_hash_hex)
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
    let meta = app_registry::tapp::load(bytes).map_err(|error| BundleReviewError::Verify {
        version: version.version.clone(),
        reason: error.to_string(),
    })?;
    app_registry::validate_payload_shape(&meta).map_err(|error| BundleReviewError::Verify {
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
    match version.target.platform.as_str() {
        registry_types::TILO_OS_PLATFORM => check_native_target(version, &meta.manifest)?,
        registry_types::WEB_PLATFORM => check_web_target(version, &meta.manifest, bytes.len())?,
        registry_types::LINUX_PLATFORM
        | registry_types::WINDOWS_PLATFORM
        | registry_types::MACOS_PLATFORM
        | registry_types::ANDROID_PLATFORM => {
            check_desktop_target(version, &meta.manifest, bytes.len())?;
        }
        _ => {
            return Err(BundleReviewError::UnsupportedTarget {
                version: version.version.clone(),
            });
        }
    }
    Ok(InspectedVersion {
        version: version.version.clone(),
        package_id: meta.package_id.to_string(),
        size_bytes: bytes.len() as u64,
        sandbox: serde_json::to_value(meta.manifest.sandbox).unwrap_or(serde_json::Value::Null),
    })
}

/// The signed manifest of a web build records exactly the target the version
/// binds; an app build additionally obeys the application size limit.
fn check_web_target(
    version: &RegistryVersion,
    manifest: &tilo_app_package::Manifest,
    size: usize,
) -> Result<(), BundleReviewError> {
    check_signed_target(version, manifest)?;
    if !tilo_app_package::tapp_size_allowed(manifest.package.kind, size as u64) {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: size as u64,
            limit: tilo_app_package::MAX_APP_TAPP_BYTES,
        });
    }
    Ok(())
}

/// A Linux, Windows, macOS or Android build is a self-contained executable:
/// its signed `[target]` names the version exactly, under the desktop size
/// limit.
fn check_desktop_target(
    version: &RegistryVersion,
    manifest: &tilo_app_package::Manifest,
    size: usize,
) -> Result<(), BundleReviewError> {
    check_signed_target(version, manifest)?;
    let limit = tilo_app_package::platform_size_limit(&version.target.platform);
    if size as u64 > limit {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: size as u64,
            limit,
        });
    }
    Ok(())
}

/// The manifest's signed `[target]` records exactly the target the version binds.
fn check_signed_target(
    version: &RegistryVersion,
    manifest: &tilo_app_package::Manifest,
) -> Result<(), BundleReviewError> {
    let target = &version.target;
    if target.validate().is_err() {
        return Err(BundleReviewError::UnsupportedTarget {
            version: version.version.clone(),
        });
    }
    let matches = manifest.target.as_ref().is_some_and(|signed| {
        signed.platform == target.platform
            && signed.architecture == target.architecture
            && signed.isa == target.isa
            && target.runtime.as_deref() == Some(signed.runtime.as_str())
    });
    if !matches {
        return Err(BundleReviewError::TargetMismatch {
            version: version.version.clone(),
        });
    }
    Ok(())
}

fn check_native_target(
    version: &RegistryVersion,
    manifest: &tilo_app_package::Manifest,
) -> Result<(), BundleReviewError> {
    let target = &version.target;
    if manifest.target.is_some() {
        return Err(BundleReviewError::TargetMismatch {
            version: version.version.clone(),
        });
    }
    let architecture = match target.architecture.as_str() {
        "x86_64" => tilo_app_package::manifest::Arch::X86_64,
        "aarch64" => tilo_app_package::manifest::Arch::AArch64,
        _ => {
            return Err(BundleReviewError::UnsupportedTarget {
                version: version.version.clone(),
            });
        }
    };
    let manifest_isa = manifest
        .package
        .isa
        .for_arch(architecture)
        .map(|isa| isa.wire_label().to_string());
    if manifest_isa.as_deref() != Some(target.isa.as_str())
        || manifest.package.runtime != target.runtime
    {
        return Err(BundleReviewError::TargetMismatch {
            version: version.version.clone(),
        });
    }
    Ok(())
}

fn parse_32(value: &str) -> Result<[u8; 32], String> {
    let bytes = hex::decode(value).map_err(|error| error.to_string())?;
    bytes
        .try_into()
        .map_err(|bytes: Vec<u8>| format!("expected 32 bytes, got {}", bytes.len()))
}

pub(crate) fn public_ip(ip: IpAddr) -> bool {
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
    use tilo_app_package::Manifest;
    use tilo_app_package::pack::{SigningIdentity, pack};

    #[test]
    fn review_fetch_policy_refuses_before_network_io() {
        // phase: default_review_policy_enforces_the_app_limit_before_download
        {
            let policy = InspectionPolicy::default();
            assert_eq!(policy.max_package_bytes, app_registry::MAX_APP_TAPP_BYTES);

            let mut entry = sample_entry("https://example.com/app.tapp");
            entry.versions[0].tapp_size_bytes = app_registry::MAX_APP_TAPP_BYTES + 1;
            assert!(matches!(
                inspect_entry(&entry, policy),
                Err(BundleReviewError::TooLarge { limit, .. })
                    if limit == app_registry::MAX_APP_TAPP_BYTES
            ));
        }
        // phase: review_fetch_refuses_local_and_credentialed_urls_before_network_io
        {
            let mut entry = sample_entry("http://127.0.0.1/app.tapp");
            assert!(matches!(
                inspect_entry(&entry, InspectionPolicy::default()),
                Err(BundleReviewError::Url { .. })
            ));
            entry.versions[0].tapp_url = "https://user@example.com/app.tapp".into();
            assert!(matches!(
                inspect_entry(&entry, InspectionPolicy::default()),
                Err(BundleReviewError::Url { .. })
            ));
            entry.versions[0].tapp_url = "https://127.0.0.1/app.tapp".into();
            assert!(matches!(
                inspect_entry(&entry, InspectionPolicy::default()),
                Err(BundleReviewError::PrivateAddress(_))
            ));
        }
        // phase: public_address_classification_covers_special_ranges
        {
            assert!(!public_ip("127.0.0.1".parse().unwrap()));
            assert!(!public_ip("169.254.1.2".parse().unwrap()));
            assert!(!public_ip("100.64.0.1".parse().unwrap()));
            assert!(!public_ip("2001:db8::1".parse().unwrap()));
            assert!(public_ip("1.1.1.1".parse().unwrap()));
            assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        }
    }

    #[test]
    fn artifact_inspection_binds_identity_manifest_size_and_isa() {
        // phase: package_inspection_verifies_identity_manifest_and_size
        {
            let identity = SigningIdentity::generate();
            let (bytes, package_id) = signed_tapp(&identity);
            let mut entry = sample_entry("https://example.com/app.tapp");
            entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
            entry.versions[0].tapp_root_hash_hex = hex::encode(package_id.as_bytes());
            entry.versions[0].tapp_size_bytes = bytes.len() as u64;
            entry.versions[0].tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();

            let inspected = inspect_bytes(&entry, &entry.versions[0], &bytes).unwrap();
            assert_eq!(inspected.version, "1.0.0");
            assert_eq!(inspected.package_id, package_id.to_string());
            assert_eq!(inspected.sandbox["landlock"], "strict");

            let mut wrong_size = entry.versions[0].clone();
            wrong_size.tapp_size_bytes += 1;
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
            wrong_manifest.package_id = "so.tilo.other".into();
            assert!(matches!(
                inspect_bytes(&wrong_manifest, &wrong_manifest.versions[0], &bytes),
                Err(BundleReviewError::ManifestMismatch { .. })
            ));
        }
        // phase: arm_artifact_inspection_binds_architecture_and_isa
        {
            let identity = SigningIdentity::generate();
            let (bytes, package_id) = signed_tapp_arch(&identity, "aarch64 = \"v8.2-a\"");
            let mut entry = sample_entry("https://example.com/arm.tapp");
            entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
            let version = &mut entry.versions[0];
            version.tapp_root_hash_hex = hex::encode(package_id.as_bytes());
            version.tapp_size_bytes = bytes.len() as u64;
            version.tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();
            let target = &mut version.target;
            target.architecture = "aarch64".into();
            target.isa = "aarch64-v8.2".into();
            assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());
            entry.versions[0].target.isa = "aarch64-v9".into();
            assert!(matches!(
                inspect_bytes(&entry, &entry.versions[0], &bytes),
                Err(BundleReviewError::TargetMismatch { .. })
            ));
        }
    }

    #[test]
    fn web_artifact_inspection_binds_the_signed_web_target() {
        let identity = SigningIdentity::generate();
        let (native, _) = signed_tapp(&identity);
        let authored = app_registry::tapp::load(&native).unwrap().manifest;
        let manifest = tilo_app_package::web::web_manifest(
            &authored,
            "editor.wasm",
            &tilo_app_package::web::WasmModuleInfo {
                runtime_abi: Some(
                    "tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff".into(),
                ),
                features: Some(vec!["sign-ext".into()]),
            },
            &tilo_app_package::web::WebBuildOptions::default(),
        );
        // The fixture declares no runtime; a web app needs one.
        assert!(manifest.is_err());
        let mut authored = authored;
        authored.package.runtime = Some("tilo-runtime/2026.07".into());
        let manifest = tilo_app_package::web::web_manifest(
            &authored,
            "editor.wasm",
            &tilo_app_package::web::WasmModuleInfo {
                runtime_abi: Some(
                    "tilo-runtime/2026.07+wasm.00112233445566778899aabbccddeeff".into(),
                ),
                features: Some(vec!["sign-ext".into()]),
            },
            &tilo_app_package::web::WebBuildOptions::default(),
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("editor.wasm"), b"\0asm\x01\0\0\0").unwrap();
        let packed = pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
        let signed = manifest.target.unwrap();

        let mut entry = sample_entry("https://example.com/editor-web.tapp");
        entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
        let version = &mut entry.versions[0];
        version.tapp_root_hash_hex = hex::encode(packed.id().as_bytes());
        version.tapp_size_bytes = bytes.len() as u64;
        version.tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();
        version.target = registry_types::RegistryTarget {
            platform: signed.platform,
            architecture: signed.architecture,
            isa: signed.isa,
            runtime: Some(signed.runtime),
        };
        assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());

        let mut other_abi = entry.versions[0].clone();
        other_abi.target.runtime =
            Some("tilo-runtime/2026.07+wasm.ffeeddccbbaa99887766554433221100".into());
        assert!(matches!(
            inspect_bytes(&entry, &other_abi, &bytes),
            Err(BundleReviewError::TargetMismatch { .. })
        ));
        let mut as_native = entry.versions[0].clone();
        as_native.target = registry_types::RegistryTarget {
            platform: registry_types::TILO_OS_PLATFORM.into(),
            architecture: "x86_64".into(),
            isa: "x86_64-v3".into(),
            runtime: Some("tilo-runtime/2026.07".into()),
        };
        assert!(matches!(
            inspect_bytes(&entry, &as_native, &bytes),
            Err(BundleReviewError::TargetMismatch { .. })
        ));

        let policy = InspectionPolicy::default();
        let mut runtime_sized = entry.versions[0].clone();
        runtime_sized.tapp_size_bytes = app_registry::MAX_APP_TAPP_BYTES + 1;
        assert_eq!(
            policy.download_limit(&runtime_sized),
            MAX_WEB_RUNTIME_TAPP_BYTES
        );
        assert_eq!(
            policy.download_limit(&sample_entry("https://example.com/a.tapp").versions[0]),
            app_registry::MAX_APP_TAPP_BYTES
        );
    }

    #[test]
    fn desktop_artifact_inspection_binds_the_signed_desktop_target() {
        let identity = SigningIdentity::generate();
        let (native, _) = signed_tapp(&identity);
        let mut authored = app_registry::tapp::load(&native).unwrap().manifest;
        authored.package.runtime = Some("tilo-runtime/2026.07".into());
        let manifest = tilo_app_package::desktop::desktop_manifest(
            &authored,
            registry_types::MACOS_PLATFORM,
            "aarch64",
            "aarch64",
            "bin/editor",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/editor"), b"\xcf\xfa\xed\xfe").unwrap();
        let packed = pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();

        let mut entry = sample_entry("https://example.com/editor-macos.tapp");
        entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
        let version = &mut entry.versions[0];
        version.tapp_root_hash_hex = hex::encode(packed.id().as_bytes());
        version.tapp_size_bytes = bytes.len() as u64;
        version.tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();
        version.target = registry_types::RegistryTarget {
            platform: registry_types::MACOS_PLATFORM.into(),
            architecture: "aarch64".into(),
            isa: "aarch64".into(),
            runtime: Some("tilo-runtime/2026.07".into()),
        };
        assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());

        for other in [
            registry_types::WINDOWS_PLATFORM,
            registry_types::LINUX_PLATFORM,
            registry_types::ANDROID_PLATFORM,
        ] {
            let mut relabelled = entry.versions[0].clone();
            relabelled.target.platform = other.into();
            assert!(matches!(
                inspect_bytes(&entry, &relabelled, &bytes),
                Err(BundleReviewError::TargetMismatch { .. })
            ));
        }
        let mut as_intel = entry.versions[0].clone();
        as_intel.target.architecture = "x86_64".into();
        as_intel.target.isa = "x86_64".into();
        assert!(matches!(
            inspect_bytes(&entry, &as_intel, &bytes),
            Err(BundleReviewError::TargetMismatch { .. })
        ));
        let mut as_tilo_os = entry.versions[0].clone();
        as_tilo_os.target.platform = registry_types::TILO_OS_PLATFORM.into();
        as_tilo_os.target.isa = "aarch64-v8.2".into();
        assert!(inspect_bytes(&entry, &as_tilo_os, &bytes).is_err());

        let policy = InspectionPolicy::default();
        assert_eq!(
            policy.download_limit(&entry.versions[0]),
            tilo_app_package::MAX_DESKTOP_APP_TAPP_BYTES.max(policy.max_package_bytes)
        );
    }

    #[test]
    fn android_artifact_inspection_binds_the_signed_android_target() {
        let identity = SigningIdentity::generate();
        let (native, _) = signed_tapp(&identity);
        let mut authored = app_registry::tapp::load(&native).unwrap().manifest;
        authored.package.runtime = Some("tilo-runtime/2026.07".into());
        let manifest = tilo_app_package::desktop::desktop_manifest(
            &authored,
            registry_types::ANDROID_PLATFORM,
            "aarch64",
            "aarch64",
            "editor.apk",
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("editor.apk"), b"PK\x03\x04").unwrap();
        let packed = pack(dir.path(), &manifest, &identity).unwrap();
        let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();

        let mut entry = sample_entry("https://example.com/editor-android.tapp");
        entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
        let version = &mut entry.versions[0];
        version.tapp_root_hash_hex = hex::encode(packed.id().as_bytes());
        version.tapp_size_bytes = bytes.len() as u64;
        version.tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();
        version.target = registry_types::RegistryTarget {
            platform: registry_types::ANDROID_PLATFORM.into(),
            architecture: "aarch64".into(),
            isa: "aarch64".into(),
            runtime: Some("tilo-runtime/2026.07".into()),
        };
        assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());

        for other in [
            registry_types::LINUX_PLATFORM,
            registry_types::MACOS_PLATFORM,
        ] {
            let mut relabelled = entry.versions[0].clone();
            relabelled.target.platform = other.into();
            assert!(matches!(
                inspect_bytes(&entry, &relabelled, &bytes),
                Err(BundleReviewError::TargetMismatch { .. })
            ));
        }
        let mut as_x86 = entry.versions[0].clone();
        as_x86.target.architecture = "x86_64".into();
        as_x86.target.isa = "x86_64".into();
        assert!(matches!(
            inspect_bytes(&entry, &as_x86, &bytes),
            Err(BundleReviewError::TargetMismatch { .. })
        ));
        let mut as_tilo_os = entry.versions[0].clone();
        as_tilo_os.target.platform = registry_types::TILO_OS_PLATFORM.into();
        as_tilo_os.target.isa = "aarch64-v8.2".into();
        assert!(inspect_bytes(&entry, &as_tilo_os, &bytes).is_err());

        let policy = InspectionPolicy::default();
        assert_eq!(
            policy.download_limit(&entry.versions[0]),
            tilo_app_package::MAX_DESKTOP_APP_TAPP_BYTES.max(policy.max_package_bytes)
        );
    }

    fn signed_tapp(identity: &SigningIdentity) -> (Vec<u8>, tilo_app_package::PackageId) {
        signed_tapp_arch(identity, "x86_64 = \"v3\"")
    }

    fn signed_tapp_arch(
        identity: &SigningIdentity,
        architecture: &str,
    ) -> (Vec<u8>, tilo_app_package::PackageId) {
        let publisher = hex::encode(identity.public_key_bytes());
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(payload.join("bin")).unwrap();
        std::fs::write(payload.join("bin/editor"), b"#!/bin/true\n").unwrap();
        let manifest = Manifest::from_toml_str(&format!(
            r#"
[package]
id = "so.tilo.editor"
version = "1.0.0"
kind = "app"

[package.isa]
{architecture}

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "bin/editor"
type = "gui"
"#
        ))
        .unwrap();
        let packed = pack(&payload, &manifest, identity).unwrap();
        (
            tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap(),
            packed.id(),
        )
    }

    fn sample_entry(url: &str) -> RegistryEntry {
        RegistryEntry {
            package_id: "so.tilo.editor".into(),
            name: "Editor".into(),
            description: "Text editor".into(),
            tagline: "Edit together".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![RegistryVersion {
                version: "1.0.0".into(),
                tapp_url: url.into(),
                tapp_root_hash_hex: "bb".repeat(32),
                tapp_size_bytes: 10,
                released_at_unix: 1,
                tapp_blake3_hex: "cc".repeat(32),
                target: registry_types::RegistryTarget {
                    platform: registry_types::TILO_OS_PLATFORM.into(),
                    architecture: "x86_64".into(),
                    isa: "x86_64-v3".into(),
                    runtime: None,
                },
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        }
    }
}
