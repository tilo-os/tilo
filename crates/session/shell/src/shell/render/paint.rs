//! Shared retained Vello GPU target and CPU-fallback XRGB bridge for DRM.
//!
//! Vello renders into a wgpu texture, then this bridge reads it back and swizzles directly into
//! the scanout's mapped XRGB dumb buffer. The DRM path does not use GLES; `Scanout::present_into`
//! owns the dumb buffer and page flip. Zero-copy would require exporting the wgpu texture as a
//! DMA-BUF that KMS can scan out, which remains a backend-specific follow-up. Until then this
//! bridge keeps the production render → readback → XRGB path explicit.
//!
//! `wgpu::copy_texture_to_buffer` requires `bytes_per_row` to be a multiple of
//! `COPY_BYTES_PER_ROW_ALIGNMENT` (256); for unaligned tight strides we allocate a padded
//! readback buffer and strip the padding on the way into the public `readback`.
//!
//! Tests need a wgpu device (`llvmpipe`/`vulkan_lvp` inside the distrobox); no real DRM device is touched.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64},
    },
    time::{Duration, Instant},
};

use vello::peniko::Color;
// `ui::gpu`'s context, not `vello::util`'s: this device is shared with every other client on the machine, so it needs the same memory hint — see the header of `crates/sdk/ui-core/src/gpu.rs`.
use ui::gpu::RenderContext;
use vello::{Renderer, wgpu};
use wgpu::util::TextureBlitter;

use super::hdr_composite::{HdrCompositor, HdrPass};
use super::hdr_encode::HdrEncodePipeline;
use super::orientation::OrientedCopy;
pub use super::xrgb_history::{
    XrgbHistoryCommit, XrgbHistoryIdentity, XrgbHistoryTransaction, XrgbTargetHistory,
};

mod bridge;
mod damage;
mod diagnostics;
mod output_view;
mod retained_target;
mod subrect;
#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod window_cycle_memory_tests;
mod xrgb;
pub(crate) use damage::{coalesce_damage, damage_area};
use diagnostics::DispatchStageObserver;
use output_view::OutputSurface;
pub use output_view::{OutputSignal, OutputTarget, OutputView};
pub use subrect::{
    SubrectComposite, SubrectSource, copy_rgba_subrect,
    encode_subrect_rgba_to_xbgr2101010_transformed, swizzle_subrect_rgba_to_xrgb8888,
    swizzle_subrect_rgba_to_xrgb8888_composited, swizzle_subrect_rgba_to_xrgb8888_transformed,
};
use tilo_lock::MutexRecover as _;

/// Errors the bridge can return; each variant maps to an operator-visible message and the DRM backend logs+skips the frame.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error(
        "failed to acquire a wgpu adapter for DRM CPU-fallback paint (is llvmpipe installed?): {0}"
    )]
    NoAdapter(String),

    #[error("failed to construct vello renderer: {0}")]
    Renderer(String),

    #[error("vello render_to_texture failed: {0}")]
    Render(String),

    #[error("vello still overflowed after {attempts} full render attempts")]
    RecoveryExhausted { attempts: usize },

    #[error("wgpu buffer mapping failed: {0}")]
    Map(String),

    #[error("invalid target size: {0}")]
    InvalidSize(String),

    #[error("invalid internal device handle: {0}")]
    InvalidDevice(String),

    #[error("failed to import direct scanout render target: {0}")]
    Import(String),

    #[error("GPU-only absolute deadline exceeded")]
    DeadlineExceeded,
    #[error("bump counter readback: {0}")]
    BumpReadback(String),
    #[error("device poll: {0}")]
    DevicePoll(String),
    #[error("device lost: {0}")]
    DeviceLost(String),
    #[error("retained GPU target must be reconstructed")]
    RecreateRequired,

    /// The previous software-rendered submission timed out.  Keep the last scanout intact and
    /// avoid submitting another frame until the old work has had a chance to drain.
    #[error("DRM paint recovery backoff active; retry in {retry_in_ms}ms")]
    RecoveryBackoff { retry_in_ms: u64 },

    #[error("XRGB destination-history transaction does not match this scene damage")]
    XrgbHistoryMismatch,

    /// Submitted GPU work did not complete within its Linux allowance. This, and only this,
    /// starts the recovery back-off: the work is still queued on the device, so another frame
    /// would only queue behind it.
    #[error("GPU completion timed out during {phase:?}")]
    CompletionTimeout { phase: CompletionPhase },
}

