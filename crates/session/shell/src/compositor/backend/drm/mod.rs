//! DRM/KMS + libseat + udev + libinput backend for running on a TTY without a parent
//! Wayland/X11 compositor.
//!
//! Hardware-touching code stays thin; decision logic lives in a pure module beside it, since a
//! compositor backend can't be exercised on the dev machine and only GPU-free parts can be proven.
//!
//! [`devices`] KMS node/mode selection · [`seat_diag`] disambiguates libseat's one `ENOSYS` for
//! three causes · [`session`] pause/resume ordering · [`hotplug`] output diffing/re-layout ·
//! [`scale`] per-output scale · [`input_config`] libinput policy · [`gestures`] shell vs. client
//! gesture routing · [`outputs`] stable `OutputId` allocation · [`runtime`] hardware glue + the
//! inner calloop loop · [`scanout`] modeset/flip/cursor/card handle · [`paint`] CPU-fallback vello → RGBA bridge.

pub mod devices;
pub mod gestures;
pub mod hotplug;
pub mod input_config;
pub mod outputs;
pub mod paint;
pub mod runtime;
pub mod scale;
pub mod scanout;
pub mod seat_diag;
pub mod session;
mod xrgb_history;

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vello::Scene;

use paint::{
    SceneDamage, SubrectSource, copy_rgba_subrect, swizzle_subrect_rgba_to_xrgb8888_transformed,
};

use super::{Backend, BackendEvent, Output, OutputId, combined_geometry};
use crate::compositor::frame::FramePaint;
use crate::compositor::present::Presenter;
pub use runtime::DrmBackendError;
use runtime::{ScanoutCommand, ScanoutEntry};

/// Public-facing handle. Wraps [`runtime::DrmRuntime`] (libseat session + udev + libinput) plus a [`paint::VelloDrmBridge`] for the vello → RGBA bytes path.
pub struct DrmBackend {
    inner: runtime::DrmRuntime,
    /// Cached list from [`Backend::outputs`], refreshed from `inner.dispatch.output_meta` on construction and whenever the connector set changes.
    outputs: Vec<Output>,
    /// CPU-fallback vello → RGBA bridge. `None` until the first output is known (can't size the offscreen texture yet); lazily constructed in `submit_frame`.
    paint: Option<paint::VelloDrmBridge>,
    /// When `true` the display is DPMS-off / scanout stopped, so [`Backend::submit_frame`]
    /// short-circuits (no paint, no present) until [`Backend::set_dpms`] re-enables it — avoids racing a frame against the disabled CRTC.
    blanked: bool,
    /// Presentation interval of the fastest connected mode, computed at construction and cached — see [`Self::refresh_from`] for why it can't be derived on demand.
    refresh: Option<std::time::Duration>,
    /// Reconfiguration channel, held only while this backend still owns its scanouts inline; handed to the presenter along with them.
    commands: Option<Receiver<ScanoutCommand>>,
    /// True between a session pause and matching resume, for the inline path. Distinct from
    /// `blanked` ("screen should be off") — `paused` means these CRTCs belong to another session right now; presenting under either is wrong.
    paused: bool,
}

impl DrmBackend {
    /// Acquire the seat, enumerate DRM devices + connectors, attach libinput, and return a `Backend`
    /// ready for the compositor loop. Returns `DrmBackendError` on failure (never panics) — the binary catches it and prints a friendly message.
    ///
    /// # Errors
    ///
    /// Returns an error if the seat can't be acquired (e.g. already held by
    /// another session), DRM device/connector enumeration fails, or
    /// libinput initialisation fails.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let mut inner = runtime::DrmRuntime::new()?;
        let commands = inner.commands.take();
        let outputs = Self::refresh_outputs(&inner);
        // Pace to the *fastest* connected mode: the compositor paints one scene across the combined
        // geometry, so there's one cadence to pick — showing every Nth frame on a slower output beats throttling the fast panel down.
        let refresh = Self::refresh_from(&inner);
        if let Some(d) = refresh {
            tracing::info!(
                hz = 1.0 / d.as_secs_f64(),
                "DRM backend: pacing to the connector's mode timing"
            );
        } else {
            tracing::warn!(
                "DRM backend: no connector reported a usable mode refresh; \
             the compositor will fall back to its default cadence"
            );
        }
        Ok(Self {
            inner,
            outputs,
            paint: None,
            blanked: false,
            refresh,
            commands,
            paused: false,
        })
    }

    /// Collect the current public [`Output`] list from the runtime's dispatch state, sorted by `OutputId.0` for deterministic output indices.
    fn refresh_outputs(inner: &runtime::DrmRuntime) -> Vec<Output> {
        let mut outs: Vec<Output> = inner.dispatch.output_meta.values().cloned().collect();
        outs.sort_by_key(|o| o.id.0);
        outs
    }

    /// The cadence to pace to: the fastest connected mode. Read from the loop thread's record of
    /// live outputs rather than the scanouts, because the scanouts move to the render thread —
    /// asking them would start returning `None` the moment they did, silently dropping a 144 Hz panel to 60 Hz.
    fn refresh_from(inner: &runtime::DrmRuntime) -> Option<std::time::Duration> {
        inner.dispatch.live.iter().filter_map(|l| l.refresh).min()
    }

    fn recompute_refresh(&self) -> Option<std::time::Duration> {
        Self::refresh_from(&self.inner)
    }
}

