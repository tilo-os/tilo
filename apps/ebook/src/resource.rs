//! Books' library: the record every device of a reader shares, its verbs,
//! the places derived from it, and the app.
//!
//! The shelf, where the reader is in each book, the reading settings, the
//! fonts and colour schemes, and every bookmark, highlight, note and comment
//! are fields of [`Ebook`]. Plain fields change through the built-in field
//! edits their paths offer (the text size, the sort order, a scheme's colour,
//! a note's text); the methods below are what is more than setting a field.
//! Each book is published as a place offering what acts on it and on its
//! annotations. What the reader shows (the open book, its page, a selection)
//! is the reader's own view state and its [`BooksEffect`]s, never the
//! document's.

use std::rc::Rc;

use serde::{Deserialize, Serialize};
use ui::framework::{
    EffectAnswer, EffectCx, Effected, Fixture, LaunchInput, LiveRow, ResourceApp, ResourceSurface,
    SeedCx, SurfaceCx, ViewCx,
};
use ui::i18n::EmbeddedCatalog;
use ui::prelude::Effect;
use ui::tilo_resource::{
    self, ActionCx, ActionDecl, Atom, Collection, Derived, Id, Invoke, JsonSchema, Map, Model,
    Refusal, resource,
};
use ui::widget::Element;
use uuid::Uuid;

use crate::app::BooksApp;
use crate::component::EPUB_MEDIA_TYPE;
use crate::model::{
    AnnoKind, Annotation, ColorScheme, FontEntry, HiColor, LibraryEntry, LibraryPrefs, Mark,
    Progress, Settings, annotation_key, default_fonts, default_schemes,
};

ui::embedded_catalog!(static CATALOG, ["en-US"]);

/// How many books the published library carries as places; the rest are
/// resolved one by one when addressed.
const PUBLISHED_BOOKS: usize = 64;

/// The reader's library: what they read, where they are in it, how they
/// like it set, and what they marked in it.
#[derive(Model, Clone, Debug, PartialEq)]
pub struct Ebook {
    /// The books on the shelf, each under its content id, in the order they
    /// were added.
    pub entries: Collection<LibraryEntry>,
    /// Where the reader left off in each book, by its content id. Kept when
    /// a book leaves the shelf, so adding it again resumes there. One entry
    /// per book ever read: a page turn copies its chunk, not the library's.
    pub progress: Map<Progress>,
    /// Reading settings
    pub settings: Settings,
    /// The font used where none is chosen: empty for the first one.
    pub default_font: Atom,
    /// How the shelf is sorted and filtered
    pub prefs: LibraryPrefs,
    /// The reading fonts, in the order the pickers offer them.
    pub fonts: Collection<FontEntry>,
    /// The colour schemes, in the order the pickers offer them.
    pub schemes: Collection<ColorScheme>,
    /// Every bookmark, highlight, note and comment, each under its id.
    #[model(unit)]
    pub annotations: Collection<Mark>,
}

impl Default for Ebook {
    /// A new library: an empty shelf with the built-in fonts and schemes.
    fn default() -> Self {
        let mut library = Self {
            entries: Collection::new(),
            progress: Map::new(),
            settings: Settings::default(),
            default_font: Atom::default(),
            prefs: LibraryPrefs::default(),
            fonts: Collection::new(),
            schemes: Collection::new(),
            annotations: Collection::new(),
        };
        for font in default_fonts() {
            library.fonts.push(Id::from_raw(font.id.clone()), font);
        }
        for scheme in default_schemes() {
            library
                .schemes
                .push(Id::from_raw(scheme.id.clone()), scheme);
        }
        library
    }
}

/// A passage of a book: the characters of `block` of `chapter` from
/// `start` to `end`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Passage {
    /// The book's id
    pub book: String,
    /// Chapter, counted from 0
    pub chapter: usize,
    /// Paragraph, counted from 0 in the chapter
    pub block: usize,
    /// First character of the passage
    pub start: usize,
    /// Character after the passage
    pub end: usize,
    /// The passage's text
    pub excerpt: String,
}

/// Bytes a content store keeps for a book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StoredContent {
    /// Their content hash
    pub hash: String,
    /// How many bytes
    pub size: u64,
}

/// One paragraph's share of a highlight: the characters of `block` from
/// `start` to `end`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HiPiece {
    /// Paragraph, counted from 0 in the chapter
    pub block: usize,
    /// First character highlighted
    pub start: usize,
    /// Character after the last one highlighted
    pub end: usize,
    /// The highlighted text
    pub excerpt: String,
}

#[resource(
    kind = "ebook.library",
    label = "Books",
    params = books,
    children = Ebook::places,
    search = Ebook::search,
)]
impl Ebook {
    /// Put a book on the shelf, or refresh the row it has.
    #[action]
    pub fn shelve(
        &mut self,
        cx: &mut ActionCx,
        #[param("The book's id: book: and the start of its content hash")] book: String,
        #[param("Title")] title: String,
        #[param("Author")] author: String,
        #[param("Subject")] subject: String,
        #[param("Whether a copy is in the shared workspace")] shared: bool,
        #[param("The book's bytes, when a content store keeps them")] content: Option<
            StoredContent,
        >,
    ) -> Result<(), Refusal> {
        let content = match content {
            Some(stored) => Some(
                LibraryEntry::stored(&stored.hash, stored.size)
                    .ok_or("the content hash is not a content hash")?,
            ),
            None => None,
        };
        self.put_on_shelf(cx, book, [title, author, subject], shared, content);
        Ok(())
    }

