//! Screenshot scenarios: the sample shelf a capture reads its books from,
//! and the library each scenario stages through Books' own actions.
//!
//! What a scenario shows splits in three. The books' text is the sample
//! shelf, a [`Fixture`] value the reader reads instead of files. The shelf
//! rows, reading settings, fonts, schemes, progress and annotations are the
//! library's, staged by [`actions`] as field edits through the same door as
//! every control, under ids and times fixed so every capture is the same.
//! Where the reader is — the view, the open book, a selection, an open panel
//! — is the reader's own view state, staged by `BooksApp::stage`.

use std::path::PathBuf;

use tilo_document::RichText;
use ui::framework::Fixture;
use ui::tilo_resource::{Edit, Id, Invoke, Map};
use uuid::Uuid;

use crate::content;
use crate::model::{
    AnnoKind, Book, ColorScheme, FontEntry, FontKind, HiColor, LibraryEntry, Mark, Progress,
    ReadMode, ReadTheme, Settings, default_fonts, default_schemes,
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
    "search",
    "search-in-book",
    "search-elsewhere",
    "fresh-device",
    "fresh-device-waiting",
    "fresh-device-unavailable",
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
    if fixture.scenario() == Some("search-elsewhere") {
        // Another device indexed it; the index reached this one.
        let _ = fixture.content().put(&elsewhere_segment());
    }
}

/// What stands for the bytes of [`elsewhere_book`]: its id and its row's
/// content hash are theirs.
const ELSEWHERE_TEXT: &[u8] = b"The Glassmaker's Apprentice";

/// The book of "search-elsewhere" whose text is on another device: on the
/// shelf, its index here, its text not.
fn elsewhere_book() -> Book {
    use crate::model::{Block, Chapter, Span};
    let chapter = |title: &str, paragraphs: &[&str]| Chapter {
        title: title.to_owned(),
        blocks: paragraphs
            .iter()
            .map(|text| Block::Paragraph {
                spans: vec![Span::plain(*text)],
            })
            .collect(),
    };
    Book {
        id: crate::epub::content_id(ELSEWHERE_TEXT),
        title: "The Glassmaker's Apprentice".into(),
        author: "Mara Quill".into(),
        chapters: vec![
            chapter(
                "I. The Furnace",
                &[
                    "The furnace never slept, and neither, it seemed, did old Tamsin.",
                    "She turned the pipe and the glass swelled like a held breath.",
                ],
            ),
            chapter(
                "II. Sand and Ash",
                &[
                    "Every pane in the town had passed through her hands.",
                    "He learned that glass remembers every hurry and every doubt.",
                ],
            ),
            chapter(
                "III. The Lens",
                &["The lens she ground that winter brought the far hills near."],
            ),
        ],
        ..Book::default()
    }
}

