//! What the reader does beside its markup, and the typed edits and verbs it
//! submits.
//!
//! Presentation — which page is shown, the zoom / pan, the current
//! selection, the active tool, and the sidebar tab — stays out of the
//! document: those are [`PdfEffect`]s. Every change of the markup the
//! on-page affordances make (the selection HUD's highlight swatches, the
//! toolbar colour swatch, the comment rail's edit / reply / resolve / delete
//! buttons, the note-marker drag, the page rail) is one of the typed field
//! edits or verbs built here and submitted through the resource's
//! dispatcher: the same values the palette and agents invoke.
//!
//! The open document's outline and the current find hits are live rows of
//! the surface, published beside the resource with view operations only.

use ui::framework::LiveRow;
// `#[derive(Effect)]` names the crate as `tilo_resource`.
use ui::tilo_resource;
use ui::tilo_resource::{Edit, Effect, Id, Invoke};

use crate::markup::{PagePlanEntry, PdfMarkup};
use crate::model::{Annotation, FormEntry, GeomSpace, PtRect, Signature, Swatch};
use crate::resource::{NewAnnotation, NewComment, NewNote, NewSignature, pdf_markup};

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
    /// Read text
    ///
    /// Answers with the text of the PDF's pages as its file holds them,
    /// page by page, at most 50 pages at a time: read on from the `next` an
    /// answer gives. Changes nothing.
    #[effect(kind = "read", returns, idempotent)]
    ReadText {
        #[param("The first page to read, 1-based; the first page when omitted")]
        from_page: Option<usize>,
        #[param("The last page to read, 1-based; 50 pages on when omitted")]
        to_page: Option<usize>,
    },
    /// Open PDF
    ///
    /// Opens a PDF on its own markup record, in this window.
    #[effect(job, cancellable, destructive, device_only)]
    OpenPdf {
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

/// The paths of annotation `id`.
fn annotation(
    id: &str,
) -> <crate::markup::StoredAnnotation as tilo_resource::Model>::Paths<PdfMarkup> {
    PdfMarkup::paths().annotations.item(&Id::from_raw(id))
}

/// The paths of comment `id`.
fn comment(id: &str) -> <crate::markup::StoredComment as tilo_resource::Model>::Paths<PdfMarkup> {
    PdfMarkup::paths().comments.item(&Id::from_raw(id))
}

/// One field edit as an invocation.
fn edit(edit: &Edit<PdfMarkup>) -> Invoke<PdfMarkup> {
    edit.into()
}

/// Field edits as one change: one version, one undo step.
fn group(edits: &[Edit<PdfMarkup>]) -> Invoke<PdfMarkup> {
    Invoke::group(edits.iter().map(Invoke::from))
}

/// Add annotation `a`, made by the tools, on page occurrence `page_id`.
pub fn add_annotation_on(a: &Annotation, page_id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::ADD_ANNOTATION.with(NewAnnotation {
        id: a.id.clone(),
        page_id: page_id.to_owned(),
        mark: a.kind.clone(),
        color: a.color,
        chars: a.chars.map(|(start, end)| [start, end]),
        space: a.space,
    })
}

pub fn set_annotation_color_action(id: &str, color: Swatch) -> Invoke<PdfMarkup> {
    edit(&annotation(id).color.set(color))
}

/// Move note `id` to `(x, y)` in its page's PDF user space.
pub fn set_annotation_position_user_action(
    markup: &PdfMarkup,
    id: &str,
    x: f64,
    y: f64,
) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    matches!(stored.mark, crate::model::AnnoKind::Note { .. }).then(|| {
        let paths = annotation(id);
        group(&[
            paths.mark.set(crate::model::AnnoKind::Note { x, y }),
            paths.space.set(GeomSpace::User),
        ])
    })
}

pub fn set_form_value_action(name: &str, value: &FormEntry) -> Invoke<PdfMarkup> {
    edit(&PdfMarkup::paths().form_values.key(name).set(value.clone()))
}

