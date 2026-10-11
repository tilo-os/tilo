//! The library on its own: verbs, field edits, invariants, merging, the
//! published places and history, through the same door the host uses.

use super::*;
use crate::model::{CompositionStyle, FontKind, LibrarySort, MAX_NOTE_BYTES, ReadMode, ReadTheme};
use tilo_document::RichTextAction;
use ui::tilo_actions::Resource as Published;
use ui::tilo_resource::{Edit, Replica, Resource};

const BOOK: &str = "book:0123456789abcdef0123456789abcdef";

fn edit(library: &mut Replica<Ebook>, edit: &Edit<Ebook>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    library
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn apply(library: &mut Replica<Ebook>, invoke: Invoke<Ebook>) -> Result<(), String> {
    library
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn row(id: &str, title: &str) -> LibraryEntry {
    LibraryEntry {
        id: id.into(),
        title: title.into(),
        author: "A. Writer".into(),
        subject: "Fiction".into(),
        ..LibraryEntry::default()
    }
}

fn state(library: &Replica<Ebook>) -> Ebook {
    library.state().unwrap()
}

fn marks(library: &Replica<Ebook>) -> Vec<Annotation> {
    state(library).annotations_for(BOOK)
}

/// A shelf with one book holding a highlight with two comments, a note and
/// a bookmark; the highlight's group.
fn annotated() -> (Replica<Ebook>, Uuid) {
    let mut library = Replica::<Ebook>::new();
    apply(&mut library, shelve(&row(BOOK, "Annotated"))).unwrap();
    apply(
        &mut library,
        Ebook::HIGHLIGHT.with(books::Highlight {
            book: BOOK.into(),
            chapter: 0,
            color: HiColor::Green,
            replacing: Vec::new(),
            pieces: vec![HiPiece {
                block: 3,
                start: 0,
                end: 12,
                excerpt: "a passage".into(),
            }],
        }),
    )
    .unwrap();
    let group = marks(&library)[0].group;
    for text in ["first comment", "second comment"] {
        apply(
            &mut library,
            Ebook::ADD_COMMENT.with(books::AddComment {
                highlight: group,
                text: text.into(),
            }),
        )
        .unwrap();
    }
    apply(&mut library, note(5, "standalone")).unwrap();
    apply(
        &mut library,
        Ebook::TOGGLE_BOOKMARK.with(books::ToggleBookmark {
            book: BOOK.into(),
            chapter: 0,
            block: 1,
            excerpt: "the first page".into(),
        }),
    )
    .unwrap();
    (library, group)
}

fn note(block: usize, text: &str) -> Invoke<Ebook> {
    Ebook::ADD_NOTE.with(books::AddNote {
        passage: Passage {
            book: BOOK.into(),
            chapter: 0,
            block,
            start: 0,
            end: 4,
            excerpt: "here".into(),
        },
        color: HiColor::Blue,
        text: text.into(),
    })
}

fn find(library: &Replica<Ebook>, test: impl Fn(&Annotation) -> bool) -> Annotation {
    marks(library)
        .into_iter()
        .find(|mark| test(mark))
        .expect("the annotation is there")
}

/// The shelf: a book joins once under its content id, refreshing its row
/// keeps when it was added, and leaving the shelf keeps what was read and
/// marked in it.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn books_join_and_leave_the_shelf_under_their_content_ids() {
    let mut library = Replica::<Ebook>::new();
    library.set_now_ms(1_000);
    apply(&mut library, shelve(&row(BOOK, "Novel"))).unwrap();
    let shelf = state(&library).entries;
    assert_eq!(shelf.len(), 1);
    let first = shelf.values().next().unwrap().clone();
    assert_eq!((first.title.as_str(), first.added), ("Novel", 1_000));

    library.set_now_ms(9_999);
    apply(
        &mut library,
        shelve(&LibraryEntry {
            shared: true,
            ..row(BOOK, "Novel, revised")
        }),
    )
    .unwrap();
    let shelf = state(&library).entries;
    assert_eq!(shelf.len(), 1, "the same content id updates in place");
    let again = shelf.values().next().unwrap();
    assert!(again.shared, "shared availability is replicated");
    assert_eq!(again.title, "Novel, revised");
    assert_eq!(
        again.added, 1_000,
        "added time is preserved across a re-import"
    );
    apply(&mut library, shelve(&row(BOOK, "Novel"))).unwrap();
    assert!(
        state(&library).entry(BOOK).unwrap().shared,
        "a later import without the workspace does not withdraw it"
    );

    assert!(
        apply(&mut library, shelve(&row("not-a-book", "Bad"))).is_err(),
        "a row names its book by book: and its content hash"
    );
    let content = "f".repeat(64);
    assert!(
        apply(
            &mut library,
            shelve(&LibraryEntry {
                content: LibraryEntry::stored(&content, 9).unwrap(),
                ..row(BOOK, "Novel")
            })
        )
        .is_err(),
        "content that is not this book's is refused"
    );
    let content = format!("0123456789abcdef0123456789abcdef{}", "0".repeat(32));
    apply(
        &mut library,
        shelve(&LibraryEntry {
            content: LibraryEntry::stored(&content, 9).unwrap(),
            ..row(BOOK, "Novel")
        }),
    )
    .unwrap();
    assert_eq!(
        state(&library)
            .entry(BOOK)
            .unwrap()
            .content_hash()
            .as_deref(),
        Some(content.as_str()),
        "the row names the bytes a content store keeps"
    );

    apply(
        &mut library,
        Ebook::RECORD_PROGRESS.with(books::RecordProgress {
            book: BOOK.into(),
            chapter: 2,
            block: 5,
            fraction: 0.42,
            opened: true,
        }),
    )
    .unwrap();
    apply(
        &mut library,
        Ebook::RECORD_PROGRESS.with(books::RecordProgress {
            book: BOOK.into(),
            chapter: 2,
            block: 7,
            fraction: 0.5,
            opened: false,
        }),
    )
    .unwrap();
    assert_eq!(
        state(&library).progress_of(BOOK),
        Progress {
            chapter: 2,
            block: 7,
            fraction: 0.5,
            opened: 9_999,
        },
        "reading on keeps when the book was opened"
    );
    assert_eq!(
        state(&library).progress_of("book:nope"),
        Progress::default()
    );

    apply(&mut library, note(1, "kept")).unwrap();
    let remove = Ebook::REMOVE_FROM_LIBRARY.with(books::RemoveFromLibrary { book: BOOK.into() });
    assert!(
        library.apply(remove.clone()).is_err(),
        "removing a book asks for confirmation"
    );
    library.apply_confirmed(remove).unwrap();
    let after = state(&library);
    assert!(after.entries.is_empty(), "removed from the shelf");
    assert_eq!(
        after.progress_of(BOOK).chapter,
        2,
        "where the reader was stays"
    );
    assert_eq!(
        after.annotations_for(BOOK).len(),
        1,
        "what was marked stays"
    );
    let bytes = library.save();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("/home/reader/"),
        "a source path never enters the library"
    );
}