    /// A book found in a folder the reader watches joins the shelf: content
    /// from outside, which syncs like any row but is nobody's undo step.
    #[action(external)]
    pub fn shelve_found(
        &mut self,
        cx: &mut ActionCx,
        #[param("The book's id: book: and the start of its content hash")] book: String,
        #[param("Title")] title: String,
        #[param("Author")] author: String,
        #[param("Subject")] subject: String,
    ) {
        self.put_on_shelf(cx, book, [title, author, subject], false, None);
    }

    /// Remove from library
    ///
    /// Takes the book off the shelf. Where the reader was and what they
    /// marked in it stay, so adding it again brings them back. Undo puts the
    /// row back but not this device's access to the book's file, which the
    /// reader forgets with it, so this is asked first.
    #[action(destructive, irreversible, writes = annotations)]
    pub fn remove_from_library(&mut self, #[param("The book's id")] book: String) {
        self.entries.remove(&Id::from_raw(book));
    }

    /// Keep the index of a book's text with its row, so that every device
    /// finds a passage in the book before the book itself is there.
    #[action(housekeeping)]
    pub fn index_book(
        &mut self,
        #[param("The book's id")] book: String,
        #[param("The index of its text, as a content store keeps it")] index: StoredContent,
    ) -> Result<(), Refusal> {
        let index = LibraryEntry::stored(&index.hash, index.size)
            .ok_or("the index hash is not a content hash")?;
        let entry = self
            .entries
            .get_mut(&Id::from_raw(book))
            .ok_or("the book is not in the library")?;
        entry.index = index;
        Ok(())
    }

    /// Record where the reader is in a book.
    #[action(housekeeping)]
    pub fn record_progress(
        &mut self,
        cx: &mut ActionCx,
        #[param("The book's id")] book: String,
        #[param("Chapter, counted from 0")] chapter: usize,
        #[param("First paragraph on screen, counted from 0 in the chapter")] block: usize,
        #[param("How far through the whole book, from 0 to 1")] fraction: f32,
        #[param("Whether the book was just opened")] opened: bool,
    ) -> Result<(), Refusal> {
        if !fraction.is_finite() {
            return Err(Refusal::new("progress is a fraction from 0 to 1"));
        }
        let mut progress = self.progress_of(&book);
        progress.chapter = chapter;
        progress.block = block;
        progress.fraction = fraction.clamp(0.0, 1.0);
        if opened {
            progress.opened = u64::try_from(cx.now_ms()).unwrap_or_default();
        }
        self.progress.insert(book, progress);
        Ok(())
    }

    /// Add note
    ///
    /// Adds a note to a passage of a book.
    #[action(writes = annotations)]
    pub fn add_note(
        &mut self,
        cx: &mut ActionCx,
        #[param("The passage")] passage: Passage,
        #[param("Colour")] color: HiColor,
        #[param("The note's text")] text: String,
    ) {
        let id = cx.uuid();
        self.annotate(Mark {
            id,
            doc_id: passage.book,
            kind: AnnoKind::Note,
            chapter: passage.chapter,
            block: passage.block,
            start: passage.start,
            end: passage.end,
            excerpt: passage.excerpt,
            text: tilo_document::RichText::from_markdown(text),
            color,
            created: now(cx),
            group: id,
        });
    }

    /// Add comment
    ///
    /// Comments on a highlight.
    #[action(writes = annotations)]
    pub fn add_comment(
        &mut self,
        cx: &mut ActionCx,
        #[param("The highlight: the id of one of its pieces or its group")] highlight: Uuid,
        #[param("The comment")] text: String,
    ) -> Result<(), Refusal> {
        let group = self.highlight_group(highlight)?;
        let piece = self
            .highlight_pieces(group)
            .min_by_key(|piece| (piece.block, piece.start))
            .cloned()
            .ok_or("highlight is no longer available")?;
        let id = cx.uuid();
        self.annotate(Mark {
            id,
            kind: AnnoKind::Note,
            text: tilo_document::RichText::from_markdown(text),
            created: now(cx),
            group,
            ..piece
        });
        Ok(())
    }

