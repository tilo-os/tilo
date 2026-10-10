//! Feedback reports are attributed Chat messages with an immutable submission
//! identity. Metadata and triage are resources in the same governed document.
//! A receipt is only offered after the host confirms this document is durable.

use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value, transaction::Transactable};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, Doc};
use ui::feedback::{Destination, Receipt, Submission};
use ui::tilo_resource::Effect as _;

use crate::{
    doc::{ChatDoc, NewMessage},
    model::Attachment,
};

const REPORTS: &str = "feedback_reports";

pub(crate) mod destination_read;
pub(crate) mod receipt_read;
pub(crate) mod status_read;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum FeedbackAction {
    Submit {
        submission: Submission,
        ts: i64,
    },
    Triage {
        id: String,
        channel: String,
        update: TriageUpdate,
        ts: i64,
    },
}

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
    pub submission: Submission,
    pub author: String,
    pub ts: i64,
    #[serde(default)]
    pub triage: Vec<TriageEntry>,
}

fn refused(reason: impl Into<String>) -> ActionError {
    ActionError::Refused(reason.into())
}

fn maps(doc: &Doc) -> Result<Vec<ObjId>, ActionError> {
    doc.get_all(ROOT, REPORTS)
        .map_err(|error| refused(error.to_string()))?
        .into_iter()
        .map(|(value, id)| match value {
            Value::Object(ObjType::Map) => Ok(id),
            _ => Err(refused("invalid feedback report index")),
        })
        .collect()
}

/// Read every conflicting CRDT map/value: concurrent first submissions cannot
/// hide another report, and contradictory payloads never produce a receipt.
///
/// # Errors
/// Refuses corrupt metadata or conflicting author/payload bindings.
pub fn report(doc: &Doc, id: &str) -> Result<Option<Report>, ActionError> {
    let mut result: Option<Report> = None;
    for map in maps(doc)? {
        for (value, _) in doc
            .get_all(&map, id)
            .map_err(|error| refused(error.to_string()))?
        {
            let text = value
                .as_str()
                .ok_or_else(|| refused("invalid feedback report"))?;
            let incoming: Report =
                serde_json::from_str(text).map_err(|_| refused("invalid feedback report"))?;
            incoming
                .submission
                .validate()
                .map_err(|error| refused(error.to_string()))?;
            if incoming.submission.id != id {
                return Err(refused("feedback id conflict"));
            }
            match &mut result {
                Some(previous) => {
                    if previous.author != incoming.author
                        || previous.submission != incoming.submission
                    {
                        return Err(refused("feedback id conflict"));
                    }
                    previous.ts = previous.ts.min(incoming.ts);
                    for entry in incoming.triage {
                        if !previous.triage.contains(&entry) {
                            previous.triage.push(entry);
                        }
                    }
                }
                None => result = Some(incoming),
            }
        }
    }
    if let Some(report) = &mut result {
        report.triage.sort_by_key(|entry| {
            (
                entry.ts,
                entry.author.clone(),
                serde_json::to_string(&entry.update).unwrap_or_default(),
            )
        });
    }
    Ok(result)
}