/// Reading settings, shelf preferences, fonts and schemes are fields,
/// edited at their paths and held to their invariants.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn settings_fonts_and_schemes_are_edited_at_their_paths() {
    let mut library = Replica::<Ebook>::new();
    let fresh = state(&library);
    assert_eq!(fresh.settings.mode, ReadMode::Paged, "readers open paged");
    assert_eq!(fresh.settings.composition, CompositionStyle::Book);
    assert!(
        fresh
            .fonts
            .values()
            .any(|font| font.kind == FontKind::Serif),
        "a new library has the built-in fonts"
    );
    assert_eq!(fresh.schemes.len(), crate::model::default_schemes().len());

    let settings = Ebook::paths().settings;
    let wanted = Settings {
        font_scale: 1.3,
        line_spacing: 1.45,
        font_id: "serif".into(),
        theme: ReadTheme::Dark,
        scheme_id: "midnight".into(),
        mode: ReadMode::TwoUp,
        composition: CompositionStyle::Ragged,
    };
    for change in settings.path().edits(&fresh.settings, &wanted) {
        edit(&mut library, &change).unwrap();
    }
    assert_eq!(state(&library).settings, wanted);
    assert!(
        edit(&mut library, &settings.font_scale.set(9.0)).is_err(),
        "the text size stays within its bounds, however it is set"
    );
    assert!(edit(&mut library, &settings.line_spacing.set(f32::NAN)).is_err());

    let prefs = Ebook::paths().prefs;
    edit(&mut library, &prefs.sort.set(LibrarySort::Recent)).unwrap();
    edit(&mut library, &prefs.hide_read.set(true)).unwrap();
    assert_eq!(
        state(&library).prefs,
        LibraryPrefs {
            sort: LibrarySort::Recent,
            hide_read: true,
        }
    );

    let literata = FontEntry::new(
        "u1",
        FontKind::Named("Literata".into()),
        "Literata",
        1.1,
        true,
    );
    let fonts = Ebook::paths().fonts;
    edit(
        &mut library,
        &fonts.insert_as(&Id::from_raw("u1"), fresh.fonts.len(), &literata),
    )
    .unwrap();
    let u1 = fonts.item(&Id::from_raw("u1"));
    edit(&mut library, &u1.name.replace("Book")).unwrap();
    edit(&mut library, &u1.shown.set(false)).unwrap();
    assert!(edit(&mut library, &u1.size_mult.set(3.0)).is_err());
    assert!(
        edit(&mut library, &u1.id.replace("other")).is_err(),
        "a font's id is the key it is stored under"
    );
    let stored = state(&library).fonts.values().last().unwrap().clone();
    assert_eq!(
        (stored.name.as_str(), stored.shown, stored.kind.clone()),
        ("Book", false, FontKind::Named("Literata".into()))
    );
    edit(&mut library, &settings.font_id.replace("u1")).unwrap();
    edit(
        &mut library,
        &Ebook::paths().default_font.set(Atom("u1".into())),
    )
    .unwrap();
    library
        .apply_confirmed(Ebook::REMOVE_FONT.with(books::RemoveFont { font: "u1".into() }))
        .unwrap();
    let after = state(&library);
    assert!(after.fonts.values().all(|font| font.id != "u1"));
    assert!(
        after.settings.font_id.is_empty() && after.default_font.0.is_empty(),
        "nothing keeps choosing a removed font"
    );

    let ocean = ColorScheme::from_bg("u1", "Ocean", [0x0A, 0x22, 0x33]);
    let schemes = Ebook::paths().schemes;
    edit(
        &mut library,
        &schemes.insert_as(&Id::from_raw("u1"), after.schemes.len(), &ocean),
    )
    .unwrap();
    edit(
        &mut library,
        &schemes.item(&Id::from_raw("light")).enabled.set(false),
    )
    .unwrap();
    let stored = state(&library);
    assert!(!stored.schemes.get(&Id::from_raw("light")).unwrap().enabled);
    let custom = stored.schemes.get(&Id::from_raw("u1")).unwrap();
    assert_eq!(custom.page, [0x0A, 0x22, 0x33]);
    assert!(custom.dark && !custom.builtin);
    assert!(
        library
            .apply_confirmed(Ebook::DELETE_SCHEME.with(books::DeleteScheme {
                scheme: "light".into(),
            }))
            .is_err(),
        "a built-in scheme is turned off, never deleted"
    );
    library
        .apply_confirmed(Ebook::DELETE_SCHEME.with(books::DeleteScheme {
            scheme: "u1".into(),
        }))
        .unwrap();
    assert!(state(&library).schemes.get(&Id::from_raw("u1")).is_none());
}

