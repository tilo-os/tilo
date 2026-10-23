// Copyright 2026 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Sizing the bump-allocated buffers from what frames actually use.
//!
//! # The problem this solves
//!
//! `BufferSizes` reserves seven buffers — lines, binning data, tiles,
//! segment counts, segments, blend spill and the per-tile command list — whose sizes
//! upstream picks as constants, with the comment that they are "hand
//! picked to accommodate the vello test scenes as well as paris-30k" and
//! "should instead get derived from the scene layout using reasonable
//! heuristics". They come to about 165 MB.
//!
//! That is per `Renderer`, and a `Renderer` is per process. A desktop of
//! a dozen small applications on one integrated GPU therefore reserves
//! two gigabytes of the machine's own memory before anything is drawn,
//! and an allocation that can neither be served nor fail does not fail —
//! it wedges. Shrinking the constants by a fixed fraction is not a fix
//! either: `render_encoding_full` has no robust dynamic memory, so a
//! scene that outgrows them renders wrong *silently*.
//!
//! # What is done instead
//!
//! The counts are already measured. `BumpAllocators` is written by the
//! GPU during the coarse stages and holds, per buffer, what the frame
//! asked for — including the part that did not fit, because the shaders
//! bump the counter first and check afterwards, setting a bit in
//! `failed` when the write had to be dropped.
//!
//! The synchronous path schedules a 32-byte counter download and collects
//! it once wgpu reports the mapping ready. The asynchronous path already
//! waits between the coarse and fine stages, so it reads the counters there.
//! In either case, later renders grow [`BumpBudget`] with headroom instead
//! of every renderer permanently starting at a stress scene's budget.
//!
//! # What this costs, and when the guess is wrong
//!
//! The synchronous readback does not deliberately block, but callback
//! readiness is not bounded to one frame. The asynchronous path does wait
//! for its mapping. If a budget is too small, that render can omit work;
//! later renders grow from the counters that were reached before failure.
//! This is therefore appropriate for Tilo only because its first-render
//! floors cover measured application and compositor scenes. A one-shot
//! renderer should not rely on adaptation to repair its only output.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use vello_encoding::{BumpAllocators, BumpBudget};
use wgpu::{Buffer, Device};

use crate::BumpBudgetError;
use crate::recording::BufferProxy;
use crate::wgpu_engine::WgpuEngine;

/// Set to any non-empty value to print each collected frame's usage
/// against its adaptive policy budget.
///
/// This is the switch the reservation was calibrated with, and the one
/// to reach for when a scene renders with pieces missing.
const LOG_ENV: &str = "VELLO_BUMP_LOG";

/// Set to `upstream` to reserve upstream vello's constants and never
/// measure or grow. A field name (`lines`, `binning`, `tiles`,
/// `seg_counts`, `segments`, `blend`, or `ptcl`) pins only that reservation
/// to its upstream value while the others remain adaptive. `ptcl=N` accepts
/// an exact diagnostic word count. These variants are A/B tools for finding an
/// allocator whose counter/failure signal does not yet cover a real workload;
/// they are intentionally not production fallbacks.
///
/// Scene and viewport safety floors still apply, so this is a conservative
/// fallback rather than an exact reproduction of upstream allocation sizes.
const BUDGET_ENV: &str = "VELLO_BUMP_BUDGET";

/// A readback that has been started and not yet collected.
struct Pending {
    proxy: BufferProxy,
    /// Set by wgpu's map callback, from whichever thread polls.
    done: Arc<Mutex<Option<bool>>>,
    /// The render ran coarse over the whole scene, so its counters are the
    /// scene's needs; a damage render's cover only the bins it redrew.
    dense: bool,
    /// Tiles in the viewport of the render measured: what its growth may
    /// reach ([`BumpBudget::ceiling_for_viewport`]), whatever viewport the
    /// render that collects it draws into.
    n_tiles: u32,
}

/// Dense frames in a row whose needs stay far below the reservation before
/// it shrinks to them, in a renderer that opted in
/// ([`BudgetTracker::set_shrink`]).
const SHRINK_AFTER_FRAMES: u32 = 120;
/// The same while a shrink is asked for soon ([`BudgetTracker::shrink_soon`]):
/// the scene got smaller for good, and an idle desktop draws few frames.
const SHRINK_SOON_FRAMES: u32 = 2;
/// Measurements a soon request stays live for.
const SHRINK_SOON_MEASUREMENTS: u32 = 16;
/// A shrink keeps this many times the most any frame of its window used:
/// twice what growth reserves for it, so a scene that doubles again still
/// renders without overflowing, and one that stays put never shrinks and
/// regrows.
const SHRINK_HEADROOM: u32 = 4;
/// A shrink is worth reallocating every buffer for when it hands back at
/// least this fraction of what the viewport reserves, floors included.
const SHRINK_MIN_FRACTION: u64 = 8;

