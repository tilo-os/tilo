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
pub mod cpu_present;
pub mod devices;
mod direct_retry;
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

use std::sync::atomic::{AtomicBool, Ordering};
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
    /// The CPU renderer, when this device cannot run vello on its GPU.
    cpu: Option<cpu_present::CpuRenderer>,
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
    /// Some powered output presented through the combined readback in the
    /// latest frame. Only that readback replays retained workspace pixels, so
    /// clip-only transition frames are requested only while it is in use.
    /// Shared with the presenter, which updates it after every frame.
    readback_presenting: Arc<AtomicBool>,
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
        let outputs = Self::refresh_outputs(&inner.dispatch);
        // The loop ticks at the *fastest* connected mode. Each output renders only once its own
        // previous flip has retired, so a slower output skips the frames it cannot show rather
        // than throttling the fast panel down (`DrmPresenter::retire_direct_flips`).
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
            cpu: None,
            blanked: false,
            refresh,
            commands,
            paused: false,
            readback_presenting: Arc::new(AtomicBool::new(true)),
            signal_reports,
        })
    }

    /// Collect the current public [`Output`] list from the runtime's dispatch state, primary first
    /// (see `DrmDispatchState::output_table`).
    fn refresh_outputs(dispatch: &runtime::DrmDispatchState) -> Vec<Output> {
        dispatch.output_table()
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

    /// The only way a frame reaches the scanouts while this backend still owns them inline,
    /// before [`Backend::take_presenter`] moves them to [`DrmPresenter::submit`].
    ///
    /// This runs on the loop thread, the one that drops DRM master, so it cannot overlap
    /// that drop; it takes the present gate anyway so both submission paths keep one rule.
    /// It never blocks on a vblank: a queued flip retires through the next frame's
    /// non-blocking drain in `paint_scanouts`, and reconfiguration is drained by
    /// [`Backend::poll_events`].
    fn submit_inline(&mut self, frame: ScanoutFrame<'_>) {
        let present_gate = self.inner.dispatch.present_gate.clone();
        let _quiesced = quiesce_presentations(present_gate.as_ref());
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
            &mut self.cpu,
            pointer,
            frame,
            None,
        );
        publish_readback_presenting(&self.inner.scanouts, &self.readback_presenting);
        report_signal_status(&mut self.inner.scanouts, self.signal_reports.as_ref());
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
pub struct DrmPresenter {
    scanouts: Vec<ScanoutEntry>,
    outputs: Vec<Output>,
    /// Pointer position in combined coordinates, for the software-cursor fallback only — unused
    /// when the hardware cursor plane is active. Read from `shared_pointer` at each frame.
    pointer: (f64, f64),
    /// Where the loop thread last moved the pointer.
    shared_pointer: Arc<Mutex<(f64, f64)>>,
    paint: Option<paint::VelloDrmBridge>,
    /// The CPU renderer, when this device cannot run vello on its GPU.
    cpu: Option<cpu_present::CpuRenderer>,
    blanked: bool,
    /// True between a session pause and matching resume. Distinct from `blanked`: `paused` means these CRTCs aren't ours right now and the hardware must be rebuilt before presenting resumes.
    paused: bool,
    /// Reconfiguration commands from the loop thread (hotplug, VT switches, resume), drained at the top of every frame so a change lands between frames, not during one.
    commands: Option<Receiver<ScanoutCommand>>,
    /// Shared with the dispatch thread so dropping DRM master cannot race a
    /// present already in progress. [`Self::submit`] holds it across one complete
    /// frame — reconfiguration drain, paint and page-flip retirement — and
    /// [`Presenter::set_dpms`] across a power change.
    present_gate: Option<Arc<Mutex<()>>>,
    /// Opt-in physical-hardware profiling. The production session wrapper
    /// enables this while the readback presenter is under development so a
    /// hard reset still leaves exact phase costs in the flight record.
    profile_phases: bool,
    profile_seq: u64,
    frame_phases: Option<crate::shell::present::FramePhaseTiming>,
    /// Bounded aggregate diagnostics are always collected. They make a slow physical session
    /// actionable even when its wrapper forgot the opt-in per-frame trace environment: one log
    /// record every two seconds is cheap enough for production and survives a hard reset.
    phase_window: PhaseWindow,
    /// See [`DrmBackend::readback_presenting`].
    readback_presenting: Arc<AtomicBool>,
    /// Signalling facts learned while presenting, for the loop thread.
    signal_reports: Option<Sender<SignalReport>>,
    /// Vblanks of the page flips retired during the current frame, by card and CRTC.
    flip_vblanks: Vec<FlipVblank>,
    /// [`Presenter::display_timing`] of the frame just presented.
    display_timing: crate::shell::present::DisplayTiming,
    /// Frames admitted behind the one presenting while vblank-paced: 0, or 1
    /// under [`PIPELINED_ENV`].
    paced_queue_depth: usize,
}

/// Env var that keeps the vblank-paced presenter admitting one frame behind
/// the one presenting, as every presenter did before frames were scheduled
/// against the vblank. A hardware A/B knob for input-to-photon measurement.
pub const PIPELINED_ENV: &str = "TILO_PRESENT_PIPELINED";

/// One page flip retired by the presenter's blocking wait.
#[derive(Clone, Copy, Debug)]
struct FlipVblank {
    device_id: u64,
    crtc: smithay::reexports::drm::control::crtc::Handle,
    at: Instant,
}

/// Everything the render thread takes over from the backend in
/// [`Backend::take_presenter`].
///
/// Deliberately carries no vello bridge: wgpu/Vulkan devices must be created on
/// the thread which will drive them. An inline frame can initialise the
/// backend's bridge before the split (for example during startup or a backend
/// probe), and moving that live device to the render thread wedges Mesa
/// software adapters after the first submissions. [`DrmPresenter::new`] starts
/// without one and `paint_scanouts` constructs a fresh bridge on the
/// presentation thread. Handing a bridge across means adding a field here.
struct PresenterParts {
    scanouts: Vec<ScanoutEntry>,
    outputs: Vec<Output>,
    pointer: (f64, f64),
    shared_pointer: Arc<Mutex<(f64, f64)>>,
    cpu: Option<cpu_present::CpuRenderer>,
    blanked: bool,
    paused: bool,
    /// The scanouts and the commands addressed to them travel together:
    /// whoever owns one owns the other.
    commands: Option<Receiver<ScanoutCommand>>,
    present_gate: Option<Arc<Mutex<()>>>,
    profile_phases: bool,
    readback_presenting: Arc<AtomicBool>,
    signal_reports: Option<Sender<SignalReport>>,
}

impl DrmPresenter {
    /// The presenter for `parts`, with no GPU bridge yet: the first frame
    /// builds one on the thread that will drive it (see [`PresenterParts`]).
    fn new(parts: PresenterParts) -> Self {
        let PresenterParts {
            scanouts,
            outputs,
            pointer,
            shared_pointer,
            cpu,
            blanked,
            paused,
            commands,
            present_gate,
            profile_phases,
            readback_presenting,
            signal_reports,
        } = parts;
        Self {
            scanouts,
            outputs,
            pointer,
            shared_pointer,
            paint: None,
            cpu,
            blanked,
            paused,
            commands,
            present_gate,
            profile_phases,
            profile_seq: 0,
            frame_phases: None,
            phase_window: PhaseWindow::default(),
            readback_presenting,
            signal_reports,
            flip_vblanks: Vec::new(),
            display_timing: crate::shell::present::DisplayTiming::default(),
            paced_queue_depth: usize::from(
                std::env::var_os(PIPELINED_ENV).is_some_and(|value| value == "1"),
            ),
        }
    }

    /// Every powered output renders straight into its own scanout target and
    /// this presenter waits for its page flip, so a frame retires on a vblank.
    /// The CPU renderer and the combined readback do not wait for one.
    fn vblank_paced(&self) -> bool {
        !self.blanked
            && !self.paused
            && self.cpu.is_none()
            && self.scanouts.iter().any(|entry| !entry.powered_off)
            && self
                .scanouts
                .iter()
                .filter(|entry| !entry.powered_off)
                .all(|entry| entry.direct_targets.is_some() && !entry.present_by_readback)
    }

    /// The display timing of the frame whose flips were just retired. The
    /// vblank is the earliest among the outputs that pace the loop: the next
    /// frame must be due before the first of their next vblanks, or that
    /// output repeats a frame.
    fn frame_display_timing(
        &mut self,
        flip_queued_at: Option<Instant>,
    ) -> crate::shell::present::DisplayTiming {
        let retired = std::mem::take(&mut self.flip_vblanks);
        if !self.vblank_paced() {
            return crate::shell::present::DisplayTiming::default();
        }
        let fastest = fastest_direct_refresh(&self.scanouts, &self.outputs);
        let vblank = retired
            .iter()
            .filter(|flip| {
                self.scanouts.iter().any(|entry| {
                    entry.device_id == flip.device_id
                        && entry.scanout.crtc() == flip.crtc
                        && !entry.powered_off
                        && paces(refresh_of(&self.outputs, entry.id), fastest)
                })
            })
            .map(|flip| flip.at)
            .min();
        crate::shell::present::DisplayTiming {
            vblank_paced: true,
            vblank,
            flip_queued_at,
        }
    }
}

/// Publish whether any powered output presents through the combined readback.
fn publish_readback_presenting(scanouts: &[ScanoutEntry], flag: &AtomicBool) {
    flag.store(
        scanouts.iter().any(|entry| entry.present_by_readback),
        Ordering::Relaxed,
    );
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
/// The GPU budget for one direct frame. Missing it hands the output to the
/// readback presenter until the retry the failure earns.
const DIRECT_FRAME_DEADLINE: Duration = Duration::from_secs(1);

/// What the frame that just submitted does about one direct output's pending page flip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FlipWait {
    /// The output paces the loop: the frame completes when its flip retires.
    Await,
    /// A slower output: its flip retires through a later frame's drain, and until then
    /// the output skips frames.
    Leave,
    /// A slower output whose flip has not retired within [`DIRECT_RETIREMENT_TIMEOUT`]:
    /// checked once more without waiting, then demoted.
    Overdue,
}

/// Rates within this many percent of the fastest pace the loop with it: a 59.94 Hz panel
/// beside a 60 Hz one is the same cadence, not a slower output.
const PACING_TOLERANCE_PERCENT: u64 = 5;

/// Whether the loop waits for a direct output's pending flip. Outputs at the fastest
/// refresh (`fastest_mhz`) do; an output of unknown rate, or any output when no rate is
/// known, does too, as every output did before outputs were paced apart.
fn flip_wait(
    refresh_mhz: Option<u32>,
    fastest_mhz: Option<u32>,
    pending_for: Duration,
) -> FlipWait {
    if paces(refresh_mhz, fastest_mhz) {
        FlipWait::Await
    } else if pending_for >= DIRECT_RETIREMENT_TIMEOUT {
        FlipWait::Overdue
    } else {
        FlipWait::Leave
    }
}

/// Whether an output at `refresh_mhz` paces the loop beside the fastest one.
fn paces(refresh_mhz: Option<u32>, fastest_mhz: Option<u32>) -> bool {
    match (refresh_mhz, fastest_mhz) {
        (Some(rate), Some(fastest)) => {
            u64::from(rate).saturating_mul(100)
                >= u64::from(fastest)
                    .saturating_mul(100_u64.saturating_sub(PACING_TOLERANCE_PERCENT))
        }
        _ => true,
    }
}

/// The refresh rate `id` scans out at, in millihertz.
fn refresh_of(outputs: &[Output], id: OutputId) -> Option<u32> {
    outputs
        .iter()
        .find(|output| output.id == id)
        .and_then(|output| output.refresh_mhz)
}

/// The fastest refresh among the powered outputs presenting direct: the loop's cadence.
fn fastest_direct_refresh(scanouts: &[ScanoutEntry], outputs: &[Output]) -> Option<u32> {
    scanouts
        .iter()
        .filter(|entry| !entry.powered_off && entry.direct_targets.is_some())
        .filter_map(|entry| refresh_of(outputs, entry.id))
        .max()
}

/// Hand an output back to the readback presenter after direct scanout
/// failed in `stage`, until the back-off the failure earns has passed.
///
/// The readback presenter copies every frame through the CPU, so this is a
/// degradation the session log states at error level, every time.
fn demote_direct(entry: &mut ScanoutEntry, hdr: bool, stage: &str, reason: &str) {
    if entry.direct_targets.is_some() {
        // KMS may still scan out a target: the dumb front takes the CRTC
        // back before the targets are released.
        entry.scanout.restore_legacy_front();
        entry.direct_targets = None;
    }
    let degradation = if hdr {
        &mut entry.hdr_direct_degradation
    } else {
        &mut entry.direct_degradation
    };
    let detail = format!("{stage}: {reason}");
    let retry_in = degradation.record_failure(detail.clone(), Instant::now());
    let failures = degradation.failures();
    tracing::error!(
        output = entry.id.0,
        hdr,
        stage,
        %reason,
        failures,
        retry_in_ms = retry_in.as_millis(),
        "DRM direct scanout failed; presenting by readback until the retry"
    );
    eprintln!(
        "direct_scanout_status=degraded direct_scanout_reason={detail:?} output={} hdr={hdr} failures={failures} retry_in_ms={} DRM direct scanout degraded to readback",
        entry.id.0,
        retry_in.as_millis(),
    );
}

/// Direct-scanout failures since the outputs last ran direct stably.
fn direct_failures(scanouts: &[ScanoutEntry]) -> u32 {
    scanouts
        .iter()
        .map(|entry| {
            entry
                .direct_degradation
                .failures()
                .max(entry.hdr_direct_degradation.failures())
        })
        .max()
        .unwrap_or(0)
}

/// Why an output is not on direct scanout, if one is not.
fn direct_reason(scanouts: &[ScanoutEntry]) -> &str {
    scanouts
        .iter()
        .find_map(|entry| {
            entry
                .direct_ineligible
                .or_else(|| entry.direct_degradation.reason())
                .or_else(|| entry.hdr_direct_degradation.reason())
        })
        .unwrap_or("not-initialized")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one output, reported together when direct scanout is refused"
)]
struct DirectEligibility<'a> {
    normal_transform: bool,
    color_managed: bool,
    hardware_cursor: bool,
    event_pageflips: bool,
    renderer_pci: &'a str,
    renderer_drm_matches: bool,
    drm_bus_id: Option<&'a str>,
}