/// Apply through Chat's signed action envelope; no payload author is trusted.
///
/// # Errors
/// Refuses missing attribution, conflicting retries, malformed submissions or
/// channels that cannot show the report. Existing history/messages are preserved.
pub fn apply(action: &FeedbackAction, doc: &mut Doc) -> Result<(), ActionError> {
    let author = crate::actions::author().ok_or_else(|| refused("feedback needs its author"))?;
    if let FeedbackAction::Triage {
        id,
        channel,
        update,
        ts,
    } = action
    {
        return apply_triage(
            doc,
            id,
            channel,
            TriageEntry {
                author,
                ts: *ts,
                update: update.clone(),
            },
        );
    }
    match action {
        FeedbackAction::Triage { .. } => Ok(()),
        FeedbackAction::Submit { submission, ts } => {
            submission
                .validate()
                .map_err(|error| refused(error.to_string()))?;
            if let Some(previous) = report(doc, &submission.id)? {
                return if previous.author == author && previous.submission == *submission {
                    Ok(())
                } else {
                    Err(refused("feedback id conflict"))
                };
            }
            let id = format!("m_feedback_{}", submission.id);
            let text = std::iter::once(submission.feedback.text.as_str())
                .chain(
                    submission
                        .feedback
                        .context
                        .iter()
                        .map(|field| field.value.as_str()),
                )
                .collect::<Vec<_>>()
                .join("\n\n");
            let attachment =
                submission
                    .feedback
                    .screenshot
                    .as_ref()
                    .map(|image| Attachment::Image {
                        name: "feedback.png".into(),
                        blob: image.hash.clone(),
                        w: image.width,
                        h: image.height,
                    });
            let channel = &submission.destination.channel;
            if !ChatDoc::list_channels(doc).iter().any(|candidate| {
                candidate.id == *channel && candidate.workspace == submission.destination.room
            }) {
                return Err(refused("feedback workspace mismatch"));
            }
            if !ChatDoc::accepts_message_action(doc, channel, &id, &text, None, attachment.as_ref())
            {
                return Err(refused("feedback destination cannot accept this report"));
            }
            let metadata = Report {
                submission: submission.clone(),
                author: author.clone(),
                ts: *ts,
                triage: Vec::new(),
            };
            let json =
                serde_json::to_string(&metadata).map_err(|error| refused(error.to_string()))?;
            let map = if let Some(map) = maps(doc)?.into_iter().next() {
                map
            } else {
                doc.put_object(ROOT, REPORTS, ObjType::Map)
                    .map_err(|error| refused(error.to_string()))?
            };
            doc.put(&map, &submission.id, json)
                .map_err(|error| refused(error.to_string()))?;
            ChatDoc::push_message_with_id(
                doc,
                channel,
                &id,
                NewMessage {
                    author,
                    ts: *ts,
                    text,
                    attachment,
                    ..NewMessage::default()
                },
            );
            Ok(())
        }
    }
}

fn apply_triage(
    doc: &mut Doc,
    id: &str,
    channel: &str,
    entry: TriageEntry,
) -> Result<(), ActionError> {
    let mut current = report(doc, id)?.ok_or_else(|| refused("feedback report not found"))?;
    if current.submission.destination.channel != channel {
        return Err(refused("feedback channel mismatch"));
    }
    match &entry.update {
        TriageUpdate::Duplicate { report: duplicate } => {
            if duplicate == id || report(doc, duplicate)?.is_none() {
                return Err(refused("duplicate target not found"));
            }
        }
        TriageUpdate::Task { reference } => {
            let bounded = |value: &str, max: usize| {
                !value.trim().is_empty()
                    && value.len() <= max
                    && !value.chars().any(char::is_control)
            };
            if !bounded(&reference.kind, 128)
                || !bounded(&reference.id, 2048)
                || !bounded(&reference.label, 512)
            {
                return Err(refused("invalid task reference"));
            }
        }
        _ => {}
    }
    if current.triage.contains(&entry) {
        return Ok(());
    }
    if current.triage.len() >= 128 {
        return Err(refused("feedback triage history is full"));
    }
    current.triage.push(entry);
    let json = serde_json::to_string(&current).map_err(|error| refused(error.to_string()))?;
    for map in maps(doc)? {
        if doc
            .get(&map, id)
            .map_err(|error| refused(error.to_string()))?
            .is_some()
        {
            doc.put(&map, id, json.clone())
                .map_err(|error| refused(error.to_string()))?;
        }
    }
    Ok(())
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
    let message = format!("m_feedback_{id}");
    if ChatDoc::message_author(doc, &actual_destination.channel, &message).as_deref()
        != Some(report.author.as_str())
    {
        return Err(refused("feedback message not found"));
    }
    Ok(Receipt {
        id: id.into(),
        digest: report
            .submission
            .digest()
            .map_err(|error| refused(error.to_string()))?,
        destination: actual_destination.clone(),
        report: format!("chat.message:{message}"),
    })
}