/// What [`BudgetTracker`] has measured since the reservation last changed,
/// for handing back what a busier scene made it reserve.
#[derive(Default)]
struct Shrink {
    enabled: bool,
    /// Field-wise most the dense frames of this window used.
    peak: BumpAllocators,
    /// Dense frames in this window.
    frames: u32,
    /// Measurements left in which [`SHRINK_SOON_FRAMES`] applies.
    soon: u32,
    /// Shrinks so far.
    shrinks: u64,
}

impl Shrink {
    fn restart(&mut self) {
        self.peak = BumpAllocators::default();
        self.frames = 0;
    }
}

/// The budget for one renderer, and the readback that keeps it honest.
pub(crate) struct BudgetTracker {
    budget: BumpBudget,
    /// Every measured render's readback not collected yet, oldest first.
    /// Each render is measured, not only the first after a collection: a
    /// renderer that draws a small scene and then a large one before the
    /// first readback lands (a backdrop blur's ground, then the frame over
    /// it) would otherwise never measure the large one, and the large one
    /// would overflow, unseen, on every frame.
    pending: VecDeque<Pending>,
    /// A failed observation cannot be repaired by later successful frames.
    /// Legacy collection leaves this intact for checked settlement.
    error: Option<BumpBudgetError>,
    log: bool,
    /// Hold the policy budget at upstream's constants.
    pinned: bool,
    /// An overflow that growth could not answer has been reported.
    limit_reported: bool,
    /// Bytes the whole adaptive budget may grow to.
    max_total_bytes: u64,
    /// Grow straight to `max_total_bytes` on the first growth, then never
    /// again: one reallocation per renderer instead of a doubling per frame
    /// while a scene fills up.
    grow_once: bool,
    /// `grow_once` has grown.
    saturated: bool,
    /// Tiles in the viewport last rendered: the log's floors, and the
    /// viewport-scaled ceiling growth stops at.
    n_tiles: u32,
    /// Frames seen, for the log only.
    frame: u64,
    shrink: Shrink,
}

/// Tile edge in pixels, from `TILE_WIDTH`/`TILE_HEIGHT` in
/// `vello_encoding`'s config.
const TILE_SIZE: u32 = 16;

impl BudgetTracker {
    pub(crate) fn new() -> Self {
        let override_name = std::env::var_os(BUDGET_ENV);
        let pinned = override_name.as_deref().is_some_and(|v| v == "upstream");
        let mut budget = if pinned {
            BumpBudget::UPSTREAM
        } else {
            BumpBudget::INITIAL
        };
        if let Some(name) = override_name.as_deref().and_then(|name| name.to_str()) {
            match name {
                "lines" => budget.lines = BumpBudget::UPSTREAM.lines,
                "binning" => budget.binning = BumpBudget::UPSTREAM.binning,
                "tiles" => budget.tiles = BumpBudget::UPSTREAM.tiles,
                "seg_counts" => budget.seg_counts = BumpBudget::UPSTREAM.seg_counts,
                "segments" => budget.segments = BumpBudget::UPSTREAM.segments,
                "blend" => budget.blend_spill = BumpBudget::UPSTREAM.blend_spill,
                "ptcl" => budget.ptcl = BumpBudget::UPSTREAM.ptcl,
                "upstream" | "" => {}
                exact if exact.starts_with("ptcl=") => {
                    if let Some(words) = exact
                        .strip_prefix("ptcl=")
                        .and_then(|words| words.parse::<u32>().ok())
                    {
                        budget.ptcl = words.min(BumpBudget::CEILING.ptcl);
                    } else {
                        eprintln!("vello: ignoring invalid {BUDGET_ENV}={exact:?}");
                    }
                }
                other => eprintln!(
                    "vello: ignoring unknown {BUDGET_ENV}={other:?}; expected upstream or an allocator field"
                ),
            }
        }
        Self {
            budget,
            pending: VecDeque::new(),
            error: None,
            log: std::env::var_os(LOG_ENV).is_some_and(|v| !v.is_empty()),
            pinned,
            limit_reported: false,
            max_total_bytes: u64::MAX,
            grow_once: false,
            saturated: false,
            n_tiles: 0,
            frame: 0,
            shrink: Shrink::default(),
        }
    }

    /// Hand back reservation a busier scene grew: once dense frames have
    /// needed far less for a while, lower each field towards
    /// [`SHRINK_HEADROOM`] times the most they used, by at most half per
    /// shrink. Off by default, where growth is one-way.
    pub(crate) fn set_shrink(&mut self, enabled: bool) {
        self.shrink.enabled = enabled;
        self.shrink.restart();
    }

