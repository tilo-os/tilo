//! Document marks transformed into renderable rich-text runs and annotations.

use super::{byte_to_char, char_to_byte};
use crate::num;
use crate::review_overlay::{DELETION_RGB, INSERTION_RGB};
use std::collections::HashMap;
use tilo_doc::document::{MarkSpan, MarkValue};
use tilo_doc::snapshot::{BlockView, NoteView};
use ui::richtext::{BaselineShift, FontFamily, RichSpan};
use ui::text_edit::EditCore;

pub(super) type BlockRuns = (Vec<RichSpan>, Vec<(usize, usize)>, Vec<(usize, usize)>);
type ChangeDecoRanges = (Vec<(usize, usize)>, Vec<(usize, usize)>);

/// True when a mark named `name` fully covers the half-open char range
/// `[start, end)`. Same cover rule as `tilo_doc::model::has_mark_over`.
pub(super) fn marks_cover_range(marks: &[MarkSpan], name: &str, start: usize, end: usize) -> bool {
    marks
        .iter()
        .any(|m| m.name == name && m.start <= start && m.end >= end)
}

pub(super) fn char_has(index: usize, marks: &[MarkSpan], name: &str) -> bool {
    marks
        .iter()
        .any(|mark| mark.name == name && index >= mark.start && index < mark.end)
}

pub(super) fn char_font_size(index: usize, marks: &[MarkSpan]) -> Option<f32> {
    marks
        .iter()
        .find(|mark| {
            matches!(mark.name.as_str(), "size" | "fontSize")
                && index >= mark.start
                && index < mark.end
        })
        .and_then(|mark| mark.value.as_int())
        .map(num::f32_of_i64)
}

pub(super) fn char_font_family(index: usize, marks: &[MarkSpan]) -> Option<FontFamily> {
    marks
        .iter()
        .find(|mark| {
            matches!(mark.name.as_str(), "font" | "fontFamily")
                && index >= mark.start
                && index < mark.end
        })
        .and_then(|mark| mark.value.as_str())
        .map(|value| match value {
            "serif" => FontFamily::Serif,
            "mono" => FontFamily::Mono,
            _ => FontFamily::Sans,
        })
}

const LINK_RGB: [u8; 3] = [0x1a, 0x56, 0xc4];

pub(super) fn char_color(index: usize, marks: &[MarkSpan]) -> Option<[u8; 3]> {
    if char_has(index, marks, "link") {
        return Some(LINK_RGB);
    }
    marks
        .iter()
        .find(|mark| mark.name == "color" && index >= mark.start && index < mark.end)
        .and_then(|mark| mark.value.as_str())
        .and_then(parse_hex_rgb)
}

pub(super) fn char_highlight(index: usize, marks: &[MarkSpan]) -> Option<[u8; 3]> {
    marks
        .iter()
        .find(|mark| mark.name == "highlight" && index >= mark.start && index < mark.end)
        .and_then(|mark| mark.value.as_str())
        .and_then(parse_hex_rgb)
}

pub(super) fn char_baseline(index: usize, marks: &[MarkSpan]) -> BaselineShift {
    if char_has(index, marks, "super") {
        BaselineShift::Super
    } else if char_has(index, marks, "sub") {
        BaselineShift::Sub
    } else {
        BaselineShift::None
    }
}

pub(super) fn parse_hex_rgb(value: &str) -> Option<[u8; 3]> {
    ui::color::parse_hex(value).map(ui::color::to_rgb8)
}

/// Shape `block` into style runs plus underline and strike byte ranges.
/// With `show_markup`, tracked insertions and deletions are tinted.
pub(super) fn block_runs(block: &BlockView, show_markup: bool) -> BlockRuns {
    let chars: Vec<char> = block.text.chars().collect();
    let len = chars.len();
    // An `ins` run is green, a `del` run red.
    let track_color = |i: usize| -> Option<[u8; 3]> {
        if !show_markup {
            None
        } else if char_has(i, &block.marks, tilo_doc::schema::MARK_INS) {
            Some(INSERTION_RGB)
        } else if char_has(i, &block.marks, tilo_doc::schema::MARK_DEL) {
            Some(DELETION_RGB)
        } else {
            None
        }
    };
    // Run key: f32 size as bits so the tuple is comparable. Highlight
    // (background) and super/subscript baseline join the key so a change in
    // either splits runs. The tracked-change review colour overrides any
    // explicit `color` mark over the same range (review colour wins for
    // legibility).
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
    for (i, j, k) in mark_style_segments(len, &block.marks, key) {
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
        });
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
    let strikes = deco("strike");
    // NOTE: the `ins`/`del` review decorations (underline for ins, strike for
    // del) are *not* added here — they are drawn in the body pass in the review
    // colour (`draw_rich` paints these decorations in the block ink, which would
    // mismatch the coloured glyphs). Keeping them out of the shaped layout also
    // keeps the underline/strike ranges independent of review mode.
    (runs, underlines, strikes)
}

pub(super) fn block_change_decos(block: &BlockView) -> ChangeDecoRanges {
    let collect = |name: &str| {
        block
            .marks
            .iter()
            .filter(|mark| mark.name == name && mark.end > mark.start)
            .map(|mark| {
                (
                    char_to_byte(&block.text, mark.start),
                    char_to_byte(&block.text, mark.end),
                )
            })
            .collect::<Vec<_>>()
    };
    (
        collect(tilo_doc::schema::MARK_INS),
        collect(tilo_doc::schema::MARK_DEL),
    )
}

fn identified_ranges(block: &BlockView, names: &[&str]) -> Vec<(usize, usize, String)> {
    block
        .marks
        .iter()
        .filter(|mark| names.contains(&mark.name.as_str()) && mark.end > mark.start)
        .filter_map(|mark| {
            Some((
                char_to_byte(&block.text, mark.start),
                char_to_byte(&block.text, mark.end),
                mark.value.as_str()?.to_string(),
            ))
        })
        .collect()
}

