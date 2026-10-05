//! Screenshot scenarios: the sample shelf a capture reads its books from,
//! and the library each scenario stages through Books' own actions.
//!
//! What a scenario shows splits in three. The books' text is the sample
//! shelf, a [`Fixture`] value the reader reads instead of files. The shelf
//! rows, reading settings, fonts, schemes, progress and annotations are the
//! library's, staged by [`actions`] through the same door as every control.
//! Where the reader is — the view, the open book, a selection, an open panel
//! — is the reader's own view state, staged by `BooksApp::stage`.

use std::path::PathBuf;

use ui::framework::Fixture;
use ui::tilo_resource::Invoke;
use uuid::Uuid;

use crate::actions::{
    CommentDraft, EbookAction, HiPiece, HighlightPlan, NoteDraft, finalize_annotation_action,
    restore_library_book_action, set_bookmark_action, set_progress_action,
    set_reading_settings_action, upsert_color_scheme_action, upsert_font_action,
};
use crate::content;
use crate::model::{
    AnnotationAnchor, Book, ColorScheme, FontEntry, FontKind, HiColor, LibraryEntry, Progress,
    ReadMode, ReadTheme, Settings, default_schemes,
};
use crate::resource::Ebook;

/// Every scenario `--shots --scenario NAME` captures.
pub(crate) const SCENARIOS: &[&str] = &[
    "onboarding",
    "read",
    "shelf",
    "library",
    "page-bubble",
    "settings-library",
    "narrow",
    "import-error",
    "import-loading",
    "type",
    "large-text",
    "library-scrolled",
    "long-toc",
    "long-annotations",
    "serif",
    "dark",
    "light",
    "sepia",
    "hicontrast",
    "reduced-motion",
    "font-picker",
    "settings-colours",
    "settings-fonts",
    "settings-fonts-rename",
    "settings-shortcuts",
    "scheme-editor",
    "picker",
    "word-select",
    "select",
    "highlight",
    "select-drag",
    "note",
    "comments",
    "continuous",
    "continuous-select",
    "two-up",
    "spread",
    "two-up-select",
    "spread-select",
];

/// The books a capture shows, and where each would live on a reader's
/// device: one on the shared workspace, so the shelf shows its badge.
pub(crate) struct SampleShelf {
    pub books: Vec<(Book, PathBuf)>,
}

impl SampleShelf {
    pub(crate) fn new() -> Self {
        let books = content::load_sample_library()
            .into_iter()
            .enumerate()
            .map(|(index, book)| {
                let path = if index == 1 {
                    content::workspace_books_dir().join("field-notes.epub")
                } else {
                    PathBuf::from(format!("/home/reader/Books/book-{index}.epub"))
                };
                (book, path)
            })
            .collect();
        Self { books }
    }

    /// The sample book `id`, and where it lives.
    pub(crate) fn book(&self, id: &str) -> Option<&(Book, PathBuf)> {
        self.books.iter().find(|(book, _)| book.id == id)
    }
}

/// The sample shelf every capture reads its books from.
pub(crate) fn fixture(fixture: &mut Fixture<Ebook>) {
    fixture.provide(SampleShelf::new());
}

/// When the scenes were written: a fixed time, so every capture is the same.
const STAGED_AT: u64 = 1_767_225_600_000;

/// The note the "note" scenario has open in the composer.
pub(crate) const COMPOSED_NOTE: Uuid = Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0001);
/// The highlight whose comments the "comments" scenario has open.
pub(crate) const COMMENTED_HIGHLIGHT: Uuid =
    Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0000_0002);
/// The "long-annotations" note a jump from search reveals.
pub(crate) const REVEALED_NOTE: Uuid = long_note(25);
/// The custom scheme of the colour settings scenarios.
pub(crate) const CUSTOM_SCHEME: &str = "scheme1";
/// The added system face of the font settings scenarios.
pub(crate) const ADDED_FONT: &str = "font1";

const fn long_note(index: u128) -> Uuid {
    Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0001_0000 + index)
}

const fn staged_id(index: u128) -> Uuid {
    Uuid::from_u128(0xB00C_0000_0000_0000_0000_0000_0002_0000 + index)
}

