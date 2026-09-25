//! Toolbar action vocabulary, palettes, and responsive row packing.

use ui::chrome::{LabelCue, ToolbarItem};

use super::TOOLBAR_MODEL;
use crate::num;

pub(super) const STYLE_PALETTE: [[u8; 3]; 7] = [
    [0x1c, 0x1c, 0x20],
    [0x55, 0x57, 0x60],
    [0xb0, 0x3a, 0x2e],
    [0x1a, 0x56, 0xc4],
    [0x2e, 0x7d, 0x46],
    [0xb5, 0x7e, 0x10],
    [0x7a, 0x3a, 0xa0],
];

pub(super) const HIGHLIGHT_PALETTE: [[u8; 3]; 5] = [
    [0xff, 0xf3, 0x5c],
    [0x9b, 0xf0, 0x6a],
    [0x6c, 0xe0, 0xf0],
    [0xff, 0x9a, 0xd0],
    [0xff, 0xb1, 0x4d],
];

pub(super) const COMPACT_TOOLBAR_BREAKPOINT: f64 = 720.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ToolAction {
    Bold,
    Italic,
    Underline,
    Strike,
    FontCycle,
    FontSizeDec,
    FontSizeInc,
    TextColor(Option<[u8; 3]>),
    Highlight(Option<[u8; 3]>),
    Super,
    Sub,
    Hyperlink,
    InsertImage,
    InsertTable,
    TableRowAdd,
    TableRowDel,
    TableColAdd,
    TableColDel,
    TableDelete,
    ToggleStyles,
    ToggleRuler,
    ToggleOutline,
    ToggleHistory,
    InsertToc,
    NewMenu,
    More,
}

pub(super) fn item(
    action: ToolAction,
    label: impl Into<String>,
    width: f64,
    active: bool,
) -> (ToolAction, ToolbarItem) {
    let cue = match action {
        ToolAction::Bold => LabelCue::Bold,
        ToolAction::Underline => LabelCue::Underline,
        ToolAction::Strike => LabelCue::Strike,
        _ => LabelCue::None,
    };
    let group_start = matches!(
        action,
        ToolAction::NewMenu
            | ToolAction::Bold
            | ToolAction::FontCycle
            | ToolAction::InsertTable
            | ToolAction::InsertImage
            | ToolAction::TableRowAdd
            | ToolAction::ToggleStyles
            | ToolAction::ToggleOutline
            | ToolAction::More
    );
    (
        action,
        ToolbarItem::new(label, width)
            .active(active)
            .cue(cue)
            .group_start(group_start),
    )
}

pub(super) fn swatch_reserve_width() -> f64 {
    let sw = 20.0;
    let gap = 5.0;
    let n_color = num::f64_of_usize(STYLE_PALETTE.len() + 1); // +1 neutral/default swatch
    let n_hl = num::f64_of_usize(HIGHLIGHT_PALETTE.len() + 1); // +1 "no highlight" swatch
    let color = n_color * sw + (n_color - 1.0) * gap;
    let hl = n_hl * sw + (n_hl - 1.0) * gap;
    // right edge margin + colour strip + inter-strip gap + highlight strip
    14.0 + color + 10.0 + hl
}

pub(super) fn pack_rows(
    widths: &[f64],
    group_starts: &[bool],
    avail_w: f64,
    text_scale: f64,
) -> Vec<(usize, usize)> {
    let mut rows = Vec::new();
    let metrics = ui::chrome::toolbar::metrics(num::f32_of_f64(text_scale));
    let usable = (avail_w - metrics.leading_pad - TOOLBAR_MODEL.right_pad * text_scale).max(1.0);
    let mut row_start = 0usize;
    let mut x = 0.0f64; // running width consumed within the current row (post-pad)
    for (i, (&w, &group_start)) in widths.iter().zip(group_starts.iter()).enumerate() {
        let item_w = metrics.item_advance(w, group_start);
        // If this isn't the first item on the row and it would overflow, wrap.
        if i > row_start && x + item_w > usable {
            rows.push((row_start, i));
            row_start = i;
            x = 0.0;
        }
        x += item_w;
    }
    rows.push((row_start, widths.len()));
    rows
}
