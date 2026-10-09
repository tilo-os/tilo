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
            append_bgra_row::<false>(src, out);
            Ok(())
        }
        wl_shm::Format::Xrgb8888 => {
            append_bgra_row::<true>(src, out);
            Ok(())
        }
        other => Err(ReadbackError::UnsupportedFormat(other)),
    }
}

fn append_bgra_row<const OPAQUE: bool>(src: &[u8], out: &mut Vec<u8>) {
    // One bulk append checks capacity once per row. Converting the initialized
    // tail lets LLVM vectorize without a Vec capacity check for each channel.
    // The source and existing prefix remain untouched; no CPU-specific ISA or
    // uninitialized-memory access is required.
    let start = out.len();
    out.extend_from_slice(src);
    let (_, tail) = out.split_at_mut(start);
    bgra_to_rgba_in_place::<OPAQUE>(tail);
}

/// Swap blue and red of every whole pixel in `pixels`, forcing alpha opaque
/// for an `X` format.
fn bgra_to_rgba_in_place<const OPAQUE: bool>(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let [blue, _, red, alpha] = pixel else {
            continue;
        };
        std::mem::swap(blue, red);
        if OPAQUE {
            *alpha = 255;
        }
    }
}

/// Convert one row of `wl_shm` pixels into `dst`, which must be exactly as
/// long as `src` (a whole number of pixels). `dst` is untouched on error.
///
/// # Errors
/// - [`ReadbackError::InvalidDimensions`] — the lengths differ or are not whole pixels.
/// - [`ReadbackError::UnsupportedFormat`] — `format` isn't `Argb8888` or `Xrgb8888`.
pub fn swizzle_row_into(
    format: wl_shm::Format,
    src: &[u8],
    dst: &mut [u8],
) -> Result<(), ReadbackError> {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return Err(ReadbackError::InvalidDimensions {
            width: i32::try_from(dst.len() / 4).unwrap_or(i32::MAX),
            height: 1,
            stride: i32::try_from(src.len()).unwrap_or(i32::MAX),
        });
    }
    match format {
        wl_shm::Format::Argb8888 => {
            dst.copy_from_slice(src);
            bgra_to_rgba_in_place::<false>(dst);
            Ok(())
        }
        wl_shm::Format::Xrgb8888 => {
            dst.copy_from_slice(src);
            bgra_to_rgba_in_place::<true>(dst);
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

/// A rectangle of an SHM buffer, in buffer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShmRegion {
    pub x: usize,
    pub y: usize,
    pub width: usize,
    pub height: usize,
}

impl ShmRegion {
    /// The part of this region inside a `width`×`height` buffer, if any.
    #[must_use]
    pub fn clipped(self, width: usize, height: usize) -> Option<Self> {
        let right = self.x.saturating_add(self.width).min(width);
        let bottom = self.y.saturating_add(self.height).min(height);
        (self.x < right && self.y < bottom).then(|| Self {
            x: self.x,
            y: self.y,
            width: right.saturating_sub(self.x),
            height: bottom.saturating_sub(self.y),
        })
    }

    /// The region's area in pixels.
    #[must_use]
    pub fn pixels(self) -> usize {
        self.width.saturating_mul(self.height)
    }
}

/// An owned copy of what one valid SHM commit changed. Copying bytes from the
/// client mapping must happen while Smithay holds the pool access guard; the
/// more expensive format conversion can then safely run on a worker thread.
#[derive(Debug)]
pub struct OwnedShmFrame {
    bytes: Vec<u8>,
    width: usize,
    height: usize,
    stride: usize,
    format: wl_shm::Format,
    /// `None`: `bytes` is the whole buffer, rows `stride` apart. Otherwise
    /// `bytes` holds only these regions, each one's rows packed in turn
    /// (`region.width * 4` bytes a row); every other pixel is unchanged from
    /// the frame the commit replaced.
    regions: Option<Vec<ShmRegion>>,
}

impl OwnedShmFrame {
    /// A tightly packed `Argb8888` frame, as a client's pool would hold it.
    #[cfg(test)]
    pub(crate) fn argb8888(bytes: Vec<u8>, width: usize, height: usize) -> Self {
        Self {
            bytes,
            width,
            height,
            stride: width.saturating_mul(4),
            format: wl_shm::Format::Argb8888,
            regions: None,
        }
    }

    /// An `Argb8888` copy of only `regions` of a `width`×`height` buffer,
    /// their rows packed in turn, as [`copy_shm_damage`] makes one.
    #[cfg(test)]
    pub(crate) fn argb8888_damage(
        bytes: Vec<u8>,
        size: (usize, usize),
        regions: Vec<ShmRegion>,
    ) -> Self {
        Self::damage(bytes, size, regions, wl_shm::Format::Argb8888)
    }

    /// [`Self::argb8888_damage`] of an `Xrgb8888` buffer.
    #[cfg(test)]
    pub(crate) fn xrgb8888_damage(
        bytes: Vec<u8>,
        size: (usize, usize),
        regions: Vec<ShmRegion>,
    ) -> Self {
        Self::damage(bytes, size, regions, wl_shm::Format::Xrgb8888)
    }

    #[cfg(test)]
    fn damage(
        bytes: Vec<u8>,
        (width, height): (usize, usize),
        regions: Vec<ShmRegion>,
        format: wl_shm::Format,
    ) -> Self {
        Self {
            bytes,
            width,
            height,
            stride: width.saturating_mul(4),
            format,
            regions: Some(regions),
        }
    }

    /// The buffer's width and height in pixels.
    #[must_use]
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The buffer's size and pixel format.
    #[must_use]
    pub fn shape(&self) -> ShmShape {
        ShmShape {
            width: self.width,
            height: self.height,
            format: self.format,
        }
    }

    /// The changed regions, or `None` when this carries the whole buffer.
    #[must_use]
    pub fn regions(&self) -> Option<&[ShmRegion]> {
        self.regions.as_deref()
    }

    /// Bytes copied out of the client's pool.
    #[must_use]
    pub fn copied_bytes(&self) -> usize {
        self.bytes.len()
    }

    /// The copy's allocation, for a later commit to reuse.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn invalid(&self) -> ReadbackError {
        ReadbackError::InvalidDimensions {
            width: i32::try_from(self.width).unwrap_or(i32::MAX),
            height: i32::try_from(self.height).unwrap_or(i32::MAX),
            stride: i32::try_from(self.stride).unwrap_or(i32::MAX),
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
    copy_shm_damage(buffer, &ShmCopy::default(), Vec::new())
}

/// What one SHM commit needs copied out of its pool.
#[derive(Clone, Copy, Debug, Default)]
pub struct ShmCopy<'a> {
    /// The commit's buffer damage; `None` when unknown (the whole buffer).
    pub damage: Option<&'a [ShmRegion]>,
    /// The shape of the frame this commit's pixels are laid over. Damage
    /// describes change against that frame, so it only applies when the new
    /// buffer has the same size and format: pixels outside it were converted
    /// from the old format (an `X` format forces alpha opaque).
    pub base: Option<ShmShape>,
}

impl ShmCopy<'_> {
    /// The bytes a copy of a buffer shaped like `base` will take, when the
    /// buffer has that shape; `None` without a base.
    #[must_use]
    pub fn expected_bytes(&self) -> Option<usize> {
        let base = self.base?;
        let pixels = damaged_regions(self, base).map_or_else(
            || base.width.saturating_mul(base.height),
            |regions| {
                regions
                    .iter()
                    .map(|region| region.pixels())
                    .fold(0, usize::saturating_add)
            },
        );
        Some(pixels.saturating_mul(4))
    }
}

