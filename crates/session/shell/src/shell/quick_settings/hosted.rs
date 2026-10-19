//! Rows the overlay host describes and the root page paints: the Devices
//! section's device fabric rows and the Clipboard section's copies.
//!
//! The host holds the device fabric and the clipboard history, so it builds
//! each row's face ([`HostedRow`]) when the section opens and while it shows;
//! the root page lays the rows out and paints them inside the section, on
//! its disclosure motion. Where a row's buttons sit is one set of functions
//! of the row's rect, read by the painting here and by the host's
//! hit-testing, so the two cannot drift apart.

use vello::kurbo::Rect;

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::primitives::IconButton;
use ui::widget::{Button, ButtonFace, ButtonRole, Interaction, ListRowFace, RowLeading};

/// What a row offers besides a press on its body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostedActions {
    None,
    /// The row's one action, named on a button (stop, cancel, try again).
    Select(String),
    /// Two choices: allow and decline, or move down and up.
    Pair {
        first: (Icon, String),
        second: (Icon, String),
    },
    /// A display's placement beside this one (left, right, above, below)
    /// and taking its pointer.
    Placement {
        names: [String; 4],
        control: String,
        /// This machine's pointer is driving it now.
        controlling: bool,
        /// It can be placed: online, with a display, not this machine.
        placeable: bool,
        /// It can be driven: online and not this machine.
        controllable: bool,
    },
    /// A copy's pin and remove.
    Clipboard {
        pinned: bool,
        pin: String,
        remove: String,
    },
}

/// One row's face.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedRow {
    pub title: String,
    pub detail: String,
    pub icon: Icon,
    /// The row is the chosen one (the media source the volume follows).
    pub selected: bool,
    pub enabled: bool,
    pub actions: HostedActions,
}

impl HostedRow {
    /// What a screen reader says for the row.
    #[must_use]
    pub fn spoken(&self) -> String {
        match &self.actions {
            HostedActions::Select(action) => crate::shell::i18n::t_args(
                "qs-hosted-row-action-a11y",
                &[
                    ("title", self.title.as_str().into()),
                    ("detail", self.detail.as_str().into()),
                    ("action", action.as_str().into()),
                ],
            ),
            _ => crate::shell::i18n::t_args(
                "qs-hosted-row-a11y",
                &[
                    ("title", self.title.as_str().into()),
                    ("detail", self.detail.as_str().into()),
                ],
            ),
        }
    }
}

/// The side of a row's square icon buttons.
fn button_side(row: Rect) -> f64 {
    (row.height() - 2.0 * GAP).clamp(0.0, 32.0)
}

const GAP: f64 = 4.0;
/// A named action button's width.
const SELECT_W: f64 = 84.0;
/// A small placement arrow.
const ARROW: f64 = 22.0;

/// A row's named action button, at its trailing end.
#[must_use]
pub fn select_rect(row: Rect) -> Rect {
    let h = button_side(row).min(28.0);
    Rect::new(
        row.x1 - GAP - SELECT_W,
        row.center().y - h / 2.0,
        row.x1 - GAP,
        row.center().y + h / 2.0,
    )
}

/// A row's two trailing choices, the first before the second.
#[must_use]
pub fn pair_rects(row: Rect) -> (Rect, Rect) {
    let side = button_side(row);
    let y0 = row.center().y - side / 2.0;
    let second = Rect::new(row.x1 - GAP - side, y0, row.x1 - GAP, y0 + side);
    let first = Rect::new(second.x0 - GAP - side, y0, second.x0 - GAP, y0 + side);
    (first, second)
}

/// A display's four placement arrows (left, right, above, below) in a
/// two-by-two block, and its control button after them.
#[must_use]
pub fn placement_rects(row: Rect) -> ([Rect; 4], Rect) {
    let side = button_side(row);
    let control = Rect::new(
        row.x1 - GAP - side,
        row.center().y - side / 2.0,
        row.x1 - GAP,
        row.center().y + side / 2.0,
    );
    let right = control.x0 - GAP;
    let left = right - 2.0 * ARROW - 2.0;
    let top = row.center().y - ARROW - 1.0;
    let bottom = row.center().y + 1.0;
    let cell = |x: f64, y: f64| Rect::new(x, y, x + ARROW, y + ARROW);
    (
        [
            cell(left, top),
            cell(left + ARROW + 2.0, top),
            cell(left, bottom),
            cell(left + ARROW + 2.0, bottom),
        ],
        control,
    )
}

