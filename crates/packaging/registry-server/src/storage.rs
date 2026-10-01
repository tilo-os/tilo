//! On-disk state for a registry — one JSON file per entry, plus a
//! separate `publishers.toml` for the owner-controlled
//! allowlist.
//!
//! Layout under the data dir (`--data-dir` or
//! `$XDG_STATE_HOME/tilo-registry`):
//!
//! ```text
//! data-dir/
//!     handshake.toml       # server identity (name, description, base_url)
//!     publishers.toml      # owner-controlled allowlist
//!     entries/
//!         <urlencoded-pkg-id>.json   # one file per RegistryEntry
//!     pending/
//!         <urlencoded-pkg-id>.json   # one pending signed change per package
//!     reviews/
//!         <review-id>.json           # immutable moderator audit records
//!     quarantined/
//!         <urlencoded-pkg-id>.json   # entries removed from public visibility
//!     snapshots/
//!         <sequence>-<hash>.json     # immutable owner-signed history
//!     published-snapshot.json        # atomic current channel pointer
//!     payload-blobs/
//!         <blake3>                   # reviewed payload blobs, by content
//!     thin-packages/
//!         <package-root>.tapp        # reviewed thin tapps, by package root
//!     artifacts/
//!         <blake3>.tapp              # operator-imported full tapps, by content
//!     uploads/
//!         <blake3>.tapp              # publisher uploads awaiting review; private
//! ```
//!
//! Writes go through atomic rename (`.tmp` → target path) so a
//! crash mid-write cannot leave a torn file.
//!
//! Every call here is blocking std I/O; the HTTP layer runs them off
//! the async workers. Mutations serialize through [`SnapshotLock`].

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use std::fs::OpenOptions;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use registry_types::{
    ChannelAuthorization, EntryAction, EntryChangeRequest, ModerationEvent, RegistryEntry,
    RegistryTarget, SignedRegistrySnapshot,
};

use crate::allowlist::PublisherAllowlist;

/// Directory + on-disk file layout for one registry.
#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
}