    /// Toggle bookmark
    ///
    /// Bookmarks a paragraph, or removes its bookmark.
    #[action(writes = annotations)]
    pub fn toggle_bookmark(
        &mut self,
        cx: &mut ActionCx,
        #[param("The book's id")] book: String,
        #[param("Chapter, counted from 0")] chapter: usize,
        #[param("Paragraph, counted from 0 in the chapter")] block: usize,
        #[param("The paragraph's opening words")] excerpt: String,
    ) {
        let marks: Vec<Id> = self
            .annotations
            .iter()
            .filter(|(_, mark)| {
                mark.kind == AnnoKind::Bookmark
                    && mark.doc_id == book
                    && mark.chapter == chapter
                    && mark.block == block
            })
            .map(|(id, _)| id.clone())
            .collect();
        if !marks.is_empty() {
            for id in &marks {
                self.annotations.remove(id);
            }
            return;
        }
        let id = cx.uuid();
        self.annotate(Mark {
            id,
            doc_id: book,
            kind: AnnoKind::Bookmark,
            chapter,
            block,
            excerpt,
            created: now(cx),
            group: id,
            ..Mark::default()
        });
    }

    /// Highlight
    ///
    /// Highlights a passage, one piece per paragraph, replacing the
    /// highlights it overlaps; with no pieces it only removes them.
    #[action(writes = annotations)]
    pub fn highlight(
        &mut self,
        cx: &mut ActionCx,
        #[param("The book's id")] book: String,
        #[param("Chapter, counted from 0")] chapter: usize,
        #[param("Colour")] color: HiColor,
        #[param("Highlights the new one replaces, by their groups")] replacing: Vec<Uuid>,
        #[param("The paragraphs' pieces of the passage")] pieces: Vec<HiPiece>,
    ) -> Result<(), Refusal> {
        if pieces.iter().any(|piece| piece.start >= piece.end) {
            return Err(Refusal::new("a highlighted piece holds some text"));
        }
        for group in replacing {
            self.remove_group(group);
        }
        if pieces.is_empty() {
            return Ok(());
        }
        let group = cx.uuid();
        let created = now(cx);
        for piece in pieces {
            self.annotate(Mark {
                id: cx.uuid(),
                doc_id: book.clone(),
                kind: AnnoKind::Highlight,
                chapter,
                block: piece.block,
                start: piece.start,
                end: piece.end,
                excerpt: piece.excerpt,
                color,
                created,
                group,
                ..Mark::default()
            });
        }
        Ok(())
    }

    /// Change colour
    ///
    /// Recolours a highlight, its pieces and its comments together.
    #[action(writes = annotations)]
    pub fn change_color(
        &mut self,
        #[param("The highlight: the id of one of its pieces or its group")] highlight: Uuid,
        #[param("Colour")] color: HiColor,
    ) -> Result<(), Refusal> {
        let group = self.highlight_group(highlight)?;
        for (_, mark) in self.annotations.iter_mut() {
            if mark.group == group {
                mark.color = color;
            }
        }
        Ok(())
    }

    /// Delete highlight
    ///
    /// Deletes a highlight with every comment on it.
    #[action(writes = annotations)]
    pub fn delete_highlight(
        &mut self,
        #[param("The highlight: the id of one of its pieces or its group")] highlight: Uuid,
    ) -> Result<(), Refusal> {
        let group = self.highlight_group(highlight)?;
        self.remove_group(group);
        Ok(())
    }

    /// Delete
    ///
    /// Deletes a bookmark, a note or a comment.
    #[action(writes = annotations)]
    pub fn delete_annotation(
        &mut self,
        #[param("The bookmark's, note's or comment's id")] annotation: Uuid,
    ) -> Result<(), Refusal> {
        self.annotations
            .remove(&annotation_key(annotation))
            .map(drop)
            .ok_or_else(|| Refusal::new("annotation is no longer available"))
    }

    /// Remove font
    #[action(destructive)]
    pub fn remove_font(&mut self, #[param("The font's id")] font: String) -> Result<(), Refusal> {
        self.fonts
            .remove(&Id::from_raw(font.clone()))
            .ok_or("no such font")?;
        if self.settings.font_id == font {
            self.settings.font_id.clear();
        }
        if self.default_font.0 == font {
            self.default_font = Atom::default();
        }
        Ok(())
    }

    /// Delete scheme
    #[action(destructive, writes = annotations)]
    pub fn delete_scheme(
        &mut self,
        #[param("The colour scheme's id")] scheme: String,
    ) -> Result<(), Refusal> {
        let key = Id::from_raw(scheme.clone());
        if self
            .schemes
            .get(&key)
            .ok_or("no such colour scheme")?
            .builtin
        {
            return Err(Refusal::new(
                "a built-in scheme can be turned off, not deleted",
            ));
        }
        self.schemes.remove(&key);
        if self.settings.scheme_id == scheme {
            self.settings.scheme_id.clear();
        }
        Ok(())
    }
}

/// When an action happened, in ms since the epoch, as its author's clock
/// read it.
fn now(cx: &ActionCx) -> u64 {
    u64::try_from(cx.now_ms()).unwrap_or_default()
}

