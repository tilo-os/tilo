//! Browser text commits must reach the same slash commands as native keys.
use notes::{actions, read};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;
use ui::winit::event::{Ime, WindowEvent};

fn fixture() -> (TestApp<notes::NotesShell>, notes::PageId) {
    let mut app = TestApp::<notes::NotesShell>::new().unwrap();
    let page = notes::PageId::new_v4();
    app.invoke(Invoke::apply(&actions::create_page_action(page, None, "Slash text")).unwrap())
        .unwrap();
    let rect = app.harness().targets()["block_editor"];
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    app.harness().type_text("Before");
    app.harness().key("Enter");
    (app, page)
}

#[test]
fn committed_slash_opens_filter_and_conversion_is_one_undo_step() {
    let (mut app, page) = fixture();
    app.harness()
        .send(&WindowEvent::Ime(Ime::Commit("/".into())));
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(before[1].text, "/");
    app.harness()
        .send(&WindowEvent::Ime(Ime::Commit("quote".into())));
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before,
        "the filter owns committed query text"
    );
    app.harness().key("Enter");
    let converted = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(converted[1].kind, ui::widget::BlockKind::Quote);
    assert_eq!(converted[1].text, "");
    assert_eq!(converted[1].id, before[1].id);
    assert!(app.undo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    assert!(app.redo());
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        converted
    );
}

#[test]
fn preedit_and_multi_character_commits_do_not_open_a_command() {
    let (mut app, page) = fixture();
    let before = app.read_document(|doc| read::page_blocks(doc, page));
    app.harness()
        .send(&WindowEvent::Ime(Ime::Preedit("/".into(), Some((1, 1)))));
    assert_eq!(
        app.read_document(|doc| read::page_blocks(doc, page)),
        before
    );
    app.harness()
        .send(&WindowEvent::Ime(Ime::Commit("/quote".into())));
    let after = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(after[1].text, "/quote");
    assert_eq!(after[1].kind, ui::widget::BlockKind::Paragraph);
    app.harness().key("Enter");
    let split = app.read_document(|doc| read::page_blocks(doc, page));
    assert_eq!(split[1], after[1]);
    assert_eq!(split.len(), 3);
}

#[test]
fn code_keeps_committed_slash_literal_and_escape_keeps_prose_source() {
    for code in [false, true] {
        let (mut app, page) = fixture();
        if code {
            app.harness().type_text("/code");
            app.harness().key("Enter");
        }
        app.harness()
            .send(&WindowEvent::Ime(Ime::Commit("/".into())));
        if !code {
            app.harness().key("Escape");
        }
        app.harness()
            .send(&WindowEvent::Ime(Ime::Commit("quote".into())));
        let after = app.read_document(|doc| read::page_blocks(doc, page));
        assert_eq!(after[1].text, "/quote", "code={code}");
        assert_eq!(
            matches!(after[1].kind, ui::widget::BlockKind::CodeBlock { .. }),
            code
        );
    }
}

#[test]
fn slash_replacing_local_selection_strips_only_the_trigger() {
    for committed in [false, true] {
        for reverse in [false, true] {
            let (mut app, page) = fixture();
            app.harness().type_text("Ä😀 XYZtail");
            app.harness().key("Home");
            for _ in 0..if reverse { 6 } else { 3 } {
                app.harness().key("ArrowRight");
            }
            app.harness().set_modifiers(&["shift".into()]);
            for _ in 0..3 {
                app.harness()
                    .key(if reverse { "ArrowLeft" } else { "ArrowRight" });
            }
            app.harness().set_modifiers(&[]);
            if committed {
                app.harness()
                    .send(&WindowEvent::Ime(Ime::Commit("/".into())));
            } else {
                app.harness().type_text("/");
            }
            let before = app.read_document(|doc| read::page_blocks(doc, page));
            assert_eq!(before[1].text, "Ä😀 /tail");
            app.harness()
                .send(&WindowEvent::Ime(Ime::Commit("quote".into())));
            app.harness().key("Enter");
            let after = app.read_document(|doc| read::page_blocks(doc, page));
            assert_eq!(
                after[1].text, "Ä😀 tail",
                "committed={committed}, reverse={reverse}"
            );
            assert_eq!(after[1].kind, ui::widget::BlockKind::Quote);
            assert_eq!(after[1].id, before[1].id);
            assert!(app.undo());
            assert_eq!(
                app.read_document(|doc| read::page_blocks(doc, page)),
                before
            );
            assert!(app.redo());
            assert_eq!(app.read_document(|doc| read::page_blocks(doc, page)), after);
        }
    }
}
