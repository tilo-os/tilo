//! Toolbar action vocabulary and the document's colour palettes.

use ui::icons::Icon;

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
    FontCycle,
    FontSizeDec,
    FontSizeInc,
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
    ToggleHistory,
    InsertToc,
    NewMenu,
}

impl ToolAction {
    /// Whether the command changes the document (and so ends an undo step).
    /// View toggles and commands that only open a menu or a picker do not.
    pub(super) fn edits_document(self) -> bool {
        !matches!(
            self,
            ToolAction::ToggleStyles
                | ToolAction::ToggleRuler
                | ToolAction::ToggleOutline
                | ToolAction::ToggleHistory
                | ToolAction::NewMenu
                | ToolAction::TextColorPalette
                | ToolAction::HighlightPalette
                | ToolAction::Hyperlink
                | ToolAction::InsertImage
        )
    }
}

/// One entry of the toolbar this frame: a command (with its face, and
/// whether it is a toggle that is on), a group separator or the flexible
/// space that pushes the view toggles to the trailing edge.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum ToolEntry {
    Command {
        action: ToolAction,
        label: String,
        icon: Option<Icon>,
        active: Option<bool>,
    },
    Separator,
    Spacer,
}

impl ToolEntry {
    pub(super) fn command(action: ToolAction, label: impl Into<String>) -> Self {
        ToolEntry::Command {
            action,
            label: label.into(),
            icon: None,
            active: None,
        }
    }

    pub(super) fn toggle(action: ToolAction, label: impl Into<String>, on: bool) -> Self {
        ToolEntry::Command {
            action,
            label: label.into(),
            icon: None,
            active: Some(on),
        }
    }

    pub(super) fn icon(action: ToolAction, icon: Icon, name: impl Into<String>) -> Self {
        ToolEntry::Command {
            action,
            label: name.into(),
            icon: Some(icon),
            active: None,
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
                    icon: ia,
                    active: aa,
                },
                ToolEntry::Command {
                    action: b,
                    label: lb,
                    icon: ib,
                    active: ab,
                },
            ) => a == b && la == lb && ia == ib && aa.is_some() == ab.is_some(),
            (ToolEntry::Separator, ToolEntry::Separator)
            | (ToolEntry::Spacer, ToolEntry::Spacer) => true,
            _ => false,
        }
    }
}
