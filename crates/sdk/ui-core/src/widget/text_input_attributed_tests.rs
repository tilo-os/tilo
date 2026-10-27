//! A field over an attributed line: shaping during measure, the preedit,
//! chips and links by shaped range, the style inputs that invalidate the shape,
//! and the runs a sync rebuilds.
use super::super::*;
use super::body_size;
use super::draw_rich_line_for_test;
use super::draw_to_populate;
use crate::testing::dispatch;
use automerge::ObjType;
use automerge::transaction::Transactable as _;

#[test]
fn attributed_single_line_is_shaped_during_measure_before_draw() {
    use crate::widget::marks::{Mark, MarkSpan};
    use crate::widget::rich_text_line::RichTextLine;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
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
            .single_layout
            .as_ref()
            .expect("measurement must shape styled text");
        let weights: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
            .collect();
        assert_eq!(weights, [400, 700, 400]);
        assert_eq!(layout.buffer.metrics().font_size, body_size() * scale);
    }
}

#[test]
fn attributed_single_line_preedit_preserves_utf8_marks_and_commit_route() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut line = crate::widget::RichTextLine::new().with_text("éW");
        line.set_marks(vec![
            MarkSpan::new(0..3, Mark::Bold),
            MarkSpan::new(0..3, Mark::Code),
            MarkSpan::new(0..3, Mark::Underline),
            MarkSpan::new(0..3, Mark::Strikethrough),
        ]);
        line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
        line.set_focus(true);
        line.set_cursor_pos(2);
        let send = |line: &mut crate::widget::RichTextLine, event: WindowEvent| {
            dispatch(line, &event);
        };
        send(
            &mut line,
            WindowEvent::Ime(winit::event::Ime::Preedit("漢".into(), Some((3, 3)))),
        );
        draw_rich_line_for_test(&mut line, &theme);
        assert_eq!(line.value(), "éW");
        let input = line.inner();
        let layout = input.single_layout.as_ref().unwrap();
        assert_eq!(layout.buffer.lines[0].text(), "é漢W");
        assert_eq!(layout.underlines, [(0, 2), (5, 6), (2, 5)]);
        assert_eq!(layout.strikes, [(0, 2), (5, 6)]);
        let weights: Vec<_> = layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter().map(|g| g.font_weight.0))
            .collect();
        assert_eq!(weights, [700, 400, 700]);
        let anchor = input.ime_cursor_rect().unwrap();
        assert!(anchor.x0 > line.caret_rect(&theme, 2).x0);
        assert_eq!(
            input.display_ranges(&input.single_attributes.as_ref().unwrap().chips),
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

#[test]
fn attributed_single_line_chip_and_link_rules_follow_shaped_ranges() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for scale in [1.0, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut line = crate::widget::RichTextLine::new().with_text("aWWWz");
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
        let layout = input.single_layout.as_ref().unwrap();
        assert_eq!(layout.strikes, [(1, 4)]);
        assert!(
            layout
                .buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .all(|g| g.font_size == body_size() * scale)
        );
        for (background, pad) in [(true, 2.0), (false, 0.0)] {
            let mut decorations = Scene::new();
            input.draw_single_line_decorations(
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

#[test]
fn attributed_single_line_style_inputs_invalidate_cached_shape() {
    use crate::richtext::{BaselineShift, FontFamily};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local().with_initial_value("WWW");
    input.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
    let mut attributes = SingleLineAttributes {
        spans: vec![RichSpan::new("WWW")],
        ..Default::default()
    };
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    let original_width = input.single_layout.as_ref().unwrap().width();
    attributes.spans[0].size = Some(28.0);
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    let sized_width = input.single_layout.as_ref().unwrap().width();
    assert!(sized_width > original_width * 1.5);
    attributes.spans[0].family = Some(FontFamily::Mono);
    input.set_single_line_attributes(attributes.clone());
    draw_to_populate(&mut input);
    assert!((input.single_layout.as_ref().unwrap().width() - sized_width).abs() > 1.0);
    attributes.spans[0].background = Some([40, 50, 60]);
    attributes.spans[0].baseline = BaselineShift::Super;
    attributes.spans[0].color = Some([100, 110, 120]);
    attributes.underlines = vec![(0, 1)];
    attributes.strikes = vec![(1, 3)];
    input.set_single_line_attributes(attributes);
    draw_to_populate(&mut input);
    let layout = input.single_layout.as_ref().unwrap();
    assert_eq!(layout.backgrounds, [(0, 3, [40, 50, 60])]);
    assert_eq!(layout.baselines, [(0, 3, BaselineShift::Super)]);
    assert_eq!(layout.underlines, [(0, 1)]);
    assert_eq!(layout.strikes, [(1, 3)]);
    assert!(
        layout
            .buffer
            .layout_runs()
            .flat_map(|r| r.glyphs.iter())
            .all(|g| g.color_opt == Some(cosmic_text::Color::rgb(100, 110, 120)))
    );
}

#[test]
fn attributed_single_line_sync_undo_redo_rebuilds_runs() {
    use crate::widget::marks::{Mark, MarkSpan};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut state = crate::State::new();
    let text = state
        .doc
        .put_object(&state.root, "body", ObjType::Text)
        .unwrap();
    state.doc.splice_text(&text, 0, 0, "WWW").unwrap();
    state.commit_action(None, None);
    let mut line = crate::widget::RichTextLine::new();
    line.bind_text(text.clone());
    line.sync(&state.doc, &state.root);
    line.set_marks(vec![MarkSpan::new(0..3, Mark::Bold)]);
    line.apply_layout_external(Rect::new(0.0, 0.0, 400.0, 60.0));
    let theme = Theme::dark().with_text_scale(2.0);
    draw_rich_line_for_test(&mut line, &theme);
    let wide = line.pixel_x_for_byte(&theme, 3);
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
                .single_layout
                .as_ref()
                .unwrap()
                .buffer
                .layout_runs()
                .flat_map(|r| r.glyphs.iter())
                .all(|g| g.font_weight.0 == 700)
        );
        let x = line.pixel_x_for_byte(&theme, 3);
        if expected == "WWW" {
            assert!((x - wide).abs() < 0.01);
        } else {
            assert!(x < wide * 0.6);
        }
        assert_eq!(line.byte_for_pixel_x(&theme, x), 3);
    }
}