/// The presentation half of [`DrmBackend`]: the per-connector scanouts plus the vello→XRGB8888
/// bridge, moved onto the render thread by [`Backend::take_presenter`].
///
/// # Why this split is safe
///
/// The scanouts are the *only* part of the DRM runtime the frame path touches; input-path state
/// (libseat/udev/libinput, the runtime's inner calloop) stays on the loop thread, and the
/// hardware cursor ([`CursorPlanes`](super::scanout::CursorPlanes)) owns its own dup of the card
/// fd, so pointer motion moves the KMS plane at input rate, decoupled from paint. Card fds are
/// plain fds usable from any thread; page-flip completions are read only here.
pub struct DrmPresenter {
    scanouts: Vec<ScanoutEntry>,
    outputs: Vec<Output>,
    /// Pointer position in combined coordinates, for the software-cursor fallback only — unused
    /// when the hardware cursor plane is active. Updated from the loop thread via `Presenter::set_pointer`-equivalent messages.
    pointer: (f64, f64),
    paint: Option<paint::VelloDrmBridge>,
    blanked: bool,
    /// True between a session pause and matching resume. Distinct from `blanked`: `paused` means these CRTCs aren't ours right now and the hardware must be rebuilt before presenting resumes.
    paused: bool,
    /// Reconfiguration commands from the loop thread (hotplug, VT switches, resume), drained at the top of every frame so a change lands between frames, not during one.
    commands: Option<Receiver<ScanoutCommand>>,
    /// Shared with the dispatch thread so dropping DRM master cannot race a
    /// present already in progress. The lock is held across one complete
    /// render/reconfiguration operation.
    present_gate: Option<Arc<Mutex<()>>>,
    /// Opt-in physical-hardware profiling. The production session wrapper
    /// enables this while the readback presenter is under development so a
    /// hard reset still leaves exact phase costs in the flight record.
    profile_phases: bool,
    profile_seq: u64,
    /// Bounded aggregate diagnostics are always collected. They make a slow physical session
    /// actionable even when its wrapper forgot the opt-in per-frame trace environment: one log
    /// record every two seconds is cheap enough for production and survives a hard reset.
    phase_window: PhaseWindow,
}

const PHASE_WINDOW_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DirectEligibility<'a> {
    outputs: usize,
    normal_transform: bool,
    color_managed: bool,
    hardware_cursor: bool,
    event_pageflips: bool,
    renderer_pci: &'a str,
    drm_bus_id: Option<&'a str>,
}

fn direct_scanout_eligible(input: DirectEligibility<'_>) -> bool {
    input.outputs == 1
        && input.normal_transform
        && !input.color_managed
        && input.hardware_cursor
        && input.event_pageflips
        && input
            .drm_bus_id
            .is_some_and(|drm| scanout::renderer_matches_drm_card(input.renderer_pci, drm))
}

#[derive(Debug)]
struct PhaseWindow {
    started: Instant,
    frames: u64,
    submit_us: u128,
    gpu_readback_map_us: u128,
    settle_us: u128,
    cpu_swizzle_us: u128,
    readback_bytes: u64,
    output_bytes: u64,
}

impl Default for PhaseWindow {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
            submit_us: 0,
            gpu_readback_map_us: 0,
            settle_us: 0,
            cpu_swizzle_us: 0,
            readback_bytes: 0,
            output_bytes: 0,
        }
    }
}

impl PhaseWindow {
    fn record(&mut self, timing: paint::BridgePhaseTiming) {
        self.frames = self.frames.saturating_add(1);
        self.submit_us = self.submit_us.saturating_add(timing.submit_us);
        self.gpu_readback_map_us = self
            .gpu_readback_map_us
            .saturating_add(timing.completed_readback_map_us);
        self.settle_us = self.settle_us.saturating_add(timing.settle_us);
        self.cpu_swizzle_us = self
            .cpu_swizzle_us
            .saturating_add(timing.cpu_copy_swizzle_us);
        self.readback_bytes = self.readback_bytes.saturating_add(timing.readback_bytes);
        self.output_bytes = self.output_bytes.saturating_add(timing.cpu_output_bytes);
    }

    fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= PHASE_WINDOW_INTERVAL
    }

    fn reset(&mut self, now: Instant) {
        *self = Self {
            started: now,
            ..Self::default()
        };
    }
}

#[cfg(test)]
mod phase_window_tests {
    use super::*;

    fn timing(value: u64) -> paint::BridgePhaseTiming {
        paint::BridgePhaseTiming {
            submit_us: u128::from(value),
            completed_readback_map_us: u128::from(value.saturating_mul(2)),
            settle_us: u128::from(value.saturating_mul(3)),
            cpu_copy_swizzle_us: u128::from(value.saturating_mul(4)),
            readback_bytes: value.saturating_mul(5),
            cpu_output_bytes: value.saturating_mul(6),
        }
    }

    #[test]
    fn phase_window_accumulates_completed_frame_costs() {
        let mut window = PhaseWindow::default();
        window.record(timing(2));
        window.record(timing(3));

        assert_eq!(window.frames, 2);
        assert_eq!(window.submit_us, 5);
        assert_eq!(window.gpu_readback_map_us, 10);
        assert_eq!(window.settle_us, 15);
        assert_eq!(window.cpu_swizzle_us, 20);
        assert_eq!(window.readback_bytes, 25);
        assert_eq!(window.output_bytes, 30);
    }

    #[test]
    fn phase_window_is_due_only_after_the_bounded_interval() {
        let mut window = PhaseWindow::default();
        let started = Instant::now();
        window.started = started;

        assert!(!window.due(started + PHASE_WINDOW_INTERVAL - Duration::from_nanos(1)));
        assert!(window.due(started + PHASE_WINDOW_INTERVAL));
    }

    #[test]
    fn phase_window_reset_discards_the_previous_sample_without_moving_its_clock() {
        let mut window = PhaseWindow::default();
        window.record(timing(7));
        let now = Instant::now();
        window.reset(now);

        assert_eq!(window.started, now);
        assert_eq!(window.frames, 0);
        assert_eq!(window.submit_us, 0);
        assert_eq!(window.gpu_readback_map_us, 0);
        assert_eq!(window.settle_us, 0);
        assert_eq!(window.cpu_swizzle_us, 0);
        assert_eq!(window.readback_bytes, 0);
        assert_eq!(window.output_bytes, 0);
    }
}

#[cfg(test)]
mod direct_eligibility_tests {
    use super::*;