impl Storage {
    /// Open (creating on first use) the store rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the state directories can't be created.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, StorageError> {
        let root = root.into();
        for name in [
            "entries",
            "pending",
            "publisher-applications",
            "deferred-pending",
            "reviews",
            "quarantined",
            "snapshots",
            "moderation-cases",
            "transparency",
            "moderation-requests",
            "staged-artifacts",
            "payload-blobs",
            "thin-packages",
            "artifacts",
            "uploads",
        ] {
            std::fs::create_dir_all(root.join(name))
                .map_err(|e| StorageError::Io(format!("mkdir {name}: {e}")))?;
        }
        Ok(Self { root })
    }

    /// The `handshake.toml` path.
    pub fn handshake_path(&self) -> PathBuf {
        self.root.join("handshake.toml")
    }

    /// The `publishers.toml` path.
    pub fn publishers_path(&self) -> PathBuf {
        self.root.join("publishers.toml")
    }

    /// Public snapshot served to clients. It is written only by the offline
    /// signing command, never by the HTTP serving process.
    pub fn snapshot_path(&self) -> PathBuf {
        self.root.join("published-snapshot.json")
    }

    fn snapshot_transaction_path(&self) -> PathBuf {
        self.root.join("snapshot-publish.pending.json")
    }

    #[cfg(unix)]
    fn snapshot_lock_path(&self) -> PathBuf {
        // Keep the established filename so mixed-version rolling deployments
        // still share one kernel lock while its scope expands to all registry
        // mutations.
        self.root.join("snapshot.lock")
    }

    fn promotion_transaction_path(&self) -> PathBuf {
        self.root.join("first-party-promotion.pending.json")
    }

    fn staged_promotion_path(&self) -> PathBuf {
        self.root.join("first-party-promotion.staged.json")
    }

    fn channel_authorization_path(&self) -> PathBuf {
        self.root.join("channel-authorization.json")
    }

    fn staged_artifact_path(&self, blake3_hex: &str) -> Result<PathBuf, StorageError> {
        if blake3_hex.len() != 64 || !blake3_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StorageError::Encode(
                "invalid staged artifact digest".into(),
            ));
        }
        Ok(self
            .root
            .join("staged-artifacts")
            .join(format!("{}.tapp", blake3_hex.to_ascii_lowercase())))
    }

    /// Load the active root-signed channel authorization.
    ///
    /// # Errors
    /// Returns an I/O or parse error for corrupt or unreadable state.
    pub fn load_channel_authorization(&self) -> Result<Option<ChannelAuthorization>, StorageError> {
        load_json(&self.channel_authorization_path(), "channel authorization")
    }

    /// Atomically install the active root-signed channel authorization.
    ///
    /// # Errors
    /// Returns an encoding or durable-write error.
    pub fn save_channel_authorization(
        &self,
        authorization: &ChannelAuthorization,
    ) -> Result<(), StorageError> {
        save_json(&self.channel_authorization_path(), authorization)
    }

    /// Load the collector's untrusted staged batch manifest.
    ///
    /// # Errors
    /// Returns an I/O or parse error for corrupt or unreadable state.
    pub fn load_staged_promotion(&self) -> Result<Option<StagedPromotion>, StorageError> {
        load_json(
            &self.staged_promotion_path(),
            "staged first-party promotion",
        )
    }

    /// Atomically replace the collector's staged batch manifest.
    ///
    /// # Errors
    /// Returns an encoding or durable-write error.
    pub fn save_staged_promotion(&self, staged: &StagedPromotion) -> Result<(), StorageError> {
        save_json(&self.staged_promotion_path(), staged)
    }

    /// Remove a consumed or empty staged batch manifest.
    ///
    /// # Errors
    /// Returns an I/O error if the durable deletion fails.
    pub fn delete_staged_promotion(&self) -> Result<bool, StorageError> {
        delete_if_exists(&self.staged_promotion_path())
    }

    fn payload_blob_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        is_hex_digest(blake3_hex).then(|| {
            self.root
                .join("payload-blobs")
                .join(blake3_hex.to_ascii_lowercase())
        })
    }

    fn thin_package_path(&self, root_hex: &str) -> Option<PathBuf> {
        is_hex_digest(root_hex).then(|| {
            self.root
                .join("thin-packages")
                .join(format!("{}.tapp", root_hex.to_ascii_lowercase()))
        })
    }

    fn artifact_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        is_hex_digest(blake3_hex).then(|| {
            self.root
                .join("artifacts")
                .join(format!("{}.tapp", blake3_hex.to_ascii_lowercase()))
        })
    }

    /// Keep every payload blob a full package carries, each at its BLAKE3
    /// path, and the package's thin form at its package root, so a client
    /// installing the thin form can fetch just the blobs it lacks from this
    /// registry. Only reviewed artifacts pass through here, so the registry
    /// serves no bytes it has not inspected. Bytes that do not decode as a
    /// package carry no payload to serve.
    ///
    /// # Errors
    /// Returns an error if a blob cannot be written durably.
    pub fn save_payload_blobs(&self, tapp_bytes: &[u8]) -> Result<usize, StorageError> {
        let mut blobs = std::collections::BTreeMap::new();
        let Ok(decoded) = tilo_app_package::tapp::read(tapp_bytes, &mut blobs) else {
            return Ok(0);
        };
        let count = blobs.len();
        for (hash, bytes) in blobs {
            let path = self
                .payload_blob_path(&hash.to_hex())
                .ok_or_else(|| StorageError::Encode("invalid payload blob name".into()))?;
            if !path.exists() {
                atomic_write(&path, &bytes)?;
            }
        }
        let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None)
            .map_err(|error| StorageError::Encode(format!("thin package: {error}")))?;
        let path = self
            .thin_package_path(&decoded.meta.id.to_hex())
            .ok_or_else(|| StorageError::Encode("invalid package root".into()))?;
        if !path.exists() {
            atomic_write(&path, &thin)?;
        }
        Ok(count)
    }

    /// The thin form of the reviewed package whose signed root hashes to
    /// `root_hex`, re-checked against that root, or `None` when this
    /// registry holds no such package.
    ///
    /// # Errors
    /// Returns an error when a stored package cannot be read or no longer
    /// matches its name.
    pub fn load_thin_package(&self, root_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(path) = self.thin_package_path(root_hex) else {
            return Ok(None);
        };
        let Some(bytes) = read_if_exists(&path, "thin package")? else {
            return Ok(None);
        };
        let decoded =
            tilo_app_package::tapp::read(bytes.as_slice(), &mut tilo_app_package::DiscardBlobs)
                .map_err(|error| StorageError::Parse(format!("thin package: {error}")))?;
        if !decoded.carried.is_empty() || !decoded.meta.id.to_hex().eq_ignore_ascii_case(root_hex) {
            return Err(StorageError::Parse(
                "thin package does not match its package root".into(),
            ));
        }
        Ok(Some(bytes))
    }

    /// Store a full package under its BLAKE3 so this registry can host it at
    /// `/artifacts/<blake3>.tapp`, returning the digest. Nothing is published
    /// by importing: a version still binds these exact bytes, and review
    /// still verifies them.
    ///
    /// # Errors
    /// Returns an error if the bytes are not a package or cannot be written.
    pub fn import_artifact(&self, tapp_bytes: &[u8]) -> Result<String, StorageError> {
        tilo_app_package::tapp::read(tapp_bytes, &mut tilo_app_package::DiscardBlobs)
            .map_err(|error| StorageError::Parse(format!("artifact: {error}")))?;
        let digest = blake3::hash(tapp_bytes).to_hex().to_string();
        let path = self
            .artifact_path(&digest)
            .ok_or_else(|| StorageError::Encode("invalid artifact digest".into()))?;
        if !path.exists() {
            atomic_write(&path, tapp_bytes)?;
        }
        Ok(digest)
    }

    fn upload_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        is_hex_digest(blake3_hex).then(|| {
            self.root
                .join("uploads")
                .join(format!("{}.tapp", blake3_hex.to_ascii_lowercase()))
        })
    }

    /// Keep a publisher's upload, which the caller has authenticated, under
    /// its BLAKE3. Uploads are never served; review reads them, and approval
    /// imports the ones a version binds as artifacts.
    ///
    /// # Errors
    /// Returns an error if the bytes cannot be written durably.
    pub fn save_upload(&self, tapp_bytes: &[u8]) -> Result<String, StorageError> {
        let digest = blake3::hash(tapp_bytes).to_hex().to_string();
        let path = self
            .upload_path(&digest)
            .ok_or_else(|| StorageError::Encode("invalid upload digest".into()))?;
        if !path.exists() {
            atomic_write(&path, tapp_bytes)?;
        }
        Ok(digest)
    }

    /// A publisher upload, re-checked against its digest, or `None`.
    ///
    /// # Errors
    /// Returns an error when a stored upload cannot be read or no longer
    /// matches its name.
    pub fn load_upload(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(path) = self.upload_path(blake3_hex) else {
            return Ok(None);
        };
        let Some(bytes) = read_if_exists(&path, "upload")? else {
            return Ok(None);
        };
        if !blake3::hash(&bytes)
            .to_hex()
            .eq_ignore_ascii_case(blake3_hex)
        {
            return Err(StorageError::Parse(
                "upload content does not match its name".into(),
            ));
        }
        Ok(Some(bytes))
    }

    /// An imported artifact, re-checked against its digest.
    ///
    /// # Errors
    /// Returns an error when a stored artifact cannot be read or no longer
    /// matches its name.
    pub fn load_artifact(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(path) = self.artifact_path(blake3_hex) else {
            return Ok(None);
        };
        let Some(bytes) = read_if_exists(&path, "artifact")? else {
            return Ok(None);
        };
        if !blake3::hash(&bytes)
            .to_hex()
            .eq_ignore_ascii_case(blake3_hex)
        {
            return Err(StorageError::Parse(
                "artifact content does not match its name".into(),
            ));
        }
        Ok(Some(bytes))
    }

    /// The payload blob named `blake3_hex`, re-checked against its name, or
    /// `None` when this registry holds no such blob.
    ///
    /// # Errors
    /// Returns an error when a stored blob cannot be read or no longer matches
    /// its name.
    pub fn load_payload_blob(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(path) = self.payload_blob_path(blake3_hex) else {
            return Ok(None);
        };
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(StorageError::Io(format!("read payload blob: {error}"))),
        };
        if !blake3::hash(&bytes)
            .to_hex()
            .eq_ignore_ascii_case(blake3_hex)
        {
            return Err(StorageError::Parse(
                "payload blob content does not match its name".into(),
            ));
        }
        Ok(Some(bytes))
    }

    /// Store exact package bytes at their BLAKE3-addressed path.
    ///
    /// # Errors
    /// Returns an error for an invalid digest, digest mismatch, path collision,
    /// or durable-write failure.
    pub fn save_staged_artifact(&self, blake3_hex: &str, bytes: &[u8]) -> Result<(), StorageError> {
        let actual = blake3::hash(bytes).to_hex().to_string();
        if !actual.eq_ignore_ascii_case(blake3_hex) {
            return Err(StorageError::Encode(
                "staged artifact bytes do not match digest".into(),
            ));
        }
        let path = self.staged_artifact_path(blake3_hex)?;
        if path.exists() {
            let existing = std::fs::read(&path)
                .map_err(|error| StorageError::Io(format!("read staged artifact: {error}")))?;
            if existing == bytes {
                return Ok(());
            }
            return Err(StorageError::Parse(
                "staged artifact digest path contains different bytes".into(),
            ));
        }
        atomic_write(&path, bytes)?;
        self.save_payload_blobs(bytes).map(drop)
    }

    /// Load package bytes and re-check their content-addressed path.
    ///
    /// # Errors
    /// Returns an error for an invalid digest, missing bytes, or content mismatch.
    pub fn load_staged_artifact(&self, blake3_hex: &str) -> Result<Vec<u8>, StorageError> {
        let path = self.staged_artifact_path(blake3_hex)?;
        let bytes = std::fs::read(path)
            .map_err(|error| StorageError::Io(format!("read staged artifact: {error}")))?;
        if blake3::hash(&bytes).to_hex().to_string() != blake3_hex.to_ascii_lowercase() {
            return Err(StorageError::Parse(
                "staged artifact content digest mismatch".into(),
            ));
        }
        Ok(bytes)
    }

    /// Load package bytes previously staged under `blake3_hex`, if any.
    ///
    /// # Errors
    /// Returns an error for an invalid digest, unreadable bytes, or content
    /// mismatch.
    pub fn find_staged_artifact(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        match std::fs::metadata(self.staged_artifact_path(blake3_hex)?) {
            Ok(_) => self.load_staged_artifact(blake3_hex).map(Some),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StorageError::Io(format!("stat staged artifact: {error}"))),
        }
    }

    /// Acquire the registry-wide mutation lock. Promotion selection, pending
    /// changes, moderation, and snapshot publication all use this one lock in
    /// that order. The retained lock file and kernel `flock` make this safe
    /// across `CronJobs`, the HTTP server, and CLI processes; the in-process
    /// mutex also covers independently opened [`Storage`]s.
    ///
    /// # Errors
    ///
    /// Returns an error if the process mutex is poisoned or the cross-process
    /// lock file cannot be opened or locked.
    pub fn lock_snapshot(&self) -> Result<SnapshotLock<'_>, StorageError> {
        SnapshotLock::acquire(self)
    }

    /// Load a crash-recovery record for the first-party batch promoter.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal cannot be read or decoded.
    pub fn load_promotion_transaction(&self) -> Result<Option<PromotionJournal>, StorageError> {
        load_json(
            &self.promotion_transaction_path(),
            "first-party promotion transaction",
        )
    }

    /// Persist the exact batch and candidate snapshot before applying reviews.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal cannot be encoded or written atomically.
    pub fn save_promotion_transaction(
        &self,
        journal: &PromotionJournal,
    ) -> Result<(), StorageError> {
        save_json(&self.promotion_transaction_path(), journal)
    }

    /// Remove a completed promotion transaction.
    ///
    /// # Errors
    ///
    /// Returns an error if the journal cannot be removed from durable storage.
    pub fn delete_promotion_transaction(&self) -> Result<bool, StorageError> {
        delete_if_exists(&self.promotion_transaction_path())
    }

    /// Loads the currently published owner-signed snapshot, if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot exceeds the local size bound, cannot
    /// be read, or is not valid JSON.
    pub fn load_snapshot(&self) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        let path = self.snapshot_path();
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 16_777_216)
        {
            return Err(StorageError::Parse(
                "published registry snapshot exceeds 16 MiB".into(),
            ));
        }
        load_json(&path, "published registry snapshot")
    }

    /// Atomically publishes an owner-signed snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be encoded or written.
    pub fn save_snapshot(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        let guard = self.lock_snapshot()?;
        guard.publish(snapshot)
    }

    fn archive_snapshot(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        let body =
            serde_json::to_vec_pretty(snapshot).map_err(|e| StorageError::Encode(e.to_string()))?;
        let hash = registry_types::snapshot_hash(snapshot)
            .map_err(|e| StorageError::Encode(e.to_string()))?;
        let archive = self.snapshot_history_path(snapshot.sequence, &hash)?;
        match self.load_snapshot_sequence(snapshot.sequence)? {
            Some(existing) if existing == *snapshot => return Ok(()),
            Some(_) => {
                return Err(StorageError::Parse(format!(
                    "snapshot history collision at sequence {}",
                    snapshot.sequence
                )));
            }
            None => {}
        }
        atomic_write(&archive, &body)
    }

    /// Load an immutable historical snapshot by its monotonic sequence.
    ///
    /// Exactly one archive member must match. The hash remains in the filename
    /// so operators can compare backup/object-store listings without parsing
    /// JSON, while sequence is the stable rollback handle.
    ///
    /// # Errors
    ///
    /// Returns an error if history cannot be read, contains multiple members
    /// for the sequence, or the selected member fails filename/body checks.
    pub fn load_snapshot_sequence(
        &self,
        sequence: u64,
    ) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        let prefix = format!("{sequence:020}-");
        let mut matches = Vec::new();
        for item in std::fs::read_dir(self.root.join("snapshots"))
            .map_err(|e| StorageError::Io(format!("read snapshot history: {e}")))?
        {
            let path = item
                .map_err(|e| StorageError::Io(format!("read snapshot history entry: {e}")))?
                .path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if name.starts_with(&prefix) && path.extension() == Some(std::ffi::OsStr::new("json")) {
                matches.push(path);
            }
        }
        match matches.as_slice() {
            [] => Ok(None),
            [path] => {
                let snapshot: SignedRegistrySnapshot =
                    load_json(path, "historical registry snapshot")?.ok_or_else(|| {
                        StorageError::Parse("snapshot history entry disappeared".into())
                    })?;
                if snapshot.sequence != sequence {
                    return Err(StorageError::Parse(format!(
                        "snapshot history member names sequence {sequence} but contains sequence {}",
                        snapshot.sequence
                    )));
                }
                let expected = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .and_then(|name| name.strip_prefix(&prefix))
                    .and_then(|name| name.strip_suffix(".json"))
                    .ok_or_else(|| {
                        StorageError::Parse("invalid snapshot history filename".into())
                    })?;
                let actual = registry_types::snapshot_hash(&snapshot)
                    .map_err(|e| StorageError::Parse(e.to_string()))?;
                if !actual.eq_ignore_ascii_case(expected) {
                    return Err(StorageError::Parse(format!(
                        "snapshot history digest mismatch at sequence {sequence}"
                    )));
                }
                Ok(Some(snapshot))
            }
            _ => Err(StorageError::Parse(format!(
                "multiple snapshot history entries exist for sequence {sequence}"
            ))),
        }
    }

    fn snapshot_history_path(&self, sequence: u64, hash: &str) -> Result<PathBuf, StorageError> {
        if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StorageError::Encode("invalid snapshot hash".into()));
        }
        Ok(self
            .root
            .join("snapshots")
            .join(format!("{sequence:020}-{}.json", hash.to_ascii_lowercase())))
    }

    /// Verified current moderation head for inclusion in an owner-signed
    /// snapshot. Refuses a corrupt or non-contiguous log.
    ///
    /// # Errors
    ///
    /// Returns an error if the transparency log or registry identity cannot
    /// be read, or if the log's chain, signatures, or role authorization fail.
    pub fn moderation_head(&self) -> Result<String, StorageError> {
        let mut events = self.load_transparency::<ModerationEvent>()?;
        events.sort_by_key(|event| event.sequence);
        for (index, event) in events.iter().enumerate() {
            if event.sequence != u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1) {
                return Err(StorageError::Parse(
                    "moderation log is not contiguous".into(),
                ));
            }
        }
        let head = registry_types::verify_transparency_chain(&events, "")
            .map_err(|error| StorageError::Parse(error.to_string()))?;
        let identity = self.load_identity()?;
        registry_types::verify_moderation_authorization(&events, &identity.owner_pubkey_hex)
            .map_err(|error| StorageError::Parse(error.to_string()))?;
        Ok(head)
    }

    /// Load the server-identity file. Missing → default-empty
    /// (used on first-run before `serve` writes one).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it isn't valid TOML.
    pub fn load_identity(&self) -> Result<ServerIdentity, StorageError> {
        let path = self.handshake_path();
        if !path.exists() {
            return Ok(ServerIdentity::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read handshake: {e}")))?;
        toml::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))
    }

    /// Atomic-rename write of the server-identity file.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `ident` fails to encode as
    /// TOML, or [`StorageError::Io`] if the write fails.
    pub fn save_identity(&self, ident: &ServerIdentity) -> Result<(), StorageError> {
        let path = self.handshake_path();
        let body =
            toml::to_string_pretty(ident).map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, body.as_bytes())
    }

    /// Load the publisher allowlist. Missing → empty (nobody can
    /// post).
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it isn't valid TOML.
    pub fn load_allowlist(&self) -> Result<PublisherAllowlist, StorageError> {
        let path = self.publishers_path();
        if !path.exists() {
            return Ok(PublisherAllowlist::default());
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read publishers: {e}")))?;
        PublisherAllowlist::from_toml(&text).map_err(|e| StorageError::Parse(e.to_string()))
    }

    /// Atomic-rename write of the allowlist.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `allow` fails to encode as
    /// TOML, or [`StorageError::Io`] if the write fails.
    pub fn save_allowlist(&self, allow: &PublisherAllowlist) -> Result<(), StorageError> {
        let path = self.publishers_path();
        let body = allow
            .to_toml()
            .map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, body.as_bytes())
    }

    /// One entry's file path — a filesystem-safe rendering of the
    /// package id.
    fn entry_path(&self, package_id: &str) -> PathBuf {
        self.root
            .join("entries")
            .join(format!("{}.json", filename_encode(package_id)))
    }

    /// One quarantined entry's file path.
    fn quarantined_entry_path(&self, package_id: &str) -> PathBuf {
        self.root
            .join("quarantined")
            .join(format!("{}.json", filename_encode(package_id)))
    }

    /// One pending submission's file path.
    fn pending_path(&self, submission_id: &str) -> PathBuf {
        self.root
            .join("pending")
            .join(format!("{}.json", filename_encode(submission_id)))
    }

    fn deferred_pending_path(&self, submission_id: &str) -> PathBuf {
        self.root
            .join("deferred-pending")
            .join(format!("{}.json", filename_encode(submission_id)))
    }

    /// One review record's file path.
    fn review_path(&self, review_id: &str) -> PathBuf {
        self.root
            .join("reviews")
            .join(format!("{}.json", filename_encode(review_id)))
    }

    fn moderation_case_path(&self, case_id: &str) -> PathBuf {
        self.root
            .join("moderation-cases")
            .join(format!("{}.json", filename_encode(case_id)))
    }

    fn moderation_request_path(&self, request_id: &str) -> PathBuf {
        self.root
            .join("moderation-requests")
            .join(format!("{}.json", filename_encode(request_id)))
    }

    fn transparency_path(&self, sequence: u64) -> PathBuf {
        self.root
            .join("transparency")
            .join(format!("{sequence:020}.json"))
    }

    /// Load every entry from disk. Order is not defined.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the `entries/` directory or any
    /// `.json` file in it can't be read, or [`StorageError::Parse`] if
    /// one fails to decode as a [`RegistryEntry`].
    pub fn load_entries(&self) -> Result<Vec<RegistryEntry>, StorageError> {
        let dir = self.root.join("entries");
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir).map_err(|e| StorageError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| StorageError::Io(e.to_string()))?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .map_err(|e| StorageError::Io(format!("read entry {}: {e}", path.display())))?;
            let parsed: RegistryEntry =
                serde_json::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))?;
            out.push(parsed);
        }
        Ok(out)
    }

    /// Load one entry by id, or `Ok(None)` if it isn't on disk.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be
    /// read, or [`StorageError::Parse`] if it fails to decode as a
    /// [`RegistryEntry`].
    pub fn load_entry(&self, package_id: &str) -> Result<Option<RegistryEntry>, StorageError> {
        let path = self.entry_path(package_id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)
            .map_err(|e| StorageError::Io(format!("read entry: {e}")))?;
        let parsed: RegistryEntry =
            serde_json::from_str(&text).map_err(|e| StorageError::Parse(e.to_string()))?;
        Ok(Some(parsed))
    }

    /// Atomic-rename write of one entry.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Encode`] if `entry` fails to encode as
    /// JSON, or [`StorageError::Io`] if the write fails.
    pub fn save_entry(&self, entry: &RegistryEntry) -> Result<(), StorageError> {
        let path = self.entry_path(&entry.package_id);
        let body =
            serde_json::to_vec_pretty(entry).map_err(|e| StorageError::Encode(e.to_string()))?;
        atomic_write(&path, &body)
    }

    /// Remove an entry from disk. `Ok(false)` if it wasn't there.
    ///
    /// # Errors
    ///
    /// Returns [`StorageError::Io`] if the file exists but can't be removed.
    pub fn delete_entry(&self, package_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.entry_path(package_id))
    }

    /// Load an entry that moderation has removed from public visibility.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry cannot be read or decoded.
    pub fn load_quarantined_entry(
        &self,
        package_id: &str,
    ) -> Result<Option<RegistryEntry>, StorageError> {
        load_json(
            &self.quarantined_entry_path(package_id),
            "quarantined entry",
        )
    }

    /// Atomically persist an entry outside the public index.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry cannot be encoded or persisted.
    pub fn save_quarantined_entry(&self, entry: &RegistryEntry) -> Result<(), StorageError> {
        save_json(&self.quarantined_entry_path(&entry.package_id), entry)
    }

    /// Remove an entry from quarantine after it has been restored.
    ///
    /// # Errors
    ///
    /// Returns an error when the entry exists but cannot be removed.
    pub fn delete_quarantined_entry(&self, package_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.quarantined_entry_path(package_id))
    }

    /// Load one pending publisher submission.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission cannot be read or decoded.
    pub fn load_pending(
        &self,
        submission_id: &str,
    ) -> Result<Option<PendingSubmission>, StorageError> {
        load_json(&self.pending_path(submission_id), "pending submission")
    }

    /// Load all pending submissions. Ordering is not defined.
    ///
    /// # Errors
    ///
    /// Returns an error when the queue cannot be read or decoded.
    pub fn load_pending_submissions(&self) -> Result<Vec<PendingSubmission>, StorageError> {
        load_json_dir(&self.root.join("pending"), "pending submission")
    }

    /// Atomically create or replace a pending submission.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission cannot be encoded or persisted.
    pub fn save_pending(&self, submission: &PendingSubmission) -> Result<(), StorageError> {
        save_json(&self.pending_path(&submission.submission_id), submission)
    }

    /// The publisher application from `pubkey_hex`, if any.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is not hex or the record cannot be read.
    pub fn load_publisher_application(
        &self,
        pubkey_hex: &str,
    ) -> Result<Option<crate::applications::StoredApplication>, StorageError> {
        load_json(
            &self.publisher_application_path(pubkey_hex)?,
            "publisher application",
        )
    }

    /// Every publisher application, in no particular order.
    ///
    /// # Errors
    ///
    /// Returns an error when a record cannot be read.
    pub fn load_publisher_applications(
        &self,
    ) -> Result<Vec<crate::applications::StoredApplication>, StorageError> {
        load_json_dir(
            &self.root.join("publisher-applications"),
            "publisher application",
        )
    }

    /// Atomically create or replace a publisher application.
    ///
    /// # Errors
    ///
    /// Returns an error when the record cannot be encoded or persisted.
    pub fn save_publisher_application(
        &self,
        application: &crate::applications::StoredApplication,
    ) -> Result<(), StorageError> {
        save_json(
            &self.publisher_application_path(&application.application.pubkey_hex)?,
            application,
        )
    }

    fn publisher_application_path(&self, pubkey_hex: &str) -> Result<PathBuf, StorageError> {
        if !is_hex_digest(pubkey_hex) {
            return Err(StorageError::Parse(format!(
                "{pubkey_hex:?} is not a 32-byte hex public key"
            )));
        }
        Ok(self
            .root
            .join("publisher-applications")
            .join(format!("{}.json", pubkey_hex.to_ascii_lowercase())))
    }

    /// Remove a pending submission after its terminal review record is durable.
    ///
    /// # Errors
    ///
    /// Returns an error when the submission exists but cannot be removed.
    pub fn delete_pending(&self, submission_id: &str) -> Result<bool, StorageError> {
        delete_if_exists(&self.pending_path(submission_id))
    }

    /// Load a newer revision held behind an active promotion journal.
    ///
    /// # Errors
    ///
    /// Returns an error when the deferred revision cannot be read or decoded.
    pub(crate) fn load_deferred_pending(
        &self,
        submission_id: &str,
    ) -> Result<Option<PendingSubmission>, StorageError> {
        load_json(
            &self.deferred_pending_path(submission_id),
            "deferred pending submission",
        )
    }

    /// Durably retain a newer revision without replacing a journal reservation.
    ///
    /// # Errors
    ///
    /// Returns an error when the revision cannot be encoded or persisted.
    pub(crate) fn save_deferred_pending(
        &self,
        submission: &PendingSubmission,
    ) -> Result<(), StorageError> {
        save_json(
            &self.deferred_pending_path(&submission.submission_id),
            submission,
        )
    }

    /// Remove a deferred revision after it has been restored to `pending/`.
    ///
    /// # Errors
    ///
    /// Returns an error when the deferred file exists but cannot be removed.
    pub(crate) fn delete_deferred_pending(
        &self,
        submission_id: &str,
    ) -> Result<bool, StorageError> {
        delete_if_exists(&self.deferred_pending_path(submission_id))
    }

    /// Load one durable moderator review/audit record.
    ///
    /// # Errors
    ///
    /// Returns an error when the review cannot be read or decoded.
    pub fn load_review(&self, review_id: &str) -> Result<Option<ReviewRecord>, StorageError> {
        load_json(&self.review_path(review_id), "review")
    }

    /// Load all moderator review/audit records. Ordering is not defined.
    ///
    /// # Errors
    ///
    /// Returns an error when the review log cannot be read or decoded.
    pub fn load_reviews(&self) -> Result<Vec<ReviewRecord>, StorageError> {
        load_json_dir(&self.root.join("reviews"), "review")
    }

    /// Atomically persist a review/audit record.
    ///
    /// # Errors
    ///
    /// Returns an error when the review cannot be encoded or persisted.
    pub fn save_review(&self, review: &ReviewRecord) -> Result<(), StorageError> {
        save_json(&self.review_path(&review.review_id), review)
    }

    pub(crate) fn load_moderation_case<T: serde::de::DeserializeOwned>(
        &self,
        case_id: &str,
    ) -> Result<Option<T>, StorageError> {
        load_json(&self.moderation_case_path(case_id), "moderation case")
    }

    pub(crate) fn save_moderation_case<T: Serialize>(
        &self,
        case_id: &str,
        value: &T,
    ) -> Result<(), StorageError> {
        save_json(&self.moderation_case_path(case_id), value)
    }

    pub(crate) fn load_moderation_request<T: serde::de::DeserializeOwned>(
        &self,
        request_id: &str,
    ) -> Result<Option<T>, StorageError> {
        load_json(
            &self.moderation_request_path(request_id),
            "moderation request",
        )
    }

    pub(crate) fn save_moderation_request<T: Serialize>(
        &self,
        request_id: &str,
        value: &T,
    ) -> Result<(), StorageError> {
        save_json(&self.moderation_request_path(request_id), value)
    }

    pub(crate) fn load_transparency<T: serde::de::DeserializeOwned>(
        &self,
    ) -> Result<Vec<T>, StorageError> {
        load_json_dir(&self.root.join("transparency"), "transparency record")
    }

    pub(crate) fn save_transparency<T: Serialize>(
        &self,
        sequence: u64,
        value: &T,
    ) -> Result<(), StorageError> {
        save_json(&self.transparency_path(sequence), value)
    }
}

