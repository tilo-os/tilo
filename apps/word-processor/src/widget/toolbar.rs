//! Toolbar action vocabulary, palettes, and responsive row packing.

use ui::chrome::{LabelCue, ToolbarItem};

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

pub(super) const TOOLBAR_RIGHT_PAD: f64 = 18.0;
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

pub(super) fn swatch_reserve_width(text_scale: f64) -> f64 {
    let swatch = 20.0 * text_scale;
    let gap = 5.0 * text_scale;
    let colors = num::f64_of_usize(STYLE_PALETTE.len() + 1);
    let highlights = num::f64_of_usize(HIGHLIGHT_PALETTE.len() + 1);
    let color_width = colors * swatch + (colors - 1.0) * gap;
    let highlight_width = highlights * swatch + (highlights - 1.0) * gap;
    14.0 * text_scale + color_width + 10.0 * text_scale + highlight_width
}

pub(super) fn pack_rows(
    widths: &[f64],
    group_starts: &[bool],
    available_width: f64,
    text_scale: f64,
) -> Vec<(usize, usize)> {
    let metrics = ui::chrome::toolbar::metrics(text_scale as f32);
    let usable = (available_width - metrics.left_padding - TOOLBAR_RIGHT_PAD * text_scale).max(1.0);
    let mut rows = Vec::new();
    let mut row_start = 0;
    let mut used = 0.0;
    for (index, (&width, &group_start)) in widths.iter().zip(group_starts).enumerate() {
        let separator = if group_start { metrics.group_gap } else { 0.0 };
        let item_width = separator + width * text_scale + metrics.item_gap;
        if index > row_start && used + item_width > usable {
            rows.push((row_start, index));
            row_start = index;
            used = 0.0;
        }
        used += item_width;
    }
    rows.push((row_start, widths.len()));
    rows
}
