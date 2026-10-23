//! Actual per-surface streaming staging and capture-path accounting.
#[cfg(target_os = "linux")]
use crate::compositor::surface_readback::{ReadbackError, buffer_to_rgba};
use crate::network::protocol::SurfaceId;
#[cfg(target_os = "linux")]
use smithay::backend::allocator::dmabuf::Dmabuf;
#[cfg(target_os = "linux")]
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use std::collections::HashMap;
/// Which capture path a given `on_commit` took for the last-committed buffer of a surface.
///
/// The DMABUF direct path avoids CPU-side readback + swizzle (~4 ms per 720p frame) by handing
/// the fd straight to the encoder; SHM readback is the fallback. Kept in sync with the mirror enum in `diagnostics.rs` via [`CapturePath::to_metrics_path`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum CapturePath {
    /// SHM pool readback + CPU-side swizzle.
    ShmReadback,
    /// DMABUF fd handed directly to the encoder — zero-copy.
    DmabufDirect,
}

impl CapturePath {
    /// Convert to the diagnostics-side enum for [`crate::diagnostics::PeerSurfaceMetrics::set_capture_path`].
    pub fn to_metrics_path(self) -> crate::diagnostics::CapturePath {
        match self {
            CapturePath::ShmReadback => crate::diagnostics::CapturePath::ShmReadback,
            CapturePath::DmabufDirect => crate::diagnostics::CapturePath::DmabufDirect,
        }
    }
}

/// Owner-side per-surface buffer staging.
///
/// One instance per `State`. The commit hook calls [`Self::on_commit`] for every committed
/// foreign surface registered in [`crate::shell::state::Workspace::shared_surfaces`]; the
/// streaming task drains via [`Self::take_latest`] per encode tick.
///
/// The map reuses one `Vec<u8>` per surface — `take_latest` swaps in a fresh empty `Vec`, so
/// steady state is one allocation per (commit, take) cycle, the minimum without aliasing into
/// smithay's mmap. [`Self::note_capture_path`] records SHM vs. DMABUF per commit.
#[derive(Default)]
pub struct SurfaceTracker {
    latest: HashMap<SurfaceId, SurfaceBuffer>,
    /// Per-surface most-recently attached [`Dmabuf`], populated by the commit hook on a DMABUF-backed `wl_buffer`, for the encoder's next drain.
    ///
    /// `Dmabuf` is `Arc`-backed in smithay, so the map entry is a cheap clone keeping every plane's `OwnedFd` alive; [`Self::take_latest_dmabuf`] hands ownership to the encoder without extra dupes.
    #[cfg(target_os = "linux")]
    latest_dmabufs: HashMap<SurfaceId, Dmabuf>,
    /// Per-surface last-observed capture path; `None` before the first commit.
    last_capture_path: HashMap<SurfaceId, CapturePath>,
    /// Counter for the direct DMABUF path, incremented once per commit routed straight to the encoder.
    dmabuf_direct_count: u64,
    /// Counter for the SHM readback fallback path.
    shm_readback_count: u64,
}

/// Per-surface staged frame.
struct SurfaceBuffer {
    rgba: Vec<u8>,
    width: u32,
    height: u32,
}

impl SurfaceTracker {
    /// Construct an empty tracker. Equivalent to [`Default::default`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Pull the latest `wl_buffer` for `surface_id` and stage its RGBA bytes; idempotent across re-commits with the same dimensions. On error the slot is left empty — the caller decides whether to log+drop or retry.
    ///
    /// # Errors
    /// Propagates [`buffer_to_rgba`]'s errors — see its `# Errors` section.
    #[tracing::instrument(level = "trace", skip(self, buffer), fields(surface_id))]
    #[cfg(target_os = "linux")]
    pub fn on_commit(
        &mut self,
        surface_id: SurfaceId,
        buffer: &WlBuffer,
    ) -> Result<(u32, u32), ReadbackError> {
        let entry = self
            .latest
            .entry(surface_id)
            .or_insert_with(SurfaceBuffer::empty);
        match buffer_to_rgba(buffer, &mut entry.rgba) {
            Ok((w, h)) => {
                entry.width = w;
                entry.height = h;
                Ok((w, h))
            }
            Err(err) => {
                entry.rgba.clear();
                entry.width = 0;
                entry.height = 0;
                Err(err)
            }
        }
    }