static REGISTRY_TRANSACTION_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();

/// Exclusive registry mutation transaction. A signed snapshot is first
/// recorded as an exact durable intent, then archived, then installed as the
/// public pointer. Pending submissions and moderation use the same guard, so
/// their state cannot change between promotion selection and publication.
pub struct SnapshotLock<'a> {
    storage: &'a Storage,
    _process_mutex: std::sync::MutexGuard<'static, ()>,
    #[cfg(unix)]
    _file: advisory_lock::FileLock,
}

impl<'a> SnapshotLock<'a> {
    fn acquire(storage: &'a Storage) -> Result<Self, StorageError> {
        let process_mutex = REGISTRY_TRANSACTION_MUTEX
            .get_or_init(|| Mutex::new(()))
            .lock()
            .map_err(|_| StorageError::Io("snapshot writer lock was poisoned".into()))?;
        #[cfg(unix)]
        {
            let path = storage.snapshot_lock_path();
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|error| {
                    StorageError::Io(format!("open snapshot lock {}: {error}", path.display()))
                })?;
            let file = advisory_lock::FileLock::exclusive(file).map_err(|error| {
                StorageError::Io(format!("lock snapshot writer {}: {error}", path.display()))
            })?;
            Ok(Self {
                storage,
                _process_mutex: process_mutex,
                _file: file,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(Self {
                storage,
                _process_mutex: process_mutex,
            })
        }
    }

    /// Current published snapshot while the writer lock is held.
    ///
    /// # Errors
    ///
    /// Returns an error if the current snapshot cannot be read or decoded.
    pub fn current(&self) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        self.storage.load_snapshot()
    }

