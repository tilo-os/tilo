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

use qos_actions::{
    Action, ActionArgs, ActionContract, ActionError, ActionParam, ActionRegistry, ActionSpec, Doc,
    EncodedAction, ExecutionLocus, ExecutionMode, OperationKind, ParamType, Resource,
    ResourceProvider, ResultSpec, RetryPolicy,
};

use crate::doc::{ImportedPage, PagePlanEntry, PdfDoc};
use crate::model::{
    AnnoKind, Annotation, Comment, MARGIN, PAGE_H, PAGE_W, PtRect, Reply, Signature, SignatureInk,
    Swatch,
};

/// Registry `type_id` for every PDF-reader action.
pub const PDF_VIEWER_ACTION_TYPE: &str = "pdf_viewer.action";
pub const PDF_VIEWER_OPERATION_TYPE: &str = "pdf_viewer.operation";

/// The action palette is refreshed frequently. Keep that contextual snapshot
/// small even for a scanned book with thousands of pages or a heavily-reviewed
/// contract, while [`ResourceProvider::resource`] retains complete reachability
/// by stable id.
pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;

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
    GoToPage { page: usize },
    Open { path: Option<String> },
    SaveAs { path: Option<String> },
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
        | PdfOperation::GoToPage { .. } => spec.with_contract(ActionContract::view()),
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
        risk: qos_actions::RiskLevel::Destructive,
        confirmation: qos_actions::ConfirmationPolicy::Confirm,
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
    Highlight { x: f64, y: f64, w: f64, h: f64 },
    Ink { points: Vec<(f64, f64)> },
    Note { x: f64, y: f64 },
}

impl AnnoGeom {
    fn from_kind(kind: &AnnoKind) -> Self {
        match kind {
            AnnoKind::Highlight { rect } => AnnoGeom::Highlight {
                x: rect.x,
                y: rect.y,
                w: rect.w,
                h: rect.h,
            },
            AnnoKind::Ink { points } => AnnoGeom::Ink {
                points: points.clone(),
            },
            AnnoKind::Note { x, y } => AnnoGeom::Note { x: *x, y: *y },
        }
    }

    fn to_kind(&self) -> AnnoKind {
        match self {
            AnnoGeom::Highlight { x, y, w, h } => AnnoKind::Highlight {
                rect: PtRect::new(*x, *y, *w, *h),
            },
            AnnoGeom::Ink { points } => AnnoKind::Ink {
                points: points.clone(),
            },
            AnnoGeom::Note { x, y } => AnnoKind::Note { x: *x, y: *y },
        }
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
    /// payload contains only immutable descriptors; qdoc transports the bytes.
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
    /// Add one annotation (highlight band, ink stroke, or sticky-note marker).
    /// `id` is minted at authoring time and carried here.
    AddAnnotation {
        id: String,
        page: usize,
        geom: AnnoGeom,
        color: String,
        author: String,
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
    },
    /// Replace a comment's body text (the inline editor / palette edit).
    SetCommentBody {
        id: String,
        body: String,
    },
    /// Set a comment's resolved flag (the resolve chip toggles it).
    SetCommentResolved {
        id: String,
        resolved: bool,
    },
    /// Append a reply to a comment thread.
    AddReply {
        comment_id: String,
        author: String,
        body: String,
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
    },
    DeleteSignature {
        id: String,
    },
}

impl Action for PdfViewerAction {
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive deterministic reducer keeps every collaborative PDF mutation auditable"
    )]
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
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
            } => {
                let a = Annotation {
                    id: id.clone(),
                    page: *page,
                    kind: geom.to_kind(),
                    color: Swatch::parse(color),
                    author: author.clone(),
                };
                PdfDoc::add_annotation(doc, &a);
            }
            PdfViewerAction::SetAnnotationColor { id, color } => {
                PdfDoc::set_annotation_color(doc, id, Swatch::parse(color));
            }
            PdfViewerAction::SetAnnotationPosition { id, x, y } => {
                PdfDoc::set_annotation_position(doc, id, *x, *y);
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
            } => {
                let a = Annotation {
                    id: anno_id.clone(),
                    page: *page,
                    kind: AnnoKind::Note { x: *x, y: *y },
                    color: Swatch::parse(color),
                    author: author.clone(),
                };
                PdfDoc::add_annotation(doc, &a);
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
                };
                PdfDoc::add_comment(doc, &c);
            }
            PdfViewerAction::SetCommentBody { id, body } => {
                PdfDoc::set_comment_body(doc, id, body);
            }
            PdfViewerAction::SetCommentResolved { id, resolved } => {
                PdfDoc::set_comment_resolved(doc, id, *resolved);
            }
            PdfViewerAction::AddReply {
                comment_id,
                author,
                body,
            } => {
                PdfDoc::add_reply(
                    doc,
                    comment_id,
                    &Reply {
                        author: author.clone(),
                        body: body.clone(),
                    },
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
                },
            ),
            PdfViewerAction::DeleteSignature { id } => PdfDoc::delete_signature(doc, id),
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
    PdfViewerAction::AddAnnotation {
        id: a.id.clone(),
        page: a.page,
        geom: AnnoGeom::from_kind(&a.kind),
        color: a.color.key().to_string(),
        author: a.author.clone(),
    }
}