/// The library `scenario` shows, as the actions that stage it.
pub(crate) fn actions(scenario: Option<&str>) -> Vec<Invoke<Ebook>> {
    let scenario = scenario.unwrap_or("read");
    if scenario == "onboarding" {
        return Vec::new();
    }
    let shelf = content::load_sample_library();
    let mut actions: Vec<EbookAction> = shelf
        .iter()
        .enumerate()
        .map(|(index, book)| {
            restore_library_book_action(&LibraryEntry {
                id: book.id.clone(),
                shared: false,
                title: book.title.clone(),
                author: book.author.clone(),
                subject: book.subject.clone(),
                added: STAGED_AT + index as u64,
                content: None,
            })
        })
        .collect();
    if lived_in(scenario) {
        actions.extend(lived_in_shelf(&shelf));
    }
    if let Some(settings) = settings(scenario) {
        actions.push(set_reading_settings_action(Settings::default(), settings));
    }
    match scenario {
        "settings-colours" => {
            let mut built_in = default_schemes();
            if let Some(first) = built_in.first_mut() {
                first.enabled = false;
                actions.push(upsert_color_scheme_action(first.clone()));
            }
            actions.push(upsert_color_scheme_action(custom_scheme([
                0x15, 0x1A, 0x24,
            ])));
        }
        "scheme-editor" => {
            actions.push(upsert_color_scheme_action(custom_scheme([
                0x15, 0x1A, 0x24,
            ])));
        }
        "picker" => {
            actions.push(upsert_color_scheme_action(custom_scheme(PICKED_PAGE)));
        }
        "settings-fonts" | "settings-fonts-rename" => {
            actions.push(upsert_font_action(FontEntry::new(
                ADDED_FONT,
                FontKind::Named("DejaVu Serif".into()),
                "DejaVu Serif",
                1.08,
                true,
            )));
        }
        "note" => actions.extend(composed_note(&shelf)),
        "comments" => actions.extend(commented_highlight(&shelf)),
        "long-annotations" => actions.extend(long_annotations(&shelf)),
        _ => {}
    }
    actions
        .iter()
        .filter_map(|action| Invoke::apply(action).ok())
        .collect()
}

/// The page colour of the scheme the "picker" scenario edits.
pub(crate) const PICKED_PAGE: [u8; 3] = [0x1A, 0x22, 0x33];

fn custom_scheme(page: [u8; 3]) -> ColorScheme {
    ColorScheme::from_bg(CUSTOM_SCHEME, "Midnight", page)
}

/// Scenarios that show the reader's marks and progress across the shelf;
/// the others prove one thing on a clean page.
fn lived_in(scenario: &str) -> bool {
    !matches!(
        scenario,
        "long-annotations"
            | "large-text"
            | "word-select"
            | "select"
            | "select-drag"
            | "note"
            | "comments"
            | "continuous-select"
            | "two-up-select"
            | "spread-select"
    )
}

/// The reading settings `scenario` reads with.
fn settings(scenario: &str) -> Option<Settings> {
    let mut settings = Settings::default();
    match scenario {
        "serif" => settings.font_id = "serif".into(),
        "dark" => settings.theme = ReadTheme::Dark,
        "light" => settings.theme = ReadTheme::Light,
        "sepia" => settings.theme = ReadTheme::Sepia,
        "hicontrast" => settings.theme = ReadTheme::HighContrast,
        "large-text" => settings.font_scale = Settings::FONT_MAX,
        "continuous" | "continuous-select" => settings.mode = ReadMode::Continuous,
        "two-up" | "spread" | "two-up-select" | "spread-select" => settings.mode = ReadMode::TwoUp,
        _ => return None,
    }
    Some(settings)
}

/// A first chapter's block `index`, cut to an excerpt.
fn excerpt(book: &Book, index: usize) -> String {
    let text = book
        .chapters
        .first()
        .map(|chapter| chapter.block_text(index))
        .unwrap_or_default();
    let mut excerpt: String = text.chars().take(96).collect();
    if text.chars().count() > 96 {
        excerpt.push('…');
    }
    excerpt
}

/// The end of a highlight on block `index` near `target`, at a word
/// boundary so the band never cuts a word.
fn word_end(book: &Book, index: usize, target: usize) -> usize {
    let chars: Vec<char> = book
        .chapters
        .first()
        .map(|chapter| chapter.block_text(index))
        .unwrap_or_default()
        .chars()
        .collect();
    let end = target.min(chars.len());
    chars
        .get(..end)
        .and_then(|slice| slice.iter().rposition(|&c| c == ' '))
        .unwrap_or(end)
}

