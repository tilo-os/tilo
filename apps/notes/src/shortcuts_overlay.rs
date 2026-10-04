//! Keyboard-shortcut help: a [`ModalDialog`] over a reference of every Notes
//! shortcut, grouped by what they do. `?` opens it; Escape, the close control
//! or a scrim click dismisses it.

use std::cell::Cell;
use std::rc::Rc;

use vello::kurbo::Rect;
use winit::event::WindowEvent;

use ui::Widget;
use ui::text_render::TextKind;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    Column, Element, ModalDialog, Property, PropertyList, Scrollable, Section, SectionHeader,
};

/// One group of shortcuts. The label titles the group; the rows list each
/// shortcut.
struct Group {
    label: &'static str,
    rows: &'static [(&'static str, &'static str)],
}

/// Canonical list of every shortcut surfaced in the help overlay. The
/// `?` shortcut deliberately includes itself so a user who's just
/// opened the overlay can find their way back out.
const GROUPS: &[Group] = &[
    Group {
        label: "Navigation",
        rows: &[
            ("Jump to note", "{cmd:K}"),
            ("Back / forward note", "Alt-← / Alt-→"),
            ("Move selection up / down", "↑ / ↓"),
            ("Collapse / expand note", "← / →"),
            ("Open shortcuts overlay", "?"),
            ("Close overlay / popover", "Esc"),
        ],
    },
    Group {
        label: "Editing",
        rows: &[
            ("Find in note", "{cmd:F}"),
            ("New paragraph", "Enter"),
            ("notes-help-outline-indent", "Tab / Shift-Tab"),
            ("notes-help-block-indent", "{cmd:]} / {cmd:[}"),
            ("Open block menu", "/"),
            ("Link another note", "[["),
            ("Toggle checklist item", "click"),
            ("Move focused block", "Alt-↑ / Alt-↓"),
            ("notes-help-code-exit", "{cmd:Enter}"),
            ("notes-help-code-before", "{cmd+shift:Enter}"),
            ("notes-help-code-copy", "{cmd+shift:C}"),
            ("notes-help-code-indent", "Tab / Shift-Tab"),
            ("notes-help-code-blank", "Shift-Enter"),
            ("notes-help-code-select", "{cmd:A}"),
        ],
    },
    Group {
        label: "Formatting",
        rows: &[
            ("Bold", "**text** or {cmd:B}"),
            ("Italic", "*text* or {cmd:I}"),
            ("Inline code", "`text` or {cmd:E}"),
            ("Heading 1 / 2 / 3", "# / ## / ###"),
            ("Quote", "> "),
            ("notes-help-code", "```python ↵"),
            ("notes-help-bullet", "- Space"),
            ("notes-help-numbered", "1. Space"),
            ("notes-help-checklist", "[ ] Space"),
        ],
    },
    Group {
        label: "Notes",
        rows: &[
            ("New note", "+ button"),
            ("New child of selected", "{cmd:N}"),
            ("Indent / outdent note", "Tab / Shift-Tab"),
            ("Reorder note up / down", "Alt-↑ / Alt-↓"),
            ("Rename note", "double-click / F2"),
            ("Note actions", "Menu / Shift-F10"),
            ("Link a note", "[[Note]]"),
            ("Embed a note", "![[Note]]"),
            ("Delete note", "Delete"),
        ],
    },
];

/// A row's keys as this platform spells them. `{cmd:K}` is the command key
/// with K (Ctrl+K, or the Command key on a Mac) and `{cmd+shift:Enter}` adds
/// Shift; everything else is shown as written.
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
        let (modifiers, key) = spec.split_once(':').unwrap_or(("cmd", spec));
        let mut shortcut = ui::Shortcut::command(key);
        if modifiers.contains("shift") {
            shortcut = shortcut.shift();
        }
        out.push_str(&shortcut.label(conventions));
        rest = tail;
    }
    out.push_str(rest);
    out
}

/// One group of the reference: its title over its shortcut rows.
fn group(group: &Group) -> Element {
    let rows = group
        .rows
        .iter()
        .map(|(label, keys)| {
            Property::new(ui::i18n::I18nManager::lookup(label, None), spell(keys))
                .with_kind(TextKind::Identifier)
        })
        .collect();
    Box::new(
        Section::new(vec![Box::new(PropertyList::new(rows))])
            .with_header(SectionHeader::group(group.label)),
    )
}

