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

#[cfg(test)]
use automerge::ActorId;
use automerge::AutoCommit;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

use tilo_actions::{
    Action, ActionArgs, ActionError, ActionRegistry, ActionSpec, ConfirmationPolicy, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, Resource, ResourceProvider,
    ResultSpec, RetryPolicy, RiskLevel,
};

use crate::doc::BooksDoc;
use crate::model::{
    AnnoKind, Annotation, AnnotationAnchor, ColorScheme, CompositionStyle, FontEntry, HiColor,
    LibraryEntry, LibraryPrefs, LibrarySort, Progress, ReadMode, ReadTheme, Settings,
};

/// Registry `type_id` for every Books action.
pub const EBOOK_ACTION_TYPE: &str = "ebook.action";
pub const EBOOK_OPERATION_TYPE: &str = "ebook.operation";

const MAX_ID_BYTES: usize = 256;
const MAX_LABEL_BYTES: usize = 4 * 1024;
const MAX_EXCERPT_BYTES: usize = 16 * 1024;
const MAX_NOTE_BYTES: usize = 64 * 1024;
const MAX_ACTION_ITEMS: usize = 2_048;

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
    /// Request a user-consented import through the host portal. Deliberately
    /// carries no path: an action can originate from search, an agent, or a
    /// peer and must not turn that authority into arbitrary local file access.
    OpenBook,
    /// Materialise the exact already-imported book into this device's shared
    /// workspace. The operation carries only its stable content id, never a
    /// device path or a peer's local capability grant.
    ShareBook {
        book_id: String,
    },
    /// Hydrate a book that the collaborative library already advertises from
    /// its deterministic workspace object, then open it locally.
    HydrateSharedBook {
        book_id: String,
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
        EbookOperation::OpenBook
        | EbookOperation::ShareBook { .. }
        | EbookOperation::HydrateSharedBook { .. } => OperationKind::Effect,
    }
}

#[cfg(test)]
mod file_action_tests {
    use super::*;

