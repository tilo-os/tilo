use super::*;
use crate::semantic_document_read::retirement::measure_test_allocations;

#[test]
fn warm_multiline_preparation_allocates_nothing() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    for highlighted in [false, true] {
        let mut input = TextInput::local()
            .multiline(true)
            .with_initial_value(&"fn main() { println!(\"é שלום 👩‍💻\"); }\n".repeat(100));
        if highlighted {
            input.set_highlighter(Some(Box::new(
                crate::widget::code_editor::SyntaxHighlighter::for_token_or_plain("rs"),
            )));
        }
        let style = input.multiline_style(theme.text_scale);
        input.ensure_shaped_layout(&mut tr, &theme, &style, 300.0);
        let ((), allocations) = measure_test_allocations(|| {
            input.set_cursor_pos(3);
            input.ensure_shaped_layout(&mut tr, &theme, &style, 300.0);
        });
        assert_eq!(
            allocations.requested, 0,
            "highlighted={highlighted}: {allocations:?}"
        );
    }
}

#[test]
fn complete_block_style_invalidates_preparation() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut input = TextInput::local()
        .multiline(true)
        .with_initial_value("office שלום\nsecond line");
    let style = TextStyle::new(16.0);
    input.ensure_shaped_layout(&mut tr, &theme, &style, 180.0);
    let changed = style.with_line_height(2.0).with_letter_spacing(1.5);
    input.ensure_shaped_layout(&mut tr, &theme, &changed, 180.0);
    let fresh = RichTextLayout::shape_styled(
        &mut tr.font_system,
        &input.highlighted_display_spans(&theme),
        &changed,
        Some(180.0),
        input.align,
        vec![],
        vec![],
    );
    assert_eq!(
        input.single_layout.as_ref().unwrap().height(),
        fresh.height()
    );
    for byte in input.text.char_indices().map(|(byte, _)| byte) {
        assert_eq!(
            input.single_layout.as_ref().unwrap().caret_geometry(byte),
            fresh.caret_geometry(byte)
        );
    }
}

fn assert_cold_geometry(
    input: &TextInput,
    tr: &mut TextRenderer,
    theme: &Theme,
    style: &TextStyle,
    width: f32,
) {
    let display = input.layout_display_text();
    let fresh = RichTextLayout::shape_styled(
        &mut tr.font_system,
        &input.highlighted_display_spans(theme),
        style,
        Some(width),
        input.align,
        vec![],
        vec![],
    );
    let retained = input.single_layout.as_ref().unwrap();
    assert_eq!(
        (retained.width(), retained.height()),
        (fresh.width(), fresh.height())
    );
    assert_eq!(
        retained.logical_line_origins(),
        fresh.logical_line_origins()
    );
    assert_eq!(retained.backgrounds, fresh.backgrounds);
    for (actual, expected) in retained.buffer.lines.iter().zip(&fresh.buffer.lines) {
        assert_eq!(actual.attrs_list(), expected.attrs_list());
    }
    for byte in display
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([display.len()])
    {
        for affinity in [TextAffinity::Before, TextAffinity::After] {
            let position = TextPosition::with_affinity(byte, affinity);
            let caret = retained.caret_geometry_at(position);
            assert_eq!(caret, fresh.caret_geometry_at(position));
            assert_eq!(
                retained.hit_position(caret.0, caret.1 + caret.2 / 2.0),
                fresh.hit_position(caret.0, caret.1 + caret.2 / 2.0)
            );
        }
        assert_eq!(
            retained.selection_rects(0, byte),
            fresh.selection_rects(0, byte)
        );
    }
}

