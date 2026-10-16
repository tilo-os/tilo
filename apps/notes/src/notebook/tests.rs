//! The notebook on its own: its verbs and field edits through the same door
//! the host uses, merging, and what it publishes.

use super::*;
use tilo_document::action::RegionKind;
use ui::tilo_resource::{Edit, Invoke, Replica, Resource};
use ui::widget::marks::MarkSpan;

fn edit(notebook: &mut Replica<Notebook>, edit: &Edit<Notebook>) -> Result<(), String> {
    let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
    notebook
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn apply(notebook: &mut Replica<Notebook>, invoke: Invoke<Notebook>) -> Result<(), String> {
    notebook
        .apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A new note titled `title` under `parent`, through the verb.
fn note(notebook: &mut Replica<Notebook>, title: &str, parent: Option<&Id>) -> Id {
    let before: HashSet<Id> = notebook.state().unwrap().pages.ids().cloned().collect();
    apply(
        notebook,
        Notebook::NEW_NOTE.with(notebook::NewNote {
            title: title.to_owned(),
            parent: parent.cloned(),
        }),
    )
    .unwrap();
    notebook
        .state()
        .unwrap()
        .pages
        .ids()
        .find(|id| !before.contains(*id))
        .cloned()
        .expect("a new note")
}

/// The notes under `parent`, in order.
fn children(notebook: &Replica<Notebook>, parent: Option<&Id>) -> Vec<Id> {
    notebook
        .state()
        .unwrap()
        .pages
        .iter()
        .filter(|(_, page)| page.parent.as_ref() == parent)
        .map(|(id, _)| id.clone())
        .collect()
}

fn page(notebook: &Replica<Notebook>, id: &Id) -> Page {
    notebook.state().unwrap().pages.get(id).cloned().unwrap()
}

fn texts(notebook: &Replica<Notebook>, id: &Id) -> Vec<String> {
    page(notebook, id)
        .blocks()
        .into_iter()
        .map(|block| block.text)
        .collect()
}

fn write(notebook: &mut Replica<Notebook>, id: &Id, blocks: &[BlockContent]) {
    let op = clips_op(blocks);
    edit(
        notebook,
        &Notebook::paths().pages.item(id).body.perform(&op).unwrap(),
    )
    .unwrap();
}

fn uuid(id: &Id) -> PageId {
    page_of(id).expect("a note's id is a UUID")
}

/// Notes are made, placed, moved and deleted by verbs that keep the tree a
/// tree: siblings keep their order, a move reorders within its group or
/// files the note under another, and no note goes under itself.
#[test]
fn verbs_make_move_and_delete_notes_and_keep_the_tree_a_tree() {
    let mut notebook = Replica::<Notebook>::new();
    let a = note(&mut notebook, "A", None);
    let b = note(&mut notebook, " B ", None);
    let c = note(&mut notebook, "", None);
    assert_eq!(children(&notebook, None), [a.clone(), b.clone(), c.clone()]);
    assert!(page_of(&a).is_some(), "a note is kept under a UUID");
    assert_eq!(page(&notebook, &b).title, "B", "titles are trimmed");
    assert_eq!(page(&notebook, &c).title, UNTITLED);

    for (verb, note_, want) in [
        (Notebook::MOVE_UP.with(c.clone()), &c, [&a, &c, &b]),
        (Notebook::MOVE_DOWN.with(a.clone()), &a, [&c, &a, &b]),
    ] {
        apply(&mut notebook, verb).unwrap();
        assert_eq!(
            children(&notebook, None),
            want.map(Clone::clone),
            "moved {note_}"
        );
    }
    assert!(
        apply(&mut notebook, Notebook::MOVE_UP.with(c.clone())).is_err(),
        "the first note cannot move up"
    );
    assert!(
        apply(&mut notebook, Notebook::MOVE_DOWN.with(b.clone())).is_err(),
        "the last note cannot move down"
    );

    // Notes under a note reorder within it.
    let first = note(&mut notebook, "First", Some(&a));
    let second = note(&mut notebook, "Second", Some(&a));
    apply(&mut notebook, Notebook::MOVE_DOWN.with(first.clone())).unwrap();
    assert_eq!(
        children(&notebook, Some(&a)),
        [second.clone(), first.clone()]
    );
    assert_eq!(children(&notebook, None), [c.clone(), a.clone(), b.clone()]);

    // Before a sibling, then last again.
    for (before, want) in [(Some(c.clone()), [&b, &c, &a]), (None, [&c, &a, &b])] {
        apply(
            &mut notebook,
            Notebook::MOVE_NOTE.with(notebook::MoveNote {
                page: b.clone(),
                parent: None,
                before,
            }),
        )
        .unwrap();
        assert_eq!(children(&notebook, None), want.map(Clone::clone));
    }

    // Under another note, then out again right after it.
    apply(&mut notebook, Notebook::INDENT.with(b.clone())).unwrap();
    assert_eq!(
        page(&notebook, &b).parent,
        Some(a.clone()),
        "under the note above"
    );
    assert_eq!(
        children(&notebook, Some(&a)),
        [second, first.clone(), b.clone()]
    );
    apply(&mut notebook, Notebook::OUTDENT.with(b.clone())).unwrap();
    assert_eq!(children(&notebook, None), [c.clone(), a.clone(), b.clone()]);
    assert!(
        apply(&mut notebook, Notebook::OUTDENT.with(b.clone())).is_err(),
        "a top-level note has nothing to come out of"
    );

    // No note goes under itself or a note under it.
    for parent in [a.clone(), first] {
        assert!(
            apply(
                &mut notebook,
                Notebook::MOVE_NOTE.with(notebook::MoveNote {
                    page: a.clone(),
                    parent: Some(parent),
                    before: None,
                }),
            )
            .is_err()
        );
    }
    assert_eq!(page(&notebook, &a).parent, None);
    assert!(
        apply(
            &mut notebook,
            Notebook::NEW_NOTE.with(notebook::NewNote {
                title: "Orphan".into(),
                parent: Some(Id::from("gone")),
            }),
        )
        .is_err(),
        "a note's parent exists"
    );

    // Deleting a note deletes the notes under it, and nothing else.
    assert!(
        apply(&mut notebook, Notebook::DELETE.with(a.clone())).is_err(),
        "deleting asks for confirmation"
    );
    notebook
        .apply_confirmed(Notebook::DELETE.with(a.clone()))
        .unwrap();
    assert_eq!(children(&notebook, None), [c, b]);
    assert_eq!(notebook.state().unwrap().pages.len(), 2);
}

/// The title and the place in the tree change only through their verbs;
/// the body and the tags are plain field edits.
#[test]
fn titles_and_parents_change_only_through_verbs() {
    let mut notebook = Replica::<Notebook>::new();
    let a = note(&mut notebook, "A", None);
    let b = note(&mut notebook, "B", None);
    let paths = Notebook::paths().pages.item(&a);
    assert!(edit(&mut notebook, &paths.title.replace("Hacked")).is_err());
    assert!(edit(&mut notebook, &paths.parent.set(Some(b))).is_err());
    apply(
        &mut notebook,
        Notebook::RENAME.with(notebook::Rename {
            page: a.clone(),
            title: " Renamed ".into(),
        }),
    )
    .unwrap();
    assert_eq!(page(&notebook, &a).title, "Renamed");
    edit(
        &mut notebook,
        &paths
            .body
            .perform(&RichTextAction::AppendParagraph {
                text: "Hello".into(),
            })
            .unwrap(),
    )
    .unwrap();
    assert_eq!(texts(&notebook, &a), ["Hello"]);
}

/// Renaming a note relabels every link to it in every note, keeping each
/// link a link to the same note.
#[test]
fn a_rename_relabels_the_links_to_the_note() {
    let mut notebook = Replica::<Notebook>::new();
    let target = note(&mut notebook, "Old", None);
    let source = note(&mut notebook, "Source", None);
    let mut linked = BlockContent::paragraph("", "See Old and Old.");
    linked.marks = vec![
        MarkSpan::new(4..7, Mark::Link(uuid(&target))),
        MarkSpan::new(12..15, Mark::Transclusion(uuid(&target))),
    ];
    write(&mut notebook, &source, &[linked]);
    apply(
        &mut notebook,
        Notebook::RENAME.with(notebook::Rename {
            page: target.clone(),
            title: "Brand new".into(),
        }),
    )
    .unwrap();
    let blocks = page(&notebook, &source).blocks();
    assert_eq!(blocks[0].text, "See Brand new and Brand new.");
    assert_eq!(
        blocks[0].marks,
        [
            MarkSpan::new(4..13, Mark::Link(uuid(&target))),
            MarkSpan::new(18..27, Mark::Transclusion(uuid(&target))),
        ]
    );
}

/// A copy of a note copies the notes under it and their bodies; links
/// between the copied notes lead to the copies, links out stay.
#[test]
fn a_duplicate_copies_the_subtree_and_its_links_follow_the_copy() {
    let mut notebook = Replica::<Notebook>::new();
    let source = note(&mut notebook, "Source", None);
    let child = note(&mut notebook, "Child", Some(&source));
    let outside = note(&mut notebook, "Outside", None);
    let mut linked = BlockContent::paragraph("", "self and out");
    linked.marks = vec![
        MarkSpan::new(0..4, Mark::Link(uuid(&child))),
        MarkSpan::new(9..12, Mark::Link(uuid(&outside))),
    ];
    write(&mut notebook, &source, &[linked]);
    apply(&mut notebook, Notebook::DUPLICATE.with(source.clone())).unwrap();

    let roots = children(&notebook, None);
    assert_eq!(roots.len(), 3);
    assert_eq!(roots[0], source, "the copy goes right after its original");
    let copy = roots[1].clone();
    assert_eq!(page(&notebook, &copy).title, "Source copy");
    let copied_children = children(&notebook, Some(&copy));
    assert_eq!(copied_children.len(), 1);
    let copied_child = &copied_children[0];
    assert_eq!(page(&notebook, copied_child).title, "Child");
    let blocks = page(&notebook, &copy).blocks();
    assert_eq!(blocks[0].text, "self and out");
    assert_eq!(
        blocks[0].marks,
        [
            MarkSpan::new(0..4, Mark::Link(uuid(copied_child))),
            MarkSpan::new(9..12, Mark::Link(uuid(&outside))),
        ]
    );
    assert_eq!(
        page(&notebook, &source).blocks()[0].marks[0],
        MarkSpan::new(0..4, Mark::Link(uuid(&child))),
        "the original is untouched"
    );
}

/// Tags are items with names; giving a note a tag or taking it off is a
/// field edit of the note's tags, the same whatever the order; deleting a
/// tag takes it off every note.
#[test]
fn tags_are_named_items_notes_carry_and_deleting_one_unassigns_it() {
    let mut notebook = Replica::<Notebook>::new();
    let a = note(&mut notebook, "A", None);
    let b = note(&mut notebook, "B", None);
    apply(
        &mut notebook,
        Notebook::NEW_TAG.with(notebook::NewTag {
            name: " Release ".into(),
        }),
    )
    .unwrap();
    let release = notebook.state().unwrap().tag_named("Release").unwrap();
    assert!(
        apply(
            &mut notebook,
            Notebook::NEW_TAG.with(notebook::NewTag {
                name: "Release".into()
            }),
        )
        .is_err(),
        "one tag per name"
    );
    let tags = Notebook::paths().pages.item(&a).tags;
    edit(&mut notebook, &tags.insert(&release)).unwrap();
    edit(&mut notebook, &tags.insert(&release)).unwrap();
    assert_eq!(page(&notebook, &a).tags.len(), 1, "a tag is on a note once");
    apply(
        &mut notebook,
        Notebook::TAG.with(notebook::Tag {
            page: b.clone(),
            name: "Writing".into(),
        }),
    )
    .unwrap();
    let writing = notebook.state().unwrap().tag_named("Writing").unwrap();
    assert!(
        page(&notebook, &b).tags.contains(&writing),
        "tagging names a new tag"
    );
    apply(
        &mut notebook,
        Notebook::TAG.with(notebook::Tag {
            page: b.clone(),
            name: "Release".into(),
        }),
    )
    .unwrap();
    assert_eq!(
        notebook.state().unwrap().tags.len(),
        2,
        "or finds the one named so"
    );

    assert!(
        edit(
            &mut notebook,
            &Notebook::paths()
                .tags
                .item(&release)
                .name
                .set(Atom("  ".into()))
        )
        .is_err(),
        "a tag has a name"
    );
    edit(
        &mut notebook,
        &Notebook::paths()
            .tags
            .item(&release)
            .name
            .set(Atom("Shipped".into())),
    )
    .unwrap();
    notebook
        .apply_confirmed(Notebook::DELETE_TAG.with(release.clone()))
        .unwrap();
    let state = notebook.state().unwrap();
    assert!(state.tags.get(&release).is_none());
    assert!(!page(&notebook, &a).tags.contains(&release));
    assert!(!page(&notebook, &b).tags.contains(&release));
    assert!(page(&notebook, &b).tags.contains(&writing));
    edit(&mut notebook, &tags.remove(&release)).unwrap();
}

/// Two people working on one notebook at once end with the same notebook:
/// each one's notes, moves and typing.
#[test]
fn two_replicas_converge() {
    let mut left = Replica::<Notebook>::new();
    let shared = note(&mut left, "Shared", None);
    write(&mut left, &shared, &[BlockContent::paragraph("", "Start")]);
    let mut right = left.fork();

    let mine = note(&mut left, "Mine", None);
    let theirs = note(&mut right, "Theirs", Some(&shared));
    let block = page(&left, &shared).blocks()[0].id.clone();
    let splice = |at: usize, text: &str| RichTextAction::SpliceText {
        region: RegionKind::Body,
        block: block.clone(),
        at,
        del: 0,
        ins: text.into(),
    };
    let body = Notebook::paths().pages.item(&shared).body;
    edit(&mut left, &body.perform(&splice(0, "L ")).unwrap()).unwrap();
    edit(&mut right, &body.perform(&splice(5, " R")).unwrap()).unwrap();

    left.sync_with(&mut right).unwrap();
    let (l, r) = (left.state().unwrap(), right.state().unwrap());
    assert_eq!(l, r, "both read the same notebook");
    assert!(l.pages.contains(&mine) && l.pages.contains(&theirs));
    assert_eq!(texts(&left, &shared), ["L Start R"]);
    assert_eq!(left.live().unwrap(), &l, "the live state follows the merge");
}

/// Importing Markdown makes a note whose `[[Title]]` links lead to the notes
/// of those titles; importing under an id twice is refused.
#[test]
fn an_import_resolves_links_and_is_not_repeated() {
    let mut notebook = Replica::<Notebook>::new();
    let target = note(&mut notebook, "Target", None);
    let id = Id::from(uuid::Uuid::new_v4().to_string());
    let import = |id: &Id| {
        Notebook::IMPORT_MARKDOWN.with(notebook::ImportMarkdown {
            title: "Imported".into(),
            markdown: "See [[Target]].\n\n- one\n- two\n".into(),
            images: Vec::new(),
            note: Some(id.clone()),
        })
    };
    apply(&mut notebook, import(&id)).unwrap();
    let imported = page(&notebook, &id);
    assert_eq!(imported.title, "Imported");
    assert_eq!(imported.parent, None);
    let blocks = imported.blocks();
    assert_eq!(blocks[0].text, "See Target.");
    assert_eq!(
        blocks[0].marks,
        [MarkSpan::new(4..10, Mark::Link(uuid(&target)))]
    );
    assert_eq!(blocks.len(), 3);
    assert!(apply(&mut notebook, import(&id)).is_err());
    let too_big = "x".repeat(MAX_MARKDOWN_IMPORT_BYTES + 1);
    assert!(
        apply(
            &mut notebook,
            Notebook::IMPORT_MARKDOWN.with(notebook::ImportMarkdown {
                title: "Big".into(),
                markdown: too_big,
                images: Vec::new(),
                note: None,
            }),
        )
        .is_err()
    );
}

/// A picture a note shows is kept by its digest — a small one inside the
/// notebook — and shown in the note; anything but a picture is refused.
#[test]
fn pictures_are_kept_by_digest_and_shown_in_their_note() {
    let mut notebook = Replica::<Notebook>::new();
    let page = note(&mut notebook, "Photos", None);
    let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
    png.extend_from_slice(&[0; 32]);
    apply(
        &mut notebook,
        Notebook::PLACE_IMAGE.with(notebook::PlaceImage {
            page: page.clone(),
            bytes: png.clone(),
            alt: None,
        }),
    )
    .unwrap();
    let digest = blake3::hash(&png).to_hex().to_string();
    let state = notebook.state().unwrap();
    assert_eq!(state.images.get(&digest), Some(&Bytes::of(&png)));
    let snapshot = image_snapshot(&state.images, |_| None);
    assert_eq!(snapshot.get(&format!("asset:{digest}")), Some(&png));
    assert!(
        texts(&notebook, &page)
            .iter()
            .any(|text| text.contains(&format!("asset:{digest}"))),
        "{:?}",
        texts(&notebook, &page)
    );
    assert!(
        apply(
            &mut notebook,
            Notebook::PLACE_IMAGE.with(notebook::PlaceImage {
                page,
                bytes: b"not a picture".to_vec(),
                alt: None,
            }),
        )
        .is_err()
    );
}

/// The notebook publishes each note as an item offering the verbs that act
/// on one note, the notebook's own verbs at its root, and reads as its
/// notes and a note as its blocks.
#[test]
fn the_notebook_publishes_notes_as_items_and_reads_as_its_content() {
    let mut notebook = Replica::<Notebook>::new();
    let a = note(&mut notebook, "Alpha", None);
    write(
        &mut notebook,
        &a,
        &[
            BlockContent::paragraph("", "First line"),
            BlockContent::paragraph("", "Second line"),
        ],
    );
    let tree = notebook.published().unwrap();
    for verb in ["new-note", "new-tag", "import-markdown", "field-edit"] {
        assert!(
            tree.actions.iter().any(|spec| spec.id == verb),
            "the notebook offers {verb}"
        );
    }
    let item = crate::actions::page_resource_id(uuid(&a));
    let published =
        Notebook::child(&notebook.state().unwrap(), &item).expect("the note is published");
    assert_eq!(published.label, "Alpha");
    for verb in [
        "rename",
        "delete",
        "move-note",
        "move-up",
        "move-down",
        "indent",
        "outdent",
        "duplicate",
        "new-child",
        "tag",
        "place-image",
    ] {
        assert!(
            published.actions.iter().any(|spec| spec.id == verb),
            "a note offers {verb}"
        );
    }

    let state = notebook.state().unwrap();
    let whole = Notebook::read_content(&state, notebook.doc(), None).unwrap();
    assert_eq!(whole["pages"][0]["title"], "Alpha");
    assert_eq!(whole["pages"][0]["item"], item);
    // A note reads as Markdown, one line per block, each ending in the id
    // its body's block actions take.
    let one = Notebook::read_content(&state, notebook.doc(), Some(&item)).unwrap();
    let lines: Vec<(&str, &str)> = one["markdown"]
        .as_str()
        .expect("the body as Markdown")
        .lines()
        .map(|line| line.rsplit_once(" ^").expect("a block id on every line"))
        .collect();
    let texts: Vec<&str> = lines.iter().map(|(text, _)| *text).collect();
    assert_eq!(texts, ["First line", "Second line"], "{one}");
    assert_eq!(one["blocks"], 2, "{one}");
    let (second, second_id) = lines[1];
    assert_eq!(second, "Second line");
    assert!(!second_id.is_empty() && second_id != lines[0].1, "{one}");
    // Its body is the rich-text resource published beneath the note, read as
    // the same lines.
    let body = crate::actions::body_resource_id(uuid(&a));
    assert_eq!(one["body"], body.as_str());
    let published_body = Notebook::child(&state, &body).expect("the body is published");
    assert_eq!(published_body.kind, "tilo.rich-text");
    let read_body = Notebook::read_content(&state, notebook.doc(), Some(&body)).unwrap();
    assert_eq!(read_body["markdown"], one["markdown"]);
    assert!(
        Notebook::read_content(&state, notebook.doc(), Some("notes.notebook/pages/gone")).is_err()
    );

    let hits = crate::search::entries(&state);
    assert!(hits.iter().any(|hit| hit.title == "Alpha"));
    assert!(hits.iter().any(|hit| hit.text == "Second line"));
}

/// The field-edit listing stays small with a rich-text body in every note:
/// the body's vocabulary is named by its operations rather than copied, and
/// an agent still builds a rich edit of a note's body from the listing.
#[test]
fn an_agent_edits_a_note_body_from_a_compact_field_edit_listing() {
    let mut notebook = Replica::<Notebook>::new();
    let id = note(&mut notebook, "Plan", None);
    write(
        &mut notebook,
        &id,
        &[BlockContent::paragraph("b1", "the plan")],
    );
    let published = notebook.published().unwrap();
    let spec = published
        .actions
        .iter()
        .find(|spec| spec.id == ui::tilo_resource::FIELD_EDIT)
        .unwrap();
    let params = serde_json::to_string(&spec.params()).unwrap();
    assert!(
        params.len() < 16 * 1024,
        "{} bytes of field-edit params",
        params.len()
    );
    let schema = spec.params()[0].schema.as_deref().unwrap().clone();
    let text = schema.to_string();
    assert!(
        text.contains("`pages.*.body`") && text.contains("SpliceText"),
        "{text}"
    );

    let block = page(&notebook, &id).blocks()[0].id.clone();
    let edit = serde_json::json!({
        "path": [{"field": "pages"}, {"item": id.as_str()}, {"field": "body"}],
        "op": "rich",
        "rich": {"SpliceText": {"region": "Body", "block": block, "at": 0, "del": 0, "ins": "Read "}},
    });
    tilo_actions::validate_json_schema(&edit, &schema).expect("the edit fits the listing");
    let edit: Edit<Notebook> = serde_json::from_value(edit).unwrap();
    self::edit(&mut notebook, &edit).unwrap();
    assert_eq!(texts(&notebook, &id), ["Read the plan"]);
}

/// A note-creating action answers with the note it made; a rename of an
/// existing note and a new tag answer with nothing.
#[test]
fn creating_a_note_reports_its_page_and_renaming_does_not() {
    let mut notebook = Replica::<Notebook>::new();
    let before = notebook.heads();
    let id = note(&mut notebook, "Fresh", None);
    let after = notebook.heads();
    let created = notebook.doc_mut().diff(&before, &after);
    let result = Notebook::created_page(notebook.doc(), "new-note", &created)
        .expect("the answer names the new note");
    assert_eq!(result["page"], uuid(&id).to_string());
    assert_eq!(
        result["resource"],
        crate::actions::page_resource_id(uuid(&id))
    );

    let before = notebook.heads();
    apply(
        &mut notebook,
        Notebook::RENAME.with(notebook::Rename {
            page: id,
            title: "Renamed".into(),
        }),
    )
    .unwrap();
    let after = notebook.heads();
    let renamed = notebook.doc_mut().diff(&before, &after);
    assert_eq!(
        Notebook::created_page(notebook.doc(), "rename", &renamed),
        None
    );

    let before = notebook.heads();
    apply(
        &mut notebook,
        Notebook::NEW_TAG.with(notebook::NewTag {
            name: "Work".into(),
        }),
    )
    .unwrap();
    let after = notebook.heads();
    let tagged = notebook.doc_mut().diff(&before, &after);
    assert_eq!(
        Notebook::created_page(notebook.doc(), "new-tag", &tagged),
        None,
        "a new tag is no note"
    );
}
