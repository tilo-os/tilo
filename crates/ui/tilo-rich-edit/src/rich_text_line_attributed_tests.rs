//! A field over an attributed line: shaping during measure, the preedit,
//! chips and links by shaped range, the style inputs that invalidate the shape,
//! and the runs a sync rebuilds.
use super::*;
use taffy::{Size, TaffyTree};
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use ui::testing::dispatch;
use vello::Scene;
use vello::kurbo::Affine;

fn body_size() -> f32 {
    tilo_ui_base::tokens::typography::stock().size(TextRole::Body)
}

fn draw_rich_line_for_test(line: &mut RichTextLine, theme: &Theme) -> Scene {
    let mut renderer = TextRenderer::new();
    renderer.set_text_scale(theme.text_scale);
    let mut scene = Scene::new();
    line.draw(&mut tilo_ui_base::DrawCtx::new(
        &mut scene,
        Affine::IDENTITY,
        &mut renderer,
        theme,
        tilo_ui_base::presence::PresenceCtx::empty().draw_env(),
    ));
    scene
}
use automerge::ObjType;
use automerge::transaction::Transactable as _;

tilo_test_env::scenario! {
    /// A single-line field with attributed text.
    /// Each phase was a test of its own and builds its own fixture.
    fn an_attributed_line_shapes_marks_and_rebuilds_its_runs;

    phase attributed_single_line_is_shaped_during_measure_before_draw;
    {
        use ui::widget::marks::{Mark, MarkSpan};
        use crate::rich_text_line::RichTextLine;
        let _settled = tilo_ui_base::motion::testing::settled();
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut line = RichTextLine::new().with_text("aBz");
            line.set_marks(vec![MarkSpan::new(1..2, Mark::Bold)]);
            let mut taffy = TaffyTree::new();
            line.build_layout(&mut taffy, &theme, &mut Default::default());
            let mut renderer = TextRenderer::new(); // intentionally stale 1x
            line.measure(
                Size::NONE,
                Size {
                    width: taffy::AvailableSpace::MaxContent,
                    height: taffy::AvailableSpace::MaxContent,
                },
                &mut renderer,
            );
            let layout = line
                .inner()
                .shaped_layout_for_test()
                .expect("measurement must shape styled text");
            let weights: Vec<_> = layout
                .buffer_for_test()
                .layout_runs()
                .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
                .collect();
            assert_eq!(weights, [400, 700, 400]);
            assert_eq!(layout.buffer_for_test().metrics().font_size, body_size() * scale);
        }
    }

    phase attributed_single_line_preedit_preserves_utf8_marks_and_commit_route;
    {
        use ui::widget::marks::{Mark, MarkSpan};
        let _settled = tilo_ui_base::motion::testing::settled();
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut line = crate::RichTextLine::new().with_text("éW");
            line.set_marks(vec![
                MarkSpan::new(0..3, Mark::Bold),
                MarkSpan::new(0..3, Mark::Code),
                MarkSpan::new(0..3, Mark::Underline),
                MarkSpan::new(0..3, Mark::Strikethrough),
            ]);
            line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
            line.set_focus(true);
            line.set_cursor_pos(2);
            let send = |line: &mut crate::RichTextLine, event: WindowEvent| {
                dispatch(line, &event);
            };
            send(
                &mut line,
                WindowEvent::Ime(winit::event::Ime::Preedit("漢".into(), Some((3, 3)))),
            );
            draw_rich_line_for_test(&mut line, &theme);
            assert_eq!(line.value(), "éW");
            let input = line.inner();
            let layout = input.shaped_layout_for_test().unwrap();
            assert_eq!(layout.buffer_for_test().lines[0].text(), "é漢W");
            assert_eq!(layout.decorations_for_test().0, [(0, 2), (5, 6), (2, 5)]);
            assert_eq!(layout.decorations_for_test().1, [(0, 2), (5, 6)]);
            let weights: Vec<_> = layout
                .buffer_for_test()
                .layout_runs()
                .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
                .collect();
            assert_eq!(weights, [700, 400, 700]);
            let anchor = input.ime_cursor_rect().unwrap();
            assert!(anchor.x0 > line.caret_rect(&theme, 2).x0);
            assert_eq!(
                input.chip_display_ranges_for_test(),
                [(0, 2), (5, 6)]
            );
            send(
                &mut line,
                WindowEvent::Ime(winit::event::Ime::Commit("漢".into())),
            );
            let scene = draw_rich_line_for_test(&mut line, &theme);
            assert_eq!(line.value(), "é漢W");
            assert_eq!(line.cursor_pos(), 5);
            assert_eq!(scene.encoding().resources.glyphs.len(), 3);
            assert!(
                line.marks()
                    .iter()
                    .all(|m| line.value().is_char_boundary(m.range.start)
                        && line.value().is_char_boundary(m.range.end))
            );
        }
    }

    phase attributed_single_line_chip_and_link_rules_follow_shaped_ranges;
    {
        use ui::widget::marks::{Mark, MarkSpan};
        let _settled = tilo_ui_base::motion::testing::settled();
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut line = crate::RichTextLine::new().with_text("aWWWz");
            line.set_marks(vec![
                MarkSpan::new(1..4, Mark::Bold),
                MarkSpan::new(1..4, Mark::Code),
                MarkSpan::new(1..4, Mark::Link(uuid::Uuid::from_u128(1))),
                MarkSpan::new(1..4, Mark::Strikethrough),
            ]);
            line.apply_layout_external(Rect::new(10.0, 20.0, 400.0, 90.0));
            let scene = draw_rich_line_for_test(&mut line, &theme);
            assert_eq!(scene.encoding().resources.glyphs.len(), 5);
            let input = line.inner();
            let left = input.caret_rect(&theme, 1).x0;
            let right = input.caret_rect(&theme, 4).x0;
            let layout = input.shaped_layout_for_test().unwrap();
            assert_eq!(layout.decorations_for_test().1, [(1, 4)]);
            assert!(
                layout
                    .buffer_for_test()
                    .layout_runs()
                    .flat_map(|r| r.glyphs.iter())
                    .all(|g| g.font_size == body_size() * scale)
            );
            for (background, pad) in [(true, 2.0), (false, 0.0)] {
                let mut decorations = Scene::new();
                input.draw_single_line_decorations_for_test(
                    &mut decorations,
                    Affine::IDENTITY,
                    &theme,
                    background,
                );
                let xs: Vec<_> = decorations
                    .encoding()
                    .path_data
                    .chunks_exact(2)
                    .map(|xy| f64::from(f32::from_bits(xy[0])))
                    .collect();
                let min = xs.iter().copied().fold(f64::INFINITY, f64::min);
                let max = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
                assert!((min - (left - pad)).abs() < 0.01);
                assert!((max - (right + pad)).abs() < 0.01);
                assert_eq!(
                    decorations.encoding().resources.glyphs.len(),
                    0,
                    "decorations never repaint text"
                );
            }
        }
    }

    phase attributed_single_line_sync_undo_redo_rebuilds_runs;
    {
        use ui::widget::marks::{Mark, MarkSpan};
        let _settled = tilo_ui_base::motion::testing::settled();
        let mut state = tilo_ui_model::State::new();
        let text = state
            .doc
            .put_object(&state.root, "body", ObjType::Text)
            .unwrap();
        state.doc.splice_text(&text, 0, 0, "WWW").unwrap();
        state.commit_action(None, None);
        let mut line = crate::RichTextLine::new();
        line.bind_text(text.clone());
        line.sync(&state.doc, &state.root);
        line.set_marks(vec![MarkSpan::new(0..3, Mark::Bold)]);
        line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
        let theme = Theme::dark().with_text_scale(2.0);
        draw_rich_line_for_test(&mut line, &theme);
        let wide = line.caret_rect(&theme, 3).x0;
        state.doc.splice_text(&text, 0, 3, "iii").unwrap();
        state.commit_action(None, None);
        for (step, expected) in [(0, "iii"), (1, "WWW"), (2, "iii")] {
            if step == 1 {
                state.undo(None, None).unwrap();
            }
            if step == 2 {
                state.redo(None, None).unwrap();
            }
            line.sync(&state.doc, &state.root);
            let _ = line.take_dirty(); // preserve direct-binding reconciliation semantics
            assert!(line.layout_needs_measure());
            draw_rich_line_for_test(&mut line, &theme);
            assert_eq!(line.value(), expected);
            let input = line.inner();
            assert!(
                input
                    .shaped_layout_for_test()
                    .unwrap()
                    .buffer_for_test()
                    .layout_runs()
                    .flat_map(|r| r.glyphs.iter())
                    .all(|g| g.font_weight.0 == 700)
            );
            let x = line.caret_rect(&theme, 3).x0;
            if expected == "WWW" {
                assert!((x - wide).abs() < 0.01);
            } else {
                assert!(x < wide * 0.6);
            }
            assert_eq!(line.byte_for_pixel_x(&theme, x), 3);
        }
    }
}