    /// The scene got smaller for good (a window closed): for the next few
    /// measurements, shrink once after [`SHRINK_SOON_FRAMES`] dense frames
    /// instead of [`SHRINK_AFTER_FRAMES`]. Frames measured before the
    /// request do not count.
    pub(crate) fn shrink_soon(&mut self) {
        self.shrink.soon = SHRINK_SOON_MEASUREMENTS;
        self.shrink.restart();
    }

    /// Learn from one collected measurement whether the reservation can come
    /// down; `true` if it did. Only dense frames that fit count: a damage
    /// render's counters miss the bins it skipped, and an overflowing one's
    /// stop where it ran out. Growth starts the window again.
    fn shrink_after(&mut self, used: &BumpAllocators, dense: bool, grew: bool) -> bool {
        if !self.shrink.enabled || self.pinned {
            return false;
        }
        let shrink = &mut self.shrink;
        shrink.soon = shrink.soon.saturating_sub(1);
        if grew || used.failed != 0 {
            shrink.restart();
            return false;
        }
        if !dense {
            return false;
        }
        let peak = &mut shrink.peak;
        peak.lines = peak.lines.max(used.lines);
        peak.binning = peak.binning.max(used.binning);
        peak.tile = peak.tile.max(used.tile);
        peak.seg_counts = peak.seg_counts.max(used.seg_counts);
        peak.segments = peak.segments.max(used.segments.max(used.seg_counts));
        peak.blend = peak.blend.max(used.blend);
        peak.ptcl = peak.ptcl.max(used.ptcl);
        shrink.frames += 1;
        let needed = if shrink.soon > 0 {
            SHRINK_SOON_FRAMES
        } else {
            SHRINK_AFTER_FRAMES
        };
        if shrink.frames < needed {
            return false;
        }
        let peak = shrink.peak;
        shrink.restart();
        // At most half per shrink: growth reserves twice what a frame used,
        // so the scene that grew a field to this size still fits in one
        // shrink's result and renders without overflowing when it comes
        // back. A wrapped counter is huge and keeps its field as it is.
        let keep =
            |field: u32, used: u32| field.min(used.saturating_mul(SHRINK_HEADROOM).max(field / 2));
        let target = BumpBudget {
            lines: keep(self.budget.lines, peak.lines),
            binning: keep(self.budget.binning, peak.binning),
            tiles: keep(self.budget.tiles, peak.tile),
            seg_counts: keep(self.budget.seg_counts, peak.seg_counts),
            segments: keep(self.budget.segments, peak.segments),
            blend_spill: keep(self.budget.blend_spill, peak.blend),
            ptcl: keep(self.budget.ptcl, peak.ptcl),
            ..self.budget
        };
        let before = self.effective().size_in_bytes();
        let current = std::mem::replace(&mut self.budget, target);
        let after = self.effective().size_in_bytes();
        if before.saturating_sub(after) < before / SHRINK_MIN_FRACTION {
            self.budget = current;
            return false;
        }
        // A grow-once budget that shrank may grow again.
        self.saturated = false;
        // One shrink per request: a second would no longer fit the scene
        // that grew the reservation.
        self.shrink.soon = 0;
        self.shrink.shrinks += 1;
        true
    }

    /// Times [`Self::shrink_after`] lowered the reservation.
    pub(crate) fn shrinks(&self) -> u64 {
        self.shrink.shrinks
    }

    /// Limit every reservation to what `limits` lets one binding hold.
    pub(crate) fn set_device_limits(&mut self, limits: &wgpu::Limits) {
        self.budget = self
            .budget
            .with_max_binding_bytes(max_binding_bytes(limits));
    }

    /// Limit the growth of the whole budget to `bytes`, shrinking it now if
    /// it is already larger.
    pub(crate) fn set_max_total_bytes(&mut self, bytes: u64) {
        self.max_total_bytes = bytes;
        self.budget.fit_total_bytes(bytes);
    }

    /// See [`Self::grow_once`].
    pub(crate) fn set_grow_once(&mut self, grow_once: bool) {
        self.grow_once = grow_once;
    }

    /// Grow to cover `used`, within what this viewport may reserve, the
    /// device can bind and the total limit.
    /// Growth is bounded by the viewport last rendered into.
    fn grow(&mut self, used: &BumpAllocators) -> bool {
        self.grow_in(used, self.n_tiles)
    }