impl Ebook {
    /// Put the book `book` on the shelf: a new row added now, or the row it
    /// has refreshed, keeping when it was first added.
    fn put_on_shelf(
        &mut self,
        cx: &ActionCx,
        book: String,
        [title, author, subject]: [String; 3],
        shared: bool,
        content: Option<tilo_resource::Bytes>,
    ) {
        let key = Id::from_raw(book.clone());
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.title = title;
            entry.author = author;
            entry.subject = subject;
            entry.shared |= shared;
            if let Some(content) = content {
                entry.content = content;
            }
            return;
        }
        self.entries.push(
            key,
            LibraryEntry {
                id: book,
                shared,
                title,
                author,
                subject,
                added: now(cx),
                content: content.unwrap_or_default(),
                index: ui::tilo_resource::Bytes::Empty,
            },
        );
    }

    fn annotate(&mut self, annotation: Mark) {
        self.annotations.push(annotation.key(), annotation);
    }

    /// The group of the highlight `id` names: one of its pieces, or the
    /// group itself.
    fn highlight_group(&self, id: Uuid) -> Result<Uuid, Refusal> {
        let group = self
            .annotations
            .get(&annotation_key(id))
            .map_or(id, |mark| mark.group);
        if self.highlight_pieces(group).next().is_some() {
            Ok(group)
        } else {
            Err(Refusal::new("highlight is no longer available"))
        }
    }

    fn highlight_pieces(&self, group: Uuid) -> impl Iterator<Item = &Mark> {
        self.annotations
            .values()
            .filter(move |mark| mark.kind == AnnoKind::Highlight && mark.group == group)
    }

    /// Remove every row of `group`: a highlight's pieces with its comments.
    fn remove_group(&mut self, group: Uuid) {
        self.annotations.retain(|_, mark| mark.group != group);
    }

    /// The book `id` on the shelf.
    pub fn entry(&self, id: &str) -> Option<&LibraryEntry> {
        self.entries.get(&Id::from_raw(id))
    }

    /// Where the reader left off in the book `id`.
    pub fn progress_of(&self, id: &str) -> Progress {
        self.progress.get(id).copied().unwrap_or_default()
    }

    /// The annotations the reader shows: every one but a comment whose
    /// highlight was deleted while it was written, as deleting a highlight
    /// deletes its comments.
    pub fn live_marks(&self) -> impl Iterator<Item = &Mark> {
        self.annotations.values().filter(|mark| {
            mark.kind != AnnoKind::Note
                || mark.group == mark.id
                || self.highlight_pieces(mark.group).next().is_some()
        })
    }

    /// Every annotation the reader shows, as it draws and searches them.
    pub fn marks(&self) -> Vec<Annotation> {
        self.live_marks().map(Mark::annotation).collect()
    }

    /// The annotations of the book `id`, by place, then by when they were
    /// made.
    pub fn annotations_for(&self, id: &str) -> Vec<Annotation> {
        let mut marks: Vec<Annotation> = self
            .live_marks()
            .filter(|mark| mark.doc_id == id)
            .map(Mark::annotation)
            .collect();
        marks.sort_by(|a, b| {
            (a.chapter, a.block, a.start, a.created).cmp(&(b.chapter, b.block, b.start, b.created))
        });
        marks
    }

    /// The shelved book last opened (the latest added when none was), which
    /// a bundled library shows again, so another mount of the library reads
    /// the same book.
    pub fn most_recently_opened(&self) -> Option<String> {
        self.entries
            .values()
            .enumerate()
            .max_by_key(|(order, entry)| (self.progress_of(&entry.id).opened, *order))
            .map(|(_, entry)| entry.id.clone())
    }

    /// Books and their annotations are indexed by the reader's search, with
    /// the text of the books they belong to, which the library does not hold.
    #[allow(
        clippy::unused_self,
        reason = "the library's search hook reads the record; the reader's search covers it"
    )]
    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        Vec::new()
    }

    /// Each book on the shelf as a place, its annotations beneath it, and
    /// the reading settings with the fonts and colour schemes.
    fn places(&self) -> Vec<Derived<Self>> {
        let mut places: Vec<Derived<Self>> = self
            .entries
            .values()
            .take(PUBLISHED_BOOKS)
            .map(|entry| {
                let marks: Vec<Annotation> = self
                    .live_marks()
                    .filter(|mark| mark.doc_id == entry.id)
                    .map(Mark::annotation)
                    .collect();
                Self::book_place(entry, &marks.iter().collect::<Vec<_>>())
            })
            .collect();
        places.push(self.settings_place());
        places
    }

    /// The reading settings as a place: opening them, with each font and
    /// colour scheme beneath, offering what removes it.
    fn settings_place(&self) -> Derived<Self> {
        let mut place = Derived::new("Reading settings")
            .of_kind("settings")
            .identified("settings")
            .with_effect(&BooksEffect::Settings);
        for font in self.fonts.values() {
            place = place.with_child(
                Derived::new(font.name.clone())
                    .of_kind("font")
                    .identified(format!("font:{}", font.id))
                    .with(Self::REMOVE_FONT.bind(books::RemoveFont::paths().font, font.id.clone())),
            );
        }
        for scheme in self.schemes.values() {
            let mut child = Derived::new(scheme.name.clone())
                .of_kind("color-scheme")
                .identified(format!("scheme:{}", scheme.id));
            if !scheme.builtin {
                child = child.with(
                    Self::DELETE_SCHEME
                        .bind(books::DeleteScheme::paths().scheme, scheme.id.clone()),
                );
            }
            place = place.with_child(child);
        }
        place
    }

    /// The book `entry` as a place: opening it, sharing or fetching its
    /// bytes, taking it off the shelf, with `marks` (its annotations)
    /// beneath it.
    pub(crate) fn book_place(entry: &LibraryEntry, marks: &[&Annotation]) -> Derived<Self> {
        let book = entry.id.clone();
        let lifecycle = if entry.shared && entry.content.is_empty() {
            BooksEffect::Download { book: book.clone() }
        } else {
            BooksEffect::Share { book: book.clone() }
        };
        let mut place = Derived::new(entry.title.clone())
            .of_kind("book")
            .identified(book_place_id(&book))
            .with_effect(&BooksEffect::Open {
                book: book.clone(),
                chapter: 0,
                page: 0,
                block: None,
            })
            .with_effect(&lifecycle)
            // Reading a chapter of this book, asking which.
            .with_effect_asking(
                &BooksEffect::ReadChapter {
                    book: book.clone(),
                    chapter: Some(0),
                },
                &["chapter"],
            )
            // Going to and selecting a passage of this book, asking where.
            .with_effect_asking(
                &BooksEffect::SelectPassage {
                    book: book.clone(),
                    chapter: Some(0),
                    start_block: 0,
                    start_char: None,
                    end_block: None,
                    end_char: None,
                },
                &[
                    "chapter",
                    "start_block",
                    "start_char",
                    "end_block",
                    "end_char",
                ],
            )
            .with_effect_asking(
                &BooksEffect::GoTo {
                    book: book.clone(),
                    chapter: Some(0),
                    block: None,
                },
                &["chapter", "block"],
            )
            .with(Self::REMOVE_FROM_LIBRARY.bind(books::RemoveFromLibrary::paths().book, book));
        let highlighted: std::collections::HashSet<Uuid> = marks
            .iter()
            .filter(|mark| mark.kind == AnnoKind::Highlight)
            .map(|mark| mark.group)
            .collect();
        let comment = |mark: &Annotation| is_comment(mark, &highlighted);
        let mut highlights: Vec<Uuid> = Vec::new();
        for mark in marks {
            match mark.kind {
                AnnoKind::Highlight => {
                    if highlights.contains(&mark.group) {
                        continue;
                    }
                    highlights.push(mark.group);
                    let pieces: Vec<&Annotation> = marks
                        .iter()
                        .copied()
                        .filter(|piece| {
                            piece.kind == AnnoKind::Highlight && piece.group == mark.group
                        })
                        .collect();
                    let comments: Vec<&Annotation> = marks
                        .iter()
                        .copied()
                        .filter(|note| comment(note) && note.group == mark.group)
                        .collect();
                    place = place.with_child(Self::highlight_place(&pieces, &comments));
                }
                AnnoKind::Note if comment(mark) => {}
                AnnoKind::Note | AnnoKind::Bookmark => {
                    place = place.with_child(Self::mark_place(mark, false));
                }
            }
        }
        place
    }

    /// A highlight made of `pieces`, with `comments` beneath it.
    fn highlight_place(pieces: &[&Annotation], comments: &[&Annotation]) -> Derived<Self> {
        let Some(first) = pieces.first() else {
            return Derived::new(String::new());
        };
        let label = pieces
            .iter()
            .map(|piece| mark_label(piece))
            .collect::<Vec<_>>()
            .join(" ");
        let group = first.group;
        let mut place = Derived::new(label)
            .of_kind("highlight")
            .identified(mark_place_id(AnnoKind::Highlight, false, group))
            .with_effect(&open_at(first))
            .with(Self::CHANGE_COLOR.bind(books::ChangeColor::paths().highlight, group))
            .with(Self::DELETE_HIGHLIGHT.bind(books::DeleteHighlight::paths().highlight, group))
            .with(Self::ADD_COMMENT.bind(books::AddComment::paths().highlight, group));
        for comment in comments {
            place = place.with_child(Self::mark_place(comment, true));
        }
        place
    }

    /// A bookmark, a note or a comment as a place.
    fn mark_place(mark: &Annotation, comment: bool) -> Derived<Self> {
        let kind = match mark.kind {
            AnnoKind::Bookmark => "bookmark",
            _ if comment => "comment",
            _ => "note",
        };
        Derived::new(mark_label(mark))
            .of_kind(kind)
            .identified(mark_place_id(mark.kind, comment, mark.id))
            .with_effect(&open_at(mark))
            .with(
                Self::DELETE_ANNOTATION.bind(books::DeleteAnnotation::paths().annotation, mark.id),
            )
    }
}

