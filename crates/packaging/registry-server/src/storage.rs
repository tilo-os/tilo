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
//!         <sequence>-<hash>.json     # newest owner-signed history, bounded
//!         compacted.json             # chain head of the pruned prefix
//!     published-snapshot.json        # atomic current channel pointer
//!     published-compact-snapshot.json # compact form of the current snapshot
//!     version-indexes/
//!         <blake3>.json              # complete signed entries the compact
//!                                    # form names, by content
//!     staged-artifacts/
//!         <blake3>.root              # collected tapp, rebuilt from its thin
//!                                    # form and payload blobs
//!         <blake3>.tapp              # collected tapp that does not rebuild
//!     payload-blobs/
//!         <blake3>                   # reviewed payload blobs and chunks, by
//!                                    # content
//!         <blake3>.chunks            # chunk list of a reviewed blob kept as
//!                                    # chunks (see `payload_objects`)
//!         <blake3>.zst               # compressed twin of a blob or chunk
//!                                    # (see `twins`)
//!     thin-packages/
//!         <package-root>.tapp        # reviewed thin tapps, by package root
//!     artifacts/
//!         <blake3>.tapp              # operator-imported full tapps, by content
//!         <blake3>.tapp.zst          # its compressed twin
//!     uploads/
//!         <blake3>.tapp              # publisher uploads awaiting review; private
//!         <blake3>.program           # bot programs awaiting review; private
//! ```
//!
//! Writes go through atomic rename (`.tmp` → target path) so a
//! crash mid-write cannot leave a torn file.
//!
//! Every call here is blocking std I/O; the HTTP layer runs them off
//! the async workers. Mutations serialize through [`SnapshotLock`].

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[cfg(unix)]
use std::fs::OpenOptions;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use registry_types::{
    ChannelAuthorization, CompactRegistrySnapshot, EntryAction, EntryChangeRequest,
    ModerationEvent, RegistryEntry, RegistryTarget, SignedRegistrySnapshot, VersionIndexFile,
};

use crate::allowlist::PublisherAllowlist;

mod checked;
mod payload_objects;
mod twins;
pub(crate) use checked::FileStamp;
pub use payload_objects::IncomingObject;
pub use twins::TwinReport;

/// Signed snapshots kept on disk, newest first. Older members are pruned
/// behind a [`HistoryCompaction`] record; the oldest retained member's signed
/// predecessor hash commits to everything pruned.
pub const SNAPSHOT_HISTORY_RETAINED: usize = 16;

/// The pruned prefix of the snapshot history: every sequence up to and
/// including `through_sequence` was removed, and `through_snapshot_hash_hex`
/// is that member's hash, which the next retained snapshot signs as its
/// predecessor. No sequence at or below it may be published or archived again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCompaction {
    pub through_sequence: u64,
    pub through_snapshot_hash_hex: String,
}

/// What [`SnapshotLock::compact_history`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct HistoryCompactionReport {
    /// Members removed by this run.
    pub pruned: usize,
    /// Members left on disk.
    pub retained: usize,
    /// Highest pruned sequence so far, 0 when nothing was ever pruned.
    pub through_sequence: u64,
    /// Bytes of retained members.
    pub retained_bytes: u64,
}