    /// [`Self::grow`] for counters a render of `n_tiles` reported: its
    /// own viewport bounds the growth, not the viewport of the render that
    /// collects them.
    fn grow_in(&mut self, used: &BumpAllocators, n_tiles: u32) -> bool {
        if self.saturated {
            return false;
        }
        let before = self.budget;
        self.budget
            .observe_within(used, &BumpBudget::ceiling_for_viewport(n_tiles));
        if self.grow_once && self.budget != before && self.max_total_bytes != u64::MAX {
            // The observed needs, and the ceiling's proportions for the rest,
            // scaled together to the limit.
            let mut ceiling =
                BumpBudget::CEILING.with_max_binding_bytes(self.budget.max_binding_bytes);
            ceiling.fit_total_bytes(self.max_total_bytes);
            let b = &mut self.budget;
            b.lines = b.lines.max(ceiling.lines);
            b.binning = b.binning.max(ceiling.binning);
            b.tiles = b.tiles.max(ceiling.tiles);
            b.seg_counts = b.seg_counts.max(ceiling.seg_counts);
            b.segments = b.segments.max(ceiling.segments);
            b.blend_spill = b.blend_spill.max(ceiling.blend_spill);
            b.ptcl = b.ptcl.max(ceiling.ptcl);
            self.saturated = true;
        }
        self.budget.fit_total_bytes(self.max_total_bytes);
        self.budget != before
    }

    /// The reservation to render the next frame with.
    pub(crate) fn budget(&self) -> &BumpBudget {
        &self.budget
    }

    /// Note the viewport this renderer is drawing into, so the log can
    /// include the viewport policy floors. [`BufferSizes`] also applies
    /// scene-derived and structural prefixes that this tracker cannot see.
    ///
    /// [`BufferSizes`]: vello_encoding::BufferSizes
    pub(crate) fn set_viewport(&mut self, width: u32, height: u32) {
        self.n_tiles = width.max(1).div_ceil(TILE_SIZE) * height.max(1).div_ceil(TILE_SIZE);
    }

    /// The policy budget after viewport floors, excluding scene-derived
    /// floors and structural prefixes added by [`BufferSizes`].
    fn effective(&self) -> BumpBudget {
        let floor = BumpBudget::floor_for_viewport(self.n_tiles);
        BumpBudget {
            lines: self.budget.lines.max(floor.lines),
            binning: self.budget.binning.max(floor.binning),
            tiles: self.budget.tiles.max(floor.tiles),
            seg_counts: self.budget.seg_counts.max(floor.seg_counts),
            segments: self.budget.segments.max(floor.segments),
            blend_spill: self.budget.blend_spill.max(floor.blend_spill),
            ptcl: self.budget.ptcl.max(floor.ptcl),
            max_binding_bytes: u32::MAX,
        }
        .with_max_binding_bytes(self.budget.max_binding_bytes)
    }

    /// Whether this frame should download the bump counters: every render
    /// of an adaptive budget does. A download still in flight is kept
    /// beside it until its map callback lands, and collected then.
    pub(crate) fn wants_readback(&self) -> bool {
        !self.pinned
    }

    /// Whether adaptive sizing needs counters from a render.
    pub(crate) fn wants_measurement(&self) -> bool {
        !self.pinned
    }