    /// Drain the most recently staged RGBA for `surface_id`, replacing the slot with a fresh empty vec. `None` when no frame has been staged, or the slot was last touched by a failed `on_commit`.
    pub fn take_latest(&mut self, surface_id: SurfaceId) -> Option<TakenFrame> {
        let slot = self.latest.get_mut(&surface_id)?;
        if slot.rgba.is_empty() {
            return None;
        }
        let rgba = std::mem::take(&mut slot.rgba);
        let width = slot.width;
        let height = slot.height;
        slot.width = 0;
        slot.height = 0;
        Some(TakenFrame {
            rgba,
            width,
            height,
        })
    }

    /// Non-draining borrow of the most-recent staged RGBA; the slot stays populated for a subsequent [`Self::take_latest`]. `None` under the same conditions as [`Self::take_latest`].
    pub fn peek_latest(&self, surface_id: SurfaceId) -> Option<TakenFrame> {
        let slot = self.latest.get(&surface_id)?;
        if slot.rgba.is_empty() {
            return None;
        }
        Some(TakenFrame {
            rgba: slot.rgba.clone(),
            width: slot.width,
            height: slot.height,
        })
    }

    /// Drop the tracking slot for `surface_id` entirely — called when the share is revoked or the surface destroyed.
    pub fn forget(&mut self, surface_id: SurfaceId) {
        self.latest.remove(&surface_id);
        #[cfg(target_os = "linux")]
        self.latest_dmabufs.remove(&surface_id);
        self.last_capture_path.remove(&surface_id);
    }

    /// Stash the DMABUF attached to the surface's latest commit, called after [`smithay::wayland::dmabuf::get_dmabuf`] returns `Ok`; replaces any prior DMABUF for the surface.
    #[cfg(target_os = "linux")]
    pub fn note_dmabuf_committed(&mut self, surface_id: SurfaceId, dmabuf: Dmabuf) {
        self.latest_dmabufs.insert(surface_id, dmabuf);
    }

    /// Drain the most-recently-staged DMABUF for `surface_id`, symmetric with [`Self::take_latest`]. Called right after [`Self::note_dmabuf_committed`] so the encoder can build a `FrameInput::DmabufFd`.
    #[cfg(target_os = "linux")]
    pub fn take_latest_dmabuf(&mut self, surface_id: SurfaceId) -> Option<Dmabuf> {
        #[cfg(target_os = "linux")]
        self.latest_dmabufs.remove(&surface_id)
    }

    /// Record the capture path for the current commit of `surface_id`, called after the commit hook decides DMABUF vs. SHM; bumps the per-path counter the integration test reads.
    pub fn note_capture_path(&mut self, surface_id: SurfaceId, path: CapturePath) {
        self.last_capture_path.insert(surface_id, path);
        match path {
            CapturePath::DmabufDirect => {
                self.dmabuf_direct_count = self.dmabuf_direct_count.saturating_add(1);
            }
            CapturePath::ShmReadback => {
                self.shm_readback_count = self.shm_readback_count.saturating_add(1);
            }
        }
    }

    /// Most-recent capture path observed for `surface_id`; `None` until the first commit.
    pub fn last_capture_path(&self, surface_id: SurfaceId) -> Option<CapturePath> {
        self.last_capture_path.get(&surface_id).copied()
    }

    /// Number of buffers that took the DMABUF-direct path.
    pub fn dmabuf_direct_count(&self) -> u64 {
        self.dmabuf_direct_count
    }

    /// Number of buffers that took the SHM readback path.
    pub fn shm_readback_count(&self) -> u64 {
        self.shm_readback_count
    }

