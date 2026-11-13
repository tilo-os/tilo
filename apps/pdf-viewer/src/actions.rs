//! What the reader does beside its markup, and the typed edits and verbs it submits.
//! Presentation stays out of the document and is a [`PdfEffect`]; every markup change
//! an affordance makes is one typed edit built here, the same value the palette and an
//! agent invoke. The outline and the find hits are live rows with view operations only.

use ui::framework::LiveRow;
use ui::tilo_resource;
use ui::tilo_resource::{Edit, Effect, Id, Invoke};

use crate::markup::{PagePlanEntry, PdfMarkup};
use crate::model::{Annotation, FormEntry, GeomSpace, HighlightColor, PtRect, Signature, Tool};
use crate::resource::{NewAnnotation, NewComment, NewNote, NewSignature, pdf_markup};

pub const CONTEXTUAL_CHILD_LIMIT: usize = 32;
/// The first page of matches: resources are consumed by search, agents and history,
/// so a pathological PDF must not become an unbounded action directory.
pub const SEARCH_RESULT_RESOURCE_LIMIT: usize = 32;

pub fn search_result_token(query: &str, whole_word: bool, case_sensitive: bool) -> String {
    let mut hasher = fnv::FnvHasher::default();
    std::hash::Hasher::write(&mut hasher, query.trim().as_bytes());
    std::hash::Hasher::write(
        &mut hasher,
        &[u8::from(whole_word), u8::from(case_sensitive)],
    );
    format!("{:016x}", std::hash::Hasher::finish(&hasher))
}

