//! Which sheet the caret stands on, told to the view each time it changes: the "N" a status band
//! writes in "Page N of M".

use super::*;

/// A page of one-line paragraphs, laid out on Letter paper at `width`, with `page_shown` told to
/// the view from the first layout on.
fn paged(
    width: f32,
    lines: usize,
    page_shown: tilo_ui_base::component::Event,
) -> (Box<BlockEditor>, TextRenderer, Theme) {
    let mut editor = Box::new(BlockEditor::new().with_wrap_lines(true));
    editor.set_blocks(
        (0..lines)
            .map(|index| Block::paragraph(&format!("line {index}")))
            .collect(),
    );
    editor.set_pages(Pages::of("Letter", "", ""));
    editor.page_shown_event = Some(page_shown);
    let theme = Theme::dark();
    let mut text = tilo_ui_base::font_catalog::fixture_renderer();
    layout_image_editor(&mut editor, width, &mut text);
    (editor, text, theme)
}

fn page_shown_event(
    queue: &tilo_ui_base::Intents<tilo_ui_base::component::Fired>,
) -> tilo_ui_base::component::Event {
    tilo_ui_base::component::Event::View(tilo_ui_base::component::Intent::new(
        queue,
        "doc",
        "page_shown",
    ))
}

#[test]
fn the_sheet_the_caret_stands_on_is_reported_when_it_changes() {
    let queue = tilo_ui_base::Intents::<tilo_ui_base::component::Fired>::default();
    let (mut editor, _text, _theme) = paged(520.0, 60, page_shown_event(&queue));
    assert!(
        editor.stack.pages.pages.len() > 1,
        "the fixture fills more than a page"
    );
    // The first layout already told the view where a document with no caret shows.
    let first = queue.take();
    assert_eq!(
        first.last().map(|fired| fired.value.clone()),
        Some(Some(tilo_ui_base::component::Value::Int(1))),
        "the document opens on its first sheet"
    );
    editor.focus_block(0, CursorPlacement::Start);
    assert!(
        queue.take().is_empty(),
        "a caret that stays on its sheet says nothing"
    );
    let second_page = editor
        .stack
        .pages
        .pages
        .get(1)
        .copied()
        .expect("a second sheet");
    let on_second = editor
        .stack
        .tops
        .iter()
        .position(|top| *top >= second_page - 0.01)
        .expect("a block on the second sheet");
    editor.focus_block(on_second, CursorPlacement::Start);
    let fired = queue.take();
    assert_eq!(
        fired
            .iter()
            .map(|fired| fired.value.clone())
            .collect::<Vec<_>>(),
        vec![Some(tilo_ui_base::component::Value::Int(2))],
        "the caret moved to the second sheet"
    );
    assert_eq!(fired[0].event, "page_shown");
}

#[test]
fn with_no_caret_the_sheet_the_view_shows_is_reported() {
    let queue = tilo_ui_base::Intents::<tilo_ui_base::component::Fired>::default();
    let (mut editor, mut text, theme) = paged(520.0, 60, page_shown_event(&queue));
    let _ = queue.take();
    let second_page = editor
        .stack
        .pages
        .pages
        .get(1)
        .copied()
        .expect("a second sheet");
    let viewport = Rect::new(
        editor.rect.x0,
        editor.rect.y0 + f64::from(second_page),
        editor.rect.x1,
        editor.rect.y0 + f64::from(second_page) + 400.0,
    );
    let presence = PresenceCtx::empty();
    presence.push_viewport(viewport);
    let mut scene = Scene::new();
    Widget::draw(
        &mut *editor,
        &mut tilo_ui_base::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            &theme,
            presence.draw_env(),
        ),
    );
    let fired = queue.take();
    assert_eq!(
        fired
            .iter()
            .map(|fired| fired.value.clone())
            .collect::<Vec<_>>(),
        vec![Some(tilo_ui_base::component::Value::Int(2))],
        "a view scrolled to the second sheet shows it"
    );
}
