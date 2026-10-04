//! The PDF reader's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every markup-mutating operation the reader performs on its shared markup
//! store is a [`PdfViewerAction`]: a small serializable value that
//! deterministically mutates the CRDT. Because the markup already lives in the
//! shared document (`root["annotations"]` / `root["comments"]`, keyed by id —
//! see [`crate::doc`]), an action replicates and replays for free, and feeds
//! straight back into the reader's annotation + comment lists. The exact same
//! [`EncodedAction`] is what the on-page affordances (the selection HUD's
//! highlight swatches, the toolbar colour swatch, the comment rail's edit /
//! reply / resolve / delete buttons, the note-marker drag) dispatch and what the
//! Ctrl+K palette dispatches — one source of truth, one replay path.
//!
//! The nested resource tree mirrors the data model: the open document owns its
//! annotations; an annotation owns the comments pinned to it. Free comments (not
//! pinned to any annotation) hang directly off the document. Each resource
//! exports the [`ActionSpec`]s Ctrl+K offers for it.
//!
//! Presentation — which page is shown, the zoom / pan, the current selection,
//! the active tool, and the sidebar tab — stays out of the document and never
//! becomes an action. The *page plan* is different: it is a durable CRDT list
//! of stable occurrence ids and immutable source-page references. Page
//! duplicate/delete/move therefore use [`PdfViewerAction`] just like markup,
//! while loading/rasterising source bytes remains a local capability. A peer
//! that does not have the source can still converge on (and inspect) the shared
//! intent/order, and reports that its local renderer cannot materialise it.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, PermissionRequirement,
    Resource, ResultSpec, RetryPolicy,
};

use crate::doc::{ImportedPage, PagePlanEntry, PdfDoc};
use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, MARGIN, MarkupStyle, PAGE_H, PAGE_W,
    PtRect, Reply, Signature, SignatureInk, Swatch,
};

/// Registry `type_id` for every PDF-reader action.
pub const PDF_VIEWER_ACTION_TYPE: &str = "pdf_viewer.action";
pub const PDF_VIEWER_OPERATION_TYPE: &str = "pdf_viewer.operation";

/// The action palette is refreshed frequently. Keep that contextual snapshot
/// small even for a scanned book with thousands of pages or a heavily-reviewed
/// contract, while [`ResourceProvider::resource`] retains complete reachability
/// by stable id.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;
/// A find query can produce hundreds of matches, but resources are consumed by
/// global search, agents and history.  Project a useful first page without
/// turning a pathological PDF into an unbounded action directory.
pub const SEARCH_RESULT_RESOURCE_LIMIT: usize = 32;

/// Stable, non-secret identity for one local find projection.  Include the
/// matching options because the same words under a different option set are a
/// different result set.  This is deliberately an address token rather than a
/// display value; the original query remains in the operation payload.
pub fn search_result_token(query: &str, whole_word: bool, case_sensitive: bool) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, query.trim().as_bytes());
    std::hash::Hasher::write(
        &mut hasher,
        &[u8::from(whole_word), u8::from(case_sensitive)],
    );
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

/// Return the stable resource id for one source-PDF outline entry.
///
/// Outline order is presentation state: changing an earlier bookmark must not
/// make a saved action for a later bookmark open a different section.  The
/// identity therefore covers the immutable entry value plus its occurrence
/// among identical entries (PDFs may legitimately repeat a title/page/level).
/// A changed or removed entry fails closed during resolution instead of being
/// rebound to a new ordinal.
pub fn outline_resource_id(ctx: &PdfResourceCtx, index: usize) -> Option<String> {
    let (title, page, level) = ctx.outline.get(index)?;
    let occurrence = ctx
        .outline
        .get(..index)?
        .iter()
        .filter(|(prior_title, prior_page, prior_level)| {
            prior_title == title && prior_page == page && prior_level == level
        })
        .count();
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, title.as_bytes());
    std::hash::Hasher::write_u8(&mut hasher, 0);
    std::hash::Hasher::write(&mut hasher, page.to_string().as_bytes());
    std::hash::Hasher::write_u8(&mut hasher, 0);
    std::hash::Hasher::write(&mut hasher, level.to_string().as_bytes());
    std::hash::Hasher::write_u8(&mut hasher, 0);
    std::hash::Hasher::write(&mut hasher, occurrence.to_string().as_bytes());
    let hash = std::hash::Hasher::finish(&hasher);
    Some(format!("pdf.outline:{}:{hash:016x}", ctx.doc_id))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfOperation {
    PreviousPage,
    NextPage,
    ZoomIn,
    ZoomOut,
    FitPage,
    FitWidth,
    ToggleSidebar,
    ToggleRail,
    OpenSearch,
    GoToPage {
        page: usize,
    },
    /// A materialised, bounded find result.  Carry the complete local query
    /// state so invocation restores the exact result before navigating.
    GoToSearchResult {
        query: String,
        whole_word: bool,
        case_sensitive: bool,
        index: usize,
        page: usize,
    },
    Open {
        path: Option<String>,
    },
    SaveAs {
        path: Option<String>,
    },
    /// Choose how the highlight tool marks text: `highlight`, `underline`
    /// or `strikeout`.
    MarkupStyle {
        style: String,
    },
}

#[cfg(test)]
mod file_action_tests {
    use super::*;
    #[test]
    fn path_argument_binds_into_background_open() {
        let spec = file_operation("open", "Open PDF", false);
        let encoded = spec
            .build_json(
                serde_json::json!({"path":"/tmp/a.pdf"})
                    .as_object()
                    .unwrap(),
            )
            .unwrap();
        assert_eq!(
            decode_operation(&encoded.type_id, &encoded.payload).unwrap(),
            Some(PdfOperation::Open {
                path: Some("/tmp/a.pdf".into())
            })
        );
        assert!(matches!(
            spec.contract.execution,
            ExecutionMode::Background { .. }
        ));
        assert_eq!(spec.contract.result, ResultSpec::Job);
    }
}

/// Decode a PDF Viewer operation envelope.
///
/// # Errors
///
/// Returns a JSON decoding error when `type_id` identifies this operation
/// family but `payload` is malformed.
pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<PdfOperation>, String> {
    if type_id != PDF_VIEWER_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

#[allow(
    clippy::expect_used,
    clippy::needless_pass_by_value,
    reason = "the owned enum is inspected after serialization and contains no fallible serde fields"
)]
fn operation(id: &str, label: &str, kind: OperationKind, value: PdfOperation) -> ActionSpec {
    let spec = ActionSpec::operation(id, label, kind, PDF_VIEWER_OPERATION_TYPE, &value)
        .expect("PdfOperation is always serializable");
    match value {
        PdfOperation::FitPage | PdfOperation::FitWidth => spec
            .with_contract(ActionContract::view())
            .retry_policy(RetryPolicy::Idempotent),
        PdfOperation::PreviousPage
        | PdfOperation::NextPage
        | PdfOperation::ZoomIn
        | PdfOperation::ZoomOut
        | PdfOperation::ToggleSidebar
        | PdfOperation::ToggleRail
        | PdfOperation::OpenSearch
        | PdfOperation::GoToPage { .. }
        | PdfOperation::GoToSearchResult { .. }
        | PdfOperation::MarkupStyle { .. } => spec.with_contract(ActionContract::view()),
        PdfOperation::Open { .. } | PdfOperation::SaveAs { .. } => spec.destructive(),
    }
}

fn file_operation(id: &str, label: &str, save: bool) -> ActionSpec {
    ActionSpec::typed_operation(
        id,
        label,
        "optional absolute path",
        OperationKind::Effect,
        PDF_VIEWER_OPERATION_TYPE,
        vec![ActionParam::optional(
            "path",
            ParamType::Text,
            "Filesystem path; omit to use the system picker",
        )],
        move |args: &ActionArgs| {
            let path = args
                .text("path")
                .filter(|path| !path.trim().is_empty())
                .map(str::to_string);
            let operation = if save {
                PdfOperation::SaveAs { path }
            } else {
                PdfOperation::Open { path }
            };
            EncodedAction {
                type_id: PDF_VIEWER_OPERATION_TYPE.into(),
                payload: serde_json::to_vec(&operation).unwrap_or_default(),
            }
        },
    )
    .with_contract(ActionContract {
        risk: tilo_actions::RiskLevel::Destructive,
        confirmation: tilo_actions::ConfirmationPolicy::Confirm,
        execution: ExecutionMode::Background { cancellable: true },
        result: ResultSpec::Job,
        ..ActionContract::default()
    })
}

/// Serializable mirror of an annotation's geometry. [`model::AnnoKind`] carries
/// non-serde types and lives in the pure model, so the wire form is spelled out
/// here in primitives — which also keeps a replayed payload byte-identical.
///
/// [`model::AnnoKind`]: crate::model::AnnoKind
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AnnoGeom {
    /// One highlight band (the form envelopes had before multi-band markup).
    Highlight {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    },
    /// Text markup over one band per line, with the engine character range
    /// it covers when it was made from text.
    Markup {
        style: String,
        quads: Vec<(f64, f64, f64, f64)>,
        chars: Option<(u32, u32)>,
    },
    Ink {
        points: Vec<(f64, f64)>,
    },
    Note {
        x: f64,
        y: f64,
    },
    /// Freehand ink of more than one stroke.
    InkStrokes {
        strokes: Vec<Vec<(f64, f64)>>,
    },
    /// A free text box.
    FreeText {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        text: String,
        size: f64,
    },
}

impl AnnoGeom {
    fn from_kind(kind: &AnnoKind, chars: Option<(u32, u32)>) -> Self {
        match kind {
            AnnoKind::Markup { style, quads } => AnnoGeom::Markup {
                style: style.key().to_string(),
                quads: quads.iter().map(|q| (q.x, q.y, q.w, q.h)).collect(),
                chars,
            },
            AnnoKind::Ink { strokes } => match strokes.as_slice() {
                [points] => AnnoGeom::Ink {
                    points: points.clone(),
                },
                _ => AnnoGeom::InkStrokes {
                    strokes: strokes.clone(),
                },
            },
            AnnoKind::Note { x, y } => AnnoGeom::Note { x: *x, y: *y },
            AnnoKind::FreeText { rect, text, size } => AnnoGeom::FreeText {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
                text: text.clone(),
                size: *size,
            },
        }
    }

    fn to_kind(&self) -> (AnnoKind, Option<(u32, u32)>) {
        match self {
            AnnoGeom::Highlight { x, y, w, h } => {
                (AnnoKind::highlight(PtRect::new(*x, *y, *w, *h)), None)
            }
            AnnoGeom::Markup {
                style,
                quads,
                chars,
            } => (
                AnnoKind::Markup {
                    style: MarkupStyle::parse(style).unwrap_or(MarkupStyle::Highlight),
                    quads: quads
                        .iter()
                        .filter(|(x, y, w, h)| [x, y, w, h].iter().all(|n| n.is_finite()))
                        .map(|(x, y, w, h)| PtRect::new(*x, *y, *w, *h))
                        .collect(),
                },
                *chars,
            ),
            AnnoGeom::Ink { points } => (AnnoKind::ink(points.clone()), None),
            AnnoGeom::InkStrokes { strokes } => (
                AnnoKind::Ink {
                    strokes: strokes.clone(),
                },
                None,
            ),
            AnnoGeom::Note { x, y } => (AnnoKind::Note { x: *x, y: *y }, None),
            AnnoGeom::FreeText {
                x,
                y,
                w,
                h,
                text,
                size,
            } => (
                AnnoKind::FreeText {
                    rect: PtRect::new(*x, *y, *w, *h),
                    text: text.clone(),
                    size: if size.is_finite() && *size > 0.0 {
                        *size
                    } else {
                        crate::model::FREE_TEXT_SIZE
                    },
                },
                None,
            ),
        }
    }
}

