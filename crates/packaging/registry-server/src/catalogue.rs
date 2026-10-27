//! The published catalogue as `/entries` serves it: the owner-signed
//! snapshot and its compact form, read, verified and encoded once per
//! published revision rather than once per request.
//!
//! Verifying a snapshot checks the owner's signature and every entry's
//! publisher signature, and the catalogue is a megabyte of JSON; doing that,
//! hashing it and encoding it again for each of thousands of clients
//! revalidating the same bytes made every `/entries` request seconds of CPU
//! on the threads every other request also waits for. The snapshot changes
//! only when the offline signing command publishes one, which it does by
//! renaming a new file over the old, so the stamps of the three files the
//! answer depends on (snapshot, compact form, channel authorization) name a
//! revision exactly. Each request compares stamps; only a new revision is
//! read and verified again, once, on the blocking pool, while requests for
//! it wait for that one load instead of repeating it.
//!
//! Signatures and shape are verified once. Freshness is the one part that
//! depends on the clock, and it is the client's to judge: an authentic
//! snapshot is served after it expires too, so a device can tell an expired
//! catalogue from an unreachable one (and say so), and `/metrics` reports
//! both expiries for the alerts that ask for a renewal in time.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use registry_types::{ChannelAuthorization, CompactRegistrySnapshot, SignedRegistrySnapshot};

use crate::storage::{FileStamp, Storage, StorageError};

/// Which files the published catalogue was read from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Revision {
    snapshot: Option<FileStamp>,
    compact: Option<FileStamp>,
    channel: Option<FileStamp>,
}

impl Revision {
    /// The revision on disk now.
    ///
    /// # Errors
    /// Returns an error when a file exists but cannot be inspected.
    pub(crate) fn of(storage: &Storage) -> Result<Self, StorageError> {
        Ok(Self {
            snapshot: FileStamp::at(&storage.snapshot_path())?,
            compact: FileStamp::at(&storage.compact_snapshot_path())?,
            channel: FileStamp::at(&storage.channel_authorization_path())?,
        })
    }
}

/// One form of the snapshot, encoded for the wire.
#[derive(Debug)]
pub(crate) struct Encoded {
    /// The JSON body, shared by every response.
    pub(crate) body: Bytes,
    /// Its hash, the response's `ETag`.
    pub(crate) etag: Option<String>,
    expires_at_unix: u64,
}

impl Encoded {
    fn new<T: serde::Serialize>(
        value: &T,
        etag: Option<String>,
        expires_at_unix: u64,
    ) -> Result<Self, StorageError> {
        let body =
            serde_json::to_vec(value).map_err(|error| StorageError::Encode(error.to_string()))?;
        Ok(Self {
            body: Bytes::from(body),
            etag,
            expires_at_unix,
        })
    }

    /// When the signer said this form stops being valid (Unix seconds).
    pub(crate) fn expires_at_unix(&self) -> u64 {
        self.expires_at_unix
    }
}

/// The owner's authorization of the online signing channel, as far as
/// knowing when it must be renewed goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ChannelTerm {
    pub(crate) generation: u64,
    pub(crate) expires_at_unix: u64,
}

/// The full snapshot as this server may serve it.
#[derive(Debug)]
pub(crate) enum FullSnapshot {
    /// Nothing has been published.
    Missing,
    /// What was published does not verify against the owner's key or the
    /// active channel.
    Refused,
    /// A verified snapshot and the packages it lists.
    Verified {
        encoded: Encoded,
        packages: HashSet<String>,
    },
}

#[derive(Debug)]
pub(crate) struct Published {
    revision: Revision,
    pub(crate) full: FullSnapshot,
    /// The compact form, present only while it verifies and is the form of
    /// the full snapshot.
    pub(crate) compact: Option<Encoded>,
    /// The active channel authorization's term, whatever the snapshot's state:
    /// it is what has to be renewed before anything can be signed again.
    pub(crate) channel: Option<ChannelTerm>,
}