/// An SHM buffer's size and pixel format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShmShape {
    pub width: usize,
    pub height: usize,
    pub format: wl_shm::Format,
}

/// Copy what a commit changed out of an SHM `buffer`'s pool into `scratch`
/// (reused, never shrunk): only the damaged regions when [`ShmCopy`] allows,
/// otherwise the whole buffer. Regions covering half the buffer or more are
/// copied whole: one contiguous copy beats many short ones.
///
/// # Errors
///
/// Returns an error when the buffer is not SHM-backed or its declared
/// geometry does not fit inside the pool.
pub fn copy_shm_damage(
    buffer: &WlBuffer,
    request: &ShmCopy<'_>,
    scratch: Vec<u8>,
) -> Result<OwnedShmFrame, ReadbackError> {
    with_buffer_contents(buffer, |ptr, len, data| {
        // SAFETY: Smithay guarantees this mapping for the callback duration,
        // and `copy_from_pool` borrows nothing from it past its return.
        let pool = unsafe { std::slice::from_raw_parts(ptr, len) };
        let layout = ShmLayout {
            offset: data.offset,
            width: data.width,
            height: data.height,
            stride: data.stride,
            format: data.format,
        };
        copy_from_pool(pool, &layout, request, scratch)
    })?
}

/// Where one SHM buffer lies in its pool, as the client declared it.
#[derive(Clone, Copy, Debug)]
pub struct ShmLayout {
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub format: wl_shm::Format,
}

