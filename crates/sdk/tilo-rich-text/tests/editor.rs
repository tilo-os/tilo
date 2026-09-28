//! The SDK rich-text editor in a framework app, driven the way a person
//! drives it: typing into the real component runtime, record storage,
//! permissions, per-author undo and a collaborator's replica.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_document::{RichText, RichTextAction};
use tilo_rich_text::RichTextViews;
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::permissions::Role;
use ui::tilo_actions::Action;
use ui::widget::Element;

/// An app whose whole view is one line.
struct Document;

impl ResourceApp for Document {
    type Resource = RichText;
    const ID: &'static str = "so.tilo.rich-text-example";
    const TITLE: &'static str = "Rich text";

    fn view(cx: &mut ViewCx<RichText>) -> Element {
        cx.rich_text()
    }
}

/// The same document, edited by a participant whose writing is attributed.
struct Attributed;

impl ResourceApp for Attributed {
    type Resource = RichText;
    const ID: &'static str = "so.tilo.rich-text-attributed-example";
    const TITLE: &'static str = "Rich text";

    fn view(cx: &mut ViewCx<RichText>) -> Element {
        let mut surface = tilo_rich_text::RichTextSurface::new();
        surface.editor_mut().set_local_author(Some("ada".into()));
        cx.rich_text_in(surface)
    }
}

fn open() -> TestApp<Document> {
    open_as()
}

fn open_as<A: ResourceApp<Resource = RichText>>() -> TestApp<A> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<A>::new().unwrap();
    // Focus the editor the way a person does: click into it.
    let target = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("the editor is addressable");
    app.harness().click_at(target.x0 + 20.0, target.y0 + 10.0);
    app
}

fn paragraphs_of(doc: &ui::tilo_resource::Doc) -> Vec<String> {
    let c = tilo_doc::model::open(doc, &ui::automerge::ROOT).unwrap();
    tilo_doc::model::materialize(doc, &c)
        .blocks
        .into_iter()
        .map(|block| block.text)
        .collect()
}

fn paragraphs<A: ResourceApp<Resource = RichText>>(app: &TestApp<A>) -> Vec<String> {
    app.read_document(paragraphs_of)
}

fn authors<A: ResourceApp<Resource = RichText>>(app: &TestApp<A>) -> Vec<(usize, usize, String)> {
    app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &ui::automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .flat_map(|block| tilo_doc::author_runs(&block.marks))
            .collect()
    })
}

fn bold_ranges(app: &TestApp<Document>) -> Vec<(usize, usize)> {
    app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &ui::automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .flat_map(|block| block.marks)
            .filter(|mark| mark.name == "strong")
            .map(|mark| (mark.start, mark.end))
            .collect()
    })
}

#[test]
fn a_keystroke_is_an_action_and_undo_takes_back_a_word_at_a_time() {
    let mut app = open();
    app.harness().type_text("hello world");
    assert_eq!(paragraphs(&app), ["hello world"]);
    let last = app.versions().pop().unwrap();
    let context = last.context.as_ref().unwrap();
    assert_eq!(context.action_label, "Splice Text");
    assert_eq!(
        context
            .resource
            .as_ref()
            .map(|resource| resource.kind.as_str()),
        Some("tilo.rich-text")
    );

    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["hello "]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app), ["hello world"]);
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(paragraphs(&app), [""]);
}

#[test]
fn enter_splits_the_paragraph_as_its_own_step_and_typing_continues_in_it() {
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
fn backspace_at_a_paragraph_start_merges_it_into_the_one_before() {
    let mut app = open();
    app.harness().type_text("one");
    app.harness().key("Enter");
    app.harness().type_text("two");
    app.harness().key("Home");
    app.harness().key("Backspace");
    assert_eq!(paragraphs(&app), ["onetwo"]);
    app.harness().type_text("-");
    assert_eq!(paragraphs(&app), ["one-two"]);
}

#[test]
fn bold_typed_after_ctrl_b_is_stored_as_a_mark() {
    let mut app = open();
    app.harness().type_text("plain ");
    app.harness().set_modifiers(&["ctrl".into()]);
    app.harness().key("b");
    app.harness().set_modifiers(&[]);
    app.harness().type_text("bold");
    assert_eq!(paragraphs(&app), ["plain bold"]);
    assert_eq!(bold_ranges(&app), [(6, 10)]);
}

#[test]
fn moving_a_paragraph_up_is_one_move_the_document_keeps() {
    let mut app = open();
    app.harness().type_text("first");
    app.harness().key("Enter");
    app.harness().type_text("second");
    app.harness().set_modifiers(&["alt".into()]);
    app.harness().key("ArrowUp");
    app.harness().set_modifiers(&[]);
    assert_eq!(paragraphs(&app), ["second", "first"]);
    let last = app.versions().pop().unwrap();
    assert_eq!(last.context.as_ref().unwrap().action_label, "Move Blocks");
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["first", "second"]);
}