/// The completion wait a [`BridgeError::CompletionTimeout`] ran out in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionPhase {
    /// The device fence for the submitted frame.
    Fence,
    /// The framebuffer `map_async` callback after the fence signalled.
    MapCallback,
    /// The Vello bump-counter readback that decides whether the frame overflowed.
    CounterSettlement,
    /// The frame's one budget across every attempt and band ran out between its waits.
    FrameBudget,
}

/// One-shot latch so the "CPU fallback active" warning fires once per process; shared across all bridges since the message is process-global.
static WARNED_CPU_FALLBACK: AtomicBool = AtomicBool::new(false);

/// wgpu errors no call site captured, held for the bridge to act on.
///
/// wgpu's default handler for them panics, and the compositor is built with `panic = "abort"`:
/// one oversized binding on the present thread took the whole session down, every client
/// with it. A validation or out-of-memory error leaves the device usable; what it invalidates
/// is the objects of the frame that raised it. So the bridge takes the error at its next
/// check, reports the frame as failed, and rebuilds the renderer on the same device.
#[derive(Clone, Default)]
struct DeviceErrorLatch(std::sync::Arc<std::sync::Mutex<Option<String>>>);

impl DeviceErrorLatch {
    /// Route `device`'s uncaptured errors into a new latch.
    fn install(device: &wgpu::Device) -> Self {
        let latch = Self::default();
        let sink = latch.clone();
        device.on_uncaptured_error(std::sync::Arc::new(move |error: wgpu::Error| {
            let mut slot = sink.0.lock_recover();
            if slot.is_none() {
                *slot = Some(error.to_string());
            }
        }));
        latch
    }

    fn take(&self) -> Option<String> {
        self.0.lock_recover().take()
    }
}

/// Process-unique bridge identities prevent a replacement renderer from inheriting a stale
/// caller destination claim. Exhaustion makes destination tracking unavailable rather than
/// wrapping; ordinary bridge construction and full rendering remain available.
static NEXT_XRGB_HISTORY_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A complex first frame can require Vello to grow its bump allocation before it produces every
/// pixel. Retrying is deliberately bounded: an exhausted recovery reports an error rather than
/// presenting incomplete geometry as a successful frame.
const RESILIENT_RENDER_ATTEMPTS: usize = 4;
/// Pixels along one side of a Vello bin (16 tiles of 16 pixels).
const VELLO_BIN_PIXELS: u32 = 256;
/// Bins one Vello render can address: `N_TILE` in the shaders' `config.wgsl`.
const MAX_VELLO_BINS: u32 = 256;
/// A software Vulkan frame can legitimately exceed the normal one-second phase budget.  Once it
/// does, repeatedly allocating a replacement target every compositor tick only queues more
/// llvmpipe work and makes the guest look frozen.  Give the timed-out submission one more full
/// fence interval to drain before retrying, then back off exponentially while preserving the
/// already displayed scanout.
const RECOVERY_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const RECOVERY_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// Identity of the adapter actually opened by this bridge. This comes from the wgpu adapter
/// that owns the renderer, rather than from a separate GL probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeAdapterInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
    pub vendor: u32,
    pub device: u32,
    pub pci_bus_id: String,
    #[cfg(target_os = "linux")]
    pub drm_device: Option<ui::dmabuf_import::VulkanDrmDeviceIdentity>,
}