/// Directory + on-disk file layout for one registry.
#[derive(Clone)]
pub struct Storage {
    root: PathBuf,
    /// Served objects already checked against their names.
    checked: std::sync::Arc<checked::CheckedFiles>,
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
            "version-indexes",
        ] {
            std::fs::create_dir_all(root.join(name))
                .map_err(|e| StorageError::Io(format!("mkdir {name}: {e}")))?;
        }
        Ok(Self {
            root,
            checked: std::sync::Arc::default(),
        })
    }

    /// How many times a served object's content has been checked against
    /// its name: once per stored file, however often it is downloaded.
    pub fn object_checks(&self) -> u64 {
        self.checked.checks()
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

    /// Compact form of the public snapshot, signed with the same key.
    pub fn compact_snapshot_path(&self) -> PathBuf {
        self.root.join("published-compact-snapshot.json")
    }

    fn version_index_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        (is_hex_digest(blake3_hex) && blake3_hex == blake3_hex.to_ascii_lowercase()).then(|| {
            self.root
                .join("version-indexes")
                .join(format!("{blake3_hex}.json"))
        })
    }

    /// The compact snapshot last published, if any. It may lag the full
    /// snapshot; callers compare its `snapshot_hash_hex`.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or decoded.
    pub fn load_compact_snapshot(&self) -> Result<Option<CompactRegistrySnapshot>, StorageError> {
        let path = self.compact_snapshot_path();
        if path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 16_777_216)
        {
            return Err(StorageError::Parse(
                "published compact snapshot exceeds 16 MiB".into(),
            ));
        }
        load_json(&path, "published compact snapshot")
    }

    /// The version index named `blake3_hex`, re-checked against its name.
    ///
    /// # Errors
    /// Returns an error when a stored index cannot be read or no longer
    /// matches its name.
    pub fn load_version_index(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.read_served(
            self.version_index_path(blake3_hex),
            "version index",
            blake3_hex,
            None,
        )
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

    pub(crate) fn channel_authorization_path(&self) -> PathBuf {
        self.root.join("channel-authorization.json")
    }

    /// A whole staged package.
    fn staged_artifact_path(&self, blake3_hex: &str) -> Result<PathBuf, StorageError> {
        self.staged_path(blake3_hex, "tapp")
    }

    /// The package root of a staged package kept as blobs and a thin form.
    fn staged_root_path(&self, blake3_hex: &str) -> Result<PathBuf, StorageError> {
        self.staged_path(blake3_hex, "root")
    }

    fn staged_path(&self, blake3_hex: &str, extension: &str) -> Result<PathBuf, StorageError> {
        if !is_hex_digest(blake3_hex) {
            return Err(StorageError::Encode(
                "invalid staged artifact digest".into(),
            ));
        }
        Ok(self
            .root
            .join("staged-artifacts")
            .join(format!("{}.{extension}", blake3_hex.to_ascii_lowercase())))
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
    /// Returns an error if the registry lock cannot be taken or a blob cannot
    /// be written durably.
    pub fn save_payload_blobs(&self, tapp_bytes: &[u8]) -> Result<usize, StorageError> {
        let _lock = self.lock_snapshot()?;
        Ok(self
            .store_package(tapp_bytes)?
            .map_or(0, |stored| stored.blobs.len()))
    }

    /// Write a package's payload blobs and thin form, returning what was
    /// decoded, or `None` for bytes that are not a package. Callers hold the
    /// registry lock, so pruning never sees blobs without their thin package.
    fn store_package(&self, tapp_bytes: &[u8]) -> Result<Option<StoredPackage>, StorageError> {
        let mut blobs = std::collections::BTreeMap::new();
        let Ok(decoded) = tilo_app_package::tapp::read(tapp_bytes, &mut blobs) else {
            return Ok(None);
        };
        for (hash, bytes) in &blobs {
            let path = self
                .payload_blob_path(&hash.to_hex())
                .ok_or_else(|| StorageError::Encode("invalid payload blob name".into()))?;
            write_once(&path, bytes)?;
            self.write_payload_twin(hash, bytes)?;
        }
        let thin = tilo_app_package::tapp::to_bytes(&decoded.meta, None)
            .map_err(|error| StorageError::Encode(format!("thin package: {error}")))?;
        let path = self
            .thin_package_path(&decoded.meta.id.to_hex())
            .ok_or_else(|| StorageError::Encode("invalid package root".into()))?;
        write_once(&path, &thin)?;
        Ok(Some(StoredPackage {
            meta: decoded.meta,
            blobs,
        }))
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
        self.checked.read(
            &path,
            "thin package",
            |stamp| Ok(stamp.len()),
            |bytes| {
                let decoded =
                    tilo_app_package::tapp::read(bytes, &mut tilo_app_package::DiscardBlobs)
                        .map_err(|error| StorageError::Parse(format!("thin package: {error}")))?;
                if !decoded.carried.is_empty()
                    || !decoded.meta.id.to_hex().eq_ignore_ascii_case(root_hex)
                {
                    return Err(StorageError::Parse(
                        "thin package does not match its package root".into(),
                    ));
                }
                Ok(())
            },
        )
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
        write_once(&path, tapp_bytes)?;
        self.write_artifact_twin(&digest, tapp_bytes)?;
        Ok(digest)
    }

    fn program_upload_path(&self, blake3_hex: &str) -> Option<PathBuf> {
        is_hex_digest(blake3_hex).then(|| {
            self.root
                .join("uploads")
                .join(format!("{}.program", blake3_hex.to_ascii_lowercase()))
        })
    }

    /// Keep a publisher's bot program, which the caller authenticated and
    /// checked, under its BLAKE3. Like package uploads it stays private until
    /// review approves a version that binds it.
    ///
    /// # Errors
    /// Returns an error if the file cannot be written.
    pub fn save_program_upload(&self, bytes: &[u8]) -> Result<String, StorageError> {
        let digest = blake3::hash(bytes).to_hex().to_string();
        let path = self
            .program_upload_path(&digest)
            .ok_or_else(|| StorageError::Encode("invalid program digest".into()))?;
        write_once(&path, bytes)?;
        Ok(digest)
    }

    /// The uploaded program named `blake3_hex`, re-checked against its name.
    ///
    /// # Errors
    /// Returns an error if the file cannot be read or no longer matches its name.
    pub fn load_program_upload(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        read_named(
            self.program_upload_path(blake3_hex),
            "program upload",
            blake3_hex,
        )
    }

    /// Publish an approved program as a payload blob, served at
    /// `/blobs/<blake3>` with its compressed twin like any package blob.
    /// Holding the registry lock keeps a concurrent prune from seeing the
    /// blob before the entry that binds it.
    ///
    /// # Errors
    /// Returns an error if the lock cannot be taken or the blob or its twin
    /// cannot be written.
    pub fn save_program_blob(&self, bytes: &[u8]) -> Result<String, StorageError> {
        let _lock = self.lock_snapshot()?;
        let hash = tilo_app_package::BlobHash::of(bytes);
        let digest = hash.to_hex();
        let path = self
            .payload_blob_path(&digest)
            .ok_or_else(|| StorageError::Encode("invalid program digest".into()))?;
        write_once(&path, bytes)?;
        self.write_payload_twin(&hash, bytes)?;
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
        write_once(&path, tapp_bytes)?;
        Ok(digest)
    }

    /// A publisher upload, re-checked against its digest, or `None`.
    ///
    /// # Errors
    /// Returns an error when a stored upload cannot be read or no longer
    /// matches its name.
    pub fn load_upload(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        read_named(self.upload_path(blake3_hex), "upload", blake3_hex)
    }

    /// An imported artifact, re-checked against its digest.
    ///
    /// # Errors
    /// Returns an error when a stored artifact cannot be read or no longer
    /// matches its name.
    pub fn load_artifact(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        self.read_served(self.artifact_path(blake3_hex), "artifact", blake3_hex, None)
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
        self.read_served(
            Some(path),
            "payload blob",
            blake3_hex,
            Some(tilo_app_package::chunks::MAX_OBJECT_BYTES),
        )
    }

    /// Stage exact package bytes under their BLAKE3. A package is kept once:
    /// as its payload blobs plus its thin form, which every reviewed package
    /// keeps anyway for thin installs, with only its package root recorded
    /// here. Bytes that cannot be rebuilt exactly from those parts are kept
    /// whole.
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
        let _lock = self.lock_snapshot()?;
        if let Some(existing) = self.find_staged_artifact(blake3_hex)? {
            if existing == bytes {
                return Ok(());
            }
            return Err(StorageError::Parse(
                "staged artifact digest path contains different bytes".into(),
            ));
        }
        self.stage(blake3_hex, bytes)
    }

    /// Write staged bytes in their smallest exact form.
    fn stage(&self, blake3_hex: &str, bytes: &[u8]) -> Result<(), StorageError> {
        if self.stage_root(blake3_hex, bytes)? {
            return Ok(());
        }
        atomic_write(&self.staged_artifact_path(blake3_hex)?, bytes)
    }

    /// Record just the package root when the stored blobs and thin form
    /// rebuild `bytes` exactly. The blobs and thin package land before the
    /// root that names them.
    fn stage_root(&self, blake3_hex: &str, bytes: &[u8]) -> Result<bool, StorageError> {
        let Some(stored) = self.store_package(bytes)? else {
            return Ok(false);
        };
        // A thin package is small and kept whole; its payload is fetched
        // into the blob store separately.
        if stored
            .meta
            .payload_hashes()
            .iter()
            .any(|hash| !stored.blobs.contains_key(hash))
        {
            return Ok(false);
        }
        let rebuilt = tilo_app_package::tapp::to_bytes(&stored.meta, Some(&stored.blobs))
            .map_err(|error| StorageError::Encode(format!("rebuild package: {error}")))?;
        if rebuilt != bytes {
            return Ok(false);
        }
        let root = stored.meta.id.to_hex();
        atomic_write(&self.staged_root_path(blake3_hex)?, root.as_bytes())?;
        Ok(true)
    }

    /// Load package bytes and re-check their content-addressed path.
    ///
    /// # Errors
    /// Returns an error for an invalid digest, missing bytes, or content mismatch.
    pub fn load_staged_artifact(&self, blake3_hex: &str) -> Result<Vec<u8>, StorageError> {
        self.find_staged_artifact(blake3_hex)?
            .ok_or_else(|| StorageError::Io(format!("staged artifact {blake3_hex} is missing")))
    }

    /// Load package bytes previously staged under `blake3_hex`, if any.
    ///
    /// # Errors
    /// Returns an error for an invalid digest, unreadable bytes, or content
    /// mismatch.
    pub fn find_staged_artifact(&self, blake3_hex: &str) -> Result<Option<Vec<u8>>, StorageError> {
        // Compaction writes the root before it removes the whole package,
        // so reading in this order always finds one of them.
        let bytes = if let Some(bytes) =
            read_if_exists(&self.staged_artifact_path(blake3_hex)?, "staged artifact")?
        {
            bytes
        } else if let Some(root) =
            read_if_exists(&self.staged_root_path(blake3_hex)?, "staged package root")?
        {
            self.rebuild_staged(&root)?
        } else {
            return Ok(None);
        };
        if !blake3::hash(&bytes)
            .to_hex()
            .eq_ignore_ascii_case(blake3_hex)
        {
            return Err(StorageError::Parse(
                "staged artifact content digest mismatch".into(),
            ));
        }
        Ok(Some(bytes))
    }

    /// The full package whose thin form is stored under `root`, with every
    /// payload blob read back from this registry.
    fn rebuild_staged(&self, root: &[u8]) -> Result<Vec<u8>, StorageError> {
        let root = std::str::from_utf8(root)
            .map_err(|_| StorageError::Parse("staged package root is not text".into()))?;
        let thin = self.load_thin_package(root)?.ok_or_else(|| {
            StorageError::Parse(format!("staged package {root} has no thin package"))
        })?;
        let decoded =
            tilo_app_package::tapp::read(thin.as_slice(), &mut tilo_app_package::DiscardBlobs)
                .map_err(|error| StorageError::Parse(format!("thin package: {error}")))?;
        tilo_app_package::tapp::to_bytes(&decoded.meta, Some(&StoredBlobs(self)))
            .map_err(|error| StorageError::Parse(format!("rebuild staged package {root}: {error}")))
    }

    /// Store every whole staged package that its blobs and thin form rebuild
    /// exactly as just its package root, and return how many were
    /// compacted. Each package is converted under the registry lock on its
    /// own, so serving and publishing interleave with a long compaction.
    ///
    /// # Errors
    /// Returns an error when the staging directory or a staged package cannot
    /// be read or rewritten.
    pub fn compact_staged_artifacts(&self) -> Result<usize, StorageError> {
        let mut compacted: usize = 0;
        for digest in list_digests(&self.root.join("staged-artifacts"), ".tapp")? {
            let _lock = self.lock_snapshot()?;
            let path = self.staged_artifact_path(&digest)?;
            let Some(bytes) = read_if_exists(&path, "staged artifact")? else {
                continue;
            };
            if !blake3::hash(&bytes).to_hex().eq_ignore_ascii_case(&digest) {
                return Err(StorageError::Parse(format!(
                    "staged artifact {digest} does not match its name"
                )));
            }
            if self.stage_root(&digest, &bytes)? {
                delete_if_exists(&path)?;
                compacted = compacted.saturating_add(1);
            }
        }
        Ok(compacted)
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
        if let Some(compaction) = self.history_compaction()?
            && snapshot.sequence <= compaction.through_sequence
        {
            return Err(StorageError::Parse(format!(
                "snapshot sequence {} lies in history compacted through sequence {}",
                snapshot.sequence, compaction.through_sequence
            )));
        }
        let body = serde_json::to_vec(snapshot).map_err(|e| StorageError::Encode(e.to_string()))?;
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

    fn history_compaction_path(&self) -> PathBuf {
        self.root.join("snapshots").join("compacted.json")
    }

    /// The record of the pruned history prefix, if any was pruned.
    ///
    /// # Errors
    ///
    /// Returns an error if the record cannot be read or decoded.
    pub fn history_compaction(&self) -> Result<Option<HistoryCompaction>, StorageError> {
        load_json(
            &self.history_compaction_path(),
            "snapshot history compaction",
        )
    }

    /// Archived members by sequence, from their filenames only.
    fn history_members(&self) -> Result<BTreeMap<u64, (PathBuf, u64)>, StorageError> {
        let mut members = BTreeMap::new();
        for item in std::fs::read_dir(self.root.join("snapshots"))
            .map_err(|e| StorageError::Io(format!("read snapshot history: {e}")))?
        {
            let item =
                item.map_err(|e| StorageError::Io(format!("read snapshot history entry: {e}")))?;
            let path = item.path();
            let Some((sequence, hash)) = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".json"))
                .and_then(|name| name.split_once('-'))
            else {
                continue;
            };
            let Ok(sequence) = sequence.parse::<u64>() else {
                continue;
            };
            if !is_hex_digest(hash) {
                continue;
            }
            let len = item
                .metadata()
                .map_err(|e| StorageError::Io(format!("stat {}: {e}", path.display())))?
                .len();
            if members.insert(sequence, (path, len)).is_some() {
                return Err(StorageError::Parse(format!(
                    "multiple snapshot history entries exist for sequence {sequence}"
                )));
            }
        }
        Ok(members)
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
        load_json(&self.entry_path(package_id), "entry")
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

    /// Publish the compact form of the current snapshot: first the version
    /// indexes it names, then the pointer. Index files neither it nor the
    /// compact snapshot it replaces names are removed, so a client one
    /// snapshot behind can still fetch the indexes it was given. The caller
    /// signs `compact` with the key that signed the current snapshot.
    ///
    /// # Errors
    ///
    /// Returns an error if `compact` is not the compact form of the current
    /// snapshot, an index does not match its name, or a write fails.
    pub fn publish_compact(
        &self,
        compact: &CompactRegistrySnapshot,
        indexes: &[VersionIndexFile],
    ) -> Result<(), StorageError> {
        let current = self
            .current()?
            .ok_or_else(|| StorageError::Parse("no current snapshot to compact".into()))?;
        let current_hash = registry_types::snapshot_hash(&current)
            .map_err(|error| StorageError::Encode(error.to_string()))?;
        if compact.sequence != current.sequence || compact.snapshot_hash_hex != current_hash {
            return Err(StorageError::Parse(
                "compact snapshot is not the form of the current snapshot".into(),
            ));
        }
        let mut keep = BTreeSet::new();
        for named in compact.entries.iter().map(|entry| &entry.version_index) {
            let index = indexes
                .iter()
                .find(|index| index.blake3_hex == named.blake3_hex)
                .ok_or_else(|| {
                    StorageError::Parse("compact snapshot names a missing index".into())
                })?;
            if blake3::hash(&index.bytes).to_hex().as_str() != index.blake3_hex
                || index.bytes.len() as u64 != named.size_bytes
            {
                return Err(StorageError::Parse(
                    "version index does not match its name".into(),
                ));
            }
            let path = self
                .storage
                .version_index_path(&index.blake3_hex)
                .ok_or_else(|| StorageError::Parse("invalid version index name".into()))?;
            write_once(&path, &index.bytes)?;
            keep.insert(index.blake3_hex.clone());
        }
        if let Ok(Some(previous)) = self.storage.load_compact_snapshot() {
            keep.extend(
                previous
                    .entries
                    .into_iter()
                    .map(|entry| entry.version_index.blake3_hex),
            );
        }
        let body =
            serde_json::to_vec(compact).map_err(|error| StorageError::Encode(error.to_string()))?;
        atomic_write(&self.storage.compact_snapshot_path(), &body)?;
        let dir = self.storage.root.join("version-indexes");
        for digest in list_digests(&dir, ".json")? {
            if !keep.contains(&digest) {
                remove_file(&dir.join(format!("{digest}.json")))?;
            }
        }
        sync_dir(&dir);
        Ok(())
    }

    /// Delete the staged packages that no entry this registry holds or may
    /// still publish references, then the thin packages and payload blobs
    /// only they used. An entry counts while it is published, live,
    /// quarantined, pending, deferred, staged, journalled for promotion, in a
    /// retained history snapshot, or in a version index still served, so a
    /// client one snapshot behind can still fetch what it was offered.
    /// Holding the lock keeps every writer of staged bytes out meanwhile.
    ///
    /// # Errors
    ///
    /// Returns an error if registry state cannot be read or a file cannot be
    /// removed.
    pub fn prune_unreferenced(&self) -> Result<PruneReport, StorageError> {
        let plan = self.plan_prune(&self.referenced_entries()?)?;
        let report = plan.report();
        for path in plan
            .staged_artifacts
            .iter()
            .chain(&plan.thin_packages)
            .chain(&plan.payload_blobs)
            .chain(&plan.payload_twins)
            .map(|(path, _)| path)
        {
            remove_file(path)?;
        }
        for dir in ["staged-artifacts", "thin-packages", "payload-blobs"] {
            sync_dir(&self.storage.root.join(dir));
        }
        Ok(report)
    }

    /// What [`Self::prune_unreferenced`] would delete now, and, under
    /// `retention`, which versions publishers applying it would drop from
    /// the published entries and what pruning would then delete. Nothing is
    /// changed.
    ///
    /// # Errors
    ///
    /// Returns an error if registry state cannot be read.
    pub fn preview_prune(
        &self,
        retention: Option<&registry_types::retention::RetentionPolicy>,
    ) -> Result<PrunePreview, StorageError> {
        let referenced = self.referenced_entries()?;
        let now = self.plan_prune(&referenced)?.report();
        let Some(policy) = retention else {
            return Ok(PrunePreview {
                now,
                retention: None,
            });
        };
        let mut dropped = Vec::new();
        if let Some(current) = self.current()? {
            for entry in &current.entries {
                let kept = policy.retain(&entry.versions);
                dropped.extend(
                    entry
                        .versions
                        .iter()
                        .filter(|version| !kept.contains(version))
                        .filter_map(|version| {
                            let tapp = version.tapp()?;
                            Some(DroppedVersion {
                                package_id: entry.package_id.clone(),
                                version: version.version.clone(),
                                platform: tapp.target.platform.clone(),
                                architecture: tapp.target.architecture.clone(),
                                tapp_size_bytes: tapp.tapp_size_bytes,
                            })
                        }),
                );
            }
        }
        let retained: Vec<RegistryEntry> = referenced
            .into_iter()
            .map(|mut entry| {
                entry.versions = policy.retain(&entry.versions);
                entry
            })
            .collect();
        let after = self.plan_prune(&retained)?.report();
        Ok(PrunePreview {
            now,
            retention: Some(RetentionPreview {
                releases: policy.limit(false),
                prereleases: policy.limit(true),
                dropped_versions: dropped,
                prune: after,
            }),
        })
    }

    /// Every entry whose versions must stay downloadable.
    fn referenced_entries(&self) -> Result<Vec<RegistryEntry>, StorageError> {
        let storage = self.storage;
        let mut entries = storage.load_entries()?;
        entries.extend(load_json_dir::<RegistryEntry>(
            &storage.root.join("quarantined"),
            "quarantined entry",
        )?);
        for snapshot in [self.current()?, self.pending_transaction()?]
            .into_iter()
            .flatten()
        {
            entries.extend(snapshot.entries);
        }
        for sequence in storage.history_members()?.into_keys() {
            if let Some(snapshot) = storage.load_snapshot_sequence(sequence)? {
                entries.extend(snapshot.entries);
            }
        }
        let index_dir = storage.root.join("version-indexes");
        for digest in list_digests(&index_dir, ".json")? {
            if let Some(bytes) = storage.load_version_index(&digest)? {
                let index: registry_types::RegistryVersionIndex = serde_json::from_slice(&bytes)
                    .map_err(|error| StorageError::Parse(format!("version index: {error}")))?;
                entries.push(index.entry);
            }
        }
        let mut submissions = storage.load_pending_submissions()?;
        submissions.extend(load_json_dir::<PendingSubmission>(
            &storage.root.join("deferred-pending"),
            "deferred pending submission",
        )?);
        if let Some(staged) = storage.load_staged_promotion()? {
            submissions.extend(staged.submissions);
        }
        if let Some(journal) = storage.load_promotion_transaction()? {
            submissions.extend(journal.submissions);
            entries.extend(journal.candidate_snapshot.entries);
        }
        entries.extend(submissions.into_iter().map(|pending| pending.change.entry));
        Ok(entries)
    }

    /// The staged packages, thin packages and payload blobs that none of
    /// `entries` reaches.
    fn plan_prune(&self, entries: &[RegistryEntry]) -> Result<PrunePlan, StorageError> {
        let storage = self.storage;
        let mut digests = BTreeSet::new();
        let mut roots = BTreeSet::new();
        for tapp in entries
            .iter()
            .flat_map(|entry| &entry.versions)
            .filter_map(registry_types::RegistryVersion::tapp)
        {
            digests.insert(tapp.tapp_blake3_hex.to_ascii_lowercase());
            roots.insert(tapp.tapp_root_hash_hex.to_ascii_lowercase());
        }

        let mut plan = PrunePlan::default();
        let staged_dir = storage.root.join("staged-artifacts");
        for extension in [".tapp", ".root"] {
            for digest in list_digests(&staged_dir, extension)? {
                let path = staged_dir.join(format!("{digest}{extension}"));
                if !digests.contains(&digest) {
                    plan.staged_artifacts.push(sized(path)?);
                } else if extension == ".root"
                    && let Some(root) = read_if_exists(&path, "staged package root")?
                {
                    roots.insert(String::from_utf8_lossy(&root).to_ascii_lowercase());
                }
            }
        }
        let thin_dir = storage.root.join("thin-packages");
        let mut blobs = BTreeSet::new();
        // A bot program has no thin package; the entry binding it keeps it.
        blobs.extend(
            entries
                .iter()
                .flat_map(|entry| &entry.versions)
                .filter_map(registry_types::RegistryVersion::bot)
                .map(|bot| bot.program_blake3_hex.to_ascii_lowercase()),
        );
        for root in list_digests(&thin_dir, ".tapp")? {
            let path = thin_dir.join(format!("{root}.tapp"));
            if !roots.contains(&root) {
                plan.thin_packages.push(sized(path)?);
                continue;
            }
            let Some(thin) = read_if_exists(&path, "thin package")? else {
                continue;
            };
            let decoded =
                tilo_app_package::tapp::read(thin.as_slice(), &mut tilo_app_package::DiscardBlobs)
                    .map_err(|error| {
                        StorageError::Parse(format!("thin package {root}: {error}"))
                    })?;
            blobs.extend(
                decoded
                    .meta
                    .payload_hashes()
                    .into_iter()
                    .map(|hash| hash.to_hex()),
            );
        }
        let blob_dir = storage.root.join("payload-blobs");
        let chunks = storage.live_chunks(&blobs)?;
        for blob in list_digests(&blob_dir, "")? {
            if !blobs.contains(&blob) && !chunks.contains(&blob) {
                plan.payload_blobs.push(sized(blob_dir.join(&blob))?);
            }
        }
        // A twin goes with its object.
        for object in list_digests(&blob_dir, payload_objects::TWIN_SUFFIX)? {
            if !blobs.contains(&object) && !chunks.contains(&object) {
                plan.payload_twins.push(sized(
                    blob_dir.join(format!("{object}{}", payload_objects::TWIN_SUFFIX)),
                )?);
            }
        }
        for suffix in [
            payload_objects::CHUNKS_SUFFIX,
            payload_objects::PENDING_CHUNKS_SUFFIX,
        ] {
            for blob in list_digests(&blob_dir, suffix)? {
                if !blobs.contains(&blob) {
                    plan.payload_blobs
                        .push(sized(blob_dir.join(format!("{blob}{suffix}")))?);
                }
            }
        }
        for stale in storage.stale_incoming(std::time::Duration::from_secs(24 * 60 * 60))? {
            plan.payload_blobs.push(sized(stale)?);
        }
        Ok(plan)
    }

    /// Bound the snapshot history to the newest [`SNAPSHOT_HISTORY_RETAINED`]
    /// members. Before anything is removed, the pruned members and the oldest
    /// retained one must form one contiguous hash chain from the previous
    /// compaction record (or the first archived member), so the signed
    /// predecessor hash of the oldest retained snapshot commits to all of it.
    /// The new record is written before any file is removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the published pointer is missing or would be
    /// pruned, the chain is broken, or a file cannot be read or removed.
    pub fn compact_history(&self) -> Result<HistoryCompactionReport, StorageError> {
        let storage = self.storage;
        let previous = storage.history_compaction()?;
        let mut members = storage.history_members()?;
        let mut leftovers = 0_usize;
        if let Some(previous) = &previous {
            // Members a crashed run already committed to but did not remove.
            let stale: Vec<u64> = members
                .range(..=previous.through_sequence)
                .map(|(sequence, _)| *sequence)
                .collect();
            for sequence in stale {
                if let Some((path, _)) = members.remove(&sequence) {
                    remove_file(&path)?;
                    leftovers = leftovers.saturating_add(1);
                }
            }
        }
        let excess = members.len().saturating_sub(SNAPSHOT_HISTORY_RETAINED);
        let mut report = HistoryCompactionReport {
            pruned: leftovers,
            retained: members.len(),
            through_sequence: previous
                .as_ref()
                .map_or(0, |record| record.through_sequence),
            retained_bytes: members.values().map(|(_, len)| *len).sum(),
        };
        if excess == 0 {
            sync_dir(&storage.root.join("snapshots"));
            return Ok(report);
        }
        let current = self.current()?.ok_or_else(|| {
            StorageError::Parse("refusing to compact history without a published snapshot".into())
        })?;
        let pruned: Vec<u64> = members.keys().take(excess).copied().collect();
        let through_sequence = pruned.last().copied().unwrap_or_default();
        if current.sequence <= through_sequence {
            return Err(StorageError::Parse(
                "refusing to compact history past the published snapshot".into(),
            ));
        }
        let oldest_retained = members
            .keys()
            .nth(excess)
            .copied()
            .ok_or_else(|| StorageError::Parse("snapshot history vanished".into()))?;
        let mut link = previous.as_ref().map(|record| {
            (
                record.through_sequence,
                record.through_snapshot_hash_hex.clone(),
            )
        });
        for sequence in pruned.iter().copied().chain([oldest_retained]) {
            let member = storage.load_snapshot_sequence(sequence)?.ok_or_else(|| {
                StorageError::Parse(format!("snapshot history member {sequence} vanished"))
            })?;
            let linked = match &link {
                Some((before, hash)) => {
                    before.checked_add(1) == Some(sequence)
                        && member.previous_snapshot_hash_hex == *hash
                }
                None => sequence != 1 || member.previous_snapshot_hash_hex.is_empty(),
            };
            if !linked {
                return Err(StorageError::Parse(format!(
                    "snapshot history chain is broken at sequence {sequence}; refusing to compact"
                )));
            }
            if sequence == oldest_retained {
                break;
            }
            let hash = registry_types::snapshot_hash(&member)
                .map_err(|error| StorageError::Parse(error.to_string()))?;
            link = Some((sequence, hash));
        }
        let through_snapshot_hash_hex = link.map(|(_, hash)| hash).unwrap_or_default();
        save_json(
            &storage.history_compaction_path(),
            &HistoryCompaction {
                through_sequence,
                through_snapshot_hash_hex,
            },
        )?;
        for sequence in &pruned {
            if let Some((path, len)) = members.remove(sequence) {
                remove_file(&path)?;
                report.retained_bytes = report.retained_bytes.saturating_sub(len);
            }
        }
        sync_dir(&storage.root.join("snapshots"));
        report.pruned = report.pruned.saturating_add(pruned.len());
        report.retained = members.len();
        report.through_sequence = through_sequence;
        tracing::info!(
            pruned = report.pruned,
            retained = report.retained,
            through_sequence,
            retained_bytes = report.retained_bytes,
            "compacted snapshot history"
        );
        Ok(report)
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
                if let Some(compaction) = self.storage.history_compaction()? {
                    return Err(StorageError::Parse(format!(
                        "published snapshot is missing but history was compacted through \
                         sequence {}; restore published-snapshot.json instead of restarting \
                         the channel",
                        compaction.through_sequence
                    )));
                }
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

/// Names in `dir` that are a lowercase BLAKE3 hex digest followed by
/// `extension`, as the digests. Temporary and foreign files are skipped.
fn list_digests(dir: &Path, extension: &str) -> Result<Vec<String>, StorageError> {
    let mut digests = Vec::new();
    for entry in std::fs::read_dir(dir)
        .map_err(|error| StorageError::Io(format!("list {}: {error}", dir.display())))?
    {
        let entry = entry.map_err(|error| StorageError::Io(error.to_string()))?;
        let name = entry.file_name();
        let Some(digest) = name.to_str().and_then(|name| name.strip_suffix(extension)) else {
            continue;
        };
        if is_hex_digest(digest) && digest == digest.to_ascii_lowercase() {
            digests.push(digest.to_owned());
        }
    }
    Ok(digests)
}

fn sync_dir(dir: &Path) {
    if let Ok(directory) = std::fs::File::open(dir) {
        let _result = directory.sync_all();
    }
}

fn remove_file(path: &Path) -> Result<(), StorageError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(StorageError::Io(format!(
            "remove {}: {error}",
            path.display()
        ))),
    }
}