#[test]
fn typed_text_is_attributed_to_its_writer_in_the_same_step() {
    let mut app = open_as::<Attributed>();
    app.harness().type_text("hello");
    app.harness().key("Enter");
    app.harness().type_text("world");
    assert_eq!(paragraphs(&app), ["hello", "world"]);
    assert_eq!(
        authors(&app),
        [(0, 5, "ada".to_owned()), (0, 5, "ada".to_owned())]
    );
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["hello", ""]);
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(paragraphs(&app), [""]);
    assert!(authors(&app).is_empty());
}

#[test]
fn text_nobody_is_named_for_stays_unattributed() {
    let mut app = open();
    app.harness().type_text("hello");
    assert!(authors(&app).is_empty());
}

#[test]
fn shift_enter_is_a_line_break_inside_the_paragraph() {
    let mut app = open();
    app.harness().type_text("Alpha");
    app.harness().set_modifiers(&["shift".into()]);
    app.harness().key("Enter");
    app.harness().set_modifiers(&[]);
    app.harness().type_text("Beta");
    assert_eq!(paragraphs(&app), ["Alpha\nBeta"]);
}

#[test]
fn a_viewer_is_refused_and_the_document_is_untouched() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<Document>::with_role(Role::Viewer).unwrap();
    let target = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .unwrap();
    app.harness().click_at(target.x0 + 20.0, target.y0 + 10.0);
    let before = app.versions().len();
    app.harness().type_text("sneaky");
    app.harness().key("Enter");
    assert_eq!(paragraphs(&app), [""]);
    assert_eq!(app.versions().len(), before, "nothing was written");
}

#[test]
fn a_role_taken_away_while_typing_stops_the_next_keystroke() {
    let mut app = open();
    app.harness().type_text("mine");
    app.set_role(Role::Viewer);
    app.harness().type_text(" theirs");
    assert_eq!(paragraphs(&app), ["mine"]);
}

#[test]
fn a_collaborator_typing_in_the_same_paragraph_converges_and_keeps_my_caret() {
    let mut app = open();
    app.harness().type_text("world");
    let mut peer = app.peer_document();
    let block = tilo_doc::model::blocks_in_order(
        &peer,
        &tilo_doc::model::open(&peer, &ui::automerge::ROOT).unwrap(),
    )[0]
    .clone();
    // While I keep typing, the collaborator types at the start.
    RichTextAction::SpliceText {
        region: tilo_document::action::RegionKind::Body,
        block,
        at: 0,
        del: 0,
        ins: "hello ".into(),
    }
    .apply(&mut peer)
    .unwrap();
    peer.commit();
    app.harness().type_text("!");
    app.merge_peer(&mut peer).unwrap();

    assert_eq!(paragraphs(&app), ["hello world!"]);
    assert_eq!(
        paragraphs_of(&peer),
        paragraphs(&app),
        "both replicas agree"
    );
    // My caret stayed after my own text, past what arrived before it.
    app.harness().type_text("?");
    assert_eq!(paragraphs(&app), ["hello world!?"]);
}

#[test]
fn a_collaborators_new_paragraph_appears_without_moving_me() {
    let mut app = open();
    app.harness().type_text("draft");
    let mut peer = app.peer_document();
    let first = tilo_doc::model::blocks_in_order(
        &peer,
        &tilo_doc::model::open(&peer, &ui::automerge::ROOT).unwrap(),
    )[0]
    .clone();
    RichTextAction::InsertParagraph {
        after: first,
        text: "their note".into(),
    }
    .apply(&mut peer)
    .unwrap();
    peer.commit();
    app.merge_peer(&mut peer).unwrap();
    app.harness().type_text("s");
    assert_eq!(paragraphs(&app), ["drafts", "their note"]);
    app.merge_peer(&mut peer).unwrap();
    assert_eq!(paragraphs_of(&peer), ["drafts", "their note"]);
}

#[test]
fn an_agent_edit_through_the_action_door_is_shown_and_undoable() {
    let mut app = open();
    app.harness().type_text("typed");
    let block = app.read_document(|doc| {
        tilo_doc::model::blocks_in_order(
            doc,
            &tilo_doc::model::open(doc, &ui::automerge::ROOT).unwrap(),
        )[0]
        .clone()
    });
    app.invoke(
        ui::tilo_resource::Invoke::<RichText>::apply(&RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at: 5,
            del: 0,
            ins: " by hand".into(),
        })
        .unwrap(),
    )
    .unwrap();
    assert_eq!(paragraphs(&app), ["typed by hand"]);
    app.harness().key("End");
    app.harness().type_text(".");
    assert_eq!(paragraphs(&app), ["typed by hand."]);
}