/// Per-paint wall-clock breakdown, available only when explicit profiling is enabled.
/// Values are accumulated across bounded Vello recovery submissions.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgePhaseTiming {
    /// CPU time used to prepare and submit Vello plus the appended texture copy.
    pub submit_us: u128,
    /// Blocking completion time for GPU rendering plus the texture copy, followed by map setup;
    /// this is intentionally not a copy-only duration.
    pub completed_readback_map_us: u128,
    /// CPU time to inspect and, if necessary, grow Vello's bump budget after completion.
    pub settle_us: u128,
    /// CPU time to strip padding and copy/swizzle mapped pixels into the selected output.
    pub cpu_copy_swizzle_us: u128,
    /// Pixel payload bytes copied from the texture, once for every submitted render attempt.
    /// Row-alignment holes in the mapped staging buffer are deliberately excluded.
    pub readback_bytes: u64,
    /// Bytes actually swizzled into the caller's output, after destination truncation.
    pub cpu_output_bytes: u64,
    /// CPU time to render, blur and substitute the backdrops of the frame's floating
    /// surfaces, before the frame's own submission. Their GPU work completes inside
    /// `completed_readback_map_us`.
    pub backdrop_us: u128,
}

impl BridgePhaseTiming {
    /// Complete measured bridge wall time. The phases are sequential and
    /// bounded by one bridge call, so their saturating sum is suitable for
    /// separating renderer/readback work from the following KMS submission.
    #[must_use]
    pub fn total_us(self) -> u128 {
        self.submit_us
            .saturating_add(self.backdrop_us)
            .saturating_add(self.completed_readback_map_us)
            .saturating_add(self.settle_us)
            .saturating_add(self.cpu_copy_swizzle_us)
    }
}

/// Versioned native-pixel damage for [`VelloDrmBridge::paint_damage_into_xrgb8888`].
///
/// `regions: None` deliberately requests a conservative full render. Sparse rendering is only
/// eligible when `previous_revision` names the complete frame retained by this bridge. This
/// makes a skipped KMS presentation or a compositor/model snapshot gap a full-frame boundary
/// instead of allowing stale pixels outside the supplied rectangles to leak through.
#[derive(Clone, Copy, Debug)]
pub struct SceneDamage<'a> {
    pub revision: u64,
    pub previous_revision: Option<u64>,
    pub regions: Option<&'a [vello::FineDamageRect]>,
}

impl<'a> SceneDamage<'a> {
    /// The same frame with `regions` in place of its own, when there are any.
    fn with_regions<'b>(self, regions: Option<&'b [vello::FineDamageRect]>) -> SceneDamage<'b>
    where
        'a: 'b,
    {
        SceneDamage {
            revision: self.revision,
            previous_revision: self.previous_revision,
            regions: regions.or(self.regions),
        }
    }
}

/// The most regions one sparse dispatch takes.
pub(crate) const MAX_DAMAGE_RECTS: usize = 64;

/// Whether repainting `dirty_pixels` of `full_pixels` through the sparse path beats a dense
/// frame. A sparse frame costs a fixed part (flatten, bin, tile) plus its dirty share of the
/// fine raster; a dense one costs that fixed part plus the whole raster. On the Adreno X1-85 at
/// 2944x1840 a dense frame is 33 ms and a small sparse one 9.5 ms, so the fixed part is at most
/// 9.5 ms and the raster at least 23.5 ms: the sparse path still saves about 6 ms with three
/// quarters of the output dirty. A window-sized update (typing in a half-screen window, which
/// repaints its tile) is therefore sparse; only a near-whole-screen change renders densely.
pub(crate) fn damage_is_profitable(dirty_pixels: u64, full_pixels: u64) -> bool {
    full_pixels != 0 && dirty_pixels.saturating_mul(4) < full_pixels.saturating_mul(3)
}

/// Mapped readback layout needed by the direct XRGB swizzle.
#[derive(Clone, Copy)]
struct MappedXrgbSource<'a> {
    bytes: &'a [u8],
    padded_bytes_per_row: usize,
    width: usize,
    height: usize,
}

/// Actual CPU destination write selected by the opt-in buffer-age bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum XrgbCpuWriteMode {
    Full,
    Partial,
}

/// Result of a completed opt-in history-aware XRGB write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct XrgbWriteResult {
    pub mode: XrgbCpuWriteMode,
    pub cpu_output_bytes: u64,
    pub damage_admitted: bool,
}