/// Where the text of the note or comment `id` is in the library.
pub(crate) fn note_text(id: Uuid) -> tilo_resource::Path<Ebook, tilo_document::RichText> {
    Ebook::paths().annotations.item(&annotation_key(id)).text
}

/// The dispatcher the editor of the note or comment `id` writes through:
/// each of its edits is the library's field edit of the note's text.
pub(crate) fn note_text_dispatcher(
    library: &ui::framework::Dispatcher<Ebook>,
    id: Uuid,
) -> ui::framework::Dispatcher<tilo_document::RichText> {
    let text = note_text(id);
    library.child::<tilo_document::RichText>(
        &tilo_resource::child_resource_id::<Ebook>(&format!("note:{id}")),
        move |op| tilo_resource::Stamped {
            operation: EbookAction::FieldEdit(text.edit(tilo_resource::EditOp::Rich {
                rich: serde_json::to_value(&op).unwrap_or_default(),
            })),
            now_ms: 0,
        },
    )
}

/// How an annotation is named: its note, else the passage it marks, on
/// one line.
pub(crate) fn mark_label(mark: &Annotation) -> String {
    let note = mark.note.clone();
    let text = if note.trim().is_empty() {
        mark.excerpt.trim()
    } else {
        note.trim()
    };
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The passage `mark` sits in, opened.
fn open_at(mark: &Annotation) -> BooksEffect {
    BooksEffect::Open {
        book: mark.doc_id.clone(),
        chapter: mark.chapter,
        page: 0,
        block: Some(mark.block),
    }
}

/// The id of the book `book`'s place among the library's.
pub(crate) fn book_place_id(book: &str) -> String {
    format!("book:{book}")
}

/// The published id of the book `book`'s place.
pub fn book_place_resource(book: &str) -> String {
    format!(
        "{}/derived/{}",
        <Ebook as tilo_resource::Resource>::KIND,
        book_place_id(book)
    )
}

/// Whether the note `mark` is a comment on one of the `highlighted` groups.
pub(crate) fn is_comment(mark: &Annotation, highlighted: &std::collections::HashSet<Uuid>) -> bool {
    mark.kind == AnnoKind::Note && mark.group != mark.id && highlighted.contains(&mark.group)
}

/// The published id of `mark`'s place, beneath its book's; a `comment`'s
/// is beneath its highlight's.
pub(crate) fn mark_place_resource(mark: &Annotation, comment: bool) -> String {
    let book = book_place_resource(&mark.doc_id);
    let highlight = mark_place_id(AnnoKind::Highlight, false, mark.group);
    if comment {
        format!(
            "{book}/{highlight}/{}",
            mark_place_id(mark.kind, true, mark.id)
        )
    } else if mark.kind == AnnoKind::Highlight {
        format!("{book}/{highlight}")
    } else {
        format!("{book}/{}", mark_place_id(mark.kind, false, mark.id))
    }
}

/// The id of an annotation's place, beneath its book's: a highlight by its
/// group, anything else by its own id.
pub(crate) fn mark_place_id(kind: AnnoKind, comment: bool, id: Uuid) -> String {
    let kind = match kind {
        AnnoKind::Highlight => "highlight",
        AnnoKind::Bookmark => "bookmark",
        AnnoKind::Note if comment => "comment",
        AnnoKind::Note => "note",
    };
    format!("{kind}:{id}")
}

/// Books: the reader widget is the surface over the [`Ebook`] library.
pub struct EbookShell;

impl ResourceApp for EbookShell {
    type Resource = Ebook;
    const ID: &'static str = "so.tilo.ebook";
    const TITLE: &'static str = "Books";
    const ICON: Option<&'static str> = Some("ebook");
    const OPENS: &'static [&'static str] = &[EPUB_MEDIA_TYPE];
    /// A book to open (`ebook book.epub`).
    const ARGUMENTS: usize = 1;

    fn commands() -> &'static [ui::framework::Command] {
        crate::commands::COMMANDS
    }

    /// Whether this device keeps the book catalogues: a setting of the
    /// device's, published so search and agents reach it.
    fn records(records: &mut ui::framework::Records<'_>) {
        records.settings::<crate::catalogue::CatalogueCopies>();
    }

    fn view(cx: &mut ViewCx<Ebook>) -> Element {
        let app = BooksApp::new(cx.dispatcher(), bundled_library());
        // A search hit, or an agent's `open`, shows the book, chapter,
        // annotation or match it is anchored at.
        let opens = app.opens();
        cx.on_open(move |anchor: &str| opens.borrow_mut().push(anchor.to_owned()));
        cx.surface(app)
    }

    fn label(state: &Ebook) -> String {
        if state.entries.is_empty() {
            "Books".into()
        } else {
            format!("{} books", state.entries.len())
        }
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(&CATALOG)
    }

    /// A book handed over (opened with Books, named on the command line)
    /// is kept in the content store and opened from there, so the shelf row
    /// names its bytes and every mount of the library reads them.
    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Ebook>> {
        launch_file(input).into_iter().collect()
    }

    /// Where files have no paths (the browser), a new library holds the
    /// book compiled into the app: the one book there is to read.
    fn seed(_cx: &SeedCx<'_>) -> Vec<Invoke<Ebook>> {
        bundled_seed(bundled_library())
    }

    fn scenarios() -> &'static [&'static str] {
        crate::scenarios::SCENARIOS
    }

    fn fixture(fixture: &mut Fixture<Ebook>) {
        crate::scenarios::fixture(fixture);
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Ebook>> {
        crate::scenarios::actions(name)
    }
}