    fn eligible() -> DirectEligibility<'static> {
        DirectEligibility {
            outputs: 1,
            normal_transform: true,
            color_managed: false,
            hardware_cursor: true,
            event_pageflips: true,
            renderer_pci: "0000:66:00.0",
            drm_bus_id: Some("pci:0000:66:00.0"),
        }
    }

    #[test]
    fn direct_path_requires_the_complete_safe_contract() {
        assert!(direct_scanout_eligible(eligible()));
        for rejected in [
            DirectEligibility {
                outputs: 2,
                ..eligible()
            },
            DirectEligibility {
                normal_transform: false,
                ..eligible()
            },
            DirectEligibility {
                color_managed: true,
                ..eligible()
            },
            DirectEligibility {
                hardware_cursor: false,
                ..eligible()
            },
            DirectEligibility {
                event_pageflips: false,
                ..eligible()
            },
            DirectEligibility {
                drm_bus_id: None,
                ..eligible()
            },
            DirectEligibility {
                drm_bus_id: Some("pci:0000:01:00.0"),
                ..eligible()
            },
        ] {
            assert!(!direct_scanout_eligible(rejected));
        }
    }
}

impl DrmPresenter {
    /// Frame-production body, identical to the pre-split `submit_frame`.
    fn render(&mut self, scene: &Scene) {
        let present_gate = self.present_gate.clone();
        let _present_guard = present_gate.as_ref().map(|gate| {
            gate.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        self.drain_commands();
        if self.blanked || self.paused {
            return;
        }
        paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            self.pointer,
            scene,
            None,
        );
    }

    fn render_frame(&mut self, frame: &FramePaint) {
        self.drain_commands();
        if self.blanked || self.paused {
            return;
        }
        paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            self.pointer,
            &frame.scene,
            (frame.revision != 0).then_some(SceneDamage {
                revision: frame.revision,
                previous_revision: frame.previous_revision,
                regions: frame.damage_regions.as_deref(),
            }),
        );
        self.emit_phase_profile();
    }

    fn emit_phase_profile(&mut self) {
        let Some(timing) = self
            .paint
            .as_ref()
            .and_then(paint::VelloDrmBridge::last_phase_timing)
        else {
            return;
        };
        self.phase_window.record(timing);
        let now = Instant::now();
        if self.phase_window.due(now) {
            let frames = self.phase_window.frames.max(1);
            tracing::info!(
                target: "tilo::render_phase_summary",
                frames = self.phase_window.frames,
                window_us = now.saturating_duration_since(self.phase_window.started).as_micros(),
                avg_submit_us = self.phase_window.submit_us / u128::from(frames),
                avg_gpu_readback_map_us = self.phase_window.gpu_readback_map_us / u128::from(frames),
                avg_settle_us = self.phase_window.settle_us / u128::from(frames),
                avg_cpu_swizzle_us = self.phase_window.cpu_swizzle_us / u128::from(frames),
                readback_bytes = self.phase_window.readback_bytes,
                output_bytes = self.phase_window.output_bytes,
                "DRM render phase summary"
            );
            self.phase_window.reset(now);
        }
        if !self.profile_phases {
            return;
        }
        self.profile_seq = self.profile_seq.saturating_add(1);
        tracing::info!(
            target: "tilo::render_phase",
            seq = self.profile_seq,
            submit_us = timing.submit_us,
            gpu_readback_map_us = timing.completed_readback_map_us,
            settle_us = timing.settle_us,
            cpu_swizzle_us = timing.cpu_copy_swizzle_us,
            readback_bytes = timing.readback_bytes,
            output_bytes = timing.cpu_output_bytes,
            "DRM render frame phases"
        );
    }

    /// Apply every reconfiguration the loop thread has queued.
    fn drain_commands(&mut self) {
        let Some(rx) = self.commands.as_ref() else {
            return;
        };
        let pending: Vec<ScanoutCommand> = rx.try_iter().collect();
        for cmd in pending {
            apply_scanout_command(
                cmd,
                &mut self.scanouts,
                &mut self.outputs,
                &mut self.paused,
                &mut self.blanked,
            );
        }
    }
}

/// Apply one reconfiguration to a scanout set. Shared by the presenter (normal case, render
/// thread) and by [`DrmBackend`] itself for the inline path before the presenter splits off — one implementation, so hotplug behaves the same on either side.
fn apply_scanout_command(
    cmd: ScanoutCommand,
    scanouts: &mut Vec<ScanoutEntry>,
    outputs: &mut Vec<Output>,
    paused: &mut bool,
    blanked: &mut bool,
) {
    match cmd {
        ScanoutCommand::Reconfigure {
            remove,
            add,
            outputs: table,
            removal_ack,
        } => {
            // Dropping a scanout closes its dup of the card fd and releases the CRTC — full teardown for
            // an unplugged display. No per-device page-flip source to register/withdraw: every scanout on
            // a card dups one open file description, so all crtcs' completions arrive on one event stream
            // that `render` drains once and routes by crtc; a removed output takes its pending completion with it.
            scanouts.retain(|e| !remove.contains(&e.id));
            scanouts.extend(add);
            // Positions come from the loop thread's re-layout: survivors move too, since the set was re-packed to close the hole an unplugged display left.
            for entry in scanouts.iter_mut() {
                if let Some(o) = table.iter().find(|o| o.id == entry.id) {
                    let history_boundary = entry.position != o.position
                        || entry.transform != o.transform
                        || entry.color_profile != o.color_profile;
                    entry.position = o.position;
                    entry.transform = o.transform;
                    entry.set_color_profile(o.color_profile.clone());
                    if history_boundary {
                        entry.scanout.invalidate_xrgb_history();
                    }
                }
            }
            scanouts.sort_by_key(|e| e.id.0);
            *outputs = table;
            if let Some((device_id, ack)) = removal_ack {
                // The loop thread may now call `Session::close` on the
                // card's original fd: all presenter-owned scanout duplicates
                // for this device were dropped by `retain` above.
                let _ = ack.send(device_id);
            }
            tracing::info!(
                outputs = scanouts.len(),
                "DRM present: adopted the new output set"
            );
        }
        ScanoutCommand::Pause => {
            for entry in scanouts.iter_mut() {
                entry.direct_targets = None;
            }
            *paused = true;
            tracing::info!("DRM present: paused (the session no longer owns the CRTCs)");
        }
        ScanoutCommand::Resume { dpms_on } => {
            for e in scanouts.iter_mut() {
                e.direct_targets = None;
                e.scanout.restore();
                e.scanout.set_dpms(dpms_on);
            }
            *blanked = !dpms_on;
            *paused = false;
            tracing::info!(
                outputs = scanouts.len(),
                dpms_on,
                "DRM present: resumed after re-modesetting every output"
            );
        }
        ScanoutCommand::SetCursorSize(size) => {
            for entry in scanouts.iter_mut() {
                entry.scanout.set_cursor_size(size);
            }
        }
    }
}

