//! HTTP client for a Tilo registry.
//!
//! Two endpoints, both blocking — every caller runs these on a worker
//! thread, never on a frame thread:
//!
//!   * `GET {base_url}/registry` — informational metadata, accepted only
//!     when it names the already-pinned owner key.
//!   * `GET {base_url}/entries?format=compact` — an owner-signed catalogue
//!     snapshot. A registry that has a compact form answers with it: per
//!     entry only the newest versions, and the hash of a version index
//!     (`GET {base_url}/indexes/<blake3>.json`) holding the rest. Any other
//!     registry answers with the full snapshot; both verify the same way.
//!
//! Every fetched entry is signature-checked against its own declared
//! publisher pubkey via [`registry_types::verify_entry_signature`]. A
//! separate allowlist check compares the entry's publisher pubkey
//! against the registry *owner's* allowlisted publishers.
//!
//! Nothing here is a trust decision about *installing*. A registry
//! describes what exists; `tilo-install` re-verifies the actual bytes
//! fail-closed before it extracts one of them. An entry that fails its
//! signature check is [`TrustLevel::Untrusted`], and callers that put
//! software in front of a user are expected to drop those rather than
//! rank them — see [`crate::available`].

use std::io::Read as _;
use std::time::Duration;

use registry_types::{CatalogueSnapshot, RegistryEntry, RegistryHandshake, VersionIndexRef};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::config::{RegistryConfig, config_dir, placeholder_owner_pubkey};

/// How long one step of a request may wait: connecting and receiving the
/// response head, then each read of the body. The blocking client applies it
/// per operation, not to the whole transfer, so a large catalogue on a slow
/// but progressing link is not cut off. Overridable via
/// `TILO_STORE_HTTP_TIMEOUT`.
const DEFAULT_TIMEOUT_SECS: u64 = 10;
const MAX_INDEX_RESPONSE_BYTES: u64 = registry_types::MAX_SNAPSHOT_ENCODED_BYTES as u64;

fn http_timeout() -> Duration {
    std::env::var("TILO_STORE_HTTP_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map_or_else(
            || Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            Duration::from_secs,
        )
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("http: {0}")]
    Http(String),
    #[error("json decode: {0}")]
    Decode(String),
    #[error("empty response")]
    Empty,
    #[error("registry snapshot proof failed: {0}")]
    Proof(#[from] registry_types::SnapshotError),
    /// The registry's owner-signed catalogue is authentic but past its
    /// expiry: the registry has not renewed it. Nothing in it is offered.
    #[error(
        "the registry's catalogue expired at {expired_at_unix} (Unix time) and has not been renewed"
    )]
    Expired { expired_at_unix: u64 },
    #[error("registry snapshot was rolled back or replayed")]
    Rollback,
    #[error(
        "registry serves a catalogue signed with sequence {served_sequence}, behind the sequence {held_sequence} this device already accepted from it"
    )]
    BehindAccepted {
        /// The sequence this device's checkpoint holds.
        held_sequence: u64,
        /// The sequence the registry just served.
        served_sequence: u64,
    },
    #[error("registry snapshot checkpoint could not be persisted: {0}")]
    Checkpoint(String),
    #[error("this device holds no verified catalogue from the registry")]
    NotStored,
    #[error("registry handshake owner does not match the pinned key")]
    OwnerMismatch,
    #[error("registry URL must be credential-free HTTPS (or loopback HTTP for development)")]
    UnsafeEndpoint,
    #[error("{0} is not in the registry")]
    NotListed(String),
    #[error("registry version index proof failed: {0}")]
    VersionIndex(#[from] registry_types::VersionIndexError),
    #[error("bot program does not match its listing: {0}")]
    ProgramMismatch(String),
}

impl RegistryError {
    /// The regression a registry that came back behind this device's own
    /// checkpoint is, as opposed to any other fetch failure.
    #[must_use]
    pub fn behind_accepted(&self) -> Option<BehindAccepted> {
        match self {
            RegistryError::BehindAccepted {
                held_sequence,
                served_sequence,
            } => Some(BehindAccepted {
                held_sequence: *held_sequence,
                served_sequence: *served_sequence,
            }),
            _ => None,
        }
    }
}

/// A registry answered with a catalogue older than the one this device
/// already accepted from it — the signature and the identity check out, only
/// the sequence went backwards, which is what a host rebuilt from a backup
/// serves until it publishes past what it lost.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BehindAccepted {
    /// The sequence this device's checkpoint holds.
    pub held_sequence: u64,
    /// The sequence the registry just served.
    pub served_sequence: u64,
}