/// Progress across the shelf (a couple of books finished) and, in the first
/// book, a bookmark, two highlights and a note.
fn lived_in_shelf(shelf: &[Book]) -> Vec<EbookAction> {
    const FRACTIONS: [f32; 5] = [0.34, 0.72, 0.08, 0.5, 1.0];
    let mut actions: Vec<EbookAction> = shelf
        .iter()
        .enumerate()
        .map(|(index, book)| {
            let fraction = FRACTIONS
                .get(index % FRACTIONS.len())
                .copied()
                .unwrap_or(0.5);
            set_progress_action(
                &book.id,
                Progress {
                    chapter: 0,
                    page: (fraction * 6.0) as usize,
                    fraction,
                    opened: STAGED_AT - index as u64 * 3_600_000,
                },
            )
        })
        .collect();
    let Some(book) = shelf.first() else {
        return actions;
    };
    actions.push(set_bookmark_action(
        &book.id,
        staged_id(1),
        0,
        0,
        &excerpt(book, 0),
        true,
        Vec::new(),
    ));
    for (index, (block, end, color)) in [(1, 70, HiColor::Yellow), (3, 58, HiColor::Green)]
        .into_iter()
        .enumerate()
    {
        let index = index as u128;
        actions.push(
            HighlightPlan {
                book_id: book.id.clone(),
                group: staged_id(10 + index).to_string(),
                chapter: 0,
                color: color.as_str().into(),
                remove_groups: Vec::new(),
                pieces: vec![HiPiece {
                    id: staged_id(20 + index).to_string(),
                    block,
                    start: 0,
                    end: word_end(book, block, end),
                    excerpt: excerpt(book, block),
                }],
            }
            .into_action(),
        );
    }
    let note = staged_id(30);
    actions.push(
        NoteDraft {
            book_id: book.id.clone(),
            id: note,
            anchor: anchor(0, 2, 0, 0, excerpt(book, 2)),
            color: HiColor::Blue,
            text:
                "The blue and the amber meeting in the middle — keep this image for the last pane."
                    .into(),
        }
        .into_action(),
    );
    actions.push(finalize_annotation_action(note, STAGED_AT));
    actions
}

fn anchor(
    chapter: usize,
    block: usize,
    start: usize,
    end: usize,
    excerpt: String,
) -> AnnotationAnchor {
    AnnotationAnchor {
        chapter,
        block,
        start,
        end,
        excerpt,
    }
}

/// The note the "note" scenario is writing, on the selected passage.
fn composed_note(shelf: &[Book]) -> Vec<EbookAction> {
    let Some(book) = shelf.first() else {
        return Vec::new();
    };
    vec![
        NoteDraft {
            book_id: book.id.clone(),
            id: COMPOSED_NOTE,
            anchor: anchor(
                0,
                1,
                0,
                0,
                "the keeper climbed the iron stair and set the great wick alight".into(),
            ),
            color: HiColor::Blue,
            text: "A note, written right here on the page — no separate screen.".into(),
        }
        .into_action(),
    ]
}

/// A highlight in the Tideglass letters with two comments on it.
fn commented_highlight(shelf: &[Book]) -> Vec<EbookAction> {
    let Some(book) = shelf.get(2) else {
        return Vec::new();
    };
    let quote = "the trick of pouring colour into a pane";
    let (block, start) = (2, 200);
    let end = start + quote.chars().count();
    let mut actions = vec![
        HighlightPlan {
            book_id: book.id.clone(),
            group: COMMENTED_HIGHLIGHT.to_string(),
            chapter: 0,
            color: HiColor::Yellow.as_str().into(),
            remove_groups: Vec::new(),
            pieces: vec![HiPiece {
                id: staged_id(40).to_string(),
                block,
                start,
                end,
                excerpt: quote.into(),
            }],
        }
        .into_action(),
    ];
    for (index, text) in [
        "Love this image — colour that keeps the time of day.",
        "Compare with the amber/ebb line later in the letter.",
    ]
    .into_iter()
    .enumerate()
    {
        let id = staged_id(41 + index as u128);
        actions.push(
            CommentDraft {
                group: COMMENTED_HIGHLIGHT,
                note: NoteDraft {
                    book_id: book.id.clone(),
                    id,
                    anchor: anchor(0, block, start, end, quote.into()),
                    color: HiColor::Yellow,
                    text: text.into(),
                },
            }
            .into_action(),
        );
        actions.push(finalize_annotation_action(id, STAGED_AT + index as u64));
    }
    actions
}

/// Thirty notes down the first book, one of them the target of a jump.
fn long_annotations(shelf: &[Book]) -> Vec<EbookAction> {
    let Some(book) = shelf.first() else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for index in 0..30_usize {
        let id = long_note(index as u128);
        let color = HiColor::ALL
            .get(index % HiColor::ALL.len())
            .copied()
            .unwrap_or(HiColor::Yellow);
        let text = if index == 25 {
            "Targeted annotation — revealed here after a jump from search.".into()
        } else {
            format!("Reading note {} — a durable, shared thought.", index + 1)
        };
        actions.push(
            NoteDraft {
                book_id: book.id.clone(),
                id,
                anchor: anchor(
                    0,
                    index,
                    0,
                    0,
                    format!(
                        "Saved passage {} from the lantern keeper's journal",
                        index + 1
                    ),
                ),
                color,
                text,
            }
            .into_action(),
        );
        actions.push(finalize_annotation_action(id, STAGED_AT + index as u64));
    }
    actions
}
