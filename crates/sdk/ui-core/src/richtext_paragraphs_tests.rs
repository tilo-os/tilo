use super::*;
use crate::text_selection::{TextAffinity, TextPosition};

/// Paragraphs in many scripts: bidi runs, Thai (dictionary breaks become
/// virtual insertions), CJK, emoji, combining marks, blank lines.
const SEED: &str = "The rehearsal ran long, so the doors opened late.\n\
\n\
שלום עולם, mixed with English and 123 numbers.\n\
مرحبا بالعالم (Arabic) — and back.\n\
ภาษาไทยไม่มีช่องว่างระหว่างคำเลยครับ\n\
日本語の文章は単語の間に空白がありません。\n\
emoji 👩‍💻 and flags 🇩🇪🇫🇷 and é with e\u{301} combining.\n\
\n\
last paragraph";

/// Text pieces an edit inserts: plain letters, a line feed, several
/// paragraphs at once, other scripts.
const INSERTS: [&str; 8] = [
    "x",
    " ",
    "\n",
    "pasted\n\nparagraphs\n",
    "ש",
    "ไทย",
    "👍",
    "\n\n",
];

struct Lcg(u64);

impl Lcg {
    fn next(&mut self, bound: usize) -> usize {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        usize::try_from(self.0 >> 33).unwrap_or(0) % bound.max(1)
    }
}

fn char_boundary(text: &str, byte: usize) -> usize {
    let mut byte = byte.min(text.len());
    while !text.is_char_boundary(byte) {
        byte -= 1;
    }
    byte
}

fn edit(text: &str, random: &mut Lcg) -> String {
    let at = char_boundary(text, random.next(text.len().saturating_add(1)));
    let mut out = text.to_owned();
    if random.next(3) == 0 && !text.is_empty() {
        let end = char_boundary(text, at.saturating_add(random.next(24)));
        out.replace_range(at..end.max(at), "");
    } else {
        out.insert_str(at, INSERTS[random.next(INSERTS.len())]);
    }
    out
}

fn paragraphs(
    fs: &mut FontSystem,
    text: &str,
    style: &TextStyle,
    width: f32,
    previous: Option<RichTextLayout>,
) -> RichTextLayout {
    RichTextLayout::shape_paragraphs_reusing_browser(
        fs,
        text,
        style,
        Some(width),
        TextAlign::Left,
        previous,
        false,
        None,
    )
}

/// Everything painting, wrapping, carets, selection and hit-testing read.
fn assert_same_layout(spliced: &RichTextLayout, fresh: &RichTextLayout, text: &str, case: &str) {
    assert!(
        spliced
            .prepared
            .as_ref()
            .is_some_and(|p| p.paragraph_planned),
        "{case}: the splice stays on the paragraph path"
    );
    assert_eq!(spliced.text_len, fresh.text_len, "{case}");
    assert_eq!(spliced.line_starts, fresh.line_starts, "{case}");
    assert_eq!(spliced.line_lens, fresh.line_lens, "{case}");
    assert_eq!(
        spliced.offset_map.insertions, fresh.offset_map.insertions,
        "{case}"
    );
    assert_eq!(
        spliced.discretionary_hyphen_width, fresh.discretionary_hyphen_width,
        "{case}"
    );
    assert_eq!(spliced.buffer.size(), fresh.buffer.size(), "{case}");
    assert_eq!(
        spliced.buffer.lines.len(),
        fresh.buffer.lines.len(),
        "{case}"
    );
    for (index, (a, b)) in spliced
        .buffer
        .lines
        .iter()
        .zip(&fresh.buffer.lines)
        .enumerate()
    {
        assert_eq!(a.text(), b.text(), "{case}: line {index}");
        assert!(a.attrs_list() == b.attrs_list(), "{case}: line {index}");
        assert_eq!(a.align(), b.align(), "{case}: line {index}");
        assert_eq!(
            format!("{:?}", a.layout_opt()),
            format!("{:?}", b.layout_opt()),
            "{case}: line {index}"
        );
    }
    assert_eq!(spliced.height(), fresh.height(), "{case}");
    assert_eq!(spliced.width(), fresh.width(), "{case}");
    assert_eq!(
        spliced.logical_line_origins(),
        fresh.logical_line_origins(),
        "{case}"
    );
    for byte in text.char_indices().map(|(i, _)| i).chain([text.len()]) {
        for affinity in [TextAffinity::Before, TextAffinity::After] {
            let position = TextPosition::with_affinity(byte, affinity);
            let caret = spliced.caret_geometry_at(position);
            assert_eq!(
                caret,
                fresh.caret_geometry_at(position),
                "{case}: byte {byte}"
            );
            assert_eq!(
                spliced.hit_position(caret.0, caret.1 + caret.2 / 2.0),
                fresh.hit_position(caret.0, caret.1 + caret.2 / 2.0),
                "{case}: byte {byte}"
            );
            assert_eq!(
                spliced.visual_motion_position(position, true),
                fresh.visual_motion_position(position, true),
                "{case}: byte {byte}"
            );
        }
    }
    assert_eq!(
        spliced.selection_rects(0, text.len()),
        fresh.selection_rects(0, text.len()),
        "{case}"
    );
}