/// Pure canonical report projection for the system's feedback receipt service.
/// The owner service must check its exact `RecordStore`'s current durability;
/// this projection itself grants neither delivery confirmation nor authorship.
/// # Errors
/// Refuses missing reports, conflicting payloads or invalid attributed messages.
pub fn canonical_feedback_projection(
    doc: &Doc,
    submission: &Submission,
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

impl FeedbackAction {
    pub fn locus(&self) -> (&String, &String) {
        match self {
            Self::Submit { submission, .. } => (&submission.id, &submission.destination.channel),
            Self::Triage { id, channel, .. } => (id, channel),
        }
    }

    pub fn permission(&self) -> tilo_actions::PermissionRequirement {
        match self {
            Self::Submit { .. } => tilo_actions::PermissionRequirement::Comment,
            Self::Triage { .. } => tilo_actions::PermissionRequirement::Write,
        }
    }
}

/// Read the canonical reports without losing concurrent root maps.
///
/// # Errors
/// Refuses corrupt or contradictory report metadata.
pub fn reports(doc: &Doc) -> Result<Vec<Report>, ActionError> {
    let mut ids = std::collections::BTreeSet::new();
    for map in maps(doc)? {
        ids.extend(doc.keys(&map));
    }
    ids.into_iter()
        .map(|id| report(doc, &id)?.ok_or_else(|| refused("feedback report not found")))
        .collect()
}

/// The normal channel resource supplies the delivery action. Its builder pins
/// channel identity; the admitted transport supplies room/provider identity.
pub fn submit_spec(channel: &str) -> tilo_actions::ActionSpec {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType};
    let channel = channel.to_owned();
    ActionSpec::typed(
        "submit-feedback",
        "Submit reviewed feedback",
        "File an approved report with its stable retry identity",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "submission",
            ParamType::Object,
            "Approved bounded submission",
        )],
        move |args| {
            let submission = args
                .get("submission")
                .and_then(|value| serde_json::from_value::<Submission>(value.to_json()).ok());
            match submission.filter(|submission| {
                submission.validate().is_ok() && submission.destination.channel == channel
            }) {
                Some(submission) => crate::actions::encode(&crate::actions::ChatAction::Feedback(
                    FeedbackAction::Submit {
                        submission,
                        ts: tilo_clock::unix_now_i64(),
                    },
                )),
                None => EncodedAction {
                    type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                    payload: b"A reviewed submission for this channel is required".to_vec(),
                },
            }
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Comment)
}

/// Reports are visible to automation and accessibility beside their messages.
pub fn publish(report: &Report) -> tilo_actions::Resource {
    tilo_actions::Resource::new(
        format!("chat.feedback:{}", report.submission.id),
        "chat.feedback",
        report.submission.feedback.text.clone(),
    )
    .with_action(
        crate::effects::ChatEffect::RevealMessage {
            message: format!("m_feedback_{}", report.submission.id),
        }
        .bound(),
    )
    .with_action(receipt_read::spec(&report.submission.id))
    .with_action(status_read::spec(&report.submission.id))
    .with_action(triage_spec(report))
}

