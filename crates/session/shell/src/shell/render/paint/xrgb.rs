//! Painting damage into XRGB8888 scanout buffers: damage normalisation, destination
//! histories, and the mapped-staging swizzle.

use std::{sync::atomic::Ordering, time::Instant};

use vello::Scene;

use super::{
    BridgeError, BridgePhaseTiming, MAX_DAMAGE_RECTS, MappedXrgbSource, SceneDamage,
    VelloDrmBridge, WARNED_CPU_FALLBACK, XrgbCpuWriteMode, XrgbHistoryTransaction, XrgbWriteResult,
    coalesce_damage, damage_area, damage_is_profitable,
};
use crate::shell::render::xrgb_history::XrgbWritePlan;

impl VelloDrmBridge {
    /// Render a versioned scene into an XRGB8888 destination, retaining the GPU target and its
    /// padded staging buffer so a later compatible frame can update only its damaged pixels.
    ///
    /// This is deliberately stricter than an ordinary "damage hint": every condition that could
    /// make an untouched pixel ambiguous (first frame, revision gap, resize/base-colour change,
    /// unsupported sparse shaders, invalid/empty/large damage, or any failed attempt) selects
    /// the dense path. Regardless of the GPU copy size, every call swizzles the complete retained
    /// staging image into `dst`, so callers may rotate DRM dumb buffers without backbuffer-age
    /// ghosts.
    ///
    /// # Errors
    /// Returns [`BridgeError`] when Vello cannot render or settle a complete frame, or when the
    /// same bounded texture-readback mapping used by [`Self::paint_into_xrgb8888`] fails.
    pub fn paint_damage_into_xrgb8888(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<(), BridgeError> {
        let _ = self.paint_damage_into_xrgb8888_core(
            scene,
            damage,
            dst,
            dst_pitch,
            XrgbWritePlan::Full,
            false,
        )?;
        Ok(())
    }

    /// Render a versioned scene through one staged destination-history transaction.
    ///
    /// The transaction owns validation of destination identity, revision continuity, and bounded
    /// write rectangles. Its private plan is deliberately inaccessible to arbitrary callers.
    /// Call [`XrgbHistoryTransaction::commit`] only after any scanout-owned software cursor has
    /// been written; drop or abort it on every error.
    #[doc(hidden)]
    pub fn paint_damage_into_xrgb8888_with_history(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        transaction: &XrgbHistoryTransaction<'_>,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<XrgbWriteResult, BridgeError> {
        if !transaction.matches_frame(damage.revision, damage.previous_revision, damage.regions) {
            return Err(BridgeError::XrgbHistoryMismatch);
        }
        let expected_identity = self.xrgb_history_identity(
            dst_pitch,
            transaction
                .prepared_identity()
                .map_or(1, |identity| identity.targets),
        );
        // The scanout adds its own epoch to the transaction identity. It is deliberately not a
        // bridge property, so compare only the renderer-owned contract here; the history itself
        // retains and compares the complete identity on commit.
        let identity_matches = matches!(
            (expected_identity, transaction.prepared_identity()),
            (Some(expected), Some(prepared))
                if expected.bridge_generation == prepared.bridge_generation
                    && expected.bridge_epoch == prepared.bridge_epoch
                    && expected.width == prepared.width
                    && expected.height == prepared.height
                    && expected.pitch == prepared.pitch
                    && expected.targets == prepared.targets
        );
        if !identity_matches {
            return Err(BridgeError::XrgbHistoryMismatch);
        }
        self.paint_damage_into_xrgb8888_core(
            scene,
            damage,
            dst,
            dst_pitch,
            transaction.write_plan(),
            true,
        )
    }

    fn paint_damage_into_xrgb8888_core(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        dst: &mut [u8],
        dst_pitch: usize,
        requested_plan: XrgbWritePlan<'_>,
        require_complete_destination: bool,
    ) -> Result<XrgbWriteResult, BridgeError> {
        if require_complete_destination {
            Self::validate_complete_xrgb_destination(dst, dst_pitch, self.target.size)?;
        }
        self.check_recovery_backoff()?;
        self.last_phase_timing = None;
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (retained sparse damage when version-safe); zero-copy DMA-BUF interop remains \
                 a follow-up."
            );
        }

