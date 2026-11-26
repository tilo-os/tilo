//! The Control surface a list row, tile or card stands on (`docs/architecture/ui-inventory.md`,
//! "Quick Settings list language").

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect};
use vello::peniko::{Color, Fill};

use crate::widget::paint_focus_ring;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Tone;
use tilo_ui_base::{chrome::ChromeRole, chrome::SelectionState, chrome::selection_floor_ramp};

/// A Control's fill: the shared hover tier when hovered, otherwise a quiet floor on the same
/// ramp, so a row reads as a surface rather than bare text on the panel.
#[must_use]
pub fn control_fill(theme: &Theme, hovered: bool) -> Color {
    selection_floor_ramp(
        theme,
        SelectionState::new(false, hovered, false),
        theme.tokens.colors.surfaces.base,
    )
}

#[must_use]
pub fn control_radius(theme: &Theme, rect: Rect) -> f64 {
    ChromeRole::Control.radius(theme, rect.height())
}

/// Paint one Control: its fill with the derived rest edge at the Control radius, and the
/// toolkit's 2 px focus ring when `focused`.
pub fn paint_control_row(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    rect: Rect,
    hovered: bool,
    focused: bool,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let radius = control_radius(theme, rect);
    tilo_ui_base::surface::card(scene, transform, rect, radius, control_fill(theme, hovered));
    if focused {
        paint_focus_ring(scene, transform, rect, radius, theme);
    }
}

/// The rail a warning or a failure keeps beside its text, so a row's status
/// is scannable without taking another line; plain information carries no
/// colour, so the rail means something wherever it appears.
pub fn paint_severity_rail(
    scene: &mut Scene,
    transform: Affine,
    theme: &Theme,
    row: Rect,
    tone: Tone,
    inset: f64,
) {
    if tone == Tone::Neutral || row.height() <= 2.0 * inset {
        return;
    }
    let rail = Rect::new(row.x0, row.y0 + inset, row.x0 + 3.0, row.y1 - inset);
    scene.fill(
        Fill::NonZero,
        transform,
        tone.colour(theme),
        None,
        &RoundedRect::from_rect(rail, rail.width() / 2.0),
    );
}