/// A comment thread as an import carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommentWire {
    pub id: String,
    pub anno_id: Option<String>,
    pub page: usize,
    pub author: String,
    pub body: String,
    pub created: i64,
    /// `(id, author, body)` per reply.
    pub replies: Vec<(String, String, String)>,
}

/// An imported annotation, in its page's user space.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedAnnotationWire {
    pub id: String,
    pub page: usize,
    pub page_id: String,
    pub geom: AnnoGeom,
    pub color: String,
    pub author: String,
    pub created_ms: i64,
}

impl ImportedAnnotationWire {
    fn to_annotation(&self) -> Annotation {
        let (kind, chars) = self.geom.to_kind();
        Annotation {
            id: self.id.clone(),
            page: self.page,
            kind,
            color: Swatch::parse(&self.color),
            author: self.author.clone(),
            chars,
            created_ms: self.created_ms,
            space: GeomSpace::User,
        }
    }
}

/// A form value as carried by an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormValueWire {
    Text(String),
    Checked { on: bool, export: Option<String> },
    Choice(String),
}

impl FormValueWire {
    pub fn from_entry(entry: &FormEntry) -> Self {
        match entry {
            FormEntry::Text(t) => FormValueWire::Text(t.clone()),
            FormEntry::Checked { on, export } => FormValueWire::Checked {
                on: *on,
                export: export.clone(),
            },
            FormEntry::Choice(c) => FormValueWire::Choice(c.clone()),
        }
    }

    fn to_entry(&self) -> FormEntry {
        match self {
            FormValueWire::Text(t) => FormEntry::Text(t.clone()),
            FormValueWire::Checked { on, export } => FormEntry::Checked {
                on: *on,
                export: export.clone(),
            },
            FormValueWire::Choice(c) => FormEntry::Choice(c.clone()),
        }
    }
}

fn space(user: bool) -> GeomSpace {
    if user {
        GeomSpace::User
    } else {
        GeomSpace::Page
    }
}

/// Every collaborative operation the reader exports on its markup store.
///
/// Determinism contract: ids and the comment ordinal are minted at *authoring*
/// time and carried in the payload, never inside [`Action::apply`], which must
/// not touch the RNG or the wall-clock. A malformed payload is a **no-op**
/// rather than an error, so a bad replay can't wedge a peer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PdfViewerAction {
    /// Insert another occurrence of an immutable source page directly after
    /// `id`. Both identities are captured in the envelope, so replay and undo
    /// reproduce the same shared page plan on every peer.
    DuplicatePagePlan {
        id: String,
        new_id: String,
        source_page: usize,
        imported: Option<ImportedPage>,
    },
    /// Content-addressed pages, inserted atomically after `after_id`. The
    /// payload carries content hashes, never bytes; the content store holds those.
    ImportPages {
        after_id: Option<String>,
        pages: Vec<PagePlanEntry>,
    },
    /// Remove one durable page-plan entry (never the final entry).
    DeletePagePlan {
        id: String,
    },
    /// Move an entry after `after_id`, or to the front when it is absent.
    MovePagePlan {
        id: String,
        after_id: Option<String>,
    },
    /// Add one annotation (text markup, ink stroke, or sticky-note marker).
    /// `id` is minted at authoring time and carried here. `page_id` names the
    /// page occurrence (envelopes without it use the page number); with
    /// `user_space` the geometry is in the page's PDF user space.
    AddAnnotation {
        id: String,
        page: usize,
        geom: AnnoGeom,
        color: String,
        author: String,
        #[serde(default)]
        page_id: Option<String>,
        #[serde(default)]
        user_space: bool,
        #[serde(default)]
        created_ms: i64,
    },
    /// Recolour an existing annotation.
    SetAnnotationColor {
        id: String,
        color: String,
    },
    /// Move an annotation's anchor (the draggable sticky-note marker). A no-op
    /// for ink, whose geometry is the polyline rather than a single anchor.
    SetAnnotationPosition {
        id: String,
        x: f64,
        y: f64,
        #[serde(default)]
        user_space: bool,
    },
    /// Delete an annotation. Its anchored comments are removed with it, so a
    /// deleted note never leaves a dangling thread behind.
    DeleteAnnotation {
        id: String,
    },
    /// Create a sticky note: a `Note` annotation plus the empty (or prompted)
    /// comment pinned to it. Both ids are minted at authoring time.
    CreateNote {
        anno_id: String,
        comment_id: String,
        page: usize,
        x: f64,
        y: f64,
        color: String,
        author: String,
        /// Monotonic ordinal for the comment's stable sort, minted at authoring.
        created: i64,
        body: String,
        #[serde(default)]
        page_id: Option<String>,
        #[serde(default)]
        user_space: bool,
    },
    /// Replace a comment's text (the inline editor / palette edit). `rich`,
    /// when given, is the text as written (paragraphs, kinds, marks) and
    /// `body` its plain text; otherwise `body` is the whole text.
    SetCommentBody {
        id: String,
        body: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rich: Vec<tilo_document::ClipBlock>,
    },
    /// Set a comment's resolved flag (the resolve chip toggles it).
    SetCommentResolved {
        id: String,
        resolved: bool,
    },
    /// Append a reply to a comment thread.
    AddReply {
        comment_id: String,
        /// Minted by the author, never by `Action::apply`, so replay and
        /// concurrent replicas retain the exact same reply identity.
        id: String,
        author: String,
        body: String,
        /// The reply as written, when it carries more than plain text.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        rich: Vec<tilo_document::ClipBlock>,
    },
    /// Delete a comment. If it pins a sticky note, the note annotation is
    /// removed too (its cascade drops this comment and any siblings), matching
    /// the on-page rail's delete. Decided deterministically against the doc.
    DeleteComment {
        id: String,
    },
    /// Place a signature using the exact user-authored ink carried by the
    /// envelope, so peers and history replay the same mark.
    AddSignature {
        id: String,
        page: usize,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        signer: String,
        date: String,
        strokes: Option<Vec<Vec<(f64, f64)>>>,
        #[serde(default)]
        page_id: Option<String>,
        #[serde(default)]
        user_space: bool,
    },
    DeleteSignature {
        id: String,
    },
    /// Set a page occurrence's rotation (degrees clockwise, a multiple of 90).
    RotatePage {
        id: String,
        degrees: i32,
    },
    /// Record a value for a form field of the document.
    SetFormValue {
        name: String,
        value: FormValueWire,
    },
    /// Drop a recorded form value, showing the document's own again.
    ClearFormValue {
        name: String,
    },
    /// Pin the content hash of the PDF bytes the markup is made against. The
    /// first pin of a record wins.
    PinSource {
        hash: String,
        len: u64,
        mime: String,
    },
    /// Take over the markup and form values already in the source bytes
    /// `source_hash` as records (see [`crate::adopt`]). A record imports a
    /// given source once; ids derive from the source, so concurrent imports
    /// merge.
    ImportExisting {
        source_hash: String,
        annotations: Vec<ImportedAnnotationWire>,
        comments: Vec<CommentWire>,
        form_values: Vec<(String, FormValueWire)>,
        adopted: Vec<crate::adopt::AdoptedEntry>,
    },
    /// Replace a free text box's text.
    SetFreeText {
        id: String,
        text: String,
    },
    /// Move or resize a free text box and set its text size.
    SetFreeTextBox {
        id: String,
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        size: f64,
        #[serde(default)]
        user_space: bool,
    },
    /// Create the shared page plan for a document that does not have one yet.
    /// Page ids are deterministic, so two peers seeding the same file agree.
    EnsurePagePlan {
        source_page_count: usize,
    },
}

impl PdfViewerAction {
    /// The permission an action needs: replying and commenting need
    /// `Comment`, every other change to the document needs `Write`.
    pub fn required_verb(&self) -> ui::permissions::Verb {
        use ui::permissions::Verb;
        match self {
            PdfViewerAction::AddReply { .. }
            | PdfViewerAction::SetCommentBody { .. }
            | PdfViewerAction::SetCommentResolved { .. } => Verb::Comment,
            _ => Verb::Write,
        }
    }
}