/// Caller-owned destinations and source layout for one two-output replay readback.
struct ReplayOutputDestinations<'a> {
    left: &'a mut [u8],
    right: &'a mut [u8],
    dst_pitch: usize,
    width: usize,
    height: usize,
    left_padded_bytes_per_row: usize,
    right_padded_bytes_per_row: usize,
}

/// Secondary output used only by the explicit shared-coarse diagnostic.
#[derive(Clone, Copy)]
struct SharedCoarseDiagnosticTarget<'a> {
    texture: &'a wgpu::Texture,
    view: &'a wgpu::TextureView,
    readback: &'a wgpu::Buffer,
    padded_bytes_per_row: u32,
}

/// CPU-owned geometry captured from the same settled shared-coarse diagnostic frame as the
/// paired dense and sparse outputs. This is intentionally diagnostic-only and is never retained
/// by the ordinary paint path.
#[doc(hidden)]
pub struct SharedCoarseGeometryDiagnostic {
    /// Fine-pass selection statistics from the captured frame.
    pub stats: vello::SparseRenderStats,
    /// Exact `vello.config` uniform bytes submitted for the frame.
    pub config: Vec<u8>,
    /// Logical PTCL download bytes, excluding any pool allocation tail.
    pub ptcl: Vec<u8>,
    /// Logical segment download bytes, excluding any pool allocation tail.
    pub segments: Vec<u8>,
}

/// CPU-owned witnesses from the guarded fixed-order Area replay. The two full outputs are
/// swizzled directly into the caller's buffers and are intentionally not retained here.
#[doc(hidden)]
pub struct SegmentOrderReplayDiagnostic {
    pub left_witness: Vec<u32>,
    pub right_witness: Vec<u32>,
}

#[derive(Clone, Copy, Debug)]
enum CompletionPolicy {
    /// One caller deadline for the whole frame; running out is [`BridgeError::DeadlineExceeded`].
    Absolute(Instant),
    /// Each Linux completion phase gets its own allowance, all of them clamped to the one
    /// `frame_deadline` every attempt and band of the frame shares; running out is
    /// [`BridgeError::CompletionTimeout`].
    LinuxPerPhase { frame_deadline: Instant },
}

impl CompletionPolicy {
    /// The error for a completion wait under this policy that ran out of time.
    fn timed_out(self, phase: CompletionPhase) -> BridgeError {
        match self {
            Self::Absolute(_) => BridgeError::DeadlineExceeded,
            Self::LinuxPerPhase { .. } => BridgeError::CompletionTimeout { phase },
        }
    }

    fn deadline(self) -> Instant {
        match self {
            Self::Absolute(deadline) => deadline,
            Self::LinuxPerPhase { frame_deadline } => frame_deadline,
        }
    }
}
const LINUX_FENCE_WAIT: Duration = Duration::from_secs(1);
const LINUX_MAP_CALLBACK_WAIT: Duration = Duration::from_secs(1);
const BUMP_SETTLEMENT_WAIT: Duration = Duration::from_secs(1);
/// Bound on everything one Linux frame blocks the present thread for, across all of its
/// recovery attempts and bands. The per-phase allowances above stay as they are -- a software
/// Vulkan frame can legitimately need each of them -- but without a frame bound they multiply:
/// three phases × [`RESILIENT_RENDER_ATTEMPTS`] × bands, 12 s and more. This leaves room for
/// two attempts at their full allowances and most of a third; past it the frame fails as a
/// completion timeout and the recovery back-off takes over.
const LINUX_FRAME_BUDGET: Duration = Duration::from_secs(8);
/// Rounds of "poll the counters, block on the submission" before settlement gives up. wgpu
/// runs every callback of a submission before its `Wait` returns, so one round settles the
/// counters; the bound only keeps a pending readback whose submission already completed from
/// spinning until the deadline.
const COUNTER_SETTLEMENT_ROUNDS: usize = 8;
/// Bound on the construction-time warm-up frames, which include pipeline compilation on CPU
/// Vulkan drivers and so are not frame work.
const PIPELINE_WARMUP: Duration = Duration::from_secs(60);

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WaitPhase {
    Fence,
    MapCallback,
    Marker,
}