/// Trust classification for a fetched entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub enum TrustLevel {
    /// The publisher signature and the pinned registry owner's exact snapshot
    /// signature and authorization policy all verified.
    PublisherSigned,
    /// The publisher signature failed to verify.
    Untrusted,
}

impl TrustLevel {
    /// Whether an entry at this level may be offered to a user as
    /// something to open.
    ///
    /// `Untrusted` means the metadata itself — the name, the
    /// description, the download URL — was not signed by the key it
    /// claims. Offering that as a row would let a registry put any
    /// name it liked on any download, so it is dropped before it
    /// reaches a list.
    pub fn is_offerable(self) -> bool {
        matches!(self, TrustLevel::PublisherSigned)
    }
}

/// One entry plus its trust state.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifiedEntry {
    pub entry: RegistryEntry,
    pub trust: TrustLevel,
    /// Where the complete version list lives when the snapshot was compact
    /// and `entry` holds only the newest versions; `None` when `entry`
    /// already holds every version.
    pub version_index: Option<VersionIndexRef>,
}

/// Last accepted proof for one pinned registry identity.
#[derive(Debug, Deserialize, Serialize)]
struct SnapshotCheckpoint {
    sequence: u64,
    snapshot_hash_hex: String,
}

/// The process's one registry client, so the requests of a run share its
/// connection pool instead of each paying a new TCP and TLS handshake. Its
/// timeout ([`http_timeout`]) bounds each step of a request, not the whole
/// transfer.
fn client() -> Result<&'static reqwest::blocking::Client, RegistryError> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::http::blocking_client()
                .user_agent("app/0.1")
                .timeout(http_timeout())
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(|e| RegistryError::Http(e.clone()))
}

/// Fetch and parse informational registry metadata, rejecting an owner identity
/// that differs from the preconfigured pin. This endpoint never establishes
/// trust on first contact.
pub fn fetch_handshake(reg: &RegistryConfig) -> Result<RegistryHandshake, RegistryError> {
    let url = registry_url(reg, "registry")?;
    let c = client()?;
    let text = c
        .get(url)
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .and_then(reqwest::blocking::Response::text)
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if text.trim().is_empty() {
        return Err(RegistryError::Empty);
    }
    let handshake: RegistryHandshake =
        serde_json::from_str(&text).map_err(|e| RegistryError::Decode(e.to_string()))?;
    if !handshake
        .owner_pubkey_hex
        .eq_ignore_ascii_case(&reg.owner_pubkey_hex)
    {
        return Err(RegistryError::OwnerMismatch);
    }
    Ok(handshake)
}

/// Fetch `/entries` and classify each result.
pub fn fetch_entries(reg: &RegistryConfig) -> Result<Vec<VerifiedEntry>, RegistryError> {
    let snapshot = fetch_snapshot(reg)?;
    let now = tilo_clock::unix_now();
    classify_snapshot(&snapshot, &reg.owner_pubkey_hex, now)
}

/// Fetch and verify the owner-signed snapshot, compact when the registry
/// offers it, while advancing the durable anti-rollback checkpoint.
///
/// # Errors
///
/// Returns an error if the endpoint is unsafe, the response fails or exceeds
/// the size limit, any proof check fails, or the snapshot would roll back or
/// fork the durable checkpoint.
pub fn fetch_snapshot(reg: &RegistryConfig) -> Result<CatalogueSnapshot, RegistryError> {
    let mut url = registry_url(reg, "entries")?;
    url.set_query(Some("format=compact"));
    // The registry names each form of its snapshot by its hash (`ETag`), so
    // asking with the hash of the one this device holds costs an empty 304
    // when nothing changed: an update with nothing new downloads nothing.
    let held = checkpoint_path(reg).and_then(|path| held_snapshot(&path));
    let etag = held.as_ref().and_then(form_hash);
    let now = tilo_clock::unix_now();
    let Some(bytes) =
        fetch_revalidated(&url, MAX_INDEX_RESPONSE_BYTES, "snapshot", etag.as_deref())?
    else {
        // Not modified: the held snapshot is the registry's, and is verified
        // again against the pinned key and the clock as a download would be.
        let held = held.ok_or(RegistryError::NotStored)?;
        if let Err(error) = classify_snapshot(&held, &reg.owner_pubkey_hex, now) {
            return Err(expired_or(error, &held, &reg.owner_pubkey_hex, now));
        }
        return Ok(held);
    };
    let snapshot =
        CatalogueSnapshot::from_json(&bytes).map_err(|e| RegistryError::Decode(e.to_string()))?;
    if let Err(error) = classify_snapshot(&snapshot, &reg.owner_pubkey_hex, now) {
        return Err(expired_or(error, &snapshot, &reg.owner_pubkey_hex, now));
    }
    persist_checkpoint(reg, &snapshot, &bytes)?;
    Ok(snapshot)
}

