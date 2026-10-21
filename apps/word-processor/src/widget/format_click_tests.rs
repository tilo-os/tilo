//! Formatting a selection made with the mouse, by clicking the toolbar, in
//! the real app on the framework: the way a person bolds a word.

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

/// The centre of the published control named `name`.
fn control_centre(app: &mut App, name: &str) -> Point {
    let id = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("{name} is published"));
    let bounds = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(node, data)| (node == id).then(|| data.bounds()))
        .flatten()
        .unwrap_or_else(|| panic!("{name} has bounds"));
    Point::new(
        f64::midpoint(bounds.x0, bounds.x1),
        f64::midpoint(bounds.y0, bounds.y1),
    )
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

/// Double-click a word, click Bold: the word is bold, and one undo takes the
/// bold back rather than the typing before it.
#[test]
fn toolbar_bold_bolds_a_double_clicked_word_as_one_step() {
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

    let bold = control_centre(&mut app, &t("a11y-bold"));
    app.harness().click_at(bold.x, bold.y);
    app.settle();
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
