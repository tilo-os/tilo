//! Replayable Store-document actions for durable, collaborative reports.
//!
//! Resource-tree builders for [`tilo_resource::publish`] live here so the
//! palette door and the CRDT mutations stay one vocabulary.

use automerge::transaction::Transactable;
use automerge::{ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use tilo_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
};

use crate::resource::{ReportProjection, StoreState};

pub const STORE_DOCUMENT_ACTION_TYPE: &str = "store.document";
pub const REPORT_RESOURCE_KEY_PREFIX: &str = "store.report_resource.";
pub const REPORT_CASE_KEY_PREFIX: &str = "store.report_case.";
pub const REPORT_NOTE_KEY_PREFIX: &str = "store.report_note.";
const REPORT_SNAPSHOT_LIMIT: usize = 128;
const MAX_REPORT_STATUS_CHARS: usize = 128;
const MAX_REPORT_NOTE_CHARS: usize = 4_096;
const MAX_CASE_ID_CHARS: usize = 128;

fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum StoreDocumentAction {
    CreateReportDraft {
        app_id: String,
    },
    SetReportNote {
        app_id: String,
        note: String,
    },
    /// Create the shared note text when a report has none yet.
    EnsureReportNote {
        app_id: String,
    },
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
                    "Not submitted",
                )?;
            }
            Self::SetReportNote { app_id, note } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) || note.chars().count() > 4_096 {
                    return Err(ActionError::Refused("invalid report note".into()));
                }
                // An absolute agent operation replaces the text object rather
                // than splicing an existing one. Concurrent absolute writes
                // therefore resolve to one authored value instead of
                // interleaving sentences; ordinary on-screen typing remains
                // character-granular CRDT text within the chosen object.
                let object =
                    doc.put_object(&ROOT, format!("store.report_note.{app_id}"), ObjType::Text)?;
                if !note.is_empty() {
                    doc.splice_text(&object, 0, 0, note)?;
                }
            }
            Self::EnsureReportNote { app_id } => {
                let app_id = app_id.trim();
                if !valid_app_id(app_id) {
                    return Err(ActionError::Refused(
                        "invalid app id for report note".into(),
                    ));
                }
                let key = format!("{REPORT_NOTE_KEY_PREFIX}{app_id}");
                match doc.get(&ROOT, &key)? {
                    Some((Value::Object(ObjType::Text), _)) => {}
                    Some(_) => {
                        return Err(ActionError::Refused("report note is not text".into()));
                    }
                    None => {
                        doc.put_object(&ROOT, key, ObjType::Text)?;
                    }
                }
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

pub fn register(registry: &mut ActionRegistry) {
    registry.register::<StoreDocumentAction>(STORE_DOCUMENT_ACTION_TYPE);
}

fn valid_case_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_CASE_ID_CHARS
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == ':')
}

fn note_at(doc: &Doc, app_id: &str) -> String {
    ui::doc::read_text(doc, &ROOT, format!("{REPORT_NOTE_KEY_PREFIX}{app_id}"))
        .unwrap_or_default()
        .chars()
        .take(MAX_REPORT_NOTE_CHARS)
        .collect()
}

/// Shared report projection for [`tilo_resource::publish`].
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

