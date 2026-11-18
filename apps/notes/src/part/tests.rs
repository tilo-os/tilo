#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion"
)]

//! The notes kept in parts: the verbs carried out as the chain carries them
//! out (each read against the notebook and every part where it is invoked,
//! written into the part holding its note, and what it means for other
//! parts written there), merging, what a part publishes, and what an edit
//! costs as the notebook grows.

use std::collections::HashSet;
use std::rc::Rc;

use tilo_document::RichTextAction;
use tilo_document::action::RegionKind;
use ui::tilo_resource::{
    Bytes, Edit, Fields as _, Id, Invoke, NonBlank, Origin, Replica, Resource,
};
use ui::widget::BlockContent;
use ui::widget::marks::{Mark, MarkSpan};

use super::*;
use crate::model::PageId;
use crate::notebook::tests::MATERIALIZED_BLOCKS;
use crate::notebook::{
    MAX_MARKDOWN_IMPORT_BYTES, NO_TITLE, Notebook, PAGE_KIND, clips_op, image_snapshot,
    note_and_anchor, notebook,
};

/// A notebook and its parts as one device holds them: every action goes to
/// the part the chain sends it to, read against the notebook and the parts
/// held where it is invoked, as a window carries it out.
pub(crate) struct Book {
    notebook: Replica<Notebook>,
    parts: Vec<(u64, Replica<PagePart>)>,
    /// Who acts: what an action naming "mine" reads.
    author: Option<MemberId>,
    /// The part new notes go to, when the test places them rather than the
    /// chain's own rule: notes spread over parts without filling each.
    placing: Option<u64>,
    now_ms: i64,
}

impl Book {
    pub(crate) fn new() -> Self {
        Self {
            notebook: Replica::new(),
            parts: Vec::new(),
            author: None,
            placing: None,
            now_ms: 1_000,
        }
    }

    fn by(mut self, member: MemberId) -> Self {
        self.notebook = self.notebook.as_member(member.clone());
        for (_, part) in &mut self.parts {
            *part = std::mem::replace(part, Replica::new()).as_member(member.clone());
        }
        self.author = Some(member);
        self
    }

    /// The parts as they stand, oldest first.
    pub(crate) fn states(&self) -> Vec<(u64, Rc<PagePart>)> {
        self.parts
            .iter()
            .map(|(index, part)| (*index, Rc::new(part.state().unwrap())))
            .collect()
    }

    /// Every note of every part, as one part reads: what a test about notes
    /// that all fit one part asserts on.
    fn merged(&self) -> PagePart {
        let mut merged = PagePart::default();
        for (_, part) in self.states() {
            for (id, page) in part.pages.iter() {
                merged.pages.push(id.clone(), page.clone());
            }
            for (digest, picture) in part.images.iter() {
                merged.images.insert(digest.clone(), picture.clone());
            }
        }
        merged
    }

    fn tags(&self) -> Notebook {
        self.notebook.state().unwrap()
    }

    /// Part `index`, started (empty) when nobody has.
    fn part(&mut self, index: u64) -> &mut Replica<PagePart> {
        if !self.parts.iter().any(|(held, _)| *held == index) {
            let mut part = Replica::<PagePart>::new();
            if let Some(author) = &self.author {
                part = part.as_member(author.clone());
            }
            part.set_now_ms(self.now_ms);
            self.parts.push((index, part));
            self.parts.sort_by_key(|(held, _)| *held);
        }
        &mut self
            .parts
            .iter_mut()
            .find(|(held, _)| *held == index)
            .unwrap()
            .1
    }

    /// Carry `invoke` out as the chain does; the part it was written to.
    fn apply(&mut self, invoke: impl Into<Invoke<PagePart>>) -> Result<u64, String> {
        let action = invoke.into().build(self.now_ms)?;
        self.apply_action(&action)
    }

    fn apply_action(&mut self, action: &<PagePart as Resource>::Action) -> Result<u64, String> {
        let mut action = action.clone();
        let parts = self.states();
        prepare(&mut action, &self.tags(), &parts, self.author.as_ref())?;
        let index = match (self.placing, &action.operation) {
            (
                Some(placing),
                PagePartAction::NewNote(_)
                | PagePartAction::NewChild(_)
                | PagePartAction::CreatePage(_)
                | PagePartAction::Duplicate(_),
            ) => placing,
            _ => part_for(&action, &parts)?,
        };
        self.part(index)
            .apply_action(&action)
            .map_err(|error| error.to_string())?;
        let parts = self.states();
        for (other, further) in spread(&action, index, &parts) {
            let part = self.part(other);
            tilo_actions::apply_local(part.doc_mut(), &further, crate::actions::encode(&further))
                .map_err(|error| error.to_string())?;
        }
        Ok(index)
    }

    fn edit(&mut self, edit: &Edit<PagePart>) -> Result<u64, String> {
        self.apply(Invoke::edit(edit).map_err(|error| error.to_string())?)
    }