impl Action for PdfViewerAction {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive deterministic reducer keeps every collaborative PDF mutation auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
            PdfViewerAction::EnsurePagePlan { source_page_count } => {
                PdfDoc::ensure_page_plan(doc, *source_page_count);
                PdfDoc::migrate_page_anchors(doc);
            }
            PdfViewerAction::DuplicatePagePlan {
                id,
                new_id,
                source_page,
                imported,
            } => PdfDoc::duplicate_page_plan(
                doc,
                id,
                new_id,
                &PagePlanEntry {
                    id: id.clone(),
                    source_page: *source_page,
                    imported: imported.clone(),
                    rotation: 0,
                },
            ),
            PdfViewerAction::ImportPages { after_id, pages } => {
                PdfDoc::import_pages(doc, after_id.as_deref(), pages);
            }
            PdfViewerAction::DeletePagePlan { id } => {
                PdfDoc::migrate_page_anchors(doc);
                PdfDoc::delete_page_plan(doc, id);
            }
            PdfViewerAction::MovePagePlan { id, after_id } => {
                PdfDoc::move_page_plan(doc, id, after_id.as_deref());
            }
            PdfViewerAction::AddAnnotation {
                id,
                page,
                geom,
                color,
                author,
                page_id,
                user_space,
                created_ms,
            } => {
                let (kind, chars) = geom.to_kind();
                let a = Annotation {
                    id: id.clone(),
                    page: *page,
                    kind,
                    color: Swatch::parse(color),
                    author: author.clone(),
                    chars,
                    created_ms: *created_ms,
                    space: space(*user_space),
                };
                PdfDoc::add_annotation(doc, &a, page_id.as_deref());
            }
            PdfViewerAction::SetAnnotationColor { id, color } => {
                PdfDoc::set_annotation_color(doc, id, Swatch::parse(color));
            }
            PdfViewerAction::SetAnnotationPosition {
                id,
                x,
                y,
                user_space,
            } => {
                PdfDoc::set_annotation_position(doc, id, *x, *y, space(*user_space));
            }
            PdfViewerAction::DeleteAnnotation { id } => {
                PdfDoc::delete_annotation(doc, id);
            }
            PdfViewerAction::CreateNote {
                anno_id,
                comment_id,
                page,
                x,
                y,
                color,
                author,
                created,
                body,
                page_id,
                user_space,
            } => {
                let a = Annotation {
                    id: anno_id.clone(),
                    page: *page,
                    kind: AnnoKind::Note { x: *x, y: *y },
                    color: Swatch::parse(color),
                    author: author.clone(),
                    chars: None,
                    created_ms: 0,
                    space: space(*user_space),
                };
                PdfDoc::add_annotation(doc, &a, page_id.as_deref());
                let c = Comment {
                    id: comment_id.clone(),
                    anno_id: Some(anno_id.clone()),
                    page: *page,
                    author: author.clone(),
                    body: body.clone(),
                    // Deterministic: the ordinal is carried, no wall-clock here.
                    created: *created,
                    resolved: false,
                    replies: Vec::new(),
                    blocks: Vec::new(),
                };
                PdfDoc::add_comment(doc, &c);
            }
            PdfViewerAction::SetCommentBody { id, body, rich } => {
                PdfDoc::set_comment_text(doc, id, &comment_text(body, rich)?);
            }
            PdfViewerAction::SetCommentResolved { id, resolved } => {
                PdfDoc::set_comment_resolved(doc, id, *resolved);
            }
            PdfViewerAction::AddReply {
                comment_id,
                id,
                author,
                body,
                rich,
            } => {
                let clips = comment_text(body, rich)?;
                PdfDoc::add_reply(
                    doc,
                    comment_id,
                    &Reply {
                        id: id.clone(),
                        author: author.clone(),
                        body: body.clone(),
                        blocks: Vec::new(),
                    },
                    &clips,
                );
            }
            PdfViewerAction::DeleteComment { id } => {
                // Mirror the on-page rail: a comment pinning a sticky note takes
                // the note with it (delete_annotation cascades to this comment
                // and any siblings); a free comment just drops. Read from the
                // doc's own state so replay is deterministic.
                if let Some(anno) = PdfDoc::list_comments(doc)
                    .into_iter()
                    .find(|c| &c.id == id)
                    .and_then(|c| c.anno_id)
                {
                    PdfDoc::delete_annotation(doc, &anno);
                }
                PdfDoc::delete_comment(doc, id);
            }
            PdfViewerAction::AddSignature {
                id,
                page,
                x,
                y,
                w,
                h,
                signer,
                date,
                strokes,
                page_id,
                user_space,
            } => PdfDoc::add_signature(
                doc,
                &Signature {
                    id: id.clone(),
                    page: *page,
                    rect: PtRect::new(*x, *y, *w, *h),
                    signer: signer.clone(),
                    date: date.clone(),
                    ink: strokes.as_ref().map_or(SignatureInk::Typed, |strokes| {
                        SignatureInk::Drawn {
                            strokes: strokes.clone(),
                        }
                    }),
                    space: space(*user_space),
                },
                page_id.as_deref(),
            ),
            PdfViewerAction::DeleteSignature { id } => PdfDoc::delete_signature(doc, id),
            PdfViewerAction::RotatePage { id, degrees } => {
                PdfDoc::set_page_rotation(doc, id, *degrees);
            }
            PdfViewerAction::SetFormValue { name, value } => {
                PdfDoc::set_form_value(doc, name, &value.to_entry());
            }
            PdfViewerAction::ClearFormValue { name } => PdfDoc::clear_form_value(doc, name),
            PdfViewerAction::PinSource { hash, len, mime } => {
                PdfDoc::pin_source(doc, hash, *len, mime);
            }
            PdfViewerAction::ImportExisting {
                source_hash,
                annotations,
                comments,
                form_values,
                adopted,
            } => {
                if annotations.len() > crate::adopt::MAX_IMPORTED_ANNOTATIONS
                    || adopted.len() > 4 * crate::adopt::MAX_IMPORTED_ANNOTATIONS
                {
                    return Ok(());
                }
                let plan = crate::adopt::ImportPlan {
                    annotations: annotations
                        .iter()
                        .map(|a| (a.to_annotation(), a.page_id.clone()))
                        .collect(),
                    comments: comments
                        .iter()
                        .map(|c| Comment {
                            id: c.id.clone(),
                            anno_id: c.anno_id.clone(),
                            page: c.page,
                            author: c.author.clone(),
                            body: c.body.clone(),
                            created: c.created,
                            resolved: false,
                            replies: c
                                .replies
                                .iter()
                                .map(|(id, author, body)| Reply {
                                    id: id.clone(),
                                    author: author.clone(),
                                    body: body.clone(),
                                    blocks: Vec::new(),
                                })
                                .collect(),
                            blocks: Vec::new(),
                        })
                        .collect(),
                    form_values: form_values
                        .iter()
                        .map(|(name, value)| (name.clone(), value.to_entry()))
                        .collect(),
                    adopted: adopted.clone(),
                };
                PdfDoc::import_existing(doc, source_hash, &plan);
            }
            PdfViewerAction::SetFreeText { id, text } => PdfDoc::set_free_text(doc, id, text),
            PdfViewerAction::SetFreeTextBox {
                id,
                x,
                y,
                w,
                h,
                size,
                user_space,
            } => PdfDoc::set_free_text_box(
                doc,
                id,
                &PtRect::new(*x, *y, *w, *h),
                *size,
                space(*user_space),
            ),
        }
        Ok(())
    }

    fn locus(&self) -> ExecutionLocus {
        // Personal review markup: the client applies + replicates, peers replay.
        ExecutionLocus::ClientLocal
    }
}

/// Register the reader's action decoder for palette execution + remote replay.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<PdfViewerAction>(PDF_VIEWER_ACTION_TYPE);
}

/// Encode any [`PdfViewerAction`] into the wire form the palette and the on-page
/// affordances both dispatch — the single point that turns the reader's actions
/// into replayable envelopes.
///
/// `PdfViewerAction`'s fields are all plain, finite-checked primitives, so
/// encoding does not fail in practice. If a caller ever manages to pass a
/// non-finite float through, this falls back to an empty payload — the same
/// shape the registry already treats as a malformed, inert action — rather
/// than panicking the palette or an on-page affordance.
pub fn encode(action: &PdfViewerAction) -> EncodedAction {
    EncodedAction::encode(PDF_VIEWER_ACTION_TYPE, action).unwrap_or_else(|_| EncodedAction {
        type_id: PDF_VIEWER_ACTION_TYPE.to_string(),
        payload: Vec::new(),
    })
}

// ===========================================================================
// Action builders — the single source of truth shared by the on-page
// affordances (see `crate::app`) and the Ctrl+K resource tree below.
// ===========================================================================

/// Build an "add annotation" action straight from the model annotation the
/// on-page tools construct, so the tool path and the action path never diverge.
pub fn add_annotation_action(a: &Annotation) -> PdfViewerAction {
    add_annotation_on(a, None)
}

/// An "add annotation" action anchored to page occurrence `page_id`, with
/// the annotation's geometry in whatever space it carries.
pub fn add_annotation_on(a: &Annotation, page_id: Option<&str>) -> PdfViewerAction {
    PdfViewerAction::AddAnnotation {
        id: a.id.clone(),
        page: a.page,
        geom: AnnoGeom::from_kind(&a.kind, a.chars),
        color: a.color.key(),
        author: a.author.clone(),
        page_id: page_id.map(str::to_string),
        user_space: a.space == GeomSpace::User,
        created_ms: a.created_ms,
    }
}

pub fn set_annotation_color_action(id: &str, color: Swatch) -> PdfViewerAction {
    PdfViewerAction::SetAnnotationColor {
        id: id.to_string(),
        color: color.key(),
    }
}

pub fn set_annotation_position_action(id: &str, x: f64, y: f64) -> PdfViewerAction {
    PdfViewerAction::SetAnnotationPosition {
        id: id.to_string(),
        x,
        y,
        user_space: false,
    }
}

/// Move a note to a point in its page's PDF user space.
pub fn set_annotation_position_user_action(id: &str, x: f64, y: f64) -> PdfViewerAction {
    PdfViewerAction::SetAnnotationPosition {
        id: id.to_string(),
        x,
        y,
        user_space: true,
    }
}

pub fn rotate_page_action(id: &str, degrees: i32) -> PdfViewerAction {
    PdfViewerAction::RotatePage {
        id: id.to_string(),
        degrees: degrees.rem_euclid(360),
    }
}

pub fn set_form_value_action(name: &str, value: &FormEntry) -> PdfViewerAction {
    PdfViewerAction::SetFormValue {
        name: name.to_string(),
        value: FormValueWire::from_entry(value),
    }
}

/// Take over the existing markup an import plan found in `source_hash`.
pub fn import_existing_action(
    source_hash: &str,
    plan: &crate::adopt::ImportPlan,
) -> PdfViewerAction {
    PdfViewerAction::ImportExisting {
        source_hash: source_hash.to_string(),
        annotations: plan
            .annotations
            .iter()
            .map(|(a, page_id)| ImportedAnnotationWire {
                id: a.id.clone(),
                page: a.page,
                page_id: page_id.clone(),
                geom: AnnoGeom::from_kind(&a.kind, a.chars),
                color: a.color.key(),
                author: a.author.clone(),
                created_ms: a.created_ms,
            })
            .collect(),
        comments: plan
            .comments
            .iter()
            .map(|c| CommentWire {
                id: c.id.clone(),
                anno_id: c.anno_id.clone(),
                page: c.page,
                author: c.author.clone(),
                body: c.body.clone(),
                created: c.created,
                replies: c
                    .replies
                    .iter()
                    .map(|r| (r.id.clone(), r.author.clone(), r.body.clone()))
                    .collect(),
            })
            .collect(),
        form_values: plan
            .form_values
            .iter()
            .map(|(name, value)| (name.clone(), FormValueWire::from_entry(value)))
            .collect(),
        adopted: plan.adopted.clone(),
    }
}

pub fn set_free_text_action(id: &str, text: &str) -> PdfViewerAction {
    PdfViewerAction::SetFreeText {
        id: id.to_string(),
        text: text.to_string(),
    }
}

/// Move/resize a free text box; `rect` and `size` in its page's user space.
pub fn set_free_text_box_user_action(id: &str, rect: &PtRect, size: f64) -> PdfViewerAction {
    PdfViewerAction::SetFreeTextBox {
        id: id.to_string(),
        x: rect.x,
        y: rect.y,
        w: rect.w,
        h: rect.h,
        size,
        user_space: true,
    }
}

pub fn pin_source_action(hash: &str, len: u64) -> PdfViewerAction {
    PdfViewerAction::PinSource {
        hash: hash.to_string(),
        len,
        mime: "application/pdf".to_string(),
    }
}

pub fn delete_annotation_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeleteAnnotation { id: id.to_string() }
}

/// A sticky note being authored: the ids minted for its marker and its
/// comment thread, its colour and author, the comment's creation ordinal,
/// and its opening text.
pub struct NoteDraft<'a> {
    pub anno_id: &'a str,
    pub comment_id: &'a str,
    pub color: Swatch,
    pub author: &'a str,
    pub created: i64,
    pub body: &'a str,
}

/// A note on `page` at page-point `(x, y)`.
pub fn create_note_action(note: &NoteDraft<'_>, page: usize, x: f64, y: f64) -> PdfViewerAction {
    PdfViewerAction::CreateNote {
        anno_id: note.anno_id.to_string(),
        comment_id: note.comment_id.to_string(),
        page,
        x,
        y,
        color: note.color.key(),
        author: note.author.to_string(),
        created: note.created,
        body: note.body.to_string(),
        page_id: None,
        user_space: false,
    }
}

/// A note anchored to page occurrence `page_id` at a point of its PDF user
/// space.
pub fn create_note_on_action(
    note: &NoteDraft<'_>,
    page: usize,
    page_id: &str,
    at_user: (f64, f64),
) -> PdfViewerAction {
    PdfViewerAction::CreateNote {
        anno_id: note.anno_id.to_string(),
        comment_id: note.comment_id.to_string(),
        page,
        x: at_user.0,
        y: at_user.1,
        color: note.color.key(),
        author: note.author.to_string(),
        created: note.created,
        body: note.body.to_string(),
        page_id: Some(page_id.to_string()),
        user_space: true,
    }
}