/// The resource id of one outline entry: its immutable value and its occurrence
/// among identical entries, never its position, so a changed entry fails closed.
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

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "pdf_viewer.effects")]
pub enum PdfEffect {
    /// Previous page
    #[effect(view)]
    #[effect(label = "pdf-bar-previous-page")]
    PreviousPage,
    /// Next page
    #[effect(view)]
    #[effect(label = "pdf-bar-next-page")]
    NextPage,
    /// Zoom in
    #[effect(view)]
    #[effect(label = "pdf-bar-zoom-in")]
    ZoomIn,
    /// Zoom out
    #[effect(view)]
    #[effect(label = "pdf-bar-zoom-out")]
    ZoomOut,
    /// Fit page
    #[effect(view, idempotent)]
    #[effect(label = "pdf-bar-fit-page")]
    FitPage,
    /// Fit width
    #[effect(view, idempotent)]
    #[effect(label = "pdf-bar-fit-width")]
    FitWidth,
    /// Toggle sidebar
    #[effect(view)]
    ToggleSidebar,
    /// Toggle page thumbnails
    #[effect(view)]
    TogglePages,
    /// Find in document
    ///
    /// Opens the window's search on the document.
    #[effect(view, idempotent)]
    Find,
    /// Use tool
    ///
    /// What a press on the page does: select, highlight, draw, note, text or sign.
    #[effect(view, idempotent)]
    UseTool {
        #[param("select, highlight, ink, note, text or sign")]
        tool: Tool,
    },
    /// Marker colour
    ///
    /// The colour the next mark takes; a selected mark takes it too.
    #[effect(view, idempotent)]
    #[effect(label = "pdf-bar-marker-colour")]
    SetMarkerColour {
        #[param("yellow, green, pink, blue or purple")]
        colour: HighlightColor,
    },
    /// Mark text as highlight
    #[effect(view, idempotent)]
    #[effect(label = "pdf-tool-highlight")]
    MarkHighlight,
    /// Mark text as underline
    #[effect(view, idempotent)]
    #[effect(label = "pdf-annotation-underline")]
    MarkUnderline,
    /// Mark text as strike-through
    #[effect(view, idempotent)]
    MarkStrikeout,
    /// Go to
    ///
    /// Shows the page an item is on.
    #[effect(view, row, idempotent)]
    GoTo {
        #[param("1-based page")]
        page: usize,
    },
    /// Go to match
    ///
    /// A bounded find result, carrying its query so the exact result is restored.
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
    /// The text of the PDF's pages, at most 50 at a time: read on from `next`.
    /// Reads the PDF the record pinned; changes nothing.
    #[effect(kind = "read", returns = crate::model::PdfText, idempotent)]
    ReadText {
        #[param("The first page to read, 1-based; the first page when omitted")]
        from_page: Option<usize>,
        #[param("The last page to read, 1-based; 50 pages on when omitted")]
        to_page: Option<usize>,
    },
    /// Read form fields
    ///
    /// The PDF's form fields: name, kind, value, the path a field edit takes
    /// (`form_values/<name>`), choices, page, read-only and required. Changes nothing.
    #[effect(kind = "read", returns = crate::model::FormFields, idempotent)]
    ReadFormFields,
    /// Look at a page
    ///
    /// A picture of one page as the PDF draws it, kept as content. Changes nothing.
    #[effect(kind = "read", returns, idempotent)]
    LookPage {
        #[param("The page to look at, 1-based")]
        page: usize,
        #[param("The picture's longest side in pixels, 64 to 1536; 1024 when omitted")]
        size: Option<u32>,
    },
    /// Open PDF from content
    ///
    /// Pins a PDF the workspace keeps as content as the document this record
    /// marks up. A record marks up one PDF: another is refused.
    #[effect(kind = "undoable", returns, idempotent, audience = "agents")]
    ImportPdfContent {
        #[param(content, "The PDF's content reference, `tilo.content:<reference>`")]
        content: String,
        #[param("The file's name, such as report.pdf; Document.pdf when omitted")]
        name: Option<String>,
    },
    /// Select text
    ///
    /// Selects `text` on a page, all of it when omitted.
    #[effect(job, returns)]
    SelectText {
        #[param(
            "The text to select, its first occurrence on the page; the whole page when omitted"
        )]
        text: Option<String>,
        #[param("The page, 1-based; the current page when omitted")]
        page: Option<usize>,
    },
    /// Select all
    #[effect(view, idempotent)]
    SelectAll,
    /// Copy selection
    ///
    /// Copies the selected text to the clipboard and answers with it.
    #[effect(job, returns)]
    CopySelection,
    /// Open PDF
    ///
    /// Opens a PDF on its own markup record, in this window.
    #[effect(job, cancellable, destructive)]
    #[effect(label = "pdf-empty-open")]
    OpenPdf {
        #[param("Absolute path; omit to use the system picker")]
        path: Option<tilo_resource::AbsolutePath>,
    },
    /// Insert pages from a file
    ///
    /// Pages of the PDF or picture at `path`, all of them or those `pages` names,
    /// the first at position `at`.
    #[effect(job, returns)]
    InsertPages {
        #[param("Absolute path of the PDF or picture")]
        path: tilo_resource::AbsolutePath,
        #[param(
            "The position the first new page takes, 1-based; 1 puts them first; after the last page when omitted"
        )]
        at: Option<usize>,
        #[param("Its pages to insert, 1-based, in this order; every page when omitted")]
        pages: Option<Vec<usize>>,
    },
    /// Save annotated PDF
    #[effect(job, cancellable, destructive)]
    #[effect(label = "pdf-bar-save")]
    SaveAs {
        #[param("Absolute path; omit to use the system picker")]
        path: Option<tilo_resource::AbsolutePath>,
    },
}

fn annotation(id: &str) -> <Annotation as tilo_resource::Model>::Paths<PdfMarkup> {
    PdfMarkup::paths().annotations.item(&Id::from_raw(id))
}

fn comment(id: &str) -> <crate::model::Comment as tilo_resource::Model>::Paths<PdfMarkup> {
    PdfMarkup::paths().comments.item(&Id::from_raw(id))
}

