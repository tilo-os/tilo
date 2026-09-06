//! Books' exported **actions** and the **resource tree** it publishes to Ctrl+K.
//!
//! Every document-mutating operation on the shared annotation store is an
//! [`EbookAction`]: a serializable value that deterministically mutates the
//! CRDT. The exact same [`EncodedAction`] is what on-page affordances (the
//! selection-toolbar swatch, the comment overlay's buttons) dispatch and what
//! the Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The resource tree mirrors the data model: a book owns its highlights,
//! bookmarks and notes; a highlight owns its comments.
//!
//! Presentation state (open book, live page, selection, overlay) stays out
//! of the document and never becomes an action.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ROOT, ReadDoc, Value};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use qos_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource, ResourceProvider,
    ResultSpec,
};

use crate::doc::BooksDoc;
use crate::model::{AnnoKind, Annotation, HiColor, Progress};

/// Registry `type_id` for every Books action.
pub const EBOOK_ACTION_TYPE: &str = "ebook.action";
pub const EBOOK_OPERATION_TYPE: &str = "ebook.operation";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EbookOperation {
    PreviousPage,
    NextPage,
    ToggleContents,
    ToggleNotes,
    OpenSettings,
    /// Reveal an existing library resource without changing the shared
    /// document. `block` is present for annotation hits and absent for a book
    /// or chapter row.
    Reveal {
        book_id: String,
        chapter: usize,
        page: usize,
        block: Option<usize>,
    },
    OpenBook {
        path: Option<String>,
    },
}

pub fn operation_kind(operation: &EbookOperation) -> OperationKind {
    match operation {
        EbookOperation::PreviousPage
        | EbookOperation::NextPage
        | EbookOperation::ToggleContents
        | EbookOperation::ToggleNotes
        | EbookOperation::OpenSettings
        | EbookOperation::Reveal { .. } => OperationKind::View,
        EbookOperation::OpenBook { .. } => OperationKind::Effect,
    }
}

#[cfg(test)]
mod file_action_tests {
    use super::*;
    #[test]
    fn path_argument_binds_into_background_open() {
        let spec = open_book_operation();
        let encoded = spec
            .build_json(
                serde_json::json!({"path":"/tmp/a.epub"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(EbookOperation::OpenBook {
                path: Some("/tmp/a.epub".into())
            })
        );
        assert!(matches!(
            spec.contract.execution,
            ExecutionMode::Background { .. }
        ));
        assert_eq!(spec.contract.result, ResultSpec::Job);
    }
}

/// Decode a presentation operation when `type_id` belongs to Books.
///
/// # Errors
///
/// Returns a descriptive JSON error when a Books payload is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<EbookOperation>, String> {
    if type_id != EBOOK_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::needless_pass_by_value,
    clippy::expect_used,
    reason = "the owned enum makes call sites legible and this fieldless/string-only operation schema is infallibly serializable"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: EbookOperation) -> ActionSpec {
    ActionSpec::operation(id, label, kind, EBOOK_OPERATION_TYPE, &value)
        .expect("EbookOperation is always serializable")
}

fn reveal_operation(
    book_id: &str,
    chapter: usize,
    page: usize,
    block: Option<usize>,
) -> ActionSpec {
    operation(
        "open",
        "Open",
        OperationKind::View,
        EbookOperation::Reveal {
            book_id: book_id.to_string(),
            chapter,
            page,
            block,
        },
    )
}

#[allow(
    clippy::expect_used,
    reason = "EbookOperation contains only JSON-representable strings and integers"
)]
fn open_book_operation() -> ActionSpec {
    ActionSpec::typed_operation(
        "open-book",
        "Open book",
        "optional absolute path",
        OperationKind::Effect,
        EBOOK_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "EPUB path; omit to use the system picker",
        )],
        |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|p| !p.trim().is_empty())
                .map(str::to_string);
            let operation = EbookOperation::OpenBook { path };
            EncodedAction {
                type_id: EBOOK_OPERATION_TYPE.into(),
                payload: serde_json::to_vec(&operation).expect("EbookOperation serializes"),
            }
        },
    )
    .with_contract(qos_actions::ActionContract {
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        ..qos_actions::ActionContract::default()
    })
}

/// One per-block piece of a ranged highlight. A selection that spans several
/// paragraphs is stored as one highlight row per touched block, all sharing the
/// highlight's `group`. Every id is minted at *authoring* time and carried here
/// so replay reproduces the byte-identical rows — `apply` never calls the RNG.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HiPiece {
    /// Row uuid (string form), minted at authoring time.
    pub id: String,
    pub block: usize,
    pub start: usize,
    pub end: usize,
    pub excerpt: String,
}

