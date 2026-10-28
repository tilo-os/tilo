//! Where an app's palette stands in its window: under the window's bar,
//! over the modal scrim that dims the rest of the window.

use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, WindowEvent};

use super::ActionPalette;
use crate::theme::Theme;

/// Where the palette's card stands in `window`, the part of the window it
/// belongs to under the window's bar: its top-left corner and its width.
/// Centred across the window, a little below its top, and never wider than
/// the window, so a palette of an app in a window narrower than the screen
/// stays inside that window. A compact window gives it its whole width
/// less the standard insets, from just under the bar.
#[must_use]
pub fn placement(window: Rect, theme: &Theme) -> (Point, f64) {
    let edge = f64::from(theme.tokens.spacing.s2);
    if window.width() < f64::from(theme.tokens.metrics.compact_breakpoint) {
        return (
            Point::new(window.x0 + edge, window.y0 + edge),
            (window.width() - 2.0 * edge).max(1.0),
        );
    }
    let width = (window.width() * 0.6)
        .clamp(360.0, 640.0)
        .min((window.width() - 2.0 * edge).max(1.0));
    let x0 = window.x0 + ((window.width() - width) / 2.0).max(0.0);
    let y0 = window.y0 + (window.height() * 0.12).max(24.0);
    (Point::new(x0, y0), width)
}

impl ActionPalette {
    /// Stand in `window`, the part of the app's window under its bar: the
    /// card where [`placement`] puts it, the modal scrim over the rest of
    /// `window`, and the bar left clear and live. Returns the card's origin
    /// and width for [`Self::layout`].
    pub fn place_in(&mut self, window: Rect, theme: &Theme) -> (Point, f64) {
        self.scrim = Some(window);
        placement(window, theme)
    }

    /// The scrim the palette dims its window with, while it is shown.
    #[must_use]
    pub fn scrim_rect(&self) -> Option<Rect> {
        self.scrim.filter(|_| self.is_presented())
    }

    /// The window a confirmation's sheet stands in and dims.
    pub(super) fn sheet_window(&self, presence: &crate::presence::PresenceCtx) -> Rect {
        self.scrim
            .or_else(|| presence.viewport.get())
            .unwrap_or(self.rect)
    }

    /// Dim the window behind the card as it arrives, and at once where
    /// motion is reduced, as the arrival itself lands.
    pub(super) fn paint_scrim(
        &self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        theme: &Theme,
    ) {
        if let Some(scrim) = self.scrim {
            crate::overlay::scrim::modal(
                scene,
                transform,
                scrim,
                crate::num::f32_of_f64(self.appear.progress()),
                theme,
            );
        }
    }

    /// A press on the scrim, outside the card, dismisses the palette, as
    /// Escape does; returns whether it did.
    pub(super) fn dismissed_on_scrim(&mut self, event: &WindowEvent, scale: f64) -> bool {
        if self.pressed_on_scrim(event, scale) {
            self.close();
            return true;
        }
        false
    }

    /// Whether `event` is a press on the scrim, outside the card.
    fn pressed_on_scrim(&mut self, event: &WindowEvent, scale: f64) -> bool {
        let Some(scrim) = self.scrim else {
            return false;
        };
        let scale = if scale > 0.0 { scale } else { 1.0 };
        let at = match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.scrim_pointer = Point::new(position.x / scale, position.y / scale);
                return false;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            } => self.scrim_pointer,
            WindowEvent::Touch(touch) if touch.phase == winit::event::TouchPhase::Started => {
                Point::new(touch.location.x / scale, touch.location.y / scale)
            }
            _ => return false,
        };
        scrim.contains(at) && !self.rect.contains(at)
    }
}