/// Annotation verbs: a highlight goes with its comments, what is gone is
/// refused, and a malformed or oversized change is refused whole.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn annotations_change_through_their_verbs_and_refuse_what_is_gone() {
    let (mut library, group) = annotated();
    assert_eq!(
        marks(&library).len(),
        5,
        "highlight + 2 comments + note + bookmark"
    );
    let comments: Vec<String> = marks(&library)
        .into_iter()
        .filter(Annotation::is_comment)
        .map(|comment| comment.note)
        .collect();
    assert_eq!(comments, ["first comment", "second comment"]);
    let comment = find(&library, Annotation::is_comment);
    assert_eq!(
        (comment.block, comment.start, comment.end, comment.color),
        (3, 0, 12, HiColor::Green),
        "a comment sits on its highlight's first piece"
    );

    apply(
        &mut library,
        Ebook::CHANGE_COLOR.with(books::ChangeColor {
            highlight: group,
            color: HiColor::Purple,
        }),
    )
    .unwrap();
    assert!(
        marks(&library)
            .iter()
            .filter(|mark| mark.group == group)
            .all(|mark| mark.color == HiColor::Purple),
        "a highlight is recoloured with its comments"
    );

    // Finishing a note stamps it; moving a piece re-anchors that row only.
    let standalone = find(&library, |mark| {
        mark.kind == AnnoKind::Note && !mark.is_comment()
    });
    edit(
        &mut library,
        &note_paths(standalone.id).created.set(1_700_000_123_456),
    )
    .unwrap();
    assert_eq!(
        find(&library, |mark| mark.id == standalone.id).created,
        1_700_000_123_456
    );
    let piece = find(&library, |mark| mark.kind == AnnoKind::Highlight);
    let at = note_paths(piece.id);
    for change in [at.block.set(9), at.end.set(5), at.excerpt.replace("moved")] {
        edit(&mut library, &change).unwrap();
    }
    let moved = find(&library, |mark| mark.id == piece.id);
    assert_eq!((moved.block, moved.start, moved.end), (9, 0, 5));
    assert_eq!(
        marks(&library)
            .iter()
            .filter(|mark| mark.is_comment())
            .count(),
        2,
        "the highlight's comments survive the re-anchor"
    );
    assert!(
        edit(&mut library, &at.start.set(7)).is_err(),
        "a range never ends before it starts"
    );

    // A malformed highlight changes nothing, not even what it would replace.
    assert!(
        apply(
            &mut library,
            Ebook::HIGHLIGHT.with(books::Highlight {
                book: BOOK.into(),
                chapter: 0,
                color: HiColor::Yellow,
                replacing: vec![group],
                pieces: vec![HiPiece {
                    block: 1,
                    start: 9,
                    end: 2,
                    excerpt: "backwards".into(),
                }],
            })
        )
        .is_err()
    );
    assert_eq!(marks(&library).len(), 5);
    assert!(
        edit(
            &mut library,
            &note_text(standalone.id)
                .perform(&RichTextAction::ReplaceBody {
                    markdown: "x".repeat(MAX_NOTE_BYTES + 1),
                })
                .unwrap()
        )
        .is_err(),
        "an oversized note is refused"
    );
    assert_eq!(
        find(&library, |mark| mark.id == standalone.id).note,
        "standalone"
    );

    apply(
        &mut library,
        Ebook::DELETE_HIGHLIGHT.with(books::DeleteHighlight { highlight: group }),
    )
    .unwrap();
    let left = marks(&library);
    assert_eq!(
        left.len(),
        2,
        "only the standalone note and the bookmark survive"
    );
    assert!(left.iter().all(|mark| mark.group != group));
    for late in [
        Ebook::ADD_COMMENT.with(books::AddComment {
            highlight: group,
            text: "late comment".into(),
        }),
        Ebook::CHANGE_COLOR.with(books::ChangeColor {
            highlight: group,
            color: HiColor::Pink,
        }),
        Ebook::DELETE_HIGHLIGHT.with(books::DeleteHighlight { highlight: group }),
    ] {
        assert!(
            apply(&mut library, late).is_err(),
            "a deleted highlight is refused"
        );
    }

    apply(
        &mut library,
        Ebook::DELETE_ANNOTATION.with(books::DeleteAnnotation {
            annotation: standalone.id,
        }),
    )
    .unwrap();
    assert!(
        edit(
            &mut library,
            &note_text(standalone.id)
                .perform(&RichTextAction::ReplaceBody {
                    markdown: "late edit".into(),
                })
                .unwrap()
        )
        .is_err(),
        "a deleted note is not written to"
    );
    assert!(edit(&mut library, &note_paths(standalone.id).created.set(1)).is_err());
    assert!(
        apply(
            &mut library,
            Ebook::DELETE_ANNOTATION.with(books::DeleteAnnotation {
                annotation: standalone.id,
            })
        )
        .is_err(),
        "an annotation that is gone is said to be gone"
    );
}