pub fn set_comment_body_action(id: &str, body: &str) -> PdfViewerAction {
    PdfViewerAction::SetCommentBody {
        id: id.to_string(),
        body: body.to_string(),
        rich: Vec::new(),
    }
}

/// Replace a comment's text with what was written in the editor: its
/// paragraphs as `clips`, its plain text derived from them.
pub fn set_comment_text_action(id: &str, clips: Vec<tilo_document::ClipBlock>) -> PdfViewerAction {
    PdfViewerAction::SetCommentBody {
        id: id.to_string(),
        body: plain_of(&clips),
        rich: clips,
    }
}

/// Reply to a comment with what was written in the editor.
pub fn add_reply_text_action(
    comment_id: &str,
    author: &str,
    clips: Vec<tilo_document::ClipBlock>,
) -> PdfViewerAction {
    PdfViewerAction::AddReply {
        comment_id: comment_id.to_string(),
        id: Reply::new_id(),
        author: author.to_string(),
        body: plain_of(&clips),
        rich: clips,
    }
}

/// The plain text of paragraphs, a line each.
fn plain_of(clips: &[tilo_document::ClipBlock]) -> String {
    clips
        .iter()
        .map(|clip| clip.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The most text one comment or reply holds.
const MAX_COMMENT_CHARS: usize = 100_000;

/// A comment or reply's text as stored: `rich` when given, checked against
/// its plain text `body`; otherwise `body`, a paragraph per line.
fn comment_text(
    body: &str,
    rich: &[tilo_document::ClipBlock],
) -> Result<Vec<tilo_document::ClipBlock>, ActionError> {
    let refuse = |why: &str| Err(ActionError::Refused(why.to_owned()));
    if body.chars().count() > MAX_COMMENT_CHARS {
        return refuse("the comment is too long");
    }
    if rich.is_empty() {
        return Ok(tilo_rich_text::plain_clips(body));
    }
    if rich.len() > 1_000 || plain_of(rich) != body {
        return refuse("the comment's paragraphs do not match its text");
    }
    let safe_name = |name: &str| {
        !name.is_empty()
            && name.len() <= 128
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    let valid = rich.iter().all(|clip| {
        let len = clip.text.chars().count();
        safe_name(&clip.kind)
            && clip.level.is_none_or(|level| (0..=9).contains(&level))
            && clip.attrs.len() <= 16
            && clip.attrs.keys().all(|key| safe_name(key))
            && clip
                .marks
                .iter()
                .all(|mark| safe_name(&mark.name) && mark.start < mark.end && mark.end <= len)
    });
    if !valid {
        return refuse("the comment's paragraphs are not valid");
    }
    Ok(rich.to_vec())
}

pub fn set_comment_resolved_action(id: &str, resolved: bool) -> PdfViewerAction {
    PdfViewerAction::SetCommentResolved {
        id: id.to_string(),
        resolved,
    }
}

pub fn add_reply_action(comment_id: &str, author: &str, body: &str) -> PdfViewerAction {
    add_reply_action_with_id(comment_id, &Reply::new_id(), author, body)
}

/// Build a reply action with its already-minted durable id.  The explicit
/// variant is used by replay/merge tests and by integrations that author an
/// envelope before it is dispatched.
pub fn add_reply_action_with_id(
    comment_id: &str,
    reply_id: &str,
    author: &str,
    body: &str,
) -> PdfViewerAction {
    PdfViewerAction::AddReply {
        comment_id: comment_id.to_string(),
        id: reply_id.to_string(),
        author: author.to_string(),
        body: body.to_string(),
        rich: Vec::new(),
    }
}

pub fn delete_comment_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeleteComment { id: id.to_string() }
}

pub fn add_signature_action(signature: &Signature) -> PdfViewerAction {
    add_signature_on(signature, None)
}

/// Place a signature on page occurrence `page_id`, its box in whatever space
/// the signature carries.
pub fn add_signature_on(signature: &Signature, page_id: Option<&str>) -> PdfViewerAction {
    PdfViewerAction::AddSignature {
        id: signature.id.clone(),
        page: signature.page,
        x: signature.rect.x,
        y: signature.rect.y,
        w: signature.rect.w,
        h: signature.rect.h,
        signer: signature.signer.clone(),
        date: signature.date.clone(),
        strokes: match &signature.ink {
            SignatureInk::Typed => None,
            SignatureInk::Drawn { strokes } => Some(strokes.clone()),
        },
        page_id: page_id.map(str::to_string),
        user_space: signature.space == GeomSpace::User,
    }
}

pub fn delete_signature_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeleteSignature { id: id.to_string() }
}

pub fn duplicate_page_plan_action(entry: &PagePlanEntry) -> PdfViewerAction {
    PdfViewerAction::DuplicatePagePlan {
        id: entry.id.clone(),
        new_id: format!("page-{}", uuid::Uuid::new_v4()),
        source_page: entry.source_page,
        imported: entry.imported.clone(),
    }
}

pub fn import_pages_action(after_id: Option<&str>, pages: Vec<PagePlanEntry>) -> PdfViewerAction {
    PdfViewerAction::ImportPages {
        after_id: after_id.map(str::to_string),
        pages,
    }
}

pub fn delete_page_plan_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeletePagePlan { id: id.to_string() }
}

pub fn move_page_plan_action(id: &str, after_id: Option<&str>) -> PdfViewerAction {
    PdfViewerAction::MovePagePlan {
        id: id.to_string(),
        after_id: after_id.map(str::to_string),
    }
}

// ===========================================================================
// The resource tree published to Ctrl+K.
// ===========================================================================

/// The presentation context the reader hands [`provide_resources`] so the
/// document-level "Add note" action can be built from the live reading position
/// and author — state that lives in the app, not the document.
#[derive(Debug, Clone)]
pub struct PdfResourceCtx {
    /// Stable identity for the open document's markup record.
    pub doc_id: String,
    pub title: String,
    /// The page a palette-authored note anchors to.
    pub page: usize,
    pub page_count: usize,
    /// Durable CRDT occurrence ids parallel to the current shared page order.
    /// They move with a page, so an old resource address never starts
    /// addressing whichever page later occupies its ordinal.
    pub page_ids: Vec<String>,
    /// Immutable source-page identities parallel to `page_ids`.
    pub page_sources: Vec<usize>,
    /// Imported content descriptors parallel to `page_ids`; absent for pages
    /// from the originally opened PDF.
    pub page_imports: Vec<Option<ImportedPage>>,
    /// Recorded rotation (degrees) parallel to `page_ids`.
    pub page_rotations: Vec<i32>,
    pub author: String,
    /// Date captured when the resource snapshot is published. Action payloads
    /// carry this value so a palette invocation replays identically on peers.
    pub signature_date: String,
    /// Base ordinal for a palette-authored comment's stable sort.
    pub created_seed: i64,
    /// Current bounded find projection. Search itself stays local view state;
    /// each materialized hit is still navigable by agents and history.
    pub search_query: String,
    pub search_whole_word: bool,
    pub search_case_sensitive: bool,
    /// Stable token for the query plus options.  It prevents a stale result
    /// address from being rebound to ordinal `N` in a later find query.
    pub search_token: String,
    pub search_pages: Vec<usize>,
    pub outline: Vec<(String, usize, u8)>,
}

/// Build an immediate action spec, or `None` if `action` cannot be
/// represented as JSON (only reachable via a non-finite float slipping into
/// the geometry) — such an action is silently omitted from the palette
/// rather than panicking the whole resource tree.
fn encode_spec(id: &str, label: &str, action: &PdfViewerAction) -> Option<ActionSpec> {
    let spec = ActionSpec::immediate(id, label, PDF_VIEWER_ACTION_TYPE, action).ok()?;
    Some(with_permission(spec, action))
}

/// Declare the permission an action needs, so the palette offers it only to
/// users who hold it (commenters may comment; editing needs write access).
fn with_permission(spec: ActionSpec, action: &PdfViewerAction) -> ActionSpec {
    match action.required_verb() {
        ui::permissions::Verb::Comment => spec.requiring(PermissionRequirement::Comment),
        _ => spec,
    }
}

fn anno_label(a: &Annotation) -> String {
    let kind = match a.kind {
        AnnoKind::Markup { .. } => "Highlight",
        AnnoKind::Ink { .. } => "Ink",
        AnnoKind::Note { .. } => "Note",
        AnnoKind::FreeText { .. } => "Text",
    };
    format!("{kind} on page {}", a.page)
}

/// One-line comment preview for the palette label.
fn comment_label(c: &Comment) -> String {
    let mut t: String = c.body.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > 48 {
        t = t.chars().take(45).collect::<String>() + "…";
    }
    if t.is_empty() {
        "Comment".to_string()
    } else {
        format!("Comment — {t}")
    }
}

fn reply_label(reply: &Reply) -> String {
    let mut text: String = reply.body.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() > 48 {
        text = text.chars().take(45).collect::<String>() + "…";
    }
    if text.is_empty() {
        format!("Reply — {}", reply.author)
    } else {
        format!("Reply — {text}")
    }
}

fn reply_resource(comment: &Comment, reply: &Reply) -> Resource {
    Resource::new(
        format!("pdf.reply:{}:{}", comment.id, reply.id),
        "pdf.reply",
        reply_label(reply),
    )
    .with_action(operation(
        "open",
        "Go to reply",
        OperationKind::View,
        PdfOperation::GoToPage { page: comment.page },
    ))
}

/// Build a comment resource: edit / reply / resolve / delete, all one source of
/// truth with the rail's own buttons.
fn comment_resource(c: &Comment, author: &str) -> Resource {
    let mut res = Resource::new(
        format!("pdf.comment:{}", c.id),
        "pdf.comment",
        comment_label(c),
    );
    res = res.with_action(operation(
        "open",
        "Go to comment",
        OperationKind::View,
        PdfOperation::GoToPage { page: c.page },
    ));

    let edit_id = c.id.clone();
    res = res.with_action(
        ActionSpec::prompted(
            "edit",
            "Edit comment",
            "Comment",
            "edit the comment…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&set_comment_body_action(&edit_id, text)),
        )
        .requiring(PermissionRequirement::Comment),
    );

    let reply_id = c.id.clone();
    let reply_author = author.to_string();
    res = res.with_action(
        ActionSpec::prompted(
            "reply",
            "Reply",
            "Reply",
            "your reply…",
            ExecutionLocus::ClientLocal,
            move |text: &str| encode(&add_reply_action(&reply_id, &reply_author, text)),
        )
        .requiring(PermissionRequirement::Comment),
    );

    // Resolve toggles against the comment's current state; the target value is
    // baked into the pre-encoded action, so the row reads "Resolve" or "Reopen".
    let toggle_label = if c.resolved { "Reopen" } else { "Resolve" };
    if let Some(spec) = encode_spec(
        "toggle-resolved",
        toggle_label,
        &set_comment_resolved_action(&c.id, !c.resolved),
    ) {
        res = res.with_action(spec);
    }

    if let Some(spec) = encode_spec("delete", "Delete comment", &delete_comment_action(&c.id)) {
        res = res.with_action(spec);
    }
    for reply in c.replies.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        res = res.with_child(reply_resource(c, reply));
    }
    res
}

