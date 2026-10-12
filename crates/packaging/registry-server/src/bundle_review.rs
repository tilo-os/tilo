//! Fail-closed inspection of package bytes behind a pending registry entry.
//!
//! A publisher signature authenticates metadata, but it does not prove that a
//! moderator reviewed the bytes at the advertised URL. The reviewer CLI uses
//! this module before approval. Redirects and non-public destinations are
//! refused so inspection cannot become an SSRF primitive.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::time::Duration;

use registry_types::{RegistryEntry, RegistryVersion};
use serde::Serialize;
use thiserror::Error;

/// Read size of the streaming copies that move payload objects to storage.
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct InspectionPolicy {
    pub max_package_bytes: u64,
    /// Distinct payload bytes a `kind = "runtime"` package may reference.
    /// Its payload travels as objects beside a thin `.tapp`, each at most
    /// [`tilo_app_package::chunks::MAX_OBJECT_BYTES`].
    pub max_runtime_payload_bytes: u64,
    /// Ceiling of one request: the `.tapp`, or one payload object.
    pub timeout: Duration,
    /// Development-only escape hatch for an explicitly local registry.
    pub allow_insecure_local: bool,
}

impl Default for InspectionPolicy {
    fn default() -> Self {
        Self {
            max_package_bytes: app_registry::MAX_APP_TAPP_BYTES,
            max_runtime_payload_bytes: tilo_app_package::MAX_RUNTIME_PAYLOAD_BYTES,
            timeout: Duration::from_secs(30),
            allow_insecure_local: false,
        }
    }
}

impl InspectionPolicy {
    fn download_limit(&self, version: &RegistryVersion) -> u64 {
        if registry_types::DESKTOP_PLATFORMS.contains(&version.target.platform.as_str()) {
            self.max_package_bytes
                .max(tilo_app_package::platform_size_limit(
                    &version.target.platform,
                ))
        } else {
            self.max_package_bytes
        }
    }

    /// Ceiling on the distinct payload bytes the package may reference:
    /// the application budget for an app, the runtime payload limit for a
    /// runtime, and the download limit for other kinds, which travel full.
    fn payload_limit(
        &self,
        version: &RegistryVersion,
        manifest: &tilo_app_package::Manifest,
    ) -> u64 {
        if manifest.package.kind == tilo_app_package::Kind::Runtime {
            return self.max_runtime_payload_bytes;
        }
        tilo_app_package::app_size_limit(manifest).unwrap_or_else(|| self.download_limit(version))
    }
}

/// Security-relevant facts read from the verified, publisher-signed manifest.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct InspectedVersion {
    pub version: String,
    pub package_id: String,
    pub size_bytes: u64,
    /// Distinct payload bytes, carried or fetched as objects.
    pub payload_bytes: u64,
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
    #[error(
        "package {version} payload blob {blob} is not carried, not held, and not beside the package as {blob}.blob or {blob}.chunks"
    )]
    MissingPayload { version: String, blob: String },
    #[error("package {version} payload object {url:?}: {reason}")]
    PayloadObject {
        version: String,
        url: String,
        reason: String,
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
    let uploaded = if version
        .tapp_url
        .ends_with(&format!("/artifacts/{digest}.tapp"))
    {
        storage
            .load_upload(&digest)
            .map_err(|error| BundleReviewError::Storage {
                version: version.version.clone(),
                reason: error.to_string(),
            })?
    } else {
        None
    };
    let bytes = if let Some(bytes) = uploaded {
        check_size(version, bytes.len() as u64, policy.download_limit(version))?;
        bytes
    } else {
        download_tapp(version, policy)?
    };
    let inspection = inspect_stored_bytes(storage, entry, version, &bytes, Some(policy))?;
    Ok((inspection, bytes))
}

