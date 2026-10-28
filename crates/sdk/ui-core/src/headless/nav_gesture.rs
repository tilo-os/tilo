//! The system's back and forward gestures, delivered to the whole tree as
//! the host delivers them once it has recognised one.

use super::FrameHarness;
use crate::gesture::NavDirection;

impl FrameHarness {
    /// The system's back (or forward) gesture, as the host delivers it to
    /// the whole tree once recognised; whether something took it. The
    /// frame after it is laid out.
    pub fn nav_gesture(&mut self, dir: NavDirection) -> bool {
        self.invalidate_local();
        let taken = self
            .root
            .on_nav_gesture(dir, &self.state.doc, &self.state.root);
        self.relayout();
        taken
    }
}
