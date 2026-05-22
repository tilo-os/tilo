//! Foreign-surface buffer readback — Peer-sync Phase 4 v2.
//!
//! Phase 4 v1 ([`crate::network::streaming`] + [`crate::network::encoder`])
//! plumbed the H.264 encode loop end-to-end against a *synthesised* RGBA
//! gradient so the round-trip could be verified before the real
//! foreign-surface path landed. v2 closes that gap: the compositor's
//! `CompositorHandler::commit` hook calls into a [`SurfaceTracker`] which
//! pulls the surface's most-recent `wl_buffer` and stages it as a flat
//! RGBA vector that the streaming task drains per encode tick.
//!
//! ## Two layers
//!
//! 1. **Pure logic.** [`buffer_to_rgba`] takes a `&WlBuffer` and, when it's
//!    a `wl_shm` buffer, calls smithay's [`with_buffer_contents`] to read
//!    the bytes + dimensions + stride + format out of the pool. The byte
//!    swizzle that converts `Argb8888`/`Xrgb8888` (the only two formats
//!    smithay's [`ShmState`] advertises by default) into RGBA is itself
//!    extracted as [`swizzle_to_rgba`] so the unit tests can exercise it
//!    without a wayland connection. The dmabuf path is deferred — Phase 4
//!    v2 logs and returns [`ReadbackError::DmaBufUnsupported`].
//! 2. **State plumbing.** [`SurfaceTracker`] is a small `HashMap` keyed on
//!    [`SurfaceId`] that holds the latest RGBA frame per shared surface.
//!    The compositor's commit handler funnels into
//!    [`SurfaceTracker::on_commit`]; the streaming task drains with
//!    [`SurfaceTracker::take_latest`] which returns the buffer **and
//!    empties the slot** so a follow-up tick without a new commit produces
//!    `None` (the throttle then skips the encode).
//!
//! ## Allocation discipline
//!
//! Per the plan's "no allocation per encode tick" rule, the tracker keeps
//! one `Vec<u8>` per surface and reuses it across commits — re-growing only
//! on a dimension change. `take_latest` `mem::take`s the vec so the caller
//! owns the bytes for the encode call, and a freshly emptied `Vec<u8>` slot
//! takes its place ready for the next commit. The next commit grows that
//! emptied vec back to the buffer size in one allocation; steady-state
//! after the second commit per surface is zero per-tick allocations from
//! the tracker side. The encoder copies its inputs internally, so the
//! caller can hand the vec straight to [`crate::network::H264Encoder::encode`]
//! without an extra clone.
//!
//! ## Safety
//!
//! [`with_buffer_contents`] hands the closure a raw `*const u8` pointing
//! into the client's shm pool, plus the byte length. Smithay's contract is
//! that the pointer is valid for the closure's entire lifetime; the client
//! cannot resize or unmap the pool while the closure runs (the wayland
//! event loop is single-threaded with respect to a given client). We
//! materialise a `&[u8]` from `ptr.add(offset)` for the swizzle pass and
//! drop it before the closure returns. The lone `unsafe` block in this
//! file is the `slice::from_raw_parts` call inside the closure; everything
//! else is safe Rust.

use crate::network::protocol::SurfaceId;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::wayland::shm::{BufferAccessError, with_buffer_contents};
use std::collections::HashMap;

/// Error variants surfaced by [`buffer_to_rgba`] and
/// [`SurfaceTracker::on_commit`].
///
/// `thiserror` isn't a direct dep of this module (the crate pulls it in
/// elsewhere but the surface-readback path stays self-contained), so we
/// hand-roll `Display`/`Error`.
#[derive(Debug)]
pub enum ReadbackError {
    /// The buffer isn't a `wl_shm` buffer — smithay's
    /// [`with_buffer_contents`] returned [`BufferAccessError::NotManaged`].
    /// The dmabuf import path is Phase 4 v3 territory.
    DmaBufUnsupported,
    /// The buffer's shm `Format` isn't one of the two
    /// [`ShmState`](smithay::wayland::shm::ShmState) advertises by default
    /// (`Argb8888`, `Xrgb8888`). Wraps the offending format for logs.
    UnsupportedFormat(wl_shm::Format),
    /// Smithay reported a SIGBUS or other pool-access fault.
    BufferAccess(BufferAccessError),
    /// `data.width * 4 > data.stride` or the pool isn't large enough to
    /// hold the buffer at the declared offset/stride/height. Defensive —
    /// well-behaved clients never trip this, but a hostile client could.
    InvalidDimensions {
        width: i32,
        height: i32,
        stride: i32,
    },
}

