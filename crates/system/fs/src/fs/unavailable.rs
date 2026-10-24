//! A blocked read of bytes a peer must supply, noted on the file's record.
//!
//! Such a read fails with `EIO` at [`crate::hydration::BLOCKED_READ_BOUND`].
//! The reader then knows only that it failed; everyone else would still see
//! the file as merely not stored here. So the failure is written on the
//! file's availability record, where Files and the apps read it, and Files
//! shows the file as unavailable until a device that keeps it is back. The
//! fetch outlives the read, and the note is cleared as soon as bytes arrive.
//!
//! Bytes that arrive can also be the last ones the file was missing. Then
//! the record says the file is stored here, as a pin's download does when
//! it ends, so a file read whole is shown stored and its folder's watchers
//! hear of it (`crate::change_signal`). Every write rewrites the whole
//! registry and wakes the pinned-hydration pass, so files found whole are
//! written together a moment later ([`StoredWrites`]), off the read's
//! reply: a folder read through costs one write, not one per file.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use fs_protocol::{FileRecordId, FileVersionId, NO_PEER_DELIVERED_ERROR, Residency, TransferPhase};
use iroh_blobs::Hash;
use tilo_lock::MutexRecover as _;

use crate::availability::{AvailabilityRegistry, RecordAvailability, RecordKind, TransferError};
use crate::hydration::Hydrator;

/// The record a blocked read notes its outcome on.
#[derive(Clone)]
pub(super) struct UnavailableNote {
    pub(super) registry: Arc<Mutex<AvailabilityRegistry>>,
    pub(super) record_id: FileRecordId,
    /// What the read fetches, when it is the file's committed content.
    pub(super) content: Option<NotedContent>,
}

/// The committed content a read fetches: the version the file had when the
/// read began, and where its bytes are.
#[derive(Clone)]
pub(super) struct NotedContent {
    pub(super) hydrator: Arc<Hydrator>,
    pub(super) hash: Hash,
    pub(super) version: FileVersionId,
    /// Where a file found whole waits to be written with the others.
    pub(super) stored: Arc<StoredWrites>,
}

/// How long a file found whole waits for others before they are written
/// together: short enough that Files shows it stored at once, long enough
/// that a folder read through (a copy, a search, thumbnails) is one write.
const STORED_SETTLE: Duration = Duration::from_millis(200);

/// Files reads found whole, written to the registry together. Each write
/// of the registry rewrites all of it durably and wakes the
/// pinned-hydration pass over the whole namespace, so one per file would
/// make reading a folder cost a rewrite per file.
pub(super) struct StoredWrites {
    state: Mutex<StoredState>,
    /// [`STORED_SETTLE`], but for tests that need a burst to fit for sure.
    settle: Duration,
    /// Ends the wait early: a test writes when its burst is in.
    write_now: tokio::sync::Notify,
}

impl Default for StoredWrites {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            settle: STORED_SETTLE,
            write_now: tokio::sync::Notify::new(),
        }
    }
}

#[derive(Default)]
struct StoredState {
    /// Found whole since the last write; `Some` while a write is scheduled.
    pending: Option<Vec<FoundWhole>>,
    /// Writes taken off `pending` and not yet committed.
    writing: usize,
}

/// A file a read found whole: its record, and the version and content read.
struct FoundWhole {
    record_id: FileRecordId,
    version: FileVersionId,
    hash: Hash,
}

impl StoredWrites {
    /// Queues `found` for the next write, scheduling one when none is.
    fn add(
        self: &Arc<Self>,
        found: FoundWhole,
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        hydrator: &Arc<Hydrator>,
    ) {
        {
            let mut state = self.state.lock_recover();
            if let Some(pending) = state.pending.as_mut() {
                pending.push(found);
                return;
            }
            state.pending = Some(vec![found]);
        }
        let stored = Arc::clone(self);
        let registry = Arc::clone(registry);
        let hydrator = Arc::clone(hydrator);
        hydrator.runtime_handle.clone().spawn(async move {
            tokio::select! {
                () = tokio::time::sleep(stored.settle) => {}
                () = stored.write_now.notified() => {}
            }
            let found = {
                let mut state = stored.state.lock_recover();
                state.writing = state.writing.saturating_add(1);
                state.pending.take().unwrap_or_default()
            };
            write_stored(found, registry, &hydrator).await;
            let mut state = stored.state.lock_recover();
            state.writing = state.writing.saturating_sub(1);
        });
    }