#[test]
fn preparation_tracks_unwitnessed_text_marks_theme_ime_and_renderer() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let mut theme = Theme::dark();
    let mut input = TextInput::local()
        .multiline(true)
        .with_initial_value("let var = 17; é\nשלום 👩‍💻\nlast\n");
    let mut style = input.multiline_style(theme.text_scale);
    let mut width = 160.0;
    for transition in 0..10 {
        match transition {
            // Deliberately bypass dirty flags: they are not a complete text witness.
            1 => {
                input.text.replace_range(..3, "pub");
            }
            2 => {
                input.set_rich_spans(Some(vec![
                    RichSpan::new(&input.text)
                        .bold(true)
                        .background(Some([10, 20, 30])),
                ]));
            }
            3 => {
                input.core.set_caret(7);
                input.core.set_preedit("漢字", Some((0, 0)));
            }
            4 => {
                input.core.set_caret(0);
            }
            5 => {
                input.core.clear_preedit();
                input.set_rich_spans(None);
                input.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
            }
            6 => {
                theme = Theme::light();
            }
            7 => {
                width = 80.0;
                style = style.with_family(FontChoice::Serif);
            }
            8 => {
                tr.font_system
                    .db_mut()
                    .set_serif_family(crate::text_render::UI_MONOSPACE_FAMILY);
                tr.bump_generation();
            }
            9 => {
                tr = crate::font_catalog::fixture_renderer();
            }
            _ => {}
        }
        input.ensure_shaped_layout(&mut tr, &theme, &style, width);
        assert_cold_geometry(&input, &mut tr, &theme, &style, width);
        let ((), stats) =
            measure_test_allocations(|| input.ensure_shaped_layout(&mut tr, &theme, &style, width));
        assert_eq!(
            stats.requested, 0,
            "warm transition {transition}: {stats:?}"
        );
    }
}

#[test]
fn measure_keeps_warm_attributed_preparation() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut input = TextInput::local()
        .multiline(true)
        .with_initial_value("styled é\nשלום 👩‍💻\n");
    input.set_rich_spans(Some(vec![RichSpan::new(&input.text).bold(true)]));
    let style = input.multiline_style(theme.text_scale);
    let width = 300.0;
    let content_width = (f64::from(width) - input.ml_left() - input.ml_pad_x()) as f32;
    input.ensure_shaped_layout(&mut tr, &theme, &style, content_width);
    let before = input.single_layout.as_ref().unwrap().height();
    let ((), stats) = measure_test_allocations(|| {
        Widget::measure(
            &mut input,
            taffy::Size {
                width: Some(width),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut tr,
        );
        input.ensure_shaped_layout(&mut tr, &theme, &style, content_width);
    });
    assert_eq!(input.single_layout.as_ref().unwrap().height(), before);
    assert_eq!(stats.requested, 0, "{stats:?}");
}

#[test]
fn enabling_secret_discards_retained_plaintext_preparation() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    let mut input = TextInput::local()
        .multiline(true)
        .with_initial_value("private é value");
    input.set_highlighter(crate::widget::code_editor::Language::Rust.highlighter());
    let style = input.multiline_style(theme.text_scale);
    input.ensure_shaped_layout(&mut tr, &theme, &style, 200.0);
    input.set_secret(true);
    assert!(input.shaped_preparation.is_none());
    assert!(input.hl_text.is_none());
    assert!(input.single_layout.is_none());
    assert_eq!(input.display_text(), "•".repeat(input.text.chars().count()));
}

#[test]
fn revealed_secret_does_not_leave_rich_input_snapshot() {
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    for multiline in [true, false] {
        let mut input = TextInput::local()
            .multiline(multiline)
            .with_initial_value("private é value")
            .with_secret(true);
        input.set_reveal(true);
        let style = input.multiline_style(theme.text_scale);
        let spans = vec![RichSpan::new(&input.text)];
        let width = if multiline { Some(200.0) } else { None };
        if multiline {
            input.ensure_shaped_layout(&mut tr, &theme, &style, 200.0);
        } else {
            input.set_single_line_attributes(SingleLineAttributes {
                spans: spans.clone(),
                ..Default::default()
            });
            input.build_single_line_metrics(
                &mut tr,
                &spans[0].text,
                input.effective_font_size(),
                theme.text_scale,
            );
        }
        let previous = input.single_layout.take();
        // A rich input snapshot permits allocation-free reconstruction. A revealed
        // secret must leave only its display geometry, never that extra snapshot.
        let (_, stats) = measure_test_allocations(|| {
            RichTextLayout::shape_styled_reusing(
                &mut tr.font_system,
                &spans,
                &style,
                width,
                input.align,
                vec![],
                vec![],
                previous,
            )
        });
        assert!(
            stats.requested > 0,
            "secret retained an exact rich input snapshot: multiline={multiline}"
        );
        assert!(input.shaped_preparation.is_none());
    }
}