pub fn set_annotation_color_action(id: &str, color: Swatch) -> PdfViewerAction {
    PdfViewerAction::SetAnnotationColor {
        id: id.to_string(),
        color: color.key().to_string(),
    }
}

pub fn set_annotation_position_action(id: &str, x: f64, y: f64) -> PdfViewerAction {
    PdfViewerAction::SetAnnotationPosition {
        id: id.to_string(),
        x,
        y,
    }
}

pub fn delete_annotation_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeleteAnnotation { id: id.to_string() }
}

#[allow(clippy::too_many_arguments)]
pub fn create_note_action(
    anno_id: &str,
    comment_id: &str,
    page: usize,
    x: f64,
    y: f64,
    color: Swatch,
    author: &str,
    created: i64,
    body: &str,
) -> PdfViewerAction {
    PdfViewerAction::CreateNote {
        anno_id: anno_id.to_string(),
        comment_id: comment_id.to_string(),
        page,
        x,
        y,
        color: color.key().to_string(),
        author: author.to_string(),
        created,
        body: body.to_string(),
    }
}

pub fn set_comment_body_action(id: &str, body: &str) -> PdfViewerAction {
    PdfViewerAction::SetCommentBody {
        id: id.to_string(),
        body: body.to_string(),
    }
}

pub fn set_comment_resolved_action(id: &str, resolved: bool) -> PdfViewerAction {
    PdfViewerAction::SetCommentResolved {
        id: id.to_string(),
        resolved,
    }
}

pub fn add_reply_action(comment_id: &str, author: &str, body: &str) -> PdfViewerAction {
    PdfViewerAction::AddReply {
        comment_id: comment_id.to_string(),
        author: author.to_string(),
        body: body.to_string(),
    }
}

pub fn delete_comment_action(id: &str) -> PdfViewerAction {
    PdfViewerAction::DeleteComment { id: id.to_string() }
}

pub fn add_signature_action(signature: &Signature) -> PdfViewerAction {
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
    pub author: String,
    /// Date captured when the resource snapshot is published. Action payloads
    /// carry this value so a palette invocation replays identically on peers.
    pub signature_date: String,
    /// Base ordinal for a palette-authored comment's stable sort.
    pub created_seed: i64,
    /// Current bounded find projection. Search itself stays local view state;
    /// each materialized hit is still navigable by agents and history.
    pub search_query: String,
    pub search_pages: Vec<usize>,
    pub outline: Vec<(String, usize, u8)>,
}

/// Build an immediate action spec, or `None` if `action` cannot be
/// represented as JSON (only reachable via a non-finite float slipping into
/// the geometry) — such an action is silently omitted from the palette
/// rather than panicking the whole resource tree.
fn encode_spec(id: &str, label: &str, action: &PdfViewerAction) -> Option<ActionSpec> {
    ActionSpec::immediate(id, label, PDF_VIEWER_ACTION_TYPE, action).ok()
}

fn anno_label(a: &Annotation) -> String {
    let kind = match a.kind {
        AnnoKind::Highlight { .. } => "Highlight",
        AnnoKind::Ink { .. } => "Ink",
        AnnoKind::Note { .. } => "Note",
    };
    format!("{kind} · p.{}", a.page)
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
    res = res.with_action(ActionSpec::prompted(
        "edit",
        "Edit comment",
        "Comment",
        "edit the comment…",
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&set_comment_body_action(&edit_id, text)),
    ));

    let reply_id = c.id.clone();
    let reply_author = author.to_string();
    res = res.with_action(ActionSpec::prompted(
        "reply",
        "Reply",
        "Reply",
        "your reply…",
        ExecutionLocus::ClientLocal,
        move |text: &str| encode(&add_reply_action(&reply_id, &reply_author, text)),
    ));

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
    res
}

