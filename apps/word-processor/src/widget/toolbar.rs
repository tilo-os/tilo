//! Toolbar action vocabulary and the document's colour palettes.

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum ToolAction {
    Bold,
    Italic,
    Underline,
    Strike,
    /// Set the selection's font family (a row of the Format menu's fonts).
    SetFont(ui::richtext::FontFamily),
    FontSizeDec,
    FontSizeInc,
    /// Make the selected paragraphs a bulleted list, or plain paragraphs
    /// again when they already are one.
    BulletList,
    NumberedList,
    TextColor(Option<[u8; 3]>),
    Highlight(Option<[u8; 3]>),
    TextColorPalette,
    /// Open the highlight swatch grid.
    HighlightPalette,
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
    InsertToc,
}

impl ToolAction {
    /// The glyph the command shows in the toolbar instead of its name (the
    /// name stays its tooltip and accessible name); `None` keeps the name.
    pub(super) fn icon(self) -> Option<ui::icons::Icon> {
        use ui::icons::Icon;
        Some(match self {
            ToolAction::Bold => Icon::Bold,
            ToolAction::Italic => Icon::Italic,
            ToolAction::Underline => Icon::Underline,
            ToolAction::Strike => Icon::Strikethrough,
            ToolAction::Super => Icon::Superscript,
            ToolAction::Sub => Icon::Subscript,
            ToolAction::TextColorPalette => Icon::Baseline,
            ToolAction::HighlightPalette => Icon::Highlighter,
            ToolAction::Hyperlink => Icon::Link,
            ToolAction::InsertImage => Icon::Image,
            ToolAction::InsertTable => Icon::Table,
            ToolAction::InsertToc => Icon::ListTree,
            ToolAction::FontSizeDec => Icon::AArrowDown,
            ToolAction::FontSizeInc => Icon::AArrowUp,
            ToolAction::ToggleRuler => Icon::Ruler,
            ToolAction::BulletList => Icon::List,
            ToolAction::NumberedList => Icon::ListOrdered,
            _ => return None,
        })
    }
}
