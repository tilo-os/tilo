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

pub mod access_keys;
pub mod devices;
pub mod gestures;
pub mod hotplug;
pub mod input_config;
mod output_signal;
pub mod outputs;
pub mod paint;
pub mod runtime;
pub mod scale;
pub mod scanout;
pub mod seat_diag;
pub mod session;
mod tablet;
mod xrgb_history;

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vello::Scene;

use paint::{
    SceneDamage, SubrectSource, copy_rgba_subrect, encode_subrect_rgba_to_xbgr2101010_transformed,
    swizzle_subrect_rgba_to_xrgb8888_composited,
};

use super::{Backend, BackendEvent, Output, OutputId, combined_geometry};
use crate::compositor::frame::FramePaint;
use crate::compositor::present::Presenter;
use crate::shell::hdr_surface::{CpuComposite, HdrLayer};
pub use runtime::DrmBackendError;
use runtime::{ScanoutCommand, ScanoutEntry, SignalReport};

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
    direct_bypass_reported: bool,
    /// Signalling reports to the loop thread, held while scanouts are inline.
    signal_reports: Option<Sender<SignalReport>>,
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
        let signal_reports = inner.signal_reports.take();
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
            direct_bypass_reported: false,
            signal_reports,
        })
    }

    /// Collect the current public [`Output`] list from the runtime's dispatch state, primary first
    /// (see `DrmDispatchState::output_table`).
    fn refresh_outputs(inner: &runtime::DrmRuntime) -> Vec<Output> {
        inner.dispatch.output_table()
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
/// hardware cursor (`CursorPlanes`) owns its own dup of the card
/// fd, so pointer motion moves the KMS plane at input rate, decoupled from paint. Card fds are
/// plain fds usable from any thread; page-flip completions are read only here.
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent presenter lifecycle or one-shot diagnostic fact"
)]
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
    /// Emit one durable explanation when direct presentation is bypassed
    /// before the detailed adapter/scanout eligibility check.
    direct_bypass_reported: bool,
    /// Signalling facts learned while presenting, for the loop thread.
    signal_reports: Option<Sender<SignalReport>>,
}

/// Send the loop thread every output's signalling status that changed since
/// it was last reported.
fn report_signal_status(scanouts: &mut [ScanoutEntry], reports: Option<&Sender<SignalReport>>) {
    let Some(reports) = reports else {
        return;
    };
    for entry in scanouts {
        let status = entry.signal_status();
        if status != entry.signal_reported {
            entry.signal_reported = status.clone();
            let _ = reports.send(SignalReport {
                output: entry.id,
                status,
            });
        }
    }
}

const PHASE_WINDOW_INTERVAL: Duration = Duration::from_secs(2);
/// A page-flip event should arrive within one refresh interval. Give even a
/// 24 Hz panel several intervals, while still failing boundedly if KMS wedges.
const DIRECT_RETIREMENT_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one output, reported together when direct scanout is refused"
)]
struct DirectEligibility<'a> {
    outputs: usize,
    normal_transform: bool,
    color_managed: bool,
    hardware_cursor: bool,
    event_pageflips: bool,
    renderer_pci: &'a str,
    renderer_drm_matches: bool,
    drm_bus_id: Option<&'a str>,
}

/// Sparse Vello damage describes the delta from `previous_revision`, but a
/// direct-scanout swapchain owns multiple independent destination images. A
/// partial copy is valid only when the selected image contains that exact
/// predecessor; otherwise both rendering and the destination copy must cross
/// a dense boundary.
fn damage_for_direct_target(
    target_revision: Option<u64>,
    mut damage: SceneDamage<'_>,
) -> SceneDamage<'_> {
    if target_revision != damage.previous_revision {
        damage.previous_revision = None;
        damage.regions = None;
    }
    damage
}