fn page_resource_id(doc_id: &str, page_id: &str) -> String {
    format!("pdf.page:{doc_id}:{page_id}")
}

fn published_page_resource(
    page: &PagePlanEntry,
    index: usize,
    pages: &[PagePlanEntry],
) -> Resource {
    let page_number = index.saturating_add(1);
    let page_count = pages.len();
    let mut resource = Resource::new(
        format!("pdf.page:{}", page.id),
        "pdf.page",
        format!("Page {page_number}"),
    )
    .with_action(operation(
        "open",
        "Go to page",
        OperationKind::View,
        PdfOperation::GoToPage { page: page_number },
    ));
    if let Some(spec) = encode_spec(
        "duplicate",
        "Duplicate page",
        &duplicate_page_plan_action(page),
    ) {
        resource = resource.with_action(spec.reversible());
    }
    if page_count > 1
        && let Some(spec) = encode_spec("delete", "Delete page", &delete_page_plan_action(&page.id))
    {
        resource = resource.with_action(spec.destructive().reversible());
    }
    if page_number > 1
        && let Some(spec) = encode_spec(
            "move-up",
            "Move page up",
            &move_page_plan_action(
                &page.id,
                page_number
                    .checked_sub(3)
                    .and_then(|index| pages.get(index))
                    .map(|page| page.id.as_str()),
            ),
        )
    {
        resource = resource.with_action(spec.reversible());
    }
    if page_number < page_count {
        let after = pages.get(page_number).map(|page| page.id.as_str());
        if let Some(spec) = encode_spec(
            "move-down",
            "Move page down",
            &move_page_plan_action(&page.id, after),
        ) {
            resource = resource.with_action(spec.reversible());
        }
    }
    for (id, label, turn) in [
        ("rotate-cw", "Rotate page clockwise", 90),
        ("rotate-ccw", "Rotate page counter-clockwise", 270),
    ] {
        if let Some(spec) = encode_spec(
            id,
            label,
            &rotate_page_action(&page.id, page.rotation.saturating_add(turn)),
        ) {
            resource = resource.with_action(spec.reversible());
        }
    }
    resource
}

fn page_resource(ctx: &PdfResourceCtx, page: usize, page_id: &str) -> Resource {
    let source_page = ctx.page_sources.get(page - 1).copied().unwrap_or(page);
    let entry = PagePlanEntry {
        id: page_id.to_string(),
        source_page,
        imported: ctx.page_imports.get(page - 1).cloned().flatten(),
        rotation: 0,
    };
    let mut page_resource = Resource::new(
        page_resource_id(&ctx.doc_id, page_id),
        "pdf.page",
        format!("Page {page}"),
    )
    .with_action(operation(
        "open",
        "Go to page",
        OperationKind::View,
        PdfOperation::GoToPage { page },
    ));
    if let Some(spec) = encode_spec(
        "duplicate",
        "Duplicate page",
        &duplicate_page_plan_action(&entry),
    ) {
        page_resource = page_resource.with_action(spec.reversible());
    }
    if ctx.page_count > 1
        && let Some(spec) = encode_spec("delete", "Delete page", &delete_page_plan_action(page_id))
    {
        page_resource = page_resource.with_action(spec.destructive().reversible());
    }
    if page > 1
        && let Some(spec) = encode_spec(
            "move-up",
            "Move page up",
            &move_page_plan_action(
                page_id,
                page.checked_sub(3)
                    .and_then(|index| ctx.page_ids.get(index))
                    .map(String::as_str),
            ),
        )
    {
        page_resource = page_resource.with_action(spec.reversible());
    }
    if page < ctx.page_count {
        let after = ctx.page_ids.get(page).map(String::as_str);
        if let Some(spec) = encode_spec(
            "move-down",
            "Move page down",
            &move_page_plan_action(page_id, after),
        ) {
            page_resource = page_resource.with_action(spec.reversible());
        }
    }
    let rotation = ctx.page_rotations.get(page - 1).copied().unwrap_or(0);
    for (id, label, turn) in [
        ("rotate-cw", "Rotate page clockwise", 90),
        ("rotate-ccw", "Rotate page counter-clockwise", 270),
    ] {
        if let Some(spec) = encode_spec(id, label, &rotate_page_action(page_id, rotation + turn)) {
            page_resource = page_resource.with_action(spec.reversible());
        }
    }
    page_resource
}

fn outline_resource(ctx: &PdfResourceCtx, index: usize) -> Option<Resource> {
    let (title, page, _) = ctx.outline.get(index)?;
    let id = outline_resource_id(ctx, index)?;
    Some(
        Resource::new(id, "pdf.outline-item", title.as_str()).with_action(operation(
            "open",
            &format!("Go to page {page}"),
            OperationKind::View,
            PdfOperation::GoToPage { page: *page },
        )),
    )
}

fn search_result_resource(ctx: &PdfResourceCtx, index: usize, page: usize) -> Resource {
    Resource::new(
        format!(
            "pdf.search-result:{}:{}:{index}:{page}",
            ctx.doc_id, ctx.search_token
        ),
        "pdf.search-result",
        format!("Match {} on page {page}", index + 1),
    )
    .with_action(operation(
        "open",
        "Go to match",
        OperationKind::View,
        PdfOperation::GoToSearchResult {
            query: ctx.search_query.clone(),
            whole_word: ctx.search_whole_word,
            case_sensitive: ctx.search_case_sensitive,
            index,
            page,
        },
    ))
}

/// Live find projection for the palette — presentation cache, not CRDT.
///
/// Standalone [`provide_resources`] nests this under `pdf.document`. Hosted
/// [`PublishesResources`] returns the same slice alone so agents keep match
/// rows without a second document root beside [`crate::resource::PdfViewer`].
pub fn find_result_resources(ctx: &PdfResourceCtx) -> Vec<Resource> {
    if ctx.search_query.trim().is_empty() {
        return Vec::new();
    }
    let mut search = Resource::new(
        format!("pdf.search:{}", ctx.doc_id),
        "pdf.search",
        format!("Find results for “{}”", ctx.search_query),
    );
    for (index, page) in ctx
        .search_pages
        .iter()
        .copied()
        .take(SEARCH_RESULT_RESOURCE_LIMIT)
        .enumerate()
    {
        search = search.with_child(search_result_resource(ctx, index, page));
    }
    vec![search]
}

/// A compact page of contextual children, centred on the current page. The
/// cursor is the current durable page-plan id rather than an ordinal, so a
/// page remains addressable after a shared reorder.
fn contextual_pages(ctx: &PdfResourceCtx) -> impl Iterator<Item = (usize, &str)> {
    let count = ctx.page_ids.len().min(ctx.page_count);
    let start = ctx.page.saturating_sub(1).min(count.saturating_sub(1));
    ctx.page_ids
        .iter()
        .enumerate()
        .skip(start)
        .take(CONTEXTUAL_CHILD_LIMIT)
        .map(|(index, id)| (index + 1, id.as_str()))
}

/// Build the resource tree the reader currently has open: the open document, its
/// annotations (each owning its pinned comments), and any free comments — every
/// document-mutating operation exported as an action.
#[allow(
    clippy::too_many_lines,
    reason = "the resource tree is deliberately assembled in one linear document hierarchy"
)]
/// Stable document root only; pages, outline entries and annotations are
/// retained/paged separately and are never materialized for root browsing.
/// Nested markup catalogue for [`crate::resource::PdfViewer::children`].
pub(crate) fn markup_children(state: &crate::resource::PdfMarkup) -> Vec<Resource> {
    let mut children = Vec::new();
    for (index, page) in state.pages.iter().enumerate().take(64) {
        children.push(published_page_resource(page, index, &state.pages));
    }
    for anno in state.annotations.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let mut res = Resource::new(
            format!("pdf.annotation:{}", anno.id),
            "pdf.annotation",
            anno_label(anno),
        );
        res = res.with_action(operation(
            "open",
            "Go to annotation",
            OperationKind::View,
            PdfOperation::GoToPage { page: anno.page },
        ));
        let aid = anno.id.clone();
        res = res.with_action(ActionSpec::prompted(
            "change-color",
            "Change colour",
            "Colour",
            "yellow, green, pink, blue, purple",
            ExecutionLocus::ClientLocal,
            move |name: &str| {
                encode(&set_annotation_color_action(
                    &aid,
                    Swatch::parse(name.trim()),
                ))
            },
        ));
        if let Some(spec) = encode_spec(
            "delete",
            "Delete annotation",
            &delete_annotation_action(&anno.id),
        ) {
            res = res.with_action(spec);
        }
        for comment in state
            .comments
            .iter()
            .filter(|comment| comment.anno_id.as_deref() == Some(anno.id.as_str()))
        {
            res = res.with_child(comment_resource(comment, &comment.author));
        }
        children.push(res);
    }
    for comment in state
        .comments
        .iter()
        .filter(|comment| comment.anno_id.is_none())
        .take(CONTEXTUAL_CHILD_LIMIT)
    {
        children.push(comment_resource(comment, &comment.author));
    }
    for signature in state.signatures.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let mut resource = Resource::new(
            format!("pdf.signature:{}", signature.id),
            "pdf.signature",
            format!(
                "Signature by {} on page {}",
                signature.signer, signature.page
            ),
        );
        if let Some(spec) = encode_spec(
            "delete",
            "Delete signature",
            &delete_signature_action(&signature.id),
        ) {
            resource = resource.with_action(spec);
        }
        resource = resource.with_action(operation(
            "open",
            "Go to signature",
            OperationKind::View,
            PdfOperation::GoToPage {
                page: signature.page,
            },
        ));
        children.push(resource);
    }
    children
}

/// Root View/Effect operations for [`crate::resource::PdfViewer::local_operations`].
pub(crate) fn document_local_operations() -> Vec<ActionSpec> {
    let ctx = PdfResourceCtx {
        doc_id: "default".into(),
        title: "PDF".into(),
        page: 0,
        page_count: 0,
        page_ids: Vec::new(),
        page_sources: Vec::new(),
        page_imports: Vec::new(),
        page_rotations: Vec::new(),
        author: String::new(),
        signature_date: String::new(),
        created_seed: 0,
        search_query: String::new(),
        search_whole_word: false,
        search_case_sensitive: false,
        search_token: String::new(),
        search_pages: Vec::new(),
        outline: Vec::new(),
    };
    document_root(&ctx)
        .actions
        .into_iter()
        .filter(|spec| spec.kind != OperationKind::Document)
        .collect()
}