    /// Load an immutable historical member while the writer lock is held.
    ///
    /// # Errors
    ///
    /// Returns an error if history cannot be read or the member is invalid.
    pub fn sequence(&self, sequence: u64) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        self.storage.load_snapshot_sequence(sequence)
    }

    /// Archive a legacy current pointer without changing the live pointer.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot conflicts with history or cannot be
    /// written atomically.
    pub fn archive_current(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        self.storage.archive_snapshot(snapshot)
    }

    /// Exact signed transaction left by a crashed writer, if any. Callers
    /// verify its owner signature before invoking [`Self::recover`].
    ///
    /// # Errors
    ///
    /// Returns an error if the pending transaction cannot be read or decoded.
    pub fn pending_transaction(&self) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        load_json(
            &self.storage.snapshot_transaction_path(),
            "pending snapshot transaction",
        )
    }

    /// Finish the existing exact transaction after the caller authenticates it.
    ///
    /// # Errors
    ///
    /// Returns an error if recovery cannot validate or durably publish the
    /// recorded snapshot.
    pub fn recover(&self) -> Result<Option<SignedRegistrySnapshot>, StorageError> {
        let Some(snapshot) = self.pending_transaction()? else {
            return Ok(None);
        };
        self.complete(&snapshot)?;
        Ok(Some(snapshot))
    }

    /// Durably publish one already signed snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if another transaction is pending, the transition is
    /// stale, or any journal, archive, or pointer write fails.
    pub fn publish(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        if let Some(pending) = self.pending_transaction()? {
            if pending != *snapshot {
                return Err(StorageError::Parse(
                    "another snapshot transaction requires recovery".into(),
                ));
            }
        } else {
            // Refuse a stale candidate before making it durable. Leaving a
            // journal that can never be a child of the current pointer would
            // wedge every later writer.
            self.validate_transition(snapshot)?;
            let body = serde_json::to_vec_pretty(snapshot)
                .map_err(|error| StorageError::Encode(error.to_string()))?;
            atomic_write(&self.storage.snapshot_transaction_path(), &body)?;
        }
        self.complete(snapshot)
    }

    fn complete(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        self.validate_transition(snapshot)?;
        self.storage.archive_snapshot(snapshot)?;
        let body = serde_json::to_vec_pretty(snapshot)
            .map_err(|error| StorageError::Encode(error.to_string()))?;
        atomic_write(&self.storage.snapshot_path(), &body)?;
        delete_if_exists(&self.storage.snapshot_transaction_path())?;
        Ok(())
    }

    fn validate_transition(&self, snapshot: &SignedRegistrySnapshot) -> Result<(), StorageError> {
        let current = self.storage.load_snapshot()?;
        match current.as_ref() {
            Some(current) if current == snapshot => {}
            Some(current) => {
                let next = current
                    .sequence
                    .checked_add(1)
                    .ok_or_else(|| StorageError::Parse("snapshot sequence is exhausted".into()))?;
                let previous_hash = registry_types::snapshot_hash(current)
                    .map_err(|error| StorageError::Parse(error.to_string()))?;
                if snapshot.sequence != next || snapshot.previous_snapshot_hash_hex != previous_hash
                {
                    return Err(StorageError::Parse(
                        "pending snapshot is not the direct child of the published snapshot".into(),
                    ));
                }
            }
            None => {
                if snapshot.sequence != 1 || !snapshot.previous_snapshot_hash_hex.is_empty() {
                    return Err(StorageError::Parse(
                        "first snapshot transaction must be sequence 1 without a predecessor"
                            .into(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn load_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    label: &str,
) -> Result<Option<T>, StorageError> {
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| StorageError::Io(format!("read {label} {}: {e}", path.display())))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| StorageError::Parse(e.to_string()))
}

fn load_json_dir<T: serde::de::DeserializeOwned>(
    dir: &Path,
    label: &str,
) -> Result<Vec<T>, StorageError> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| StorageError::Io(e.to_string()))? {
        let path = entry.map_err(|e| StorageError::Io(e.to_string()))?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(value) = load_json(&path, label)? {
            out.push(value);
        }
    }
    Ok(out)
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), StorageError> {
    let body = serde_json::to_vec_pretty(value).map_err(|e| StorageError::Encode(e.to_string()))?;
    atomic_write(path, &body)
}

fn delete_if_exists(path: &Path) -> Result<bool, StorageError> {
    if !path.exists() {
        return Ok(false);
    }
    std::fs::remove_file(path).map_err(|e| StorageError::Io(e.to_string()))?;
    // Match `tilo_files::native::atomic`'s durability property for the transition's delete
    // side: a successful return should not resurrect this state after power
    // loss because the containing directory entry was never flushed.
    if let Some(parent) = path.parent()
        && let Ok(directory) = std::fs::File::open(parent)
    {
        let _result = directory.sync_all();
    }
    Ok(true)
}

/// A publisher request awaiting a moderator's decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSubmission {
    /// The package id. There is at most one pending revision for a package,
    /// so a newer revision can atomically replace its predecessor.
    pub submission_id: String,
    /// Time the registry accepted this pending revision.
    pub submitted_at_unix: u64,
    /// The publisher-signed request that will be re-verified at approval.
    pub change: EntryChangeRequest,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedArtifact {
    pub submission_id: String,
    pub version: String,
    /// Signed execution target for this exact artifact. Older single-target
    /// staged manifests omit it; promotion accepts those only when the
    /// version string identifies exactly one artifact in the signed entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<RegistryTarget>,
    pub blake3_hex: String,
}

/// Untrusted collector output. The networkless signer treats this only as an
/// index into content-addressed bytes and independently re-verifies every
/// request, policy constraint, digest, and tapp signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedPromotion {
    pub publisher_pubkey_hex: String,
    pub collected_at_unix: u64,
    pub submissions: Vec<PendingSubmission>,
    pub artifacts: Vec<StagedArtifact>,
}

/// Durable intent for one bounded automated promotion. The selected signed
/// requests and exact owner-signed candidate make partially applied approval
/// work distinguishable and retryable after a crash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PromotionJournal {
    pub publisher_pubkey_hex: String,
    pub reviewer: String,
    pub reason: String,
    pub submissions: Vec<PendingSubmission>,
    pub candidate_snapshot: SignedRegistrySnapshot,
}