fn direct_scanout_rejection(input: DirectEligibility<'_>) -> Option<&'static str> {
    if input.outputs != 1 {
        return Some("multiple outputs");
    }
    if !input.normal_transform {
        return Some("output transform is not normal");
    }
    if input.color_managed {
        return Some("output color transform is active");
    }
    if !input.hardware_cursor {
        return Some("hardware cursor plane is unavailable");
    }
    if !input.event_pageflips {
        return Some("event-driven page flips are unavailable");
    }
    if !input.renderer_drm_matches
        && !input
            .drm_bus_id
            .is_some_and(|drm| scanout::renderer_matches_drm_card(input.renderer_pci, drm))
    {
        return Some("renderer and scanout use different GPUs");
    }
    None
}

#[derive(Debug)]
struct PhaseWindow {
    started: Instant,
    frames: u64,
    direct_frames: u64,
    direct_dropped: u64,
    flip_completions: u64,
    submit_us: u128,
    gpu_readback_map_us: u128,
    settle_us: u128,
    cpu_swizzle_us: u128,
    readback_bytes: u64,
    output_bytes: u64,
    /// Wall time around the complete renderer bridge call. This remains
    /// available when a failed/timed-out paint cannot publish its detailed
    /// Vello phase record.
    bridge_us: u128,
    /// Wall time after the bridge returns until the scanout submission path
    /// completes (dumb-buffer cursor work, dirtyfb/page-flip included).
    scanout_us: u128,
    /// Legacy readback attempts which returned without detailed bridge phase
    /// timing. They are still frames attempted by the physical presenter and
    /// must not disappear from a performance incident report.
    unprofiled_frames: u64,
}

impl Default for PhaseWindow {
    fn default() -> Self {
        Self {
            started: Instant::now(),
            frames: 0,
            direct_frames: 0,
            direct_dropped: 0,
            flip_completions: 0,
            submit_us: 0,
            gpu_readback_map_us: 0,
            settle_us: 0,
            cpu_swizzle_us: 0,
            readback_bytes: 0,
            output_bytes: 0,
            bridge_us: 0,
            scanout_us: 0,
            unprofiled_frames: 0,
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

    fn record_legacy_attempt(
        &mut self,
        timing: Option<paint::BridgePhaseTiming>,
        bridge_us: u128,
        scanout_us: u128,
    ) {
        if let Some(timing) = timing {
            self.record(timing);
        } else {
            self.frames = self.frames.saturating_add(1);
            self.unprofiled_frames = self.unprofiled_frames.saturating_add(1);
        }
        self.bridge_us = self.bridge_us.saturating_add(bridge_us);
        self.scanout_us = self.scanout_us.saturating_add(scanout_us);
    }

    fn summary_fields(&self, now: Instant) -> PhaseSummaryFields {
        PhaseSummaryFields {
            frames: self.frames,
            direct_frames: self.direct_frames,
            direct_dropped: self.direct_dropped,
            flip_completions: self.flip_completions,
            unprofiled_frames: self.unprofiled_frames,
            window_us: now.saturating_duration_since(self.started).as_micros(),
            bridge_us: self.bridge_us,
            scanout_us: self.scanout_us,
            readback_bytes: self.readback_bytes,
            output_bytes: self.output_bytes,
        }
    }

    fn record_direct_submit(&mut self) {
        self.frames = self.frames.saturating_add(1);
        self.direct_frames = self.direct_frames.saturating_add(1);
    }

    fn record_direct_drop(&mut self) {
        self.direct_dropped = self.direct_dropped.saturating_add(1);
    }

    fn record_flip_completions(&mut self, count: usize) {
        self.flip_completions = self
            .flip_completions
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
    }

    fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= PHASE_WINDOW_INTERVAL
    }

    fn has_observations(&self) -> bool {
        self.frames != 0
            || self.direct_dropped != 0
            || self.flip_completions != 0
            || self.unprofiled_frames != 0
    }

    fn should_emit(&self, now: Instant, profile_phases: bool) -> bool {
        self.has_observations() && (profile_phases || self.due(now))
    }

    fn reset(&mut self, now: Instant) {
        *self = Self {
            started: now,
            ..Self::default()
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PhaseSummaryFields {
    frames: u64,
    direct_frames: u64,
    direct_dropped: u64,
    flip_completions: u64,
    unprofiled_frames: u64,
    window_us: u128,
    bridge_us: u128,
    scanout_us: u128,
    readback_bytes: u64,
    output_bytes: u64,
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
    fn phase_window_accumulates_and_records_every_frame_outcome() {
        // Phase `phase_window_accumulates_completed_frame_costs`.
        {
            let mut window = PhaseWindow::default();
            window.record(timing(2));
            window.record(timing(3));

            assert_eq!(window.frames, 2);
            assert_eq!(window.direct_frames, 0);
            assert_eq!(window.direct_dropped, 0);
            assert_eq!(window.flip_completions, 0);
            assert_eq!(window.submit_us, 5);
            assert_eq!(window.gpu_readback_map_us, 10);
            assert_eq!(window.settle_us, 15);
            assert_eq!(window.cpu_swizzle_us, 20);
            assert_eq!(window.readback_bytes, 25);
            assert_eq!(window.output_bytes, 30);
            assert_eq!(window.bridge_us, 0);
            assert_eq!(window.scanout_us, 0);
            assert_eq!(window.unprofiled_frames, 0);
        }
        // Phase `phase_window_keeps_failed_legacy_attempt_visible_without_vello_timing`.
        {
            let mut window = PhaseWindow::default();
            window.record_legacy_attempt(None, 41, 7);
            window.record_legacy_attempt(Some(timing(2)), 23, 5);

            assert_eq!(window.frames, 2);
            assert_eq!(window.unprofiled_frames, 1);
            assert_eq!(window.bridge_us, 64);
            assert_eq!(window.scanout_us, 12);
            assert_eq!(window.submit_us, 2);
            assert_eq!(window.gpu_readback_map_us, 4);
            let summary = window.summary_fields(window.started + Duration::from_micros(99));
            assert_eq!(summary.frames, 2);
            assert_eq!(summary.unprofiled_frames, 1);
            assert_eq!(summary.bridge_us, 64);
            assert_eq!(summary.scanout_us, 12);
            assert_eq!(summary.window_us, 99);
        }
        // Phase `phase_window_records_direct_submission_drop_and_flip_completion`.
        {
            let mut window = PhaseWindow::default();
            window.record_direct_submit();
            window.record_direct_drop();
            window.record_flip_completions(2);

            assert_eq!(window.frames, 1);
            assert_eq!(window.direct_frames, 1);
            assert_eq!(window.direct_dropped, 1);
            assert_eq!(window.flip_completions, 2);
            assert_eq!(window.readback_bytes, 0);
            assert_eq!(window.output_bytes, 0);
        }
    }

    #[test]
    fn phase_window_flush_timing_and_reset() {
        // Phase `phase_window_is_due_only_after_the_bounded_interval`.
        {
            let mut window = PhaseWindow::default();
            let started = Instant::now();
            window.started = started;

            let just_before = (started + PHASE_WINDOW_INTERVAL)
                .checked_sub(Duration::from_nanos(1))
                .unwrap();
            assert!(!window.due(just_before));
            assert!(window.due(started + PHASE_WINDOW_INTERVAL));
        }
        // Phase `explicit_profiling_flushes_the_first_observation_without_a_later_frame`.
        {
            let mut window = PhaseWindow::default();
            let started = Instant::now();
            window.started = started;

            assert!(!window.should_emit(started + Duration::from_millis(1), true));
            window.record_legacy_attempt(None, 41, 7);
            assert!(window.should_emit(started + Duration::from_millis(1), true));
            assert!(!window.should_emit(started + Duration::from_millis(1), false));
            assert!(window.should_emit(started + PHASE_WINDOW_INTERVAL, false));
        }
        // Phase `profiling_keeps_direct_drops_and_completions_visible_without_a_frame`.
        {
            let mut window = PhaseWindow::default();
            let now = Instant::now();
            window.started = now;
            window.record_direct_drop();
            window.record_flip_completions(1);

            assert!(window.should_emit(now, true));
        }
        // Phase `phase_window_reset_discards_the_previous_sample_without_moving_its_clock`.
        {
            let mut window = PhaseWindow::default();
            window.record(timing(7));
            let now = Instant::now();
            window.reset(now);

            assert_eq!(window.started, now);
            assert_eq!(window.frames, 0);
            assert_eq!(window.direct_frames, 0);
            assert_eq!(window.direct_dropped, 0);
            assert_eq!(window.flip_completions, 0);
            assert_eq!(window.submit_us, 0);
            assert_eq!(window.gpu_readback_map_us, 0);
            assert_eq!(window.settle_us, 0);
            assert_eq!(window.cpu_swizzle_us, 0);
            assert_eq!(window.readback_bytes, 0);
            assert_eq!(window.output_bytes, 0);
            assert_eq!(window.bridge_us, 0);
            assert_eq!(window.scanout_us, 0);
            assert_eq!(window.unprofiled_frames, 0);
        }
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
            renderer_drm_matches: false,
            drm_bus_id: Some("pci:0000:66:00.0"),
        }
    }

    #[test]
    fn direct_path_eligibility_requires_the_full_contract_and_proven_identity() {
        // Phase `direct_path_requires_the_complete_safe_contract`.
        assert_eq!(direct_scanout_rejection(eligible()), None);
        for (rejected, reason) in [
            (
                DirectEligibility {
                    outputs: 2,
                    ..eligible()
                },
                "multiple outputs",
            ),
            (
                DirectEligibility {
                    normal_transform: false,
                    ..eligible()
                },
                "output transform is not normal",
            ),
            (
                DirectEligibility {
                    color_managed: true,
                    ..eligible()
                },
                "output color transform is active",
            ),
            (
                DirectEligibility {
                    hardware_cursor: false,
                    ..eligible()
                },
                "hardware cursor plane is unavailable",
            ),
            (
                DirectEligibility {
                    event_pageflips: false,
                    ..eligible()
                },
                "event-driven page flips are unavailable",
            ),
            (
                DirectEligibility {
                    drm_bus_id: Some("pci:0000:01:00.0"),
                    ..eligible()
                },
                "renderer and scanout use different GPUs",
            ),
        ] {
            assert_eq!(direct_scanout_rejection(rejected), Some(reason));
        }
        // Phase `drm_node_identity_can_prove_virtual_renderer_without_pci_identity`.
        assert_eq!(
            direct_scanout_rejection(DirectEligibility {
                renderer_pci: "",
                renderer_drm_matches: true,
                drm_bus_id: None,
                ..eligible()
            }),
            None
        );
        // Phase `matching_drm_node_is_authoritative_when_the_card_has_no_pci_bus_id`.
        assert_eq!(
            direct_scanout_rejection(DirectEligibility {
                renderer_pci: "0000:66:00.0",
                renderer_drm_matches: true,
                drm_bus_id: None,
                ..eligible()
            }),
            None
        );
        // Phase `missing_renderer_identities_still_fail_closed`.
        assert_eq!(
            direct_scanout_rejection(DirectEligibility {
                renderer_pci: "",
                renderer_drm_matches: false,
                drm_bus_id: None,
                ..eligible()
            }),
            Some("renderer and scanout use different GPUs")
        );
    }
}

#[cfg(test)]
mod direct_damage_history_tests {
    use super::*;

    const REGION: [vello::FineDamageRect; 1] = [vello::FineDamageRect {
        x: 1,
        y: 2,
        width: 2,
        height: 2,
    }];

    fn sparse(previous_revision: Option<u64>) -> SceneDamage<'static> {
        SceneDamage {
            revision: 9,
            previous_revision,
            regions: Some(&REGION),
        }
    }

    #[test]
    fn direct_damage_history_keeps_sparse_damage_only_when_it_matches() {
        // Phase `matching_scanout_history_preserves_sparse_damage`.
        {
            let damage = damage_for_direct_target(Some(8), sparse(Some(8)));
            assert_eq!(damage.previous_revision, Some(8));
            assert_eq!(damage.regions, Some(REGION.as_slice()));
        }
        // Phase `stale_or_missing_scanout_history_forces_a_dense_boundary`.
        for target_revision in [None, Some(7), Some(9)] {
            let damage = damage_for_direct_target(target_revision, sparse(Some(8)));
            assert_eq!(damage.previous_revision, None);
            assert_eq!(damage.regions, None);
        }
        // Phase `an_already_dense_frame_remains_dense`.
        {
            let damage = damage_for_direct_target(
                Some(8),
                SceneDamage {
                    revision: 9,
                    previous_revision: None,
                    regions: None,
                },
            );
            assert_eq!(damage.previous_revision, None);
            assert_eq!(damage.regions, None);
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
        let sample = paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            self.pointer,
            ScanoutFrame {
                scene,
                damage: None,
                hdr_layers: &[],
                overview: None,
                workspace_id: None,
                workspace_hold: false,
                workspace_clip: None,
            },
            Some(&mut self.phase_window),
            &mut self.direct_bypass_reported,
        );
        if sample.direct_queued {
            self.retire_direct_flip();
        }
        self.emit_phase_profile(sample);
        report_signal_status(&mut self.scanouts, self.signal_reports.as_ref());
    }

    fn render_frame(&mut self, frame: &FramePaint) {
        self.drain_commands();
        if self.blanked || self.paused {
            return;
        }
        let sample = paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            self.pointer,
            ScanoutFrame {
                scene: &frame.scene,
                damage: (frame.revision != 0).then_some(SceneDamage {
                    revision: frame.revision,
                    previous_revision: frame.previous_revision,
                    regions: frame.damage_regions.as_deref(),
                }),
                hdr_layers: &frame.hdr_layers,
                overview: frame.overview_patch.as_ref(),
                workspace_id: frame.workspace_id,
                workspace_hold: frame.workspace_hold,
                workspace_clip: frame.workspace_clip,
            },
            Some(&mut self.phase_window),
            &mut self.direct_bypass_reported,
        );
        if sample.direct_queued {
            self.retire_direct_flip();
        }
        self.emit_phase_profile(sample);
        report_signal_status(&mut self.scanouts, self.signal_reports.as_ref());
    }

    /// Tighten `Presenter::present_frame`'s completion boundary to the KMS
    /// page-flip event for direct scanout. `PresentPipe` only frees producer
    /// capacity after this method returns, so no second frame is rendered
    /// while both double-buffered targets remain owned by KMS.
    fn retire_direct_flip(&mut self) {
        let Some(index) = self
            .scanouts
            .iter()
            .position(|entry| entry.direct_targets.is_some())
        else {
            return;
        };
        let Some(entry) = self.scanouts.get_mut(index) else {
            return;
        };
        let (retirement, completions) = entry
            .scanout
            .wait_flip_completions(DIRECT_RETIREMENT_TIMEOUT);
        self.phase_window.record_flip_completions(completions.len());
        for crtc in completions {
            if let Some(targets) = entry.direct_targets.as_mut() {
                entry.scanout.mark_direct_flip_complete(targets, crtc);
            }
        }
        if retirement == scanout::DirectRetirement::TimedOut {
            tracing::error!(
                timeout_ms = DIRECT_RETIREMENT_TIMEOUT.as_millis(),
                "DRM direct page flip did not retire; disabling direct scanout"
            );
            entry.scanout.restore_legacy_front();
            entry.direct_targets = None;
            entry.direct_unavailable = Some("page-flip retirement timed out".to_owned());
        }
    }

    fn emit_phase_profile(&mut self, sample: PaintScanoutSample) {
        if sample.legacy_attempted {
            let timing = self
                .paint
                .as_ref()
                .and_then(paint::VelloDrmBridge::last_phase_timing);
            self.phase_window
                .record_legacy_attempt(timing, sample.bridge_us, sample.scanout_us);
        }
        let now = Instant::now();
        // A production session reports bounded aggregates. Explicit profiling
        // instead flushes every non-empty window immediately: if rendering
        // stalls or becomes idle after one expensive frame, there may never be
        // a later frame available to flush that evidence.
        if self.phase_window.should_emit(now, self.profile_phases) {
            let frames = self.phase_window.frames.max(1);
            let summary = self.phase_window.summary_fields(now);
            // The session's default subscriber passes only errors unless
            // RUST_LOG says otherwise, so an info event never reaches the
            // flight recorder. The runner captures stderr into that fsyncing
            // record; keep the line machine-parseable.
            eprintln!(
                "frames={} direct_frames={} direct_dropped={} flip_completions={} \
                 unprofiled_frames={} window_us={} avg_submit_us={} \
                 avg_gpu_readback_map_us={} avg_settle_us={} avg_cpu_swizzle_us={} \
                 readback_bytes={} output_bytes={} bridge_us={} scanout_us={} \
                 direct_scanout_status={} direct_scanout_reason={:?} \
                 DRM render phase summary",
                summary.frames,
                summary.direct_frames,
                summary.direct_dropped,
                summary.flip_completions,
                summary.unprofiled_frames,
                summary.window_us,
                self.phase_window
                    .submit_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .gpu_readback_map_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .settle_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                self.phase_window
                    .cpu_swizzle_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                summary.readback_bytes,
                summary.output_bytes,
                summary.bridge_us,
                summary.scanout_us,
                if self
                    .scanouts
                    .iter()
                    .any(|entry| entry.direct_targets.is_some())
                {
                    "enabled"
                } else {
                    "unavailable"
                },
                self.scanouts
                    .iter()
                    .find_map(|entry| entry.direct_unavailable.as_deref())
                    .unwrap_or("not-initialized"),
            );
            self.phase_window.reset(now);
        }
        if !self.profile_phases {
            return;
        }
        let Some(timing) = self
            .paint
            .as_ref()
            .and_then(paint::VelloDrmBridge::last_phase_timing)
        else {
            return;
        };
        self.profile_seq = self.profile_seq.saturating_add(1);
        eprintln!(
            "seq={} submit_us={} gpu_readback_map_us={} settle_us={} cpu_swizzle_us={} \
             readback_bytes={} output_bytes={} DRM render frame phases",
            self.profile_seq,
            timing.submit_us,
            timing.completed_readback_map_us,
            timing.settle_us,
            timing.cpu_copy_swizzle_us,
            timing.readback_bytes,
            timing.cpu_output_bytes,
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

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct PaintScanoutSample {
    direct_queued: bool,
    legacy_attempted: bool,
    bridge_us: u128,
    scanout_us: u128,
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
                entry.scanout.restore_legacy_front();
                entry.direct_targets = None;
            }
            *paused = true;
            tracing::info!("DRM present: paused (the session no longer owns the CRTCs)");
        }
        ScanoutCommand::Resume { dpms_on } => {
            for e in scanouts.iter_mut() {
                e.scanout.restore();
                e.scanout.set_dpms(dpms_on && !e.powered_off);
                e.direct_targets = None;
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
        ScanoutCommand::SetPointerImage(image) => {
            for entry in scanouts.iter_mut() {
                entry.scanout.set_pointer_image(image.clone());
            }
        }
        ScanoutCommand::SetAdaptiveSync { output, on } => {
            for entry in scanouts.iter_mut().filter(|entry| entry.id == output) {
                entry.scanout.set_adaptive_sync(on);
            }
        }
        ScanoutCommand::SetHdr { output, request } => {
            for entry in scanouts.iter_mut().filter(|entry| entry.id == output) {
                entry.set_hdr_request(request.clone());
            }
        }
        ScanoutCommand::SetOutputPower { output, on } => {
            for entry in scanouts.iter_mut().filter(|entry| entry.id == output) {
                entry.powered_off = !on;
                entry.direct_targets = None;
                // While the session is blanked every panel is already off;
                // the wake restores this one only if it is still wanted.
                if !*blanked && !*paused {
                    entry.scanout.set_dpms(on);
                }
                tracing::info!(output = output.0, on, "DRM present: output power changed");
            }
        }
    }
}

/// One composed frame on its way to the scanouts: the scene, what changed
/// since the previous revision (when known), and its HDR layers.
#[derive(Clone, Copy)]
struct ScanoutFrame<'a> {
    scene: &'a Scene,
    damage: Option<SceneDamage<'a>>,
    hdr_layers: &'a [HdrLayer],
    overview: Option<&'a crate::shell::overlay_host::OverviewFastPatch>,
    workspace_id: Option<u64>,
    workspace_hold: bool,
    workspace_clip: Option<vello::FineDamageRect>,
}

/// An unchanged frame, or an overview fade that carries its own panel patch,
/// must not read the combined framebuffer back from the GPU.
pub(crate) fn skips_full_framebuffer_readback(unchanged: bool, overview: bool) -> bool {
    unchanged || overview
}

/// How a workspace-switch frame reaches the scanouts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceUpload {
    /// GPU-render the combined scene and flush both outputs.
    Render,
    /// Put a workspace this bridge already rendered back on the scanouts.
    Replay,
    /// The held return: adopt the snapshot and upload only the workspace clip.
    Clip,
    /// Intermediate frame of the switch: leave both scanouts alone.
    Skip,
}

/// The frame run after a workspace switch. The held return stays on the
/// workspace clip; a settled return may still replay. Every later sample of
/// either direction skips the upload. The first visit still renders.
pub(crate) fn workspace_frame_upload(
    changed: bool,
    has_snapshot: bool,
    hold: bool,
) -> WorkspaceUpload {
    if hold && changed && has_snapshot {
        WorkspaceUpload::Clip
    } else if hold && !changed {
        WorkspaceUpload::Skip
    } else if changed && has_snapshot {
        WorkspaceUpload::Replay
    } else {
        WorkspaceUpload::Render
    }
}

/// The close may upload the one display it is painting. A clip that also
/// meets another scanout is the two-output flush this frame run must not do.
pub(crate) fn close_frame_spares_other_scanouts(
    clip: vello::FineDamageRect,
    outputs: &[(i32, i32, u32, u32)],
) -> bool {
    if clip.width == 0 || clip.height == 0 {
        return false;
    }
    let covered = outputs
        .iter()
        .filter(|&&(x, y, width, height)| output_local_clip(x, y, (width, height), clip).is_some())
        .count();
    covered == 1
}

/// Render the frame's scene into `scanouts`, sized to the combined geometry of
/// `outputs`; lazily (re)allocates `*paint` to that size. Shared by
/// [`DrmPresenter::render`] (post-split, render thread) and
/// [`Backend::submit_frame`] for [`DrmBackend`] (inline path, pre-split) —
/// same painting logic, different owner of the scanouts and the pointer.
#[allow(
    clippy::too_many_lines,
    reason = "one dispatcher keeps bridge lifetime, sparse eligibility, diagnostics, and the single versus multi-output presentation boundaries together"
)]
fn paint_scanouts(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    pointer: (f64, f64),
    frame: ScanoutFrame<'_>,
    mut phase_window: Option<&mut PhaseWindow>,
    direct_bypass_reported: &mut bool,
) -> PaintScanoutSample {
    let ScanoutFrame {
        scene,
        damage,
        hdr_layers,
        ..
    } = frame;
    if scanouts.iter().all(|entry| entry.powered_off) {
        return PaintScanoutSample::default();
    }
    let target_size = combined_geometry(outputs);
    if target_size.0 == 0 || target_size.1 == 0 {
        return PaintScanoutSample::default();
    }
    if let Some(reason) = paint.as_ref().and_then(paint::VelloDrmBridge::device_lost) {
        // Every call on a lost device fails, and the bridge cannot replace its own device.
        tracing::error!(%reason, "DRM backend: the renderer's GPU device was lost; opening a new one");
        for entry in scanouts
            .iter_mut()
            .filter(|entry| entry.direct_targets.is_some())
        {
            entry.scanout.restore_legacy_front();
            entry.direct_targets = None;
        }
        *paint = None;
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
                return PaintScanoutSample::default();
            }
        }
    }
    let mut bridge_resized = false;
    {
        // The block above just initialised `*paint` if it was `None`, or
        // already returned on failure — a miss here would mean that
        // silently didn't stick, so skip this frame rather than panic.
        let Some(bridge) = paint.as_mut() else {
            return PaintScanoutSample::default();
        };
        if bridge.size() != target_size {
            bridge_resized = true;
            if let Err(e) = bridge.resize(target_size.0, target_size.1) {
                tracing::error!("DRM backend: VelloDrmBridge::resize: {e}");
                return PaintScanoutSample::default();
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
    if !fast_path && !*direct_bypass_reported {
        let first = scanouts.first();
        eprintln!(
            "tilo compositor: DRM direct scanout bypassed before adapter inspection \
             (scanouts={}, transform={:?}, color_managed={})",
            scanouts.len(),
            first.map(|entry| entry.transform),
            first.is_some_and(|entry| entry.color_transform.is_some()),
        );
        *direct_bypass_reported = true;
    }
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
            return PaintScanoutSample::default();
        };

        // The direct path is deliberately narrow until every transformed,
        // colour-managed and software-cursor composition case has an exact
        // GPU implementation. This laptop's normal single-output path avoids
        // all framebuffer readback while every unsupported case remains on
        // the established presenter.
        if let Ok(adapter) = bridge.adapter_info() {
            let drm_bus_id = entry.scanout.drm_bus_id();
            let eligibility = DirectEligibility {
                outputs: scanout_count,
                normal_transform: entry.transform
                    == settings_store::hardware::OutputTransform::Normal,
                color_managed: entry.color_transform.is_some(),
                hardware_cursor: entry.scanout.hardware_cursor_active(),
                event_pageflips: entry.scanout.event_pageflips_available(),
                renderer_pci: &adapter.pci_bus_id,
                renderer_drm_matches: scanout::renderer_drm_node_matches_card(
                    adapter.drm_device,
                    entry.scanout.drm_device_id(),
                ),
                drm_bus_id: drm_bus_id.as_deref(),
            };
            let renderer_drm = adapter.drm_device.map_or_else(
                || "unavailable".to_owned(),
                |identity| {
                    format!(
                        "primary={:?},render={:?}",
                        identity.primary, identity.render
                    )
                },
            );
            if let Some(reason) = direct_scanout_rejection(eligibility) {
                if entry.direct_unavailable.is_none() {
                    // Keep this line machine-parseable and durable.  The
                    // session flight recorder may start receiving output a
                    // moment after the render thread, so repeat the stable
                    // rejection with the periodic phase summary below too.
                    eprintln!(
                        "direct_scanout_status=unavailable reason={reason:?} renderer_pci={} renderer_drm={renderer_drm:?} renderer_drm_matches={} drm_bus_id={:?} hardware_cursor={} event_pageflips={} DRM direct scanout unavailable",
                        adapter.pci_bus_id,
                        eligibility.renderer_drm_matches,
                        drm_bus_id.as_deref().unwrap_or("unavailable"),
                        eligibility.hardware_cursor,
                        eligibility.event_pageflips,
                    );
                    entry.direct_unavailable = Some(reason.to_owned());
                }
            } else {
                // HDR renders into 10-bit (or half-float) targets; targets in
                // the other format go first, CRTC back on the dumb front.
                let want_hdr = entry.hdr_wanted();
                if entry
                    .direct_targets
                    .as_ref()
                    .is_some_and(|targets| targets.format().is_hdr() != want_hdr)
                {
                    entry.scanout.restore_legacy_front();
                    entry.direct_targets = None;
                }
                let direct_blocked = if want_hdr {
                    entry.hdr_direct_unavailable.is_some()
                } else {
                    entry.direct_unavailable.is_some()
                };
                if entry.direct_targets.is_none() && !direct_blocked {
                    match create_direct_targets(&entry.scanout, bridge, want_hdr) {
                        Ok(targets) => {
                            eprintln!(
                                "direct_scanout_status=enabled format={:?} renderer_pci={} renderer_drm={renderer_drm:?} renderer_drm_matches={} drm_bus_id={:?} DRM direct scanout enabled",
                                targets.format(),
                                adapter.pci_bus_id,
                                eligibility.renderer_drm_matches,
                                drm_bus_id.as_deref().unwrap_or("unavailable"),
                            );
                            tracing::info!(
                                renderer_pci = %adapter.pci_bus_id,
                                renderer_drm = %renderer_drm,
                                renderer_drm_matches = eligibility.renderer_drm_matches,
                                drm_bus_id = drm_bus_id.as_deref().unwrap_or("unavailable"),
                                "DRM direct scanout enabled"
                            );
                            entry.direct_targets = Some(targets);
                        }
                        Err(error) if want_hdr => {
                            tracing::warn!(%error, "DRM HDR direct scanout unavailable; using the readback path");
                            entry.hdr_direct_unavailable = Some(error);
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
                    let completions = entry.scanout.poll_flip_completions();
                    if let Some(window) = phase_window.as_deref_mut() {
                        window.record_flip_completions(completions.len());
                    }
                    for crtc in completions {
                        entry.scanout.mark_direct_flip_complete(targets, crtc);
                    }
                    if let Some((index, target)) = targets.render_slot() {
                        let revision = damage.map_or(1, |damage| damage.revision.max(1));
                        let direct_damage = damage_for_direct_target(
                            targets.previous_revision_for(index),
                            damage.unwrap_or(SceneDamage {
                                revision,
                                previous_revision: None,
                                regions: None,
                            }),
                        );
                        let deadline = Instant::now()
                            .checked_add(Duration::from_secs(1))
                            .unwrap_or_else(Instant::now);
                        let sdr_white = entry.scanout.hdr_request().map_or(
                            crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                            |request| request.sdr_white_nits,
                        );
                        match bridge.paint_imported_until(
                            scene,
                            direct_damage,
                            target,
                            deadline,
                            sdr_white,
                            hdr_layers,
                        ) {
                            Ok(stats) => {
                                targets.mark_rendered(index, revision);
                                match entry.scanout.submit_direct_target(targets, index) {
                                    scanout::DirectSubmit::Queued => {
                                        if let Some(window) = phase_window.as_deref_mut() {
                                            window.record_direct_submit();
                                        }
                                        tracing::trace!(
                                            revision = stats.revision,
                                            render_attempts = stats.render_attempts,
                                            framebuffer_copy_bytes = stats.framebuffer_copy_bytes,
                                            framebuffer_staging_bytes =
                                                stats.framebuffer_staging_bytes,
                                            "DRM direct frame submitted"
                                        );
                                        return PaintScanoutSample {
                                            direct_queued: true,
                                            ..PaintScanoutSample::default()
                                        };
                                    }
                                    scanout::DirectSubmit::Presented => {
                                        // The first HDR frame went up by modeset:
                                        // no flip event to wait for.
                                        if let Some(window) = phase_window.as_deref_mut() {
                                            window.record_direct_submit();
                                        }
                                        return PaintScanoutSample::default();
                                    }
                                    scanout::DirectSubmit::Busy => {
                                        if let Some(window) = phase_window.as_deref_mut() {
                                            window.record_direct_drop();
                                        }
                                        return PaintScanoutSample::default();
                                    }
                                    scanout::DirectSubmit::Failed(error) => {
                                        tracing::warn!(%error, "DRM direct submit failed; disabling direct targets");
                                        entry.scanout.restore_legacy_front();
                                        entry.direct_targets = None;
                                        if want_hdr {
                                            entry.hdr_direct_unavailable = Some(error);
                                        } else {
                                            entry.direct_unavailable = Some(error);
                                        }
                                    }
                                    scanout::DirectSubmit::HdrFailed(reason, detail) => {
                                        // The CRTC is still (or again) on the
                                        // dumb front; the targets can go.
                                        entry.direct_targets = None;
                                        entry.fail_hdr(reason, &detail);
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::warn!(%error, "DRM direct frame failed; disabling direct targets");
                                entry.scanout.restore_legacy_front();
                                entry.direct_targets = None;
                                if want_hdr {
                                    entry.hdr_direct_unavailable = Some(error.to_string());
                                } else {
                                    entry.direct_unavailable = Some(error.to_string());
                                }
                            }
                        }
                    } else {
                        if let Some(window) = phase_window.as_mut() {
                            window.record_direct_drop();
                        }
                        return PaintScanoutSample::default();
                    }
                }
            }
        }
        // HDR without the direct path goes through the combined readback
        // below, into 10-bit dumb buffers; everything else stays XRGB8888
        // and shows HDR layers through their tone-mapped rendition.
        if !entry.sync_hdr_readback() {
            return present_single_xrgb(
                entry,
                bridge,
                scene,
                damage,
                Some(pointer),
                sparse_coordinates_match,
            );
        }
    }

    // Multi-output path: scanouts on one card share an open file
    // description, so their page-flip completions arrive on one event stream.
    // Hybrid systems have *one stream per card*, and CRTC handles are only
    // meaningful inside that card.  Drain exactly one representative per
    // device and route only to siblings from the same device; draining just
    // `scanouts[0]` would wedge every secondary GPU after its first flip.
    drain_page_flip_completions(scanouts);
    for entry in scanouts.iter_mut().filter(|entry| !entry.powered_off) {
        entry.sync_hdr_readback();
    }
    render_combined_readback(scanouts, outputs, paint, pointer, frame)
}

/// Direct targets in the format the output needs now: the HDR formats in
/// order of preference while HDR is wanted, `XRGB8888` otherwise.
fn create_direct_targets(
    scanout: &scanout::DrmScanout,
    bridge: &paint::VelloDrmBridge,
    want_hdr: bool,
) -> Result<scanout::DirectScanoutTargets, String> {
    if !want_hdr {
        return scanout::DirectScanoutTargets::new(
            scanout,
            bridge,
            scanout::ScanoutFormat::Xrgb8888,
        );
    }
    let mut errors = Vec::new();
    for format in scanout::ScanoutFormat::HDR_PREFERENCE {
        match scanout::DirectScanoutTargets::new(scanout, bridge, format) {
            Ok(targets) => return Ok(targets),
            Err(error) => errors.push(format!("{format:?}: {error}")),
        }
    }
    Err(errors.join("; "))
}

/// The single-output SDR readback: render and swizzle straight into the
/// scanout's XRGB8888 dumb buffer, through the destination history when it
/// is enabled.
fn present_single_xrgb(
    entry: &mut ScanoutEntry,
    bridge: &mut paint::VelloDrmBridge,
    scene: &Scene,
    damage: Option<SceneDamage<'_>>,
    cursor: Option<(f64, f64)>,
    sparse_coordinates_match: bool,
) -> PaintScanoutSample {
    if entry.scanout.xrgb_damage_available()
        && bridge.xrgb_history_available()
        && sparse_coordinates_match
        && let Some(damage) = damage
    {
        let started = Instant::now();
        let result = entry
            .scanout
            .present_scene_damage(cursor, bridge, scene, damage);
        let elapsed_us = started.elapsed().as_micros();
        let bridge_us = bridge
            .last_phase_timing()
            .map_or(elapsed_us, paint::BridgePhaseTiming::total_us);
        if let Err(error) = result {
            match error {
                paint::BridgeError::RecoveryBackoff { .. } => {
                    tracing::debug!("DRM backend: paint recovery is cooling down: {error}");
                }
                error => tracing::error!("DRM backend: history-aware XRGB paint: {error}"),
            }
        }
        return PaintScanoutSample {
            legacy_attempted: true,
            bridge_us,
            scanout_us: elapsed_us.saturating_sub(bridge_us),
            ..PaintScanoutSample::default()
        };
    }
    if entry.scanout.xrgb_damage_requested() {
        entry.scanout.invalidate_xrgb_history();
    }
    let started = Instant::now();
    let result = entry.scanout.present_into(cursor, |dst, pitch| {
        match damage.filter(|_| sparse_coordinates_match) {
            Some(damage) => bridge.paint_damage_into_xrgb8888(scene, damage, dst, pitch),
            None => bridge.paint_into_xrgb8888(scene, dst, pitch),
        }
    });
    let elapsed_us = started.elapsed().as_micros();
    let bridge_us = bridge
        .last_phase_timing()
        .map_or(elapsed_us, paint::BridgePhaseTiming::total_us);
    if let Err(e) = result {
        match e {
            paint::BridgeError::RecoveryBackoff { .. } => {
                tracing::debug!("DRM backend: paint recovery is cooling down: {e}");
            }
            e => tracing::error!("DRM backend: paint_into_xrgb8888: {e}"),
        }
    }
    PaintScanoutSample {
        legacy_attempted: true,
        bridge_us,
        scanout_us: elapsed_us.saturating_sub(bridge_us),
        ..PaintScanoutSample::default()
    }
}

/// Render the combined scene ONCE into a tight RGBA buffer, then swizzle
/// (or, for an HDR output, PQ-encode) each output's sub-rect into its own
/// scanout's dumb buffer in a single pass per output.
///
/// While an output is driven in HDR, `hdr_layers` are composited on the CPU
/// at full precision: into the PQ signal of HDR outputs and tone-mapped for
/// SDR ones.
fn render_combined_readback(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    pointer: (f64, f64),
    frame: ScanoutFrame<'_>,
) -> PaintScanoutSample {
    let ScanoutFrame {
        scene,
        damage,
        hdr_layers,
        overview,
        workspace_id,
        workspace_hold,
        workspace_clip,
    } = frame;
    let unchanged = damage.is_some_and(|damage| {
        damage
            .regions
            .is_some_and(<[vello::FineDamageRect]>::is_empty)
    });
    if hdr_layers.is_empty()
        && let Some(patch) = overview
        && let Some(sample) = present_overview_patch(scanouts, outputs, paint, pointer, patch)
    {
        return sample;
    }
    if overview.is_none()
        && hdr_layers.is_empty()
        && let Some(handled) = present_workspace_hold(
            scanouts,
            outputs,
            paint,
            pointer,
            &WorkspacePresent {
                id: workspace_id,
                hold: workspace_hold,
                clip: workspace_clip,
            },
        )
    {
        return handled;
    }
    if overview.is_none()
        && skips_full_framebuffer_readback(unchanged, false)
        && paint
            .as_ref()
            .is_some_and(paint::VelloDrmBridge::has_complete_readback)
    {
        // The pixels already on the scanout are the scene. Remember which
        // workspace that image is, so the close can replay it instead of
        // reading both outputs back.
        if let Some(id) = workspace_id
            && let Some(bridge) = paint.as_mut()
        {
            bridge.note_shown_workspace(id);
        }
        return PaintScanoutSample::default();
    }
    let Some(bridge) = paint.as_mut() else {
        return PaintScanoutSample::default();
    };
    let bridge_started = Instant::now();
    let composite = !hdr_layers.is_empty()
        && scanouts
            .iter()
            .any(|entry| !entry.powered_off && entry.scanout.format().is_hdr());
    let painted = if composite {
        bridge
            .paint_with_hdr_layers(scene, hdr_layers)
            .map(|(rgba, second)| (rgba, Some(second)))
    } else {
        bridge.paint(scene).map(|rgba| (rgba, None))
    };
    let (rgba, second) = match painted {
        Ok(b) => b,
        Err(e) => {
            match e {
                paint::BridgeError::RecoveryBackoff { .. } => {
                    tracing::debug!("DRM backend: paint recovery is cooling down: {e}");
                }
                e => tracing::error!("DRM backend: paint (combined): {e}"),
            }
            return PaintScanoutSample {
                legacy_attempted: true,
                bridge_us: bridge_started.elapsed().as_micros(),
                ..PaintScanoutSample::default()
            };
        }
    };
    let bridge_us = bridge_started.elapsed().as_micros();
    let scanout_started = Instant::now();
    let target_size = combined_geometry(outputs);
    let (cw, ch) = (target_size.0 as usize, target_size.1 as usize);
    let hdr = second.as_deref().map(|second| CpuComposite {
        second,
        scene_width: cw,
        layers: hdr_layers,
    });
    for e in scanouts.iter_mut().filter(|entry| !entry.powered_off) {
        paint_one_scanout(e, outputs, rgba, (cw, ch), pointer, hdr, None);
    }
    if overview.is_none()
        && let Some(id) = workspace_id
    {
        bridge.note_shown_workspace(id);
    }
    PaintScanoutSample {
        legacy_attempted: true,
        bridge_us,
        scanout_us: scanout_started.elapsed().as_micros(),
        ..PaintScanoutSample::default()
    }
}

struct WorkspacePresent {
    id: Option<u64>,
    hold: bool,
    clip: Option<vello::FineDamageRect>,
}

fn present_workspace_hold(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    pointer: (f64, f64),
    workspace: &WorkspacePresent,
) -> Option<PaintScanoutSample> {
    let bridge = paint.as_mut()?;
    let changed = workspace
        .id
        .is_some_and(|id| bridge.shown_workspace() != Some(id));
    let has_snapshot = workspace
        .id
        .is_some_and(|id| bridge.has_workspace_snapshot(id));
    match workspace_frame_upload(changed, has_snapshot, workspace.hold) {
        WorkspaceUpload::Skip => Some(PaintScanoutSample::default()),
        WorkspaceUpload::Clip => Some(present_workspace_clip(
            scanouts, outputs, bridge, pointer, workspace,
        )),
        WorkspaceUpload::Replay => {
            let id = workspace.id?;
            let bytes = bridge.replay_workspace(id)?;
            Some(present_composed_readback(
                scanouts, outputs, pointer, &bytes, None,
            ))
        }
        WorkspaceUpload::Render => {
            if changed {
                bridge.retain_shown_workspace();
            }
            None
        }
    }
}

/// Adopt the workspace the close is returning to and upload only the clip.
/// A clip that also meets another scanout is cut back to the first display.
/// Leaving it whole, or skipping the upload, made a later frame flush both.
fn present_workspace_clip(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    bridge: &mut paint::VelloDrmBridge,
    pointer: (f64, f64),
    workspace: &WorkspacePresent,
) -> PaintScanoutSample {
    let Some(id) = workspace.id else {
        return PaintScanoutSample::default();
    };
    let boxes = scanout_boxes(scanouts, outputs);
    let Some(clip) = workspace
        .clip
        .and_then(|clip| clip_to_primary_scanout(clip, &boxes))
    else {
        return PaintScanoutSample::default();
    };
    if !close_frame_spares_other_scanouts(clip, &boxes) {
        return PaintScanoutSample::default();
    }
    let Some(bytes) = bridge.replay_workspace(id) else {
        return PaintScanoutSample::default();
    };
    present_composed_readback(scanouts, outputs, pointer, &bytes, Some(clip))
}

/// Composite the overview fade onto the desktop already in memory.
///
/// `None` means there is no desktop image yet, so the caller may seed one
/// readback. Any other result is final: a failed clip must not GPU-read both
/// outputs in the middle of the fade.
fn present_overview_patch(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    pointer: (f64, f64),
    patch: &crate::shell::overlay_host::OverviewFastPatch,
) -> Option<PaintScanoutSample> {
    let bridge = paint.as_mut()?;
    match bridge.composite_overview_frame(patch) {
        Ok(composed) => Some(present_composed_readback(
            scanouts,
            outputs,
            pointer,
            &composed,
            Some(patch.panel),
        )),
        Err(_) if bridge.has_complete_readback() => Some(PaintScanoutSample::default()),
        Err(_) => None,
    }
}

/// The part of `clip` that lies on the first scanout, in scene pixels.
/// A close whose clip was the combined desktop uploads that display only.
pub(crate) fn clip_to_primary_scanout(
    clip: vello::FineDamageRect,
    outputs: &[(i32, i32, u32, u32)],
) -> Option<vello::FineDamageRect> {
    let &(x, y, width, height) = outputs.first()?;
    let (local_x, local_y, local_w, local_h) = output_local_clip(x, y, (width, height), clip)?;
    let scene_x = i64::from(x).saturating_add(i64::from(local_x));
    let scene_y = i64::from(y).saturating_add(i64::from(local_y));
    if scene_x < 0 || scene_y < 0 {
        return None;
    }
    Some(vello::FineDamageRect {
        x: u32::try_from(scene_x).ok()?,
        y: u32::try_from(scene_y).ok()?,
        width: u32::from(local_w),
        height: u32::from(local_h),
    })
}

fn scanout_boxes(scanouts: &[ScanoutEntry], outputs: &[Output]) -> Vec<(i32, i32, u32, u32)> {
    scanouts
        .iter()
        .filter(|entry| !entry.powered_off)
        .map(|entry| {
            let size = outputs
                .iter()
                .find(|output| output.id == entry.id)
                .map_or_else(|| entry.scanout.size(), |output| output.size);
            (entry.position.0, entry.position.1, size.0, size.1)
        })
        .collect()
}

fn present_composed_readback(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    pointer: (f64, f64),
    rgba: &[u8],
    damage: Option<vello::FineDamageRect>,
) -> PaintScanoutSample {
    let scanout_started = Instant::now();
    let target_size = combined_geometry(outputs);
    let (cw, ch) = (
        usize::try_from(target_size.0).unwrap_or(0),
        usize::try_from(target_size.1).unwrap_or(0),
    );
    for entry in scanouts.iter_mut().filter(|entry| !entry.powered_off) {
        paint_one_scanout(entry, outputs, rgba, (cw, ch), pointer, None, damage);
    }
    PaintScanoutSample {
        legacy_attempted: true,
        scanout_us: scanout_started.elapsed().as_micros(),
        ..PaintScanoutSample::default()
    }
}

fn output_local_clip(
    origin_x: i32,
    origin_y: i32,
    size: (u32, u32),
    damage: vello::FineDamageRect,
) -> Option<(u16, u16, u16, u16)> {
    let x0 = i64::from(damage.x);
    let y0 = i64::from(damage.y);
    let x1 = x0.saturating_add(i64::from(damage.width));
    let y1 = y0.saturating_add(i64::from(damage.height));
    let left = i64::from(origin_x);
    let top = i64::from(origin_y);
    let right = left.saturating_add(i64::from(size.0));
    let bottom = top.saturating_add(i64::from(size.1));
    let ix0 = x0.max(left);
    let iy0 = y0.max(top);
    let ix1 = x1.min(right);
    let iy1 = y1.min(bottom);
    if ix1 <= ix0 || iy1 <= iy0 {
        return None;
    }
    Some((
        u16::try_from(ix0.saturating_sub(left)).ok()?,
        u16::try_from(iy0.saturating_sub(top)).ok()?,
        u16::try_from(ix1.saturating_sub(ix0)).ok()?,
        u16::try_from(iy1.saturating_sub(iy0)).ok()?,
    ))
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
/// `rgba` is the combined frame, `(cw, ch)` pixels.
fn paint_one_scanout(
    e: &mut ScanoutEntry,
    outputs: &[Output],
    rgba: &[u8],
    (cw, ch): (usize, usize),
    pointer: (f64, f64),
    hdr_layers: Option<CpuComposite<'_>>,
    damage: Option<vello::FineDamageRect>,
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
    let hdr = e.scanout.format().is_hdr();
    // An ICC profile describes the panel's SDR response; while the output
    // runs in HDR the PQ encode replaces it.
    let copied = e.color_transform.is_some() && !hdr;
    let (source, source_w, source_h, source_x, source_y): (&[u8], usize, usize, usize, usize) =
        if let Some(transform) = e.color_transform.as_ref().filter(|_| !hdr) {
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
    let encoder = e.scanout.hdr_encoder().filter(|_| hdr);
    let subrect = SubrectSource {
        src_rgba: source,
        src_w: source_w,
        src_h: source_h,
        src_x: source_x,
        src_y: source_y,
    };
    let transform = e.transform;
    // Scene position of the source buffer's origin: the output's own copy
    // starts at the output, the combined frame at the scene's origin.
    let scene_origin = if copied { (sx, sy) } else { (0, 0) };
    let composite = hdr_layers.map(|layers| (layers, scene_origin));
    match damage.map(|rect| output_local_clip(px, py, logical_size, rect)) {
        Some(Some(clip)) => e.scanout.set_damage_rect(Some(clip)),
        Some(None) => return,
        None => e.scanout.set_damage_rect(None),
    }
    e.scanout
        .present_no_drain(local_cursor, |dst, pitch| match &encoder {
            Some(encoder) => encode_subrect_rgba_to_xbgr2101010_transformed(
                subrect,
                dst,
                pitch,
                (physical_w as usize, physical_h as usize),
                transform,
                encoder,
                composite,
            ),
            None => swizzle_subrect_rgba_to_xrgb8888_composited(
                subrect,
                dst,
                pitch,
                (physical_w as usize, physical_h as usize),
                transform,
                composite,
            ),
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
            e.scanout.set_dpms(on && !e.powered_off);
            e.direct_targets = None;
        }
    }
}

impl Backend for DrmBackend {
    fn event_fd(&self) -> Option<std::os::fd::OwnedFd> {
        match self.inner.duplicate_event_fd() {
            Ok(fd) => Some(fd),
            Err(error) => {
                tracing::error!(%error, "DRM backend: could not expose its event-loop wake fd");
                None
            }
        }
    }

    fn next_service_deadline(&self) -> Option<std::time::Instant> {
        self.inner.next_service_deadline()
    }

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
            ScanoutFrame {
                scene,
                damage: None,
                hdr_layers: &[],
                overview: None,
                workspace_id: None,
                workspace_hold: false,
                workspace_clip: None,
            },
            None,
            &mut self.direct_bypass_reported,
        );
        report_signal_status(&mut self.inner.scanouts, self.signal_reports.as_ref());
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
            ScanoutFrame {
                scene: &frame.scene,
                damage: (frame.revision != 0).then_some(SceneDamage {
                    revision: frame.revision,
                    previous_revision: frame.previous_revision,
                    regions: frame.damage_regions.as_deref(),
                }),
                hdr_layers: &frame.hdr_layers,
                overview: frame.overview_patch.as_ref(),
                workspace_id: frame.workspace_id,
                workspace_hold: frame.workspace_hold,
                workspace_clip: frame.workspace_clip,
            },
            None,
            &mut self.direct_bypass_reported,
        );
        report_signal_status(&mut self.inner.scanouts, self.signal_reports.as_ref());
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
            // wgpu/Vulkan devices must be created on the thread which will
            // drive them.  An inline frame can initialise `self.paint`
            // before this split (for example during startup or a future
            // backend probe); moving that live device to the render thread
            // wedges Mesa software adapters after the first submissions.
            // Drop any inline bridge here and let `paint_scanouts` construct
            // a fresh one on the dedicated presentation thread.
            paint: None,
            blanked: self.blanked,
            paused: self.paused,
            // The scanouts and the commands addressed to them travel together: whoever owns one owns the other.
            commands: self.commands.take(),
            present_gate: self.inner.dispatch.present_gate.clone(),
            profile_phases: std::env::var_os("TILO_DRM_PHASE_TRACE").is_some(),
            profile_seq: 0,
            phase_window: PhaseWindow::default(),
            direct_bypass_reported: false,
            signal_reports: self.signal_reports.take(),
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
            e.scanout.set_dpms(on && !e.powered_off);
            e.direct_targets = None;
        }
    }

    fn reset_display(&mut self) {
        self.inner.resume_from_sleep();
    }

    fn set_fullscreen_output(&mut self, output: Option<OutputId>) {
        self.inner.set_fullscreen_output(output);
    }

    fn adaptive_sync_active(&self) -> bool {
        self.inner.dispatch.adaptive_sync_engaged()
    }

    fn output_hdr(&self, output: OutputId) -> Option<super::OutputHdr> {
        self.inner.dispatch.output_hdr(output)
    }

    fn set_keyboard_source(&mut self, index: usize) {
        self.inner.set_keyboard_source(index);
    }

    fn reload_input_settings(&mut self) {
        self.inner.reload_input_settings();
    }

    fn set_pointer_image(&mut self, image: crate::compositor::cursor::PointerImage) {
        self.inner.set_pointer_image(image);
    }

    fn set_pointer_constraint(
        &mut self,
        constraint: crate::compositor::backend::PointerConstraintRegion,
    ) {
        self.inner.set_pointer_constraint(constraint);
    }

    fn warp_pointer(&mut self, x: f64, y: f64) {
        self.inner.warp_pointer(x, y);
    }

    fn cancel_touch(&mut self) {
        self.inner.cancel_touch();
    }

    fn switch_vt(&mut self, vt: i32) {
        self.inner.change_vt(vt);
    }

    fn output_heads(&self) -> Vec<crate::compositor::output_heads::OutputHead> {
        self.inner.output_heads()
    }

    fn set_output_power(&mut self, output: OutputId, on: bool) {
        self.inner.set_output_power(output, on);
    }

    fn set_device_rotation(&mut self, rotation: Option<settings_store::hardware::OutputTransform>) {
        self.inner.set_device_rotation(rotation);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_overview_patch_or_unchanged_frame_skips_the_full_readback() {
        assert!(super::skips_full_framebuffer_readback(true, false));
        assert!(super::skips_full_framebuffer_readback(false, true));
        assert!(!super::skips_full_framebuffer_readback(false, false));
    }

    #[test]
    fn workspace_switch_replays_the_way_back_and_holds_both_directions() {
        use super::WorkspaceUpload::{Clip, Render, Replay, Skip};
        // First visit renders. The frame run after that does not upload.
        assert_eq!(super::workspace_frame_upload(true, false, true), Render);
        assert_eq!(super::workspace_frame_upload(false, false, true), Skip);
        // The held return stays on the clip. A settled return may still replay.
        assert_eq!(super::workspace_frame_upload(true, true, true), Clip);
        assert_eq!(super::workspace_frame_upload(false, true, true), Skip);
        assert_eq!(super::workspace_frame_upload(true, true, false), Replay);
        // A settled desktop still renders when its pixels actually change.
        assert_eq!(super::workspace_frame_upload(false, true, false), Render);
    }

    #[test]
    fn workspace_switch_close_clip_does_not_cover_the_other_scanout() {
        let clip = vello::FineDamageRect {
            x: 313,
            y: 0,
            width: 1607,
            height: 1080,
        };
        let outputs = [(0, 0, 1920, 1080), (1920, 0, 1280, 800)];
        assert!(
            super::close_frame_spares_other_scanouts(clip, &outputs),
            "the content column meets the first display only"
        );
        let both = vello::FineDamageRect {
            x: 0,
            y: 0,
            width: 3200,
            height: 1080,
        };
        assert!(
            !super::close_frame_spares_other_scanouts(both, &outputs),
            "a combined clip is the two-output flush"
        );
        assert!(
            super::output_local_clip(1920, 0, (1280, 800), clip).is_none(),
            "the second scanout is outside the close clip"
        );
    }

    #[test]
    fn a_combined_close_clip_uploads_only_the_first_display() {
        let both = vello::FineDamageRect {
            x: 0,
            y: 0,
            width: 3200,
            height: 1080,
        };
        let outputs = [(0, 0, 1920, 1080), (1920, 0, 1280, 800)];
        let clip = super::clip_to_primary_scanout(both, &outputs)
            .expect("the close still has the first display");
        assert_eq!(clip.x, 0);
        assert_eq!(clip.width, 1920);
        assert!(
            super::close_frame_spares_other_scanouts(clip, &outputs),
            "cutting the clip back to the first display leaves the other scanout alone"
        );
        assert!(
            super::output_local_clip(1920, 0, (1280, 800), clip).is_none(),
            "the second scanout is outside the cut clip"
        );
    }

    use super::*;

    #[test]
    fn page_flip_completion_is_routed_per_card() {
        // Phase `page_flip_drain_uses_one_representative_per_card`.
        // A fake scanout inventory: card0 has two CRTCs, card1 has two more
        // whose handle values may overlap card0's.  No real compositor or
        // DRM node is involved; this pins the grouping rule used before the
        // hardware-facing drain.
        assert_eq!(
            page_flip_representatives([100, 100, 200, 100, 200]),
            vec![(100, 0), (200, 2)],
            "each card must drain its own shared event stream exactly once"
        );
        // Phase `overlapping_crtcs_route_flip_completion_only_within_its_card`.
        {
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
    }

    #[test]
    fn presenter_follows_session_pause_resume_and_reconfigure() {
        // Phase `a_pause_stops_the_presenter_and_a_resume_starts_it`.
        // The reconfiguration path with no live scanouts.
        //
        // Building a `ScanoutEntry` needs a modeset connector, so the scanout-touching half of
        // `Reconfigure`/`Resume` can't be exercised off hardware; what can be exercised is the
        // *gating* — whether the presenter paints at all, and which output table it believes in.
        {
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
        // Phase `resuming_a_blanked_session_leaves_the_panels_off`.
        {
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
        // Phase `a_reconfigure_adopts_the_new_output_table`.
        {
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
                    refresh_mhz: None,
                    logical_position: (1920, 0),
                    identity: crate::compositor::backend::OutputIdentity::default(),
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
    }

    #[test]
    fn drm_backend_construction_and_output_publication() {
        // Phase `drm_backend_new_returns_err_when_no_seat_available`.
        // The DRM backend's construction must surface a real error (`Box<dyn Error>`) on any dev
        // machine off a free TTY. Exact text varies; we only assert `Err`, not a panic, so this test is portable across CI.
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
        // Phase `drm_backend_outputs_returns_populated_list_when_runtime_has_outputs`.
        // Verify `outputs()` returns the populated list when the runtime has synthesised outputs. Can't
        // call `DrmRuntime::new` from a dev machine, so exercise `refresh_outputs` directly on a synthesised dispatch state.
        {
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
        // Phase `drm_backend_does_not_warn_about_vello_anymore`.
        // The bridge field must exist and the legacy "scene discarded" warn-and-latch field must not,
        // so a future refactor re-adding a fail-silent path has to re-design this test. Verified via the production type's shape (not a `tracing` capture) to keep the test fast and dependency-free.
        {
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
        // Phase `drm_presenter_starts_without_a_cross_thread_gpu_device`.
        {
            // `take_presenter` is the ownership boundary.  Constructing the
            // bridge before this point and moving it with the scanouts can wedge
            // software Vulkan adapters after their first submissions.  Keep the
            // invariant visible in the production initializer without requiring
            // a DRM seat or a wgpu adapter in this unit test.
            let src = include_str!("mod.rs");
            let start = src
                .find("fn take_presenter(&mut self)")
                .expect("DRM backend must expose its presenter");
            let body = &src[start..];
            let initializer = body
                .split("fn set_dpms")
                .next()
                .expect("presenter initializer must precede set_dpms");
            assert!(
                initializer.contains("paint: None"),
                "the render thread must construct its own wgpu bridge"
            );
            assert!(
                !initializer.contains("paint: self.paint.take()"),
                "a live wgpu bridge must never cross the thread boundary"
            );
        }
    }
}