/// [`copy_shm_damage`] over a pool's bytes: the declared geometry is checked
/// against `pool` before anything is read, as a hostile client controls it.
///
/// # Errors
///
/// Returns an error when the declared geometry does not fit inside `pool`.
pub fn copy_from_pool(
    pool: &[u8],
    layout: &ShmLayout,
    request: &ShmCopy<'_>,
    mut scratch: Vec<u8>,
) -> Result<OwnedShmFrame, ReadbackError> {
    let (width, height, stride) = (layout.width, layout.height, layout.stride);
    let invalid = ReadbackError::InvalidDimensions {
        width,
        height,
        stride,
    };
    if width <= 0 || height <= 0 || stride < width.saturating_mul(4) {
        return Err(invalid);
    }
    let offset = usize::try_from(layout.offset).unwrap_or(usize::MAX);
    let stride_u = usize::try_from(stride).unwrap_or(usize::MAX);
    let height_u = usize::try_from(height).unwrap_or(usize::MAX);
    let width_u = usize::try_from(width).unwrap_or(usize::MAX);
    let Some(whole) = offset
        .checked_add(stride_u.saturating_mul(height_u))
        .and_then(|needed| pool.get(offset..needed))
    else {
        return Err(invalid);
    };
    let shape = ShmShape {
        width: width_u,
        height: height_u,
        format: layout.format,
    };
    let regions = damaged_regions(request, shape);
    scratch.clear();
    match &regions {
        None => scratch.extend_from_slice(whole),
        Some(regions) => {
            let pixels = regions
                .iter()
                .map(|region| region.pixels())
                .fold(0_usize, usize::saturating_add);
            scratch.reserve(pixels.saturating_mul(4));
            for region in regions {
                for row in region.y..region.y.saturating_add(region.height) {
                    let start = row
                        .saturating_mul(stride_u)
                        .saturating_add(region.x.saturating_mul(4));
                    let end = start.saturating_add(region.width.saturating_mul(4));
                    let Some(bytes) = whole.get(start..end) else {
                        return Err(invalid);
                    };
                    scratch.extend_from_slice(bytes);
                }
            }
        }
    }
    Ok(OwnedShmFrame {
        bytes: scratch,
        width: width_u,
        height: height_u,
        stride: stride_u,
        format: layout.format,
        regions,
    })
}

/// The regions of a buffer of `shape` a commit needs copied, or `None` for
/// the whole buffer.
fn damaged_regions(request: &ShmCopy<'_>, shape: ShmShape) -> Option<Vec<ShmRegion>> {
    if request.base != Some(shape) {
        return None;
    }
    let ShmShape { width, height, .. } = shape;
    let regions: Vec<ShmRegion> = request
        .damage?
        .iter()
        .filter_map(|region| region.clipped(width, height))
        .collect();
    let pixels = regions
        .iter()
        .map(|region| region.pixels())
        .fold(0_usize, usize::saturating_add);
    (pixels.saturating_mul(2) < width.saturating_mul(height)).then_some(regions)
}

