//! The Store document: collaborative report drafts, their notes and the
//! moderation case each one became.
//!
//! Everything else the Store shows — the catalogue, what is installed, the
//! Flatpak remotes — is live device state its surface publishes as rows.

use automerge::transaction::Transactable;
use automerge::{ObjId, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{Action, ActionError, ActionRegistry, Doc};
use ui::tilo_resource::TextEdit;

use crate::resource::{ReportProjection, StoreState};

pub const STORE_DOCUMENT_ACTION_TYPE: &str = "store.document";
pub const REPORT_RESOURCE_KEY_PREFIX: &str = "store.report_resource.";
pub const REPORT_CASE_KEY_PREFIX: &str = "store.report_case.";
pub const REPORT_NOTE_KEY_PREFIX: &str = "store.report_note.";
const REPORT_SNAPSHOT_LIMIT: usize = 128;
const MAX_REPORT_STATUS_CHARS: usize = 128;
pub const MAX_REPORT_NOTE_CHARS: usize = 4_096;
const MAX_CASE_ID_CHARS: usize = 128;
/// The status a report has until the registry knows of it.
pub const DRAFT_STATUS: &str = "Draft · not submitted";

pub(crate) fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreDocumentAction {
    /// Start a report about an app, with an empty note to describe it in.
    CreateReportDraft { app_id: String },
    /// Edit the note of an app's report.
    EditReportNote { app_id: String, edit: TextEdit },
    /// Record the registry's case id and status for one app's report.
    RecordModerationCase {
        app_id: String,
        case_id: String,
        status: String,
    },
}

impl Action for StoreDocumentAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            Self::CreateReportDraft { app_id } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) {
                    return Err(ActionError::Refused(
                        "invalid app id for report draft".into(),
                    ));
                }
                doc.put(
                    &ROOT,
                    format!("{REPORT_RESOURCE_KEY_PREFIX}{app_id}"),
                    DRAFT_STATUS,
                )?;
                note_object(doc, app_id)?;
            }
            Self::EditReportNote { app_id, edit } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) {
                    return Err(ActionError::Refused(
                        "invalid app id for report note".into(),
                    ));
                }
                let note = note_object(doc, app_id)?;
                let current = doc.text(&note)?.chars().count();
                let edited = match edit {
                    TextEdit::Splice { delete, insert, .. } => current
                        .saturating_sub(*delete)
                        .saturating_add(insert.chars().count()),
                    TextEdit::ReplaceText { text } => text.chars().count(),
                };
                if edited > MAX_REPORT_NOTE_CHARS {
                    return Err(ActionError::Refused(format!(
                        "a report note holds at most {MAX_REPORT_NOTE_CHARS} characters"
                    )));
                }
                edit.apply_under(doc, &note)?;
            }
            Self::RecordModerationCase {
                app_id,
                case_id,
                status,
            } => {
                let app_id = app_id.trim();
                let case_id = case_id.trim();
                if !valid_app_id(app_id)
                    || !valid_case_id(case_id)
                    || status.is_empty()
                    || status.chars().count() > MAX_REPORT_STATUS_CHARS
                {
                    return Err(ActionError::Refused(
                        "invalid moderation case record".into(),
                    ));
                }
                doc.put(&ROOT, format!("{REPORT_CASE_KEY_PREFIX}{app_id}"), case_id)?;
                doc.put(
                    &ROOT,
                    format!("{REPORT_RESOURCE_KEY_PREFIX}{app_id}"),
                    status.as_str(),
                )?;
            }
        }
        Ok(())
    }
}

/// The note text of `app_id`'s report, created when the report has none.
fn note_object(doc: &mut Doc, app_id: &str) -> Result<ObjId, ActionError> {
    let key = format!("{REPORT_NOTE_KEY_PREFIX}{app_id}");
    match doc.get(&ROOT, &key)? {
        Some((Value::Object(ObjType::Text), note)) => Ok(note),
        Some(_) => Err(ActionError::Refused("report note is not text".into())),
        None => Ok(doc.put_object(&ROOT, key, ObjType::Text)?),
    }
}

/// The note text of `app_id`'s report in `doc`, if it has one.
pub fn report_note(doc: &Doc, app_id: &str) -> Option<ObjId> {
    match doc.get(&ROOT, format!("{REPORT_NOTE_KEY_PREFIX}{app_id}")) {
        Ok(Some((Value::Object(ObjType::Text), note))) => Some(note),
        _ => None,
    }
}

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<StoreDocumentAction>(STORE_DOCUMENT_ACTION_TYPE);
}

pub(crate) fn valid_case_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_CASE_ID_CHARS
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == ':')
}

