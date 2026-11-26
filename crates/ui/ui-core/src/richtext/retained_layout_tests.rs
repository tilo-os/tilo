use cosmic_text::FontSystem;
use tilo_ui_base::richtext::*;
use tilo_ui_base::text_render::TextStyle;
use tilo_ui_base::text_selection::TextAffinity;
use tilo_ui_base::text_selection::TextPosition;

fn shape(fs: &mut FontSystem, text: &str, width: f32, style: &TextStyle) -> RichTextLayout {
    RichTextLayout::shape_styled(
        fs,
        &[RichSpan::new(text)],
        style,
        Some(width),
        TextAlign::Left,
        vec![],
        vec![],
    )
}

tilo_test_env::scenario! {
    fn a_retained_layout_reuses_unchanged_paragraphs_and_matches_a_cold_one;

    phase resize_retains_shaping_but_invalidates_line_wrapping;
    {
        let mut fs = tilo_ui_base::font_catalog::fixture_font_system();
        let style = TextStyle::new(16.0);
        let text = "alpha beta gamma delta epsilon\nשלום mixed paragraph\né 👩‍💻 tail";
        let old = shape(&mut fs, text, 240.0, &style);
        let identities: Vec<_> = old
            .buffer
            .lines
            .iter()
            .map(|line| line.shape_opt().unwrap().spans.as_ptr())
            .collect();
        let mut next = shape(&mut fs, text, 80.0, &style);
        RichTextLayout::reuse_unchanged_paragraphs(&mut next.buffer, old.buffer);
        for (line, identity) in next.buffer.lines.iter().zip(identities) {
            assert_eq!(
                line.shape_opt().unwrap().spans.as_ptr(),
                identity,
                "a resize must retain exact paragraph shaping allocations"
            );
            assert!(
                line.layout_opt().is_none(),
                "old line wrapping must be invalidated"
            );
        }
        next.buffer.shape_until_scroll(&mut fs, false);
        let fresh = shape(&mut fs, text, 80.0, &style);
        assert_geometry(&next, &fresh, text);
    }

    phase separated_edits_retain_unchanged_interior_paragraphs;
    {
        let mut fs = tilo_ui_base::font_catalog::fixture_font_system();
        let style = TextStyle::new(16.0);
        let old_text = "first before\nשלום interior é\n👩‍💻 another interior\nlast before";
        let text = "first changed\nשלום interior é\n👩‍💻 another interior\nlast changed";
        let old = shape(&mut fs, old_text, 180.0, &style);
        let identities: Vec<_> = old.buffer.lines[1..3]
            .iter()
            .map(|line| {
                (
                    line.shape_opt().unwrap().spans.as_ptr(),
                    line.layout_opt().unwrap().as_ptr(),
                )
            })
            .collect();
        let retained = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &[RichSpan::new(text)],
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        for (line, (shape, layout)) in retained.buffer.lines[1..3].iter().zip(identities) {
            assert_eq!(line.shape_opt().unwrap().spans.as_ptr(), shape);
            assert_eq!(line.layout_opt().unwrap().as_ptr(), layout);
        }
        let fresh = shape(&mut fs, text, 180.0, &style);
        assert_geometry(&retained, &fresh, text);
    }

    phase interior_reuse_handles_insertions_repeated_text_and_marks;
    {
        let mut fs = tilo_ui_base::font_catalog::fixture_font_system();
        let style = TextStyle::new(16.0);
        let old_spans = [
            RichSpan::new("start old\n"),
            RichSpan::new("same שלום").bold(true),
            RichSpan::new("\n"),
            RichSpan::new("same שלום"),
            RichSpan::new("\n"),
            RichSpan::new("tail old"),
        ];
        let old = RichTextLayout::shape_styled(
            &mut fs,
            &old_spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let bold_shape = old.buffer.lines[1].shape_opt().unwrap().spans.as_ptr();
        let normal_shape = old.buffer.lines[2].shape_opt().unwrap().spans.as_ptr();
        let normal_attrs = old.buffer.lines[2].attrs_list().clone();
        let bold_attrs = old.buffer.lines[1].attrs_list().clone();
        let spans = [
            RichSpan::new("inserted\nstart new\n"),
            RichSpan::new("same שלום"),
            RichSpan::new("\n"),
            RichSpan::new("same שלום").bold(true),
            RichSpan::new("\n"),
            RichSpan::new("tail new\n"),
        ];
        let retained = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        assert_eq!(retained.buffer.lines[2].attrs_list(), &normal_attrs);
        assert_eq!(retained.buffer.lines[3].attrs_list(), &bold_attrs);
        assert_eq!(
            retained.buffer.lines[2].shape_opt().unwrap().spans.as_ptr(),
            normal_shape
        );
        assert_eq!(
            retained.buffer.lines[3].shape_opt().unwrap().spans.as_ptr(),
            bold_shape
        );
        let fresh = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        assert_geometry(&retained, &fresh, &text);
    }

    phase changed_paragraph_default_attributes_match_cold_geometry;
    {
        let mut fs = tilo_ui_base::font_catalog::fixture_font_system();
        let style = TextStyle::new(16.0);
        let old_spans = [
            RichSpan::new("start old\n"),
            RichSpan::new("same שלום\n").bold(true),
            RichSpan::new("same שלום\n"),
            RichSpan::new("tail old"),
        ];
        let old = RichTextLayout::shape_styled(
            &mut fs,
            &old_spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let normal_attrs = old.buffer.lines[2].attrs_list().clone();
        let spans = [
            RichSpan::new("inserted\nstart new\n"),
            RichSpan::new("same שלום\n"),
            RichSpan::new("same שלום\n").bold(true),
            RichSpan::new("tail new\n"),
        ];
        let retained = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        assert_ne!(retained.buffer.lines[2].attrs_list(), &normal_attrs);
        let fresh = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &style,
            Some(180.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        let text: String = spans.iter().map(|span| span.text.as_str()).collect();
        for (actual, expected) in retained.buffer.lines.iter().zip(&fresh.buffer.lines) {
            assert_eq!(actual.attrs_list(), expected.attrs_list());
        }
        assert_geometry(&retained, &fresh, &text);
    }

    phase retained_preparation_replaces_decorations_and_rechecks_custom_hyphenation;
    {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        struct MutableBreaks(AtomicU64);
        impl tilo_ui_base::text_render::Hyphenator for MutableBreaks {
            fn breaks(&self, _: &str) -> Vec<usize> {
                if self.0.load(Ordering::Relaxed) == 0 {
                    vec![]
                } else {
                    vec![3, 6, 9]
                }
            }
            fn cache_key(&self) -> u64 {
                self.0.load(Ordering::Relaxed)
            }
        }
        let mut fs = tilo_ui_base::font_catalog::fixture_font_system();
        let spans = [RichSpan::new("customizable")];
        let base = TextStyle::new(16.0);
        let old = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &base,
            Some(60.0),
            TextAlign::Left,
            vec![(0, 3)],
            vec![(3, 6)],
        );
        let retained = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &spans,
            &base,
            Some(60.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        assert!(retained.underlines.is_empty());
        assert!(retained.strikes.is_empty());
        let custom = Arc::new(MutableBreaks(AtomicU64::new(0)));
        let style = base.with_hyphenator(custom.clone());
        let old = RichTextLayout::shape_styled(
            &mut fs,
            &spans,
            &style,
            Some(60.0),
            TextAlign::Left,
            vec![],
            vec![],
        );
        assert_eq!(old.offset_map.shape_text(&spans[0].text), "customizable");
        custom.0.store(1, Ordering::Relaxed);
        let retained = RichTextLayout::shape_styled_reusing(
            &mut fs,
            &spans,
            &style,
            Some(60.0),
            TextAlign::Left,
            vec![],
            vec![],
            Some(old),
        );
        assert_eq!(
            retained.offset_map.shape_text(&spans[0].text),
            "cus\u{ad}tom\u{ad}iza\u{ad}ble"
        );
        let fresh = shape(&mut fs, &spans[0].text, 60.0, &style);
        assert_geometry(&retained, &fresh, &spans[0].text);
    }
}

fn assert_geometry(actual: &RichTextLayout, fresh: &RichTextLayout, text: &str) {
    assert_eq!(
        (actual.width(), actual.height()),
        (fresh.width(), fresh.height())
    );
    assert_eq!(actual.logical_line_origins(), fresh.logical_line_origins());
    for (a, b) in actual.buffer.lines.iter().zip(&fresh.buffer.lines) {
        assert_eq!(
            format!("{:?}", a.layout_opt()),
            format!("{:?}", b.layout_opt())
        );
    }
    for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
        assert_eq!(
            actual.selection_rects(0, byte),
            fresh.selection_rects(0, byte)
        );
        for affinity in [TextAffinity::Before, TextAffinity::After] {
            let position = TextPosition::with_affinity(byte, affinity);
            let caret = actual.caret_geometry_at(position);
            assert_eq!(caret, fresh.caret_geometry_at(position));
            assert_eq!(
                actual.hit_position(caret.0, caret.1 + caret.2 / 2.0),
                fresh.hit_position(caret.0, caret.1 + caret.2 / 2.0)
            );
        }
    }
}