    fn tag_verb(&mut self, invoke: Invoke<Notebook>) -> Result<(), String> {
        self.notebook
            .apply_confirmed(invoke)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn fork(&mut self) -> Self {
        Self {
            notebook: self.notebook.fork(),
            parts: self
                .parts
                .iter_mut()
                .map(|(index, part)| (*index, part.fork()))
                .collect(),
            author: self.author.clone(),
            placing: None,
            now_ms: self.now_ms,
        }
    }

    fn sync_with(&mut self, other: &mut Self) {
        self.notebook.sync_with(&mut other.notebook).unwrap();
        let indexes: HashSet<u64> = self
            .parts
            .iter()
            .chain(&other.parts)
            .map(|(index, _)| *index)
            .collect();
        for index in indexes {
            let mut theirs = std::mem::replace(other.part(index), Replica::new());
            self.part(index).sync_with(&mut theirs).unwrap();
            *other.part(index) = theirs;
        }
    }

    fn workspace(&self) -> crate::Workspace {
        crate::read::workspace(&self.tags(), &self.states())
    }
}

/// A new note titled `title` under `parent`, through the verb.
fn note(book: &mut Book, title: &str, parent: Option<&Id>) -> Id {
    let before: HashSet<Id> = book.merged().pages.ids().cloned().collect();
    book.apply(PagePart::NEW_NOTE.with(page_part::NewNote {
        title: title.to_owned(),
        parent: parent.cloned(),
        note: None,
        rank: None,
    }))
    .unwrap();
    book.merged()
        .pages
        .ids()
        .find(|id| !before.contains(*id))
        .cloned()
        .expect("a new note")
}

/// The notes shown under `parent`, in order.
fn children(book: &Book, parent: Option<&Id>) -> Vec<Id> {
    let parts = book.states();
    Pages::of(&parts)
        .children(parent)
        .map(|(id, _)| id.clone())
        .collect()
}

fn page(book: &Book, id: &Id) -> Page {
    book.merged().pages.get(id).cloned().unwrap()
}

fn texts(book: &Book, id: &Id) -> Vec<String> {
    page(book, id)
        .blocks()
        .into_iter()
        .map(|block| block.text)
        .collect()
}

fn write(book: &mut Book, id: &Id, blocks: &[BlockContent]) {
    let op = clips_op(blocks);
    book.edit(&PagePart::paths().pages.item(id).body.perform(&op).unwrap())
        .unwrap();
}

fn uuid(id: &Id) -> PageId {
    page_of(id).expect("a note's id is a UUID")
}

fn move_up(page: &Id) -> Invoke<PagePart> {
    PagePart::MOVE_UP.with(page_part::MoveUp {
        page: page.clone(),
        place: None,
    })
}

fn move_down(page: &Id) -> Invoke<PagePart> {
    PagePart::MOVE_DOWN.with(page_part::MoveDown {
        page: page.clone(),
        place: None,
    })
}

fn move_note(page: &Id, parent: Option<&Id>, before: Option<&Id>) -> Invoke<PagePart> {
    PagePart::MOVE_NOTE.with(page_part::MoveNote {
        page: page.clone(),
        parent: parent.cloned(),
        before: before.cloned(),
        place: None,
    })
}

fn rename(page: &Id, title: &str) -> Invoke<PagePart> {
    PagePart::RENAME.with(page_part::Rename {
        page: page.clone(),
        title: title.into(),
    })
}

fn import(title: &str, markdown: &str, note: Option<&Id>) -> Invoke<PagePart> {
    PagePart::IMPORT_MARKDOWN.with(page_part::ImportMarkdown {
        title: title.into(),
        markdown: markdown.into(),
        images: Vec::new(),
        note: note.cloned(),
        path: None,
        origin: None,
        reads: None,
    })
}

tilo_test_env::scenario! {
    fn a_part_reads_publishes_and_merges;

    phase an_unnamed_note_is_offered_under_the_word_for_one;
    // A note nobody named is offered under the word for one, never under its own body's first line: a
    // list and the tools that read it aloud then say the same name, and the record holds no title.
    {
        let mut book = Book::new();
        let made = note(&mut book, "", None);
        write(&mut book, &made, &[BlockContent::paragraph("", "Plan")]);
        let offered = book.merged().item_label("pages", &made);
        assert_eq!(offered.as_deref(), Some("Untitled"));
    }

    phase two_replicas_converge;
    // Two people working on one notebook at once end with the same notes:
    // each one's notes, moves and typing.
    {
        let mut left = Book::new();
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
        let body = PagePart::paths().pages.item(&shared).body;
        left.edit(&body.perform(&splice(0, "L ")).unwrap()).unwrap();
        right.edit(&body.perform(&splice(5, " R")).unwrap()).unwrap();

        left.sync_with(&mut right);
        let (l, r) = (left.merged(), right.merged());
        assert_eq!(l, r, "both read the same notes");
        assert!(l.pages.contains(&mine) && l.pages.contains(&theirs));
        assert_eq!(texts(&left, &shared), ["L Start R"]);
    }

    phase pictures_are_kept_by_digest_and_shown_in_their_note;
    // A picture a note shows is kept by its digest — a small one inside the
    // part — and shown in the note; anything but a picture is refused.
    {
        let mut book = Book::new();
        let page_id = note(&mut book, "Photos", None);
        let mut png = vec![137, 80, 78, 71, 13, 10, 26, 10];
        png.extend_from_slice(&[0; 32]);
        book.apply(PagePart::PLACE_IMAGE.with(page_part::PlaceImage {
            page: page_id.clone(),
            bytes: Bytes::to_keep(png.clone()),
            alt: None,
        }))
        .unwrap();
        let digest = blake3::hash(&png).to_hex().to_string();
        let images = images(&book.states());
        assert_eq!(images.get(&digest), Some(&Bytes::of(&png)));
        let snapshot = image_snapshot(&images, &tilo_content::ContentStore::memory());
        assert_eq!(snapshot.get(&format!("asset:{digest}")), Some(&png));
        assert!(
            texts(&book, &page_id)
                .iter()
                .any(|text| text.contains(&format!("asset:{digest}"))),
            "{:?}",
            texts(&book, &page_id)
        );
        assert!(
            book.apply(PagePart::PLACE_IMAGE.with(page_part::PlaceImage {
                page: page_id,
                bytes: Bytes::to_keep(b"not a picture".to_vec()),
                alt: None,
            }))
            .is_err()
        );
    }

    phase a_part_publishes_notes_as_items_and_reads_as_its_content;
    // A part publishes each note as an item offering the verbs that act on
    // one note, and reads as its notes and a note as its blocks.
    {
        let mut book = Book::new();
        let a = note(&mut book, "Alpha", None);
        write(
            &mut book,
            &a,
            &[
                BlockContent::paragraph("", "First line"),
                BlockContent::paragraph("", "Second line"),
            ],
        );
        let tree = book.part(0).published().unwrap();
        for verb in ["new-note", "import-markdown", "field-edit"] {
            assert!(
                tree.actions.iter().any(|spec| spec.id == verb),
                "the notes offer {verb}"
            );
        }
        let state = book.states()[0].1.clone();
        let item = crate::actions::page_resource_id(uuid(&a));
        let published = PagePart::child(&state, &item).expect("the note is published");
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
        for hidden in ["relabel-links", "show-image", "import-notes"] {
            assert!(
                !tree
                    .actions
                    .iter()
                    .chain(&published.actions)
                    .any(|spec| spec.id == hidden),
                "{hidden} is the app's own"
            );
        }

        let doc = book.part(0).doc().clone();
        let whole = PagePart::read_content(&state, &doc, None).unwrap();
        assert_eq!(whole["pages"][0]["title"], "Alpha");
        assert_eq!(whole["pages"][0]["item"], item);
        // A note reads as Markdown, one line per block, each ending in the id
        // its body's block actions take.
        let one = PagePart::read_content(&state, &doc, Some(&item)).unwrap();
        let lines: Vec<(&str, &str)> = one["markdown"]
            .as_str()
            .expect("the body as Markdown")
            .lines()
            .map(|line| line.rsplit_once(" ^").expect("a block id on every line"))
            .collect();
        let texts: Vec<&str> = lines.iter().map(|(text, _)| *text).collect();
        assert_eq!(texts, ["First line", "Second line"], "{one}");
        assert_eq!(one["blocks"], 2, "{one}");
        // Its body is the rich-text resource published beneath the note,
        // read as the same lines.
        let body = crate::actions::body_resource_id(uuid(&a));
        assert_eq!(one["body"], body.as_str());
        let published_body = PagePart::child(&state, &body).expect("the body is published");
        assert_eq!(published_body.kind, "tilo.rich-text");
        let read_body = PagePart::read_content(&state, &doc, Some(&body)).unwrap();
        assert_eq!(read_body["markdown"], one["markdown"]);
        assert!(PagePart::read_content(&state, &doc, Some("notes.pages/pages/gone")).is_err());

        let hits = state.search();
        assert!(hits.iter().any(|hit| hit.title == "Alpha"));
        assert!(hits.iter().any(|hit| hit.text == "Second line"));
    }

    phase an_agent_edits_a_note_body_from_a_compact_field_edit_listing;
    // The field-edit listing stays small with a rich-text body in every note:
    // the body's vocabulary is named by its operations rather than copied, and
    // an agent still builds a rich edit of a note's body from the listing.
    {
        let mut book = Book::new();
        let id = note(&mut book, "Plan", None);
        write(&mut book, &id, &[BlockContent::paragraph("b1", "the plan")]);
        let published = book.part(0).published().unwrap();
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
        let block = page(&book, &id).blocks()[0].id.clone();
        let edit = serde_json::json!({
            "path": [{"field": "pages"}, {"item": id.as_str()}, {"field": "body"}],
            "op": "rich",
            "rich": {"SpliceText": {"region": "Body", "block": block, "at": 0, "del": 0, "ins": "Read "}},
        });
        tilo_actions::validate_json_schema(&edit, &schema).expect("the edit fits the listing");
        let edit: Edit<PagePart> = serde_json::from_value(edit).unwrap();
        book.edit(&edit).unwrap();
        assert_eq!(texts(&book, &id), ["Read the plan"]);
    }
}

tilo_test_env::scenario! {
    fn the_verbs_change_the_notes;

    phase verbs_make_move_and_delete_notes_and_keep_the_tree_a_tree;
    // Notes are made, placed, moved and deleted by verbs that keep the tree a
    // tree: siblings keep their order, a move reorders within its group or
    // files the note under another, and no note goes under itself.
    {
        let mut book = Book::new();
        let a = note(&mut book, "A", None);
        let b = note(&mut book, " B ", None);
        let c = note(&mut book, "", None);
        assert_eq!(children(&book, None), [a.clone(), b.clone(), c.clone()]);
        assert!(page_of(&a).is_some(), "a note is kept under a UUID");
        assert_eq!(page(&book, &b).title, "B", "titles are trimmed");
        assert_eq!(page(&book, &c).title, NO_TITLE);

        book.apply(move_up(&c)).unwrap();
        assert_eq!(children(&book, None), [a.clone(), c.clone(), b.clone()]);
        book.apply(move_down(&a)).unwrap();
        assert_eq!(children(&book, None), [c.clone(), a.clone(), b.clone()]);
        assert!(book.apply(move_up(&c)).is_err(), "the first note cannot move up");
        assert!(book.apply(move_down(&b)).is_err(), "the last note cannot move down");

        // Notes under a note reorder within it.
        let first = note(&mut book, "First", Some(&a));
        let second = note(&mut book, "Second", Some(&a));
        book.apply(move_down(&first)).unwrap();
        assert_eq!(children(&book, Some(&a)), [second.clone(), first.clone()]);
        assert_eq!(children(&book, None), [c.clone(), a.clone(), b.clone()]);

        // Before a sibling, then last again.
        book.apply(move_note(&b, None, Some(&c))).unwrap();
        assert_eq!(children(&book, None), [b.clone(), c.clone(), a.clone()]);
        book.apply(move_note(&b, None, None)).unwrap();
        assert_eq!(children(&book, None), [c.clone(), a.clone(), b.clone()]);

        // Under another note, then out again right after it.
        book.apply(PagePart::INDENT.with(page_part::Indent {
            page: b.clone(),
            place: None,
        }))
        .unwrap();
        assert_eq!(page(&book, &b).parent, Some(a.clone()), "under the note above");
        assert_eq!(children(&book, Some(&a)), [second, first.clone(), b.clone()]);
        let outdent = || {
            PagePart::OUTDENT.with(page_part::Outdent {
                page: b.clone(),
                place: None,
            })
        };
        book.apply(outdent()).unwrap();
        assert_eq!(children(&book, None), [c.clone(), a.clone(), b.clone()]);
        assert!(
            book.apply(outdent()).is_err(),
            "a top-level note has nothing to come out of"
        );

        // No note goes under itself or a note under it.
        for parent in [a.clone(), first] {
            assert!(book.apply(move_note(&a, Some(&parent), None)).is_err());
        }
        assert_eq!(page(&book, &a).parent, None);
        assert!(
            book.apply(PagePart::NEW_NOTE.with(page_part::NewNote {
                title: "Orphan".into(),
                parent: Some(Id::from("gone")),
                note: None,
                rank: None,
            }))
            .is_err(),
            "a note's parent exists"
        );

        // Deleting a note deletes the notes under it, and nothing else.
        book.apply(PagePart::DELETE.with(a.clone())).unwrap();
        assert_eq!(children(&book, None), [c, b]);
        assert_eq!(book.merged().pages.len(), 2);
    }

    phase titles_and_parents_change_only_through_verbs;
    // The title and the place in the tree change only through their verbs;
    // the body and the tags are plain field edits.
    {
        let mut book = Book::new();
        let a = note(&mut book, "A", None);
        let b = note(&mut book, "B", None);
        let paths = PagePart::paths().pages.item(&a);
        assert!(book.edit(&paths.title.replace("Hacked")).is_err());
        assert!(book.edit(&paths.parent.set(Some(b))).is_err());
        assert!(book.edit(&paths.rank.replace("0")).is_err());
        book.apply(rename(&a, " Renamed ")).unwrap();
        assert_eq!(page(&book, &a).title, "Renamed");
        book.edit(
            &paths
                .body
                .perform(&RichTextAction::AppendParagraph {
                    text: "Hello".into(),
                })
                .unwrap(),
        )
        .unwrap();
        assert_eq!(texts(&book, &a), ["Hello"]);
    }

    phase notes_carry_the_notebooks_tags_and_a_deleted_tag_is_not_shown;
    // Giving a note a tag or taking it off is a field edit of the note's
    // tags; tagging by name finds the notebook's tag of that name and is
    // refused for a name it has none of. A deleted tag is shown on no note.
    {
        let mut book = Book::new();
        let a = note(&mut book, "A", None);
        let b = note(&mut book, "B", None);
        book.tag_verb(Notebook::NEW_TAG.with(notebook::NewTag {
            name: "Release".into(),
        }))
        .unwrap();
        let release = book.tags().tag_named("Release").unwrap();
        let tags = PagePart::paths().pages.item(&a).tags;
        book.edit(&tags.insert(&release)).unwrap();
        book.edit(&tags.insert(&release)).unwrap();
        assert_eq!(page(&book, &a).tags.len(), 1, "a tag is on a note once");
        let tag = |name: &str| {
            PagePart::TAG.with(page_part::Tag {
                page: b.clone(),
                name: name.into(),
                tag: None,
            })
        };
        let refused = book.apply(tag("Writing")).unwrap_err();
        assert!(refused.contains("new-tag"), "{refused}");
        book.apply(tag(" Release ")).unwrap();
        assert!(page(&book, &b).tags.contains(&release));

        book.tag_verb(Notebook::DELETE_TAG.with(release.clone()))
            .unwrap();
        let shown = book.workspace();
        assert!(shown.tags().is_empty());
        assert!(shown.data.tag_assignments.is_empty(), "the deleted tag is on no note");
        assert!(
            page(&book, &a).tags.contains(&release),
            "the note itself is not rewritten"
        );
    }

    phase creating_a_note_reports_its_page_and_renaming_does_not;
    // A note-creating action answers with the note it made; a rename of an
    // existing note answers with nothing.
    {
        let mut book = Book::new();
        note(&mut book, "First", None);
        let part = book.part(0);
        let before = part.heads();
        let mut book_after = book;
        let id = note(&mut book_after, "Fresh", None);
        let part = book_after.part(0);
        let after = part.heads();
        let created = part.doc_mut().diff(&before, &after);
        let result = PagePart::created_page(part.doc(), "new-note", &created)
            .expect("the answer names the new note");
        assert_eq!(result["page"], uuid(&id).to_string());
        assert_eq!(result["resource"], crate::actions::page_resource_id(uuid(&id)));

        let before = book_after.part(0).heads();
        book_after.apply(rename(&id, "Renamed")).unwrap();
        let part = book_after.part(0);
        let after = part.heads();
        let renamed = part.doc_mut().diff(&before, &after);
        assert_eq!(PagePart::created_page(part.doc(), "rename", &renamed), None);
    }
}

tilo_test_env::scenario! {
    fn a_rename_or_a_duplicate_carries_the_links_to_the_note;

    phase a_rename_relabels_the_links_to_the_note;
    {
        let mut book = Book::new();
        let target = note(&mut book, "Old", None);
        let source = note(&mut book, "Source", None);
        let mut linked = BlockContent::paragraph("", "See Old and Old.");
        linked.marks = vec![
            MarkSpan::new(4..7, Mark::Link(uuid(&target))),
            MarkSpan::new(12..15, Mark::Transclusion(uuid(&target))),
        ];
        write(&mut book, &source, &[linked]);
        book.apply(rename(&target, "Brand new")).unwrap();
        let blocks = page(&book, &source).blocks();
        assert_eq!(blocks[0].text, "See Brand new and Brand new.");
        assert_eq!(
            blocks[0].marks,
            [
                MarkSpan::new(4..13, Mark::Link(uuid(&target))),
                MarkSpan::new(18..27, Mark::Transclusion(uuid(&target))),
            ]
        );
    }

    phase a_rename_scans_shared_marks_without_materializing_editor_blocks;
    // Only two of 200 pages link to the renamed note. Scanning must not copy
    // their prose into editor blocks, or write unrelated or already-right labels.
    {
        let target = page_id(PageId::from_u128(1));
        let other = PageId::from_u128(300);
        let mut seed = PagePart::default();
        for number in 1..=200 {
            let mut page = Page::titled(if number == 1 { "Old" } else { "Source" }, None);
            page.body = if number == 2 || number == 3 {
                let mut linked = BlockContent::paragraph("", "é Old / Old / Néw title / other");
                linked.marks = vec![
                    MarkSpan::new(3..6, Mark::Link(uuid(&target))),
                    MarkSpan::new(9..12, Mark::Transclusion(uuid(&target))),
                    MarkSpan::new(15..25, Mark::Link(uuid(&target))),
                    MarkSpan::new(28..33, Mark::Link(other)),
                ];
                crate::notebook::written(&[linked])
            } else {
                RichText::from_markdown("Unrelated prose. ".repeat(128))
            };
            seed.pages.push(page_id(PageId::from_u128(number)), page);
        }
        let mut doc = tilo_resource::Doc::new();
        seed.write_fields(&mut doc, &automerge::ROOT).unwrap();
        tilo_resource::evolution::write_version(&mut doc, PagePart::VERSION).unwrap();
        doc.commit();
        let mut book = Book::new();
        book.parts.push((0, Replica::<PagePart>::from_doc(doc)));
        let before = book.merged();
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(book.part(0).doc_mut());
        MATERIALIZED_BLOCKS.with(|count| count.set(0));
        book.apply(rename(&target, "Néw title")).unwrap();
        let materialized = MATERIALIZED_BLOCKS.with(std::cell::Cell::get);
        let delta = tracker.delta(book.part(0).doc_mut());
        let after = book.merged();
        assert_eq!(after.pages.get(&target).unwrap().title, "Néw title");
        for (id, page) in after.pages.iter() {
            let body = page.body.state().unwrap();
            if id == &page_id(PageId::from_u128(2)) || id == &page_id(PageId::from_u128(3)) {
                let blocks = page.blocks();
                assert_eq!(blocks[0].text, "é Néw title / Néw title / Néw title / other");
                assert_eq!(
                    blocks[0].marks,
                    [
                        MarkSpan::new(3..13, Mark::Link(uuid(&target))),
                        MarkSpan::new(16..26, Mark::Transclusion(uuid(&target))),
                        MarkSpan::new(29..39, Mark::Link(uuid(&target))),
                        MarkSpan::new(42..47, Mark::Link(other)),
                    ]
                );
                assert!(delta.touches(&body.containers().root));
            } else {
                assert_eq!(page.body, before.pages.get(id).unwrap().body);
                assert!(!delta.touches(&body.containers().root), "unrelated body {id}");
            }
        }
        assert_eq!(materialized, 0, "rename copied editor blocks across the notebook");
    }

    phase a_rename_does_not_rewrite_labels_already_matching_the_new_title;
    // A link or embed may already carry the new title (for example after a
    // member edited it). Renaming must leave that body's CRDT untouched.
    {
        let mut book = Book::new();
        let target = note(&mut book, "Old", None);
        let source = note(&mut book, "Source", None);
        let mut linked = BlockContent::paragraph("", "Brand new and Brand new");
        linked.marks = vec![
            MarkSpan::new(0..9, Mark::Link(uuid(&target))),
            MarkSpan::new(14..23, Mark::Transclusion(uuid(&target))),
        ];
        write(&mut book, &source, &[linked]);
        let before = page(&book, &source).body;
        let body = body_object(book.part(0).doc(), uuid(&source)).unwrap();
        let mut tracker = tilo_resource::Tracker::new();
        tracker.start(book.part(0).doc_mut());
        book.apply(rename(&target, "Brand new")).unwrap();
        let delta = tracker.delta(book.part(0).doc_mut());
        assert_eq!(page(&book, &target).title, "Brand new");
        assert_eq!(page(&book, &source).body, before);
        assert!(!delta.touches(&body), "already-right labels were rewritten");
    }

    phase a_rename_relabels_the_links_kept_in_other_parts;
    // Renaming a note relabels the links to it in every part holding one, as
    // that part's own change; a part holding none is not written.
    {
        let mut book = Book::new();
        let target = note(&mut book, "Old", None);
        book.placing = Some(1);
        let source = note(&mut book, "Source", None);
        book.placing = Some(2);
        note(&mut book, "Unrelated", None);
        book.placing = None;
        let mut linked = BlockContent::paragraph("", "See Old.");
        linked.marks = vec![MarkSpan::new(4..7, Mark::Link(uuid(&target)))];
        write(&mut book, &source, &[linked]);
        let untouched = book.part(2).heads();
        book.apply(rename(&target, "New")).unwrap();
        let blocks = page(&book, &source).blocks();
        assert_eq!(blocks[0].text, "See New.");
        assert_eq!(blocks[0].marks, [MarkSpan::new(4..7, Mark::Link(uuid(&target)))]);
        assert_eq!(book.part(2).heads(), untouched, "a part without a link is not written");
    }

    phase a_duplicate_copies_the_subtree_and_its_links_follow_the_copy;
    // A copy of a note copies the notes under it and their bodies, wherever
    // each is kept; links between the copied notes lead to the copies, links
    // out stay.
    {
        let mut book = Book::new();
        let source = note(&mut book, "Source", None);
        book.placing = Some(1);
        let child = note(&mut book, "Child", Some(&source));
        let grandchild = note(&mut book, "Grandchild", Some(&child));
        book.placing = None;
        let outside = note(&mut book, "Outside", None);
        let mut linked = BlockContent::paragraph("", "self and out");
        linked.marks = vec![
            MarkSpan::new(0..4, Mark::Link(uuid(&child))),
            MarkSpan::new(9..12, Mark::Link(uuid(&outside))),
        ];
        write(&mut book, &source, &[linked]);
        write(&mut book, &grandchild, &[BlockContent::paragraph("", "deep")]);
        book.apply(PagePart::DUPLICATE.with(page_part::Duplicate {
            page: source.clone(),
            copies: None,
        }))
        .unwrap();

        let roots = children(&book, None);
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0], source, "the copy goes right after its original");
        let copy = roots[1].clone();
        assert_eq!(roots[2], outside);
        assert_eq!(page(&book, &copy).title, "Source copy");
        let copied_children = children(&book, Some(&copy));
        assert_eq!(copied_children.len(), 1);
        let copied_child = &copied_children[0];
        assert_eq!(page(&book, copied_child).title, "Child");
        let copied_grandchildren = children(&book, Some(copied_child));
        assert_eq!(copied_grandchildren.len(), 1);
        assert_eq!(texts(&book, &copied_grandchildren[0]), ["deep"]);
        let blocks = page(&book, &copy).blocks();
        assert_eq!(blocks[0].text, "self and out");
        assert_eq!(
            blocks[0].marks,
            [
                MarkSpan::new(0..4, Mark::Link(uuid(copied_child))),
                MarkSpan::new(9..12, Mark::Link(uuid(&outside))),
            ]
        );
        assert_eq!(
            page(&book, &source).blocks()[0].marks[0],
            MarkSpan::new(0..4, Mark::Link(uuid(&child))),
            "the original is untouched"
        );
    }
}