/// Every collaborative operation Books exports on its durable reader store.
///
/// Determinism contract: ids and timestamps are minted at *authoring* time and
/// carried in the payload, never inside [`Action::apply`], which must not touch
/// the RNG or the wall-clock. A malformed payload is a **no-op** rather than an
/// error, so a bad replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EbookAction {
    /// Persist one book's reading position. This is intentionally separate
    /// from the reader's transient page animation and open rails: the
    /// position is the durable, collaborative part of navigation.
    SetProgress {
        book_id: String,
        chapter: usize,
        page: usize,
        fraction: f32,
        /// `0` preserves the existing last-opened time. A nonzero timestamp
        /// is minted by the caller and makes an explicit open replayable.
        opened: u64,
    },
    /// Replace an annotation's free-text note (also used to edit a comment,
    /// which is a note row attached to a highlight group).
    EditAnnotationNote { id: String, note: String },
    /// Mark an annotation as finished at an author-supplied timestamp. Draft
    /// editors create their row first and then use this action on Save/Escape;
    /// carrying the timestamp keeps the lifecycle replayable and avoids a
    /// hidden wall-clock write in the widget layer.
    FinalizeAnnotation { id: String, created: u64 },
    /// Delete a single annotation row (a standalone note, a bookmark, or one
    /// comment).
    DeleteAnnotation { id: String },
    /// Add a free-text note anchored to a passage in a book. `id` is minted at
    /// authoring time and carried here.
    AddNote {
        book_id: String,
        id: String,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: String,
        color: String,
        text: String,
    },
    /// Toggle a bookmark at `(chapter, block)`. Deterministic against the
    /// document at the replay's base state: if a bookmark already sits there it
    /// is removed, otherwise one is added carrying the pre-minted `id`.
    ToggleBookmark {
        book_id: String,
        id: String,
        chapter: usize,
        block: usize,
        excerpt: String,
    },
    /// Highlight a text selection: first clear every `remove_groups` highlight
    /// (so a colour never layers and a re-colour never leaves a stray band), then
    /// write each of `pieces` as a highlight row sharing `group`. A pure toggle
    /// off is `pieces` empty with `remove_groups` naming the highlight to drop.
    HighlightSelection {
        book_id: String,
        group: String,
        chapter: usize,
        color: String,
        remove_groups: Vec<String>,
        pieces: Vec<HiPiece>,
    },
    /// Recolour every row of a highlight group (its per-block pieces and its
    /// comments) so the mark stays coherent.
    ChangeHighlightColor { group: String, color: String },
    /// Delete a whole highlight — its per-block pieces *and* every comment on it.
    DeleteHighlight { group: String },
    /// Add a comment (a note row sharing the highlight's `group`) to a highlight.
    AddComment {
        book_id: String,
        group: String,
        id: String,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: String,
        color: String,
        text: String,
    },
    /// Re-anchor a single annotation row (a highlight piece or a note) to a new
    /// passage, preserving its text, colour, and `group`. Powers the draggable
    /// margin markers: moving a highlight to another paragraph never disturbs its
    /// sibling rows (e.g. a highlight's comments), because it touches only the
    /// one row named by `id`.
    MoveAnnotation {
        id: String,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: String,
    },
}

// Per-variant `apply` bodies, kept as inherent methods (not trait methods) so
// `too_many_lines` can be satisfied without moving code out of this impl.
impl EbookAction {
    fn apply_set_progress(
        doc: &mut Doc,
        book_id: &str,
        chapter: usize,
        page: usize,
        fraction: f32,
        opened: u64,
    ) {
        BooksDoc::write_progress(
            doc,
            book_id,
            &Progress {
                chapter,
                page,
                fraction: fraction.clamp(0.0, 1.0),
                opened,
            },
        );
    }