/// Take over the existing markup an import plan found in `source_hash`.
pub fn import_existing_action(
    source_hash: &str,
    plan: &crate::adopt::ImportPlan,
) -> Invoke<PdfMarkup> {
    use crate::resource::{AdoptedAnnotation, AdoptedReply, AdoptedThread, ExistingMarkup};
    let markup = ExistingMarkup {
        annotations: plan
            .annotations
            .iter()
            .map(|(a, page_id)| AdoptedAnnotation {
                id: a.id.clone(),
                page_id: page_id.clone(),
                mark: a.kind.clone(),
                color: a.color,
                author: a.author.clone(),
                created_ms: a.created_ms,
            })
            .collect(),
        comments: plan
            .comments
            .iter()
            .map(|c| AdoptedThread {
                id: c.id.clone(),
                anno: c.anno_id.clone(),
                page_id: plan
                    .annotations
                    .iter()
                    .find(|(a, _)| Some(&a.id) == c.anno_id.as_ref())
                    .map_or_else(
                        || format!("source-{}", c.page),
                        |(_, page_id)| page_id.clone(),
                    ),
                author: c.author.clone(),
                text: c.body.clone(),
                created: c.created,
                replies: c
                    .replies
                    .iter()
                    .map(|r| AdoptedReply {
                        id: r.id.clone(),
                        author: r.author.clone(),
                        text: r.body.clone(),
                    })
                    .collect(),
            })
            .collect(),
        form_values: plan.form_values.iter().cloned().collect(),
        adopted: plan.adopted.clone(),
    };
    PdfMarkup::ADOPT_EXISTING.with((source_hash.to_owned(), markup))
}

/// Change free text `id`'s words, keeping its box.
pub fn set_free_text_action(markup: &PdfMarkup, id: &str, text: &str) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    let crate::model::AnnoKind::FreeText { rect, size, .. } = &stored.mark else {
        return None;
    };
    Some(edit(&annotation(id).mark.set(
        crate::model::AnnoKind::FreeText {
            rect: *rect,
            text: text.to_owned(),
            size: *size,
        },
    )))
}

/// Move or resize free text `id` and set its size; `rect` and `size` in
/// its page's user space.
pub fn set_free_text_box_user_action(
    markup: &PdfMarkup,
    id: &str,
    rect: &PtRect,
    size: f64,
) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    let crate::model::AnnoKind::FreeText { text, .. } = &stored.mark else {
        return None;
    };
    let paths = annotation(id);
    Some(group(&[
        paths.mark.set(crate::model::AnnoKind::FreeText {
            rect: *rect,
            text: text.clone(),
            size,
        }),
        paths.space.set(GeomSpace::User),
    ]))
}

pub fn pin_source_action(hash: &str, len: u64, name: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::PIN_SOURCE.with((hash.to_owned(), len, name.to_owned()))
}

pub fn delete_annotation_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_ANNOTATION.with(Id::from_raw(id))
}

/// A sticky note being authored: the ids minted for its marker and its
/// comment thread, its colour and its opening text.
pub struct NoteDraft<'a> {
    pub anno_id: &'a str,
    pub comment_id: &'a str,
    pub color: Swatch,
    pub body: &'a str,
}

/// A note anchored to page occurrence `page_id` at a point of its PDF user
/// space.
pub fn create_note_on_action(
    note: &NoteDraft<'_>,
    page_id: &str,
    at_user: (f64, f64),
) -> Invoke<PdfMarkup> {
    PdfMarkup::CREATE_NOTE.with(NewNote {
        annotation: note.anno_id.to_owned(),
        comment: note.comment_id.to_owned(),
        page_id: page_id.to_owned(),
        x: at_user.0,
        y: at_user.1,
        color: note.color,
        text: note.body.to_owned(),
        space: GeomSpace::User,
    })
}

/// A comment free on page occurrence `page_id`, or on annotation `anno`.
pub fn add_comment_action(
    id: &str,
    anno: Option<&str>,
    page_id: Option<&str>,
    body: &str,
) -> Invoke<PdfMarkup> {
    PdfMarkup::ADD_COMMENT.with(NewComment {
        id: id.to_owned(),
        anno: anno.map(str::to_owned),
        page_id: page_id.map(str::to_owned),
        text: body.to_owned(),
        rich: Vec::new(),
    })
}

/// Replace a comment's text with plain `body`.
#[cfg(test)]
pub fn set_comment_body_action(id: &str, body: &str) -> Invoke<PdfMarkup> {
    let paths = comment(id);
    group(&[paths.text.replace(body), paths.rich.set(Vec::new())])
}

