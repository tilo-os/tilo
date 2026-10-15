//! Regression coverage for the scaled writing surface and its hosted header.

use super::*;
use ui::headless::Framed;

fn document(theme: &Theme) -> Framed<WordProcessorWidget> {
    imported_document(
        b"# A considered document\n\nA paragraph with enough words to wrap while retaining readable type.\n\n| Item | Detail |\n| --- | --- |\n| Writing | Room to think |\n",
        theme,
    )
}

fn imported_document(markdown: &[u8], theme: &Theme) -> Framed<WordProcessorWidget> {
    let mut state = crate::initialized_state();
    let action = crate::import_action(
        markdown,
        WpFormat::Markdown,
        &tilo_content::ContentStore::memory(),
    )
    .unwrap();
    crate::model::apply(&mut state.doc, &action).unwrap();
    let c = crate::model::open(&state.doc).unwrap();
    let blocks = tilo_doc::model::materialize(&state.doc, &c).blocks;
    let intro = blocks.iter().find(|b| b.kind == "paragraph").unwrap();
    crate::model::comment_by(
        &mut state.doc,
        "Ada",
        &intro.id,
        (0, 11),
        "Keep this thought.",
    );
    let mut wp = WordProcessorWidget::detached();
    wp.sync(&state.doc, &state.root);
    wp.outline.toggle();
    Framed::new(state, wp, (1440, 810), theme.clone())
}

#[test]
fn ui_scaling_keeps_the_document_readable_beside_outline_and_review() {
    for theme in [Theme::dark(), Theme::light()] {
        for scale in [1.0, 1.5, 2.0] {
            let theme = theme.clone().with_ui_scale(scale);
            let framed = document(&theme);
            let body = framed.laid.iter().find(|l| l.kind == "paragraph").unwrap();
            assert!(
                body.size * framed.zoom.factor() as f32 >= 16.0 * scale,
                "UI scale {scale}: authored body type must grow along with the controls"
            );
            let to_window = framed.zoom.transform(Affine::IDENTITY);
            let page = to_window.transform_rect_bbox(framed.page_rect);
            let canvas = framed.zoom_viewport;
            assert!(
                page.x0 >= canvas.x0 && page.x1 <= canvas.x1,
                "the scaled sheet fits its canvas: {page:?}, {canvas:?}"
            );
            let placement = ReviewPlacement {
                canvas,
                page_x1: page.x1,
                to_window,
            };
            let (review_x, _) = ReviewOverlay::column(&theme, &placement);
            assert!(
                review_x >= page.x1,
                "review cards stand beside the sheet, not over its text"
            );
            for (_, rect) in &framed.toolbar {
                assert_eq!(rect.intersect(framed.tool_band), *rect);
            }
            for rect in framed
                .layout_chips
                .iter()
                .map(|(_, rect)| rect)
                .chain([&framed.zoom_rect])
            {
                assert!(
                    rect.y0 >= canvas.y1 && rect.y1 <= 810.0,
                    "status controls remain wholly below the writing canvas"
                );
            }
        }
    }
}

#[test]
fn showcase_document_keeps_its_rich_content_on_one_page() {
    let framed = imported_document(
        include_bytes!("../../examples/showcase.md"),
        &Theme::dark().with_ui_scale(1.5),
    );
    assert!(framed.laid.len() > 10);
    assert!(framed.laid.iter().all(|block| block.page == 0));
    assert!(framed.laid.iter().any(|block| block.kind == "cell"));
    assert!(
        framed
            .laid
            .iter()
            .any(|block| block.text.starts_with("A thoughtful draft"))
    );
}

#[test]
fn scrollbar_is_six_output_pixels_and_uses_content_space_at_every_scale() {
    let mut renderer = ui::offscreen::OffscreenRenderer::new(200, 200);
    for theme in [Theme::dark(), Theme::light()] {
        for scale in [1.0, 1.5, 2.0] {
            for output_scale in [1.0, 2.0] {
                let mut wp = WordProcessorWidget::detached();
                wp.desk_theme = theme.clone().with_ui_scale(scale);
                wp.scroll_viewport = 100.0;
                wp.max_scroll = 100.0;
                let transform = Affine::scale(output_scale);
                let canvas = Rect::new(0.0, 0.0, 200.0 / output_scale, 200.0 / output_scale);
                for offset in [0.0, 100.0] {
                    wp.scroll_y = offset;
                    let mut scene = Scene::new();
                    wp.draw_scrollbar(&mut scene, transform, canvas);
                    let pixels = renderer.render(&scene, Color::TRANSPARENT);
                    let painted: Vec<_> = pixels
                        .chunks_exact(4)
                        .enumerate()
                        .filter(|(_, pixel)| pixel[3] > 0)
                        .map(|(index, _)| (index % 200, index / 200))
                        .collect();
                    let left = painted.iter().map(|(x, _)| *x).min().unwrap();
                    let right = painted.iter().map(|(x, _)| *x).max().unwrap();
                    let top = painted.iter().map(|(_, y)| *y).min().unwrap();
                    let bottom = painted.iter().map(|(_, y)| *y).max().unwrap();
                    assert_eq!(right - left + 1, 6, "thumb must not fill the gutter");
                    assert_eq!((left, right), (191, 196));
                    // Half the content is visible, independent of output/page scale.
                    assert_eq!(
                        (top, bottom),
                        if offset == 0.0 { (3, 99) } else { (100, 196) }
                    );
                }
                for overflow in [0.0, ui::widget::scrollbar::FIT_EPS] {
                    wp.max_scroll = overflow;
                    wp.scroll_y = 0.0;
                    let mut scene = Scene::new();
                    wp.draw_scrollbar(&mut scene, transform, canvas);
                    assert!(
                        renderer
                            .render(&scene, Color::TRANSPARENT)
                            .chunks_exact(4)
                            .all(|pixel| pixel[3] == 0),
                        "a fitting document has no scrollbar"
                    );
                }
            }
        }
    }
}