/// The terminal effect recorded for a moderator action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Rejected,
    Quarantined,
    Restored,
}

/// What an audit record reviewed. Publisher submissions retain the complete
/// signed request; operational quarantine/restore records retain the entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReviewSubject {
    Submission(PendingSubmission),
    Entry {
        action: EntryAction,
        entry: RegistryEntry,
    },
}

/// Durable audit record for every terminal moderation decision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewRecord {
    /// Globally unique ID for this decision.
    pub review_id: String,
    /// Result of the moderation action.
    pub decision: ReviewDecision,
    /// Moderator identity, supplied by the caller.
    pub reviewer: String,
    /// Operator-supplied explanation; an empty string is allowed.
    pub reason: String,
    /// Time this terminal decision was made.
    pub reviewed_at_unix: u64,
    /// The exact data reviewed.
    pub subject: ReviewSubject,
}

/// The server's own greeting: name, description, `base_url` plus
/// the owner pubkey pinned by clients. Sits in `handshake.toml`
/// so operators can edit it with a text editor.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerIdentity {
    /// Owner Ed25519 pubkey (32 bytes, hex).
    pub owner_pubkey_hex: String,
    /// Human-readable registry name.
    pub name: String,
    /// Human-readable registry description.
    pub description: String,
    /// Public base URL the registry advertises for itself.
    pub base_url: String,
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn read_if_exists(path: &Path, what: &str) -> Result<Option<Vec<u8>>, StorageError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(StorageError::Io(format!("read {what}: {error}"))),
    }
}

