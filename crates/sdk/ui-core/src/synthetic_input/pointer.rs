//! Pointer gestures use the same logical coordinates as widget inspection.
//! The host's input scale converts those coordinates to physical winit input.

use crate::widget::{EventCtx, Widget};
use vello::kurbo::Point;
use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};

/// One hardware-shaped input edge, never a direct action or state mutation.
#[derive(Clone, Copy, Debug)]
pub enum PointerAction {
    MoveTo(Point),
    Press(MouseButton),
    Release(MouseButton),
    Leave,
    LoseFocus,
}

impl PointerAction {
    /// Build input for a host whose EventCtx uses this positive input scale.
    /// A logical point is converted exactly once; button edges carry no position.
    pub fn event(self, scale_factor: f64) -> WindowEvent {
        let device_id = DeviceId::dummy();
        match self {
            Self::MoveTo(point) => WindowEvent::CursorMoved {
                device_id,
                position: winit::dpi::PhysicalPosition::new(
                    point.x * scale_factor,
                    point.y * scale_factor,
                ),
            },
            Self::Press(button) => WindowEvent::MouseInput {
                device_id,
                state: ElementState::Pressed,
                button,
            },
            Self::Release(button) => WindowEvent::MouseInput {
                device_id,
                state: ElementState::Released,
                button,
            },
            Self::Leave => WindowEvent::CursorLeft { device_id },
            Self::LoseFocus => WindowEvent::Focused(false),
        }
    }

    /// Drive the production Widget::handle_event path with the host's own context.
    /// Layout, painting, focus and per-frame harvest remain the host's responsibility.
    pub fn dispatch(self, widget: &mut dyn Widget, ctx: &mut EventCtx<'_>) {
        widget.handle_event(&self.event(ctx.scale_factor), ctx);
    }
}
