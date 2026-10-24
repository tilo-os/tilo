//! Sub-rectangle copies of the combined-scene readback into per-output scanout buffers.

use super::BridgeError;

/// The source half of a sub-rectangle copy/swizzle: the combined-scene RGBA buffer, its full
/// width/height, and the sub-rect's top-left origin within it. The three free functions below
/// share this shape; grouping it keeps each function under clippy's argument-count limit.
#[derive(Clone, Copy)]
pub struct SubrectSource<'a> {
    pub src_rgba: &'a [u8],
    pub src_w: usize,
    pub src_h: usize,
    pub src_x: usize,
    pub src_y: usize,
}

/// Swizzle a sub-rectangle of a tight-packed RGBA source buffer (`[R, G, B, A]`, row stride
/// `src_w * 4`) into an XRGB8888 destination (`[B, G, R, X]`, row stride `dst_pitch`).
///
/// The per-output present primitive for multi-monitor: the compositor renders one scene
/// spanning the combined output geometry into `source.src_rgba` (via [`super::VelloDrmBridge::paint`]),
/// then each output presents the sub-rect at its `Output.position`/`size` into its own scanout
/// dumb buffer. `source.src_x`/`source.src_y` is the sub-rect's top-left in the source; `out_w`×`out_h`
/// is the output's size.
///
/// Rows/cols outside the source or destination bounds are skipped (clamped, not panicking). For
/// a single output at the origin matching the combined geometry, this is byte-for-byte [`super::VelloDrmBridge::paint_into_xrgb8888`].
pub fn swizzle_subrect_rgba_to_xrgb8888(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    out_w: usize,
    out_h: usize,
) {
    swizzle_subrect_rgba_to_xrgb8888_transformed(
        source,
        dst,
        dst_pitch,
        out_w,
        out_h,
        settings_store::hardware::OutputTransform::Normal,
    );
}

/// Rotation-aware sibling of [`swizzle_subrect_rgba_to_xrgb8888`]. Source
/// coordinates are in the post-transform logical plane; destination pixels
/// are in the physical KMS mode. This makes the scene, cursor, touch mapping,
/// and `wl_output` transform describe one geometry.
pub fn swizzle_subrect_rgba_to_xrgb8888_transformed(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    physical_w: usize,
    physical_h: usize,
    transform: settings_store::hardware::OutputTransform,
) {
    write_subrect_transformed(
        source,
        dst,
        dst_pitch,
        (physical_w, physical_h),
        transform,
        |dpix, spix, _, _| {
            if let ([db, dg, dr, dx], [sr, sg, sb, _sa]) = (dpix, spix) {
                *db = *sb;
                *dg = *sg;
                *dr = *sr;
                *dx = 0xFF;
            }
        },
    );
}

/// The HDR layers an output composites on the CPU, with the scene position
/// of the source buffer's origin.
pub type SubrectComposite<'a> = (crate::shell::hdr_surface::CpuComposite<'a>, (usize, usize));

/// A scene pixel HDR layers can reach, as the CPU composite needs it.
struct CompositedPixel<'a> {
    layers: crate::shell::hdr_surface::CpuComposite<'a>,
    first: [u8; 4],
    second: [u8; 4],
    x: u32,
    y: u32,
}

pub(super) fn tight_clip_rows(
    mapped: &[u8],
    padded: u32,
    clip: vello::FineDamageRect,
) -> Result<Vec<u8>, BridgeError> {
    let row = usize::try_from(clip.width).unwrap_or(0).saturating_mul(4);
    let stride = usize::try_from(padded).unwrap_or(0);
    let rows = usize::try_from(clip.height).unwrap_or(0);
    if row == 0 || stride < row || rows == 0 {
        return Err(BridgeError::InvalidSize(
            "clip readback row does not fit".to_owned(),
        ));
    }
    let mut tight = Vec::with_capacity(row.saturating_mul(rows));
    for row_index in 0..rows {
        let start = row_index.saturating_mul(stride);
        let Some(src) = mapped.get(start..start.saturating_add(row)) else {
            return Err(BridgeError::Map(
                "clip readback row is outside the mapped buffer".to_owned(),
            ));
        };
        tight.extend_from_slice(src);
    }
    Ok(tight)
}

impl CompositedPixel<'_> {
    fn light(&self, target: crate::shell::hdr_surface::CompositeTarget) -> [f32; 3] {
        crate::shell::hdr_surface::composite_hdr_pixel(
            self.first,
            self.second,
            self.x,
            self.y,
            self.layers.layers,
            target,
        )
    }
}