/// A package written as payload blobs and a thin form.
struct StoredPackage {
    meta: tilo_app_package::PackageMeta,
    blobs: BTreeMap<tilo_app_package::BlobHash, Vec<u8>>,
}

/// The registry's payload blobs as a source for rebuilding full packages.
/// Readers verify every blob against its hash.
struct StoredBlobs<'a>(&'a Storage);

impl tilo_app_package::BlobSource for StoredBlobs<'_> {
    fn open_blob(
        &self,
        hash: &tilo_app_package::BlobHash,
    ) -> std::io::Result<Option<tilo_app_package::OpenBlob<'_>>> {
        Ok(self
            .0
            .open_payload_stream(hash)?
            .map(|(len, reader)| tilo_app_package::OpenBlob { len, reader }))
    }
}

/// What [`SnapshotLock::prune_unreferenced`] removed, or would remove.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct PruneReport {
    pub staged_artifacts: usize,
    pub thin_packages: usize,
    pub payload_blobs: usize,
    /// Compressed twins of the payload blobs removed.
    pub payload_twins: usize,
    /// Apparent size of everything removed.
    pub bytes: u64,
}

/// Files pruning removes, with their sizes.
#[derive(Default)]
struct PrunePlan {
    staged_artifacts: Vec<(PathBuf, u64)>,
    thin_packages: Vec<(PathBuf, u64)>,
    payload_blobs: Vec<(PathBuf, u64)>,
    payload_twins: Vec<(PathBuf, u64)>,
}