#[cfg(test)]
struct CompletionTest {
    start: Instant,
    elapsed: Duration,
    encode_elapsed: Duration,
    fence_elapsed: Duration,
    callback_elapsed: Duration,
    pending_elapsed: Duration,
    waits: Vec<(WaitPhase, Duration)>,
    /// Remaining settlement time at each bump-counter poll: one entry per poll.
    counter_remaining: Vec<Duration>,
    /// The timeout of each blocking wait settlement issued on the counters' submission.
    counter_waits: Vec<Duration>,
    counters: std::collections::VecDeque<Result<vello::BumpBudgetPoll, vello::BumpBudgetError>>,
    failure: Option<WaitPhase>,
    destroy_at_marker: bool,
    /// Drop the framebuffer map callback's sender before it reports, as a lost device does.
    disconnect_map_callback: bool,
}

#[cfg(test)]
impl CompletionTest {
    fn now(&self) -> Instant {
        self.start
            .checked_add(self.elapsed)
            .expect("injected clock fits Instant")
    }

    fn advance(&mut self, elapsed: Duration) {
        self.elapsed = self
            .elapsed
            .checked_add(elapsed)
            .expect("bounded injected elapsed time");
    }
}

#[derive(Clone, Copy)]
enum FrameOutput<'a> {
    GpuOnly,
    Xrgb {
        buffer: &'a wgpu::Buffer,
        stride: u32,
        partial: bool,
    },
}

impl FrameOutput<'_> {
    fn append(
        self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        size: (u32, u32),
        damage: Option<&[vello::FineDamageRect]>,
    ) {
        let Self::Xrgb {
            buffer,
            stride,
            partial,
        } = self
        else {
            return;
        };
        if let Some(damage) = damage.filter(|_| partial) {
            for rect in damage {
                encoder.copy_texture_to_buffer(
                    wgpu::TexelCopyTextureInfo {
                        texture,
                        mip_level: 0,
                        origin: wgpu::Origin3d {
                            x: rect.x,
                            y: rect.y,
                            z: 0,
                        },
                        aspect: wgpu::TextureAspect::All,
                    },
                    wgpu::TexelCopyBufferInfo {
                        buffer,
                        layout: wgpu::TexelCopyBufferLayout {
                            offset: u64::from(rect.y)
                                .saturating_mul(u64::from(stride))
                                .saturating_add(u64::from(rect.x).saturating_mul(4)),
                            bytes_per_row: Some(stride),
                            rows_per_image: Some(rect.height),
                        },
                    },
                    wgpu::Extent3d {
                        width: rect.width,
                        height: rect.height,
                        depth_or_array_layers: 1,
                    },
                );
            }
        } else {
            VelloDrmBridge::append_full_copy(encoder, texture, buffer, stride, size.0, size.1);
        }
    }

    fn copy_bytes(self, size: (u32, u32), damage: Option<&[vello::FineDamageRect]>) -> u64 {
        match self {
            Self::GpuOnly => 0,
            Self::Xrgb { partial: true, .. } if damage.is_some() => {
                VelloDrmBridge::sparse_damage_readback_bytes(damage.unwrap_or_default())
            }
            Self::Xrgb { .. } => u64::from(size.0)
                .saturating_mul(u64::from(size.1))
                .saturating_mul(4),
        }
    }

    fn unmap(self) {
        if let Self::Xrgb { buffer, .. } = self {
            buffer.unmap();
        }
    }
}

/// Accounting for actual Vello attempts, completion markers and framebuffer output.
#[derive(Debug, Default)]
pub struct GpuFrameStats {
    pub revision: u64,
    pub damage_admitted: bool,
    pub render_attempts: usize,
    pub sparse_attempts: usize,
    pub full_attempts: usize,
    pub completion_markers: usize,
    pub framebuffer_copy_bytes: u64,
    pub framebuffer_staging_bytes: u64,
}

/// A GPU fault a development session's test control injects into the presenting renderer
/// (`gpu_fault`), so a test can prove the desktop outlives a GPU that dies under it. The
/// renderer's next frame applies it to its own device through the vendored HAL's switches
/// (`vendor/wgpu-hal/TILO-PATCH.md`), so everything after is the production path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InjectedGpuFault {
    /// The GPU is reset under the frame and works again.
    Reset,
    /// The GPU is reset and cannot be brought back: every device opened on it is lost too,
    /// until the renderer gives it up for software.
    Dead,
    /// The GPU stops finishing work, and nothing says so.
    Hang,
}