        let (width, height) = self.target.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        let (mapped, damage_admitted) =
            match self.render_normal(scene, Some(damage), timing.as_mut()) {
                Ok(result) => result,
                Err(error) => {
                    self.note_render_failure(&error);
                    return Err(error);
                }
            };

        let write_plan = if damage_admitted {
            requested_plan
        } else {
            XrgbWritePlan::Full
        };
        let cpu_copy_started = self.phase_timing_enabled.then(Instant::now);
        #[cfg(test)]
        let mapped_bytes = if std::mem::take(&mut self.fail_next_tracked_swizzle) {
            mapped
                .get(..mapped.len().saturating_sub(1))
                .unwrap_or_default()
        } else {
            &mapped
        };
        #[cfg(not(test))]
        let mapped_bytes = &mapped;
        let swizzle_result = if dst_pitch == 0 {
            Ok((0, XrgbCpuWriteMode::Full))
        } else {
            Self::swizzle_mapped_xrgb8888_with_plan(
                MappedXrgbSource {
                    bytes: mapped_bytes,
                    padded_bytes_per_row: padded_bytes_per_row as usize,
                    width: width as usize,
                    height: height as usize,
                },
                dst,
                dst_pitch,
                self.phase_timing_enabled,
                write_plan,
                require_complete_destination,
            )
        };
        drop(mapped);
        self.readback_buf.unmap();
        let (cpu_output_bytes, mode) = swizzle_result?;
        if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
            timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            timing.cpu_output_bytes = cpu_output_bytes;
        }
        self.last_phase_timing = timing;
        self.mark_retained_complete(Some(damage.revision));
        Ok(XrgbWriteResult {
            mode,
            cpu_output_bytes,
            damage_admitted,
        })
    }

    #[cfg(test)]
    pub(super) fn inject_next_tracked_swizzle_failure_for_test(&mut self) {
        self.fail_next_tracked_swizzle = true;
    }

    /// Normalize exact native rectangles without ever expanding their covered area. Overlapping
    /// rectangles intentionally stay separate unless one contains the other: bounding-box merges
    /// turn an L-shaped top bar plus popover into a large false damage band. Vello's sparse tile
    /// masks already deduplicate the raster work, while the copies below account for their actual
    /// (possibly overlapping) payload. A sparse dispatch with [`MAX_DAMAGE_RECTS`] distinct
    /// regions is first coalesced ([`coalesce_damage`]) and, with the output
    /// [`damage_is_profitable`] no longer, remains a deliberate full-frame boundary. What counts
    /// there is the pixels the rectangles cover, each once: overlapping damage repaints its
    /// union, not its sum.
    pub(super) fn normalize_damage(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        if damage.is_empty() || !damage.iter().all(|&rect| Self::inside(rect, size)) {
            return None;
        }
        let (width, height) = size;
        // More regions than one dispatch takes are merged, never refused: a repaint of a little
        // more than the damage is still far cheaper than a dense frame. Coalescing drops the
        // rectangles another contains on the way.
        let normalized = coalesce_damage(damage.to_vec(), MAX_DAMAGE_RECTS);
        let dirty_pixels = damage_area(&normalized);
        let full_pixels = u64::from(width).saturating_mul(u64::from(height));
        if !damage_is_profitable(dirty_pixels, full_pixels) {
            return None;
        }
        Some(normalized)
    }

    /// `damage` inside a `size` target, each rect no other holds: `None`
    /// when there is none, too many, or one leaves the target.
    pub(super) fn normalize_rects(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        if damage.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        Self::normalize_unbounded(damage, size)
    }

    /// [`Self::normalize_rects`] without the bound on how many there may be.
    fn normalize_unbounded(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        if damage.is_empty() {
            return None;
        }
        let mut normalized = Vec::with_capacity(damage.len());
        for &rect in damage {
            if !Self::inside(rect, size) {
                return None;
            }

            // Do not bounding-box partial overlap or touching edges. It is cheap for Vello's
            // masks to union them, but expensive for our texture→staging copies when the union
            // spans unrelated empty pixels.
            let mut contained = false;
            let mut index = 0;
            while let Some(existing) = normalized.get(index).copied() {
                if Self::damage_rect_contains(existing, rect) {
                    contained = true;
                    break;
                }
                if Self::damage_rect_contains(rect, existing) {
                    normalized.swap_remove(index);
                } else {
                    index = index.saturating_add(1);
                }
            }
            if !contained {
                normalized.push(rect);
            }
        }
        Some(normalized)
    }

    /// `rect` is non-empty and lies wholly inside a `size` target.
    fn inside(rect: vello::FineDamageRect, (width, height): (u32, u32)) -> bool {
        let fits = |start: u32, length: u32, limit: u32| {
            length != 0 && start.checked_add(length).is_some_and(|end| end <= limit)
        };
        fits(rect.x, rect.width, width) && fits(rect.y, rect.height, height)
    }

    #[inline]
    fn damage_rect_contains(outer: vello::FineDamageRect, inner: vello::FineDamageRect) -> bool {
        let outer_right = outer.x.saturating_add(outer.width);
        let outer_bottom = outer.y.saturating_add(outer.height);
        let inner_right = inner.x.saturating_add(inner.width);
        let inner_bottom = inner.y.saturating_add(inner.height);
        outer.x <= inner.x
            && outer.y <= inner.y
            && outer_right >= inner_right
            && outer_bottom >= inner_bottom
    }

    /// Swizzle mapped RGBA into XRGB. The ordinary branch deliberately has no per-row
    /// diagnostics, while the diagnostic branch counts only destination bytes actually written.
    pub(super) fn swizzle_mapped_xrgb8888(
        source: MappedXrgbSource<'_>,
        dst: &mut [u8],
        dst_pitch: usize,
        count_output_bytes: bool,
    ) -> u64 {
        let MappedXrgbSource {
            bytes: mapped,
            padded_bytes_per_row,
            width,
            height,
        } = source;
        let tight_row = width.saturating_mul(4);
        let max_rows = dst.len().checked_div(dst_pitch).unwrap_or(0).min(height);
        let cols = (dst_pitch / 4).min(width);
        if count_output_bytes {
            let mut bytes = 0_u64;
            for row in 0..max_rows {
                let s_start = row.saturating_mul(padded_bytes_per_row);
                let s_end = s_start.saturating_add(tight_row);
                let Some(srow) = mapped.get(s_start..s_end) else {
                    tracing::warn!(
                        "DRM paint: readback row {row} out of the mapped buffer's bounds; stopping swizzle early"
                    );
                    break;
                };
                let d_start = row.saturating_mul(dst_pitch);
                let d_end = d_start.saturating_add(cols.saturating_mul(4));
                let Some(drow) = dst.get_mut(d_start..d_end) else {
                    break;
                };
                Self::swizzle_xrgb_row(drow, srow);
                bytes = bytes.saturating_add(u64::try_from(drow.len()).unwrap_or(u64::MAX));
            }
            bytes
        } else {
            for row in 0..max_rows {
                let s_start = row.saturating_mul(padded_bytes_per_row);
                let s_end = s_start.saturating_add(tight_row);
                let Some(srow) = mapped.get(s_start..s_end) else {
                    tracing::warn!(
                        "DRM paint: readback row {row} out of the mapped buffer's bounds; stopping swizzle early"
                    );
                    break;
                };
                let d_start = row.saturating_mul(dst_pitch);
                let d_end = d_start.saturating_add(cols.saturating_mul(4));
                let Some(drow) = dst.get_mut(d_start..d_end) else {
                    break;
                };
                Self::swizzle_xrgb_row(drow, srow);
            }
            0
        }
    }

    pub(super) fn validate_complete_xrgb_destination(
        dst: &[u8],
        dst_pitch: usize,
        size: (u32, u32),
    ) -> Result<(), BridgeError> {
        let (width, height) = size;
        let tight = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| BridgeError::InvalidSize("XRGB row stride overflow".to_owned()))?;
        let height = usize::try_from(height)
            .map_err(|_| BridgeError::InvalidSize("XRGB height does not fit usize".to_owned()))?;
        let needed = dst_pitch.checked_mul(height);
        if dst_pitch < tight || needed.is_none_or(|needed| dst.len() < needed) {
            return Err(BridgeError::InvalidSize(format!(
                "XRGB destination is incomplete: pitch={dst_pitch} len={} for {width}x{height}",
                dst.len()
            )));
        }
        Ok(())
    }

    #[allow(
        clippy::expect_used,
        reason = "the complete bounded plan, every source range, and every destination range are validated before its first byte is written"
    )]
    pub(super) fn swizzle_mapped_xrgb8888_with_plan(
        source: MappedXrgbSource<'_>,
        dst: &mut [u8],
        dst_pitch: usize,
        count_output_bytes: bool,
        plan: XrgbWritePlan<'_>,
        require_complete_source: bool,
    ) -> Result<(u64, XrgbCpuWriteMode), BridgeError> {
        match plan {
            XrgbWritePlan::Full => {
                if require_complete_source {
                    Self::validate_complete_mapped_xrgb_source(source)?;
                }
                let bytes =
                    Self::swizzle_mapped_xrgb8888(source, dst, dst_pitch, count_output_bytes);
                let full_bytes = u64::try_from(source.width)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(u64::try_from(source.height).unwrap_or(u64::MAX))
                    .saturating_mul(4);
                Ok((
                    if count_output_bytes {
                        bytes
                    } else {
                        full_bytes
                    },
                    XrgbCpuWriteMode::Full,
                ))
            }
            XrgbWritePlan::Rects(rects) => {
                // Validate the entire bounded plan before writing one byte. A live dumb buffer
                // cannot be rolled back if a later rectangle turns out to be malformed.
                Self::validate_complete_mapped_xrgb_source(source)?;
                Self::validate_xrgb_rect_plan(source, dst, dst_pitch, rects)?;
                let mut bytes = 0_u64;
                for &rect in rects {
                    let x = usize::try_from(rect.x).unwrap_or(usize::MAX);
                    let y = usize::try_from(rect.y).unwrap_or(usize::MAX);
                    let width = usize::try_from(rect.width).unwrap_or(usize::MAX);
                    let height = usize::try_from(rect.height).unwrap_or(usize::MAX);
                    let bottom = y
                        .checked_add(height)
                        .expect("partial plan was validated before writing");
                    let row_bytes = width
                        .checked_mul(4)
                        .expect("partial plan was validated before writing");
                    let x_bytes = x
                        .checked_mul(4)
                        .expect("partial plan was validated before writing");
                    for row in y..bottom {
                        let src_start = row
                            .checked_mul(source.padded_bytes_per_row)
                            .and_then(|offset| offset.checked_add(x_bytes))
                            .expect("partial plan was validated before writing");
                        let src_end = src_start
                            .checked_add(row_bytes)
                            .expect("partial plan was validated before writing");
                        let dst_start = row
                            .checked_mul(dst_pitch)
                            .and_then(|offset| offset.checked_add(x_bytes))
                            .expect("partial plan was validated before writing");
                        let dst_end = dst_start
                            .checked_add(row_bytes)
                            .expect("partial plan was validated before writing");
                        let src = source
                            .bytes
                            .get(src_start..src_end)
                            .expect("partial plan was validated before writing");
                        let dst_row = dst
                            .get_mut(dst_start..dst_end)
                            .expect("partial plan was validated before writing");
                        Self::swizzle_xrgb_row(dst_row, src);
                    }
                    bytes = bytes.saturating_add(
                        u64::from(rect.width)
                            .saturating_mul(u64::from(rect.height))
                            .saturating_mul(4),
                    );
                }
                Ok((bytes, XrgbCpuWriteMode::Partial))
            }
        }
    }

    pub(super) fn validate_complete_mapped_xrgb_source(
        source: MappedXrgbSource<'_>,
    ) -> Result<(), BridgeError> {
        let tight = source.width.checked_mul(4).ok_or_else(|| {
            BridgeError::InvalidSize("XRGB mapped source row stride overflow".to_owned())
        })?;
        let required = source
            .padded_bytes_per_row
            .checked_mul(source.height)
            .ok_or_else(|| {
                BridgeError::InvalidSize("XRGB mapped source size overflow".to_owned())
            })?;
        if source.padded_bytes_per_row < tight || source.bytes.len() < required {
            return Err(BridgeError::InvalidSize(format!(
                "XRGB mapped source is incomplete: stride={} len={} for {}x{}",
                source.padded_bytes_per_row,
                source.bytes.len(),
                source.width,
                source.height,
            )));
        }
        Ok(())
    }

    fn validate_xrgb_rect_plan(
        source: MappedXrgbSource<'_>,
        dst: &[u8],
        dst_pitch: usize,
        rects: &[vello::FineDamageRect],
    ) -> Result<(), BridgeError> {
        if rects.len() > 64 {
            return Err(BridgeError::InvalidSize(
                "XRGB history supplied too many partial rectangles".to_owned(),
            ));
        }
        for &rect in rects {
            let x = usize::try_from(rect.x).map_err(|_| {
                BridgeError::InvalidSize("XRGB partial x does not fit usize".to_owned())
            })?;
            let y = usize::try_from(rect.y).map_err(|_| {
                BridgeError::InvalidSize("XRGB partial y does not fit usize".to_owned())
            })?;
            let width = usize::try_from(rect.width).map_err(|_| {
                BridgeError::InvalidSize("XRGB partial width does not fit usize".to_owned())
            })?;
            let height = usize::try_from(rect.height).map_err(|_| {
                BridgeError::InvalidSize("XRGB partial height does not fit usize".to_owned())
            })?;
            let right = x.checked_add(width);
            let bottom = y.checked_add(height);
            let row_bytes = width.checked_mul(4);
            let x_bytes = x.checked_mul(4);
            let (Some(right), Some(bottom), Some(row_bytes), Some(x_bytes)) =
                (right, bottom, row_bytes, x_bytes)
            else {
                return Err(BridgeError::InvalidSize(
                    "XRGB history supplied an invalid partial rectangle".to_owned(),
                ));
            };
            if width == 0 || height == 0 || right > source.width || bottom > source.height {
                return Err(BridgeError::InvalidSize(
                    "XRGB history supplied an invalid partial rectangle".to_owned(),
                ));
            }
            let last_row = bottom.saturating_sub(1);
            let source_end = last_row
                .checked_mul(source.padded_bytes_per_row)
                .and_then(|offset| offset.checked_add(x_bytes))
                .and_then(|offset| offset.checked_add(row_bytes));
            let destination_end = last_row
                .checked_mul(dst_pitch)
                .and_then(|offset| offset.checked_add(x_bytes))
                .and_then(|offset| offset.checked_add(row_bytes));
            if source_end.is_none_or(|end| end > source.bytes.len())
                || destination_end.is_none_or(|end| end > dst.len())
            {
                return Err(BridgeError::InvalidSize(
                    "XRGB partial write exceeds complete destination/source".to_owned(),
                ));
            }
        }
        Ok(())
    }

    #[inline]
    #[doc(hidden)]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "every offset is below the prevalidated pixel count times four"
    )]
    /// Convert one row of tight RGBA pixels to little-endian DRM XRGB8888.
    ///
    /// Public only so the CPU-only fallback benchmark can exercise the exact
    /// production primitive without constructing a GPU bridge.
    pub fn swizzle_xrgb_row(dst: &mut [u8], source: &[u8]) {
        // Use one prevalidated pixel count and raw byte pointers. This matters
        // for developer sessions: `shell` is deliberately built with debug
        // assertions, where the old chunks/zip/slice-pattern loop paid iterator
        // and bounds-check overhead for every one of the 2.07M pixels in a
        // 1080p frame. LLVM still recognises and vectorises this byte shuffle in
        // release builds.
        let pixels = dst.len().min(source.len()) / 4;
        let src = source.as_ptr();
        let out = dst.as_mut_ptr();
        for pixel in 0..pixels {
            let offset = pixel * 4;
            // SAFETY: `pixels` is the smaller slice length divided by four,
            // so `offset + 3` is in bounds of both `source` and `dst`.
            unsafe {
                *out.add(offset) = *src.add(offset + 2);
                *out.add(offset + 1) = *src.add(offset + 1);
                *out.add(offset + 2) = *src.add(offset);
                *out.add(offset + 3) = 0xff;
            }
        }
    }

    #[cfg(test)]
    pub(super) fn swizzle_xrgb_row_reference(dst: &mut [u8], source: &[u8]) {
        for (d, s) in dst.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 0xff;
        }
    }
}
