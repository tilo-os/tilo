use super::*;
use crate::text_render::{FontChoice, TextStyle, TextWrapMode};

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

#[test]
fn resize_retains_shaping_but_invalidates_line_wrapping() {
    let mut fs = crate::font_catalog::fixture_font_system();
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

#[test]
fn resize_and_style_transitions_match_cold_unicode_layout() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let text = "Latin ligatures ffi é\nשלום mixed العربية\n👩‍💻 中文\nlast\n";
    let base = TextStyle::new(16.0);
    let mut previous = Some(shape(&mut fs, text, 240.0, &base));
    for style in [
        base.clone(),
        base.clone().with_line_height(1.7),
        TextStyle::new(24.0),
        base.clone().with_family(FontChoice::Serif),
        base.clone().with_italic(true),
        base.clone().with_letter_spacing(0.8),
        base.clone().with_wrap_mode(TextWrapMode::Balance),
        base.with_auto_hyphenation("en-US"),
    ] {
        for width in [80.0, 160.0, 240.0, 80.0] {
            let actual = RichTextLayout::shape_styled_reusing(
                &mut fs,
                &[RichSpan::new(text)],
                &style,
                Some(width),
                TextAlign::Left,
                vec![],
                vec![],
                previous,
            );
            let fresh = shape(&mut fs, text, width, &style);
            assert_geometry(&actual, &fresh, text);
            previous = Some(actual);
        }
    }
}

#[test]
fn retained_idle_paragraphs_keep_both_caches_across_many_layout_owners() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = TextStyle::new(16.0);
    let mut owners: Vec<_> = (0..64)
        .map(|i| {
            let text = format!("window {i}: unchanged text שלום\nsecond line");
            let layout = shape(&mut fs, &text, 160.0, &style);
            (text, layout)
        })
        .collect();
    for (text, old) in &mut owners {
        let shape_ptr = old.buffer.lines[0].shape_opt().unwrap().spans.as_ptr();
        let layout_ptr = old.buffer.lines[0].layout_opt().unwrap().as_ptr();
        // Re-admission after an idle/hidden interval uses the same owner value;
        // visibility scheduling itself belongs to the host, not this cache.
        let replacement = shape(&mut fs, text, 160.0, &style);
        let previous = std::mem::replace(old, replacement);
        RichTextLayout::reuse_unchanged_paragraphs(&mut old.buffer, previous.buffer);
        assert_eq!(
            old.buffer.lines[0].shape_opt().unwrap().spans.as_ptr(),
            shape_ptr
        );
        assert_eq!(
            old.buffer.lines[0].layout_opt().unwrap().as_ptr(),
            layout_ptr
        );
    }
}