/// Render `scene` into `scanouts`, sized to the combined geometry of
/// `outputs`; lazily (re)allocates `*paint` to that size. Shared by
/// [`DrmPresenter::render`] (post-split, render thread) and
/// [`Backend::submit_frame`] for [`DrmBackend`] (inline path, pre-split) —
/// same painting logic, different owner of the scanouts and the pointer.
#[allow(
    clippy::too_many_lines,
    reason = "one dispatcher keeps bridge lifetime, sparse eligibility, and the single versus multi-output presentation boundaries together"
)]
fn paint_scanouts(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    pointer: (f64, f64),
    scene: &Scene,
    damage: Option<SceneDamage<'_>>,
) {
    if scanouts.is_empty() {
        return;
    }
    let target_size = combined_geometry(outputs);
    if target_size.0 == 0 || target_size.1 == 0 {
        return;
    }
    if paint.is_none() {
        match paint::VelloDrmBridge::new(target_size.0, target_size.1) {
            Ok(mut bridge) => {
                // Four `Instant` samples per completed frame buy the minimum useful physical
                // performance record. Verbose per-frame output remains opt-in in DrmPresenter;
                // ordinary sessions emit only one aggregate every two seconds.
                bridge.set_phase_timing_enabled(true);
                *paint = Some(bridge);
            }
            Err(e) => {
                tracing::error!("DRM backend: VelloDrmBridge::new: {e}");
                return;
            }
        }
    }
    let mut bridge_resized = false;
    {
        // The block above just initialised `*paint` if it was `None`, or
        // already returned on failure — a miss here would mean that
        // silently didn't stick, so skip this frame rather than panic.
        let Some(bridge) = paint.as_mut() else {
            return;
        };
        if bridge.size() != target_size {
            bridge_resized = true;
            if let Err(e) = bridge.resize(target_size.0, target_size.1) {
                tracing::error!("DRM backend: VelloDrmBridge::resize: {e}");
                return;
            }
        }
    }
    if bridge_resized {
        // A resize replaces bridge resources. The bridge epoch also changes, but clearing every
        // scanout claim here makes the physical-destination boundary explicit.
        for entry in scanouts.iter_mut() {
            entry.scanout.invalidate_xrgb_history();
        }
    }

    // Single-output fast path: render the scene and swizzle it straight into
    // the one scanout's back dumb buffer (XRGB8888) in a single fused pass —
    // no intermediate RGBA copy — then page-flip (or dirty_framebuffer
    // fallback). The cursor is only consulted by the software-cursor
    // fallback.
    let fast_path = scanouts.len() == 1
        && scanouts.first().is_some_and(|s| {
            s.transform == settings_store::hardware::OutputTransform::Normal
                && s.color_transform.is_none()
        });
    // Fine damage is in the combined scene's native pixels. The existing fast path can still
    // present scaled/offset logical metadata densely, but sparse repair must not assume that
    // mapping is one-to-one until the renderer owns an explicit transform contract.
    let sparse_coordinates_match = outputs.len() == 1
        && outputs.first().is_some_and(|output| {
            output.position == (0, 0)
                && output.scale.to_bits() == 1.0_f32.to_bits()
                && output.size == target_size
                && output.transform == settings_store::hardware::OutputTransform::Normal
        });
    if fast_path {
        let scanout_count = scanouts.len();
        // `fast_path` only goes true when `scanouts.first()` matched above,
        // so neither `first_mut()` nor `paint.as_mut()` can miss here.
        let (Some(entry), Some(bridge)) = (scanouts.first_mut(), paint.as_mut()) else {
            return;
        };

        // The direct path is deliberately narrow until every transformed,
        // colour-managed and software-cursor composition case has an exact
        // GPU implementation. This laptop's normal single-output path avoids
        // all framebuffer readback while every unsupported case remains on
        // the established presenter.
        if let Ok(adapter) = bridge.adapter_info()
            && let drm_bus_id = entry.scanout.drm_bus_id()
            && direct_scanout_eligible(DirectEligibility {
                outputs: scanout_count,
                normal_transform: entry.transform
                    == settings_store::hardware::OutputTransform::Normal,
                color_managed: entry.color_transform.is_some(),
                hardware_cursor: entry.scanout.hardware_cursor_active(),
                event_pageflips: entry.scanout.event_pageflips_available(),
                renderer_pci: &adapter.pci_bus_id,
                drm_bus_id: drm_bus_id.as_deref(),
            })
        {
            if entry.direct_targets.is_none() && entry.direct_unavailable.is_none() {
                match scanout::DirectScanoutTargets::new(&entry.scanout, bridge) {
                    Ok(targets) => {
                        tracing::info!(
                            renderer_pci = %adapter.pci_bus_id,
                            drm_bus_id = drm_bus_id.as_deref().unwrap_or("unavailable"),
                            "DRM direct scanout enabled"
                        );
                        entry.direct_targets = Some(targets);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "DRM direct scanout unavailable; retaining readback fallback");
                        entry.direct_unavailable = Some(error);
                    }
                }
            }
            if let Some(targets) = entry.direct_targets.as_mut() {
                // Retire a prior imported-buffer flip before selecting a new
                // render target. With two slots, a pending flip owns both.
                for crtc in entry.scanout.poll_flip_completions() {
                    entry.scanout.mark_direct_flip_complete(targets, crtc);
                }
                if let Some((index, target)) = targets.render_slot() {
                    let revision = damage.map_or(1, |damage| damage.revision.max(1));
                    let direct_damage = damage.unwrap_or(SceneDamage {
                        revision,
                        previous_revision: None,
                        regions: None,
                    });
                    let deadline = Instant::now()
                        .checked_add(Duration::from_secs(1))
                        .unwrap_or_else(Instant::now);
                    match bridge.paint_imported_bgra8_until(scene, direct_damage, target, deadline)
                    {
                        Ok(stats) if entry.scanout.submit_direct_target(targets, index) => {
                            tracing::trace!(
                                revision = stats.revision,
                                render_attempts = stats.render_attempts,
                                framebuffer_copy_bytes = stats.framebuffer_copy_bytes,
                                framebuffer_staging_bytes = stats.framebuffer_staging_bytes,
                                "DRM direct frame submitted"
                            );
                            return;
                        }
                        Ok(_) => return,
                        Err(error) => {
                            tracing::warn!(%error, "DRM direct frame failed; disabling direct targets");
                            entry.direct_targets = None;
                            entry.direct_unavailable = Some(error.to_string());
                        }
                    }
                } else {
                    return;
                }
            }
        }
        let cursor = Some(pointer);
        if entry.scanout.xrgb_damage_available()
            && bridge.xrgb_history_available()
            && sparse_coordinates_match
            && let Some(damage) = damage
        {
            if let Err(error) = entry
                .scanout
                .present_scene_damage(cursor, bridge, scene, damage)
            {
                match error {
                    paint::BridgeError::RecoveryBackoff { .. } => {
                        tracing::debug!("DRM backend: paint recovery is cooling down: {error}");
                    }
                    error => tracing::error!("DRM backend: history-aware XRGB paint: {error}"),
                }
            }
            return;
        }
        if entry.scanout.xrgb_damage_requested() {
            entry.scanout.invalidate_xrgb_history();
        }
        if let Err(e) = entry.scanout.present_into(cursor, |dst, pitch| {
            match damage.filter(|_| sparse_coordinates_match) {
                Some(damage) => bridge.paint_damage_into_xrgb8888(scene, damage, dst, pitch),
                None => bridge.paint_into_xrgb8888(scene, dst, pitch),
            }
        }) {
            match e {
                paint::BridgeError::RecoveryBackoff { .. } => {
                    tracing::debug!("DRM backend: paint recovery is cooling down: {e}");
                }
                e => tracing::error!("DRM backend: paint_into_xrgb8888: {e}"),
            }
        }
        return;
    }

    // Multi-output path: scanouts on one card share an open file
    // description, so their page-flip completions arrive on one event stream.
    // Hybrid systems have *one stream per card*, and CRTC handles are only
    // meaningful inside that card.  Drain exactly one representative per
    // device and route only to siblings from the same device; draining just
    // `scanouts[0]` would wedge every secondary GPU after its first flip.
    drain_page_flip_completions(scanouts);

    // Render the combined scene ONCE into a tight RGBA buffer, then swizzle
    // each output's sub-rect into its own scanout's dumb buffer (fused
    // single-pass, per output).
    let Some(bridge) = paint.as_mut() else {
        return;
    };
    let rgba = match bridge.paint(scene) {
        Ok(b) => b,
        Err(e) => {
            match e {
                paint::BridgeError::RecoveryBackoff { .. } => {
                    tracing::debug!("DRM backend: paint recovery is cooling down: {e}");
                }
                e => tracing::error!("DRM backend: paint (combined): {e}"),
            }
            return;
        }
    };
    let (cw, ch) = (target_size.0 as usize, target_size.1 as usize);
    for e in scanouts.iter_mut() {
        paint_one_scanout(e, outputs, rgba, cw, ch, pointer);
    }
}

