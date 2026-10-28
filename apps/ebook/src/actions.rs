//! What the reader publishes about where it is in the open book, and the
//! library's verbs bound to that place.
//!
//! The library's own tree (its books, their annotations, the fonts and
//! schemes, every verb and field edit) is published by
//! [`Ebook`]. The reader adds the page it shows:
//! turning pages, the contents and notes panels, selecting a passage
//! (all view effects), and the library's verbs with the page or the
//! selection filled in — bookmark this page, note this page, highlight or
//! note the selection.

use tilo_actions::{ActionSpec, Resource};
use ui::tilo_resource::{ActionDecl, Effect};

use crate::model::HiColor;
use crate::resource::{BooksEffect, Ebook, HiPiece, books};

/// What invoking `effect` where it is bound sends its handler.
#[cfg(test)]
pub(crate) fn effect_payload(effect: &BooksEffect) -> Vec<u8> {
    effect
        .bound()
        .build_json(&serde_json::Map::new())
        .map(|encoded| encoded.payload)
        .unwrap_or_default()
}

/// Going to and selecting a passage of `book`, asking for the place; a
/// chapter left out is `chapter`.
pub(crate) fn passage_effects(book: &str, chapter: usize) -> [ActionSpec; 2] {
    [
        BooksEffect::SelectPassage {
            book: book.to_owned(),
            chapter: Some(chapter),
            start_block: 0,
            start_char: None,
            end_block: None,
            end_char: None,
        }
        .bound_asking(&[
            "chapter",
            "start_block",
            "start_char",
            "end_block",
            "end_char",
        ]),
        BooksEffect::GoTo {
            book: book.to_owned(),
            chapter: Some(chapter),
            block: None,
        }
        .bound_asking(&["chapter", "block"]),
    ]
}

/// `effect` bound to the row it acts on, under that row's own `label`.
pub(crate) fn bound(effect: &BooksEffect, label: &str) -> ActionSpec {
    let mut spec = effect.bound();
    label.clone_into(&mut spec.type_mut().label);
    spec
}

pub(crate) fn open_at(book: &str, chapter: usize, block: Option<usize>, label: &str) -> ActionSpec {
    bound(
        &BooksEffect::Open {
            book: book.to_owned(),
            chapter,
            page: 0,
            block,
        },
        label,
    )
}

/// Where the reader is in the open book, and the library's verbs with that
/// place filled in.
#[derive(Debug, Clone)]
pub struct BookResourceCtx {
    pub book_id: String,
    pub chapter: usize,
    /// Bookmarking the page the reader is on (or removing its bookmark).
    pub bookmark: books::ToggleBookmark,
    /// A note on the page the reader is on, its text left to write.
    pub page_note: books::AddNote,
    /// Highlighting the selection (or removing its highlight).
    pub selection_highlight: Option<books::Highlight>,
    /// A note on the selection, its text left to write.
    pub selection_note: Option<books::AddNote>,
}

/// Where the reader is in the open book, published beside the library: page
/// chrome and passage selection, which change only this reader's view and
/// never the library. The library's verbs bound to the same place are
/// [`reading_declarations`].
pub(crate) fn reading_resource(ctx: &BookResourceCtx) -> Resource {
    let mut resource = Resource::new(
        format!("ebook.selection:{}", ctx.book_id),
        "ebook.selection",
        "Current reading",
    );
    for spec in passage_effects(&ctx.book_id, ctx.chapter) {
        resource = resource.with_action(spec);
    }
    for (effect, label) in [
        (BooksEffect::PreviousPage, "Previous page"),
        (BooksEffect::NextPage, "Next page"),
        (BooksEffect::Contents, "Toggle table of contents"),
        (BooksEffect::Notes, "Toggle notes"),
        (BooksEffect::Settings, "Open reading settings"),
    ] {
        resource = resource.with_action(bound(&effect, label));
    }
    resource
}

/// The library's verbs bound to where the reader is in the open book: the
/// page's bookmark, a note on the page, and a highlight or note on the
/// selection.
pub(crate) fn reading_declarations(ctx: &BookResourceCtx) -> Vec<ActionDecl<Ebook>> {
    let mut declarations = Vec::new();
    declarations.extend(
        bookmark(&ctx.bookmark)
            .declaration("bookmark-page", "Toggle bookmark")
            .map(|decl| {
                decl.describe("Bookmark the page the reader is on, or remove its bookmark.")
            }),
    );
    declarations.extend(
        note(&ctx.page_note)
            .declaration("note-page", "Add note")
            .map(|decl| decl.describe("Add a note to the page the reader is on.")),
    );
    if let Some(plan) = &ctx.selection_highlight {
        declarations.extend(
            highlight(plan)
                .declaration("highlight-selection", "Highlight selection")
                .map(|decl| {
                    decl.describe("Highlight the selected passage, or remove its highlight.")
                }),
        );
    }
    if let Some(draft) = &ctx.selection_note {
        declarations.extend(
            note(draft)
                .declaration("note-selection", "Add note to selection")
                .map(|decl| decl.describe("Add a note to the selected passage.")),
        );
    }
    declarations
}

fn bookmark(params: &books::ToggleBookmark) -> ui::tilo_resource::Bound<Ebook> {
    let paths = books::ToggleBookmark::paths();
    Ebook::TOGGLE_BOOKMARK
        .bind(paths.book, params.book.clone())
        .and(paths.chapter, params.chapter)
        .and(paths.block, params.block)
        .and(paths.excerpt, params.excerpt.clone())
}

/// A note at the passage `params` names, its text left to write.
fn note(params: &books::AddNote) -> ui::tilo_resource::Bound<Ebook> {
    let paths = books::AddNote::paths();
    Ebook::ADD_NOTE
        .bind(paths.passage, params.passage.clone())
        .and(paths.color, params.color)
}

fn highlight(params: &books::Highlight) -> ui::tilo_resource::Bound<Ebook> {
    let paths = books::Highlight::paths();
    Ebook::HIGHLIGHT
        .bind(paths.book, params.book.clone())
        .and(paths.chapter, params.chapter)
        .and(paths.color, params.color)
        .and(paths.replacing, params.replacing.clone())
        .and(paths.pieces, params.pieces.clone())
}

/// A highlight of `pieces` of `chapter` of `book` in `color`, replacing the
/// highlights `replacing`.
pub(crate) fn highlight_params(
    book: &str,
    chapter: usize,
    color: HiColor,
    replacing: Vec<uuid::Uuid>,
    pieces: Vec<HiPiece>,
) -> books::Highlight {
    books::Highlight {
        book: book.to_owned(),
        chapter,
        color,
        replacing,
        pieces,
    }
}

#[cfg(test)]
mod tests;
