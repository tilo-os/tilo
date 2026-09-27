//! Focused widget IME intent shared by native windows and browser input bridges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImeWindowState {
    pub purpose: winit::window::ImePurpose,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl ImeWindowState {
    pub fn from_widget(widget: &dyn crate::Widget) -> Option<Self> {
        let purpose = widget.ime_purpose()?;
        let rect = widget.ime_cursor_rect()?;
        let bounded = |v: f64| {
            if v.is_finite() {
                #[allow(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "value is clamped to [0.0, u32::MAX] on the line above, so the cast is exact"
                )]
                {
                    v.max(0.0).min(f64::from(u32::MAX)).round() as u32
                }
            } else {
                0
            }
        };
        Some(ImeWindowState {
            purpose,
            x: bounded(rect.x0),
            y: bounded(rect.y0),
            width: bounded(rect.width()).max(1),
            height: bounded(rect.height()).max(1),
        })
    }
}