fn edit(edit: &Edit<PdfMarkup>) -> Invoke<PdfMarkup> {
    edit.into()
}

fn group(edits: &[Edit<PdfMarkup>]) -> Invoke<PdfMarkup> {
    Invoke::group(edits.iter().map(Invoke::from))
}

pub fn add_annotation_on(a: &Annotation, page_id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::ADD_ANNOTATION.with(NewAnnotation {
        id: a.id.clone(),
        page_id: page_id.to_owned(),
        mark: a.kind.clone(),
        color: a.color,
        chars: a.chars,
        space: a.space,
    })
}

pub fn set_annotation_color_action(id: &str, color: HighlightColor) -> Invoke<PdfMarkup> {
    edit(&annotation(id).color.set(color))
}

pub fn set_annotation_position_user_action(
    markup: &PdfMarkup,
    id: &str,
    x: f64,
    y: f64,
) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    matches!(stored.kind, crate::model::AnnoKind::Note { .. }).then(|| {
        let paths = annotation(id);
        group(&[
            paths.kind.set(crate::model::AnnoKind::Note { x, y }),
            paths.space.set(GeomSpace::User),
        ])
    })
}

pub fn set_form_value_action(name: &str, value: &FormEntry) -> Invoke<PdfMarkup> {
    edit(&PdfMarkup::paths().form_values.key(name).set(value.clone()))
}

pub fn import_existing_action(
    source_hash: &str,
    plan: &crate::adopt::ImportPlan,
) -> Invoke<PdfMarkup> {
    use crate::resource::{AdoptedAnnotation, AdoptedReply, AdoptedThread, ExistingMarkup};
    let markup = ExistingMarkup {
        annotations: plan
            .annotations
            .iter()
            .map(|a| AdoptedAnnotation {
                id: a.id.clone(),
                page_id: a.page_id.clone(),
                mark: a.kind.clone(),
                color: a.color,
                author: a.author.clone(),
                created_ms: a.created_ms,
            })
            .collect(),
        comments: plan
            .comments
            .iter()
            .map(|t| AdoptedThread {
                id: t.id.clone(),
                anno: t.pinned().map(str::to_owned),
                page_id: t.comment.anchor.page_id.clone(),
                author: t.comment.author.clone(),
                text: t.comment.text.clone(),
                date: t.comment.date.clone(),
                replies: t
                    .replies()
                    .map(|(id, r)| AdoptedReply {
                        id: id.to_owned(),
                        author: r.author.clone(),
                        text: r.text.clone(),
                    })
                    .collect(),
            })
            .collect(),
        form_values: plan.form_values.iter().cloned().collect(),
        adopted: plan.adopted.clone(),
    };
    PdfMarkup::ADOPT_EXISTING.with((source_hash.to_owned(), markup))
}

pub fn set_free_text_action(markup: &PdfMarkup, id: &str, text: &str) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    let crate::model::AnnoKind::FreeText { rect, size, .. } = &stored.kind else {
        return None;
    };
    Some(edit(&annotation(id).kind.set(
        crate::model::AnnoKind::FreeText {
            rect: *rect,
            text: text.to_owned(),
            size: *size,
        },
    )))
}

pub fn set_free_text_box_user_action(
    markup: &PdfMarkup,
    id: &str,
    rect: &PtRect,
    size: f64,
) -> Option<Invoke<PdfMarkup>> {
    let stored = markup.annotations.find(&Id::from_raw(id))?;
    let crate::model::AnnoKind::FreeText { text, .. } = &stored.kind else {
        return None;
    };
    let paths = annotation(id);
    Some(group(&[
        paths.kind.set(crate::model::AnnoKind::FreeText {
            rect: *rect,
            text: text.clone(),
            size,
        }),
        paths.space.set(GeomSpace::User),
    ]))
}

pub fn pin_source_action(source: &ui::tilo_resource::Bytes, name: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::PIN_SOURCE.with((source.clone(), name.to_owned()))
}

