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
    #[error("registry snapshot was rolled back or replayed")]
    Rollback,
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
///
/// # Errors
///
/// Returns an error if the request fails, the response is empty, or
/// the body doesn't decode as a [`RegistryHandshake`].
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
///
/// # Errors
///
/// Returns an error if the request fails, is oversized, is not a signed
/// snapshot, fails any signature/authorization/freshness check, or rolls back
/// the durable checkpoint.
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
    let bytes = fetch_bounded(&url, MAX_INDEX_RESPONSE_BYTES, "snapshot")?;
    let snapshot =
        CatalogueSnapshot::from_json(&bytes).map_err(|e| RegistryError::Decode(e.to_string()))?;
    let now = tilo_clock::unix_now();
    let _ = classify_snapshot(&snapshot, &reg.owner_pubkey_hex, now)?;
    persist_checkpoint(reg, &snapshot, &bytes)?;
    Ok(snapshot)
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
    classify_snapshot(&snapshot, owner_pubkey_hex, now_unix)
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
///
/// # Errors
///
/// Returns an error if the pinned owner key is absent or a placeholder, or if
/// the snapshot fails its owner signature, publisher authorization,
/// freshness, bounds, or publisher-signature checks.
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
    if sequence < old.sequence || (sequence == old.sequence && hash != old.snapshot_hash_hex) {
        Err(RegistryError::Rollback)
    } else {
        Ok(())
    }
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
    fn the_last_accepted_catalogue_is_read_back_only_while_it_still_verifies() {
        let dir = tempfile::tempdir().unwrap();
        let checkpoint = dir.path().join("registry-checkpoints/owner.json");
        let owner = SigningKey::from_bytes(&[8; 32]);
        let owner_hex = hex::encode(owner.verifying_key().to_bytes());
        assert!(matches!(
            last_verified_at(&checkpoint, &owner_hex, 150),
            Err(RegistryError::NotStored)
        ));

        // An accepted snapshot is described offline, as fetched.
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

        // Once a newer snapshot is accepted, the older bytes are a rollback.
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

        // Bytes edited on disk fail their signatures.
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
        // Phase `only_owner_authorized_snapshot_is_offerable`.
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
        // Phase `durable_checkpoint_rejects_rollback_and_same_sequence_fork`.
        {
            let old = SnapshotCheckpoint {
                sequence: 8,
                snapshot_hash_hex: "aa".repeat(32),
            };
            assert!(matches!(
                validate_checkpoint(&old, 7, &"bb".repeat(32)),
                Err(RegistryError::Rollback)
            ));
            assert!(matches!(
                validate_checkpoint(&old, 8, &"bb".repeat(32)),
                Err(RegistryError::Rollback)
            ));
            validate_checkpoint(&old, 8, &"aa".repeat(32)).unwrap();
            validate_checkpoint(&old, 9, &"bb".repeat(32)).unwrap();
        }
        // Phase `registry_endpoint_rejects_credentials_remote_plaintext_and_redirectable_query`.
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