tilo_test_env::scenario! {
    fn notes_read_across_parts;

    phase siblings_are_ordered_by_rank_whatever_part_keeps_them;
    // Notes beside each other read in the order they were made and moved to,
    // across the parts that keep them.
    {
        let mut book = Book::new();
        let a = note(&mut book, "A", None);
        book.placing = Some(1);
        let b = note(&mut book, "B", None);
        book.placing = Some(2);
        let c = note(&mut book, "C", None);
        book.placing = None;
        assert_eq!(children(&book, None), [a.clone(), b.clone(), c.clone()]);
        book.apply(move_up(&c)).unwrap();
        assert_eq!(children(&book, None), [a.clone(), c.clone(), b.clone()]);
        book.apply(move_note(&a, None, None)).unwrap();
        assert_eq!(children(&book, None), [c, b, a]);
    }

    phase a_note_moves_under_one_kept_in_another_part;
    // A note moves under a note another part keeps, and never under one of
    // its own descendants there.
    {
        let mut book = Book::new();
        let parent = note(&mut book, "Parent", None);
        book.placing = Some(1);
        let child = note(&mut book, "Child", Some(&parent));
        let other = note(&mut book, "Other", None);
        book.placing = None;
        book.apply(move_note(&other, Some(&child), None)).unwrap();
        assert_eq!(children(&book, Some(&child)), std::slice::from_ref(&other));
        assert_eq!(page(&book, &other).parent, Some(child));
        let refused = book.apply(move_note(&parent, Some(&other), None));
        assert!(refused.is_err(), "a note cannot go under its own descendant");
        book.apply(PagePart::INDENT.with(page_part::Indent {
            page: parent.clone(),
            place: None,
        }))
        .unwrap_err();
    }

    phase a_deleted_note_hides_the_notes_under_it_in_other_parts;
    // Deleting a note removes the notes under it that its part keeps; those
    // other parts keep are not shown, by the sidebar, search or a read.
    {
        let mut book = Book::new();
        let parent = note(&mut book, "Parent", None);
        let near = note(&mut book, "Near", Some(&parent));
        book.placing = Some(1);
        let far = note(&mut book, "Far", Some(&parent));
        let deeper = note(&mut book, "Deeper", Some(&far));
        let kept = note(&mut book, "Kept", None);
        book.placing = None;
        book.apply(PagePart::DELETE.with(parent.clone())).unwrap();
        assert!(!book.merged().pages.contains(&near), "deleted with it");
        assert!(book.merged().pages.contains(&far), "kept in its part");
        let parts = book.states();
        let shown = Pages::of(&parts);
        assert!(!shown.contains(&far) && !shown.contains(&deeper));
        assert_eq!(children(&book, None), [kept]);
        let gone = hidden(&parts);
        assert!(gone.contains(&crate::actions::page_resource_id(uuid(&far))));
        assert!(gone.contains(&crate::actions::page_resource_id(uuid(&deeper))));
        let titles: Vec<String> = book
            .workspace()
            .pages()
            .iter()
            .map(|page| page.title.clone())
            .collect();
        assert_eq!(titles, ["Kept"]);
    }
}

