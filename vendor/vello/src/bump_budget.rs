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
//! This is therefore appropriate for qOS only because its first-render
//! floors cover measured application and compositor scenes. A one-shot
//! renderer should not rely on adaptation to repair its only output.

use std::sync::{Arc, Mutex};

use vello_encoding::{BumpAllocators, BumpBudget};
use wgpu::{Buffer, Device};

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
}

/// The budget for one renderer, and the readback that keeps it honest.
pub(crate) struct BudgetTracker {
    budget: BumpBudget,
    pending: Option<Pending>,
    log: bool,
    /// Hold the policy budget at upstream's constants.
    pinned: bool,
    /// Tiles in the viewport last rendered, for the log only.
    n_tiles: u32,
    /// Frames seen, for the log only.
    frame: u64,
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
            pending: None,
            log: std::env::var_os(LOG_ENV).is_some_and(|v| !v.is_empty()),
            pinned,
            n_tiles: 0,
            frame: 0,
        }
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
        }
    }

    /// Whether this frame should download the bump counters.
    ///
    /// Only when the previous download has been collected, so a frame
    /// that outruns the map callback does not queue up a second buffer
    /// to leak.
    pub(crate) fn wants_readback(&self) -> bool {
        !self.pinned && self.pending.is_none()
    }

    /// Whether adaptive sizing needs counters from a render.
    pub(crate) fn wants_measurement(&self) -> bool {
        !self.pinned
    }

    pub(crate) fn has_pending_readback(&self) -> bool {
        self.pending.is_some()
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
    #[allow(
        clippy::print_stderr,
        reason = "qOS applications do not install a log-to-tracing bridge"
    )]
    pub(crate) fn collect(&mut self, device: &Device, engine: &mut WgpuEngine) -> (bool, bool) {
        let Some(pending) = self.pending.take() else {
            return (false, false);
        };
        // Non-blocking: this only gives already-finished callbacks a
        // chance to run. wgpu also runs them on submit, so by the top of
        // the next frame the usual case is that this has nothing to do.
        let _ = device.poll(wgpu::PollType::Poll);
        let ready = pending
            .done
            .lock()
            .map(|guard| *guard)
            .unwrap_or(Some(false));
        let Some(ok) = ready else {
            // Still in flight. Put it back and look again next frame.
            self.pending = Some(pending);
            return (false, false);
        };
        let before = self.effective();
        let mut changed = false;
        let mut overflowed = false;
        if ok && let Some(buf) = engine.get_download(pending.proxy) {
            let used: BumpAllocators = {
                let view = buf.slice(..).get_mapped_range();
                bytemuck::pod_read_unaligned(&view)
            };
            overflowed = used.failed != 0;
            buf.unmap();
            let in_effect = self.effective();
            let budget_changed = !self.pinned && self.budget.observe(&used);
            changed = before != self.effective();
            if self.log {
                self.report(&used, &in_effect, budget_changed);
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
        }
        engine.free_download(pending.proxy);
        (changed, overflowed)
    }

    /// Grow from counters the caller has already read back itself.
    ///
    /// The async render path awaits them between the coarse and fine
    /// stages, so it has no use for the deferred machinery above.
    pub(crate) fn observe_now(&mut self, used: &BumpAllocators) -> bool {
        self.frame += 1;
        let in_effect = self.effective();
        let changed = !self.pinned && self.budget.observe(used);
        if self.log {
            self.report(used, &in_effect, changed);
        }
        changed
    }

    /// Start a readback of the bump counters written by this frame.
    pub(crate) fn begin_readback(&mut self, engine: &WgpuEngine, proxy: BufferProxy) {
        let Some(buf) = engine.get_download(proxy) else {
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
        self.pending = Some(Pending { proxy, done });
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

fn map_read(buf: &Buffer, f: impl FnOnce(bool) + Send + 'static) {
    buf.slice(..).map_async(wgpu::MapMode::Read, move |res| {
        f(res.is_ok());
    });
}