/// Moderation child with report drafts / set-note for the framework door.
pub(crate) fn report_document_children(state: &StoreState) -> Vec<Resource> {
    let mut moderation = Resource::new(
        "store:moderation",
        "store.moderation",
        "Trust and moderation",
    );
    if let Ok(show) = ActionSpec::operation(
        "show",
        "Show trust and moderation",
        tilo_actions::OperationKind::View,
        "store.view-trust",
        &(),
    ) {
        moderation.actions.push(show);
    }
    for report in &state.reports {
        let mut report_resource = Resource::new(
            report.id.clone(),
            "store.report",
            format!("{}: {}", report.app_id, report.status),
        );
        let note_app_id = report.app_id.clone();
        report_resource.actions.push(ActionSpec::prompted(
            "set-note",
            "Set report concern",
            "Concern and supporting evidence",
            "Describe what happened and how it can be verified",
            ExecutionLocus::ClientLocal,
            move |note| {
                EncodedAction::encode(
                    STORE_DOCUMENT_ACTION_TYPE,
                    &StoreDocumentAction::SetReportNote {
                        app_id: note_app_id.clone(),
                        note: note.to_owned(),
                    },
                )
                .unwrap_or_else(|_| EncodedAction {
                    type_id: STORE_DOCUMENT_ACTION_TYPE.into(),
                    payload: Vec::new(),
                })
            },
        ));
        if !report.note.trim().is_empty() {
            report_resource.children.push(Resource::new(
                format!("{}:note", report.id),
                "store.report-note",
                report.note.clone(),
            ));
        }
        moderation.children.push(report_resource);
    }
    vec![moderation]
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{ReadDoc, Value};
    use tilo_automerge::ApplyIncoming;
    use ui::widget::Widget;

    fn note(doc: &Doc) -> String {
        doc.get(&ROOT, "store.report_note.so.tilo.notes")
            .unwrap()
            .and_then(|(value, id)| matches!(value, Value::Object(ObjType::Text)).then_some(id))
            .and_then(|id| doc.text(&id).ok())
            .unwrap_or_default()
    }

    fn registry() -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        register(&mut registry);
        registry
    }

    fn encode(action: &StoreDocumentAction) -> tilo_actions::EncodedAction {
        tilo_actions::EncodedAction::encode(STORE_DOCUMENT_ACTION_TYPE, action)
            .expect("store document actions serialize")
    }

    /// The Store as the live host holds it: the screenshot catalogue, with
    /// report records hydrated from `doc` the way every frame's `sync` does.
    fn synced_app(doc: &mut Doc) -> crate::app::StoreApp {
        let mut app = crate::app::StoreApp::new();
        app.set_flatpak_available(true);
        app.set_items(crate::shots::fixture_catalog());
        app.sync(doc, &ROOT);
        app.sync(doc, &ROOT);
        app
    }

    /// A document holding one report draft for Notes, written by the
    /// dispatched draft action and then reconciled by the app's own sync
    /// (which materialises the draft's shared note text).
    fn seeded_doc() -> Doc {
        let mut doc = Doc::new();
        let draft = StoreDocumentAction::CreateReportDraft {
            app_id: "so.tilo.notes".into(),
        };
        tilo_actions::apply_local(&mut doc, &draft, encode(&draft)).expect("draft applies");
        synced_app(&mut doc);
        doc.commit();
        doc
    }

    ui::action_contract_tests! {
        seed: seeded_doc(),
        registry: registry(),
        resources: |doc| ui::resources::resources(&synced_app(&mut doc.clone()), doc),
        palette: ["Create report draft", "Set report concern", "Refresh catalog", "Search apps"],
        replay: [
            draft_report => |_| encode(&StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.terminal".into(),
            }),
            set_report_note => |_| encode(&StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: "The listing omits a network dependency.".into(),
            }),
        ],
        undo: [
            set_report_note => |_| encode(&StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: "The listing omits a network dependency.".into(),
            }),
            draft_report => |_| encode(&StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.files".into(),
            }),
        ],
        encodes: [
            draft_report => ("app:so.tilo.terminal", "draft-report", |_, _| {
                encode(&StoreDocumentAction::CreateReportDraft {
                    app_id: "so.tilo.terminal".into(),
                })
            }),
            set_report_note => ("report:so.tilo.notes", "set-note", arg: "Unsigned binary", |_, _| {
                encode(&StoreDocumentAction::SetReportNote {
                    app_id: "so.tilo.notes".into(),
                    note: "Unsigned binary".into(),
                })
            }),
        ],
    }

    /// Report drafts and notes are replayable document resources that converge without interleaving.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario / publish-ownership assertion kept top-to-bottom"
    )]
    fn report_drafts_are_bounded_replayable_and_converge() {
        // Case `report_draft_is_a_replayable_document_resource`.
        {
            let mut doc = Doc::new();
            StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.notes".into(),
            }
            .apply(&mut doc)
            .unwrap();
            assert!(
                doc.get(&ROOT, "store.report_resource.so.tilo.notes")
                    .unwrap()
                    .is_some()
            );
        }

        // Case `absolute_report_note_is_bounded_and_replayable`.
        {
            let action = StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: "The listing omits a network dependency.".into(),
            };
            let mut doc = Doc::new();
            action.apply(&mut doc).unwrap();
            assert_eq!(note(&doc), "The listing omits a network dependency.");
            action.apply(&mut doc).unwrap();
            assert_eq!(note(&doc), "The listing omits a network dependency.");
            assert!(
                StoreDocumentAction::SetReportNote {
                    app_id: "so.tilo.notes".into(),
                    note: "x".repeat(4_097),
                }
                .apply(&mut Doc::new())
                .is_err()
            );
        }

        // Case `divergent_absolute_report_notes_converge_without_sentence_interleaving`.
        {
            let mut seed = Doc::new();
            StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.notes".into(),
            }
            .apply(&mut seed)
            .unwrap();
            let bytes = seed.save();
            let mut left = Doc::load(&bytes).unwrap();
            let mut right = Doc::load(&bytes).unwrap();
            for (doc, value) in [(&mut left, "left evidence"), (&mut right, "right evidence")] {
                StoreDocumentAction::SetReportNote {
                    app_id: "so.tilo.notes".into(),
                    note: value.into(),
                }
                .apply(doc)
                .unwrap();
            }
            let mut left_then_right = left.clone();
            left_then_right.merge_incoming(&mut right.clone()).unwrap();
            let mut right_then_left = right;
            right_then_left.merge_incoming(&mut left).unwrap();
            let resolved = note(&left_then_right);
            assert_eq!(resolved, note(&right_then_left));
            assert!(matches!(
                resolved.as_str(),
                "left evidence" | "right evidence"
            ));
        }

        // Case `a_moderation_case_is_a_replayable_document_action`.
        {
            let mut doc = Doc::new();
            StoreDocumentAction::RecordModerationCase {
                app_id: "so.tilo.notes".into(),
                case_id: "case-authoritative".into(),
                status: "Open".into(),
            }
            .apply(&mut doc)
            .unwrap();
            assert_eq!(
                doc.get(&ROOT, "store.report_case.so.tilo.notes")
                    .unwrap()
                    .and_then(|(value, _)| value.as_str().map(str::to_owned)),
                Some("case-authoritative".into())
            );
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

        // Case `a_missing_report_note_is_created_by_the_action`.
        {
            let mut doc = Doc::new();
            StoreDocumentAction::CreateReportDraft {
                app_id: "so.tilo.notes".into(),
            }
            .apply(&mut doc)
            .unwrap();
            let _app = synced_app(&mut doc);
            assert!(
                matches!(
                    doc.get(&ROOT, "store.report_note.so.tilo.notes").unwrap(),
                    Some((Value::Object(ObjType::Text), _))
                ),
                "sync creates the note through the document action"
            );
            StoreDocumentAction::SetReportNote {
                app_id: "so.tilo.notes".into(),
                note: "kept".into(),
            }
            .apply(&mut doc)
            .unwrap();
            StoreDocumentAction::EnsureReportNote {
                app_id: "so.tilo.notes".into(),
            }
            .apply(&mut doc)
            .unwrap();
            assert_eq!(note(&doc), "kept");
            assert!(
                StoreDocumentAction::EnsureReportNote {
                    app_id: "../x".into(),
                }
                .apply(&mut doc)
                .is_err()
            );
        }
    }
}
