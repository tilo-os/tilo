//! End-to-end spine guard: create a note the way the tree does, type every
//! block kind into it, and find all of it again after the notebook reopens.
//! Breaks when Markdown shortcuts, the body's actions, the record or the
//! reload path do.

use crate::framework_support::{notes, surface};
use notes::read;
use notes::sidebar::OutEvent;
use ui::widget::BlockKind;

#[test]
fn full_lifecycle_round_trips_through_the_record() {
    let mut app = notes();
    surface(&mut app, |notes| {
        notes.apply_event_for_test(OutEvent::CreateChild { parent: None });
    });
    let target = surface(&mut app, |notes| notes.selected_for_test()).expect("new note opened");
    surface(&mut app, |notes| {
        notes.apply_event_for_test(OutEvent::Rename {
            id: target,
            new_title: "Test Page".into(),
        });
    });

    let editor = *app.harness().targets().get("block_editor").unwrap();
    app.harness().click_at(editor.x0 + 20.0, editor.y0 + 10.0);
    for (index, line) in [
        "Hello world.",
        "## Section title",
        "- bullet",
        // Enter continued the list: a task marker turns the bullet into a
        // done checklist item.
        "[x] buy coffee",
    ]
    .iter()
    .enumerate()
    {
        if index > 0 {
            app.harness().key("Enter");
        }
        app.harness().type_text(line);
    }

    let mut app = app.reopen().unwrap();
    let page = crate::framework_support::shown(&app)
        .pages()
        .iter()
        .find(|page| page.id == target)
        .cloned()
        .expect("Test Page still in the notebook");
    assert_eq!(page.title, "Test Page");
    let stored = app.read_document(|doc| read::page_blocks(doc, target));
    assert_eq!(stored.len(), 4, "{stored:?}");

    surface(&mut app, |notes| notes.navigate_to(target));
    let blocks = surface(&mut app, |notes| {
        assert_eq!(notes.main_panel_mut().page_id(), Some(target));
        notes
            .main_panel_mut()
            .editor()
            .blocks()
            .iter()
            .map(|block| (block.kind.clone(), block.text().to_owned()))
            .collect::<Vec<_>>()
    });
    assert_eq!(blocks.len(), 4, "{blocks:?}");
    assert!(matches!(blocks[0].0, BlockKind::Paragraph));
    assert_eq!(blocks[0].1, "Hello world.");
    assert!(matches!(blocks[1].0, BlockKind::Heading(2)));
    assert_eq!(blocks[1].1, "Section title");
    assert!(matches!(blocks[2].0, BlockKind::Bullet { indent: 0 }));
    assert_eq!(blocks[2].1, "bullet");
    assert!(matches!(
        blocks[3].0,
        BlockKind::Checklist {
            indent: 0,
            done: true
        }
    ));
    assert_eq!(blocks[3].1, "buy coffee");
}