    fn apply_edit_note(doc: &mut Doc, id: &str, note: &str) -> Result<(), ActionError> {
        if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")?
            && let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id)?
        {
            doc.put(&entry, "note", note)?;
        }
        Ok(())
    }

    fn apply_finalize_annotation(doc: &mut Doc, id: &str, created: u64) -> Result<(), ActionError> {
        if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")?
            && let Some((Value::Object(ObjType::Map), entry)) = doc.get(&map, id)?
        {
            let created = i64::try_from(created).unwrap_or(i64::MAX);
            doc.put(&entry, "created", created)?;
        }
        Ok(())
    }

    fn apply_delete_annotation(doc: &mut Doc, id: &str) -> Result<(), ActionError> {
        if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")? {
            doc.delete(&map, id)?;
        }
        Ok(())
    }

    // A malformed id (never produced by our own builder) is a no-op rather
    // than an error, so a replay can't wedge on bad input.
    #[allow(clippy::too_many_arguments)]
    fn apply_add_note(
        doc: &mut Doc,
        book_id: &str,
        id: &str,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: &str,
        color: &str,
        text: &str,
    ) {
        if let Ok(uuid) = id.parse::<Uuid>() {
            let anno = Annotation {
                id: uuid,
                doc_id: book_id.to_string(),
                kind: AnnoKind::Note,
                chapter,
                block,
                start,
                end,
                excerpt: excerpt.to_string(),
                note: text.to_string(),
                color: HiColor::from_str(color),
                // Deterministic: no wall-clock in `apply`.
                created: 0,
                group: uuid,
            };
            BooksDoc::upsert_annotation(doc, &anno);
        }
    }

    fn apply_toggle_bookmark(
        doc: &mut Doc,
        book_id: &str,
        id: &str,
        chapter: usize,
        block: usize,
        excerpt: &str,
    ) {
        // Deterministic against the document at the replay's base state.
        let existing: Vec<Uuid> = BooksDoc::annotations_for(doc, book_id)
            .into_iter()
            .filter(|a| a.kind == AnnoKind::Bookmark && a.chapter == chapter && a.block == block)
            .map(|a| a.id)
            .collect();
        if existing.is_empty() {
            if let Ok(uuid) = id.parse::<Uuid>() {
                let anno = Annotation {
                    id: uuid,
                    doc_id: book_id.to_string(),
                    kind: AnnoKind::Bookmark,
                    chapter,
                    block,
                    start: 0,
                    end: 0,
                    excerpt: excerpt.to_string(),
                    note: String::new(),
                    color: HiColor::Yellow,
                    created: 0,
                    group: uuid,
                };
                BooksDoc::upsert_annotation(doc, &anno);
            }
        } else {
            for id in existing {
                BooksDoc::remove_annotation(doc, id);
            }
        }
    }

    fn apply_highlight_selection(
        doc: &mut Doc,
        book_id: &str,
        group: &str,
        chapter: usize,
        color: &str,
        remove_groups: &[String],
        pieces: &[HiPiece],
    ) {
        for g in remove_groups {
            if let Ok(uuid) = g.parse::<Uuid>() {
                BooksDoc::remove_group(doc, uuid);
            }
        }
        if let Ok(group_uuid) = group.parse::<Uuid>() {
            let col = HiColor::from_str(color);
            for p in pieces {
                let Ok(pid) = p.id.parse::<Uuid>() else {
                    continue;
                };
                let anno = Annotation {
                    id: pid,
                    doc_id: book_id.to_string(),
                    kind: AnnoKind::Highlight,
                    chapter,
                    block: p.block,
                    start: p.start,
                    end: p.end,
                    excerpt: p.excerpt.clone(),
                    note: String::new(),
                    color: col,
                    created: 0,
                    group: group_uuid,
                };
                BooksDoc::upsert_annotation(doc, &anno);
            }
        }
    }

    fn apply_change_highlight_color(doc: &mut Doc, group: &str, color: &str) {
        if let Ok(group_uuid) = group.parse::<Uuid>() {
            let col = HiColor::from_str(color);
            let rows: Vec<Annotation> = BooksDoc::list_annotations(doc)
                .into_iter()
                .filter(|a| a.group == group_uuid)
                .collect();
            for mut a in rows {
                a.color = col;
                BooksDoc::upsert_annotation(doc, &a);
            }
        }
    }

    fn apply_delete_highlight(doc: &mut Doc, group: &str) {
        if let Ok(group_uuid) = group.parse::<Uuid>() {
            BooksDoc::remove_group(doc, group_uuid);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_add_comment(
        doc: &mut Doc,
        book_id: &str,
        group: &str,
        id: &str,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: &str,
        color: &str,
        text: &str,
    ) {
        if let (Ok(group_uuid), Ok(cid)) = (group.parse::<Uuid>(), id.parse::<Uuid>()) {
            let anno = Annotation {
                id: cid,
                doc_id: book_id.to_string(),
                kind: AnnoKind::Note,
                chapter,
                block,
                start,
                end,
                excerpt: excerpt.to_string(),
                note: text.to_string(),
                color: HiColor::from_str(color),
                created: 0,
                group: group_uuid,
            };
            BooksDoc::upsert_annotation(doc, &anno);
        }
    }

    fn apply_move_annotation(
        doc: &mut Doc,
        id: &str,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: &str,
    ) {
        // Reposition exactly the one row; a missing id (never produced by
        // our builder) is a no-op so a bad replay can't wedge a peer.
        if let Ok(uuid) = id.parse::<Uuid>()
            && let Some(mut a) = BooksDoc::list_annotations(doc)
                .into_iter()
                .find(|a| a.id == uuid)
        {
            a.chapter = chapter;
            a.block = block;
            a.start = start;
            a.end = end;
            a.excerpt = excerpt.to_string();
            BooksDoc::upsert_annotation(doc, &a);
        }
    }
}

impl Action for EbookAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            EbookAction::SetProgress {
                book_id,
                chapter,
                page,
                fraction,
                opened,
            } => {
                Self::apply_set_progress(doc, book_id, *chapter, *page, *fraction, *opened);
                Ok(())
            }
            EbookAction::EditAnnotationNote { id, note } => Self::apply_edit_note(doc, id, note),
            EbookAction::FinalizeAnnotation { id, created } => {
                Self::apply_finalize_annotation(doc, id, *created)
            }
            EbookAction::DeleteAnnotation { id } => Self::apply_delete_annotation(doc, id),
            EbookAction::AddNote {
                book_id,
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
                color,
                text,
            } => {
                Self::apply_add_note(
                    doc, book_id, id, *chapter, *block, *start, *end, excerpt, color, text,
                );
                Ok(())
            }
            EbookAction::ToggleBookmark {
                book_id,
                id,
                chapter,
                block,
                excerpt,
            } => {
                Self::apply_toggle_bookmark(doc, book_id, id, *chapter, *block, excerpt);
                Ok(())
            }
            EbookAction::HighlightSelection {
                book_id,
                group,
                chapter,
                color,
                remove_groups,
                pieces,
            } => {
                Self::apply_highlight_selection(
                    doc,
                    book_id,
                    group,
                    *chapter,
                    color,
                    remove_groups,
                    pieces,
                );
                Ok(())
            }
            EbookAction::ChangeHighlightColor { group, color } => {
                Self::apply_change_highlight_color(doc, group, color);
                Ok(())
            }
            EbookAction::DeleteHighlight { group } => {
                Self::apply_delete_highlight(doc, group);
                Ok(())
            }
            EbookAction::AddComment {
                book_id,
                group,
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
                color,
                text,
            } => {
                Self::apply_add_comment(
                    doc, book_id, group, id, *chapter, *block, *start, *end, excerpt, color, text,
                );
                Ok(())
            }
            EbookAction::MoveAnnotation {
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
            } => {
                Self::apply_move_annotation(doc, id, *chapter, *block, *start, *end, excerpt);
                Ok(())
            }
        }
    }

    fn locus(&self) -> ExecutionLocus {
        ExecutionLocus::ClientLocal
    }
}

/// Register Books' action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<EbookAction>(EBOOK_ACTION_TYPE);
}