fn note_paths(id: Uuid) -> <Mark as Model>::Paths<Ebook> {
    Ebook::paths().annotations.item(&annotation_key(id))
}

/// Two readers changing one library at once converge: each field of a
/// note, a recolour and a comment's text, settings and fonts side by side,
/// and a deleted highlight keeps its comments deleted.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test keeps each phase and its fixture visible"
)]
fn concurrent_changes_converge() {
    let (mut ours, group) = annotated();
    let mut theirs = ours.fork();
    let standalone = find(&ours, |mark| {
        mark.kind == AnnoKind::Note && !mark.is_comment()
    });
    let comment = find(&ours, Annotation::is_comment);
    edit(
        &mut ours,
        &note_text(standalone.id)
            .perform(&RichTextAction::ReplaceBody {
                markdown: "edited concurrently".into(),
            })
            .unwrap(),
    )
    .unwrap();
    edit(
        &mut ours,
        &note_text(comment.id)
            .perform(&RichTextAction::ReplaceBody {
                markdown: "comment edited".into(),
            })
            .unwrap(),
    )
    .unwrap();
    edit(
        &mut ours,
        &Ebook::paths().settings.theme.set(ReadTheme::Dark),
    )
    .unwrap();
    let at = note_paths(standalone.id);
    for change in [
        at.chapter.set(2),
        at.block.set(7),
        at.excerpt.replace("new anchor"),
    ] {
        edit(&mut theirs, &change).unwrap();
    }
    apply(
        &mut theirs,
        Ebook::CHANGE_COLOR.with(books::ChangeColor {
            highlight: group,
            color: HiColor::Purple,
        }),
    )
    .unwrap();
    edit(&mut theirs, &Ebook::paths().settings.line_spacing.set(1.55)).unwrap();
    ours.sync_with(&mut theirs).unwrap();
    assert_eq!(state(&ours), state(&theirs));
    let merged = find(&ours, |mark| mark.id == standalone.id);
    assert_eq!(merged.note, "edited concurrently");
    assert_eq!((merged.chapter, merged.block), (2, 7));
    assert_eq!(merged.excerpt, "new anchor");
    let merged = find(&ours, |mark| mark.id == comment.id);
    assert_eq!(
        (merged.note.as_str(), merged.color),
        ("comment edited", HiColor::Purple)
    );
    let settings = state(&ours).settings;
    assert_eq!(
        (settings.theme, settings.line_spacing),
        (ReadTheme::Dark, 1.55)
    );

    // A font removed while another reader renames it stays removed; fonts
    // added side by side are both kept.
    let fonts = Ebook::paths().fonts;
    for (replica, id) in [(&mut ours, "left"), (&mut theirs, "right")] {
        let font = FontEntry::new(id, FontKind::Named(id.into()), id, 1.0, true);
        edit(replica, &fonts.insert_as(&Id::from_raw(id), 99, &font)).unwrap();
    }
    ours.sync_with(&mut theirs).unwrap();
    ours.apply_confirmed(Ebook::REMOVE_FONT.with(books::RemoveFont {
        font: "left".into(),
    }))
    .unwrap();
    edit(
        &mut theirs,
        &fonts
            .item(&Id::from_raw("left"))
            .name
            .replace("Late offline rename"),
    )
    .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    for replica in [&ours, &theirs] {
        let ids: Vec<String> = state(replica)
            .fonts
            .values()
            .map(|font| font.id.clone())
            .collect();
        assert!(ids.contains(&"right".to_owned()));
        assert!(
            !ids.contains(&"left".to_owned()),
            "the removal wins over the rename"
        );
    }

    // A highlight deleted while another reader comments on it keeps its
    // comments gone, the late one too.
    apply(
        &mut ours,
        Ebook::DELETE_HIGHLIGHT.with(books::DeleteHighlight { highlight: group }),
    )
    .unwrap();
    apply(
        &mut theirs,
        Ebook::ADD_COMMENT.with(books::AddComment {
            highlight: group,
            text: "offline comment".into(),
        }),
    )
    .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    for replica in [&ours, &theirs] {
        assert!(
            marks(replica).iter().all(|mark| mark.group != group),
            "no row of the deleted highlight shows"
        );
    }

    // Two readers bookmarking the same page at once keep both bookmarks;
    // the next toggle takes the page's bookmarks off.
    let bookmark = |block| {
        Ebook::TOGGLE_BOOKMARK.with(books::ToggleBookmark {
            book: BOOK.into(),
            chapter: 0,
            block,
            excerpt: "bm".into(),
        })
    };
    apply(&mut ours, bookmark(9)).unwrap();
    apply(&mut theirs, bookmark(9)).unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let on_nine = |replica: &Replica<Ebook>| {
        marks(replica)
            .iter()
            .filter(|mark| mark.kind == AnnoKind::Bookmark && mark.block == 9)
            .count()
    };
    assert_eq!((on_nine(&ours), on_nine(&theirs)), (2, 2));
    apply(&mut ours, bookmark(9)).unwrap();
    ours.sync_with(&mut theirs).unwrap();
    assert_eq!((on_nine(&ours), on_nine(&theirs)), (0, 0));
}