    pub(crate) fn has_pending_readback(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Errors remain visible until this tracker is replaced, including when
    /// an ordinary render collected the failed readback first.
    pub(crate) fn check_readback(&self) -> Result<(), BumpBudgetError> {
        self.error.clone().map_or(Ok(()), Err)
    }

    pub(crate) fn effective_size_in_bytes(&self) -> u64 {
        self.effective().size_in_bytes()
    }

    /// Collect a finished readback, if there is one, and grow the budget
    /// to cover what it reports.
    ///
    /// Returns `(changed, overflowed)`: `changed` tells the caller to drop
    /// pooled buffers at obsolete sizes, while `overflowed` lets a one-shot
    /// renderer reject the incomplete pixels and immediately submit again.
    pub(crate) fn collect(&mut self, device: &Device, engine: &mut WgpuEngine) -> (bool, bool) {
        self.collect_with_poll(engine, || device.poll(wgpu::PollType::Poll))
    }

    fn collect_with_poll(
        &mut self,
        engine: &mut WgpuEngine,
        poll: impl FnOnce() -> Result<wgpu::PollStatus, wgpu::PollError>,
    ) -> (bool, bool) {
        if self.pending.is_empty() {
            return (false, false);
        }
        // Non-blocking: this only gives already-finished callbacks a
        // chance to run. wgpu also runs them on submit, so by the top of
        // the next frame the usual case is that this has nothing to do.
        if let Err(error) = poll() {
            self.error
                .get_or_insert_with(|| BumpBudgetError::Poll(error.to_string()));
            // Poll failure leaves mapping readiness unknown. Do not put these
            // allocations through the pool's known-unmapped reuse path.
            for pending in self.pending.drain(..) {
                engine.free_download(pending.proxy);
            }
            return (false, false);
        }
        let mut changed = false;
        let mut overflowed = false;
        let mut in_flight = VecDeque::with_capacity(self.pending.len());
        while let Some(pending) = self.pending.pop_front() {
            match self.collect_one(engine, pending) {
                Ok((grew, overflow)) => {
                    changed |= grew;
                    overflowed |= overflow;
                }
                // Still in flight: look again next frame, in order.
                Err(pending) => in_flight.push_back(pending),
            }
        }
        self.pending = in_flight;
        (changed, overflowed)
    }

    /// Collect `pending` if its map callback has landed: `(changed,
    /// overflowed)` as [`Self::collect`] says them. Gives it back while it
    /// is still in flight.
    #[allow(
        clippy::print_stderr,
        reason = "Tilo applications do not install a log-to-tracing bridge"
    )]
    fn collect_one(
        &mut self,
        engine: &mut WgpuEngine,
        pending: Pending,
    ) -> Result<(bool, bool), Pending> {
        let Some(buf) = engine.get_download(pending.proxy) else {
            self.error.get_or_insert(BumpBudgetError::MissingDownload);
            return Ok((false, false));
        };
        let ready = pending
            .done
            .lock()
            .map(|guard| *guard)
            .unwrap_or(Some(false));
        let Some(ok) = ready else {
            return Err(pending);
        };
        if !ok {
            self.error.get_or_insert(BumpBudgetError::MapFailed);
            // A failed map is not a known-unmapped reusable allocation.
            engine.free_download(pending.proxy);
            return Ok((false, false));
        }
        let used = {
            let view = buf.slice(..).get_mapped_range();
            bytemuck::try_pod_read_unaligned::<BumpAllocators>(&view)
        };
        buf.unmap();
        let Ok(used) = used else {
            self.error.get_or_insert(BumpBudgetError::InvalidDownload);
            engine.free_download(pending.proxy);
            return Ok((false, false));
        };
        let before = self.effective();
        let overflowed = used.failed != 0;
        let in_effect = self.effective();
        let budget_changed = !self.pinned && self.grow_in(&used, pending.n_tiles);
        let shrunk = self.shrink_after(&used, pending.dense, budget_changed);
        let changed = before != self.effective();
        if self.log {
            self.report(&used, &in_effect, budget_changed || shrunk);
        } else if used.failed != 0 && !budget_changed {
            if !self.limit_reported {
                self.limit_reported = true;
                eprintln!(
                    "vello: a frame overflowed its bump buffers (failed={:#x}) and the \
                     reservation cannot grow (ceiling or device limit of {} bytes per \
                     buffer); the frame is not drawn. It asked for lines {}, binning {}, \
                     tiles {}, seg_counts {}, segments {}, blend {}, ptcl {} against \
                     lines {}, binning {}, tiles {}, seg_counts {}, segments {}, \
                     blend {}, ptcl {}. Reported once. Set {LOG_ENV}=1 for detail.",
                    used.failed,
                    self.budget.max_binding_bytes,
                    used.lines,
                    used.binning,
                    used.tile,
                    used.seg_counts,
                    used.segments.max(used.seg_counts),
                    used.blend,
                    used.ptcl,
                    in_effect.lines,
                    in_effect.binning,
                    in_effect.tiles,
                    in_effect.seg_counts,
                    in_effect.segments,
                    in_effect.blend_spill,
                    in_effect.ptcl,
                );
            }
        } else if used.failed != 0 {
            // `eprintln!`, matching `report` below, and deliberately
            // not `log::warn!`: nothing in this workspace installs a
            // `log` subscriber or a log-to-tracing bridge, and
            // applications built on the toolkit never initialise
            // tracing at all -- so the record went nowhere, in
            // exactly the processes most likely to overflow. An
            // overflow drops geometry with no other symptom, so a
            // warning that cannot be read is the same as no warning.
            eprintln!(
                "vello: a frame overflowed its bump buffers (failed={:#x}); \
                 growing the reservation to {} KB. Set {LOG_ENV}=1 for detail.",
                used.failed,
                self.budget.size_in_bytes() / 1024,
            );
        }
        engine.recycle_bump_download(pending.proxy);
        Ok((changed, overflowed))
    }

    /// Grow from counters the caller has already read back itself.
    ///
    /// The async render path awaits them between the coarse and fine
    /// stages, so it has no use for the deferred machinery above.
    pub(crate) fn observe_now(&mut self, used: &BumpAllocators) -> bool {
        self.frame += 1;
        let in_effect = self.effective();
        let changed = !self.pinned && self.grow(used);
        if self.log {
            self.report(used, &in_effect, changed);
        }
        changed
    }

    /// Start a readback of the bump counters written by this frame.
    pub(crate) fn begin_readback(&mut self, engine: &WgpuEngine, proxy: BufferProxy) {
        self.begin_readback_of(engine, proxy, false);
    }

    /// [`Self::begin_readback`] for a render that ran coarse over the whole
    /// scene, whose counters may shrink the reservation.
    pub(crate) fn begin_dense_readback(&mut self, engine: &WgpuEngine, proxy: BufferProxy) {
        self.begin_readback_of(engine, proxy, true);
    }

    fn begin_readback_of(&mut self, engine: &WgpuEngine, proxy: BufferProxy, dense: bool) {
        let Some(buf) = engine.get_download(proxy) else {
            self.error.get_or_insert(BumpBudgetError::MissingDownload);
            return;
        };
        let done = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&done);
        map_read(buf, move |ok| {
            if let Ok(mut guard) = slot.lock() {
                *guard = Some(ok);
            }
        });
        self.frame += 1;
        self.pending.push_back(Pending {
            proxy,
            done,
            dense,
            n_tiles: self.n_tiles,
        });
    }

    /// One line per measured frame: measured use against the policy budget.
    /// Scene-derived floors and structural prefixes are not included.
    #[allow(
        clippy::print_stderr,
        reason = "explicit diagnostics must work without logging initialization"
    )]
    fn report(&self, used: &BumpAllocators, in_effect: &BumpBudget, changed: bool) {
        let b = in_effect;
        eprintln!(
            "vello frame {}: lines {}/{} binning {}/{} tiles {}/{} seg_counts {}/{} \
             segments {}/{} blend {}/{} ptcl {}/{} failed={:#x} policy={} KB{}",
            self.frame,
            used.lines,
            b.lines,
            used.binning,
            b.binning,
            used.tile,
            b.tiles,
            used.seg_counts,
            b.seg_counts,
            used.segments,
            b.segments,
            used.blend,
            b.blend_spill,
            used.ptcl,
            b.ptcl,
            used.failed,
            b.size_in_bytes() / 1024,
            if changed { " (grew)" } else { "" },
        );
    }
}