/// [`inspect_bytes`] for a package that may be thin. Each payload blob the
/// `.tapp` does not carry must be held by `storage` already or, given a
/// `fetch` policy, is fetched from the objects the publisher hosts beside
/// `tapp_url` (`<blob>.blob`, or `<blob>.chunks` and its chunks), verified,
/// and streamed straight into `storage`'s payload blobs. The payload shape
/// and size limits are then checked on the verified lengths, so a thin
/// application stays within the application budget.
///
/// # Errors
/// As [`inspect_bytes`], or a payload blob that cannot be found, fetched,
/// verified, stored, or that exceeds the package's payload limit.
pub fn inspect_stored_bytes(
    storage: &crate::Storage,
    entry: &RegistryEntry,
    version: &RegistryVersion,
    bytes: &[u8],
    fetch: Option<InspectionPolicy>,
) -> Result<InspectedVersion, BundleReviewError> {
    let meta = inspect_metadata(entry, version, bytes)?;
    let mut lengths = meta.tapp.carried.clone();
    let missing = meta.tapp.missing();
    let policy = fetch.unwrap_or_default();
    let limit = policy.payload_limit(version, &meta.manifest);
    let storage_error = |error: crate::StorageError| BundleReviewError::Storage {
        version: version.version.clone(),
        reason: error.to_string(),
    };
    if !missing.is_empty() && fetch.is_some() {
        // The thin form lands first: pruning keeps the blobs of every thin
        // package a pending version names, so blobs arriving below survive
        // a concurrent prune. Only collection fetches; publication inspects
        // what collection already stored, while holding the registry lock
        // that storing takes.
        storage.save_payload_blobs(bytes).map_err(storage_error)?;
    }
    let mut total = payload_total(&lengths);
    for hash in missing {
        let len = if let Some(len) = storage.payload_blob_len(&hash).map_err(storage_error)? {
            len
        } else if fetch.is_some() {
            fetch_payload_blob(storage, version, &hash, policy, (total, limit))?
        } else {
            return Err(BundleReviewError::MissingPayload {
                version: version.version.clone(),
                blob: hash.to_hex(),
            });
        };
        total = total.saturating_add(len);
        if total > limit {
            return Err(BundleReviewError::TooLarge {
                version: version.version.clone(),
                size: total,
                limit,
            });
        }
        lengths.insert(hash, len);
    }
    if total > limit {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: total,
            limit,
        });
    }
    app_registry::validate_payload_shape_with(&meta, |hash| lengths.get(hash).copied()).map_err(
        |error| BundleReviewError::Verify {
            version: version.version.clone(),
            reason: error.to_string(),
        },
    )?;
    Ok(inspected(version, &meta, bytes, total))
}

fn payload_total(lengths: &std::collections::BTreeMap<tilo_app_package::BlobHash, u64>) -> u64 {
    lengths
        .values()
        .fold(0u64, |sum, len| sum.saturating_add(*len))
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
    let bytes = download_tapp(version, policy)?;
    let inspection = inspect_bytes(entry, version, &bytes)?;
    Ok((inspection, bytes))
}

/// Download the exact `.tapp` bytes a version names, within policy.
fn download_tapp(
    version: &RegistryVersion,
    policy: InspectionPolicy,
) -> Result<Vec<u8>, BundleReviewError> {
    let limit = policy.download_limit(version);
    if version.tapp_size_bytes > limit {
        return Err(BundleReviewError::TooLarge {
            version: version.version.clone(),
            size: version.tapp_size_bytes,
            limit,
        });
    }
    let url = parse_url(&version.tapp_url, policy)?;
    let fetch = |error: reqwest::Error| fetch_error(&version.tapp_url, &error);
    let mut response = safe_get(&url, policy)?.error_for_status().map_err(fetch)?;
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
    Ok(bytes)
}

/// Parse a URL review may fetch: HTTPS (or, for an explicitly local
/// registry, HTTP) without credentials or a fragment.
fn parse_url(text: &str, policy: InspectionPolicy) -> Result<reqwest::Url, BundleReviewError> {
    let url = reqwest::Url::parse(text).map_err(|error| BundleReviewError::Url {
        url: text.to_owned(),
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
            url: text.to_owned(),
            reason: "expected HTTPS without credentials or a fragment".into(),
        });
    }
    Ok(url)
}

