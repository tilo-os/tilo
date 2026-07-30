//! Books' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Annotations already live in the shared CRDT (`root["annotations"]`, keyed by
//! uuid — see [`crate::doc`]), so the actions here mutate that exact store: they
//! replicate + replay for free and feed straight back into the reader's
//! annotation list. Editing a note and deleting an annotation are
//! [`ExecutionLocus::ClientLocal`]; adding a note is too (a personal mark).
//!
//! Presentation — which book is open, the live page, the selection — stays out
//! of the document and never becomes an action.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc, Value, ROOT};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::BooksDoc;
use crate::model::{AnnoKind, Annotation, HiColor};

/// Registry `type_id` for every Books action.
pub const EBOOK_ACTION_TYPE: &str = "ebook.action";

/// Every collaborative operation Books exports on its annotation store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EbookAction {
    /// Replace an annotation's free-text note.
    EditAnnotationNote { id: String, note: String },
    /// Delete an annotation row.
    DeleteAnnotation { id: String },
    /// Add a new free-text note anchored to a book. `id` is minted at authoring
    /// time (carried in the payload) so replay is deterministic — never inside
    /// `apply`, which must not call the RNG.
    AddNote {
        book_id: String,
        id: String,
        text: String,
    },
}

impl Action for EbookAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            EbookAction::EditAnnotationNote { id, note } => {
                if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")? {
                    if let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id.as_str())? {
                        doc.put(&entry, "note", note.as_str())?;
                    }
                }
            }
            EbookAction::DeleteAnnotation { id } => {
                if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")? {
                    doc.delete(&map, id.as_str())?;
                }
            }
            EbookAction::AddNote { book_id, id, text } => {
                // A malformed id (never produced by our own builder) is a no-op
                // rather than an error, so a replay can't wedge on bad input.
                if let Ok(uuid) = id.parse::<Uuid>() {
                    let anno = Annotation {
                        id: uuid,
                        doc_id: book_id.clone(),
                        kind: AnnoKind::Note,
                        chapter: 0,
                        block: 0,
                        start: 0,
                        end: 0,
                        excerpt: String::new(),
                        note: text.clone(),
                        color: HiColor::Yellow,
                        // Deterministic: no wall-clock in `apply`.
                        created: 0,
                        group: uuid,
                    };
                    BooksDoc::upsert_annotation(doc, &anno);
                }
            }
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Books' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<EbookAction>(EBOOK_ACTION_TYPE);
}

fn encode(action: &EbookAction) -> EncodedAction {
    EncodedAction::encode(EBOOK_ACTION_TYPE, action).expect("EbookAction is always serializable")
}

/// One-line annotation preview for the palette label.
fn anno_label(a: &Annotation) -> String {
    let body = if !a.note.trim().is_empty() {
        a.note.trim()
    } else {
        a.excerpt.trim()
    };
    let mut t: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 48 {
        t = t.chars().take(45).collect::<String>() + "…";
    }
    if t.is_empty() {
        a.kind.label().to_string()
    } else {
        format!("{} — {}", a.kind.label(), t)
    }
}