#[test]
fn the_component_uses_writers_existing_header_for_history() {
    let mut app =
        ui::framework::testing::HeadlessApp::<crate::framework::WordProcessorApp>::capture_at(
            None,
            (1440, 810),
        )
        .unwrap();
    app.harness().set_theme(Theme::dark().with_ui_scale(1.5));
    let wp = app.surface_mut::<WordProcessorWidget>();
    assert!(ui::widget::capability::capability::<dyn ui::widget::WindowHeader>(wp).is_some());
    assert!(
        wp.window_history.is_shown(),
        "the host offers History to Writer's header"
    );
    assert_eq!(
        wp.tool_band.y0, 0.0,
        "there is no second, empty header above the formatting row"
    );
    assert_eq!(
        wp.window_history_rect.intersect(wp.tool_band),
        wp.window_history_rect
    );
    let rect = wp.window_history_rect;
    app.harness().click_at(rect.center().x, rect.center().y);
    assert!(
        app.harness().targets().contains_key("activity_timeline"),
        "the header button opens the registered History"
    );
}

#[test]
fn insertion_precedes_specialist_formats_in_toolbar_overflow_order() {
    let framed = document(&Theme::dark().with_ui_scale(1.5));
    let entries = framed.toolbar_entries();
    let index = |wanted| {
        entries
            .iter()
            .position(
                |entry| matches!(entry, ToolEntry::Command { action, .. } if *action == wanted),
            )
            .unwrap()
    };
    assert!(index(ToolAction::InsertTable) < index(ToolAction::Strike));
    assert!(index(ToolAction::InsertImage) < index(ToolAction::Super));
    for action in [
        ToolAction::Strike,
        ToolAction::Super,
        ToolAction::Sub,
        ToolAction::InsertToc,
    ] {
        assert!(entries.iter().any(|entry| matches!(entry, ToolEntry::Command { action: offered, .. } if *offered == action)), "no command disappears while simplifying the toolbar");
    }
}

#[test]
fn collaborator_names_and_selections_cannot_escape_the_canvas() {
    let theme = Theme::dark().with_ui_scale(1.5);
    let framed = document(&theme);
    let body = framed.laid.iter().find(|l| l.kind == "paragraph").unwrap();
    let mut presence = PresenceCtx::empty();
    let user = UserId("ada".into());
    let key = block_presence_key(&body.id);
    presence.set_display_name(user.clone(), "Ada");
    presence.carets.insert(key.clone(), vec![(user.clone(), 0)]);
    presence.selections.insert(
        key,
        vec![(user, ui::presence::SelectionRange { start: 0, end: 64 })],
    );
    let base = framed.content_transform(Affine::IDENTITY);
    let canvas = framed.zoom_viewport;
    let top = (base * Point::new(body.text_x, body.y_top)).y;
    // Scroll the line across the canvas edge, including its hanging name.
    let ctf = Affine::translate((0.0, canvas.y0 - top - 5.0)) * base;
    let mut scene = Scene::new();
    framed.draw_remote_overlays(
        &mut scene,
        ctf,
        canvas,
        &mut TextRenderer::new(),
        &theme,
        &presence,
    );
    let pixels =
        ui::offscreen::OffscreenRenderer::new(1440, 810).render(&scene, Color::TRANSPARENT);
    let mut visible = 0;
    for (index, pixel) in pixels.chunks_exact(4).enumerate() {
        let point = Point::new((index % 1440) as f64 + 0.5, (index / 1440) as f64 + 0.5);
        if canvas.contains(point) {
            visible += usize::from(pixel[3] > 0);
        } else {
            assert_eq!(
                pixel[3], 0,
                "presence painted outside the document at {point:?}"
            );
        }
    }
    assert!(
        visible > 100,
        "the oracle must observe a real, partly visible selection"
    );
}