pub fn document_root(ctx: &PdfResourceCtx) -> Resource {
    let mut root = Resource::new(
        format!("pdf.document:{}", ctx.doc_id),
        "pdf.document",
        ctx.title.clone(),
    );

    for (id, label, kind, value) in [
        (
            "previous-page",
            "Previous page",
            OperationKind::View,
            PdfOperation::PreviousPage,
        ),
        (
            "next-page",
            "Next page",
            OperationKind::View,
            PdfOperation::NextPage,
        ),
        (
            "zoom-in",
            "Zoom in",
            OperationKind::View,
            PdfOperation::ZoomIn,
        ),
        (
            "zoom-out",
            "Zoom out",
            OperationKind::View,
            PdfOperation::ZoomOut,
        ),
        (
            "fit-page",
            "Fit page",
            OperationKind::View,
            PdfOperation::FitPage,
        ),
        (
            "fit-width",
            "Fit width",
            OperationKind::View,
            PdfOperation::FitWidth,
        ),
        (
            "toggle-sidebar",
            "Toggle sidebar",
            OperationKind::View,
            PdfOperation::ToggleSidebar,
        ),
        (
            "toggle-pages",
            "Toggle page thumbnails",
            OperationKind::View,
            PdfOperation::ToggleRail,
        ),
        (
            "find",
            "Find in document",
            OperationKind::View,
            PdfOperation::OpenSearch,
        ),
        (
            "mark-highlight",
            "Mark text as highlight",
            OperationKind::View,
            PdfOperation::MarkupStyle {
                style: "highlight".into(),
            },
        ),
        (
            "mark-underline",
            "Mark text as underline",
            OperationKind::View,
            PdfOperation::MarkupStyle {
                style: "underline".into(),
            },
        ),
        (
            "mark-strikeout",
            "Mark text as strike-through",
            OperationKind::View,
            PdfOperation::MarkupStyle {
                style: "strikeout".into(),
            },
        ),
    ] {
        let mut spec = operation(id, label, kind, value.clone());
        if value == PdfOperation::PreviousPage && ctx.page <= 1 {
            spec = spec.unavailable("first_page", "Already at the first page");
        }
        if value == PdfOperation::NextPage && ctx.page >= ctx.page_count {
            spec = spec.unavailable("last_page", "Already at the last page");
        }
        root = root.with_action(spec);
    }
    root = root.with_action(file_operation("open", "Open PDF", false));
    root = root.with_action(file_operation("save-as", "Save annotated PDF", true));

    root
}

#[allow(
    clippy::too_many_lines,
    reason = "one resource projection keeps the document, page, annotation, comment, signature, outline, and search subtrees in one auditable traversal"
)]
pub fn provide_resources(ctx: &PdfResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
    let state =
        <crate::resource::PdfViewer as ui::tilo_resource::Resource>::read(doc).unwrap_or_default();
    let mut root = ui::tilo_resource::publish::<crate::resource::PdfViewer>(
        &format!("pdf.document:{}", ctx.doc_id),
        &ctx.title,
        &state,
    );
    // Published page rows already carry open, duplicate, delete, move, and
    // rotate. Keep the window around the current page. A live page the
    // document has not published yet is added beside them.
    let window: Vec<(usize, &str)> = contextual_pages(ctx).collect();
    let window_ids: std::collections::HashSet<&str> =
        window.iter().map(|(_, page_id)| *page_id).collect();
    root.children.retain(|child| {
        if child.kind != "pdf.page" {
            return true;
        }
        let Some(page_id) = child.id.as_str().strip_prefix("pdf.page:") else {
            return false;
        };
        let bare = page_id.rsplit(':').next().unwrap_or(page_id);
        window_ids.contains(bare)
    });
    let published_page_ids: std::collections::HashSet<String> = root
        .children
        .iter()
        .filter(|child| child.kind == "pdf.page")
        .filter_map(|child| {
            child
                .id
                .as_str()
                .strip_prefix("pdf.page:")
                .map(|page_id| page_id.rsplit(':').next().unwrap_or(page_id).to_string())
        })
        .collect();
    for action in &mut root.actions {
        if action.id == "previous-page" && ctx.page <= 1 {
            let current = action.clone();
            *action = current.unavailable("first_page", "Already at the first page");
        }
        if action.id == "next-page" && ctx.page >= ctx.page_count {
            let current = action.clone();
            *action = current.unavailable("last_page", "Already at the last page");
        }
    }
    for (page, page_id) in window {
        if published_page_ids.contains(page_id) {
            continue;
        }
        root = root.with_child(page_resource(ctx, page, page_id));
    }

    for index in 0..ctx.outline.len().min(CONTEXTUAL_CHILD_LIMIT) {
        if let Some(resource) = outline_resource(ctx, index) {
            root = root.with_child(resource);
        }
    }
    for search in find_result_resources(ctx) {
        root = root.with_child(search);
    }

    // Document-level: add a sticky note carrying the prompted text at the
    // current page's top margin. The ids are minted once when the argument is
    // committed, then baked into the envelope, so replay stays deterministic.
    let page = ctx.page;
    let author = ctx.author.clone();
    let created = ctx.created_seed + 1;
    root = root.with_action(ActionSpec::prompted(
        "add-note",
        "Add note",
        "Note",
        "your note…",
        ExecutionLocus::ClientLocal,
        move |text: &str| {
            encode(&create_note_action(
                &NoteDraft {
                    anno_id: &Annotation::new_id(),
                    comment_id: &Comment::new_id(),
                    color: Swatch::Yellow,
                    author: &author,
                    created,
                    body: text,
                },
                page,
                MARGIN,
                MARGIN,
            ))
        },
    ));

    // Agents and keyboard users do not have a pointer location to drop a
    // signature onto. Offer the canonical typed equivalent at a predictable,
    // readable location on the current page; the timestamp is captured in the
    // resource snapshot, then carried in the replayable action payload.
    let signature_page = ctx.page;
    let signature_date = ctx.signature_date.clone();
    let fallback_signer = ctx.author.clone();
    root = root.with_action(ActionSpec::prompted(
        "add-typed-signature",
        "Add typed signature",
        "Signer",
        "signer name (leave blank to use the current author)…",
        ExecutionLocus::ClientLocal,
        move |text: &str| {
            let signer = if text.trim().is_empty() {
                fallback_signer.clone()
            } else {
                text.trim().to_string()
            };
            encode(&add_signature_action(&Signature {
                id: Signature::new_id(),
                page: signature_page,
                // Keep the automatic, keyboard/palette-created signature out
                // of the opening paragraph while retaining a familiar signing
                // location on the page.
                rect: PtRect::new(PAGE_W - MARGIN - 218.0, PAGE_H - MARGIN - 74.0, 218.0, 74.0),
                signer,
                date: signature_date.clone(),
                ink: SignatureInk::Typed,
                space: crate::model::GeomSpace::Page,
            }))
        },
    ));

    vec![root]
}

/// Resolve an omitted resource from current state without rebuilding a large
/// contextual tree. Every branch checks the stable id against the live model;
/// a deleted annotation, moved-away local page, or stale search hit therefore
/// fails closed instead of being rebound by list position.
#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive stable-id resolver makes stale resource refusal auditable"
)]
fn find_pdf(resources: &[Resource], id: &str) -> Option<Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_pdf(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

#[allow(
    clippy::too_many_lines,
    reason = "PDF resource resolution still mirrors the standalone widget surface"
)]
pub fn resolve_resource(ctx: &PdfResourceCtx, doc: &AutoCommit, id: &str) -> Option<Resource> {
    let root_id = format!("pdf.document:{}", ctx.doc_id);
    if id == root_id {
        return provide_resources(ctx, doc).into_iter().next();
    }
    let page_prefix = format!("pdf.page:{}:", ctx.doc_id);
    if let Some(page_id) = id.strip_prefix(&page_prefix) {
        let index = ctx
            .page_ids
            .iter()
            .position(|candidate| candidate == page_id)?;
        let contextual = page_resource(ctx, index + 1, page_id);
        let Some(mut published) = provide_resources(ctx, doc)
            .into_iter()
            .next()
            .and_then(|root| {
                root.children.into_iter().find(|child| {
                    child.kind == "pdf.page"
                        && child
                            .id
                            .as_str()
                            .strip_prefix("pdf.page:")
                            .is_some_and(|bare| bare.rsplit(':').next().unwrap_or(bare) == page_id)
                })
            })
        else {
            return Some(contextual);
        };
        // Keep the address the caller used. Document actions stay the
        // published page's; views the published page lacks are added.
        published.id = contextual.id;
        published.label = contextual.label;
        for action in contextual.actions {
            if !published.actions.iter().any(|have| have.id == action.id) {
                published.actions.push(action);
            }
        }
        return Some(published);
    }
    if (id.starts_with("pdf.annotation:") || id.starts_with("pdf.comment:"))
        && let Some(found) = find_pdf(&provide_resources(ctx, doc), id)
    {
        return Some(found);
    }
    if let Some(raw) = id.strip_prefix("pdf.annotation:") {
        return PdfDoc::list_annotations(doc)
            .into_iter()
            .find(|annotation| annotation.id == raw)
            .map(|annotation| {
                let mut resource = Resource::new(
                    format!("pdf.annotation:{}", annotation.id),
                    "pdf.annotation",
                    anno_label(&annotation),
                );
                resource = resource.with_action(operation(
                    "open",
                    "Go to annotation",
                    OperationKind::View,
                    PdfOperation::GoToPage {
                        page: annotation.page,
                    },
                ));
                let aid = annotation.id.clone();
                resource = resource.with_action(ActionSpec::prompted(
                    "change-color",
                    "Change colour",
                    "Colour",
                    "yellow, green, pink, blue, purple",
                    ExecutionLocus::ClientLocal,
                    move |name: &str| {
                        encode(&set_annotation_color_action(
                            &aid,
                            Swatch::parse(name.trim()),
                        ))
                    },
                ));
                if let Some(spec) = encode_spec(
                    "delete",
                    "Delete annotation",
                    &delete_annotation_action(&annotation.id),
                ) {
                    resource = resource.with_action(spec);
                }
                resource
            });
    }
    if let Some(raw) = id.strip_prefix("pdf.comment:") {
        return PdfDoc::list_comments(doc)
            .into_iter()
            .find(|comment| comment.id == raw)
            .map(|comment| comment_resource(&comment, &ctx.author));
    }
    if let Some(raw) = id.strip_prefix("pdf.reply:") {
        if let Some(found) = find_pdf(&provide_resources(ctx, doc), id) {
            return Some(found);
        }
        let (comment_id, reply_id) = raw.split_once(':')?;
        return PdfDoc::list_comments(doc)
            .into_iter()
            .find(|comment| comment.id == comment_id)
            .and_then(|comment| {
                comment
                    .replies
                    .iter()
                    .find(|reply| reply.id == reply_id)
                    .map(|reply| reply_resource(&comment, reply))
            });
    }
    if let Some(raw) = id.strip_prefix("pdf.signature:") {
        if let Some(found) = find_pdf(&provide_resources(ctx, doc), id) {
            return Some(found);
        }
        return PdfDoc::list_signatures(doc)
            .into_iter()
            .find(|signature| signature.id == raw)
            .map(|signature| {
                let mut resource = Resource::new(
                    format!("pdf.signature:{}", signature.id),
                    "pdf.signature",
                    format!(
                        "Signature by {} on page {}",
                        signature.signer, signature.page
                    ),
                );
                if let Some(spec) = encode_spec(
                    "delete",
                    "Delete signature",
                    &delete_signature_action(&signature.id),
                ) {
                    resource = resource.with_action(spec);
                }
                resource.with_action(operation(
                    "open",
                    "Go to signature",
                    OperationKind::View,
                    PdfOperation::GoToPage {
                        page: signature.page,
                    },
                ))
            });
    }
    let outline_prefix = format!("pdf.outline:{}:", ctx.doc_id);
    if id.strip_prefix(&outline_prefix).is_some() {
        if let Some(found) = find_pdf(&provide_resources(ctx, doc), id) {
            return Some(found);
        }
        return ctx.outline.iter().enumerate().find_map(|(index, _)| {
            (outline_resource_id(ctx, index).as_deref() == Some(id))
                .then(|| outline_resource(ctx, index))
                .flatten()
        });
    }
    let search_prefix = format!("pdf.search-result:{}:", ctx.doc_id);
    if let Some(raw) = id.strip_prefix(&search_prefix) {
        if let Some(found) = find_pdf(&provide_resources(ctx, doc), id) {
            return Some(found);
        }
        let mut parts = raw.split(':');
        let token = parts.next()?;
        let index = parts.next()?;
        let page = parts.next()?;
        if parts.next().is_some() || token != ctx.search_token {
            return None;
        }
        let index = index.parse::<usize>().ok()?;
        let page = page.parse::<usize>().ok()?;
        return ctx
            .search_pages
            .get(index)
            .filter(|current| **current == page)
            .map(|_| search_result_resource(ctx, index, page));
    }
    None
}

