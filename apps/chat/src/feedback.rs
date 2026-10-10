//! Feedback reports are attributed Chat messages with an immutable submission
//! identity, kept with their triage in the chat record
//! ([`crate::record::Chat::feedback`]). A receipt is only offered after the
//! host confirms the record is durable.

use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, Doc};
use ui::feedback::{Destination, Receipt};
use ui::tilo_resource::Id;

pub(crate) mod destination_read;
pub(crate) mod receipt_read;
pub(crate) mod status_read;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TriageUpdate {
    Investigating,
    Resolved,
    Reopen,
    Duplicate {
        report: String,
    },
    Task {
        reference: tilo_reference::ResourceRef,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageEntry {
    pub author: String,
    pub ts: i64,
    pub update: TriageUpdate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub submission: ui::feedback::Submission,
    pub author: String,
    pub ts: i64,
    #[serde(default)]
    pub triage: Vec<TriageEntry>,
}

fn refused(reason: impl Into<String>) -> ActionError {
    ActionError::Refused(reason.into())
}

/// The id of the message a report is posted as.
#[must_use]
pub fn message_id(report: &str) -> String {
    format!("m_feedback_{report}")
}

/// The report `id` as the record keeps it, its triage oldest first.
fn report_of(id: &Id, report: &crate::record::FeedbackReport) -> Result<Report, ActionError> {
    let submission = report
        .submission
        .0
        .clone()
        .ok_or_else(|| refused("invalid feedback report"))?;
    submission
        .validate()
        .map_err(|error| refused(error.to_string()))?;
    if submission.id != id.as_str() {
        return Err(refused("feedback id conflict"));
    }
    let mut triage: Vec<TriageEntry> = report
        .triage
        .values()
        .filter_map(|entry| {
            Some(TriageEntry {
                author: entry.author.0.clone(),
                ts: entry.ts,
                update: entry.update.0.clone()?,
            })
        })
        .collect();
    triage.sort_by_key(|entry| {
        (
            entry.ts,
            entry.author.clone(),
            serde_json::to_string(&entry.update).unwrap_or_default(),
        )
    });
    Ok(Report {
        submission,
        author: report.author.0.clone(),
        ts: report.ts,
        triage,
    })
}

/// The report `id` filed in the chat `doc` holds.
///
/// # Errors
/// Refuses a report whose submission is corrupt or filed under another id.
pub fn report(doc: &Doc, id: &str) -> Result<Option<Report>, ActionError> {
    let chat = crate::projection::read(doc);
    let id = Id::from_raw(id);
    chat.feedback
        .get(&id)
        .map(|report| report_of(&id, report))
        .transpose()
}

/// Every report filed in the chat `doc` holds.
///
/// # Errors
/// Refuses corrupt report metadata.
pub fn reports(doc: &Doc) -> Result<Vec<Report>, ActionError> {
    let chat = crate::projection::read(doc);
    chat.feedback
        .iter()
        .map(|(id, report)| report_of(id, report))
        .collect()
}

/// Called by the authenticated host's receipt reader after its canonical
/// record reports `Saved`; the source is retained transport identity, not input.
///
/// # Errors
/// Refuses a non-durable record or a report filed to a different destination.
pub fn durable_receipt(
    doc: &Doc,
    id: &str,
    actual_destination: &Destination,
    save_status: &ui::record::RecordSaveStatus,
) -> Result<Receipt, ActionError> {
    if *save_status != ui::record::RecordSaveStatus::Saved {
        return Err(refused("feedback is awaiting durable destination storage"));
    }
    receipt_content(doc, id, actual_destination)
}

fn receipt_content(
    doc: &Doc,
    id: &str,
    actual_destination: &Destination,
) -> Result<Receipt, ActionError> {
    let report = report(doc, id)?.ok_or_else(|| refused("feedback report not found"))?;
    if report.submission.destination != *actual_destination {
        return Err(refused("feedback destination mismatch"));
    }
    let message = Id::from_raw(message_id(id));
    let posted = crate::projection::read(doc)
        .messages
        .get(&message)
        .is_some_and(|posted| {
            posted.author.0 == report.author
                && posted.channel.as_str() == actual_destination.channel
        });
    if !posted {
        return Err(refused("feedback message not found"));
    }
    Ok(Receipt {
        id: id.into(),
        digest: report
            .submission
            .digest()
            .map_err(|error| refused(error.to_string()))?,
        destination: actual_destination.clone(),
        report: ui::tilo_resource::items::item_resource_id::<crate::resource::Chat>(
            "messages", &message,
        ),
    })
}

/// Pure canonical report projection for the system's feedback receipt service.
/// The owner service must check its exact `RecordStore`'s current durability;
/// this projection itself grants neither delivery confirmation nor authorship.
/// # Errors
/// Refuses missing reports, conflicting payloads or invalid attributed messages.
pub fn canonical_feedback_projection(
    doc: &Doc,
    submission: &ui::feedback::Submission,
) -> Result<(Receipt, String), ui::feedback::Error> {
    submission.validate()?;
    let report = report(doc, &submission.id)
        .map_err(|_| ui::feedback::Error::ReceiptMismatch)?
        .ok_or(ui::feedback::Error::ReceiptMismatch)?;
    if report.submission != *submission {
        return Err(ui::feedback::Error::ReceiptMismatch);
    }
    let receipt = receipt_content(doc, &submission.id, &submission.destination)
        .map_err(|_| ui::feedback::Error::ReceiptMismatch)?;
    Ok((receipt, report.author))
}

/// Read the report from its canonical owner after checking that exact record's
/// current save status. The store is single-threaded, and no callback or yield
/// separates this status check from the immutable document read.
///
/// Hosts retain the admitted room/provider identity; neither the report nor an
/// external URL selects this store or record.
///
/// # Errors
/// Refuses a missing/busy record, undurable edits or a destination mismatch.
pub fn receipt_from_record(
    store: &ui::record::RecordStore,
    record: &ui::record::RecordId,
    id: &str,
    actual_destination: &Destination,
) -> Result<Receipt, ActionError> {
    let status = store
        .save_status(record)
        .map_err(|error| refused(error.to_string()))?;
    let (package, room) = store.package_and_home();
    if package != crate::definition().app_id || room != actual_destination.room {
        return Err(refused("feedback canonical store mismatch"));
    }
    store
        .read(record, |doc| {
            let scope = ui::record::room_scope(doc);
            if scope
                .as_deref()
                .is_some_and(|room| room != actual_destination.room)
                || (!record.is_package_primary() && scope.is_none())
            {
                return Err(refused("feedback canonical room mismatch"));
            }
            durable_receipt(doc, id, actual_destination, &status)
        })
        .ok_or_else(|| refused("feedback destination record is missing or busy"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::chat;
    use crate::resource::Chat;
    use crate::testing::{apply_as, demo_doc, messages};
    use automerge::{ROOT, transaction::Transactable as _};
    use tilo_automerge::ApplyIncoming as _;
    use ui::feedback::{ReviewedFeedback, Submission};
    use ui::tilo_resource::Invoke;

    fn seeded() -> (Doc, Submission) {
        let doc = demo_doc();
        let channel = crate::testing::channels(&doc)
            .into_iter()
            .find(|channel| channel.kind != crate::model::ChannelKind::Voice)
            .unwrap();
        (
            doc,
            Submission {
                id: "0123456789abcdef0123456789abcdef".into(),
                destination: Destination {
                    room: channel.workspace,
                    provider: "demo-chat".into(),
                    channel: channel.id,
                },
                feedback: ReviewedFeedback {
                    text: "The reader loses my position".into(),
                    context: Vec::new(),
                    screenshot: None,
                },
            },
        )
    }

    fn action(submission: &Submission) -> Invoke<Chat> {
        Chat::SUBMIT_FEEDBACK.with(chat::SubmitFeedback {
            channel: Id::from_raw(submission.destination.channel.clone()),
            submission: serde_json::to_value(submission).unwrap(),
        })
    }

    fn triage(submission: &Submission, update: &TriageUpdate) -> Invoke<Chat> {
        Chat::TRIAGE.with(chat::Triage {
            report: Id::from_raw(submission.id.clone()),
            update: serde_json::to_value(update).unwrap(),
        })
    }

    #[test]
    fn retry_after_reload_keeps_one_attributed_message_and_report() {
        use ui::tilo_resource::Resource as _;
        let (mut doc, submission) = seeded();
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        let saved = doc.save();
        let mut restored = Doc::load(&saved).unwrap();
        apply_as(&mut restored, "visitor:one", action(&submission)).unwrap();
        let messages = messages(&restored, &submission.destination.channel);
        let matching: Vec<_> = messages
            .iter()
            .filter(|message| message.id == message_id(&submission.id))
            .collect();
        assert_eq!(matching.len(), 1);
        assert_eq!(matching[0].author, "visitor:one");
        assert_eq!(
            report(&restored, &submission.id)
                .unwrap()
                .unwrap()
                .submission,
            submission
        );
        let state = Chat::read(&restored).unwrap();
        let report_id = crate::testing::item("feedback", &submission.id);
        let entries = Chat::search(&state);
        let matching: Vec<_> = entries
            .iter()
            .filter(|entry| entry.anchor.as_deref() == Some(report_id.as_str()))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "a retried report has one typed search result"
        );
        assert_eq!(matching[0].title, submission.feedback.text);
        let published = Chat::child(&state, &report_id).expect("the report is an item");
        assert!(published.actions.iter().any(|spec| spec.id == "triage"));
    }

    #[test]
    fn unauthored_and_conflicting_retries_are_refused_without_changes() {
        let (mut doc, submission) = seeded();
        let mut unauthored = ui::tilo_resource::Replica::<Chat>::from_doc(doc.fork());
        assert!(unauthored.apply(action(&submission)).is_err());
        assert!(reports(unauthored.doc()).unwrap().is_empty());
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        let before = doc.get_heads();
        assert!(apply_as(&mut doc, "visitor:other", action(&submission)).is_err());
        let mut changed = submission.clone();
        changed.feedback.text = "different".into();
        assert!(apply_as(&mut doc, "visitor:one", action(&changed)).is_err());
        let mut elsewhere = submission;
        elsewhere.id = "fedcba9876543210fedcba9876543210".into();
        elsewhere.destination.channel = "c_random".into();
        assert!(
            apply_as(
                &mut doc,
                "visitor:one",
                Chat::SUBMIT_FEEDBACK.with(chat::SubmitFeedback {
                    channel: Id::from_raw("c_general"),
                    submission: serde_json::to_value(&elsewhere).unwrap(),
                })
            )
            .is_err(),
            "a submission is filed in the channel it names"
        );
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    fn concurrent_first_submissions_both_survive() {
        let (mut doc, submission) = seeded();
        let mut peer = doc.fork();
        let mut other = submission.clone();
        other.id = "fedcba9876543210fedcba9876543210".into();
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        apply_as(&mut peer, "visitor:two", action(&other)).unwrap();
        doc.merge_incoming(&mut peer).unwrap();
        assert_eq!(reports(&doc).unwrap().len(), 2);
        assert!(report(&doc, &submission.id).unwrap().is_some());
        assert!(report(&doc, &other.id).unwrap().is_some());
    }

    #[test]
    fn triage_is_attributed_and_preserves_the_issue() {
        let (mut doc, submission) = seeded();
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        apply_as(
            &mut doc,
            "maintainer:one",
            triage(&submission, &TriageUpdate::Investigating),
        )
        .unwrap();
        apply_as(
            &mut doc,
            "maintainer:two",
            triage(
                &submission,
                &TriageUpdate::Duplicate {
                    report: "fedcba9876543210fedcba9876543210".into(),
                },
            ),
        )
        .unwrap();
        let report = report(&doc, &submission.id).unwrap().unwrap();
        assert_eq!(report.submission, submission);
        assert_eq!(report.author, "visitor:one");
        assert_eq!(
            report
                .triage
                .iter()
                .map(|entry| entry.author.as_str())
                .collect::<Vec<_>>(),
            ["maintainer:one", "maintainer:two"]
        );
        assert_eq!(report.triage[0].update, TriageUpdate::Investigating);
        assert!(
            apply_as(
                &mut doc,
                "maintainer:one",
                Chat::TRIAGE.with(chat::Triage {
                    report: Id::from_raw(submission.id),
                    update: serde_json::json!({"status": "lost"}),
                })
            )
            .is_err()
        );
    }

    #[test]
    fn concurrent_retry_deduplicates_the_visible_report_message() {
        let (mut doc, submission) = seeded();
        let mut peer = doc.fork();
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        apply_as(&mut peer, "visitor:one", action(&submission)).unwrap();
        doc.merge_incoming(&mut peer).unwrap();
        assert_eq!(reports(&doc).unwrap().len(), 1);
        assert_eq!(
            messages(&doc, &submission.destination.channel)
                .iter()
                .filter(|message| message.id == message_id(&submission.id))
                .count(),
            1
        );
    }

    /// Seed the demo and file `submission` as `visitor:one` in `doc`.
    fn filed(doc: &mut Doc, submission: &Submission) {
        crate::demo::seed(doc).unwrap();
        apply_as(doc, "visitor:one", action(submission)).unwrap();
    }

    #[test]
    fn canonical_owner_receipt_refuses_newer_unsaved_edits_and_memory_only_records() {
        use ui::record::{RecordCheckout, RecordId, RecordKind, RecordOwners, RecordStore};
        let (_, submission) = seeded();
        let temp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(temp.path().join("profile"));
        let store = owners
            .open("so.tilo.chat", &submission.destination.room)
            .unwrap();
        let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("chat.workspace"));
        store
            .reserve_record(&checkout)
            .unwrap()
            .initialize(|doc| {
                filed(doc, &submission);
                Ok::<(), String>(())
            })
            .unwrap();
        store.try_flush().unwrap();
        assert!(
            receipt_from_record(
                &store,
                &checkout.id,
                &submission.id,
                &submission.destination
            )
            .is_ok()
        );
        store
            .with(&checkout.id, |doc| {
                doc.put(ROOT, "newer_edit", "not yet stored").unwrap();
            })
            .unwrap();
        assert!(
            receipt_from_record(
                &store,
                &checkout.id,
                &submission.id,
                &submission.destination
            )
            .is_err()
        );
        store.try_flush().unwrap();
        assert!(
            receipt_from_record(
                &store,
                &checkout.id,
                &submission.id,
                &submission.destination
            )
            .is_ok()
        );
        let memory = RecordStore::new(
            ui::record::RecordSet::in_memory().with_home(&submission.destination.room),
        );
        memory.create_record(&checkout).unwrap();
        memory
            .with(&checkout.id, |doc| filed(doc, &submission))
            .unwrap();
        assert!(
            receipt_from_record(
                &memory,
                &checkout.id,
                &submission.id,
                &submission.destination
            )
            .is_err()
        );
    }

    #[test]
    fn primary_chat_receipt_uses_its_saved_store_namespace_without_a_fake_record_stamp() {
        use ui::record::{RecordCheckout, RecordKind, RecordOwners};
        let (_, submission) = seeded();
        let temp = tempfile::tempdir().unwrap();
        let mut owners = RecordOwners::new(temp.path().join("profile"));
        let store = owners
            .open(crate::definition().app_id, &submission.destination.room)
            .unwrap();
        let primary = RecordCheckout::package_primary(RecordKind::new("chat.workspace"));
        store
            .reserve_record(&primary)
            .unwrap()
            .initialize(|doc| {
                filed(doc, &submission);
                Ok::<(), String>(())
            })
            .unwrap();
        store.try_flush().unwrap();
        assert!(
            store
                .read(&primary.id, |doc| ui::record::room_scope(doc).is_none())
                .unwrap()
        );
        assert!(
            receipt_from_record(&store, &primary.id, &submission.id, &submission.destination)
                .is_ok()
        );
        let mut elsewhere = submission.destination.clone();
        elsewhere.room = "another-room".into();
        assert!(receipt_from_record(&store, &primary.id, &submission.id, &elsewhere).is_err());
        store
            .with(&primary.id, |doc| {
                doc.put(ROOT, "later", "unsaved").unwrap();
            })
            .unwrap();
        assert!(
            receipt_from_record(&store, &primary.id, &submission.id, &submission.destination)
                .is_err()
        );
    }

    #[test]
    fn receipt_requires_saved_destination_and_canonical_attributed_message() {
        use ui::record::RecordSaveStatus;
        let (mut doc, submission) = seeded();
        apply_as(&mut doc, "visitor:one", action(&submission)).unwrap();
        for status in [
            RecordSaveStatus::Saving,
            RecordSaveStatus::MemoryOnly,
            RecordSaveStatus::Failed("disk full".into()),
        ] {
            assert!(
                durable_receipt(&doc, &submission.id, &submission.destination, &status).is_err()
            );
        }
        let receipt = durable_receipt(
            &doc,
            &submission.id,
            &submission.destination,
            &RecordSaveStatus::Saved,
        )
        .unwrap();
        assert_eq!(receipt.digest, submission.digest().unwrap());
        assert_eq!(
            receipt.report,
            crate::testing::item("messages", &message_id(&submission.id))
        );
        let mut elsewhere = submission.destination.clone();
        elsewhere.room = "other-room".into();
        assert!(
            durable_receipt(&doc, &submission.id, &elsewhere, &RecordSaveStatus::Saved).is_err()
        );
        apply_as(
            &mut doc,
            "visitor:one",
            Chat::DELETE_MESSAGE.with(chat::DeleteMessage {
                message: Id::from_raw(message_id(&submission.id)),
            }),
        )
        .unwrap();
        assert!(
            durable_receipt(
                &doc,
                &submission.id,
                &submission.destination,
                &RecordSaveStatus::Saved
            )
            .is_err(),
            "no receipt once its message is gone"
        );
    }
}
