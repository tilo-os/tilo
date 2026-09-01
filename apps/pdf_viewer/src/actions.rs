//! The PDF reader's exported **actions** and the **resource tree** it publishes
//! to Ctrl+K.
//!
//! Every document-mutating operation the reader performs on its shared markup
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
//! the active tool, the sidebar tab — stays out of the document and never
//! becomes an action. Loading and rasterising the PDF, and extracting its text,
//! are external too: neither is a shared-CRDT mutation.

use automerge::AutoCommit;
use serde::{Deserialize, Serialize};

use qos_actions::{
    Action, ActionContract, ActionError, ActionRegistry, ActionSpec, Doc, EncodedAction,
    ExecutionLocus, OperationKind, Resource, ResourceProvider, RetryPolicy,
};

use crate::doc::PdfDoc;
use crate::model::{AnnoKind, Annotation, Comment, MARGIN, PtRect, Reply, Swatch};

/// Registry `type_id` for every PDF-reader action.
pub const PDF_VIEWER_ACTION_TYPE: &str = "pdf_viewer.action";
pub const PDF_VIEWER_OPERATION_TYPE: &str = "pdf_viewer.operation";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PdfOperation {
    PreviousPage,
    NextPage,
    ZoomIn,
    ZoomOut,
    FitPage,
    FitWidth,
    ToggleSidebar,
    Open,
    SaveAs,
}

pub fn decode_operation(type_id: &str, payload: &[u8]) -> Result<Option<PdfOperation>, String> {
    if type_id != PDF_VIEWER_OPERATION_TYPE {
        return Ok(None);
    }
    serde_json::from_slice(payload)
        .map(Some)
        .map_err(|error| error.to_string())
}

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
        | PdfOperation::ToggleSidebar => spec.with_contract(ActionContract::view()),
        PdfOperation::Open | PdfOperation::SaveAs => spec.destructive(),
    }
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
    SetAnnotationColor { id: String, color: String },
    /// Move an annotation's anchor (the draggable sticky-note marker). A no-op
    /// for ink, whose geometry is the polyline rather than a single anchor.
    SetAnnotationPosition { id: String, x: f64, y: f64 },
    /// Delete an annotation. Its anchored comments are removed with it, so a
    /// deleted note never leaves a dangling thread behind.
    DeleteAnnotation { id: String },
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
    SetCommentBody { id: String, body: String },
    /// Set a comment's resolved flag (the resolve chip toggles it).
    SetCommentResolved { id: String, resolved: bool },
    /// Append a reply to a comment thread.
    AddReply {
        comment_id: String,
        author: String,
        body: String,
    },
    /// Delete a comment. If it pins a sticky note, the note annotation is
    /// removed too (its cascade drops this comment and any siblings), matching
    /// the on-page rail's delete. Decided deterministically against the doc.
    DeleteComment { id: String },
}

impl Action for PdfViewerAction {
    fn apply(&self, doc: &mut Doc) -> Result<(), ActionError> {
        match self {
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
    pub author: String,
    /// Base ordinal for a palette-authored comment's stable sort.
    pub created_seed: i64,
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

/// Build the resource tree the reader currently has open: the open document, its
/// annotations (each owning its pinned comments), and any free comments — every
/// document-mutating operation exported as an action.
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
            "open",
            "Open PDF",
            OperationKind::Effect,
            PdfOperation::Open,
        ),
        (
            "save-as",
            "Save annotated PDF",
            OperationKind::Effect,
            PdfOperation::SaveAs,
        ),
    ] {
        let mut spec = operation(id, label, kind, value);
        if value == PdfOperation::PreviousPage && ctx.page <= 1 {
            spec = spec.unavailable("first_page", "Already at the first page");
        }
        if value == PdfOperation::NextPage && ctx.page >= ctx.page_count {
            spec = spec.unavailable("last_page", "Already at the last page");
        }
        root = root.with_action(spec);
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

    let annotations = PdfDoc::list_annotations(doc);
    let comments = PdfDoc::list_comments(doc);

    // Each annotation → a resource owning the comments pinned to it.
    for a in &annotations {
        let mut res = Resource::new(
            format!("pdf.annotation:{}", a.id),
            "pdf.annotation",
            anno_label(a),
        );

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
    for c in comments.iter().filter(|c| c.anno_id.is_none()) {
        root = root.with_child(comment_resource(c, &ctx.author));
    }

    vec![root]
}

impl ResourceProvider for crate::app::PdfApp {
    fn resources(&self, doc: &AutoCommit) -> Vec<Resource> {
        match self.pdf_resource_ctx() {
            Some(ctx) => provide_resources(&ctx, doc),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::PdfApp;
    use qos_actions::{ActionInput, ActionRegistry, LocalTransport};
    use ui::widget::Widget;

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
}
