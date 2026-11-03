//! What a pass on the desk reports: the revision a host saves and shares
//! against, and the count of what the person sees changing.

use super::{Presentation, Shared};

impl Shared {
    /// A change a host saves and shares: the revision, and a wake for the
    /// host holding this desk.
    pub(super) fn changed(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.wake.wake();
    }

    /// What the person sees changed, or did not.
    pub(super) fn present(&mut self, presentation: Presentation) {
        if self.presentation != presentation {
            self.presentation = presentation;
            self.presented = self.presented.wrapping_add(1);
        }
    }
}
