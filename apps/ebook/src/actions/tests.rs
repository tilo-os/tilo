use super::*;
use crate::model::{AnnoKind, annotation_key};
use crate::resource::{BooksEffect, Passage, mark_place_resource};
use tilo_actions::{ActionRegistry, EncodedAction};
use ui::tilo_resource::{Invoke, Replica, Resource as _};
use uuid::Uuid;

const BOOK: &str = "book:0123456789abcdef0123456789abcdef";

/// A library holding one book with a highlight carrying two comments, a
/// note and a bookmark.
fn seeded() -> Replica<Ebook> {
    let mut library = Replica::<Ebook>::new();
    library
        .apply(crate::resource::shelve(&crate::model::LibraryEntry {
            id: BOOK.into(),
            title: "Annotated".into(),
            ..crate::model::LibraryEntry::default()
        }))
        .unwrap();
    library
        .apply(Ebook::HIGHLIGHT.with(highlight_params(
            BOOK,
            0,
            HiColor::Green,
            Vec::new(),
            vec![HiPiece {
                block: 3,
                start: 0,
                end: 12,
                excerpt: "a passage".into(),
            }],
        )))
        .unwrap();
    let group = highlight(&library).group;
    for text in ["first comment", "second comment"] {
        library
            .apply(Ebook::ADD_COMMENT.with(books::AddComment {
                highlight: group,
                text: text.into(),
            }))
            .unwrap();
    }
    library
        .apply(Ebook::ADD_NOTE.with(books::AddNote {
            passage: Passage {
                book: BOOK.into(),
                chapter: 0,
                block: 5,
                start: 0,
                end: 0,
                excerpt: String::new(),
            },
            color: HiColor::Yellow,
            text: "standalone".into(),
        }))
        .unwrap();
    library
        .apply(Ebook::TOGGLE_BOOKMARK.with(books::ToggleBookmark {
            book: BOOK.into(),
            chapter: 0,
            block: 1,
            excerpt: "the first page".into(),
        }))
        .unwrap();
    library
}

fn seeded_doc() -> ui::automerge::AutoCommit {
    seeded().doc().clone()
}

fn highlight(library: &Replica<Ebook>) -> crate::model::Annotation {
    library
        .state()
        .unwrap()
        .annotations_for(BOOK)
        .into_iter()
        .find(|mark| mark.kind == AnnoKind::Highlight)
        .expect("the seeded highlight")
}

fn read(doc: &ui::automerge::AutoCommit) -> Ebook {
    Ebook::read(doc).unwrap()
}

fn registry() -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    Ebook::register(&mut registry);
    registry
}

fn encoded(invoke: &Invoke<Ebook>) -> EncodedAction {
    let action = invoke.build(1_700_000_000_000).unwrap();
    ui::tilo_resource::encode::<Ebook>(&action).unwrap()
}

fn group_of(doc: &ui::automerge::AutoCommit) -> Uuid {
    read(doc)
        .annotations_for(BOOK)
        .into_iter()
        .find(|mark| mark.kind == AnnoKind::Highlight)
        .unwrap()
        .group
}

/// The library's published tree for `doc`, with the reader's effects.
fn resources_for(doc: &ui::automerge::AutoCommit) -> Vec<Resource> {
    let mut library = ui::tilo_resource::publish::<Ebook>("ebook.library", "Books", &read(doc));
    library
        .actions
        .extend(<BooksEffect as ui::tilo_resource::Effect>::specs());
    vec![library]
}

ui::action_contract_tests! {
    seed: seeded_doc(),
    registry: registry(),
    resources: |doc| resources_for(doc),
    palette: ["Open book", "Delete highlight", "Add comment"],
    replay: [
        add_comment => |doc| encoded(&Ebook::ADD_COMMENT.with(books::AddComment {
            highlight: group_of(doc),
            text: "converge".into(),
        })),
        record_progress => |_| encoded(&Ebook::RECORD_PROGRESS.with(books::RecordProgress {
            book: BOOK.into(),
            chapter: 2,
            block: 7,
            fraction: 0.625,
            opened: true,
        })),
        finish_note => |doc| {
            let note = read(doc)
                .annotations_for(BOOK)
                .into_iter()
                .find(|mark| mark.kind == AnnoKind::Note && !mark.is_comment())
                .unwrap();
            encoded(&Invoke::edit(
                &Ebook::paths().annotations.item(&annotation_key(note.id)).created.set(1_700_000_123_456),
            ).unwrap())
        },
        move_highlight => |doc| {
            let piece = read(doc)
                .annotations_for(BOOK)
                .into_iter()
                .find(|mark| mark.kind == AnnoKind::Highlight)
                .unwrap();
            encoded(&Invoke::edit(
                &Ebook::paths().annotations.item(&annotation_key(piece.id)).block.set(9),
            ).unwrap())
        },
    ],
    undo: [
        add_bookmark => |_| encoded(&Ebook::TOGGLE_BOOKMARK.with(books::ToggleBookmark {
            book: BOOK.into(),
            chapter: 0,
            block: 9,
            excerpt: "bm".into(),
        })),
        delete_highlight => |doc| encoded(&Ebook::DELETE_HIGHLIGHT.with(books::DeleteHighlight {
            highlight: group_of(doc),
        })),
    ],
}