/// The notes in the chain's head part as they stand.
fn chain_titles(chain: &ui::framework::Chain<PagePart>) -> Vec<String> {
    let parts = chain.parts();
    Pages::of(&parts)
        .iter()
        .map(|(_, page)| page.title.clone())
        .collect()
}

#[test]
fn undoing_a_delete_brings_back_the_notes_under_it_kept_elsewhere() {
    let chain = ui::framework::Chain::<PagePart>::detached(
        ui::apps::NOTES,
        &MemberId::person("ada"),
        || Rc::new(Notebook::default()),
    )
    .unwrap();
    let make = |title: &str, parent: Option<&Id>| {
        chain
            .invoke(&PagePart::NEW_NOTE.with(page_part::NewNote {
                title: title.into(),
                parent: parent.cloned(),
                note: None,
                rank: None,
            }))
            .unwrap()
            .unwrap()
    };
    let id_of = |title: &str| {
        let parts = chain.parts();
        Pages::of(&parts)
            .iter()
            .find(|(_, page)| page.title == title)
            .map(|(id, _)| id.clone())
            .unwrap()
    };
    assert_eq!(make("Parent", None), 0);
    let parent = id_of("Parent");
    for filler in 1..PART_PAGES {
        make(&format!("Filler {filler}"), None);
    }
    assert_eq!(make("Child", Some(&parent)), 1, "the next part takes it");
    let child = id_of("Child");

    chain
        .invoke(&PagePart::DELETE.with(parent.clone()))
        .unwrap();
    assert!(!chain_titles(&chain).contains(&"Child".to_owned()));
    chain.undo_in(0).unwrap();
    let parts = chain.parts();
    let shown = Pages::of(&parts);
    assert!(shown.contains(&parent));
    assert!(shown.contains(&child), "the note under it is shown again");
    assert_eq!(shown.parent_of(&child), Some(&parent));
}

