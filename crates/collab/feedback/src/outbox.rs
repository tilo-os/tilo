use std::{collections::BTreeMap, rc::Rc};

use durable_store::{Durability, DurableStore, Lease, Revision, StorageKey, WriteOp};
use serde::{Deserialize, Serialize};

use crate::{Destination, Error, Receipt, Submission};

const MAX_ENTRIES: usize = 64;
const MAX_BYTES: usize = 4_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    submission: Submission,
    receipt: Option<Receipt>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    entries: BTreeMap<String, Entry>,
}

/// Local state suitable for the composer's localized status and retry controls.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryState {
    Saving,
    StorageFailed,
    AwaitingReceipt,
    Sent { report: String },
}

/// One writer of the local device's feedback outbox. Hosts supply the same
/// profile storage they already use for records; this introduces no file path.
pub struct Outbox {
    store: Rc<dyn DurableStore>,
    key: StorageKey,
    _lease: Lease,
    snapshot: Snapshot,
    revision: Option<Revision>,
    rejected_write: bool,
}

impl Outbox {
    /// Restore immutable pending submissions from the host's durable store.
    pub fn open(store: Rc<dyn DurableStore>, key: StorageKey) -> Result<Self, Error> {
        let lease = store.lease(&key)?;
        // What is stored becomes durable on the store's own thread where this
        // one must not wait (the compositor's loop); reading needs no flush.
        match store.flush_where_allowed(&key) {
            Ok(()) | Err(durable_store::StoreError::Pending(_)) => {}
            Err(error) => return Err(error.into()),
        }
        let snapshot: Snapshot = match store.read(&key)? {
            Some(stored) => {
                if stored.bytes.len() > MAX_BYTES {
                    return Err(Error::Full);
                }
                serde_json::from_slice(&stored.bytes)?
            }
            None => Snapshot::default(),
        };
        if snapshot.entries.len() > MAX_ENTRIES {
            return Err(Error::Full);
        }
        for (id, entry) in &snapshot.entries {
            entry.submission.validate()?;
            if *id != entry.submission.id {
                return Err(Error::Conflict);
            }
            if let Some(receipt) = &entry.receipt {
                validate_receipt(&entry.submission.destination, &entry.submission, receipt)?;
            }
        }
        Ok(Self {
            store,
            key,
            _lease: lease,
            snapshot,
            revision: None,
            rejected_write: false,
        })
    }

    /// Persist an approved payload before the transport may see it. Queueing the
    /// identical id/payload twice is idempotent; reusing an id for edits fails.
    pub fn queue(&mut self, submission: Submission) -> Result<(), Error> {
        submission.validate()?;
        if let Some(existing) = self.snapshot.entries.get(&submission.id) {
            return if existing.submission == submission {
                Ok(())
            } else {
                Err(Error::Conflict)
            };
        }
        if self.snapshot.entries.len() >= MAX_ENTRIES {
            return Err(Error::Full);
        }
        let mut next = self.snapshot.clone();
        next.entries.insert(
            submission.id.clone(),
            Entry {
                submission,
                receipt: None,
            },
        );
        self.persist(next)
    }