static INJECTED_GPU_FAULT: std::sync::Mutex<Option<InjectedGpuFault>> = std::sync::Mutex::new(None);

/// Have the presenting renderer's next frame suffer `fault` ([`InjectedGpuFault`]).
pub fn inject_gpu_fault(fault: InjectedGpuFault) {
    *INJECTED_GPU_FAULT.lock_recover() = Some(fault);
}

/// The fault a renderer on a device opened on the GPU (not in software) applies now: a reset
/// or a hang once, a dead GPU to every such device.
#[cfg(target_os = "linux")]
fn injected_gpu_fault(in_software: bool) -> Option<InjectedGpuFault> {
    let mut slot = INJECTED_GPU_FAULT.lock_recover();
    match *slot {
        Some(InjectedGpuFault::Dead) => (!in_software).then_some(InjectedGpuFault::Dead),
        _ => slot.take(),
    }
}

/// Whether the GPU was declared dead ([`InjectedGpuFault::Dead`]), so a device opened on it
/// now (not in software) is lost from its birth.
#[cfg(target_os = "linux")]
fn dead_gpu_injected(in_software: bool) -> bool {
    !in_software && *INJECTED_GPU_FAULT.lock_recover() == Some(InjectedGpuFault::Dead)
}

/// A renderer's device on `adapter` was lost, after it finished at least one frame or before
/// (`finished_frames`). Decides what opens next (`ui::gpu::recover`): when that gives the GPU
/// up, the person is told the desktop keeps working, slower, and the session's GPU record says
/// why.
pub fn gpu_lost(
    adapter: &wgpu::AdapterInfo,
    finished_frames: bool,
    reason: &str,
) -> ui::gpu::Recovery {
    let given_up_before = ui::gpu::software_fallback().is_some();
    let recovery = ui::gpu::recover(adapter, finished_frames, reason);
    if recovery == ui::gpu::Recovery::Software && !given_up_before {
        crate::shell::graphics_notice::gpu_given_up();
        #[cfg(target_os = "linux")]
        crate::compositor::gpu_health::record_gpu_lost(reason);
    }
    recovery
}

/// Drop `paint`'s bridge if its device was lost, so the next frame opens another: on the same
/// adapter, or in software once the GPU cannot be brought back ([`gpu_lost`]). Never waits for
/// the GPU. Returns why the device was lost.
pub fn retire_lost_bridge(paint: &mut Option<VelloDrmBridge>) -> Option<String> {
    let bridge = paint.as_mut()?;
    let reason = bridge.device_lost()?;
    let recovery = gpu_lost(bridge.adapter(), bridge.finished_frames(), &reason);
    tracing::error!(
        %reason,
        ?recovery,
        "the renderer's GPU device was lost; opening a new one"
    );
    *paint = None;
    Some(reason)
}

/// Same-device retained Vello output. Sampling is permitted only after successful completion.
pub struct RetainedGpuTarget {
    device: wgpu::Device,
    queue: wgpu::Queue,
    loss: ui::gpu::DeviceLossMonitor,
    poisoned: bool,
    geometry_generation: u64,
    completion_markers: usize,
    #[cfg(test)]
    completion_test: Option<CompletionTest>,
    /// Replaces the Linux per-phase completion allowance for correctness fixtures that must
    /// wait for the GPU to finish rather than for a production frame budget.
    #[cfg(test)]
    phase_wait_override: Option<Duration>,
    /// The vello renderer parented to the same wgpu device as `texture`, cached across frames so we don't pay the pipeline-compile cost on every paint.
    renderer: Renderer,
    /// Client frames `renderer` draws straight from their GPU textures.
    client_frames: crate::shell::gpu_frames::AppliedFrames,
    /// The presenting-device generation client frames are imported on, when
    /// this target renders on that device.
    present_generation: Option<u64>,