/// Convert a whole copied SHM frame to tightly packed RGBA8 in `out`,
/// returning its width and height.
///
/// # Errors
///
/// Returns an error when the frame carries only damaged regions, a row lies
/// outside the copied bytes or the pixel format is unsupported.
pub fn owned_shm_to_rgba(
    frame: &OwnedShmFrame,
    out: &mut Vec<u8>,
) -> Result<(u32, u32), ReadbackError> {
    if frame.regions.is_some() {
        return Err(frame.invalid());
    }
    out.clear();
    out.reserve(frame.width.saturating_mul(frame.height).saturating_mul(4));
    let row_bytes = frame.width.saturating_mul(4);
    for row in 0..frame.height {
        let start = row.saturating_mul(frame.stride);
        let src = frame
            .bytes
            .get(start..start.saturating_add(row_bytes))
            .ok_or_else(|| frame.invalid())?;
        swizzle_row_to_rgba(frame.format, src, frame.width, out)?;
    }
    Ok((
        u32::try_from(frame.width).unwrap_or(u32::MAX),
        u32::try_from(frame.height).unwrap_or(u32::MAX),
    ))
}

/// Write `frame` into `target`, a tightly packed RGBA8 image of the frame's
/// size: every pixel for a whole frame, only the damaged regions otherwise.
///
/// # Errors
///
/// Returns an error when `target` is not the frame's size, a row lies outside
/// the copied bytes or the pixel format is unsupported. `target` may then be
/// partly written.
pub fn apply_owned_shm(frame: &OwnedShmFrame, target: &mut [u8]) -> Result<(), ReadbackError> {
    let row_bytes = frame.width.saturating_mul(4);
    if target.len() != row_bytes.saturating_mul(frame.height) {
        return Err(frame.invalid());
    }
    let Some(regions) = &frame.regions else {
        for (row, dst) in target.chunks_exact_mut(row_bytes.max(1)).enumerate() {
            let start = row.saturating_mul(frame.stride);
            let src = frame
                .bytes
                .get(start..start.saturating_add(row_bytes))
                .ok_or_else(|| frame.invalid())?;
            swizzle_row_into(frame.format, src, dst)?;
        }
        return Ok(());
    };
    let mut packed = 0_usize;
    for region in regions {
        let span = region.width.saturating_mul(4);
        for row in region.y..region.y.saturating_add(region.height) {
            let src = frame
                .bytes
                .get(packed..packed.saturating_add(span))
                .ok_or_else(|| frame.invalid())?;
            packed = packed.saturating_add(span);
            let start = row
                .saturating_mul(row_bytes)
                .saturating_add(region.x.saturating_mul(4));
            let dst = target
                .get_mut(start..start.saturating_add(span))
                .ok_or_else(|| frame.invalid())?;
            swizzle_row_into(frame.format, src, dst)?;
        }
    }
    Ok(())
}