/// The library as a device creates it on its own: the genesis every
/// device writes alike for one record, then this device's own changes.
fn started_apart(device: &[u8]) -> Replica<Ebook> {
    let mut doc = ui::automerge::AutoCommit::new()
        .with_actor(ui::automerge::ActorId::from(b"ebook-genesis".as_slice()));
    <Ebook as Resource>::initialize(&mut doc).unwrap();
    doc.commit();
    doc.set_actor(ui::automerge::ActorId::from(device));
    Replica::from_doc(doc)
}

/// Two devices that created the library on their own merge every book and
/// annotation.
#[test]
fn libraries_started_apart_merge_whole() {
    let mut left = started_apart(b"left");
    let mut right = started_apart(b"right");
    apply(&mut left, shelve(&row("book:left", "Left"))).unwrap();
    apply(&mut right, shelve(&row("book:right", "Right"))).unwrap();
    apply(&mut left, note(1, "left note")).unwrap();
    apply(&mut right, note(2, "right note")).unwrap();
    left.sync_with(&mut right).unwrap();
    for replica in [&left, &right] {
        let library = state(replica);
        let mut books: Vec<&str> = library.entries.values().map(|e| e.id.as_str()).collect();
        books.sort_unstable();
        assert_eq!(books, ["book:left", "book:right"]);
        assert_eq!(library.annotations_for(BOOK).len(), 2);
    }
}