/// The scene pixel under source pixel `(sx, sy)`, when it is one HDR layers
/// can reach.
fn composited_source<'a>(
    composite: Option<SubrectComposite<'a>>,
    spix: &[u8],
    sx: usize,
    sy: usize,
) -> Option<CompositedPixel<'a>> {
    let (layers, (origin_x, origin_y)) = composite?;
    let x = u32::try_from(sx.saturating_add(origin_x)).ok()?;
    let y = u32::try_from(sy.saturating_add(origin_y)).ok()?;
    if !layers.touches(x, y) {
        return None;
    }
    Some(CompositedPixel {
        layers,
        first: spix.try_into().ok()?,
        second: layers.second_at(x, y),
        x,
        y,
    })
}

/// [`swizzle_subrect_rgba_to_xrgb8888_transformed`] for an SDR output while
/// HDR layers are composited: pixels they reach get them tone-mapped.
pub fn swizzle_subrect_rgba_to_xrgb8888_composited(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    physical_size: (usize, usize),
    transform: settings_store::hardware::OutputTransform,
    composite: Option<SubrectComposite<'_>>,
) {
    write_subrect_transformed(
        source,
        dst,
        dst_pitch,
        physical_size,
        transform,
        |dpix, spix, sx, sy| {
            let [sr, sg, sb] = match composited_source(composite, spix, sx, sy) {
                Some(pixel) => crate::shell::hdr_surface::encode_srgb8(
                    pixel.light(crate::shell::hdr_surface::CompositeTarget::Sdr),
                ),
                None => match spix {
                    [r, g, b, _] => [*r, *g, *b],
                    _ => return,
                },
            };
            if let [db, dg, dr, dx] = dpix {
                *db = sb;
                *dg = sg;
                *dr = sr;
                *dx = 0xFF;
            }
        },
    );
}

/// [`swizzle_subrect_rgba_to_xrgb8888_transformed`] for an output driven in HDR: each sRGB
/// pixel becomes one little-endian `XBGR2101010` word of PQ BT.2020 signal, and pixels HDR
/// layers reach get them composited at full precision.
pub fn encode_subrect_rgba_to_xbgr2101010_transformed(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    physical_size: (usize, usize),
    transform: settings_store::hardware::OutputTransform,
    encoder: &crate::shell::color_pipeline::SdrToPqEncoder,
    composite: Option<SubrectComposite<'_>>,
) {
    let target = crate::shell::hdr_surface::CompositeTarget::Pq {
        sdr_white_nits: encoder.sdr_white_nits(),
    };
    write_subrect_transformed(
        source,
        dst,
        dst_pitch,
        physical_size,
        transform,
        |dpix, spix, sx, sy| {
            let word = match composited_source(composite, spix, sx, sy) {
                Some(pixel) => {
                    crate::shell::hdr_surface::encode_pq_xbgr2101010(pixel.light(target))
                }
                None => match *spix {
                    [r, g, b, _] => encoder.encode_xbgr2101010([r, g, b]),
                    _ => return,
                },
            };
            dpix.copy_from_slice(&word.to_le_bytes());
        },
    );
}

/// Walk one output's physical pixels, find the combined-scene source pixel under each through
/// `transform`, and let `write` produce the 4-byte destination pixel from the RGBA source.
fn write_subrect_transformed(
    source: SubrectSource<'_>,
    dst: &mut [u8],
    dst_pitch: usize,
    (physical_w, physical_h): (usize, usize),
    transform: settings_store::hardware::OutputTransform,
    write: impl Fn(&mut [u8], &[u8], usize, usize),
) {
    let SubrectSource {
        src_rgba,
        src_w,
        src_h,
        src_x,
        src_y,
    } = source;
    if dst_pitch == 0 {
        return;
    }
    let max_rows = dst
        .len()
        .checked_div(dst_pitch)
        .unwrap_or(0)
        .min(physical_h);
    let cols = (dst_pitch / 4).min(physical_w);
    let (logical_w, logical_h) = match transform {
        settings_store::hardware::OutputTransform::Rotate90
        | settings_store::hardware::OutputTransform::Rotate270 => (physical_h, physical_w),
        _ => (physical_w, physical_h),
    };
    let logical = |col: usize, row: usize| match transform {
        settings_store::hardware::OutputTransform::Normal => (col, row),
        settings_store::hardware::OutputTransform::Rotate90 => {
            (row, logical_h.saturating_sub(1).saturating_sub(col))
        }
        settings_store::hardware::OutputTransform::Rotate180 => (
            logical_w.saturating_sub(1).saturating_sub(col),
            logical_h.saturating_sub(1).saturating_sub(row),
        ),
        settings_store::hardware::OutputTransform::Rotate270 => {
            (logical_w.saturating_sub(1).saturating_sub(row), col)
        }
    };
    for row in 0..max_rows {
        let (_, logical_y) = logical(0, row);
        let sy_base = src_y.saturating_add(logical_y);
        if sy_base >= src_h
            && matches!(transform, settings_store::hardware::OutputTransform::Normal)
        {
            break;
        }
        let d_start = row.saturating_mul(dst_pitch);
        let d_end = d_start.saturating_add(cols.saturating_mul(4));
        let Some(drow) = dst.get_mut(d_start..d_end) else {
            break;
        };
        for (col, dpix) in drow.chunks_exact_mut(4).enumerate() {
            let (logical_x, logical_y) = logical(col, row);
            let sx = src_x.saturating_add(logical_x);
            let sy = src_y.saturating_add(logical_y);
            if sx >= src_w || sy >= src_h {
                continue;
            }
            let Some(s) = sy
                .checked_mul(src_w)
                .and_then(|v| v.checked_add(sx))
                .and_then(|v| v.checked_mul(4))
            else {
                continue;
            };
            let Some(spix) = src_rgba.get(s..s.saturating_add(4)) else {
                continue;
            };
            write(dpix, spix, sx, sy);
        }
    }
}

