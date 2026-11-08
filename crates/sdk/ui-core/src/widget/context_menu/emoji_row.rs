//! A menu's emoji row: the person's most used emoji to react with, in one row at the top of a
//! message's menu, and a last cell that opens the emoji picker for any other.
//!
//! The row is one row of the menu for the arrows Up and Down; Left and Right move between its
//! cells, Enter picks the armed one. The pointer arms the cell under it. Each cell is published
//! to assistive technology as a menu item of its own, named by the emoji.

use vello::kurbo::Rect;

use super::{ContextMenu, MenuAction, MenuItem};
use crate::a11y::{AccessNode, AccessState, Role};

/// An emoji row's height as a share of a command row's: the emoji are drawn larger than a
/// label, to be told apart at a glance.
pub(super) const ROW_SHARE: f64 = 1.5;

/// A row of emoji to pick one from.
#[derive(Clone)]
pub struct EmojiRow {
    pub(super) emoji: Vec<String>,
    pub(super) pick: std::rc::Rc<dyn Fn(&str)>,
    /// Opens the emoji picker, as the last cell.
    pub(super) more: Option<MenuAction>,
}

impl EmojiRow {
    /// How many cells it has: one per emoji, and the picker's.
    pub(super) fn cells(&self) -> usize {
        self.emoji
            .len()
            .saturating_add(usize::from(self.more.is_some()))
    }

    /// The emoji offered, in order.
    #[must_use]
    pub fn emoji(&self) -> &[String] {
        &self.emoji
    }

    /// Choose the cell `cell`: its emoji is picked, or the picker opens. Whether anything ran.
    pub(super) fn choose(&self, cell: usize) -> bool {
        if let Some(emoji) = self.emoji.get(cell) {
            (self.pick)(emoji);
            return true;
        }
        match &self.more {
            Some(open) if cell == self.emoji.len() => {
                open();
                true
            }
            _ => false,
        }
    }

    /// What the cell `cell` is called: its emoji, or what the picker's cell does.
    fn name(&self, cell: usize) -> String {
        self.emoji
            .get(cell)
            .cloned()
            .unwrap_or_else(|| crate::i18n::widget_format("emoji-more", &[]))
    }
}

impl ContextMenu {
    /// The cells of the emoji row `index`, side by side across it, each as wide as it is tall
    /// or as the row allows. The cells run the panel's whole width: they are
    /// emoji, not a label, and keep the menu's own inset from the edge.
    fn emoji_cells(&self, index: usize, row: &EmojiRow) -> Vec<Rect> {
        let Some(rect) = self.row_bounds(index) else {
            return Vec::new();
        };
        super::rows::emoji_cells(rect, self.list.row_inset(), row.cells())
    }

    /// The armed row's cell under the pointer, when the armed row is an emoji row and the
    /// pointer is over one of its cells.
    pub(super) fn follow_emoji_cell(&mut self) {
        let Some(index) = self.hovered else {
            return;
        };
        let Some(MenuItem::Emoji(row)) = self.items.get(index) else {
            return;
        };
        let cells = self.emoji_cells(index, row);
        let point = self.last_cursor;
        let under = cells.iter().position(|cell| {
            point.x >= cell.x0 - f64::from(crate::tokens::Spacing::BASE.s2) / 2.0
                && point.x < cell.x1 + f64::from(crate::tokens::Spacing::BASE.s2) / 2.0
        });
        if let Some(cell) = under
            && self.list.cell() != cell
        {
            self.list.set_cell(cell);
            self.dirty = true;
        }
    }

    /// Move the armed cell of the armed emoji row one step (`forward`: towards the trailing
    /// end). Whether the armed row is an emoji row, which takes the step.
    pub(super) fn step_emoji_cell(&mut self, forward: bool) -> bool {
        let Some(MenuItem::Emoji(row)) = self.hovered.and_then(|index| self.items.get(index))
        else {
            return false;
        };
        let count = row.cells();
        if count == 0 {
            return true;
        }
        let cell = if forward {
            self.list.cell().saturating_add(1) % count
        } else {
            self.list
                .cell()
                .checked_sub(1)
                .unwrap_or(count.saturating_sub(1))
        };
        self.list.set_cell(cell);
        self.dirty = true;
        true
    }

    /// The emoji row `index` for assistive technology: a group of menu items, one per cell.
    pub(super) fn emoji_row_node(&self, index: usize, row: &EmojiRow) -> AccessNode {
        let cells = self.emoji_cells(index, row);
        let count = row.cells();
        let children = (0..count)
            .map(|cell| {
                AccessNode::new(Role::MenuItem)
                    .with_name(row.name(cell))
                    .with_action_target(format!("context-menu:emoji:{index}:{cell}"))
                    .with_bounds(cells.get(cell).copied().unwrap_or(Rect::ZERO))
                    .with_position(cell.saturating_add(1), count)
                    .with_state(AccessState {
                        focusable: true,
                        focused: self.open
                            && self.hovered == Some(index)
                            && self.list.cell() == cell,
                        hidden: !self.open,
                        ..Default::default()
                    })
            })
            .collect();
        AccessNode::new(Role::Group)
            .with_name(crate::i18n::widget_format("emoji-bar", &[]))
            .with_bounds(self.row_bounds(index).unwrap_or(Rect::ZERO))
            .with_children(children)
    }

    /// Pick the cell an assistive technology activated: `context-menu:emoji:<row>:<cell>`.
    pub(super) fn activate_emoji_target(&mut self, target: &str) -> bool {
        let Some((row, cell)) = target
            .strip_prefix("context-menu:emoji:")
            .and_then(|rest| rest.split_once(':'))
        else {
            return false;
        };
        let (Ok(row), Ok(cell)) = (row.parse::<usize>(), cell.parse::<usize>()) else {
            return false;
        };
        if !matches!(self.items.get(row), Some(MenuItem::Emoji(_))) {
            return false;
        }
        self.set_hover(Some(row));
        self.list.set_cell(cell);
        self.click_item(row);
        true
    }
}