    /// An immutable report eligible for send/retry. Local persistence must have
    /// completed; repeated calls return the same id and reviewed payload.
    ///
    /// # Errors
    /// Refuses an unknown id or local writes that are not durable.
    pub fn pending(&self, id: &str) -> Result<Option<&Submission>, Error> {
        self.require_durable()?;
        let entry = self
            .snapshot
            .entries
            .get(id)
            .ok_or(Error::Invalid("unknown submission"))?;
        Ok(entry.receipt.is_none().then_some(&entry.submission))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> {
        self.snapshot.entries.keys().map(String::as_str)
    }

    /// Confirm only a receipt read through an authenticated destination adapter
    /// after the destination record has committed. Passing a locally generated
    /// or merely dispatched receipt violates the adapter contract.
    ///
    /// # Errors
    /// Refuses mismatched identity, digest, source, or report; storage failures
    /// preserve the queued submission and never report successful delivery.
    pub fn confirm_durable_receipt(
        &mut self,
        authenticated_source: &Destination,
        receipt: Receipt,
    ) -> Result<(), Error> {
        self.require_durable()?;
        let entry = self
            .snapshot
            .entries
            .get(&receipt.id)
            .ok_or(Error::ReceiptMismatch)?;
        validate_receipt(authenticated_source, &entry.submission, &receipt)?;
        if let Some(previous) = &entry.receipt {
            return if *previous == receipt {
                Ok(())
            } else {
                Err(Error::ReceiptMismatch)
            };
        }
        let mut next = self.snapshot.clone();
        let target = next
            .entries
            .get_mut(&receipt.id)
            .ok_or(Error::ReceiptMismatch)?;
        target.receipt = Some(receipt);
        self.persist(next)
    }

    /// Visible success waits for both destination confirmation and durable
    /// persistence of that confirmation on this device.
    ///
    /// # Errors
    /// Refuses an unknown submission id.
    pub fn state(&self, id: &str) -> Result<DeliveryState, Error> {
        let entry = self
            .snapshot
            .entries
            .get(id)
            .ok_or(Error::Invalid("unknown submission"))?;
        if self.rejected_write {
            return Ok(DeliveryState::StorageFailed);
        }
        if let Some(revision) = self.revision {
            match self.store.durability(&self.key, revision) {
                Durability::Pending => return Ok(DeliveryState::Saving),
                Durability::Failed(_) => return Ok(DeliveryState::StorageFailed),
                Durability::Durable => {}
            }
        }
        Ok(entry
            .receipt
            .as_ref()
            .map_or(DeliveryState::AwaitingReceipt, |receipt| {
                DeliveryState::Sent {
                    report: receipt.report.clone(),
                }
            }))
    }

    /// Retry the local write without changing the submission or its receipt.
    ///
    /// # Errors
    /// Reports a failure accepting the replacement snapshot.
    pub fn retry_storage(&mut self) -> Result<(), Error> {
        self.persist(self.snapshot.clone())
    }

    fn require_durable(&self) -> Result<(), Error> {
        if self.rejected_write
            || self.revision.is_some_and(|revision| {
                self.store.durability(&self.key, revision) != Durability::Durable
            })
        {
            return Err(Error::NotDurable);
        }
        Ok(())
    }

    fn persist(&mut self, next: Snapshot) -> Result<(), Error> {
        let bytes = serde_json::to_vec(&next)?;
        if bytes.len() > MAX_BYTES {
            return Err(Error::Full);
        }
        let revision = match self.store.write(&self.key, WriteOp::Replace(bytes), None) {
            Ok(revision) => revision,
            Err(error) => {
                // Keep the approved identity in memory for an honest retry,
                // even when the writer rejects its very first write.
                self.snapshot = next;
                self.rejected_write = true;
                return Err(error.into());
            }
        };
        self.snapshot = next;
        self.revision = Some(revision);
        self.rejected_write = false;
        Ok(())
    }
}

fn validate_receipt(
    source: &Destination,
    submission: &Submission,
    receipt: &Receipt,
) -> Result<(), Error> {
    if *source != submission.destination
        || receipt.destination != *source
        || receipt.id != submission.id
        || receipt.digest != submission.digest()?
        || receipt.report.trim().is_empty()
        || receipt.report.len() > 256
        || receipt.report.chars().any(char::is_control)
    {
        return Err(Error::ReceiptMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Draft, Receipt};
    use durable_store::MemoryStore;

    fn destination() -> Destination {
        Destination {
            room: "demo".into(),
            provider: "record:chat:feedback".into(),
            channel: "feedback".into(),
        }
    }
    fn submission() -> Submission {
        let mut preview = Draft {
            text: "Opening the book failed".into(),
            context: Vec::new(),
        }
        .preview()
        .unwrap();
        preview.approve();
        preview
            .submission("0123456789abcdef0123456789abcdef".into(), destination())
            .unwrap()
    }
    fn key() -> StorageKey {
        StorageKey::package("so.tilo.feedback-outbox")
    }
    fn commit(store: &MemoryStore, outcome: &Result<(), String>) {
        for write in store.drain() {
            store.acknowledge(write.revision, outcome.clone()).unwrap();
        }
    }
    fn receipt(submission: &Submission) -> Receipt {
        Receipt {
            id: submission.id.clone(),
            digest: submission.digest().unwrap(),
            destination: submission.destination.clone(),
            report: "report:42".into(),
        }
    }

    #[test]
    fn reload_and_retry_keep_the_reviewed_id_and_payload_until_destination_confirmation() {
        let store = Rc::new(MemoryStore::queued());
        let report = submission();
        let mut outbox = Outbox::open(store.clone(), key()).unwrap();
        outbox.queue(report.clone()).unwrap();
        assert_eq!(outbox.state(&report.id).unwrap(), DeliveryState::Saving);
        assert!(matches!(outbox.pending(&report.id), Err(Error::NotDurable)));
        commit(&store, &Ok(()));
        assert_eq!(outbox.pending(&report.id).unwrap(), Some(&report));
        drop(outbox);
        let mut outbox = Outbox::open(store.clone(), key()).unwrap();
        assert_eq!(outbox.pending(&report.id).unwrap(), Some(&report));
        assert_eq!(outbox.pending(&report.id).unwrap(), Some(&report));
        assert_eq!(
            outbox.state(&report.id).unwrap(),
            DeliveryState::AwaitingReceipt
        );
        outbox
            .confirm_durable_receipt(&destination(), receipt(&report))
            .unwrap();
        assert_eq!(outbox.state(&report.id).unwrap(), DeliveryState::Saving);
        commit(&store, &Ok(()));
        assert_eq!(
            outbox.state(&report.id).unwrap(),
            DeliveryState::Sent {
                report: "report:42".into()
            }
        );
        assert!(outbox.pending(&report.id).unwrap().is_none());
    }

    #[test]
    fn conflicting_retries_and_receipts_from_other_rooms_are_refused() {
        let store = Rc::new(MemoryStore::new());
        let report = submission();
        let mut outbox = Outbox::open(store, key()).unwrap();
        outbox.queue(report.clone()).unwrap();
        outbox.queue(report.clone()).unwrap();
        let mut changed = report.clone();
        changed.feedback.text = "Different issue".into();
        assert!(matches!(outbox.queue(changed), Err(Error::Conflict)));
        let mut other = destination();
        other.room = "personal".into();
        assert!(matches!(
            outbox.confirm_durable_receipt(&other, receipt(&report)),
            Err(Error::ReceiptMismatch)
        ));
        let mut wrong = receipt(&report);
        wrong.digest = "wrong".into();
        assert!(matches!(
            outbox.confirm_durable_receipt(&destination(), wrong),
            Err(Error::ReceiptMismatch)
        ));
        assert_eq!(outbox.pending(&report.id).unwrap(), Some(&report));
    }

    #[test]
    fn failed_local_commit_can_be_retried_without_losing_the_issue() {
        let store = Rc::new(MemoryStore::queued());
        let report = submission();
        let mut outbox = Outbox::open(store.clone(), key()).unwrap();
        outbox.queue(report.clone()).unwrap();
        commit(&store, &Err("synthetic disk failure".into()));
        assert_eq!(
            outbox.state(&report.id).unwrap(),
            DeliveryState::StorageFailed
        );
        assert!(matches!(outbox.pending(&report.id), Err(Error::NotDurable)));
        outbox.retry_storage().unwrap();
        commit(&store, &Ok(()));
        assert_eq!(outbox.pending(&report.id).unwrap(), Some(&report));
    }
}