/// The index of [`elsewhere_book`], as the device holding its text built it.
fn elsewhere_segment() -> Vec<u8> {
    crate::library_index::Segment::build(&elsewhere_book()).encode()
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

/// The library `scenario` shows, as the edits that stage it.
pub(crate) fn actions(scenario: Option<&str>) -> Vec<Invoke<Ebook>> {
    let scenario = scenario.unwrap_or("read");
    if scenario == "onboarding" {
        return Vec::new();
    }
    if scenario.starts_with("fresh-device") {
        return fresh_device_library();
    }
    let shelf = content::load_sample_library();
    let paths = Ebook::paths();
    let mut edits: Vec<Edit<Ebook>> = shelf
        .iter()
        .enumerate()
        .map(|(index, book)| {
            paths.entries.insert_as(
                &Id::from_raw(book.id.clone()),
                index,
                &LibraryEntry {
                    id: book.id.clone(),
                    shared: false,
                    title: book.title.clone(),
                    author: book.author.clone(),
                    subject: book.subject.clone(),
                    added: STAGED_AT + index as u64,
                    content: ui::tilo_resource::Bytes::Empty,
                    index: ui::tilo_resource::Bytes::Empty,
                },
            )
        })
        .collect();
    let mut marks = Vec::new();
    if lived_in(scenario) {
        edits.push(paths.progress.set(lived_in_progress(&shelf)));
        marks.extend(lived_in_shelf(&shelf));
    }
    if let Some(settings) = settings(scenario) {
        edits.extend(paths.settings.path().edits(&Settings::default(), &settings));
    }
    match scenario {
        "settings-colours" => {
            if let Some(first) = default_schemes().first() {
                edits.push(
                    paths
                        .schemes
                        .item(&Id::from_raw(first.id.clone()))
                        .enabled
                        .set(false),
                );
            }
            edits.push(new_scheme(&custom_scheme([0x15, 0x1A, 0x24])));
        }
        "scheme-editor" => edits.push(new_scheme(&custom_scheme([0x15, 0x1A, 0x24]))),
        "picker" => edits.push(new_scheme(&custom_scheme(PICKED_PAGE))),
        "settings-fonts" | "settings-fonts-rename" => {
            edits.push(paths.fonts.insert_as(
                &Id::from_raw(ADDED_FONT),
                default_fonts().len(),
                &FontEntry::new(
                    ADDED_FONT,
                    FontKind::Named("DejaVu Serif".into()),
                    "DejaVu Serif",
                    1.08,
                    true,
                ),
            ));
        }
        "note" => marks.extend(composed_note(&shelf)),
        "comments" => marks.extend(commented_highlight(&shelf)),
        "long-annotations" => marks.extend(long_annotations(&shelf)),
        _ => {}
    }
    edits.extend(
        marks
            .iter()
            .enumerate()
            .map(|(at, mark)| paths.annotations.insert_as(&mark.key(), at, mark)),
    );
    let mut invokes: Vec<Invoke<Ebook>> = edits
        .iter()
        .filter_map(|edit| Invoke::edit(edit).ok())
        .collect();
    if scenario == "search-elsewhere" {
        invokes.extend(elsewhere_shelved());
    }
    invokes
}

/// The books of "fresh-device": a library synced from other devices, none
/// of whose text is here yet, each its title, author and how far it is read.
const FRESH_DEVICE_BOOKS: &[(&str, &str, f32)] = &[
    ("Pride and Prejudice", "Jane Austen", 0.42),
    ("Moby Dick", "Herman Melville", 0.0),
    ("Middlemarch", "George Eliot", 0.18),
    (
        "The Adventures of Sherlock Holmes",
        "Arthur Conan Doyle",
        1.0,
    ),
    ("Frankenstein", "Mary Shelley", 0.0),
    ("War and Peace", "Leo Tolstoy", 0.07),
    ("Great Expectations", "Charles Dickens", 0.0),
    ("Dracula", "Bram Stoker", 0.63),
];

/// A fresh device's library: the rows and reading positions of
/// [`FRESH_DEVICE_BOOKS`], synced; their text is on other devices.
fn fresh_device_library() -> Vec<Invoke<Ebook>> {
    let mut progress: Vec<(String, Progress)> = Vec::new();
    let mut invokes = Vec::new();
    for (index, (title, author, fraction)) in FRESH_DEVICE_BOOKS.iter().enumerate() {
        let text = format!("{title} by {author}");
        let entry = LibraryEntry {
            id: crate::epub::content_id(text.as_bytes()),
            shared: false,
            title: (*title).to_owned(),
            author: (*author).to_owned(),
            subject: String::new(),
            added: STAGED_AT + index as u64,
            content: LibraryEntry::stored(
                &tilo_content::hash_bytes(text.as_bytes()),
                text.len() as u64,
            )
            .unwrap_or_default(),
            index: ui::tilo_resource::Bytes::Empty,
        };
        if *fraction > 0.0 {
            progress.push((
                entry.id.clone(),
                Progress {
                    chapter: 0,
                    block: 0,
                    fraction: *fraction,
                    opened: STAGED_AT - index as u64 * 3_600_000,
                },
            ));
        }
        invokes.push(crate::resource::shelve(&entry));
    }
    let progress: Map<Progress> = progress.into_iter().collect();
    invokes.extend(Invoke::edit(&Ebook::paths().progress.set(progress)).ok());
    invokes
}

/// [`elsewhere_book`] joining the shelf with its index; its text, kept in
/// a content store, is not in this one.
fn elsewhere_shelved() -> Vec<Invoke<Ebook>> {
    use crate::resource::StoredContent;
    let book = elsewhere_book();
    let segment = elsewhere_segment();
    let text = tilo_content::hash_bytes(ELSEWHERE_TEXT);
    let entry = LibraryEntry {
        id: book.id.clone(),
        shared: false,
        title: book.title.clone(),
        author: book.author.clone(),
        subject: String::new(),
        added: STAGED_AT,
        content: LibraryEntry::stored(&text, ELSEWHERE_TEXT.len() as u64).unwrap_or_default(),
        index: ui::tilo_resource::Bytes::Empty,
    };
    vec![
        crate::resource::shelve(&entry),
        crate::resource::index_book(
            &book.id,
            StoredContent {
                hash: tilo_content::hash_bytes(&segment),
                size: u64::try_from(segment.len()).unwrap_or(u64::MAX),
            },
        ),
    ]
}

/// Adding the colour scheme `scheme` after the built-in ones.
fn new_scheme(scheme: &ColorScheme) -> Edit<Ebook> {
    Ebook::paths().schemes.insert_as(
        &Id::from_raw(scheme.id.clone()),
        default_schemes().len(),
        scheme,
    )
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
        "large-text" => settings.font_scale = 2.0,
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

/// Progress across the shelf: a couple of books finished, each opened an
/// hour before the next.
fn lived_in_progress(shelf: &[Book]) -> Map<Progress> {
    const FRACTIONS: [f32; 5] = [0.34, 0.72, 0.08, 0.5, 1.0];
    shelf
        .iter()
        .enumerate()
        .map(|(index, book)| {
            let fraction = FRACTIONS
                .get(index % FRACTIONS.len())
                .copied()
                .unwrap_or(0.5);
            (
                book.id.clone(),
                Progress {
                    chapter: 0,
                    block: (fraction * 6.0) as usize,
                    fraction,
                    opened: STAGED_AT - index as u64 * 3_600_000,
                },
            )
        })
        .collect()
}

/// In the first book, a bookmark, two highlights and a note.
fn lived_in_shelf(shelf: &[Book]) -> Vec<Mark> {
    let Some(book) = shelf.first() else {
        return Vec::new();
    };
    let mut marks = vec![mark(
        book,
        staged_id(1),
        AnnoKind::Bookmark,
        (0, 0, 0),
        excerpt(book, 0),
    )];
    for (index, (block, end, color)) in [(1, 70, HiColor::Yellow), (3, 58, HiColor::Green)]
        .into_iter()
        .enumerate()
    {
        let index = index as u128;
        marks.push(Mark {
            color,
            group: staged_id(10 + index),
            ..mark(
                book,
                staged_id(20 + index),
                AnnoKind::Highlight,
                (block, 0, word_end(book, block, end)),
                excerpt(book, block),
            )
        });
    }
    marks.push(Mark {
        color: HiColor::Blue,
        text: RichText::from_markdown(
            "The blue and the amber meeting in the middle — keep this image for the last pane.",
        ),
        ..mark(
            book,
            staged_id(30),
            AnnoKind::Note,
            (2, 0, 0),
            excerpt(book, 2),
        )
    });
    marks
}

/// An annotation `id` of `kind` on chapter 0 of `book` at `(block, start,
/// end)`, made when the scenes were written.
fn mark(
    book: &Book,
    id: Uuid,
    kind: AnnoKind,
    (block, start, end): (usize, usize, usize),
    excerpt: String,
) -> Mark {
    Mark {
        id,
        doc_id: book.id.clone(),
        kind,
        chapter: 0,
        block,
        start,
        end,
        excerpt,
        text: RichText::default(),
        color: HiColor::Yellow,
        created: STAGED_AT,
        group: id,
    }
}

/// The note the "note" scenario is writing, on the selected passage.
fn composed_note(shelf: &[Book]) -> Vec<Mark> {
    let Some(book) = shelf.first() else {
        return Vec::new();
    };
    vec![Mark {
        color: HiColor::Blue,
        text: RichText::from_markdown(
            "A note, written right here on the page — no separate screen.",
        ),
        ..mark(
            book,
            COMPOSED_NOTE,
            AnnoKind::Note,
            (1, 0, 0),
            "the keeper climbed the iron stair and set the great wick alight".into(),
        )
    }]
}

/// A highlight in the Tideglass letters with two comments on it.
fn commented_highlight(shelf: &[Book]) -> Vec<Mark> {
    let Some(book) = shelf.get(2) else {
        return Vec::new();
    };
    let quote = "the trick of pouring colour into a pane";
    let (block, start) = (2, 200);
    let end = start + quote.chars().count();
    let mut marks = vec![Mark {
        group: COMMENTED_HIGHLIGHT,
        ..mark(
            book,
            staged_id(40),
            AnnoKind::Highlight,
            (block, start, end),
            quote.into(),
        )
    }];
    for (index, text) in [
        "Love this image — colour that keeps the time of day.",
        "Compare with the amber/ebb line later in the letter.",
    ]
    .into_iter()
    .enumerate()
    {
        marks.push(Mark {
            group: COMMENTED_HIGHLIGHT,
            text: RichText::from_markdown(text),
            created: STAGED_AT + index as u64,
            ..mark(
                book,
                staged_id(41 + index as u128),
                AnnoKind::Note,
                (block, start, end),
                quote.into(),
            )
        });
    }
    marks
}

/// Thirty notes down the first book, one of them the target of a jump.
fn long_annotations(shelf: &[Book]) -> Vec<Mark> {
    let Some(book) = shelf.first() else {
        return Vec::new();
    };
    (0..30_usize)
        .map(|index| {
            let color = HiColor::ALL
                .get(index % HiColor::ALL.len())
                .copied()
                .unwrap_or(HiColor::Yellow);
            let text = if index == 25 {
                "Targeted annotation — revealed here after a jump from search.".into()
            } else {
                format!("Reading note {} — a durable, shared thought.", index + 1)
            };
            Mark {
                color,
                text: RichText::from_markdown(text),
                created: STAGED_AT + index as u64,
                ..mark(
                    book,
                    long_note(index as u128),
                    AnnoKind::Note,
                    (index, 0, 0),
                    format!(
                        "Saved passage {} from the lantern keeper's journal",
                        index + 1
                    ),
                )
            }
        })
        .collect()
}