/// Build the resource tree Books currently has open: the book being read, with
/// its annotations as nested resources. The book carries an "Add note" action;
/// each annotation carries edit (prompted) + delete.
pub fn provide_resources(book_id: &str, book_title: &str, doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new(
        format!("ebook.book:{book_id}"),
        "ebook.book",
        book_title.to_string(),
    );

    // Book-level: add a note. The uuid is minted when the reply/argument is
    // committed (once), then baked into the envelope, so replay is deterministic.
    let bid = book_id.to_string();
    root = root.with_action(ActionSpec::prompted(
        "add-note",
        "Add note",
        "Note",
        "your note…",
        ExecutionLocus::ClientLocal,
        move |text: &str| {
            encode(&EbookAction::AddNote {
                book_id: bid.clone(),
                id: Uuid::new_v4().to_string(),
                text: text.to_string(),
            })
        },
    ));

    for a in BooksDoc::annotations_for(doc, book_id) {
        let id = a.id.to_string();
        let edit_id = id.clone();
        let child = Resource::new(
            format!("ebook.annotation:{id}"),
            "ebook.annotation",
            anno_label(&a),
        )
        .with_action(ActionSpec::prompted(
            "edit",
            "Edit note",
            "Note",
            "edit the note…",
            ExecutionLocus::ClientLocal,
            move |text: &str| {
                encode(&EbookAction::EditAnnotationNote {
                    id: edit_id.clone(),
                    note: text.to_string(),
                })
            },
        ))
        .with_action(
            ActionSpec::immediate(
                "delete",
                "Delete annotation",
                EBOOK_ACTION_TYPE,
                &EbookAction::DeleteAnnotation { id: id.clone() },
            )
            .expect("encode delete"),
        );
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::BooksApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.open_book() {
            Some(book) => provide_resources(&book.id, &book.title, doc),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BooksApp;
    use qos_actions::{ActionInput, ActionRegistry, LocalTransport};
    use ui::widget::Widget;

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    /// A reader with a book open and one seeded annotation on it, in the CRDT.
    fn open_with_one_annotation() -> (BooksApp, AutoCommit, String, Uuid) {
        let app = BooksApp::with_library(crate::content::load_sample_library());
        let book_id = app.open_book().expect("sample library is non-empty").id.clone();
        let mut doc = AutoCommit::new();
        let aid = Uuid::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        let anno = Annotation {
            id: aid,
            doc_id: book_id.clone(),
            kind: AnnoKind::Highlight,
            chapter: 0,
            block: 3,
            start: 0,
            end: 12,
            excerpt: "a passage".into(),
            note: "original note".into(),
            color: HiColor::Green,
            created: 1,
            group: aid,
        };
        BooksDoc::upsert_annotation(&mut doc, &anno);
        (app, doc, book_id, aid)
    }

    /// Ctrl+K on Books lists the open book + its annotations, each with edit +
    /// delete actions. Drives the real resource-collection hook + palette.
    #[test]
    fn ctrl_k_lists_open_book_and_annotations() {
        let (app, doc, _book, _aid) = open_with_one_annotation();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one open book");
        let book = &resources[0];
        assert_eq!(book.kind, "ebook.book");
        assert!(
            book.actions.iter().any(|a| a.id == "add-note"),
            "book exports add-note"
        );
        assert_eq!(book.children.len(), 1, "the seeded annotation is a child");
        let anno = &book.children[0];
        assert_eq!(anno.kind, "ebook.annotation");
        let ids: Vec<&str> = anno.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(ids.contains(&"edit"), "annotation exports edit");
        assert!(ids.contains(&"delete"), "annotation exports delete");

        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Delete annotation")), "{labels:?}");
        assert!(labels.iter().any(|l| l.contains("Edit note")));
    }

    /// Executing the immediate "Delete annotation" action removes it from the
    /// CRDT — driven through the real palette activate path.
    #[test]
    fn executing_delete_annotation_mutates_crdt() {
        let (app, mut doc, book_id, _aid) = open_with_one_annotation();
        assert_eq!(BooksDoc::annotations_for(&doc, &book_id).len(), 1);

        let resources = Widget::resources(&app, &doc);
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();

        let idx = palette
            .result_labels()
            .iter()
            .position(|l| l.contains("Delete annotation"))
            .expect("delete row present");
        palette.set_cursor(idx);
        palette.activate_selected();
        let mut tx = LocalTransport::new();
        let d = palette
            .take_dispatch(&mut doc, &registry(), &mut tx)
            .expect("selection pending")
            .expect("dispatch ok");
        assert!(matches!(d, qos_actions::Dispatched::AppliedLocal(_)));
        assert_eq!(
            BooksDoc::annotations_for(&doc, &book_id).len(),
            0,
            "annotation deleted from the CRDT"
        );
    }

    /// The prompted "Edit note" action: its build closure turns a typed argument
    /// into a replayable action that, dispatched, rewrites the note in the CRDT.
    /// (The palette's argument-typing UI is exercised by `command_palette`'s own
    /// tests; here we prove the resource→build→decode→apply pipeline.)
    #[test]
    fn prompted_edit_note_builds_and_applies() {
        let (app, mut doc, book_id, aid) = open_with_one_annotation();
        let resources = Widget::resources(&app, &doc);
        let anno_res = &resources[0].children[0];
        let edit = anno_res
            .actions
            .iter()
            .find(|a| a.id == "edit")
            .expect("edit action");

        let encoded = match &edit.input {
            ActionInput::Prompted { build, .. } => build("a better note"),
            _ => panic!("edit is a prompted action"),
        };
        // Decode + dispatch exactly as the palette does on Enter.
        let action = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, action.as_ref(), encoded, &mut tx).unwrap();

        let updated = BooksDoc::annotations_for(&doc, &book_id)
            .into_iter()
            .find(|a| a.id == aid)
            .expect("annotation still present");
        assert_eq!(updated.note, "a better note", "note rewritten in the CRDT");
    }
}