/// Whether the host has no native files, so the one book there is to read is
/// the one compiled into the app: the browser.
pub(crate) fn bundled_library() -> bool {
    !tilo_files::available(tilo_files::Feature::NativePaths)
}

/// What a new library starts with: the bundled book where the library is
/// bundled, else nothing.
pub(crate) fn bundled_seed(bundled: bool) -> Vec<Invoke<Ebook>> {
    if !bundled {
        return Vec::new();
    }
    crate::content::bundled_book_entry()
        .map(|entry| shelve(&entry))
        .into_iter()
        .collect()
}

/// Putting `entry` on the shelf, as the library's verb.
pub(crate) fn shelve(entry: &LibraryEntry) -> Invoke<Ebook> {
    Ebook::SHELVE.with(books::Shelve {
        book: entry.id.clone(),
        title: entry.title.clone(),
        author: entry.author.clone(),
        subject: entry.subject.clone(),
        shared: entry.shared,
        content: entry.content_hash().map(|hash| StoredContent {
            hash,
            size: entry.content.len(),
        }),
    })
}

/// The index of `book`'s text, kept as `stored`, joining its row.
pub(crate) fn index_book(book: &str, stored: StoredContent) -> Invoke<Ebook> {
    Ebook::INDEX_BOOK.with(books::IndexBook {
        book: book.to_owned(),
        index: stored,
    })
}