/// GET `url` from a public address pinned against DNS rebinding, without
/// following redirects, within the policy's per-request timeout.
fn safe_get(
    url: &reqwest::Url,
    policy: InspectionPolicy,
) -> Result<reqwest::blocking::Response, BundleReviewError> {
    let text = url.as_str();
    let host = url.host_str().ok_or_else(|| BundleReviewError::Url {
        url: text.to_owned(),
        reason: "host is missing".into(),
    })?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| BundleReviewError::Url {
            url: text.to_owned(),
            reason: "port is missing".into(),
        })?;
    let addresses: Vec<SocketAddr> = (host, port)
        .to_socket_addrs()
        .map_err(|error| BundleReviewError::Resolve {
            url: text.to_owned(),
            reason: error.to_string(),
        })?
        .collect();
    let Some(address) = addresses.first().copied() else {
        return Err(BundleReviewError::PrivateAddress(text.to_owned()));
    };
    if !policy.allow_insecure_local && addresses.iter().any(|addr| !public_ip(addr.ip())) {
        return Err(BundleReviewError::PrivateAddress(text.to_owned()));
    }
    // Pin the already-checked address to prevent DNS rebinding.
    let client = tilo_net::http::blocking_client()
        .timeout(policy.timeout)
        .redirect(reqwest::redirect::Policy::none())
        .resolve(host, address)
        .build()
        .map_err(|error| fetch_error(text, &error))?;
    let response = client
        .get(url.clone())
        .send()
        .map_err(|error| fetch_error(text, &error))?;
    if response.status().is_redirection() {
        return Err(BundleReviewError::Fetch {
            url: text.to_owned(),
            reason: "redirects are not allowed during moderation review".into(),
        });
    }
    Ok(response)
}

/// Fetch the payload blob `hash` from the objects beside the version's
/// `tapp_url` into `storage`, verified, returning its length. A whole
/// `<hash>.blob` streams to disk and is published only once it hashed
/// right; a `<hash>.chunks` list is followed chunk by chunk, each streamed
/// and checked the same way and the joined bytes checked against `hash`
/// before the list is published. Memory stays at one copy buffer. `spent`
/// payload bytes of the package's `limit` are already accounted for.
fn fetch_payload_blob(
    storage: &crate::Storage,
    version: &RegistryVersion,
    hash: &tilo_app_package::BlobHash,
    policy: InspectionPolicy,
    (spent, limit): (u64, u64),
) -> Result<u64, BundleReviewError> {
    let objects = Objects {
        storage,
        version,
        base: parse_url(&version.tapp_url, policy)?,
        policy,
        spent,
        limit,
    };
    if let Some(len) = objects.fetch_whole(hash)? {
        return Ok(len);
    }
    objects.fetch_chunked(hash)
}

/// The payload objects a publisher hosts beside one version's `.tapp`.
struct Objects<'a> {
    storage: &'a crate::Storage,
    version: &'a RegistryVersion,
    base: reqwest::Url,
    policy: InspectionPolicy,
    spent: u64,
    limit: u64,
}