impl Published {
    /// Read and verify the revision on disk.
    fn load(storage: &Storage, owner_pubkey_hex: &str) -> Result<Self, StorageError> {
        // Stamped before reading: a file replaced in between is simply
        // loaded again by the next request, which sees a newer stamp.
        let revision = Revision::of(storage)?;
        // An unreadable authorization activates no channel, as a missing one.
        let channel = storage.load_channel_authorization().ok().flatten();
        let term = channel.as_ref().map(|authorization| ChannelTerm {
            generation: authorization.generation,
            expires_at_unix: authorization.expires_at_unix,
        });
        let Some(snapshot) = storage.load_snapshot()? else {
            return Ok(Self {
                revision,
                full: FullSnapshot::Missing,
                compact: None,
                channel: term,
            });
        };
        let Some(snapshot_hash) = verified_snapshot(&snapshot, owner_pubkey_hex, channel.as_ref())
        else {
            return Ok(Self {
                revision,
                full: FullSnapshot::Refused,
                compact: None,
                channel: term,
            });
        };
        let compact = match storage.load_compact_snapshot()? {
            Some(compact)
                if verified_compact(
                    &compact,
                    owner_pubkey_hex,
                    channel.as_ref(),
                    &snapshot_hash,
                ) =>
            {
                Some(Encoded::new(
                    &compact,
                    registry_types::compact_snapshot_hash(&compact).ok(),
                    compact.expires_at_unix,
                )?)
            }
            _ => None,
        };
        let packages = snapshot
            .entries
            .iter()
            .map(|entry| entry.package_id.clone())
            .collect();
        Ok(Self {
            revision,
            full: FullSnapshot::Verified {
                encoded: Encoded::new(&snapshot, Some(snapshot_hash), snapshot.expires_at_unix)?,
                packages,
            },
            compact,
            channel: term,
        })
    }
}

/// The snapshot's hash when it verifies against the owner's key and the
/// active channel, apart from freshness. Verifying at its own issue time
/// passes every time-dependent check, so only signatures, shape and bounds
/// decide; freshness is checked per request.
fn verified_snapshot(
    snapshot: &SignedRegistrySnapshot,
    owner_pubkey_hex: &str,
    channel: Option<&ChannelAuthorization>,
) -> Option<String> {
    registry_types::verify_snapshot(snapshot, owner_pubkey_hex, snapshot.issued_at_unix).ok()?;
    channel_is_active(snapshot.channel_authorization.as_ref(), channel)
        .then(|| registry_types::snapshot_hash(snapshot).ok())
        .flatten()
}

fn verified_compact(
    compact: &CompactRegistrySnapshot,
    owner_pubkey_hex: &str,
    channel: Option<&ChannelAuthorization>,
    snapshot_hash: &str,
) -> bool {
    registry_types::verify_compact_snapshot(compact, owner_pubkey_hex, compact.issued_at_unix)
        .is_ok()
        && compact.snapshot_hash_hex == snapshot_hash
        && channel_is_active(compact.channel_authorization.as_ref(), channel)
}

/// A snapshot signed under a channel is served only while that channel is
/// the active one.
fn channel_is_active(
    signed_under: Option<&ChannelAuthorization>,
    active: Option<&ChannelAuthorization>,
) -> bool {
    signed_under.is_none_or(|signed_under| active == Some(signed_under))
}

/// The catalogue of the latest revision a request has seen.
#[derive(Default)]
pub(crate) struct Catalogue {
    current: Mutex<Option<Arc<Published>>>,
    /// Held while a revision loads, so concurrent requests wait for one load.
    loading: tokio::sync::Mutex<()>,
    loads: AtomicU64,
}

impl Catalogue {
    /// The catalogue of the revision on disk.
    ///
    /// # Errors
    /// Returns an error when the published files cannot be read or the
    /// blocking pool is gone.
    pub(crate) async fn current(
        &self,
        storage: &Storage,
        owner_pubkey_hex: &str,
    ) -> Result<Arc<Published>, StorageError> {
        let revision = {
            let storage = storage.clone();
            tilo_rt::io::run(move || Revision::of(&storage).map_err(std::io::Error::other))
                .await
                .map_err(|error| StorageError::Io(error.to_string()))?
        };
        if let Some(published) = self.cached(&revision) {
            return Ok(published);
        }
        let _turn = self.loading.lock().await;
        if let Some(published) = self.cached(&revision) {
            return Ok(published);
        }
        let storage = storage.clone();
        let owner_pubkey_hex = owner_pubkey_hex.to_owned();
        let published = Arc::new(
            tokio::task::spawn_blocking(move || Published::load(&storage, &owner_pubkey_hex))
                .await
                .map_err(|error| StorageError::Io(format!("load the catalogue: {error}")))??,
        );
        self.loads.fetch_add(1, Ordering::Relaxed);
        *self.lock() = Some(published.clone());
        Ok(published)
    }

    /// How many times a revision has been read and verified.
    pub(crate) fn loads(&self) -> u64 {
        self.loads.load(Ordering::Relaxed)
    }

    fn cached(&self, revision: &Revision) -> Option<Arc<Published>> {
        self.lock()
            .as_ref()
            .filter(|published| published.revision == *revision)
            .cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Arc<Published>>> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