/// A book found in a watched folder joining the shelf, as the library's verb.
pub(crate) fn shelve_found(entry: &LibraryEntry) -> Invoke<Ebook> {
    Ebook::SHELVE_FOUND.with(books::ShelveFound {
        book: entry.id.clone(),
        title: entry.title.clone(),
        author: entry.author.clone(),
        subject: entry.subject.clone(),
    })
}

/// The launch's book, kept in the launch's content store, as the effect that
/// opens it.
pub(crate) fn launch_file(input: &LaunchInput<'_>) -> Option<Invoke<Ebook>> {
    let file = input.file()?;
    let Some(content) = input.content() else {
        tracing::warn!(
            name = file.name(),
            "ebook: no content store to keep a handed book in"
        );
        return None;
    };
    match content.put(file.bytes()) {
        Ok(content) => Some(Invoke::effect(&BooksEffect::OpenStored {
            content,
            name: file.name().to_owned(),
        })),
        Err(error) => {
            tracing::warn!(%error, name = file.name(), "ebook: a handed book could not be kept");
            None
        }
    }
}

/// What Books does that changes no document by itself: what this reader
/// shows, opening books on this device, and sharing or fetching a book's
/// bytes through the workspace. Effects on one book, chapter or annotation
/// ride its place; going to or selecting a passage asks for the place in the
/// book whose place it rides.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "ebook.effect")]
pub enum BooksEffect {
    /// Open book
    ///
    /// Choose an EPUB through the system picker and open it.
    #[effect(attended, job, cancellable)]
    OpenBook,
    /// Open a book the content store holds, putting it on the shelf.
    #[effect(job)]
    OpenStored {
        #[param("Content hash of the book's bytes")]
        content: String,
        #[param("The book's file name, for messages")]
        name: String,
    },
    /// Previous page
    #[effect(view, row)]
    PreviousPage,
    /// Next page
    #[effect(view, row)]
    NextPage,
    /// Toggle table of contents
    #[effect(view, row)]
    Contents,
    /// Toggle notes
    #[effect(view, row)]
    Notes,
    /// Open reading settings
    #[effect(view, row)]
    Settings,
    /// Open
    ///
    /// Shows a book at a chapter, page and block. A place that the book no
    /// longer has is refused rather than opened somewhere near it.
    #[effect(view, row)]
    Open {
        book: String,
        chapter: usize,
        page: usize,
        block: Option<usize>,
    },
    /// Go to
    ///
    /// Opens the book at a chapter and paragraph without selecting
    /// anything.
    #[effect(view, row)]
    GoTo {
        #[param("The book's id")]
        book: String,
        #[param("Chapter, counted from 0; the open chapter when omitted")]
        chapter: Option<usize>,
        #[param(
            "Block (paragraph or heading) to open at, counted from 0 in the chapter; its start when omitted"
        )]
        block: Option<usize>,
    },
    /// Select passage
    ///
    /// Selects a passage by paragraph and character, as a drag would, and
    /// shows its page.
    #[effect(view, row)]
    SelectPassage {
        #[param("The book's id")]
        book: String,
        #[param("Chapter, counted from 0; the open chapter when omitted")]
        chapter: Option<usize>,
        #[param(
            "Block (paragraph or heading) the selection starts in, counted from 0 in the chapter"
        )]
        start_block: usize,
        #[param("Character offset into the start block; 0 when omitted")]
        start_char: Option<usize>,
        #[param("Block the selection ends in; the start block when omitted")]
        end_block: Option<usize>,
        #[param("Exclusive character offset into the end block; the block's end when omitted")]
        end_char: Option<usize>,
    },
    /// Read chapter
    ///
    /// Answers with a chapter's text block by block, each with the index a
    /// passage, a highlight or a note names it by, and the book's title,
    /// author and chapters. Changes nothing.
    #[effect(kind = "read", returns, idempotent)]
    ReadChapter {
        #[param("The book's id")]
        book: String,
        #[param("Chapter, counted from 0; the open chapter when omitted")]
        chapter: Option<usize>,
    },
    /// Share with workspace
    ///
    /// Puts this device's copy of the book in the shared workspace, so
    /// collaborators can open it.
    #[effect(row, job, cancellable, idempotent)]
    Share { book: String },
    /// Download and open
    ///
    /// Fetches a book a collaborator shared from the workspace and opens it.
    #[effect(row, job, cancellable, idempotent)]
    Download { book: String },
}

