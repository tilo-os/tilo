//! Formatting a selection made with the mouse, by choosing Bold in the
//! window bar, in the real app on the framework: the way a person bolds a
//! word.

use super::*;
use ui::framework::testing::TestApp;

type App = TestApp<crate::framework::WordProcessorApp>;

fn open() -> App {
    ui::i18n::I18nManager::init("en-US");
    let mut app = App::new().unwrap();
    app.harness().resize((1000, 700)).unwrap();
    app.settle();
    app
}

/// Choose the window bar's slot `id`, as the bar sends it.
fn press_bar(app: &mut App, id: &str) {
    let declared = ui::window_toolbar::declared().expect("the editor declares its bar");
    assert!(
        declared
            .actions
            .iter()
            .flat_map(|action| std::iter::once(action).chain(&action.items))
            .any(|slot| slot.id == id),
        "the window bar offers {id}: {declared:?}"
    );
    ui::window_toolbar::deliver(ui::window_toolbar::ToolbarEvent::Action { id: id.into() });
    app.settle();
}

fn first_block_marks(app: &mut App) -> Vec<(String, usize, usize)> {
    app.with_surface::<WordProcessorWidget, _>(|w| {
        w.blocks[0]
            .marks
            .iter()
            .map(|m| (m.name.clone(), m.start, m.end))
            .collect()
    })
    .unwrap()
}

/// Double-click a word, choose Bold: the word is bold, and one undo takes the
/// bold back rather than the typing before it.
#[test]
fn bar_bold_bolds_a_double_clicked_word_as_one_step() {
    let mut app = open();
    let body = app
        .with_surface::<WordProcessorWidget, _>(|w| w.zoom_viewport.center())
        .unwrap();
    app.harness().click_at(body.x, body.y);
    app.harness().type_text("Meeting notes");
    app.settle();
    let word = app
        .with_surface::<WordProcessorWidget, _>(|w| {
            let l = w.laid.first().expect("a laid block");
            Point::new(
                w.rect.x0 + l.text_x + 12.0,
                w.rect.y0 + l.y_top + l.height / 2.0,
            )
        })
        .unwrap();
    app.harness().double_click_at(word.x, word.y);
    app.settle();
    assert!(
        app.with_surface::<WordProcessorWidget, _>(|w| w.inner.has_selection())
            .unwrap(),
        "the double-click selected the word"
    );

    press_bar(&mut app, "bold");
    let marks = first_block_marks(&mut app);
    assert!(
        marks
            .iter()
            .any(|(name, start, end)| name == "strong" && *start == 0 && *end == "Meeting".len()),
        "Bold marked the selected word: {marks:?}"
    );

    assert!(app.undo());
    assert!(
        first_block_marks(&mut app)
            .iter()
            .all(|(name, ..)| name != "strong"),
        "undo takes back the bold"
    );
    let text = app
        .with_surface::<WordProcessorWidget, _>(|w| w.blocks[0].text.clone())
        .unwrap();
    assert_eq!(text, "Meeting notes", "and leaves the typing");
}
