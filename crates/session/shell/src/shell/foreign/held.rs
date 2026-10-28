//! Every window's content held still while a screenshot is chosen.
//!
//! When the screenshot tool opens, each window's current frame (its buffer
//! and every subsurface's) is held: tiles, captures and the screen draw from
//! the held frames until the tool closes or the capture completes. Holding
//! takes references to the frames, never copies of their pixels: a GPU frame
//! stays alive (and its client buffer with it), a CPU frame is shared. The
//! clients go on committing; their newer frames become the live ones and
//! show once the hold is released.
//!
//! The windows themselves still move: the strip scrolls and focus moves as
//! usual, which only moves frozen contents around.

use std::collections::HashMap;
use std::sync::Arc;

use super::{ChildFrame, ForeignEntry, ForeignFrameStore, SurfaceId};
use tilo_lock::MutexRecover as _;

/// The frames held when the screenshot opened, and how the subsurfaces lay
/// on their windows then.
#[derive(Default)]
pub(super) struct HeldFrames {
    entries: HashMap<SurfaceId, ForeignEntry>,
    children: HashMap<SurfaceId, Vec<ChildFrame>>,
}

impl ForeignEntry {
    /// What draws this surface's current frame, by reference: the pixels,
    /// the GPU frame, the HDR plane and how they lie on the surface.
    fn held_copy(&self) -> Self {
        let mut held = Self::new();
        held.adopted = self.adopted;
        held.rgba = Arc::clone(&self.rgba);
        held.width = self.width;
        held.height = self.height;
        held.seq = self.seq;
        held.geometry = self.geometry;
        held.scale = self.scale;
        held.hdr.clone_from(&self.hdr);
        held.gpu.clone_from(&self.gpu);
        held.mapped = self.mapped;
        held.presented = self.presented;
        held
    }
}

impl ForeignFrameStore {
    /// Hold every window's current frame, and those of the subsurfaces and
    /// popups drawn on it, until [`Self::release_frames`].
    /// Returns whether a hold began; one already in place is kept, so the
    /// frames are those of the moment the hold first began.
    pub fn hold_frames(&self) -> bool {
        let mut held = self.held.lock_recover();
        if held.is_some() {
            return false;
        }
        // Every window the compositor adopted, with the subsurfaces and
        // popups drawn on it.
        let all_children = self.children.lock_recover().clone();
        let guard = self.entries.lock_recover();
        let windows: Vec<SurfaceId> = guard
            .iter()
            .filter(|(_, entry)| entry.adopted)
            .map(|(id, _)| *id)
            .collect();
        let mut entries = HashMap::new();
        let mut children = HashMap::new();
        for window in windows {
            let drawn_on = all_children.get(&window).cloned().unwrap_or_default();
            for id in std::iter::once(window).chain(drawn_on.iter().map(|child| child.surface)) {
                if let Some(entry) = guard.get(&id).filter(|entry| entry.seq != 0) {
                    entries.insert(id, entry.held_copy());
                }
            }
            children.insert(window, drawn_on);
        }
        drop(guard);
        *held = Some(HeldFrames { entries, children });
        true
    }

    /// Let go of the held frames: the live ones show again, and the frames
    /// and client buffers only the hold kept go back. Returns whether a hold
    /// was released.
    pub fn release_frames(&self) -> bool {
        let released = self.held.lock_recover().take();
        // Dropped outside the lock: the last reference to a GPU frame
        // releases its texture and its client's buffer.
        released.is_some()
    }

    /// A surface destroyed while held: its window has left the screen, so
    /// its held frame goes back now rather than with the hold.
    pub(super) fn forget_held(&self, id: SurfaceId) {
        let forgotten = self.held.lock_recover().as_mut().and_then(|held| {
            held.children.remove(&id);
            for children in held.children.values_mut() {
                children.retain(|child| child.surface != id);
            }
            held.entries.remove(&id)
        });
        // Dropped outside the lock, as on release.
        drop(forgotten);
    }

    /// Whether the windows' frames are held.
    pub fn frames_held(&self) -> bool {
        self.held.lock_recover().is_some()
    }

    /// Run `f` over the frames `id` is drawn from: the held ones while a hold
    /// covers it, else the live ones, with `id`'s subsurfaces as they lay.
    pub(super) fn with_frames<R>(
        &self,
        id: SurfaceId,
        f: impl FnOnce(&HashMap<SurfaceId, ForeignEntry>, &[ChildFrame]) -> R,
    ) -> R {
        {
            let held = self.held.lock_recover();
            if let Some(held) = held.as_ref()
                && held.entries.contains_key(&id)
            {
                let children = held.children.get(&id).map_or(&[][..], Vec::as_slice);
                return f(&held.entries, children);
            }
        }
        let children = self.children_of(id);
        let guard = self.entries.lock_recover();
        f(&guard, &children)
    }
}
