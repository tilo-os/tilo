//! The Alt+Tab window switcher as a stacked layer.

use std::time::Instant;

use tilo_desktop::window_switcher::{SwitcherOutcome, WindowSwitcher};
use ui::a11y::AccessNode;
use vello::kurbo::Rect;
use winit::event::WindowEvent;

use super::layers::{
    Anchor, LayerCx, LayerGeometry, LayerId, LayerIntent, LayerPaint, Material, Modality, Role,
    Routed, ShellLayer, Surface,
};

/// What the host must do after the switcher settled on `outcome`; `None`
/// when the switcher ignored the input.
pub(super) fn outcome_intent(outcome: SwitcherOutcome) -> Option<LayerIntent> {
    match outcome {
        SwitcherOutcome::Ignored => None,
        SwitcherOutcome::Consumed | SwitcherOutcome::Cancelled => Some(LayerIntent::Repaint),
        SwitcherOutcome::Commit(address) => Some(LayerIntent::FocusWindow(address)),
    }
}

/// Open while Alt is held after Alt+Tab. Keys all belong to it; the pointer
/// does once the panel is on screen.
impl ShellLayer for WindowSwitcher {
    fn id(&self) -> LayerId {
        LayerId::WindowSwitcher
    }

    /// A mode the held modifier keeps open over every display at once:
    /// window-modal, drawn on the floating material so the windows it
    /// chooses between stay visible.
    fn surface(&self) -> Option<Surface> {
        Some(Surface::shell(
            Role::Overlay,
            Anchor::Fill,
            Modality::WindowModal,
            Material::Floating,
            self.is_active(),
        ))
    }

    fn is_open(&self) -> bool {
        self.is_active()
    }

    /// Alt+Tab tapped and released quickly switches without the panel ever
    /// appearing, so it is announced only once revealed.
    fn is_shown(&self) -> bool {
        self.is_revealed(Instant::now())
    }

    fn handle_event(&mut self, event: &WindowEvent, cx: &mut LayerCx<'_, '_>) -> Routed {
        let outcome =
            self.route_event(event, cx.event.modifiers.state(), cx.cursor, Instant::now());
        let Some(intent) = outcome_intent(outcome) else {
            return Routed::Pass;
        };
        cx.request(intent);
        // A modifier change settles the switcher but still reaches the rest
        // of the shell.
        if matches!(event, WindowEvent::ModifiersChanged(_)) {
            Routed::Pass
        } else {
            Routed::Consumed
        }
    }

    fn draw(&mut self, cx: &mut LayerPaint<'_>) {
        Self::draw(
            self,
            cx.scene,
            cx.transform,
            cx.text_renderer,
            cx.theme,
            cx.geometry.display_for(None),
            Instant::now(),
            cx.thumbnails,
        );
    }

    fn accessibility(&self, _geometry: LayerGeometry<'_>) -> Option<AccessNode> {
        Self::accessibility(self)
    }

    fn accessibility_rect(&self, _geometry: LayerGeometry<'_>, path: &[usize]) -> Option<Rect> {
        match path {
            [] | [0] => Some(self.panel_rect()),
            [0, index] => self.card_rect(*index),
            _ => None,
        }
    }

    fn focus_accessibility(&mut self, path: &[usize]) -> bool {
        match path {
            [0, index] => self.focus_index(*index),
            _ => false,
        }
    }

    fn dismiss(&mut self) -> bool {
        Self::dismiss(self)
    }

    fn is_animating(&self, now: Instant) -> bool {
        self.needs_frame(now)
    }
}