/// Each book is a place offering what acts on it, its annotations beneath
/// it with theirs; fonts and schemes are items with their verbs.
#[test]
fn books_and_their_annotations_are_published_as_places() {
    let (library, group) = annotated();
    let published = library.published().unwrap();
    assert_eq!(published.kind, "ebook.library");
    let book = child(&published, &book_place_resource(BOOK));
    let actions = ids(book);
    for expected in ["open", "share", "remove-from-library"] {
        assert!(
            actions.contains(&expected.to_owned()),
            "{expected} in {actions:?}"
        );
    }
    let marks = marks(&library);
    let piece = marks.iter().find(|mark| mark.group == group).unwrap();
    let highlight = child(book, &mark_place_resource(piece, false));
    for expected in ["open", "change-color", "delete-highlight", "add-comment"] {
        assert!(ids(highlight).contains(&expected.to_owned()), "{expected}");
    }
    assert_eq!(highlight.children.len(), 2, "its two comments beneath it");
    let comment = marks.iter().find(|mark| mark.is_comment()).unwrap();
    assert_eq!(
        child(highlight, &mark_place_resource(comment, true)).label,
        comment.note
    );
    let standalone = marks
        .iter()
        .find(|mark| mark.kind == AnnoKind::Note && !mark.is_comment())
        .unwrap();
    let place = child(book, &mark_place_resource(standalone, false));
    assert_eq!(place.label, "standalone");
    assert!(ids(place).contains(&"delete-annotation".to_owned()));

    let state = state(&library);
    let font = state.fonts.values().next().unwrap();
    let settings = published
        .children
        .iter()
        .find(|place| place.kind == "ebook.library.settings")
        .expect("the reading settings are a place");
    assert!(
        settings
            .children
            .iter()
            .any(|row| row.label == font.name && ids(row).contains(&"remove-font".to_owned())),
        "a font offers to be removed"
    );
    assert!(
        published
            .actions
            .iter()
            .any(|action| action.id == "field-edit"),
        "settings, preferences and names are field edits"
    );

    let mut library = library;
    let delete = place
        .actions
        .iter()
        .find(|action| action.id == "delete-annotation")
        .unwrap();
    let encoded = delete.build_json(&serde_json::Map::new()).unwrap();
    let action: <Ebook as Resource>::Action = serde_json::from_slice(&encoded.payload).unwrap();
    library.apply_action(&action).unwrap();
    assert!(
        library
            .state()
            .unwrap()
            .annotations_for(BOOK)
            .iter()
            .all(|mark| mark.id != standalone.id),
        "a place's verb acts on what it names"
    );
}