/// The reader's verbs bound to where it is build the library's verbs with
/// the page or the selection filled in.
#[test]
fn reading_declarations_build_the_librarys_verbs_for_the_place() {
    let mut library = seeded();
    let ctx = BookResourceCtx {
        book_id: BOOK.into(),
        chapter: 0,
        bookmark: books::ToggleBookmark {
            book: BOOK.into(),
            chapter: 0,
            block: 4,
            excerpt: "page four".into(),
        },
        page_note: books::AddNote {
            passage: Passage {
                book: BOOK.into(),
                chapter: 0,
                block: 4,
                start: 0,
                end: 0,
                excerpt: String::new(),
            },
            color: HiColor::Yellow,
            text: String::new(),
        },
        selection_highlight: Some(highlight_params(
            BOOK,
            0,
            HiColor::Pink,
            Vec::new(),
            vec![HiPiece {
                block: 6,
                start: 2,
                end: 9,
                excerpt: "selected".into(),
            }],
        )),
        selection_note: None,
    };
    let declarations = reading_declarations(&ctx);
    let ids: Vec<&str> = declarations.iter().map(|decl| decl.id).collect();
    assert_eq!(ids, ["bookmark-page", "note-page", "highlight-selection"]);
    for (id, args) in [
        ("bookmark-page", serde_json::json!({})),
        ("note-page", serde_json::json!({ "text": "on this page" })),
        ("highlight-selection", serde_json::json!({})),
    ] {
        let decl = declarations.iter().find(|decl| decl.id == id).unwrap();
        let serde_json::Value::Object(args) = args else {
            unreachable!()
        };
        let args = tilo_actions::ActionArgs::bind_json(&decl.params, &args).unwrap();
        let action = (decl.build)(&args, 1).unwrap();
        library.apply_action(&action).unwrap();
    }
    let marks = library.state().unwrap().annotations_for(BOOK);
    assert!(
        marks
            .iter()
            .any(|mark| mark.kind == AnnoKind::Bookmark && mark.block == 4)
    );
    assert!(marks.iter().any(|mark| mark.note == "on this page"));
    assert!(marks.iter().any(|mark| mark.kind == AnnoKind::Highlight
        && mark.color == HiColor::Pink
        && (mark.block, mark.start, mark.end) == (6, 2, 9)));
}

/// A search hit on an annotation names its place, which offers what acts
/// on it and opens its passage.
#[test]
fn an_annotation_of_any_book_offers_its_verbs_at_its_place() {
    let library = seeded();
    let state = library.state().unwrap();
    let piece = highlight(&library);
    let place = Ebook::child(&state, &mark_place_resource(&piece, false))
        .expect("the highlight resolves by id");
    let actions: Vec<&str> = place
        .actions
        .iter()
        .map(|action| action.id.as_str())
        .collect();
    for expected in ["open", "change-color", "delete-highlight", "add-comment"] {
        assert!(actions.contains(&expected), "{expected} in {actions:?}");
    }
    let note = state
        .annotations_for(BOOK)
        .into_iter()
        .find(|mark| mark.kind == AnnoKind::Note && !mark.is_comment())
        .unwrap();
    let place = Ebook::child(&state, &mark_place_resource(&note, false)).unwrap();
    assert!(
        place
            .actions
            .iter()
            .any(|action| action.id == "delete-annotation")
    );
    let gone = crate::model::Annotation {
        id: Uuid::nil(),
        group: Uuid::nil(),
        ..note
    };
    assert!(
        Ebook::child(&state, &mark_place_resource(&gone, false)).is_none(),
        "an annotation that is gone resolves to nothing"
    );
}