impl std::fmt::Display for ReadbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadbackError::DmaBufUnsupported => write!(
                f,
                "buffer is not wl_shm (likely dmabuf) — readback v2 only handles shm"
            ),
            ReadbackError::UnsupportedFormat(fmt) => {
                write!(f, "unsupported wl_shm format: {fmt:?}")
            }
            ReadbackError::BufferAccess(err) => {
                write!(f, "smithay buffer access failed: {err:?}")
            }
            ReadbackError::InvalidDimensions {
                width,
                height,
                stride,
            } => write!(
                f,
                "invalid buffer dimensions: width={width}, height={height}, stride={stride}"
            ),
        }
    }
}

impl std::error::Error for ReadbackError {}

impl From<BufferAccessError> for ReadbackError {
    fn from(err: BufferAccessError) -> Self {
        match err {
            BufferAccessError::NotManaged => ReadbackError::DmaBufUnsupported,
            other => ReadbackError::BufferAccess(other),
        }
    }
}

/// Convert one row of pixel data from a `wl_shm` format into a freshly
/// appended RGBA tail on `out`.
///
/// `src` must be exactly `width * 4` bytes long. Extracted as a free
/// function so the unit tests can exercise every supported format without
/// having to construct a `wl_shm::WlBuffer` (which requires a live
/// wayland-server `Display`).
///
/// Wayland convention: pixel byte order is **little-endian** with the
/// format name reading the high-order byte first. So `Argb8888` on the
/// wire is `[B, G, R, A]`; `Xrgb8888` is `[B, G, R, X]` with `X` forced
/// to `0xFF` on the way out so the H.264 round-trip's
/// alpha-compare assertion stays well-defined.
pub fn swizzle_row_to_rgba(
    format: wl_shm::Format,
    src: &[u8],
    width: usize,
    out: &mut Vec<u8>,
) -> Result<(), ReadbackError> {
    // D-5: the previous guard was a bare `debug_assert_eq!` which
    // compiles out in release. `chunks_exact(4)` then silently
    // swallows trailing bytes, so a hostile / buggy client could
    // sneak padding bytes into the wire — surface that as a
    // structured `InvalidDimensions` error in both debug and release.
    let expected = width.saturating_mul(4);
    if src.len() != expected {
        tracing::error!(
            src_len = src.len(),
            expected,
            "swizzle_row_to_rgba called with mismatched src length; \
             refusing to process the row"
        );
        return Err(ReadbackError::InvalidDimensions {
            width: width as i32,
            // height/stride aren't known at this level — surface the
            // mismatched row length via `stride` so logs are useful.
            height: 1,
            stride: src.len() as i32,
        });
    }
    // Keep the debug-loud-failure mode for callers who pass through
    // the precondition correctly: an internal bug that violates the
    // contract should still panic in development builds.
    debug_assert_eq!(src.len(), expected);
    match format {
        wl_shm::Format::Argb8888 => {
            // [B, G, R, A] -> [R, G, B, A]
            for px in src.chunks_exact(4) {
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
                out.push(px[3]);
            }
            Ok(())
        }
        wl_shm::Format::Xrgb8888 => {
            // [B, G, R, X] -> [R, G, B, 0xFF]
            for px in src.chunks_exact(4) {
                out.push(px[2]);
                out.push(px[1]);
                out.push(px[0]);
                out.push(0xFF);
            }
            Ok(())
        }
        other => Err(ReadbackError::UnsupportedFormat(other)),
    }
}