    #[test]
    fn open_action_is_background_portal_only() {
        let spec = open_book_operation();
        let encoded = spec.build_json(&serde_json::Map::new()).unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(EbookOperation::OpenBook)
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

#[allow(
    clippy::expect_used,
    reason = "EbookOperation contains only JSON-representable strings and integers"
)]
fn open_book_operation() -> ActionSpec {
    ActionSpec::typed_operation(
        "open-book",
        "Open book",
        "choose an EPUB through the system picker",
        OperationKind::Effect,
        EBOOK_OPERATION_TYPE,
        Vec::new(),
        |_args: &ActionArgs| {
            let operation = EbookOperation::OpenBook;
            EncodedAction {
                type_id: EBOOK_OPERATION_TYPE.into(),
                payload: serde_json::to_vec(&operation).expect("EbookOperation serializes"),
            }
        },
    )
    .with_contract(tilo_actions::ActionContract {
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        ..tilo_actions::ActionContract::default()
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
/// the RNG or the wall-clock. A malformed payload is refused before the first
/// CRDT write, so it cannot partially mutate or wedge a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EbookAction {
    /// Add or explicitly restore one content-addressed library row. The path
    /// remains device-local; every replicated field is absolute and authored
    /// before dispatch, including the first-added timestamp.
    RestoreLibraryBook {
        book_id: String,
        shared: bool,
        title: String,
        author: String,
        subject: String,
        added: u64,
    },
    /// Replace the authored reading-preference snapshot. Every field is an
    /// absolute value, making replay independent of a receiver's UI state.
    SetReadingSettings {
        before: Settings,
        settings: Settings,
    },
    /// Upsert one stable-id font without disturbing concurrent edits to other
    /// faces.
    UpsertFont { font: FontEntry },
    /// Tombstone one font id so delayed peer edits cannot resurrect it.
    DeleteFont { id: String },
    /// Choose the registry-wide default face by stable font id.
    SetDefaultFont { id: String },
    /// Upsert one stable-id colour scheme without replacing its siblings.
    UpsertColorScheme { scheme: ColorScheme },
    /// Tombstone one custom scheme.
    DeleteColorScheme { id: String },
    /// Persist the shelf's absolute sort/filter preferences.
    SetLibraryPreferences {
        before: LibraryPrefs,
        preferences: LibraryPrefs,
    },
    /// Tombstone one library membership row by stable content id. The book's
    /// annotations and progress remain intact, so an explicit re-import can
    /// restore the shelf row without losing collaborative reading state.
    RemoveLibraryBook { book_id: String },
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
    /// Legacy receiver-dependent bookmark toggle. Kept only so old envelopes
    /// decode; applying it is refused because it cannot replay deterministically.
    ToggleBookmark {
        book_id: String,
        id: String,
        chapter: usize,
        block: usize,
        excerpt: String,
    },
    /// Set one bookmark target to an absolute authored state. `present` and
    /// every row to remove are decided before dispatch, never from a receiving
    /// peer's document state.
    SetBookmark {
        book_id: String,
        id: String,
        chapter: usize,
        block: usize,
        excerpt: String,
        present: bool,
        remove_ids: Vec<String>,
    },
    /// Apply an absolute authored highlight change: clear exactly
    /// `remove_groups`, then write exactly `pieces` sharing `group`. A pure
    /// toggle-off carries no pieces and explicitly names the removed groups.
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

struct SerializedNote<'a> {
    book_id: &'a str,
    id: &'a str,
    chapter: usize,
    block: usize,
    start: usize,
    end: usize,
    excerpt: &'a str,
    color: &'a str,
    text: &'a str,
}

impl SerializedNote<'_> {
    fn into_annotation(self, id: Uuid, group: Uuid) -> Annotation {
        Annotation {
            id,
            doc_id: self.book_id.to_string(),
            kind: AnnoKind::Note,
            chapter: self.chapter,
            block: self.block,
            start: self.start,
            end: self.end,
            excerpt: self.excerpt.to_string(),
            note: self.text.to_string(),
            color: HiColor::from_str(self.color),
            created: 0,
            group,
        }
    }
}

// Per-variant `apply` bodies, kept as inherent methods (not trait methods) so
// `too_many_lines` can be satisfied without moving code out of this impl.
impl EbookAction {
    fn refused(message: impl Into<String>) -> ActionError {
        ActionError::Refused(message.into())
    }

    fn parse_uuid(id: &str, target: &str) -> Result<Uuid, ActionError> {
        id.parse()
            .map_err(|_| Self::refused(format!("{target} id is invalid")))
    }

    fn valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= MAX_ID_BYTES
            && !id.chars().any(char::is_control)
            && !id.contains(['/', '\\'])
    }

    fn validate_book_id(id: &str) -> Result<(), ActionError> {
        if Self::valid_id(id)
            && id
                .strip_prefix("book:")
                .is_some_and(|tail| !tail.is_empty())
        {
            Ok(())
        } else {
            Err(Self::refused("book id is invalid"))
        }
    }

    fn validate_text(value: &str, limit: usize, field: &str) -> Result<(), ActionError> {
        if value.len() <= limit {
            Ok(())
        } else {
            Err(Self::refused(format!("{field} exceeds its size limit")))
        }
    }

    fn validate_anchor(
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: &str,
    ) -> Result<(), ActionError> {
        if chapter > i64::MAX as usize
            || block > i64::MAX as usize
            || start > i64::MAX as usize
            || end > i64::MAX as usize
            || start > end
        {
            return Err(Self::refused("annotation anchor is invalid"));
        }
        Self::validate_text(excerpt, MAX_EXCERPT_BYTES, "annotation excerpt")
    }

    fn validate_color(color: &str) -> Result<(), ActionError> {
        if HiColor::ALL.iter().any(|value| value.as_str() == color) {
            Ok(())
        } else {
            Err(Self::refused("annotation colour is invalid"))
        }
    }

    fn values_are_unique(values: impl IntoIterator<Item = impl AsRef<str>>) -> bool {
        let mut seen = HashSet::new();
        values
            .into_iter()
            .all(|value| seen.insert(value.as_ref().to_string()))
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per action variant, each a short run of field checks"
    )]
    fn validate(&self) -> Result<(), ActionError> {
        match self {
            Self::RestoreLibraryBook {
                book_id,
                title,
                author,
                subject,
                added,
                ..
            } => {
                Self::validate_book_id(book_id)?;
                Self::validate_text(title, MAX_LABEL_BYTES, "book title")?;
                Self::validate_text(author, MAX_LABEL_BYTES, "book author")?;
                Self::validate_text(subject, MAX_LABEL_BYTES, "book subject")?;
                if *added > i64::MAX as u64 {
                    return Err(Self::refused("book timestamp is invalid"));
                }
            }
            Self::SetReadingSettings { before, settings } => {
                for value in [before, settings] {
                    if !value.font_scale.is_finite()
                        || !(Settings::FONT_MIN..=Settings::FONT_MAX).contains(&value.font_scale)
                        || !value.line_spacing.is_finite()
                        || !(Settings::SPACING_MIN..=Settings::SPACING_MAX)
                            .contains(&value.line_spacing)
                        || (!value.font_id.is_empty() && !Self::valid_id(&value.font_id))
                        || (!value.scheme_id.is_empty() && !Self::valid_id(&value.scheme_id))
                    {
                        return Err(Self::refused("reading settings are invalid"));
                    }
                }
            }
            Self::UpsertFont { font } => {
                if !Self::valid_id(&font.id)
                    || !font.size_mult.is_finite()
                    || !(FontEntry::MULT_MIN..=FontEntry::MULT_MAX).contains(&font.size_mult)
                {
                    return Err(Self::refused("font is invalid"));
                }
                Self::validate_text(&font.name, MAX_LABEL_BYTES, "font name")?;
                Self::validate_text(&font.kind.as_code(), MAX_LABEL_BYTES, "font family")?;
            }
            Self::DeleteFont { id } => {
                if !Self::valid_id(id) {
                    return Err(Self::refused("font id is invalid"));
                }
            }
            Self::SetDefaultFont { id } => {
                if !id.is_empty() && !Self::valid_id(id) {
                    return Err(Self::refused("font id is invalid"));
                }
            }
            Self::UpsertColorScheme { scheme } => {
                if !Self::valid_id(&scheme.id) {
                    return Err(Self::refused("colour scheme id is invalid"));
                }
                Self::validate_text(&scheme.name, MAX_LABEL_BYTES, "colour scheme name")?;
            }
            Self::DeleteColorScheme { id } => {
                if !Self::valid_id(id) {
                    return Err(Self::refused("colour scheme id is invalid"));
                }
            }
            Self::SetLibraryPreferences { .. } | Self::ToggleBookmark { .. } => {}
            Self::RemoveLibraryBook { book_id } => Self::validate_book_id(book_id)?,
            Self::SetProgress {
                book_id,
                chapter,
                page,
                fraction,
                opened,
            } => {
                Self::validate_book_id(book_id)?;
                if *chapter > i64::MAX as usize
                    || *page > i64::MAX as usize
                    || !fraction.is_finite()
                    || !(0.0..=1.0).contains(fraction)
                    || *opened > i64::MAX as u64
                {
                    return Err(Self::refused("reading progress is invalid"));
                }
            }
            Self::EditAnnotationNote { id, note } => {
                Self::parse_uuid(id, "annotation")?;
                Self::validate_text(note, MAX_NOTE_BYTES, "annotation note")?;
            }
            Self::FinalizeAnnotation { id, created } => {
                Self::parse_uuid(id, "annotation")?;
                if *created > i64::MAX as u64 {
                    return Err(Self::refused("annotation timestamp is invalid"));
                }
            }
            Self::DeleteAnnotation { id } => {
                Self::parse_uuid(id, "annotation")?;
            }
            Self::AddNote {
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
                Self::validate_book_id(book_id)?;
                Self::parse_uuid(id, "annotation")?;
                Self::validate_anchor(*chapter, *block, *start, *end, excerpt)?;
                Self::validate_color(color)?;
                Self::validate_text(text, MAX_NOTE_BYTES, "annotation note")?;
            }
            Self::SetBookmark {
                book_id,
                id,
                chapter,
                block,
                excerpt,
                remove_ids,
                ..
            } => {
                Self::validate_book_id(book_id)?;
                Self::parse_uuid(id, "bookmark")?;
                Self::validate_anchor(*chapter, *block, 0, 0, excerpt)?;
                if remove_ids.len() > MAX_ACTION_ITEMS {
                    return Err(Self::refused("bookmark removal list is too large"));
                }
                if !Self::values_are_unique(remove_ids) {
                    return Err(Self::refused("bookmark removal list contains duplicates"));
                }
                for id in remove_ids {
                    Self::parse_uuid(id, "bookmark")?;
                }
            }
            Self::HighlightSelection {
                book_id,
                group,
                chapter,
                color,
                remove_groups,
                pieces,
            } => {
                Self::validate_book_id(book_id)?;
                Self::parse_uuid(group, "highlight")?;
                Self::validate_color(color)?;
                if remove_groups.len() > MAX_ACTION_ITEMS || pieces.len() > MAX_ACTION_ITEMS {
                    return Err(Self::refused("highlight action has too many items"));
                }
                if !Self::values_are_unique(remove_groups)
                    || !Self::values_are_unique(pieces.iter().map(|piece| piece.id.as_str()))
                    // A pure toggle-off deliberately reuses the removed group
                    // as its otherwise-unused stable `group` field. An add,
                    // however, must never tombstone the group it is creating.
                    || (!pieces.is_empty() && remove_groups.iter().any(|removed| removed == group))
                {
                    return Err(Self::refused("highlight action contains duplicate targets"));
                }
                for group in remove_groups {
                    Self::parse_uuid(group, "highlight")?;
                }
                for piece in pieces {
                    Self::parse_uuid(&piece.id, "highlight piece")?;
                    Self::validate_anchor(
                        *chapter,
                        piece.block,
                        piece.start,
                        piece.end,
                        &piece.excerpt,
                    )?;
                }
            }
            Self::ChangeHighlightColor { group, color } => {
                Self::parse_uuid(group, "highlight")?;
                Self::validate_color(color)?;
            }
            Self::DeleteHighlight { group } => {
                Self::parse_uuid(group, "highlight")?;
            }
            Self::AddComment {
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
                Self::validate_book_id(book_id)?;
                Self::parse_uuid(group, "highlight")?;
                Self::parse_uuid(id, "comment")?;
                Self::validate_anchor(*chapter, *block, *start, *end, excerpt)?;
                Self::validate_color(color)?;
                Self::validate_text(text, MAX_NOTE_BYTES, "comment")?;
            }
            Self::MoveAnnotation {
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
            } => {
                Self::parse_uuid(id, "annotation")?;
                Self::validate_anchor(*chapter, *block, *start, *end, excerpt)?;
            }
        }
        Ok(())
    }

    fn require_live_annotation(doc: &Doc, id: &str) -> Result<Uuid, ActionError> {
        let id = Self::parse_uuid(id, "annotation")?;
        if BooksDoc::annotation_is_live(doc, id) {
            Ok(id)
        } else {
            Err(Self::refused("annotation is no longer available"))
        }
    }

    fn require_new_annotation_target(doc: &Doc, id: Uuid, group: Uuid) -> Result<(), ActionError> {
        if BooksDoc::annotation_is_deleted(doc, id) {
            return Err(Self::refused(
                "annotation was deleted and cannot be recreated",
            ));
        }
        if BooksDoc::group_is_deleted(doc, group) {
            return Err(Self::refused("highlight is no longer available"));
        }
        Ok(())
    }

    fn require_live_highlight(doc: &Doc, group: Uuid) -> Result<(), ActionError> {
        if BooksDoc::group_is_deleted(doc, group) {
            return Err(Self::refused("highlight is no longer available"));
        }
        if BooksDoc::list_annotations(doc)
            .iter()
            .any(|annotation| annotation.kind == AnnoKind::Highlight && annotation.group == group)
        {
            Ok(())
        } else {
            Err(Self::refused("highlight is no longer available"))
        }
    }

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

    fn apply_remove_library_book(doc: &mut Doc, book_id: &str) -> Result<(), ActionError> {
        if !book_id.starts_with("book:") {
            return Err(Self::refused("book id is invalid"));
        }
        // Repeating an already-tombstoned removal is a successful convergence
        // operation, not a stale mutation failure.
        BooksDoc::remove_library_entry(doc, book_id);
        Ok(())
    }

    fn apply_edit_note(doc: &mut Doc, id: &str, note: &str) -> Result<(), ActionError> {
        let id = Self::require_live_annotation(doc, id)?;
        BooksDoc::write_annotation_note(doc, id, note)
            .then_some(())
            .ok_or_else(|| Self::refused("annotation is no longer available"))
    }

    fn apply_finalize_annotation(doc: &mut Doc, id: &str, created: u64) -> Result<(), ActionError> {
        let id = Self::require_live_annotation(doc, id)?;
        BooksDoc::write_annotation_created(doc, id, created)
            .then_some(())
            .ok_or_else(|| Self::refused("annotation is no longer available"))
    }

    fn apply_delete_annotation(doc: &mut Doc, id: &str) {
        // Delete is intentionally idempotent: writing an existing tombstone
        // again is a successful convergence operation, not a stale mutation.
        if let Ok(uuid) = id.parse::<Uuid>() {
            BooksDoc::remove_annotation(doc, uuid);
        }
    }

    fn apply_add_note(doc: &mut Doc, note: SerializedNote<'_>) -> Result<(), ActionError> {
        let uuid = Self::parse_uuid(note.id, "annotation")?;
        Self::require_new_annotation_target(doc, uuid, uuid)?;
        let anno = note.into_annotation(uuid, uuid);
        BooksDoc::upsert_annotation(doc, &anno);
        Ok(())
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "the serialized bookmark action is deliberately flat so every authored target and removal id is independently auditable"
    )]
    fn apply_set_bookmark(
        doc: &mut Doc,
        book_id: &str,
        id: &str,
        chapter: usize,
        block: usize,
        excerpt: &str,
        present: bool,
        remove_ids: &[String],
    ) -> Result<(), ActionError> {
        let removals = remove_ids
            .iter()
            .map(|id| Self::parse_uuid(id, "bookmark"))
            .collect::<Result<Vec<_>, _>>()?;
        let add_id = if present {
            let uuid = Self::parse_uuid(id, "bookmark")?;
            Self::require_new_annotation_target(doc, uuid, uuid)?;
            Some(uuid)
        } else {
            None
        };
        for id in removals {
            BooksDoc::remove_annotation(doc, id);
        }
        if let Some(uuid) = add_id {
            BooksDoc::upsert_annotation(
                doc,
                &Annotation {
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
                },
            );
        }
        Ok(())
    }

    fn apply_highlight_selection(
        doc: &mut Doc,
        book_id: &str,
        group: &str,
        chapter: usize,
        color: &str,
        remove_groups: &[String],
        pieces: &[HiPiece],
    ) -> Result<(), ActionError> {
        // Pure toggles only remove existing groups; an add must validate every
        // authored id before it changes anything, avoiding a partial action.
        let parsed_group = if pieces.is_empty() {
            None
        } else {
            Some(Self::parse_uuid(group, "highlight")?)
        };
        if let Some(group_uuid) = parsed_group {
            if BooksDoc::group_is_deleted(doc, group_uuid) {
                return Err(Self::refused(
                    "highlight was deleted and cannot be recreated",
                ));
            }
            for piece in pieces {
                let id = Self::parse_uuid(&piece.id, "highlight piece")?;
                Self::require_new_annotation_target(doc, id, group_uuid)?;
            }
        }
        let remove_groups = remove_groups
            .iter()
            .map(|group| Self::parse_uuid(group, "highlight"))
            .collect::<Result<Vec<_>, _>>()?;
        for group in remove_groups {
            BooksDoc::remove_group(doc, group);
        }
        if let Some(group_uuid) = parsed_group {
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
        Ok(())
    }

    fn apply_change_highlight_color(
        doc: &mut Doc,
        group: &str,
        color: &str,
    ) -> Result<(), ActionError> {
        let group_uuid = Self::parse_uuid(group, "highlight")?;
        Self::require_live_highlight(doc, group_uuid)?;
        let col = HiColor::from_str(color);
        let rows: Vec<Uuid> = BooksDoc::list_annotations(doc)
            .into_iter()
            .filter(|a| a.group == group_uuid)
            .map(|annotation| annotation.id)
            .collect();
        for id in rows {
            let _ = BooksDoc::write_annotation_color(doc, id, col);
        }
        Ok(())
    }

    fn apply_delete_highlight(doc: &mut Doc, group: &str) {
        // As with a single annotation delete, a repeated group tombstone is a
        // successful idempotent convergence operation.
        if let Ok(group_uuid) = group.parse::<Uuid>() {
            BooksDoc::remove_group(doc, group_uuid);
        }
    }

    fn apply_add_comment(
        doc: &mut Doc,
        group: &str,
        note: SerializedNote<'_>,
    ) -> Result<(), ActionError> {
        let group_uuid = Self::parse_uuid(group, "highlight")?;
        let cid = Self::parse_uuid(note.id, "comment")?;
        Self::require_live_highlight(doc, group_uuid)?;
        Self::require_new_annotation_target(doc, cid, group_uuid)?;
        let anno = note.into_annotation(cid, group_uuid);
        BooksDoc::upsert_annotation(doc, &anno);
        Ok(())
    }

    fn apply_move_annotation(
        doc: &mut Doc,
        id: &str,
        chapter: usize,
        block: usize,
        start: usize,
        end: usize,
        excerpt: &str,
    ) -> Result<(), ActionError> {
        let uuid = Self::require_live_annotation(doc, id)?;
        BooksDoc::move_annotation(
            doc,
            uuid,
            &AnnotationAnchor {
                chapter,
                block,
                start,
                end,
                excerpt: excerpt.to_string(),
            },
        )
        .then_some(())
        .ok_or_else(|| Self::refused("annotation is no longer available"))
    }
}

