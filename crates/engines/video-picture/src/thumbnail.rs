//! How the frame source gives up on a source that never answers.

/// One bad seek near a damaged GOP must not condemn a whole clip.
pub const FAILURES_BEFORE_GIVING_UP: u32 = 3;