/// Convert a `wl_shm` buffer into a flat row-major RGBA vector.
///
/// `out` is cleared and re-filled. Reusing the same buffer across commits
/// is the no-allocation steady-state path the [`SurfaceTracker`] relies on.
///
/// # Errors
/// - [`ReadbackError::DmaBufUnsupported`] — buffer isn't shm.
/// - [`ReadbackError::UnsupportedFormat`] — format other than
///   `Argb8888` / `Xrgb8888`.
/// - [`ReadbackError::InvalidDimensions`] — declared dimensions don't
///   produce a sane pixel array (stride < width\*4, negative dims, etc.).
/// - [`ReadbackError::BufferAccess`] — smithay-side SIGBUS / NotManaged
///   for non-shm reasons.
pub fn buffer_to_rgba(
    buffer: &WlBuffer,
    out: &mut Vec<u8>,
) -> Result<(u32, u32), ReadbackError> {
    let result: Result<(u32, u32), ReadbackError> = with_buffer_contents(
        buffer,
        |ptr, len, data| -> Result<(u32, u32), ReadbackError> {
            let width = data.width;
            let height = data.height;
            let stride = data.stride;
            // Reject zero / negative dimensions and strides that don't
            // hold a full row of pixels. Hostile clients could provide a
            // tiny stride to short-read the pool; reject loudly.
            if width <= 0
                || height <= 0
                || stride < width.saturating_mul(4)
            {
                return Err(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                });
            }
            // Also reject a buffer whose declared (offset + stride*height)
            // exceeds the pool's allocated length — that's how a hostile
            // client could trick us into reading past the mmap.
            let offset = data.offset as usize;
            let stride_u = stride as usize;
            let height_u = height as usize;
            let needed = offset
                .checked_add(stride_u.saturating_mul(height_u))
                .ok_or(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                })?;
            if needed > len {
                return Err(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                });
            }

            // SAFETY: smithay guarantees `ptr..ptr+len` is a valid mmap
            // for the duration of this closure. We restrict ourselves to
            // `[offset .. needed]` which we just bounds-checked.
            // `from_raw_parts` does not retain the slice past the
            // closure's lifetime.
            let pool: &[u8] = unsafe { std::slice::from_raw_parts(ptr, len) };
            let buf_bytes = &pool[offset..needed];

            out.clear();
            out.reserve((width as usize) * (height as usize) * 4);
            let width_u = width as usize;
            let row_bytes = width_u * 4;
            for row in 0..height_u {
                let row_start = row * stride_u;
                let src_row = &buf_bytes[row_start..row_start + row_bytes];
                swizzle_row_to_rgba(data.format, src_row, width_u, out)?;
            }
            Ok((width as u32, height as u32))
        },
    )?;
    result
}

/// Owner-side per-surface buffer staging.
///
/// One instance per `State` (see `crate::compositor::state::State`). The
/// compositor's `CompositorHandler::commit` hook calls
/// [`Self::on_commit`] for every committed foreign surface whose
/// `SurfaceId` is registered in [`crate::shell::state::Workspace::shared_surfaces`].
/// The streaming task drains via [`Self::take_latest`] per encode tick.
///
/// ## Allocation discipline
///
/// The internal map stores `Vec<u8>` per surface and reuses each vec
/// across commits. `take_latest` swaps in a fresh empty `Vec` (which
/// allocates only its inline header, not the heap data) and hands the
/// previous one to the caller. The next commit grows the fresh vec; the
/// **previous** vec leaves the tracker on the encode path. Steady state:
/// one alloc per surface per (commit, take) cycle, which is the minimum
/// achievable without unsafely aliasing into smithay's mmap.
///
/// ## Errors
///
/// `on_commit` returns the [`ReadbackError`] verbatim so the caller can
/// decide whether to drop the frame (e.g. on `DmaBufUnsupported` — that's
/// expected, the dmabuf v3 path will eventually handle it) or escalate.
/// Phase 4 v2 logs at `tracing::warn!` in the commit handler.
#[derive(Default)]
pub struct SurfaceTracker {
    latest: HashMap<SurfaceId, SurfaceBuffer>,
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