fn triage_spec(report: &Report) -> tilo_actions::ActionSpec {
    use tilo_actions::{ActionParam, ActionSpec, EncodedAction, ExecutionLocus, ParamType};
    let id = report.submission.id.clone();
    let channel = report.submission.destination.channel.clone();
    ActionSpec::typed(
        "triage",
        "Triage feedback",
        "Update status, name a duplicate or link an existing task",
        ExecutionLocus::ClientLocal,
        vec![ActionParam::required(
            "update",
            ParamType::Object,
            "Investigating, Resolved, Reopen, Duplicate or Task",
        )],
        move |args| match args
            .get("update")
            .and_then(|value| serde_json::from_value::<TriageUpdate>(value.to_json()).ok())
        {
            Some(update) => crate::actions::encode(&crate::actions::ChatAction::Feedback(
                FeedbackAction::Triage {
                    id: id.clone(),
                    channel: channel.clone(),
                    update,
                    ts: tilo_clock::unix_now_i64(),
                },
            )),
            None => EncodedAction {
                type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.into(),
                payload: b"A typed triage update is required".to_vec(),
            },
        },
    )
    .requiring(tilo_actions::PermissionRequirement::Write)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{ChatAction, apply_as};
    use tilo_actions::Action as _;
    use tilo_automerge::ApplyIncoming as _;
    use ui::feedback::ReviewedFeedback;

    fn seeded() -> (Doc, Submission) {
        let mut doc = Doc::new();
        ChatDoc::seed_if_empty(&mut doc);
        let channel = ChatDoc::list_channels(&doc)
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

    fn action(submission: &Submission) -> ChatAction {
        ChatAction::Feedback(FeedbackAction::Submit {
            submission: submission.clone(),
            ts: 123,
        })
    }

    #[test]
    fn retry_after_reload_keeps_one_attributed_message_and_report() {
        let (mut doc, submission) = seeded();
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
        let saved = doc.save();
        let mut restored = Doc::load(&saved).unwrap();
        apply_as(&mut restored, "visitor:one", &action(&submission)).unwrap();
        let messages = ChatDoc::list_messages(&restored, &submission.destination.channel);
        let matching: Vec<_> = messages
            .iter()
            .filter(|message| message.id == format!("m_feedback_{}", submission.id))
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
        let state =
            <crate::resource::Chat as ui::tilo_resource::Resource>::read(&restored).unwrap();
        assert!(
            <crate::resource::Chat as ui::tilo_resource::Resource>::child(
                &state,
                &format!("chat.feedback:{}", submission.id)
            )
            .is_some()
        );
        let report_id = format!("chat.feedback:{}", submission.id);
        let entries = <crate::resource::Chat as ui::tilo_resource::Resource>::search(&state);
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
        assert_eq!(
            matching[0].action,
            Some(tilo_actions::ActionAddress::new(
                &report_id,
                "reveal-message"
            ))
        );
        let published =
            <crate::resource::Chat as ui::tilo_resource::Resource>::child(&state, &report_id)
                .unwrap();
        assert!(published.actions.iter().any(|spec| spec.id == "triage"
            && spec.contract.permission == tilo_actions::PermissionRequirement::Write));
    }

    #[test]
    fn unauthored_and_conflicting_retries_are_refused_without_changes() {
        let (mut doc, submission) = seeded();
        assert!(action(&submission).apply(&mut doc).is_err());
        assert!(reports(&doc).unwrap().is_empty());
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
        let before = doc.get_heads();
        assert!(apply_as(&mut doc, "visitor:other", &action(&submission)).is_err());
        let mut changed = submission;
        changed.feedback.text = "different".into();
        assert!(apply_as(&mut doc, "visitor:one", &action(&changed)).is_err());
        assert_eq!(doc.get_heads(), before);
    }

    #[test]
    fn concurrent_first_submissions_survive_conflicting_root_maps() {
        let (mut doc, submission) = seeded();
        let mut peer = doc.fork();
        let mut other = submission.clone();
        other.id = "fedcba9876543210fedcba9876543210".into();
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
        apply_as(&mut peer, "visitor:two", &action(&other)).unwrap();
        doc.merge_incoming(&mut peer).unwrap();
        assert_eq!(reports(&doc).unwrap().len(), 2);
        assert!(report(&doc, &submission.id).unwrap().is_some());
        assert!(report(&doc, &other.id).unwrap().is_some());
    }

    #[test]
    fn triage_is_attributed_preserves_issue_and_uses_write_permission() {
        use ui::tilo_resource::Resource as _;
        let (mut doc, submission) = seeded();
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
        let update = ChatAction::Feedback(FeedbackAction::Triage {
            id: submission.id.clone(),
            channel: submission.destination.channel.clone(),
            update: TriageUpdate::Investigating,
            ts: 456,
        });
        assert_eq!(
            crate::resource::Chat::permission(&action(&submission)),
            tilo_actions::PermissionRequirement::Comment
        );
        assert_eq!(
            crate::resource::Chat::permission(&update),
            tilo_actions::PermissionRequirement::Write
        );
        apply_as(&mut doc, "maintainer:one", &update).unwrap();
        apply_as(&mut doc, "maintainer:one", &update).unwrap();
        let report = report(&doc, &submission.id).unwrap().unwrap();
        assert_eq!(report.submission, submission);
        assert_eq!(report.author, "visitor:one");
        assert_eq!(report.triage.len(), 1);
        assert_eq!(report.triage[0].author, "maintainer:one");
        assert!(
            publish(&report)
                .actions
                .iter()
                .any(|spec| spec.id == "triage")
        );
    }

    #[test]
    fn concurrent_retry_deduplicates_the_visible_report_message() {
        let (mut doc, submission) = seeded();
        let mut peer = doc.fork();
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
        apply_as(&mut peer, "visitor:one", &action(&submission)).unwrap();
        doc.merge_incoming(&mut peer).unwrap();
        assert_eq!(reports(&doc).unwrap().len(), 1);
        assert_eq!(
            ChatDoc::list_messages(&doc, &submission.destination.channel)
                .iter()
                .filter(|message| message.id == format!("m_feedback_{}", submission.id))
                .count(),
            1
        );
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
                ChatDoc::seed_if_empty(doc);
                apply_as(doc, "visitor:one", &action(&submission))
                    .map_err(|error| error.to_string())
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
            .with(&checkout.id, |doc| {
                ChatDoc::seed_if_empty(doc);
                apply_as(doc, "visitor:one", &action(&submission)).unwrap();
            })
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
                ChatDoc::seed_if_empty(doc);
                apply_as(doc, "visitor:one", &action(&submission))
                    .map_err(|error| error.to_string())
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
        apply_as(&mut doc, "visitor:one", &action(&submission)).unwrap();
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
        let mut elsewhere = submission.destination.clone();
        elsewhere.room = "other-room".into();
        assert!(
            durable_receipt(&doc, &submission.id, &elsewhere, &RecordSaveStatus::Saved).is_err()
        );
    }
}
