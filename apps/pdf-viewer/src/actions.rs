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

use serde::{Deserialize, Serialize};

use tilo_actions::{
    Action, ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, ParamType, PermissionRequirement, Resource,
};
use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::Effect;

use crate::doc::{ImportedPage, PagePlanEntry, PdfDoc};
use crate::model::{
    AnnoKind, Annotation, Comment, FormEntry, GeomSpace, MARGIN, MarkupStyle, PAGE_H, PAGE_W,
    PtRect, Reply, Signature, SignatureInk, Swatch,
};

/// Registry `type_id` for every PDF-reader action.
pub const PDF_VIEWER_ACTION_TYPE: &str = "pdf_viewer.action";

/// The action palette is refreshed frequently. Keep that contextual snapshot
/// small even for a scanned book with thousands of pages or a heavily-reviewed
/// contract, while `PublishesResources::resource` retains complete reachability
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

/// What the reader does that changes no markup: moving through the
/// document and its view, the highlight tool's style, and opening and saving
/// files. Every caller — a toolbar button, the palette, an agent — reaches
/// the reader's handler as one of these.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "pdf_viewer.effects")]
pub enum PdfEffect {
    /// Previous page
    #[effect(view)]
    PreviousPage,
    /// Next page
    #[effect(view)]
    NextPage,
    /// Zoom in
    #[effect(view)]
    ZoomIn,
    /// Zoom out
    #[effect(view)]
    ZoomOut,
    /// Fit page
    #[effect(view, idempotent)]
    FitPage,
    /// Fit width
    #[effect(view, idempotent)]
    FitWidth,
    /// Toggle sidebar
    #[effect(view)]
    ToggleSidebar,
    /// Toggle page thumbnails
    #[effect(view)]
    TogglePages,
    /// Find in document
    #[effect(view, idempotent)]
    Find,
    /// Mark text as highlight
    #[effect(view, idempotent)]
    MarkHighlight,
    /// Mark text as underline
    #[effect(view, idempotent)]
    MarkUnderline,
    /// Mark text as strike-through
    #[effect(view, idempotent)]
    MarkStrikeout,
    /// Go to
    ///
    /// Shows the page the item (a page, an annotation, a comment, a
    /// bookmark) is on.
    #[effect(view, row, idempotent)]
    GoTo {
        #[param("1-based page")]
        page: usize,
    },
    /// Go to match
    ///
    /// A bounded find result, carrying the complete query so the exact
    /// result is restored before navigating.
    #[effect(view, row, idempotent)]
    GoToMatch {
        #[param("the find query")]
        query: String,
        #[param("match whole words only")]
        whole_word: bool,
        #[param("match case")]
        case_sensitive: bool,
        #[param("the result's position")]
        index: usize,
        #[param("1-based page")]
        page: usize,
    },
    /// Open PDF
    ///
    /// Opens a PDF on its own markup record, in this window.
    #[effect(job, cancellable, destructive, device_only)]
    Open {
        #[param("Filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
    /// Save annotated PDF
    #[effect(job, cancellable, destructive, device_only)]
    SaveAs {
        #[param("Filesystem path; omit to use the system picker")]
        path: Option<String>,
    },
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

/// `named`, or — when the payload leaves it empty — who invoked the action,
/// as its envelope pinned them, so a palette or agent edit is signed by its
/// author on every replica without carrying a name.
fn signed(named: &str) -> String {
    if !named.is_empty() {
        return named.to_owned();
    }
    tilo_actions::invoking_context()
        .and_then(|context| context.invoker)
        .map(|invoker| invoker.actor)
        .unwrap_or_default()
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
    /// Add a comment on `page`, pinned to annotation `anno_id` or free on the
    /// page. Ids and the ordinal are minted at authoring time.
    AddComment {
        id: String,
        anno_id: Option<String>,
        page: usize,
        author: String,
        created: i64,
        body: String,
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
        /// The file's name, shown when the record reopens its source.
        #[serde(default)]
        name: String,
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
            | PdfViewerAction::AddComment { .. }
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
            PdfViewerAction::DeletePagePlan { id } => PdfDoc::delete_page_plan(doc, id),
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
                    author: signed(author),
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
                let author = signed(author);
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
                    author,
                    body: body.clone(),
                    // Deterministic: the ordinal is carried, no wall-clock here.
                    created: *created,
                    resolved: false,
                    replies: Vec::new(),
                    blocks: Vec::new(),
                };
                PdfDoc::add_comment(doc, &c);
            }
            PdfViewerAction::AddComment {
                id,
                anno_id,
                page,
                author,
                created,
                body,
            } => {
                comment_text(body, &[])?;
                PdfDoc::add_comment(
                    doc,
                    &Comment {
                        id: id.clone(),
                        anno_id: anno_id.clone(),
                        page: *page,
                        author: signed(author),
                        body: body.clone(),
                        created: *created,
                        resolved: false,
                        replies: Vec::new(),
                        blocks: Vec::new(),
                    },
                );
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
                        author: signed(author),
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
                    signer: signed(signer),
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
            PdfViewerAction::PinSource {
                hash,
                len,
                mime,
                name,
            } => {
                PdfDoc::pin_source(doc, hash, *len, mime, name);
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

    /// What the reader records when it meets a document — the bytes the
    /// markup is made against, the page plan they start with, the markup
    /// the PDF already carries — came from the file, not from anyone's
    /// edit: it syncs like any markup, but no undo takes it back.
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        match self {
            PdfViewerAction::PinSource { .. }
            | PdfViewerAction::EnsurePagePlan { .. }
            | PdfViewerAction::ImportExisting { .. } => tilo_actions::ChangeKind::External,
            _ => tilo_actions::ChangeKind::Content,
        }
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

pub fn pin_source_action(hash: &str, len: u64, name: &str) -> PdfViewerAction {
    PdfViewerAction::PinSource {
        hash: hash.to_string(),
        len,
        mime: "application/pdf".to_string(),
        name: name.to_string(),
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
    .with_action(PdfEffect::GoTo { page: comment.page }.bound())
}

/// Build a comment resource: edit / reply / resolve / delete, all one source of
/// truth with the rail's own buttons.
fn comment_resource(c: &Comment) -> Resource {
    let mut res = Resource::new(
        format!("pdf.comment:{}", c.id),
        "pdf.comment",
        comment_label(c),
    );
    res = res.with_action(PdfEffect::GoTo { page: c.page }.bound());

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
    res = res.with_action(
        ActionSpec::prompted(
            "reply",
            "Reply",
            "Reply",
            "your reply…",
            ExecutionLocus::ClientLocal,
            // Signed by whoever replies (see `signed`).
            move |text: &str| encode(&add_reply_action(&reply_id, "", text)),
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
    .with_action(PdfEffect::GoTo { page: page_number }.bound());
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

fn outline_row(ctx: &PdfResourceCtx, index: usize) -> Option<LiveRow> {
    let (title, page, _) = ctx.outline.get(index)?;
    let id = outline_resource_id(ctx, index)?;
    Some(
        LiveRow::new(id, "pdf.outline-item", title.as_str())
            .effect(&PdfEffect::GoTo { page: *page }),
    )
}

fn search_result_row(ctx: &PdfResourceCtx, index: usize, page: usize) -> LiveRow {
    LiveRow::new(
        format!(
            "pdf.search-result:{}:{}:{index}:{page}",
            ctx.doc_id, ctx.search_token
        ),
        "pdf.search-result",
        format!("Match {} on page {page}", index + 1),
    )
    .effect(&PdfEffect::GoToMatch {
        query: ctx.search_query.clone(),
        whole_word: ctx.search_whole_word,
        case_sensitive: ctx.search_case_sensitive,
        index,
        page,
    })
}

/// The current find results — presentation, not markup — as one live row
/// whose children are the bounded first matches.
fn find_result_row(ctx: &PdfResourceCtx) -> Option<LiveRow> {
    if ctx.search_query.trim().is_empty() {
        return None;
    }
    let mut search = LiveRow::new(
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
        search = search.row(search_result_row(ctx, index, page));
    }
    Some(search)
}

/// The published children of a document's markup: its pages, its
/// annotations (each owning the comments pinned to it), its free comments and
/// its signatures, each with the actions that change it. A bounded first page
/// of each; [`markup_child`] resolves the rest by id.
pub(crate) fn markup_children(state: &crate::resource::PdfMarkup) -> Vec<Resource> {
    let mut children = Vec::new();
    for (index, page) in state.pages.iter().enumerate().take(64) {
        children.push(published_page_resource(page, index, &state.pages));
    }
    for anno in state.annotations.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        children.push(annotation_resource(anno, &state.comments));
    }
    for comment in state
        .comments
        .iter()
        .filter(|comment| comment.anno_id.is_none())
        .take(CONTEXTUAL_CHILD_LIMIT)
    {
        children.push(comment_resource(comment));
    }
    for signature in state.signatures.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        children.push(signature_resource(signature));
    }
    children
}

/// The child `id` of a document's markup, wherever its list puts it.
pub(crate) fn markup_child(state: &crate::resource::PdfMarkup, id: &str) -> Option<Resource> {
    if let Some(page_id) = id.strip_prefix("pdf.page:") {
        let index = state.pages.iter().position(|page| page.id == page_id)?;
        return Some(published_page_resource(
            state.pages.get(index)?,
            index,
            &state.pages,
        ));
    }
    if let Some(raw) = id.strip_prefix("pdf.annotation:") {
        return state
            .annotations
            .iter()
            .find(|anno| anno.id == raw)
            .map(|anno| annotation_resource(anno, &state.comments));
    }
    if let Some(raw) = id.strip_prefix("pdf.comment:") {
        return state
            .comments
            .iter()
            .find(|comment| comment.id == raw)
            .map(comment_resource);
    }
    if let Some(raw) = id.strip_prefix("pdf.reply:") {
        let (comment_id, reply_id) = raw.split_once(':')?;
        let comment = state.comments.iter().find(|c| c.id == comment_id)?;
        let reply = comment.replies.iter().find(|r| r.id == reply_id)?;
        return Some(reply_resource(comment, reply));
    }
    let raw = id.strip_prefix("pdf.signature:")?;
    state
        .signatures
        .iter()
        .find(|signature| signature.id == raw)
        .map(signature_resource)
}

fn annotation_resource(anno: &Annotation, comments: &[Comment]) -> Resource {
    let mut res = Resource::new(
        format!("pdf.annotation:{}", anno.id),
        "pdf.annotation",
        anno_label(anno),
    );
    res = res.with_action(PdfEffect::GoTo { page: anno.page }.bound());
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
    for comment in comments
        .iter()
        .filter(|comment| comment.anno_id.as_deref() == Some(anno.id.as_str()))
    {
        res = res.with_child(comment_resource(comment));
    }
    res
}

fn signature_resource(signature: &Signature) -> Resource {
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
    resource.with_action(
        PdfEffect::GoTo {
            page: signature.page,
        }
        .bound(),
    )
}

/// Add a sticky note with this text (`value`) at the top of `page`.
pub const ADD_NOTE: ui::tilo_resource::ActionRef<crate::resource::PdfViewer, (String, usize)> =
    ui::tilo_resource::ActionRef::new("add-note", &[tilo_actions::PROMPTED_PARAM, "page"]);
/// Sign `page` with a typed signature by this signer (`value`; the signer's
/// own name when empty).
pub const ADD_TYPED_SIGNATURE: ui::tilo_resource::ActionRef<
    crate::resource::PdfViewer,
    (String, usize),
> = ui::tilo_resource::ActionRef::new(
    "add-typed-signature",
    &[tilo_actions::PROMPTED_PARAM, "page"],
);

/// The page an action names: its `page` argument, 1 when it names none.
fn page_argument(args: &ActionArgs) -> Result<usize, String> {
    match args.integer("page") {
        None => Ok(1),
        Some(page) => usize::try_from(page)
            .ok()
            .filter(|page| *page >= 1)
            .ok_or_else(|| format!("there is no page {page}")),
    }
}

/// The document's named edits that need no pointer: a sticky note and a
/// typed signature at a predictable, readable place on a page, for the
/// palette, keyboard users and agents. Both are signed by whoever invokes
/// them.
pub(crate) fn declarations() -> Vec<ui::tilo_resource::ActionDecl<crate::resource::PdfViewer>> {
    use ui::tilo_resource::ActionDecl;
    let page = ActionParam::optional("page", ParamType::Integer, "Page (1 when omitted)");
    vec![
        ActionDecl::new(
            "add-note",
            "Add note",
            vec![
                ActionParam::required(tilo_actions::PROMPTED_PARAM, ParamType::Text, "Note"),
                page.clone(),
            ],
            |args, now_ms| {
                let body = args.text(tilo_actions::PROMPTED_PARAM).unwrap_or_default();
                Ok(create_note_action(
                    &NoteDraft {
                        anno_id: &Annotation::new_id(),
                        comment_id: &Comment::new_id(),
                        color: Swatch::Yellow,
                        author: "",
                        created: now_ms,
                        body,
                    },
                    page_argument(args)?,
                    MARGIN,
                    MARGIN,
                ))
            },
        )
        .describe("Add a sticky note with this text at the top of a page."),
        ActionDecl::new(
            "add-typed-signature",
            "Add typed signature",
            vec![
                ActionParam::optional(
                    tilo_actions::PROMPTED_PARAM,
                    ParamType::Text,
                    "Signer (the signer's own name when omitted)",
                ),
                page,
            ],
            |args, now_ms| {
                let signer = args
                    .text(tilo_actions::PROMPTED_PARAM)
                    .map(str::trim)
                    .unwrap_or_default();
                Ok(add_signature_action(&Signature {
                    id: Signature::new_id(),
                    page: page_argument(args)?,
                    // A familiar signing place, clear of the opening paragraph.
                    rect: PtRect::new(PAGE_W - MARGIN - 218.0, PAGE_H - MARGIN - 74.0, 218.0, 74.0),
                    signer: signer.to_owned(),
                    date: crate::app::ymd_from_unix_ms(now_ms),
                    ink: SignatureInk::Typed,
                    space: GeomSpace::Page,
                }))
            },
        )
        .describe("Sign a page with a typed signature dated today."),
    ]
}

/// The open document's live rows: its outline (a bounded first page) and
/// the current find hits. Both come from the PDF and the find bar rather
/// than the record and carry only view operations.
pub fn live_rows(ctx: &PdfResourceCtx) -> Vec<LiveRow> {
    let mut rows: Vec<LiveRow> = (0..ctx.outline.len().min(CONTEXTUAL_CHILD_LIMIT))
        .filter_map(|index| outline_row(ctx, index))
        .collect();
    rows.extend(find_result_row(ctx));
    rows
}

/// The live row `id`, beyond the bounded page [`live_rows`] lists. Every id
/// is checked against the live model, so a changed outline entry or a stale
/// find hit fails closed instead of being rebound by position.
pub fn live_row(ctx: &PdfResourceCtx, id: &str) -> Option<LiveRow> {
    let outline_prefix = format!("pdf.outline:{}:", ctx.doc_id);
    if id.starts_with(&outline_prefix) {
        return ctx.outline.iter().enumerate().find_map(|(index, _)| {
            (outline_resource_id(ctx, index).as_deref() == Some(id))
                .then(|| outline_row(ctx, index))
                .flatten()
        });
    }
    if id == format!("pdf.search:{}", ctx.doc_id) {
        return find_result_row(ctx);
    }
    let search_prefix = format!("pdf.search-result:{}:", ctx.doc_id);
    let raw = id.strip_prefix(&search_prefix)?;
    let mut parts = raw.split(':');
    let token = parts.next()?;
    let index = parts.next()?;
    let page = parts.next()?;
    if parts.next().is_some() || token != ctx.search_token {
        return None;
    }
    let index = index.parse::<usize>().ok()?;
    let page = page.parse::<usize>().ok()?;
    ctx.search_pages
        .get(index)
        .filter(|current| **current == page)
        .map(|_| search_result_row(ctx, index, page))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PdfApp;
    use automerge::AutoCommit;
    use std::collections::BTreeSet;
    use std::io::Read;
    use std::sync::Mutex;
    use tilo_actions::{ActionRegistry, LocalTransport, OperationKind};
    use tilo_content::{
        ContentBackend, ContentError, ContentReader, ContentState, ContentStore, MemoryBackend,
        Stored,
    };

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
        ActionRegistry::with(register)
    }

    /// The sample reader, synced once so its demonstration markup (two
    /// highlights, a sticky note with a two-reply thread, a highlight comment,
    /// and a free resolved comment) is in the shared document.
    fn open_with_markup() -> (PdfApp, ui::State) {
        let mut app = crate::scenarios::sample_reader();
        let mut state = crate::scenarios::sample_state();
        app.sync_settled(&mut state.doc, &state.root);
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
            let app = app.as_ref().expect("the contract seed ran first");
            let mut resources = vec![published(doc)];
            resources.extend(app.document_rows().into_iter().map(LiveRow::into_resource));
            resources
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

    /// The record's publication as the palette and agents see it.
    fn published(doc: &AutoCommit) -> Resource {
        let state = <crate::resource::PdfViewer as ui::tilo_resource::Resource>::read(doc)
            .expect("the record reads");
        ui::tilo_resource::publish::<crate::resource::PdfViewer>("pdf.document", "PDF", &state)
    }

    /// Ctrl+K on the reader yields the full nested tree: the open document →
    /// annotation → comment, plus free-comment resources, each with the actions
    /// the data model implies; the outline and find hits are live rows beside
    /// it. Large documents stay bounded with omitted children resolvable by
    /// id, and outline ids survive reordering but fail closed when changed.
    #[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
    #[test]
    #[allow(
        clippy::similar_names,
        reason = "merged phases keep their original state/stale binding names"
    )]
    fn document_resources_publish_the_nested_tree_with_bounded_stable_children() {
        let (app, state) = open_with_markup();
        let document = published(&state.doc);
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
        let live: Vec<Resource> = app
            .document_rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        let outline = live
            .iter()
            .find(|r| r.kind == "pdf.outline-item")
            .expect("an outline item beside the published document");
        assert!(
            outline
                .actions
                .iter()
                .all(|action| action.kind == OperationKind::View),
            "the outline is a live row with view operations only"
        );
        let resolved = app
            .document_row(outline.id.as_str())
            .map(LiveRow::into_resource)
            .expect("an eager outline item resolves");
        assert_eq!(
            resolved.id, outline.id,
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
        assert!(reply.actions.iter().any(|action| action.id == "go-to"));
        let markup = <crate::resource::PdfViewer as ui::tilo_resource::Resource>::read(&state.doc)
            .expect("the record reads");
        assert!(
            markup_child(&markup, reply.id.as_str()).is_some(),
            "a reply remains addressable by id for agents and history"
        );

        // The seeded free comment (pinned to no annotation) is a document child.
        assert!(
            document.children.iter().any(|r| r.kind == "pdf.comment"),
            "a free-comment resource hangs off the document"
        );

        // A large document's publication is bounded and omitted pages resolve
        // by id.
        let page_count = 64 + 9;
        let large = crate::resource::PdfMarkup {
            pages: (1..=page_count)
                .map(|page| PagePlanEntry {
                    id: format!("page-{page}"),
                    source_page: page,
                    imported: None,
                    rotation: 0,
                })
                .collect(),
            ..crate::resource::PdfMarkup::default()
        };
        let eager_pages = markup_children(&large)
            .iter()
            .filter(|resource| resource.kind == "pdf.page")
            .count();
        assert_eq!(eager_pages, 64);
        let omitted = format!("pdf.page:page-{page_count}");
        let resolved = markup_child(&large, &omitted).expect("an omitted page resolves");
        assert_eq!(resolved.id.as_str(), omitted);
        assert_eq!(resolved.label, format!("Page {page_count}"));
        assert!(markup_child(&large, "pdf.page:page-999").is_none());

        let mut find = resource_ctx("Large fixture", Vec::new(), Vec::new());
        find.search_query = "fixture".into();
        find.search_token = search_result_token("fixture", false, false);
        find.search_pages = (1..=SEARCH_RESULT_RESOURCE_LIMIT + 9).collect();
        let find_resource = live_rows(&find)
            .into_iter()
            .map(LiveRow::into_resource)
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
            live_row(&find, &omitted_find).is_some(),
            "an omitted find hit remains addressable by its stable query/result id"
        );
        let mut stale = find;
        stale.search_token = search_result_token("other", false, false);
        assert!(live_row(&stale, &omitted_find).is_none());

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
            assert!(live_row(&ctx, id).is_some(), "outline id {id}");
        }
        ctx.outline.insert(0, ("Preface".into(), 1, 0));
        assert!(
            live_row(&ctx, &first_security).is_some(),
            "inserting another bookmark does not rebind the saved action"
        );
        assert!(live_row(&ctx, &introduction).is_some());
        assert!(live_row(&ctx, &duplicate_security).is_some());
        ctx.outline[1].1 = 3;
        assert!(
            live_row(&ctx, &introduction).is_none(),
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
    fn markup_actions_apply_through_the_registry_and_undo_as_their_author() {
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

        // The declared typed signature is replayable, uses the named signer
        // and signs as the invoker when none is named.
        let mut app =
            ui::framework::testing::TestApp::<crate::PdfViewerShell>::new().expect("the reader");
        app.invoke(ADD_TYPED_SIGNATURE.with(("Grace Hopper".into(), 2)))
            .expect("the owner signs");
        app.invoke(ADD_TYPED_SIGNATURE.with((String::new(), 1)))
            .expect("the owner signs as themselves");
        app.invoke(ADD_NOTE.with(("Check the figures".into(), 3)))
            .expect("the owner adds a note");
        let state = app.state();
        let signature = state
            .signatures
            .iter()
            .find(|signature| signature.signer == "Grace Hopper")
            .expect("named signer is saved");
        assert_eq!(signature.ink, SignatureInk::Typed);
        assert_eq!(signature.page, 2);
        let own = state
            .signatures
            .iter()
            .find(|signature| signature.page == 1)
            .expect("unnamed signature is saved");
        assert!(
            !own.signer.is_empty(),
            "an unnamed signature is the invoker's"
        );
        let note = state
            .comments
            .iter()
            .find(|comment| comment.body == "Check the figures")
            .expect("the note's comment");
        assert_eq!(note.page, 3);
        assert_eq!(note.author, own.signer, "both are signed by the invoker");

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

        // A markup action is one undo step of its author's.
        {
            let mut app = ui::framework::testing::TestApp::<crate::PdfViewerShell>::new()
                .expect("the reader");
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
            app.invoke(ui::tilo_resource::Invoke::apply(&add_annotation_action(&a)).unwrap())
                .expect("the owner highlights");
            assert_eq!(
                app.state().annotations.len(),
                1,
                "the action added the highlight"
            );
            assert!(app.undo(), "the highlight is an undo step");
            assert!(
                app.state().annotations.is_empty(),
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