impl PrunePlan {
    fn report(&self) -> PruneReport {
        let all = || {
            self.staged_artifacts
                .iter()
                .chain(&self.thin_packages)
                .chain(&self.payload_blobs)
                .chain(&self.payload_twins)
        };
        PruneReport {
            staged_artifacts: self.staged_artifacts.len(),
            thin_packages: self.thin_packages.len(),
            payload_blobs: self.payload_blobs.len(),
            payload_twins: self.payload_twins.len(),
            bytes: all().map(|(_, len)| *len).sum(),
        }
    }
}

fn sized(path: PathBuf) -> Result<(PathBuf, u64), StorageError> {
    match std::fs::metadata(&path) {
        Ok(metadata) => Ok((path, metadata.len())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok((path, 0)),
        Err(error) => Err(StorageError::Io(format!(
            "stat {}: {error}",
            path.display()
        ))),
    }
}

/// What [`SnapshotLock::preview_prune`] found.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PrunePreview {
    /// What pruning would delete now.
    pub now: PruneReport,
    /// What a retention policy would drop, when one was given.
    pub retention: Option<RetentionPreview>,
}

/// The effect of a retention policy on the published entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RetentionPreview {
    /// Versions kept per target among releases and pre-releases.
    pub releases: usize,
    pub prereleases: usize,
    /// Published versions the policy drops.
    pub dropped_versions: Vec<DroppedVersion>,
    /// What pruning deletes once no snapshot or index still lists them.
    pub prune: PruneReport,
}