#[test]
fn renaming_relabels_through_the_chain_in_another_part() {
    let chain = ui::framework::Chain::<PagePart>::detached(
        ui::apps::NOTES,
        &MemberId::person("ada"),
        || Rc::new(Notebook::default()),
    )
    .unwrap();
    chain
        .invoke(&PagePart::NEW_NOTE.with(page_part::NewNote {
            title: "Old".into(),
            parent: None,
            note: None,
            rank: None,
        }))
        .unwrap();
    let target = Pages::of(&chain.parts()).iter().next().unwrap().0.clone();
    for filler in 1..PART_PAGES {
        chain
            .invoke(&PagePart::NEW_NOTE.with(page_part::NewNote {
                title: format!("Filler {filler}"),
                parent: None,
                note: None,
                rank: None,
            }))
            .unwrap();
    }
    chain
        .invoke(&PagePart::CREATE_PAGE.with(page_part::CreatePage {
            title: NonBlank::parse("Source").unwrap(),
            markdown: Some("See [[Old]].".into()),
            origin: None,
            rank: None,
            links: None,
        }))
        .unwrap();
    assert_eq!(chain.head(), Some(1));
    chain.invoke(&rename(&target, "New")).unwrap();
    let parts = chain.parts();
    let pages = Pages::of(&parts);
    let source = pages
        .iter()
        .find(|(_, page)| page.title == "Source")
        .unwrap();
    assert_eq!(pages.part_of(source.0), Some(1));
    let blocks = source.1.blocks();
    assert_eq!(blocks[0].text, "See New.");
    assert_eq!(blocks[0].marks[0].mark, Mark::Link(uuid(&target)));
}