#[test]
fn spliced_edits_match_a_fresh_layout() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let mut random = Lcg(0x5eed);
    let mut hyphenated = TextStyle::new(14.0).with_letter_spacing(0.4);
    hyphenated.hyphenation = crate::text_render::Hyphenation::Auto("en".into());
    for style in [TextStyle::new(16.0).with_line_height(1.5), hyphenated] {
        let mut text = SEED.to_owned();
        let mut layout = paragraphs(&mut fs, &text, &style, 220.0, None);
        for step in 0..50 {
            text = edit(&text, &mut random);
            // Most keystrokes keep the width; some measure at another one.
            let width = if step % 7 == 3 { 140.0 } else { 220.0 };
            layout = paragraphs(&mut fs, &text, &style, width, Some(layout));
            let fresh = paragraphs(&mut fs, &text, &style, width, None);
            assert_same_layout(&layout, &fresh, &text, &format!("step {step}: {text:?}"));
        }
    }
}

#[test]
fn a_keystroke_shapes_only_its_paragraph() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = TextStyle::new(16.0);
    let text = (0..200)
        .map(|index| format!("Paragraph {index} of a long note, with words to wrap."))
        .collect::<Vec<_>>()
        .join("\n");
    let layout = paragraphs(&mut fs, &text, &style, 200.0, None);
    assert_eq!(take_prepared_paragraphs(), 200);
    let wraps = |layout: &RichTextLayout| -> Vec<usize> {
        layout
            .buffer
            .lines
            .iter()
            .map(|line| line.layout_opt().map_or(0, |rows| rows.len()))
            .collect()
    };
    let before = wraps(&layout);
    let at = text.find("Paragraph 100 ").unwrap() + "Paragraph 100".len();
    let mut edited = text.clone();
    edited.insert(at, 'x');
    let layout = paragraphs(&mut fs, &edited, &style, 200.0, Some(layout));
    assert_eq!(take_prepared_paragraphs(), 1, "only the edited paragraph");
    // Every other line kept its wrap: still laid out, at the same rows.
    assert_eq!(wraps(&layout), before);
    assert!(
        layout
            .buffer
            .lines
            .iter()
            .all(|line| line.layout_opt().is_some())
    );
    assert_same_layout(
        &layout,
        &paragraphs(&mut fs, &edited, &style, 200.0, None),
        &edited,
        "keystroke",
    );
    // A pasted paragraph break prepares the two paragraphs it leaves.
    let mut split = edited.clone();
    split.insert(at, '\n');
    let layout = paragraphs(&mut fs, &split, &style, 200.0, Some(layout));
    take_prepared_paragraphs();
    let mut joined = split.clone();
    joined.remove(at);
    let _ = paragraphs(&mut fs, &joined, &style, 200.0, Some(layout));
    assert_eq!(
        take_prepared_paragraphs(),
        1,
        "joining prepares one paragraph"
    );
}

