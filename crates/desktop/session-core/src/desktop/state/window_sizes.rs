//! The shape of each window the compositor shows, as the host names it: a view that pictures a
//! window gives its place the window's own aspect, so the picture fills it and nothing of the
//! ground shows round it.
//!
//! The compositor knows every window's size; the host hands them here each pass
//! ([`DesktopHandle::set_window_sizes`]), and the reading of the windows carries each one's shape.
//! A window with no window here to measure (one on another device, or a placeholder what its app
//! left) is absent, and a view keeps its own shape for it.

use std::collections::BTreeMap;

use super::DesktopHandle;

impl DesktopHandle {
    /// The size of each window the compositor shows, in logical pixels, by its id for it: the host
    /// names them as they stand. Returns whether that changed anything.
    pub fn set_window_sizes(&self, sizes: Vec<(u64, (i32, i32))>) -> bool {
        let mut shared = self.shared.borrow_mut();
        let sizes: BTreeMap<u64, (i32, i32)> = sizes
            .into_iter()
            .filter(|(_, (width, height))| *width > 0 && *height > 0)
            .collect();
        if shared.window_sizes == sizes {
            return false;
        }
        shared.window_sizes = sizes;
        shared.window_sizes_revision = shared.window_sizes_revision.wrapping_add(1);
        shared.wake.wake();
        true
    }

    /// The shape of the window the compositor knows as `window`, in logical pixels, as the host
    /// last said; `None` for a window with no window here to measure.
    #[must_use]
    pub fn window_size(&self, window: u64) -> Option<(i32, i32)> {
        self.shared.borrow().window_sizes.get(&window).copied()
    }

    /// Bumped whenever the windows' shapes change, so a reading made from them is published again.
    #[must_use]
    pub fn window_sizes_revision(&self) -> u64 {
        self.shared.borrow().window_sizes_revision
    }
}
