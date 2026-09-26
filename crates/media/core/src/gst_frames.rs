//! Turning appsink samples into [`Frame`]s, shared by playback
//! ([`crate::gst_backend`]) and the real-time decoder
//! ([`crate::realtime`]).

use std::sync::Arc;

use gstreamer as gst;
use gstreamer_video as gst_video;

use crate::dmabuf::{ColorRange, Colorimetry, YuvMatrix};
#[cfg(unix)]
use crate::dmabuf::{DmabufImage, DmabufPlane};
use crate::frame::{Frame, FrameAcceptance, FramePayload, PlanarFormat, PlanarImage, PlaneData};

// ---------------------------------------------------------------------------
// libgstallocators — the two entry points needed to read a dmabuf fd out of a
// GstMemory. Bound directly rather than pulling in `gstreamer-allocators`,
// which would add a crate to the workspace lock for three C symbols. Linked
// only when a GStreamer feature is on, alongside the rest of the GStreamer stack.
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[link(name = "gstallocators-1.0")]
unsafe extern "C" {
    /// Whether `mem` was allocated by the dmabuf allocator.
    fn gst_is_dmabuf_memory(mem: *mut gst::ffi::GstMemory) -> gst::glib::ffi::gboolean;
    /// The dmabuf fd behind `mem`. Owned by the memory — the caller must `dup`
    /// it to keep it past the buffer's lifetime.
    fn gst_dmabuf_memory_get_fd(mem: *mut gst::ffi::GstMemory) -> i32;
}

// ---------------------------------------------------------------------------
// Sample → Frame
// ---------------------------------------------------------------------------