    /// Offscreen render target. Recreated on resize.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// The band a target past [`MAX_VELLO_BINS`] is rendered through, and its height; see
    /// [`RetainedGpuTarget::band_rows`]. Allocated on the first banded frame.
    band: Option<(wgpu::Texture, wgpu::TextureView, u32)>,
    /// Current native extent; resize recreates the target texture.
    size: (u32, u32),
    /// The latest render covered the whole target. Its scratch buffers are
    /// sized for that and stay pooled until trimmed.
    last_frame_full: bool,
    /// Base color used to clear when the caller's scene is empty; matches the winit backend's fallback so the host window doesn't flash white.
    base_color: Color,
    /// The target contains a complete settled frame, independent of CPU staging validity.
    retained_target_valid: bool,
    /// Frames the device finished on this target since its warm-up. A later loss of a device
    /// that finished any is a GPU reset, not a GPU that cannot draw at all (`ui::gpu::recover`).
    finished_frames: u64,
    /// Revision associated with the complete retained frame. Legacy full-render APIs do not
    /// carry a revision, so they intentionally clear this even though their target is complete.
    last_successful_revision: Option<u64>,
    /// Damage of frames an output skipped while its page flip was pending, in target pixels.
    /// `last_successful_revision` already names the newest of them; the next direct render
    /// repairs these regions with its own (see [`VelloDrmBridge::defer_output`]).
    owed_damage: Vec<vello::FineDamageRect>,
    /// Total Vello submissions used by the latest paint, including sparse work and bounded
    /// bump-budget recovery retries.
    last_render_attempts: usize,
    /// Sparse submissions used by the latest paint. Kept separate from dense recovery so
    /// benchmark reporting never calls a sparse frame a full frame.
    last_sparse_render_attempts: usize,
    /// Dense submissions used by the latest paint, including bump-budget retries.
    last_full_render_attempts: usize,
    /// Full submissions from the latest paint whose recorded Vello pass selected canonical
    /// segment order. This is route-attempt telemetry only, not a completion or speed claim.
    last_canonical_segment_order_full_attempts: usize,
    /// Sparse submissions from the latest paint whose recorded Vello pass selected canonical
    /// segment order. Sparse overflow is still counted before its dense recovery.
    last_canonical_segment_order_sparse_attempts: usize,
    /// HDR layers the next render composites: each attempt first renders the
    /// scene with the layer images in their channel colours, then the frame
    /// itself with them black.
    hdr_pass: Option<HdrPass>,
    /// What the floating surfaces of the frames rendered from now on are
    /// drawn over. Each render attempt blurs it first, so a recovery retry
    /// redraws the backdrop with the grown budget too.
    backdrop: Option<Arc<ui::backdrop::BackdropFrame>>,
    /// Built with the first frame that has a backdrop.
    backdrop_blur: Option<ui::backdrop::BackdropBlur>,
    /// Full submissions from the latest paint that actually selected the experimental Area PPT8
    /// shader. This is route-attempt telemetry only, not a completion or speed claim.
    last_area_ppt8_full_attempts: usize,
    /// Sparse submissions from the latest paint that actually selected the experimental Area PPT8
    /// shader, including an overflowed sparse attempt before dense recovery.
    last_area_ppt8_sparse_attempts: usize,
    last_clip_single_group_full_attempts: usize,
    /// Sparse submissions from the latest paint that selected the single-workgroup clip leaf.
    last_clip_single_group_sparse_attempts: usize,
    /// Actual coarse-dispatch choice returned by the latest sparse submission. This remains
    /// available when that submission overflows and is followed by a dense recovery, so
    /// diagnostics describe the submitted sparse work rather than guessing from capability.
    last_sparse_render_stats: Option<vello::SparseRenderStats>,
    /// The layers strip pans are composited from, allocated by the first
    /// frame that comes as layers ([`Self::render_layers_until`]).
    strip_layers: Option<super::strip_layers::StripLayers>,
    /// The work the latest layered frame did.
    last_layer_work: Option<super::strip_layers::LayerWork>,
    /// The scroll copies of the frame of the revision named, for the next
    /// render of it ([`Self::stage_scroll_copies`]).
    staged_copies: Option<(u64, crate::shell::frame::ScrollCopies)>,
    /// What the next sparse render rasterises instead of its damage: the
    /// staged copies were made, so it repaints only what they did not move.
    raster_override: Option<Vec<vello::FineDamageRect>>,
    /// Scratch space copies within `texture` move through.
    copy_scratch: ui::scroll_copy::CopyScratch,
    /// The pixels the latest render rasterised and copied, when it made
    /// scroll copies.
    last_scroll_copy_work: Option<(u64, u64)>,
}

