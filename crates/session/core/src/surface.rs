//! Where a layer of the session's own stands on the desk, on every host.

/// The rectangle a layer is shown on, in the host's logical layout, and the
/// scale it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// Top-left corner, logical pixels.
    pub origin: (i32, i32),
    /// Size, logical pixels.
    pub size: (u32, u32),
    /// Physical pixels per logical pixel.
    pub scale: f64,
}

impl Placement {
    /// The rectangle in physical pixels: each side scaled, and at least one
    /// pixel wide.
    #[must_use]
    pub fn physical_size(self) -> (u32, u32) {
        let scale = |side: u32| {
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "a surface side on a screen, scaled by a small positive factor"
            )]
            let physical = (f64::from(side) * self.scale).round().max(1.0) as u32;
            physical
        };
        (scale(self.size.0), scale(self.size.1))
    }
}