pub use crate::network::surface_tracker::{CapturePath, SurfaceTracker, TakenFrame};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swizzle_matches_every_pixel_and_preserves_the_existing_tail() {
        for width in [
            0_usize, 1, 3, 7, 15, 16, 17, 31, 32, 33, 129, 801, 1920, 3840,
        ] {
            let source: Vec<u8> = (0..width.saturating_mul(4))
                .map(|index: usize| index.wrapping_mul(131).wrapping_add(17).to_le_bytes()[0])
                .collect();
            for format in [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888] {
                let mut expected = vec![91, 92, 93];
                for pixel in source.chunks_exact(4) {
                    let [blue, green, red, alpha] = pixel else {
                        unreachable!("whole pixels in the fixture");
                    };
                    expected.extend_from_slice(&[
                        *red,
                        *green,
                        *blue,
                        if format == wl_shm::Format::Xrgb8888 {
                            255
                        } else {
                            *alpha
                        },
                    ]);
                }
                let mut output = vec![91, 92, 93];
                swizzle_row_to_rgba(format, &source, width, &mut output).unwrap();
                assert_eq!(output, expected, "format={format:?}, width={width}");
            }
        }
    }

    #[test]
    fn rejected_swizzle_rows_preserve_the_destination() {
        for (format, source, width) in [
            (wl_shm::Format::Argb8888, vec![0; 7], 2),
            (wl_shm::Format::Xrgb8888, vec![0; 9], 2),
            (wl_shm::Format::Argb8888, vec![0; 4], usize::MAX),
            (wl_shm::Format::Rgb565, vec![0; 8], 2),
            (wl_shm::Format::Rgb565, vec![], 0),
        ] {
            let mut output = vec![91, 92, 93];
            assert!(swizzle_row_to_rgba(format, &source, width, &mut output).is_err());
            assert_eq!(output, [91, 92, 93]);
        }
    }

    #[test]
    fn owned_shm_conversion_excludes_stride_padding() {
        for format in [wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888] {
            let frame = OwnedShmFrame {
                bytes: vec![10, 20, 30, 40, 99, 99, 50, 60, 70, 80, 99, 99],
                width: 1,
                height: 2,
                stride: 6,
                format,
                regions: None,
            };
            let mut output = vec![91, 92, 93];
            assert_eq!(owned_shm_to_rgba(&frame, &mut output).unwrap(), (1, 2));
            let expected = if format == wl_shm::Format::Argb8888 {
                vec![30, 20, 10, 40, 70, 60, 50, 80]
            } else {
                vec![30, 20, 10, 255, 70, 60, 50, 255]
            };
            assert_eq!(output, expected);
        }
    }

    #[test]
    fn damaged_copies_land_only_on_their_regions() {
        // A 3×2 RGBA image; one 2×1 region at (1, 1) carries new pixels.
        let regions = vec![ShmRegion {
            x: 1,
            y: 1,
            width: 2,
            height: 1,
        }];
        for (format, alpha) in [
            (wl_shm::Format::Argb8888, 40),
            (wl_shm::Format::Xrgb8888, 255),
        ] {
            let frame = OwnedShmFrame {
                bytes: vec![10, 20, 30, 40, 50, 60, 70, 40],
                width: 3,
                height: 2,
                stride: 12,
                format,
                regions: Some(regions.clone()),
            };
            let mut target = vec![9; 3 * 2 * 4];
            apply_owned_shm(&frame, &mut target).unwrap();
            let mut expected = vec![9; 16];
            expected.extend_from_slice(&[30, 20, 10, alpha, 70, 60, 50, alpha]);
            assert_eq!(target, expected, "format={format:?}");
            assert!(
                owned_shm_to_rgba(&frame, &mut Vec::new()).is_err(),
                "a damaged-only copy is not a whole frame"
            );
            let mut wrong_size = vec![0; 8];
            assert!(apply_owned_shm(&frame, &mut wrong_size).is_err());
        }
    }

    #[test]
    fn pool_copies_take_only_damaged_rows_and_reject_hostile_geometry() {
        // A 4×3 buffer at offset 2 with 4 bytes of row padding: pixel bytes
        // are their own (x, y) so a misplaced copy shows.
        let layout = ShmLayout {
            offset: 2,
            width: 4,
            height: 3,
            stride: 20,
            format: wl_shm::Format::Argb8888,
        };
        let mut pool = vec![0xee; 2 + 20 * 3];
        for y in 0_u8..3 {
            for x in 0_u8..4 {
                let start = 2 + usize::from(y) * 20 + usize::from(x) * 4;
                pool[start..start + 4].copy_from_slice(&[x, y, 0, 255]);
            }
        }
        let region = [ShmRegion {
            x: 1,
            y: 1,
            width: 2,
            height: 2,
        }];
        let request = ShmCopy {
            damage: Some(&region),
            base: Some(shape(4, 3, wl_shm::Format::Argb8888)),
        };
        assert_eq!(request.expected_bytes(), Some(16));
        let damaged = copy_from_pool(&pool, &layout, &request, Vec::with_capacity(64)).unwrap();
        assert_eq!(damaged.regions(), Some(&region[..]));
        assert_eq!(damaged.copied_bytes(), 16, "two rows of two pixels");
        let mut target = vec![0; 4 * 3 * 4];
        apply_owned_shm(&damaged, &mut target).unwrap();
        let pixel = |x: usize, y: usize| &target[(y * 4 + x) * 4..][..4];
        assert_eq!(pixel(1, 1), [0, 1, 1, 255]);
        assert_eq!(pixel(2, 2), [0, 2, 2, 255]);
        assert_eq!(pixel(0, 0), [0, 0, 0, 0], "outside the damage untouched");
        let whole = copy_from_pool(&pool, &layout, &ShmCopy::default(), Vec::new()).unwrap();
        assert_eq!(whole.regions(), None);
        let other_format = ShmLayout {
            format: wl_shm::Format::Xrgb8888,
            ..layout
        };
        assert_eq!(
            copy_from_pool(&pool, &other_format, &request, Vec::new())
                .unwrap()
                .regions(),
            None,
            "a buffer of another format is copied whole"
        );
        let mut target = vec![0; 4 * 3 * 4];
        apply_owned_shm(&whole, &mut target).unwrap();
        assert_eq!(&target[44..48], [0, 2, 3, 255]);
        for hostile in [
            ShmLayout {
                offset: 3,
                ..layout
            },
            ShmLayout {
                stride: 15,
                ..layout
            },
            ShmLayout {
                height: 4,
                ..layout
            },
            ShmLayout {
                offset: i32::MAX,
                ..layout
            },
        ] {
            assert!(
                copy_from_pool(&pool, &hostile, &request, Vec::new()).is_err(),
                "{hostile:?} reads outside the pool or the row"
            );
        }
    }

    #[test]
    fn whole_copies_fill_the_target_without_stride_padding() {
        let frame = OwnedShmFrame {
            bytes: vec![10, 20, 30, 40, 99, 99, 50, 60, 70, 80, 99, 99],
            width: 1,
            height: 2,
            stride: 6,
            format: wl_shm::Format::Argb8888,
            regions: None,
        };
        let mut target = vec![0; 8];
        apply_owned_shm(&frame, &mut target).unwrap();
        assert_eq!(target, [30, 20, 10, 40, 70, 60, 50, 80]);
    }

    fn shape(width: usize, height: usize, format: wl_shm::Format) -> ShmShape {
        ShmShape {
            width,
            height,
            format,
        }
    }

    #[test]
    fn only_damage_against_a_same_shaped_frame_is_copied_alone() {
        let argb = wl_shm::Format::Argb8888;
        let uhd = shape(3840, 2160, argb);
        let caret = [ShmRegion {
            x: 3800,
            y: 2150,
            width: 64,
            height: 32,
        }];
        let damage = ShmCopy {
            damage: Some(&caret),
            base: Some(uhd),
        };
        assert_eq!(
            damaged_regions(&damage, uhd),
            Some(vec![ShmRegion {
                x: 3800,
                y: 2150,
                width: 40,
                height: 10,
            }]),
            "clipped to the buffer"
        );
        assert_eq!(damage.expected_bytes(), Some(40 * 10 * 4));
        assert_eq!(
            damaged_regions(&damage, shape(1920, 1080, argb)),
            None,
            "a resized buffer is copied whole"
        );
        assert_eq!(
            damaged_regions(&damage, shape(3840, 2160, wl_shm::Format::Xrgb8888)),
            None,
            "a buffer of another format is copied whole"
        );
        let unknown = ShmCopy {
            damage: None,
            base: Some(uhd),
        };
        assert_eq!(damaged_regions(&unknown, uhd), None);
        assert_eq!(unknown.expected_bytes(), Some(3840 * 2160 * 4));
        let no_base = ShmCopy {
            damage: Some(&caret),
            base: None,
        };
        assert_eq!(damaged_regions(&no_base, uhd), None);
        assert_eq!(no_base.expected_bytes(), None);
        let most = [ShmRegion {
            x: 0,
            y: 0,
            width: 3840,
            height: 1080,
        }];
        let large = ShmCopy {
            damage: Some(&most),
            base: Some(uhd),
        };
        assert_eq!(
            damaged_regions(&large, uhd),
            None,
            "half the buffer or more is one contiguous copy"
        );
    }

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