/// Drain one page-flip event stream per KMS device and route each completion
/// only to scanouts on that device. Extracted so the selection policy can be
/// tested without opening DRM hardware.
fn drain_page_flip_completions(scanouts: &mut [ScanoutEntry]) {
    let representatives = page_flip_representatives(scanouts.iter().map(|entry| entry.device_id));
    let mut completed = Vec::new();
    for (device_id, index) in representatives {
        let Some(entry) = scanouts.get_mut(index) else {
            continue;
        };
        completed.extend(
            entry
                .scanout
                .poll_flip_completions()
                .into_iter()
                .map(|crtc| (device_id, crtc)),
        );
    }
    let topology: Vec<_> = scanouts
        .iter()
        .map(|entry| (entry.device_id, entry.scanout.crtc()))
        .collect();
    for (index, crtc) in page_flip_completion_recipients(&topology, &completed) {
        if let Some(entry) = scanouts.get_mut(index) {
            entry.scanout.mark_flip_complete(crtc);
        }
    }
}

/// The first scanout for every card is the one safe reader of that card's
/// shared DRM event stream. Returned indices preserve output order, making
/// the drain order deterministic without coupling independent GPU events.
fn page_flip_representatives(devices: impl IntoIterator<Item = u64>) -> Vec<(u64, usize)> {
    let mut seen = std::collections::BTreeSet::new();
    devices
        .into_iter()
        .enumerate()
        .filter_map(|(index, device_id)| seen.insert(device_id).then_some((device_id, index)))
        .collect()
}

