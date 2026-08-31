//! Foreign-surface buffer readback.
//!
//! `CompositorHandler::commit` calls into a [`SurfaceTracker`], which pulls the surface's
//! latest `wl_buffer` and stages it as a flat RGBA vector the streaming task drains per encode tick.
//!
//! [`buffer_to_rgba`] reads a `wl_shm` buffer and swizzles `Argb8888`/`Xrgb8888` (the only two
//! formats smithay's [`ShmState`] advertises by default) into RGBA; the dmabuf path isn't
//! handled here ([`ReadbackError::DmaBufUnsupported`]). [`SurfaceTracker`] reuses one `Vec<u8>`
//! per surface so steady-state readback is allocation-free after the second commit.

use crate::network::protocol::SurfaceId;
use smithay::backend::allocator::dmabuf::Dmabuf;
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::wayland::shm::{BufferAccessError, with_buffer_contents};
use std::collections::HashMap;

/// Error variants surfaced by [`buffer_to_rgba`] and [`SurfaceTracker::on_commit`]. `thiserror` isn't a dep of this module, so we hand-roll `Display`/`Error`.
#[derive(Debug)]
pub enum ReadbackError {
    /// The buffer isn't a `wl_shm` buffer — smithay's [`with_buffer_contents`] returned [`BufferAccessError::NotManaged`].
    DmaBufUnsupported,
    /// The buffer's shm `Format` isn't one of the two [`ShmState`](smithay::wayland::shm::ShmState) advertises by default (`Argb8888`, `Xrgb8888`). Wraps the offending format for logs.
    UnsupportedFormat(wl_shm::Format),
    /// Smithay reported a SIGBUS or other pool-access fault.
    BufferAccess(BufferAccessError),
    /// `data.width * 4 > data.stride` or the pool isn't large enough for the declared offset/stride/height. Defensive — well-behaved clients never trip this, but a hostile client could.
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

/// Convert one row of pixel data from a `wl_shm` format into a freshly appended RGBA tail on `out`.
///
/// `src` must be exactly `width * 4` bytes; a free function so unit tests exercise every format
/// without a live `Display`. Wayland byte order is little-endian with the format name reading
/// high-order byte first: `Argb8888` on the wire is `[B, G, R, A]`, `Xrgb8888` is `[B, G, R, X]`
/// with `X` forced to `0xFF` so the H.264 alpha-compare assertion stays well-defined.
///
/// # Errors
/// - [`ReadbackError::InvalidDimensions`] — `src.len() != width * 4`.
/// - [`ReadbackError::UnsupportedFormat`] — `format` isn't `Argb8888` or `Xrgb8888`.
pub fn swizzle_row_to_rgba(
    format: wl_shm::Format,
    src: &[u8],
    width: usize,
    out: &mut Vec<u8>,
) -> Result<(), ReadbackError> {
    // A bare `debug_assert_eq!` compiles out in release, and `chunks_exact(4)` silently swallows trailing bytes a hostile client could sneak in — surface this as a structured error in both debug and release.
    let expected = width.saturating_mul(4);
    if src.len() != expected {
        tracing::error!(
            src_len = src.len(),
            expected,
            "swizzle_row_to_rgba called with mismatched src length; \
             refusing to process the row"
        );
        return Err(ReadbackError::InvalidDimensions {
            width: i32::try_from(width).unwrap_or(i32::MAX),
            // height/stride aren't known here — surface the mismatched row length via `stride` so logs are useful.
            height: 1,
            stride: i32::try_from(src.len()).unwrap_or(i32::MAX),
        });
    }
    // Keep the debug-loud-failure mode for callers passing the precondition correctly: an internal bug that violates the contract should still panic in dev builds.
    debug_assert_eq!(src.len(), expected);
    match format {
        wl_shm::Format::Argb8888 => {
            // [B, G, R, A] -> [R, G, B, A]
            for px in src.chunks_exact(4) {
                let &[b, g, r, a] = px else {
                    continue; // chunks_exact(4) guarantees len == 4
                };
                out.push(r);
                out.push(g);
                out.push(b);
                out.push(a);
            }
            Ok(())
        }
        wl_shm::Format::Xrgb8888 => {
            // [B, G, R, X] -> [R, G, B, 0xFF]
            for px in src.chunks_exact(4) {
                let &[b, g, r, _x] = px else {
                    continue; // chunks_exact(4) guarantees len == 4
                };
                out.push(r);
                out.push(g);
                out.push(b);
                out.push(0xFF);
            }
            Ok(())
        }
        other => Err(ReadbackError::UnsupportedFormat(other)),
    }
}

/// Convert a `wl_shm` buffer into a flat row-major RGBA vector.
///
/// `out` is cleared and re-filled; reusing the same buffer across commits is the no-allocation
/// steady-state path [`SurfaceTracker`] relies on.
///
/// # Errors
/// - [`ReadbackError::DmaBufUnsupported`] — buffer isn't shm.
/// - [`ReadbackError::UnsupportedFormat`] — format other than `Argb8888`/`Xrgb8888`.
/// - [`ReadbackError::InvalidDimensions`] — stride < width\*4, negative dims, etc.
/// - [`ReadbackError::BufferAccess`] — smithay-side SIGBUS/NotManaged.
pub fn buffer_to_rgba(buffer: &WlBuffer, out: &mut Vec<u8>) -> Result<(u32, u32), ReadbackError> {
    let result: Result<(u32, u32), ReadbackError> = with_buffer_contents(
        buffer,
        |ptr, len, data| -> Result<(u32, u32), ReadbackError> {
            let width = data.width;
            let height = data.height;
            let stride = data.stride;
            // Reject zero/negative dims and strides that don't hold a full row — a hostile client could give a tiny stride to short-read the pool.
            if width <= 0 || height <= 0 || stride < width.saturating_mul(4) {
                return Err(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                });
            }
            // Also reject a declared (offset + stride*height) exceeding the pool's length — how a hostile client could read past the mmap.
            // A negative offset/stride/height can't happen here (all rejected above as <= 0),
            // so try_from only ever falls back on paper; the fallback still routes into the
            // same bounds-check-then-error path rather than assuming success.
            let offset = usize::try_from(data.offset).unwrap_or(usize::MAX);
            let stride_u = usize::try_from(stride).unwrap_or(usize::MAX);
            let height_u = usize::try_from(height).unwrap_or(usize::MAX);
            let width_u = usize::try_from(width).unwrap_or(usize::MAX);
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
            let buf_bytes = pool
                .get(offset..needed)
                .ok_or(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                })?;

            out.clear();
            out.reserve(width_u.saturating_mul(height_u).saturating_mul(4));
            let row_bytes = width_u.saturating_mul(4);
            for row in 0..height_u {
                let row_start = row.saturating_mul(stride_u);
                let row_end = row_start.saturating_add(row_bytes);
                let src_row =
                    buf_bytes
                        .get(row_start..row_end)
                        .ok_or(ReadbackError::InvalidDimensions {
                            width,
                            height,
                            stride,
                        })?;
                swizzle_row_to_rgba(data.format, src_row, width_u, out)?;
            }
            Ok((
                u32::try_from(width).unwrap_or(u32::MAX),
                u32::try_from(height).unwrap_or(u32::MAX),
            ))
        },
    )?;
    result
}

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
        self.latest_dmabufs.remove(&surface_id);
        self.last_capture_path.remove(&surface_id);
    }

    /// Stash the DMABUF attached to the surface's latest commit, called after [`smithay::wayland::dmabuf::get_dmabuf`] returns `Ok`; replaces any prior DMABUF for the surface.
    pub fn note_dmabuf_committed(&mut self, surface_id: SurfaceId, dmabuf: Dmabuf) {
        self.latest_dmabufs.insert(surface_id, dmabuf);
    }

    /// Drain the most-recently-staged DMABUF for `surface_id`, symmetric with [`Self::take_latest`]. Called right after [`Self::note_dmabuf_committed`] so the encoder can build a `FrameInput::DmabufFd`.
    pub fn take_latest_dmabuf(&mut self, surface_id: SurfaceId) -> Option<Dmabuf> {
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
    /// [`crate::compositor::state::State::toplevel_destroyed`] reads as "drop this surface's tracker buffer entirely" rather than the ambiguous "forget".
    ///
    /// Without this call, a client destroying its toplevel without a prior `attach NULL` leaves its
    /// `width*height*4`-byte `Vec<u8>` slot allocated forever — a per-surface leak that compounds across long sessions.
    pub fn drop_surface(&mut self, surface_id: SurfaceId) {
        self.latest.remove(&surface_id);
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

impl SurfaceBuffer {
    fn empty() -> Self {
        Self {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        }
    }
}

/// A drained frame returned from [`SurfaceTracker::take_latest`]. `rgba` is row-major, no padding, `width * height * 4` bytes — suitable for direct hand-off to [`crate::network::H264Encoder::encode`].
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

    /// Test fixture: deterministic single-row Argb8888 buffer exercising the byte swizzle without client-server plumbing.
    fn argb_row(width: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(width.saturating_mul(4));
        for x in 0..width {
            // Wire bytes [B, G, R, A]: distinguishable per-channel values so a missed swizzle (e.g. R/B swap) shows up immediately.
            let byte = u8::try_from(x % 256).unwrap_or(0);
            out.push(byte.wrapping_mul(3)); // B
            out.push(byte.wrapping_mul(5)); // G
            out.push(byte.wrapping_mul(7)); // R
            out.push(0xC0); // A
        }
        out
    }

    #[test]
    fn swizzle_row_argb_to_rgba_swaps_b_and_r() {
        let src = argb_row(4);
        let mut out = Vec::new();
        swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 4, &mut out).expect("argb swizzle");
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
        // Xrgb8888: wire bytes [B, G, R, X]; X may be anything from the client — we always emit alpha = 0xFF.
        let src = vec![10, 20, 30, 0x00, 40, 50, 60, 0x7F];
        let mut out = Vec::new();
        swizzle_row_to_rgba(wl_shm::Format::Xrgb8888, &src, 2, &mut out).expect("xrgb swizzle");
        assert_eq!(out, vec![30, 20, 10, 0xFF, 60, 50, 40, 0xFF]);
    }

    #[test]
    fn swizzle_row_rejects_unsupported_format() {
        // RGB565 isn't advertised by smithay's default ShmState, so an attached one should error cleanly rather than produce a wrong-looking frame.
        let src = vec![0u8; 4 * 4];
        let mut out = Vec::new();
        let err = swizzle_row_to_rgba(wl_shm::Format::Rgb565, &src, 4, &mut out).unwrap_err();
        match err {
            ReadbackError::UnsupportedFormat(wl_shm::Format::Rgb565) => {}
            other => panic!("expected UnsupportedFormat(Rgb565), got {other:?}"),
        }
    }

    #[test]
    fn swizzle_to_rgba_returns_early_on_mismatched_lengths() {
        // src.len() == 10, but width * 4 == 8.
        let src = [0u8; 10];
        let mut out: Vec<u8> = Vec::new();
        let result = swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 2, &mut out);
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
        // buffer_to_rgba's caller clears `out` before the row loop, but swizzle_row itself is
        // append-only — verify that contract so a regression clearing inside the loop breaks loudly.
        let src = argb_row(2);
        let mut out = vec![0xAA, 0xBB]; // pre-existing tail
        swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 2, &mut out).expect("argb append");
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

    /// `drop_surface` is the "client tore down its toplevel; reclaim the buffer" entry point — verifies the slot is actually emptied, not just cleared.
    #[test]
    fn drop_surface_removes_buffer() {
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

    #[test]
    fn capture_path_counter_tracks_dmabuf_direct_vs_shm() {
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

    #[test]
    fn drop_surface_clears_capture_path() {
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