    /// Route an incoming buffer commit through the DMABUF-direct or SHM readback path; the commit
    /// hook calls this instead of [`Self::on_commit`] for automatic path detection.
    ///
    /// On DMABUF, returns the tentative `CapturePath::DmabufDirect` — the caller must pull the
    /// `Dmabuf`, stash it via [`Self::note_dmabuf_committed`], and only call
    /// [`Self::note_capture_path`] once the encoder actually consumes it, so metrics reflect real
    /// zero-copy rather than a rejected attempt. Also returns the SHM readback result (`Err` on the DMABUF branch).
    #[cfg(target_os = "linux")]
    pub fn on_commit_with_path_detection(
        &mut self,
        surface_id: SurfaceId,
        buffer: &WlBuffer,
    ) -> (CapturePath, Result<(u32, u32), ReadbackError>) {
        // DMABUF surfaces as `BufferAccessError::NotManaged` from smithay, mapped to `DmaBufUnsupported` — our signal to hand the DMABUF branch to the caller.
        let result = self.on_commit(surface_id, buffer);
        let path = match &result {
            Err(ReadbackError::DmaBufUnsupported) => CapturePath::DmabufDirect,
            _ => CapturePath::ShmReadback,
        };
        // SHM readbacks bump the counter here (path taken end-to-end); DMABUF-direct's is bumped by the caller only once the encoder accepts the fd, so a rejected DMABUF frame isn't double-counted.
        if matches!(path, CapturePath::ShmReadback) {
            self.note_capture_path(surface_id, path);
        }
        (path, result)
    }

    /// Test-only: deposit a synthetic capture-path observation without touching the `wl_buffer` plumbing. Used by the dmabuf-direct integration test.
    #[cfg(test)]
    pub fn note_capture_path_for_test(&mut self, surface_id: SurfaceId, path: CapturePath) {
        self.note_capture_path(surface_id, path);
    }

    /// Identical to [`Self::forget`]; named separately so the wire-up from
    /// `crate::compositor::state::State::toplevel_destroyed` reads as "drop this surface's tracker buffer entirely" rather than the ambiguous "forget".
    ///
    /// Without this call, a client destroying its toplevel without a prior `attach NULL` leaves its
    /// `width*height*4`-byte `Vec<u8>` slot allocated forever — a per-surface leak that compounds across long sessions.
    pub fn drop_surface(&mut self, surface_id: SurfaceId) {
        self.latest.remove(&surface_id);
        #[cfg(target_os = "linux")]
        self.latest_dmabufs.remove(&surface_id);
        self.last_capture_path.remove(&surface_id);
    }

    /// Whether the tracker currently holds **any** staged frame. Used by the streaming-tick test to assert draining empties the tracker.
    #[cfg(test)]
    pub(crate) fn is_empty_for(&self, surface_id: SurfaceId) -> bool {
        match self.latest.get(&surface_id) {
            None => true,
            Some(slot) => slot.rgba.is_empty(),
        }
    }

    /// Test-only injection path: deposit an already-RGBA frame into the per-surface slot as if
    /// [`Self::on_commit`] had run. Behind `#[cfg(test)]` — no production caller should skip the
    /// `wl_buffer` pull; keeps the egress side ([`Self::take_latest`]) identical to production.
    #[cfg(test)]
    pub(crate) fn deposit_for_test(
        &mut self,
        surface_id: SurfaceId,
        rgba: Vec<u8>,
        width: u32,
        height: u32,
    ) {
        let slot = self
            .latest
            .entry(surface_id)
            .or_insert_with(SurfaceBuffer::empty);
        slot.rgba = rgba;
        slot.width = width;
        slot.height = height;
    }
}

#[cfg(any(test, target_os = "linux"))]
impl SurfaceBuffer {
    fn empty() -> Self {
        Self {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        }
    }
}