/// The largest buffer one storage binding may hold on a device with `limits`,
/// rounded down to a size the engine's pool allocates exactly. The pool
/// quantizes requests up to `2^k` or `3 * 2^(k-1)` bytes and binds the whole
/// buffer, so a request at an unrounded limit would allocate past it.
pub(crate) fn max_binding_bytes(limits: &wgpu::Limits) -> u32 {
    let limit = limits
        .max_storage_buffer_binding_size
        .min(limits.max_buffer_size)
        .min(u64::from(u32::MAX));
    if limit < 4 {
        return 4;
    }
    let power = 1_u64 << (63 - limit.leading_zeros());
    let class = if power + power / 2 <= limit {
        power + power / 2
    } else {
        power
    };
    u32::try_from(class).unwrap_or(u32::MAX)
}

fn map_read(buf: &Buffer, f: impl FnOnce(bool) + Send + 'static) {
    buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
        f(res.is_ok());
    });
}

#[cfg(test)]
mod tests {
    use super::{
        BudgetTracker, SHRINK_AFTER_FRAMES, SHRINK_HEADROOM, SHRINK_SOON_FRAMES,
        SHRINK_SOON_MEASUREMENTS, max_binding_bytes,
    };
    use vello_encoding::{BumpAllocators, BumpBudget};

    // Catches a budget that doubles several times while a scene fills up:
    // each growth reallocates every buffer, briefly held twice by a browser.
    #[test]
    fn grow_once_reallocates_once_and_then_holds() {
        let mut tracker = BudgetTracker::new();
        tracker.set_max_total_bytes(32 << 20);
        tracker.set_grow_once(true);
        let small = BumpAllocators {
            lines: 1000,
            ptcl: 1000,
            ..Default::default()
        };
        assert!(tracker.grow(&small), "the first growth reallocates");
        let grown = *tracker.budget();
        assert!(grown.size_in_bytes() <= 32 << 20);
        assert!(
            grown.size_in_bytes() > 16 << 20,
            "it grows to the limit, not to 2x use"
        );
        let big = BumpAllocators {
            lines: 1 << 22,
            ptcl: 1 << 24,
            ..Default::default()
        };
        assert!(!tracker.grow(&big), "no second reallocation");
        assert_eq!(*tracker.budget(), grown);
    }

    /// A tracker for a 1080p viewport whose reservation a busy scene grew.
    fn grown_tracker() -> BudgetTracker {
        let mut tracker = BudgetTracker::new();
        tracker.set_viewport(1920, 1080);
        tracker.set_shrink(true);
        assert!(tracker.grow(&busy()));
        tracker
    }