fn page_resource_id(doc_id: &str, page_id: &str) -> String {
    format!("pdf.page:{doc_id}:{page_id}")
}

fn page_resource(ctx: &PdfResourceCtx, page: usize, page_id: &str) -> Resource {
    let source_page = ctx.page_sources.get(page - 1).copied().unwrap_or(page);
    let entry = PagePlanEntry {
        id: page_id.to_string(),
        source_page,
        imported: ctx.page_imports.get(page - 1).cloned().flatten(),
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
    page_resource
}

fn outline_resource(ctx: &PdfResourceCtx, index: usize, title: &str, page: usize) -> Resource {
    Resource::new(
        format!("pdf.outline:{}:{index}", ctx.doc_id),
        "pdf.outline-item",
        title,
    )
    .with_action(operation(
        "open",
        &format!("Go to page {page}"),
        OperationKind::View,
        PdfOperation::GoToPage { page },
    ))
}

fn search_result_resource(ctx: &PdfResourceCtx, index: usize, page: usize) -> Resource {
    Resource::new(
        format!("pdf.search-result:{}:{index}:{page}", ctx.doc_id),
        "pdf.search-result",
        format!("Match {} · page {page}", index + 1),
    )
    .with_action(operation(
        "open",
        "Go to match",
        OperationKind::View,
        PdfOperation::GoToPage { page },
    ))
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
pub fn provide_resources(ctx: &PdfResourceCtx, doc: &AutoCommit) -> Vec<Resource> {
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

    for (page, page_id) in contextual_pages(ctx) {
        root = root.with_child(page_resource(ctx, page, page_id));
    }

    for (index, (title, page, _level)) in
        ctx.outline.iter().take(CONTEXTUAL_CHILD_LIMIT).enumerate()
    {
        root = root.with_child(outline_resource(ctx, index, title, *page));
    }
    if !ctx.search_query.trim().is_empty() {
        let mut search = Resource::new(
            format!("pdf.search:{}", ctx.doc_id),
            "pdf.search",
            format!("Find results for “{}”", ctx.search_query),
        );
        for (index, page) in ctx
            .search_pages
            .iter()
            .copied()
            .take(CONTEXTUAL_CHILD_LIMIT)
            .enumerate()
        {
            search = search.with_child(search_result_resource(ctx, index, page));
        }
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
                &Annotation::new_id(),
                &Comment::new_id(),
                page,
                MARGIN,
                MARGIN,
                Swatch::Yellow,
                &author,
                created,
                text,
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
            }))
        },
    ));

    let annotations = PdfDoc::list_annotations(doc);
    let comments = PdfDoc::list_comments(doc);

    // Each annotation → a resource owning the comments pinned to it.
    for a in annotations.iter().take(CONTEXTUAL_CHILD_LIMIT) {
        let mut res = Resource::new(
            format!("pdf.annotation:{}", a.id),
            "pdf.annotation",
            anno_label(a),
        );
        res = res.with_action(operation(
            "open",
            "Go to annotation",
            OperationKind::View,
            PdfOperation::GoToPage { page: a.page },
        ));

        let aid = a.id.clone();
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
            &delete_annotation_action(&a.id),
        ) {
            res = res.with_action(spec);
        }

        for c in comments
            .iter()
            .filter(|c| c.anno_id.as_deref() == Some(a.id.as_str()))
        {
            res = res.with_child(comment_resource(c, &ctx.author));
        }
        root = root.with_child(res);
    }

    // Free comments (pinned to no annotation) hang directly off the document.
    for c in comments
        .iter()
        .filter(|c| c.anno_id.is_none())
        .take(CONTEXTUAL_CHILD_LIMIT)
    {
        root = root.with_child(comment_resource(c, &ctx.author));
    }

    for signature in PdfDoc::list_signatures(doc)
        .into_iter()
        .take(CONTEXTUAL_CHILD_LIMIT)
    {
        let label = format!("Signature by {} · p.{}", signature.signer, signature.page);
        let mut resource = Resource::new(
            format!("pdf.signature:{}", signature.id),
            "pdf.signature",
            label,
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
        root = root.with_child(resource);
    }

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
pub fn resolve_resource(ctx: &PdfResourceCtx, doc: &AutoCommit, id: &str) -> Option<Resource> {
    let root_id = format!("pdf.document:{}", ctx.doc_id);
    if id == root_id {
        return provide_resources(ctx, doc).into_iter().next();
    }
    let page_prefix = format!("pdf.page:{}:", ctx.doc_id);
    if let Some(page_id) = id.strip_prefix(&page_prefix) {
        return ctx
            .page_ids
            .iter()
            .position(|candidate| candidate == page_id)
            .map(|index| page_resource(ctx, index + 1, page_id));
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
    if let Some(raw) = id.strip_prefix("pdf.signature:") {
        return PdfDoc::list_signatures(doc)
            .into_iter()
            .find(|signature| signature.id == raw)
            .map(|signature| {
                let mut resource = Resource::new(
                    format!("pdf.signature:{}", signature.id),
                    "pdf.signature",
                    format!("Signature by {} · p.{}", signature.signer, signature.page),
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
    if let Some(raw) = id.strip_prefix(&outline_prefix)
        && let Ok(index) = raw.parse::<usize>()
    {
        return ctx
            .outline
            .get(index)
            .map(|(title, page, _)| outline_resource(ctx, index, title, *page));
    }
    let search_prefix = format!("pdf.search-result:{}:", ctx.doc_id);
    if let Some(raw) = id.strip_prefix(&search_prefix) {
        let (index, page) = raw.split_once(':')?;
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

impl ResourceProvider for crate::app::PdfApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.pdf_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
            None => Vec::new(),
        }
    }

    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<Resource> {
        self.pdf_resource_ctx()
            .and_then(|ctx| resolve_resource(&ctx, doc, id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PdfApp;
    use qdoc::blobstore::{self, BlobStore, BlobTransport, MemoryBlobStore};
    use qos_actions::{ActionInput, ActionRegistry, LocalTransport};
    use serial_test::serial;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use ui::widget::Widget;

    /// A tiny synchronous two-peer bridge. `offer()` belongs to peer A; a
    /// later `want()` from peer B verifies and installs those exact bytes into
    /// B's separate store. It exercises the same qdoc transport boundary used
    /// by runtime-collab without sharing a process-global store by accident.
    #[derive(Default)]
    struct TestBlobBridge {
        offered: Mutex<BTreeMap<String, Vec<u8>>>,
        destination: Mutex<Option<Arc<MemoryBlobStore>>>,
    }

    impl BlobTransport for TestBlobBridge {
        fn offer(&self, hash: &str, bytes: &[u8]) {
            if let Ok(mut offered) = self.offered.lock() {
                offered.insert(hash.to_string(), bytes.to_vec());
            }
        }

        fn want(&self, hash: &str) {
            let bytes = self
                .offered
                .lock()
                .ok()
                .and_then(|offered| offered.get(hash).cloned());
            let destination = self.destination.lock().ok().and_then(|slot| slot.clone());
            if let (Some(bytes), Some(destination)) = (bytes, destination) {
                let _ = destination.put(hash, &bytes);
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
            ActionInput::Immediate(_) => panic!("expected a prompted action: {}", spec.id),
        }
    }

    fn immediate_encode(spec: &ActionSpec) -> EncodedAction {
        match &spec.input {
            ActionInput::Immediate(enc) => enc.clone(),
            ActionInput::Prompted { .. } => panic!("expected an immediate action: {}", spec.id),
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

    /// Ctrl+K on the reader yields the full nested tree: the open document →
    /// annotation → comment, plus free-comment resources, each with the actions
    /// the data model implies. Drives the real resource hook.
    #[test]
    fn ctrl_k_publishes_the_nested_tree() {
        let (app, state) = open_with_markup();
        let resources = Widget::resources(&app, &state.doc);
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
            registry().decode(&immediate_encode(duplicate)).is_ok(),
            "page-plan actions use the same registry as all other shared PDF mutations"
        );
        assert!(
            document
                .children
                .iter()
                .any(|r| r.kind == "pdf.outline-item")
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

        // The seeded free comment (pinned to no annotation) is a document child.
        assert!(
            document.children.iter().any(|r| r.kind == "pdf.comment"),
            "a free-comment resource hangs off the document"
        );

        // The one palette surfaces the tree's actions as rows.
        let mut palette = ui::ActionPalette::new();
        palette.load_open_resources(&resources);
        palette.open();
        palette.set_query("Add note");
        let labels = palette.result_labels();
        assert!(labels.iter().any(|l| l.contains("Add note")), "{labels:?}");
        palette.set_query("Delete annotation");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete annotation")),
            "{labels:?}"
        );
        palette.set_query("Delete comment");
        let labels = palette.result_labels();
        assert!(
            labels.iter().any(|l| l.contains("Delete comment")),
            "{labels:?}"
        );
    }

    #[test]
    fn signature_actions_round_trip_through_the_registry() {
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
        };
        let encoded = encode(&add_signature_action(&signature));
        let decoded = registry().decode(&encoded).unwrap();
        let mut transport = LocalTransport::new();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut transport).unwrap();
        assert_eq!(PdfDoc::list_signatures(&doc)[0].ink, signature.ink);

        let encoded = encode(&delete_signature_action(&signature.id));
        let decoded = registry().decode(&encoded).unwrap();
        qos_actions::dispatch(&mut doc, decoded.as_ref(), encoded, &mut transport).unwrap();
        assert!(PdfDoc::list_signatures(&doc).is_empty());
    }

    #[test]
    fn resource_typed_signature_is_replayable_and_uses_the_prompted_signer() {
        let (app, mut state) = open_with_markup();
        let expected_page = app.pdf_resource_ctx().expect("open document").page;
        let resources = Widget::resources(&app, &state.doc);
        let action = resources[0]
            .actions
            .iter()
            .find(|action| action.id == "add-typed-signature")
            .expect("typed signature action");
        let encoded = prompted_encode(action, "Grace Hopper");
        let decoded = registry().decode(&encoded).expect("decodable action");
        let mut transport = LocalTransport::new();
        qos_actions::dispatch(&mut state.doc, decoded.as_ref(), encoded, &mut transport)
            .expect("signature action applies");
        let signature = PdfDoc::list_signatures(&state.doc)
            .into_iter()
            .find(|signature| signature.signer == "Grace Hopper")
            .expect("prompted signer is saved");
        assert_eq!(signature.ink, SignatureInk::Typed);
        assert_eq!(signature.page, expected_page);
    }

    /// Deleting an annotation through its action removes the annotation *and*
    /// every comment pinned to it.
    #[test]
    fn delete_annotation_removes_its_comments() {
        let (_app, mut state) = open_with_markup();
        let doc = &mut state.doc;
        // Find a seeded annotation that carries at least one pinned comment.
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

        let action = delete_annotation_action(&target.id);
        let encoded = encode(&action);
        let decoded = registry().decode(&encoded).unwrap();
        let mut tx = LocalTransport::new();
        qos_actions::dispatch(doc, decoded.as_ref(), encoded, &mut tx).unwrap();

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
    }

    /// The full undo covenant on one action via the real affordance path:
    /// dispatched through [`PdfApp::perform_action`] it mutates the doc and
    /// stashes exactly one undo boundary, which the host records — and Ctrl+Z
    /// (the host's `undo`) reverts precisely that action.
    #[test]
    fn action_records_one_undo_boundary_and_reverts() {
        let (mut app, mut state) = open_with_markup();
        let before_len = PdfDoc::list_annotations(&state.doc).len();
        let before_heads = state.doc.get_heads();

        // Add a highlight through the exact path the highlight tool uses.
        let a = Annotation {
            id: Annotation::new_id(),
            page: 1,
            kind: AnnoKind::Highlight {
                rect: PtRect::new(70.0, 300.0, 120.0, 12.0),
            },
            color: Swatch::Green,
            author: "You".into(),
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

    /// The collaboration guarantee: replaying the same encoded action on a second
    /// independent replica (from the same base state) lands on the byte-identical
    /// document — same head hashes, not merely the same values.
    #[test]
    fn action_replays_byte_identical_on_a_peer() {
        let (_app, state) = open_with_markup();
        // Two replicas sharing history.
        let mut a = state.doc;
        a.commit();
        let bytes = a.save();
        let mut b = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            a.get_heads(),
            b.get_heads(),
            "precondition: same base state"
        );

        let anno = Annotation {
            id: Annotation::new_id(),
            page: 2,
            kind: AnnoKind::Ink {
                points: vec![(1.0, 2.0), (3.5, 4.25), (10.0, 0.0)],
            },
            color: Swatch::Blue,
            author: "You".into(),
        };
        let action = add_annotation_action(&anno);
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
            PdfDoc::list_annotations(&a).len(),
            PdfDoc::list_annotations(&b).len(),
        );
    }

    #[test]
    fn content_addressed_import_replays_byte_identically() {
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
        };
        let action = import_pages_action(Some("source-1"), vec![page]);
        let encoded = encode(&action);
        assert_eq!(encoded.payload, encode(&action).payload);
        let env = qos_actions::apply_local(&mut a, &action, encoded).unwrap();
        qos_actions::replay(&mut b, &env, &registry()).unwrap();
        assert_eq!(a.get_heads(), b.get_heads());
        assert_eq!(PdfDoc::list_page_plan(&a), PdfDoc::list_page_plan(&b));
    }

    #[test]
    #[serial(pdf_blobstore)]
    fn imported_bytes_cross_a_blob_transport_without_entering_the_action() {
        let producer = Arc::new(MemoryBlobStore::default());
        let consumer = Arc::new(MemoryBlobStore::default());
        let bridge = Arc::new(TestBlobBridge::default());
        blobstore::set_store(producer.clone());
        blobstore::set_transport(&(bridge.clone() as Arc<dyn BlobTransport>));
        let bytes = b"content that is deliberately not in Automerge";
        let hash = blobstore::put(bytes);
        assert!(producer.has(&hash));
        assert!(!consumer.has(&hash));

        let Ok(mut destination) = bridge.destination.lock() else {
            return;
        };
        *destination = Some(consumer.clone());
        drop(destination);
        blobstore::set_store(consumer.clone());
        // First read is intentionally non-blocking; it asks the transport.
        assert!(blobstore::get(&hash).is_none());
        assert_eq!(blobstore::state(&hash), blobstore::BlobState::Resolved);
        assert_eq!(blobstore::get(&hash).as_deref(), Some(bytes.as_slice()));
        assert!(!consumer.put(&hash, b"tampered"));
        assert_eq!(consumer.get(&hash).as_deref(), Some(bytes.as_slice()));
        blobstore::clear_transport();
        blobstore::set_store(Arc::new(MemoryBlobStore::default()));
    }

    /// Single source of truth: a palette "Edit comment" spec and a directly built
    /// edit action encode byte-identically, as do the immediate delete specs.
    #[test]
    fn palette_and_builder_encode_identically() {
        let (app, state) = open_with_markup();
        let resources = Widget::resources(&app, &state.doc);
        let document = &resources[0];
        let anno = document
            .children
            .iter()
            .find(|r| r.kind == "pdf.annotation" && !r.children.is_empty())
            .unwrap();

        // The annotation's delete spec == the delete builder for its id.
        let anno_id = anno.id.as_str().trim_start_matches("pdf.annotation:");
        let spec = anno.actions.iter().find(|a| a.id == "delete").unwrap();
        let from_spec = immediate_encode(spec);
        let from_builder = encode(&delete_annotation_action(anno_id));
        assert_eq!(from_spec.type_id, from_builder.type_id);
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "one source of truth"
        );

        // And a comment's edit spec matches the edit builder for the same text.
        let comment = &anno.children[0];
        let cid = comment.id.as_str().trim_start_matches("pdf.comment:");
        let edit_spec = comment.actions.iter().find(|a| a.id == "edit").unwrap();
        let from_spec = prompted_encode(edit_spec, "revised");
        let from_builder = encode(&set_comment_body_action(cid, "revised"));
        assert_eq!(
            from_spec.payload, from_builder.payload,
            "edit is one source of truth"
        );
    }

    #[test]
    fn large_document_snapshot_is_bounded_and_omitted_pages_resolve_lazily() {
        let page_count = CONTEXTUAL_CHILD_LIMIT + 9;
        let ctx = PdfResourceCtx {
            doc_id: "fixture-document".into(),
            title: "Large fixture".into(),
            page: 1,
            page_count,
            page_ids: (1..=page_count)
                .map(|page| format!("page-{page}"))
                .collect(),
            page_sources: (1..=page_count).collect(),
            page_imports: vec![None; page_count],
            author: "Ada".into(),
            signature_date: "2026-09-06".into(),
            created_seed: 0,
            search_query: String::new(),
            search_pages: Vec::new(),
            outline: Vec::new(),
        };
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

        let mut stale = ctx;
        stale.page_ids.pop();
        stale.page_count -= 1;
        assert!(resolve_resource(&stale, &doc, &omitted).is_none());
    }
}