pub(super) fn block_change_ranges(block: &BlockView) -> Vec<(usize, usize, String)> {
    identified_ranges(
        block,
        &[tilo_doc::schema::MARK_INS, tilo_doc::schema::MARK_DEL],
    )
}
pub(super) fn block_comment_ranges(block: &BlockView) -> Vec<(usize, usize, String)> {
    identified_ranges(block, &[tilo_doc::schema::MARK_COMMENT])
}

fn is_page_number_field(index: usize, marks: &[MarkSpan]) -> bool {
    marks.iter().any(|mark| {
        mark.name == tilo_doc::schema::MARK_FIELD
            && index >= mark.start
            && index < mark.end
            && mark.value.as_str() == Some(tilo_doc::schema::FIELD_PAGE_NUMBER)
    })
}

#[derive(Clone, Copy)]
enum FieldDisplay {
    Page(usize),
    Placeholder,
}

fn running_head_runs_with(block: &BlockView, display: FieldDisplay) -> Vec<RichSpan> {
    let chars = block.text.chars().collect::<Vec<_>>();
    let key = |index| {
        (
            char_has(index, &block.marks, "strong"),
            char_has(index, &block.marks, "em"),
            char_font_size(index, &block.marks).map(f32::to_bits),
            char_font_family(index, &block.marks),
            char_color(index, &block.marks),
            is_page_number_field(index, &block.marks),
        )
    };
    let mut runs = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let run_key = key(start);
        let mut end = start + 1;
        while end < chars.len() && key(end) == run_key {
            end += 1;
        }
        let (bold, italic, size_bits, family, color, is_field) = run_key;
        let text = if is_field {
            match display {
                FieldDisplay::Page(page) => page.to_string().repeat(end - start),
                FieldDisplay::Placeholder => "#".repeat(end - start),
            }
        } else {
            chars.get(start..end).unwrap_or(&[]).iter().collect()
        };
        runs.push(RichSpan {
            text,
            bold,
            italic,
            size: size_bits.map(f32::from_bits),
            family,
            color,
            background: None,
            baseline: BaselineShift::None,
            kind: None,
        });
        start = end;
    }
    if runs.is_empty() {
        runs.push(RichSpan::new(""));
    }
    runs
}

pub(super) fn running_head_runs(block: &BlockView, page: usize) -> Vec<RichSpan> {
    running_head_runs_with(block, FieldDisplay::Page(page))
}
pub(super) fn running_head_runs_editing(block: &BlockView) -> Vec<RichSpan> {
    running_head_runs_with(block, FieldDisplay::Placeholder)
}

/// `block` as it shows while the input method composes in it: the
/// composition laid into the text at the caret and underlined, every mark
/// moved with the text after it. A mark that runs up to the caret covers the
/// composition too, so it reads in the style it will be typed in.
pub(super) fn composed_block(block: &BlockView, core: &EditCore) -> BlockView {
    let Some(preedit) = core.preedit_display_range(&block.text) else {
        return block.clone();
    };
    let text = core.display_text(&block.text).into_owned();
    let at = byte_to_char(&text, preedit.start);
    let len = text
        .get(preedit)
        .map_or(0, |composed| composed.chars().count());
    let shift = |offset: usize| if offset < at { offset } else { offset + len };
    let mut marks: Vec<MarkSpan> = block
        .marks
        .iter()
        .map(|mark| MarkSpan {
            start: shift(mark.start),
            end: shift(mark.end),
            ..mark.clone()
        })
        .collect();
    marks.push(MarkSpan {
        start: at,
        end: at + len,
        name: "underline".to_owned(),
        value: MarkValue::Bool(true),
    });
    BlockView {
        text,
        marks,
        ..block.clone()
    }
}

pub(super) fn block_note_refs(block: &BlockView) -> Vec<(usize, String)> {
    (0..block.text.chars().count())
        .filter_map(|index| {
            block
                .marks
                .iter()
                .find(|mark| {
                    mark.name == tilo_doc::schema::MARK_NOTE_REF
                        && index >= mark.start
                        && index < mark.end
                })
                .and_then(|mark| mark.value.as_str())
                .map(|id| (index, id.to_string()))
        })
        .collect()
}

pub(super) fn note_numbering(blocks: &[BlockView]) -> HashMap<String, usize> {
    let mut numbers = HashMap::new();
    let mut next = 1;
    for block in blocks {
        for (_, id) in block_note_refs(block) {
            numbers.entry(id).or_insert_with(|| {
                let number = next;
                next += 1;
                number
            });
        }
    }
    numbers
}

pub(super) fn note_text(note: &NoteView) -> String {
    note.blocks
        .iter()
        .map(|block| block.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn mark_style_segments<K: PartialEq>(
    len: usize,
    marks: &[MarkSpan],
    mut key: impl FnMut(usize) -> K,
) -> Vec<(usize, usize, K)> {
    if len == 0 {
        return Vec::new();
    }
    let mut boundaries = vec![0, len];
    for mark in marks {
        boundaries.push(mark.start.min(len));
        boundaries.push(mark.end.min(len));
    }
    boundaries.sort_unstable();
    boundaries.dedup();
    let mut segments: Vec<(usize, usize, K)> = Vec::new();
    for pair in boundaries.windows(2) {
        let [start, end] = pair else { continue };
        let style = key(*start);
        if let Some((_, previous_end, previous_style)) = segments.last_mut()
            && *previous_style == style
        {
            *previous_end = *end;
        } else {
            segments.push((*start, *end, style));
        }
    }
    segments
}
