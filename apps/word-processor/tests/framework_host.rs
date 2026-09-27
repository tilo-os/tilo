//! The word processor on the app framework: the real component runtime,
//! record storage, permissions, per-author undo and History mode, driven the
//! way a person drives the editor — by typing into it.

use qdoc_rich_text::RichText;
use ui::framework::testing::TestApp;
use ui::permissions::Role;
use word_processor::framework::WordProcessorApp;

fn paragraphs(app: &TestApp<WordProcessorApp>) -> Vec<String> {
    app.read_document(|doc| {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.text)
            .collect()
    })
}

fn open() -> TestApp<WordProcessorApp> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<WordProcessorApp>::new().unwrap()
}

#[test]
fn a_keystroke_is_an_action_and_typing_undoes_a_word_at_a_time() {
    let mut app = open();
    app.harness().type_text("hello world");
    assert_eq!(paragraphs(&app), ["hello world"]);
    let typed = app.versions().len();
    assert!(typed > 1, "every keystroke is an attributed change");
    let last = app.versions().pop().unwrap();
    let context = last.context.as_ref().unwrap();
    assert_eq!(context.action_label, "Splice Text");
    assert_eq!(
        context
            .resource
            .as_ref()
            .map(|resource| resource.kind.as_str()),
        Some("qdoc.rich-text")
    );

    // A word is one step: undo takes back "world", redo restores it, and a
    // second undo takes back "hello ".
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["hello "]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app), ["hello world"]);
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(paragraphs(&app), [""]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app), ["hello "]);
}

#[test]
fn enter_splits_the_paragraph_as_its_own_step() {
    let mut app = open();
    app.harness().type_text("first");
    app.harness().key("Enter");
    app.harness().type_text("second");
    assert_eq!(paragraphs(&app), ["first", "second"]);
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["first", ""]);
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["first"]);
}

#[test]
fn a_viewer_is_refused_and_the_document_is_untouched() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::with_role(Role::Viewer).unwrap();
    let before = app.versions().len();
    app.harness().type_text("sneaky");
    app.harness().key("Enter");
    assert_eq!(paragraphs(&app), [""]);
    assert_eq!(app.versions().len(), before, "nothing was written");
}

#[test]
fn history_draws_a_past_version_with_the_editors_own_layout() {
    let mut app = open();
    app.harness().type_text("First draft");
    app.flush().unwrap();
    let past = app.read_document(|doc| doc.clone().get_heads());
    app.harness().key("Enter");
    app.harness().type_text("Added later");
    assert_eq!(paragraphs(&app), ["First draft", "Added later"]);

    let version = app.past_version(&past).unwrap();
    let shown = version.texts().join("\n");
    assert!(shown.contains("First draft"), "{shown}");
    assert!(!shown.contains("Added later"), "{shown}");
    assert!(version.size().width() > 0.0, "the page was laid out");
    assert_eq!(version.state().body().len(), 1);
}

#[test]
fn the_document_survives_a_relaunch() {
    let mut app = open();
    app.harness().type_text("kept");
    let app = app.reopen().unwrap();
    assert_eq!(paragraphs(&app), ["kept"]);
    assert_eq!(
        <RichText as ui::tilo_resource::Resource>::KIND,
        "qdoc.rich-text"
    );
}

#[test]
fn one_apply_creates_a_filled_table_and_one_undo_takes_it_back() {
    use qdoc_rich_text::action::insert_filled_table_action;
    let mut app = open();
    app.harness().type_text("These are the people");
    app.flush().unwrap();
    let after = app.read_document(|doc| {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::blocks_in_order(doc, &c).remove(0)
    });
    let cells: Vec<Vec<String>> = [["Name", "Role"], ["Priya Natarajan", "Product lead"]]
        .iter()
        .map(|row| row.iter().map(|&text| text.to_owned()).collect())
        .collect();
    let versions = app.versions().len();
    app.invoke_confirmed(
        ui::tilo_resource::Invoke::<RichText>::apply(&insert_filled_table_action(&after, cells))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(app.versions().len(), versions + 1, "one change");
    assert_eq!(
        paragraphs(&app),
        [
            "These are the people",
            "Name",
            "Role",
            "Priya Natarajan",
            "Product lead"
        ]
    );
    let kinds = app.read_document(|doc| {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.kind)
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds, ["paragraph", "cell", "cell", "cell", "cell"]);

    // One step: the whole table, cells and text, goes with one undo.
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["These are the people"]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app).len(), 5);
}