impl Objects<'_> {
    fn url(&self, name: &str) -> Result<reqwest::Url, BundleReviewError> {
        let url = self
            .base
            .join(name)
            .map_err(|error| BundleReviewError::Url {
                url: self.version.tapp_url.clone(),
                reason: error.to_string(),
            })?;
        parse_url(url.as_str(), self.policy)
    }

    fn error(&self, url: &reqwest::Url, reason: impl std::fmt::Display) -> BundleReviewError {
        BundleReviewError::PayloadObject {
            version: self.version.version.clone(),
            url: url.to_string(),
            reason: reason.to_string(),
        }
    }

    fn storage_error(&self, error: &crate::StorageError) -> BundleReviewError {
        BundleReviewError::Storage {
            version: self.version.version.clone(),
            reason: error.to_string(),
        }
    }

    fn budget(&self) -> u64 {
        self.limit.saturating_sub(self.spent)
    }

    fn too_large(&self, size: u64) -> BundleReviewError {
        BundleReviewError::TooLarge {
            version: self.version.version.clone(),
            size: self.spent.saturating_add(size),
            limit: self.limit,
        }
    }

    /// `<hash>.blob` into storage, or `None` when it is not hosted.
    fn fetch_whole(
        &self,
        hash: &tilo_app_package::BlobHash,
    ) -> Result<Option<u64>, BundleReviewError> {
        use tilo_app_package::chunks::{MAX_OBJECT_BYTES, blob_object_name};
        let url = self.url(&blob_object_name(hash))?;
        let Some(mut response) = get_object(&url, self.policy)? else {
            return Ok(None);
        };
        let object_limit = MAX_OBJECT_BYTES.min(self.budget());
        let oversized = |size: u64| {
            if size > self.budget() {
                self.too_large(size)
            } else {
                self.error(
                    &url,
                    format!("exceeds the {MAX_OBJECT_BYTES}-byte object limit"),
                )
            }
        };
        if let Some(length) = response.content_length()
            && length > object_limit
        {
            return Err(oversized(length));
        }
        let mut incoming = self
            .storage
            .incoming_payload_object()
            .map_err(|error| self.storage_error(&error))?;
        let mut hasher = blake3::Hasher::new();
        let len = copy_bounded(&mut response, object_limit, |bytes| {
            hasher.update(bytes);
            incoming.write_all(bytes)
        })
        .map_err(|error| self.error(&url, error))?
        .ok_or_else(|| oversized(object_limit.saturating_add(1)))?;
        if hasher.finalize().as_bytes() != hash.as_bytes() {
            return Err(self.error(&url, "bytes do not match the blob"));
        }
        incoming
            .commit(hash)
            .map_err(|error| self.storage_error(&error))?;
        Ok(Some(len))
    }

    /// `<hash>.chunks` and every chunk it names into storage.
    fn fetch_chunked(&self, hash: &tilo_app_package::BlobHash) -> Result<u64, BundleReviewError> {
        use tilo_app_package::chunks::{
            ChunkList, MAX_CHUNK_LIST_BYTES, Reassembly, blob_object_name, chunk_list_name,
        };
        let list_url = self.url(&chunk_list_name(hash))?;
        let Some(mut response) = get_object(&list_url, self.policy)? else {
            return Err(BundleReviewError::MissingPayload {
                version: self.version.version.clone(),
                blob: hash.to_hex(),
            });
        };
        let mut list_bytes = Vec::new();
        copy_bounded(&mut response, MAX_CHUNK_LIST_BYTES, |bytes| {
            list_bytes.extend_from_slice(bytes);
            Ok(())
        })
        .map_err(|error| self.error(&list_url, error))?
        .ok_or_else(|| self.error(&list_url, "chunk list is too large"))?;
        let list =
            ChunkList::decode(&list_bytes, hash).map_err(|error| self.error(&list_url, error))?;
        if list.len > self.budget() {
            return Err(self.too_large(list.len));
        }
        let stored = |error: crate::StorageError| self.storage_error(&error);
        self.storage.begin_chunked_payload(&list).map_err(stored)?;
        let mut assembly = Reassembly::new(list.clone());
        while let Some(chunk) = assembly.current() {
            let url = self.url(&blob_object_name(&chunk.hash))?;
            if let Some(file) = self
                .storage
                .open_payload_object(&chunk.hash)
                .map_err(stored)?
            {
                // Already held, verified when stored; it still feeds the
                // whole-blob check.
                copy_bounded(file, chunk.len, |bytes| {
                    assembly.update(bytes).map_err(std::io::Error::from)
                })
                .map_err(|error| self.error(&url, error))?;
                assembly
                    .finish_chunk()
                    .map_err(|error| self.error(&url, error))?;
                continue;
            }
            let mut response = get_object(&url, self.policy)?
                .ok_or_else(|| self.error(&url, "chunk is missing"))?;
            let mut incoming = self.storage.incoming_payload_object().map_err(stored)?;
            copy_bounded(&mut response, chunk.len, |bytes| {
                assembly.update(bytes).map_err(std::io::Error::from)?;
                incoming.write_all(bytes)
            })
            .map_err(|error| self.error(&url, error))?
            .ok_or_else(|| self.error(&url, "chunk is longer than listed"))?;
            assembly
                .finish_chunk()
                .map_err(|error| self.error(&url, error))?;
            incoming.commit(&chunk.hash).map_err(stored)?;
        }
        self.storage.finish_chunked_payload(&list).map_err(stored)?;
        Ok(list.len)
    }
}

/// GET a payload object: `None` on 404, an error for any other failure.
fn get_object(
    url: &reqwest::Url,
    policy: InspectionPolicy,
) -> Result<Option<reqwest::blocking::Response>, BundleReviewError> {
    let response = safe_get(url, policy)?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    response
        .error_for_status()
        .map(Some)
        .map_err(|error| fetch_error(url.as_str(), &error))
}

/// Stream `reader` to `sink` in bounded pieces: the byte count, or `None`
/// when the reader yields more than `limit` bytes.
fn copy_bounded(
    mut reader: impl Read,
    limit: u64,
    mut sink: impl FnMut(&[u8]) -> std::io::Result<()>,
) -> std::io::Result<Option<u64>> {
    let mut buffer = vec![0u8; COPY_BUFFER_BYTES];
    let mut total: u64 = 0;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) => return Ok(Some(total)),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        total = total.saturating_add(read as u64);
        if total > limit {
            return Ok(None);
        }
        sink(buffer.get(..read).unwrap_or_default())?;
    }
}

