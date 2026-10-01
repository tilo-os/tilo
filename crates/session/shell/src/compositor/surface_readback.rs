//! Foreign-surface buffer readback.
//!
//! `CompositorHandler::commit` calls into a [`SurfaceTracker`], which pulls the surface's
//! latest `wl_buffer` and stages it as a flat RGBA vector the streaming task drains per encode tick.
//!
//! [`buffer_to_rgba`] reads a `wl_shm` buffer and swizzles `Argb8888`/`Xrgb8888` (the only two
//! formats smithay's `ShmState` advertises by default) into RGBA; the dmabuf path isn't
//! handled here ([`ReadbackError::DmaBufUnsupported`]). [`SurfaceTracker`] reuses one `Vec<u8>`
//! per surface so steady-state readback is allocation-free after the second commit.

use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_shm;
use smithay::wayland::shm::{BufferAccessError, with_buffer_contents};

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

/// An owned snapshot of one valid SHM buffer. Copying bytes from the client
/// mapping must happen while Smithay holds the pool access guard; the more
/// expensive format conversion can then safely run on a worker thread.
#[derive(Debug)]
pub struct OwnedShmFrame {
    bytes: Vec<u8>,
    width: usize,
    height: usize,
    stride: usize,
    format: wl_shm::Format,
}

#[cfg(test)]
impl OwnedShmFrame {
    /// A tightly packed `Argb8888` frame, as a client's pool would hold it.
    pub(crate) fn argb8888(bytes: Vec<u8>, width: usize, height: usize) -> Self {
        Self {
            bytes,
            width,
            height,
            stride: width.saturating_mul(4),
            format: wl_shm::Format::Argb8888,
        }
    }
}

/// Copy the committed contents of an SHM `buffer` out of the client's pool.
///
/// # Errors
///
/// Returns an error when the buffer is not SHM-backed or its declared
/// geometry does not fit inside the pool.
pub fn copy_shm_frame(buffer: &WlBuffer) -> Result<OwnedShmFrame, ReadbackError> {
    with_buffer_contents(buffer, |ptr, len, data| {
        let (width, height, stride) = (data.width, data.height, data.stride);
        if width <= 0 || height <= 0 || stride < width.saturating_mul(4) {
            return Err(ReadbackError::InvalidDimensions {
                width,
                height,
                stride,
            });
        }
        let offset = usize::try_from(data.offset).unwrap_or(usize::MAX);
        let stride_u = usize::try_from(stride).unwrap_or(usize::MAX);
        let height_u = usize::try_from(height).unwrap_or(usize::MAX);
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
        // SAFETY: Smithay guarantees this mapping for the callback duration;
        // bounds above restrict the copied range to the declared buffer.
        let pool = unsafe { std::slice::from_raw_parts(ptr, len) };
        Ok(OwnedShmFrame {
            bytes: pool
                .get(offset..needed)
                .ok_or(ReadbackError::InvalidDimensions {
                    width,
                    height,
                    stride,
                })?
                .to_vec(),
            width: usize::try_from(width).unwrap_or(usize::MAX),
            height: height_u,
            stride: stride_u,
            format: data.format,
        })
    })?
}

/// Convert a copied SHM frame to tightly packed RGBA8 in `out`, returning
/// its width and height.
///
/// # Errors
///
/// Returns an error when a row lies outside the copied bytes or the pixel
/// format is unsupported.
pub fn owned_shm_to_rgba(
    frame: &OwnedShmFrame,
    out: &mut Vec<u8>,
) -> Result<(u32, u32), ReadbackError> {
    out.clear();
    out.reserve(frame.width.saturating_mul(frame.height).saturating_mul(4));
    let row_bytes = frame.width.saturating_mul(4);
    for row in 0..frame.height {
        let start = row.saturating_mul(frame.stride);
        let src = frame
            .bytes
            .get(start..start.saturating_add(row_bytes))
            .ok_or_else(|| ReadbackError::InvalidDimensions {
                width: i32::try_from(frame.width).unwrap_or(i32::MAX),
                height: i32::try_from(frame.height).unwrap_or(i32::MAX),
                stride: i32::try_from(frame.stride).unwrap_or(i32::MAX),
            })?;
        swizzle_row_to_rgba(frame.format, src, frame.width, out)?;
    }
    Ok((
        u32::try_from(frame.width).unwrap_or(u32::MAX),
        u32::try_from(frame.height).unwrap_or(u32::MAX),
    ))
}

pub use crate::network::surface_tracker::{CapturePath, SurfaceTracker, TakenFrame};

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
    fn swizzle_converts_supported_formats_and_rejects_the_rest() {
        // Phase `swizzle_to_rgba_returns_early_on_mismatched_lengths`.
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
        // Phase `swizzle_row_argb_to_rgba_swaps_b_and_r`.
        {
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
        // Phase `swizzle_row_xrgb_forces_alpha_to_ff`.
        {
            // Xrgb8888: wire bytes [B, G, R, X]; X may be anything from the client — we always emit alpha = 0xFF.
            let src = vec![10, 20, 30, 0x00, 40, 50, 60, 0x7F];
            let mut out = Vec::new();
            swizzle_row_to_rgba(wl_shm::Format::Xrgb8888, &src, 2, &mut out).expect("xrgb swizzle");
            assert_eq!(out, vec![30, 20, 10, 0xFF, 60, 50, 40, 0xFF]);
        }
        // Phase `swizzle_row_rejects_unsupported_format`.
        {
            // RGB565 isn't advertised by smithay's default ShmState, so an attached one should error cleanly rather than produce a wrong-looking frame.
            let src = vec![0u8; 4 * 4];
            let mut out = Vec::new();
            let err = swizzle_row_to_rgba(wl_shm::Format::Rgb565, &src, 4, &mut out).unwrap_err();
            match err {
                ReadbackError::UnsupportedFormat(wl_shm::Format::Rgb565) => {}
                other => panic!("expected UnsupportedFormat(Rgb565), got {other:?}"),
            }
        }
        // Phase `swizzle_to_rgba_returns_early_on_mismatched_lengths` (defined above).
        swizzle_to_rgba_returns_early_on_mismatched_lengths();
        // Phase `swizzle_appends_without_clearing`.
        {
            // buffer_to_rgba's caller clears `out` before the row loop, but swizzle_row itself is
            // append-only — verify that contract so a regression clearing inside the loop breaks loudly.
            let src = argb_row(2);
            let mut out = vec![0xAA, 0xBB]; // pre-existing tail
            swizzle_row_to_rgba(wl_shm::Format::Argb8888, &src, 2, &mut out).expect("argb append");
            assert_eq!(out.len(), 2 + 8);
            assert_eq!(&out[0..2], &[0xAA, 0xBB]);
        }
    }
}