/// Why one output cannot render straight into its own scanout target, or `None` when it
/// can. Each output is judged alone: other outputs, and how they present, play no part.
fn direct_scanout_rejection(input: DirectEligibility<'_>) -> Option<&'static str> {
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
    backdrop_us: u128,
    /// Render-thread time from a direct frame's start to its completed GPU
    /// work, summed and at most, over the window's direct frames.
    direct_us: u128,
    max_direct_us: u128,
    /// Direct frames the renderer repaired sparsely rather than in full.
    direct_sparse_frames: u64,
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
            backdrop_us: 0,
            readback_bytes: 0,
            output_bytes: 0,
            bridge_us: 0,
            scanout_us: 0,
            direct_us: 0,
            max_direct_us: 0,
            direct_sparse_frames: 0,
            unprofiled_frames: 0,
        }
    }
}

impl PhaseWindow {
    fn record(&mut self, timing: paint::BridgePhaseTiming) {
        self.frames = self.frames.saturating_add(1);
        self.accumulate(timing);
    }

    fn accumulate(&mut self, timing: paint::BridgePhaseTiming) {
        self.submit_us = self.submit_us.saturating_add(timing.submit_us);
        self.gpu_readback_map_us = self
            .gpu_readback_map_us
            .saturating_add(timing.completed_readback_map_us);
        self.settle_us = self.settle_us.saturating_add(timing.settle_us);
        self.cpu_swizzle_us = self
            .cpu_swizzle_us
            .saturating_add(timing.cpu_copy_swizzle_us);
        self.backdrop_us = self.backdrop_us.saturating_add(timing.backdrop_us);
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

    /// One frame presented direct: `render_us` summed over its outputs, `sparse`
    /// when every output repaired damage only. Each output's phases go to
    /// [`Self::accumulate`] as it renders.
    fn record_direct_frame(&mut self, render_us: u128, sparse: bool) {
        self.frames = self.frames.saturating_add(1);
        self.direct_frames = self.direct_frames.saturating_add(1);
        self.direct_us = self.direct_us.saturating_add(render_us);
        self.max_direct_us = self.max_direct_us.max(render_us);
        if sparse {
            self.direct_sparse_frames = self.direct_sparse_frames.saturating_add(1);
        }
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
            backdrop_us: u128::from(value.saturating_mul(7)),
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
            assert_eq!(window.backdrop_us, 35);
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
            window.accumulate(paint::BridgePhaseTiming {
                submit_us: 2,
                completed_readback_map_us: 4,
                settle_us: 6,
                backdrop_us: 14,
                ..Default::default()
            });
            window.record_direct_frame(7_000, false);
            window.record_direct_frame(3_000, true);
            window.record_direct_drop();
            window.record_flip_completions(2);

            assert_eq!(window.frames, 2);
            assert_eq!(window.direct_frames, 2);
            assert_eq!(window.submit_us, 2);
            assert_eq!(window.gpu_readback_map_us, 4);
            assert_eq!(window.settle_us, 6);
            assert_eq!(window.backdrop_us, 14);
            assert_eq!(window.direct_us, 10_000);
            assert_eq!(window.max_direct_us, 7_000);
            assert_eq!(window.direct_sparse_frames, 1);
            assert_eq!(window.direct_dropped, 1);
            assert_eq!(window.flip_completions, 2);
            assert_eq!(window.readback_bytes, 0);
            assert_eq!(window.output_bytes, 0);
        }
        // Phase `two_direct_outputs_are_one_frame`.
        {
            let mut window = PhaseWindow::default();
            let mut frame = DirectFrame::default();
            frame.add(4_000, true);
            frame.add(5_000, false);
            frame.record(&mut window);
            let mut sparse = DirectFrame::default();
            sparse.add(1_000, true);
            sparse.add(2_000, true);
            sparse.record(&mut window);
            DirectFrame::default().record(&mut window);

            assert_eq!(window.frames, 2, "each frame counts once, not per output");
            assert_eq!(window.direct_frames, 2);
            assert_eq!(window.direct_us, 12_000);
            assert_eq!(window.max_direct_us, 9_000);
            assert_eq!(
                window.direct_sparse_frames, 1,
                "a frame is sparse only when every output repaired damage only"
            );
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
    fn direct_eligibility_is_per_scanout() {
        // Two outputs side by side: the normal one renders direct while its
        // rotated sibling names its own reason. Neither is judged by the count
        // of outputs it shares the desktop with.
        let normal = eligible();
        let rotated = DirectEligibility {
            normal_transform: false,
            ..eligible()
        };
        assert_eq!(direct_scanout_rejection(normal), None);
        assert_eq!(
            direct_scanout_rejection(rotated),
            Some("output transform is not normal")
        );
    }

    #[test]
    fn direct_path_eligibility_requires_the_full_contract_and_proven_identity() {
        // Phase `direct_path_requires_the_complete_safe_contract`.
        assert_eq!(direct_scanout_rejection(eligible()), None);
        for (rejected, reason) in [
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

fn physical_flip_proof(
    retirement: scanout::DirectRetirement,
    events: &[smithay::reexports::drm::control::PageFlipEvent],
    crtc: smithay::reexports::drm::control::crtc::Handle,
    eligible_clock: bool,
) -> Option<crate::shell::present::PhysicalPresentation> {
    if retirement != scanout::DirectRetirement::Complete || !eligible_clock {
        return None;
    }
    let event = events.iter().find(|event| event.crtc == crtc)?;
    Some(crate::shell::present::PhysicalPresentation {
        timestamp: event.duration,
        sequence: u64::from(event.frame),
        refresh: None,
    })
}

impl DrmPresenter {
    /// The only way a frame reaches the scanouts from the render thread: every [`Presenter`]
    /// entry point that presents lands here.
    ///
    /// The present gate is held for the whole frame, flip retirement included, so dropping
    /// DRM master for a VT switch (`DrmDispatchState::perform(SessionAction::DropMaster)`)
    /// waits until no output's page flip is queued or in flight. A `Pause` queued before
    /// that drop is drained here under the same gate, so the frame after it paints nothing.
    fn submit(
        &mut self,
        frame: ScanoutFrame<'_>,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        self.frame_phases = None;
        self.display_timing = crate::shell::present::DisplayTiming::default();
        self.flip_vblanks.clear();
        if let Some(bridge) = &mut self.paint {
            let _ = bridge.take_phase_timing();
        }
        self.pointer = *self
            .shared_pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let present_gate = self.present_gate.clone();
        let _quiesced = quiesce_presentations(present_gate.as_ref());
        self.drain_commands();
        if self.blanked || self.paused {
            return None;
        }
        let before = self
            .scanouts
            .first()
            .map(|entry| entry.scanout.flip_submission());
        let sample = paint_scanouts(
            &mut self.scanouts,
            &self.outputs,
            &mut self.paint,
            &mut self.cpu,
            self.pointer,
            frame,
            Some(&mut self.phase_window),
        );
        publish_readback_presenting(&self.scanouts, &self.readback_presenting);
        let flip_queued_at = sample.direct_queued.then(Instant::now);
        let retirement_started = self.profile_phases.then(Instant::now);
        let retirement_attempted = sample.direct_queued
            || (self.scanouts.len() == 1
                && self
                    .scanouts
                    .first()
                    .map(|entry| entry.scanout.flip_submission())
                    != before);
        // A slower output's flip from an earlier frame is checked as well: one that is overdue
        // is demoted even on a frame that queued nothing.
        let physical = if sample.direct_queued || self.direct_flip_pending() {
            self.retire_direct_flips()
        } else if self.scanouts.len() == 1
            && self
                .scanouts
                .first()
                .map(|entry| entry.scanout.flip_submission())
                != before
        {
            self.retire_physical_flip(0, DIRECT_RETIREMENT_TIMEOUT)
        } else {
            None
        };
        if let Some(started) = retirement_started {
            self.frame_phases = self
                .paint
                .as_ref()
                .and_then(paint::VelloDrmBridge::last_phase_timing)
                .map(|paint| crate::shell::present::FramePhaseTiming {
                    paint,
                    kms_retirement_us: if retirement_attempted {
                        started.elapsed().as_micros()
                    } else {
                        0
                    },
                });
        }
        self.display_timing = self.frame_display_timing(flip_queued_at);
        self.emit_phase_profile(sample);
        if let Some(bridge) = &mut self.paint {
            let _ = bridge.take_phase_timing();
        }
        report_signal_status(&mut self.scanouts, self.signal_reports.as_ref());
        physical
    }

    /// Tighten `Presenter::present_frame`'s completion boundary to the KMS page-flip events of
    /// the outputs that pace the loop: those at the fastest refresh (`flip_wait`).
    /// `PresentPipe` only frees producer capacity after this method returns, so the loop
    /// renders at the fastest output's rate. A slower output's flip is left pending and
    /// retires through the next frame's non-blocking drain; until it has, that output skips
    /// the frames it could not show (`DirectOutcome::Deferred`), so no output renders more
    /// than once per its own vblank and no target KMS owns is rendered into. Waiting for
    /// every pacing output, rather than the first, keeps one whose vblank trails another's
    /// from dropping every other frame. Each gets the whole timeout, so one wedged output
    /// cannot spend a healthy sibling's budget and demote it; a slow output whose flip is
    /// overdue is demoted without a wait.
    fn retire_direct_flips(&mut self) -> Option<crate::shell::present::PhysicalPresentation> {
        let fastest = fastest_direct_refresh(&self.scanouts, &self.outputs);
        let mut proof = None;
        // Each pass retires or demotes one output; the bound is a backstop.
        for _ in 0..self.scanouts.len() {
            let now = Instant::now();
            let Some((index, timeout)) =
                self.scanouts.iter().enumerate().find_map(|(index, entry)| {
                    let targets = entry.direct_targets.as_ref()?;
                    let queued_at = targets.flip_queued_at()?;
                    let wait = flip_wait(
                        refresh_of(&self.outputs, entry.id),
                        fastest,
                        now.saturating_duration_since(queued_at),
                    );
                    match wait {
                        FlipWait::Await => Some((index, DIRECT_RETIREMENT_TIMEOUT)),
                        FlipWait::Overdue => Some((index, Duration::ZERO)),
                        FlipWait::Leave => None,
                    }
                })
            else {
                break;
            };
            let retired = self.retire_physical_flip(index, timeout);
            proof = proof.or(retired);
        }
        proof
    }

    /// Some direct output still waits for a page flip queued by an earlier frame.
    fn direct_flip_pending(&self) -> bool {
        self.scanouts.iter().any(|entry| {
            entry
                .direct_targets
                .as_ref()
                .is_some_and(scanout::DirectScanoutTargets::flip_pending)
        })
    }

    /// Wait up to `timeout` for the page flip queued on `scanouts[index]`.
    /// Every completion read on the way goes to the scanout it belongs to: the
    /// card's sibling outputs share this event stream.
    fn retire_physical_flip(
        &mut self,
        index: usize,
        timeout: Duration,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        let single_output = self.scanouts.len() == 1;
        let entry = self.scanouts.get_mut(index)?;
        let (retirement, completions) = entry.scanout.wait_flip_completions(timeout);
        if retirement != scanout::DirectRetirement::Complete {
            entry.scanout.invalidate_physical_feedback();
        }
        // Every completion read here comes from this card's event stream. A
        // driver without monotonic event stamps leaves the wakeup as the vblank.
        let woke = Instant::now();
        let monotonic = entry.scanout.monotonic_flip_timestamps();
        let device_id = entry.device_id;
        self.flip_vblanks
            .extend(completions.iter().map(|event| FlipVblank {
                device_id,
                crtc: event.crtc,
                at: if monotonic {
                    scanout::instant_from_monotonic(event.duration)
                } else {
                    woke
                },
            }));
        self.phase_window.record_flip_completions(completions.len());
        let proof = physical_flip_proof(
            retirement,
            &completions,
            entry.scanout.crtc(),
            single_output && entry.scanout.can_prove_physical_flip(),
        )
        .map(|mut proof| {
            proof.refresh = if entry.scanout.adaptive_sync_active() {
                None
            } else {
                self.outputs
                    .first()
                    .and_then(|output| output.refresh_mhz)
                    .and_then(|rate| 1_000_000_000_000_u64.checked_div(u64::from(rate)))
                    .map(Duration::from_nanos)
            };
            proof
        });
        let completed: Vec<_> = completions
            .iter()
            .map(|event| (device_id, event.crtc))
            .collect();
        route_flip_completions(&mut self.scanouts, &completed);
        let entry = self.scanouts.get_mut(index)?;
        if retirement == scanout::DirectRetirement::TimedOut && entry.direct_targets.is_some() {
            let hdr = entry
                .direct_targets
                .as_ref()
                .is_some_and(|targets| targets.format().is_hdr());
            demote_direct(
                entry,
                hdr,
                "retire",
                &format!(
                    "page flip did not retire within {} ms",
                    DIRECT_RETIREMENT_TIMEOUT.as_millis()
                ),
            );
        }
        proof
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
                 avg_backdrop_us={} readback_bytes={} output_bytes={} bridge_us={} scanout_us={} \
                 avg_direct_us={} max_direct_us={} direct_sparse_frames={} \
                 direct_failures={} \
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
                self.phase_window
                    .backdrop_us
                    .checked_div(u128::from(frames))
                    .unwrap_or(0),
                summary.readback_bytes,
                summary.output_bytes,
                summary.bridge_us,
                summary.scanout_us,
                self.phase_window
                    .direct_us
                    .checked_div(u128::from(summary.direct_frames))
                    .unwrap_or(0),
                self.phase_window.max_direct_us,
                self.phase_window.direct_sparse_frames,
                direct_failures(&self.scanouts),
                if self
                    .scanouts
                    .iter()
                    .any(|entry| entry.direct_targets.is_some())
                {
                    "enabled"
                } else {
                    "unavailable"
                },
                direct_reason(&self.scanouts),
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
             backdrop_us={} readback_bytes={} output_bytes={} DRM render frame phases",
            self.profile_seq,
            timing.submit_us,
            timing.completed_readback_map_us,
            timing.settle_us,
            timing.cpu_copy_swizzle_us,
            timing.backdrop_us,
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
            // that `paint_scanouts` drains once and routes by crtc; a removed output takes its pending completion with it.
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
        ScanoutCommand::SetCursorScale { output, scale } => {
            for entry in scanouts.iter_mut().filter(|entry| entry.id == output) {
                entry.scanout.set_cursor_scale(scale);
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
                if entry.direct_targets.is_some() {
                    // KMS may still scan out a target: the dumb front takes the
                    // CRTC back before the targets are released.
                    entry.scanout.restore_legacy_front();
                    entry.direct_targets = None;
                }
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
    /// What the scene's floating surfaces are drawn over.
    backdrop: Option<&'a std::sync::Arc<ui::backdrop::BackdropFrame>>,
}

// These two constructors are the only places a frame is translated for the scanouts, so a field
// added to or removed from `ScanoutFrame` changes them and `paint_scanouts`, nothing else.
impl<'a> ScanoutFrame<'a> {
    /// A complete scene with no relation to what is shown: every output repaints it whole.
    fn scene_only(scene: &'a Scene) -> Self {
        Self {
            scene,
            damage: None,
            hdr_layers: &[],
            overview: None,
            workspace_id: None,
            workspace_hold: false,
            workspace_clip: None,
            backdrop: None,
        }
    }
}

impl<'a> From<&'a FramePaint> for ScanoutFrame<'a> {
    /// A producer frame. Revision 0 carries no retained-damage authority
    /// ([`FramePaint::full`]), so it presents without damage, as a scene-only frame does.
    fn from(frame: &'a FramePaint) -> Self {
        Self {
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
            backdrop: frame.backdrop.as_ref(),
        }
    }
}

/// Hold the present gate (see [`DrmPresenter::present_gate`]): every frame submission and the
/// master drop on a VT switch (`DrmDispatchState::perform(SessionAction::DropMaster)`) go
/// through here, so one of them waits for the other. A poisoned gate still quiesces: the frame that panicked is over, and
/// refusing the lock would let master drop under the next one.
fn quiesce_presentations(gate: Option<&Arc<Mutex<()>>>) -> Option<std::sync::MutexGuard<'_, ()>> {
    gate.map(|gate| {
        gate.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    })
}

/// An unchanged frame, or an overview fade that carries its own panel patch,
/// must not read the combined framebuffer back from the GPU.
pub(crate) fn skips_full_framebuffer_readback(unchanged: bool, overview: bool) -> bool {
    unchanged || overview
}

/// One head of a multi-output present.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PresentedHead {
    /// A real connector page-flips. A virtual head publishes a clip.
    pub pageflip: bool,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// The clip a virtual head publishes from an already-encoded frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PublishedClip {
    pub head: usize,
    pub scene_x: u32,
    pub scene_y: u32,
    pub local_x: u16,
    pub local_y: u16,
    pub width: u16,
    pub height: u16,
}

/// Bytes copied onto scanouts for these clips.
pub(crate) fn encoded_clip_bytes(clips: &[PublishedClip]) -> u64 {
    clips.iter().fold(0_u64, |bytes, clip| {
        bytes.saturating_add(
            u64::from(clip.width)
                .saturating_mul(u64::from(clip.height))
                .saturating_mul(4),
        )
    })
}

/// Clips a two-head virtual present publishes.
///
/// `None` keeps the combined readback, which is what a real connector
/// page-flips. An empty `clips` list still publishes only the first head:
/// the other scanout's buffer is already showing its encoded frame.
pub(crate) fn virtual_encoded_clips(
    heads: &[PresentedHead],
    clips: &[vello::FineDamageRect],
) -> Option<Vec<PublishedClip>> {
    if heads.len() < 2 || heads.iter().any(|head| head.pageflip) {
        return None;
    }
    let boxes: Vec<(i32, i32, u32, u32)> = heads
        .iter()
        .map(|head| (head.x, head.y, head.width, head.height))
        .collect();
    let mut regions = Vec::new();
    if clips.is_empty() {
        let head = heads.first()?;
        regions.push(vello::FineDamageRect {
            x: u32::try_from(head.x.max(0)).ok()?,
            y: u32::try_from(head.y.max(0)).ok()?,
            width: head.width,
            height: head.height,
        });
    } else {
        regions.extend_from_slice(clips);
    }
    let mut published = Vec::with_capacity(regions.len());
    for clip in regions {
        let clip = if close_frame_spares_other_scanouts(clip, &boxes) {
            clip
        } else {
            clip_to_primary_scanout(clip, &boxes)?
        };
        if !close_frame_spares_other_scanouts(clip, &boxes) {
            return None;
        }
        let head = boxes.iter().position(|&(x, y, width, height)| {
            output_local_clip(x, y, (width, height), clip).is_some()
        })?;
        let (x, y, width, height) = boxes.get(head).copied()?;
        let (local_x, local_y, local_w, local_h) = output_local_clip(x, y, (width, height), clip)?;
        let scene_x = u32::try_from(i64::from(x).saturating_add(i64::from(local_x))).ok()?;
        let scene_y = u32::try_from(i64::from(y).saturating_add(i64::from(local_y))).ok()?;
        published.push(PublishedClip {
            head,
            scene_x,
            scene_y,
            local_x,
            local_y,
            width: local_w,
            height: local_h,
        });
    }
    (!published.is_empty()).then_some(published)
}

/// Swizzle one already-encoded clip into an XRGB scanout. Pixels outside
/// the clip stay as they are, so the other output is not part of the copy.
pub(crate) fn write_encoded_clip_xrgb(
    source: SubrectSource<'_>,
    clip: (u16, u16, u16, u16),
    dst: &mut [u8],
    pitch: usize,
) {
    let (clip_x, clip_y, clip_w, clip_h) = clip;
    let clip_x = usize::from(clip_x);
    let clip_y = usize::from(clip_y);
    let clip_w = usize::from(clip_w);
    let clip_h = usize::from(clip_h);
    if pitch == 0 {
        return;
    }
    for row in 0..clip_h {
        let dest_y = clip_y.saturating_add(row);
        let src_y = source.src_y.saturating_add(dest_y);
        if src_y >= source.src_h {
            break;
        }
        let Some(row_off) = dest_y.checked_mul(pitch) else {
            break;
        };
        for col in 0..clip_w {
            let dest_x = clip_x.saturating_add(col);
            let src_x = source.src_x.saturating_add(dest_x);
            if src_x >= source.src_w {
                continue;
            }
            let Some(src_off) = src_y
                .checked_mul(source.src_w)
                .and_then(|offset| offset.checked_add(src_x))
                .and_then(|offset| offset.checked_mul(4))
            else {
                continue;
            };
            let Some(src_pix) = source.src_rgba.get(src_off..src_off.saturating_add(4)) else {
                continue;
            };
            let Some(dest_off) = row_off.checked_add(dest_x.saturating_mul(4)) else {
                continue;
            };
            let Some(dest_pix) = dst.get_mut(dest_off..dest_off.saturating_add(4)) else {
                continue;
            };
            if let ([dest_b, dest_g, dest_r, dest_x], [src_r, src_g, src_b, _]) =
                (dest_pix, src_pix)
            {
                *dest_b = *src_b;
                *dest_g = *src_g;
                *dest_r = *src_r;
                *dest_x = 0xFF;
            }
        }
    }
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

/// The frame run after a workspace switch. Every held sample, including the
/// first visit, stays on one display's clip. A settled return may still
/// replay. Later samples of either direction skip the upload.
pub(crate) fn workspace_frame_upload(
    changed: bool,
    has_snapshot: bool,
    hold: bool,
) -> WorkspaceUpload {
    if hold && changed {
        WorkspaceUpload::Clip
    } else if hold {
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

/// Construct the vello bridge on the calling thread if there is none. Returns whether one exists.
fn ensure_bridge(paint: &mut Option<paint::VelloDrmBridge>, target_size: (u32, u32)) -> bool {
    if paint.is_some() {
        return true;
    }
    match paint::VelloDrmBridge::new(target_size.0, target_size.1) {
        Ok(mut bridge) => {
            // Four `Instant` samples per completed frame buy the minimum useful physical
            // performance record. Verbose per-frame output remains opt-in in DrmPresenter;
            // ordinary sessions emit only one aggregate every two seconds.
            bridge.set_phase_timing_enabled(true);
            *paint = Some(bridge);
            true
        }
        Err(e) => {
            tracing::error!("DRM backend: VelloDrmBridge::new: {e}");
            false
        }
    }
}

/// Whether the bridge's target should span the desktop this frame: some powered output
/// presented through the combined readback last frame, or cannot render direct.
fn expects_readback(scanouts: &[ScanoutEntry]) -> bool {
    scanouts.iter().any(|entry| {
        !entry.powered_off && (entry.present_by_readback || entry.direct_ineligible.is_some())
    })
}

/// The extent of the bridge's own target. The combined readback renders the bounding box of
/// every output, so while one presents through it (`readback`) the target spans the desktop.
/// Otherwise every output renders into its own GPU target and nothing needs that box: the
/// bridge's target is the output at the scene origin, which renders straight into it, so no
/// texture is larger than an output and none rasterises the space between outputs.
fn bridge_extent(outputs: &[Output], readback: bool) -> (u32, u32) {
    let combined = combined_geometry(outputs);
    if readback {
        return combined;
    }
    outputs
        .iter()
        .find(|output| output.position == (0, 0))
        .or_else(|| {
            outputs.iter().max_by_key(|output| {
                u64::from(output.size.0).saturating_mul(u64::from(output.size.1))
            })
        })
        .map_or(combined, |output| output.size)
}

/// Render the frame's scene into `scanouts`. Each output renders through its own view of the
/// scene where it can; the rest present through the combined readback. `*paint` is lazily
/// (re)allocated to [`bridge_extent`]. Shared by
/// [`DrmPresenter::submit`] (post-split, render thread) and
/// [`DrmBackend::submit_inline`] (inline path, pre-split) — same painting logic,
/// different owner of the scanouts and the pointer. Nothing else calls it.
#[allow(
    clippy::too_many_lines,
    reason = "one dispatcher keeps bridge lifetime, sparse eligibility, diagnostics, and the single versus multi-output presentation boundaries together"
)]
fn paint_scanouts(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    cpu: &mut Option<cpu_present::CpuRenderer>,
    pointer: (f64, f64),
    frame: ScanoutFrame<'_>,
    mut phase_window: Option<&mut PhaseWindow>,
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
    // Reconfiguration is drained before a queued frame is presented. An older
    // multi-output clip may therefore arrive after hot-unplug to one output.
    // It has no scene to rasterise: preserve the shown pixels until the next
    // complete producer frame rather than clearing the new scanout to black.
    if frame.workspace_hold
        && scene.encoding().is_empty()
        && (scanouts.len() == 1 || cpu_present::cpu_render_reason().is_some())
    {
        return PaintScanoutSample::default();
    }
    let combined = combined_geometry(outputs);
    if combined.0 == 0 || combined.1 == 0 {
        return PaintScanoutSample::default();
    }
    // Chosen once from the device, never as a fallback: a GPU that cannot
    // run vello must not be initialised for the desktop at all.
    if cpu_present::cpu_render_reason().is_some() {
        let renderer = cpu.get_or_insert_with(cpu_present::CpuRenderer::new);
        paint_scanouts_cpu(scanouts, outputs, renderer, pointer, frame, combined);
        return PaintScanoutSample::default();
    }
    let readback_expected = expects_readback(scanouts);
    let target_size = bridge_extent(outputs, readback_expected);
    if target_size.0 == 0 || target_size.1 == 0 {
        return PaintScanoutSample::default();
    }
    let sharing = TargetSharing::of(
        readback_expected,
        scanouts.iter().filter(|entry| !entry.powered_off).count(),
    );
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
    if !ensure_bridge(paint, target_size) {
        return PaintScanoutSample::default();
    }
    let mut bridge_resized = false;
    {
        // The block above just initialised `*paint` if it was `None`, or
        // already returned on failure — a miss here would mean that
        // silently didn't stick, so skip this frame rather than panic.
        let Some(bridge) = paint.as_mut() else {
            return PaintScanoutSample::default();
        };
        bridge.set_backdrop(frame.backdrop.cloned());
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

    // Fine damage is in the combined scene's native pixels. The single-output readback can
    // still present scaled/offset logical metadata densely, but sparse repair must not assume
    // that mapping is one-to-one until the renderer owns an explicit transform contract.
    let sparse_coordinates_match = outputs.len() == 1
        && outputs.first().is_some_and(|output| {
            output.position == (0, 0)
                && output.scale.to_bits() == 1.0_f32.to_bits()
                && output.size == combined
                && output.transform == settings_store::hardware::OutputTransform::Normal
        });
    // Scanouts on one card share an open file description, so their page-flip completions
    // arrive on one event stream. Hybrid systems have *one stream per card*, and CRTC
    // handles are only meaningful inside that card. Drain exactly one representative per
    // device and route only to siblings from the same device, direct or dumb: draining per
    // scanout would hand one output its sibling's completion and wedge the sibling.
    let completions = drain_page_flip_completions(scanouts);
    if let Some(window) = phase_window.as_deref_mut() {
        window.record_flip_completions(completions);
    }

    // Every output that can is rendered on the GPU straight into its own scanout target
    // (`direct_scanout_rejection` names why one cannot). The rest present through the
    // readback below, and only they do.
    let mut direct = PaintScanoutSample::default();
    let mut readback = false;
    {
        let Some(bridge) = paint.as_mut() else {
            return PaintScanoutSample::default();
        };
        let renderer = RendererIdentity::of(bridge);
        let mut render_totals = DirectFrame::default();
        for entry in scanouts.iter_mut() {
            if entry.powered_off {
                entry.present_by_readback = false;
                entry.readback_current = false;
                continue;
            }
            let view = output_view(entry, outputs);
            let outcome = present_direct(
                entry,
                bridge,
                &renderer,
                &view,
                &frame,
                phase_window.as_deref_mut(),
                sharing,
            );
            entry.present_by_readback = outcome == DirectOutcome::Readback;
            match outcome {
                DirectOutcome::Submitted {
                    queued,
                    render_us,
                    sparse,
                } => {
                    direct.direct_queued |= queued;
                    render_totals.add(render_us, sparse);
                    entry.readback_current = false;
                }
                // The output keeps its current image.
                DirectOutcome::Dropped | DirectOutcome::Kept | DirectOutcome::Deferred => {
                    entry.readback_current = false;
                }
                DirectOutcome::Readback => readback = true,
            }
        }
        if let Some(window) = phase_window {
            render_totals.record(window);
        }
        // Only outputs on the direct path keep a retained surface.
        let live: Vec<OutputId> = scanouts
            .iter()
            .filter(|entry| !entry.powered_off && entry.direct_targets.is_some())
            .map(|entry| entry.id)
            .collect();
        bridge.retain_output_views(&live);
    }
    if !readback {
        return direct;
    }
    // An output fell back to the readback while the bridge's target was one output's: the
    // readback renders the bounding box of every output, so the target grows to it, and the
    // next frames keep that size while any output presents by readback (`bridge_extent`).
    let mut resized_for_readback = false;
    if let Some(bridge) = paint.as_mut()
        && bridge.size() != combined
    {
        tracing::info!(
            width = combined.0,
            height = combined.1,
            "DRM backend: an output presents by readback; rendering the combined desktop"
        );
        if let Err(e) = bridge.resize(combined.0, combined.1) {
            tracing::error!("DRM backend: VelloDrmBridge::resize: {e}");
            return direct;
        }
        for entry in scanouts.iter_mut() {
            entry.scanout.invalidate_xrgb_history();
        }
        resized_for_readback = true;
    }

    // One unrotated, uncalibrated output: render and swizzle straight into its back dumb
    // buffer (XRGB8888) in a single fused pass, then page-flip. HDR without the direct path
    // goes through the combined readback below, into 10-bit dumb buffers; everything else
    // stays XRGB8888 and shows HDR layers through their tone-mapped rendition.
    if scanouts.len() == 1
        && let (Some(entry), Some(bridge)) = (scanouts.first_mut(), paint.as_mut())
        && entry.transform == settings_store::hardware::OutputTransform::Normal
        && entry.color_transform.is_none()
        && !entry.sync_hdr_readback()
    {
        // Its dumb buffers now hold this path's image, not the combined readback's.
        entry.readback_current = false;
        return present_single_xrgb(
            entry,
            bridge,
            scene,
            damage,
            Some(pointer),
            sparse_coordinates_match,
        );
    }

    // The combined readback: render the bounding box of every output once, then copy each
    // remaining output's sub-rect out of it on the CPU.
    // A target grown this frame holds no readback image to patch.
    let mut stale = resized_for_readback;
    for entry in scanouts
        .iter_mut()
        .filter(|entry| entry.present_by_readback)
    {
        let format = entry.scanout.format();
        entry.sync_hdr_readback();
        // A format change swapped in the other dumb pair, which shows nothing current.
        stale |= !entry.readback_current || entry.scanout.format() != format;
    }
    // An output that was not on the readback last frame (it was direct, or is new) does not
    // show the readback's last image, so the shortcuts that patch or skip pixels already on
    // the scanout would leave it stale: upload the whole scene once.
    let sample = if stale && let Some(bridge) = paint.as_mut() {
        upload_combined_scene(
            scanouts,
            outputs,
            bridge,
            pointer,
            &CombinedUpload {
                scene,
                hdr_layers,
                workspace_id: frame.workspace_id,
                note_workspace: frame.overview.is_none(),
            },
        )
    } else {
        render_combined_readback(scanouts, outputs, paint, pointer, frame)
    };
    for entry in scanouts
        .iter_mut()
        .filter(|entry| entry.present_by_readback)
    {
        entry.readback_current = true;
    }
    PaintScanoutSample {
        direct_queued: direct.direct_queued,
        ..sample
    }
}

/// The identity of the GPU the bridge renders on, for matching it to each scanout's card.
struct RendererIdentity {
    pci_bus_id: String,
    drm_device: Option<ui::dmabuf_import::VulkanDrmDeviceIdentity>,
    /// `drm_device` for the log.
    described: String,
}

impl RendererIdentity {
    /// An adapter that cannot be identified matches no card, so every output fails
    /// closed with "renderer and scanout use different GPUs".
    fn of(bridge: &paint::VelloDrmBridge) -> Self {
        match bridge.adapter_info() {
            Ok(adapter) => Self {
                described: adapter.drm_device.map_or_else(
                    || "unavailable".to_owned(),
                    |identity| {
                        format!(
                            "primary={:?},render={:?}",
                            identity.primary, identity.render
                        )
                    },
                ),
                pci_bus_id: adapter.pci_bus_id,
                drm_device: adapter.drm_device,
            },
            Err(error) => {
                tracing::debug!(%error, "DRM backend: the renderer's adapter identity is unavailable");
                Self {
                    pci_bus_id: String::new(),
                    drm_device: None,
                    described: "unavailable".to_owned(),
                }
            }
        }
    }
}

/// The part of the scene `entry` shows.
fn output_view(entry: &ScanoutEntry, outputs: &[Output]) -> paint::OutputView {
    view_of(entry.id, entry.position, entry.scanout.size(), outputs)
}

/// The view of output `id`, at `position` in the scene: its extent from the output table, or
/// its scanout `mode` while the table does not list it.
fn view_of(
    id: OutputId,
    position: (i32, i32),
    mode: (u32, u32),
    outputs: &[Output],
) -> paint::OutputView {
    let size = outputs
        .iter()
        .find(|output| output.id == id)
        .map_or(mode, |output| output.size);
    paint::OutputView::at(id, position, size)
}

/// What the direct path did with one output's frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DirectOutcome {
    /// The frame went to KMS: a flip is queued (`queued`), or a modeset showed it.
    /// `render_us` is the GPU frame's wall time; `sparse` that it repaired damage only.
    Submitted {
        queued: bool,
        render_us: u128,
        sparse: bool,
    },
    /// KMS refused the flip as busy: the output keeps its current image this frame.
    Dropped,
    /// The output's previous flip has not retired, so KMS owns both its targets: it skips
    /// this frame, which it could not show, and owes the frame's damage to its next render.
    /// This is how an output slower than the loop's cadence is paced to its own vblank.
    Deferred,
    /// Nothing new reaches this output (no damage falls on it, or the frame is a
    /// clip that only the readback replays): it keeps its current image.
    Kept,
    /// The output is not eligible, or its direct frame failed: the readback presents it.
    Readback,
}

/// The direct renders of one frame, recorded as one frame however many outputs it took.
#[derive(Default)]
struct DirectFrame {
    outputs: usize,
    render_us: u128,
    sparse: bool,
}

impl DirectFrame {
    fn add(&mut self, render_us: u128, sparse: bool) {
        self.sparse = sparse && (self.outputs == 0 || self.sparse);
        self.outputs = self.outputs.saturating_add(1);
        self.render_us = self.render_us.saturating_add(render_us);
    }

    fn record(&self, window: &mut PhaseWindow) {
        if self.outputs > 0 {
            window.record_direct_frame(self.render_us, self.sparse);
        }
    }
}

/// Who else renders into the bridge's own target this frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TargetSharing {
    /// Several outputs and no readback: every output's pixels are its own, the bridge's target
    /// included. An undamaged output keeps its image, and one whose flip is pending owes its
    /// damage to its next render.
    PerOutput,
    /// One powered output and no readback: the bridge's target is that output's alone, and
    /// only the direct path renders into it.
    Lone,
    /// A readback renders the bridge's target too. It can bring the target up to a revision
    /// KMS does not show on a direct output (after that output's busy flip), so the target
    /// proves nothing about what a direct output shows.
    Readback,
}

impl TargetSharing {
    fn of(readback_expected: bool, powered_outputs: usize) -> Self {
        if readback_expected {
            Self::Readback
        } else if powered_outputs > 1 {
            Self::PerOutput
        } else {
            Self::Lone
        }
    }

    /// Whether `view`'s retained pixels are its own: a surface of its own, or the bridge's
    /// target while no other output or readback shares it.
    fn owns_pixels(self, bridge: &paint::VelloDrmBridge, view: &paint::OutputView) -> bool {
        self == Self::PerOutput || !bridge.view_is_whole_scene(view)
    }
}

/// Whether a direct output keeps the image KMS shows for `frame`: no render, no copy into a
/// scanout slot, no flip. Its targets must have been `shown` and its retained pixels must hold
/// the previous revision with no skipped frame's damage owed (`keep_unchanged_output`), which
/// it then advances to this revision.
///
/// An output whose pixels are its own keeps any frame whose damage falls elsewhere. The lone
/// output, whose view is the scene as a whole, keeps only a frame proven identical to the
/// previous one (empty damage). An output sharing the bridge's target with a readback keeps
/// nothing: the readback may have re-rendered that target after this output's flip failed.
fn keeps_shown_image(
    bridge: &mut paint::VelloDrmBridge,
    view: &paint::OutputView,
    frame: &ScanoutFrame<'_>,
    shown: bool,
    sharing: TargetSharing,
) -> bool {
    let Some(damage) = frame.damage else {
        return false;
    };
    let unchanged = damage.regions.is_some_and(<[_]>::is_empty);
    let eligible =
        sharing.owns_pixels(bridge, view) || (sharing == TargetSharing::Lone && unchanged);
    eligible && frame.hdr_layers.is_empty() && shown && bridge.keep_unchanged_output(view, damage)
}

/// Render one output straight into its own direct scanout target and hand it to KMS.
///
/// The direct path stays narrow until every transformed, colour-managed and software-cursor
/// composition case has an exact GPU implementation; every unsupported case remains on the
/// established readback presenter, with the reason logged.
#[allow(
    clippy::too_many_lines,
    reason = "one output's eligibility, target lifetime, render and submission form one transaction whose failures each pick the presenter"
)]
fn present_direct(
    entry: &mut ScanoutEntry,
    bridge: &mut paint::VelloDrmBridge,
    renderer: &RendererIdentity,
    view: &paint::OutputView,
    frame: &ScanoutFrame<'_>,
    mut phase_window: Option<&mut PhaseWindow>,
    sharing: TargetSharing,
) -> DirectOutcome {
    let drm_bus_id = entry.scanout.drm_bus_id();
    let eligibility = DirectEligibility {
        normal_transform: entry.transform == settings_store::hardware::OutputTransform::Normal,
        color_managed: entry.color_transform.is_some(),
        hardware_cursor: entry.scanout.hardware_cursor_active(),
        event_pageflips: entry.scanout.event_pageflips_available(),
        renderer_pci: &renderer.pci_bus_id,
        renderer_drm_matches: scanout::renderer_drm_node_matches_card(
            renderer.drm_device,
            entry.scanout.drm_device_id(),
        ),
        drm_bus_id: drm_bus_id.as_deref(),
    };
    if let Some(reason) = direct_scanout_rejection(eligibility) {
        if entry.direct_ineligible != Some(reason) {
            // Keep this line machine-parseable and durable. The session flight recorder may
            // start receiving output a moment after the render thread, so the periodic phase
            // summary repeats the stable rejection too.
            eprintln!(
                "direct_scanout_status=unavailable reason={reason:?} output={} renderer_pci={} renderer_drm={:?} renderer_drm_matches={} drm_bus_id={:?} hardware_cursor={} event_pageflips={} DRM direct scanout unavailable",
                entry.id.0,
                renderer.pci_bus_id,
                renderer.described,
                eligibility.renderer_drm_matches,
                drm_bus_id.as_deref().unwrap_or("unavailable"),
                eligibility.hardware_cursor,
                eligibility.event_pageflips,
            );
            entry.direct_ineligible = Some(reason);
        }
        if entry.direct_targets.is_some() {
            // The output changed shape under live targets: the dumb front takes the CRTC
            // back before they are released.
            entry.scanout.restore_legacy_front();
            entry.direct_targets = None;
        }
        return DirectOutcome::Readback;
    }
    // Eligibility is a fact about the output's current shape, so a cursor plane regained
    // re-enables it.
    entry.direct_ineligible = None;
    // HDR renders into 10-bit (or half-float) targets; targets in the other format go
    // first, CRTC back on the dumb front.
    let want_hdr = entry.hdr_wanted();
    if entry
        .direct_targets
        .as_ref()
        .is_some_and(|targets| targets.format().is_hdr() != want_hdr)
    {
        entry.scanout.restore_legacy_front();
        entry.direct_targets = None;
    }
    let degradation = if want_hdr {
        &entry.hdr_direct_degradation
    } else {
        &entry.direct_degradation
    };
    if entry.direct_targets.is_none() && !degradation.blocked(Instant::now()) {
        let earlier_failures = degradation.failures();
        match create_direct_targets(&entry.scanout, bridge, want_hdr, view.size) {
            Ok(targets) => {
                eprintln!(
                    "direct_scanout_status=enabled format={:?} output={} renderer_pci={} renderer_drm={:?} renderer_drm_matches={} drm_bus_id={:?} earlier_failures={earlier_failures} DRM direct scanout enabled",
                    targets.format(),
                    entry.id.0,
                    renderer.pci_bus_id,
                    renderer.described,
                    eligibility.renderer_drm_matches,
                    drm_bus_id.as_deref().unwrap_or("unavailable"),
                );
                tracing::info!(
                    output = entry.id.0,
                    renderer_pci = %renderer.pci_bus_id,
                    renderer_drm = %renderer.described,
                    renderer_drm_matches = eligibility.renderer_drm_matches,
                    drm_bus_id = drm_bus_id.as_deref().unwrap_or("unavailable"),
                    earlier_failures,
                    "DRM direct scanout enabled"
                );
                if want_hdr {
                    entry.hdr_direct_degradation.record_enabled(Instant::now());
                } else {
                    entry.direct_degradation.record_enabled(Instant::now());
                }
                entry.direct_targets = Some(targets);
            }
            Err(error) => demote_direct(entry, want_hdr, "create targets", &error),
        }
    }
    // No targets: the failure that took them is cooling down, and was logged when it
    // happened.
    let Some(targets) = entry.direct_targets.as_mut() else {
        return DirectOutcome::Readback;
    };
    // A clip-only transition frame carries no scene: only the readback can replay
    // retained workspace pixels, so a direct output keeps what it shows until the
    // next complete frame, whose damage is relative to the last one it rendered.
    if frame.scene.encoding().is_empty() {
        // Targets that have not shown a frame leave the dumb front on the CRTC, and
        // the readback is what can replay it.
        return if targets.presented() {
            DirectOutcome::Kept
        } else {
            DirectOutcome::Readback
        };
    }
    let own_pixels = sharing.owns_pixels(bridge, view);
    if keeps_shown_image(bridge, view, frame, targets.presented(), sharing) {
        return DirectOutcome::Kept;
    }
    // A queued imported-buffer flip retires before a new render target is selected. With
    // two slots, a pending flip owns both. Only the outputs pacing the loop are waited for
    // (`DrmPresenter::retire_direct_flips`), so a slower output arrives here until its flip
    // retires on its own vblank, and renders at most once per vblank.
    let Some((index, target)) = targets.render_slot() else {
        if own_pixels && let Some(damage) = frame.damage {
            bridge.defer_output(view, damage);
        }
        return DirectOutcome::Deferred;
    };
    let revision = frame.damage.map_or(1, |damage| damage.revision.max(1));
    // The output's own retained surface holds the previous revision whichever slot comes
    // up, and the slot receives a whole copy of it: scene damage stays sparse across the
    // swapchain.
    let direct_damage = frame.damage.unwrap_or(SceneDamage {
        revision,
        previous_revision: None,
        regions: None,
    });
    let render_started = Instant::now();
    let deadline = render_started
        .checked_add(DIRECT_FRAME_DEADLINE)
        .unwrap_or(render_started);
    let sdr_white_nits = entry.scanout.hdr_request().map_or(
        crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
        |request| request.sdr_white_nits,
    );
    let painted = bridge.paint_output_until(
        frame.scene,
        view,
        direct_damage,
        target,
        deadline,
        paint::OutputSignal {
            sdr_white_nits,
            hdr_layers: frame.hdr_layers,
        },
    );
    let stats = match painted {
        Ok(stats) => stats,
        Err(error) => {
            demote_direct(entry, want_hdr, "render", &error.to_string());
            return DirectOutcome::Readback;
        }
    };
    let render_us = render_started.elapsed().as_micros();
    match entry.scanout.submit_direct_target(targets, index) {
        submitted @ (scanout::DirectSubmit::Queued | scanout::DirectSubmit::Presented) => {
            if let Some(window) = phase_window.as_deref_mut()
                && let Some(timing) = bridge.last_phase_timing()
            {
                window.accumulate(timing);
            }
            let degradation = if want_hdr {
                &mut entry.hdr_direct_degradation
            } else {
                &mut entry.direct_degradation
            };
            degradation.record_presented(Instant::now());
            tracing::trace!(
                output = entry.id.0,
                revision = stats.revision,
                render_attempts = stats.render_attempts,
                render_us,
                "DRM direct frame submitted"
            );
            // The first HDR frame went up by modeset: no flip event to wait for.
            DirectOutcome::Submitted {
                queued: matches!(submitted, scanout::DirectSubmit::Queued),
                render_us,
                sparse: stats.sparse_attempts > 0 && stats.full_attempts == 0,
            }
        }
        scanout::DirectSubmit::Busy => {
            if let Some(window) = phase_window {
                window.record_direct_drop();
            }
            // The surface is ahead of the image KMS shows: the next frame must not
            // count this output as unchanged, a lone output's unchanged frame included.
            bridge.invalidate_output_view(view);
            DirectOutcome::Dropped
        }
        scanout::DirectSubmit::Failed(error) => {
            demote_direct(entry, want_hdr, "submit", &error);
            DirectOutcome::Readback
        }
        scanout::DirectSubmit::HdrFailed(reason, detail) => {
            // The CRTC is still (or again) on the dumb front; the targets can go.
            entry.direct_targets = None;
            entry.fail_hdr(reason, &detail);
            DirectOutcome::Readback
        }
    }
}

/// Longest wait for the previous page flip before a CPU frame is copied.
/// A flip retires on the next vblank; a much longer wait means the flip was
/// lost (for instance with the CRTC switched off) and the output is re-set.
const CPU_FLIP_TIMEOUT: Duration = Duration::from_millis(100);

/// Render one frame on the CPU. A single unrotated output at scale one gets
/// sparse copies into its dumb buffers; any other layout copies the whole
/// canvas through the established per-output readback.
fn paint_scanouts_cpu(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    renderer: &mut cpu_present::CpuRenderer,
    pointer: (f64, f64),
    frame: ScanoutFrame<'_>,
    target_size: (u32, u32),
) {
    let single = scanouts.len() == 1
        && outputs.len() == 1
        && outputs.first().is_some_and(|output| {
            output.position == (0, 0)
                && output.scale.to_bits() == 1.0_f32.to_bits()
                && output.size == target_size
                && output.transform == settings_store::hardware::OutputTransform::Normal
        })
        && scanouts.first().is_some_and(|entry| {
            entry.transform == settings_store::hardware::OutputTransform::Normal
                && entry.color_transform.is_none()
                && entry.scanout.size() == target_size
        });
    let damage = frame.damage.filter(|_| single);
    let cpu_frame = cpu_present::CpuFrame {
        scene: frame.scene,
        revision: damage.map_or(0, |damage| damage.revision),
        previous_revision: damage.and_then(|damage| damage.previous_revision),
        regions: damage.and_then(|damage| damage.regions),
    };
    renderer.update(cpu_frame, target_size);

    if single
        && let Some(entry) = scanouts.first_mut()
        && !entry.powered_off
        && !entry.sync_hdr_readback()
    {
        match entry.scanout.wait_cpu_flip(CPU_FLIP_TIMEOUT) {
            Ok((wait, latency)) => renderer.record_flip_wait(wait, latency),
            Err(wait) => {
                eprintln!(
                    "FRAME-LIFECYCLE event=drm-flip-lost renderer=cpu waited_us={}; setting the output again",
                    wait.as_micros()
                );
                renderer.record_flip_wait(wait, None);
                entry.scanout.restore();
                renderer.invalidate_screen();
            }
        }
        entry.scanout.present_cpu(renderer, Some(pointer));
        renderer.finish_frame();
        return;
    }

    drain_page_flip_completions(scanouts);
    for entry in scanouts.iter_mut().filter(|entry| !entry.powered_off) {
        entry.sync_hdr_readback();
    }
    let started = Instant::now();
    let rgba = renderer.screen_rgba();
    let size = (target_size.0 as usize, target_size.1 as usize);
    for entry in scanouts.iter_mut().filter(|entry| !entry.powered_off) {
        paint_one_scanout(entry, outputs, rgba, size, pointer, None, None);
    }
    let pixels = u64::from(target_size.0).saturating_mul(u64::from(target_size.1));
    renderer.record_present(started.elapsed(), pixels, scanouts.len());
    renderer.invalidate_screen();
    renderer.finish_frame();
}

/// Direct targets in the format the output needs now: the HDR formats in
/// order of preference while HDR is wanted, `XRGB8888` otherwise.
fn create_direct_targets(
    scanout: &scanout::DrmScanout,
    bridge: &paint::VelloDrmBridge,
    want_hdr: bool,
    output_size: (u32, u32),
) -> Result<scanout::DirectScanoutTargets, String> {
    if !want_hdr {
        return scanout::DirectScanoutTargets::new(
            scanout,
            bridge,
            scanout::ScanoutFormat::Xrgb8888,
            output_size,
        );
    }
    let mut errors = Vec::new();
    for format in scanout::ScanoutFormat::HDR_PREFERENCE {
        match scanout::DirectScanoutTargets::new(scanout, bridge, format, output_size) {
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
        backdrop: _,
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
    if hdr_layers.is_empty()
        && let Some(sample) = publish_already_encoded_clips(
            scanouts,
            outputs,
            paint,
            &EncodedClipPresent {
                scene,
                regions: damage.and_then(|damage| damage.regions).unwrap_or(&[]),
                pointer,
                workspace_id,
            },
        )
    {
        return sample;
    }
    let Some(bridge) = paint.as_mut() else {
        return PaintScanoutSample::default();
    };
    upload_combined_scene(
        scanouts,
        outputs,
        bridge,
        pointer,
        &CombinedUpload {
            scene,
            hdr_layers,
            workspace_id,
            note_workspace: overview.is_none(),
        },
    )
}

struct EncodedClipPresent<'a> {
    scene: &'a Scene,
    regions: &'a [vello::FineDamageRect],
    pointer: (f64, f64),
    workspace_id: Option<u64>,
}

fn presented_heads(scanouts: &[ScanoutEntry], outputs: &[Output]) -> Vec<PresentedHead> {
    scanouts
        .iter()
        .filter(|entry| entry.present_by_readback)
        .map(|entry| {
            let size = outputs
                .iter()
                .find(|output| output.id == entry.id)
                .map_or_else(|| entry.scanout.size(), |output| output.size);
            PresentedHead {
                pageflip: entry.scanout.event_pageflips_available(),
                x: entry.position.0,
                y: entry.position.1,
                width: size.0,
                height: size.1,
            }
        })
        .collect()
}

/// Publish clips from the desktop image this bridge already encoded.
///
/// A missing image, or a clip the GPU cannot refresh, falls through to the
/// combined readback. That path is also how a real connector page-flips.
fn publish_already_encoded_clips(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    paint: &mut Option<paint::VelloDrmBridge>,
    present: &EncodedClipPresent<'_>,
) -> Option<PaintScanoutSample> {
    let published = virtual_encoded_clips(&presented_heads(scanouts, outputs), present.regions)?;
    tracing::trace!(
        bytes = encoded_clip_bytes(&published),
        heads = published.len(),
        "virtual present publishes the encoded clip"
    );
    let bridge = paint.as_mut()?;
    if !bridge.has_complete_readback() {
        return None;
    }
    for clip in &published {
        let rect = vello::FineDamageRect {
            x: clip.scene_x,
            y: clip.scene_y,
            width: u32::from(clip.width),
            height: u32::from(clip.height),
        };
        if bridge.refresh_encoded_clip(present.scene, rect).is_err() {
            return None;
        }
    }
    let started = Instant::now();
    {
        let rgba = bridge.encoded_rgba()?;
        for clip in &published {
            let rect = vello::FineDamageRect {
                x: clip.scene_x,
                y: clip.scene_y,
                width: u32::from(clip.width),
                height: u32::from(clip.height),
            };
            present_composed_readback(scanouts, outputs, present.pointer, rgba, Some(rect));
        }
    }
    if let Some(id) = present.workspace_id {
        bridge.note_shown_workspace(id);
    }
    Some(PaintScanoutSample {
        legacy_attempted: true,
        scanout_us: started.elapsed().as_micros(),
        ..PaintScanoutSample::default()
    })
}

struct CombinedUpload<'a> {
    scene: &'a Scene,
    hdr_layers: &'a [HdrLayer],
    workspace_id: Option<u64>,
    note_workspace: bool,
}

/// Paint the combined scene and copy it onto every powered scanout.
fn upload_combined_scene(
    scanouts: &mut [ScanoutEntry],
    outputs: &[Output],
    bridge: &mut paint::VelloDrmBridge,
    pointer: (f64, f64),
    upload: &CombinedUpload<'_>,
) -> PaintScanoutSample {
    let scene = upload.scene;
    let hdr_layers = upload.hdr_layers;
    let workspace_id = upload.workspace_id;
    let note_workspace = upload.note_workspace;
    let bridge_started = Instant::now();
    let composite = !hdr_layers.is_empty()
        && scanouts
            .iter()
            .any(|entry| entry.present_by_readback && entry.scanout.format().is_hdr());
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
    for entry in scanouts
        .iter_mut()
        .filter(|entry| entry.present_by_readback)
    {
        paint_one_scanout(entry, outputs, rgba, (cw, ch), pointer, hdr, None);
    }
    if note_workspace && let Some(id) = workspace_id {
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
            let clip = virtual_encoded_clips(&presented_heads(scanouts, outputs), &[])
                .and_then(|clips| clips.first().copied())
                .map(|clip| vello::FineDamageRect {
                    x: clip.scene_x,
                    y: clip.scene_y,
                    width: u32::from(clip.width),
                    height: u32::from(clip.height),
                });
            Some(present_composed_readback(
                scanouts, outputs, pointer, &bytes, clip,
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
/// `None` means there is no bridge to composite with. A missing desktop
/// image skips the frame: seeding a combined readback here is the stall
/// that finishes the fade before a single present.
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
        Err(_) => Some(PaintScanoutSample::default()),
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
        .filter(|entry| entry.present_by_readback)
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
    for entry in scanouts
        .iter_mut()
        .filter(|entry| entry.present_by_readback)
    {
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
/// tested without opening DRM hardware. Returns how many completions arrived.
fn drain_page_flip_completions(scanouts: &mut [ScanoutEntry]) -> usize {
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
    route_flip_completions(scanouts, &completed);
    completed.len()
}

/// Hand each `(device, crtc)` completion to the scanout it belongs to: the
/// direct targets' ring when the output renders direct, the dumb pair otherwise.
fn route_flip_completions(
    scanouts: &mut [ScanoutEntry],
    completed: &[(u64, smithay::reexports::drm::control::crtc::Handle)],
) {
    let topology: Vec<_> = scanouts
        .iter()
        .map(|entry| (entry.device_id, entry.scanout.crtc()))
        .collect();
    for (index, crtc) in page_flip_completion_recipients(&topology, completed) {
        let Some(entry) = scanouts.get_mut(index) else {
            continue;
        };
        if let Some(targets) = entry.direct_targets.as_mut() {
            entry.scanout.mark_direct_flip_complete(targets, crtc);
        } else {
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

/// Software-cursor position on this output, in physical pixels.
fn output_local_cursor(
    origin: (i32, i32),
    logical_size: (u32, u32),
    pointer: (f64, f64),
    transform: settings_store::hardware::OutputTransform,
) -> Option<(f64, f64)> {
    let (px, py) = origin;
    let width_i32 = i32::try_from(logical_size.0).unwrap_or(i32::MAX);
    let height_i32 = i32::try_from(logical_size.1).unwrap_or(i32::MAX);
    let right = px.saturating_add(width_i32);
    let bottom = py.saturating_add(height_i32);
    if pointer.0 < f64::from(px)
        || pointer.0 >= f64::from(right)
        || pointer.1 < f64::from(py)
        || pointer.1 >= f64::from(bottom)
    {
        return None;
    }
    // Sub-pixel pointer position intentionally truncates to a pixel
    // coordinate; bounded to on-screen extents, well within `i32`.
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
        transform,
    );
    Some((f64::from(x), f64::from(y)))
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
    let local_cursor = output_local_cursor((px, py), logical_size, pointer, e.transform);
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
    let local = damage.map(|rect| output_local_clip(px, py, logical_size, rect));
    let pageflip = e.scanout.event_pageflips_available();
    if matches!(local, Some(None)) && !pageflip {
        return;
    }
    let clip = local.flatten();
    e.scanout.set_damage_rect(clip);
    let clip_write = clip.filter(|_| {
        !pageflip
            && encoder.is_none()
            && transform == settings_store::hardware::OutputTransform::Normal
            && composite.is_none()
    });
    e.scanout.present_no_drain(local_cursor, |dst, pitch| {
        if let Some(clip) = clip_write {
            write_encoded_clip_xrgb(subrect, clip, dst, pitch);
            return;
        }
        match &encoder {
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
        }
    });
}

impl Presenter for DrmPresenter {
    fn present(&mut self, scene: &Scene, _output: OutputId) {
        let _ = self.submit(ScanoutFrame::scene_only(scene));
    }

    fn frame_phase_timing(&self) -> Option<crate::shell::present::FramePhaseTiming> {
        self.frame_phases
    }

    /// No frame waits behind one presenting while every output flips straight
    /// from its own target: this presenter returns only once the flip has
    /// retired, so a queued frame would start rendering at that vblank and be
    /// shown one interval later than a frame built for it. Readback, CPU and
    /// a blanked or paused session keep one queued frame.
    fn queue_depth(&self) -> usize {
        if self.display_timing.vblank_paced {
            self.paced_queue_depth
        } else {
            crate::shell::present::QUEUE_DEPTH
        }
    }

    fn display_timing(&self) -> crate::shell::present::DisplayTiming {
        self.display_timing
    }

    /// Mesa's CPU Vulkan driver compiles each pipeline on its first dispatch, seconds of work
    /// the bridge's warm-up pays up front. Starting it here overlaps the compositor's startup.
    fn prepare(&mut self) {
        // Device selection must precede GPU warm-up as well as frame rendering.
        if cpu_present::cpu_render_reason().is_some()
            || self.scanouts.iter().all(|entry| entry.powered_off)
        {
            return;
        }
        let target_size = bridge_extent(&self.outputs, expects_readback(&self.scanouts));
        if target_size.0 != 0 && target_size.1 != 0 {
            ensure_bridge(&mut self.paint, target_size);
        }
    }

    fn present_frame(&mut self, frame: &FramePaint, _output: OutputId) {
        let _ = self.submit(frame.into());
    }

    fn present_frame_observed(
        &mut self,
        frame: &FramePaint,
        _output: OutputId,
    ) -> Option<crate::shell::present::PhysicalPresentation> {
        self.submit(frame.into())
    }

    fn frame_detail(&mut self) -> Option<String> {
        self.cpu
            .as_mut()
            .and_then(cpu_present::CpuRenderer::take_finished)
            .map(|stats| stats.log_fields())
    }

    fn set_dpms(&mut self, on: bool) {
        let present_gate = self.present_gate.clone();
        let _quiesced = quiesce_presentations(present_gate.as_ref());
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

    fn pixel_aligned_strip_pan(&self) -> bool {
        cpu_present::cpu_render_reason().is_some()
    }

    /// Inline presentation, reached only when the presenter hasn't been split off yet — kept so a caller that never splits still paints.
    fn submit_frame(&mut self, scene: &Scene, _output: OutputId) {
        self.submit_inline(ScanoutFrame::scene_only(scene));
    }

    fn supports_workspace_clip(&self) -> bool {
        // Only combined GPU readback implements workspace replay. Direct
        // outputs, the single-output XRGB path and the CPU renderer consume
        // complete scenes; a clip frame reaching a direct output keeps its image.
        self.outputs.len() > 1
            && cpu_present::cpu_render_reason().is_none()
            && self.readback_presenting.load(Ordering::Relaxed)
    }

    fn submit_frame_with_damage(&mut self, frame: &FramePaint, _output: OutputId) {
        self.submit_inline(frame.into());
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
            self.outputs = Self::refresh_outputs(&self.inner.dispatch);
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
        let shared_pointer = self.inner.dispatch.hw_cursor.shared_position();
        *shared_pointer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = self.inner.dispatch.pointer;
        // Any inline bridge in `self.paint` stays behind and is dropped with
        // the backend: `PresenterParts` has no field to carry it across.
        Some(Box::new(DrmPresenter::new(PresenterParts {
            scanouts,
            outputs: self.outputs.clone(),
            pointer: self.inner.dispatch.pointer,
            shared_pointer,
            cpu: self.cpu.take(),
            blanked: self.blanked,
            paused: self.paused,
            commands: self.commands.take(),
            present_gate: self.inner.dispatch.present_gate.clone(),
            profile_phases: std::env::var_os("TILO_DRM_PHASE_TRACE").is_some()
                || std::env::var_os("TILO_PROFILE_GPU_STAGES").is_some_and(|value| value == "1"),
            readback_presenting: Arc::clone(&self.readback_presenting),
            signal_reports: self.signal_reports.take(),
        })))
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

    fn reload_display_settings(&mut self) {
        self.inner.dispatch.reload_display_settings();
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
    fn two_head_virtual_present_publishes_the_encoded_clip_not_both_outputs() {
        let heads = [
            super::PresentedHead {
                pageflip: false,
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
            },
            super::PresentedHead {
                pageflip: false,
                x: 1920,
                y: 0,
                width: 1280,
                height: 800,
            },
        ];
        let clip = vello::FineDamageRect {
            x: 80,
            y: 40,
            width: 300,
            height: 48,
        };
        let published = super::virtual_encoded_clips(&heads, &[clip])
            .expect("a virtual pair publishes the encoded clip");
        assert_eq!(published.len(), 1, "the other scanout is not copied");
        assert_eq!(published[0].head, 0);
        let bytes = super::encoded_clip_bytes(&published);
        assert_eq!(bytes, 300 * 48 * 4);
        let both_outputs = (1920 * 1080 + 1280 * 800) * 4;
        assert!(
            bytes < both_outputs,
            "the clip publish is not a combined framebuffer copy"
        );
        let primary = super::virtual_encoded_clips(&heads, &[])
            .expect("without a smaller clip the first head is the publish");
        assert_eq!(primary.len(), 1, "the second scanout is not copied");
        assert_eq!(primary[0].head, 0);
        assert_eq!(super::encoded_clip_bytes(&primary), 1920 * 1080 * 4);
        assert!(
            super::virtual_encoded_clips(
                &[
                    super::PresentedHead {
                        pageflip: true,
                        ..heads[0]
                    },
                    heads[1],
                ],
                &[clip],
            )
            .is_none(),
            "a real connector stays on the combined present and page-flips"
        );
    }

    #[test]
    fn encoded_clip_write_leaves_the_other_output_untouched() {
        let width = 8_usize;
        let height = 4_usize;
        let mut rgba = vec![0_u8; width.saturating_mul(height).saturating_mul(4)];
        let source_at = 1_usize
            .saturating_mul(width)
            .saturating_add(1)
            .saturating_mul(4);
        rgba[source_at] = 255;
        let mut destination = vec![7_u8; rgba.len()];
        super::write_encoded_clip_xrgb(
            super::SubrectSource {
                src_rgba: &rgba,
                src_w: width,
                src_h: height,
                src_x: 0,
                src_y: 0,
            },
            (1, 1, 1, 1),
            &mut destination,
            width.saturating_mul(4),
        );
        let written = &destination[source_at..source_at.saturating_add(4)];
        assert_eq!(written, &[0, 0, 255, 255]);
        let clip_pixel = width.saturating_add(1);
        assert!(
            destination
                .chunks_exact(4)
                .enumerate()
                .all(|(pixel, bytes)| {
                    pixel == clip_pixel || bytes.iter().all(|byte| *byte == 7)
                }),
            "pixels outside the clip, including the other output, stay put"
        );
    }

    #[test]
    fn workspace_switch_replays_the_way_back_and_holds_both_directions() {
        use super::WorkspaceUpload::{Clip, Render, Replay, Skip};
        // A held switch stays on one display's clip, including the first
        // visit. Rendering here is the combined two-output readback.
        assert_eq!(super::workspace_frame_upload(true, false, true), Clip);
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
            // The presenter sizes its paint target from this table (`bridge_extent`) and renders each output's view from its position, so adopting it is what makes an unplugged monitor stop being painted for.
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
    fn the_backend_publishes_the_runtime_output_table() {
        // `DrmRuntime::new` needs a seat, so drive `refresh_outputs` on the dispatch state it reads:
        // the external monitor enumerated first, yet the built-in panel is published first, the
        // order everything downstream takes as primary.
        let dispatch = runtime::DrmRuntime::synthesize_for_tests(&[
            ("DP-2", (2560, 1440)),
            ("eDP-1", (1920, 1080)),
        ]);
        let outputs = DrmBackend::refresh_outputs(&dispatch);
        assert_eq!(
            outputs
                .iter()
                .map(|output| (output.id, output.size))
                .collect::<Vec<_>>(),
            [(OutputId(1), (1920, 1080)), (OutputId(0), (2560, 1440))],
        );
    }

    #[test]
    fn presenter_starts_without_a_bridge() {
        // `take_presenter` is the ownership boundary: a bridge built before it and moved with the
        // scanouts can wedge software Vulkan adapters after their first submissions. The render
        // thread must build its own on the first frame.
        let (_commands, receiver) = std::sync::mpsc::channel();
        let presenter = DrmPresenter::new(PresenterParts {
            scanouts: Vec::new(),
            outputs: vec![Output::new(OutputId(0), (1920, 1080), 1.0)],
            pointer: (0.0, 0.0),
            shared_pointer: Arc::new(Mutex::new((0.0, 0.0))),
            cpu: None,
            blanked: false,
            paused: false,
            commands: Some(receiver),
            present_gate: Some(Arc::new(Mutex::new(()))),
            profile_phases: false,
            readback_presenting: Arc::new(AtomicBool::new(true)),
            signal_reports: None,
        });
        assert!(
            presenter.paint.is_none(),
            "the render thread must construct its own wgpu bridge"
        );
        assert!(
            presenter.commands.is_some(),
            "the scanouts' command channel travels with them"
        );
    }

    #[test]
    fn a_presenter_without_a_direct_output_keeps_a_queued_frame_and_no_vblank() {
        // Only a presenter that waits for every output's flip may refuse a
        // frame behind the one presenting. With nothing powered (or blanked,
        // paused, on readback or the CPU path) the frame retires without a
        // vblank, and the loop must keep its retirement-paced grid.
        for (blanked, paused) in [(false, false), (true, false), (false, true)] {
            let (_commands, receiver) = std::sync::mpsc::channel();
            let mut presenter = DrmPresenter::new(PresenterParts {
                scanouts: Vec::new(),
                outputs: vec![Output::new(OutputId(0), (1920, 1080), 1.0)],
                pointer: (0.0, 0.0),
                shared_pointer: Arc::new(Mutex::new((0.0, 0.0))),
                cpu: None,
                blanked,
                paused,
                commands: Some(receiver),
                present_gate: Some(Arc::new(Mutex::new(()))),
                profile_phases: false,
                readback_presenting: Arc::new(AtomicBool::new(false)),
                signal_reports: None,
            });
            assert!(!presenter.vblank_paced());
            let proof =
                presenter.present_frame_observed(&FramePaint::full(Scene::new()), OutputId(0));
            assert_eq!(proof, None);
            assert_eq!(
                presenter.display_timing(),
                crate::shell::present::DisplayTiming::default()
            );
            assert_eq!(presenter.queue_depth(), crate::shell::present::QUEUE_DEPTH);
        }
    }

    #[test]
    fn every_present_entry_point_waits_for_a_master_drop_in_progress() {
        // A VT switch queues `Pause` to the presenter, then drops DRM master while holding the
        // present gate (`DrmDispatchState::perform`). A frame the render thread starts in that
        // window must neither run nor queue a page flip until master is gone, and must then see
        // the `Pause` — so it never flips into CRTCs this session no longer owns.
        type Entry = fn(&mut DrmPresenter);
        let entries: [(&str, Entry); 3] = [
            ("present", |presenter| {
                presenter.present(&Scene::new(), OutputId(0));
            }),
            ("present_frame", |presenter| {
                presenter.present_frame(&FramePaint::full(Scene::new()), OutputId(0));
            }),
            ("present_frame_observed", |presenter| {
                let _ =
                    presenter.present_frame_observed(&FramePaint::full(Scene::new()), OutputId(0));
            }),
        ];
        for (name, entry) in entries {
            let gate = Arc::new(Mutex::new(()));
            let (commands, receiver) = std::sync::mpsc::channel();
            let mut presenter = DrmPresenter::new(PresenterParts {
                scanouts: Vec::new(),
                outputs: vec![Output::new(OutputId(0), (1920, 1080), 1.0)],
                pointer: (0.0, 0.0),
                shared_pointer: Arc::new(Mutex::new((0.0, 0.0))),
                cpu: None,
                blanked: false,
                paused: false,
                commands: Some(receiver),
                present_gate: Some(Arc::clone(&gate)),
                profile_phases: false,
                readback_presenting: Arc::new(AtomicBool::new(false)),
                signal_reports: None,
            });
            commands
                .send(ScanoutCommand::Pause)
                .expect("the presenter holds the command receiver");
            let dropping_master = quiesce_presentations(Some(&gate));
            let (finished_tx, finished) = std::sync::mpsc::channel();
            let render_thread = std::thread::spawn(move || {
                entry(&mut presenter);
                let _ = finished_tx.send(());
                presenter
            });
            assert_eq!(
                finished.recv_timeout(Duration::from_millis(100)),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout),
                "{name} presented while DRM master was being dropped"
            );
            drop(dropping_master);
            tilo_test_env::recv(
                format!("{name} to present once the master drop finished"),
                &finished,
            );
            let presenter = render_thread.join().expect("the render thread");
            assert!(
                presenter.paused,
                "{name} drained the Pause queued before the master drop under the same gate"
            );
        }
    }
}

#[cfg(test)]
mod presentation_proof_tests {
    use super::*;
    #[test]
    fn physical_flip_proof_preserves_the_kernel_fields_and_rejects_uncertain_completion() {
        let own: smithay::reexports::drm::control::crtc::Handle = std::num::NonZeroU32::new(11)
            .expect("nonzero fixture CRTC")
            .into();
        let sibling: smithay::reexports::drm::control::crtc::Handle = std::num::NonZeroU32::new(12)
            .expect("nonzero fixture CRTC")
            .into();
        let event = smithay::reexports::drm::control::PageFlipEvent {
            crtc: own,
            frame: 76543,
            duration: Duration::new(1_234_567, 987_654_000),
        };
        let events = [event];
        let proof =
            physical_flip_proof(scanout::DirectRetirement::Complete, &events, own, true).unwrap();
        assert_eq!(proof.timestamp, events[0].duration);
        assert_eq!(proof.sequence, u64::from(events[0].frame));
        assert!(
            physical_flip_proof(scanout::DirectRetirement::TimedOut, &events, own, true).is_none()
        );
        assert!(
            physical_flip_proof(scanout::DirectRetirement::Complete, &events, own, false).is_none()
        );
        assert!(
            physical_flip_proof(scanout::DirectRetirement::Complete, &events, sibling, true)
                .is_none()
        );
    }
}

#[cfg(test)]
mod per_output_presentation_tests {
    use super::*;

    /// An output at `position` (scene and logical alike) scanning out at `refresh_mhz`.
    fn head(id: u32, position: (i32, i32), size: (u32, u32), refresh_mhz: u32) -> Output {
        Output {
            position,
            logical_position: position,
            refresh_mhz: Some(refresh_mhz),
            ..Output::new(OutputId(id), size, 1.0)
        }
    }

    fn area((width, height): (u32, u32)) -> u64 {
        u64::from(width).saturating_mul(u64::from(height))
    }

    #[test]
    fn no_bridge_target_spans_the_desktop_while_every_output_is_direct() {
        // Two 4K heads side by side: 7680x2160 rasterised the whole desktop in two bands.
        let twin = [
            head(0, (0, 0), (3840, 2160), 60_000),
            head(1, (3840, 0), (3840, 2160), 60_000),
        ];
        assert_eq!(bridge_extent(&twin, false), (3840, 2160));
        assert_eq!(
            bridge_extent(&twin, true),
            (7680, 2160),
            "the readback fallback renders the bounding box"
        );
        // A landscape monitor beside a portrait one: 4000x2560 combined, 28 % of it
        // between the outputs.
        let mixed = [
            head(0, (0, 0), (2560, 1440), 60_000),
            head(1, (2560, 0), (1440, 2560), 60_000),
        ];
        let extent = bridge_extent(&mixed, false);
        assert_eq!(
            extent,
            (2560, 1440),
            "the target is the output at the origin"
        );
        assert!(
            mixed.iter().any(|output| area(output.size) >= area(extent)),
            "no target is larger than the larger output"
        );
        // One output is the whole scene, as before.
        let single = [head(0, (0, 0), (1920, 1080), 60_000)];
        assert_eq!(bridge_extent(&single, false), (1920, 1080));
        assert_eq!(bridge_extent(&single, true), (1920, 1080));
    }

    #[test]
    fn damage_is_routed_only_to_the_output_it_touches() {
        let outputs = [
            head(0, (0, 0), (2560, 1440), 60_000),
            head(1, (2560, 0), (1440, 2560), 60_000),
        ];
        let [first, second] = [OutputId(0), OutputId(1)].map(|id| {
            let output = outputs
                .iter()
                .find(|output| output.id == id)
                .expect("the head is listed");
            view_of(id, output.position, (1, 1), &outputs)
        });
        // A caret blinking on head 2.
        let caret = [vello::FineDamageRect {
            x: 2600,
            y: 2000,
            width: 8,
            height: 20,
        }];
        assert_eq!(
            first.damage(&caret),
            Some(Vec::new()),
            "head 1 receives no damage region, so it is neither rendered nor flipped"
        );
        assert_eq!(
            second.damage(&caret),
            Some(vec![vello::FineDamageRect {
                x: 40,
                y: 2000,
                width: 8,
                height: 20,
            }]),
            "head 2 receives the damage in its own pixels"
        );
        // Damage straddling the seam splits exactly at it: a window across both heads
        // repairs both halves and nothing else.
        let seam = [vello::FineDamageRect {
            x: 2500,
            y: 100,
            width: 100,
            height: 50,
        }];
        assert_eq!(
            first.damage(&seam),
            Some(vec![vello::FineDamageRect {
                x: 2500,
                y: 100,
                width: 60,
                height: 50,
            }])
        );
        assert_eq!(
            second.damage(&seam),
            Some(vec![vello::FineDamageRect {
                x: 0,
                y: 100,
                width: 40,
                height: 50,
            }])
        );
    }

    #[test]
    fn slow_output_skips_frames_while_its_flip_is_pending() {
        // A 144 Hz monitor beside a 60 Hz panel, both rendering direct and damaged every
        // frame. The loop ticks at 144 Hz and waits only for the fast output's flip; the
        // panel renders only once its previous flip has retired on its own vblank.
        let outputs = [
            head(0, (0, 0), (2560, 1440), 144_000),
            head(1, (2560, 0), (1920, 1080), 60_000),
        ];
        let fastest = outputs.iter().filter_map(|output| output.refresh_mhz).max();
        // Refresh periods in nanoseconds.
        let fast_period: u64 = 6_944_444;
        let slow_period: u64 = 16_666_667;
        let mut monitor = scanout::DirectScanoutState::new(0);
        let mut panel = scanout::DirectScanoutState::new(0);
        // The vblank (in nanoseconds) the panel's pending flip retires on.
        let mut slow_retires_at: Option<u64> = None;
        let (mut fast_renders, mut slow_renders, mut slow_skips) = (0_u32, 0_u32, 0_u32);
        // One second of 144 Hz ticks.
        for tick in 0..144_u64 {
            let now = tick.saturating_mul(fast_period);
            // The frame's non-blocking drain: the panel's flip retired at its vblank.
            if slow_retires_at.is_some_and(|at| now >= at) {
                assert!(panel.complete());
                slow_retires_at = None;
            }
            let monitor_slot = monitor
                .render_slot()
                .expect("the fast output's flip retired before the next frame");
            assert!(monitor.submit(monitor_slot));
            fast_renders = fast_renders.saturating_add(1);
            if let Some(panel_slot) = panel.render_slot() {
                assert!(panel.submit(panel_slot));
                slow_renders = slow_renders.saturating_add(1);
                let next_vblank = now
                    .checked_div(slow_period)
                    .expect("the period is not zero")
                    .saturating_add(1);
                slow_retires_at = Some(next_vblank.saturating_mul(slow_period));
            } else {
                slow_skips = slow_skips.saturating_add(1);
            }
            // The frame retires once the outputs pacing the loop have flipped.
            assert_eq!(
                flip_wait(refresh_of(&outputs, OutputId(0)), fastest, Duration::ZERO),
                FlipWait::Await,
                "the frame waits for the fast output"
            );
            assert!(monitor.complete());
            assert_eq!(
                flip_wait(refresh_of(&outputs, OutputId(1)), fastest, Duration::ZERO),
                FlipWait::Leave,
                "the frame does not wait for the slower output"
            );
        }
        assert_eq!(fast_renders, 144, "the fast output renders every frame");
        assert!(
            slow_renders <= 60,
            "the 60 Hz output renders at most once per its own vblank: {slow_renders}"
        );
        assert!(
            slow_renders >= 59,
            "and once on every vblank it can: {slow_renders}"
        );
        assert_eq!(
            slow_skips,
            144_u32.saturating_sub(slow_renders),
            "it skips the frames it cannot show"
        );
    }

    #[test]
    fn only_outputs_at_the_fastest_rate_pace_the_loop() {
        let fastest = Some(144_000);
        let quick = Duration::from_millis(1);
        assert_eq!(flip_wait(Some(144_000), fastest, quick), FlipWait::Await);
        assert_eq!(flip_wait(Some(60_000), fastest, quick), FlipWait::Leave);
        assert_eq!(
            flip_wait(Some(60_000), fastest, DIRECT_RETIREMENT_TIMEOUT),
            FlipWait::Overdue,
            "a slow output whose flip never retires is demoted, not waited for"
        );
        // A 59.94 Hz panel beside a 60 Hz one shares its cadence.
        assert_eq!(
            flip_wait(Some(59_940), Some(60_000), quick),
            FlipWait::Await
        );
        // Without known rates every output paces the loop, as all did before.
        assert_eq!(flip_wait(None, fastest, quick), FlipWait::Await);
        assert_eq!(flip_wait(Some(60_000), None, quick), FlipWait::Await);
    }
}

#[cfg(test)]
mod lone_output_tests {
    use super::*;
    use vello::FineDamageRect;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    const SIZE: (u32, u32) = (64, 48);

    /// One display at the origin: its view is the whole scene, so it renders straight into the
    /// bridge's own target and shares it with nothing else (`TargetSharing::Lone`).
    fn lone_output() -> (paint::VelloDrmBridge, paint::OutputView) {
        let mut bridge = paint::VelloDrmBridge::new(SIZE.0, SIZE.1)
            .expect("renderer gate requires a real wgpu adapter");
        bridge.warm_renderer_for_test();
        let view = view_of(OutputId(0), (0, 0), SIZE, &[]);
        assert!(
            bridge.view_is_whole_scene(&view),
            "a lone output renders into the bridge's target"
        );
        (bridge, view)
    }

    fn grey_scene() -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0x40, 0x40, 0x40),
            None,
            &Rect::new(0.0, 0.0, f64::from(SIZE.0), f64::from(SIZE.1)),
        );
        scene
    }

    fn frame<'a>(scene: &'a Scene, damage: SceneDamage<'a>) -> ScanoutFrame<'a> {
        ScanoutFrame {
            damage: Some(damage),
            ..ScanoutFrame::scene_only(scene)
        }
    }

    /// Revision `revision`, relative to the one before it.
    fn damage(revision: u64, regions: Option<&[FineDamageRect]>) -> SceneDamage<'_> {
        SceneDamage {
            revision,
            previous_revision: revision.checked_sub(1).filter(|previous| *previous > 0),
            regions,
        }
    }

    /// Whether the lone output keeps what it shows for `damage`, its targets shown.
    fn kept(
        bridge: &mut paint::VelloDrmBridge,
        view: &paint::OutputView,
        scene: &Scene,
        damage: SceneDamage<'_>,
    ) -> bool {
        keeps_shown_image(
            bridge,
            view,
            &frame(scene, damage),
            true,
            TargetSharing::Lone,
        )
    }

    /// The production final pass, into an offscreen stand-in for the output's scanout slot.
    fn render(
        bridge: &mut paint::VelloDrmBridge,
        view: &paint::OutputView,
        scene: &Scene,
        damage: SceneDamage<'_>,
    ) -> paint::GpuFrameStats {
        let (device, _) = bridge.gpu();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lone-output-test-scanout"),
            size: wgpu::Extent3d {
                width: SIZE.0,
                height: SIZE.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target = texture.create_view(&wgpu::TextureViewDescriptor::default());
        bridge
            .paint_output_into(
                scene,
                view,
                damage,
                paint::OutputTarget {
                    view: &target,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    size: SIZE,
                },
                Instant::now()
                    .checked_add(tilo_test_env::hang_bound())
                    .expect("the hang bound fits in an Instant"),
                paint::OutputSignal {
                    sdr_white_nits: crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                    hdr_layers: &[],
                },
            )
            .expect("the output's frame renders")
    }

    #[test]
    fn lone_output_keeps_an_unchanged_frame_without_render_or_flip() {
        let (mut bridge, view) = lone_output();
        let grey = grey_scene();
        let first = render(&mut bridge, &view, &grey, damage(1, None));
        assert_eq!(first.full_attempts, 1, "the first frame renders in full");

        // Pointer motion over a static desktop: the producer proves the frame identical.
        assert!(
            kept(&mut bridge, &view, &grey, damage(2, Some(&[]))),
            "a lone output keeps the image it shows for a frame proven unchanged"
        );
        assert!(
            kept(&mut bridge, &view, &grey, damage(3, Some(&[]))),
            "and keeps it for every further unchanged frame"
        );

        // The kept frames advanced the retained revision, so the next change still repairs
        // only its damage rather than rendering the output in full.
        let rect = [FineDamageRect {
            x: 8,
            y: 8,
            width: 16,
            height: 16,
        }];
        assert!(
            !kept(&mut bridge, &view, &grey, damage(4, Some(&rect))),
            "a lone output renders every frame whose content changed"
        );
        let mut red = Scene::new();
        red.append(&grey, None);
        red.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0xff, 0, 0),
            None,
            &Rect::new(8.0, 8.0, 24.0, 24.0),
        );
        let next = render(&mut bridge, &view, &red, damage(4, Some(&rect)));
        assert_eq!(
            (next.sparse_attempts, next.full_attempts),
            (1, 0),
            "the change after kept frames is repaired sparsely"
        );
    }

    #[test]
    fn lone_output_renders_unless_its_shown_pixels_are_proven_current() {
        let (mut bridge, view) = lone_output();
        let grey = grey_scene();
        render(&mut bridge, &view, &grey, damage(1, None));

        assert!(
            !keeps_shown_image(
                &mut bridge,
                &view,
                &frame(&grey, damage(2, Some(&[]))),
                false,
                TargetSharing::Lone
            ),
            "targets KMS has never shown leave the dumb front up: the frame renders"
        );
        assert!(
            !kept(&mut bridge, &view, &grey, damage(2, None)),
            "a frame without damage authority renders"
        );
        let gap = SceneDamage {
            revision: 3,
            previous_revision: Some(2),
            regions: Some(&[]),
        };
        assert!(
            !kept(&mut bridge, &view, &grey, gap),
            "an unchanged frame relative to a revision the output never held renders"
        );
    }

    #[test]
    fn busy_lone_output_is_not_kept_on_the_next_unchanged_frame() {
        let (mut bridge, view) = lone_output();
        let grey = grey_scene();
        render(&mut bridge, &view, &grey, damage(1, None));
        let rect = [FineDamageRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        }];
        // Revision 2 renders, but KMS refuses its flip as busy (`DirectSubmit::Busy`): the
        // retained target is ahead of the image KMS shows, and `present_direct` invalidates
        // it, for a lone output as for any other.
        render(&mut bridge, &view, &grey, damage(2, Some(&rect)));
        bridge.invalidate_output_view(&view);
        assert!(
            !kept(&mut bridge, &view, &grey, damage(3, Some(&[]))),
            "after a dropped flip the unchanged frame must render, or KMS would keep revision 1"
        );
        let repaired = render(&mut bridge, &view, &grey, damage(3, Some(&[])));
        assert_eq!(repaired.full_attempts, 1, "the repair renders in full");
        assert!(
            kept(&mut bridge, &view, &grey, damage(4, Some(&[]))),
            "once repaired, unchanged frames are kept again"
        );
    }

    #[test]
    fn only_one_powered_output_without_readback_is_lone() {
        assert_eq!(TargetSharing::of(false, 1), TargetSharing::Lone);
        assert_eq!(TargetSharing::of(false, 2), TargetSharing::PerOutput);
        assert_eq!(
            TargetSharing::of(true, 1),
            TargetSharing::Readback,
            "an output returning from the readback shares the target this frame"
        );
        assert_eq!(TargetSharing::of(true, 2), TargetSharing::Readback);
    }

    #[test]
    fn a_target_shared_with_a_readback_never_keeps_an_unchanged_frame() {
        // A direct output whose view is the whole scene, beside an output presenting by
        // readback from the same target (a mirrored or contained display).
        let (mut bridge, view) = lone_output();
        let grey = grey_scene();
        render(&mut bridge, &view, &grey, damage(1, None));
        let rect = [FineDamageRect {
            x: 0,
            y: 0,
            width: 8,
            height: 8,
        }];
        // Revision 2: the direct output renders, KMS refuses its flip as busy and
        // `present_direct` invalidates the target; then the readback renders that same
        // target to revision 2 for its own output. KMS still shows revision 1 on the direct
        // output, yet the target proves revision 2.
        render(&mut bridge, &view, &grey, damage(2, Some(&rect)));
        bridge.invalidate_output_view(&view);
        render(&mut bridge, &view, &grey, damage(2, Some(&rect)));
        assert!(
            !keeps_shown_image(
                &mut bridge,
                &view,
                &frame(&grey, damage(3, Some(&[]))),
                true,
                TargetSharing::Readback
            ),
            "the direct output must render revision 3, or KMS would keep revision 1"
        );
    }
}