tilo_test_env::scenario! {
    fn imported_links_survive_import_and_export;

    phase an_import_resolves_links_and_is_not_repeated;
    // Importing Markdown makes a note whose `[[Title]]` links lead to the notes
    // of those titles; importing under an id twice is refused.
    {
        let mut book = Book::new();
        book.placing = Some(1);
        let target = note(&mut book, "Target", None);
        book.placing = None;
        let id = Id::from(uuid::Uuid::new_v4().to_string());
        book.apply(import("Imported", "See [[Target]].\n\n- one\n- two\n", Some(&id)))
            .unwrap();
        let imported = page(&book, &id);
        assert_eq!(imported.title, "Imported");
        assert_eq!(imported.parent, None);
        let blocks = imported.blocks();
        assert_eq!(blocks[0].text, "See Target.");
        assert_eq!(blocks[0].marks, [MarkSpan::new(4..10, Mark::Link(uuid(&target)))]);
        assert_eq!(blocks.len(), 3);
        assert!(book.apply(import("Imported", "Again", Some(&id))).is_err());
        let too_big = "x".repeat(MAX_MARKDOWN_IMPORT_BYTES + 1);
        assert!(book.apply(import("Big", &too_big, None)).is_err());
    }

    phase importing_a_formatted_link_preserves_label_text_and_marks_on_export;
    {
        let mut book = Book::new();
        let id = Id::from(uuid::Uuid::new_v4().to_string());
        let markdown = "See [Read **日本** now](https://example.com).";
        book.apply(import("Linked note", markdown, Some(&id))).unwrap();
        let blocks = page(&book, &id).blocks();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].text, "See Read 日本 now.");
        assert_eq!(
            blocks[0].marks,
            [
                MarkSpan::new(4..19, Mark::ExternalLink("https://example.com".into())),
                MarkSpan::new(9..15, Mark::Bold),
            ]
        );
        assert_eq!(
            crate::markdown::serialize_inline(&crate::markdown::InlineText::new(
                blocks[0].text.clone(),
                blocks[0].marks.clone(),
            )),
            markdown
        );
    }

    phase exporting_imported_links_preserves_shared_start_mark_nesting;
    {
        for markdown in [
            "[**Bold** normal](https://example.com)",
            "[*Italic* normal](https://example.com)",
            "[**Bold *nested* text** normal](https://example.com)",
            "**[Bold](https://example.com) normal**",
            "**[Bold](https://example.com)**",
            "[normal **Bold**](https://example.com)",
        ] {
            let mut book = Book::new();
            let id = Id::from(uuid::Uuid::new_v4().to_string());
            book.apply(import("Formatting", markdown, Some(&id))).unwrap();
            let exported = crate::actions::page_markdown(&book.workspace(), uuid(&id));
            assert_eq!(exported.trim_end(), markdown);
            assert_eq!(crate::markdown::parse(&exported), crate::markdown::parse(markdown));
        }
    }

    phase importing_a_link_keeps_balanced_parentheses_in_its_destination;
    {
        let mut book = Book::new();
        for url in [
            "https://example.com/wiki/Function_(mathematics)",
            "https://example.com/a_(b_(c))?q=1",
        ] {
            let id = Id::from(uuid::Uuid::new_v4().to_string());
            let markdown = format!("See [reference]({url}).");
            book.apply(import("Reference", &markdown, Some(&id))).unwrap();
            let blocks = page(&book, &id).blocks();
            assert_eq!(blocks.len(), 1);
            assert_eq!(blocks[0].text, "See reference.");
            assert_eq!(blocks[0].marks, [MarkSpan::new(4..13, Mark::ExternalLink(url.into()))]);
            assert_eq!(
                crate::markdown::serialize_inline(&crate::markdown::InlineText::new(
                    blocks[0].text.clone(),
                    blocks[0].marks.clone(),
                )),
                markdown
            );
        }
    }
}

/// A verb on a note names it in its change by one line of its title: the change does not
/// grow with the title.
#[test]
fn a_change_names_its_note_by_one_line() {
    let added_inside = |title: &str| {
        let mut book = Book::new();
        let page = note(&mut book, title, None);
        let part = book
            .apply(PagePart::NEW_CHILD.with(page_part::NewChild {
                page,
                title: "Inside".to_owned(),
                rank: None,
            }))
            .unwrap();
        let changes = book.part(part).doc_mut().get_changes(&[]);
        let change = changes.last().unwrap();
        let label = change
            .message()
            .and_then(tilo_actions::ActionContext::from_change_message)
            .and_then(|entry| entry.item)
            .map(|item| item.label)
            .unwrap();
        (label, change.raw_bytes().len())
    };
    let (label, short) = added_inside("Plan");
    assert_eq!(label, "Plan");
    let long = "Read the plan ".repeat(4_000);
    let (label, bytes) = added_inside(&long);
    assert_eq!(label.chars().count(), 96, "{label}");
    assert!(long.starts_with(&label), "{label}");
    assert!(
        bytes.saturating_sub(short) < 200,
        "a note inside one titled in {} bytes is a change of {bytes} bytes; inside one titled in \
         4 bytes, {short}",
        long.len()
    );
}

