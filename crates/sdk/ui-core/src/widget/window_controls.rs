//! The window's own controls at the trailing end of its bar: History (when
//! the window has it), maximize/restore and close.
//!
//! They belong to the window, not to a tab: they stand in the same place
//! whether the window shows one tab or several, and act on the active tab.
//! Each is an icon button with no label; its accessible name and tooltip say
//! what it does.

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::icons;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

/// One of the window's controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowControl {
    History,
    Maximize,
    Close,
}

/// Side of each control's square.
pub const CONTROL_PX: f64 = 28.0;
/// Between two neighbouring controls.
pub const CONTROL_GAP: f64 = 4.0;
/// Between the last control and the window's trailing edge.
pub const EDGE_INSET: f64 = 8.0;
/// Between the controls and whatever stands before them in the row.
pub const LEAD_GAP: f64 = 16.0;

/// Where each control sits; `Rect::ZERO` for one the window doesn't offer.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WindowControls {
    pub history: Rect,
    pub maximize: Rect,
    pub close: Rect,
}

/// What the controls show this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ControlsState {
    pub hovered: Option<WindowControl>,
    pub pressed: Option<WindowControl>,
    pub maximized: bool,
    pub history_open: bool,
}

impl WindowControls {
    /// The controls at the trailing end of `row`, centred on its centre line.
    #[must_use]
    pub fn lay_out(row: Rect, history: bool) -> Self {
        if row.width() <= 0.0 || row.height() <= 0.0 {
            return Self::default();
        }
        let y0 = row.center().y - CONTROL_PX / 2.0;
        let square = |x1: f64| Rect::new(x1 - CONTROL_PX, y0, x1, y0 + CONTROL_PX);
        let close = square(row.x1 - EDGE_INSET);
        let maximize = square(close.x0 - CONTROL_GAP);
        let history = if history {
            square(maximize.x0 - CONTROL_GAP)
        } else {
            Rect::ZERO
        };
        Self {
            history,
            maximize,
            close,
        }
    }

    /// Where the controls begin, less the gap anything before them keeps;
    /// the row's trailing edge for none.
    #[must_use]
    pub fn content_limit(&self, row: Rect) -> f64 {
        self.iter()
            .map(|(_, r)| r.x0)
            .fold(None, |min: Option<f64>, x| {
                Some(min.map_or(x, |m| m.min(x)))
            })
            .map_or(row.x1, |x0| x0 - LEAD_GAP)
    }

    /// Every offered control with its rect.
    pub fn iter(&self) -> impl Iterator<Item = (WindowControl, Rect)> {
        [
            (WindowControl::History, self.history),
            (WindowControl::Maximize, self.maximize),
            (WindowControl::Close, self.close),
        ]
        .into_iter()
        .filter(|(_, r)| r.area() > 0.0)
    }

    /// The control under `p`.
    #[must_use]
    pub fn hit(&self, p: Point) -> Option<WindowControl> {
        self.iter().find(|(_, r)| r.contains(p)).map(|(c, _)| c)
    }

    /// Move every control by `shift`.
    pub fn offset(&mut self, shift: vello::kurbo::Vec2) {
        for r in [&mut self.history, &mut self.maximize, &mut self.close] {
            if r.area() > 0.0 {
                *r = *r + shift;
            }
        }
    }

    /// Paint the controls with the shared icon button.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        state: ControlsState,
    ) {
        for (control, rect) in self.iter() {
            let icon = match control {
                WindowControl::History => icons::Icon::History,
                WindowControl::Maximize if state.maximized => icons::Icon::Minimize2,
                WindowControl::Maximize => icons::Icon::Maximize2,
                WindowControl::Close => icons::Icon::X,
            };
            paint_icon_button(
                scene,
                transform,
                text,
                theme,
                (rect, icon),
                ButtonFace {
                    hovered: state.hovered == Some(control),
                    pressed: state.pressed == Some(control),
                    on: control == WindowControl::History && state.history_open,
                    enabled: true,
                },
            );
        }
    }
}

/// How one icon button of the window's bar shows this frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ButtonFace {
    pub hovered: bool,
    pub pressed: bool,
    /// A toggle that is on.
    pub on: bool,
    /// It can act now.
    pub enabled: bool,
}

/// Paint one icon button of the window's bar with the shared button face
/// ([`crate::widget::Button::paint`]): the window's own controls, the
/// app's slots ([`crate::window_toolbar`]) and the new-tab button are the
/// same control, so the bar reads as one row of them.
pub fn paint_icon_button(
    scene: &mut Scene,
    transform: Affine,
    text: &mut TextRenderer,
    theme: &Theme,
    (rect, icon): (Rect, icons::Icon),
    face: ButtonFace,
) {
    let face = crate::widget::ButtonFace::icon_only(rect, icon, "")
        .control_size(crate::widget::ControlSize::Small)
        .hovered(face.hovered && face.enabled)
        .pressed(face.pressed && face.enabled)
        .toggled(face.on)
        .disabled(!face.enabled);
    crate::widget::Button::paint(scene, transform, text, theme, &face);
}

#[cfg(test)]
mod tests {
    use super::*;

    // USER REQUIREMENT (2026-10-19): "an X at the top right and either a
    // maximize or minimize button there" — one toggle, beside close, with
    // History before them; there is no minimize.
    #[test]
    fn close_is_outermost_then_one_maximize_toggle_then_history() {
        let row = Rect::new(0.0, 0.0, 800.0, 44.0);
        let c = WindowControls::lay_out(row, true);
        assert!(c.close.x1 <= row.x1 && c.close.x1 > row.x1 - 16.0);
        assert!(c.maximize.x1 < c.close.x0);
        assert!(c.history.x1 < c.maximize.x0);
        assert_eq!(c.iter().count(), 3);
        let without = WindowControls::lay_out(row, false);
        assert_eq!(without.iter().count(), 2);
        assert_eq!(without.close, c.close);
        assert_eq!(without.maximize, c.maximize);
    }

    #[test]
    fn content_stops_before_the_controls() {
        let row = Rect::new(0.0, 0.0, 800.0, 44.0);
        let c = WindowControls::lay_out(row, false);
        assert!(c.content_limit(row) <= c.maximize.x0 - LEAD_GAP + f64::EPSILON);
        assert_eq!(c.hit(c.close.center()), Some(WindowControl::Close));
        assert_eq!(c.hit(Point::new(10.0, 22.0)), None);
    }
}