/// `error`, unless `snapshot` failed only by having expired: then
/// [`RegistryError::Expired`], so a device can say the catalogue expired
/// rather than that the registry is unreachable.
fn expired_or(
    error: RegistryError,
    snapshot: &CatalogueSnapshot,
    owner_pubkey_hex: &str,
    now_unix: u64,
) -> RegistryError {
    match (&error, snapshot.expired_at(owner_pubkey_hex, now_unix)) {
        (RegistryError::Proof(registry_types::SnapshotError::Freshness), Some(expired_at_unix)) => {
            RegistryError::Expired { expired_at_unix }
        }
        _ => error,
    }
}

/// What [`fetch_snapshot_answer`] found: the catalogue to read, and whether
/// it came from the registry just now or from this device's own checkpoint
/// because the registry answered behind it.
#[derive(Clone, Debug)]
pub enum SnapshotAnswer {
    /// The registry's catalogue is the newest this device has seen.
    Advanced,
    /// The registry served one signed catalogue older than the last accepted
    /// from it, so the last accepted one is what stays in force.
    BehindAccepted(BehindAccepted),
}

impl SnapshotAnswer {
    /// Whether the catalogue is the one this device keeps rather than the one
    /// the registry just served.
    #[must_use]
    pub fn held(&self) -> bool {
        matches!(self, SnapshotAnswer::BehindAccepted(_))
    }
}

/// Fetch the owner-signed snapshot from `reg` and say what happened to the
/// device's view of it.
///
/// A registry that answers with a signed catalogue older than the one this
/// device already accepted — what a host rebuilt from a backup serves until
/// it publishes new state — is not an error here: the answer says so and the
/// snapshot is the one this device holds, verified again, so callers keep
/// reading a catalogue that was not walked backwards.
///
/// # Errors
///
/// Returns [`RegistryError::BehindAccepted`] only when no held catalogue can
/// stand in (none was accepted yet, or the held one no longer verifies);
/// otherwise the same errors as [`fetch_snapshot`].
pub fn fetch_snapshot_answer(
    reg: &RegistryConfig,
) -> Result<(CatalogueSnapshot, SnapshotAnswer), RegistryError> {
    match fetch_snapshot(reg) {
        Err(error @ RegistryError::BehindAccepted { .. }) => {
            let Some(behind) = error.behind_accepted() else {
                return Err(error);
            };
            let Some(path) = checkpoint_path(reg) else {
                return Err(error);
            };
            let held =
                last_verified_snapshot_at(&path, &reg.owner_pubkey_hex, tilo_clock::unix_now())
                    .map_err(|_| error)?;
            Ok((held, SnapshotAnswer::BehindAccepted(behind)))
        }
        other => other.map(|snapshot| (snapshot, SnapshotAnswer::Advanced)),
    }
}

/// The catalogue this device last accepted from `reg`, read from disk without
/// touching the network, so a registry that is slow or unreachable still
/// describes what it listed.
///
/// The stored bytes are not trusted: they must be exactly the snapshot the
/// durable anti-rollback checkpoint names, and they are verified again against
/// the pinned owner key and the current time, so an expired catalogue is
/// refused here just as a freshly fetched one would be.
///
/// # Errors
///
/// [`RegistryError::NotStored`] when no catalogue was accepted yet, and the
/// proof, freshness or checkpoint error that makes a stored one unusable.
pub fn last_verified_entries(reg: &RegistryConfig) -> Result<Vec<VerifiedEntry>, RegistryError> {
    let path = checkpoint_path(reg).ok_or(RegistryError::NotStored)?;
    last_verified_at(&path, &reg.owner_pubkey_hex, tilo_clock::unix_now())
}

