//! The theme SDK: how a theme draws every part of every `ui` component.
//!
//! A theme at the full level is a **painter set**: a [`PartPainter`]. The
//! toolkit asks it to draw one [`Part`] at a time, a pure function of
//! `(part, size, state, progress, tokens)`, and gets back a [`PaintedPart`]:
//! a scene of kurbo paths and peniko-style brushes ([`PartScene`]) and the
//! shape that takes the pointer. The toolkit owns everything else:
//!
//! * **The clock.** A painter never reads time. When a part's
//!   [`States`] change, the toolkit asks the painter how the change moves
//!   ([`PartPainter::transition`]), runs that [`Transition`] on its own
//!   clock, and asks for the moment it reached (`progress`, 0 to 1). Reduce
//!   motion lands every transition at once.
//! * **The cache.** A painted part is kept by part, size, state and tokens;
//!   only parts that are moving are painted again.
//! * **Hit testing.** The painted shape ([`PaintedPart::hit`]) is where the
//!   part takes a click; focus and accessibility bounds follow it.
//! * **The backdrop.** A [`Brush::Backdrop`] is filled with what lies below
//!   the part, which only the toolkit's backdrop pass knows.
//!
//! The built-in painter sets (Default, Glass, Block) and a third-party set
//! loaded from wasm implement this one trait; a wasm module exports it
//! through [`export_painter!`] and the [`wire`] encoding.
//! `docs/architecture/theming.md`, "Part painters".

mod motion;
mod part;
mod scene;
mod tokens;
pub mod wire;

#[doc(hidden)]
pub mod guest;

pub use kurbo;
pub use peniko;

pub use motion::{Easing, Transition};
pub use part::{Part, PartState, States};
pub use scene::{Brush, Lens, Op, PaintedPart, PartImage, PartScene, Stop};
pub use tokens::{Param, PartTokens};

use kurbo::Size;

/// A painter set: how a theme draws every part.
///
/// Every method is a pure function of its arguments: no clock, no I/O, no
/// state kept between calls. The toolkit caches what it returns and calls
/// it again only when an argument changes.
pub trait PartPainter: Send + Sync {
    /// Draw `part` at `size` (logical pixels, origin at its top left), in
    /// `state`, `progress` of the way through the transition from
    /// `state.was` to `state.now` (1 when settled), with `tokens`.
    ///
    /// `None` leaves the part to the built-in Default set, so a theme draws
    /// only the parts it changes.
    fn paint(
        &self,
        part: Part,
        size: Size,
        state: PartState,
        progress: f32,
        tokens: &PartTokens,
    ) -> Option<PaintedPart>;

    /// How `part` moves from `from` to `to`. The toolkit runs it on its
    /// clock; [`Transition::Inherit`] takes the toolkit's own motion.
    fn transition(&self, part: Part, from: States, to: States) -> Transition {
        let _ = (part, from, to);
        Transition::Inherit
    }
}