/// Write bytes to `path` via a `.tmp` sibling + rename. Both the
/// tmp and the final file end up in the same directory so the
/// rename is atomic on every POSIX filesystem Tilo supports.
///
/// # Errors
///
/// Returns [`StorageError::Io`] if `path`'s parent can't be created
/// or the write/rename fails.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StorageError::Io(e.to_string()))?;
    }
    tilo_files::native::atomic::write(path, bytes).map_err(|e| StorageError::Io(e.to_string()))?;
    Ok(())
}

/// Percent-encode-ish rendering of a package id so it round-trips
/// through the filesystem on macOS + Linux + a case-preserving
/// fallback. Reverse-DNS ids ("so.tilo.editor") already only
/// contain safe characters, but we lower-case them and swap `/`
/// for `_` defensively against future publishers.
fn filename_encode(pkg_id: &str) -> String {
    let mut out = String::with_capacity(pkg_id.len());
    for ch in pkg_id.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => out.push(ch),
            _ => {
                use std::fmt::Write as _;
                let _ = write!(out, "_{:02x}", ch as u32);
            }
        }
    }
    out
}

/// Errors surfaced by disk I/O + serde encode/decode.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O: {0}")]
    Io(String),
    #[error("parse: {0}")]
    Parse(String),
    #[error("encode: {0}")]
    Encode(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use registry_types::SNAPSHOT_PROTOCOL;

    fn sample_entry(id: &str) -> RegistryEntry {
        RegistryEntry {
            package_id: id.into(),
            name: "Test".into(),
            description: "Native test app".into(),
            tagline: "Test on Tilo OS".into(),
            images: registry_types::LocalizedImages::english_icon(
                "https://apps.tilo.so/images/test-icon.svg",
                "Test icon",
            ),
            supported_languages: vec!["en".into()],
            categories: vec![],
            source_url: None,
            homepage: None,
            publisher_pubkey_hex: "aa".repeat(32),
            versions: vec![],
            updated_at_unix: 100,
            signature_hex: String::new(),
        }
    }

    fn snapshot(sequence: u64, previous: String, marker: &str) -> SignedRegistrySnapshot {
        SignedRegistrySnapshot {
            protocol: SNAPSHOT_PROTOCOL,
            sequence,
            issued_at_unix: sequence,
            expires_at_unix: sequence.checked_add(100).unwrap(),
            previous_snapshot_hash_hex: previous,
            moderation_head_hex: String::new(),
            publishers: Vec::new(),
            entries: vec![sample_entry(marker)],
            owner_signature_hex: marker.to_string(),
            channel_authorization: None,
        }
    }

    #[test]
    fn entries_and_identity_round_trip_through_the_storage_layout() {
        // phase: open_creates_layout
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            assert!(s.root.join("entries").is_dir());
            assert!(s.root.join("pending").is_dir());
            assert!(s.root.join("deferred-pending").is_dir());
            assert!(s.root.join("reviews").is_dir());
            assert!(s.root.join("quarantined").is_dir());
            assert!(s.root.join("snapshots").is_dir());
        }
        // phase: save_and_load_entry_roundtrip
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            let e = sample_entry("so.tilo.editor");
            s.save_entry(&e).unwrap();
            let back = s.load_entry("so.tilo.editor").unwrap().unwrap();
            assert_eq!(back, e);
        }
        // phase: load_entry_missing_returns_none
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            assert!(s.load_entry("nope").unwrap().is_none());
        }
        // phase: load_entries_lists_all
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            s.save_entry(&sample_entry("so.tilo.a")).unwrap();
            s.save_entry(&sample_entry("so.tilo.b")).unwrap();
            let mut ids: Vec<String> = s
                .load_entries()
                .unwrap()
                .into_iter()
                .map(|e| e.package_id)
                .collect();
            ids.sort();
            assert_eq!(ids, vec!["so.tilo.a", "so.tilo.b"]);
        }
        // phase: delete_entry_removes_file
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            s.save_entry(&sample_entry("so.tilo.a")).unwrap();
            assert!(s.delete_entry("so.tilo.a").unwrap());
            assert!(!s.delete_entry("so.tilo.a").unwrap());
            assert!(s.load_entry("so.tilo.a").unwrap().is_none());
        }
        // phase: filename_encode_escapes_unsafe_chars
        {
            assert_eq!(filename_encode("so.tilo.editor"), "so.tilo.editor");
            // A slash gets escaped so it can't escape the entries/
            // directory.
            assert_eq!(filename_encode("evil/../etc"), "evil_2f.._2fetc");
        }
        // phase: identity_roundtrip
        {
            let dir = tempfile::tempdir().unwrap();
            let s = Storage::open(dir.path()).unwrap();
            let ident = ServerIdentity {
                owner_pubkey_hex: "cc".repeat(32),
                name: "Tilo Official".into(),
                description: "The default registry".into(),
                base_url: "https://apps.tilo.so".into(),
            };
            s.save_identity(&ident).unwrap();
            let back = s.load_identity().unwrap();
            assert_eq!(back, ident);
        }
    }

    #[test]
    fn staged_packages_stay_readable_for_thin_installs() {
        // phase: staging_a_package_keeps_its_payload_blobs_for_thin_installs
        {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            let identity = tilo_app_package::SigningIdentity::generate();
            let payload = dir.path().join("payload");
            std::fs::create_dir_all(payload.join("bin")).unwrap();
            std::fs::write(payload.join("bin/editor"), b"#!/bin/true\n").unwrap();
            let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
            "[package]\nid = \"so.tilo.editor\"\nversion = \"1.0.0\"\nkind = \"app\"\n\
             [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\
             [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\
             [sandbox.net]\nmode = \"off\"\n\
             [entrypoint]\nbin = \"bin/editor\"\ntype = \"gui\"\n",
            hex::encode(identity.public_key_bytes())
        ))
        .unwrap();
            let packed = tilo_app_package::pack(&payload, &manifest, &identity).unwrap();
            let bytes = tilo_app_package::tapp::to_bytes(&packed.meta, Some(&packed)).unwrap();
            let digest = blake3::hash(&bytes).to_hex().to_string();
            storage.save_staged_artifact(&digest, &bytes).unwrap();

            let blob = tilo_app_package::BlobHash::of(b"#!/bin/true\n").to_hex();
            assert_eq!(
                storage.load_payload_blob(&blob).unwrap().as_deref(),
                Some(&b"#!/bin/true\n"[..])
            );
            assert_eq!(storage.load_payload_blob(&"0".repeat(64)).unwrap(), None);
            assert_eq!(storage.load_payload_blob("../../etc/passwd").unwrap(), None);

            let root = packed.meta.id.to_hex();
            let thin = storage.load_thin_package(&root).unwrap().unwrap();
            assert_eq!(
                thin,
                tilo_app_package::tapp::to_bytes(&packed.meta, None).unwrap()
            );
            assert_eq!(storage.load_thin_package(&"0".repeat(64)).unwrap(), None);
            assert_eq!(storage.load_thin_package("../x").unwrap(), None);
            std::fs::write(
                dir.path()
                    .join("thin-packages")
                    .join(format!("{}.tapp", "1".repeat(64))),
                &thin,
            )
            .unwrap();
            assert!(storage.load_thin_package(&"1".repeat(64)).is_err());

            let imported = storage.import_artifact(&bytes).unwrap();
            assert_eq!(imported, digest);
            assert_eq!(
                storage.load_artifact(&digest).unwrap().as_deref(),
                Some(bytes.as_slice())
            );
            assert!(storage.import_artifact(b"not a package").is_err());
            assert_eq!(storage.load_artifact("../../etc/passwd").unwrap(), None);
        }
        // phase: targetless_single_architecture_staging_remains_readable
        {
            let artifact: StagedArtifact = serde_json::from_value(serde_json::json!({
                "submission_id": "so.tilo.editor",
                "version": "1.0.0",
                "blake3_hex": "ab".repeat(32),
            }))
            .unwrap();
            assert_eq!(artifact.target, None);
        }
    }

    #[test]
    fn snapshot_chain_transactions_are_crash_safe_and_monotonic() {
        // phase: snapshot_transaction_recovers_each_crash_boundary
        {
            for crash_point in ["intent", "archive", "pointer"] {
                let dir = tempfile::tempdir().unwrap();
                let storage = Storage::open(dir.path()).unwrap();
                let candidate = snapshot(1, String::new(), crash_point);
                let body = serde_json::to_vec_pretty(&candidate).unwrap();
                atomic_write(&storage.snapshot_transaction_path(), &body).unwrap();
                if matches!(crash_point, "archive" | "pointer") {
                    storage.archive_snapshot(&candidate).unwrap();
                }
                if crash_point == "pointer" {
                    atomic_write(&storage.snapshot_path(), &body).unwrap();
                }

                let writer = storage.lock_snapshot().unwrap();
                assert_eq!(writer.recover().unwrap(), Some(candidate.clone()));
                assert_eq!(writer.current().unwrap(), Some(candidate.clone()));
                assert_eq!(writer.sequence(1).unwrap(), Some(candidate));
                drop(writer);
                assert!(!storage.snapshot_transaction_path().exists());
            }
        }
        // phase: concurrent_snapshot_writers_form_one_chain_without_sequence_forks
        {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
            let mut threads = Vec::new();
            for marker in ["so.tilo.a", "so.tilo.b"] {
                let storage = storage.clone();
                let barrier = barrier.clone();
                threads.push(std::thread::spawn(move || {
                    barrier.wait();
                    let writer = storage.lock_snapshot().unwrap();
                    let current = writer.current().unwrap();
                    let (sequence, previous) =
                        current.as_ref().map_or((1, String::new()), |head| {
                            (
                                head.sequence.checked_add(1).unwrap(),
                                registry_types::snapshot_hash(head).unwrap(),
                            )
                        });
                    writer
                        .publish(&snapshot(sequence, previous, marker))
                        .unwrap();
                }));
            }
            barrier.wait();
            for thread in threads {
                thread.join().unwrap();
            }
            let first = storage.load_snapshot_sequence(1).unwrap().unwrap();
            let second = storage.load_snapshot_sequence(2).unwrap().unwrap();
            assert_eq!(
                second.previous_snapshot_hash_hex,
                registry_types::snapshot_hash(&first).unwrap()
            );
            assert_eq!(storage.load_snapshot().unwrap(), Some(second));
        }
        // phase: history_filename_sequence_must_match_signed_body_sequence
        {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            let wrong = snapshot(2, String::new(), "wrong");
            let hash = registry_types::snapshot_hash(&wrong).unwrap();
            let path = storage.snapshot_history_path(1, &hash).unwrap();
            atomic_write(&path, &serde_json::to_vec_pretty(&wrong).unwrap()).unwrap();
            assert!(matches!(
                storage.load_snapshot_sequence(1),
                Err(StorageError::Parse(message)) if message.contains("contains sequence 2")
            ));
        }
        // phase: stale_candidate_is_rejected_without_leaving_a_transaction
        {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            let writer = storage.lock_snapshot().unwrap();
            let first = snapshot(1, String::new(), "first");
            writer.publish(&first).unwrap();
            let second = snapshot(2, registry_types::snapshot_hash(&first).unwrap(), "second");
            writer.publish(&second).unwrap();
            let stale = snapshot(2, registry_types::snapshot_hash(&first).unwrap(), "stale");
            assert!(writer.publish(&stale).is_err());
            assert!(!storage.snapshot_transaction_path().exists());
            assert_eq!(writer.current().unwrap(), Some(second));
            drop(writer);
        }
        // phase: archiving_a_stale_pointer_never_rewinds_the_live_pointer
        {
            let dir = tempfile::tempdir().unwrap();
            let storage = Storage::open(dir.path()).unwrap();
            let writer = storage.lock_snapshot().unwrap();
            let first = snapshot(1, String::new(), "first");
            writer.publish(&first).unwrap();
            let second = snapshot(2, registry_types::snapshot_hash(&first).unwrap(), "second");
            writer.publish(&second).unwrap();
            writer.archive_current(&first).unwrap();
            assert_eq!(writer.current().unwrap(), Some(second));
            drop(writer);
        }
    }
}