/// Encode any [`EbookAction`] into the wire form the palette and the on-page
/// affordances both dispatch. This is the single point that turns Books' actions
/// into replayable envelopes.
///
/// # Panics
///
/// Panics if `action` fails to serialize to JSON. `EbookAction`'s fields are
/// all primitive (`String`, `usize`, `Vec<String>`/`Vec<HiPiece>`), so this
/// cannot happen in practice. The alternative — threading `Result` through
/// every builder in this file and through `qos_actions::ActionSpec::prompted`,
/// whose `build: Fn(&str) -> EncodedAction` closure signature is infallible
/// by contract — would push a theoretical error through call sites that can
/// never observe it.
#[allow(
    clippy::expect_used,
    reason = "EbookAction always serializes; ActionSpec::prompted's build closure must return EncodedAction, not Result"
)]
pub fn encode(action: &EbookAction) -> EncodedAction {
    EncodedAction::encode(EBOOK_ACTION_TYPE, action).expect("EbookAction is always serializable")
}

// --- Action builders: single source of truth shared by the on-page
// affordances (see `crate::app`) and the Ctrl+K resource tree below. --------

pub fn edit_note_action(id: Uuid, note: &str) -> EbookAction {
    EbookAction::EditAnnotationNote {
        id: id.to_string(),
        note: note.to_string(),
    }
}

/// Build the replayable draft-finalisation action used by both note and
/// comment editors. `created` is minted at the user-visible completion point,
/// never inside action application.
pub fn finalize_annotation_action(id: Uuid, created: u64) -> EbookAction {
    EbookAction::FinalizeAnnotation {
        id: id.to_string(),
        created,
    }
}

pub fn delete_annotation_action(id: Uuid) -> EbookAction {
    EbookAction::DeleteAnnotation { id: id.to_string() }
}