/// Copy one logical output rectangle out of the combined RGBA scene. Used
/// only when a qcms ICC transform is active; the common unprofiled path keeps
/// the zero-extra-copy swizzle above.
pub fn copy_rgba_subrect(
    source: SubrectSource<'_>,
    width: usize,
    height: usize,
    dst: &mut Vec<u8>,
) {
    let SubrectSource {
        src_rgba: src,
        src_w,
        src_h,
        src_x,
        src_y,
    } = source;
    dst.clear();
    dst.resize(width.saturating_mul(height).saturating_mul(4), 0);
    for row in 0..height {
        let Some(src_row) = src_y.checked_add(row) else {
            break;
        };
        if src_row >= src_h || src_x >= src_w {
            break;
        }
        let cols = width.min(src_w.saturating_sub(src_x));
        let Some(byte_offset) = src_row
            .checked_mul(src_w)
            .and_then(|v| v.checked_add(src_x))
            .and_then(|v| v.checked_mul(4))
        else {
            break;
        };
        let target = row.saturating_mul(width).saturating_mul(4);
        let bytes = cols.saturating_mul(4);
        let source_end = byte_offset.saturating_add(bytes);
        let target_end = target.saturating_add(bytes);
        if let (Some(dst_slice), Some(src_slice)) = (
            dst.get_mut(target..target_end),
            src.get(byte_offset..source_end),
        ) && dst_slice.len() == src_slice.len()
        {
            dst_slice.copy_from_slice(src_slice);
        }
    }
}

#[cfg(test)]
mod tests {
    //! Pure-CPU tests for the sub-rect swizzle — no wgpu/GPU needed, run everywhere `cargo test -p shell --lib` runs.
    use super::{
        SubrectSource, copy_rgba_subrect, swizzle_subrect_rgba_to_xrgb8888,
        swizzle_subrect_rgba_to_xrgb8888_transformed,
    };