/// One published version a retention policy drops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DroppedVersion {
    pub package_id: String,
    pub version: String,
    pub platform: String,
    pub architecture: String,
    pub tapp_size_bytes: u64,
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

impl PendingSubmission {
    /// Identify the exact queued request an operator inspected, including
    /// both signatures and the receipt timestamp. This is a snapshot id,
    /// not an authorization token. Localized maps serialize in sorted order.
    ///
    /// # Errors
    /// Returns an error if the stored request cannot be serialized.
    pub fn review_id(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| blake3::hash(&bytes).to_hex().to_string())
    }
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
    /// Public base URL the registry advertises for itself. Moderation v2
    /// destinations must match this exact configured string, including any path
    /// or trailing slash; proxy headers and the listen address do not override it.
    pub base_url: String,
}

fn is_hex_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Write content-addressed `bytes` to `path` unless they are already there:
/// a file named by its digest never changes.
fn write_once(path: &Path, bytes: &[u8]) -> Result<(), StorageError> {
    if path.exists() {
        return Ok(());
    }
    atomic_write(path, bytes)
}

/// The bytes at `path`, which is named by their BLAKE3 `blake3_hex`, or
/// `None` without a valid name or a file. Stored bytes that no longer match
/// their name are an error, never served.
impl Storage {
    /// [`read_named`] for objects served to clients: the name check runs once
    /// per stored file rather than on every request. `max_len` refuses files
    /// larger than that before reading them.
    fn read_served(
        &self,
        path: Option<PathBuf>,
        what: &str,
        blake3_hex: &str,
        max_len: Option<u64>,
    ) -> Result<Option<Vec<u8>>, StorageError> {
        let Some(path) = path else {
            return Ok(None);
        };
        self.checked.read(
            &path,
            what,
            |stamp| match max_len {
                Some(max) if stamp.len() > max => Err(StorageError::Parse(format!(
                    "{what} exceeds the object size limit"
                ))),
                _ => Ok(stamp.len()),
            },
            |bytes| {
                if blake3::hash(bytes)
                    .to_hex()
                    .eq_ignore_ascii_case(blake3_hex)
                {
                    Ok(())
                } else {
                    Err(StorageError::Parse(format!(
                        "{what} content does not match its name"
                    )))
                }
            },
        )
    }
}