fn last_verified_at(
    path: &std::path::Path,
    owner_pubkey_hex: &str,
    now_unix: u64,
) -> Result<Vec<VerifiedEntry>, RegistryError> {
    let snapshot = last_verified_snapshot_at(path, owner_pubkey_hex, now_unix)?;
    classify_snapshot(&snapshot, owner_pubkey_hex, now_unix)
}

/// The snapshot this device last accepted from the registry that `path`
/// checkpoints, verified exactly as [`last_verified_at`] verifies it.
fn last_verified_snapshot_at(
    path: &std::path::Path,
    owner_pubkey_hex: &str,
    now_unix: u64,
) -> Result<CatalogueSnapshot, RegistryError> {
    #[cfg(unix)]
    let _lock = checkpoint_lock(path)?;
    let read = |path: &std::path::Path| match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Err(RegistryError::NotStored),
        Err(error) => Err(RegistryError::Checkpoint(error.to_string())),
    };
    let checkpoint: SnapshotCheckpoint = serde_json::from_slice(&read(path)?)
        .map_err(|error| RegistryError::Checkpoint(error.to_string()))?;
    let snapshot = CatalogueSnapshot::from_json(&read(&snapshot_path(path))?)
        .map_err(|error| RegistryError::Decode(error.to_string()))?;
    // Anything but the checkpointed snapshot is an older (or foreign) one.
    if snapshot.sequence() != checkpoint.sequence
        || snapshot.checkpoint_hash()? != checkpoint.snapshot_hash_hex
    {
        return Err(RegistryError::Rollback);
    }
    // Verified against the pinned owner key and the current time here too, so
    // a stored catalogue that expired while it sat on disk is refused rather
    // than handed out.
    let _ = classify_snapshot(&snapshot, owner_pubkey_hex, now_unix)?;
    Ok(snapshot)
}

/// The snapshot this device accepted last from the registry whose
/// checkpoint is `path`, when its bytes are the ones the checkpoint names.
/// Not verified: it names what to revalidate, and is verified before use.
fn held_snapshot(path: &std::path::Path) -> Option<CatalogueSnapshot> {
    if path.parent().is_some_and(|parent| !parent.is_dir()) {
        return None;
    }
    #[cfg(unix)]
    let _lock = checkpoint_lock(path).ok()?;
    let checkpoint: SnapshotCheckpoint = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let snapshot = CatalogueSnapshot::from_json(&std::fs::read(snapshot_path(path)).ok()?).ok()?;
    (snapshot.sequence() == checkpoint.sequence
        && snapshot.checkpoint_hash().ok()? == checkpoint.snapshot_hash_hex)
        .then_some(snapshot)
}

/// The hash the registry names this form of a snapshot by, its `ETag`.
fn form_hash(snapshot: &CatalogueSnapshot) -> Option<String> {
    match snapshot {
        CatalogueSnapshot::Full(full) => registry_types::snapshot_hash(full).ok(),
        CatalogueSnapshot::Compact(compact) => registry_types::compact_snapshot_hash(compact).ok(),
    }
}

/// Every retained version of `package_id`: from a freshly verified snapshot,
/// and when that snapshot is compact, from the version index it signs.
///
/// # Errors
///
/// Returns an error under the same conditions as [`fetch_snapshot`], when the
/// package is not listed, or when the version index cannot be fetched or is
/// not the one the snapshot signs.
pub fn fetch_all_versions(
    reg: &RegistryConfig,
    package_id: &str,
) -> Result<VerifiedEntry, RegistryError> {
    let snapshot = fetch_snapshot(reg)?;
    all_versions_in(reg, &snapshot, package_id)
}

/// [`fetch_all_versions`] from a snapshot the caller already fetched and
/// verified, so asking about several packages downloads, verifies and
/// checkpoints the snapshot once instead of once per package.
///
/// # Errors
///
/// Returns an error when the package is not listed in `snapshot`, or when the
/// version index cannot be fetched or is not the one the snapshot signs.
pub fn all_versions_in(
    reg: &RegistryConfig,
    snapshot: &CatalogueSnapshot,
    package_id: &str,
) -> Result<VerifiedEntry, RegistryError> {
    let listed = snapshot
        .entries()
        .into_iter()
        .find(|listed| listed.entry.package_id == package_id)
        .ok_or_else(|| RegistryError::NotListed(package_id.to_string()))?;
    let entry = match snapshot.compact_entry(package_id) {
        None => listed.entry.clone(),
        Some(compact) => {
            let index = &compact.version_index;
            let url = registry_url(reg, &format!("indexes/{}.json", index.blake3_hex))?;
            let bytes = fetch_bounded(&url, index.size_bytes, "version index")?;
            registry_types::verify_version_index(compact, &bytes)?
        }
    };
    Ok(VerifiedEntry {
        entry,
        trust: TrustLevel::PublisherSigned,
        version_index: None,
    })
}