/// A field measured at several widths keeps one layout per width; each
/// keystroke is spliced into all of them, the later ones borrowing the shapes
/// of the one spliced first.
#[test]
fn layouts_kept_per_width_track_edits_and_share_shapes() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = TextStyle::new(16.0);
    let widths = [1.0, 400.0, 160.0];
    let mut random = Lcg(0xd0b1e);
    let mut text = SEED.to_owned();
    let first = paragraphs(&mut fs, &text, &style, widths[0], None);
    // The other widths start from copies, as a field's first frame does.
    let mut slots: Vec<RichTextLayout> = vec![first.duplicate(&mut fs), first.duplicate(&mut fs)];
    slots.insert(0, first);
    for (slot, width) in slots.iter_mut().zip(widths).skip(1) {
        let copy = std::mem::replace(slot, paragraphs(&mut fs, "", &style, width, None));
        *slot = paragraphs(&mut fs, &text, &style, width, Some(copy));
    }
    take_prepared_paragraphs();
    for step in 0..20 {
        text = edit(&text, &mut random);
        let mut sibling: Option<RichTextLayout> = None;
        for (index, width) in widths.into_iter().enumerate() {
            let previous = std::mem::replace(
                &mut slots[index],
                paragraphs(&mut fs, "", &style, width, None),
            );
            take_prepared_paragraphs();
            let layout = RichTextLayout::shape_paragraphs_reusing_browser(
                &mut fs,
                &text,
                &style,
                Some(width),
                TextAlign::Left,
                Some(previous),
                false,
                sibling.as_ref(),
            );
            if index > 0 {
                assert_eq!(
                    take_prepared_paragraphs(),
                    0,
                    "step {step}: shapes are borrowed"
                );
            }
            let fresh = paragraphs(&mut fs, &text, &style, width, None);
            assert_same_layout(
                &layout,
                &fresh,
                &text,
                &format!("step {step} width {width}"),
            );
            sibling = Some(layout.duplicate(&mut fs));
            slots[index] = layout;
        }
    }
}

#[test]
fn unchanged_inputs_return_the_retained_layout() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = TextStyle::new(16.0);
    let mut layout = paragraphs(&mut fs, SEED, &style, 200.0, None);
    layout.add_underline(0..3);
    let retained = layout.buffer.lines[0].layout_opt().unwrap().as_ptr();
    let layout = paragraphs(&mut fs, SEED, &style, 200.0, Some(layout));
    assert_eq!(
        retained,
        layout.buffer.lines[0].layout_opt().unwrap().as_ptr()
    );
    assert!(layout.underlines.is_empty(), "decorations are the caller's");
}

#[test]
fn dependent_paragraphs_take_the_whole_text_path() {
    let mut fs = crate::font_catalog::fixture_font_system();
    let style = TextStyle::new(16.0);
    for text in [
        "carriage\r\nreturn",
        "separator\u{2029}between",
        "next line\u{85}here",
        &"x".repeat(HUGE_PARAGRAPH_BYTES + 1),
    ] {
        let layout = paragraphs(&mut fs, text, &style, 200.0, None);
        assert!(
            layout
                .prepared
                .as_ref()
                .is_some_and(|p| !p.paragraph_planned),
            "{text:?}"
        );
        let styled = RichTextLayout::shape_styled_reusing_browser(
            &mut fs,
            &[RichSpan::new(text)],
            &style,
            Some(200.0),
            TextAlign::Left,
            None,
            false,
        );
        assert_eq!(layout.height(), styled.height());
        assert_eq!(layout.line_starts, styled.line_starts);
    }
    // Typing a separator into a paragraph layout leaves the paragraph path
    // and lands exactly where a fresh layout of the result does.
    let layout = paragraphs(&mut fs, "one\ntwo", &style, 200.0, None);
    let layout = paragraphs(&mut fs, "one\ntw\u{2029}o", &style, 200.0, Some(layout));
    let fresh = paragraphs(&mut fs, "one\ntw\u{2029}o", &style, 200.0, None);
    assert_eq!(layout.height(), fresh.height());
    assert_eq!(layout.line_starts, fresh.line_starts);
}

#[test]
fn paragraph_edit_covers_whole_touched_paragraphs() {
    fn edit<'a>(old: &'a str, new: &'a str) -> (&'a str, &'a str) {
        let edit = LineEdit::between(old, new);
        (
            &old[edit.start..edit.old_end],
            &new[edit.start..edit.new_end],
        )
    }
    assert_eq!(edit("ab\ncd", "ab\nxcd"), ("cd", "xcd"));
    assert_eq!(edit("abcd", "ab\ncd"), ("abcd", "ab\ncd"));
    assert_eq!(edit("ab\ncd", "abcd"), ("ab\ncd", "abcd"));
    assert_eq!(edit("ab\ncd", "abX\ncd"), ("ab", "abX"));
    assert_eq!(edit("ab\n", "ab\nc"), ("", "c"));
    assert_eq!(edit("a\nb\nc", "a\nc"), ("b\nc", "c"));
    assert_eq!(edit("aaa", "aa"), ("aaa", "aa"));
    assert_eq!(edit("", "x"), ("", "x"));
    assert_eq!(edit("x", ""), ("x", ""));
}