    /// Pull the latest `wl_buffer` for `surface_id` and stage its RGBA
    /// bytes. Idempotent across re-commits with the same dimensions —
    /// the per-surface `Vec<u8>` is reused.
    ///
    /// On error the slot is left empty (so a subsequent `take_latest`
    /// returns `None`); the caller decides whether to log + drop or
    /// retry.
    pub fn on_commit(
        &mut self,
        surface_id: SurfaceId,
        buffer: &WlBuffer,
    ) -> Result<(u32, u32), ReadbackError> {
        // Pull (or create) the slot's storage vec.
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
                // Leave the slot drained on error so `take_latest`
                // returns `None`. The caller decides whether to forget
                // the slot entirely.
                entry.rgba.clear();
                entry.width = 0;
                entry.height = 0;
                Err(err)
            }
        }
    }

    /// Drain the most recently staged RGBA for `surface_id`, replacing
    /// the slot with a fresh empty vec ready for the next commit.
    ///
    /// Returns `None` when no frame has been staged since the last
    /// `take_latest`, or when the slot was last touched by a failed
    /// `on_commit`.
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

    /// Drop the tracking slot for `surface_id` entirely. Called from the
    /// compositor when the share is revoked or the surface destroyed.
    pub fn forget(&mut self, surface_id: SurfaceId) {
        self.latest.remove(&surface_id);
    }

    /// Whether the tracker currently holds **any** staged frame. Used by
    /// the streaming-tick test to assert that draining empties the
    /// tracker.
    #[cfg(test)]
    pub(crate) fn is_empty_for(&self, surface_id: SurfaceId) -> bool {
        match self.latest.get(&surface_id) {
            None => true,
            Some(slot) => slot.rgba.is_empty(),
        }
    }

    /// Test-only injection path: deposit an already-RGBA frame into the
    /// per-surface slot as if [`Self::on_commit`] had run with a
    /// matching shm buffer.
    ///
    /// Lives behind `#[cfg(test)]` because no production caller should
    /// be skipping the wl_buffer pull. The mock keeps the
    /// production path on the *egress* side intact —
    /// [`Self::take_latest`] is the same function the streaming task
    /// calls — which is the rule Phase 4 v2's spec calls out
    /// ("the mock must use the real production code path through
    /// `SurfaceTracker::take_latest`").
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

impl SurfaceBuffer {
    fn empty() -> Self {
        Self {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        }
    }
}