fn read_named(
    path: Option<PathBuf>,
    what: &str,
    blake3_hex: &str,
) -> Result<Option<Vec<u8>>, StorageError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(bytes) = read_if_exists(&path, what)? else {
        return Ok(None);
    };
    if !blake3::hash(&bytes)
        .to_hex()
        .eq_ignore_ascii_case(blake3_hex)
    {
        return Err(StorageError::Parse(format!(
            "{what} content does not match its name"
        )));
    }
    Ok(Some(bytes))
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
    use registry_types::PACKAGE_FORMAT_VERSION;

    fn sample_entry(id: &str) -> RegistryEntry {
        RegistryEntry {
            package_id: id.into(),
            kind: registry_types::EntryKind::App,
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

    /// A payload blob and its compressed twin are checked against their
    /// name the first time they are served, not on every download, and a
    /// missing one costs no check at all.
    #[test]
    fn served_objects_are_checked_once_per_stored_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut storage = Storage::open(dir.path()).unwrap();
        storage.checked = std::sync::Arc::new(checked::CheckedFiles::settling_for(
            std::time::Duration::ZERO,
        ));
        let bytes = b"a reviewed payload blob".repeat(64);
        let hash = tilo_app_package::BlobHash::of(&bytes);
        atomic_write(&storage.payload_dir().join(hash.to_hex()), &bytes).unwrap();
        storage.write_payload_twin(&hash, &bytes).unwrap();

        let missing = tilo_app_package::BlobHash::of(b"never stored").to_hex();
        assert_eq!(storage.load_payload_blob(&missing).unwrap(), None);
        assert_eq!(storage.load_payload_twin(&missing).unwrap(), None);
        assert_eq!(storage.object_checks(), 0);

        for _ in 0..5 {
            assert_eq!(
                storage
                    .load_payload_blob(&hash.to_hex())
                    .unwrap()
                    .as_deref(),
                Some(bytes.as_slice())
            );
            assert!(storage.load_payload_twin(&hash.to_hex()).unwrap().is_some());
        }
        assert_eq!(
            storage.object_checks(),
            2,
            "one check each for the blob and its twin"
        );
    }

    fn snapshot(sequence: u64, previous: String, marker: &str) -> SignedRegistrySnapshot {
        SignedRegistrySnapshot {
            protocol: PACKAGE_FORMAT_VERSION,
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
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
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

            // phase: whole_staged_packages_compact_to_their_root
            let staged = dir.path().join("staged-artifacts");
            let root_file = staged.join(format!("{digest}.root"));
            assert_eq!(std::fs::read_to_string(&root_file).unwrap(), root);
            std::fs::remove_file(&root_file).unwrap();
            std::fs::write(staged.join(format!("{digest}.tapp")), &bytes).unwrap();
            let opaque = b"bytes that are no package".to_vec();
            let opaque_digest = blake3::hash(&opaque).to_hex().to_string();
            storage
                .save_staged_artifact(&opaque_digest, &opaque)
                .unwrap();
            assert_eq!(storage.compact_staged_artifacts().unwrap(), 1);
            assert!(root_file.exists());
            assert!(!staged.join(format!("{digest}.tapp")).exists());
            assert_eq!(storage.load_staged_artifact(&digest).unwrap(), bytes);
            assert_eq!(
                storage.load_staged_artifact(&opaque_digest).unwrap(),
                opaque
            );
            storage.save_staged_artifact(&digest, &bytes).unwrap();
            assert!(storage.save_staged_artifact(&digest, &opaque).is_err());

            // phase: a_staged_package_missing_a_blob_fails_closed
            let blob_file = dir.path().join("payload-blobs").join(&blob);
            let aside = dir.path().join("blob-aside");
            std::fs::rename(&blob_file, &aside).unwrap();
            assert!(storage.load_staged_artifact(&digest).is_err());
            std::fs::rename(&aside, &blob_file).unwrap();

            // phase: unreferenced_staged_packages_are_pruned_with_their_parts
            let report = storage
                .lock_snapshot()
                .unwrap()
                .prune_unreferenced()
                .unwrap();
            assert_eq!(
                (
                    report.staged_artifacts,
                    report.thin_packages,
                    report.payload_blobs,
                    report.payload_twins
                ),
                (2, 2, 1, 1)
            );
            assert!(report.bytes > 0);
            assert_eq!(storage.find_staged_artifact(&digest).unwrap(), None);
            assert_eq!(storage.load_payload_blob(&blob).unwrap(), None);
            assert!(
                !dir.path()
                    .join("payload-blobs")
                    .join(format!("{blob}.zst"))
                    .exists(),
                "a twin goes with its blob"
            );
            assert_eq!(storage.load_thin_package(&root).unwrap(), None);
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

    #[test]
    fn compaction_keeps_a_bounded_linked_suffix_and_refuses_broken_chains() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Storage::open(dir.path()).unwrap();
        let writer = storage.lock_snapshot().unwrap();
        let mut chain = Vec::new();
        let mut previous = String::new();
        for sequence in 1..=20_u64 {
            let next = snapshot(sequence, previous, "so.tilo.chain");
            previous = registry_types::snapshot_hash(&next).unwrap();
            chain.push(next);
        }
        // A history whose links do not match is never pruned.
        for member in &chain {
            let mut member = member.clone();
            if member.sequence == 3 {
                member.previous_snapshot_hash_hex = "ee".repeat(32);
            }
            storage.archive_snapshot(&member).unwrap();
        }
        atomic_write(
            &storage.snapshot_path(),
            &serde_json::to_vec(&chain[19]).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            writer.compact_history(),
            Err(StorageError::Parse(message)) if message.contains("broken at sequence 3")
        ));
        assert_eq!(storage.history_members().unwrap().len(), 20);
        assert!(storage.history_compaction().unwrap().is_none());

        // A linked history keeps exactly the newest members.
        for entry in std::fs::read_dir(storage.root.join("snapshots")).unwrap() {
            std::fs::remove_file(entry.unwrap().path()).unwrap();
        }
        for member in &chain {
            storage.archive_snapshot(member).unwrap();
        }
        let report = writer.compact_history().unwrap();
        assert_eq!(report.pruned, 20 - SNAPSHOT_HISTORY_RETAINED);
        assert_eq!(report.retained, SNAPSHOT_HISTORY_RETAINED);
        let through = u64::try_from(20 - SNAPSHOT_HISTORY_RETAINED).unwrap();
        assert_eq!(report.through_sequence, through);
        let record = storage.history_compaction().unwrap().unwrap();
        let pruned_head = &chain[usize::try_from(through - 1).unwrap()];
        assert_eq!(
            record.through_snapshot_hash_hex,
            registry_types::snapshot_hash(pruned_head).unwrap()
        );
        assert_eq!(
            record.through_snapshot_hash_hex,
            chain[usize::try_from(through).unwrap()].previous_snapshot_hash_hex
        );
        assert!(storage.load_snapshot_sequence(through).unwrap().is_none());
        assert!(
            storage
                .load_snapshot_sequence(through + 1)
                .unwrap()
                .is_some()
        );
        // A pruned sequence can never be archived again.
        assert!(storage.archive_snapshot(&chain[0]).is_err());

        // A run that crashed after writing its record finishes removal.
        let stale = &chain[usize::try_from(through - 1).unwrap()];
        let hash = registry_types::snapshot_hash(stale).unwrap();
        atomic_write(
            &storage
                .snapshot_history_path(stale.sequence, &hash)
                .unwrap(),
            &serde_json::to_vec(stale).unwrap(),
        )
        .unwrap();
        let report = writer.compact_history().unwrap();
        assert_eq!(report.pruned, 1);
        assert_eq!(report.retained, SNAPSHOT_HISTORY_RETAINED);

        // Without the pointer the channel cannot restart at sequence 1.
        std::fs::remove_file(storage.snapshot_path()).unwrap();
        assert!(matches!(
            writer.publish(&snapshot(1, String::new(), "so.tilo.restart")),
            Err(StorageError::Parse(message)) if message.contains("compacted through")
        ));
        drop(writer);
    }
}