/// Build the replayable navigation action used whenever a reader changes its
/// durable position. `opened` is caller-supplied so applying an action never
/// consults the wall clock.
pub fn set_progress_action(book_id: &str, progress: Progress) -> EbookAction {
    EbookAction::SetProgress {
        book_id: book_id.to_string(),
        chapter: progress.chapter,
        page: progress.page,
        fraction: progress.fraction.clamp(0.0, 1.0),
        opened: progress.opened,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn add_note_action(
    book_id: &str,
    id: Uuid,
    chapter: usize,
    block: usize,
    start: usize,
    end: usize,
    color: HiColor,
    excerpt: &str,
    text: &str,
) -> EbookAction {
    EbookAction::AddNote {
        book_id: book_id.to_string(),
        id: id.to_string(),
        chapter,
        block,
        start,
        end,
        excerpt: excerpt.to_string(),
        color: color.as_str().to_string(),
        text: text.to_string(),
    }
}

pub fn toggle_bookmark_action(
    book_id: &str,
    id: Uuid,
    chapter: usize,
    block: usize,
    excerpt: &str,
) -> EbookAction {
    EbookAction::ToggleBookmark {
        book_id: book_id.to_string(),
        id: id.to_string(),
        chapter,
        block,
        excerpt: excerpt.to_string(),
    }
}

pub fn change_color_action(group: Uuid, color: HiColor) -> EbookAction {
    EbookAction::ChangeHighlightColor {
        group: group.to_string(),
        color: color.as_str().to_string(),
    }
}

/// Re-anchor a single annotation row (a highlight piece or a note) to a new
/// passage. Used by the draggable margin markers.
pub fn move_annotation_action(
    id: Uuid,
    chapter: usize,
    block: usize,
    start: usize,
    end: usize,
    excerpt: &str,
) -> EbookAction {
    EbookAction::MoveAnnotation {
        id: id.to_string(),
        chapter,
        block,
        start,
        end,
        excerpt: excerpt.to_string(),
    }
}

pub fn delete_highlight_action(group: Uuid) -> EbookAction {
    EbookAction::DeleteHighlight {
        group: group.to_string(),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn add_comment_action(
    book_id: &str,
    group: Uuid,
    id: Uuid,
    chapter: usize,
    block: usize,
    start: usize,
    end: usize,
    color: HiColor,
    excerpt: &str,
    text: &str,
) -> EbookAction {
    EbookAction::AddComment {
        book_id: book_id.to_string(),
        group: group.to_string(),
        id: id.to_string(),
        chapter,
        block,
        start,
        end,
        excerpt: excerpt.to_string(),
        color: color.as_str().to_string(),
        text: text.to_string(),
    }
}

/// A fully-resolved plan to apply/toggle a highlight over the live selection.
/// Both the selection-toolbar swatch and the book-level "Highlight selection"
/// palette action build one from the same presentation state, so both dispatch
/// the identical [`EbookAction`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HighlightPlan {
    pub book_id: String,
    pub group: String,
    pub chapter: usize,
    pub color: String,
    pub remove_groups: Vec<String>,
    pub pieces: Vec<HiPiece>,
}

impl HighlightPlan {
    pub fn into_action(self) -> EbookAction {
        EbookAction::HighlightSelection {
            book_id: self.book_id,
            group: self.group,
            chapter: self.chapter,
            color: self.color,
            remove_groups: self.remove_groups,
            pieces: self.pieces,
        }
    }
}

/// The presentation context the reader hands [`provide_resources`] so the
/// book-level actions (bookmark toggle, highlight-the-selection) can be built
/// from the live reading position and selection — state that lives in the app,
/// not the document.
#[derive(Debug, Clone)]
pub struct BookResourceCtx {
    pub book_id: String,
    pub book_title: String,
    pub chapter: usize,
    pub bookmark_block: usize,
    pub bookmark_excerpt: String,
    /// A pre-resolved plan for the current selection, present only while the
    /// reader has a non-empty selection. Drives the book-level "Highlight
    /// selection" action, identical to what the toolbar swatch would dispatch.
    pub selection_highlight: Option<HighlightPlan>,
}

// Same infallibility as `encode` above: `ActionSpec::immediate` can only fail
// on JSON serialization, which `EbookAction`'s primitive fields never trigger.
#[allow(
    clippy::expect_used,
    reason = "EbookAction always serializes; see encode()'s doc comment"
)]
fn encode_spec(id: &str, label: &str, action: &EbookAction) -> ActionSpec {
    ActionSpec::immediate(id, label, EBOOK_ACTION_TYPE, action).expect("encode ebook action")
}

/// One-line annotation preview for the palette label.
fn anno_label(a: &Annotation) -> String {
    let body = if a.note.trim().is_empty() {
        a.excerpt.trim()
    } else {
        a.note.trim()
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

/// Book-level actions: toggle bookmark, highlight-the-selection (only when
/// one exists), and add a free-text note at the current reading position.
fn book_level_actions(mut root: Resource, ctx: &BookResourceCtx) -> Resource {
    // Toggle a bookmark at the current reading position. One action covers
    // both add and remove (it decides deterministically against the
    // document); the id is only used on the add branch.
    root = root.with_action(encode_spec(
        "toggle-bookmark",
        "Toggle bookmark",
        &toggle_bookmark_action(
            &ctx.book_id,
            Uuid::new_v4(),
            ctx.chapter,
            ctx.bookmark_block,
            &ctx.bookmark_excerpt,
        ),
    ));

    if let Some(plan) = &ctx.selection_highlight {
        root = root.with_action(encode_spec(
            "highlight-selection",
            "Highlight selection",
            &plan.clone().into_action(),
        ));
    }

    // The uuid is minted once when the argument is committed, then baked
    // into the envelope, so replay stays deterministic.
    let bid = ctx.book_id.clone();
    let chapter = ctx.chapter;
    let block = ctx.bookmark_block;
    root.with_action(ActionSpec::prompted(
        "add-note",
        "Add note",
        "Note",
        "your note…",
        ExecutionLocus::ClientLocal,
        move |text: &str| {
            encode(&add_note_action(
                &bid,
                Uuid::new_v4(),
                chapter,
                block,
                0,
                0,
                HiColor::Yellow,
                "",
                text,
            ))
        },
    ))
}

/// One highlight-group resource (change colour, delete, add comment) with its
/// comments nested as children, oldest first.
fn highlight_resource(group: Uuid, annos: &[Annotation], book_id: &str) -> Option<Resource> {
    let first = annos
        .iter()
        .filter(|a| a.kind == AnnoKind::Highlight && a.group == group)
        .min_by_key(|a| (a.block, a.start))?;
    let mut hl = Resource::new(
        format!("ebook.highlight:{group}"),
        "ebook.highlight",
        anno_label(first),
    )
    .with_action(reveal_operation(
        book_id,
        first.chapter,
        0,
        Some(first.block),
    ));

    // Change colour — a prompted action taking a colour name.
    hl = hl.with_action(ActionSpec::prompted(
        "change-color",
        "Change colour",
        "Colour",
        "yellow, green, blue, pink, purple",
        ExecutionLocus::ClientLocal,
        move |name: &str| encode(&change_color_action(group, HiColor::from_str(name.trim()))),
    ));
    // Delete the whole highlight (pieces + comments).
    hl = hl.with_action(encode_spec(
        "delete-highlight",
        "Delete highlight",
        &delete_highlight_action(group),
    ));
    // Add a comment — prompted; the comment id is minted at commit time and
    // the anchor is copied from the highlight's first piece.
    let bid = book_id.to_string();
    let anchor = (
        first.chapter,
        first.block,
        first.start,
        first.end,
        first.color,
        first.excerpt.clone(),
    );
    hl = hl.with_action(ActionSpec::prompted(
        "add-comment",
        "Add comment",
        "Comment",
        "your comment…",
        ExecutionLocus::ClientLocal,
        move |text: &str| {
            encode(&add_comment_action(
                &bid,
                group,
                Uuid::new_v4(),
                anchor.0,
                anchor.1,
                anchor.2,
                anchor.3,
                anchor.4,
                &anchor.5,
                text,
            ))
        },
    ));

    // Comments on this highlight (note rows sharing the group), oldest first.
    let mut comments: Vec<&Annotation> = annos
        .iter()
        .filter(|a| a.kind == AnnoKind::Note && a.group == group)
        .collect();
    comments.sort_by_key(|a| a.created);
    for cm in comments {
        let cid = cm.id;
        let child = Resource::new(
            format!("ebook.comment:{cid}"),
            "ebook.comment",
            anno_label(cm),
        )
        .with_action(reveal_operation(book_id, cm.chapter, 0, Some(cm.block)))
        .with_action(ActionSpec::prompted(
            "edit-comment",
            "Edit comment",
            "Comment",
            "edit the comment…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&edit_note_action(cid, text)),
        ))
        .with_action(encode_spec(
            "delete-comment",
            "Delete comment",
            &delete_annotation_action(cid),
        ));
        hl = hl.with_child(child);
    }
    Some(hl)
}

/// Highlight-group resources (deduped, in reading order), plus the set of
/// groups covered — so standalone-note resources can exclude comment rows.
fn highlight_resources(annos: &[Annotation], book_id: &str) -> (Vec<Resource>, Vec<Uuid>) {
    let mut groups: Vec<Uuid> = Vec::new();
    for a in annos.iter().filter(|a| a.kind == AnnoKind::Highlight) {
        if !groups.contains(&a.group) {
            groups.push(a.group);
        }
    }
    let resources = groups
        .iter()
        .filter_map(|&group| highlight_resource(group, annos, book_id))
        .collect();
    (resources, groups)
}

/// Bookmark resources, each with a single delete action.
fn bookmark_resources(annos: &[Annotation], book_id: &str) -> Vec<Resource> {
    annos
        .iter()
        .filter(|a| a.kind == AnnoKind::Bookmark)
        .map(|a| {
            let id = a.id;
            Resource::new(
                format!("ebook.bookmark:{id}"),
                "ebook.bookmark",
                anno_label(a),
            )
            .with_action(reveal_operation(book_id, a.chapter, 0, Some(a.block)))
            .with_action(encode_spec(
                "delete",
                "Delete bookmark",
                &delete_annotation_action(id),
            ))
        })
        .collect()
}

/// Standalone-note resources (note rows not attached to a highlight group),
/// each with edit + delete actions.
fn standalone_note_resources(
    annos: &[Annotation],
    highlight_groups: &[Uuid],
    book_id: &str,
) -> Vec<Resource> {
    annos
        .iter()
        .filter(|a| a.kind == AnnoKind::Note && !highlight_groups.contains(&a.group))
        .map(|a| {
            let id = a.id;
            Resource::new(format!("ebook.note:{id}"), "ebook.note", anno_label(a))
                .with_action(reveal_operation(book_id, a.chapter, 0, Some(a.block)))
                .with_action(ActionSpec::prompted(
                    "edit",
                    "Edit note",
                    "Note",
                    "edit the note…",
                    ExecutionLocus::ClientLocal,
                    move |text: &str| encode(&edit_note_action(id, text)),
                ))
                .with_action(encode_spec(
                    "delete",
                    "Delete note",
                    &delete_annotation_action(id),
                ))
        })
        .collect()
}

/// Build the resource tree Books currently has open: the book being read, its
/// highlights (each owning its comments), its bookmarks, and its standalone
/// notes — every document-mutating operation exported as an action.
fn book_resource(
    book: &crate::model::Book,
    current: Option<&BookResourceCtx>,
    doc: &AutoCommit,
) -> Resource {
    let root = Resource::new(
        format!("ebook.book:{}", book.id),
        "ebook.book",
        if book.title.trim().is_empty() {
            "Untitled".to_string()
        } else {
            book.title.clone()
        },
    );
    let progress = BooksDoc::read_progress(doc, &book.id);
    let root = root.with_action(reveal_operation(
        &book.id,
        progress.chapter,
        progress.page,
        None,
    ));
    let mut root = match current {
        Some(ctx) if ctx.book_id == book.id => book_level_actions(root, ctx),
        _ => root,
    };

    for (chapter, item) in book.chapters.iter().enumerate() {
        let label = if item.title.trim().is_empty() {
            format!("Chapter {}", chapter + 1)
        } else {
            item.title.clone()
        };
        root = root.with_child(
            Resource::new(
                format!("ebook.chapter:{}:{chapter}", book.id),
                "ebook.chapter",
                label,
            )
            .with_action(reveal_operation(&book.id, chapter, 0, None)),
        );
    }

    if current.is_some_and(|ctx| ctx.book_id == book.id) {
        for (id, label, kind, value) in [
            (
                "previous-page",
                "Previous page",
                OperationKind::View,
                EbookOperation::PreviousPage,
            ),
            (
                "next-page",
                "Next page",
                OperationKind::View,
                EbookOperation::NextPage,
            ),
            (
                "contents",
                "Toggle table of contents",
                OperationKind::View,
                EbookOperation::ToggleContents,
            ),
            (
                "notes",
                "Toggle notes",
                OperationKind::View,
                EbookOperation::ToggleNotes,
            ),
            (
                "settings",
                "Open reading settings",
                OperationKind::View,
                EbookOperation::OpenSettings,
            ),
        ] {
            root = root.with_action(operation(id, label, kind, value));
        }
        root = root.with_action(open_book_operation());
    }

    let annos = BooksDoc::annotations_for(doc, &book.id);
    let (highlights, highlight_groups) = highlight_resources(&annos, &book.id);
    for hl in highlights {
        root = root.with_child(hl);
    }
    for bm in bookmark_resources(&annos, &book.id) {
        root = root.with_child(bm);
    }
    for note in standalone_note_resources(&annos, &highlight_groups, &book.id) {
        root = root.with_child(note);
    }

    root
}

/// Build the canonical resource tree for every book in the shelf. Search,
/// agents and accessibility all resolve the same stable addresses from this
/// tree, including resources that are not currently visible.
pub fn provide_resources(
    books: &[crate::model::Book],
    current: Option<&BookResourceCtx>,
    doc: &AutoCommit,
) -> Vec<Resource> {
    books
        .iter()
        .map(|book| book_resource(book, current, doc))
        .collect()
}

impl ResourceProvider for crate::app::BooksApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        let ctx = self.book_resource_ctx();
        provide_resources(self.library(), ctx.as_ref(), doc)
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

    /// Build an [`EncodedAction`] from a prompted spec's closure (the same path
    /// the palette takes on Enter).
    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
        }
    }

    /// A reader with a book open and one seeded highlight carrying two comments,
    /// plus a standalone note and a bookmark, in the CRDT.
    fn open_with_annotations() -> (BooksApp, AutoCommit, String, Uuid) {
        let app = BooksApp::with_library(crate::content::load_sample_library());
        let book_id = app
            .open_book()
            .expect("sample library is non-empty")
            .id
            .clone();
        let mut doc = AutoCommit::new();
        let group = Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888);
        // Highlight piece.
        let hl = Annotation {
            id: group,
            doc_id: book_id.clone(),
            kind: AnnoKind::Highlight,
            chapter: 0,
            block: 3,
            start: 0,
            end: 12,
            excerpt: "a passage".into(),
            note: String::new(),
            color: HiColor::Green,
            created: 1,
            group,
        };
        BooksDoc::upsert_annotation(&mut doc, &hl);
        // Two comments on it.
        for (i, text) in ["first comment", "second comment"].into_iter().enumerate() {
            let cm = Annotation {
                id: Uuid::from_u128(0xAAAA_0000_0000_0000_0000_0000_0000_0000 + i as u128 + 1),
                doc_id: book_id.clone(),
                kind: AnnoKind::Note,
                chapter: 0,
                block: 3,
                start: 0,
                end: 12,
                excerpt: "a passage".into(),
                note: text.into(),
                color: HiColor::Green,
                created: 10 + i as u64,
                group,
            };
            BooksDoc::upsert_annotation(&mut doc, &cm);
        }
        // A standalone note.
        let note = Annotation::new(&book_id, AnnoKind::Note, 0, 5).with_note("standalone");
        BooksDoc::upsert_annotation(&mut doc, &note);
        // A bookmark.
        let bm = Annotation::new(&book_id, AnnoKind::Bookmark, 0, 1);
        BooksDoc::upsert_annotation(&mut doc, &bm);
        (app, doc, book_id, group)
    }

    /// Ctrl+K on Books yields the whole shelf. The open book owns its chapter
    /// and annotation resources, with highlight → comments nested beneath it.
    #[test]
    fn ctrl_k_publishes_the_full_nested_tree() {
        let (app, doc, book_id, _group) = open_with_annotations();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), app.library().len(), "every shelf book");
        assert!(resources.iter().all(|book| {
            book.actions
                .iter()
                .any(|action| action.id == "open" && action.kind == OperationKind::View)
        }));
        let book = resources
            .iter()
            .find(|book| book.id.as_str() == format!("ebook.book:{book_id}"))
            .expect("open book resource");
        assert_eq!(book.kind, "ebook.book");
        assert!(book.children.iter().any(|r| r.kind == "ebook.chapter"));
        let book_actions: Vec<&str> = book.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(
            book_actions.contains(&"toggle-bookmark"),
            "{book_actions:?}"
        );
        assert!(book_actions.contains(&"add-note"), "{book_actions:?}");

        // Highlight resource with its nested comment, plus bookmark + note.
        let hl = book
            .children
            .iter()
            .find(|r| r.kind == "ebook.highlight")
            .expect("a highlight resource");
        let hl_actions: Vec<&str> = hl.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(hl_actions.contains(&"change-color"), "{hl_actions:?}");
        assert!(hl_actions.contains(&"delete-highlight"), "{hl_actions:?}");
        assert!(hl_actions.contains(&"add-comment"), "{hl_actions:?}");
        assert_eq!(
            hl.children.len(),
            2,
            "both comments nest under the highlight"
        );
        let comment = &hl.children[0];
        assert_eq!(comment.kind, "ebook.comment");
        let cm_actions: Vec<&str> = comment.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(cm_actions.contains(&"edit-comment"), "{cm_actions:?}");
        assert!(cm_actions.contains(&"delete-comment"), "{cm_actions:?}");

        assert!(
            book.children.iter().any(|r| r.kind == "ebook.bookmark"),
            "a bookmark resource"
        );
        assert!(
            book.children.iter().any(|r| r.kind == "ebook.note"),
            "a standalone-note resource"
        );

        // The one palette surfaces the tree's actions as rows (the Actions group
        // is capped, so assert on the leading, always-present ones).
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("Toggle bookmark");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Toggle bookmark")),
            "{labels:?}"
        );
        palette.set_query("Delete highlight");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete highlight")),
            "{labels:?}"
        );
        palette.set_query("Delete comment");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete comment")),
            "{labels:?}"
        );
    }

    /// Deleting the highlight through its action removes the highlight *and*
    /// both of its comments — the whole logical group.
    #[test]
    fn delete_highlight_removes_the_group_and_its_comments() {
        let (_app, mut doc, book_id, group) = open_with_annotations();
        assert_eq!(
            BooksDoc::annotations_for(&doc, &book_id).len(),
            5,
            "hl + 2 comments + note + bookmark"
        );

        let action = delete_highlight_action(group);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let left = BooksDoc::annotations_for(&doc, &book_id);
        assert_eq!(left.len(), 2, "only the standalone note + bookmark survive");
        assert!(
            left.iter().all(|a| a.group != group),
            "no row of the deleted group remains (highlight + both comments gone)"
        );
    }

    /// The full undo covenant on one action: dispatched through the executor it
    /// mutates the doc and records exactly one undo boundary, and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (_app, seeded, book_id, _group) = open_with_annotations();
        // Move the seeded doc into a host State so undo is the real host path.
        let mut state = ui::State::new();
        state.doc = seeded;
        let before_heads = state.doc.get_heads();
        let before_len = BooksDoc::annotations_for(&state.doc, &book_id).len();

        // Add a bookmark at a fresh block via the action + host undo recording.
        let action = toggle_bookmark_action(
            &book_id,
            Uuid::from_u128(0xDEAD_BEEF_0000_0000_0000_0000_0000_0001),
            0,
            9,
            "bm",
        );
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
        state.record_committed_action(before_heads, None, None);

        assert_eq!(
            BooksDoc::annotations_for(&state.doc, &book_id).len(),
            before_len + 1,
            "the action added a bookmark"
        );
        assert!(
            state.peek_last_undo().is_some(),
            "exactly one undo boundary recorded"
        );

        // Ctrl+Z reverts exactly this action.
        state.undo(None, None).expect("an undo entry to pop");
        assert_eq!(
            BooksDoc::annotations_for(&state.doc, &book_id).len(),
            before_len,
            "undo reverted the added bookmark and nothing else"
        );
    }

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, seeded, book_id, _group) = open_with_annotations();
        // Two replicas sharing history.
        let mut a = seeded;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = add_comment_action(
            &book_id,
            Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888),
            Uuid::from_u128(0x0BAD_0000_0000_0000_0000_0000_0000_00FE),
            0,
            3,
            0,
            12,
            HiColor::Green,
            "a passage",
            "converge",
        );
        let encoded = encode(&action);

        // A applies locally, producing the envelope; B replays it.
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "replay reproduced the identical change"
        );
        assert_eq!(
            BooksDoc::annotations_for(&a, &book_id).len(),
            BooksDoc::annotations_for(&b, &book_id).len(),
        );
    }

    #[test]
    fn progress_action_replays_byte_identically_and_preserves_open_timestamp() {
        let (_app, mut a, book_id, _group) = open_with_annotations();
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).expect("replica loads");

        let action = set_progress_action(
            &book_id,
            Progress {
                chapter: 2,
                page: 7,
                fraction: 0.625,
                opened: 1_700_000_000_000,
            },
        );
        let encoded = encode(&action);
        let envelope = qos_actions::apply_local(&mut a, &action, encoded).expect("action applies");
        qos_actions::replay(&mut b, &envelope, &registry()).expect("action replays");

        assert_eq!(a.get_heads(), b.get_heads(), "position converges exactly");
        assert_eq!(
            BooksDoc::read_progress(&a, &book_id),
            Progress {
                chapter: 2,
                page: 7,
                fraction: 0.625,
                opened: 1_700_000_000_000,
            }
        );
    }

    #[test]
    fn editor_finalisation_is_replayable_and_never_needs_a_direct_doc_write() {
        let (_app, seeded, book_id, _group) = open_with_annotations();
        let id = BooksDoc::annotations_for(&seeded, &book_id)
            .into_iter()
            .find(|annotation| annotation.kind == AnnoKind::Note)
            .expect("seeded note")
            .id;
        let mut a = seeded;
        a.commit();
        let mut b = AutoCommit::load(&a.save()).expect("replica loads");
        let action = finalize_annotation_action(id, 1_700_000_123_456);
        let envelope = qos_actions::apply_local(&mut a, &action, encode(&action))
            .expect("finalisation applies");
        qos_actions::replay(&mut b, &envelope, &registry()).expect("finalisation replays");

        assert_eq!(a.get_heads(), b.get_heads(), "peers converge exactly");
        assert_eq!(
            BooksDoc::annotations_for(&a, &book_id)
                .into_iter()
                .find(|annotation| annotation.id == id)
                .expect("note remains")
                .created,
            1_700_000_123_456,
        );
    }

    /// Re-anchoring a highlight (the draggable marker's action) moves exactly the
    /// one row it names — leaving the highlight's comments in place — and replays
    /// byte-identically on a peer.
    #[test]
    fn move_annotation_reanchors_one_row_and_replays() {
        let (_app, seeded, book_id, group) = open_with_annotations();
        let hl = BooksDoc::annotations_for(&seeded, &book_id)
            .into_iter()
            .find(|a| a.kind == AnnoKind::Highlight)
            .expect("the seeded highlight");
        assert_eq!(hl.block, 3, "precondition: the highlight starts on block 3");

        let mut a = seeded;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let action = move_annotation_action(hl.id, 0, 9, 0, 5, "moved");
        let encoded = encode(&action);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();

        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "the re-anchor replays byte-identically"
        );
        // The same highlight row moved — its id is preserved.
        let moved = BooksDoc::annotations_for(&a, &book_id)
            .into_iter()
            .find(|x| x.id == hl.id)
            .expect("the highlight still exists after the move");
        assert_eq!(
            moved.block, 9,
            "the highlight re-anchored to the target block"
        );
        assert_eq!((moved.start, moved.end), (0, 5), "with the moved range");
        // Its two comments (Note rows sharing the group) are untouched.
        let comments = BooksDoc::annotations_for(&a, &book_id)
            .into_iter()
            .filter(|x| x.kind == AnnoKind::Note && x.group == group)
            .count();
        assert_eq!(
            comments, 2,
            "the highlight's comments survived the re-anchor"
        );
    }

    /// Single source of truth: a prompted "Edit comment" spec and a directly
    /// built edit action encode byte-identically.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, doc, _book, group) = open_with_annotations();
        let resources = Widget::resources(&app, &doc);
        let hl = resources[0]
            .children
            .iter()
            .find(|r| r.kind == "ebook.highlight")
            .unwrap();
        // The highlight's delete-highlight spec is the same encoded action the
        // on-page trash button dispatches (`delete_highlight_action(group)`).
        let spec = hl
            .actions
            .iter()
            .find(|a| a.id == "delete-highlight")
            .unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&delete_highlight_action(group));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth"
        );

        // And a comment's edit spec matches the edit builder for the same text.
        let comment = &hl.children[0];
        let cid = comment.id.as_str().trim_start_matches("ebook.comment:");
        let cid: Uuid = cid.parse().unwrap();
        let edit_spec = comment
            .actions
            .iter()
            .find(|a| a.id == "edit-comment")
            .unwrap();
        let from_spec = prompted_encode(edit_spec, "revised");
        let from_builder = encode(&edit_note_action(cid, "revised"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "edit is one source of truth"
        );
    }
}
