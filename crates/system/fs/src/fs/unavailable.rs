//! A blocked read of bytes a peer must supply, noted on the file's record.
//!
//! Such a read fails with `EIO` at [`crate::hydration::BLOCKED_READ_BOUND`].
//! The reader then knows only that it failed; everyone else would still see
//! the file as merely not stored here. So the failure is written on the
//! file's availability record, where Files and the apps read it, and Files
//! shows the file as unavailable until a device that keeps it is back. The
//! fetch outlives the read, and the note is cleared as soon as bytes arrive.

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, anyhow};
use fs_protocol::{FileRecordId, NO_PEER_DELIVERED_ERROR, TransferPhase};
use tilo_lock::MutexRecover as _;

use crate::availability::{AvailabilityRegistry, RecordAvailability, RecordKind, TransferError};

/// The record a blocked read notes its outcome on.
#[derive(Clone)]
pub(super) struct UnavailableNote {
    pub(super) registry: Arc<Mutex<AvailabilityRegistry>>,
    pub(super) record_id: FileRecordId,
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
    fn clear(&self) {
        let mut registry = self.registry.lock_recover();
        let Some(record) = registry.get(self.record_id) else {
            return;
        };
        let noted = record.transfer.phase == TransferPhase::Idle
            && record.transfer.retry_at_ms.is_none()
            && record
                .transfer
                .last_error
                .as_ref()
                .is_some_and(|error| error.message == NO_PEER_DELIVERED_ERROR);
        if !noted {
            return;
        }
        let mut record = record.clone();
        record.transfer.last_error = None;
        // Bytes arriving ends the run of failures a retry counted, so the
        // next read no peer answers is noted again.
        record.transfer.retry_count = 0;
        if let Err(error) = registry.put(record) {
            eprintln!("could not note that a file is available again: {error}");
        }
    }
}

/// Waits at most `bound` for `read`, like
/// [`crate::hydration::bounded_read`], and notes on `note`'s record whether
/// the bytes arrived: a failed read marks the file unavailable, bytes that
/// arrive (in time, or later through the fetch that outlived the read)
/// mark it available again.
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
                        note.clear();
                    }
                });
            }
            return Err(anyhow!("no peer delivered the bytes within {bound:?}"));
        }
    };
    if let Some(note) = note {
        match &outcome {
            Ok(_) => note.clear(),
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