/// Replace a comment's text with what was written in the editor: its
/// paragraphs as `clips`, its plain text derived from them.
pub fn set_comment_text_action(
    id: &str,
    clips: Vec<tilo_document::ClipBlock>,
) -> Invoke<PdfMarkup> {
    let paths = comment(id);
    group(&[paths.text.replace(plain_of(&clips)), paths.rich.set(clips)])
}

/// Reply to a comment with what was written in the editor.
pub fn add_reply_text_action(
    comment_id: &str,
    clips: Vec<tilo_document::ClipBlock>,
) -> Invoke<PdfMarkup> {
    PdfMarkup::REPLY.with(pdf_markup::Reply {
        comment: Id::from_raw(comment_id),
        id: Some(crate::model::Reply::new_id()),
        text: plain_of(&clips),
        rich: Some(clips),
    })
}

/// The plain text of paragraphs, a line each.
pub(crate) fn plain_of(clips: &[tilo_document::ClipBlock]) -> String {
    clips
        .iter()
        .map(|clip| clip.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn set_comment_resolved_action(id: &str, resolved: bool) -> Invoke<PdfMarkup> {
    edit(&comment(id).resolved.set(resolved))
}

#[cfg(test)]
pub fn add_reply_action(comment_id: &str, body: &str) -> Invoke<PdfMarkup> {
    add_reply_action_with_id(comment_id, &crate::model::Reply::new_id(), body)
}

/// A reply with its already-minted durable id.
pub fn add_reply_action_with_id(comment_id: &str, reply_id: &str, body: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::REPLY.with(pdf_markup::Reply {
        comment: Id::from_raw(comment_id),
        id: Some(reply_id.to_owned()),
        text: body.to_owned(),
        rich: None,
    })
}

pub fn delete_comment_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_COMMENT.with(Id::from_raw(id))
}

/// Place a signature on page occurrence `page_id`, its box in whatever
/// space the signature carries.
pub fn add_signature_on(signature: &Signature, page_id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::ADD_SIGNATURE.with(NewSignature {
        id: signature.id.clone(),
        page_id: page_id.to_owned(),
        rect: signature.rect,
        signer: signature.signer.clone(),
        date: signature.date.clone(),
        ink: signature.ink.clone(),
        space: signature.space,
    })
}

pub fn delete_signature_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_SIGNATURE.with(Id::from_raw(id))
}

pub fn duplicate_page_plan_action(entry: &PagePlanEntry) -> Invoke<PdfMarkup> {
    PdfMarkup::DUPLICATE_PAGE.with((Id::from_raw(entry.id.clone()), None))
}

pub fn import_pages_action(after_id: Option<&str>, pages: Vec<PagePlanEntry>) -> Invoke<PdfMarkup> {
    PdfMarkup::IMPORT_PAGES.with((after_id.map(str::to_owned), pages))
}

pub fn delete_page_plan_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_PAGE.with(Id::from_raw(id))
}

pub fn move_page_plan_action(id: &str, after_id: Option<&str>) -> Invoke<PdfMarkup> {
    PdfMarkup::MOVE_PAGE.with((Id::from_raw(id), after_id.map(str::to_owned)))
}

/// Turn page occurrence `id` to `degrees` clockwise.
#[cfg(test)]
pub fn rotate_page_action(id: &str, degrees: i32) -> Invoke<PdfMarkup> {
    edit(
        &PdfMarkup::paths()
            .pages
            .item(&Id::from_raw(id))
            .rotation
            .set(degrees.rem_euclid(360)),
    )
}

/// The presentation context the reader hands [`provide_resources`] so the
/// document-level "Add note" action can be built from the live reading position
/// and author — state that lives in the app, not the document.
#[derive(Debug, Clone)]
pub struct PdfResourceCtx {
    /// Stable identity for the open document's markup record.
    pub doc_id: String,
    pub title: String,
    /// Durable CRDT occurrence ids parallel to the current shared page order.
    /// They move with a page, so an old resource address never starts
    /// addressing whichever page later occupies its ordinal.
    pub page_ids: Vec<String>,
    /// Immutable source-page identities parallel to `page_ids`.
    pub page_sources: Vec<usize>,
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
