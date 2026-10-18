//! Keyboard-shortcut help: a [`ModalDialog`] over a reference of every Notes
//! shortcut, grouped by what they do. Ctrl+/ (or F1) opens it; Escape, the
//! close control or a scrim click dismisses it.

use std::cell::Cell;
use std::rc::Rc;

use vello::kurbo::Rect;
use winit::event::WindowEvent;

use ui::Widget;
use ui::text_render::TextKind;
use ui::widget::{
    Column, Element, ModalDialog, Property, PropertyColumn, PropertyList, Section, SectionHeader,
};

/// One group of shortcuts: a Fluent key for its title, and a Fluent key and
/// the keys for each row.
struct Group {
    label: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

/// Every Notes shortcut, in the order a person meets the tasks: real key
/// chords only, each listed once. Keys are spelled by [`spell`].
const GROUPS: &[Group] = &[
    Group {
        label: "notes-help-group-notes",
        rows: &[
            ("notes-help-new-note", "{cmd:N}"),
            ("notes-help-new-sub-note", "{cmd+shift:N}"),
            ("notes-help-jump", "{cmd:K}"),
            ("notes-help-back-forward", "{alt:Left} / {alt:Right}"),
            ("notes-help-rename", "{F2}"),
            ("notes-help-note-actions", "{shift:F10}"),
            ("notes-help-note-move", "{alt:Up} / {alt:Down}"),
            ("notes-help-note-indent", "{Tab} / {shift:Tab}"),
            ("notes-help-delete", "{Delete}"),
        ],
    },
    Group {
        label: "notes-help-group-writing",
        rows: &[
            ("notes-help-find", "{cmd:F}"),
            ("notes-help-soft-break", "{shift:Enter}"),
            ("notes-help-block-menu", "/"),
            ("notes-help-link", "[["),
            ("notes-help-embed", "![["),
            ("notes-help-outline-indent", "{Tab} / {shift:Tab}"),
            ("notes-help-block-indent", "{cmd:]} / {cmd:[}"),
            ("notes-help-block-move", "{alt:Up} / {alt:Down}"),
            ("notes-help-block-duplicate", "{cmd:D}"),
            ("notes-help-editor-leave", "{Esc}, {Tab}"),
        ],
    },
    Group {
        label: "notes-help-group-formatting",
        rows: &[
            ("notes-help-bold", "{cmd:B}"),
            ("notes-help-italic", "{cmd:I}"),
            ("notes-help-inline-code", "{cmd:E}"),
            ("notes-help-headings", "# / ## / ###"),
            ("notes-help-bullet", "- "),
            ("notes-help-numbered", "1. "),
            ("notes-help-checklist", "[ ] "),
            ("notes-help-quote", "> "),
            ("notes-help-code", "```"),
        ],
    },
    Group {
        label: "notes-help-group-code",
        rows: &[
            ("notes-help-code-exit", "{cmd:Enter}"),
            ("notes-help-code-before", "{cmd+shift:Enter}"),
            ("notes-help-code-blank", "{shift:Enter}"),
            ("notes-help-code-indent", "{Tab} / {shift:Tab}"),
            ("notes-help-code-copy", "{cmd+shift:C}"),
            ("notes-help-code-select", "{cmd:A}"),
        ],
    },
    Group {
        label: "notes-help-group-help",
        rows: &[
            ("notes-help-shortcuts", "{cmd:/}"),
            ("notes-help-close", "{Esc}"),
        ],
    },
];

/// A row's keys as this platform spells them. `{cmd:K}` is the command key
/// with K (Ctrl+K, or the Command key on a Mac), `{cmd+shift:Enter}` adds
/// Shift, `{alt:Up}` is Alt with Up and `{F2}` is a key on its own, each in
/// the active keyboard's legends; everything else is shown as written.
fn spell(template: &str) -> String {
    let conventions = ui::PlatformConventions::default();
    let mut out = String::new();
    let mut rest = template;
    while let Some((before, after)) = rest.split_once('{') {
        out.push_str(before);
        let Some((spec, tail)) = after.split_once('}') else {
            out.push('{');
            rest = after;
            break;
        };
        let (modifiers, key) = spec.split_once(':').unwrap_or(("", spec));
        let mut shortcut = if modifiers.contains("cmd") {
            ui::Shortcut::command(key)
        } else {
            ui::Shortcut::key(key)
        };
        if modifiers.contains("shift") {
            shortcut = shortcut.shift();
        }
        if modifiers.contains("alt") {
            shortcut = shortcut.alt();
        }
        out.push_str(&shortcut.label(conventions));
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// One group of the reference: its title over its shortcut rows, the keys
/// in the column every group shares.
fn group(group: &Group, keys: &PropertyColumn) -> Element {
    let rows = group
        .rows
        .iter()
        .map(|(label, keys)| {
            Property::new(ui::i18n::I18nManager::lookup(label, None), spell(keys))
                .with_kind(TextKind::Identifier)
        })
        .collect();
    Box::new(
        Section::new(vec![Box::new(PropertyList::new(rows).shared_column(keys))])
            .with_header(SectionHeader::group(ui::i18n!(group.label))),
    )
}

/// The body of the sheet: every group in one column. A window too short for
/// all of it gets the dialog's own scrolling body.
fn sheet_body() -> Element {
    let keys = PropertyColumn::new();
    let mut column = Column::new().with_intrinsic_height().with_no_padding();
    for entry in GROUPS {
        column = column.push(group(entry, &keys));
    }
    Box::new(column)
}

/// The shortcuts sheet, closed until Ctrl+/ opens it.
pub struct ShortcutsOverlay {
    dialog: ModalDialog<Column>,
    dismissed: Rc<Cell<bool>>,
    visible: bool,
}

impl ShortcutsOverlay {
    pub fn new() -> Self {
        let dismissed = Rc::new(Cell::new(false));
        Self {
            dialog: Self::dialog(&dismissed),
            dismissed,
            visible: false,
        }
    }

    fn dialog(dismissed: &Rc<Cell<bool>>) -> ModalDialog<Column> {
        let dismissed = Rc::clone(dismissed);
        ModalDialog::new(
            ui::i18n!("notes-help-title"),
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(sheet_body()),
        )
        .with_test_id("notes-shortcuts")
        .on_dismiss(move || dismissed.set(true))
    }

    pub fn is_visible(&self) -> bool {
        self.visible
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.dismissed.set(false);
        if self.visible != visible {
            self.visible = visible;
            self.dialog.set_open(visible);
        }
    }

    pub fn toggle(&mut self) {
        self.set_visible(!self.visible);
    }

    /// The sheet's card, for hit-testing in tests.
    pub fn card(&self) -> Rect {
        self.dialog.dialog_rect()
    }

    /// The sheet's close control, once it has laid out.
    pub fn close_rect(&self) -> Option<Rect> {
        self.dialog.close_rect()
    }
}

impl Default for ShortcutsOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl ui::widget::Decorator for ShortcutsOverlay {
    type Inner = ModalDialog<Column>;

    fn inner(&self) -> &Self::Inner {
        &self.dialog
    }

    fn inner_mut(&mut self) -> &mut Self::Inner {
        &mut self.dialog
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if !self.visible {
            return;
        }
        self.dialog.handle_event(event, ctx);
        if self.dismissed.replace(false) {
            self.set_visible(false);
        }
        // The sheet is modal: nothing reaches the editor behind it.
        *ctx.handled = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(
        clippy::literal_string_with_formatting_args,
        reason = "the templates are not format strings"
    )]
    #[test]
    fn keys_are_spelled_in_this_platforms_conventions() {
        let command = ui::Shortcut::command("K").label(ui::PlatformConventions::default());
        assert_eq!(spell("{cmd:K}"), command);
        assert_eq!(
            spell("**text** or {cmd:B}"),
            format!(
                "**text** or {}",
                ui::Shortcut::command("B").label(ui::PlatformConventions::default())
            )
        );
        assert!(spell("{cmd+shift:C}").contains('C'));
        assert_eq!(
            spell("{alt:Up}"),
            ui::Shortcut::key("Up")
                .alt()
                .label(ui::PlatformConventions::default())
        );
        assert_eq!(spell("{F2}"), "F2");
        assert_eq!(spell("[["), "[[");
        for entry in GROUPS {
            for (_, keys) in entry.rows {
                let spelled = spell(keys);
                assert!(!spelled.contains('{'), "unspelled keys: {spelled}");
                assert!(
                    !spelled.contains(['←', '→', '↑', '↓', '↵']),
                    "keys are named, never drawn with arrows: {spelled}"
                );
                assert!(
                    !spelled.contains("Cmd-") && !spelled.contains("Ctrl-"),
                    "a platform's keys are never both listed: {spelled}"
                );
            }
        }
    }

    #[test]
    fn the_sheet_opens_and_closes_and_reports_it() {
        let mut overlay = ShortcutsOverlay::new();
        assert!(!overlay.is_visible());
        overlay.set_visible(true);
        assert!(overlay.is_visible());
        overlay.toggle();
        assert!(!overlay.is_visible());
    }
}