fn fetch_error(url: &str, error: &reqwest::Error) -> BundleReviewError {
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
        url: url.to_owned(),
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
    let meta = inspect_metadata(entry, version, bytes)?;
    app_registry::validate_payload_shape(&meta).map_err(|error| BundleReviewError::Verify {
        version: version.version.clone(),
        reason: error.to_string(),
    })?;
    Ok(inspected(
        version,
        &meta,
        bytes,
        payload_total(&meta.tapp.carried),
    ))
}

fn inspected(
    version: &RegistryVersion,
    meta: &app_registry::TappMeta,
    bytes: &[u8],
    payload_bytes: u64,
) -> InspectedVersion {
    InspectedVersion {
        version: version.version.clone(),
        package_id: meta.package_id.to_string(),
        size_bytes: bytes.len() as u64,
        payload_bytes,
        sandbox: serde_json::to_value(&meta.manifest.sandbox).unwrap_or(serde_json::Value::Null),
    }
}

/// Everything [`inspect_bytes`] checks except the payload shape, which
/// needs every payload blob's length: size, digest, signature, manifest
/// identity and signed target.
fn inspect_metadata(
    entry: &RegistryEntry,
    version: &RegistryVersion,
    bytes: &[u8],
) -> Result<app_registry::TappMeta, BundleReviewError> {
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
    Ok(meta)
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
            && target.interfaces.is_empty()
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
    // The interfaces a version records are exactly the ones its signed
    // manifest requires: a device decides what to offer from them before it
    // has the package, so an entry must not understate them. A version
    // published before versions recorded interfaces records none; entries
    // keep such versions, and refusing them would refuse every update of
    // their entry. `tilo-install` and the launcher check the signed
    // manifest itself, so such a build still never installs or runs where
    // its interfaces are not served.
    let interfaces_bound =
        target.interfaces.is_empty() || manifest.required_interface_wire() == target.interfaces;
    if manifest_isa.as_deref() != Some(target.isa.as_str())
        || manifest.package.native_runtime_binding() != target.runtime.as_deref()
        || !interfaces_bound
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
            // The interfaces a version claims are the manifest's, exactly.
            entry.versions[0].target.isa = "aarch64-v8.2".into();
            entry.versions[0].target.interfaces = vec!["system@1".into()];
            assert!(matches!(
                inspect_bytes(&entry, &entry.versions[0], &bytes),
                Err(BundleReviewError::TargetMismatch { .. })
            ));
        }
        // phase: a_version_recorded_before_interfaces_still_reviews
        {
            let identity = SigningIdentity::generate();
            let (bytes, package_id) = signed_tapp_with(
                &identity,
                "aarch64 = \"v8.2-a\"",
                "[sandbox.services]\nsystem = \"required@1\"\n",
            );
            let mut entry = sample_entry("https://example.com/arm.tapp");
            entry.publisher_pubkey_hex = hex::encode(identity.public_key_bytes());
            let version = &mut entry.versions[0];
            version.tapp_root_hash_hex = hex::encode(package_id.as_bytes());
            version.tapp_size_bytes = bytes.len() as u64;
            version.tapp_blake3_hex = blake3::hash(&bytes).to_hex().to_string();
            version.target.architecture = "aarch64".into();
            version.target.isa = "aarch64-v8.2".into();
            // Recorded before versions carried interfaces: none recorded.
            assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());
            entry.versions[0].target.interfaces = vec!["system@1".into()];
            assert!(inspect_bytes(&entry, &entry.versions[0], &bytes).is_ok());
            // Recorded, it must be exactly the manifest's.
            entry.versions[0].target.interfaces = vec!["system@2".into()];
            assert!(matches!(
                inspect_bytes(&entry, &entry.versions[0], &bytes),
                Err(BundleReviewError::TargetMismatch { .. })
            ));
        }
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
            interfaces: Vec::new(),
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
            interfaces: Vec::new(),
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
        signed_tapp_with(identity, architecture, "")
    }

    fn signed_tapp_with(
        identity: &SigningIdentity,
        architecture: &str,
        extra: &str,
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
{extra}
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
                    interfaces: Vec::new(),
                },
            }],
            updated_at_unix: 1,
            signature_hex: String::new(),
        }
    }
}
