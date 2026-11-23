//! The markup a document's bytes already carry, read as the marks a reader takes over.

use tilo_pdf::AnnotationKind;

use crate::{Error, Existing, ExistingMark, Field, Rect};

/// The most marks one file's markup is taken over with.
pub const MAX_IMPORTED_ANNOTATIONS: usize = 5_000;

/// The language a colour is written in for a mark (`#rrggbb`).
#[must_use]
pub fn colour(rgba: tilo_pdf::Rgba) -> String {
    crate::colour_of(rgba)
}

/// The name a mark taken from the file at `page` (1-based) and `index` is kept under: the
/// source's content address, shortened, and its place in the file. Two readers taking over the
/// same bytes make the same id, so their resources merge.
#[must_use]
pub fn imported_id(source_hash: &str, page: usize, index: usize) -> String {
    let short = source_hash.get(..16).unwrap_or(source_hash);
    format!("pdf-{short}-{page}-{index}")
}

/// The key a mark's kind is written by: what the app stores so a save can name the file's own
/// object again.
#[must_use]
pub fn annotation_key(kind: &AnnotationKind) -> &'static str {
    match kind {
        AnnotationKind::Text => "note",
        AnnotationKind::FreeText => "freetext",
        AnnotationKind::Highlight => "highlight",
        AnnotationKind::Underline => "underline",
        AnnotationKind::Squiggly => "squiggly",
        AnnotationKind::StrikeOut => "strikeout",
        AnnotationKind::Ink => "ink",
        AnnotationKind::Popup => "popup",
        _ => "other",
    }
}

/// The kind a key names, for a save that removes or keeps the file's object.
#[must_use]
pub fn kind_of(key: &str) -> Option<AnnotationKind> {
    Some(match key {
        "note" => AnnotationKind::Text,
        "freetext" => AnnotationKind::FreeText,
        "highlight" => AnnotationKind::Highlight,
        "underline" => AnnotationKind::Underline,
        "squiggly" => AnnotationKind::Squiggly,
        "strikeout" => AnnotationKind::StrikeOut,
        "ink" => AnnotationKind::Ink,
        "popup" => AnnotationKind::Popup,
        _ => return None,
    })
}

fn mark(page: usize, info: &tilo_pdf::AnnotationInfo) -> ExistingMark {
    ExistingMark {
        page,
        index: info.index,
        name: info.name.clone(),
        kind: annotation_key(&info.kind).to_owned(),
        color: info.color.map(colour),
        contents: info.contents.clone(),
        author: info.author.clone(),
        created: info.created.clone(),
        modified: info.modified.clone(),
        rect: Rect::of(info.rect),
        quads: info.quads.iter().map(|q| Rect::of(*q)).collect(),
        strokes: info
            .ink
            .iter()
            .map(|stroke| stroke.iter().flat_map(|p| [p.x, p.y]).collect())
            .collect(),
        size: info.font_size,
        in_reply_to: info.in_reply_to,
        reply: matches!(info.reply_type, tilo_pdf::ReplyType::Reply)
            && info.in_reply_to.is_some(),
        hidden: info.is_hidden(),
        readable: info.kind.is_reader_markup(),
    }
}

/// The markup and filled form the document carries.
pub(crate) fn existing(document: &tilo_pdf::Document) -> Result<Existing, Error> {
    let mut marks = Vec::new();
    for page in 0..document.page_count() {
        if marks.len() >= MAX_IMPORTED_ANNOTATIONS {
            break;
        }
        let annotations = match document.annotations(page) {
            Ok(annotations) => annotations,
            Err(_) => continue,
        };
        for info in annotations {
            if marks.len() >= MAX_IMPORTED_ANNOTATIONS {
                break;
            }
            marks.push(mark(page, &info));
        }
    }
    let fields = crate::fields_of(document)
        .unwrap_or_default()
        .into_iter()
        .filter(|field: &Field| !field.name.is_empty())
        .collect();
    Ok(Existing { marks, fields })
}