/// CPU-fallback bridge between a vello `Scene` and an RGBA byte buffer. Owns its wgpu device +
/// offscreen texture + readback buffer so callers can reuse it across frames. Single-output by design — multi-output is one bridge per output.
#[cfg_attr(
    test,
    allow(
        clippy::struct_excessive_bools,
        reason = "the cfg(test)-only tracked-swizzle fault bit is a narrow cleanup-path injection, not production state"
    )
)]
pub struct VelloDrmBridge {
    context: RenderContext,
    target: RetainedGpuTarget,
    device_errors: DeviceErrorLatch,
    /// The device's loss, wgpu's report or work that stopped finishing ([`Self::device_lost`]).
    loss_watch: ui::gpu::LossWatch,
    adapter: wgpu::AdapterInfo,
    bgra_blitter: TextureBlitter,
    dispatch_stage_observer: Option<DispatchStageObserver>,
    /// SDR → PQ encode passes, one per HDR scanout format in use.
    hdr_encoders: Vec<HdrEncodePipeline>,
    /// The SDR final pass of rotated or calibrated outputs; built with the first one.
    oriented_copy: Option<OrientedCopy>,
    /// Composites HDR layers; built with the first frame that has any.
    hdr_compositor: Option<HdrCompositor>,
    dev_id: usize,

    /// Readback buffer (padded to the wgpu row alignment requirement).
    readback_buf: wgpu::Buffer,
    /// Bytes-per-row in the padded readback buffer; may exceed `width * 4` when the tight stride doesn't divide 256.
    padded_bytes_per_row: u32,
    /// Tight (no padding) RGBA buffer the public `paint` returns.
    readback: Vec<u8>,

    /// Profiling is off in production; when on, timings describe only the latest paint.
    phase_timing_enabled: bool,
    last_phase_timing: Option<BridgePhaseTiming>,

    retained_staging_valid: bool,
    staging_content: Option<(u64, Option<u64>)>,
    /// No frame has been painted through this bridge yet ([`Self::take_first_frame`]).
    first_frame_pending: bool,

    /// Process-unique identity and monotonic content epoch for opt-in destination histories.
    xrgb_history_generation: Option<u64>,
    xrgb_history_epoch: Option<u64>,
    /// Deadline before another replacement renderer may be submitted after a Linux completion
    /// timeout.  A timed-out readback leaves the previous scanout untouched; retrying at the
    /// compositor's normal cadence would otherwise queue one more llvmpipe frame every tick.
    recovery_not_before: Option<Instant>,
    recovery_backoff: Duration,
    /// One retained surface per output that is not the whole scene, for
    /// [`Self::paint_output_until`]. They share `target`'s renderer and device.
    output_surfaces: Vec<(super::OutputId, OutputSurface)>,
    /// On a software rasteriser, has each heap trim return the device memory
    /// closed windows freed for as long as this bridge lives.
    _device_memory: Option<crate::shell::heap_trim::DeviceRegistration>,
    /// Atlas side and renderer allocations at the last `RENDERER-MEMORY` line.
    logged_memory: Option<[u64; 6]>,
    /// [`crate::shell::heap_trim::windows_closed`] at the last frame.
    windows_closed_seen: u64,
    /// The renderer's [`vello::Renderer::scratch_shrinks`] at the last frame.
    scratch_shrinks_seen: u64,
    /// Narrow fault injection for the tracked-swizzle cleanup test. It is absent from ordinary
    /// binaries and shortens only the test's mapped source view after GPU completion.
    #[cfg(test)]
    fail_next_tracked_swizzle: bool,
}