/// How much of a row's trailing end its buttons take.
fn trailing_width(row: Rect, actions: &HostedActions) -> f64 {
    let side = button_side(row);
    match actions {
        HostedActions::None => 0.0,
        HostedActions::Select(_) => SELECT_W + GAP,
        HostedActions::Pair { .. } | HostedActions::Clipboard { .. } => 2.0 * (side + GAP),
        HostedActions::Placement { .. } => side + 2.0 * GAP + 2.0 * ARROW + 2.0,
    }
}

/// Paint `row` at `rect`. `cursor` is in the same space as `rect`.
pub fn paint(
    ctx: &mut PaintCtx<'_>,
    rect: Rect,
    row: &HostedRow,
    cursor: Option<vello::kurbo::Point>,
    focused: bool,
) {
    let over = |r: Rect| cursor.is_some_and(|p| r.contains(p));
    ListRowFace::new(&row.title)
        .secondary(&row.detail)
        .leading(RowLeading::Icon(row.icon))
        .selected(row.selected)
        .trailing_width(trailing_width(rect, &row.actions))
        .interaction(Interaction {
            hovered: row.enabled && over(rect),
            focused,
            disabled: !row.enabled,
            ..Interaction::default()
        })
        .paint(ctx, rect);
    let icon_button = |ctx: &mut PaintCtx<'_>, r: Rect, icon: Icon, name: &str, enabled: bool| {
        Button::paint(
            ctx.scene,
            ctx.transform,
            ctx.text,
            ctx.theme,
            &IconButton::face(r, icon, name)
                .hovered(enabled && over(r))
                .disabled(!enabled),
        );
    };
    match &row.actions {
        HostedActions::None => {}
        HostedActions::Select(label) => {
            let r = select_rect(rect);
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::new(r, label)
                    .hovered(row.enabled && over(r))
                    .disabled(!row.enabled),
            );
        }
        HostedActions::Pair { first, second } => {
            let (a, b) = pair_rects(rect);
            icon_button(ctx, a, first.0, &first.1, true);
            icon_button(ctx, b, second.0, &second.1, true);
        }
        HostedActions::Placement {
            names,
            control,
            controlling,
            placeable,
            controllable,
        } => {
            let (arrows, control_rect) = placement_rects(rect);
            let icons = [
                Icon::ArrowLeft,
                Icon::ArrowRight,
                Icon::ArrowUp,
                Icon::ArrowDown,
            ];
            for ((r, icon), name) in arrows.into_iter().zip(icons).zip(names) {
                icon_button(ctx, r, icon, name, *placeable);
            }
            let mut face = IconButton::face(control_rect, Icon::Mouse, control)
                .hovered(*controllable && over(control_rect))
                .disabled(!controllable);
            if *controlling {
                face = face.role(ButtonRole::Primary);
            }
            Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
        }
        HostedActions::Clipboard {
            pinned,
            pin,
            remove,
        } => {
            let (a, b) = pair_rects(rect);
            let mut face = IconButton::face(a, Icon::Pin, pin).hovered(over(a));
            if *pinned {
                face = face.role(ButtonRole::Primary);
            }
            Button::paint(ctx.scene, ctx.transform, ctx.text, ctx.theme, &face);
            icon_button(ctx, b, Icon::X, remove, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rows_buttons_stay_inside_it_and_apart() {
        let row = Rect::new(0.0, 0.0, 320.0, 52.0);
        let (a, b) = pair_rects(row);
        assert!(a.x1 <= b.x0 && b.x1 <= row.x1 && a.y0 >= row.y0 && a.y1 <= row.y1);
        let (arrows, control) = placement_rects(row);
        for arrow in arrows {
            assert!(arrow.x1 <= control.x0 && arrow.y0 >= row.y0 && arrow.y1 <= row.y1);
        }
        let select = select_rect(row);
        assert!(select.x0 > row.x0 && select.x1 <= row.x1);
    }
}