/// A record holding `paragraphs` paragraphs, pasted through the editor's own
/// paste action.
fn long_document(paragraphs: usize) -> TestApp<WordProcessorApp> {
    use qdoc_rich_text::ClipBlock;
    use qdoc_rich_text::action::{RegionKind, RichTextAction};
    let mut app = open();
    let blocks: Vec<ClipBlock> = (0..paragraphs)
        .map(|n| ClipBlock {
            kind: if n % 40 == 0 { "heading" } else { "paragraph" }.into(),
            text: format!("Paragraph {n}: the rehearsal ran long and the doors opened late."),
            marks: Vec::new(),
            level: (n % 40 == 0).then_some(2),
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::default(),
        })
        .collect();
    let first = app.read_document(|doc| {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::blocks_in_order(doc, &c).remove(0)
    });
    let paste = RichTextAction::PasteFragment {
        region: RegionKind::Body,
        start_block: first.clone(),
        start: 0,
        end_block: first,
        end: 0,
        blocks,
        adopt_first_style: false,
    };
    app.invoke_confirmed(ui::tilo_resource::Invoke::<RichText>::apply(&paste).unwrap())
        .unwrap();
    app
}

/// The median time of a run of keystrokes.
fn keystroke_cost(app: &mut TestApp<WordProcessorApp>) -> std::time::Duration {
    app.harness().type_text("warm ");
    let mut costs: Vec<_> = "typing along"
        .chars()
        .map(|ch| {
            let started = std::time::Instant::now();
            app.harness().key(&ch.to_string());
            started.elapsed()
        })
        .collect();
    costs.sort();
    costs[costs.len() / 2]
}

#[test]
fn a_keystroke_in_a_long_document_costs_what_it_costs_in_a_short_one() {
    let mut short = long_document(200);
    let mut long = long_document(4_000);
    let short_cost = keystroke_cost(&mut short);

    // Typing and Enter in the long document never list or sort its
    // paragraphs, and read only the paragraphs around the page on screen.
    let scans = qdoc::model::ordered_keys_scan_count();
    let long_cost = keystroke_cost(&mut long);
    long.harness().key("Enter");
    long.harness().type_text("new line");
    assert_eq!(
        qdoc::model::ordered_keys_scan_count(),
        scans,
        "a keystroke listed the document's paragraphs"
    );
    let text = paragraphs(&long);
    assert_eq!(text.len(), 4_001, "{:?}", &text[..3]);
    assert_eq!(text[0], "warm typing along");
    assert!(
        text[1].starts_with("new lineParagraph 0"),
        "{:?}",
        &text[..3]
    );

    // Cost independent of size: twenty times the paragraphs, well under
    // twice the time (slack for a loaded machine).
    assert!(
        long_cost < short_cost * 3,
        "a keystroke cost {long_cost:?} in 4000 paragraphs against {short_cost:?} in 200"
    );
}

/// The document as stored, formatting included.
fn snapshot(app: &TestApp<WordProcessorApp>) -> qdoc::DocSnapshot {
    app.read_document(|doc| {
        let c = qdoc::model::open(doc, &automerge::ROOT).unwrap();
        qdoc::model::materialize(doc, &c)
    })
}

#[test]
fn undoing_a_mark_over_several_paragraphs_clears_it_from_every_one() {
    use qdoc_rich_text::action::{MarkVal, RegionKind, RichTextAction, Span};
    use ui::tilo_resource::Invoke;
    let mut app = open();
    let load = RichTextAction::ReplaceBody {
        markdown: "Background\n\nMethod\n\nResults\n".into(),
    };
    app.invoke_confirmed(Invoke::<RichText>::apply(&load).unwrap())
        .unwrap();
    let before = snapshot(&app);
    let spans = before
        .blocks
        .iter()
        .map(|block| Span {
            block: block.id.clone(),
            start: 0,
            end: block.text.chars().count(),
        })
        .collect();
    let bold = RichTextAction::SetMark {
        region: RegionKind::Body,
        spans,
        name: "bold".into(),
        value: Some(MarkVal::Bool(true)),
        also_clear: Vec::new(),
    };
    // Through `group`, the door an agent uses for a multi-part edit.
    app.invoke_confirmed(Invoke::group([Invoke::<RichText>::apply(&bold).unwrap()]))
        .unwrap();
    let marked = snapshot(&app);
    assert_eq!(marked.blocks.len(), 3);
    for block in &marked.blocks {
        assert!(
            block.marks.iter().any(|mark| mark.name == "bold"),
            "{block:?}"
        );
    }

    assert!(app.undo());
    assert_eq!(snapshot(&app).blocks, before.blocks);
}