pub fn record_reading_position_action(page: &str, fraction: f64, zoom: f64) -> Invoke<PdfMarkup> {
    PdfMarkup::RECORD_READING_POSITION.with((page.to_owned(), fraction, zoom))
}

pub fn delete_annotation_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_ANNOTATION.with(Id::from_raw(id))
}

pub struct NoteDraft<'a> {
    pub anno_id: &'a str,
    pub comment_id: &'a str,
    pub color: HighlightColor,
    pub body: &'a str,
}

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

#[cfg(test)]
pub fn set_comment_body_action(id: &str, body: &str) -> Invoke<PdfMarkup> {
    let paths = comment(id);
    group(&[paths.text.replace(body), paths.body.set(Vec::new())])
}

pub fn set_comment_text_action(
    id: &str,
    clips: Vec<tilo_document::ClipBlock>,
) -> Invoke<PdfMarkup> {
    let paths = comment(id);
    group(&[paths.text.replace(plain_of(&clips)), paths.body.set(clips)])
}

pub fn add_reply_text_action(
    comment_id: &str,
    clips: Vec<tilo_document::ClipBlock>,
) -> Invoke<PdfMarkup> {
    PdfMarkup::REPLY.with(pdf_markup::Reply {
        comment: Id::from_raw(comment_id),
        id: Some(crate::model::new_id()),
        text: plain_of(&clips),
        rich: Some(clips),
    })
}

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
    add_reply_action_with_id(comment_id, &crate::model::new_id(), body)
}

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

pub fn insert_pages_action(after: Option<&str>, pages: Vec<PagePlanEntry>) -> Invoke<PdfMarkup> {
    if after.is_some() {
        return import_pages_action(after, pages);
    }
    let ids: Vec<String> = pages.iter().map(|page| page.id.clone()).collect();
    let mut steps = vec![import_pages_action(None, pages)];
    let mut previous: Option<&str> = None;
    for id in &ids {
        steps.push(move_page_plan_action(id, previous));
        previous = Some(id);
    }
    Invoke::group(steps)
}

pub fn delete_page_plan_action(id: &str) -> Invoke<PdfMarkup> {
    PdfMarkup::DELETE_PAGE.with(Id::from_raw(id))
}

pub fn move_page_plan_action(id: &str, after_id: Option<&str>) -> Invoke<PdfMarkup> {
    PdfMarkup::MOVE_PAGE.with((Id::from_raw(id), after_id.map(str::to_owned)))
}

pub fn reorder_page_plan_action(
    ids: &[String],
    from: usize,
    to: usize,
) -> Option<Invoke<PdfMarkup>> {
    let id = ids.get(from)?;
    let after = match to.cmp(&from) {
        std::cmp::Ordering::Equal => return None,
        std::cmp::Ordering::Greater => Some(ids.get(to)?),
        std::cmp::Ordering::Less => to.checked_sub(1).and_then(|before| ids.get(before)),
    };
    Some(move_page_plan_action(id, after.map(String::as_str)))
}

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

#[derive(Debug, Clone)]
pub struct PdfResourceCtx {
    pub doc_id: String,
    pub title: String,
    /// Durable ids parallel to the page order, so an old address never rebinds to
    /// whichever page later takes that ordinal.
    pub page_ids: Vec<String>,
    pub page_sources: Vec<usize>,
    pub search_query: String,
    pub search_whole_word: bool,
    pub search_case_sensitive: bool,
    /// A token for the query and options, so a stale result never rebinds by ordinal.
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

pub fn live_rows(ctx: &PdfResourceCtx) -> Vec<LiveRow> {
    let mut rows: Vec<LiveRow> = (0..ctx.outline.len().min(CONTEXTUAL_CHILD_LIMIT))
        .filter_map(|index| outline_row(ctx, index))
        .collect();
    rows.extend(find_result_row(ctx));
    rows
}

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