/// The note titled `title`.
fn titled(book: &Book, title: &str) -> Id {
    book.merged()
        .pages
        .iter()
        .find(|(_, page)| page.title == title)
        .map(|(id, _)| id.clone())
        .expect("the note is there")
}

fn create_page(book: &mut Book, title: &str, markdown: &str) -> Id {
    book.apply(PagePart::CREATE_PAGE.with(page_part::CreatePage {
        title: NonBlank::parse(title).unwrap(),
        markdown: Some(markdown.into()),
        origin: None,
        rank: None,
        links: None,
    }))
    .unwrap();
    titled(book, title)
}

fn append(title: Option<&str>, markdown: &str) -> Invoke<PagePart> {
    PagePart::APPEND_TO_NOTE.with(page_part::AppendToNote {
        title: title.map(str::to_owned),
        markdown: markdown.into(),
        note: None,
        links: None,
    })
}

/// The note and heading anchor a heading link's address names.
fn heading_link(mark: &Mark) -> Option<(PageId, Option<String>)> {
    let Mark::ResourceLink(href) = mark else {
        return None;
    };
    let address = crate::actions::resource_address_in(href)?;
    assert_eq!(address.kind, PAGE_KIND);
    let (page, anchor) = note_and_anchor(&address.id)?;
    Some((page, anchor.map(str::to_owned)))
}

#[test]
fn wrapped_lines_read_as_one_paragraph_and_hard_breaks_stay() {
    let mut book = Book::new();
    let id = create_page(
        &mut book,
        "Wrapped",
        "A paragraph wrapped\nover two lines.\n- An item\n- Next\n\nHard  \nbreak\n\n> A quote\n> wrapped\n",
    );
    assert_eq!(
        texts(&book, &id),
        [
            "A paragraph wrapped over two lines.",
            "An item",
            "Next",
            "Hard\nbreak",
            "A quote wrapped",
        ]
    );
    // Appending reads Markdown the same way.
    book.apply(append(
        Some("Wrapped"),
        "More text\nwrapped [here](#more).\n\n## More\n",
    ))
    .unwrap();
    let blocks = page(&book, &id).blocks();
    let added = blocks
        .iter()
        .find(|block| block.text == "More text wrapped here.")
        .unwrap();
    assert_eq!(
        heading_link(&added.marks[0].mark),
        Some((uuid(&id), Some("more".to_owned())))
    );
    assert_eq!(blocks.last().unwrap().text, "More");
}

#[test]
fn appending_names_its_note_by_title_or_the_one_its_author_made() {
    let mut book = Book::new().by(MemberId::person("ada"));
    let mine = create_page(&mut book, "Mine", "Start");
    book.placing = Some(1);
    let theirs = note(&mut book, "Plan", None);
    book.placing = None;
    book.apply(append(Some("plan"), "From afar")).unwrap();
    assert_eq!(texts(&book, &theirs), ["From afar"]);
    let refused = book.apply(append(Some("Nothing"), "x")).unwrap_err();
    assert!(refused.contains("no note is titled"), "{refused}");
    let refused = book.apply(append(None, "x")).unwrap_err();
    assert!(
        refused.contains("you made"),
        "two of the author's notes: {refused}"
    );
    book.apply(PagePart::DELETE.with(theirs)).unwrap();
    book.apply(append(None, "More")).unwrap();
    assert_eq!(texts(&book, &mine), ["Start", "More"]);
}

#[test]
fn a_link_to_a_heading_leads_to_it() {
    let mut book = Book::new();
    let id = create_page(
        &mut book,
        "Guide",
        "Press [Undo](#undo) to go back.\n\n## Undo\n\nIt takes back the last step.\n",
    );
    let blocks = page(&book, &id).blocks();
    assert_eq!(
        blocks[0].text, "Press Undo to go back.",
        "never the literal Markdown"
    );
    assert_eq!(blocks[0].marks.len(), 1);
    assert_eq!(blocks[0].marks[0].range, 6..10);
    assert_eq!(
        heading_link(&blocks[0].marks[0].mark),
        Some((uuid(&id), Some("undo".to_owned())))
    );
    assert_eq!(crate::markdown::heading_anchor(&blocks[1].text), "undo");
}

#[test]
fn files_imported_together_link_to_each_other_and_their_headings() {
    let mut book = Book::new();
    let file = |path: &str, markdown: &str| MarkdownFile {
        path: path.into(),
        title: None,
        markdown: markdown.into(),
        origin: None,
    };
    book.apply(PagePart::IMPORT_MARKDOWN_FILES.with(page_part::ImportMarkdownFiles {
        files: vec![
            file(
                "docs/intro.md",
                "# Intro\n\nSee [setup](guide/setup.md#install-it), [the guide](./guide/setup.md) and [elsewhere](../outside.md).\n",
            ),
            file(
                "docs/guide/setup.md",
                "# Setup\n\nBack to the [intro](../intro.md).\n\n## Install it\n\nRun it.\n",
            ),
        ],
        ranks: None,
        links: None,
    }))
    .unwrap();
    let intro = titled(&book, "Intro");
    let setup = titled(&book, "Setup");
    assert_eq!(children(&book, None), [intro.clone(), setup.clone()]);
    assert_eq!(
        page(&book, &setup).path.as_deref(),
        Some("docs/guide/setup.md")
    );
    let blocks = page(&book, &intro).blocks();
    assert_eq!(blocks[0].text, "See setup, the guide and elsewhere.");
    let marks: Vec<_> = blocks[0]
        .marks
        .iter()
        .map(|span| span.mark.clone())
        .collect();
    assert_eq!(
        marks.len(),
        2,
        "a link to a file outside the set keeps only its words: {marks:?}"
    );
    assert_eq!(
        heading_link(&marks[0]),
        Some((uuid(&setup), Some("install-it".to_owned())))
    );
    assert_eq!(marks[1], Mark::Link(uuid(&setup)));
    let back = page(&book, &setup).blocks();
    assert_eq!(back[0].marks[0].mark, Mark::Link(uuid(&intro)));

    // A file imported later links to one imported before by its path, kept
    // in another part.
    book.placing = Some(1);
    book.apply(PagePart::IMPORT_MARKDOWN.with(page_part::ImportMarkdown {
        title: "Later".into(),
        markdown: "Read the [setup](guide/setup.md).".into(),
        images: Vec::new(),
        note: None,
        path: Some("docs/later.md".into()),
        origin: None,
        reads: None,
    }))
    .unwrap();
    let later = titled(&book, "Later");
    assert_eq!(
        page(&book, &later).blocks()[0].marks[0].mark,
        Mark::Link(uuid(&setup))
    );
}