/// GET `url`, refusing a body over `limit` bytes or an empty one.
pub(crate) fn fetch_bounded(
    url: &reqwest::Url,
    limit: u64,
    what: &str,
) -> Result<Vec<u8>, RegistryError> {
    let c = client()?;
    let response = c
        .get(url.clone())
        .send()
        .and_then(reqwest::blocking::Response::error_for_status)
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    read_bounded(response, limit, what)
}

/// [`fetch_bounded`], asking the server to answer `304 Not Modified` (then
/// `None`) when what it would send has the `ETag` `etag`.
fn fetch_revalidated(
    url: &reqwest::Url,
    limit: u64,
    what: &str,
    etag: Option<&str>,
) -> Result<Option<Vec<u8>>, RegistryError> {
    let mut request = client()?.get(url.clone());
    if let Some(etag) = etag {
        request = request.header(reqwest::header::IF_NONE_MATCH, format!("\"{etag}\""));
    }
    let response = request
        .send()
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if etag.is_some() && response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let response = response
        .error_for_status()
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    read_bounded(response, limit, what).map(Some)
}

fn read_bounded(
    response: reqwest::blocking::Response,
    limit: u64,
    what: &str,
) -> Result<Vec<u8>, RegistryError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(RegistryError::Decode(format!(
            "{what} response is too large"
        )));
    }
    let mut bytes = Vec::new();
    response
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| RegistryError::Http(e.to_string()))?;
    if bytes.is_empty() {
        return Err(RegistryError::Empty);
    }
    if bytes.len() as u64 > limit {
        return Err(RegistryError::Decode(format!(
            "{what} response is too large"
        )));
    }
    Ok(bytes)
}

fn registry_url(reg: &RegistryConfig, suffix: &str) -> Result<reqwest::Url, RegistryError> {
    let mut url = reqwest::Url::parse(&reg.base_url).map_err(|_| RegistryError::UnsafeEndpoint)?;
    let loopback_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if (url.scheme() != "https" && !loopback_http)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(RegistryError::UnsafeEndpoint);
    }
    url.set_query(None);
    url.set_fragment(None);
    let path = format!("{}/{}", url.path().trim_end_matches('/'), suffix);
    url.set_path(&path);
    Ok(url)
}

/// Pure proof verification used by HTTP and fixture tests.
pub fn classify_snapshot(
    snapshot: &CatalogueSnapshot,
    owner_pubkey_hex: &str,
    now_unix: u64,
) -> Result<Vec<VerifiedEntry>, RegistryError> {
    if owner_pubkey_hex.is_empty() || owner_pubkey_hex == placeholder_owner_pubkey() {
        return Err(registry_types::SnapshotError::OwnerKey.into());
    }
    snapshot.verify(owner_pubkey_hex, now_unix)?;
    Ok(snapshot
        .entries()
        .into_iter()
        .map(|listed| VerifiedEntry {
            entry: listed.entry.clone(),
            trust: TrustLevel::PublisherSigned,
            version_index: listed.version_index.cloned(),
        })
        .collect())
}

fn checkpoint_path(reg: &RegistryConfig) -> Option<std::path::PathBuf> {
    let mut hasher = blake3::Hasher::new();
    // The owner key is the registry identity. Transport aliases may change,
    // but must not reset anti-rollback history.
    hasher.update(reg.owner_pubkey_hex.as_bytes());
    Some(
        config_dir()?
            .join("registry-checkpoints")
            .join(format!("{}.json", hasher.finalize().to_hex())),
    )
}

/// The accepted snapshot's bytes, beside the checkpoint that names them.
fn snapshot_path(checkpoint: &std::path::Path) -> std::path::PathBuf {
    checkpoint.with_extension("snapshot.json")
}

/// Advance the checkpoint to `snapshot` and keep its verified `bytes` for
/// [`last_verified_entries`]. Both are written under one lock; a crash between
/// the two writes leaves bytes the checkpoint does not name, which the reader
/// refuses.
fn persist_checkpoint(
    reg: &RegistryConfig,
    snapshot: &CatalogueSnapshot,
    bytes: &[u8],
) -> Result<(), RegistryError> {
    match checkpoint_path(reg) {
        Some(path) => persist_checkpoint_at(&path, snapshot, bytes),
        None => Ok(()),
    }
}