// Inherent resource publication (was ResourceProvider)
impl crate::app::PdfApp {
    pub fn publish_resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.pdf_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
            None => Vec::new(),
        }
    }

    pub fn publish_resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.pdf_resource_ctx()
            .and_then(|ctx| resolve_resource(&ctx, doc, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PdfApp;
    use std::collections::BTreeSet;
    use std::io::Read;
    use std::sync::Mutex;
    use tilo_actions::{ActionInput, ActionRegistry, LocalTransport};
    use tilo_content::{
        ContentBackend, ContentError, ContentReader, ContentState, ContentStore, MemoryBackend,
        Stored,
    };
    use ui::widget::Widget;

    /// Peer B's content backend: its own local content, plus a peer (A) it
    /// fetches from. A miss is not served synchronously; it asks the peer,
    /// reports the fetch as pending, and installs the bytes (verified against
    /// the hash that named them) for a later read. The two peers share nothing.
    struct PeerFetchingBackend {
        local: MemoryBackend,
        peer: ContentStore,
        requested: Mutex<BTreeSet<String>>,
    }

    impl PeerFetchingBackend {
        fn new(peer: ContentStore) -> Self {
            Self {
                local: MemoryBackend::default(),
                peer,
                requested: Mutex::new(BTreeSet::new()),
            }
        }

        /// Ask the peer for `hash`; the answer lands locally on the next
        /// `state` query, like a transfer completing between frames.
        fn want(&self, hash: &str) {
            if let Ok(mut requested) = self.requested.lock() {
                requested.insert(hash.to_string());
            }
        }

        fn settle(&self, hash: &str) {
            let wanted = self
                .requested
                .lock()
                .is_ok_and(|mut requested| requested.remove(hash));
            if wanted && let Some(mut reader) = self.peer.open(hash) {
                let _ = self.local.put(&mut reader, "");
            }
        }
    }

    impl ContentBackend for PeerFetchingBackend {
        fn put(&self, reader: &mut dyn Read, mime: &str) -> Result<Stored, ContentError> {
            self.local.put(reader, mime)
        }

        fn open(&self, hash: &str) -> Result<Option<ContentReader>, ContentError> {
            let found = self.local.open(hash)?;
            if found.is_none() {
                self.want(hash);
            }
            Ok(found)
        }

        fn has(&self, hash: &str) -> bool {
            self.local.has(hash)
        }

        fn state(&self, hash: &str) -> ContentState {
            self.settle(hash);
            if self.local.has(hash) {
                ContentState::Resolved
            } else if self
                .requested
                .lock()
                .is_ok_and(|requested| requested.contains(hash))
            {
                ContentState::Pending
            } else {
                ContentState::Unavailable
            }
        }
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        register(&mut r);
        r
    }

    fn prompted_encode(spec: &ActionSpec, arg: &str) -> EncodedAction {
        match &spec.input {
            ActionInput::Prompted { build, .. } => build(arg),
            ActionInput::Immediate(_) | ActionInput::Deferred { .. } => {
                panic!("expected a prompted action: {}", spec.id)
            }
        }
    }

    /// The sample reader, synced once so its demonstration markup (two
    /// highlights, a sticky note with a two-reply thread, a highlight comment,
    /// and a free resolved comment) is in the shared document.
    fn open_with_markup() -> (PdfApp, ui::State) {
        let mut app = PdfApp::sample();
        let mut state = ui::State::new();
        Widget::sync(&mut app, &mut state.doc, &state.root);
        (app, state)
    }

    thread_local! {
        /// The sample reader a contract seed was synced from; its page plan
        /// and resource context belong to that very document.
        static SEEDED: std::cell::RefCell<Option<PdfApp>> = const { std::cell::RefCell::new(None) };
    }

    fn markup_seed() -> AutoCommit {
        let (app, state) = open_with_markup();
        SEEDED.set(Some(app));
        state.doc
    }

    fn markup_resources(doc: &AutoCommit) -> Vec<Resource> {
        SEEDED.with_borrow(|app| {
            app.as_ref()
                .expect("the contract seed ran first")
                .publish_resources(doc)
        })
    }

    fn resource_key<'a>(res: &'a Resource, prefix: &str) -> &'a str {
        res.id
            .as_str()
            .strip_prefix(prefix)
            .expect("a keyed resource")
    }

    ui::action_contract_tests! {
        seed: markup_seed(),
        registry: registry(),
        resources: |doc| markup_resources(doc),
        palette: ["Add note", "Delete annotation", "Delete comment"],
        replay: [
            add_ink_annotation => |_| encode(&add_annotation_action(&Annotation {
                id: Annotation::new_id(),
                page: 2,
                kind: AnnoKind::ink(vec![(1.0, 2.0), (3.5, 4.25), (10.0, 0.0)]),
                color: Swatch::Blue,
                author: "You".into(),
                chars: None,
                created_ms: 0,
                space: crate::model::GeomSpace::Page,
            })),
        ],
        undo: [
            add_highlight => |_| encode(&add_annotation_action(&Annotation {
                id: Annotation::new_id(),
                page: 1,
                kind: AnnoKind::highlight(PtRect::new(70.0, 300.0, 120.0, 12.0)),
                color: Swatch::Green,
                author: "You".into(),
                chars: None,
                created_ms: 0,
                space: crate::model::GeomSpace::Page,
            })),
        ],
        encodes: [
            delete_annotation => ("pdf.annotation", "delete", |_, res| {
                encode(&delete_annotation_action(resource_key(res, "pdf.annotation:")))
            }),
            edit_comment => ("pdf.comment", "edit", arg: "revised", |_, res| {
                encode(&set_comment_body_action(resource_key(res, "pdf.comment:"), "revised"))
            }),
        ],
    }

    fn resource_ctx(
        title: &str,
        page_ids: Vec<String>,
        outline: Vec<(String, usize, u8)>,
    ) -> PdfResourceCtx {
        let page_count = page_ids.len();
        PdfResourceCtx {
            doc_id: "fixture-document".into(),
            title: title.into(),
            page: 1,
            page_count,
            page_ids,
            page_sources: (1..=page_count).collect(),
            page_imports: vec![None; page_count],
            page_rotations: Vec::new(),
            author: "Ada".into(),
            signature_date: "2026-09-06".into(),
            created_seed: 0,
            search_query: String::new(),
            search_whole_word: false,
            search_case_sensitive: false,
            search_token: search_result_token("", false, false),
            search_pages: Vec::new(),
            outline,
        }
    }

    fn dispatch_encoded(doc: &mut AutoCommit, encoded: EncodedAction) {
        let decoded = registry().decode(&encoded).expect("decodable action");
        let mut transport = LocalTransport::new();
        tilo_actions::dispatch(doc, decoded.as_ref(), encoded, &mut transport)
            .expect("action applies");
    }

    /// Ctrl+K on the reader yields the full nested tree: the open document →
    /// annotation → comment, plus free-comment resources, each with the actions
    /// the data model implies. Drives the real resource hook. Large documents
    /// stay bounded with lazily resolvable omitted children, and outline ids
    /// survive reordering but fail closed when changed.
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    #[allow(
        clippy::similar_names,
        reason = "merged phases keep their original state/stale binding names"
    )]
    fn document_resources_publish_the_nested_tree_with_bounded_stable_children() {
        let (app, state) = open_with_markup();
        let resources = ui::resources::resources(&app, &state.doc);
        assert_eq!(resources.len(), 1, "one open document");
        let document = &resources[0];
        assert_eq!(document.kind, "pdf.document");
        let doc_actions: Vec<&str> = document.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(doc_actions.contains(&"add-note"), "{doc_actions:?}");
        assert!(
            doc_actions.contains(&"add-typed-signature"),
            "{doc_actions:?}"
        );
        let page = document
            .children
            .iter()
            .find(|r| r.kind == "pdf.page")
            .expect("page resource");
        assert!(page.actions.iter().any(|action| action.id == "duplicate"));
        assert!(page.actions.iter().any(|action| action.id == "delete"));
        let duplicate = page
            .actions
            .iter()
            .find(|action| action.id == "duplicate")
            .expect("duplicate page action");
        assert_eq!(duplicate.kind, OperationKind::Document);
        assert!(duplicate.contract.reversible);
        assert!(
            registry()
                .decode(&ui::testing::action_contract::spec_encoding(
                    duplicate, None
                ))
                .is_ok(),
            "page-plan actions use the same registry as all other shared PDF mutations"
        );
        let outline = document
            .children
            .iter()
            .find(|r| r.kind == "pdf.outline-item")
            .expect("an outline item on the published document");
        let resolved = app
            .publish_resource(&state.doc, outline.id.as_str())
            .expect("an eager outline item resolves");
        assert_eq!(
            resolved
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            outline
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>(),
            "an eager outline item is the published row"
        );
        assert!(document.children.iter().any(|r| r.kind == "pdf.signature"));

        // An annotation resource owning at least one nested comment.
        let anno = document
            .children
            .iter()
            .find(|r| r.kind == "pdf.annotation" && !r.children.is_empty())
            .expect("an annotation resource with a pinned comment");
        let anno_actions: Vec<&str> = anno.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(anno_actions.contains(&"change-color"), "{anno_actions:?}");
        assert!(anno_actions.contains(&"delete"), "{anno_actions:?}");
        let comment = &anno.children[0];
        assert_eq!(comment.kind, "pdf.comment");
        let cm_actions: Vec<&str> = comment.actions.iter().map(|a| a.id.as_str()).collect();
        assert!(cm_actions.contains(&"edit"), "{cm_actions:?}");
        assert!(cm_actions.contains(&"reply"), "{cm_actions:?}");
        assert!(cm_actions.contains(&"toggle-resolved"), "{cm_actions:?}");
        assert!(cm_actions.contains(&"delete"), "{cm_actions:?}");
        let comment_with_reply = document
            .children
            .iter()
            .flat_map(|parent| parent.children.iter())
            .find(|resource| resource.kind == "pdf.comment" && !resource.children.is_empty())
            .expect("a seeded reply belongs to a canonical comment resource");
        let reply = comment_with_reply
            .children
            .first()
            .expect("durable reply is nested under its canonical comment");
        assert_eq!(reply.kind, "pdf.reply");
        assert!(reply.actions.iter().any(|action| action.id == "open"));
        assert!(
            resolve_resource(
                &app.pdf_resource_ctx().unwrap(),
                &state.doc,
                reply.id.as_str(),
            )
            .is_some(),
            "a reply remains lazily addressable for agents and history"
        );

        // The seeded free comment (pinned to no annotation) is a document child.
        assert!(
            document.children.iter().any(|r| r.kind == "pdf.comment"),
            "a free-comment resource hangs off the document"
        );

        // A large document's snapshot is bounded and omitted pages resolve lazily.
        let page_count = CONTEXTUAL_CHILD_LIMIT + 9;
        let ctx = resource_ctx(
            "Large fixture",
            (1..=page_count)
                .map(|page| format!("page-{page}"))
                .collect(),
            Vec::new(),
        );
        let doc = AutoCommit::new();
        let snapshot = provide_resources(&ctx, &doc);
        let eager_pages = snapshot[0]
            .children
            .iter()
            .filter(|resource| resource.kind == "pdf.page")
            .count();
        assert_eq!(eager_pages, CONTEXTUAL_CHILD_LIMIT);
        let omitted = page_resource_id(&ctx.doc_id, &format!("page-{page_count}"));
        let resolved = resolve_resource(&ctx, &doc, &omitted).expect("lazy page resource");
        assert_eq!(resolved.id.as_str(), omitted);
        assert_eq!(resolved.label, format!("Page {page_count}"));
        let mut find = ctx.clone();
        find.search_query = "fixture".into();
        find.search_token = search_result_token("fixture", false, false);
        find.search_pages = (1..=SEARCH_RESULT_RESOURCE_LIMIT + 9).collect();
        let find_snapshot = provide_resources(&find, &doc);
        let find_resource = find_snapshot[0]
            .children
            .iter()
            .find(|resource| resource.kind == "pdf.search")
            .expect("non-empty find query has a result resource");
        assert_eq!(find_resource.children.len(), SEARCH_RESULT_RESOURCE_LIMIT);
        let omitted_find = format!(
            "pdf.search-result:{}:{}:{}:{}",
            find.doc_id,
            find.search_token,
            SEARCH_RESULT_RESOURCE_LIMIT,
            SEARCH_RESULT_RESOURCE_LIMIT + 1
        );
        assert!(
            resolve_resource(&find, &doc, &omitted_find).is_some(),
            "an omitted find hit remains addressable by its stable query/result id"
        );
        let mut stale = ctx;
        stale.page_ids.pop();
        stale.page_count -= 1;
        assert!(resolve_resource(&stale, &doc, &omitted).is_none());

        // Outline resource ids survive reordering but fail closed when changed.
        let mut ctx = resource_ctx(
            "Outline fixture",
            vec!["p-1".into(), "p-2".into(), "p-3".into()],
            vec![
                ("Introduction".into(), 1, 0),
                ("Security model".into(), 2, 0),
                ("Security model".into(), 2, 0),
            ],
        );
        let introduction = outline_resource_id(&ctx, 0).expect("valid outline entry");
        let first_security = outline_resource_id(&ctx, 1).expect("valid outline entry");
        let duplicate_security = outline_resource_id(&ctx, 2).expect("valid duplicate entry");
        assert_ne!(
            first_security, duplicate_security,
            "duplicates retain identities"
        );
        for id in [&introduction, &first_security, &duplicate_security] {
            assert!(
                resolve_resource(&ctx, &doc, id).is_some(),
                "outline id {id}"
            );
        }
        ctx.outline.insert(0, ("Preface".into(), 1, 0));
        assert!(
            resolve_resource(&ctx, &doc, &first_security).is_some(),
            "inserting another bookmark does not rebind the saved action"
        );
        assert!(resolve_resource(&ctx, &doc, &introduction).is_some());
        assert!(resolve_resource(&ctx, &doc, &duplicate_security).is_some());
        ctx.outline[1].1 = 3;
        assert!(
            resolve_resource(&ctx, &doc, &introduction).is_none(),
            "a changed bookmark fails closed rather than opening another section"
        );
    }

    /// Signature and annotation actions apply through the shared registry;
    /// deleting an annotation removes every comment pinned to it; and the
    /// affordance path (`PdfApp::perform_action`) stashes an undo boundary the
    /// host's recorder attributes to the local user, so their author-scoped
    /// Ctrl+Z reverts it.
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    fn markup_actions_apply_through_the_registry_and_undo_as_the_local_author() {
        // Drawn signatures round-trip through add and delete actions.
        let mut doc = AutoCommit::new();
        let signature = Signature {
            id: "sig-action".into(),
            page: 2,
            rect: PtRect::new(10.0, 20.0, 180.0, 56.0),
            signer: "Ada".into(),
            date: "2026-09-05".into(),
            ink: SignatureInk::Drawn {
                strokes: vec![vec![(0.0, 0.5), (1.0, 0.5)]],
            },
            space: crate::model::GeomSpace::Page,
        };
        dispatch_encoded(&mut doc, encode(&add_signature_action(&signature)));
        assert_eq!(PdfDoc::list_signatures(&doc)[0].ink, signature.ink);
        dispatch_encoded(&mut doc, encode(&delete_signature_action(&signature.id)));
        assert!(PdfDoc::list_signatures(&doc).is_empty());

        // The resource's typed signature is replayable and uses the prompted signer.
        let (app, mut state) = open_with_markup();
        let expected_page = app.pdf_resource_ctx().expect("open document").page;
        let resources = ui::resources::resources(&app, &state.doc);
        let action = resources[0]
            .actions
            .iter()
            .find(|action| action.id == "add-typed-signature")
            .expect("typed signature action");
        dispatch_encoded(&mut state.doc, prompted_encode(action, "Grace Hopper"));
        let signature = PdfDoc::list_signatures(&state.doc)
            .into_iter()
            .find(|signature| signature.signer == "Grace Hopper")
            .expect("prompted signer is saved");
        assert_eq!(signature.ink, SignatureInk::Typed);
        assert_eq!(signature.page, expected_page);

        // Deleting a seeded annotation that carries a pinned comment.
        let (_app, mut state) = open_with_markup();
        let doc = &mut state.doc;
        let annos = PdfDoc::list_annotations(doc);
        let comments = PdfDoc::list_comments(doc);
        let target = annos
            .iter()
            .find(|a| {
                comments
                    .iter()
                    .any(|c| c.anno_id.as_deref() == Some(a.id.as_str()))
            })
            .expect("a seeded annotation with a pinned comment");
        let pinned = comments
            .iter()
            .filter(|c| c.anno_id.as_deref() == Some(target.id.as_str()))
            .count();
        assert!(
            pinned >= 1,
            "precondition: the annotation has a pinned comment"
        );
        dispatch_encoded(doc, encode(&delete_annotation_action(&target.id)));
        assert!(
            PdfDoc::list_annotations(doc)
                .iter()
                .all(|a| a.id != target.id),
            "the annotation is gone"
        );
        assert!(
            PdfDoc::list_comments(doc)
                .iter()
                .all(|c| c.anno_id.as_deref() != Some(target.id.as_str())),
            "and every comment pinned to it went with it"
        );

        // perform_action undo is attributed to the local author.
        {
            let (mut app, mut state) = open_with_markup();
            let before_len = PdfDoc::list_annotations(&state.doc).len();
            let before_heads = state.doc.get_heads();

            // Add a highlight through the exact path the highlight tool uses.
            let a = Annotation {
                id: Annotation::new_id(),
                page: 1,
                kind: AnnoKind::highlight(PtRect::new(70.0, 300.0, 120.0, 12.0)),
                color: Swatch::Green,
                author: "You".into(),
                chars: None,
                created_ms: 0,
                space: crate::model::GeomSpace::Page,
            };
            app.perform_action(&mut state.doc, &add_annotation_action(&a));

            // The host's own recorder drains the stashed heads and records the
            // boundary. Going through it rather than pushing an entry by hand is
            // the point: attribution happens in there, and undo is author-scoped.
            // The node map is empty because the app widget *is* the root here.
            let local_author = ui::collaboration::local_user_id();
            let root: *mut dyn Widget = &mut app;
            assert!(
                ui::undo::record_dispatched_action(
                    &mut state,
                    root,
                    &std::collections::HashMap::new(),
                    &local_author,
                ),
                "the host recorded an undo boundary for the dispatched action"
            );

            assert_eq!(
                PdfDoc::list_annotations(&state.doc).len(),
                before_len + 1,
                "the action added the highlight"
            );
            let entry = state
                .peek_last_undo()
                .expect("exactly one undo boundary recorded");
            assert_eq!(
                entry.before, before_heads,
                "the boundary brackets the action"
            );
            assert_eq!(
                entry.author.as_ref(),
                Some(&local_author),
                "the host attributed it to the local user, so their Ctrl+Z can reach it"
            );

            state
                .undo_as(Some(&local_author), None, None)
                .expect("an undo entry to pop");
            assert_eq!(
                PdfDoc::list_annotations(&state.doc).len(),
                before_len,
                "undo reverted the added highlight and nothing else"
            );
        }
    }

    #[test]
    fn imported_pages_replay_identically_and_bytes_travel_through_the_content_store() {
        let mut a = AutoCommit::new();
        PdfDoc::ensure_page_plan(&mut a, 1);
        a.commit();
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let page = PagePlanEntry {
            id: "import-replay-id".into(),
            source_page: 1,
            imported: Some(ImportedPage {
                hash: "b".repeat(64),
                kind: "image".into(),
                page: 1,
                width_milli: 612_000,
                height_milli: 408_000,
            }),
            rotation: 0,
        };
        let action = import_pages_action(Some("source-1"), vec![page]);
        let encoded = encode(&action);
        assert_eq!(encoded.payload, encode(&action).payload);
        let env = tilo_actions::apply_local(&mut a, &action, encoded).unwrap();
        tilo_actions::replay(&mut b, &env, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads());
        assert_eq!(PdfDoc::list_page_plan(&a), PdfDoc::list_page_plan(&b));

        // Phase: imported_bytes_cross_between_content_stores_without_entering_the_action.
        {
            let producer = ContentStore::memory();
            let consumer = ContentStore::new(PeerFetchingBackend::new(producer.clone()));
            let bytes = b"content that is deliberately not in Automerge";
            let hash = producer
                .put(bytes)
                .expect("test fixture fits the content quota");
            assert!(producer.has(&hash));
            assert!(!consumer.has(&hash));
            assert_eq!(consumer.state(&hash), ContentState::Unavailable);

            // The action names the content by hash only.
            let pages = vec![PagePlanEntry {
                id: "imported".into(),
                source_page: 1,
                imported: Some(ImportedPage {
                    hash: hash.clone(),
                    kind: "image".into(),
                    page: 1,
                    width_milli: 1_000,
                    height_milli: 1_000,
                }),
                rotation: 0,
            }];
            let action = import_pages_action(None, pages);
            let encoded = format!("{action:?}");
            assert!(encoded.contains(&hash));
            assert!(!encoded.contains("deliberately not in Automerge"));

            // First read is intentionally non-blocking; it asks the peer.
            assert!(consumer.read(&hash).is_none());
            assert_eq!(consumer.state(&hash), ContentState::Resolved);
            assert_eq!(consumer.read(&hash).as_deref(), Some(bytes.as_slice()));
            // Different bytes can never be stored under this address.
            assert_ne!(consumer.put(b"tampered").ok().as_ref(), Some(&hash));
            assert_eq!(consumer.read(&hash).as_deref(), Some(bytes.as_slice()));
        }
    }
}