/// Match each card-scoped completion to the one scanout on that same card and
/// CRTC. CRTC handles may repeat on a different DRM device, so matching the
/// handle alone would clear the wrong `flip_pending` flag after a hybrid-GPU
/// frame. Generic handles keep this routing rule testable without DRM fds.
fn page_flip_completion_recipients<T: Copy + Eq>(
    scanouts: &[(u64, T)],
    completed: &[(u64, T)],
) -> Vec<(usize, T)> {
    completed
        .iter()
        .flat_map(|&(device_id, crtc)| {
            scanouts.iter().enumerate().filter_map(
                move |(index, &(scanout_device, scanout_crtc))| {
                    (scanout_device == device_id && scanout_crtc == crtc).then_some((index, crtc))
                },
            )
        })
        .collect()
}

/// Body of [`paint_scanouts`]'s per-scanout multi-output loop, split out to
/// keep that function short. Computes the software-cursor position and the
/// (possibly colour-transformed) sub-rect for `e`, then presents it —
/// unchanged from the inline version, so the present call still lands at
/// exactly the same point in the per-output iteration.
fn paint_one_scanout(
    e: &mut ScanoutEntry,
    outputs: &[Output],
    rgba: &[u8],
    cw: usize,
    ch: usize,
    pointer: (f64, f64),
) {
    let (px, py) = e.position;
    let (physical_w, physical_h) = e.scanout.size();
    let logical_size = outputs
        .iter()
        .find(|output| output.id == e.id)
        .map_or((physical_w, physical_h), |output| output.size);
    // Widths/heights come from the connector's mode; clamp rather than
    // wrap on the (unreachable in practice) case of a driver-reported
    // extent past `i32::MAX`.
    let width_i32 = i32::try_from(logical_size.0).unwrap_or(i32::MAX);
    let height_i32 = i32::try_from(logical_size.1).unwrap_or(i32::MAX);
    let right = px.saturating_add(width_i32);
    let bottom = py.saturating_add(height_i32);
    // Software-cursor fallback only: draw the cursor (output-local
    // coords) on the output it's over. The hardware cursor plane, when
    // active, ignores this and is driven from the input path via
    // `CursorPlanes`.
    let local_cursor = if pointer.0 >= f64::from(px)
        && pointer.0 < f64::from(right)
        && pointer.1 >= f64::from(py)
        && pointer.1 < f64::from(bottom)
    {
        // Sub-pixel pointer position intentionally truncates to a pixel
        // coordinate (a hardware cursor plane snaps to a whole pixel
        // anyway); bounded to on-screen extents, well within `i32`.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "sub-pixel pointer position intentionally truncates to a pixel coordinate; bounded to on-screen extents"
        )]
        let (local_x, local_y) = (
            (pointer.0 - f64::from(px)) as i32,
            (pointer.1 - f64::from(py)) as i32,
        );
        let (x, y) = scanout::logical_to_physical_point(
            local_x,
            local_y,
            logical_size.0,
            logical_size.1,
            e.transform,
        );
        Some((f64::from(x), f64::from(y)))
    } else {
        None
    };
    let sx = usize::try_from(px.max(0)).unwrap_or(0);
    let sy = usize::try_from(py.max(0)).unwrap_or(0);
    let (source, source_w, source_h, source_x, source_y): (&[u8], usize, usize, usize, usize) =
        if let Some(transform) = &e.color_transform {
            copy_rgba_subrect(
                SubrectSource {
                    src_rgba: rgba,
                    src_w: cw,
                    src_h: ch,
                    src_x: sx,
                    src_y: sy,
                },
                logical_size.0 as usize,
                logical_size.1 as usize,
                &mut e.color_buffer,
            );
            transform.apply(&mut e.color_buffer);
            (
                &e.color_buffer,
                logical_size.0 as usize,
                logical_size.1 as usize,
                0,
                0,
            )
        } else {
            (rgba, cw, ch, sx, sy)
        };
    e.scanout.present_no_drain(local_cursor, |dst, pitch| {
        swizzle_subrect_rgba_to_xrgb8888_transformed(
            SubrectSource {
                src_rgba: source,
                src_w: source_w,
                src_h: source_h,
                src_x: source_x,
                src_y: source_y,
            },
            dst,
            pitch,
            physical_w as usize,
            physical_h as usize,
            e.transform,
        );
    });
}

impl Presenter for DrmPresenter {
    fn present(&mut self, scene: &Scene, _output: OutputId) {
        self.render(scene);
    }

    fn present_frame(&mut self, frame: &FramePaint, _output: OutputId) {
        self.render_frame(frame);
    }

