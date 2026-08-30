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
    Action, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction, ExecutionLocus, Resource,
    ResourceProvider,
};

use crate::doc::BooksDoc;
use crate::model::{AnnoKind, Annotation, HiColor};

/// Registry `type_id` for every Books action.
pub const EBOOK_ACTION_TYPE: &str = "ebook.action";

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

/// Every collaborative operation Books exports on its annotation store.
///
/// Determinism contract: ids and timestamps are minted at *authoring* time and
/// carried in the payload, never inside [`Action::apply`], which must not touch
/// the RNG or the wall-clock. A malformed payload is a **no-op** rather than an
/// error, so a bad replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EbookAction {
    /// Replace an annotation's free-text note (also used to edit a comment,
    /// which is a note row attached to a highlight group).
    EditAnnotationNote { id: String, note: String },
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

impl Action for EbookAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            EbookAction::EditAnnotationNote { id, note } => {
                if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")?
                    && let Some((Value::Object(ObjType::Map), entry)) =
                        doc.get(&map, id.as_str())?
                {
                    doc.put(&entry, "note", note.as_str())?;
                }
            }
            EbookAction::DeleteAnnotation { id } => {
                if let Some((Value::Object(ObjType::Map), map)) = doc.get(&ROOT, "annotations")? {
                    doc.delete(&map, id.as_str())?;
                }
            }
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
                // A malformed id (never produced by our own builder) is a no-op
                // rather than an error, so a replay can't wedge on bad input.
                if let Ok(uuid) = id.parse::<Uuid>() {
                    let anno = Annotation {
                        id: uuid,
                        doc_id: book_id.clone(),
                        kind: AnnoKind::Note,
                        chapter: *chapter,
                        block: *block,
                        start: *start,
                        end: *end,
                        excerpt: excerpt.clone(),
                        note: text.clone(),
                        color: HiColor::from_str(color),
                        // Deterministic: no wall-clock in `apply`.
                        created: 0,
                        group: uuid,
                    };
                    BooksDoc::upsert_annotation(doc, &anno);
                }
            }
            EbookAction::ToggleBookmark {
                book_id,
                id,
                chapter,
                block,
                excerpt,
            } => {
                // Deterministic against the document at the replay's base state.
                let existing: Vec<Uuid> = BooksDoc::annotations_for(doc, book_id)
                    .into_iter()
                    .filter(|a| {
                        a.kind == AnnoKind::Bookmark && a.chapter == *chapter && a.block == *block
                    })
                    .map(|a| a.id)
                    .collect();
                if existing.is_empty() {
                    if let Ok(uuid) = id.parse::<Uuid>() {
                        let anno = Annotation {
                            id: uuid,
                            doc_id: book_id.clone(),
                            kind: AnnoKind::Bookmark,
                            chapter: *chapter,
                            block: *block,
                            start: 0,
                            end: 0,
                            excerpt: excerpt.clone(),
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
            EbookAction::HighlightSelection {
                book_id,
                group,
                chapter,
                color,
                remove_groups,
                pieces,
            } => {
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
                            doc_id: book_id.clone(),
                            kind: AnnoKind::Highlight,
                            chapter: *chapter,
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
            EbookAction::ChangeHighlightColor { group, color } => {
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
            EbookAction::DeleteHighlight { group } => {
                if let Ok(group_uuid) = group.parse::<Uuid>() {
                    BooksDoc::remove_group(doc, group_uuid);
                }
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
                if let (Ok(group_uuid), Ok(cid)) = (group.parse::<Uuid>(), id.parse::<Uuid>()) {
                    let anno = Annotation {
                        id: cid,
                        doc_id: book_id.clone(),
                        kind: AnnoKind::Note,
                        chapter: *chapter,
                        block: *block,
                        start: *start,
                        end: *end,
                        excerpt: excerpt.clone(),
                        note: text.clone(),
                        color: HiColor::from_str(color),
                        created: 0,
                        group: group_uuid,
                    };
                    BooksDoc::upsert_annotation(doc, &anno);
                }
            }
            EbookAction::MoveAnnotation {
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
            } => {
                // Reposition exactly the one row; a missing id (never produced by
                // our builder) is a no-op so a bad replay can't wedge a peer.
                if let Ok(uuid) = id.parse::<Uuid>()
                    && let Some(mut a) = BooksDoc::list_annotations(doc)
                        .into_iter()
                        .find(|a| a.id == uuid)
                {
                    a.chapter = *chapter;
                    a.block = *block;
                    a.start = *start;
                    a.end = *end;
                    a.excerpt = excerpt.clone();
                    BooksDoc::upsert_annotation(doc, &a);
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

/// Encode any [`EbookAction`] into the wire form the palette and the on-page
/// affordances both dispatch. This is the single point that turns Books' actions
/// into replayable envelopes.
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

pub fn delete_annotation_action(id: Uuid) -> EbookAction {
    EbookAction::DeleteAnnotation { id: id.to_string() }
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

/// Build the resource tree Books currently has open: the book being read, its
/// highlights (each owning its comments), its bookmarks, and its standalone
/// notes — every document-mutating operation exported as an action.
pub fn provide_resources(ctx: &BookResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let mut root = Resource::new(
        format!("ebook.book:{}", ctx.book_id),
        "ebook.book",
        ctx.book_title.clone(),
    );

    // Book-level: toggle a bookmark at the current reading position. One action
    // covers both add and remove (it decides deterministically against the
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

    // Book-level: highlight the current selection — only offered when one exists.
    if let Some(plan) = &ctx.selection_highlight {
        root = root.with_action(encode_spec(
            "highlight-selection",
            "Highlight selection",
            &plan.clone().into_action(),
        ));
    }

    // Book-level: add a free-text note at the current reading position. The uuid
    // is minted once when the argument is committed, then baked into the
    // envelope, so replay stays deterministic.
    let bid = ctx.book_id.clone();
    let chapter = ctx.chapter;
    let block = ctx.bookmark_block;
    root = root.with_action(ActionSpec::prompted(
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
    ));

    let annos = BooksDoc::annotations_for(doc, &ctx.book_id);

    // Highlight groups (deduped, in reading order) → one resource each, owning
    // its comments.
    let mut highlight_groups: Vec<Uuid> = Vec::new();
    for a in annos.iter().filter(|a| a.kind == AnnoKind::Highlight) {
        if !highlight_groups.contains(&a.group) {
            highlight_groups.push(a.group);
        }
    }
    for group in &highlight_groups {
        let first = annos
            .iter()
            .filter(|a| a.kind == AnnoKind::Highlight && a.group == *group)
            .min_by_key(|a| (a.block, a.start));
        let Some(first) = first else { continue };
        let mut hl = Resource::new(
            format!("ebook.highlight:{group}"),
            "ebook.highlight",
            anno_label(first),
        );

        // Change colour — a prompted action taking a colour name.
        let gid = *group;
        hl = hl.with_action(ActionSpec::prompted(
            "change-color",
            "Change colour",
            "Colour",
            "yellow, green, blue, pink, purple",
            ExecutionLocus::ClientLocal,
            move |name: &str| encode(&change_color_action(gid, HiColor::from_str(name.trim()))),
        ));
        // Delete the whole highlight (pieces + comments).
        hl = hl.with_action(encode_spec(
            "delete-highlight",
            "Delete highlight",
            &delete_highlight_action(*group),
        ));
        // Add a comment — prompted; the comment id is minted at commit time and
        // the anchor is copied from the highlight's first piece.
        let bid = ctx.book_id.clone();
        let anchor = (
            first.chapter,
            first.block,
            first.start,
            first.end,
            first.color,
            first.excerpt.clone(),
        );
        let gid = *group;
        hl = hl.with_action(ActionSpec::prompted(
            "add-comment",
            "Add comment",
            "Comment",
            "your comment…",
            ExecutionLocus::ClientLocal,
            move |text: &str| {
                encode(&add_comment_action(
                    &bid,
                    gid,
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
            .filter(|a| a.kind == AnnoKind::Note && a.group == *group)
            .collect();
        comments.sort_by_key(|a| a.created);
        for cm in comments {
            let cid = cm.id;
            let child = Resource::new(
                format!("ebook.comment:{cid}"),
                "ebook.comment",
                anno_label(cm),
            )
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
        root = root.with_child(hl);
    }

    // Bookmarks → a resource each with a single delete action.
    for a in annos.iter().filter(|a| a.kind == AnnoKind::Bookmark) {
        let id = a.id;
        root = root.with_child(
            Resource::new(
                format!("ebook.bookmark:{id}"),
                "ebook.bookmark",
                anno_label(a),
            )
            .with_action(encode_spec(
                "delete",
                "Delete bookmark",
                &delete_annotation_action(id),
            )),
        );
    }

    // Standalone notes (note rows not attached to a highlight group) → edit + delete.
    for a in annos
        .iter()
        .filter(|a| a.kind == AnnoKind::Note && !highlight_groups.contains(&a.group))
    {
        let id = a.id;
        let child = Resource::new(format!("ebook.note:{id}"), "ebook.note", anno_label(a))
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
            ));
        root = root.with_child(child);
    }

    vec![root]
}

impl ResourceProvider for crate::app::BooksApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.book_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
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

    /// Build an [`EncodedAction`] from a prompted spec's closure (the same path
    /// the palette takes on Enter).
    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            _ => panic!("expected a prompted action: {}", spec.id),
        }
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            _ => panic!("expected an immediate action: {}", spec.id),
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

    /// Ctrl+K on Books yields the full nested tree: the open book →
    /// highlight → comment, plus bookmark and standalone-note resources, each
    /// with the actions the data model implies. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_full_nested_tree() {
        let (app, doc, _book, _group) = open_with_annotations();
        let resources = Widget::resources(&app, &doc);
        assert_eq!(resources.len(), 1, "one open book");
        let book = &resources[0];
        assert_eq!(book.kind, "ebook.book");
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
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Toggle bookmark")),
            "{labels:?}"
        );
        assert!(
            labels.iter().any(|l| l.contains("Delete highlight")),
            "{labels:?}"
        );
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