#[test]
fn a_moved_note_keeps_its_origin_and_who_moved_it() {
    let mut book = Book::new();
    book.notebook = book.notebook.fork().as_agent_for("grant-a8a42b74", "ada");
    book.part(0);
    let (index, part) = book.parts.remove(0);
    book.parts.push((
        index,
        part.as_agent_for("grant-a8a42b74", "ada")
            .named("Company mover"),
    ));
    let origin = Origin {
        author: "Jane Doe".into(),
        handle: Some("jane".into()),
        source: "Notion".into(),
        url: Some("https://docs.example.com/doc/handbook".into()),
        reference: None,
        created_ms: Some(1_741_000_000_000),
        updated_ms: None,
    };
    book.apply(PagePart::CREATE_PAGE.with(page_part::CreatePage {
        title: NonBlank::parse("Handbook").unwrap(),
        markdown: Some("Welcome.".into()),
        origin: Some(origin.clone()),
        rank: None,
        links: None,
    }))
    .unwrap();
    let handbook = page(&book, &titled(&book, "Handbook"));
    assert_eq!(handbook.origin, Some(origin));
    assert_eq!(
        handbook.creator,
        Some(ui::tilo_resource::MemberId::agent("grant-a8a42b74"))
    );
    assert_eq!(handbook.creator_name, "Company mover");
}

#[test]
fn every_part_action_publishes_what_it_reads() {
    let mut book = Book::new();
    create_page(&mut book, "One", "Text");
    let unstated =
        ui::tilo_resource::schema_check::unstated_arguments::<PagePart>(&book.states()[0].1);
    assert!(unstated.is_empty(), "{unstated:#?}");
}

/// Blocks read again to show a keystroke typed into the middle of a note of
/// `paragraphs` paragraphs.
fn read_for_a_keystroke(paragraphs: usize) -> usize {
    use std::fmt::Write as _;
    let mut book = Book::new();
    let page = crate::PageId::new_v4();
    book.apply_action(&crate::actions::create_page_action(page, None, "Long"))
        .unwrap();
    let mut body = String::new();
    for index in 0..paragraphs {
        write!(body, "Paragraph {index} of a long note.\n\n").unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    book.apply_action(&crate::actions::write_body_action(page, &blocks))
        .unwrap();
    let shown = crate::Workspace::of(
        &Notebook::default(),
        &[(0, Rc::new(book.part(0).live().unwrap().clone()))],
        &crate::Workspace::default(),
    );
    let middle = paragraphs / 2;
    let splice = RichTextAction::SpliceText {
        region: RegionKind::Body,
        block: shown.bodies[&page][middle].id.clone(),
        at: 0,
        del: 0,
        ins: "a".into(),
    };
    book.apply_action(&crate::actions::edit_body_action(page, &splice))
        .unwrap();
    let live = Rc::new(book.part(0).live().unwrap().clone());
    MATERIALIZED_BLOCKS.with(|count| count.set(0));
    let typed = crate::Workspace::of(&Notebook::default(), &[(0, live)], &shown);
    let read = MATERIALIZED_BLOCKS.with(std::cell::Cell::get);
    assert_eq!(typed.changed, vec![page]);
    assert!(typed.bodies[&page][middle].text.starts_with("aParagraph"));
    for at in [0, paragraphs - 1] {
        assert!(
            std::sync::Arc::ptr_eq(&typed.bodies[&page][at], &shown.bodies[&page][at]),
            "a paragraph the keystroke left alone is shared, not copied"
        );
    }
    read
}

#[test]
fn a_keystroke_reads_the_paragraph_it_changed_not_the_note() {
    let short = read_for_a_keystroke(60);
    let long = read_for_a_keystroke(600);
    assert_eq!(short, 1, "the typed paragraph alone is read again");
    assert_eq!(short, long);
}

/// What one more edit of a note costs does not grow with the notes the
/// notebook already has: the part it writes holds at most a part's worth, so
/// writing into a notebook ten times the size writes one part, the same bytes,
/// in the same time.
#[test]
fn an_edit_costs_the_same_in_a_notebook_ten_times_the_size() {
    /// Fill the notebook up to `count` notes, a part at a time: written
    /// straight into the parts, as a sync brings them.
    fn fill(book: &mut Book, count: usize) {
        let mut made: usize = book
            .parts
            .iter()
            .map(|(_, part)| part.state().unwrap().pages.len())
            .sum();
        while made < count {
            let index = u64::try_from(made / PART_PAGES).unwrap();
            let end = count.min((made / PART_PAGES + 1) * PART_PAGES);
            let part = book.part(index);
            for at in made..end {
                let mut action = crate::actions::create_page_action(
                    PageId::new_v4(),
                    None,
                    &format!("Note {at}"),
                );
                if let PagePartAction::NewNote(new) = &mut action.operation {
                    new.rank = Some(format!("{at:08}"));
                }
                part.apply_action(&action).unwrap();
            }
            made = end;
        }
    }
    /// The parts one edit of the first note writes, its bytes, the size of
    /// the part it is written to, saved, and the time it took.
    fn edit(book: &mut Book) -> (usize, usize, usize, std::time::Duration) {
        let first = titled(book, "Note 0");
        let before: Vec<_> = book
            .parts
            .iter_mut()
            .map(|(index, part)| (*index, part.heads()))
            .collect();
        let began = std::time::Instant::now();
        let index = book
            .edit(
                &PagePart::paths()
                    .pages
                    .item(&first)
                    .body
                    .perform(&RichTextAction::AppendParagraph {
                        text: "Looked at it".into(),
                    })
                    .unwrap(),
            )
            .unwrap();
        let took = began.elapsed();
        let written = book
            .parts
            .iter_mut()
            .map(|(index, part)| (*index, part.heads()))
            .filter(|(index, heads)| {
                before
                    .iter()
                    .find(|(held, _)| held == index)
                    .is_none_or(|(_, before)| before != heads)
            })
            .count();
        let part = book.part(index);
        let change = part
            .doc_mut()
            .get_last_local_change()
            .map(|change| change.raw_bytes().len())
            .unwrap();
        (written, change, part.save().len(), took)
    }
    let small = 100;
    let mut book = Book::new();
    fill(&mut book, small);
    let (written_small, change_small, record_small, took_small) = edit(&mut book);
    let mut book = Book::new();
    fill(&mut book, small * 10);
    let (written_large, change_large, record_large, took_large) = edit(&mut book);
    eprintln!(
        "an edit at {small} notes: {written_small} part written, {change_small} bytes into a \
         {record_small}-byte part in {took_small:?}; at {} notes: {written_large} part written, \
         {change_large} bytes into a {record_large}-byte part in {took_large:?}",
        small * 10
    );
    assert_eq!(book.parts.len(), 5);
    assert_eq!(
        (written_small, written_large),
        (1, 1),
        "one part is written"
    );
    assert!(
        change_large <= change_small + change_small / 10,
        "an edit is the same size: {change_small} then {change_large} bytes"
    );
}