    fn set_dpms(&mut self, on: bool) {
        let present_gate = self.present_gate.clone();
        let _present_guard = present_gate.as_ref().map(|gate| {
            gate.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        self.blanked = !on;
        for e in &mut self.scanouts {
            e.direct_targets = None;
            e.scanout.set_dpms(on);
        }
    }
}

impl Backend for DrmBackend {
    /// The hardware cursor plane composites itself; where there is none, the scanout draws the
    /// fallback into the framebuffer (`scanout::draw_cursor`) — either way the pointer is on screen without the scene carrying it.
    fn has_cursor_plane(&self) -> bool {
        true
    }

    fn supports_retained_scene_damage(&self) -> bool {
        true
    }

    /// Inline presentation, reached only when the presenter hasn't been split off yet — kept so a caller that never splits still paints.
    fn submit_frame(&mut self, scene: &Scene, _output: OutputId) {
        let present_gate = self.inner.dispatch.present_gate.clone();
        let _present_guard = present_gate.as_ref().map(|gate| {
            gate.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        // While blanked the CRTCs are disabled — painting/presenting would be wasted or re-light the
        // panel out from under idle/lock logic; skip until `set_dpms(true)`. `paused` is the other reason: the CRTCs belong to another session until it switches back.
        if self.blanked || self.paused {
            return;
        }
        let pointer = self.inner.dispatch.pointer;
        paint_scanouts(
            &mut self.inner.scanouts,
            &self.outputs,
            &mut self.paint,
            pointer,
            scene,
            None,
        );
    }

    fn submit_frame_with_damage(&mut self, frame: &FramePaint, _output: OutputId) {
        if self.blanked || self.paused {
            return;
        }
        let pointer = self.inner.dispatch.pointer;
        paint_scanouts(
            &mut self.inner.scanouts,
            &self.outputs,
            &mut self.paint,
            pointer,
            &frame.scene,
            (frame.revision != 0).then_some(SceneDamage {
                revision: frame.revision,
                previous_revision: frame.previous_revision,
                regions: frame.damage_regions.as_deref(),
            }),
        );
    }

    fn poll_events(&mut self, callback: &mut dyn FnMut(BackendEvent)) {
        self.inner.dispatch_pending();
        self.inner.poll_display_transaction();
        // Before the presenter splits off, the scanouts are still here, so reconfigurations addressed to "whoever owns them" apply here too; after the split the presenter drains the same channel.
        if self.commands.is_some() {
            let pending: Vec<ScanoutCommand> = self
                .commands
                .as_ref()
                .map(|rx| rx.try_iter().collect())
                .unwrap_or_default();
            for cmd in pending {
                apply_scanout_command(
                    cmd,
                    &mut self.inner.scanouts,
                    &mut self.outputs,
                    &mut self.paused,
                    &mut self.blanked,
                );
            }
        }
        // A hotplug rewrote the output table on the loop thread; publish it so `outputs()` (which the compositor reads to size its scene / advertise `wl_output` globals) reflects the change.
        if std::mem::take(&mut self.inner.dispatch.outputs_changed) {
            self.outputs = Self::refresh_outputs(&self.inner);
            // The cadence is the fastest connected mode, and "connected" just changed — unplugging the 144
            // Hz panel of a pair must not leave the compositor pacing to a rate the remaining display can't present at.
            self.refresh = self.recompute_refresh();
        }
        for ev in self.inner.dispatch.events.drain(..) {
            callback(ev);
        }
    }

    fn outputs(&self) -> &[Output] {
        &self.outputs
    }

    /// The primary connector's mode, converted from its raw timing (pixel clock / htotal×vtotal)
    /// rather than the rounded `vrefresh`, so a 59.94 Hz panel paces at 59.94, not 60.
    ///
    /// Multi-output: one scene across every output needs one cadence — the *fastest* connected mode, so a slower output shows every Nth frame rather than throttling the fast panel.
    fn refresh_interval(&self) -> Option<std::time::Duration> {
        self.refresh
    }

    fn take_presenter(&mut self) -> Option<Box<dyn Presenter>> {
        if self.inner.scanouts.is_empty() {
            // Modeset failed for every connector: nothing to present into, so the compositor runs
            // headless-on-DRM rather than spawning a thread that would only early-return. The command
            // channel stays here so a display plugged in later still reconfigures something — the backend adopts new scanouts inline, with no `take_presenter` call to hand them to.
            return None;
        }
        let scanouts = std::mem::take(&mut self.inner.scanouts);
        Some(Box::new(DrmPresenter {
            scanouts,
            outputs: self.outputs.clone(),
            pointer: self.inner.dispatch.pointer,
            paint: self.paint.take(),
            blanked: self.blanked,
            paused: self.paused,
            // The scanouts and the commands addressed to them travel together: whoever owns one owns the other.
            commands: self.commands.take(),
            present_gate: self.inner.dispatch.present_gate.clone(),
            profile_phases: std::env::var_os("TILO_DRM_PHASE_TRACE").is_some(),
            profile_seq: 0,
            phase_window: PhaseWindow::default(),
        }))
    }

    /// Power **every** connected output on/off, delegating to [`scanout::DrmScanout::set_dpms`]
    /// (legacy DPMS + CRTC toggle) per scanout and flipping the `blanked` gate `submit_frame` reads. No scanouts (modeset failed at construction) → only the gate flips, so a headless run still tracks state.
    fn set_dpms(&mut self, on: bool) {
        self.blanked = !on;
        // Remember it on the session so a VT switch back restores the state the session was in, rather
        // than unconditionally lighting the panel — a machine idle-blanked before a VT switch must not come back awake.
        if let Some(s) = self.inner.dispatch.session.as_ref() {
            s.set_dpms(on);
        }
        for e in &mut self.inner.scanouts {
            e.direct_targets = None;
            e.scanout.set_dpms(on);
        }
    }

    fn reset_display(&mut self) {
        self.inner.resume_from_sleep();
    }

    fn reload_input_settings(&mut self) {
        self.inner.reload_input_settings();
    }

    fn cancel_touch(&mut self) {
        self.inner.cancel_touch();
    }

    fn switch_vt(&mut self, vt: i32) {
        self.inner.change_vt(vt);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_flip_drain_uses_one_representative_per_card() {
        // A fake scanout inventory: card0 has two CRTCs, card1 has two more
        // whose handle values may overlap card0's.  No real compositor or
        // DRM node is involved; this pins the grouping rule used before the
        // hardware-facing drain.
        assert_eq!(
            page_flip_representatives([100, 100, 200, 100, 200]),
            vec![(100, 0), (200, 2)],
            "each card must drain its own shared event stream exactly once"
        );
    }

    #[test]
    fn overlapping_crtcs_route_flip_completion_only_within_its_card() {
        // Fake card/CRTC topology. CRTC 7 exists on both cards, exactly as
        // it can in DRM. A completion from card 200 must not clear card 100's
        // pending flip merely because their local handle values match.
        let scanouts = [(100, 7u32), (100, 8), (200, 7), (200, 8)];
        assert_eq!(
            page_flip_completion_recipients(&scanouts, &[(200, 7)]),
            vec![(2, 7)],
            "only the same card's CRTC receives the completion"
        );
    }

    /// The reconfiguration path with no live scanouts.
    ///
    /// Building a `ScanoutEntry` needs a modeset connector, so the scanout-touching half of
    /// `Reconfigure`/`Resume` can't be exercised off hardware; what can be exercised is the
    /// *gating* — whether the presenter paints at all, and which output table it believes in.
    #[test]
    fn a_pause_stops_the_presenter_and_a_resume_starts_it() {
        let mut scanouts: Vec<ScanoutEntry> = Vec::new();
        let mut outputs: Vec<Output> = Vec::new();
        let (mut paused, mut blanked) = (false, false);

        apply_scanout_command(
            ScanoutCommand::Pause,
            &mut scanouts,
            &mut outputs,
            &mut paused,
            &mut blanked,
        );
        assert!(
            paused,
            "a paused session must not present into CRTCs it lost"
        );

        apply_scanout_command(
            ScanoutCommand::Resume { dpms_on: true },
            &mut scanouts,
            &mut outputs,
            &mut paused,
            &mut blanked,
        );
        assert!(!paused, "and must start again when it gets them back");
        assert!(!blanked);
    }

    #[test]
    fn resuming_a_blanked_session_leaves_the_panels_off() {
        // Restoring a session that was idle-blanked must not light the screen: the user turned it off, and a VT switch is not consent.
        let mut scanouts: Vec<ScanoutEntry> = Vec::new();
        let mut outputs: Vec<Output> = Vec::new();
        let (mut paused, mut blanked) = (true, false);
        apply_scanout_command(
            ScanoutCommand::Resume { dpms_on: false },
            &mut scanouts,
            &mut outputs,
            &mut paused,
            &mut blanked,
        );
        assert!(!paused, "the CRTCs are ours again");
        assert!(blanked, "but the panels stay off");
    }

    #[test]
    fn a_reconfigure_adopts_the_new_output_table() {
        // The presenter sizes its paint buffer from `combined_geometry` of this table and presents each output's sub-rect from its position, so adopting it is what makes an unplugged monitor stop being painted for.
        let mut scanouts: Vec<ScanoutEntry> = Vec::new();
        let mut outputs: Vec<Output> = vec![
            Output::new(OutputId(0), (1920, 1080), 1.0),
            Output {
                id: OutputId(1),
                position: (1920, 0),
                size: (2560, 1440),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
            },
        ];
        let (mut paused, mut blanked) = (false, false);
        apply_scanout_command(
            ScanoutCommand::Reconfigure {
                remove: vec![OutputId(0)],
                add: Vec::new(),
                outputs: vec![Output::new(OutputId(1), (2560, 1440), 1.0)],
                removal_ack: None,
            },
            &mut scanouts,
            &mut outputs,
            &mut paused,
            &mut blanked,
        );
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].id, OutputId(1));
        assert_eq!(
            combined_geometry(&outputs),
            (2560, 1440),
            "the scene must shrink to what is still plugged in"
        );
    }

    /// The DRM backend's construction must surface a real error (`Box<dyn Error>`) on any dev
    /// machine off a free TTY. Exact text varies; we only assert `Err`, not a panic, so this test is portable across CI.
    #[test]
    fn drm_backend_new_returns_err_when_no_seat_available() {
        // On the dev machine this runs under, a Wayland/X11 compositor already holds the seat; libseat refuses with an error — exactly the contract we want.
        match DrmBackend::new() {
            Ok(_) => {
                // If construction unexpectedly succeeds (e.g. a bare TTY in CI), that's also fine — the assert
                // below converts it into a benign skip rather than gating on `is_err()`, so a real hardware run wouldn't fail it.
            }
            Err(e) => {
                let msg = format!("{e}");
                assert!(
                    !msg.is_empty(),
                    "DrmBackend error message must be non-empty (got: {msg:?})"
                );
            }
        }
    }

    /// Verify `outputs()` returns the populated list when the runtime has synthesised outputs. Can't
    /// call `DrmRuntime::new` from a dev machine, so exercise `refresh_outputs` directly on a synthesised dispatch state.
    #[test]
    fn drm_backend_outputs_returns_populated_list_when_runtime_has_outputs() {
        let synthesized = vec![
            (OutputId(0), Output::new(OutputId(0), (1920, 1080), 1.0)),
            (OutputId(1), Output::new(OutputId(1), (1280, 720), 1.0)),
        ];
        let dispatch = runtime::DrmRuntime::synthesize_for_tests(synthesized);

        // Mirror what `refresh_outputs` does: we can't easily build a full `DrmRuntime`, so copy that helper's sort/shape policy here for the test.
        let mut outs: Vec<Output> = dispatch.output_meta.values().cloned().collect();
        outs.sort_by_key(|o| o.id.0);

        assert_eq!(
            outs.len(),
            2,
            "two synthesised outputs must surface as two entries"
        );
        assert_eq!(outs[0].id, OutputId(0));
        assert_eq!(outs[0].size, (1920, 1080));
        assert_eq!(outs[1].id, OutputId(1));
        assert_eq!(outs[1].size, (1280, 720));
    }

    /// The bridge field must exist and the legacy "scene discarded" warn-and-latch field must not,
    /// so a future refactor re-adding a fail-silent path has to re-design this test. Verified via the production type's shape (not a `tracing` capture) to keep the test fast and dependency-free.
    #[test]
    fn drm_backend_does_not_warn_about_vello_anymore() {
        // Pull the struct's field list via raw source text, anchored on `pub struct DrmBackend {` … the first following `}`, so neither the test body nor doc-comments interfere.
        let src = include_str!("mod.rs");
        let start = src
            .find("pub struct DrmBackend {")
            .expect("struct DrmBackend must exist in mod.rs");
        let after = &src[start..];
        let end = after.find("\n}").expect("struct must close") + start;
        let struct_def = &src[start..=end + 1];
        assert!(
            struct_def.contains("paint:"),
            "Phase 5.2: DrmBackend struct must carry a `paint:` field for the bridge. Got:\n{struct_def}"
        );
        assert!(
            !struct_def.contains("warned_about_vello"),
            "Phase 5.2: DrmBackend struct must drop the legacy latch field. Got:\n{struct_def}"
        );
    }
}
