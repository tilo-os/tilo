//! How a part moves between states. The painter declares it; the toolkit
//! runs it on its own clock, with the curve math in its motion library
//! (`ui::motion::transition`), so a theme names a motion rather than writing
//! one.

/// A curve for [`Transition::Eased`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Easing {
    Linear,
    /// Fast out, slow in: the stock ease for a change that answers input.
    EaseOut,
    EaseInOut,
}

/// How a part moves from one set of states to another.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Transition {
    /// The toolkit's own motion for this change.
    Inherit,
    /// The end state at once.
    None,
    /// A spring from 0 to 1: `response` seconds per oscillation, `damping`
    /// the damping ratio (1 critically damped, below 1 it overshoots).
    Spring { response: f32, damping: f32 },
    /// `steps` equal jumps over `duration` seconds: frame-by-frame, pixel
    /// motion.
    Stepped { steps: u16, duration: f32 },
    /// A curve over `duration` seconds.
    Eased { duration: f32, easing: Easing },
}