impl ResourceSurface<Ebook, BooksEffect> for BooksApp {
    fn present(&mut self, state: &Rc<Ebook>) {
        self.adopt_resource_state(state);
    }

    fn attach(&mut self, cx: SurfaceCx<Ebook>) {
        self.attach_host(&cx);
    }

    const APPLIED: bool = true;

    /// A removal applied here, from the shelf, the palette, a script or an
    /// agent once the person agreed: this device forgets the book's file.
    fn applied(&mut self, action: &<Ebook as tilo_resource::Resource>::Action) {
        if let EbookAction::RemoveFromLibrary(removed) = &action.operation {
            self.forget_removed(&removed.book);
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario.unwrap_or("read"));
    }

    fn effect(&mut self, effect: BooksEffect, cx: &mut EffectCx<Ebook, Self>) -> EffectAnswer {
        match effect {
            BooksEffect::OpenBook => Ok(Self::choose_book(cx)),
            BooksEffect::OpenStored { content, name } => self.open_stored(content, name, cx),
            effect => {
                let invocation = cx.invocation().unwrap_or_default();
                let entries: Vec<LibraryEntry> = cx
                    .live()
                    .with(|state| state.entries.values().cloned().collect());
                let answer = self.begin_effect(invocation, effect, &entries)?;
                if matches!(answer, Effected::Pending) {
                    self.await_effect(invocation, cx.reply());
                }
                Ok(answer)
            }
        }
    }

    /// Where the reader is in the open book: its page chrome, a row beside
    /// the library.
    fn live_rows(&self) -> Vec<LiveRow> {
        self.reading_row().into_iter().collect()
    }

    /// The reading position, or the book, chapter, annotation or location a
    /// search hit names, when the published page leaves it out.
    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.reading_row()
            .filter(|row| row.id() == id)
            .or_else(|| self.search_row(id))
    }

    /// The library's edits bound to where the reader is: the page's
    /// bookmark and notes, and the selection's highlight and note.
    fn bound_declarations(&self) -> Vec<ActionDecl<Ebook>> {
        self.reading_declarations()
    }

    /// The books with their chapters' prose, their annotations and the
    /// library's remote books: what the reader holds, not the document.
    fn search_entries(&self) -> Vec<ui::tilo_resource::SearchEntry> {
        self.book_search_entries()
    }
}

/// A dispatcher of the library for tests that drive the reader (or one of
/// its editors) on a document of their own: its edits go through the same
/// permission check and executor a hosted reader's do.
#[cfg(test)]
pub(crate) fn library_dispatcher_for_test() -> ui::framework::Dispatcher<Ebook> {
    ui::framework::testing::dispatcher::<Ebook>(EbookShell::ID)
}

/// A library document holding `entries` and `marks`, as tests start from.
#[cfg(test)]
pub(crate) fn doc_with(
    entries: &[LibraryEntry],
    marks: &[Annotation],
) -> ui::automerge::AutoCommit {
    let mut library = tilo_resource::Replica::<Ebook>::new();
    let paths = Ebook::paths();
    for (at, entry) in entries.iter().enumerate() {
        put(
            &mut library,
            &paths.entries.insert_as(&entry.key(), at, entry),
        );
    }
    for (at, mark) in marks.iter().enumerate() {
        put(
            &mut library,
            &paths
                .annotations
                .insert_as(&mark.key(), at, &Mark::of(mark)),
        );
    }
    library.doc().clone()
}

/// Apply `edit` to the library in `doc`, as a test changes it.
#[cfg(test)]
pub(crate) fn edit_doc(doc: &mut ui::automerge::AutoCommit, edit: &tilo_resource::Edit<Ebook>) {
    let mut library = tilo_resource::Replica::<Ebook>::from_doc(doc.clone());
    put(&mut library, edit);
    *doc = library.doc().clone();
}

#[cfg(test)]
fn put(library: &mut tilo_resource::Replica<Ebook>, edit: &tilo_resource::Edit<Ebook>) {
    // The library's own fixture: removals are confirmed.
    library
        .apply_confirmed(Invoke::edit(edit).expect("a test's edit builds"))
        .expect("a test's edit applies");
}

/// The library `doc` holds.
#[cfg(test)]
pub(crate) fn read_doc(doc: &ui::automerge::AutoCommit) -> Ebook {
    <Ebook as tilo_resource::Resource>::read(doc).expect("a library document")
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::EbookShell::scenarios() {
            let mut app = TestApp::<super::EbookShell>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