/// The body of the sheet: every group in one column, scrolling inside
/// `height` when the window bounds it.
fn sheet_body(height: Option<f64>) -> Element {
    let mut column = Column::new().with_intrinsic_height().with_no_padding();
    for entry in GROUPS {
        column = column.push(group(entry));
    }
    let scroll = Scrollable::new_vertical().with_child(Box::new(column));
    Box::new(match height {
        Some(height) => scroll.with_max_height(ui::num::f32_of_f64(height)),
        None => scroll,
    })
}

/// How tall the reference may be in `window`: what the sheet leaves between
/// its margins, its padding, its title and the gap under it.
fn body_height(window: Rect, theme: &Theme) -> f64 {
    let spacing = &theme.tokens.spacing;
    let title = f64::from(theme.text(TextRole::Title).line_height_px())
        .max(f64::from(theme.tokens.metrics.min_pointer_target));
    (window.height() - 4.0 * f64::from(spacing.s6) - title - f64::from(spacing.s4))
        .max(f64::from(spacing.s16))
}

/// The shortcuts sheet, closed until `?` opens it.
pub struct ShortcutsOverlay {
    dialog: ModalDialog<Column>,
    dismissed: Rc<Cell<bool>>,
    visible: bool,
    viewport: Rect,
    /// The height the reference was last built to scroll in, so a resize
    /// rebuilds it only when that changed.
    built_for: Option<i64>,
}

impl ShortcutsOverlay {
    pub fn new() -> Self {
        let dismissed = Rc::new(Cell::new(false));
        Self {
            dialog: Self::dialog(&dismissed, None),
            dismissed,
            visible: false,
            viewport: Rect::ZERO,
            built_for: None,
        }
    }

    fn dialog(dismissed: &Rc<Cell<bool>>, height: Option<f64>) -> ModalDialog<Column> {
        let dismissed = Rc::clone(dismissed);
        ModalDialog::new(
            "Keyboard shortcuts",
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(sheet_body(height)),
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

    /// The window the sheet is shown in: how much of the reference scrolls
    /// follows its height.
    pub fn set_viewport(&mut self, viewport: Rect, theme: &Theme) {
        self.viewport = viewport;
        if viewport.width() <= 0.0 {
            return;
        }
        let height = body_height(viewport, theme);
        let key = Some(ui::num::i64_of_f64(height.round()));
        if key != self.built_for {
            self.built_for = key;
            self.dialog = Self::dialog(&self.dismissed, Some(height));
            self.dialog.set_open(self.visible);
        }
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
        assert_eq!(spell("Alt-Up"), "Alt-Up");
        assert_eq!(spell("[["), "[[");
        for entry in GROUPS {
            for (_, keys) in entry.rows {
                let spelled = spell(keys);
                assert!(!spelled.contains("{cmd"), "unspelled keys: {spelled}");
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

    #[test]
    fn the_reference_scrolls_inside_what_the_window_leaves() {
        let theme = Theme::dark();
        let short = body_height(Rect::new(0.0, 0.0, 800.0, 400.0), &theme);
        let tall = body_height(Rect::new(0.0, 0.0, 800.0, 900.0), &theme);
        assert!(short < 400.0, "the body is inside the window: {short}");
        assert!(
            (tall - short - 500.0).abs() < 1e-9,
            "every extra pixel of window is a pixel of body"
        );
        let squeezed = body_height(Rect::new(0.0, 0.0, 800.0, 20.0), &theme);
        assert!(squeezed > 0.0, "a body never collapses to nothing");
    }

    #[test]
    fn a_resize_rebuilds_the_sheet_only_when_its_scroll_height_changes() {
        let theme = Theme::dark();
        let mut overlay = ShortcutsOverlay::new();
        overlay.set_visible(true);
        overlay.set_viewport(Rect::new(0.0, 0.0, 800.0, 600.0), &theme);
        let built = overlay.built_for;
        assert!(built.is_some());
        overlay.set_viewport(Rect::new(0.0, 0.0, 1000.0, 600.0), &theme);
        assert_eq!(overlay.built_for, built, "a wider window scrolls the same");
        overlay.set_viewport(Rect::new(0.0, 0.0, 800.0, 700.0), &theme);
        assert_ne!(overlay.built_for, built, "a taller window scrolls less");
        assert!(overlay.is_visible(), "a resize does not close the sheet");
    }
}