impl Action for EbookAction {
    #[allow(
        clippy::too_many_lines,
        reason = "the action enum is the canonical replay boundary; keeping exhaustive dispatch adjacent to the per-variant helpers makes action coverage auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        // Validate the complete envelope before the first CRDT write so a
        // malformed remote action is always an atomic refusal.
        self.validate()?;
        match self {
            EbookAction::RestoreLibraryBook {
                book_id,
                shared,
                title,
                author,
                subject,
                added,
            } => {
                BooksDoc::restore_library_entry(
                    doc,
                    &LibraryEntry {
                        id: book_id.clone(),
                        shared: *shared,
                        title: title.clone(),
                        author: author.clone(),
                        subject: subject.clone(),
                        added: *added,
                    },
                );
                Ok(())
            }
            EbookAction::SetReadingSettings { before, settings } => {
                BooksDoc::write_settings_delta(doc, before, settings);
                Ok(())
            }
            EbookAction::UpsertFont { font } => {
                BooksDoc::upsert_font(doc, font);
                Ok(())
            }
            EbookAction::DeleteFont { id } => {
                BooksDoc::remove_font(doc, id);
                Ok(())
            }
            EbookAction::SetDefaultFont { id } => {
                BooksDoc::write_default_font(doc, id);
                Ok(())
            }
            EbookAction::UpsertColorScheme { scheme } => {
                BooksDoc::upsert_scheme(doc, scheme);
                Ok(())
            }
            EbookAction::DeleteColorScheme { id } => {
                BooksDoc::remove_scheme(doc, id);
                Ok(())
            }
            EbookAction::SetLibraryPreferences {
                before,
                preferences,
            } => {
                BooksDoc::write_library_prefs_delta(doc, before, preferences);
                Ok(())
            }
            EbookAction::RemoveLibraryBook { book_id } => {
                Self::apply_remove_library_book(doc, book_id)
            }
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
            EbookAction::DeleteAnnotation { id } => {
                Self::apply_delete_annotation(doc, id);
                Ok(())
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
            } => Self::apply_add_note(
                doc,
                SerializedNote {
                    book_id,
                    id,
                    chapter: *chapter,
                    block: *block,
                    start: *start,
                    end: *end,
                    excerpt,
                    color,
                    text,
                },
            ),
            EbookAction::ToggleBookmark { .. } => Err(Self::refused(
                "legacy bookmark toggles cannot replay deterministically",
            )),
            EbookAction::SetBookmark {
                book_id,
                id,
                chapter,
                block,
                excerpt,
                present,
                remove_ids,
            } => Self::apply_set_bookmark(
                doc, book_id, id, *chapter, *block, excerpt, *present, remove_ids,
            ),
            EbookAction::HighlightSelection {
                book_id,
                group,
                chapter,
                color,
                remove_groups,
                pieces,
            } => Self::apply_highlight_selection(
                doc,
                book_id,
                group,
                *chapter,
                color,
                remove_groups,
                pieces,
            ),
            EbookAction::ChangeHighlightColor { group, color } => {
                Self::apply_change_highlight_color(doc, group, color)
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
            } => Self::apply_add_comment(
                doc,
                group,
                SerializedNote {
                    book_id,
                    id,
                    chapter: *chapter,
                    block: *block,
                    start: *start,
                    end: *end,
                    excerpt,
                    color,
                    text,
                },
            ),
            EbookAction::MoveAnnotation {
                id,
                chapter,
                block,
                start,
                end,
                excerpt,
            } => Self::apply_move_annotation(doc, id, *chapter, *block, *start, *end, excerpt),
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
/// every builder in this file and through `tilo_actions::ActionSpec::prompted`,
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

pub fn remove_library_book_action(book_id: &str) -> EbookAction {
    EbookAction::RemoveLibraryBook {
        book_id: book_id.to_string(),
    }
}

pub fn restore_library_book_action(entry: &LibraryEntry) -> EbookAction {
    EbookAction::RestoreLibraryBook {
        book_id: entry.id.clone(),
        shared: entry.shared,
        title: entry.title.clone(),
        author: entry.author.clone(),
        subject: entry.subject.clone(),
        added: entry.added,
    }
}

pub fn set_reading_settings_action(before: Settings, settings: Settings) -> EbookAction {
    EbookAction::SetReadingSettings { before, settings }
}

pub fn upsert_font_action(font: FontEntry) -> EbookAction {
    EbookAction::UpsertFont { font }
}

pub fn delete_font_action(id: &str) -> EbookAction {
    EbookAction::DeleteFont { id: id.into() }
}

pub fn set_default_font_action(id: &str) -> EbookAction {
    EbookAction::SetDefaultFont { id: id.into() }
}

pub fn upsert_color_scheme_action(scheme: ColorScheme) -> EbookAction {
    EbookAction::UpsertColorScheme { scheme }
}

pub fn delete_color_scheme_action(id: &str) -> EbookAction {
    EbookAction::DeleteColorScheme { id: id.into() }
}

pub fn set_library_preferences_action(
    before: LibraryPrefs,
    preferences: LibraryPrefs,
) -> EbookAction {
    EbookAction::SetLibraryPreferences {
        before,
        preferences,
    }
}

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

/// A fully authored standalone note. Group membership is added separately by
/// [`CommentDraft`], so note/comment call sites share one anchor contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteDraft {
    pub book_id: String,
    pub id: Uuid,
    pub anchor: AnnotationAnchor,
    pub color: HiColor,
    pub text: String,
}

impl NoteDraft {
    pub fn into_action(self) -> EbookAction {
        EbookAction::AddNote {
            book_id: self.book_id,
            id: self.id.to_string(),
            chapter: self.anchor.chapter,
            block: self.anchor.block,
            start: self.anchor.start,
            end: self.anchor.end,
            excerpt: self.anchor.excerpt,
            color: self.color.as_str().to_string(),
            text: self.text,
        }
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

/// Build an absolute bookmark mutation. Callers resolve `present` and the
/// exact old rows to remove from their authoring view before dispatch.
pub fn set_bookmark_action(
    book_id: &str,
    id: Uuid,
    chapter: usize,
    block: usize,
    excerpt: &str,
    present: bool,
    remove_ids: Vec<Uuid>,
) -> EbookAction {
    EbookAction::SetBookmark {
        book_id: book_id.to_string(),
        id: id.to_string(),
        chapter,
        block,
        excerpt: excerpt.to_string(),
        present,
        remove_ids: remove_ids.into_iter().map(|id| id.to_string()).collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookmarkPlan {
    pub book_id: String,
    pub id: Uuid,
    pub chapter: usize,
    pub block: usize,
    pub excerpt: String,
    pub present: bool,
    pub remove_ids: Vec<Uuid>,
}

impl BookmarkPlan {
    pub fn into_action(self) -> EbookAction {
        set_bookmark_action(
            &self.book_id,
            self.id,
            self.chapter,
            self.block,
            &self.excerpt,
            self.present,
            self.remove_ids,
        )
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
pub fn move_annotation_action(id: Uuid, anchor: AnnotationAnchor) -> EbookAction {
    EbookAction::MoveAnnotation {
        id: id.to_string(),
        chapter: anchor.chapter,
        block: anchor.block,
        start: anchor.start,
        end: anchor.end,
        excerpt: anchor.excerpt,
    }
}

pub fn delete_highlight_action(group: Uuid) -> EbookAction {
    EbookAction::DeleteHighlight {
        group: group.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentDraft {
    pub group: Uuid,
    pub note: NoteDraft,
}

impl CommentDraft {
    pub fn into_action(self) -> EbookAction {
        let note = self.note;
        let anchor = note.anchor;
        EbookAction::AddComment {
            book_id: note.book_id,
            group: self.group.to_string(),
            id: note.id.to_string(),
            chapter: anchor.chapter,
            block: anchor.block,
            start: anchor.start,
            end: anchor.end,
            excerpt: anchor.excerpt,
            color: note.color.as_str().to_string(),
            text: note.text,
        }
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

/// Ephemeral reader state needed to author actions for the currently open
/// book. It deliberately stays out of the compact search snapshot: only an
/// exact lookup of that one book receives these presentation actions.
#[derive(Debug, Clone)]
pub struct BookResourceCtx {
    pub book_id: String,
    pub chapter: usize,
    pub bookmark_block: usize,
    pub bookmark: BookmarkPlan,
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

/// Add the controls that are meaningful only for the reader's active book to
/// an already resolved canonical book resource. This preserves the bounded
/// directory while retaining the exact same action set as the on-page reader.
pub(crate) fn current_book_actions(mut resource: Resource, ctx: &BookResourceCtx) -> Resource {
    resource = resource.with_action(encode_spec(
        "toggle-bookmark",
        "Toggle bookmark",
        &ctx.bookmark.clone().into_action(),
    ));
    if let Some(plan) = &ctx.selection_highlight {
        resource = resource.with_action(encode_spec(
            "highlight-selection",
            "Highlight selection",
            &plan.clone().into_action(),
        ));
    }
    let book_id = ctx.book_id.clone();
    let chapter = ctx.chapter;
    let block = ctx.bookmark_block;
    resource = resource.with_action(ActionSpec::prompted(
        "add-note",
        "Add note",
        "Note",
        "your note…",
        ExecutionLocus::ClientLocal,
        move |text| {
            encode(
                &NoteDraft {
                    book_id: book_id.clone(),
                    id: Uuid::new_v4(),
                    anchor: AnnotationAnchor {
                        chapter,
                        block,
                        start: 0,
                        end: 0,
                        excerpt: String::new(),
                    },
                    color: HiColor::Yellow,
                    text: text.to_string(),
                }
                .into_action(),
            )
        },
    ));
    for (id, label, value) in [
        (
            "previous-page",
            "Previous page",
            EbookOperation::PreviousPage,
        ),
        ("next-page", "Next page", EbookOperation::NextPage),
        (
            "contents",
            "Toggle table of contents",
            EbookOperation::ToggleContents,
        ),
        ("notes", "Toggle notes", EbookOperation::ToggleNotes),
        (
            "settings",
            "Open reading settings",
            EbookOperation::OpenSettings,
        ),
    ] {
        resource = resource.with_action(operation(id, label, OperationKind::View, value));
    }
    resource
}

fn settings_resource(doc: &AutoCommit) -> Resource {
    let settings = BooksDoc::read_settings(doc);
    let mut root = Resource::new("ebook.settings", "ebook.settings", "Reading settings")
        .with_action(operation(
            "open",
            "Open reading settings",
            OperationKind::View,
            EbookOperation::OpenSettings,
        ));
    for mode in ReadMode::ALL {
        let mut next = settings.clone();
        next.mode = mode;
        root = root.with_action(encode_spec(
            &format!("mode-{}", mode.as_str()),
            &format!("Use {} reading", mode.label()),
            &set_reading_settings_action(settings.clone(), next),
        ));
    }
    for composition in CompositionStyle::ALL {
        let mut next = settings.clone();
        next.composition = composition;
        root = root.with_action(encode_spec(
            &format!("composition-{}", composition.as_str()),
            &format!("Use {} composition", composition.label()),
            &set_reading_settings_action(settings.clone(), next),
        ));
    }
    for theme in ReadTheme::ALL {
        let mut next = settings.clone();
        next.theme = theme;
        next.scheme_id.clear();
        root = root.with_action(encode_spec(
            &format!("theme-{}", theme.as_str()),
            &format!("Use {} page theme", theme.label()),
            &set_reading_settings_action(settings.clone(), next),
        ));
    }

    root = font_resources(root, doc);
    scheme_resources(root, doc)
}

fn font_resources(mut root: Resource, doc: &AutoCommit) -> Resource {
    for font in BooksDoc::read_fonts(doc) {
        let id = font.id.clone();
        let mut child = Resource::new(format!("ebook.font:{id}"), "ebook.font", font.name.clone())
            .with_action(operation(
                "open-settings",
                "Open font settings",
                OperationKind::View,
                EbookOperation::OpenSettings,
            ));
        let mut toggled = font.clone();
        toggled.shown = !toggled.shown;
        child = child.with_action(encode_spec(
            "toggle-quick-picker",
            if font.shown {
                "Hide from quick picker"
            } else {
                "Show in quick picker"
            },
            &upsert_font_action(toggled),
        ));
        let rename_source = font.clone();
        child = child.with_action(ActionSpec::prompted(
            "rename",
            "Rename font",
            "Name",
            "font name…",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                let mut renamed = rename_source.clone();
                renamed.name = name.trim().to_string();
                encode(&upsert_font_action(renamed))
            },
        ));
        child = child.with_action(
            encode_spec("delete", "Remove font", &delete_font_action(&id)).with_contract(
                tilo_actions::ActionContract {
                    risk: RiskLevel::Destructive,
                    confirmation: ConfirmationPolicy::Confirm,
                    retry: RetryPolicy::Idempotent,
                    reversible: true,
                    tags: vec!["font".into(), "tombstone".into()],
                    ..tilo_actions::ActionContract::default()
                },
            ),
        );
        root = root.with_child(child);
    }
    root
}

fn scheme_resources(mut root: Resource, doc: &AutoCommit) -> Resource {
    for scheme in BooksDoc::read_schemes(doc) {
        let id = scheme.id.clone();
        let mut child = Resource::new(
            format!("ebook.scheme:{id}"),
            "ebook.color-scheme",
            scheme.name.clone(),
        )
        .with_action(operation(
            "open-settings",
            "Open colour settings",
            OperationKind::View,
            EbookOperation::OpenSettings,
        ));
        let mut toggled = scheme.clone();
        toggled.enabled = !toggled.enabled;
        child = child.with_action(encode_spec(
            "toggle-enabled",
            if scheme.enabled {
                "Disable scheme"
            } else {
                "Enable scheme"
            },
            &upsert_color_scheme_action(toggled),
        ));
        if !scheme.builtin {
            let rename_source = scheme.clone();
            child = child.with_action(ActionSpec::prompted(
                "rename",
                "Rename scheme",
                "Name",
                "scheme name…",
                ExecutionLocus::ClientLocal,
                move |name: &str| {
                    let mut renamed = rename_source.clone();
                    renamed.name = name.trim().to_string();
                    encode(&upsert_color_scheme_action(renamed))
                },
            ));
            child = child.with_action(
                encode_spec("delete", "Delete scheme", &delete_color_scheme_action(&id))
                    .with_contract(tilo_actions::ActionContract {
                        risk: RiskLevel::Destructive,
                        confirmation: ConfirmationPolicy::Confirm,
                        retry: RetryPolicy::Idempotent,
                        reversible: true,
                        tags: vec!["colour-scheme".into(), "tombstone".into()],
                        ..tilo_actions::ActionContract::default()
                    }),
            );
        }
        root = root.with_child(child);
    }
    root
}

fn library_preference_actions(mut library: Resource, doc: &AutoCommit) -> Resource {
    let prefs = BooksDoc::read_library_prefs(doc);
    let mut toggled = prefs;
    toggled.hide_read = !toggled.hide_read;
    library = library.with_action(encode_spec(
        "toggle-hide-read",
        if prefs.hide_read {
            "Show read books"
        } else {
            "Hide read books"
        },
        &set_library_preferences_action(prefs, toggled),
    ));
    for sort in LibrarySort::ALL {
        let mut next = prefs;
        next.sort = sort;
        library = library.with_action(encode_spec(
            &format!("sort-{}", sort.as_str()),
            &format!("Sort by {}", sort.label()),
            &set_library_preferences_action(prefs, next),
        ));
    }
    library
}

/// Build the bounded canonical directory root. Exact book, chapter, and
/// annotation resources are deliberately lazy through [`ResourceProvider`].
pub fn provide_resources(_books: &[crate::model::Book], doc: &AutoCommit) -> Vec<Resource> {
    let library = Resource::new("ebook.library", "ebook.library", "Books library")
        .with_action(open_book_operation());
    let mut settings = settings_resource(doc);
    settings.children.clear();
    vec![library_preference_actions(library, doc), settings]
}

impl ResourceProvider for crate::app::BooksApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        // Directory discovery is intentionally root-only. Children are exact,
        // lazy resources resolved through the retained provider; exporting a
        // whole EPUB/annotation tree here would make opening Ctrl+K O(library).
        ["ebook.library", "ebook.settings"]
            .into_iter()
            .filter_map(|id| self.resource(doc, id))
            .collect()
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        if id == "ebook.library" {
            return Some(library_preference_actions(
                Resource::new("ebook.library", "ebook.library", "Books library")
                    .with_action(open_book_operation()),
                doc,
            ));
        }
        if id == "ebook.settings" {
            let mut settings = settings_resource(doc);
            settings.children.clear();
            return Some(settings);
        }

        if [
            "ebook.book:",
            "ebook.chapter:",
            "ebook.location:",
            "ebook.highlight:",
            "ebook.bookmark:",
            "ebook.note:",
            "ebook.comment:",
        ]
        .iter()
        .any(|prefix| id.starts_with(prefix))
        {
            let address = tilo_actions::ActionAddress::new(id, "open");
            let resource = self.resolve_search_resource(&address)?;
            if let Some(ctx) = self.current_book_resource_context()
                && id == format!("ebook.book:{}", ctx.book_id)
            {
                return Some(current_book_actions(resource, &ctx));
            }
            return Some(resource);
        }

        if id.strip_prefix("ebook.font:").is_some() {
            return settings_resource(doc)
                .children
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }
        if id.strip_prefix("ebook.scheme:").is_some() {
            return settings_resource(doc)
                .children
                .into_iter()
                .find(|resource| resource.id.as_str() == id);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::BooksApp;
    use tilo_actions::{ActionInput, ActionRegistry, LocalTransport};
    use tilo_automerge::ApplyIncoming;
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

    /// Discovery exports bounded roots; exact resources are resolved from
    /// the immutable search snapshot and never materialise siblings.
    #[test]
    fn ctrl_k_publishes_root_only_and_exact_resources() {
        let (app, doc, book_id, _group) = open_with_annotations();
        let roots = Widget::resources(&app, &doc);
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].id.as_str(), "ebook.library");
        assert!(roots[0].children.is_empty());
        assert_eq!(roots[1].id.as_str(), "ebook.settings");
        assert!(roots[1].children.is_empty());
        let books: Vec<crate::model::Book> = app
            .library()
            .iter()
            .map(|book| book.as_ref().clone())
            .collect();
        let annotations = BooksDoc::list_annotations(&doc);
        let provider = crate::search_provider::BookSearchProvider::new_with_registry(
            &books,
            &annotations,
            &BooksDoc::read_library_entries(&doc),
        );
        let address = tilo_actions::ActionAddress::new(format!("ebook.book:{book_id}"), "open");
        let book = ui::search::SearchProvider::resolve_action_resource(&provider, &address)
            .expect("exact book");
        assert!(book.children.is_empty());
        assert!(book.actions.iter().any(|action| action.id == "share"));
        assert!(
            book.actions
                .iter()
                .any(|action| action.id == "remove-from-library")
        );
        let current = Widget::resource(&app, &doc, &format!("ebook.book:{book_id}"))
            .expect("current exact book");
        for id in [
            "toggle-bookmark",
            "add-note",
            "previous-page",
            "next-page",
            "contents",
            "notes",
            "settings",
        ] {
            assert!(
                current.actions.iter().any(|action| action.id == id),
                "missing current-book action {id}"
            );
        }
        let settings = Widget::resource(&app, &doc, "ebook.settings").expect("settings");
        assert!(settings.children.is_empty());
        for font in BooksDoc::read_fonts(&doc) {
            assert!(Widget::resource(&app, &doc, &format!("ebook.font:{}", font.id)).is_some());
        }
        for scheme in BooksDoc::read_schemes(&doc) {
            assert!(Widget::resource(&app, &doc, &format!("ebook.scheme:{}", scheme.id)).is_some());
        }
    }

    #[test]
    fn library_open_action_is_canonical_even_without_loaded_books() {
        let doc = AutoCommit::new();
        let resources = provide_resources(&[], &doc);
        assert_eq!(resources.len(), 2);
        let address = tilo_actions::ActionAddress::new("ebook.library", "open-book");
        let encoded = tilo_actions::door::encode(&resources, &address, None)
            .expect("library action resolves through the canonical tree");
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(EbookOperation::OpenBook)
        );
    }

    #[test]
    fn remove_library_action_replays_as_a_keyed_tombstone() {
        use crate::model::LibraryEntry;

        let entry = LibraryEntry {
            id: "book:0123456789abcdef0123456789abcdef".into(),
            shared: true,
            title: "Shared title".into(),
            author: "A. Reader".into(),
            subject: String::new(),
            added: 7,
        };
        let mut a = AutoCommit::new();
        BooksDoc::upsert_library_entry(&mut a, &entry);
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let action = remove_library_book_action(&entry.id);
        let encoded = encode(&action);
        let envelope = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &envelope, &registry()).unwrap();

        for peer in [&a, &b] {
            assert!(BooksDoc::library_is_removed(peer, &entry.id));
            assert!(BooksDoc::read_library_entries(peer).is_empty());
        }
        assert_eq!(a.get_heads(), b.get_heads(), "replay converges exactly");
    }

    #[test]
    fn encoded_import_action_restores_the_same_content_addressed_row_on_a_peer() {
        use crate::model::LibraryEntry;

        let entry = LibraryEntry {
            id: "book:fedcba9876543210fedcba9876543210".into(),
            shared: true,
            title: "A shared book".into(),
            author: "A. Reader".into(),
            subject: "Architecture".into(),
            added: 42,
        };
        let action = restore_library_book_action(&entry);
        let encoded = encode(&action);
        let mut author = AutoCommit::new();
        let envelope = tilo_actions::apply_local(&mut author, &action, encoded).unwrap();
        let mut peer = AutoCommit::new();
        tilo_actions::replay(&mut peer, &envelope, &registry()).unwrap();

        assert_eq!(BooksDoc::read_library_entries(&author), vec![entry.clone()]);
        assert_eq!(BooksDoc::read_library_entries(&peer), vec![entry]);
        assert_eq!(author.get_heads(), peer.get_heads());
    }

    #[test]
    fn encoded_registry_actions_converge_without_lost_siblings_or_resurrection() {
        use crate::model::{FontEntry, FontKind, ReadTheme};

        let mut base = AutoCommit::new().with_actor(ActorId::random());
        let original = Settings::default();
        let mut left = base.fork().with_actor(ActorId::random());
        let mut right = base.fork().with_actor(ActorId::random());
        let mut themed = original.clone();
        themed.theme = ReadTheme::Dark;
        let mut spaced = original.clone();
        spaced.line_spacing = 1.55;
        for (peer, action) in [
            (
                &mut left,
                set_reading_settings_action(original.clone(), themed),
            ),
            (&mut right, set_reading_settings_action(original, spaced)),
        ] {
            let encoded = encode(&action);
            registry().decode(&encoded).unwrap().apply(peer).unwrap();
        }
        let left_font = FontEntry::new(
            "font-left",
            FontKind::Named("Atkinson Hyperlegible".into()),
            "Atkinson",
            1.0,
            true,
        );
        let right_font = FontEntry::new(
            "font-right",
            FontKind::Named("Literata".into()),
            "Literata",
            1.08,
            true,
        );
        for (peer, action) in [
            (&mut left, upsert_font_action(left_font.clone())),
            (&mut right, upsert_font_action(right_font.clone())),
        ] {
            let encoded = encode(&action);
            registry().decode(&encoded).unwrap().apply(peer).unwrap();
        }
        left.merge_incoming(&mut right).unwrap();
        right.merge_incoming(&mut left).unwrap();
        for peer in [&left, &right] {
            let settings = BooksDoc::read_settings(peer);
            assert_eq!(settings.theme, ReadTheme::Dark);
            assert_eq!(settings.line_spacing, 1.55);
            let ids: Vec<String> = BooksDoc::read_fonts(peer)
                .into_iter()
                .map(|font| font.id)
                .collect();
            assert!(ids.contains(&left_font.id), "left sibling survives");
            assert!(ids.contains(&right_font.id), "right sibling survives");
        }

        let mut deleting = left.fork().with_actor(ActorId::random());
        let mut editing = left.fork().with_actor(ActorId::random());
        let delete = delete_font_action(&left_font.id);
        let mut late_edit = left_font.clone();
        late_edit.name = "Late offline rename".into();
        for (peer, action) in [
            (&mut deleting, delete),
            (&mut editing, upsert_font_action(late_edit)),
        ] {
            let encoded = encode(&action);
            registry().decode(&encoded).unwrap().apply(peer).unwrap();
        }
        deleting.merge_incoming(&mut editing).unwrap();
        editing.merge_incoming(&mut deleting).unwrap();
        for peer in [&deleting, &editing] {
            assert!(
                BooksDoc::read_fonts(peer)
                    .iter()
                    .all(|font| font.id != left_font.id),
                "the stable-id tombstone wins over an offline edit"
            );
        }
        assert_eq!(
            BooksDoc::read_fonts(&deleting),
            BooksDoc::read_fonts(&editing),
            "both replicas materialise the same stable registry"
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
        tilo_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut tx).unwrap();

        let left = BooksDoc::annotations_for(&doc, &book_id);
        assert_eq!(left.len(), 2, "only the standalone note + bookmark survive");
        assert!(
            left.iter().all(|a| a.group != group),
            "no row of the deleted group remains (highlight + both comments gone)"
        );
    }

    #[test]
    fn stale_annotation_edit_finalize_and_move_are_refused() {
        let (_app, mut doc, book_id, _group) = open_with_annotations();
        let id = BooksDoc::annotations_for(&doc, &book_id)
            .into_iter()
            .find(|annotation| annotation.kind == AnnoKind::Note)
            .expect("standalone note")
            .id;
        BooksDoc::remove_annotation(&mut doc, id);

        for action in [
            edit_note_action(id, "late edit"),
            finalize_annotation_action(id, 1_700_000_123_456),
            move_annotation_action(
                id,
                AnnotationAnchor {
                    chapter: 0,
                    block: 9,
                    start: 0,
                    end: 5,
                    excerpt: "late move".into(),
                },
            ),
        ] {
            assert!(
                matches!(action.apply(&mut doc), Err(ActionError::Refused(_))),
                "a stale annotation mutation must report refusal"
            );
        }

        // Repeating a delete is deliberately convergence-safe and succeeds.
        assert!(delete_annotation_action(id).apply(&mut doc).is_ok());
    }

    #[test]
    fn stale_highlight_group_mutations_are_refused() {
        let (_app, mut doc, book_id, group) = open_with_annotations();
        BooksDoc::remove_group(&mut doc, group);
        let new_id = Uuid::from_u128(0xBADD_CAFE_0000_0000_0000_0000_0000_0001);

        let actions = [
            CommentDraft {
                group,
                note: NoteDraft {
                    book_id: book_id.clone(),
                    id: new_id,
                    anchor: AnnotationAnchor {
                        chapter: 0,
                        block: 3,
                        start: 0,
                        end: 12,
                        excerpt: "a passage".into(),
                    },
                    color: HiColor::Green,
                    text: "late comment".into(),
                },
            }
            .into_action(),
            change_color_action(group, HiColor::Pink),
            EbookAction::HighlightSelection {
                book_id,
                group: group.to_string(),
                chapter: 0,
                color: HiColor::Blue.as_str().into(),
                remove_groups: Vec::new(),
                pieces: vec![HiPiece {
                    id: new_id.to_string(),
                    block: 3,
                    start: 0,
                    end: 12,
                    excerpt: "a passage".into(),
                }],
            },
        ];
        for action in actions {
            assert!(
                matches!(action.apply(&mut doc), Err(ActionError::Refused(_))),
                "a mutation against a deleted highlight group must report refusal"
            );
        }

        assert!(delete_highlight_action(group).apply(&mut doc).is_ok());
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
        let action = set_bookmark_action(
            &book_id,
            Uuid::from_u128(0xDEAD_BEEF_0000_0000_0000_0000_0000_0001),
            0,
            9,
            "bm",
            true,
            Vec::new(),
        );
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        tilo_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut tx).unwrap();
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

        let action = CommentDraft {
            group: Uuid::from_u128(0x1111_2222_3333_4444_5555_6666_7777_8888),
            note: NoteDraft {
                book_id: book_id.clone(),
                id: Uuid::from_u128(0x0BAD_0000_0000_0000_0000_0000_0000_00FE),
                anchor: AnnotationAnchor {
                    chapter: 0,
                    block: 3,
                    start: 0,
                    end: 12,
                    excerpt: "a passage".into(),
                },
                color: HiColor::Green,
                text: "converge".into(),
            },
        }
        .into_action();
        let encoded = encode(&action);

        // A applies locally, producing the envelope; B replays it.
        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

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
    fn absolute_bookmark_actions_converge_after_divergent_peer_changes() {
        let (_app, mut base, book_id, _group) = open_with_annotations();
        let mut left = base.fork().with_actor(ActorId::random());
        let mut right = base.fork().with_actor(ActorId::random());
        let authored = Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0001);
        let concurrent = Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0002);
        let add = set_bookmark_action(&book_id, authored, 0, 9, "new bookmark", true, Vec::new());

        // A second reader adds its own bookmark before it receives the exact
        // authored add action. Both replicas execute an add, never an
        // accidental receiver-state-dependent delete.
        add.apply(&mut left).expect("author add applies");
        let mut competing = Annotation::new(&book_id, AnnoKind::Bookmark, 0, 9);
        competing.id = concurrent;
        competing.group = concurrent;
        BooksDoc::upsert_annotation(&mut right, &competing);
        add.apply(&mut right).expect("peer replays authored add");
        left.merge_incoming(&mut right).expect("replicas merge");
        right.merge_incoming(&mut left).expect("replicas converge");

        for doc in [&left, &right] {
            let ids = BooksDoc::annotations_for(doc, &book_id)
                .into_iter()
                .filter(|annotation| annotation.kind == AnnoKind::Bookmark && annotation.block == 9)
                .map(|annotation| annotation.id)
                .collect::<Vec<_>>();
            assert!(ids.contains(&authored));
            assert!(ids.contains(&concurrent));
        }

        // The authored removal names exactly the bookmark it saw. It cannot
        // delete the concurrent bookmark just because a receiver has one.
        let remove = set_bookmark_action(
            &book_id,
            Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0003),
            0,
            9,
            "new bookmark",
            false,
            vec![authored],
        );
        remove.apply(&mut left).expect("author remove applies");
        remove
            .apply(&mut right)
            .expect("peer replays authored remove");
        left.merge_incoming(&mut right)
            .expect("replicas merge removal");
        right
            .merge_incoming(&mut left)
            .expect("replicas converge removal");
        for doc in [&left, &right] {
            let ids = BooksDoc::annotations_for(doc, &book_id)
                .into_iter()
                .filter(|annotation| annotation.kind == AnnoKind::Bookmark && annotation.block == 9)
                .map(|annotation| annotation.id)
                .collect::<Vec<_>>();
            assert_eq!(ids, vec![concurrent]);
        }
    }

    #[test]
    fn legacy_bookmark_toggle_is_decoded_but_refused() {
        let (_app, mut doc, book_id, _group) = open_with_annotations();
        let legacy = toggle_bookmark_action(
            &book_id,
            Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0004),
            0,
            9,
            "legacy",
        );
        assert!(matches!(
            legacy.apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
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
        let envelope = tilo_actions::apply_local(&mut a, &action, encoded).expect("action applies");
        tilo_actions::replay(&mut b, &envelope, &registry()).expect("action replays");

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
        let envelope = tilo_actions::apply_local(&mut a, &action, encode(&action))
            .expect("finalisation applies");
        tilo_actions::replay(&mut b, &envelope, &registry()).expect("finalisation replays");

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

        let action = move_annotation_action(
            hl.id,
            AnnotationAnchor {
                chapter: 0,
                block: 9,
                start: 0,
                end: 5,
                excerpt: "moved".into(),
            },
        );
        let encoded = encode(&action);
        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();

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
        let books: Vec<crate::model::Book> = app
            .library()
            .iter()
            .map(|book| book.as_ref().clone())
            .collect();
        let annotations = BooksDoc::list_annotations(&doc);
        let provider = crate::search_provider::BookSearchProvider::new_with_registry(
            &books,
            &annotations,
            &BooksDoc::read_library_entries(&doc),
        );
        let hl = ui::search::SearchProvider::resolve_action_resource(
            &provider,
            &tilo_actions::ActionAddress::new(format!("ebook.highlight:{group}"), "open"),
        )
        .expect("exact highlight resource");
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
        let comment = annotations
            .iter()
            .find(|annotation| annotation.kind == AnnoKind::Note && annotation.group == group)
            .expect("comment");
        let exact_comment = ui::search::SearchProvider::resolve_action_resource(
            &provider,
            &tilo_actions::ActionAddress::new(format!("ebook.comment:{}", comment.id), "open"),
        )
        .expect("exact comment resource");
        let edit_spec = exact_comment
            .actions
            .iter()
            .find(|a| a.id == "edit-comment")
            .unwrap();
        let from_spec = prompted_encode(edit_spec, "revised");
        let from_builder = encode(&edit_note_action(comment.id, "revised"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "edit is one source of truth"
        );
    }

    #[test]
    fn exact_resource_lookup_materialises_an_unhydrated_library_book() {
        let mut doc = AutoCommit::new();
        BooksDoc::upsert_library_entry(
            &mut doc,
            &LibraryEntry {
                id: "book:remote-only".into(),
                shared: true,
                title: "Waiting for bytes".into(),
                author: "Remote Reader".into(),
                subject: String::new(),
                added: 1,
            },
        );

        let provider = crate::search_provider::BookSearchProvider::new_with_registry(
            &[],
            &[],
            &BooksDoc::read_library_entries(&doc),
        );
        let resource = ui::search::SearchProvider::resolve_action_resource(
            &provider,
            &tilo_actions::ActionAddress::new("ebook.book:book:remote-only", "open"),
        );
        let resource = resource.expect("remote bytes expose a typed hydrate capability");
        let open = resource
            .actions
            .iter()
            .find(|action| action.id == "open")
            .expect("download and open");
        assert_eq!(open.label, "Download and open");
        let ActionInput::Immediate(operation) = &open.input else {
            panic!("remote hydrate is replayable");
        };
        assert_eq!(
            decode_operation(&operation.type_id, &operation.payload).unwrap(),
            Some(EbookOperation::HydrateSharedBook {
                book_id: "book:remote-only".into(),
            })
        );
        let roots = provide_resources(&[], &doc);
        assert!(roots[0].children.is_empty());
        let resource = ui::search::SearchProvider::search(&provider, "Waiting")
            .pop()
            .expect("remote metadata row");
        assert_eq!(resource.kind, crate::search_provider::BOOK_KIND);
    }

    #[test]
    fn concurrent_note_edit_and_move_preserve_both_fields() {
        let mut base = AutoCommit::new().with_actor(ActorId::random());
        let note = Annotation::new("book:concurrency", AnnoKind::Note, 0, 1)
            .with_excerpt("old anchor")
            .with_note("old note");
        BooksDoc::upsert_annotation(&mut base, &note);
        let mut editing = base.fork().with_actor(ActorId::random());
        let mut moving = base.fork().with_actor(ActorId::random());

        edit_note_action(note.id, "edited concurrently")
            .apply(&mut editing)
            .unwrap();
        move_annotation_action(
            note.id,
            AnnotationAnchor {
                chapter: 2,
                block: 7,
                start: 3,
                end: 12,
                excerpt: "new anchor".into(),
            },
        )
        .apply(&mut moving)
        .unwrap();
        editing.merge_incoming(&mut moving).unwrap();
        moving.merge_incoming(&mut editing).unwrap();

        for peer in [&editing, &moving] {
            let merged = BooksDoc::list_annotations(peer)
                .into_iter()
                .find(|annotation| annotation.id == note.id)
                .unwrap();
            assert_eq!(merged.note, "edited concurrently");
            assert_eq!((merged.chapter, merged.block), (2, 7));
            assert_eq!((merged.start, merged.end), (3, 12));
            assert_eq!(merged.excerpt, "new anchor");
        }
    }

    #[test]
    fn concurrent_comment_edit_and_group_recolour_preserve_both_fields() {
        let mut base = AutoCommit::new().with_actor(ActorId::random());
        let group = Uuid::from_u128(0xABCD);
        let highlight =
            Annotation::new("book:concurrency", AnnoKind::Highlight, 0, 1).with_group(group);
        let comment = Annotation::new("book:concurrency", AnnoKind::Note, 0, 1)
            .with_group(group)
            .with_note("old comment");
        BooksDoc::upsert_annotation(&mut base, &highlight);
        BooksDoc::upsert_annotation(&mut base, &comment);
        let mut editing = base.fork().with_actor(ActorId::random());
        let mut recolouring = base.fork().with_actor(ActorId::random());

        edit_note_action(comment.id, "edited concurrently")
            .apply(&mut editing)
            .unwrap();
        change_color_action(group, HiColor::Purple)
            .apply(&mut recolouring)
            .unwrap();
        editing.merge_incoming(&mut recolouring).unwrap();
        recolouring.merge_incoming(&mut editing).unwrap();

        for peer in [&editing, &recolouring] {
            let rows = BooksDoc::list_annotations(peer);
            assert!(
                rows.iter()
                    .all(|annotation| annotation.color == HiColor::Purple)
            );
            assert_eq!(
                rows.iter()
                    .find(|annotation| annotation.id == comment.id)
                    .unwrap()
                    .note,
                "edited concurrently"
            );
        }
    }

    #[test]
    fn malformed_or_oversized_actions_are_atomic_refusals() {
        let (_app, mut doc, book_id, group) = open_with_annotations();
        let before = BooksDoc::annotations_for(&doc, &book_id);
        let invalid = EbookAction::HighlightSelection {
            book_id: book_id.clone(),
            group: Uuid::new_v4().to_string(),
            chapter: 0,
            color: HiColor::Yellow.as_str().into(),
            remove_groups: vec![group.to_string()],
            pieces: vec![HiPiece {
                id: Uuid::new_v4().to_string(),
                block: 1,
                start: 9,
                end: 2,
                excerpt: "backwards".into(),
            }],
        };
        assert!(matches!(
            invalid.apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(
            BooksDoc::annotations_for(&doc, &book_id).len(),
            before.len()
        );
        assert!(!BooksDoc::group_is_deleted(&doc, group));

        let note_id = before
            .iter()
            .find(|annotation| annotation.kind == AnnoKind::Note)
            .unwrap()
            .id;
        let oversized = edit_note_action(note_id, &"x".repeat(MAX_NOTE_BYTES + 1));
        assert!(matches!(
            oversized.apply(&mut doc),
            Err(ActionError::Refused(_))
        ));
        assert_eq!(
            BooksDoc::list_annotations(&doc)
                .into_iter()
                .find(|annotation| annotation.id == note_id)
                .unwrap()
                .note,
            before
                .iter()
                .find(|annotation| annotation.id == note_id)
                .unwrap()
                .note
        );
    }
}
