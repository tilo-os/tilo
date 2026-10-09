//! Undo of edits made through the rich-text resource's own actions — the
//! door a model, a script or the palette uses — restores the document as it
//! was, formatting included.

use tilo_document::action::{MarkVal, RegionKind, RichTextAction, Span};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use word_processor::framework::{Document, WordProcessorApp};

fn snapshot(app: &TestApp<WordProcessorApp>) -> tilo_doc::DocSnapshot {
    app.read_document(|doc| {
        let c = word_processor::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &c)
    })
}

fn texts(app: &TestApp<WordProcessorApp>) -> Vec<String> {
    snapshot(app)
        .blocks
        .into_iter()
        .map(|block| block.text)
        .collect()
}

/// A record holding `markdown`, loaded through `apply`.
fn open_with(markdown: &str) -> TestApp<WordProcessorApp> {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    let load = RichTextAction::ReplaceBody {
        markdown: markdown.to_owned(),
    };
    app.invoke_confirmed(Invoke::<Document>::edit(&word_processor::model::edit(&load)).unwrap())
        .unwrap();
    app
}

/// The id of the first block whose text starts with `prefix`.
fn block(app: &TestApp<WordProcessorApp>, prefix: &str) -> String {
    snapshot(app)
        .blocks
        .into_iter()
        .find(|block| block.text.starts_with(prefix))
        .unwrap()
        .id
}

fn group(app: &mut TestApp<WordProcessorApp>, actions: &[RichTextAction]) {
    let members = actions
        .iter()
        .map(|action| Invoke::<Document>::edit(&word_processor::model::edit(action)).unwrap());
    app.invoke_confirmed(Invoke::group(members)).unwrap();
}

fn splice(block: &str, at: usize, del: usize, ins: &str) -> RichTextAction {
    RichTextAction::SpliceText {
        region: RegionKind::Body,
        block: block.to_owned(),
        at,
        del,
        ins: ins.to_owned(),
    }
}

fn bold(spans: Vec<Span>, on: bool) -> RichTextAction {
    RichTextAction::SetMark {
        region: RegionKind::Body,
        spans,
        name: "bold".into(),
        value: on.then_some(MarkVal::Bool(true)),
        also_clear: Vec::new(),
    }
}

#[test]
fn undo_takes_back_only_your_own_steps_around_a_collaborators_words() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::new().unwrap();
    app.harness().type_text("hello");
    app.harness().key("Enter");
    assert_eq!(texts(&app), ["hello", ""]);
    // A collaborator types into the new paragraph.
    let new = snapshot(&app).blocks[1].id.clone();
    app.merge_peer_edit(|doc| {
        word_processor::model::apply(doc, &splice(&new, 0, 0, "their words")).unwrap();
    });
    assert_eq!(texts(&app), ["hello", "their words"]);

    // Undo the Enter, then the word: the collaborator's words stay.
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(texts(&app), ["", "their words"]);
}

#[test]
fn undoing_a_group_of_splices_around_a_bold_run_restores_its_text() {
    let mut app = open_with(
        "Project Falcon remains the largest investment.\n\nStaffing for Project Falcon was confirmed.\n",
    );
    let first = block(&app, "Project Falcon");
    let second = block(&app, "Staffing");
    group(
        &mut app,
        &[bold(
            vec![Span {
                block: first.clone(),
                start: 0,
                end: 14,
            }],
            true,
        )],
    );
    let before = snapshot(&app);

    // Rename "Falcon" to "Osprey": insert the new name inside the bold run
    // (so it stays bold), then delete the old one on both sides of it.
    group(
        &mut app,
        &[
            splice(&first, 9, 0, "Osprey"),
            splice(&first, 8, 1, ""),
            splice(&first, 14, 5, ""),
            splice(&second, "Staffing for Project ".len(), 6, "Osprey"),
        ],
    );
    assert_eq!(
        texts(&app),
        [
            "Project Osprey remains the largest investment.",
            "Staffing for Project Osprey was confirmed."
        ]
    );

    assert!(app.undo());
    let after = snapshot(&app);
    assert_eq!(after.blocks, before.blocks);
}
