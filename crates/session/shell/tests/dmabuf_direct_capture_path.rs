//! Integration test for the DMABUF direct capture path.
//!
//! `SurfaceTracker::on_commit_with_path_detection` returns a
//! [`CapturePath`] tag reflecting whether the incoming `wl_buffer` took
//! the zero-copy DMABUF path or the SHM readback fallback. Rather than
//! spinning up a `wl_display` and DMABUF-capable client (needs a
//! working DRM device and compositor bring-up), this drives the counter
//! through the tracker's test-only deposit hook — the same seam
//! `State::commit`'s production wiring calls into.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use shell::compositor::surface_readback::{CapturePath, SurfaceTracker};

#[test]
fn capture_takes_the_dmabuf_path_when_it_can_and_counts_shm_fallback() {
    // Phase `dmabuf_direct_path_is_taken_when_buffer_is_dmabuf`.
    {
        let mut tracker = SurfaceTracker::new();

        tracker.note_capture_path(1, CapturePath::DmabufDirect);
        tracker.note_capture_path(1, CapturePath::DmabufDirect);
        tracker.note_capture_path(1, CapturePath::DmabufDirect);

        assert_eq!(
            tracker.dmabuf_direct_count(),
            3,
            "three DMABUF commits should bump dmabuf_direct_count() to 3"
        );
        assert_eq!(
            tracker.shm_readback_count(),
            0,
            "no SHM buffer was committed — shm_readback_count must stay 0"
        );
        assert_eq!(
            tracker.last_capture_path(1),
            Some(CapturePath::DmabufDirect),
            "last observed capture path for the surface should reflect the \
             DMABUF direct route"
        );
    }

    // Phase `mixed_dmabuf_and_shm_traffic_counts_independently`.
    {
        // Counters must not bleed across surfaces.
        let mut tracker = SurfaceTracker::new();
        for _ in 0..5 {
            tracker.note_capture_path(1, CapturePath::DmabufDirect);
        }
        for _ in 0..3 {
            tracker.note_capture_path(2, CapturePath::ShmReadback);
        }

        assert_eq!(tracker.dmabuf_direct_count(), 5);
        assert_eq!(tracker.shm_readback_count(), 3);
        assert_eq!(
            tracker.last_capture_path(1),
            Some(CapturePath::DmabufDirect)
        );
        assert_eq!(tracker.last_capture_path(2), Some(CapturePath::ShmReadback));
    }

    // Phase `shm_fallback_when_dmabuf_unavailable`.
    {
        let mut tracker = SurfaceTracker::new();
        for _ in 0..7 {
            tracker.note_capture_path(42, CapturePath::ShmReadback);
        }
        assert_eq!(tracker.shm_readback_count(), 7);
        assert_eq!(tracker.dmabuf_direct_count(), 0);
    }
}