    /// Build a `w×h` tight RGBA buffer where pixel `(x,y)` is `(x, y, 0, 0xFF)`, so the test can identify which source pixel landed where after the swizzle.
    fn ramp_rgba(w: usize, h: usize) -> Vec<u8> {
        let mut v = vec![0u8; w.saturating_mul(h).saturating_mul(4)];
        for y in 0..h {
            for x in 0..w {
                let o = y.saturating_mul(w).saturating_add(x).saturating_mul(4);
                v[o] = u8::try_from(x).unwrap_or(u8::MAX);
                v[o.saturating_add(1)] = u8::try_from(y).unwrap_or(u8::MAX);
                v[o.saturating_add(2)] = 0;
                v[o.saturating_add(3)] = 0xFF;
            }
        }
        v
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn subrect_swizzle_copies_exactly_the_output_viewport() {
        // Phase `whole_buffer_swizzles_like_paint_into_xrgb8888`.
        {
            // A single output at the origin: src_x=src_y=0, out size == src size. Every pixel swizzles R,G,B -> B,G,R,0xFF.
            let (w, h) = (4usize, 3usize);
            let src = ramp_rgba(w, h);
            let pitch = w * 4;
            let mut dst = vec![0u8; pitch * h];
            swizzle_subrect_rgba_to_xrgb8888(
                SubrectSource {
                    src_rgba: &src,
                    src_w: w,
                    src_h: h,
                    src_x: 0,
                    src_y: 0,
                },
                &mut dst,
                pitch,
                w,
                h,
            );
            for y in 0..h {
                for x in 0..w {
                    let s = (y * w + x) * 4;
                    let d = y * pitch + x * 4;
                    assert_eq!(dst[d], src[s + 2], "B at ({x},{y})");
                    assert_eq!(dst[d + 1], src[s + 1], "G at ({x},{y})");
                    assert_eq!(dst[d + 2], src[s], "R at ({x},{y})");
                    assert_eq!(dst[d + 3], 0xFF, "X at ({x},{y})");
                }
            }
        }

        {
            // Combined scene 6x2; second output is a 2x2 region at global position (4,0) — the swizzle must pull source columns 4 and 5, not 0 and 1.
            let (cw, ch) = (6usize, 2usize);
            let src = ramp_rgba(cw, ch);
            let (ow, oh) = (2usize, 2usize);
            let pitch = ow * 4;
            let mut dst = vec![0u8; pitch * oh];
            swizzle_subrect_rgba_to_xrgb8888(
                SubrectSource {
                    src_rgba: &src,
                    src_w: cw,
                    src_h: ch,
                    src_x: 4,
                    src_y: 0,
                },
                &mut dst,
                pitch,
                ow,
                oh,
            );
            // dst pixel (0,0) must be source pixel (4,0): R channel == 4.
            assert_eq!(dst[2], 4, "dst(0,0).R should be source column 4");
            // dst pixel (1,1) must be source pixel (5,1): R==5, G==1.
            let d = pitch + 4;
            assert_eq!(dst[d + 2], 5, "dst(1,1).R should be source column 5");
            assert_eq!(dst[d + 1], 1, "dst(1,1).G should be source row 1");
        }

        {
            // A DRM dumb buffer's pitch usually exceeds out_w*4; the trailing padding bytes must stay as they were.
            let (w, h) = (2usize, 2usize);
            let src = ramp_rgba(w, h);
            let pitch = 16;
            let mut dst = vec![0xEEu8; pitch * h];
            swizzle_subrect_rgba_to_xrgb8888(
                SubrectSource {
                    src_rgba: &src,
                    src_w: w,
                    src_h: h,
                    src_x: 0,
                    src_y: 0,
                },
                &mut dst,
                pitch,
                w,
                h,
            );
            for row in 0..h {
                // Padding after the 2 written pixels (offset 8..) stays 0xEE.
                assert_eq!(dst[row * pitch + 8], 0xEE, "row {row} padding untouched");
            }
        }

        {
            // Output wider/taller than the source region remaining: must clamp.
            let (w, h) = (3usize, 3usize);
            let src = ramp_rgba(w, h);
            let (ow, oh) = (4usize, 4usize);
            let pitch = ow * 4;
            let mut dst = vec![0u8; pitch * oh];
            // src_x=2 leaves only 1 valid source column; must not panic.
            swizzle_subrect_rgba_to_xrgb8888(
                SubrectSource {
                    src_rgba: &src,
                    src_w: w,
                    src_h: h,
                    src_x: 2,
                    src_y: 0,
                },
                &mut dst,
                pitch,
                ow,
                oh,
            );
            // Column 0 maps to source column 2 (valid); R==2.
            assert_eq!(dst[2], 2);
        }

        // Phase `rotate_90_maps_portrait_scene_to_landscape_scanout`.
        {
            let src = ramp_rgba(2, 3);
            let mut dst = vec![0u8; 3 * 2 * 4];
            swizzle_subrect_rgba_to_xrgb8888_transformed(
                SubrectSource {
                    src_rgba: &src,
                    src_w: 2,
                    src_h: 3,
                    src_x: 0,
                    src_y: 0,
                },
                &mut dst,
                3 * 4,
                3,
                2,
                settings_store::hardware::OutputTransform::Rotate90,
            );
            let top_right = 2 * 4;
            assert_eq!((dst[top_right + 2], dst[top_right + 1]), (0, 0));
            let bottom_left = 3 * 4;
            assert_eq!((dst[bottom_left + 2], dst[bottom_left + 1]), (1, 2));
        }

        {
            let src = ramp_rgba(5, 2);
            let mut output = Vec::new();
            copy_rgba_subrect(
                SubrectSource {
                    src_rgba: &src,
                    src_w: 5,
                    src_h: 2,
                    src_x: 3,
                    src_y: 0,
                },
                2,
                2,
                &mut output,
            );
            assert_eq!(output.len(), 16);
            assert_eq!(output[0], 3);
            assert_eq!(output[4], 4);
            assert_eq!(output[9], 1);
        }
    }
}