fn persist_checkpoint_at(
    path: &std::path::Path,
    snapshot: &CatalogueSnapshot,
    bytes: &[u8],
) -> Result<(), RegistryError> {
    // Both forms of one snapshot share this identity, so switching form is
    // neither a rollback nor a fork.
    let hash = snapshot.checkpoint_hash()?;
    #[cfg(unix)]
    let _lock = checkpoint_lock(path)?;
    let stored = snapshot_path(path);
    let unchanged = match std::fs::read(path) {
        Ok(old) => {
            let old: SnapshotCheckpoint = serde_json::from_slice(&old)
                .map_err(|error| RegistryError::Checkpoint(error.to_string()))?;
            validate_checkpoint(&old, snapshot.sequence(), &hash)?;
            // The same sequence passed validation, so it is this snapshot.
            old.sequence == snapshot.sequence() && stored.is_file()
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(RegistryError::Checkpoint(error.to_string())),
    };
    if unchanged {
        return Ok(());
    }
    tilo_files::native::atomic::write_creating_parent(&stored, bytes)
        .map_err(|error| RegistryError::Checkpoint(error.to_string()))?;
    let checkpoint = SnapshotCheckpoint {
        sequence: snapshot.sequence(),
        snapshot_hash_hex: hash,
    };
    let encoded = serde_json::to_vec(&checkpoint)
        .map_err(|error| RegistryError::Checkpoint(error.to_string()))?;
    tilo_files::native::atomic::write_creating_parent(path, &encoded)
        .map_err(|error| RegistryError::Checkpoint(error.to_string()))
}

#[cfg(unix)]
fn checkpoint_lock(path: &std::path::Path) -> Result<advisory_lock::FileLock, RegistryError> {
    let lock_path = path.with_extension("lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| RegistryError::Checkpoint(error.to_string()))?;
    }
    advisory_lock::FileLock::open(&lock_path, advisory_lock::LockMode::Exclusive)
        .map_err(|error| RegistryError::Checkpoint(error.to_string()))
}

fn validate_checkpoint(
    old: &SnapshotCheckpoint,
    sequence: u64,
    hash: &str,
) -> Result<(), RegistryError> {
    if sequence < old.sequence {
        // Signed, and for this registry identity — only older than what this
        // device accepted. Callers distinguish it so they can keep serving
        // the catalogue they hold instead of reporting the registry as
        // failed.
        return Err(RegistryError::BehindAccepted {
            held_sequence: old.sequence,
            served_sequence: sequence,
        });
    }
    if sequence == old.sequence && hash != old.snapshot_hash_hex {
        return Err(RegistryError::Rollback);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use registry_types::{RegistryVersion, sign_entry};

    pub(crate) fn signed_entry() -> RegistryEntry {
        // Deterministic key so signing is reproducible under `cargo
        // test`; cryptographic hygiene is not what this asserts.
        let key = SigningKey::from_bytes(&[7u8; 32]);
        let pk_hex = hex::encode(key.verifying_key().to_bytes());
        let mut entry = RegistryEntry {
            package_id: "so.tilo.editor".into(),
            kind: registry_types::EntryKind::App,
            name: "Editor".into(),
            description: "CRDT-native editor".into(),
            tagline: "Native Tilo OS app".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec!["Productivity".into()],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: pk_hex,
            versions: vec![RegistryVersion {
                version: "0.1.0".into(),
                released_at_unix: 100,
                artifact: registry_types::VersionArtifact::Tapp(registry_types::TappArtifact {
                    tapp_url: "https://apps.tilo.so/editor/0.1.0.tapp".into(),
                    tapp_root_hash_hex: "cd".repeat(32),
                    tapp_size_bytes: 1024,
                    tapp_blake3_hex: "ef".repeat(32),
                    target: registry_types::RegistryTarget {
                        platform: registry_types::TILO_OS_PLATFORM.into(),
                        architecture: std::env::consts::ARCH.into(),
                        isa: "x86_64".into(),
                        runtime: None,
                        interfaces: Vec::new(),
                    },
                }),
            }],
            updated_at_unix: 200,
            signature_hex: String::new(),
        };
        sign_entry(&mut entry, &key);
        entry
    }

    fn snapshot(owner: &SigningKey) -> CatalogueSnapshot {
        CatalogueSnapshot::Full(signed_snapshot(owner, 1))
    }

    fn signed_snapshot(
        owner: &SigningKey,
        sequence: u64,
    ) -> registry_types::SignedRegistrySnapshot {
        let entry = signed_entry();
        let mut snapshot = registry_types::SignedRegistrySnapshot {
            protocol: registry_types::PACKAGE_FORMAT_VERSION,
            sequence,
            issued_at_unix: 100,
            expires_at_unix: 200,
            previous_snapshot_hash_hex: if sequence == 1 {
                String::new()
            } else {
                "aa".repeat(32)
            },
            moderation_head_hex: String::new(),
            publishers: vec![registry_types::PublisherAuthorization {
                pubkey_hex: entry.publisher_pubkey_hex.clone(),
                can_add_new: true,
            }],
            entries: vec![entry],
            owner_signature_hex: String::new(),
            channel_authorization: None,
        };
        registry_types::sign_snapshot(&mut snapshot, owner).unwrap();
        snapshot
    }

    #[test]
    fn an_authentic_expired_catalogue_is_told_apart_from_a_failed_one() {
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        let classified = |snapshot: &CatalogueSnapshot, now| {
            classify_snapshot(snapshot, &owner_hex, now)
                .map(|_| ())
                .map_err(|error| expired_or(error, snapshot, &owner_hex, now))
        };
        // Issued at 100, valid until 200.
        let genuine = snapshot(&owner);
        assert!(classified(&genuine, 150).is_ok());
        assert!(matches!(
            classified(&genuine, 250),
            Err(RegistryError::Expired {
                expired_at_unix: 200
            })
        ));
        // Another owner's expired snapshot is a proof failure, not "expired".
        let stranger = SigningKey::from_bytes(&[9; 32]);
        assert!(matches!(
            classified(&snapshot(&stranger), 250),
            Err(RegistryError::Proof(_))
        ));
    }

    #[test]
    fn the_last_accepted_catalogue_is_read_back_only_while_it_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoint = dir.path().join("registry-checkpoints/owner.json");
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        assert!(matches!(
            last_verified_at(&checkpoint, &owner_hex, 150),
            Err(RegistryError::NotStored)
        ));

        let first = signed_snapshot(&owner, 1);
        let first_bytes = serde_json::to_vec(&first).unwrap();
        persist_checkpoint_at(&checkpoint, &CatalogueSnapshot::Full(first), &first_bytes).unwrap();
        let entries = last_verified_at(&checkpoint, &owner_hex, 150).unwrap();
        assert_eq!(entries[0].entry.name.resolve("en"), Some("Editor"));
        assert_eq!(entries[0].trust, TrustLevel::PublisherSigned);

        // Expired metadata is refused, exactly as a fresh fetch of it would be.
        assert!(matches!(
            last_verified_at(&checkpoint, &owner_hex, 250),
            Err(RegistryError::Proof(
                registry_types::SnapshotError::Freshness
            ))
        ));
        // The pinned owner key, not the stored bytes, decides.
        let stranger = hex::encode(SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes());
        assert!(last_verified_at(&checkpoint, &stranger, 150).is_err());

        let second = signed_snapshot(&owner, 2);
        let second_bytes = serde_json::to_vec(&second).unwrap();
        persist_checkpoint_at(&checkpoint, &CatalogueSnapshot::Full(second), &second_bytes)
            .unwrap();
        last_verified_at(&checkpoint, &owner_hex, 150).unwrap();
        std::fs::write(snapshot_path(&checkpoint), &first_bytes).unwrap();
        assert!(matches!(
            last_verified_at(&checkpoint, &owner_hex, 150),
            Err(RegistryError::Rollback)
        ));

        let mut tampered = signed_snapshot(&owner, 2);
        tampered.entries[0].name = "hijacked".into();
        std::fs::write(
            snapshot_path(&checkpoint),
            serde_json::to_vec(&tampered).unwrap(),
        )
        .unwrap();
        assert!(last_verified_at(&checkpoint, &owner_hex, 150).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn registry_checkpoint_cannot_walk_ancestors_outside_the_app_sandbox() {
        const CHILD: &str = "TILO_CHECKPOINT_CONFINEMENT_CHILD";
        if let Some(root) = std::env::var_os(CHILD) {
            let root = std::path::PathBuf::from(root);
            let owner = SigningKey::from_bytes(&[8; 32]);
            let registry = RegistryConfig {
                name: "main".into(),
                base_url: "https://apps.tilo.so".into(),
                owner_pubkey_hex: hex::encode(owner.verifying_key().to_bytes()),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            };
            let snapshot = snapshot(&owner);
            let plan = launcher::landlock::plan_from_paths(&[], &[root]);
            assert_eq!(
                launcher::landlock::apply_plan_or_log(&plan, false),
                launcher::landlock::LandlockOutcome::FullyEnforced
            );
            assert_eq!(
                std::fs::read_dir("/").unwrap_err().kind(),
                std::io::ErrorKind::PermissionDenied
            );
            let error = persist_checkpoint(&registry, &snapshot, b"{}").unwrap_err();
            assert!(
                matches!(error, RegistryError::Checkpoint(ref text) if text.contains("Permission denied")),
                "{error}"
            );
            assert!(
                checkpoint_path(&registry)
                    .unwrap()
                    .with_extension("lock")
                    .is_file()
            );
            return;
        }
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join(".config/tilo-store");
        std::fs::create_dir_all(&config).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "client::tests::registry_checkpoint_cannot_walk_ancestors_outside_the_app_sandbox",
                "--nocapture",
            ])
            .env(CHILD, root.path())
            .env("TILO_STORE_CONFIG_DIR", &config)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn registry_client_accepts_only_authorized_monotonic_snapshots_from_safe_endpoints() {
        {
            let owner = SigningKey::from_bytes(&[8; 32]);
            let snapshot = snapshot(&owner);
            let out = classify_snapshot(
                &snapshot,
                &hex::encode(owner.verifying_key().to_bytes()),
                150,
            )
            .unwrap();
            assert_eq!(out[0].trust, TrustLevel::PublisherSigned);
            assert!(out[0].trust.is_offerable());

            assert!(classify_snapshot(&snapshot, &placeholder_owner_pubkey(), 150).is_err());
            let CatalogueSnapshot::Full(mut tampered) = snapshot else {
                unreachable!("the fixture is a full snapshot")
            };
            tampered.entries[0].name = "hijacked".into();
            let tampered = CatalogueSnapshot::Full(tampered);
            assert!(
                classify_snapshot(
                    &tampered,
                    &hex::encode(owner.verifying_key().to_bytes()),
                    150
                )
                .is_err()
            );
        }
        {
            let old = SnapshotCheckpoint {
                sequence: 8,
                snapshot_hash_hex: "aa".repeat(32),
            };
            assert_eq!(
                validate_checkpoint(&old, 7, &"bb".repeat(32))
                    .unwrap_err()
                    .behind_accepted(),
                Some(BehindAccepted {
                    held_sequence: 8,
                    served_sequence: 7
                }),
                "an older sequence is a registry behind this device, named as such"
            );
            assert!(matches!(
                validate_checkpoint(&old, 8, &"bb".repeat(32)),
                Err(RegistryError::Rollback)
            ));
            validate_checkpoint(&old, 8, &"aa".repeat(32)).unwrap();
            validate_checkpoint(&old, 9, &"bb".repeat(32)).unwrap();
        }
        {
            for base_url in [
                "http://registry.test",
                "https://placeholder:placeholder@registry.test",
                "https://registry.test/base?next=https://evil.test",
            ] {
                let registry = RegistryConfig {
                    name: "test".into(),
                    base_url: base_url.into(),
                    owner_pubkey_hex: "11".repeat(32),
                    enabled: true,
                    channel: tilo_app_package::version::ReleaseChannel::Stable,
                };
                if base_url.contains('?') {
                    assert_eq!(
                        registry_url(&registry, "entries").unwrap().as_str(),
                        "https://registry.test/base/entries"
                    );
                } else {
                    assert!(matches!(
                        registry_url(&registry, "entries"),
                        Err(RegistryError::UnsafeEndpoint)
                    ));
                }
            }
            let loopback = RegistryConfig {
                name: "dev".into(),
                base_url: "http://127.0.0.1:8080".into(),
                owner_pubkey_hex: "11".repeat(32),
                enabled: true,
                channel: tilo_app_package::version::ReleaseChannel::Stable,
            };
            assert_eq!(
                registry_url(&loopback, "entries").unwrap().as_str(),
                "http://127.0.0.1:8080/entries"
            );
        }
    }
}