fn note_at(doc: &Doc, app_id: &str) -> String {
    report_note(doc, app_id)
        .and_then(|note| doc.text(&note).ok())
        .unwrap_or_default()
        .chars()
        .take(MAX_REPORT_NOTE_CHARS)
        .collect()
}

/// The reports `doc` holds, by app.
pub(crate) fn state_from_doc(doc: &Doc) -> StoreState {
    let mut reports = doc
        .keys(&ROOT)
        .filter_map(|key| {
            let app_id = key.strip_prefix(REPORT_RESOURCE_KEY_PREFIX)?.to_string();
            if !valid_app_id(&app_id) {
                return None;
            }
            let status = doc
                .get(&ROOT, &key)
                .ok()
                .flatten()?
                .0
                .as_str()?
                .chars()
                .take(MAX_REPORT_STATUS_CHARS)
                .collect::<String>();
            let id = doc
                .get(&ROOT, format!("{REPORT_CASE_KEY_PREFIX}{app_id}"))
                .ok()
                .flatten()
                .and_then(|(value, _)| value.as_str().map(str::to_owned))
                .filter(|value| valid_case_id(value))
                .unwrap_or_else(|| format!("report:{app_id}"));
            Some(ReportProjection {
                id,
                note: note_at(doc, &app_id),
                app_id,
                status,
            })
        })
        .take(REPORT_SNAPSHOT_LIMIT)
        .collect::<Vec<_>>();
    reports.sort_by(|a, b| a.app_id.cmp(&b.app_id));
    StoreState { reports }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automerge::ApplyIncoming;

    fn note(doc: &Doc) -> String {
        note_at(doc, "so.tilo.notes")
    }

    fn draft(doc: &mut Doc) {
        StoreDocumentAction::CreateReportDraft {
            app_id: "so.tilo.notes".into(),
        }
        .apply(doc)
        .unwrap();
    }

    fn type_note(doc: &mut Doc, at: usize, insert: &str) -> Result<(), ActionError> {
        StoreDocumentAction::EditReportNote {
            app_id: "so.tilo.notes".into(),
            edit: TextEdit::Splice {
                at,
                delete: 0,
                insert: insert.into(),
            },
        }
        .apply(doc)
    }

    /// A draft starts with its note text, keeps a note it already has,
    /// bounds the note, and two people typing in one note merge.
    #[test]
    fn report_drafts_carry_a_bounded_collaborative_note() {
        let mut doc = Doc::new();
        draft(&mut doc);
        assert_eq!(
            doc.get(&ROOT, "store.report_resource.so.tilo.notes")
                .unwrap()
                .and_then(|(value, _)| value.into_scalar().ok())
                .and_then(|value| value.into_string().ok())
                .as_deref(),
            Some(DRAFT_STATUS)
        );
        assert!(report_note(&doc, "so.tilo.notes").is_some());

        type_note(&mut doc, 0, "Unsigned binary").unwrap();
        draft(&mut doc);
        assert_eq!(note(&doc), "Unsigned binary", "a new draft keeps the note");
        assert!(
            type_note(&mut doc, 0, &"x".repeat(MAX_REPORT_NOTE_CHARS)).is_err(),
            "the note is bounded"
        );
        assert_eq!(note(&doc), "Unsigned binary");
        assert!(
            StoreDocumentAction::CreateReportDraft {
                app_id: "../x".into()
            }
            .apply(&mut doc)
            .is_err()
        );

        let bytes = doc.save();
        let mut left = Doc::load(&bytes).unwrap();
        let mut right = Doc::load(&bytes).unwrap();
        type_note(&mut left, 0, "Seen offline: ").unwrap();
        type_note(&mut right, 15, " in 0.2").unwrap();
        left.merge_incoming(&mut right).unwrap();
        assert_eq!(note(&left), "Seen offline: Unsigned binary in 0.2");
    }

    /// The registry's case is recorded with its status, and a malformed
    /// case id is refused.
    #[test]
    fn a_moderation_case_is_recorded_on_its_report() {
        let mut doc = Doc::new();
        draft(&mut doc);
        StoreDocumentAction::RecordModerationCase {
            app_id: "so.tilo.notes".into(),
            case_id: "case-authoritative".into(),
            status: "Open".into(),
        }
        .apply(&mut doc)
        .unwrap();
        let state = state_from_doc(&doc);
        assert_eq!(state.reports.len(), 1);
        assert_eq!(state.reports[0].id, "case-authoritative");
        assert_eq!(state.reports[0].status, "Open");
        assert!(
            StoreDocumentAction::RecordModerationCase {
                app_id: "so.tilo.notes".into(),
                case_id: "case-../../outside".into(),
                status: "Open".into(),
            }
            .apply(&mut doc)
            .is_err()
        );
    }
}