fn child<'a>(tree: &'a Published, id: &str) -> &'a Published {
    tree.children
        .iter()
        .find(|child| child.id.as_str() == id)
        .unwrap_or_else(|| {
            panic!(
                "{id} under {}: {:?}",
                tree.id.as_str(),
                tree.children
                    .iter()
                    .map(|c| c.id.as_str())
                    .collect::<Vec<_>>()
            )
        })
}

fn ids(resource: &Published) -> Vec<String> {
    resource
        .actions
        .iter()
        .map(|action| action.id.clone())
        .collect()
}

/// A note's text is edited at its path, by the editor, the palette or an
/// agent, and each edit undoes as one.
#[test]
fn a_notes_text_edits_at_its_path_and_undoes() {
    let mut app = ui::framework::testing::TestApp::<EbookShell>::new().unwrap();
    app.invoke(shelve(&row(BOOK, "Annotated"))).unwrap();
    app.invoke(note(0, "first")).unwrap();
    let id = app.state().annotations_for(BOOK)[0].id;
    let text = |app: &ui::framework::testing::TestApp<EbookShell>| {
        app.state().annotations_for(BOOK)[0].note.clone()
    };
    assert_eq!(text(&app), "first");
    app.invoke(
        Invoke::edit(
            &note_text(id)
                .perform(&RichTextAction::ReplaceBody {
                    markdown: "from an agent".into(),
                })
                .unwrap(),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(text(&app), "from an agent");
    assert!(app.undo());
    assert_eq!(text(&app), "first");
}

/// An agent annotates in one call, and what it did reads back in the
/// reader's language as the app's sentences, naming who did it for whom.
#[test]
fn an_agents_changes_read_back_as_the_apps_sentences() {
    use ui::tilo_actions::{ActionInvoker, ActorKind};

    let mut app = ui::framework::testing::TestApp::<EbookShell>::new().unwrap();
    app.invoke(shelve(&row(BOOK, "Annotated"))).unwrap();
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let kind = <Ebook as Resource>::KIND;
    app.call_by(
        agent.clone(),
        kind,
        "add-note",
        serde_json::json!({
            "passage": {
                "book": BOOK, "chapter": 0, "block": 2, "start": 0, "end": 4,
                "excerpt": "door",
            },
            "color": "yellow",
            "text": "Remember this",
        }),
    )
    .unwrap();
    let note = app.state().annotations_for(BOOK)[0].clone();
    assert_eq!(note.note, "Remember this", "one call makes the note");
    for change in [
        Ebook::paths().settings.theme.set(ReadTheme::Dark),
        Ebook::paths().prefs.hide_read.set(true),
        note_paths(note.id).color.set(HiColor::Pink),
    ] {
        app.call_by(
            agent.clone(),
            kind,
            "field-edit",
            serde_json::json!({ "edit": change }),
        )
        .unwrap();
    }
    app.call_by(
        agent,
        kind,
        "delete-annotation",
        serde_json::json!({ "annotation": note.id }),
    )
    .unwrap();

    let catalogue = tilo_history::Catalogue::new("en-US", &[LOCALES[0].1]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let sentences: Vec<String> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .map(|entry| reader.sentence(&entry))
        .collect();
    assert_eq!(
        sentences,
        [
            "Added a note",
            "Set the page tint to dark",
            "Hid finished books",
            "Recoloured “door”",
            "Deleted an annotation",
        ]
    );
}

#[test]
fn launching_on_an_epub_keeps_it_in_the_content_store_shelves_and_opens_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("handed-off.epub");
    std::fs::write(&path, crate::content::SAMPLE_EPUB).unwrap();
    let mut app =
        ui::framework::testing::TestApp::<EbookShell>::launched(&[path.to_str().unwrap()]).unwrap();
    let id = crate::epub::content_id(crate::content::SAMPLE_EPUB);
    let book = crate::component::book_resource(&id);
    assert!(
        app.harness()
            .wait_until(ui::time::Duration::from_secs(10), |harness| harness
                .bound_resources(Some(&book))
                .is_ok_and(|resources| !resources.is_empty())),
        "the handed-off book joins the library"
    );
    let first_chapter = crate::content::bundled_sample().unwrap().chapters[0]
        .title
        .clone();
    assert!(
        reads_chapter(&mut app, &first_chapter),
        "the handed-off book opens in the reader"
    );
    let shelf: Vec<LibraryEntry> = app.state().entries.values().cloned().collect();
    assert_eq!(shelf.len(), 1, "and is now on the shelf: {shelf:?}");
    assert_eq!(
        shelf[0].content_hash().as_deref(),
        Some(tilo_content::hash_bytes(crate::content::SAMPLE_EPUB).as_str()),
        "its row names the bytes the content store keeps, not a path"
    );

    // Reopened with nothing named, the shelf still reads it.
    let mut again = app.reopen().unwrap();
    assert_eq!(again.state().entries.len(), 1);
    assert!(
        again
            .harness()
            .wait_until(ui::time::Duration::from_secs(10), |harness| harness
                .bound_resources(Some(&book))
                .is_ok_and(|resources| !resources.is_empty())),
        "the reopened library reads the book from the content store"
    );
}

/// The open reader shows its page to assistive technology as a document
/// named after the open chapter.
fn reads_chapter(app: &mut ui::framework::testing::TestApp<EbookShell>, title: &str) -> bool {
    app.harness()
        .wait_until(ui::time::Duration::from_secs(10), |harness| {
            harness.a11y_node_named(title).is_some()
        })
}

#[test]
fn every_scenario_stages_the_sample_shelf_through_the_librarys_edits() {
    for scenario in
        std::iter::once(None).chain(crate::scenarios::SCENARIOS.iter().copied().map(Some))
    {
        let app = ui::framework::testing::TestApp::<EbookShell>::capture(scenario)
            .unwrap_or_else(|error| panic!("{scenario:?}: {error}"));
        let shelf = app.state().entries.len();
        if scenario == Some("onboarding") {
            assert_eq!(shelf, 0, "the first start is an empty shelf");
        } else {
            assert_eq!(shelf, 3, "{scenario:?} shows the sample shelf");
        }
    }
    let mut comments =
        ui::framework::testing::TestApp::<EbookShell>::capture(Some("comments")).unwrap();
    assert!(
        comments.harness().a11y_nodes().iter().any(|(_, node)| node
            .label()
            .is_some_and(|label| label.contains("colour that keeps the time of day"))),
        "the comments scenario has the highlight's comments open"
    );
}

/// The reader publishes where it is and nothing of the library: the
/// library's places and verbs are the record's.
#[test]
fn the_reader_publishes_only_where_it_is() {
    use ui::widget::Widget;

    let mut app = BooksApp::with_library(
        library_dispatcher_for_test(),
        crate::content::load_sample_library(),
    );
    let doc = ui::automerge::AutoCommit::new();
    app.sync(&doc, &ui::automerge::ROOT);
    let extras: Vec<_> = <BooksApp as ResourceSurface<Ebook, BooksEffect>>::live_rows(&app)
        .into_iter()
        .map(LiveRow::into_resource)
        .collect();
    assert_eq!(
        extras.len(),
        1,
        "the reader publishes where it is and nothing of the library: {:?}",
        extras.iter().map(|r| r.id.as_str()).collect::<Vec<_>>()
    );
    let book_id = app.open_book().expect("a sample book is open").id.clone();
    assert_eq!(extras[0].id.as_str(), format!("ebook.selection:{book_id}"));
    for id in [
        "previous-page",
        "next-page",
        "contents",
        "notes",
        "settings",
        "select-passage",
        "go-to",
    ] {
        assert!(
            extras[0].actions.iter().any(|action| action.id == id),
            "the reader offers `{id}`"
        );
    }
    assert!(
        extras[0]
            .actions
            .iter()
            .all(|action| action.kind != ui::tilo_actions::OperationKind::Document),
        "where the reader is changes no document"
    );

    let bound = |app: &BooksApp| -> Vec<&'static str> {
        app.reading_declarations()
            .iter()
            .map(|declaration| declaration.id)
            .collect()
    };
    assert_eq!(bound(&app), ["bookmark-page", "note-page"]);
    app.select_passage_for_test(0, 0, 0, 4);
    assert_eq!(
        bound(&app),
        [
            "bookmark-page",
            "note-page",
            "highlight-selection",
            "note-selection"
        ],
        "a selection binds its highlight and note"
    );

    // An empty library must not wipe a shelf it never held.
    let before = app.library().len();
    app.adopt_resource_state(&Rc::new(Ebook::default()));
    assert_eq!(app.library().len(), before);
}