/// A drained frame returned from [`SurfaceTracker::take_latest`].
///
/// `rgba` is row-major, no padding, `width * height * 4` bytes. Suitable
/// for direct hand-off to [`crate::network::H264Encoder::encode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakenFrame {
    /// Pixel rows, RGBA, row-major, no padding.
    pub rgba: Vec<u8>,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test fixture: deterministic single-row Argb8888 buffer that
    /// exercises the byte swizzle without any client-server plumbing.
    fn argb_row(width: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(width * 4);
        for x in 0..width {
            // Wire bytes [B, G, R, A]: pick distinguishable values per
            // channel so a missed swizzle (e.g. forgetting to swap R/B)
            // shows up immediately.
            out.push((x as u8).wrapping_mul(3)); // B
            out.push((x as u8).wrapping_mul(5)); // G
            out.push((x as u8).wrapping_mul(7)); // R
            out.push(0xC0); // A
        }
        out
    }

    #[test]
    fn swizzle_row_argb_to_rgba_swaps_b_and_r() {
        let src = argb_row(4);
        let mut out = Vec::new();
        swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 4, &mut out)
            .expect("argb swizzle");
        assert_eq!(out.len(), 16);
        // Pixel 0: B=0, G=0, R=0 -> RGBA = 0,0,0,0xC0
        assert_eq!(&out[0..4], &[0, 0, 0, 0xC0]);
        // Pixel 1: B=3, G=5, R=7 -> RGBA = 7,5,3,0xC0
        assert_eq!(&out[4..8], &[7, 5, 3, 0xC0]);
        // Pixel 3: B=9, G=15, R=21 -> RGBA = 21,15,9,0xC0
        assert_eq!(&out[12..16], &[21, 15, 9, 0xC0]);
    }

    #[test]
    fn swizzle_row_xrgb_forces_alpha_to_ff() {
        // Xrgb8888: wire bytes [B, G, R, X]; X may be anything from the
        // client. We always emit alpha = 0xFF.
        let src = vec![10, 20, 30, 0x00, 40, 50, 60, 0x7F];
        let mut out = Vec::new();
        swizzle_row_to_rgba(wl_shm::Format::Xrgb8888, &src, 2, &mut out)
            .expect("xrgb swizzle");
        assert_eq!(out, vec![30, 20, 10, 0xFF, 60, 50, 40, 0xFF]);
    }

    #[test]
    fn swizzle_row_rejects_unsupported_format() {
        // RGB565 isn't advertised by smithay's default ShmState, so a
        // client that somehow attached one should produce a clean
        // error rather than a wrong-looking frame.
        let src = vec![0u8; 4 * 4];
        let mut out = Vec::new();
        let err = swizzle_row_to_rgba(wl_shm::Format::Rgb565, &src, 4, &mut out)
            .unwrap_err();
        match err {
            ReadbackError::UnsupportedFormat(wl_shm::Format::Rgb565) => {}
            other => panic!("expected UnsupportedFormat(Rgb565), got {other:?}"),
        }
    }

    /// D-5 regression: the length-mismatch guard was `debug_assert_eq!`
    /// which compiles out in release builds. `chunks_exact(4)` then
    /// silently swallows trailing bytes — a hostile / buggy client
    /// could ship a row whose stride includes padding and have those
    /// bytes disappear into the swizzle. Replace with an explicit
    /// early-return that runs in both debug and release.
    #[test]
    fn swizzle_to_rgba_returns_early_on_mismatched_lengths() {
        // src.len() == 10, but width * 4 == 8. The old code would
        // happily process the first two pixels and drop the trailing
        // 2 bytes; the fix bails out before writing to `out`.
        let src = [0u8; 10];
        let mut out: Vec<u8> = Vec::new();
        let result = swizzle_row_to_rgba(
            wl_shm::Format::Argb8888,
            &src,
            2,
            &mut out,
        );
        assert!(
            result.is_err(),
            "mismatched src.len() vs width*4 must produce a structured \
             error (D-5 regression), not a silent partial swizzle"
        );
        assert!(
            out.is_empty(),
            "early-return on length mismatch must not touch `out`; \
             saw {} bytes",
            out.len()
        );
    }

    #[test]
    fn swizzle_appends_without_clearing() {
        // The buffer_to_rgba caller calls `out.clear()` before the row
        // loop, but swizzle_row itself is append-only so multiple
        // calls accumulate. Verify that contract so a regression that
        // accidentally clears inside the row loop breaks loudly.
        let src = argb_row(2);
        let mut out = vec![0xAA, 0xBB]; // pre-existing tail
        swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 2, &mut out)
            .expect("argb append");
        assert_eq!(out.len(), 2 + 8);
        assert_eq!(&out[0..2], &[0xAA, 0xBB]);
    }

    #[test]
    fn surface_tracker_take_latest_empty_returns_none() {
        let mut tracker = SurfaceTracker::new();
        assert!(tracker.take_latest(7).is_none());
    }

    #[test]
    fn surface_tracker_round_trip_via_deposit_and_take() {
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

    #[test]
    fn surface_tracker_second_deposit_overrides_first() {
        let mut tracker = SurfaceTracker::new();
        tracker.deposit_for_test(1, vec![0; 16], 2, 2);
        tracker.deposit_for_test(1, vec![1, 2, 3, 4], 1, 1);
        let frame = tracker.take_latest(1).expect("frame present");
        assert_eq!(frame.rgba, vec![1, 2, 3, 4]);
        assert_eq!(frame.width, 1);
        assert_eq!(frame.height, 1);
    }

    #[test]
    fn surface_tracker_forget_drops_slot() {
        let mut tracker = SurfaceTracker::new();
        tracker.deposit_for_test(9, vec![0xFF; 4], 1, 1);
        tracker.forget(9);
        assert!(tracker.take_latest(9).is_none());
    }

    #[test]
    fn surface_tracker_isolates_surfaces() {
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