/// A drained frame returned from [`SurfaceTracker::take_latest`]. `rgba` is row-major, no padding, `width * height * 4` bytes — suitable for direct hand-off to [`crate::network::VideoEncoder::encode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenFrame {
    /// Row-major, no padding.
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn surface_tracker_slots_hold_the_latest_frame_per_surface() {
        {
            let mut tracker = SurfaceTracker::new();
            assert!(tracker.take_latest(7).is_none());
        }
        {
            let mut tracker = SurfaceTracker::new();
            let rgba = vec![1, 2, 3, 4, 5, 6, 7, 8];
            tracker.deposit_for_test(42, rgba.clone(), 2, 1);
            let frame = tracker.take_latest(42).expect("frame present");
            assert_eq!(frame.rgba, rgba);
            assert_eq!(frame.width, 2);
            assert_eq!(frame.height, 1);
            // Slot is empty after take.
            assert!(tracker.is_empty_for(42));
            assert!(tracker.take_latest(42).is_none());
        }
        {
            let mut tracker = SurfaceTracker::new();
            tracker.deposit_for_test(1, vec![0; 16], 2, 2);
            tracker.deposit_for_test(1, vec![1, 2, 3, 4], 1, 1);
            let frame = tracker.take_latest(1).expect("frame present");
            assert_eq!(frame.rgba, vec![1, 2, 3, 4]);
            assert_eq!(frame.width, 1);
            assert_eq!(frame.height, 1);
        }
        {
            let mut tracker = SurfaceTracker::new();
            tracker.deposit_for_test(9, vec![0xFF; 4], 1, 1);
            tracker.forget(9);
            assert!(tracker.take_latest(9).is_none());
        }
        {
            // Independent surface ids must not bleed into one another.
            let mut tracker = SurfaceTracker::new();
            tracker.deposit_for_test(1, vec![1, 2, 3, 4], 1, 1);
            tracker.deposit_for_test(2, vec![5, 6, 7, 8], 1, 1);
            let f1 = tracker.take_latest(1).expect("surface 1 frame");
            assert_eq!(f1.rgba, vec![1, 2, 3, 4]);
            let f2 = tracker.take_latest(2).expect("surface 2 frame");
            assert_eq!(f2.rgba, vec![5, 6, 7, 8]);
        }
    }
    #[test]
    fn dropping_a_surface_clears_its_buffer_and_capture_path() {
        {
            let mut tracker = SurfaceTracker::new();
            // Deposit a non-trivial buffer so the slot really exists.
            tracker.deposit_for_test(42, vec![0x55; 64], 4, 4);
            assert!(!tracker.is_empty_for(42), "test fixture: slot is populated");

            tracker.drop_surface(42);

            // Slot is gone entirely: take_latest returns None and the internal map no longer carries the entry.
            assert!(
                tracker.take_latest(42).is_none(),
                "drop_surface must remove the slot, not leave it empty-but-present"
            );
            assert!(
                tracker.is_empty_for(42),
                "is_empty_for must report `true` after drop_surface (slot deleted)"
            );
        }
        {
            let mut tracker = SurfaceTracker::new();
            assert_eq!(tracker.dmabuf_direct_count(), 0);
            assert_eq!(tracker.shm_readback_count(), 0);
            tracker.note_capture_path_for_test(1, CapturePath::DmabufDirect);
            tracker.note_capture_path_for_test(1, CapturePath::DmabufDirect);
            tracker.note_capture_path_for_test(2, CapturePath::ShmReadback);
            assert_eq!(tracker.dmabuf_direct_count(), 2);
            assert_eq!(tracker.shm_readback_count(), 1);
            assert_eq!(
                tracker.last_capture_path(1),
                Some(CapturePath::DmabufDirect)
            );
            assert_eq!(tracker.last_capture_path(2), Some(CapturePath::ShmReadback));
            assert_eq!(tracker.last_capture_path(999), None);
        }
        {
            let mut tracker = SurfaceTracker::new();
            tracker.note_capture_path_for_test(7, CapturePath::DmabufDirect);
            assert_eq!(
                tracker.last_capture_path(7),
                Some(CapturePath::DmabufDirect)
            );
            tracker.drop_surface(7);
            assert_eq!(
                tracker.last_capture_path(7),
                None,
                "drop_surface must clear last-capture-path so a re-registered \
                 surface starts with a clean slate"
            );
        }
    }
}