/// Turn one appsink sample into a [`Frame`] in whatever memory it arrived in.
pub(crate) fn frame_from_sample(
    sample: &gst::Sample,
    acceptance: FrameAcceptance,
) -> Option<Frame> {
    let caps = sample.caps()?;
    let buffer = sample.buffer()?;
    let pts_ms = buffer.pts().map_or(0, gst::ClockTime::mseconds);

    // A dmabuf sample carries `memory:DMABuf` in its caps features and a
    // `DMA_DRM` format described by `drm-format`.
    if caps
        .features(0)
        .is_some_and(|f| f.contains("memory:DMABuf"))
    {
        if acceptance == FrameAcceptance::Native {
            #[cfg(unix)]
            if let Some(img) = dmabuf_from_sample(sample, caps) {
                return Some(Frame {
                    width: img.width,
                    height: img.height,
                    pts_ms,
                    payload: FramePayload::Dmabuf(Arc::new(img)),
                });
            }
            tracing::warn!("media: dmabuf sample could not be described; dropping frame");
            return None;
        }
        // An RGBA-only consumer cannot be handed a dmabuf, and mapping one
        // here would be a silent stall. Negotiation should have prevented
        // this; say so rather than pretending.
        tracing::warn!("media: dmabuf negotiated for an RGBA-only consumer; dropping frame");
        return None;
    }

    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let vframe = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
    let colorimetry = colorimetry_from_info(&info);

    match info.format() {
        gst_video::VideoFormat::Nv12 | gst_video::VideoFormat::P01010le => {
            let format = if info.format() == gst_video::VideoFormat::Nv12 {
                PlanarFormat::Nv12
            } else {
                PlanarFormat::P010
            };
            let planes = (0..2u32)
                .map(|i| {
                    let data = vframe.plane_data(i).ok()?;
                    // `VideoInfo` strides are signed — `GStreamer` spells a
                    // bottom-up image as a negative one. Reading that as a
                    // colossal unsigned row length is how a flipped frame
                    // turns into an out-of-range read downstream, so refuse
                    // the plane instead and let the frame be dropped.
                    let stride = *info.stride().get(i as usize)?;
                    let luma = i == 0;
                    Some(PlaneData {
                        bytes: Arc::new(data.to_vec()),
                        stride: u32::try_from(stride).ok()?,
                        // 4:2:0 chroma is half resolution in both axes.
                        width: if luma { info.width() } else { info.width() / 2 },
                        height: if luma {
                            info.height()
                        } else {
                            info.height() / 2
                        },
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(Frame {
                width: info.width(),
                height: info.height(),
                pts_ms,
                payload: FramePayload::Planar(Arc::new(PlanarImage {
                    format,
                    planes,
                    colorimetry,
                })),
            })
        }
        _ => {
            // Packed 8-bit formats: repack to tight RGBA, honouring stride.
            let rgba = packed_to_rgba(&vframe, &info)?;
            Some(Frame {
                width: info.width(),
                height: info.height(),
                pts_ms,
                payload: FramePayload::Rgba(Arc::new(rgba)),
            })
        }
    }
}

/// Repack a packed 8-bit sample (`RGBA` / `BGRA` / `BGRx` / `RGBx`) into tight
/// RGBA8.
fn packed_to_rgba(
    vframe: &gst_video::VideoFrameRef<&gst::BufferRef>,
    info: &gst_video::VideoInfo,
) -> Option<Vec<u8>> {
    let data = vframe.plane_data(0).ok()?;
    let (row, stride, height) = packed_row_geometry(
        info.width(),
        info.height(),
        *info.stride().first()?,
        data.len(),
    )?;
    if row == 0 || height == 0 {
        return Some(Vec::new());
    }

    // Source byte order. `BGRA`/`BGRx` carry blue first; everything else is
    // taken as already-RGB, which is what the caps this sink advertises can
    // negotiate. `x` formats have no alpha channel to read, so it is forced
    // opaque rather than copied from a byte that means nothing.
    let bgr = matches!(
        info.format(),
        gst_video::VideoFormat::Bgra | gst_video::VideoFormat::Bgrx
    );
    let opaque = matches!(
        info.format(),
        gst_video::VideoFormat::Rgbx | gst_video::VideoFormat::Bgrx
    );

    let mut out = vec![0u8; row.checked_mul(height)?];
    // `packed_row_geometry` proved `stride * height` fits the source buffer,
    // so zipping the two chunk iterators walks exactly `height` rows and no
    // offset is computed by hand.
    for (src_row, dst_row) in data.chunks_exact(stride).zip(out.chunks_exact_mut(row)) {
        // `stride >= row`, so the padding past the visible pixels is dropped
        // here rather than copied.
        let Some(src) = src_row.get(..row) else {
            continue;
        };
        if !bgr && !opaque {
            dst_row.copy_from_slice(src);
            continue;
        }
        for (s, d) in src.chunks_exact(4).zip(dst_row.chunks_exact_mut(4)) {
            let &[b0, b1, b2, b3] = s else { continue };
            let (r, g, b) = if bgr { (b2, b1, b0) } else { (b0, b1, b2) };
            d.copy_from_slice(&[r, g, b, if opaque { 255 } else { b3 }]);
        }
    }
    Some(out)
}

/// Row geometry for a packed repack: `(visible bytes per row, source stride,
/// rows)`, or `None` when the buffer cannot be walked forward row by row.
///
/// Split out from [`packed_to_rgba`] because it is the whole of the bounds
/// reasoning and needs no `GStreamer` buffer to test. It refuses three things:
///
/// * a **negative stride**, which `GStreamer` uses for a bottom-up image.
///   Reading the sign as magnitude (what `as usize` does) yields a row length
///   of billions, which sails past the `stride < row` check and indexes off
///   the end of the buffer;
/// * geometry whose byte counts do not fit `usize`, so the `stride * height`
///   bound cannot silently wrap and turn the length check into a no-op;
/// * a buffer shorter than the geometry claims.
fn packed_row_geometry(
    width: u32,
    height: u32,
    stride: i32,
    data_len: usize,
) -> Option<(usize, usize, usize)> {
    let width = width as usize;
    let height = height as usize;
    let stride = usize::try_from(stride).ok()?;
    let row = width.checked_mul(4)?;
    if stride < row || data_len < stride.checked_mul(height)? {
        return None;
    }
    Some((row, stride, height))
}

/// The colour convention the caps declare, defaulting to BT.709 limited —
/// what HD video carries when it says nothing.
fn colorimetry_from_info(info: &gst_video::VideoInfo) -> Colorimetry {
    let c = info.colorimetry();
    let matrix = match c.matrix() {
        gst_video::VideoColorMatrix::Bt601 => YuvMatrix::Bt601,
        gst_video::VideoColorMatrix::Bt2020 => YuvMatrix::Bt2020,
        gst_video::VideoColorMatrix::Rgb => YuvMatrix::Identity,
        // BT.709 is both a named matrix and the default: it is what HD video
        // carries when the caps decline to say.
        _ => YuvMatrix::Bt709,
    };
    let range = match c.range() {
        gst_video::VideoColorRange::Range0_255 => ColorRange::Full,
        _ => ColorRange::Limited,
    };
    Colorimetry { matrix, range }
}

/// Describe a `memory:DMABuf` sample: pull the fd out of each `GstMemory` and
/// read the plane layout from the buffer's `GstVideoMeta` (which the sink's
/// `propose_allocation` answer is what makes available).
#[cfg(unix)]
pub(crate) fn dmabuf_from_sample(sample: &gst::Sample, caps: &gst::CapsRef) -> Option<DmabufImage> {
    let buffer = sample.buffer()?;
    let drm = gst_video::VideoInfoDmaDrm::from_caps(caps).ok()?;

    // `DMA_DRM` caps describe an opaque buffer: the `VideoInfo` inside a
    // `VideoInfoDmaDrm` carries the *placeholder* DMA_DRM format, which has
    // zero planes and no strides. Translating it back to the real format
    // (NV12, P010, …) is what yields the plane count and layout — without
    // this the frame is described as having no planes at all, and every
    // import then fails for a reason that has nothing to do with the buffer.
    let resolved = drm.to_video_info().ok();
    let info: &gst_video::VideoInfo = resolved.as_ref().unwrap_or(&drm);
    let width = info.width();
    let height = info.height();
    let n_planes = info.n_planes() as usize;
    if n_planes == 0 {
        tracing::warn!(
            "media: dmabuf caps {} resolved to zero planes; cannot describe the frame",
            caps
        );
        return None;
    }

    // Plane geometry: prefer the buffer's VideoMeta (authoritative for the
    // actual allocation), fall back to the caps-derived info.
    //
    // Both sources are fallible per plane: a `VideoMeta` offset is a `usize`
    // and a stride is a signed `gint`, and a negative stride (a bottom-up
    // image) or an offset past 4 GiB describes a layout an importer cannot
    // bind. Those planes drop out here, and the length check below turns a
    // partial answer into "cannot describe the frame" rather than a plane
    // list that is silently short.
    let meta = buffer.meta::<gst_video::VideoMeta>();
    let (offsets, strides): (Vec<u32>, Vec<u32>) = match &meta {
        Some(m) => (
            m.offset()
                .iter()
                .take(n_planes)
                .filter_map(|o| u32::try_from(*o).ok())
                .collect(),
            m.stride()
                .iter()
                .take(n_planes)
                .filter_map(|s| u32::try_from(*s).ok())
                .collect(),
        ),
        None => (
            info.offset()
                .iter()
                .take(n_planes)
                .filter_map(|o| u32::try_from(*o).ok())
                .collect(),
            info.stride()
                .iter()
                .take(n_planes)
                .filter_map(|s| u32::try_from(*s).ok())
                .collect(),
        ),
    };
    if offsets.len() < n_planes || strides.len() < n_planes {
        tracing::warn!("media: dmabuf plane geometry incomplete; cannot describe frame");
        return None;
    }

    // One GstMemory may back several planes (offsets into one fd), or each
    // plane may have its own. Map plane → memory by GStreamer's own rule.
    let n_mem = buffer.n_memory();
    // A buffer with no memory at all (a GAP buffer, say) describes no picture.
    // Saying so here is what keeps the plane→memory mapping below from
    // computing `n_mem - 1` on zero.
    let Some(last_mem) = n_mem.checked_sub(1) else {
        tracing::warn!("media: dmabuf sample carries no memory; cannot describe frame");
        return None;
    };
    let mut planes = Vec::with_capacity(n_planes);
    for (i, (offset, stride)) in offsets.iter().zip(&strides).enumerate() {
        // One `GstMemory` may back several planes at different offsets, or
        // each plane may carry its own; clamping is `GStreamer`'s own rule for
        // the first case.
        let mem_idx = i.min(last_mem);
        let mem = buffer.peek_memory(mem_idx);
        // Safety: `mem` is a live `&MemoryRef` borrowed from the buffer for
        // the duration of this call; both C entry points only read from it.
        let raw = mem.as_ptr().cast_mut();
        let is_dmabuf = unsafe { gst_is_dmabuf_memory(raw) } != 0;
        if !is_dmabuf {
            tracing::warn!("media: caps claimed memory:DMABuf but memory {mem_idx} is not one");
            return None;
        }
        let raw_fd = unsafe { gst_dmabuf_memory_get_fd(raw) };
        if raw_fd < 0 {
            tracing::warn!("media: dmabuf memory {mem_idx} has no fd");
            return None;
        }
        // The fd belongs to the GstMemory, which is released when the sample
        // drops. Duplicate it so the frame can outlive the sample.
        let borrowed = unsafe { std::os::fd::BorrowedFd::borrow_raw(raw_fd) };
        let fd = borrowed.try_clone_to_owned().ok()?;
        planes.push(DmabufPlane {
            fd,
            offset: *offset,
            stride: *stride,
        });
    }

    Some(DmabufImage {
        planes,
        fourcc: drm.fourcc(),
        // GStreamer reports a linear buffer as modifier 0, which is exactly
        // DRM_FORMAT_MOD_LINEAR — keep it rather than reinterpreting it as
        // "unstated", or a genuinely linear buffer becomes indistinguishable
        // from one whose producer declined to say.
        modifier: drm.modifier(),
        width,
        height,
        colorimetry: colorimetry_from_info(info),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffers_that_cannot_supply_their_claimed_rows_are_refused() {
        // Phase `a_negative_stride_is_refused_rather_than_read_as_a_huge_row`.
        {
            // `GStreamer` spells a bottom-up image as a negative stride. Reading
            // the sign as magnitude — what `stride as usize` does — turns -4096
            // into 18 446 744 073 709 547 520, which sails past a `stride < row`
            // check and then indexes billions of bytes off the end of the buffer.
            // This repacker only walks rows forward, so it cannot describe the
            // frame.
            assert_eq!(
                packed_row_geometry(64, 4, -256, 4096),
                None,
                "a bottom-up buffer must be refused, not reinterpreted"
            );
            // The same geometry with the stride the right way up is fine.
            assert_eq!(
                packed_row_geometry(64, 4, 256, 4096),
                Some((256, 256, 4)),
                "a normal top-down buffer still works"
            );
        }
        // Phase `packed_geometry_refuses_a_buffer_that_cannot_supply_the_rows_claimed`.
        {
            // A buffer one byte short of four rows cannot supply four rows.
            assert_eq!(
                packed_row_geometry(64, 4, 256, 1023),
                None,
                "a short buffer is refused rather than read past its end"
            );
            // A stride narrower than the visible row is not padding, it is wrong.
            assert_eq!(packed_row_geometry(64, 4, 128, 4096), None, "stride < row");
            // Padding past the visible pixels is normal and must be accepted.
            assert_eq!(
                packed_row_geometry(60, 4, 256, 1024),
                Some((240, 256, 4)),
                "a padded row is accepted, and the padding is not part of the row"
            );
        }
    }
}