    /// Writes that wait `settle` for others instead of [`STORED_SETTLE`].
    #[cfg(test)]
    fn settling_for(settle: Duration) -> Self {
        Self {
            settle,
            ..Self::default()
        }
    }

    /// Writes what is queued now instead of after the wait.
    #[cfg(test)]
    fn write_now(&self) {
        self.write_now.notify_one();
    }

    /// Waits until everything queued so far is written.
    #[cfg(test)]
    async fn settled(&self) {
        loop {
            {
                let state = self.state.lock_recover();
                if state.pending.is_none() && state.writing == 0 {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

/// Writes `Resident` on the record of each file in `found` that is still
/// whole here, in one batch. A file whose record moved on to another
/// version, or already says so, is left alone. A file with no record gets
/// one naming the version read, never a `Resident` record without one.
async fn write_stored(
    found: Vec<FoundWhole>,
    registry: Arc<Mutex<AvailabilityRegistry>>,
    hydrator: &Hydrator,
) {
    let mut whole = Vec::with_capacity(found.len());
    for file in found {
        // Checked again: the cache budget may have taken it meanwhile.
        if let Some(bytes) = hydrator.local_len(file.hash).await {
            whole.push((file, bytes));
        }
    }
    if whole.is_empty() {
        return;
    }
    let written = tokio::task::spawn_blocking(move || {
        let verified_at = crate::clock::unix_now_ms();
        registry.lock_recover().batch(|registry| {
            for (file, bytes) in whole {
                let resident = Residency::Resident { bytes };
                let mut record = match registry.get(file.record_id) {
                    Some(record) => record.clone(),
                    None => {
                        let mut record = RecordAvailability::new(file.record_id, RecordKind::File);
                        record.current_version = Some(file.version);
                        record
                    }
                };
                if record.current_version != Some(file.version) || record.residency == resident {
                    continue;
                }
                record.residency = resident;
                record.freshness.local_verified_at_ms = Some(verified_at);
                let _ = registry.put(record);
            }
        })
    })
    .await;
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) => eprintln!("could not note that files are stored here: {error}"),
        Err(stopped) => eprintln!("the note that files are stored here stopped: {stopped}"),
    }
}

impl UnavailableNote {
    /// No device delivered the bytes. A transfer that owns the record (a
    /// pin's fetch, a retry) reports its own outcome, so it is left alone,
    /// and a note already there is not written again.
    fn mark(&self, now_ms: u64) {
        let mut registry = self.registry.lock_recover();
        let mut record = registry
            .get(self.record_id)
            .cloned()
            .unwrap_or_else(|| RecordAvailability::new(self.record_id, RecordKind::File));
        if record.transfer.phase != TransferPhase::Idle
            || record.transfer.retry_count > 0
            || record.transfer.last_error.is_some()
        {
            return;
        }
        record.transfer.last_error = Some(TransferError {
            message: NO_PEER_DELIVERED_ERROR.into(),
            occurred_at_ms: now_ms,
        });
        if let Err(error) = registry.put(record) {
            eprintln!("could not note that a file is unavailable: {error}");
        }
    }

    /// Bytes arrived, so a device that keeps the file is reachable again.
    /// The "no peer delivered" failure goes, whether a read noted it or a
    /// retry that has ended (the person's "Retry download" while the device
    /// was still offline): nothing else would ever take it back, and the
    /// file would read as unavailable while it opens. A transfer still in
    /// charge (queued, or a pin's fetch waiting for its next attempt) reports
    /// its own outcome, and any other failure stays.
    ///
    /// When they were the last bytes the file was missing, the record will
    /// say it is stored here ([`StoredWrites`]), unless the file has moved
    /// on to another version meanwhile. A record already saying so is not
    /// written again, so only the read that completes the file queues it.
    pub(super) async fn arrived(&self) {
        let whole = match &self.content {
            Some(content) => content.hydrator.local_len(content.hash).await,
            None => None,
        };
        let mut registry = self.registry.lock_recover();
        let current = registry.get(self.record_id);
        let queue = match (&self.content, whole) {
            (Some(content), Some(bytes)) => current.is_none_or(|record| {
                record.current_version == Some(content.version)
                    && record.residency != (Residency::Resident { bytes })
            }),
            _ => false,
        };
        if let Some(record) = current {
            let noted = record.transfer.phase == TransferPhase::Idle
                && record.transfer.retry_at_ms.is_none()
                && record
                    .transfer
                    .last_error
                    .as_ref()
                    .is_some_and(|error| error.message == NO_PEER_DELIVERED_ERROR);
            if noted {
                let mut record = record.clone();
                record.transfer.last_error = None;
                // Bytes arriving ends the run of failures a retry counted, so
                // the next read no peer answers is noted again.
                record.transfer.retry_count = 0;
                if let Err(error) = registry.put(record) {
                    eprintln!("could not note that a file is available again: {error}");
                }
            }
        }
        drop(registry);
        if queue && let Some(content) = &self.content {
            content.stored.add(
                FoundWhole {
                    record_id: self.record_id,
                    version: content.version,
                    hash: content.hash,
                },
                &self.registry,
                &content.hydrator,
            );
        }
    }
}

/// Waits at most `bound` for `read`, like
/// [`crate::hydration::bounded_read`], and notes on `note`'s record whether
/// the bytes arrived: a failed read marks the file unavailable, bytes that
/// arrive (in time, or later through the fetch that outlived the read)
/// mark it available again, and stored once the file is whole.
///
/// # Errors
///
/// `read`'s error, or an error naming the bound when it was reached.
pub(super) async fn noted_bounded_read<T, F>(
    bound: Duration,
    read: F,
    note: Option<UnavailableNote>,
) -> Result<T>
where
    T: Send + 'static,
    F: Future<Output = Result<T>> + Send + 'static,
{
    let mut task = tokio::spawn(read);
    let outcome = match tokio::time::timeout(bound, &mut task).await {
        Ok(Ok(result)) => result,
        Ok(Err(stopped)) => Err(anyhow!("the read stopped: {stopped}")),
        Err(_) => {
            if let Some(note) = note {
                note.mark(crate::clock::unix_now_ms());
                // The fetch goes on; when it lands, the file is back.
                tokio::spawn(async move {
                    if matches!(task.await, Ok(Ok(_))) {
                        note.arrived().await;
                    }
                });
            }
            return Err(anyhow!("no peer delivered the bytes within {bound:?}"));
        }
    };
    if let Some(note) = note {
        match &outcome {
            Ok(_) => note.arrived().await,
            Err(_) => note.mark(crate::clock::unix_now_ms()),
        }
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note() -> (tempfile::TempDir, UnavailableNote) {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let note = UnavailableNote {
            registry,
            record_id: FileRecordId::new(),
            content: None,
        };
        (dir, note)
    }

    fn projection(note: &UnavailableNote) -> fs_protocol::AvailabilityProjectionV1 {
        note.registry
            .lock_recover()
            .get(note.record_id)
            .expect("the read noted the file")
            .projection()
    }

    #[tokio::test]
    async fn a_read_no_peer_answers_shows_the_file_unavailable_until_bytes_arrive() {
        let (_dir, note) = note();
        let bound = Duration::from_millis(100);

        let failed = noted_bounded_read(
            bound,
            async { Err::<Vec<u8>, _>(anyhow!("no peer served the content")) },
            Some(note.clone()),
        )
        .await;
        assert!(failed.is_err());
        assert!(
            projection(&note).unavailable(),
            "a read no peer answered leaves the file unavailable"
        );

        let read = noted_bounded_read(bound, async { Ok(vec![7_u8]) }, Some(note.clone())).await;
        assert_eq!(read.unwrap(), vec![7]);
        let back = projection(&note);
        assert!(!back.unavailable(), "bytes that arrived make it available");
        assert_eq!(back.transfer.last_error, None);
    }

    #[tokio::test]
    async fn a_read_past_its_bound_marks_the_file_and_the_late_fetch_clears_it() {
        let (_dir, note) = note();
        let (land, landing) = tokio::sync::oneshot::channel::<()>();
        let late = noted_bounded_read(
            Duration::from_millis(100),
            async move {
                let _ = landing.await;
                Ok(())
            },
            Some(note.clone()),
        )
        .await;
        assert!(late.is_err(), "a read past its bound fails");
        assert!(projection(&note).unavailable());

        land.send(()).unwrap();
        tilo_test_env::within("the late fetch clears the note", async {
            while projection(&note).unavailable() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_transfer_that_owns_the_record_keeps_its_own_state() {
        let (_dir, note) = note();
        let mut record = RecordAvailability::new(note.record_id, RecordKind::File);
        record.transfer.phase = TransferPhase::Downloading;
        note.registry.lock_recover().put(record).unwrap();

        let failed = noted_bounded_read(
            Duration::from_millis(100),
            async { Err::<(), _>(anyhow!("no peer served the content")) },
            Some(note.clone()),
        )
        .await;
        assert!(failed.is_err());
        let state = projection(&note);
        assert_eq!(state.transfer.phase, TransferPhase::Downloading);
        assert_eq!(state.transfer.last_error, None);

        // What a pin's fetch that no peer answered leaves: queued for its
        // next attempt, which reports the outcome.
        let mut retrying = RecordAvailability::new(note.record_id, RecordKind::File);
        retrying.transfer.phase = TransferPhase::Queued;
        retrying.transfer.retry_count = 1;
        retrying.transfer.retry_at_ms = Some(9_000);
        retrying.transfer.last_error = Some(TransferError {
            message: NO_PEER_DELIVERED_ERROR.into(),
            occurred_at_ms: 5,
        });
        note.registry.lock_recover().put(retrying.clone()).unwrap();
        noted_bounded_read(
            Duration::from_millis(100),
            async { Ok(()) },
            Some(note.clone()),
        )
        .await
        .unwrap();
        assert_eq!(
            note.registry.lock_recover().get(note.record_id),
            Some(&retrying),
            "a pin's retry still to come is the pin's to report"
        );
    }

    /// A read that brings the last bytes a file was missing leaves its
    /// record saying it is stored here, written once; one that leaves bytes
    /// missing, or reads a version the file has moved on from, does not.
    #[tokio::test]
    async fn the_read_that_completes_a_file_records_it_stored() {
        use iroh::endpoint::{Endpoint, presets};

        let (_dir, mut note) = note();
        let store = Arc::new(crate::store::ContentStore::memory());
        let data = crate::test_bytes::pseudo(3 * 1024 * 1024, 5);
        let whole = store.write_content(&data).await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(
            Endpoint::bind(presets::Minimal).await.unwrap(),
            store,
        ));
        let version = FileVersionId::new();
        let stored = Arc::new(StoredWrites::default());
        let content = |hash| {
            Some(NotedContent {
                hydrator: Arc::clone(&hydrator),
                hash,
                version,
                stored: Arc::clone(&stored),
            })
        };
        let bound = Duration::from_millis(500);

        // Content with bytes still to come (here, all of them).
        note.content = content(Hash::from([3; 32]));
        noted_bounded_read(bound, async { Ok(()) }, Some(note.clone()))
            .await
            .unwrap();
        stored.settled().await;
        assert_eq!(
            note.registry.lock_recover().get(note.record_id),
            None,
            "a file with bytes still missing is not recorded stored"
        );

        note.content = content(whole);
        noted_bounded_read(bound, async { Ok(()) }, Some(note.clone()))
            .await
            .unwrap();
        stored.settled().await;
        let record = note
            .registry
            .lock_recover()
            .get(note.record_id)
            .cloned()
            .expect("the read that completed the file wrote its record");
        assert_eq!(
            record.residency,
            Residency::Resident {
                bytes: data.len() as u64
            }
        );
        assert_eq!(record.current_version, Some(version));
        assert!(record.freshness.local_verified_at_ms.is_some());
        assert_eq!(
            projection(&note).residency,
            Residency::Resident {
                bytes: data.len() as u64
            }
        );

        // Reads of a file already recorded stored write nothing.
        let changes = note.registry.lock_recover().subscribe_changes();
        noted_bounded_read(bound, async { Ok(()) }, Some(note.clone()))
            .await
            .unwrap();
        stored.settled().await;
        assert!(
            !changes.has_changed().unwrap(),
            "a stored file is written once"
        );

        // The file moved on to another version while the read ran.
        let mut moved = record;
        moved.current_version = Some(FileVersionId::new());
        moved.residency = Residency::NotResident;
        note.registry.lock_recover().put(moved.clone()).unwrap();
        noted_bounded_read(bound, async { Ok(()) }, Some(note.clone()))
            .await
            .unwrap();
        stored.settled().await;
        assert_eq!(
            note.registry.lock_recover().get(note.record_id),
            Some(&moved),
            "bytes of an older version say nothing of the current one"
        );
    }

    /// Every write of the registry rewrites all of it durably and wakes the
    /// pinned-hydration pass over the whole namespace. A folder read
    /// through in a burst (a copy, a search, thumbnails) is recorded stored
    /// in one such write, not one per file, and the reads that complete
    /// the files do not wait for it.
    #[tokio::test]
    async fn files_read_whole_together_are_recorded_in_one_write() {
        use iroh::endpoint::{Endpoint, presets};

        let (_dir, note) = note();
        let store = Arc::new(crate::store::ContentStore::memory());
        let mut files = Vec::new();
        for seed in 0..24_u64 {
            let data = crate::test_bytes::pseudo(16 * 1024, seed);
            let hash = store.write_content(&data).await.unwrap().unwrap();
            files.push((FileRecordId::new(), hash, data.len() as u64));
        }
        let hydrator = Arc::new(Hydrator::new(
            Endpoint::bind(presets::Minimal).await.unwrap(),
            store,
        ));
        // Written when the test says, however long the burst takes.
        let stored = Arc::new(StoredWrites::settling_for(Duration::from_secs(3600)));
        let version = FileVersionId::new();
        let (writes, wakes) = {
            let registry = note.registry.lock_recover();
            (registry.persist_count(), registry.hydration_generation())
        };

        for (record_id, hash, _) in &files {
            let read = UnavailableNote {
                registry: Arc::clone(&note.registry),
                record_id: *record_id,
                content: Some(NotedContent {
                    hydrator: Arc::clone(&hydrator),
                    hash: *hash,
                    version,
                    stored: Arc::clone(&stored),
                }),
            };
            noted_bounded_read(Duration::from_millis(500), async { Ok(()) }, Some(read))
                .await
                .unwrap();
        }
        assert_eq!(
            note.registry.lock_recover().persist_count(),
            writes,
            "a read that completes a file does not wait for the registry"
        );
        stored.write_now();
        stored.settled().await;

        let registry = note.registry.lock_recover();
        for (record_id, _, bytes) in &files {
            let record = registry
                .get(*record_id)
                .expect("each file read whole is recorded");
            assert_eq!(record.residency, Residency::Resident { bytes: *bytes });
            assert_eq!(record.current_version, Some(version));
        }
        assert_eq!(
            registry.persist_count() - writes,
            1,
            "the files are written together"
        );
        assert_eq!(
            registry.hydration_generation().wrapping_sub(wakes),
            1,
            "the pinned-hydration pass is woken once"
        );
    }

    /// "Retry download" while the device that keeps the file is offline
    /// fails and ends there. Once that device is back, bytes a read gets
    /// make the file available again; nothing else would.
    #[tokio::test]
    async fn bytes_after_a_retry_that_ended_make_the_file_available_again() {
        let (_dir, note) = note();
        // What a retry the person asked for leaves when no peer answered.
        let mut retried = RecordAvailability::new(note.record_id, RecordKind::File);
        retried.transfer.retry_count = 1;
        retried.transfer.last_error = Some(TransferError {
            message: NO_PEER_DELIVERED_ERROR.into(),
            occurred_at_ms: 5,
        });
        note.registry.lock_recover().put(retried).unwrap();
        assert!(projection(&note).unavailable());

        let bound = Duration::from_millis(100);
        let read = noted_bounded_read(bound, async { Ok(vec![7_u8]) }, Some(note.clone())).await;
        assert_eq!(read.unwrap(), vec![7]);
        let back = projection(&note);
        assert!(!back.unavailable(), "bytes that arrived make it available");
        assert_eq!(back.transfer.last_error, None);

        let failed = noted_bounded_read(
            bound,
            async { Err::<Vec<u8>, _>(anyhow!("no peer served the content")) },
            Some(note.clone()),
        )
        .await;
        assert!(failed.is_err());
        assert!(
            projection(&note).unavailable(),
            "the next read no peer answers is noted again"
        );
    }
}
