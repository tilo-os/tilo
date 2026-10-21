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
    /// The font picker: opens the families to choose from; never run itself.
    FontPicker,
    /// Set the selection's font family (a row of the font picker).
    SetFont(ui::richtext::FontFamily),
    FontSizeDec,
    FontSizeInc,
    /// Make the selected paragraphs a bulleted list, or plain paragraphs
    /// again when they already are one.
    BulletList,
    /// The same with a numbered list.
    NumberedList,
    TextColor(Option<[u8; 3]>),
    Highlight(Option<[u8; 3]>),
    /// Open the text-colour swatch grid.
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
    FileMenu,
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
            ToolAction::FileMenu => Icon::File,
            ToolAction::FontSizeDec => Icon::AArrowDown,
            ToolAction::FontSizeInc => Icon::AArrowUp,
            ToolAction::ToggleRuler => Icon::Ruler,
            ToolAction::BulletList => Icon::List,
            ToolAction::NumberedList => Icon::ListOrdered,
            _ => return None,
        })
    }

    /// Whether the command changes the document (and so ends an undo step).
    /// View toggles and commands that only open a menu or a picker do not.
    pub(super) fn edits_document(self) -> bool {
        !matches!(
            self,
            ToolAction::ToggleStyles
                | ToolAction::ToggleRuler
                | ToolAction::ToggleOutline
                | ToolAction::FileMenu
                | ToolAction::FontPicker
                | ToolAction::TextColorPalette
                | ToolAction::HighlightPalette
                | ToolAction::Hyperlink
                | ToolAction::InsertImage
        )
    }
}

/// One row of a toolbar command's menu: what choosing it runs, its label,
/// and whether it is the current choice.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ToolChoice {
    pub action: ToolAction,
    pub label: String,
    pub checked: bool,
}

/// One entry of the toolbar this frame: a command (with its label, whether
/// it is a toggle that is on, and the choices it opens when it is a menu),
/// a group separator or the flexible space that pushes the view toggles to
/// the trailing edge.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ToolEntry {
    Command {
        action: ToolAction,
        label: String,
        active: Option<bool>,
        /// Non-empty for a command that opens these choices as its menu.
        menu: Vec<ToolChoice>,
    },
    Separator,
    Spacer,
}

impl ToolEntry {
    pub(super) fn command(action: ToolAction, label: impl Into<String>) -> Self {
        ToolEntry::Command {
            action,
            label: label.into(),
            active: None,
            menu: Vec::new(),
        }
    }

    pub(super) fn toggle(action: ToolAction, label: impl Into<String>, on: bool) -> Self {
        ToolEntry::Command {
            action,
            label: label.into(),
            active: Some(on),
            menu: Vec::new(),
        }
    }

    /// A command labelled with the current choice that opens every choice
    /// as its menu, the current one checked (a picker).
    pub(super) fn picker(
        action: ToolAction,
        label: impl Into<String>,
        choices: Vec<ToolChoice>,
    ) -> Self {
        ToolEntry::Command {
            action,
            label: label.into(),
            active: None,
            menu: choices,
        }
    }

    /// Two entries with the same shape differ at most in a toggle's state,
    /// which the toolbar can take without being rebuilt.
    pub(super) fn same_shape(&self, other: &Self) -> bool {
        match (self, other) {
            (
                ToolEntry::Command {
                    action: a,
                    label: la,
                    active: aa,
                    menu: ma,
                },
                ToolEntry::Command {
                    action: b,
                    label: lb,
                    active: ab,
                    menu: mb,
                },
            ) => a == b && la == lb && aa.is_some() == ab.is_some() && ma == mb,
            (ToolEntry::Separator, ToolEntry::Separator)
            | (ToolEntry::Spacer, ToolEntry::Spacer) => true,
            _ => false,
        }
    }
}