    fn busy() -> BumpAllocators {
        BumpAllocators {
            lines: 1 << 20,
            tile: 1 << 20,
            seg_counts: 1 << 20,
            segments: 1 << 20,
            ptcl: 1 << 22,
            binning: 1 << 16,
            blend: 1 << 14,
            failed: 0,
        }
    }

    fn light() -> BumpAllocators {
        BumpAllocators {
            lines: 20_000,
            tile: 10_000,
            seg_counts: 8_000,
            segments: 9_000,
            ptcl: 50_000,
            binning: 2_000,
            blend: 0,
            failed: 0,
        }
    }

    /// Feed `frames` measurements of `used`; whether any shrank.
    fn feed(tracker: &mut BudgetTracker, used: &BumpAllocators, dense: bool, frames: u32) -> bool {
        let mut shrank = false;
        for _ in 0..frames {
            shrank |= tracker.shrink_after(used, dense, false);
        }
        shrank
    }

    /// Whether a frame using `used` fits `budget` without overflowing.
    fn fits(budget: &BumpBudget, used: &BumpAllocators) -> bool {
        used.lines <= budget.lines
            && used.binning <= budget.binning
            && used.tile <= budget.tiles
            && used.seg_counts <= budget.seg_counts
            && used.segments <= budget.segments
            && used.blend <= budget.blend_spill
            && used.ptcl <= budget.ptcl
    }

    // Catches a reservation that stays sized for the busiest scene ever
    // drawn after the scene got lighter; one that drops so far at once that
    // the busy scene, drawn again, overflows; and one that shrinks to exactly
    // what the light scene used, which its next growth would overflow.
    #[test]
    fn a_reservation_shrinks_after_a_window_of_lighter_dense_frames() {
        let mut tracker = grown_tracker();
        let grown = *tracker.budget();
        let reserved = tracker.effective_size_in_bytes();
        let light = light();
        assert!(!feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES - 1));
        assert_eq!(*tracker.budget(), grown, "not before the window is full");
        assert!(feed(&mut tracker, &light, true, 1));
        assert_eq!(tracker.shrinks(), 1);
        let shrunk = *tracker.budget();
        assert_eq!(shrunk.lines, grown.lines / 2, "at most half per shrink");
        assert_eq!(shrunk.max_binding_bytes, grown.max_binding_bytes);
        assert!(fits(&shrunk, &busy()), "the busy scene still fits once");
        assert!(tracker.effective_size_in_bytes() * 8 <= reserved * 7);
        // Light frames that stay light bring it the rest of the way, to
        // within the smallest step worth taking of what a renderer that only
        // ever drew them reserves, and then hold.
        while feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES) {}
        let mut light_only = BudgetTracker::new();
        light_only.set_viewport(1920, 1080);
        light_only.grow(&light);
        let floor = light_only.effective_size_in_bytes();
        let settled = tracker.effective_size_in_bytes();
        assert!(settled <= floor + floor / super::SHRINK_MIN_FRACTION);
        assert!(tracker.shrinks() > 1);
        assert!(tracker.budget().lines >= light.lines * SHRINK_HEADROOM);
        assert!(!tracker.grow(&light), "the light scene does not regrow it");
    }

    // Catches a shrink that learns from damage renders: their counters miss
    // every bin they did not redraw.
    #[test]
    fn damage_renders_neither_count_nor_shrink() {
        let mut tracker = grown_tracker();
        let grown = *tracker.budget();
        assert!(!feed(
            &mut tracker,
            &light(),
            false,
            4 * SHRINK_AFTER_FRAMES
        ));
        assert_eq!(*tracker.budget(), grown);
    }

    // Catches a shrink from a frame that overflowed (its counters stop where
    // it ran out) or across a growth.
    #[test]
    fn an_overflow_or_a_growth_starts_the_window_again() {
        let mut tracker = grown_tracker();
        let grown = *tracker.budget();
        let light = light();
        assert!(!feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES - 1));
        let overflowed = BumpAllocators {
            failed: 0x10,
            ..light
        };
        assert!(!tracker.shrink_after(&overflowed, true, false));
        assert!(!feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES - 1));
        let grew = tracker.shrink_after(&light, true, true);
        assert!(!grew, "this frame grew it");
        assert!(!feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES - 1));
        assert_eq!(*tracker.budget(), grown);
        assert!(feed(&mut tracker, &light, true, 1));
    }

    // Catches a close whose memory comes back only after a hundred more
    // frames, a soon window that still counts the busy frames before it, and
    // a request that keeps shrinking past what the closed scene needs.
    #[test]
    fn asked_soon_it_shrinks_once_after_two_dense_frames_drawn_since() {
        let mut tracker = grown_tracker();
        let light = light();
        assert!(!feed(&mut tracker, &busy(), true, 50));
        tracker.shrink_soon();
        let damage = feed(&mut tracker, &light, false, 3);
        assert!(!damage, "damage renders still do not count");
        assert!(!feed(&mut tracker, &light, true, SHRINK_SOON_FRAMES - 1));
        assert!(feed(&mut tracker, &light, true, 1));
        assert!(
            fits(tracker.budget(), &busy()),
            "the closed scene still fits"
        );
        let again = feed(&mut tracker, &light, true, SHRINK_AFTER_FRAMES - 1);
        assert!(
            !again,
            "one shrink per request; the next waits the long way"
        );
        assert!(feed(&mut tracker, &light, true, 1));
        // An unused request expires: the next shrink waits the long way too.
        let mut expired = grown_tracker();
        expired.shrink_soon();
        assert!(!feed(&mut expired, &light, false, SHRINK_SOON_MEASUREMENTS));
        assert!(!feed(&mut expired, &light, true, SHRINK_AFTER_FRAMES - 1));
        assert!(feed(&mut expired, &light, true, 1));
    }

    // Catches reallocating every buffer for a shrink that frees little: a
    // reservation already near the viewport's floors stays as it is, and a
    // renderer that did not opt in keeps upstream's one-way growth.
    #[test]
    fn small_steps_and_renderers_that_did_not_opt_in_keep_their_reservation() {
        let mut tracker = BudgetTracker::new();
        tracker.set_viewport(1920, 1080);
        tracker.set_shrink(true);
        let near_floor = BumpAllocators {
            lines: 100_000,
            ..light()
        };
        assert!(tracker.grow(&near_floor));
        let held = *tracker.budget();
        assert!(!feed(&mut tracker, &light(), true, 4 * SHRINK_AFTER_FRAMES));
        assert_eq!(*tracker.budget(), held);

        let mut upstream = grown_tracker();
        upstream.set_shrink(false);
        let grown = *upstream.budget();
        upstream.shrink_soon();
        assert!(!feed(
            &mut upstream,
            &light(),
            true,
            4 * SHRINK_AFTER_FRAMES
        ));
        assert_eq!(*upstream.budget(), grown);
    }

    fn size_class(bytes: u64) -> u64 {
        let power = bytes.next_power_of_two();
        if power / 4 * 3 >= bytes {
            power / 4 * 3
        } else {
            power
        }
    }

    #[test]
    fn binding_limit_is_a_pool_size_class_within_the_device_limits() {
        let webgpu = wgpu::Limits::default();
        assert_eq!(max_binding_bytes(&webgpu), 128 << 20);
        for (binding, buffer) in [
            (128 << 20, 256 << 20),
            (200_000_000, 256 << 20),
            (1 << 30, 256 << 20),
            (u64::MAX, u64::MAX),
        ] {
            let limits = wgpu::Limits {
                max_storage_buffer_binding_size: binding,
                max_buffer_size: buffer,
                ..wgpu::Limits::default()
            };
            let bytes = u64::from(max_binding_bytes(&limits));
            assert!(bytes <= binding.min(buffer));
            assert_eq!(size_class(bytes), bytes);
            // A clamped 24-byte-element buffer stays within it after pooling.
            assert!(size_class(bytes / 24 * 24) <= bytes);
        }
    }
}

