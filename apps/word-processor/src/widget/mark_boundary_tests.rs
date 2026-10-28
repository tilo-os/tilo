use super::text_runs::{BlockRuns, char_baseline, char_color, char_has, mark_style_segments};
use tilo_doc::document::MarkSpan;
// Independent retained per-character reference for the mark-boundary optimization.
use super::text_runs::{block_runs, char_font_family, char_font_size, char_highlight};
use super::*;
use tilo_doc::document::MarkValue;
use ui::richtext::RichSpan;

fn reference_block_runs(block: &BlockView) -> BlockRuns {
    let chars: Vec<char> = block.text.chars().collect();
    let len = chars.len();
    // Run key: f32 size as bits so the tuple is comparable. Highlight (background)
    // and super/subscript baseline join the key so a change in either splits runs.
    let key = |i: usize| {
        (
            char_has(i, &block.marks, "strong"),
            char_has(i, &block.marks, "em"),
            char_font_size(i, &block.marks).map(f32::to_bits),
            char_font_family(i, &block.marks),
            char_color(i, &block.marks),
            char_highlight(i, &block.marks),
            char_baseline(i, &block.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < len {
        let k = key(i);
        let mut j = i;
        while j < len && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family, color, background, baseline) = k;
        runs.push(RichSpan {
            text: chars.get(i..j).unwrap_or(&[]).iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background,
            baseline,
            kind: None,
            workspace_emoji: false,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        block
            .marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| {
                (
                    char_to_byte(&block.text, m.start),
                    char_to_byte(&block.text, m.end),
                )
            })
            .collect::<Vec<_>>()
    };
    // Links render underlined as well as blue.
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    (runs, underlines, deco("strike"))
}

fn reference_block_runs_markup(block: &BlockView, show_markup: bool) -> BlockRuns {
    if !show_markup {
        return reference_block_runs(block);
    }
    let chars: Vec<char> = block.text.chars().collect();
    let len = chars.len();
    // An `ins` run is green, a `del` run red.
    let track_color = |i: usize| -> Option<[u8; 3]> {
        if char_has(i, &block.marks, tilo_doc::schema::MARK_INS) {
            Some(INSERTION_RGB)
        } else if char_has(i, &block.marks, tilo_doc::schema::MARK_DEL) {
            Some(DELETION_RGB)
        } else {
            None
        }
    };
    // The run key now also folds in the tracked-change review colour, which
    // overrides any explicit `color` mark over the same range (review colour
    // wins for legibility).
    let key = |i: usize| {
        (
            char_has(i, &block.marks, "strong"),
            char_has(i, &block.marks, "em"),
            char_font_size(i, &block.marks).map(f32::to_bits),
            char_font_family(i, &block.marks),
            track_color(i).or_else(|| char_color(i, &block.marks)),
            char_highlight(i, &block.marks),
            char_baseline(i, &block.marks),
        )
    };
    let mut runs = Vec::new();
    let mut i = 0;
    while i < len {
        let k = key(i);
        let mut j = i;
        while j < len && key(j) == k {
            j += 1;
        }
        let (bold, italic, size_bits, family, color, background, baseline) = k;
        runs.push(RichSpan {
            text: chars.get(i..j).unwrap_or(&[]).iter().collect(),
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background,
            baseline,
            kind: None,
            workspace_emoji: false,
        });
        i = j;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    let deco = |name: &str| {
        block
            .marks
            .iter()
            .filter(|m| m.name == name)
            .map(|m| {
                (
                    char_to_byte(&block.text, m.start),
                    char_to_byte(&block.text, m.end),
                )
            })
            .collect::<Vec<_>>()
    };
    let mut underlines = deco("underline");
    underlines.extend(deco("link"));
    let strikes = deco("strike");
    // NOTE: the `ins`/`del` review decorations (underline for ins, strike for
    // del) are *not* added here — they are drawn in the body pass in the review
    // colour (`draw_rich` paints these decorations in the block ink, which would
    // mismatch the coloured glyphs). Keeping them out of the shaped layout also
    // keeps `block_runs`'s underline/strike contract unchanged.
    (runs, underlines, strikes)
}

/// Mark boundaries avoid per-character resolution and merge equal styles, and their runs match the
/// per-character reference.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
)]
fn mark_boundary_runs_merge_equal_styles_and_match_the_reference() {
    // Phase: mark_boundaries_avoid_per_character_resolution_and_merge_equal_styles.
    {
        let marks = vec![MarkSpan {
            start: 100,
            end: 900,
            name: "strong".into(),
            value: MarkValue::Bool(true),
        }];
        let mut calls = 0;
        let segments = mark_style_segments(1000, &marks, |i| {
            calls += 1;
            char_has(i, &marks, "strong")
        });
        assert_eq!(
            segments,
            vec![(0, 100, false), (100, 900, true), (900, 1000, false)]
        );
        assert_eq!(calls, 3);
        let same = mark_style_segments(1000, &marks, |_| false);
        assert_eq!(same, vec![(0, 1000, false)]);
        assert!(mark_style_segments(0, &marks, |_| true).is_empty());
    }

    // Phase: mark_boundary_runs_match_per_character_reference.
    {
        let names = [
            "strong",
            "em",
            "size",
            "fontSize",
            "font",
            "fontFamily",
            "color",
            "highlight",
            "super",
            "sub",
            "ins",
            "del",
            "link",
            "underline",
            "strike",
            "unknown",
        ];
        for text in [
            "",
            "a",
            "Aé中🙂e\u{301} abc defghijklmnop",
            "plain text repeated ".repeat(50).as_str(),
        ] {
            let len = text.chars().count();
            for seed in 0..48usize {
                let mut marks = Vec::new();
                for (i, name) in names.iter().enumerate() {
                    let start = (seed * 7 + i * 3) % (len + 5);
                    let end = (seed * 11 + i * 7) % (len + 5);
                    let value = match *name {
                        "size" | "fontSize" => MarkValue::Int(12 + (i % 4) as i64),
                        "font" => MarkValue::Str("serif".into()),
                        "fontFamily" => MarkValue::Str("mono".into()),
                        "color" | "highlight" => MarkValue::Str("#a14b07".into()),
                        "link" => MarkValue::Str("https://example.invalid".into()),
                        _ => MarkValue::Bool(seed % 2 == 0),
                    };
                    marks.push(MarkSpan {
                        start,
                        end,
                        name: (*name).into(),
                        value,
                    });
                }
                // First matching values, including malformed values, retain precedence.
                marks.push(MarkSpan {
                    start: 0,
                    end: len,
                    name: "size".into(),
                    value: MarkValue::Str("bad".into()),
                });
                marks.push(MarkSpan {
                    start: 0,
                    end: len,
                    name: "size".into(),
                    value: MarkValue::Int(99),
                });
                if seed % 2 == 0 {
                    marks.reverse();
                }
                let block = BlockView {
                    id: "fixture".into(),
                    kind: "paragraph".into(),
                    text: text.into(),
                    marks,
                    attrs: std::collections::BTreeMap::default(),
                    children: vec![],
                };
                for review in [false, true] {
                    let (actual, actual_underlines, actual_strikes) = block_runs(&block, review);
                    let (expected, expected_underlines, expected_strikes) =
                        reference_block_runs_markup(&block, review);
                    let fields = |r: RichSpan| {
                        (
                            r.text,
                            r.bold,
                            r.italic,
                            r.size.map(f32::to_bits),
                            r.family,
                            r.color,
                            r.background,
                            r.baseline,
                        )
                    };
                    assert_eq!(
                        actual.into_iter().map(fields).collect::<Vec<_>>(),
                        expected.into_iter().map(fields).collect::<Vec<_>>(),
                        "seed={seed} review={review} text={text:?}"
                    );
                    assert_eq!(actual_underlines, expected_underlines);
                    assert_eq!(actual_strikes, expected_strikes);
                }
            }
        }
    }
}