#[cfg(test)]
pub(crate) mod test_helpers {
    use super::*;

    /// The latest render's readback.
    pub(crate) fn pending_proxy(tracker: &BudgetTracker) -> BufferProxy {
        tracker
            .pending
            .back()
            .expect("actual pending readback")
            .proxy
    }

    // Replaces only the external callback's result slot. The real GPU buffer
    // must already have completed mapping before readiness is held this way.
    pub(crate) fn set_ready(tracker: &mut BudgetTracker, ready: Option<bool>) {
        tracker
            .pending
            .back_mut()
            .expect("actual pending readback")
            .done = Arc::new(Mutex::new(ready));
    }

    /// Hold the latest readback's callback slot empty and hand it back, so a
    /// test lands the callback itself, from another thread or later. The
    /// real GPU buffer must already have completed mapping.
    pub(crate) fn hold_ready(tracker: &mut BudgetTracker) -> Arc<Mutex<Option<bool>>> {
        let slot = Arc::new(Mutex::new(None));
        tracker
            .pending
            .back_mut()
            .expect("actual pending readback")
            .done = slot.clone();
        slot
    }

    pub(crate) fn fail_poll(tracker: &mut BudgetTracker, engine: &mut WgpuEngine) {
        tracker.collect_with_poll(engine, || Err(wgpu::PollError::Timeout));
    }

    pub(crate) fn disable_measurement(tracker: &mut BudgetTracker) {
        tracker.pinned = true;
    }
}
