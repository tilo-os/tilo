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
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use pollster::FutureExt;
use vello::peniko::Color;
// `ui::gpu`'s context, not `vello::util`'s: this device is shared with every other client on the machine, so it needs the same memory hint — see the header of `crates/sdk/ui-core/src/gpu.rs`.
use ui::gpu::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};
use wgpu::util::TextureBlitter;

use super::hdr_composite::{HdrCompositor, HdrPass};
use super::hdr_encode::HdrEncodePipeline;
use super::icc_lut::IccLut;
use super::orientation::OrientedCopy;
use super::xrgb_history::XrgbWritePlan;
pub use super::xrgb_history::{
    XrgbHistoryCommit, XrgbHistoryIdentity, XrgbHistoryTransaction, XrgbTargetHistory,
};
use crate::shell::hdr_surface::HdrLayer;
use settings_store::hardware::OutputTransform;

mod output_view;
#[cfg(test)]
#[cfg(target_os = "linux")]
mod window_cycle_memory_tests;
use output_view::OutputSurface;
pub use output_view::{OutputSignal, OutputTarget, OutputView};

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
            let mut slot = sink
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(error.to_string());
            }
        }));
        latch
    }

    fn take(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
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
    /// Full submissions from the latest paint that selected the single-workgroup clip leaf.
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
}

/// Whether this process has published its pipelines for apps (see [`ui::pipeline_seed`]).
static PIPELINE_SEED_PUBLISHED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// An empty pipeline cache for the first renderer this session builds, which then becomes the
/// seed every app starts from. Later renderers compile exactly the same pipelines, so one seed
/// per session is all there is to publish. Never in unit tests: they must not write into the
/// real session's runtime directory.
fn pipeline_seed_to_publish(gpu: &ui::gpu::DeviceHandle) -> Option<wgpu::PipelineCache> {
    if cfg!(test) || PIPELINE_SEED_PUBLISHED.swap(true, std::sync::atomic::Ordering::AcqRel) {
        return None;
    }
    ui::pipeline_seed::empty(gpu)
}

/// Replace the seed for `gpu` under `dir` atomically, readable by every app and writable by the
/// session only. `Ok(false)` when the backend has no cache data to share.
fn write_pipeline_seed(
    gpu: &ui::gpu::DeviceHandle,
    cache: &wgpu::PipelineCache,
    dir: &std::path::Path,
) -> std::io::Result<bool> {
    let Some(file) = ui::pipeline_seed::seed_file(dir, &gpu.adapter.get_info()) else {
        return Ok(false);
    };
    let Some(data) = cache.get_data() else {
        return Ok(false);
    };
    std::fs::create_dir_all(dir)?;
    let staging = file.with_extension("tmp");
    std::fs::write(&staging, &data)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o644))?;
    }
    std::fs::rename(&staging, &file)?;
    Ok(true)
}

/// Write the session's pipelines where the launcher binds them, read-only, into every app.
fn publish_pipeline_seed(gpu: &ui::gpu::DeviceHandle, cache: &wgpu::PipelineCache) {
    let dir = crate::shell::launcher::runtime_dir()
        .join("tilo")
        .join(launcher::gpu::PIPELINE_SEED_DIR_NAME);
    match write_pipeline_seed(gpu, cache, &dir) {
        Ok(true) => tracing::info!(
            target: "shell::render",
            dir = %dir.display(),
            "published the session's vello pipelines for apps to start from"
        ),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            target: "shell::render",
            dir = %dir.display(),
            %error,
            "could not publish the session's vello pipelines; apps compile their own"
        ),
    }
}

impl RetainedGpuTarget {
    /// Construct an offscreen target on the supplied context-owned device and queue.
    ///
    /// # Errors
    /// Returns a size or renderer error if construction cannot complete.
    pub fn new(gpu: &ui::gpu::DeviceHandle, width: u32, height: u32) -> Result<Self, BridgeError> {
        Self::validate_size(&gpu.device, width, height)?;
        if let Some(reason) = gpu.loss_monitor().reason() {
            return Err(BridgeError::DeviceLost(reason));
        }
        let (texture, view) = Self::allocate_texture(&gpu.device, width, height);
        #[cfg(target_os = "linux")]
        crate::shader_seed::observe_device(gpu.adapter.get_info());
        let seed = pipeline_seed_to_publish(gpu);
        let mut renderer = Renderer::new(
            &gpu.device,
            RendererOptions {
                use_cpu: false,
                // Every frame renders with area anti-aliasing; MSAA pipelines would go unused.
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: seed.clone(),
            },
        )
        .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;
        // Atomic tile allocation otherwise sums Area coverage in GPU scheduling order.
        // The same unchanged desktop can then round to different pixels in dense and
        // retained renders. Keep their segment order stable on the adaptive shell path;
        // Vello still preserves its pinned-budget and overflow-validation guards.
        renderer
            .enable_canonical_segment_order(&gpu.device)
            .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;
        // Once the desktop got lighter (windows closed) and stayed so, the
        // scratch its busiest scene grew comes back: the pool drops buffers
        // no frame takes and the bump reservation comes down, with headroom.
        renderer.set_shrink_scratch(true);
        if let Some(seed) = seed {
            publish_pipeline_seed(gpu, &seed);
        }
        let mut target = Self {
            device: gpu.device.clone(),
            queue: gpu.queue.clone(),
            renderer,
            client_frames: crate::shell::gpu_frames::AppliedFrames::default(),
            present_generation: None,

            texture,
            view,
            band: None,
            loss: gpu.loss_monitor(),
            poisoned: false,
            geometry_generation: 1,
            completion_markers: 0,
            #[cfg(test)]
            completion_test: None,
            #[cfg(test)]
            phase_wait_override: None,
            size: (width, height),
            last_frame_full: false,
            base_color: Color::from_rgb8(0x1A, 0x1B, 0x26),
            retained_target_valid: false,
            last_successful_revision: None,
            owed_damage: Vec::new(),
            last_render_attempts: 0,
            last_sparse_render_attempts: 0,
            last_full_render_attempts: 0,
            last_canonical_segment_order_full_attempts: 0,
            last_canonical_segment_order_sparse_attempts: 0,
            hdr_pass: None,
            backdrop: None,
            backdrop_blur: None,
            last_area_ppt8_full_attempts: 0,
            last_area_ppt8_sparse_attempts: 0,
            last_clip_single_group_full_attempts: 0,
            last_clip_single_group_sparse_attempts: 0,
            last_sparse_render_stats: None,
            strip_layers: None,
            last_layer_work: None,
        };
        if gpu.adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            // Lavapipe compiles each pipeline on its first dispatch: on a cold shader cache,
            // seconds of one-at-a-time work the warm-up below would add to the first frame.
            // Extra devices come from the session's shared budget; without any, this device
            // compiles in place.
            let slots = super::warmup::acquire();
            target.renderer.precompile_pipelines(
                &target.device,
                &target.queue,
                slots.len(),
                &|| gpu.open_peer(),
            );
            drop(slots);
        }
        target.warm_pipelines()?;
        Ok(target)
    }

    /// Run the full and the sparse render paths once so every frame deadline measures rendering
    /// alone. Mesa's CPU Vulkan driver (lavapipe, the DRM CPU-fallback adapter) compiles each
    /// compute pipeline on its first dispatch, not at creation. With a cold shader cache (first
    /// boot, or a fresh CI home) that compilation ran inside the first frame's one-second fence
    /// allowance and failed it, and the first sparse frame paid it again.
    ///
    /// # Errors
    /// Fails when the device cannot complete the warm-up frames within [`PIPELINE_WARMUP`].
    fn warm_pipelines(&mut self) -> Result<(), BridgeError> {
        use vello::kurbo::{Affine, Rect};
        use vello::peniko::{BlendMode, Fill};

        let (width, height) = self.size;
        let full = Rect::new(0.0, 0.0, f64::from(width), f64::from(height));
        let inset = full.inset(-f64::from(width.min(height)) / 4.0);
        // The picture is never shown, so it reuses the base colour rather than stating its own.
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            self.base_color,
            None,
            &full,
        );
        // A clip layer adds the clip reduce/leaf stages ordinary shell frames use.
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            Affine::IDENTITY,
            &inset,
        );
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            self.base_color.with_alpha(0.5),
            None,
            &full,
        );
        scene.pop_layer();
        let damage = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: width.min(16),
            height: height.min(16),
        }];
        let deadline = Instant::now()
            .checked_add(PIPELINE_WARMUP)
            .ok_or_else(|| BridgeError::Renderer("warm-up deadline out of range".to_owned()))?;
        for damage in [
            SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: None,
            },
            SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&damage),
            },
        ] {
            self.render_until(&scene, damage, deadline)?;
        }
        // The warm-up picture is not content: the first real frame renders in full.
        self.invalidate();
        self.reset_attempts();
        Ok(())
    }

    /// Change the clear color, invalidating retained content only when it changes.
    pub fn set_base_color(&mut self, color: Color) {
        if self.base_color != color {
            self.base_color = color;
            self.invalidate();
            // The backdrop layer is rendered over the base colour.
            if let Some(layers) = self.strip_layers.as_mut() {
                layers.shown = None;
            }
        }
    }

    /// Resize the target. Even a rejected or no-op resize invalidates its content relation.
    ///
    /// # Errors
    /// Rejects zero/oversized extents and poisoned or lost devices before allocation.
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        self.invalidate();
        self.check_health()?;
        Self::validate_size(&self.device, width, height)?;
        if (width, height) != self.size {
            (self.texture, self.view) = Self::allocate_texture(&self.device, width, height);
            self.band = None;
            // Pooled scratch is sized for the old extent; a session that
            // changed mode never renders at it again.
            self.renderer.trim_pooled_buffers();
            self.size = (width, height);
            self.geometry_generation = self.geometry_generation.saturating_add(1);
        }
        Ok(())
    }

    fn validate_size(device: &wgpu::Device, width: u32, height: u32) -> Result<(), BridgeError> {
        let limit = device.limits().max_texture_dimension_2d;
        if width == 0 || height == 0 || width > limit || height > limit {
            return Err(BridgeError::InvalidSize(format!(
                "{width}x{height}, device limit {limit}"
            )));
        }
        Ok(())
    }

    fn invalidate(&mut self) {
        self.retained_target_valid = false;
        self.last_successful_revision = None;
        self.owed_damage.clear();
    }

    /// `regions` together with the damage owed by frames skipped since the last render,
    /// which this takes; `None` when nothing is owed or the frame renders in full anyway.
    fn take_owed_damage(
        &mut self,
        regions: Option<&[vello::FineDamageRect]>,
    ) -> Option<Vec<vello::FineDamageRect>> {
        let owed = std::mem::take(&mut self.owed_damage);
        regions
            .filter(|_| !owed.is_empty())
            .map(|regions| [regions, owed.as_slice()].concat())
    }

    fn check_health(&self) -> Result<(), BridgeError> {
        if let Some(reason) = self.loss.reason() {
            return Err(BridgeError::DeviceLost(reason));
        }
        if self.poisoned {
            return Err(BridgeError::RecreateRequired);
        }
        Ok(())
    }

    fn check_live(&self, policy: CompletionPolicy) -> Result<(), BridgeError> {
        self.check_health()?;
        if self.now() >= policy.deadline() {
            return Err(policy.timed_out(CompletionPhase::FrameBudget));
        }
        Ok(())
    }

    #[allow(
        clippy::unused_self,
        reason = "the test clock observes this actual target instance"
    )]
    fn now(&self) -> Instant {
        #[cfg(test)]
        if let Some(clock) = &self.completion_test {
            return clock.now();
        }
        Instant::now()
    }

    /// A Linux allowance: `nominal` outside correctness fixtures.
    #[allow(
        clippy::unused_self,
        reason = "correctness fixtures replace the allowance on this actual target instance"
    )]
    fn linux_allowance(&self, nominal: Duration) -> Duration {
        #[cfg(test)]
        if let Some(wait) = self.phase_wait_override {
            return wait;
        }
        nominal
    }

    /// The Linux policy for a frame starting now: its per-phase allowances share one
    /// [`LINUX_FRAME_BUDGET`] across every attempt and band.
    fn linux_frame_policy(&self) -> Result<CompletionPolicy, BridgeError> {
        self.now()
            .checked_add(self.linux_allowance(LINUX_FRAME_BUDGET))
            .map(|frame_deadline| CompletionPolicy::LinuxPerPhase { frame_deadline })
            .ok_or_else(|| BridgeError::DevicePoll("frame deadline out of range".to_owned()))
    }

    /// How long one completion phase may block now: its own allowance on Linux, never past
    /// the frame's deadline.
    fn phase_wait(&self, policy: CompletionPolicy, nominal: Duration) -> Duration {
        let remaining = policy.deadline().saturating_duration_since(self.now());
        match policy {
            CompletionPolicy::Absolute(_) => remaining,
            CompletionPolicy::LinuxPerPhase { .. } => self.linux_allowance(nominal).min(remaining),
        }
    }

    fn eligible_sparse_damage(
        &self,
        damage: SceneDamage<'_>,
    ) -> Option<Vec<vello::FineDamageRect>> {
        if self.last_successful_revision != Some(damage.previous_revision?)
            || !self.retained_target_valid
            || !self.renderer.sparse_fine_supported()
            || Self::band_rows(self.size).is_some()
        {
            return None;
        }
        VelloDrmBridge::normalize_damage(damage.regions?, self.size)
    }

    /// Render under one caller deadline, with no framebuffer staging/copy/map/swizzle.
    /// Tiny Vello bump downloads remain mandatory recovery metadata, independently of pixels.
    ///
    /// # Errors
    /// A failure hides the view. Unresolved or failed submitted work poisons this target;
    /// reconstruct it before reuse (a lost device requires a new context-owned device).
    pub fn render_until(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        deadline: Instant,
    ) -> Result<GpuFrameStats, BridgeError> {
        let (_, admitted) = self.render(
            scene,
            Some(damage),
            FrameOutput::GpuOnly,
            CompletionPolicy::Absolute(deadline),
            None,
            |_, _, _| {},
        )?;
        Ok(GpuFrameStats {
            revision: damage.revision,
            damage_admitted: admitted,
            render_attempts: self.last_render_attempts,
            sparse_attempts: self.last_sparse_render_attempts,
            full_attempts: self.last_full_render_attempts,
            completion_markers: self.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    /// Whether [`Self::render_layers_until`] can present `cache`: a target
    /// rendered whole, in one piece, of the size the layers are drawn at.
    pub fn layers_eligible(&self, cache: &crate::shell::frame::FrameCache) -> bool {
        !cache.strips.is_empty()
            && cache.strips.len() == cache.layers.strips.len()
            && Self::band_rows(self.size).is_none()
            && cache.width >= self.size.0
            && self.hdr_pass.is_none()
            && self.backdrop.is_none()
    }

    /// Render a frame that came as layers ([`crate::shell::frame::FramePaint::cache`])
    /// by compositing retained layer textures: a strip pan copies what stays
    /// on screen and rasterises only what it uncovered (see
    /// [`super::strip_layers`]). `post_render` then reads the composited
    /// target, as after [`Self::render_until`]. Waits for completion.
    ///
    /// # Errors
    /// As [`Self::render_until`]; [`BridgeError::InvalidSize`] when the
    /// layers are not [`Self::layers_eligible`]. A failure forgets the
    /// layers, so the next layered frame rasterises them whole.
    pub fn render_layers_until(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        revision: u64,
        deadline: Instant,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<GpuFrameStats, BridgeError> {
        if !self.layers_eligible(cache) {
            return Err(BridgeError::InvalidSize(format!(
                "layers {} wide with {} strips cannot be composited into a {}x{} target",
                cache.width,
                cache.strips.len(),
                self.size.0,
                self.size.1
            )));
        }
        let policy = CompletionPolicy::Absolute(deadline);
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.reset_attempts();
        self.check_live(policy)?;
        if self
            .strip_layers
            .as_ref()
            .is_none_or(|layers| layers.size != self.size)
        {
            self.strip_layers = Some(super::strip_layers::StripLayers::new(
                &self.device,
                self.size,
            ));
        }
        let shown = self.strip_layers.as_ref().and_then(|layers| {
            layers.shown.as_ref().map(|shown| {
                let current = self.retained_target_valid
                    && shown.revision.is_some()
                    && self.last_successful_revision == shown.revision;
                (shown, current)
            })
        });
        let work = match shown {
            Some((shown, current)) => {
                super::strip_layers::plan(Some(shown), cache, self.size, current)
            }
            None => super::strip_layers::plan(None, cache, self.size, false),
        };
        // The target holds this frame only once the composite lands.
        self.invalidate();
        let result = self.run_layer_work(cache, &work, policy, post_render);
        match &result {
            Ok(()) => {
                self.retained_target_valid = true;
                self.last_successful_revision = Some(revision);
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.shown = Some(super::strip_layers::Shown::of(cache, revision));
                }
            }
            Err(error) => {
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.shown = None;
                }
                if !matches!(error, BridgeError::RecoveryExhausted { .. }) {
                    self.poisoned = true;
                }
            }
        }
        self.last_layer_work = Some(work);
        result?;
        Ok(GpuFrameStats {
            revision,
            damage_admitted: true,
            render_attempts: self.last_render_attempts,
            sparse_attempts: self.last_sparse_render_attempts,
            full_attempts: self.last_full_render_attempts,
            completion_markers: self.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    /// Bring the layer textures up to `cache` without compositing it: the
    /// desktop at rest drawn as layers
    /// ([`crate::shell::frame::FrameBuilder::layers_at_rest`]), so that the
    /// first frame of the next strip pan copies and composites instead of
    /// rasterising every layer. Rasterises only the layers that differ from
    /// what the textures hold, a whole one in bands
    /// ([`super::strip_layers::warm_bands`]), and stops between two renders
    /// as soon as `keep_going` says a frame waits, keeping what it finished.
    /// The output target is left alone. Returns the pixels rasterised, or
    /// `None` when the layers cannot present `cache`.
    ///
    /// # Errors
    /// As [`Self::render_layers_until`]; a failure forgets the layers.
    pub fn warm_layers(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        deadline: Instant,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        if self.poisoned || !self.layers_eligible(cache) {
            return Ok(None);
        }
        let policy = CompletionPolicy::Absolute(deadline);
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.reset_attempts();
        self.check_live(policy)?;
        if self
            .strip_layers
            .as_ref()
            .is_none_or(|layers| layers.size != self.size)
        {
            self.strip_layers = Some(super::strip_layers::StripLayers::new(
                &self.device,
                self.size,
            ));
        }
        if let Some(layers) = self.strip_layers.as_mut() {
            layers.prime_composite(&self.device, &self.queue);
        }
        let shown = self
            .strip_layers
            .as_ref()
            .and_then(|layers| layers.shown.as_ref());
        let work = super::strip_layers::plan(shown, cache, self.size, false);
        if work.rasterises_nothing() {
            return Ok(Some(0));
        }
        let mut shown = self
            .strip_layers
            .as_mut()
            .and_then(|layers| layers.shown.take())
            .unwrap_or_else(super::strip_layers::Shown::unknown);
        // The output keeps the frame it shows, which these textures no
        // longer composite to.
        shown.revision = None;
        let warmed = self.warm_into(&mut shown, cache, &work, policy, keep_going);
        // The pan's first frame renders the scenes the warm just rendered:
        // the scratch they left pooled is the scratch it needs, so it must
        // not be trimmed as a full frame's would be. What no render takes
        // again ages out of the pool (`Renderer::set_shrink_scratch`).
        self.last_frame_full = false;
        if let Some(layers) = self.strip_layers.as_mut() {
            layers.shown = warmed.is_ok().then_some(shown);
        }
        match warmed {
            Ok(pixels) => Ok(Some(pixels)),
            Err(error) => {
                if !matches!(error, BridgeError::RecoveryExhausted { .. }) {
                    self.poisoned = true;
                }
                Err(error)
            }
        }
    }

    /// [`Self::warm_layers`]' renders, noting in `shown` each layer as it
    /// is done. Returns the pixels rasterised.
    fn warm_into(
        &mut self,
        shown: &mut super::strip_layers::Shown,
        cache: &crate::shell::frame::FrameCache,
        work: &super::strip_layers::LayerWork,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<u64, BridgeError> {
        use super::strip_layers::{Layer, WorkspaceWork};
        let bands = super::strip_layers::warm_bands(self.size)
            .filter(|_| self.renderer.sparse_fine_supported());
        let whole = u64::from(self.size.0).saturating_mul(u64::from(self.size.1));
        let mut pixels = 0_u64;
        for (layer, wanted) in [
            (Layer::Backdrop, work.backdrop),
            (Layer::Chrome, work.chrome),
        ] {
            if !wanted {
                continue;
            }
            if !keep_going() {
                return Ok(pixels);
            }
            // A chrome texture that holds the drawing the chrome changed
            // from takes only the tiles that changed (a clock tick).
            let damaged = work
                .chrome_tiles
                .clone()
                .filter(|_| layer == Layer::Chrome && self.renderer.sparse_fine_supported());
            shown.forget(layer);
            let (scene, base) = if layer == Layer::Backdrop {
                (&cache.layers.backdrop, self.base_color)
            } else {
                (&cache.layers.chrome, Color::TRANSPARENT)
            };
            if let Some(rects) = damaged {
                let area = rects
                    .iter()
                    .map(|rect| u64::from(rect.width).saturating_mul(u64::from(rect.height)))
                    .fold(0, u64::saturating_add);
                if !rects.is_empty() {
                    self.render_layer(layer, scene, Some(rects), base, policy)?;
                }
                shown.record(layer, cache);
                pixels = pixels.saturating_add(area);
                continue;
            }
            let done =
                self.warm_whole(layer, (scene, base), bands.as_deref(), policy, keep_going)?;
            if !done {
                return Ok(pixels);
            }
            shown.record(layer, cache);
            pixels = pixels.saturating_add(whole);
        }
        match (work.wheel, cache.layers.wheel.as_ref()) {
            (Some(area), Some(wheel)) => {
                if !keep_going() {
                    return Ok(pixels);
                }
                shown.forget(Layer::Wheel);
                if let Some(layers) = self.strip_layers.as_mut() {
                    layers.size_wheel(&self.device, (wheel.width, wheel.height));
                }
                self.render_layer(Layer::Wheel, &wheel.scene, None, Color::TRANSPARENT, policy)?;
                shown.record(Layer::Wheel, cache);
                pixels = pixels.saturating_add(area);
            }
            // The wheel's texture holds what it shows, or no wheel shows.
            _ => shown.record(Layer::Wheel, cache),
        }
        if work.workspace == WorkspaceWork::Unchanged || !keep_going() {
            return Ok(pixels);
        }
        shown.forget(Layer::Workspace);
        let strips =
            self.warm_strips(cache, &work.workspace, bands.as_deref(), policy, keep_going)?;
        if let Some(strips) = strips {
            shown.record(Layer::Workspace, cache);
            pixels = pixels.saturating_add(strips);
        }
        Ok(pixels)
    }

    /// [`Self::warm_layers`]' work on the strips texture: the pixels it
    /// rasterised, or `None` when it stopped for a frame before the texture
    /// held `cache`'s strips.
    fn warm_strips(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        work: &super::strip_layers::WorkspaceWork,
        bands: Option<&[vello::FineDamageRect]>,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        use super::strip_layers::{Layer, WorkspaceWork};
        let mut scene = Scene::new();
        super::strip_layers::append_panned(&mut scene, &cache.layers, &cache.strips);
        match work {
            WorkspaceWork::Unchanged => Ok(Some(0)),
            WorkspaceWork::Whole => {
                let whole = u64::from(self.size.0).saturating_mul(u64::from(self.size.1));
                let done = self.warm_whole(
                    Layer::Workspace,
                    (&scene, Color::TRANSPARENT),
                    bands,
                    policy,
                    keep_going,
                )?;
                Ok(done.then_some(whole))
            }
            WorkspaceWork::Pan { copies, uncovered } => {
                if !copies.is_empty() {
                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("tilo-strip-layer-warm-pan"),
                            });
                    if let Some(layers) = self.strip_layers.as_mut() {
                        layers.encode_copies(&self.device, &mut encoder, copies);
                    }
                    self.queue.submit([encoder.finish()]);
                }
                if !uncovered.is_empty() {
                    let regions = self
                        .renderer
                        .sparse_fine_supported()
                        .then(|| uncovered.clone());
                    self.render_layer(
                        Layer::Workspace,
                        &scene,
                        regions,
                        Color::TRANSPARENT,
                        policy,
                    )?;
                }
                Ok(Some(
                    uncovered
                        .iter()
                        .map(|rect| u64::from(rect.width).saturating_mul(u64::from(rect.height)))
                        .fold(0, u64::saturating_add),
                ))
            }
        }
    }

    /// Rasterise `scene` over `base` into all of `layer`, band by band when
    /// `bands` cuts it, asking `keep_going` between two bands. Whether the
    /// whole layer got drawn.
    fn warm_whole(
        &mut self,
        layer: super::strip_layers::Layer,
        (scene, base): (&Scene, Color),
        bands: Option<&[vello::FineDamageRect]>,
        policy: CompletionPolicy,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<bool, BridgeError> {
        let Some(bands) = bands else {
            self.render_layer(layer, scene, None, base, policy)?;
            return Ok(true);
        };
        for (index, band) in bands.iter().enumerate() {
            if index > 0 && !keep_going() {
                return Ok(false);
            }
            self.render_layer(layer, scene, Some(vec![*band]), base, policy)?;
        }
        Ok(true)
    }

    /// The target's pixels, RGBA, row by row: what the final pass reads.
    #[cfg(test)]
    pub(crate) fn read_rgba_for_test(&self) -> Vec<u8> {
        let (width, height) = self.size;
        let row = width.saturating_mul(4);
        let padded = row
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .saturating_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let buffer = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-target-readback-for-test"),
            size: u64::from(padded).saturating_mul(u64::from(height)),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        encoder.copy_texture_to_buffer(
            self.texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        self.queue.submit([encoder.finish()]);
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        self.device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(tilo_test_env::hang_bound()),
            })
            .expect("the readback copy completes");
        rx.recv().expect("map callback").expect("map succeeds");
        let mapped = slice.get_mapped_range();
        let (padded, row) = (
            usize::try_from(padded).expect("row pitch fits usize"),
            usize::try_from(row).expect("row bytes fit usize"),
        );
        mapped
            .chunks_exact(padded)
            .flat_map(|line| &line[..row])
            .copied()
            .collect()
    }

    /// The work the latest [`Self::render_layers_until`] planned.
    #[cfg(test)]
    pub(crate) fn last_layer_work_for_test(&self) -> Option<&super::strip_layers::LayerWork> {
        self.last_layer_work.as_ref()
    }

    /// Keep the frame `damage` describes without rendering it, as the DRM
    /// presenter keeps a lone output's image for a frame proven unchanged
    /// (empty damage) relative to the one this target holds: the target then
    /// holds this revision. `false`, and nothing kept, for any other frame.
    pub fn keep_unchanged(&mut self, damage: SceneDamage<'_>) -> bool {
        let unchanged = damage.regions.is_some_and(<[_]>::is_empty);
        let holds_previous = damage.previous_revision.is_some()
            && self.last_successful_revision == damage.previous_revision;
        if !unchanged
            || !holds_previous
            || !self.retained_target_valid
            || !self.owed_damage.is_empty()
        {
            return false;
        }
        if let Some(previous) = damage.previous_revision {
            self.layers_kept(previous, damage.revision);
        }
        self.last_successful_revision = Some(damage.revision);
        true
    }

    /// The target went from `previous` to `revision` without a pixel
    /// changing: layers it was composited from as `previous` are still what
    /// it shows, so the next pan composites only what moves.
    pub(super) fn layers_kept(&mut self, previous: u64, revision: u64) {
        if let Some(shown) = self
            .strip_layers
            .as_mut()
            .and_then(|layers| layers.shown.as_mut())
            && shown.revision == Some(previous)
        {
            shown.revision = Some(revision);
        }
    }

    /// The work the latest [`Self::render_layers_until`] did, for counters.
    pub fn last_layer_raster_pixels(&self) -> Option<(u64, u64)> {
        self.last_layer_work
            .as_ref()
            .map(|work| (work.raster_pixels(self.size), work.composite_pixels()))
    }

    fn run_layer_work(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        work: &super::strip_layers::LayerWork,
        policy: CompletionPolicy,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), BridgeError> {
        use super::strip_layers::{Layer, WorkspaceWork};
        let transparent = Color::TRANSPARENT;
        if work.backdrop {
            let base = self.base_color;
            self.render_layer(Layer::Backdrop, &cache.layers.backdrop, None, base, policy)?;
        }
        // The chrome's changed tiles, or all of it.
        let chrome_tiles = work
            .chrome_tiles
            .clone()
            .filter(|_| self.renderer.sparse_fine_supported());
        if work.chrome && chrome_tiles.as_ref().is_none_or(|tiles| !tiles.is_empty()) {
            self.render_layer(
                Layer::Chrome,
                &cache.layers.chrome,
                chrome_tiles,
                transparent,
                policy,
            )?;
        }
        if let (Some(_), Some(wheel)) = (work.wheel, cache.layers.wheel.as_ref()) {
            if let Some(layers) = self.strip_layers.as_mut() {
                layers.size_wheel(&self.device, (wheel.width, wheel.height));
            }
            self.render_layer(Layer::Wheel, &wheel.scene, None, transparent, policy)?;
        }
        let panned = || {
            let mut scene = Scene::new();
            super::strip_layers::append_panned(&mut scene, &cache.layers, &cache.strips);
            scene
        };
        match &work.workspace {
            WorkspaceWork::Unchanged => {}
            WorkspaceWork::Whole => {
                let scene = panned();
                self.render_layer(Layer::Workspace, &scene, None, transparent, policy)?;
            }
            WorkspaceWork::Pan { copies, uncovered } => {
                if !copies.is_empty() {
                    let mut encoder =
                        self.device
                            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                                label: Some("tilo-strip-layer-pan"),
                            });
                    if let Some(layers) = self.strip_layers.as_mut() {
                        layers.encode_copies(&self.device, &mut encoder, copies);
                    }
                    self.queue.submit([encoder.finish()]);
                }
                if !uncovered.is_empty() {
                    let scene = panned();
                    let regions = self
                        .renderer
                        .sparse_fine_supported()
                        .then(|| uncovered.clone());
                    self.render_layer(Layer::Workspace, &scene, regions, transparent, policy)?;
                }
            }
        }
        self.check_live(policy)?;
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("tilo-strip-layer-composite"),
            });
        if let (Some(layers), Some(rect)) = (self.strip_layers.as_ref(), work.composite) {
            layers.encode_composite(
                super::strip_layers::CompositeTarget {
                    device: &self.device,
                    queue: &self.queue,
                    encoder: &mut encoder,
                    output: &self.view,
                },
                cache.layers.wheel.as_ref(),
                rect,
            );
        }
        post_render(&self.device, &mut encoder, &self.view);
        let submission = self.queue.submit([encoder.finish()]);
        self.wait_submission(submission, policy)
    }

    /// Render `scene` into one layer's texture, through the target's own
    /// render path (its recovery from vello's budget overflow included):
    /// the layer's texture stands in for the target's for the render.
    fn render_layer(
        &mut self,
        layer: super::strip_layers::Layer,
        scene: &Scene,
        regions: Option<Vec<vello::FineDamageRect>>,
        base_color: Color,
        policy: CompletionPolicy,
    ) -> Result<(), BridgeError> {
        self.swap_layer(layer);
        let base_color = std::mem::replace(&mut self.base_color, base_color);
        // Each layer has the recovery attempts a frame has.
        let earlier_attempts = std::mem::take(&mut self.last_render_attempts);
        let rendered = self.render_attempts(
            scene,
            regions,
            FrameOutput::GpuOnly,
            policy,
            None,
            |_, _, _| {},
        );
        self.last_render_attempts = self.last_render_attempts.saturating_add(earlier_attempts);
        self.base_color = base_color;
        self.swap_layer(layer);
        rendered.map(|_| ())
    }

    /// Exchange the target's texture with `layer`'s, size included: the
    /// wheel's texture is the wheel's size, not the output's.
    fn swap_layer(&mut self, layer: super::strip_layers::Layer) {
        if let Some(layers) = self.strip_layers.as_mut() {
            let slot = layers.texture_mut(layer);
            std::mem::swap(&mut self.texture, &mut slot.texture);
            std::mem::swap(&mut self.view, &mut slot.view);
            std::mem::swap(&mut self.size, &mut slot.size);
        }
    }

    /// Completed output for read-only sampling, never a display presentation claim.
    pub fn completed_view(&self) -> Option<&wgpu::TextureView> {
        (self.retained_target_valid && !self.poisoned && self.loss.reason().is_none())
            .then_some(&self.view)
    }

    /// Current native pixel extent.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    fn reset_attempts(&mut self) {
        self.last_render_attempts = 0;
        self.last_sparse_render_attempts = 0;
        self.last_full_render_attempts = 0;
        self.last_canonical_segment_order_full_attempts = 0;
        self.last_canonical_segment_order_sparse_attempts = 0;
        self.last_area_ppt8_full_attempts = 0;
        self.last_area_ppt8_sparse_attempts = 0;
        self.last_clip_single_group_full_attempts = 0;
        self.last_clip_single_group_sparse_attempts = 0;
        self.last_sparse_render_stats = None;
        self.completion_markers = 0;
    }

    fn render<F>(
        &mut self,
        scene: &Scene,
        damage: Option<SceneDamage<'_>>,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        timing: Option<&mut BridgePhaseTiming>,
        post_render: F,
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError>
    where
        F: Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    {
        let sparse = damage.and_then(|damage| self.eligible_sparse_damage(damage));
        crate::shell::gpu_frames::apply(
            &mut self.renderer,
            self.present_generation,
            &mut self.client_frames,
        );
        self.invalidate();
        self.reset_attempts();
        self.check_live(policy)?;
        // Once encoding starts, even a pre-submit abort leaves renderer-owned downloads and
        // timestamp bookkeeping unusable. Clear poison only after both completion facts hold.
        let result = match Self::band_rows(self.size).filter(|_| sparse.is_none()) {
            Some(rows) => self.render_banded(scene, rows, output, policy, timing, post_render),
            None => self.render_attempts(scene, sparse, output, policy, timing, post_render),
        };
        if let Some(pass) = self.hdr_pass.as_ref() {
            // Later renders (and captures on this renderer) draw the images
            // as they are again.
            for substitution in &pass.substitutions {
                self.renderer.override_image(&substitution.image, None);
            }
        }
        if let Some(blur) = self.backdrop_blur.as_mut() {
            blur.release(&mut self.renderer);
        }
        match &result {
            Ok(_) => {
                #[cfg(target_os = "linux")]
                crate::shader_seed::after_frame();
                self.retained_target_valid = true;
                self.last_successful_revision = damage.map(|damage| damage.revision);
            }
            // Every attempt completed and settled its counters, so the renderer is consistent
            // and holds the budget those attempts grew. Rebuilding it would start the next
            // frame from the initial budget and overflow the same way forever.
            Err(BridgeError::RecoveryExhausted { .. }) => {}
            Err(_) => self.poisoned = true,
        }
        result
    }

    /// The height of the horizontal bands a target of `size` must be rendered in, or `None` when
    /// it renders whole.
    ///
    /// Vello bins the target into 256x256-pixel bins, and its binning and coarse stages keep one
    /// 256-entry bitmap per workgroup. A target with more bins than that is not refused: the
    /// indices past 255 are clamped onto other bins, and whatever falls there is drawn wrong or
    /// not at all. Two outputs side by side reach it easily -- 5120x2880 beside 1920x1080 is a
    /// 7040x2880 target of 336 bins -- and so does a single 6K or 8K panel.
    fn band_rows((width, height): (u32, u32)) -> Option<u32> {
        let bins_x = width.div_ceil(VELLO_BIN_PIXELS);
        let bins_y = height.div_ceil(VELLO_BIN_PIXELS);
        if bins_x.saturating_mul(bins_y) <= MAX_VELLO_BINS {
            return None;
        }
        // bins_x is at least 1 for any positive width; zero width never reaches banding.
        let bins_per_band = MAX_VELLO_BINS.checked_div(bins_x).unwrap_or(1).max(1);
        Some(bins_per_band.saturating_mul(VELLO_BIN_PIXELS))
    }

    fn band_target(&mut self, rows: u32) -> (wgpu::Texture, wgpu::TextureView) {
        if let Some((texture, view, band_rows)) = &self.band
            && *band_rows == rows
        {
            return (texture.clone(), view.clone());
        }
        let (texture, view) = Self::allocate_texture(&self.device, self.size.0, rows);
        self.band = Some((texture.clone(), view.clone(), rows));
        (texture, view)
    }

    /// Dense render of a target past [`MAX_VELLO_BINS`], one band of `rows` at a time.
    ///
    /// Each band renders the scene translated up by its offset into a band-sized texture, which
    /// is copied into place in the full target; the full target's output copy and `post_render`
    /// follow the last band, so callers see exactly what a whole render would produce. Each band
    /// has its own bounded recovery, and every one is a smaller viewport, so its bump buffers are
    /// smaller too.
    #[allow(
        clippy::too_many_lines,
        reason = "one ordered band/recovery transaction serves both output paths"
    )]
    fn render_banded(
        &mut self,
        scene: &Scene,
        rows: u32,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        mut timing: Option<&mut BridgePhaseTiming>,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError> {
        use vello::kurbo::Affine;

        let (width, height) = self.size;
        let (band_texture, band_view) = self.band_target(rows);
        // Every band draws the same substituted placeholders.
        self.prepare_backdrop(timing.as_deref_mut());
        let mut top = 0;
        while top < height {
            let band_height = rows.min(height.saturating_sub(top));
            let last = top.saturating_add(band_height) >= height;
            let mut band_scene = Scene::new();
            band_scene.append(scene, Some(Affine::translate((0.0, -f64::from(top)))));
            let mut attempts = 0;
            let mapped = loop {
                if attempts == RESILIENT_RENDER_ATTEMPTS {
                    return Err(BridgeError::RecoveryExhausted {
                        attempts: self.last_render_attempts,
                    });
                }
                attempts = attempts.saturating_add(1);
                self.check_live(policy)?;
                let submit_started = timing.as_ref().map(|_| Instant::now());
                let params = RenderParams {
                    base_color: self.base_color,
                    width,
                    height: band_height,
                    antialiasing_method: AaConfig::Area,
                };
                let loss = self.loss.clone();
                let before_submit =
                    || loss.reason().is_none() && Instant::now() < policy.deadline();
                let append = |encoder: &mut wgpu::CommandEncoder| {
                    encoder.copy_texture_to_texture(
                        band_texture.as_image_copy(),
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d { x: 0, y: top, z: 0 },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width,
                            height: band_height,
                            depth_or_array_layers: 1,
                        },
                    );
                    if last {
                        output.append(encoder, &self.texture, self.size, None);
                        post_render(&self.device, encoder, &self.view);
                    }
                };
                let submission = match self
                    .renderer
                    .render_to_texture_with_post_render_checked_indexed(
                        &self.device,
                        &self.queue,
                        &band_scene,
                        &band_view,
                        &params,
                        append,
                        before_submit,
                    ) {
                    Ok(submission) => submission,
                    Err(vello::Error::SubmissionAborted) => {
                        self.check_live(policy)?;
                        return Err(BridgeError::Render(
                            "submission declined after encoding".to_owned(),
                        ));
                    }
                    Err(error) => return Err(BridgeError::Render(format!("{error:?}"))),
                };
                self.last_render_attempts = self.last_render_attempts.saturating_add(1);
                self.last_full_render_attempts = self.last_full_render_attempts.saturating_add(1);
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), submit_started) {
                    timing.submit_us = timing
                        .submit_us
                        .saturating_add(started.elapsed().as_micros());
                    if last {
                        timing.readback_bytes = timing
                            .readback_bytes
                            .saturating_add(output.copy_bytes(self.size, None));
                    }
                }
                self.check_live(policy)?;
                let map_started = timing.as_ref().map(|_| Instant::now());
                let mapped = match output {
                    FrameOutput::Xrgb { buffer, .. } if last => {
                        Some(self.map_framebuffer(buffer, policy)?)
                    }
                    FrameOutput::GpuOnly if matches!(policy, CompletionPolicy::Absolute(_)) => {
                        self.wait_submission(submission.clone(), policy)?;
                        None
                    }
                    // Counter settlement below waits on this band's submission.
                    _ => None,
                };
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), map_started) {
                    timing.completed_readback_map_us = timing
                        .completed_readback_map_us
                        .saturating_add(started.elapsed().as_micros());
                }
                let settle_started = timing.as_ref().map(|_| Instant::now());
                let settled =
                    self.settle_counters(policy, Some(&submission))
                        .and_then(|overflowed| {
                            self.check_live(policy)?;
                            Ok(overflowed)
                        });
                if let (Some(timing), Some(started)) = (timing.as_deref_mut(), settle_started) {
                    timing.settle_us = timing
                        .settle_us
                        .saturating_add(started.elapsed().as_micros());
                }
                match settled {
                    Ok(false) => break mapped,
                    Ok(true) => {
                        if mapped.is_some() {
                            drop(mapped);
                            output.unmap();
                        }
                    }
                    Err(error) => {
                        if mapped.is_some() {
                            drop(mapped);
                            output.unmap();
                        }
                        return Err(error);
                    }
                }
            };
            if last {
                return Ok((mapped, false));
            }
            top = top.saturating_add(band_height);
        }
        Err(BridgeError::InvalidSize(format!("{width}x{height}")))
    }

    /// Render, blur and substitute the backdrops of the frame about to be
    /// rendered, when it has any.
    fn prepare_backdrop(&mut self, timing: Option<&mut BridgePhaseTiming>) {
        let Some(frame) = self.backdrop.as_deref().filter(|frame| !frame.is_empty()) else {
            return;
        };
        let stats = self
            .backdrop_blur
            .get_or_insert_with(|| ui::backdrop::BackdropBlur::new(&self.device))
            .prepare(
                &self.device,
                &self.queue,
                &mut self.renderer,
                frame,
                self.size,
                self.base_color,
            );
        if let Some(timing) = timing {
            timing.backdrop_us = timing
                .backdrop_us
                .saturating_add(u128::from(stats.encode_us));
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one ordered render/recovery transaction serves both output paths"
    )]
    fn render_attempts(
        &mut self,
        scene: &Scene,
        mut sparse: Option<Vec<vello::FineDamageRect>>,
        output: FrameOutput<'_>,
        policy: CompletionPolicy,
        mut timing: Option<&mut BridgePhaseTiming>,
        post_render: impl Fn(&wgpu::Device, &mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(Option<wgpu::BufferView>, bool), BridgeError> {
        // A full frame (a lock screen, an overlay's first paint) leaves
        // full-target scratch pooled; damage frames after it never reuse it.
        let full = sparse.is_none();
        if !full && self.last_frame_full {
            self.renderer.trim_pooled_buffers();
        }
        self.last_frame_full = full;
        while self.last_render_attempts < RESILIENT_RENDER_ATTEMPTS {
            self.check_live(policy)?;
            let regions = sparse.take();
            let submit_started = timing.as_ref().map(|_| Instant::now());
            let params = RenderParams {
                base_color: self.base_color,
                width: self.size.0,
                height: self.size.1,
                antialiasing_method: AaConfig::Area,
            };
            self.prepare_backdrop(timing.as_deref_mut());
            if let Some(pass) = self.hdr_pass.as_ref() {
                for substitution in &pass.substitutions {
                    self.renderer
                        .override_image(&substitution.image, Some(substitution.lit.clone()));
                }
                let second = self.renderer.render_to_texture(
                    &self.device,
                    &self.queue,
                    scene,
                    &pass.second_view,
                    &params,
                );
                for substitution in &pass.substitutions {
                    self.renderer
                        .override_image(&substitution.image, Some(substitution.black.clone()));
                }
                second.map_err(|error| {
                    BridgeError::Render(format!("HDR layer weights: {error:?}"))
                })?;
            }
            let loss = self.loss.clone();
            #[cfg(test)]
            let gate_now = self.completion_test.as_mut().map(|clock| {
                clock.advance(clock.encode_elapsed);
                clock.now()
            });
            let before_submit = || {
                #[cfg(test)]
                let now = gate_now.unwrap_or_else(Instant::now);
                #[cfg(not(test))]
                let now = Instant::now();
                loss.reason().is_none() && now < policy.deadline()
            };
            let append = |encoder: &mut wgpu::CommandEncoder| {
                output.append(encoder, &self.texture, self.size, regions.as_deref());
                post_render(&self.device, encoder, &self.view);
            };
            let submitted = if let Some(regions) = regions.as_deref() {
                self.renderer
                    .render_to_texture_with_damage_and_post_render_checked(
                        &self.device,
                        &self.queue,
                        scene,
                        &self.view,
                        &params,
                        regions,
                        append,
                        before_submit,
                    )
                    .map(|stats| (stats, None))
            } else {
                self.renderer
                    .render_to_texture_with_post_render_checked_indexed(
                        &self.device,
                        &self.queue,
                        scene,
                        &self.view,
                        &params,
                        append,
                        before_submit,
                    )
                    .map(|submission| (None, Some(submission)))
            };
            let (stats, submission_index) = match submitted {
                Ok(submitted) => submitted,
                Err(vello::Error::SubmissionAborted) => {
                    self.check_live(policy)?;
                    return Err(BridgeError::Render(
                        "submission declined after encoding".to_owned(),
                    ));
                }
                Err(error) => return Err(BridgeError::Render(format!("{error:?}"))),
            };
            if regions.is_some() && stats.is_none() {
                // Unsupported sparse rendering encoded/submitted nothing; dense still owns all four attempts.
                continue;
            }
            self.last_render_attempts = self.last_render_attempts.saturating_add(1);
            let admitted = regions.is_some();
            if let Some(stats) = stats {
                self.last_sparse_render_attempts =
                    self.last_sparse_render_attempts.saturating_add(1);
                VelloDrmBridge::record_sparse_shader_selected_attempts(
                    &stats,
                    Some(&mut self.last_canonical_segment_order_sparse_attempts),
                    Some(&mut self.last_area_ppt8_sparse_attempts),
                    Some(&mut self.last_clip_single_group_sparse_attempts),
                );
                self.last_sparse_render_stats = Some(stats);
            } else {
                self.last_full_render_attempts = self.last_full_render_attempts.saturating_add(1);
                self.last_canonical_segment_order_full_attempts = self
                    .last_canonical_segment_order_full_attempts
                    .saturating_add(usize::from(
                        self.renderer.last_canonical_segment_order_selected(),
                    ));
                self.last_area_ppt8_full_attempts = self
                    .last_area_ppt8_full_attempts
                    .saturating_add(usize::from(self.renderer.last_area_ppt8_selected()));
                self.last_clip_single_group_full_attempts = self
                    .last_clip_single_group_full_attempts
                    .saturating_add(usize::from(self.renderer.last_clip_single_group_selected()));
            }
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), submit_started) {
                timing.submit_us = timing
                    .submit_us
                    .saturating_add(started.elapsed().as_micros());
                timing.readback_bytes = timing
                    .readback_bytes
                    .saturating_add(output.copy_bytes(self.size, regions.as_deref()));
            }
            self.check_live(policy)?;
            let map_started = timing.as_ref().map(|_| Instant::now());
            let mapped = match output {
                FrameOutput::GpuOnly => {
                    // The sparse path does not report its submission; an
                    // empty submit queued behind it completes no earlier.
                    let submission = submission_index.clone().unwrap_or_else(|| {
                        self.queue.submit(std::iter::empty::<wgpu::CommandBuffer>())
                    });
                    self.wait_submission(submission, policy)?;
                    None
                }
                FrameOutput::Xrgb { buffer, .. } => Some(self.map_framebuffer(buffer, policy)?),
            };
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), map_started) {
                timing.completed_readback_map_us = timing
                    .completed_readback_map_us
                    .saturating_add(started.elapsed().as_micros());
            }
            let settle_started = timing.as_ref().map(|_| Instant::now());
            // Without the sparse path's index, settlement waits on the latest submission.
            let settled = self
                .settle_counters(policy, submission_index.as_ref())
                .and_then(|overflowed| {
                    self.check_live(policy)?;
                    Ok(overflowed)
                });
            if let (Some(timing), Some(started)) = (timing.as_deref_mut(), settle_started) {
                timing.settle_us = timing
                    .settle_us
                    .saturating_add(started.elapsed().as_micros());
            }
            match settled {
                Ok(false) => return Ok((mapped, admitted)),
                Ok(true) => {
                    drop(mapped);
                    output.unmap();
                }
                Err(error) => {
                    drop(mapped);
                    output.unmap();
                    return Err(error);
                }
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.last_render_attempts,
        })
    }

    fn wait_submission(
        &mut self,
        submission: wgpu::SubmissionIndex,
        policy: CompletionPolicy,
    ) -> Result<(), BridgeError> {
        self.check_live(policy)?;
        let CompletionPolicy::Absolute(deadline) = policy else {
            return Err(BridgeError::DevicePoll(
                "LinuxPerPhase cannot request a GPU-only marker".to_owned(),
            ));
        };
        self.completion_markers = self.completion_markers.saturating_add(1);
        let remaining = deadline.saturating_duration_since(self.now());
        if remaining.is_zero() {
            return Err(BridgeError::DeadlineExceeded);
        }
        #[cfg(test)]
        self.before_wait(WaitPhase::Marker, remaining)?;
        let waited = self.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(remaining),
        });
        // PollError itself cannot identify device loss. Prefer the context callback's
        // actual reason even when the same poll also reported a fence error.
        self.check_health()?;
        waited.map_err(|error| match error {
            wgpu::PollError::Timeout => BridgeError::DeadlineExceeded,
            other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                BridgeError::DevicePoll(format!("GPU-only submission: {other:?}"))
            }
        })?;
        self.check_live(policy)
    }

    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            reason = "tests inject elapsed time at the real framebuffer waits"
        )
    )]
    fn map_framebuffer(
        &mut self,
        buffer: &wgpu::Buffer,
        policy: CompletionPolicy,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let slice = buffer.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            let _ = tx.send(result);
        });
        #[cfg(test)]
        let rx = match self.completion_test.as_ref() {
            Some(test) if test.disconnect_map_callback => std::sync::mpsc::channel().1,
            _ => rx,
        };
        let result = (|| {
            let fence_wait = self.phase_wait(policy, LINUX_FENCE_WAIT);
            #[cfg(test)]
            self.before_wait(WaitPhase::Fence, fence_wait)?;
            let waited = self.device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(fence_wait),
            });
            self.check_health()?;
            waited.map_err(|error| match error {
                wgpu::PollError::Timeout => policy.timed_out(CompletionPhase::Fence),
                other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                    BridgeError::Map(format!("fence wait: {other:?}"))
                }
            })?;
            // A fresh callback allowance follows the independent fence allowance.
            let callback_wait = self.phase_wait(policy, LINUX_MAP_CALLBACK_WAIT);
            #[cfg(test)]
            self.before_wait(WaitPhase::MapCallback, callback_wait)?;
            rx.recv_timeout(callback_wait)
                .map_err(|error| match error {
                    std::sync::mpsc::RecvTimeoutError::Timeout => {
                        policy.timed_out(CompletionPhase::MapCallback)
                    }
                    // The callback can no longer report: wgpu dropped it, as a lost device does.
                    // Nothing is left to drain, so this fails closed instead of backing off.
                    std::sync::mpsc::RecvTimeoutError::Disconnected => {
                        BridgeError::Map("map_async callback dropped without reporting".to_owned())
                    }
                })?
                .map_err(|error| BridgeError::Map(format!("map_async: {error:?}")))?;
            self.check_health()?;
            Ok(slice.get_mapped_range())
        })();
        if result.is_err() {
            buffer.unmap();
        }
        result
    }

    /// Wait until Vello's bump counters for the frame just submitted are read back, and report
    /// whether it overflowed.
    ///
    /// Each round polls the counters once and, while they are pending, blocks on `submission`
    /// (the latest submission when `None`) for the rest of the phase allowance: the device
    /// wakes this thread when the work completes, rather than a sleep loop sampling it.
    fn settle_counters(
        &mut self,
        policy: CompletionPolicy,
        submission: Option<&wgpu::SubmissionIndex>,
    ) -> Result<bool, BridgeError> {
        let deadline = self
            .now()
            .checked_add(self.phase_wait(policy, BUMP_SETTLEMENT_WAIT))
            .ok_or_else(|| BridgeError::BumpReadback("counter deadline out of range".to_owned()))?;
        for _ in 0..COUNTER_SETTLEMENT_ROUNDS {
            self.check_health()?;
            if self.now() >= deadline {
                tracing::warn!(
                    ?policy,
                    phase = "counter settlement",
                    "completion deadline exceeded"
                );
                return Err(policy.timed_out(CompletionPhase::CounterSettlement));
            }
            let result = self.renderer.poll_bump_budget(&self.device);
            #[cfg(test)]
            let result = if let Some(test) = self.completion_test.as_mut() {
                test.counter_remaining
                    .push(deadline.saturating_duration_since(test.now()));
                test.counters.pop_front().unwrap_or(result)
            } else {
                result
            };
            self.check_health()?;
            if self.now() >= deadline {
                continue;
            }
            match result.map_err(|error| match error {
                vello::BumpBudgetError::Poll(message) => {
                    BridgeError::DevicePoll(format!("{policy:?} counter settlement: {message}"))
                }
                other => {
                    BridgeError::BumpReadback(format!("{policy:?} counter settlement: {other:?}"))
                }
            })? {
                vello::BumpBudgetPoll::Complete { overflowed } => return Ok(overflowed),
                vello::BumpBudgetPoll::Pending => {
                    self.wait_counters(policy, submission, deadline)?;
                }
            }
        }
        self.check_health()?;
        if self.now() >= deadline {
            return Err(policy.timed_out(CompletionPhase::CounterSettlement));
        }
        // Every round's wait completed yet the readback stayed pending: nothing is left in
        // flight to drain, so this is not a timeout.
        Err(BridgeError::BumpReadback(format!(
            "counter readback still pending after {COUNTER_SETTLEMENT_ROUNDS} completed waits"
        )))
    }

    /// Block until `submission` completes (wgpu runs its map callbacks, the counters' among
    /// them, before returning) or the settlement `deadline` passes.
    #[cfg_attr(
        not(test),
        allow(
            clippy::needless_pass_by_ref_mut,
            reason = "tests inject elapsed time at the real counter wait"
        )
    )]
    fn wait_counters(
        &mut self,
        policy: CompletionPolicy,
        submission: Option<&wgpu::SubmissionIndex>,
        deadline: Instant,
    ) -> Result<(), BridgeError> {
        let remaining = deadline.saturating_duration_since(self.now());
        if remaining.is_zero() {
            return Ok(());
        }
        #[cfg(test)]
        if let Some(test) = self.completion_test.as_mut() {
            test.counter_waits.push(remaining);
            test.advance(test.pending_elapsed.min(remaining));
        }
        let waited = self.device.poll(wgpu::PollType::Wait {
            submission_index: submission.cloned(),
            timeout: Some(remaining),
        });
        self.check_health()?;
        waited.map_err(|error| match error {
            wgpu::PollError::Timeout => policy.timed_out(CompletionPhase::CounterSettlement),
            other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                BridgeError::DevicePoll(format!("{policy:?} counter settlement: {other:?}"))
            }
        })?;
        Ok(())
    }

    #[cfg(test)]
    fn before_wait(&mut self, phase: WaitPhase, requested: Duration) -> Result<(), BridgeError> {
        #[cfg(test)]
        if let Some(test) = self.completion_test.as_mut() {
            test.waits.push((phase, requested));
            if phase == WaitPhase::Marker && test.destroy_at_marker {
                self.device.destroy();
                let _ = self.device.poll(wgpu::PollType::Poll);
            }
            if test.failure == Some(phase) {
                return Err(match phase {
                    WaitPhase::Marker => BridgeError::DeadlineExceeded,
                    WaitPhase::Fence => BridgeError::CompletionTimeout {
                        phase: CompletionPhase::Fence,
                    },
                    WaitPhase::MapCallback => BridgeError::CompletionTimeout {
                        phase: CompletionPhase::MapCallback,
                    },
                });
            }
            test.advance(
                match phase {
                    WaitPhase::Fence | WaitPhase::Marker => test.fence_elapsed,
                    WaitPhase::MapCallback => test.callback_elapsed,
                }
                .min(requested),
            );
        }
        Ok(())
    }

    /// The Vello renderer underneath, for its feature and diagnostic
    /// queries.
    #[doc(hidden)]
    pub fn vello(&self) -> &Renderer {
        &self.renderer
    }

    /// The Vello renderer underneath, for the queries that take samples.
    #[doc(hidden)]
    pub fn vello_mut(&mut self) -> &mut Renderer {
        &mut self.renderer
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_render_attempts(&self) -> usize {
        self.last_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_sparse_render_attempts(&self) -> usize {
        self.last_sparse_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_full_render_attempts(&self) -> usize {
        self.last_full_render_attempts
    }

    /// Actual statistics from the latest normal render transaction.
    pub fn last_sparse_render_stats(&self) -> Option<vello::SparseRenderStats> {
        self.last_sparse_render_stats
    }

    /// Adapter identity for the actual same-device renderer.
    pub fn adapter_info(&self) -> BridgeAdapterInfo {
        let info = self.device.adapter_info();
        BridgeAdapterInfo {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            #[cfg(target_os = "linux")]
            drm_device: ui::dmabuf_import::vulkan_drm_device_identity(&self.device),
        }
    }

    fn allocate_texture(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-desktop-drm-paint-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }
}

/// Opt-in diagnostic coverage, independent of input or presentation causality.
#[derive(Default)]
struct DispatchStageObserver {
    attempts: u64,
    matched_current: u64,
    stale_samples: u64,
    missing_current: u64,
    last_record: Option<serde_json::Value>,
}

impl DispatchStageObserver {
    #[expect(
        clippy::print_stderr,
        reason = "explicit GPU diagnostics must reach the native compositor process log"
    )]
    fn record(
        &mut self,
        target: &mut RetainedGpuTarget,
        revision: u64,
        rendered: bool,
        previous_submission: Option<u64>,
        renderer_generation: Option<u64>,
    ) {
        self.attempts = self.attempts.saturating_add(1);
        let latest = target
            .renderer
            .gpu_dispatch_timestamp_latest_submission_id();
        let sample = target.renderer.take_gpu_dispatch_timestamp_sample();
        let sample_id = sample.as_ref().map(|sample| sample.submission_id);
        let expected = latest.filter(|_| latest != previous_submission && rendered);
        let matched = sample_id.is_some() && sample_id == expected;
        self.matched_current = self.matched_current.saturating_add(u64::from(matched));
        self.stale_samples = self
            .stale_samples
            .saturating_add(u64::from(sample_id.is_some() && !matched));
        self.missing_current = self.missing_current.saturating_add(u64::from(!matched));
        let stages: Vec<_> = sample.as_ref().map(|sample| {
            sample.stages.iter().take(usize::try_from(sample.stage_count).unwrap_or(0).min(sample.stages.len()))
                .map(|stage| serde_json::json!({
                    "stage_id": stage.stage_id,
                    "stage": vello::gpu_dispatch_timestamp_stage_name(stage.stage_id).unwrap_or("unknown"),
                    "dispatch_count": stage.dispatch_count,
                    "duration_ns": stage.duration_ns.to_string(),
                })).collect()
        }).unwrap_or_default();
        let row = serde_json::json!({
            "process_id": std::process::id(), "renderer_generation": renderer_generation,
            "revision": revision, "render_succeeded": rendered,
            "latest_attempted_submission_id": latest, "expected_current_submission_id": expected,
            "sample_submission_id": sample_id,
            "matched_current": matched, "missing_current": !matched,
            "stale_sample": sample_id.is_some() && !matched,
            "status": format!("{:?}", target.renderer.gpu_dispatch_timestamp_status()),
            "timestamp_buffer_bytes": target.renderer.gpu_dispatch_timestamp_buffer_bytes(),
            "coverage": {"attempts": self.attempts, "matched_current": self.matched_current,
                "stale_samples": self.stale_samples, "missing_current": self.missing_current},
            "stages": stages,
        });
        eprintln!("VELLO-STAGES {row}");
        self.last_record = Some(row);
    }
}

/// The presenting renderer's wgpu context and the index of its device.
///
/// Every backend but OpenGL first: a hardware adapter there needs no OpenGL, whose EGL
/// driver otherwise stays loaded and initialised for the whole session — on a machine
/// without a GPU, Mesa's llvmpipe beside lavapipe with a second rasteriser thread pool
/// (`ui::gpu`'s own choice keeps OpenGL for a process that sees a render node and has no
/// pinned Vulkan driver, which the compositor is). Only when those backends offer no
/// hardware adapter and OpenGL does is the context's own instance used, for a GPU only
/// OpenGL drives, exactly as before. `WGPU_BACKEND` still decides alone.
fn open_render_context() -> Option<(RenderContext, usize)> {
    let mut context = RenderContext::new();
    if wgpu::Backends::from_env().is_some() {
        let id = context.device(None).block_on()?;
        return Some((context, id));
    }
    let hardware =
        |adapter: &wgpu::Adapter| adapter.get_info().device_type != wgpu::DeviceType::Cpu;
    let own = std::mem::replace(
        &mut context.instance,
        wgpu::Instance::new(wgpu::InstanceDescriptor {
            display: None,
            backends: wgpu::Backends::all().difference(wgpu::Backends::GL),
            flags: wgpu::InstanceFlags::from_build_config().with_env(),
            memory_budget_thresholds: wgpu::MemoryBudgetThresholds::default(),
            backend_options: wgpu::BackendOptions::from_env_or_default(),
        }),
    );
    let found = context.device(None).block_on();
    let found_hardware = found
        .and_then(|id| context.devices.get(id))
        .is_some_and(|handle| hardware(&handle.adapter));
    if let Some(id) = found
        && (found_hardware
            || !own
                .enumerate_adapters(wgpu::Backends::all())
                .block_on()
                .iter()
                .any(hardware))
    {
        return Some((context, id));
    }
    // No adapter without OpenGL, or a GPU only the context's own backends drive: open it
    // there instead, as before.
    context.devices.clear();
    context.instance = own;
    let id = context.device(None).block_on()?;
    Some((context, id))
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
    /// closed windows freed (TIL-731) for as long as this bridge lives.
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

impl VelloDrmBridge {
    /// Build a bridge sized at `width × height`.
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0` — DRM connector
    /// enumeration is expected to guarantee non-zero modes, but a driver could still hand back a
    /// degenerate one, and a panic here would take down the whole compositor session (`shell`
    /// runs with `panic = "abort"`). Returns [`BridgeError::NoAdapter`] if no wgpu adapter is
    /// available, [`BridgeError::InvalidDevice`] if the freshly acquired device id is out of
    /// range (defensive; cannot happen in practice), or [`BridgeError::Renderer`] if the vello
    /// renderer fails to build.
    pub fn new(width: u32, height: u32) -> Result<Self, BridgeError> {
        if width == 0 || height == 0 {
            return Err(BridgeError::InvalidSize(format!(
                "VelloDrmBridge::new: width/height must be > 0, got {width}x{height}"
            )));
        }

        let xrgb_history_generation = Self::next_xrgb_history_generation();
        let (context, dev_id) = open_render_context().ok_or_else(|| {
            BridgeError::NoAdapter(
                "no wgpu adapter (install vulkan/llvmpipe drivers in the runtime)".to_owned(),
            )
        })?;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let adapter_info = dev_handle.adapter.get_info();
        tracing::info!(
            adapter = %adapter_info.name,
            backend = ?adapter_info.backend,
            device_type = ?adapter_info.device_type,
            driver = %adapter_info.driver,
            vendor = format_args!("{:#06x}", adapter_info.vendor),
            device = format_args!("{:#06x}", adapter_info.device),
            pci_bus_id = %adapter_info.device_pci_bus_id,
            "DRM CPU-fallback wgpu adapter selected"
        );

        let device_errors = DeviceErrorLatch::install(device);
        let mut target = RetainedGpuTarget::new(dev_handle, width, height)?;
        // Client buffers are imported where the scene is drawn, so their
        // frames reach the panel without crossing the CPU.
        target.present_generation = Some(crate::shell::gpu_frames::publish_present_device(
            device,
            &dev_handle.queue,
        ));
        let bgra_blitter = TextureBlitter::new(device, wgpu::TextureFormat::Bgra8Unorm);
        let (readback_buf, padded_bytes_per_row) = Self::allocate_staging(device, width, height);
        let device_memory = crate::shell::heap_trim::register_device(&adapter_info, device);

        Ok(Self {
            context,
            target,
            device_errors,
            bgra_blitter,
            dispatch_stage_observer: std::env::var_os("TILO_PROFILE_GPU_STAGES")
                .is_some_and(|value| value == "1")
                .then(DispatchStageObserver::default),
            hdr_encoders: Vec::new(),
            oriented_copy: None,
            hdr_compositor: None,
            dev_id,

            readback_buf,
            padded_bytes_per_row,
            readback: Vec::new(),

            phase_timing_enabled: false,
            last_phase_timing: None,

            retained_staging_valid: false,
            staging_content: None,
            first_frame_pending: true,

            xrgb_history_generation,
            xrgb_history_epoch: Some(1),
            recovery_not_before: None,
            recovery_backoff: RECOVERY_BACKOFF_INITIAL,
            output_surfaces: Vec::new(),
            _device_memory: device_memory,
            logged_memory: None,
            windows_closed_seen: crate::shell::heap_trim::windows_closed(),
            scratch_shrinks_seen: 0,
            #[cfg(test)]
            fail_next_tracked_swizzle: false,
        })
    }

    /// Draw the floating surfaces of the frames rendered from now on over
    /// `backdrop` (see [`crate::compositor::frame::FramePaint::backdrop`]).
    /// `None` draws them opaque.
    pub fn set_backdrop(&mut self, backdrop: Option<Arc<ui::backdrop::BackdropFrame>>) {
        self.target.backdrop = backdrop;
    }

    /// `true` exactly once: on the first call after this bridge was built, which its owner
    /// makes once the bridge's first frame was painted. The pipelines' compile scratch is
    /// free by then.
    pub fn take_first_frame(&mut self) -> bool {
        std::mem::take(&mut self.first_frame_pending)
    }

    /// Whether [`Self::paint`] has stored a complete tight RGBA frame.
    pub fn has_complete_readback(&self) -> bool {
        let (width, height) = self.target.size;
        let needed = usize::try_from(width)
            .unwrap_or(0)
            .saturating_mul(usize::try_from(height).unwrap_or(0))
            .saturating_mul(4);
        needed > 0 && self.readback.len() == needed
    }

    /// The tight RGBA of the last complete frame, when one is encoded.
    pub(crate) fn encoded_rgba(&self) -> Option<&[u8]> {
        self.has_complete_readback()
            .then_some(self.readback.as_slice())
    }

    /// Bring the encoded desktop up to `scene` inside `regions`: one render
    /// of the scene repairing only those regions (sparse when the target
    /// retains the previous one), then a readback of each region alone into
    /// the tight image. The combined framebuffer is not mapped; pixels
    /// outside `regions` stay as the previous frame encoded them.
    ///
    /// # Errors
    /// The desktop image is missing, `regions` is empty or one leaves it, or a
    /// render or region copy fails. The tight image is then partly stale:
    /// the caller must present a full frame instead.
    pub(crate) fn refresh_encoded_regions(
        &mut self,
        scene: &Scene,
        regions: &[vello::FineDamageRect],
    ) -> Result<(), BridgeError> {
        // A stale full-frame profile must not be reported for these regions.
        self.last_phase_timing = None;
        let started = Instant::now();
        self.render_encoded_regions(scene, regions)?;
        let rendered = Instant::now();
        let mut bytes = 0_u64;
        let mut patch_us = 0_u128;
        for &clip in regions {
            let tight = self.read_encoded_clip(clip)?;
            let patch_started = Instant::now();
            self.patch_encoded_clip(clip, &tight)?;
            patch_us = patch_us.saturating_add(patch_started.elapsed().as_micros());
            bytes = bytes.saturating_add(u64::try_from(tight.len()).unwrap_or(u64::MAX));
        }
        if self.phase_timing_enabled {
            self.last_phase_timing = Some(BridgePhaseTiming {
                submit_us: rendered.saturating_duration_since(started).as_micros(),
                completed_readback_map_us: rendered.elapsed().as_micros().saturating_sub(patch_us),
                cpu_copy_swizzle_us: patch_us,
                readback_bytes: bytes,
                cpu_output_bytes: bytes,
                ..BridgePhaseTiming::default()
            });
        }
        Ok(())
    }

    fn render_encoded_regions(
        &mut self,
        scene: &Scene,
        regions: &[vello::FineDamageRect],
    ) -> Result<(), BridgeError> {
        if !self.has_complete_readback() {
            return Err(BridgeError::InvalidSize(
                "no encoded desktop for the regions".to_owned(),
            ));
        }
        let (width, height) = self.target.size;
        let inside = |clip: &vello::FineDamageRect| {
            clip.width > 0
                && clip.height > 0
                && clip.x.checked_add(clip.width).is_some_and(|x1| x1 <= width)
                && clip
                    .y
                    .checked_add(clip.height)
                    .is_some_and(|y1| y1 <= height)
        };
        if regions.is_empty() || !regions.iter().all(inside) {
            return Err(BridgeError::InvalidSize(
                "a region is outside the encoded desktop".to_owned(),
            ));
        }
        let revision = self
            .target
            .last_successful_revision
            .map_or(1, |revision| revision.saturating_add(1));
        let previous = self.target.last_successful_revision;
        let damage = SceneDamage {
            revision,
            previous_revision: previous,
            regions: Some(regions),
        };
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .unwrap_or_else(Instant::now);
        self.target.render_until(scene, damage, deadline)?;
        Ok(())
    }

    fn read_encoded_clip(&self, clip: vello::FineDamageRect) -> Result<Vec<u8>, BridgeError> {
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let tight_row = clip.width.saturating_mul(4);
        let padded = tight_row.div_ceil(align).saturating_mul(align);
        let size = u64::from(padded).saturating_mul(u64::from(clip.height));
        if size == 0 {
            return Err(BridgeError::InvalidSize("empty clip readback".to_owned()));
        }
        let buffer = self.target.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-desktop-drm-clip-readback"),
            size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder =
            self.target
                .device
                .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("tilo-desktop-drm-clip-copy"),
                });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.target.texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: clip.x,
                    y: clip.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded),
                    rows_per_image: Some(clip.height),
                },
            },
            wgpu::Extent3d {
                width: clip.width,
                height: clip.height,
                depth_or_array_layers: 1,
            },
        );
        let submission = self.target.queue.submit(Some(encoder.finish()));
        self.target
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: Some(submission),
                timeout: Some(Duration::from_secs(1)),
            })
            .map_err(|error| BridgeError::Map(format!("clip copy: {error:?}")))?;
        let tight = {
            let mapped = Self::map_device_readback(&self.target.device, &buffer)?;
            tight_clip_rows(&mapped, padded, clip)
        };
        buffer.unmap();
        tight
    }

    fn patch_encoded_clip(
        &mut self,
        clip: vello::FineDamageRect,
        tight: &[u8],
    ) -> Result<(), BridgeError> {
        let scene_w = usize::try_from(self.target.size.0).unwrap_or(0);
        let row = usize::try_from(clip.width).unwrap_or(0).saturating_mul(4);
        let origin_x = usize::try_from(clip.x).unwrap_or(0);
        let origin_y = usize::try_from(clip.y).unwrap_or(0);
        let rows = usize::try_from(clip.height).unwrap_or(0);
        if scene_w == 0 || row == 0 || rows == 0 {
            return Err(BridgeError::InvalidSize(
                "encoded clip does not fit the desktop".to_owned(),
            ));
        }
        for row_index in 0..rows {
            let src_off = row_index.saturating_mul(row);
            let Some(src) = tight.get(src_off..src_off.saturating_add(row)) else {
                break;
            };
            let dest_off = origin_y
                .saturating_add(row_index)
                .saturating_mul(scene_w)
                .saturating_add(origin_x)
                .saturating_mul(4);
            let Some(dest) = self
                .readback
                .get_mut(dest_off..dest_off.saturating_add(row))
            else {
                break;
            };
            dest.copy_from_slice(src);
        }
        Ok(())
    }

    /// Override the per-frame clear color; matches the winit backend API so callers can swap themes uniformly.
    pub fn set_base_color(&mut self, color: Color) {
        if self.target.base_color == color {
            return;
        }
        self.target.base_color = color;
        // Sparse rendering never clears pixels outside its rectangles. A new clear color is a
        // full-frame change even if the scene recording itself is unchanged.
        self.invalidate_retained();
        self.invalidate_output_surfaces();
        self.bump_xrgb_history_epoch();
    }

    /// The device and queue this bridge renders with, for targets it renders
    /// into that were not imported from a dma-buf.
    pub fn gpu(&self) -> (&wgpu::Device, &wgpu::Queue) {
        (&self.target.device, &self.target.queue)
    }

    #[cfg(test)]
    pub(crate) fn present_generation_for_test(&self) -> Option<u64> {
        self.target.present_generation
    }

    /// Import a compositor-owned linear scanout dma-buf (`XRGB8888`, or `XBGR2101010` /
    /// `ABGR16161616F` for HDR) on this bridge's exact wgpu device.
    ///
    /// Keeping import here prevents a target created by another device from reaching the render
    /// callback. Retain the returned alias while the scanout allocation remains alive.
    ///
    /// # Errors
    ///
    /// Fails when the device cannot import the dma-buf as a render target.
    #[cfg(target_os = "linux")]
    pub fn import_scanout_target(
        &self,
        image: &ui::dmabuf_import::DmabufRef<'_>,
    ) -> Result<ui::dmabuf_import::ImportedRenderTarget, BridgeError> {
        ui::dmabuf_import::import_render_target(&self.target.device, image)
            .map_err(|error| BridgeError::Import(error.to_string()))
    }

    /// Render into an imported direct-scanout target in one submission, then wait for the
    /// existing completion marker before returning. An SDR (`Bgra8Unorm`) target receives a
    /// plain copy; an HDR target receives the PQ BT.2020 encode with SDR white at
    /// `sdr_white_nits`, compositing `hdr_layers` at full precision in linear light.
    ///
    /// The one-output case of [`Self::paint_output_until`]: the scene is the output, so the
    /// bridge's own target is the output's retained surface.
    ///
    /// # Errors
    ///
    /// Fails when the target size does not match this bridge, rendering or
    /// submission fails, or the completion marker misses `deadline`.
    #[cfg(target_os = "linux")]
    pub fn paint_imported_until(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        target: &ui::dmabuf_import::ImportedRenderTarget,
        deadline: Instant,
        sdr_white_nits: f32,
        hdr_layers: &[HdrLayer],
    ) -> Result<GpuFrameStats, BridgeError> {
        self.paint_final_pass(
            scene,
            damage,
            OutputTarget::imported(target),
            deadline,
            OutputSignal {
                sdr_white_nits,
                hdr_layers,
                color_lut: None,
                layers: None,
            },
        )
    }

    /// The PQ encode pipeline for `format` exists and encodes SDR white at `sdr_white_nits`
    /// into a target turned by `orientation`.
    fn prepare_hdr_encoder(
        &mut self,
        format: wgpu::TextureFormat,
        sdr_white_nits: f32,
        orientation: OutputTransform,
    ) {
        if !self
            .hdr_encoders
            .iter()
            .any(|pipeline| pipeline.format() == format)
        {
            self.hdr_encoders
                .push(HdrEncodePipeline::new(&self.target.device, format));
        }
        if let Some(pipeline) = self
            .hdr_encoders
            .iter()
            .find(|pipeline| pipeline.format() == format)
        {
            pipeline.set_params(&self.target.queue, sdr_white_nits, orientation);
        }
    }

    /// The final pass `output` needs exists and is set up for the next submission: the PQ
    /// encode for an HDR target, the oriented copy for an SDR one that is rotated or
    /// calibrated (`color_lut`). Returns whether the oriented copy runs; an unrotated,
    /// uncalibrated SDR target keeps the plain blit.
    fn prepare_final_pass(
        &mut self,
        output: OutputTarget<'_>,
        hdr: bool,
        sdr_white_nits: f32,
        color_lut: Option<&IccLut>,
    ) -> bool {
        if hdr {
            self.prepare_hdr_encoder(output.format, sdr_white_nits, output.orientation);
            return false;
        }
        if output.orientation == OutputTransform::Normal && color_lut.is_none() {
            return false;
        }
        let (device, queue) = (&self.target.device, &self.target.queue);
        self.oriented_copy
            .get_or_insert_with(|| OrientedCopy::new(device))
            .prepare(device, queue, output.orientation, color_lut);
        true
    }

    /// Render `scene` into the target currently swapped into `self.target`, then run the
    /// final pass into `output`: a copy for `Bgra8Unorm`, the PQ encode (and the HDR
    /// composite when there are layers) otherwise, each turned by `output.orientation`.
    /// Waits for the completion marker.
    fn paint_final_pass(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        // A strip pan is composited from the retained layers, which hold the
        // frame's SDR pixels upright, as a render of its scene would: the
        // same final pass then turns, calibrates or PQ-encodes them. HDR
        // surfaces need the scene's full-precision composite.
        if let Some(cache) = signal
            .layers
            .filter(|cache| signal.hdr_layers.is_empty() && self.target.layers_eligible(cache))
        {
            return self.paint_layers_final_pass(cache, damage.revision, output, deadline, signal);
        }
        self.paint_scene_final_pass(scene, damage, output, deadline, signal)
    }

    /// [`Self::paint_final_pass`] for a frame that came as layers: the
    /// target composites it ([`RetainedGpuTarget::render_layers_until`]),
    /// then the output's final pass (a copy, turned and calibrated when the
    /// output is, or the PQ encode of an HDR output) writes `output`.
    fn paint_layers_final_pass(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        revision: u64,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        self.last_phase_timing = None;
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        output.check_shows(self.target.size)?;
        let hdr = output.format != wgpu::TextureFormat::Bgra8Unorm;
        // The profile describes the panel's SDR response; the PQ encode replaces it.
        let color_lut = signal.color_lut.filter(|_| !hdr);
        let copies = self.prepare_final_pass(output, hdr, signal.sdr_white_nits, color_lut);
        let encode = self
            .hdr_encoders
            .iter()
            .find(|pipeline| hdr && pipeline.format() == output.format);
        let oriented = self.oriented_copy.as_ref().filter(|_| copies);
        let blitter = &self.bgra_blitter;
        let view = output.view;
        let rendered = self.target.render_layers_until(
            cache,
            revision,
            deadline,
            |device, encoder, source| match (encode, oriented) {
                (Some(pipeline), _) => pipeline.encode(device, encoder, source, view),
                (None, Some(copy)) => copy.copy(device, encoder, source, view, color_lut),
                (None, None) => blitter.copy(device, encoder, source, view),
            },
        );
        rendered.and_then(|stats| self.take_device_error().map_or(Ok(stats), Err))
    }

    /// [`Self::paint_final_pass`] for a frame rendered from its scene.
    fn paint_scene_final_pass(
        &mut self,
        scene: &Scene,
        damage: SceneDamage<'_>,
        output: OutputTarget<'_>,
        deadline: Instant,
        signal: OutputSignal<'_>,
    ) -> Result<GpuFrameStats, BridgeError> {
        let OutputSignal {
            sdr_white_nits,
            hdr_layers,
            ..
        } = signal;
        self.last_phase_timing = None;
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        if self.target.poisoned {
            // A failed direct frame must not leave the next attempt with a
            // target that refuses to render.
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        output.check_shows(self.target.size)?;
        // Frames this output skipped while its flip was pending left their damage owed: the
        // retained pixels already claim those revisions, so this render repairs it too.
        let repaired = self.target.take_owed_damage(damage.regions);
        let damage = damage.with_regions(repaired.as_deref());
        let hdr = output.format != wgpu::TextureFormat::Bgra8Unorm;
        let composite = hdr && !hdr_layers.is_empty();
        if composite {
            let (device, queue) = (&self.target.device, &self.target.queue);
            let compositor = self
                .hdr_compositor
                .get_or_insert_with(|| HdrCompositor::new(device, queue));
            self.target.hdr_pass = Some(compositor.prepare(
                self.target.size,
                hdr_layers,
                Some((output.format, sdr_white_nits)),
            ));
            compositor.set_orientation(output.orientation);
        }
        // The substituted layer images are not what the retained target holds
        // from an earlier frame, so a composited frame renders densely.
        let damage = if composite {
            SceneDamage {
                revision: damage.revision,
                previous_revision: None,
                regions: None,
            }
        } else {
            damage
        };
        // The profile describes the panel's SDR response; the PQ encode replaces it.
        let color_lut = signal.color_lut.filter(|_| !hdr);
        let copies = self.prepare_final_pass(output, hdr, sdr_white_nits, color_lut);
        let encode = self
            .hdr_encoders
            .iter()
            .find(|pipeline| hdr && pipeline.format() == output.format);
        let oriented = self.oriented_copy.as_ref().filter(|_| copies);
        let blitter = &self.bgra_blitter;
        let view = output.view;
        let format = output.format;
        let compositor = self.hdr_compositor.as_ref().filter(|_| composite);
        let previous_dispatch_submission = self.dispatch_stage_observer.as_ref().and_then(|_| {
            self.target
                .renderer
                .gpu_dispatch_timestamp_latest_submission_id()
        });
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        let rendered = self.target.render(
            scene,
            Some(damage),
            FrameOutput::GpuOnly,
            CompletionPolicy::Absolute(deadline),
            timing.as_mut(),
            |device, encoder, source| match (compositor, encode, oriented) {
                (Some(compositor), _, _) => compositor.encode(encoder, source, view, format),
                (None, Some(pipeline), _) => pipeline.encode(device, encoder, source, view),
                (None, None, Some(copy)) => copy.copy(device, encoder, source, view, color_lut),
                (None, None, None) => blitter.copy(device, encoder, source, view),
            },
        );
        self.target.hdr_pass = None;
        if composite {
            // The retained frame holds the layers black: no later frame may
            // repair it sparsely, whether or not it composites.
            self.target.last_successful_revision = None;
        }
        let rendered = rendered.and_then(|output| self.take_device_error().map_or(Ok(output), Err));
        if let Some(observer) = &mut self.dispatch_stage_observer {
            observer.record(
                &mut self.target,
                damage.revision,
                rendered.is_ok(),
                previous_dispatch_submission,
                self.xrgb_history_generation,
            );
        }
        self.last_phase_timing = rendered.as_ref().ok().and(timing);
        let (_, admitted) = rendered?;
        Ok(GpuFrameStats {
            revision: damage.revision,
            damage_admitted: admitted,
            render_attempts: self.target.last_render_attempts,
            sparse_attempts: self.target.last_sparse_render_attempts,
            full_attempts: self.target.last_full_render_attempts,
            completion_markers: self.target.completion_markers,
            framebuffer_copy_bytes: 0,
            framebuffer_staging_bytes: 0,
        })
    }

    fn next_xrgb_history_generation() -> Option<u64> {
        NEXT_XRGB_HISTORY_GENERATION
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |generation| {
                generation.checked_add(1)
            })
            .ok()
    }

    fn bump_xrgb_history_epoch(&mut self) {
        self.xrgb_history_epoch = self
            .xrgb_history_epoch
            .and_then(|epoch| epoch.checked_add(1));
    }

    /// Wait for each Linux completion phase until the GPU actually finishes, bounded only by a
    /// generous backstop, instead of the production one-second allowance.
    ///
    /// On a shared CI host llvmpipe competes with every other test for CPU, so a frame whose
    /// pixels a fixture checks can legitimately miss the per-phase budget. Fixtures that assert
    /// what is rendered, not how fast, opt in here; device loss still fails immediately.
    #[cfg(test)]
    pub(crate) fn wait_for_gpu_completion_for_test(&mut self) {
        self.target.phase_wait_override = Some(tilo_test_env::hang_bound());
    }

    /// Compile the renderer's full and sparse pipelines outside any frame deadline, then start
    /// over from an invalidated target and a new history epoch.
    ///
    /// llvmpipe builds compute variants at first dispatch. On a cold shader cache that takes
    /// longer than the Linux per-phase fence allowance, so a fixture asserting on
    /// deadline-bound frames warms the renderer first instead of timing the compiler.
    #[cfg(test)]
    pub(crate) fn warm_renderer_for_test(&mut self) {
        use vello::kurbo::{Affine, Rect};
        use vello::peniko::Fill;

        let (width, height) = self.target.size;
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0x80, 0x80, 0x80),
            None,
            &Rect::new(0.0, 0.0, f64::from(width), f64::from(height)),
        );
        let deadline = tilo_test_env::hang_deadline();
        self.target
            .render_until(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: None,
                },
                deadline,
            )
            .expect("warm-up full render");
        let region = [vello::FineDamageRect {
            x: 0,
            y: 0,
            width: width.min(16),
            height: height.min(16),
        }];
        self.target
            .render_until(
                &scene,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&region),
                },
                deadline,
            )
            .expect("warm-up sparse render");
        self.invalidate_retained();
        self.bump_xrgb_history_epoch();
    }

    /// Refuse to submit another frame while a timed-out software-rendered submission drains.
    /// Returning before `render_normal` is called leaves the caller's dumb buffer untouched, so
    /// KMS continues showing the last complete frame instead of exposing a half-painted one.
    fn check_recovery_backoff(&mut self) -> Result<(), BridgeError> {
        let Some(not_before) = self.recovery_not_before else {
            return Ok(());
        };
        let now = Instant::now();
        if now < not_before {
            let retry_in_ms = not_before
                .saturating_duration_since(now)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            return Err(BridgeError::RecoveryBackoff { retry_in_ms });
        }
        self.recovery_not_before = None;
        self.recovery_backoff = RECOVERY_BACKOFF_INITIAL;
        Ok(())
    }

    /// Completion timeouts are recoverable, but only after an explicit cool-down. Other errors
    /// retain their existing fail-closed behaviour and may be retried on the next frame.
    fn note_render_failure(&mut self, error: &BridgeError) {
        if !matches!(error, BridgeError::CompletionTimeout { .. }) {
            self.recovery_not_before = None;
            self.recovery_backoff = RECOVERY_BACKOFF_INITIAL;
            return;
        }
        let delay = self.recovery_backoff;
        self.recovery_not_before = Instant::now().checked_add(delay);
        self.recovery_backoff = delay
            .checked_mul(2)
            .unwrap_or(RECOVERY_BACKOFF_MAX)
            .min(RECOVERY_BACKOFF_MAX);
        tracing::warn!(
            ?delay,
            %error,
            "DRM paint completion timed out; preserving the last scanout and backing off renderer recovery"
        );
    }

    /// Identity required by an opt-in XRGB destination history. A saturated epoch is deliberately
    /// unavailable so stale destination state can never become valid again through wraparound.
    #[doc(hidden)]
    pub fn xrgb_history_identity(
        &self,
        dst_pitch: usize,
        targets: usize,
    ) -> Option<XrgbHistoryIdentity> {
        let generation = self.xrgb_history_generation?;
        let epoch = self.xrgb_history_epoch?;
        let (width, height) = self.target.size;
        let tight = usize::try_from(width).ok()?.checked_mul(4)?;
        if width == 0 || height == 0 || dst_pitch < tight || !(1..=2).contains(&targets) {
            return None;
        }
        Some(XrgbHistoryIdentity {
            bridge_generation: generation,
            bridge_epoch: epoch,
            width,
            height,
            pitch: dst_pitch,
            targets,
            scanout_epoch: 0,
        })
    }

    /// Whether this bridge can participate in the opt-in XRGB destination history.
    ///
    /// A saturated process identity or content epoch disables only tracking. Callers must retain
    /// their established complete-output path in that case.
    #[doc(hidden)]
    #[must_use]
    pub fn xrgb_history_available(&self) -> bool {
        self.xrgb_history_generation.is_some() && self.xrgb_history_epoch.is_some()
    }

    /// Enable wall-clock phase collection for diagnostic runs. This deliberately defaults to
    /// false so normal compositor paints do not take timestamps or retain timing state.
    pub fn set_phase_timing_enabled(&mut self, enabled: bool) {
        self.phase_timing_enabled = enabled;
        self.last_phase_timing = None;
    }

    /// Adapter opened by this bridge's `RenderContext`.
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidDevice`] if the stored device identifier is invalid.
    pub fn adapter_info(&self) -> Result<BridgeAdapterInfo, BridgeError> {
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let info = dev_handle.adapter.get_info();
        Ok(BridgeAdapterInfo {
            name: info.name,
            backend: format!("{:?}", info.backend),
            device_type: format!("{:?}", info.device_type),
            driver: info.driver,
            vendor: info.vendor,
            device: info.device,
            pci_bus_id: info.device_pci_bus_id,
            #[cfg(target_os = "linux")]
            drm_device: ui::dmabuf_import::vulkan_drm_device_identity(&dev_handle.device),
        })
    }

    /// Latest diagnostic timing. `None` means profiling was not enabled for that paint.
    pub fn last_phase_timing(&self) -> Option<BridgePhaseTiming> {
        self.last_phase_timing
    }

    /// Consume the completed paint phases so a skipped subsequent frame cannot reuse them.
    pub fn take_phase_timing(&mut self) -> Option<BridgePhaseTiming> {
        self.last_phase_timing.take()
    }

    /// Current opt-in Vello per-dispatch timestamp diagnostic state.
    ///
    /// This is a deliberately thin diagnostic facade: ordinary DRM paints never call it or
    /// allocate timestamp bookkeeping. A benchmark that opts in to Vello timestamps should read
    /// [`Self::gpu_dispatch_timestamp_latest_submission_id`] after a completed paint, then take
    /// the sample and compare its submission id exactly rather than assigning a delayed sample to
    /// a different frame.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_status(&mut self) -> vello::GpuDispatchTimestampStatus {
        self.target.renderer.gpu_dispatch_timestamp_status()
    }

    /// Consume the one bounded Vello per-dispatch timestamp sample, if its asynchronous map has
    /// completed. Diagnostic callers must match [`vello::GpuDispatchTimestampSample::submission_id`]
    /// to [`Self::gpu_dispatch_timestamp_latest_submission_id`].
    #[doc(hidden)]
    pub fn take_gpu_dispatch_timestamp_sample(
        &mut self,
    ) -> Option<vello::GpuDispatchTimestampSample> {
        self.target.renderer.take_gpu_dispatch_timestamp_sample()
    }

    /// Identity assigned to the latest Vello recording when opt-in per-dispatch timestamps are
    /// available. The id advances even when the bounded diagnostic readback slot was busy, so a
    /// missing sample must remain missing rather than being attributed to a newer paint.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.target
            .renderer
            .gpu_dispatch_timestamp_latest_submission_id()
    }

    /// Bytes allocated exclusively for opt-in per-dispatch timestamp resolve/readback buffers.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_buffer_bytes(&self) -> u64 {
        self.target.renderer.gpu_dispatch_timestamp_buffer_bytes()
    }

    /// Effective initialization-time direct path workgroup count, exposed only for an explicit
    /// diagnostic benchmark header. Normal paint behavior does not query or alter it.
    #[doc(hidden)]
    pub fn direct_path_workgroups(&self) -> u32 {
        self.target.renderer.direct_path_workgroups()
    }

    /// Exact CPU upload bytes from the most recently encoded Vello recording. This borrows
    /// renderer-owned diagnostic state and is valid only until the next render starts.
    #[doc(hidden)]
    pub fn diagnostic_packed_scene(&self) -> &[u8] {
        self.target.renderer.diagnostic_packed_scene()
    }

    /// Exact resolver-expanded geometry inputs from the latest ordinary recording.
    ///
    /// Profile-only callers use this as an input-opportunity diagnostic; it does not retain GPU
    /// geometry or infer current bump counters from the ordinary settle result.
    #[doc(hidden)]
    pub fn diagnostic_geometry_input(
        &self,
    ) -> Result<vello::GeometryInput<'_>, vello::GeometryInputError> {
        self.target.renderer.diagnostic_geometry_input()
    }

    /// Render `scene` into the offscreen texture and return a slice of the tight-packed RGBA bytes
    /// (`width * height * 4`). The slice's lifetime is tied to `&mut self`; callers must clone bytes out before the next `paint`.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] if the vello render, the texture→buffer copy, or the readback
    /// buffer mapping fails. A readback row that doesn't fit the mapped buffer (shouldn't happen
    /// given `allocate_targets`, but guarded rather than trusted) is logged and truncates the
    /// returned buffer instead of panicking.
    pub fn paint(&mut self, scene: &Scene) -> Result<&[u8], BridgeError> {
        self.last_phase_timing = None;
        self.check_recovery_backoff()?;
        // This API intentionally has no revision. It always renders densely and cannot seed a
        // later versioned sparse frame, even though it refreshes the retained texture/staging.
        self.invalidate_retained();
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → RGBA bytes; \
                 zero-copy DMA-BUF interop is the Phase 5.3 follow-up. \
                 Expect ~10ms+ per frame for a 1080p output."
            );
        }

        let (width, height) = self.target.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let (mapped, _) = match self.render_normal(scene, None, timing.as_mut()) {
            Ok(result) => result,
            Err(error) => {
                self.note_render_failure(&error);
                return Err(error);
            }
        };

        // Block-map the readback buffer and copy into the tight `self.readback`, stripping row padding.
        let tight_row = (width as usize).saturating_mul(4);
        let needed = tight_row.saturating_mul(height as usize);
        let cpu_copy_started = self.phase_timing_enabled.then(Instant::now);
        self.readback.clear();
        self.readback.reserve(needed);
        for row in 0..height as usize {
            let start = row.saturating_mul(padded_bytes_per_row as usize);
            let end = start.saturating_add(tight_row);
            let Some(src_row) = mapped.get(start..end) else {
                tracing::warn!(
                    "DRM paint: readback row {row} out of the mapped buffer's bounds; truncating frame"
                );
                break;
            };
            self.readback.extend_from_slice(src_row);
        }
        drop(mapped);
        self.readback_buf.unmap();

        if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
            timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            timing.cpu_output_bytes = u64::try_from(self.readback.len()).unwrap_or(u64::MAX);
        }
        self.last_phase_timing = timing;

        if self.readback.len() == needed {
            self.mark_retained_complete(None);
        }

        Ok(&self.readback)
    }

    /// [`paint`](Self::paint) for a frame with HDR layers: the tight RGBA render with every
    /// layer image black, and the render with each in its channel colour, for
    /// [`crate::shell::hdr_surface::composite_hdr_pixel`].
    ///
    /// # Errors
    /// Fails like [`paint`](Self::paint), or when the second render cannot be read back.
    pub fn paint_with_hdr_layers(
        &mut self,
        scene: &Scene,
        hdr_layers: &[HdrLayer],
    ) -> Result<(&[u8], Vec<u8>), BridgeError> {
        {
            let (device, queue) = (&self.target.device, &self.target.queue);
            let compositor = self
                .hdr_compositor
                .get_or_insert_with(|| HdrCompositor::new(device, queue));
            self.target.hdr_pass = Some(compositor.prepare(self.target.size, hdr_layers, None));
        }
        let painted = self.paint(scene).map(|_| ());
        self.target.hdr_pass = None;
        painted?;
        let second = self
            .hdr_compositor
            .as_ref()
            .ok_or_else(|| BridgeError::Render("HDR compositor missing".to_owned()))?
            .read_second(LINUX_FENCE_WAIT)
            .map_err(BridgeError::Render)?;
        Ok((&self.readback, second))
    }

    /// Render `scene` and write it **directly** into an XRGB8888 destination (little-endian
    /// `[B, G, R, X]`), fusing the wgpu padding-strip and the RGBA→XRGB swizzle into one pass.
    ///
    /// [`paint`](Self::paint) exposes tight RGBA bytes; this method writes the mapped readback as
    /// XRGB into the caller's destination, which is normally a DRM dumb-buffer mmap.
    ///
    /// `dst_pitch` may exceed `width * 4` for hardware pitch alignment; rows/columns are clamped to what `dst` can hold, so a short destination is logged-and-truncated rather than panicking.
    ///
    /// # Errors
    /// Returns a [`BridgeError`] under the same conditions as [`paint`](Self::paint).
    pub fn paint_into_xrgb8888(
        &mut self,
        scene: &Scene,
        dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<(), BridgeError> {
        self.last_phase_timing = None;
        self.check_recovery_backoff()?;
        self.invalidate_retained();
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (direct mapped RGBA→XRGB swizzle); \
                 zero-copy DMA-BUF interop remains a follow-up. \
                 Expect ~several ms per frame for a 1080p output."
            );
        }

        let (width, height) = self.target.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let (mapped, _) = match self.render_normal(scene, None, timing.as_mut()) {
            Ok(result) => result,
            Err(error) => {
                self.note_render_failure(&error);
                return Err(error);
            }
        };

        let cpu_copy_started = self.phase_timing_enabled.then(Instant::now);
        if dst_pitch == 0 {
            drop(mapped);
            self.readback_buf.unmap();
            if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
                timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            }
            self.last_phase_timing = timing;
            self.mark_retained_complete(None);
            return Ok(());
        }
        let cpu_output_bytes = Self::swizzle_mapped_xrgb8888(
            MappedXrgbSource {
                bytes: &mapped,
                padded_bytes_per_row: padded_bytes_per_row as usize,
                width: width as usize,
                height: height as usize,
            },
            dst,
            dst_pitch,
            self.phase_timing_enabled,
        );
        drop(mapped);
        self.readback_buf.unmap();
        if let (Some(timing), Some(started)) = (timing.as_mut(), cpu_copy_started) {
            timing.cpu_copy_swizzle_us = started.elapsed().as_micros();
            timing.cpu_output_bytes = cpu_output_bytes;
        }
        self.last_phase_timing = timing;
        self.mark_retained_complete(None);
        Ok(())
    }

    /// Diagnostic-only exact comparison of dense and sparse Area fine work sharing one coarse
    /// recording. This invalidates retained state before rendering and never publishes a
    /// revision, so a subsequent ordinary paint must conservatively render densely.
    ///
    /// This intentionally allocates one additional target and staging buffer only for this
    /// explicit call. It is not part of the production paint path or benchmark workload.
    ///
    /// # Errors
    /// Returns [`BridgeError`] if Vello cannot submit or settle either paired fine result, or if
    /// either complete diagnostic readback cannot map before the ordinary one-second fence.
    #[doc(hidden)]
    pub fn shared_coarse_fine_diagnostic_into_xrgb8888(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<vello::SparseRenderStats>, BridgeError> {
        self.shared_coarse_diagnostic(
            scene,
            damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            Self::render_shared_coarse_diagnostic_attempts,
        )
    }

    /// Like [`Self::shared_coarse_fine_diagnostic_into_xrgb8888`], while also returning the
    /// bounded CPU geometry capture from that exact settled submission.
    #[doc(hidden)]
    pub fn shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        self.shared_coarse_diagnostic(
            scene,
            damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            Self::render_shared_coarse_geometry_diagnostic_attempts,
        )
    }

    /// Validate `damage`, allocate the sparse diagnostic target and run
    /// `attempts` against it on a freshly invalidated retained state.
    fn shared_coarse_diagnostic<T>(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        attempts: impl FnOnce(
            &mut Self,
            &Scene,
            &[vello::FineDamageRect],
            &mut [u8],
            &mut [u8],
            usize,
            SharedCoarseDiagnosticTarget<'_>,
        ) -> Result<Option<T>, BridgeError>,
    ) -> Result<Option<T>, BridgeError> {
        self.target.check_health()?;
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let Some(damage) = Self::validate_diagnostic_damage(damage, self.target.size) else {
            return Ok(None);
        };

        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let (sparse_texture, sparse_view, sparse_readback, sparse_stride) =
            Self::allocate_targets(&device, width, height);
        let sparse_target = SharedCoarseDiagnosticTarget {
            texture: &sparse_texture,
            view: &sparse_view,
            readback: &sparse_readback,
            padded_bytes_per_row: sparse_stride,
        };

        attempts(
            self,
            scene,
            &damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            sparse_target,
        )
    }

    /// Replay two fixed segment orders through ordinary Area fine while holding one live coarse
    /// result. This is an explicit failure diagnostic and is not used by paint or benchmarks.
    #[doc(hidden)]
    pub fn segment_order_replay_into_xrgb8888(
        &mut self,
        scene: &Scene,
        input: &vello::SegmentOrderReplayInput,
        left_dst: &mut [u8],
        right_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<Option<SegmentOrderReplayDiagnostic>, BridgeError> {
        self.target.check_health()?;
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let (width, height) = self.target.size;
        Self::validate_replay_destination(left_dst, dst_pitch, width, height, "left")?;
        Self::validate_replay_destination(right_dst, dst_pitch, width, height, "right")?;
        Renderer::segment_order_replay_preflight(width, height, input)
            .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        let (right_texture, right_view, right_readback, right_stride) =
            Self::allocate_targets(&device, width, height);
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let replay = self
                .target
                .renderer
                .render_to_textures_segment_order_replay_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    &right_view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    input,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            &right_texture,
                            &right_readback,
                            right_stride,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some(replay) = replay else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            let buffers = [
                (&self.readback_buf, self.readback_buf.size()),
                (&right_readback, right_readback.size()),
                (&replay.left, replay.bytes_per_witness),
                (&replay.right, replay.bytes_per_witness),
            ];
            let mapped = Self::map_replay_buffers(&self.context, self.dev_id, &buffers)?;
            if self.target.renderer.settle_bump_budget(&device) {
                drop(mapped);
                Self::unmap_replay_buffers(&buffers);
                continue;
            }
            let decoded = Self::swizzle_and_decode_replay(
                &mapped,
                &mut ReplayOutputDestinations {
                    left: left_dst,
                    right: right_dst,
                    dst_pitch,
                    width: width as usize,
                    height: height as usize,
                    left_padded_bytes_per_row: self.padded_bytes_per_row as usize,
                    right_padded_bytes_per_row: right_stride as usize,
                },
            );
            drop(mapped);
            Self::unmap_replay_buffers(&buffers);
            let (left_witness, right_witness) = decoded?;
            return Ok(Some(SegmentOrderReplayDiagnostic {
                left_witness,
                right_witness,
            }));
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn render_shared_coarse_geometry_diagnostic_attempts(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .target
                .renderer
                .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            sparse_target.texture,
                            sparse_target.readback,
                            sparse_target.padded_bytes_per_row,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some((stats, geometry)) = sparse else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            self.target.last_sparse_render_attempts =
                self.target.last_sparse_render_attempts.saturating_add(1);
            self.target.last_sparse_render_stats = Some(stats);

            if let Some(capture) = self.read_shared_coarse_geometry_diagnostic_outputs(
                &device,
                sparse_target,
                full_dst,
                sparse_dst,
                dst_pitch,
                stats,
                geometry,
            )? {
                return Ok(Some(capture));
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn render_shared_coarse_diagnostic_attempts(
        &mut self,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
    ) -> Result<Option<vello::SparseRenderStats>, BridgeError> {
        let (width, height) = self.target.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .target
                .renderer
                .render_to_textures_shared_coarse_area_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.target.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.target.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.target.texture,
                            &self.readback_buf,
                            self.padded_bytes_per_row,
                            width,
                            height,
                        );
                        Self::append_full_copy(
                            encoder,
                            sparse_target.texture,
                            sparse_target.readback,
                            sparse_target.padded_bytes_per_row,
                            width,
                            height,
                        );
                    },
                )
                .map_err(|error| BridgeError::Render(format!("{error:?}")))?;
            let Some(stats) = sparse else {
                return Ok(None);
            };
            self.target.last_render_attempts = self.target.last_render_attempts.saturating_add(1);
            self.target.last_sparse_render_attempts =
                self.target.last_sparse_render_attempts.saturating_add(1);
            self.target.last_sparse_render_stats = Some(stats);

            if self.read_shared_coarse_diagnostic_outputs(
                &device,
                sparse_target,
                full_dst,
                sparse_dst,
                dst_pitch,
            )? {
                return Ok(Some(stats));
            }
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: self.target.last_render_attempts,
        })
    }

    fn read_shared_coarse_diagnostic_outputs(
        &mut self,
        device: &wgpu::Device,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
    ) -> Result<bool, BridgeError> {
        let (width, height) = self.target.size;
        let full_mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
        let sparse_mapped =
            match Self::map_readback(&self.context, self.dev_id, sparse_target.readback) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(full_mapped);
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        if self.target.renderer.settle_bump_budget(device) {
            drop(sparse_mapped);
            drop(full_mapped);
            sparse_target.readback.unmap();
            self.readback_buf.unmap();
            return Ok(false);
        }
        let full_source = MappedXrgbSource {
            bytes: &full_mapped,
            padded_bytes_per_row: self.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        let sparse_source = MappedXrgbSource {
            bytes: &sparse_mapped,
            padded_bytes_per_row: sparse_target.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        Self::swizzle_mapped_xrgb8888(full_source, full_dst, dst_pitch, false);
        Self::swizzle_mapped_xrgb8888(sparse_source, sparse_dst, dst_pitch, false);
        drop(sparse_mapped);
        drop(full_mapped);
        sparse_target.readback.unmap();
        self.readback_buf.unmap();
        Ok(true)
    }

    fn read_shared_coarse_geometry_diagnostic_outputs(
        &mut self,
        device: &wgpu::Device,
        sparse_target: SharedCoarseDiagnosticTarget<'_>,
        full_dst: &mut [u8],
        sparse_dst: &mut [u8],
        dst_pitch: usize,
        stats: vello::SparseRenderStats,
        geometry: vello::SharedCoarseGeometryDownload,
    ) -> Result<Option<SharedCoarseGeometryDiagnostic>, BridgeError> {
        let (width, height) = self.target.size;
        let vello::SharedCoarseGeometryDownload {
            config,
            ptcl,
            ptcl_bytes,
            segments,
            segments_bytes,
        } = geometry;
        let full_mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
        let sparse_mapped =
            match Self::map_readback(&self.context, self.dev_id, sparse_target.readback) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(full_mapped);
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        let ptcl_mapped =
            match Self::map_buffer_range(&self.context, self.dev_id, &ptcl, ptcl_bytes) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(sparse_mapped);
                    drop(full_mapped);
                    sparse_target.readback.unmap();
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        let segments_mapped =
            match Self::map_buffer_range(&self.context, self.dev_id, &segments, segments_bytes) {
                Ok(mapped) => mapped,
                Err(error) => {
                    drop(ptcl_mapped);
                    drop(sparse_mapped);
                    drop(full_mapped);
                    ptcl.unmap();
                    sparse_target.readback.unmap();
                    self.readback_buf.unmap();
                    return Err(error);
                }
            };
        if self.target.renderer.settle_bump_budget(device) {
            drop(segments_mapped);
            drop(ptcl_mapped);
            drop(sparse_mapped);
            drop(full_mapped);
            segments.unmap();
            ptcl.unmap();
            sparse_target.readback.unmap();
            self.readback_buf.unmap();
            return Ok(None);
        }
        let full_source = MappedXrgbSource {
            bytes: &full_mapped,
            padded_bytes_per_row: self.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        let sparse_source = MappedXrgbSource {
            bytes: &sparse_mapped,
            padded_bytes_per_row: sparse_target.padded_bytes_per_row as usize,
            width: width as usize,
            height: height as usize,
        };
        Self::swizzle_mapped_xrgb8888(full_source, full_dst, dst_pitch, false);
        Self::swizzle_mapped_xrgb8888(sparse_source, sparse_dst, dst_pitch, false);
        let ptcl_bytes = ptcl_mapped.to_vec();
        let segments_bytes = segments_mapped.to_vec();
        drop(segments_mapped);
        drop(ptcl_mapped);
        drop(sparse_mapped);
        drop(full_mapped);
        segments.unmap();
        ptcl.unmap();
        sparse_target.readback.unmap();
        self.readback_buf.unmap();
        Ok(Some(SharedCoarseGeometryDiagnostic {
            stats,
            config,
            ptcl: ptcl_bytes,
            segments: segments_bytes,
        }))
    }

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

    #[allow(
        clippy::too_many_lines,
        reason = "render, retained-resource settlement, mapped readback cleanup, and transactional publication must remain auditable as one ordered operation"
    )]
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
    fn inject_next_tracked_swizzle_failure_for_test(&mut self) {
        self.fail_next_tracked_swizzle = true;
    }

    /// Normalize exact native rectangles without ever expanding their covered area. Overlapping
    /// rectangles intentionally stay separate unless one contains the other: bounding-box merges
    /// turn an L-shaped top bar plus popover into a large false damage band. Vello's sparse tile
    /// masks already deduplicate the raster work, while the copies below account for their actual
    /// (possibly overlapping) payload. A sparse dispatch with too many regions or at least half
    /// the output dirty remains a deliberate full-frame boundary.
    fn normalize_damage(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        const MAX_DAMAGE_RECTS: usize = 64;
        if damage.is_empty() || damage.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        let (width, height) = size;
        let mut normalized = Vec::with_capacity(damage.len());
        for &rect in damage {
            if rect.width == 0
                || rect.height == 0
                || rect.x >= width
                || rect.y >= height
                || rect.x.checked_add(rect.width).is_none_or(|x1| x1 > width)
                || rect.y.checked_add(rect.height).is_none_or(|y1| y1 > height)
            {
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

        let dirty_pixels = normalized.iter().fold(0_u64, |pixels, rect| {
            pixels.saturating_add(u64::from(rect.width).saturating_mul(u64::from(rect.height)))
        });
        let full_pixels = u64::from(width).saturating_mul(u64::from(height));
        if full_pixels == 0 || dirty_pixels.saturating_mul(2) >= full_pixels {
            return None;
        }
        Some(normalized)
    }

    /// Diagnostic paired rendering needs valid native rectangles, but intentionally does not
    /// apply the production sparse profitability threshold: a complete frame is a meaningful
    /// dense-versus-sparse oracle input.
    fn validate_diagnostic_damage(
        damage: &[vello::FineDamageRect],
        size: (u32, u32),
    ) -> Option<Vec<vello::FineDamageRect>> {
        const MAX_DAMAGE_RECTS: usize = 64;
        if damage.is_empty() || damage.len() > MAX_DAMAGE_RECTS {
            return None;
        }
        let (width, height) = size;
        if damage.iter().any(|rect| {
            rect.width == 0
                || rect.height == 0
                || rect.x >= width
                || rect.y >= height
                || rect.x.checked_add(rect.width).is_none_or(|x1| x1 > width)
                || rect.y.checked_add(rect.height).is_none_or(|y1| y1 > height)
        }) {
            return None;
        }
        Some(damage.to_vec())
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

    #[inline]
    fn invalidate_retained(&mut self) {
        self.target.retained_target_valid = false;
        self.retained_staging_valid = false;
        self.staging_content = None;
        self.target.last_successful_revision = None;
        self.target.owed_damage.clear();
    }

    #[inline]
    fn mark_retained_complete(&mut self, revision: Option<u64>) {
        self.target.retained_target_valid = true;
        self.retained_staging_valid = true;
        self.staging_content = Some((self.target.geometry_generation, revision));
        self.target.last_successful_revision = revision;
    }

    #[inline]
    fn reset_render_attempts(&mut self) {
        self.target.reset_attempts();
    }

    /// Swizzle mapped RGBA into XRGB. The ordinary branch deliberately has no per-row
    /// diagnostics, while the diagnostic branch counts only destination bytes actually written.
    fn swizzle_mapped_xrgb8888(
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

    fn validate_complete_xrgb_destination(
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
    fn swizzle_mapped_xrgb8888_with_plan(
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

    fn validate_complete_mapped_xrgb_source(
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
    fn swizzle_xrgb_row_reference(dst: &mut [u8], source: &[u8]) {
        for (d, s) in dst.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
            d[0] = s[2];
            d[1] = s[1];
            d[2] = s[0];
            d[3] = 0xff;
        }
    }

    fn render_normal(
        &mut self,
        scene: &Scene,
        damage: Option<SceneDamage<'_>>,
        timing: Option<&mut BridgePhaseTiming>,
    ) -> Result<(wgpu::BufferView, bool), BridgeError> {
        if self.context.devices.get(self.dev_id).is_none() {
            self.invalidate_retained();
            return Err(BridgeError::InvalidDevice(format!(
                "wgpu device id {} out of range",
                self.dev_id
            )));
        }
        self.take_device_error();
        if self.target.poisoned {
            let (width, height) = self.target.size;
            self.reconstruct_target(width, height)?;
        }
        let partial = self.retained_staging_valid
            && self.staging_content
                == Some((
                    self.target.geometry_generation,
                    self.target.last_successful_revision,
                ));
        let result = self.target.linux_frame_policy().and_then(|policy| {
            self.target.render(
                scene,
                damage,
                FrameOutput::Xrgb {
                    buffer: &self.readback_buf,
                    stride: self.padded_bytes_per_row,
                    partial,
                },
                policy,
                timing,
                |_, _, _| {},
            )
        });
        // The bridge publishes only after CPU output succeeds as well. A swizzle error remains
        // its deliberately stricter invalidate-both boundary, independent of GPU completion.
        self.invalidate_retained();
        let (mapped, admitted) = result?;
        if let Some(error) = self.take_device_error() {
            drop(mapped);
            self.readback_buf.unmap();
            return Err(error);
        }
        mapped
            .map(|mapped| (mapped, admitted))
            .ok_or_else(|| BridgeError::Map("XRGB output missing mapped framebuffer".to_owned()))
    }

    /// Take an uncaptured wgpu error raised since the last check, poisoning the target so the
    /// next render rebuilds it.
    fn take_device_error(&mut self) -> Option<BridgeError> {
        let error = self.device_errors.take()?;
        tracing::error!(%error, "DRM paint: wgpu reported an error; rebuilding the renderer");
        self.target.poisoned = true;
        Some(BridgeError::Render(format!("wgpu: {error}")))
    }

    /// Replace the target with a fresh renderer at `width` x `height` on the same device,
    /// keeping its clear colour. This is the one way out of a poisoned target.
    fn reconstruct_target(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        let color = self.target.base_color;
        let mut target = RetainedGpuTarget::new(
            self.context.devices.get(self.dev_id).ok_or_else(|| {
                BridgeError::InvalidDevice(
                    "renderer reconstruction lost its device handle".to_owned(),
                )
            })?,
            width,
            height,
        )?;
        target.set_base_color(color);
        target.present_generation = self.target.present_generation;
        self.target = target;
        self.invalidate_output_surfaces();
        (self.readback_buf, self.padded_bytes_per_row) =
            Self::allocate_staging(&self.target.device, width, height);
        self.readback.clear();
        self.xrgb_history_generation = Self::next_xrgb_history_generation();
        self.bump_xrgb_history_epoch();
        self.invalidate_retained();
        Ok(())
    }

    /// Append one complete readback copy for a shared-coarse diagnostic output. Diagnostics use
    /// full staging images so no unwritten row/pitch bytes can influence their comparison.
    fn append_full_copy(
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        readback_buf: &wgpu::Buffer,
        padded_bytes_per_row: u32,
        width: u32,
        height: u32,
    ) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
    }

    fn record_sparse_shader_selected_attempts(
        stats: &vello::SparseRenderStats,
        canonical_segment_order_sparse_attempts: Option<&mut usize>,
        area_ppt8_sparse_attempts: Option<&mut usize>,
        clip_single_group_sparse_attempts: Option<&mut usize>,
    ) {
        if stats.canonical_segment_order_shader_selected
            && let Some(selected_attempts) = canonical_segment_order_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
        if stats.area_ppt8_shader_selected
            && let Some(selected_attempts) = area_ppt8_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
        if stats.clip_single_group_shader_selected
            && let Some(selected_attempts) = clip_single_group_sparse_attempts
        {
            *selected_attempts = selected_attempts.saturating_add(1);
        }
    }

    fn sparse_damage_readback_bytes(damage: &[vello::FineDamageRect]) -> u64 {
        damage.iter().fold(0_u64, |bytes, rect| {
            bytes.saturating_add(
                u64::from(rect.width)
                    .saturating_mul(u64::from(rect.height))
                    .saturating_mul(4),
            )
        })
    }

    /// Map the staged `readback_buf` for CPU reads, blocking until the GPU copy completes. The
    /// returned view owns a buffer handle; the caller must drop it before `readback_buf.unmap()`.
    fn map_readback(
        context: &RenderContext,
        dev_id: usize,
        readback_buf: &wgpu::Buffer,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        Self::map_device_readback(device, readback_buf)
    }

    fn map_device_readback(
        device: &wgpu::Device,
        readback_buf: &wgpu::Buffer,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let slice = readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            tx.send(r).ok();
        });
        // Bounded, not indefinite: a fence in flight across a system suspend/resume can never signal,
        // and an unbounded wait would wedge the paint path forever — the same freeze the winit
        // presenter's post-present poll had (see `backend/winit.rs`). A timeout drops this frame through the existing error path; the next one retries.
        let waited = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        // Timeouts are typed the same as on the frame path, so a caller that feeds them to the
        // recovery back-off backs off for the same reasons.
        if let Err(e) = waited {
            // Cancels an in-flight map request (or releases a map that raced to completion) so
            // the next production frame can retry instead of seeing this buffer as pending.
            readback_buf.unmap();
            return Err(match e {
                wgpu::PollError::Timeout => BridgeError::CompletionTimeout {
                    phase: CompletionPhase::Fence,
                },
                other @ wgpu::PollError::WrongSubmissionIndex(..) => {
                    BridgeError::Map(format!("fence wait: {other:?}"))
                }
            });
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(format!("{e:?}")));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                readback_buf.unmap();
                return Err(BridgeError::CompletionTimeout {
                    phase: CompletionPhase::MapCallback,
                });
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(
                    "map_async callback dropped without reporting".to_owned(),
                ));
            }
        }
        Ok(slice.get_mapped_range())
    }

    /// Map only a capture's logical range: download buffers come from a size-class pool and can
    /// be physically larger than the submitted PTCL or segment resource.
    fn map_buffer_range(
        context: &RenderContext,
        dev_id: usize,
        buffer: &wgpu::Buffer,
        logical_bytes: u64,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let slice = buffer.slice(..logical_bytes);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        let waited = device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(1)),
        });
        if let Err(error) = waited {
            buffer.unmap();
            return Err(BridgeError::Map(format!("fence wait: {error:?}")));
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => Ok(slice.get_mapped_range()),
            Ok(Err(error)) => {
                buffer.unmap();
                Err(BridgeError::Map(format!("{error:?}")))
            }
            Err(error) => {
                buffer.unmap();
                Err(BridgeError::Map(format!("map_async channel: {error}")))
            }
        }
    }

    /// Map every replay result before one bounded device poll. Unlike the older geometry helper,
    /// this has one absolute deadline for both full-frame outputs and both tiny GPU witnesses.
    fn map_replay_buffers(
        context: &RenderContext,
        dev_id: usize,
        buffers: &[(&wgpu::Buffer, u64)],
    ) -> Result<Vec<wgpu::BufferView>, BridgeError> {
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(1))
            .ok_or_else(|| BridgeError::Map("replay mapping deadline overflowed".to_owned()))?;
        let mut receivers = Vec::with_capacity(buffers.len());
        for (buffer, bytes) in buffers {
            let slice = buffer.slice(..*bytes);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |result| {
                tx.send(result).ok();
            });
            receivers.push(rx);
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if let Err(error) = dev_handle.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(timeout),
        }) {
            Self::unmap_replay_buffers(buffers);
            return Err(BridgeError::Map(format!("replay fence wait: {error:?}")));
        }
        for receiver in receivers {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match receiver.recv_timeout(remaining) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    Self::unmap_replay_buffers(buffers);
                    return Err(BridgeError::Map(format!("replay map: {error:?}")));
                }
                Err(error) => {
                    Self::unmap_replay_buffers(buffers);
                    return Err(BridgeError::Map(format!("replay map callback: {error}")));
                }
            }
        }
        Ok(buffers
            .iter()
            .map(|(buffer, bytes)| buffer.slice(..*bytes).get_mapped_range())
            .collect())
    }

    fn swizzle_and_decode_replay(
        mapped: &[wgpu::BufferView],
        destinations: &mut ReplayOutputDestinations<'_>,
    ) -> Result<(Vec<u32>, Vec<u32>), BridgeError> {
        match mapped {
            [left, right, left_witness, right_witness] => {
                let left_source = MappedXrgbSource {
                    bytes: left,
                    padded_bytes_per_row: destinations.left_padded_bytes_per_row,
                    width: destinations.width,
                    height: destinations.height,
                };
                let right_source = MappedXrgbSource {
                    bytes: right,
                    padded_bytes_per_row: destinations.right_padded_bytes_per_row,
                    width: destinations.width,
                    height: destinations.height,
                };
                Self::swizzle_mapped_xrgb8888(
                    left_source,
                    destinations.left,
                    destinations.dst_pitch,
                    false,
                );
                Self::swizzle_mapped_xrgb8888(
                    right_source,
                    destinations.right,
                    destinations.dst_pitch,
                    false,
                );
                Self::decode_replay_witness(left_witness).and_then(|left| {
                    Self::decode_replay_witness(right_witness).map(|right| (left, right))
                })
            }
            _ => Err(BridgeError::Map(
                "replay mapped an unexpected number of buffers".to_owned(),
            )),
        }
    }

    fn decode_replay_witness(payload: &[u8]) -> Result<Vec<u32>, BridgeError> {
        let mut words = Vec::with_capacity(payload.len() / 4);
        let chunks = payload.chunks_exact(4);
        if !chunks.remainder().is_empty() {
            return Err(BridgeError::Map(
                "replay witness length is not a whole number of u32 words".to_owned(),
            ));
        }
        for chunk in chunks {
            let [byte0, byte1, byte2, byte3] = chunk else {
                return Err(BridgeError::Map(
                    "replay witness contained a partial u32 word".to_owned(),
                ));
            };
            words.push(u32::from_le_bytes([*byte0, *byte1, *byte2, *byte3]));
        }
        Ok(words)
    }

    fn unmap_replay_buffers(buffers: &[(&wgpu::Buffer, u64)]) {
        for (buffer, _) in buffers {
            buffer.unmap();
        }
    }

    fn validate_replay_destination(
        destination: &[u8],
        pitch: usize,
        width: u32,
        height: u32,
        label: &str,
    ) -> Result<(), BridgeError> {
        let tight = usize::try_from(width)
            .ok()
            .and_then(|width| width.checked_mul(4))
            .ok_or_else(|| BridgeError::InvalidSize(format!("{label} width overflow")))?;
        let required = pitch
            .checked_mul(usize::try_from(height).map_err(|_| {
                BridgeError::InvalidSize(format!("{label} height does not fit usize"))
            })?)
            .ok_or_else(|| BridgeError::InvalidSize(format!("{label} destination overflow")))?;
        if pitch < tight || destination.len() < required {
            return Err(BridgeError::InvalidSize(format!(
                "{label} replay destination is {} bytes at pitch {pitch}; need at least {required} bytes and pitch {tight}",
                destination.len(),
            )));
        }
        Ok(())
    }

    /// Resize the offscreen target, reallocating texture + readback buffer; returns `Ok(())` even when the new size matches the current (no-op).
    ///
    /// # Errors
    /// Returns [`BridgeError::InvalidSize`] if `width` or `height` is `0`, or
    /// [`BridgeError::InvalidDevice`] if the bridge's device id is out of range (defensive;
    /// cannot happen in practice).
    pub fn resize(&mut self, width: u32, height: u32) -> Result<(), BridgeError> {
        // Treat even a rejected/no-op resize request as a sparse boundary. This keeps a caller
        // that changed its output model but failed to complete reallocation from reusing an
        // ambiguous target on the next frame.
        self.invalidate_retained();
        self.invalidate_output_surfaces();
        self.bump_xrgb_history_epoch();
        // A poisoned target refuses to resize; rebuilding it at the new size is what a failed
        // frame followed by a display change needs. Refusing instead left the presenter asking
        // again every frame and presenting nothing.
        self.take_device_error();
        if self.target.poisoned {
            return self.reconstruct_target(width, height);
        }
        let old_size = self.target.size;
        self.target.resize(width, height)?;
        if old_size == self.target.size {
            return Ok(());
        }
        (self.readback_buf, self.padded_bytes_per_row) =
            Self::allocate_staging(&self.target.device, width, height);
        // Reset the tight buffer for the next `paint`; the new size may be larger, so clearing is cleaner than keeping capacity.
        self.readback.clear();
        Ok(())
    }

    /// Why this bridge's device was lost, once it has been. Nothing recovers a lost device but
    /// a new bridge.
    pub fn device_lost(&self) -> Option<String> {
        self.target.loss.reason()
    }

    /// Current (width, height); useful in tests and for comparing against the connector's mode size before resizing.
    pub fn size(&self) -> (u32, u32) {
        self.target.size
    }

    /// Vello submissions used by the latest paint, including any sparse attempt and bounded
    /// dense bump-budget recovery retries.
    pub fn last_render_attempts(&self) -> usize {
        self.target.last_render_attempts
    }

    /// Sparse Vello submissions used by the latest paint. This remains zero for every legacy
    /// full-render API and for conservative versioned fallbacks.
    pub fn last_sparse_render_attempts(&self) -> usize {
        self.target.last_sparse_render_attempts
    }

    /// The image atlas and the CPU pixels its residents keep alive.
    pub fn image_cache_stats(&self) -> vello::ImageCacheStats {
        self.target.vello().image_cache_stats()
    }

    /// GPU allocations the renderer accounts for, the image atlas included.
    pub fn renderer_memory_stats(&self) -> vello::RendererMemoryStats {
        self.target.vello().memory_stats()
    }

    /// After each frame (TIL-731). When a window closed since the last
    /// frame, the image atlas comes down within the next few frames rather
    /// than after sixty: a desktop that goes idle after a close draws too
    /// few to bring it down the slow way, and kept up to 256 MiB of atlas.
    /// When the atlas did come down, so did the scene: the buffers pooled for
    /// the larger one go too (the next frame pools what it needs), and a trim
    /// gives the memory back to the kernel. The renderer's scratch (pooled
    /// buffers and bump reservation, sized for the busiest scene it drew)
    /// comes down the same way, within a few frames of a close; when the
    /// reservation shrank, a trim gives that back too. Cheap enough for
    /// every frame.
    pub fn after_paint(&mut self) {
        let closed = crate::shell::heap_trim::windows_closed();
        if closed != self.windows_closed_seen {
            self.windows_closed_seen = closed;
            self.target.renderer.shrink_image_atlas_soon();
            self.target.renderer.shrink_scratch_soon();
        }
        let atlas_before = self.logged_memory.map(|mark| mark[0]);
        let atlas_now = u64::from(self.target.vello().image_cache_stats().atlas_side);
        let shrinks = self.target.vello().scratch_shrinks();
        let scratch_shrank = shrinks != self.scratch_shrinks_seen;
        self.scratch_shrinks_seen = shrinks;
        if atlas_before.is_some_and(|was| atlas_now < was) {
            self.target.renderer.trim_pooled_buffers();
            crate::shell::heap_trim::request();
        } else if scratch_shrank {
            crate::shell::heap_trim::request();
        }
        self.log_memory_changes();
    }

    /// Warm the layers the target composites a lone output's strip pans
    /// from, at rest ([`RetainedGpuTarget::warm_layers`]): the pixels
    /// rasterised, `None` when the target cannot present these layers.
    ///
    /// # Errors
    /// As [`RetainedGpuTarget::warm_layers`], or a device error it left.
    pub fn warm_layers(
        &mut self,
        cache: &crate::shell::frame::FrameCache,
        deadline: Instant,
        keep_going: &dyn Fn() -> bool,
    ) -> Result<Option<u64>, BridgeError> {
        if let Some(error) = self.take_device_error() {
            return Err(error);
        }
        let warmed = self.target.warm_layers(cache, deadline, keep_going);
        let warmed = warmed.and_then(|pixels| self.take_device_error().map_or(Ok(pixels), Err));
        self.log_memory_changes();
        warmed
    }

    /// Log one `RENDERER-MEMORY` line when the image atlas changes size or
    /// the renderer's GPU allocations move by 8 MiB or more since the last
    /// line, so the session log tells which of them a desktop with no window
    /// left still holds.
    fn log_memory_changes(&mut self) {
        const STEP: u64 = 8 << 20;
        let memory = self.target.vello().memory_stats();
        let images = self.target.vello().image_cache_stats();
        let mark = [
            u64::from(images.atlas_side),
            memory.image_bytes,
            memory.pooled_buffer_bytes,
            memory.live_buffer_bytes,
            memory.bump_budget_bytes,
            memory.sparse_upload_bytes,
        ];
        let moved = self.logged_memory.is_none_or(|last| {
            last[0] != mark[0]
                || last
                    .iter()
                    .zip(mark)
                    .skip(1)
                    .any(|(was, now)| was.abs_diff(now) >= STEP)
        });
        if !moved {
            return;
        }
        self.logged_memory = Some(mark);
        let mib = |bytes: u64| bytes / (1 << 20);
        // A measurement line for the session log, like WORKSPACE-SWITCH.
        eprintln!(
            "RENDERER-MEMORY atlas_side={} image_mib={} pooled_mib={} live_mib={} bump_mib={} \
             sparse_upload_mib={} resident_images={}",
            images.atlas_side,
            mib(memory.image_bytes),
            mib(memory.pooled_buffer_bytes),
            mib(memory.live_buffer_bytes),
            mib(memory.bump_budget_bytes),
            mib(memory.sparse_upload_bytes),
            images.resident,
        );
    }

    /// Dense Vello submissions used by the latest paint, including bump-budget retries.
    pub fn last_full_render_attempts(&self) -> usize {
        self.target.last_full_render_attempts
    }

    /// Full render attempts from the latest paint which actually recorded canonical segment
    /// ordering. A selected pass is not evidence of completion or performance improvement.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_full_attempts(&self) -> usize {
        self.target.last_canonical_segment_order_full_attempts
    }

    /// Sparse render attempts from the latest paint which actually recorded canonical segment
    /// ordering, including an overflowed sparse attempt before dense recovery.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_sparse_attempts(&self) -> usize {
        self.target.last_canonical_segment_order_sparse_attempts
    }

    /// Full render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_full_attempts(&self) -> usize {
        self.target.last_area_ppt8_full_attempts
    }

    /// Sparse render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_sparse_attempts(&self) -> usize {
        self.target.last_area_ppt8_sparse_attempts
    }

    /// Full render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_full_attempts(&self) -> usize {
        self.target.last_clip_single_group_full_attempts
    }

    /// Sparse render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_sparse_attempts(&self) -> usize {
        self.target.last_clip_single_group_sparse_attempts
    }

    /// Actual sparse coarse-dispatch selection from the latest paint, if sparse submission was
    /// reached. This is present even when a sparse overflow forced a later dense recovery.
    pub fn last_sparse_render_stats(&self) -> Option<vello::SparseRenderStats> {
        self.target.last_sparse_render_stats
    }

    /// Whether path-level sparse tile bounds were requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_requested(&self) -> bool {
        self.target.renderer.sparse_tile_bounds_requested()
    }

    /// Whether this renderer can apply path-level sparse tile bounds.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_available(&self) -> bool {
        self.target.renderer.sparse_tile_bounds_available()
    }

    /// Whether sparse Area fine quad skipping was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_requested(&self) -> bool {
        self.target.renderer.sparse_fine_quad_skip_requested()
    }

    /// Whether sparse Area fine quad skipping can be selected for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_available(&self) -> bool {
        self.target.renderer.sparse_fine_quad_skip_available()
    }

    /// Whether sparse coarse opaque pruning was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_requested(&self) -> bool {
        self.target.renderer.sparse_opaque_prune_requested()
    }

    /// Whether sparse coarse opaque pruning can select its shader for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_available(&self) -> bool {
        self.target.renderer.sparse_opaque_prune_available()
    }

    /// Whether sparse solid-fuse selection was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_requested(&self) -> bool {
        self.target.renderer.sparse_solid_fuse_requested()
    }

    /// Whether sparse solid-fuse selection can be used by this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_available(&self) -> bool {
        self.target.renderer.sparse_solid_fuse_available()
    }

    /// Whether sparse segment-row masking was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_requested(&self) -> bool {
        self.target.renderer.sparse_segment_row_mask_requested()
    }

    /// Whether sparse segment-row masking can be selected by this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_available(&self) -> bool {
        self.target.renderer.sparse_segment_row_mask_available()
    }

    /// Render with a fixed per-tile segment order, so repeated paints of one
    /// scene are bit-identical. For exact-pixel oracles.
    ///
    /// # Errors
    /// Returns [`BridgeError::Renderer`] if the shader set fails to compile.
    #[doc(hidden)]
    pub fn enable_canonical_segment_order(&mut self) -> Result<(), BridgeError> {
        self.invalidate_retained();
        let RetainedGpuTarget {
            device, renderer, ..
        } = &mut self.target;
        renderer
            .enable_canonical_segment_order(device)
            .map_err(|e| BridgeError::Renderer(format!("{e:?}")))
    }

    /// Whether canonical segment ordering was requested for this renderer.
    #[doc(hidden)]
    pub fn canonical_segment_order_requested(&self) -> bool {
        self.target.renderer.canonical_segment_order_requested()
    }

    /// Whether this renderer has the canonical segment-ordering pass available.
    #[doc(hidden)]
    pub fn canonical_segment_order_available(&self) -> bool {
        self.target.renderer.canonical_segment_order_available()
    }

    /// Whether Area PPT8 was requested for this renderer.
    #[doc(hidden)]
    pub fn area_ppt8_requested(&self) -> bool {
        self.target.renderer.area_ppt8_requested()
    }

    /// Whether this renderer has the ordinary Area PPT8 full shader available.
    #[doc(hidden)]
    pub fn area_ppt8_available(&self) -> bool {
        self.target.renderer.area_ppt8_available()
    }

    /// Whether single-workgroup clip leaf was requested for this renderer.
    #[doc(hidden)]
    pub fn clip_single_group_requested(&self) -> bool {
        self.target.renderer.clip_single_group_requested()
    }

    /// Whether the single-workgroup clip leaf permutation is available.
    #[doc(hidden)]
    pub fn clip_single_group_available(&self) -> bool {
        self.target.renderer.clip_single_group_available()
    }

    /// Allocate (texture, view, `readback_buf`, `padded_bytes_per_row`) for the given size; shared by `new` and `resize`.
    fn allocate_targets(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32) {
        let (texture, view) = RetainedGpuTarget::allocate_texture(device, width, height);
        let (readback_buf, padded_bytes_per_row) = Self::allocate_staging(device, width, height);
        (texture, view, readback_buf, padded_bytes_per_row)
    }

    fn allocate_staging(device: &wgpu::Device, width: u32, height: u32) -> (wgpu::Buffer, u32) {
        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let readback_size = u64::from(padded_bytes_per_row).saturating_mul(u64::from(height));
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-desktop-drm-paint-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        (readback_buf, padded_bytes_per_row)
    }
}

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
/// spanning the combined output geometry into `source.src_rgba` (via [`VelloDrmBridge::paint`]),
/// then each output presents the sub-rect at its `Output.position`/`size` into its own scanout
/// dumb buffer. `source.src_x`/`source.src_y` is the sub-rect's top-left in the source; `out_w`×`out_h`
/// is the output's size.
///
/// Rows/cols outside the source or destination bounds are skipped (clamped, not panicking). For
/// a single output at the origin matching the combined geometry, this is byte-for-byte [`VelloDrmBridge::paint_into_xrgb8888`].
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

fn tight_clip_rows(
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
mod subrect_tests {
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

        // Phase `offset_subrect_picks_the_right_source_region`.
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

        // Phase `destination_pitch_padding_is_untouched`.
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

        // Phase `overhanging_output_is_clamped_not_panicking`.
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

        // Phase `profiled_path_copies_only_the_output_viewport`.
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

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]
mod tests {
    //! Renderer gates require an actual wgpu adapter on every selected host.
    use super::*;
    use crate::shell::frame::FrameBuilder;
    use crate::shell::test_support::PopulatedShellFixture;
    use automerge::{AutoCommit, ROOT};
    use ui::taffy::NodeId;
    use vello::kurbo::Affine;
    use vello::peniko::Fill;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

    #[test]
    fn pointer_swizzle_matches_scalar_oracle_for_unaligned_and_truncated_rows() {
        for source_offset in 0..4 {
            for destination_offset in 0..4 {
                for source_len in 0..68 {
                    for destination_len in 0..68 {
                        let source_storage: Vec<u8> = (0_usize..source_offset + source_len)
                            .map(|index| {
                                u8::try_from(index.wrapping_mul(37).wrapping_add(11) & 0xff)
                                    .unwrap_or_default()
                            })
                            .collect();
                        let source = &source_storage[source_offset..];
                        let mut expected_storage = vec![0x6d; destination_offset + destination_len];
                        let mut actual_storage = expected_storage.clone();
                        VelloDrmBridge::swizzle_xrgb_row_reference(
                            &mut expected_storage[destination_offset..],
                            source,
                        );
                        VelloDrmBridge::swizzle_xrgb_row(
                            &mut actual_storage[destination_offset..],
                            source,
                        );
                        assert_eq!(
                            actual_storage, expected_storage,
                            "source offset={source_offset} destination offset={destination_offset} source len={source_len} destination len={destination_len}"
                        );
                    }
                }
            }
        }
    }

    fn required_bridge(w: u32, h: u32) -> VelloDrmBridge {
        let bridge = VelloDrmBridge::new(w, h).expect("renderer gate requires a real wgpu adapter");
        let adapter = bridge.adapter_info().expect("bridge adapter identity");
        assert!(!adapter.name.is_empty());
        assert!(!adapter.backend.is_empty());
        assert!(!adapter.device_type.is_empty());
        eprintln!("renderer gate adapter: {adapter:?}");
        bridge
    }

    /// A frame that renders in full.
    fn full_frame(revision: u64) -> SceneDamage<'static> {
        SceneDamage {
            revision,
            previous_revision: None,
            regions: None,
        }
    }

    /// One output's frame through the production per-output path, into an offscreen
    /// stand-in for its imported scanout target, read back as BGRA bytes.
    fn paint_output_pixels(
        bridge: &mut VelloDrmBridge,
        scene: &Scene,
        view: &OutputView,
        revision: u64,
    ) -> Vec<u8> {
        paint_output_damage(bridge, scene, view, full_frame(revision)).1
    }

    /// [`paint_output_pixels`] for a frame carrying `damage`, with the frame's stats.
    fn paint_output_damage(
        bridge: &mut VelloDrmBridge,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
    ) -> (GpuFrameStats, Vec<u8>) {
        let (texture, stats) = paint_output_offscreen(bridge, scene, view, damage);
        (stats, read_target(bridge, &texture))
    }

    /// The bytes of a 4-byte-per-pixel scanout stand-in, rows tight.
    fn read_target(bridge: &VelloDrmBridge, texture: &wgpu::Texture) -> Vec<u8> {
        let (width, height) = (texture.width(), texture.height());
        let (device, queue) = bridge.gpu();
        let (device, queue) = (device.clone(), queue.clone());
        let (buffer, stride) = VelloDrmBridge::allocate_staging(&device, width, height);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        VelloDrmBridge::append_full_copy(&mut encoder, texture, &buffer, stride, width, height);
        queue.submit([encoder.finish()]);
        let mapped =
            VelloDrmBridge::map_device_readback(&device, &buffer).expect("scanout stand-in maps");
        let mut pixels = Vec::new();
        for row in mapped.chunks(stride as usize).take(height as usize) {
            pixels.extend_from_slice(&row[..(width as usize).saturating_mul(4)]);
        }
        drop(mapped);
        buffer.unmap();
        pixels
    }

    /// One output's frame through the production per-output path into an offscreen
    /// stand-in for its imported scanout target, which is returned unread.
    fn paint_output_offscreen(
        bridge: &mut VelloDrmBridge,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
    ) -> (wgpu::Texture, GpuFrameStats) {
        paint_output_offscreen_as(
            bridge,
            scene,
            view,
            damage,
            (wgpu::TextureFormat::Bgra8Unorm, None),
        )
    }

    /// [`paint_output_offscreen`] into a stand-in of `format`: `Bgra8Unorm` for an SDR
    /// scanout, `Rgb10a2Unorm` for an `XBGR2101010` HDR one; through `color_lut` when the
    /// output has an ICC profile.
    fn paint_output_offscreen_as(
        bridge: &mut VelloDrmBridge,
        scene: &Scene,
        view: &OutputView,
        damage: SceneDamage<'_>,
        encoding: (wgpu::TextureFormat, Option<&IccLut>),
    ) -> (wgpu::Texture, GpuFrameStats) {
        paint_output_offscreen_with(bridge, (scene, None), view, damage, encoding)
    }

    /// [`paint_output_offscreen_as`] for a frame that may also come as
    /// layers (`FramePaint::cache`), as the DRM direct path passes it.
    fn paint_output_offscreen_with(
        bridge: &mut VelloDrmBridge,
        (scene, layers): (&Scene, Option<&crate::shell::frame::FrameCache>),
        view: &OutputView,
        damage: SceneDamage<'_>,
        (format, color_lut): (wgpu::TextureFormat, Option<&IccLut>),
    ) -> (wgpu::Texture, GpuFrameStats) {
        let (width, height) = view.target_size();
        let (device, _) = bridge.gpu();
        let device = device.clone();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("output-view-test-scanout"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let target_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let stats = bridge
            .paint_output_into(
                scene,
                view,
                damage,
                OutputTarget {
                    view: &target_view,
                    format,
                    size: (width, height),
                    orientation: view.orientation,
                },
                Instant::now()
                    .checked_add(tilo_test_env::hang_bound())
                    .expect("the hang bound fits in an Instant"),
                OutputSignal {
                    sdr_white_nits: crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                    hdr_layers: &[],
                    color_lut,
                    layers,
                },
            )
            .expect("the output's frame renders");
        (texture, stats)
    }

    #[test]
    fn output_view_renders_its_subrect() {
        // A 200x100 desktop of two 100x100 outputs side by side: red on the
        // left output, blue on the right.
        let mut bridge = required_bridge(200, 100);
        let mut scene = Scene::new();
        for (x, color) in [
            (0.0, Color::from_rgb8(255, 0, 0)),
            (100.0, Color::from_rgb8(0, 0, 255)),
        ] {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                color,
                None,
                &vello::kurbo::Rect::new(x, 0.0, x + 100.0, 100.0),
            );
        }
        let right = OutputView {
            id: crate::shell::render::OutputId(1),
            transform: Affine::translate((-100.0, 0.0)),
            size: (100, 100),
            orientation: OutputTransform::Normal,
        };
        let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
        // The scanout stand-in is BGRA.
        let blue: [u8; 4] = [255, 0, 0, 255];
        let red: [u8; 4] = [0, 0, 255, 255];
        for (view, expected, name) in [(right, blue, "right"), (left, red, "left")] {
            let pixels = paint_output_pixels(&mut bridge, &scene, &view, 1);
            assert_eq!(pixels.len(), 100 * 100 * 4);
            let wrong = pixels
                .chunks_exact(4)
                .filter(|pixel| *pixel != expected)
                .count();
            assert_eq!(
                wrong, 0,
                "every pixel of the {name} output is its half of the scene"
            );
        }
        assert_eq!(
            bridge.output_surfaces.len(),
            2,
            "each output keeps its own retained surface"
        );
        assert_eq!(
            bridge.size(),
            (200, 100),
            "the scene's own target is untouched by the outputs"
        );
    }

    const ORIENTATIONS: [OutputTransform; 4] = [
        OutputTransform::Normal,
        OutputTransform::Rotate90,
        OutputTransform::Rotate180,
        OutputTransform::Rotate270,
    ];

    /// A desktop of `size` in which every turn of a pixel shows: a distinct colour in two
    /// corners, an anti-aliased diagonal, a translucent circle and a scaled image.
    fn asymmetric_scene(size: (u32, u32)) -> Scene {
        use vello::kurbo::{BezPath, Circle, Rect};
        use vello::peniko::{Blob, ImageAlphaType, ImageBrush, ImageData, ImageFormat};
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let mut scene = Scene::new();
        for (color, rect) in [
            (Color::from_rgb8(30, 60, 90), Rect::new(0.0, 0.0, w, h)),
            (
                Color::from_rgb8(230, 20, 40),
                Rect::new(0.0, 0.0, w / 3.0, h / 4.0),
            ),
            (
                Color::from_rgb8(20, 210, 70),
                Rect::new(w * 0.6, h * 0.7, w, h),
            ),
        ] {
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        }
        let mut wedge = BezPath::new();
        wedge.move_to((0.0, h));
        wedge.line_to((w * 0.8, 0.0));
        wedge.line_to((w, h * 0.2));
        wedge.close_path();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(250, 200, 20, 180),
            None,
            &wedge,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(120, 80, 250, 200),
            None,
            &Circle::new((w * 0.3, h * 0.6), h * 0.21),
        );
        let texels: Vec<u8> = (0_u8..12)
            .flat_map(|i| {
                [
                    i.wrapping_mul(21),
                    255_u8.wrapping_sub(i.wrapping_mul(19)),
                    i.wrapping_mul(7),
                    255,
                ]
            })
            .collect();
        let image = ImageData {
            data: Blob::new(std::sync::Arc::new(texels)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::AlphaPremultiplied,
            width: 4,
            height: 3,
        };
        scene.fill(
            Fill::NonZero,
            // Five times its size, at (w/2, h/10).
            Affine::new([5.0, 0.0, 0.0, 5.0, w * 0.5, h * 0.1]),
            &ImageBrush::new(image),
            None,
            &Rect::new(0.0, 0.0, 4.0, 3.0),
        );
        scene
    }

    /// BGRA scanout bytes as the RGBA source the readback kernels take.
    fn bgra_to_rgba(bgra: &[u8]) -> Vec<u8> {
        bgra.chunks_exact(4)
            .flat_map(|pixel| [pixel[2], pixel[1], pixel[0], pixel[3]])
            .collect()
    }

    /// Pixels whose colour channels differ between a GPU `Bgra8Unorm` target and the CPU
    /// kernel's `XRGB8888` bytes. The X byte is padding KMS ignores.
    fn differing_pixels(gpu: &[u8], cpu: &[u8]) -> usize {
        assert_eq!(gpu.len(), cpu.len());
        gpu.chunks_exact(4)
            .zip(cpu.chunks_exact(4))
            .filter(|(g, c)| g[..3] != c[..3])
            .count()
    }

    #[test]
    fn a_rotated_lone_output_matches_the_readback_byte_for_byte() {
        // The output is the desktop: the direct path renders into the bridge's own target,
        // which is what the readback presenter renders too, and turns it on the GPU. The
        // readback turns the same render on the CPU (`paint_one_scanout` without ICC or HDR).
        let size = (90, 52);
        let mut bridge = required_bridge(size.0, size.1);
        let scene = asymmetric_scene(size);
        let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
        for (revision, transform) in (1..).zip(ORIENTATIONS) {
            let view =
                OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
            assert!(bridge.view_is_whole_scene(&view));
            let gpu = paint_output_pixels(&mut bridge, &scene, &view, revision);
            let (pw, ph) = view.target_size();
            let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
            swizzle_subrect_rgba_to_xrgb8888_composited(
                SubrectSource {
                    src_rgba: &rgba,
                    src_w: size.0 as usize,
                    src_h: size.1 as usize,
                    src_x: 0,
                    src_y: 0,
                },
                &mut cpu,
                (pw * 4) as usize,
                (pw as usize, ph as usize),
                transform,
                None,
            );
            assert_eq!(
                differing_pixels(&gpu, &cpu),
                0,
                "{transform:?}: the GPU turn is the readback's, byte for byte"
            );
        }
    }

    #[test]
    fn a_rotated_output_beside_another_turns_its_own_surface() {
        // Two 90x52 outputs side by side; the right one is rotated and renders into its own
        // surface. Its scanout must be its unrotated image turned exactly as the readback
        // kernel turns it.
        let output = (90, 52);
        let mut bridge = required_bridge(output.0 * 2, output.1);
        let scene = asymmetric_scene((output.0 * 2, output.1));
        let id = crate::shell::render::OutputId(1);
        let upright = OutputView::at(id, (90, 0), output);
        let unrotated = bgra_to_rgba(&paint_output_pixels(&mut bridge, &scene, &upright, 1));
        for (revision, transform) in (2..).zip(ORIENTATIONS) {
            let view = upright.oriented(transform);
            assert!(!bridge.view_is_whole_scene(&view));
            let gpu = paint_output_pixels(&mut bridge, &scene, &view, revision);
            let (pw, ph) = view.target_size();
            let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
            swizzle_subrect_rgba_to_xrgb8888_transformed(
                SubrectSource {
                    src_rgba: &unrotated,
                    src_w: output.0 as usize,
                    src_h: output.1 as usize,
                    src_x: 0,
                    src_y: 0,
                },
                &mut cpu,
                (pw * 4) as usize,
                pw as usize,
                ph as usize,
                transform,
            );
            assert_eq!(
                differing_pixels(&gpu, &cpu),
                0,
                "{transform:?}: the surface is turned on the GPU exactly as on the CPU"
            );
        }
        assert_eq!(
            bridge.output_surfaces.len(),
            1,
            "the rotated output keeps one surface in scene orientation, whatever its turn"
        );
    }

    #[test]
    fn a_rotated_hdr_output_matches_the_readback_pq_encode() {
        // The readback encodes and turns on the CPU (`SdrToPqEncoder`); the direct path on the
        // GPU. The encodes differ by at most 2 of 1023 code values
        // (`gpu_encode_matches_the_cpu_reference`); the turn must add nothing to that.
        let size = (90, 52);
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
        let mut bridge = required_bridge(size.0, size.1);
        let scene = asymmetric_scene(size);
        let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
        let encoder = crate::shell::color_pipeline::SdrToPqEncoder::new(white);
        for (revision, transform) in (1..).zip(ORIENTATIONS) {
            let view =
                OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
            let (texture, _) = paint_output_offscreen_as(
                &mut bridge,
                &scene,
                &view,
                full_frame(revision),
                (wgpu::TextureFormat::Rgb10a2Unorm, None),
            );
            let gpu = read_target(&bridge, &texture);
            let (pw, ph) = view.target_size();
            let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
            encode_subrect_rgba_to_xbgr2101010_transformed(
                SubrectSource {
                    src_rgba: &rgba,
                    src_w: size.0 as usize,
                    src_h: size.1 as usize,
                    src_x: 0,
                    src_y: 0,
                },
                &mut cpu,
                (pw * 4) as usize,
                (pw as usize, ph as usize),
                transform,
                &encoder,
                None,
            );
            let word = |bytes: &[u8]| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let max_delta = gpu
                .chunks_exact(4)
                .zip(cpu.chunks_exact(4))
                .flat_map(|(g, c)| {
                    let (g, c) = (word(g), word(c));
                    [0, 10, 20].map(|shift| {
                        (i64::from((g >> shift) & 0x3FF) - i64::from((c >> shift) & 0x3FF)).abs()
                    })
                })
                .max()
                .unwrap_or(0);
            assert!(
                max_delta <= 2,
                "{transform:?}: the GPU PQ encode differs from the readback's by {max_delta} code values"
            );
        }
    }

    #[test]
    fn a_calibrated_output_matches_the_readback_qcms_pass() {
        // The readback calibrates on the CPU: the output's sub-rect copied out, qcms applied,
        // then turned and swizzled (`paint_one_scanout`). The direct path does the same in
        // its final GPU pass through the profile's `IccLut`, with no readback, and stays
        // within the LUT's stated tolerance of qcms (`icc_lut` module docs).
        let size = (90, 52);
        let panel = super::super::icc_lut::tests::adobe_rgb();
        let transform_icc = panel.transform();
        let lut = IccLut::from_transform(&transform_icc);
        let mut bridge = required_bridge(size.0, size.1);
        let scene = asymmetric_scene(size);
        let rgba = bridge.paint(&scene).expect("the dense render").to_vec();
        let mut calibrated = Vec::new();
        copy_rgba_subrect(
            SubrectSource {
                src_rgba: &rgba,
                src_w: size.0 as usize,
                src_h: size.1 as usize,
                src_x: 0,
                src_y: 0,
            },
            size.0 as usize,
            size.1 as usize,
            &mut calibrated,
        );
        transform_icc.apply(&mut calibrated);
        bridge.set_phase_timing_enabled(true);
        for (revision, transform) in (1..).zip(ORIENTATIONS) {
            let view =
                OutputView::at(crate::shell::render::OutputId(0), (0, 0), size).oriented(transform);
            let (texture, _) = paint_output_offscreen_as(
                &mut bridge,
                &scene,
                &view,
                full_frame(revision),
                (wgpu::TextureFormat::Bgra8Unorm, Some(&lut)),
            );
            assert_eq!(
                bridge
                    .last_phase_timing()
                    .expect("phase timing")
                    .readback_bytes,
                0,
                "{transform:?}: a calibrated output reads nothing back"
            );
            let gpu = read_target(&bridge, &texture);
            let (pw, ph) = view.target_size();
            let mut cpu = vec![0_u8; (pw * ph * 4) as usize];
            swizzle_subrect_rgba_to_xrgb8888_composited(
                SubrectSource {
                    src_rgba: &calibrated,
                    src_w: size.0 as usize,
                    src_h: size.1 as usize,
                    src_x: 0,
                    src_y: 0,
                },
                &mut cpu,
                (pw * 4) as usize,
                (pw as usize, ph as usize),
                transform,
                None,
            );
            let max_delta = gpu
                .chunks_exact(4)
                .zip(cpu.chunks_exact(4))
                .flat_map(|(g, c)| [0, 1, 2].map(|i| i32::from(g[i].abs_diff(c[i]))))
                .max()
                .unwrap_or(0);
            assert!(
                max_delta <= panel.max_code_delta,
                "{transform:?}: the GPU calibration differs from the readback's qcms pass by {max_delta} code values"
            );
            // Without the profile the same output is further from the readback than that.
            let uncalibrated = paint_output_pixels(&mut bridge, &scene, &view, revision + 100);
            let uncalibrated_delta = uncalibrated
                .chunks_exact(4)
                .zip(cpu.chunks_exact(4))
                .flat_map(|(g, c)| [0, 1, 2].map(|i| i32::from(g[i].abs_diff(c[i]))))
                .max()
                .unwrap_or(0);
            assert!(
                uncalibrated_delta > panel.max_code_delta,
                "{transform:?}: the profile is visible ({uncalibrated_delta} code values)"
            );
        }
    }

    #[test]
    fn an_hdr_output_ignores_its_icc_profile() {
        // The profile describes the panel's SDR response; in HDR the PQ encode replaces it,
        // on the GPU as on the readback.
        let size = (90, 52);
        let lut = IccLut::from_transform(&super::super::icc_lut::tests::adobe_rgb().transform());
        let mut bridge = required_bridge(size.0, size.1);
        let scene = asymmetric_scene(size);
        let view = OutputView::at(crate::shell::render::OutputId(0), (0, 0), size)
            .oriented(OutputTransform::Rotate90);
        let pq = |bridge: &mut VelloDrmBridge, revision, color_lut| {
            let (texture, _) = paint_output_offscreen_as(
                bridge,
                &scene,
                &view,
                full_frame(revision),
                (wgpu::TextureFormat::Rgb10a2Unorm, color_lut),
            );
            read_target(bridge, &texture)
        };
        let plain = pq(&mut bridge, 1, None);
        let profiled = pq(&mut bridge, 2, Some(&lut));
        assert_eq!(plain, profiled);
    }

    /// A one-strip frame as layers on a `size` output: the asymmetric scene
    /// behind, a strip twice the output wide read `scroll_x` pixels in, and
    /// a chip of chrome above.
    fn pan_layers(size: (u32, u32), scroll_x: u32) -> crate::shell::frame::FrameCache {
        use vello::kurbo::{Circle, Rect};
        let (w, h) = (f64::from(size.0), f64::from(size.1));
        let mut strip = Scene::new();
        for (x, green, blue) in [
            (4.0, 0, 255),
            (37.0, 40, 225),
            (81.0, 80, 195),
            (122.0, 120, 165),
            (160.0, 160, 135),
        ] {
            strip.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(200, green, blue, 230),
                None,
                &Rect::new(x, 6.0, x + 29.5, h * 0.55),
            );
            strip.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(20, 20, green),
                None,
                &Circle::new((x + 14.0, h * 0.3), 6.5),
            );
        }
        let mut chrome = Scene::new();
        chrome.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(240, 240, 240, 200),
            None,
            &Rect::new(w * 0.7, 2.0, w - 3.0, 9.0),
        );
        crate::shell::frame::FrameCache {
            width: size.0.saturating_mul(2),
            strips: vec![crate::shell::frame::StripBlit {
                dest_x: 0,
                dest_y: 0,
                dest_w: size.0,
                dest_h: size.1.saturating_mul(2).checked_div(3).unwrap_or(0),
                scroll_x,
            }],
            generation: 1,
            chrome_damage: None,
            layers: crate::shell::frame::FrameLayers {
                backdrop: asymmetric_scene(size),
                workspace: Scene::new(),
                strips: vec![strip],
                strip_parts: vec![None],
                wheel: None,
                chrome,
            },
        }
    }

    /// A strip pan on a turned, calibrated or HDR output is composited from
    /// its layers, as on a plain one, and shows what rendering the frame's
    /// scene through that output shows: the final pass turns, calibrates or
    /// PQ-encodes the composite as it would the render.
    #[test]
    fn a_turned_calibrated_or_hdr_output_composites_a_strip_pan_from_its_layers() {
        let size = (90, 52);
        let lut = IccLut::from_transform(&super::super::icc_lut::tests::adobe_rgb().transform());
        let sdr = wgpu::TextureFormat::Bgra8Unorm;
        let hdr = wgpu::TextureFormat::Rgb10a2Unorm;
        // Channel steps for 8-bit outputs, code values for the 10-bit PQ
        // encode: a composite differs from a single render by a step of
        // rounding, which the PQ curve stretches in the darks.
        let cases: [(&str, wgpu::TextureFormat, Option<&IccLut>, i64); 3] = [
            ("SDR", sdr, None, 2),
            ("calibrated", sdr, Some(&lut), 3),
            ("HDR", hdr, None, 12),
        ];
        for (name, format, color_lut, tolerance) in cases {
            for transform in ORIENTATIONS {
                let label = format!("{name}, {transform:?}");
                let view = OutputView::at(crate::shell::render::OutputId(0), (0, 0), size)
                    .oriented(transform);
                let mut layered = required_bridge(size.0, size.1);
                let mut rendered = required_bridge(size.0, size.1);
                let mut shown = Vec::new();
                for (revision, scroll_x) in [(1, 0), (2, 7)] {
                    let cache = pan_layers(size, scroll_x);
                    let scene = super::super::strip_layers::compose(&cache);
                    let (texture, _) = paint_output_offscreen_with(
                        &mut layered,
                        (&scene, Some(&cache)),
                        &view,
                        full_frame(revision),
                        (format, color_lut),
                    );
                    shown = read_target(&layered, &texture);
                    let work = layered
                        .target
                        .last_layer_work_for_test()
                        .unwrap_or_else(|| panic!("{label}: the frame came as layers"));
                    if revision == 2 {
                        assert!(
                            matches!(
                                work.workspace,
                                super::super::strip_layers::WorkspaceWork::Pan { .. }
                            ),
                            "{label}: the pan copies the strip ({work:?})"
                        );
                    }
                }
                let cache = pan_layers(size, 7);
                let scene = super::super::strip_layers::compose(&cache);
                let (texture, _) = paint_output_offscreen_with(
                    &mut rendered,
                    (&scene, None),
                    &view,
                    full_frame(1),
                    (format, color_lut),
                );
                let expected = read_target(&rendered, &texture);
                assert_eq!(shown.len(), expected.len(), "{label}");
                let channels = |bytes: &[u8]| -> [i64; 3] {
                    if format == hdr {
                        let word = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                        [0, 10, 20].map(|shift| i64::from((word >> shift) & 0x3FF))
                    } else {
                        [0, 1, 2].map(|index| i64::from(bytes[index]))
                    }
                };
                let max_delta = shown
                    .chunks_exact(4)
                    .zip(expected.chunks_exact(4))
                    .flat_map(|(a, b)| {
                        let (a, b) = (channels(a), channels(b));
                        [0, 1, 2].map(|index| (a[index] - b[index]).abs())
                    })
                    .max()
                    .unwrap_or(0);
                assert!(
                    max_delta <= tolerance,
                    "{label}: the composited pan differs from the rendered scene by {max_delta}"
                );
            }
        }
    }

    #[test]
    fn dropped_direct_frame_makes_the_output_render_in_full() {
        // Two 100x100 outputs side by side; damage falls only on the left one.
        let mut bridge = required_bridge(200, 100);
        let scene = Scene::new();
        let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
        let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
        paint_output_pixels(&mut bridge, &scene, &left, 1);
        paint_output_pixels(&mut bridge, &scene, &right, 1);
        let left_only = [vello::FineDamageRect {
            x: 10,
            y: 10,
            width: 20,
            height: 20,
        }];
        let damage = |revision| SceneDamage {
            revision,
            previous_revision: Some(revision - 1),
            regions: Some(&left_only),
        };
        assert!(
            !bridge.keep_unchanged_output(&left, damage(2)),
            "the damaged output renders"
        );
        assert!(
            bridge.keep_unchanged_output(&right, damage(2)),
            "an undamaged output holding the previous revision keeps its image"
        );
        // A busy flip dropped the right output's frame: its surface is ahead of KMS.
        bridge.invalidate_output_view(&right);
        assert!(
            !bridge.keep_unchanged_output(&right, damage(3)),
            "after a dropped flip the output must not count as unchanged"
        );
    }

    #[test]
    fn per_output_view_never_bands_two_4k_heads() {
        // Two 3840x2160 heads side by side span 7680x2160: 30x9 = 270 Vello bins, past the
        // 256 one render can address, so the combined target rendered in two bands. While
        // both render direct the bridge's target is the head at the origin (the DRM
        // presenter's `bridge_extent`), and each head is one render.
        let head = (3840, 2160);
        assert_eq!(
            RetainedGpuTarget::band_rows((7680, 2160)),
            Some(2048),
            "the combined desktop is past the bin limit"
        );
        assert_eq!(
            RetainedGpuTarget::band_rows(head),
            None,
            "one head fits one render"
        );
        let mut bridge = required_bridge(head.0, head.1);
        let mut scene = Scene::new();
        // A window spanning both heads.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(40, 120, 200),
            None,
            &vello::kurbo::Rect::new(3000.0, 400.0, 4800.0, 1600.0),
        );
        let views = [
            OutputView::at(crate::shell::render::OutputId(0), (0, 0), head),
            OutputView::at(crate::shell::render::OutputId(1), (3840, 0), head),
        ];
        for (index, view) in views.iter().enumerate() {
            let (_, stats) = paint_output_offscreen(&mut bridge, &scene, view, full_frame(1));
            assert_eq!(
                (stats.render_attempts, bridge.last_render_attempts()),
                (1, 1),
                "head {index} renders in one pass, not in bands"
            );
        }
        assert!(
            bridge.view_is_whole_scene(&views[0]),
            "the head at the origin renders straight into the bridge's target"
        );
        assert!(bridge.target.band.is_none(), "no band target was needed");
        assert!(
            bridge
                .output_surfaces
                .iter()
                .all(|(_, surface)| surface.size == head && surface.band.is_none()),
            "the other head's surface is its own size and never banded"
        );
        assert_eq!(bridge.size(), head, "no target spans both heads");
    }

    #[test]
    fn an_undamaged_origin_output_keeps_its_image_in_the_bridge_target() {
        // The bridge's target is the left output's: damage on the right one leaves it as it is.
        let mut bridge = required_bridge(100, 100);
        let scene = Scene::new();
        let left = OutputView::at(crate::shell::render::OutputId(0), (0, 0), (100, 100));
        let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
        assert!(bridge.view_is_whole_scene(&left));
        paint_output_pixels(&mut bridge, &scene, &left, 1);
        paint_output_pixels(&mut bridge, &scene, &right, 1);
        let right_only = [vello::FineDamageRect {
            x: 120,
            y: 10,
            width: 20,
            height: 20,
        }];
        let damage = SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&right_only),
        };
        assert!(
            bridge.keep_unchanged_output(&left, damage),
            "damage on the other output neither renders nor flips this one"
        );
        assert!(
            !bridge.keep_unchanged_output(&right, damage),
            "the damaged output renders"
        );
        assert_eq!(
            bridge.target.last_successful_revision,
            Some(2),
            "the kept target holds the new revision, so the next frame repairs it sparsely"
        );
    }

    #[test]
    fn a_deferred_output_repairs_its_owed_damage_sparsely() {
        // The right output skips revision 2 (its flip is pending), whose damage falls on it.
        // Revision 3 damages only the left output; the right one must still show revision 2.
        let mut bridge = required_bridge(100, 100);
        let right = OutputView::at(crate::shell::render::OutputId(1), (100, 0), (100, 100));
        let blue = Color::from_rgb8(0, 0, 255);
        let red = Color::from_rgb8(255, 0, 0);
        let mut before = Scene::new();
        before.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            blue,
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 200.0, 100.0),
        );
        let mut after = Scene::new();
        after.append(&before, None);
        after.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            red,
            None,
            &vello::kurbo::Rect::new(110.0, 10.0, 130.0, 30.0),
        );
        paint_output_pixels(&mut bridge, &before, &right, 1);
        let on_right = [vello::FineDamageRect {
            x: 110,
            y: 10,
            width: 20,
            height: 20,
        }];
        assert!(
            bridge.defer_output(
                &right,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&on_right),
                },
            ),
            "a surface holding the previous revision records the skipped damage"
        );
        let on_left = [vello::FineDamageRect {
            x: 10,
            y: 50,
            width: 10,
            height: 10,
        }];
        let next = SceneDamage {
            revision: 3,
            previous_revision: Some(2),
            regions: Some(&on_left),
        };
        assert!(
            !bridge.keep_unchanged_output(&right, next),
            "owed damage makes the output render although this frame's damage is elsewhere"
        );
        let (stats, pixels) = paint_output_damage(&mut bridge, &after, &right, next);
        assert_eq!(
            (stats.sparse_attempts, stats.full_attempts),
            (1, 0),
            "the owed damage is repaired sparsely"
        );
        let pixel = |x: usize, y: usize| {
            pixels
                .chunks_exact(4)
                .nth(y.saturating_mul(100).saturating_add(x))
                .expect("the pixel lies on the output")
                .to_vec()
        };
        // The scanout stand-in is BGRA.
        assert_eq!(
            pixel(20, 20),
            [0, 0, 255, 255],
            "the skipped frame's change shows"
        );
        assert_eq!(pixel(60, 60), [255, 0, 0, 255], "the rest is unchanged");

        // Pixels that do not hold the previous revision cannot owe a frame: the output's next
        // render is full.
        assert!(!bridge.defer_output(
            &right,
            SceneDamage {
                revision: 9,
                previous_revision: Some(7),
                regions: Some(&on_right),
            },
        ));
        let (stats, _) = paint_output_damage(
            &mut bridge,
            &after,
            &right,
            SceneDamage {
                revision: 10,
                previous_revision: Some(9),
                regions: Some(&on_left),
            },
        );
        assert_eq!(
            stats.sparse_attempts, 0,
            "an output that lost its debt renders in full"
        );
    }

    impl VelloDrmBridge {
        /// Test-only CPU oracle from the exact staging image settled by the preceding render.
        /// It adds no GPU render or copy, and is deliberately unavailable to normal callers.
        pub(crate) fn swizzle_current_staging_for_test(
            &self,
            dst: &mut [u8],
            dst_pitch: usize,
        ) -> Result<(), BridgeError> {
            if !self.retained_staging_valid {
                return Err(BridgeError::Map(
                    "test staging reference requested before a complete paint".to_owned(),
                ));
            }
            Self::validate_complete_xrgb_destination(dst, dst_pitch, self.target.size)?;
            let mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
            let result = Self::validate_complete_mapped_xrgb_source(MappedXrgbSource {
                bytes: &mapped,
                padded_bytes_per_row: self.padded_bytes_per_row as usize,
                width: self.target.size.0 as usize,
                height: self.target.size.1 as usize,
            })
            .map(|()| {
                Self::swizzle_mapped_xrgb8888(
                    MappedXrgbSource {
                        bytes: &mapped,
                        padded_bytes_per_row: self.padded_bytes_per_row as usize,
                        width: self.target.size.0 as usize,
                        height: self.target.size.1 as usize,
                    },
                    dst,
                    dst_pitch,
                    false,
                );
            });
            drop(mapped);
            self.readback_buf.unmap();
            result
        }
    }

    fn rect_scene(w: u32, h: u32, color: Color) -> Scene {
        let mut scene = Scene::new();
        let rect = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
        scene
    }

    /// A sparse frame on the combined readback (an overview fade, a hover)
    /// renders the scene once inside its damage and reads back only the
    /// damage, never the combined desktop. The encoded desktop it leaves is
    /// the one a full render of the same scene gives.
    #[test]
    fn encoded_regions_read_back_only_the_damage() {
        let (w, h) = (256_u32, 128_u32);
        let backdrop = Color::from_rgb8(0x20, 0x40, 0x60);
        let frame = |shade: u8| {
            let mut scene = rect_scene(w, h, backdrop);
            let panel = vello::kurbo::Rect::new(40.0, 20.0, 88.0, 44.0);
            let color = Color::from_rgba8(0xf0, shade, 0x10, 0xa0);
            scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &panel);
            scene
        };
        let damage = [
            vello::FineDamageRect {
                x: 32,
                y: 16,
                width: 64,
                height: 32,
            },
            vello::FineDamageRect {
                x: 192,
                y: 96,
                width: 16,
                height: 16,
            },
        ];
        let damage_bytes = (64 * 32 + 16 * 16) * 4;
        let mut bridge = required_bridge(w, h);
        bridge.set_phase_timing_enabled(true);
        bridge.paint(&frame(0x10)).expect("full frame");
        let full = bridge.last_phase_timing().expect("full frame timing");
        assert_eq!(full.cpu_output_bytes, u64::from(w * h * 4));

        for shade in [0x60, 0xb0] {
            let scene = frame(shade);
            bridge
                .refresh_encoded_regions(&scene, &damage)
                .expect("sparse frame");
            let timing = bridge.last_phase_timing().expect("sparse frame timing");
            assert_eq!(
                timing.readback_bytes, damage_bytes,
                "only the damage is read"
            );
            assert_eq!(
                timing.cpu_output_bytes, damage_bytes,
                "only the damage is copied"
            );
            let encoded = bridge.encoded_rgba().expect("encoded desktop").to_vec();
            let mut reference = required_bridge(w, h);
            let dense = reference.paint(&scene).expect("reference frame").to_vec();
            assert!(encoded == dense, "the repaired desktop is the full render");
        }
        assert_eq!(
            (
                bridge.last_sparse_render_attempts(),
                bridge.last_full_render_attempts()
            ),
            (1, 0),
            "a frame after a repaired one renders only its damage on the GPU"
        );
        assert!(
            bridge
                .refresh_encoded_regions(
                    &frame(0),
                    &[vello::FineDamageRect {
                        x: 200,
                        y: 0,
                        width: 64,
                        height: 8,
                    }],
                )
                .is_err(),
            "a region leaving the desktop is not repaired"
        );
    }

    /// Scratch pooled for a full frame or an old size is released once the
    /// session stops rendering that way, so the shell's memory comes back
    /// down after a lock screen, an overlay or a mode change.
    #[test]
    fn pooled_scratch_follows_the_frames_the_session_still_renders() {
        let busy = |w: u32, h: u32| {
            let mut scene = Scene::new();
            for i in 0..400_u32 {
                let x = f64::from(i * 37 % w);
                let y = f64::from(i * 53 % h);
                let rect = vello::kurbo::Rect::new(x, y, x + 24.0, y + 24.0);
                let color = Color::from_rgb8(u8::try_from(i % 251).expect("below 256"), 0x40, 0x80);
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
            }
            scene
        };
        let pooled =
            |bridge: &VelloDrmBridge| bridge.target.vello().memory_stats().pooled_buffer_bytes;
        let mut bridge = required_bridge(1024, 768);
        let mut pixels = vec![0; 1024 * 768 * 4];
        let scene = busy(1024, 768);
        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: None,
                },
                &mut pixels,
                1024 * 4,
            )
            .expect("full frame");
        let after_full = pooled(&bridge);
        assert!(after_full > 0, "a full frame pools its scratch");

        let rects = [vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 32,
            height: 32,
        }];
        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&rects),
                },
                &mut pixels,
                1024 * 4,
            )
            .expect("damage frame");
        assert_eq!(bridge.last_sparse_render_attempts(), 1);
        let after_damage = pooled(&bridge);
        assert!(
            after_damage < after_full,
            "damage frames must not keep the full frame's scratch: {after_damage} >= {after_full}"
        );

        bridge
            .paint_damage_into_xrgb8888(
                &scene,
                SceneDamage {
                    revision: 3,
                    previous_revision: Some(2),
                    regions: None,
                },
                &mut pixels,
                1024 * 4,
            )
            .expect("full frame again");
        let before_resize = pooled(&bridge);
        bridge.resize(320, 240).expect("resize");
        let mut small = vec![0; 320 * 240 * 4];
        bridge
            .paint_damage_into_xrgb8888(
                &Scene::new(),
                SceneDamage {
                    revision: 4,
                    previous_revision: None,
                    regions: None,
                },
                &mut small,
                320 * 4,
            )
            .expect("frame at the new size");
        let after_resize = pooled(&bridge);
        assert!(
            after_resize < before_resize,
            "a mode change must not keep the old size's scratch: {after_resize} >= {before_resize}"
        );
    }

    /// A target keeps a frame proven unchanged relative to the one it holds,
    /// as the DRM presenter keeps a lone output's image, so the next change
    /// renders sparse; it keeps nothing else.
    #[test]
    fn gpu_target_keeps_an_unchanged_frame_and_the_next_change_renders_sparse() {
        let mut target = required_target(128, 128);
        let deadline = || Instant::now() + Duration::from_secs(10);
        target
            .render_until(&Scene::new(), version(1, None, None), deadline())
            .expect("seed");
        let changed = [vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        }];
        assert!(
            !target.keep_unchanged(version(2, Some(1), Some(&changed))),
            "a frame that changes pixels is rendered"
        );
        assert!(
            !target.keep_unchanged(version(2, Some(7), Some(&[]))),
            "a frame unchanged from a revision the target does not hold is rendered"
        );
        assert!(
            !target.keep_unchanged(version(2, Some(1), None)),
            "a frame without damage authority is rendered"
        );
        assert!(target.keep_unchanged(version(2, Some(1), Some(&[]))));
        let stats = target
            .render_until(
                &Scene::new(),
                version(3, Some(2), Some(&changed)),
                deadline(),
            )
            .expect("change after the kept frame");
        assert_eq!(
            (stats.sparse_attempts, stats.full_attempts),
            (1, 0),
            "the change after a kept frame repairs only its damage"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gpu_target_refreshes_dense_whenever_retained_damage_cannot_be_trusted() {
        // Phase `gpu_target_unchanged_clear_color_preserves_retained_damage`.
        {
            let mut bridge = required_bridge(128, 128);
            let mut pixels = vec![0; 128 * 128 * 4];
            let scene = Scene::new();
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: None,
                    },
                    &mut pixels,
                    512,
                )
                .expect("seed");
            let identity = bridge.xrgb_history_identity(512, 1);
            bridge.set_base_color(bridge.target.base_color);
            let rects = [vello::FineDamageRect {
                x: 16,
                y: 16,
                width: 16,
                height: 16,
            }];
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 2,
                        previous_revision: Some(1),
                        regions: Some(&rects),
                    },
                    &mut pixels,
                    512,
                )
                .expect("retained repaint");
            assert_eq!(bridge.last_sparse_render_attempts(), 1);
            assert_eq!(bridge.xrgb_history_identity(512, 1), identity);
        }

        // Phase `gpu_target_sparse_without_staging_matches_dense`.
        {
            let mut target = required_target(129, 128);
            let stats = target
                .render_until(
                    &Scene::new(),
                    version(1, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(stats.framebuffer_staging_bytes, 0);
            assert_eq!(stats.framebuffer_copy_bytes, 0);
            let seed = target_pixels(&target);
            let allocation = target.texture.clone();
            let region = vello::FineDamageRect {
                x: 16,
                y: 16,
                width: 16,
                height: 16,
            };
            let scene = region_scene(region, Color::WHITE);
            let outcome = target
                .render_until(
                    &scene,
                    version(2, Some(1), Some(&[region])),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert!(outcome.damage_admitted);
            assert_eq!(outcome.framebuffer_staging_bytes, 0);
            assert_eq!(outcome.framebuffer_copy_bytes, 0);
            assert_eq!(
                outcome.render_attempts,
                outcome.sparse_attempts + outcome.full_attempts
            );
            assert_eq!(outcome.completion_markers, outcome.render_attempts);
            assert_eq!(
                target.texture, allocation,
                "sparse render must reuse actual target allocation"
            );
            let actual = target_pixels(&target);
            for y in 0..128 {
                for x in 0..129 {
                    if !(16..32).contains(&x) || !(16..32).contains(&y) {
                        let offset = (y * 129 + x) * 4;
                        assert_eq!(
                            &actual[offset..offset + 4],
                            &seed[offset..offset + 4],
                            "untouched pixel {x},{y}"
                        );
                    }
                }
            }
            let mut oracle = required_target(129, 128);
            oracle
                .render_until(
                    &scene,
                    version(2, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(actual, target_pixels(&oracle));
        }

        // Phase `gpu_target_revision_gap_and_resize_force_dense`.
        {
            let mut target = required_target(128, 128);
            let regions = [vello::FineDamageRect {
                x: 16,
                y: 16,
                width: 16,
                height: 16,
            }];
            let scene = region_scene(regions[0], Color::WHITE);
            target
                .render_until(
                    &scene,
                    version(1, None, None),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            let outcome = target
                .render_until(
                    &scene,
                    version(3, Some(2), Some(&regions)),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert!(!outcome.damage_admitted);
            assert_eq!(outcome.sparse_attempts, 0);
            target.resize(128, 128).unwrap();
            assert!(target.completed_view().is_none());
            let outcome = target
                .render_until(
                    &scene,
                    version(4, Some(3), Some(&regions)),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(outcome.sparse_attempts, 0);
            assert!(matches!(
                target.resize(0, 128),
                Err(BridgeError::InvalidSize(_))
            ));
            assert!(target.completed_view().is_none());
            let limit = target.device.limits().max_texture_dimension_2d;
            assert!(matches!(
                target.resize(limit + 1, 128),
                Err(BridgeError::InvalidSize(_))
            ));
            let old = target.texture.clone();
            target.resize(160, 128).unwrap();
            assert_ne!(old, target.texture);
            assert_eq!(target.size(), (160, 128));
            let outcome = target
                .render_until(
                    &scene,
                    version(5, Some(4), Some(&regions)),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            assert_eq!(outcome.sparse_attempts, 0);
            target.set_base_color(Color::WHITE);
            assert!(target.completed_view().is_none());
        }

        // Phase `gpu_target_xrgb_staging_gap_gets_complete_refresh`.
        {
            let mut bridge = required_bridge(129, 128);
            bridge.set_phase_timing_enabled(true);
            let mut actual = vec![0; 129 * 128 * 4];
            bridge
                .paint_damage_into_xrgb8888(
                    &Scene::new(),
                    version(1, None, None),
                    &mut actual,
                    129 * 4,
                )
                .unwrap();
            let first = vello::FineDamageRect {
                x: 16,
                y: 16,
                width: 16,
                height: 16,
            };
            let second = vello::FineDamageRect {
                x: 64,
                y: 64,
                width: 16,
                height: 16,
            };
            let mut scene = region_scene(first, Color::WHITE);
            bridge
                .target
                .render_until(
                    &scene,
                    version(2, Some(1), Some(&[first])),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            scene.append(&region_scene(second, Color::from_rgb8(200, 30, 50)), None);
            bridge
                .paint_damage_into_xrgb8888(
                    &scene,
                    version(3, Some(2), Some(&[second])),
                    &mut actual,
                    129 * 4,
                )
                .unwrap();
            assert_eq!(
                bridge.last_sparse_render_attempts(),
                1,
                "target continuity survives GPU-only middle frame"
            );
            assert_eq!(
                bridge.last_phase_timing().unwrap().readback_bytes,
                129 * 128 * 4,
                "stale staging needs complete refresh in same submission"
            );
            let mut oracle = required_bridge(129, 128);
            let mut expected = vec![0; actual.len()];
            oracle
                .paint_into_xrgb8888(&scene, &mut expected, 129 * 4)
                .unwrap();
            assert_eq!(actual, expected);
            assert_eq!(
                bridge.staging_content,
                Some((bridge.target.geometry_generation, Some(3)))
            );
        }
    }

    #[test]
    fn default_area_frames_keep_stable_pixels_after_counter_settlement() {
        use crate::shell::output_scene::OutputGeometry;

        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove("VELLO_CANONICAL_SEGMENT_ORDER");
        env.remove("VELLO_BUMP_BUDGET");
        env.set("TILO_CLOCK", "09:41");
        let mut target = required_target(3_840, 2_160);
        assert!(target.renderer.canonical_segment_order_available());
        let mut frame = PopulatedShellFixture::new(
            (3_840, 2_160),
            ui::theme::Theme::high_contrast().with_reduce_motion(true),
        );
        frame.set_output_geometry(vec![
            OutputGeometry::unscaled(0.0, 0.0, 1920.0, 2160.0).with_id(0),
            OutputGeometry::unscaled(1920.0, 0.0, 1920.0, 2160.0).with_id(1),
        ]);
        frame.relayout();
        let scene = frame.paint();
        target
            .render_until(
                &scene,
                version(1, None, None),
                Instant::now() + Duration::from_secs(10),
            )
            .unwrap();
        let expected = target_pixels(&target);
        // Repaint the high-contrast label edges implicated by the overview failure.
        // A whole half-desktop is deliberately ineligible under the profitability gate.
        let damage = [vello::FineDamageRect {
            x: 640,
            y: 128,
            width: 128,
            height: 64,
        }];
        for revision in 2_u64..=9 {
            let sparse = revision.is_multiple_of(2);
            let started = Instant::now();
            target
                .render_until(
                    &scene,
                    version(
                        revision,
                        revision.checked_sub(1),
                        sparse.then_some(&damage[..]),
                    ),
                    Instant::now() + Duration::from_secs(10),
                )
                .unwrap();
            eprintln!(
                "default Area revision {revision}: sparse={sparse}, completed_render_us={}",
                started.elapsed().as_micros()
            );
            assert_eq!(
                target.renderer.poll_bump_budget(&target.device).unwrap(),
                vello::BumpBudgetPoll::Complete { overflowed: false },
                "the next adaptive frame must not inherit a pending measurement"
            );
            assert_eq!(target.last_render_attempts, 1);
            if sparse {
                assert_eq!(target.last_canonical_segment_order_sparse_attempts, 1);
                assert_eq!(target.last_full_render_attempts, 0);
            } else {
                assert_eq!(target.last_canonical_segment_order_full_attempts, 1);
                assert_eq!(target.last_sparse_render_attempts, 0);
            }
            let actual = target_pixels(&target);
            let first = actual
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected);
            assert!(
                actual == expected,
                "revision {revision}: {} versus {} bytes; first differing byte {first:?}",
                actual.len(),
                expected.len()
            );
        }
    }

    fn required_target(width: u32, height: u32) -> RetainedGpuTarget {
        // Device and renderer creation read renderer controls from the
        // environment; hold the lock so a sibling test's override of them
        // cannot leak into this target.
        let _env = tilo_test_env::lock();
        let mut context = RenderContext::new();
        let index = context
            .device(None)
            .block_on()
            .expect("target tests require a real adapter");
        eprintln!(
            "target adapter: {:?}",
            context.devices[index].adapter.get_info()
        );
        RetainedGpuTarget::new(&context.devices[index], width, height).expect("target")
    }

    /// A target created while `key` is `value`; the override ends once the
    /// renderer has read it.
    fn required_target_with_env(
        key: &str,
        value: &str,
        width: u32,
        height: u32,
    ) -> RetainedGpuTarget {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(key, value);
        required_target(width, height)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gpu_target_failed_aborted_or_poisoned_frames_hide_output_and_publish_nothing() {
        // Phase `gpu_target_failure_and_device_loss_hide_output_until_reconstruction`.
        {
            // Phase `gpu_target_failure_hides_view_until_reconstruction`.
            {
                let mut target = required_target(128, 128);
                let scene = Scene::new();
                let damage = SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: None,
                };
                target
                    .render_until(&scene, damage, Instant::now() + Duration::from_secs(10))
                    .expect("seed");
                assert!(target.completed_view().is_some());
                let result = target.render_until(&scene, damage, Instant::now());
                assert!(
                    matches!(result, Err(BridgeError::DeadlineExceeded)),
                    "expired call must fail: {result:?}"
                );
                assert!(target.completed_view().is_none());
                assert_eq!(target.last_render_attempts, 0);
            }

            // Phase `gpu_target_counter_failures_and_device_loss_hide_output`.
            {
                for error in [
                    vello::BumpBudgetError::MapFailed,
                    vello::BumpBudgetError::MissingDownload,
                    vello::BumpBudgetError::InvalidDownload,
                    vello::BumpBudgetError::Poll("injected wrong submission index".to_owned()),
                ] {
                    let mut target = required_target(128, 128);
                    let mut test = completion_clock();
                    test.counters.push_back(Err(error));
                    let deadline = test.start + Duration::from_secs(1);
                    target.completion_test = Some(test);
                    let result =
                        target.render_until(&Scene::new(), version(1, None, None), deadline);
                    assert!(matches!(
                        result,
                        Err(BridgeError::BumpReadback(_) | BridgeError::DevicePoll(_))
                    ));
                    assert_eq!(target.last_render_attempts, 1);
                    assert!(target.completed_view().is_none() && target.poisoned);
                }
                let mut target = required_target(128, 128);
                target
                    .render_until(
                        &Scene::new(),
                        version(1, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                target.device.destroy();
                let _ = target.device.poll(wgpu::PollType::Poll);
                assert!(target.completed_view().is_none());
                assert!(matches!(
                    target.render_until(
                        &Scene::new(),
                        version(2, None, None),
                        Instant::now() + Duration::from_secs(10)
                    ),
                    Err(BridgeError::DeviceLost(_))
                ));
                assert_eq!(target.last_render_attempts, 0);
            }

            // Phase `gpu_target_device_loss_at_marker_hides_output`.
            {
                let mut target = required_target(128, 128);
                let mut test = completion_clock();
                test.destroy_at_marker = true;
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                let result = target.render_until(&Scene::new(), version(1, None, None), deadline);
                assert!(
                    matches!(result, Err(BridgeError::DeviceLost(_))),
                    "marker-time device loss must not publish: {result:?}"
                );
                assert!(target.completed_view().is_none());
                assert_eq!(target.completion_markers, 1);
            }
        }

        // Phase `gpu_target_aborted_or_poisoned_frames_submit_and_publish_nothing`.
        {
            // Phase `gpu_target_encoded_dense_and_sparse_abort_before_submission`.
            {
                for sparse in [false, true] {
                    let mut target = required_target(128, 128);
                    let params = RenderParams {
                        base_color: Color::BLACK,
                        width: 128,
                        height: 128,
                        antialiasing_method: AaConfig::Area,
                    };
                    let scene = Scene::new();
                    let mut encoded = false;
                    let result = if sparse {
                        let damage = [vello::FineDamageRect {
                            x: 16,
                            y: 16,
                            width: 16,
                            height: 16,
                        }];
                        target
                            .renderer
                            .render_to_texture_with_damage_and_post_render_checked(
                                &target.device,
                                &target.queue,
                                &scene,
                                &target.view,
                                &params,
                                &damage,
                                |_| encoded = true,
                                || false,
                            )
                            .map(|_| ())
                    } else {
                        target.renderer.render_to_texture_with_post_render_checked(
                            &target.device,
                            &target.queue,
                            &scene,
                            &target.view,
                            &params,
                            |_| encoded = true,
                            || false,
                        )
                    };
                    assert!(
                        encoded,
                        "gate test must reach actual post-encoding boundary"
                    );
                    assert!(
                        matches!(result, Err(vello::Error::SubmissionAborted)),
                        "gate must cancel encoded render: {result:?}"
                    );
                }
            }

            // Phase `gpu_target_expiry_after_encoding_submits_no_render_or_marker`.
            {
                for sparse in [false, true] {
                    let mut target = required_target(128, 128);
                    target
                        .render_until(
                            &Scene::new(),
                            version(1, None, None),
                            Instant::now() + Duration::from_secs(10),
                        )
                        .unwrap();
                    let mut test = completion_clock();
                    test.encode_elapsed = Duration::from_secs(2);
                    let deadline = test.start + Duration::from_secs(1);
                    target.completion_test = Some(test);
                    let regions = [vello::FineDamageRect {
                        x: 16,
                        y: 16,
                        width: 16,
                        height: 16,
                    }];
                    let result = target.render_until(
                        &Scene::new(),
                        version(2, Some(1), sparse.then_some(&regions)),
                        deadline,
                    );
                    assert!(matches!(result, Err(BridgeError::DeadlineExceeded)));
                    assert_eq!(target.last_render_attempts, 0);
                    assert_eq!(target.completion_markers, 0);
                    assert!(target.completed_view().is_none() && target.poisoned);
                }
            }

            // Phase `gpu_target_aborted_encoding_does_not_publish_timestamps`.
            {
                let mut target = required_target_with_env("VELLO_GPU_TIMESTAMPS", "1", 128, 128);
                let mut test = completion_clock();
                test.encode_elapsed = Duration::from_secs(2);
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                assert!(matches!(
                    target.render_until(&Scene::new(), version(1, None, None), deadline),
                    Err(BridgeError::DeadlineExceeded)
                ));
                assert_eq!(
                    target.vello().gpu_timestamp_latest_submission_id(),
                    None,
                    "aborted recording must not advertise a submitted timestamp identity"
                );
                assert!(target.vello_mut().take_gpu_timestamp_sample().is_none());
                assert!(target.completed_view().is_none());
            }

            // Phase `gpu_target_late_counter_callback_cannot_publish_replacement`.
            {
                let mut context = RenderContext::new();
                let id = context.device(None).block_on().expect("real GPU adapter");
                let gpu = &context.devices[id];
                assert!(matches!(
                    RetainedGpuTarget::new(gpu, 0, 128),
                    Err(BridgeError::InvalidSize(_))
                ));
                assert!(matches!(
                    RetainedGpuTarget::new(
                        gpu,
                        gpu.device.limits().max_texture_dimension_2d + 1,
                        128
                    ),
                    Err(BridgeError::InvalidSize(_))
                ));
                let mut old = RetainedGpuTarget::new(gpu, 128, 128).unwrap();
                let mut test = completion_clock();
                test.failure = Some(WaitPhase::Marker);
                let deadline = test.start + Duration::from_secs(1);
                old.completion_test = Some(test);
                assert!(matches!(
                    old.render_until(&Scene::new(), version(1, None, None), deadline),
                    Err(BridgeError::DeadlineExceeded)
                ));
                let mut replacement = RetainedGpuTarget::new(gpu, 128, 128).unwrap();
                gpu.device
                    .poll(wgpu::PollType::Wait {
                        submission_index: None,
                        timeout: Some(Duration::from_secs(1)),
                    })
                    .unwrap();
                assert!(matches!(
                    old.renderer.poll_bump_budget(&gpu.device),
                    Ok(vello::BumpBudgetPoll::Complete { .. })
                ));
                assert!(
                    old.completed_view().is_none(),
                    "late callback cannot unpoison old target"
                );
                assert!(
                    replacement.completed_view().is_none(),
                    "late callback cannot validate fresh target"
                );
                replacement
                    .render_until(
                        &Scene::new(),
                        version(2, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                assert!(replacement.completed_view().is_some());
                assert!(old.completed_view().is_none());
            }

            // Phase `gpu_target_poisoned_shared_coarse_diagnostic_rejects_before_encoding`.
            {
                assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                    bridge
                        .shared_coarse_fine_diagnostic_into_xrgb8888(
                            scene,
                            &[vello::FineDamageRect {
                                x: 16,
                                y: 16,
                                width: 16,
                                height: 16,
                            }],
                            left,
                            right,
                            512,
                        )
                        .map(|_| ())
                });
            }

            // Phase `gpu_target_poisoned_geometry_diagnostic_rejects_before_encoding`.
            {
                assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                    bridge
                        .shared_coarse_fine_geometry_diagnostic_into_xrgb8888(
                            scene,
                            &[vello::FineDamageRect {
                                x: 16,
                                y: 16,
                                width: 16,
                                height: 16,
                            }],
                            left,
                            right,
                            512,
                        )
                        .map(|_| ())
                });
            }

            // Phase `gpu_target_poisoned_segment_replay_rejects_before_encoding`.
            {
                assert_poisoned_diagnostic_rejected(|bridge, scene, left, right| {
                    let input = vello::SegmentOrderReplayInput {
                        tile: 0,
                        logical_command: 0,
                        size_and_rule: 98,
                        backdrop: 0,
                        expected: [[0; 5]; 49],
                        left: [[0; 5]; 49],
                        right: [[0; 5]; 49],
                    };
                    bridge
                        .segment_order_replay_into_xrgb8888(scene, &input, left, right, 512)
                        .map(|_| ())
                });
            }
        }
    }

    fn completion_clock() -> CompletionTest {
        CompletionTest {
            start: Instant::now(),
            elapsed: Duration::ZERO,
            encode_elapsed: Duration::ZERO,
            fence_elapsed: Duration::ZERO,
            callback_elapsed: Duration::ZERO,
            pending_elapsed: Duration::from_millis(200),
            waits: Vec::new(),
            counter_remaining: Vec::new(),
            counter_waits: Vec::new(),
            counters: std::collections::VecDeque::new(),
            failure: None,
            destroy_at_marker: false,
            disconnect_map_callback: false,
        }
    }

    /// Recovery back-off is chosen by the error's type, never its wording: a message that reads
    /// like a timeout does not start it, and a typed timeout does whatever it says.
    #[test]
    fn only_typed_timeouts_start_backoff() {
        let mut bridge = required_bridge(64, 64);
        for phase in [
            CompletionPhase::Fence,
            CompletionPhase::MapCallback,
            CompletionPhase::CounterSettlement,
            CompletionPhase::FrameBudget,
        ] {
            bridge.recovery_not_before = None;
            bridge.note_render_failure(&BridgeError::CompletionTimeout { phase });
            assert!(
                bridge.recovery_not_before.is_some(),
                "{phase:?} timeout starts the back-off"
            );
        }
        for error in [
            BridgeError::Map("LinuxPerPhase map_async channel: disconnected".to_owned()),
            BridgeError::Map("LinuxPerPhase fence wait: Timeout".to_owned()),
            BridgeError::BumpReadback("LinuxPerPhase counter settlement timeout (1s)".to_owned()),
            BridgeError::DevicePoll("Timeout".to_owned()),
            BridgeError::DeadlineExceeded,
        ] {
            bridge.recovery_not_before = Some(Instant::now() + Duration::from_secs(1));
            bridge.note_render_failure(&error);
            assert!(
                bridge.recovery_not_before.is_none(),
                "{error:?} is not a completion timeout and fails closed"
            );
            assert_eq!(bridge.recovery_backoff, RECOVERY_BACKOFF_INITIAL);
        }
    }

    /// A map callback that can never report (its sender dropped, as on device loss) is a
    /// failure to fail closed on, not a timeout to wait out: the next frame is not held back.
    #[test]
    fn disconnected_map_channel_is_not_a_timeout() {
        let mut bridge = required_bridge(128, 128);
        let mut test = completion_clock();
        test.disconnect_map_callback = true;
        bridge.target.completion_test = Some(test);
        let mut bytes = vec![0xAB; 128 * 128 * 4];
        let result = bridge.paint_damage_into_xrgb8888(
            &Scene::new(),
            version(1, None, None),
            &mut bytes,
            512,
        );
        assert!(
            matches!(result, Err(BridgeError::Map(_))),
            "a dropped callback is a mapping failure: {result:?}"
        );
        assert!(bridge.recovery_not_before.is_none(), "no back-off");
        assert!(bytes.iter().all(|byte| *byte == 0xAB), "nothing published");
        let test = bridge.target.completion_test.as_ref().unwrap();
        assert_eq!(
            test.waits.last().map(|(phase, _)| *phase),
            Some(WaitPhase::MapCallback),
            "the failure is the callback wait's own"
        );
        // The next frame renders at once on a rebuilt target.
        bridge
            .paint_damage_into_xrgb8888(&Scene::new(), version(2, None, None), &mut bytes, 512)
            .expect("no back-off holds the next frame");
    }

    /// Pending counters are settled by one blocking wait on their submission for the whole
    /// remaining allowance, not sampled in millisecond sleeps until the deadline.
    #[test]
    fn counter_settlement_waits_on_the_submission_not_a_sleep_loop() {
        let mut target = required_target(128, 128);
        let params = RenderParams {
            base_color: Color::BLACK,
            width: 128,
            height: 128,
            antialiasing_method: AaConfig::Area,
        };
        let scene = rect_scene(128, 128, Color::WHITE);
        // A real submission whose counter readback is still in flight when settlement starts.
        let submission = target
            .renderer
            .render_to_texture_with_post_render_checked_indexed(
                &target.device,
                &target.queue,
                &scene,
                &target.view,
                &params,
                |_| {},
                || true,
            )
            .expect("submitted");
        let mut test = completion_clock();
        test.pending_elapsed = Duration::from_millis(1);
        // However the real readback stands, the first poll reports it pending.
        test.counters.push_back(Ok(vello::BumpBudgetPoll::Pending));
        let frame_deadline = test.start + LINUX_FRAME_BUDGET;
        target.completion_test = Some(test);
        let overflowed = target
            .settle_counters(
                CompletionPolicy::LinuxPerPhase { frame_deadline },
                Some(&submission),
            )
            .expect("the submission completes and its counters settle");
        assert!(!overflowed);
        let test = target.completion_test.as_ref().unwrap();
        assert_eq!(
            test.counter_waits,
            [BUMP_SETTLEMENT_WAIT],
            "one wait for the whole allowance"
        );
        assert_eq!(
            test.counter_remaining.len(),
            2,
            "pending, then complete once the submission's callbacks ran"
        );
    }

    /// Recovery attempts share the frame's one Linux budget: three slow overflowing attempts
    /// fail as a completion timeout at the budget instead of a fourth taking the frame past it.
    #[test]
    fn linux_recovery_attempts_share_one_frame_budget() {
        let mut bridge = required_bridge(128, 128);
        let mut test = completion_clock();
        test.fence_elapsed = Duration::from_secs(1);
        test.callback_elapsed = Duration::from_secs(1);
        // Each attempt's counters stay pending for 800 ms of its 1 s allowance, then overflow.
        test.counters = (0..3)
            .flat_map(|_| {
                [
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
                ]
            })
            .collect();
        bridge.target.completion_test = Some(test);
        let mut bytes = vec![0xAB; 128 * 128 * 4];
        let result = bridge.paint_damage_into_xrgb8888(
            &Scene::new(),
            version(1, None, None),
            &mut bytes,
            512,
        );
        assert!(
            matches!(
                result,
                Err(BridgeError::CompletionTimeout {
                    phase: CompletionPhase::CounterSettlement
                })
            ),
            "the third attempt's settlement runs out of frame budget: {result:?}"
        );
        assert_eq!(bridge.last_render_attempts(), 3);
        let test = bridge.target.completion_test.as_ref().unwrap();
        assert_eq!(test.elapsed, LINUX_FRAME_BUDGET);
        assert!(bridge.recovery_not_before.is_some(), "a timeout backs off");
        assert!(bytes.iter().all(|byte| *byte == 0xAB), "nothing published");
    }

    /// A GPU reset under a live session (amdgpu cancelled the context during a window-switch
    /// animation at 5K) used to panic inside `Device::poll` and take the session to a black
    /// screen. The reset here is the driver's: every fence wait answers `DeviceError::Lost`, and
    /// wgpu-core invalidates the device and runs its lost callback as it does for a real one.
    // This injects a Vulkan fence failure in the Linux DRM renderer.
    // The backend-independent device-destruction tests remain available on macOS.
    #[cfg(target_os = "linux")]
    #[test]
    fn gpu_target_a_gpu_reset_mid_session_is_survived_by_a_new_bridge() {
        const SIZE: u32 = 64;
        let pitch = SIZE as usize * 4;
        let scene = rect_scene(SIZE, SIZE, Color::WHITE);
        let mut scanout = vec![0x11; pitch * SIZE as usize];
        let mut bridge = required_bridge(SIZE, SIZE);
        bridge
            .paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut scanout, pitch)
            .expect("the frame before the reset");

        // SAFETY: the hal device is only flagged; no raw handle leaves this block.
        let hal = unsafe { bridge.target.device.as_hal::<wgpu::hal::api::Vulkan>() };
        hal.expect("the compositor renders through Vulkan")
            .simulate_device_loss();

        scanout.fill(0x11);
        let result =
            bridge.paint_damage_into_xrgb8888(&scene, version(2, None, None), &mut scanout, pitch);
        assert!(
            matches!(result, Err(BridgeError::DeviceLost(_))),
            "the frame during the reset reports the loss: {result:?}"
        );
        assert!(
            scanout.iter().all(|byte| *byte == 0x11),
            "a lost frame publishes nothing, so the last good front stays on screen"
        );
        assert!(
            bridge.device_lost().is_some(),
            "the DRM backend replaces a bridge whose device_lost() is set"
        );

        let mut replacement = required_bridge(SIZE, SIZE);
        replacement
            .paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut scanout, pitch)
            .expect("the replacement bridge paints on a new device");
        assert!(
            scanout
                .chunks_exact(4)
                .all(|pixel| pixel.get(..3) == Some(&[0xFF, 0xFF, 0xFF][..])),
            "the replacement repaints the whole scene"
        );
    }

    fn version(
        revision: u64,
        previous_revision: Option<u64>,
        regions: Option<&[vello::FineDamageRect]>,
    ) -> SceneDamage<'_> {
        SceneDamage {
            revision,
            previous_revision,
            regions,
        }
    }

    fn target_pixels(target: &RetainedGpuTarget) -> Vec<u8> {
        assert!(
            target.completed_view().is_some(),
            "test-only framebuffer readback must follow completed render_until"
        );
        let (buffer, stride) =
            VelloDrmBridge::allocate_staging(&target.device, target.size.0, target.size.1);
        let mut encoder = target
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        VelloDrmBridge::append_full_copy(
            &mut encoder,
            &target.texture,
            &buffer,
            stride,
            target.size.0,
            target.size.1,
        );
        target.queue.submit([encoder.finish()]);
        let mapped = VelloDrmBridge::map_device_readback(&target.device, &buffer)
            .expect("explicit post-return pixel oracle");
        let mut result = Vec::new();
        for row in mapped.chunks(stride as usize).take(target.size.1 as usize) {
            result.extend_from_slice(
                &row[..(target.size.0 as usize)
                    .checked_mul(4)
                    .expect("target row fits usize")],
            );
        }
        drop(mapped);
        buffer.unmap();
        result
    }

    fn region_scene(rect: vello::FineDamageRect, color: Color) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &vello::kurbo::Rect::new(
                f64::from(rect.x),
                f64::from(rect.y),
                f64::from(
                    rect.x
                        .checked_add(rect.width)
                        .expect("test rectangle x extent"),
                ),
                f64::from(
                    rect.y
                        .checked_add(rect.height)
                        .expect("test rectangle y extent"),
                ),
            ),
        );
        scene
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn gpu_target_recovery_is_budgeted_and_always_waits_on_completion() {
        // Phase `gpu_target_recovery_stays_within_attempt_phase_and_deadline_budgets`.
        {
            // Phase `gpu_target_overflow_uses_four_attempt_budget`.
            {
                let mut target = required_target(384, 256);
                let region = vello::FineDamageRect {
                    x: 144,
                    y: 80,
                    width: 96,
                    height: 96,
                };
                target
                    .render_until(
                        &Scene::new(),
                        version(1, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                let scene = opaque_rect_stack_in_rect_scene(region);
                let outcome = target
                    .render_until(
                        &scene,
                        version(2, Some(1), Some(&[region])),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                assert_eq!(outcome.sparse_attempts, 1);
                assert!(
                    outcome.full_attempts >= 1,
                    "real sparse overflow must recover densely"
                );
                assert_eq!(
                    outcome.render_attempts,
                    outcome.sparse_attempts + outcome.full_attempts
                );
                assert_eq!(outcome.completion_markers, outcome.render_attempts);
                assert!(outcome.render_attempts <= 4);
                let actual = target_pixels(&target);
                let mut oracle = required_target(384, 256);
                oracle
                    .render_until(
                        &scene,
                        version(2, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                assert_eq!(actual, target_pixels(&oracle));
                let mut test = completion_clock();
                test.counters = (0..4)
                    .map(|_| Ok(vello::BumpBudgetPoll::Complete { overflowed: true }))
                    .collect();
                let deadline = test.start + Duration::from_secs(10);
                target.completion_test = Some(test);
                let result =
                    target.render_until(&scene, version(3, Some(2), Some(&[region])), deadline);
                assert!(matches!(
                    result,
                    Err(BridgeError::RecoveryExhausted { attempts: 4 })
                ));
                assert_eq!(target.last_sparse_render_attempts, 1);
                assert_eq!(target.last_full_render_attempts, 3);
                assert_eq!(target.completion_markers, 4);
                assert!(target.completed_view().is_none());
                // Every exhausted attempt completed and settled, so the renderer and the budget
                // those attempts grew are kept: rebuilding it would restart from the initial
                // budget and overflow the same way on every frame.
                assert!(!target.poisoned);
                target
                    .render_until(&scene, version(4, None, None), deadline)
                    .expect("the next frame renders on the same target");
                assert_eq!(target_pixels(&target), target_pixels(&oracle));
            }

            // Phase `gpu_target_linux_phase_limits_survive_recovery`.
            {
                let mut bridge = required_bridge(128, 128);
                let mut test = completion_clock();
                test.fence_elapsed = Duration::from_millis(750);
                test.callback_elapsed = Duration::from_millis(750);
                test.counters = [
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
                    Ok(vello::BumpBudgetPoll::Complete { overflowed: false }),
                ]
                .into();
                bridge.target.completion_test = Some(test);
                let mut actual = vec![0; 128 * 128 * 4];
                bridge
                    .paint_damage_into_xrgb8888(
                        &Scene::new(),
                        version(1, None, None),
                        &mut actual,
                        512,
                    )
                    .unwrap();
                let test = bridge.target.completion_test.as_ref().unwrap();
                assert_eq!(
                    test.waits,
                    vec![
                        (WaitPhase::Fence, Duration::from_secs(1)),
                        (WaitPhase::MapCallback, Duration::from_secs(1)),
                        (WaitPhase::Fence, Duration::from_secs(1)),
                        (WaitPhase::MapCallback, Duration::from_secs(1))
                    ]
                );
                assert_eq!(
                    &test.counter_remaining[..4],
                    &[
                        Duration::from_millis(1000),
                        Duration::from_millis(800),
                        Duration::from_millis(600),
                        Duration::from_millis(1000)
                    ]
                );
                assert!(
                    test.elapsed > Duration::from_secs(1),
                    "Linux has separate phase allowances"
                );
                assert_eq!(bridge.last_render_attempts(), 2);
                assert_eq!(bridge.target.completion_markers, 0);
                assert!(bridge.retained_staging_valid && bridge.target.retained_target_valid);
                let explicit_budget: Duration = test
                    .waits
                    .iter()
                    .map(|(_, duration)| *duration)
                    .sum::<Duration>()
                    + BUMP_SETTLEMENT_WAIT * 2;
                assert!(explicit_budget <= Duration::from_secs(12));

                let mut exhausted = required_bridge(128, 128);
                let mut test = completion_clock();
                test.fence_elapsed = Duration::from_millis(750);
                test.callback_elapsed = Duration::from_millis(750);
                test.counters = (0..4)
                    .map(|_| Ok(vello::BumpBudgetPoll::Complete { overflowed: true }))
                    .collect();
                exhausted.target.completion_test = Some(test);
                assert!(matches!(
                    exhausted.paint_damage_into_xrgb8888(
                        &Scene::new(),
                        version(1, None, None),
                        &mut actual,
                        512
                    ),
                    Err(BridgeError::RecoveryExhausted { attempts: 4 })
                ));
                let test = exhausted.target.completion_test.as_ref().unwrap();
                let requested: Duration = test
                    .waits
                    .iter()
                    .map(|(_, duration)| *duration)
                    .sum::<Duration>()
                    + BUMP_SETTLEMENT_WAIT * 4;
                assert_eq!(requested, Duration::from_secs(12));
                assert_eq!(exhausted.last_render_attempts(), 4);
                assert!(exhausted.target.completed_view().is_none());
            }

            // Phase `gpu_target_linux_phase_failures_abort_without_recovery`.
            {
                // Every individual Linux phase failure aborts immediately; no completed overflow may
                // turn a failed fence/callback/counter observation into another attempt.
                for phase in [Some(WaitPhase::Fence), Some(WaitPhase::MapCallback), None] {
                    let mut bridge = required_bridge(128, 128);
                    let mut test = completion_clock();
                    test.failure = phase;
                    if phase.is_none() {
                        test.counters =
                            (0..8).map(|_| Ok(vello::BumpBudgetPoll::Pending)).collect();
                    }
                    bridge.target.completion_test = Some(test);
                    let mut bytes = vec![0xAB; 128 * 128 * 4];
                    let result = bridge.paint_damage_into_xrgb8888(
                        &Scene::new(),
                        version(1, None, None),
                        &mut bytes,
                        512,
                    );
                    assert!(result.is_err());
                    assert_eq!(bridge.last_render_attempts(), 1);
                    assert!(
                        !bridge.retained_staging_valid && bridge.target.completed_view().is_none()
                    );
                    assert!(bytes.iter().all(|byte| *byte == 0xAB));
                    let identity = bridge.xrgb_history_identity(512, 1);
                    assert!(matches!(
                        bridge.paint_damage_into_xrgb8888(
                            &Scene::new(),
                            version(2, Some(1), None),
                            &mut bytes,
                            512,
                        ),
                        Err(BridgeError::RecoveryBackoff { .. })
                    ));
                    assert_eq!(bridge.last_render_attempts(), 1);
                    assert!(bytes.iter().all(|byte| *byte == 0xAB));
                    // Once the cool-down expires the poisoned target is reconstructed exactly once; the
                    // failure fixture belongs to the old target and cannot poison its replacement.
                    bridge.recovery_not_before = Some(
                        Instant::now()
                            .checked_sub(Duration::from_millis(1))
                            .expect("monotonic clock is past its first millisecond"),
                    );
                    bridge
                        .paint_damage_into_xrgb8888(
                            &Scene::new(),
                            version(3, Some(2), None),
                            &mut bytes,
                            512,
                        )
                        .expect("bridge reconstructs poisoned renderer after backoff");
                    assert_ne!(bridge.xrgb_history_identity(512, 1), identity);
                    assert!(bridge.target.completed_view().is_some());
                }
            }

            // Phase `gpu_target_absolute_deadline_spans_recovery`.
            {
                let mut target = required_target(128, 128);
                let mut test = completion_clock();
                test.fence_elapsed = Duration::from_millis(750);
                test.counters = [
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Pending),
                    Ok(vello::BumpBudgetPoll::Complete { overflowed: true }),
                ]
                .into();
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                let result = target.render_until(&Scene::new(), version(1, None, None), deadline);
                assert!(matches!(result, Err(BridgeError::DeadlineExceeded)));
                assert_eq!(target.last_render_attempts, 1);
                assert_eq!(target.completion_markers, 1);
                assert!(target.completed_view().is_none());
                let test = target.completion_test.as_ref().unwrap();
                assert_eq!(
                    test.counter_remaining,
                    [Duration::from_millis(250), Duration::from_millis(50)]
                );
                assert_eq!(test.elapsed, Duration::from_secs(1));

                // A completed overflow consumes the same caller deadline on the next attempt.
                let mut target = required_target(128, 128);
                let mut test = completion_clock();
                test.fence_elapsed = Duration::from_millis(750);
                test.counters
                    .push_back(Ok(vello::BumpBudgetPoll::Complete { overflowed: true }));
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                assert!(matches!(
                    target.render_until(&Scene::new(), version(1, None, None), deadline),
                    Err(BridgeError::DeadlineExceeded)
                ));
                assert_eq!(target.last_render_attempts, 2);
                assert_eq!(target.completion_markers, 2);
                assert_eq!(
                    target.completion_test.as_ref().unwrap().waits,
                    [
                        (WaitPhase::Marker, Duration::from_secs(1)),
                        (WaitPhase::Marker, Duration::from_millis(250))
                    ]
                );
            }
        }

        // Phase `gpu_target_always_waits_on_a_completion_marker`.
        {
            // Phase `gpu_target_disabled_measurement_still_waits_for_completion`.
            {
                let mut target =
                    required_target_with_env("VELLO_BUMP_BUDGET", "upstream", 128, 128);
                assert_eq!(
                    target.renderer.poll_bump_budget(&target.device).unwrap(),
                    vello::BumpBudgetPoll::Complete { overflowed: false }
                );
                let mut test = completion_clock();
                test.failure = Some(WaitPhase::Marker);
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);
                assert!(matches!(
                    target.render_until(&Scene::new(), version(1, None, None), deadline),
                    Err(BridgeError::DeadlineExceeded)
                ));
                assert_eq!(target.completion_markers, 1);
                assert_eq!(
                    target.renderer.poll_bump_budget(&target.device).unwrap(),
                    vello::BumpBudgetPoll::Complete { overflowed: false }
                );
                assert!(target.completed_view().is_none());
                let mut replacement =
                    required_target_with_env("VELLO_BUMP_BUDGET", "upstream", 128, 128);
                let stats = replacement
                    .render_until(
                        &Scene::new(),
                        version(2, None, None),
                        Instant::now() + Duration::from_secs(10),
                    )
                    .unwrap();
                assert_eq!(stats.completion_markers, stats.render_attempts);
                assert!(stats.completion_markers > 0 && replacement.completed_view().is_some());
            }

            // Phase `gpu_target_post_render_is_covered_by_existing_completion_marker`.
            {
                let mut target = required_target(8, 4);
                let output = target.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("retained-bgra-test-output"),
                    size: wgpu::Extent3d {
                        width: 8,
                        height: 4,
                        depth_or_array_layers: 1,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                });
                let view = output.create_view(&wgpu::TextureViewDescriptor::default());
                let mut test = completion_clock();
                test.failure = Some(WaitPhase::Marker);
                let deadline = test.start + Duration::from_secs(1);
                target.completion_test = Some(test);

                assert!(matches!(
                    target.render(
                        &Scene::new(),
                        Some(version(1, None, None)),
                        FrameOutput::GpuOnly,
                        CompletionPolicy::Absolute(deadline),
                        None,
                        |device, encoder, source| {
                            TextureBlitter::new(device, wgpu::TextureFormat::Bgra8Unorm)
                                .copy(device, encoder, source, &view);
                        },
                    ),
                    Err(BridgeError::DeadlineExceeded)
                ));
                assert_eq!(target.last_render_attempts, 1);
                assert_eq!(target.completion_markers, 1);
                assert!(target.completed_view().is_none() && target.poisoned);
            }
        }
    }

    fn assert_poisoned_diagnostic_rejected(
        diagnostic: impl Fn(
            &mut VelloDrmBridge,
            &Scene,
            &mut [u8],
            &mut [u8],
        ) -> Result<(), BridgeError>,
    ) {
        let mut bridge = required_bridge(128, 128);
        let mut test = completion_clock();
        test.failure = Some(WaitPhase::Fence);
        bridge.target.completion_test = Some(test);
        let mut left = vec![0xA5; 128 * 128 * 4];
        let mut right = left.clone();
        let scene = rect_scene(128, 128, Color::WHITE);
        assert!(matches!(
            bridge.paint_damage_into_xrgb8888(&scene, version(1, None, None), &mut left, 512),
            Err(BridgeError::CompletionTimeout {
                phase: CompletionPhase::Fence
            })
        ));
        assert!(bridge.target.poisoned);
        let attempts = bridge.last_render_attempts();
        let packed = bridge.diagnostic_packed_scene().to_vec();
        let texture = bridge.target.texture.clone();
        let identity = bridge.xrgb_history_identity(512, 1);
        assert!(
            matches!(
                diagnostic(&mut bridge, &Scene::new(), &mut left, &mut right),
                Err(BridgeError::RecreateRequired)
            ),
            "diagnostic must reject before reusing the poisoned renderer"
        );
        assert_eq!(bridge.last_render_attempts(), attempts);
        assert_eq!(bridge.diagnostic_packed_scene(), packed);
        assert_eq!(bridge.target.texture, texture);
        assert_eq!(bridge.xrgb_history_identity(512, 1), identity);
        assert!(bridge.target.completed_view().is_none());
        assert!(!bridge.retained_staging_valid);
        assert!(left.iter().chain(&right).all(|byte| *byte == 0xA5));
        // The poison oracle above deliberately leaves a submitted fence unresolved. Drain
        // that fixture before testing independently observed device loss; production diagnostics
        // themselves do not poll, wait, or reuse the poisoned renderer.
        bridge
            .target
            .device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_secs(1)),
            })
            .expect("bounded test-only drain before device destruction");
        bridge.target.device.destroy();
        let _ = bridge.target.device.poll(wgpu::PollType::Poll);
        assert!(
            matches!(
                diagnostic(&mut bridge, &scene, &mut left, &mut right),
                Err(BridgeError::DeviceLost(_))
            ),
            "actual device loss takes precedence over reconstruction"
        );
    }

    const NATIVE_BENCHMARK_ACTION_TARGETS: [&str; 2] =
        ["topbar_tray_pill", "topbar_overview_button"];

    fn dispatch_native_benchmark_click(
        frame: &mut FrameBuilder,
        target: &str,
        center: vello::kurbo::Point,
        document: &mut AutoCommit,
        focus: &mut Option<NodeId>,
    ) {
        let modifiers = Modifiers::default();
        let cursor = WindowEvent::CursorMoved {
            device_id: DeviceId::dummy(),
            position: PhysicalPosition::new(center.x, center.y),
        };
        let press = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        };
        let release = WindowEvent::MouseInput {
            device_id: DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        };
        let _ = frame.dispatch_event(
            &cursor,
            &modifiers,
            &mut ui::widget::Edits::lend(document),
            &ROOT,
            focus,
        );
        let pressed = frame.dispatch_event(
            &press,
            &modifiers,
            &mut ui::widget::Edits::lend(document),
            &ROOT,
            focus,
        );
        let released = frame.dispatch_event(
            &release,
            &modifiers,
            &mut ui::widget::Edits::lend(document),
            &ROOT,
            focus,
        );
        assert!(
            pressed || released,
            "native benchmark target {target:?} must handle click"
        );
    }

    fn assert_compact_xrgb_eq(actual: &[u8], expected: &[u8], label: &str) {
        assert_eq!(actual.len(), expected.len(), "{label}: output length");
        let mismatch = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual != expected);
        assert!(
            mismatch.is_none(),
            "{label}: first mismatch at byte {mismatch:?}"
        );
    }

    /// 512 individually-recorded opaque full-target fills. Their overlapping coverage overflows
    /// tiles in the initial 384×256 reservation, exercising bounded recovery.
    fn opaque_rect_stack_scene(w: u32, h: u32) -> Scene {
        let mut scene = Scene::new();
        let rect = vello::kurbo::Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
        for index in 0..512_u32 {
            let red = u8::try_from(index & 0xFF).unwrap_or(u8::MAX);
            let green = u8::try_from((index.saturating_mul(3)) & 0xFF).unwrap_or(u8::MAX);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(red, green, 0xA5),
                None,
                &rect,
            );
        }
        scene
    }

    /// 8,192 overlapping fills confined to one native-pixel damage rectangle. 512 fills fit the
    /// 96×96 sparse tile reservation, so this calibrated count is intentionally high enough to
    /// exercise a genuine sparse bump overflow and the bounded dense recovery.
    fn opaque_rect_stack_in_rect_scene(rect: vello::FineDamageRect) -> Scene {
        let mut scene = Scene::new();
        let bounds = vello::kurbo::Rect::new(
            f64::from(rect.x),
            f64::from(rect.y),
            f64::from(rect.x.saturating_add(rect.width)),
            f64::from(rect.y.saturating_add(rect.height)),
        );
        for index in 0..8_192_u32 {
            let red = u8::try_from(index & 0xFF).unwrap_or(u8::MAX);
            let green = u8::try_from((index.saturating_mul(3)) & 0xFF).unwrap_or(u8::MAX);
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(red, green, 0xA5),
                None,
                &bounds,
            );
        }
        scene
    }

    fn layered_scene(
        w: u32,
        h: u32,
        background: Color,
        overlays: &[(vello::FineDamageRect, Color)],
    ) -> Scene {
        let mut scene = rect_scene(w, h, background);
        for (rect, color) in overlays {
            let rect = vello::kurbo::Rect::new(
                f64::from(rect.x),
                f64::from(rect.y),
                f64::from(rect.x.saturating_add(rect.width)),
                f64::from(rect.y.saturating_add(rect.height)),
            );
            scene.fill(Fill::NonZero, Affine::IDENTITY, *color, None, &rect);
        }
        scene
    }

    fn assert_xrgb_pixels_eq(
        actual: &[u8],
        expected: &[u8],
        width: usize,
        height: usize,
        pitch: usize,
    ) {
        for row in 0..height {
            let start = row.checked_mul(pitch).expect("test row offset fits usize");
            let pixel_bytes = width.checked_mul(4).expect("test pixel row fits usize");
            let end = start
                .checked_add(pixel_bytes)
                .expect("test row end fits usize");
            assert_eq!(
                &actual[start..end],
                &expected[start..end],
                "pixel row {row} differs from dense oracle"
            );
        }
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn tracked_swizzle_validates_before_writing_and_unmaps_on_error() {
        // Phase `tracked_full_swizzle_rejects_truncated_source_without_mutating_destination`.
        {
            let source_bytes = [0x51_u8; 15];
            let source = MappedXrgbSource {
                bytes: &source_bytes,
                padded_bytes_per_row: 8,
                width: 2,
                height: 2,
            };
            let mut destination = [0xA7_u8; 16];
            let before = destination;
            let result = VelloDrmBridge::swizzle_mapped_xrgb8888_with_plan(
                source,
                &mut destination,
                8,
                false,
                XrgbWritePlan::Full,
                true,
            );
            assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
            assert_eq!(
                destination, before,
                "tracked source rejection must be atomic"
            );
        }

        // Phase `tracked_partial_swizzle_validates_every_rect_before_mutating_destination`.
        {
            let source_bytes = [0x51_u8; 16];
            let source = MappedXrgbSource {
                bytes: &source_bytes,
                padded_bytes_per_row: 16,
                width: 4,
                height: 1,
            };
            let rects = [
                vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 1,
                    height: 1,
                },
                vello::FineDamageRect {
                    x: 3,
                    y: 0,
                    width: 2,
                    height: 1,
                },
            ];
            let mut destination = [0xA7_u8; 16];
            let before = destination;
            let result = VelloDrmBridge::swizzle_mapped_xrgb8888_with_plan(
                source,
                &mut destination,
                16,
                false,
                XrgbWritePlan::Rects(&rects),
                true,
            );
            assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
            assert_eq!(
                destination, before,
                "later invalid rect must not expose an earlier write"
            );
        }

        // Phase `tracked_short_destination_and_prepared_damage_mismatch_abort_without_writes`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = usize::try_from(width).expect("test width") * 4;
            let scene = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
            let left = [vello::FineDamageRect {
                x: 2,
                y: 3,
                width: 5,
                height: 4,
            }];
            let right = [vello::FineDamageRect {
                x: 16,
                y: 8,
                width: 5,
                height: 4,
            }];
            let identity = bridge
                .xrgb_history_identity(pitch, 1)
                .expect("test bridge identity");
            let mut history = XrgbTargetHistory::new(1);
            let damage = SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&left),
            };
            let transaction = history.prepare(
                0,
                Some(identity),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let mut short = vec![0xA7; pitch * usize::try_from(height).expect("test height") - 1];
            let short_before = short.clone();
            let result = bridge.paint_damage_into_xrgb8888_with_history(
                &scene,
                damage,
                &transaction,
                &mut short,
                pitch,
            );
            assert!(matches!(result, Err(BridgeError::InvalidSize(_))));
            assert_eq!(
                short, short_before,
                "short tracked output must remain untouched"
            );
            transaction.abort();

            let transaction = history.prepare(0, Some(identity), 1, None, Some(&left), &[]);
            let mismatch = SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&right),
            };
            let mut destination = vec![0xB8; pitch * usize::try_from(height).expect("test height")];
            let before = destination.clone();
            let result = bridge.paint_damage_into_xrgb8888_with_history(
                &scene,
                mismatch,
                &transaction,
                &mut destination,
                pitch,
            );
            assert!(matches!(result, Err(BridgeError::XrgbHistoryMismatch)));
            assert_eq!(
                destination, before,
                "mismatched prepared damage must not write"
            );
            transaction.abort();

            let transaction = history.prepare(0, Some(identity), 1, None, Some(&right), &[]);
            bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: Some(&right),
                    },
                    &transaction,
                    &mut destination,
                    pitch,
                )
                .expect("next tracked paint after aborted errors");
            transaction.commit(XrgbHistoryCommit::FullBoundary);
        }

        // Phase `tracked_swizzle_error_unmaps_before_the_next_paint`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = usize::try_from(width).expect("test width") * 4;
            let scene = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
            let damage = [vello::FineDamageRect {
                x: 2,
                y: 3,
                width: 5,
                height: 4,
            }];
            let identity = bridge
                .xrgb_history_identity(pitch, 1)
                .expect("test bridge identity");
            let mut history = XrgbTargetHistory::new(1);
            let mut destination = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
            let before = destination.clone();
            let transaction = history.prepare(0, Some(identity), 1, None, Some(&damage), &[]);
            bridge.inject_next_tracked_swizzle_failure_for_test();
            let failed = bridge.paint_damage_into_xrgb8888_with_history(
                &scene,
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: Some(&damage),
                },
                &transaction,
                &mut destination,
                pitch,
            );
            assert!(matches!(failed, Err(BridgeError::InvalidSize(_))));
            assert_eq!(
                destination, before,
                "injected post-map swizzle error must be atomic"
            );
            transaction.abort();

            let transaction = history.prepare(0, Some(identity), 1, None, Some(&damage), &[]);
            bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    SceneDamage {
                        revision: 1,
                        previous_revision: None,
                        regions: Some(&damage),
                    },
                    &transaction,
                    &mut destination,
                    pitch,
                )
                .expect("staging must be unmapped for the next tracked paint");
            transaction.commit(XrgbHistoryCommit::FullBoundary);
        }
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn history_bridge_treats_saturation_gaps_and_scanout_epochs_as_full_boundaries() {
        // Phase `saturated_bridge_history_epoch_disables_tracking_but_not_full_paint`.
        {
            let mut bridge = required_bridge(16, 12);
            bridge.xrgb_history_epoch = Some(u64::MAX);
            bridge.set_base_color(Color::from_rgb8(0x20, 0x30, 0x40));
            assert!(!bridge.xrgb_history_available());
            let mut destination = vec![0xA7; 16 * 12 * 4];
            bridge
                .paint_into_xrgb8888(&Scene::new(), &mut destination, 16 * 4)
                .expect("ordinary full paint remains available after tracking epoch saturation");
        }

        // Phase `history_bridge_accepts_gap_and_oversized_damage_as_real_full_boundaries`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = usize::try_from(width).expect("test width") * 4 + 16;
            let mut destination = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
            let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
            let small = [vello::FineDamageRect {
                x: 3,
                y: 4,
                width: 6,
                height: 5,
            }];
            let identity = bridge
                .xrgb_history_identity(pitch, 1)
                .expect("test bridge identity");
            let mut history = XrgbTargetHistory::new(1);

            let seed = SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&small),
            };
            let transaction = history.prepare(
                0,
                Some(identity),
                seed.revision,
                seed.previous_revision,
                seed.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    seed,
                    &transaction,
                    &mut destination,
                    pitch,
                )
                .expect("seed full boundary");
            assert_eq!(write.mode, XrgbCpuWriteMode::Full);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            // This is a legitimate model gap, not an identity mismatch. It must render densely and
            // establish the new revision rather than reject before the bridge can recover.
            let gap = SceneDamage {
                revision: 4,
                previous_revision: Some(2),
                regions: Some(&small),
            };
            let transaction = history.prepare(
                0,
                Some(identity),
                gap.revision,
                gap.previous_revision,
                gap.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    gap,
                    &transaction,
                    &mut destination,
                    pitch,
                )
                .expect("gap full boundary");
            assert_eq!(write.mode, XrgbCpuWriteMode::Full);
            transaction.commit(XrgbHistoryCommit::FullBoundary);

            let oversized = vec![small[0]; 65];
            let oversized_damage = SceneDamage {
                revision: 5,
                previous_revision: Some(4),
                regions: Some(&oversized),
            };
            let transaction = history.prepare(
                0,
                Some(identity),
                oversized_damage.revision,
                oversized_damage.previous_revision,
                oversized_damage.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    oversized_damage,
                    &transaction,
                    &mut destination,
                    pitch,
                )
                .expect("oversized damage must be a full boundary, not a mismatch");
            assert_eq!(write.mode, XrgbCpuWriteMode::Full);
            transaction.commit(XrgbHistoryCommit::FullBoundary);
        }

        // Phase `history_bridge_accepts_nonzero_scanout_epoch_across_two_targets`.
        {
            let (width, height) = (32_u32, 24_u32);
            let mut bridge = required_bridge(width, height);
            let pitch = usize::try_from(width).expect("test width") * 4;
            let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
            let damage = [vello::FineDamageRect {
                x: 3,
                y: 4,
                width: 6,
                height: 5,
            }];
            let identity = bridge
                .xrgb_history_identity(pitch, 2)
                .expect("test bridge identity")
                .with_scanout_epoch(7);
            let mut history = XrgbTargetHistory::new(2);
            let mut left = vec![0xA7; pitch * usize::try_from(height).expect("test height")];
            let mut right = left.clone();

            let first = SceneDamage {
                revision: 1,
                previous_revision: None,
                regions: Some(&damage),
            };
            let transaction = history.prepare(
                0,
                Some(identity),
                first.revision,
                first.previous_revision,
                first.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    first,
                    &transaction,
                    &mut left,
                    pitch,
                )
                .expect("nonzero scanout epoch seed");
            transaction.commit(if write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });

            let second = SceneDamage {
                revision: 2,
                previous_revision: Some(1),
                regions: Some(&damage),
            };
            let transaction = history.prepare(
                1,
                Some(identity),
                second.revision,
                second.previous_revision,
                second.regions,
                &[],
            );
            let write = bridge
                .paint_damage_into_xrgb8888_with_history(
                    &scene,
                    second,
                    &transaction,
                    &mut right,
                    pitch,
                )
                .expect("nonzero scanout epoch must not mismatch bridge identity");
            transaction.commit(if write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
        }
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "the two-target fixture keeps each ordered history transaction and exact staging comparison together"
    )]
    fn history_two_targets_repair_padded_destination_from_current_staging() {
        let (width, height) = (64_u32, 48_u32);
        let mut bridge = required_bridge(width, height);
        let pitch = usize::try_from(width)
            .expect("test width")
            .checked_mul(4)
            .and_then(|tight| tight.checked_add(16))
            .expect("test pitch");
        let height_usize = usize::try_from(height).expect("test height");
        let first_delta = vello::FineDamageRect {
            x: 5,
            y: 7,
            width: 9,
            height: 8,
        };
        let second_delta = vello::FineDamageRect {
            x: 31,
            y: 19,
            width: 11,
            height: 7,
        };
        let base = Color::from_rgb8(0x11, 0x22, 0x33);
        let first_scene = rect_scene(width, height, base);
        let second_scene = layered_scene(
            width,
            height,
            base,
            &[(first_delta, Color::from_rgb8(0xC0, 0x50, 0x20))],
        );
        let third_scene = layered_scene(
            width,
            height,
            base,
            &[
                (first_delta, Color::from_rgb8(0xC0, 0x50, 0x20)),
                (second_delta, Color::from_rgb8(0x20, 0x80, 0xD0)),
            ],
        );
        let first_regions = [first_delta];
        let second_regions = [second_delta];
        let identity = bridge
            .xrgb_history_identity(pitch, 2)
            .expect("test bridge identity")
            .with_scanout_epoch(9);
        let mut history = XrgbTargetHistory::new(2);
        let mut left = vec![0xA7; pitch * height_usize];
        let mut right = vec![0xB8; pitch * height_usize];

        let initial = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&first_regions),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            initial.revision,
            initial.previous_revision,
            initial.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &first_scene,
                initial,
                &transaction,
                &mut left,
                pitch,
            )
            .expect("seed target zero");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let second = SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&first_regions),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            second.revision,
            second.previous_revision,
            second.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &second_scene,
                second,
                &transaction,
                &mut right,
                pitch,
            )
            .expect("seed target one");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let third = SceneDamage {
            revision: 3,
            previous_revision: Some(2),
            regions: Some(&second_regions),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            third.revision,
            third.previous_revision,
            third.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &third_scene,
                third,
                &transaction,
                &mut left,
                pitch,
            )
            .expect("repair target zero");
        if bridge.target.renderer.sparse_fine_supported() {
            assert_eq!(write.mode, XrgbCpuWriteMode::Partial);
        }
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let mut staging_reference = left.clone();
        bridge
            .swizzle_current_staging_for_test(&mut staging_reference, pitch)
            .expect("same settled staging reference");
        assert_eq!(
            left, staging_reference,
            "two-target partial repair must equal the current settled staging bytes"
        );
        let tight = usize::try_from(width).expect("test width") * 4;
        for row in 0..height_usize {
            let start = row.checked_mul(pitch).expect("test row");
            let tail = start.checked_add(tight).expect("test tail");
            let end = start.checked_add(pitch).expect("test row end");
            assert!(
                left[tail..end].iter().all(|byte| *byte == 0xA7),
                "tracked swizzles must preserve target-zero row padding"
            );
            assert!(
                right[tail..end].iter().all(|byte| *byte == 0xB8),
                "tracked swizzles must preserve target-one row padding"
            );
        }
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "the native action fixture keeps one- and two-target transactions adjacent to their exact current-staging checks"
    )]
    fn native_4k_framebuilder_actions_match_current_staging_for_one_and_two_targets() {
        const WIDTH: u32 = 3_840;
        const HEIGHT: u32 = 2_160;
        let mut one_bridge = required_bridge(WIDTH, HEIGHT);
        let mut two_bridge = required_bridge(WIDTH, HEIGHT);
        let width = usize::try_from(WIDTH).expect("native width");
        let height = usize::try_from(HEIGHT).expect("native height");
        let pitch = width
            .checked_mul(4)
            .and_then(|tight| tight.checked_add(16))
            .expect("native padded pitch");
        let len = pitch.checked_mul(height).expect("native output bytes");
        let one_identity = one_bridge
            .xrgb_history_identity(pitch, 1)
            .expect("one-target identity");
        let two_identity = two_bridge
            .xrgb_history_identity(pitch, 2)
            .expect("two-target identity")
            .with_scanout_epoch(5);
        let mut one_history = XrgbTargetHistory::new(1);
        let mut two_history = XrgbTargetHistory::new(2);
        let mut one_destination = vec![0xA7; len];
        let mut two_destinations = [vec![0xB8; len], vec![0xC9; len]];
        let mut frame = PopulatedShellFixture::new((WIDTH, HEIGHT), ui::theme::Theme::dark());
        let mut document = AutoCommit::new();
        let mut focus = None;
        let mut one_partial = 0_u64;
        let mut two_partial = 0_u64;
        let mut previous_quick_settings = None;
        let mut previous_pixels: Option<Vec<u8>> = None;

        for sample_index in 0..=12 {
            if sample_index != 0 {
                let target = *NATIVE_BENCHMARK_ACTION_TARGETS
                    .get((sample_index - 1) % NATIVE_BENCHMARK_ACTION_TARGETS.len())
                    .expect("native benchmark action target exists");
                let rect = frame
                    .widget_registry()
                    .get(target)
                    .copied()
                    .filter(|rect| rect.area() > 0.0)
                    .expect("native benchmark target must be painted");
                dispatch_native_benchmark_click(
                    &mut frame,
                    target,
                    rect.center(),
                    &mut document,
                    &mut focus,
                );
                frame.relayout();
            }
            let painted = frame.paint_with_damage();
            assert!(
                !painted.scene.encoding().is_empty(),
                "native benchmark sample {sample_index} must record a scene"
            );
            let overlays = frame.chrome().overlays;
            assert!(
                !overlays.overview,
                "overview click dismisses Quick Settings"
            );
            if let Some(previous) = previous_quick_settings {
                assert_ne!(
                    overlays.quick_settings, previous,
                    "native benchmark action {sample_index} must change Quick Settings state"
                );
            }
            let damage = SceneDamage {
                revision: painted.revision,
                previous_revision: painted.previous_revision,
                regions: painted.damage_regions.as_deref(),
            };

            let transaction = one_history.prepare(
                0,
                Some(one_identity),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let one_write = one_bridge
                .paint_damage_into_xrgb8888_with_history(
                    &painted.scene,
                    damage,
                    &transaction,
                    &mut one_destination,
                    pitch,
                )
                .expect("one-target native history paint");
            transaction.commit(if one_write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            if one_write.mode == XrgbCpuWriteMode::Partial {
                one_partial = one_partial.saturating_add(1);
            }
            let mut one_reference = one_destination.clone();
            one_bridge
                .swizzle_current_staging_for_test(&mut one_reference, pitch)
                .expect("one-target current staging reference");
            assert_compact_xrgb_eq(
                &one_destination,
                &one_reference,
                "one-target native current-staging output",
            );
            if let Some(previous) = previous_pixels.as_ref() {
                let visible_row_bytes = width.checked_mul(4).expect("native visible row bytes");
                assert!(
                    (0..height).any(|row| {
                        let start = row.checked_mul(pitch).expect("native row start");
                        let end = start
                            .checked_add(visible_row_bytes)
                            .expect("native visible row end");
                        previous[start..end] != one_reference[start..end]
                    }),
                    "native benchmark action {sample_index} must change actual output pixels"
                );
            }
            previous_quick_settings = Some(overlays.quick_settings);
            previous_pixels = Some(one_reference);

            let target_index = sample_index % two_destinations.len();
            let transaction = two_history.prepare(
                target_index,
                Some(two_identity),
                damage.revision,
                damage.previous_revision,
                damage.regions,
                &[],
            );
            let destination = two_destinations
                .get_mut(target_index)
                .expect("two-target destination");
            let two_write = two_bridge
                .paint_damage_into_xrgb8888_with_history(
                    &painted.scene,
                    damage,
                    &transaction,
                    destination,
                    pitch,
                )
                .expect("two-target native history paint");
            transaction.commit(if two_write.damage_admitted {
                XrgbHistoryCommit::DamageAccepted
            } else {
                XrgbHistoryCommit::FullBoundary
            });
            if two_write.mode == XrgbCpuWriteMode::Partial {
                two_partial = two_partial.saturating_add(1);
            }
            let mut two_reference = destination.clone();
            two_bridge
                .swizzle_current_staging_for_test(&mut two_reference, pitch)
                .expect("two-target current staging reference");
            assert_compact_xrgb_eq(
                destination,
                &two_reference,
                "two-target native current-staging output",
            );
        }
        assert!(
            one_partial > 0,
            "native one-target history must write partial XRGB"
        );
        assert!(
            two_partial > 0,
            "native two-target history must write partial XRGB"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn versioned_damage_copies_dirty_regions_until_invalidated() {
        // Phase `versioned_damage_copies_exactly_the_dirty_region`.
        {
            // Phase `l_shaped_damage_does_not_expand_to_a_bounding_box`.
            {
                // A QS popover overlapping a full-width top bar is the common case that made a bounding
                // union look like a near-full frame. Keep both copies precise; Vello unions their tiles.
                let damage = [
                    vello::FineDamageRect {
                        x: 0,
                        y: 0,
                        width: 1920,
                        height: 68,
                    },
                    vello::FineDamageRect {
                        x: 1440,
                        y: 40,
                        width: 480,
                        height: 800,
                    },
                ];
                let normalized = VelloDrmBridge::normalize_damage(&damage, (1920, 1080))
                    .expect("the precise L shape is sparse eligible");
                assert_eq!(
                    normalized.as_slice(),
                    &damage,
                    "partial overlap stays as exact copies"
                );
                let copied_pixels = normalized.iter().fold(0_u64, |pixels, rect| {
                    pixels + u64::from(rect.width) * u64::from(rect.height)
                });
                assert_eq!(copied_pixels, 514_560);
                let bounding_pixels = 1920_u64 * 840;
                assert!(
                    bounding_pixels > u64::from(1920_u32) * u64::from(1080_u32) / 2,
                    "the old bounding box would have forced a full fallback"
                );
            }

            // Phase `shared_coarse_diagnostic_accepts_full_damage_without_relaxing_sparse_policy`.
            {
                let full = [vello::FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 64,
                    height: 48,
                }];
                assert!(
                    VelloDrmBridge::normalize_damage(&full, (64, 48)).is_none(),
                    "ordinary sparse damage must retain its full-frame profitability fallback"
                );
                assert_eq!(
                    VelloDrmBridge::validate_diagnostic_damage(&full, (64, 48))
                        .expect("the diagnostic must compare full native damage")
                        .as_slice(),
                    &full,
                );

                let invalid = [vello::FineDamageRect {
                    x: 63,
                    y: 0,
                    width: 2,
                    height: 1,
                }];
                assert!(
                    VelloDrmBridge::validate_diagnostic_damage(&invalid, (64, 48)).is_none(),
                    "diagnostic rendering must still reject out-of-bounds rectangles"
                );
            }

            // Phase `versioned_damage_retains_full_pixels_and_copies_only_dirty_rectangles`.
            {
                let (width, height) = (64_u32, 48_u32);
                let mut bridge = required_bridge(width, height);
                let pitch = width as usize * 4 + 16;
                let first = rect_scene(width, height, Color::from_rgb8(0x11, 0x22, 0x33));
                let damage = [vello::FineDamageRect {
                    x: 7,
                    y: 9,
                    width: 13,
                    height: 11,
                }];
                let second = layered_scene(
                    width,
                    height,
                    Color::from_rgb8(0x11, 0x22, 0x33),
                    &[(damage[0], Color::from_rgb8(0xC0, 0x50, 0x20))],
                );

                bridge.set_phase_timing_enabled(true);
                let mut first_destination = vec![0xD1; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &first,
                        SceneDamage {
                            revision: 1,
                            previous_revision: None,
                            regions: Some(&damage),
                        },
                        &mut first_destination,
                        pitch,
                    )
                    .expect("first versioned paint is dense");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(width) * u64::from(height) * 4,
                    "a first versioned paint must not use stale sparse backing"
                );

                let sparse_supported = bridge.target.renderer.sparse_fine_supported();
                let mut alternating_destination = vec![0xA7; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &second,
                        SceneDamage {
                            revision: 2,
                            previous_revision: Some(1),
                            regions: Some(&damage),
                        },
                        &mut alternating_destination,
                        pitch,
                    )
                    .expect("second versioned paint");

                let mut oracle = required_bridge(width, height);
                let mut expected = vec![0x4E; pitch * height as usize];
                oracle
                    .paint_into_xrgb8888(&second, &mut expected, pitch)
                    .expect("dense oracle");
                assert_xrgb_pixels_eq(
                    &alternating_destination,
                    &expected,
                    width as usize,
                    height as usize,
                    pitch,
                );
                if sparse_supported {
                    let timing = bridge.last_phase_timing().expect("timing");
                    assert_eq!(
                        timing.readback_bytes,
                        u64::from(damage[0].width) * u64::from(damage[0].height) * 4,
                        "profiled bytes are the submitted dirty copy, not a whole staging frame"
                    );
                    assert_eq!(
                        timing.cpu_output_bytes,
                        u64::from(width) * u64::from(height) * 4,
                        "alternating DRM destinations still receive a complete CPU XRGB image"
                    );
                    assert_eq!(bridge.last_sparse_render_attempts(), 1);
                    assert_eq!(bridge.last_full_render_attempts(), 0);
                    assert_eq!(bridge.last_render_attempts(), 1);
                }
                for row in 0..height as usize {
                    assert_eq!(
                        alternating_destination[row * pitch + width as usize * 4],
                        0xA7,
                        "destination padding remains outside the XRGB image"
                    );
                }
            }

            // Phase `overlapping_disjoint_and_edge_damage_matches_dense_oracle`.
            {
                let (width, height) = (64_u32, 48_u32);
                let mut bridge = required_bridge(width, height);
                let pitch = width as usize * 4 + 32;
                let first = rect_scene(width, height, Color::from_rgb8(0x08, 0x18, 0x28));
                let damage = [
                    vello::FineDamageRect {
                        x: 0,
                        y: 0,
                        width: 7,
                        height: 9,
                    },
                    vello::FineDamageRect {
                        x: 5,
                        y: 6,
                        width: 12,
                        height: 10,
                    },
                    vello::FineDamageRect {
                        x: 48,
                        y: 31,
                        width: 16,
                        height: 17,
                    },
                ];
                let second = layered_scene(
                    width,
                    height,
                    Color::from_rgb8(0x08, 0x18, 0x28),
                    &[
                        (damage[0], Color::from_rgb8(0xCC, 0x11, 0x22)),
                        (damage[1], Color::from_rgb8(0x22, 0xCC, 0x11)),
                        (damage[2], Color::from_rgb8(0x11, 0x22, 0xCC)),
                    ],
                );
                let mut initial = vec![0; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &first,
                        SceneDamage {
                            revision: 20,
                            previous_revision: None,
                            regions: Some(&damage),
                        },
                        &mut initial,
                        pitch,
                    )
                    .expect("dense seed");
                let mut actual = vec![0xE2; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &second,
                        SceneDamage {
                            revision: 21,
                            previous_revision: Some(20),
                            regions: Some(&damage),
                        },
                        &mut actual,
                        pitch,
                    )
                    .expect("damage paint");

                let mut oracle = required_bridge(width, height);
                let mut expected = vec![0; pitch * height as usize];
                oracle
                    .paint_into_xrgb8888(&second, &mut expected, pitch)
                    .expect("dense oracle");
                assert_xrgb_pixels_eq(&actual, &expected, width as usize, height as usize, pitch);
            }

            // Phase `revision_gap_and_empty_damage_force_full_readback`.
            {
                let (width, height) = (32_u32, 24_u32);
                let mut bridge = required_bridge(width, height);
                bridge.set_phase_timing_enabled(true);
                let pitch = width as usize * 4;
                let rect = [vello::FineDamageRect {
                    x: 2,
                    y: 3,
                    width: 5,
                    height: 7,
                }];
                let mut destination = vec![0; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &rect_scene(width, height, Color::from_rgb8(1, 2, 3)),
                        SceneDamage {
                            revision: 100,
                            previous_revision: None,
                            regions: Some(&rect),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("seed");
                bridge
                    .paint_damage_into_xrgb8888(
                        &rect_scene(width, height, Color::from_rgb8(4, 5, 6)),
                        SceneDamage {
                            revision: 102,
                            previous_revision: Some(101),
                            regions: Some(&rect),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("revision gap uses full render");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(width) * u64::from(height) * 4
                );

                bridge
                    .paint_damage_into_xrgb8888(
                        &rect_scene(width, height, Color::from_rgb8(7, 8, 9)),
                        SceneDamage {
                            revision: 103,
                            previous_revision: Some(102),
                            regions: Some(&[]),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("empty damage uses full render");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(width) * u64::from(height) * 4
                );
            }
        }

        // Phase `failed_paint_base_color_and_resize_invalidate_sparse_backing`.
        {
            // Phase `failed_versioned_paint_invalidates_sparse_backing_before_retry`.
            {
                let (width, height) = (32_u32, 24_u32);
                let mut bridge = required_bridge(width, height);
                bridge.set_phase_timing_enabled(true);
                let pitch = width as usize * 4;
                let damage = [vello::FineDamageRect {
                    x: 4,
                    y: 5,
                    width: 8,
                    height: 7,
                }];
                let scene = rect_scene(width, height, Color::from_rgb8(0x12, 0x23, 0x34));
                let mut destination = vec![0; pitch * height as usize];
                bridge
                    .paint_damage_into_xrgb8888(
                        &scene,
                        SceneDamage {
                            revision: 1,
                            previous_revision: None,
                            regions: Some(&damage),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("seed complete retained frame");

                let valid_dev_id = bridge.dev_id;
                bridge.dev_id = usize::MAX;
                let failed = bridge.paint_damage_into_xrgb8888(
                    &scene,
                    SceneDamage {
                        revision: 2,
                        previous_revision: Some(1),
                        regions: Some(&damage),
                    },
                    &mut destination,
                    pitch,
                );
                assert!(
                    matches!(failed, Err(BridgeError::InvalidDevice(_))),
                    "test-only invalid device must exercise a post-gate paint failure"
                );
                assert!(
                    !bridge.target.retained_target_valid
                        && !bridge.retained_staging_valid
                        && bridge.target.last_successful_revision.is_none(),
                    "a failed paint must invalidate the previously compatible retained frame before retry"
                );
                bridge.dev_id = valid_dev_id;

                bridge
                    .paint_damage_into_xrgb8888(
                        &scene,
                        SceneDamage {
                            revision: 3,
                            previous_revision: Some(1),
                            regions: Some(&damage),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("retry after failed paint");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(width) * u64::from(height) * 4,
                    "failed paint must erase the sparse retained revision"
                );
                assert_eq!(bridge.last_sparse_render_attempts(), 0);
                assert_eq!(bridge.last_full_render_attempts(), 1);
            }

            // Phase `base_color_and_resize_invalidate_versioned_sparse_backing`.
            {
                let (width, height) = (32_u32, 24_u32);
                let mut bridge = required_bridge(width, height);
                bridge.set_phase_timing_enabled(true);
                let pitch = width as usize * 4;
                let damage = [vello::FineDamageRect {
                    x: 3,
                    y: 4,
                    width: 6,
                    height: 5,
                }];
                let mut destination = vec![0; pitch * height as usize];
                let scene = rect_scene(width, height, Color::from_rgb8(0x20, 0x30, 0x40));
                bridge
                    .paint_damage_into_xrgb8888(
                        &scene,
                        SceneDamage {
                            revision: 1,
                            previous_revision: None,
                            regions: Some(&damage),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("seed");

                bridge.set_base_color(Color::from_rgb8(0x10, 0x20, 0x30));
                bridge
                    .paint_damage_into_xrgb8888(
                        &scene,
                        SceneDamage {
                            revision: 2,
                            previous_revision: Some(1),
                            regions: Some(&damage),
                        },
                        &mut destination,
                        pitch,
                    )
                    .expect("base-color boundary");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(width) * u64::from(height) * 4,
                    "base color change must refresh the complete retained staging image"
                );

                let (resized_width, resized_height) = (width + 8, height + 4);
                bridge
                    .resize(resized_width, resized_height)
                    .expect("different-mode resize boundary");
                assert_eq!(bridge.size(), (resized_width, resized_height));
                let resized_pitch = resized_width as usize * 4;
                let mut resized_destination = vec![0; resized_pitch * resized_height as usize];
                let resized_scene = rect_scene(
                    resized_width,
                    resized_height,
                    Color::from_rgb8(0x20, 0x30, 0x40),
                );
                bridge
                    .paint_damage_into_xrgb8888(
                        &resized_scene,
                        SceneDamage {
                            revision: 3,
                            previous_revision: Some(2),
                            regions: Some(&damage),
                        },
                        &mut resized_destination,
                        resized_pitch,
                    )
                    .expect("resize boundary");
                assert_eq!(
                    bridge.last_phase_timing().expect("timing").readback_bytes,
                    u64::from(resized_width) * u64::from(resized_height) * 4,
                    "resize must not retain a sparse backing frame"
                );
            }
        }
    }

    #[test]
    fn sparse_overflow_recovers_dense_without_publishing_partial_pixels() {
        let (width, height) = (384_u32, 256_u32);
        let mut bridge = required_bridge(width, height);
        if !bridge.target.renderer.sparse_fine_supported() {
            return;
        }
        let pitch = width as usize * 4;
        let damage = [vello::FineDamageRect {
            x: 144,
            y: 80,
            width: 96,
            height: 96,
        }];
        let overflowing = opaque_rect_stack_in_rect_scene(damage[0]);
        let mut actual = vec![0; pitch * height as usize];
        bridge
            .paint_damage_into_xrgb8888(
                &Scene::new(),
                SceneDamage {
                    revision: 1,
                    previous_revision: None,
                    regions: Some(&damage),
                },
                &mut actual,
                pitch,
            )
            .expect("dense seed");
        bridge
            .paint_damage_into_xrgb8888(
                &overflowing,
                SceneDamage {
                    revision: 2,
                    previous_revision: Some(1),
                    regions: Some(&damage),
                },
                &mut actual,
                pitch,
            )
            .expect("sparse overflow recovers densely");
        assert_eq!(bridge.last_sparse_render_attempts(), 1);
        assert!(
            bridge.last_full_render_attempts() >= 1,
            "overflowing sparse work must be followed by a dense recovery"
        );
        assert!(
            bridge.last_render_attempts() <= RESILIENT_RENDER_ATTEMPTS,
            "sparse attempt plus recovery remains globally bounded"
        );

        let mut oracle = required_bridge(width, height);
        let mut expected = vec![0; pitch * height as usize];
        oracle
            .paint_into_xrgb8888(&overflowing, &mut expected, pitch)
            .expect("dense overflow oracle");
        assert_eq!(
            actual
                .iter()
                .zip(&expected)
                .position(|(actual, expected)| actual != expected),
            None,
            "overflowed sparse pixels must never reach the XRGB destination"
        );
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "the overflow fixture keeps the complete two-target recovery chain and next-paint proof together"
    )]
    fn tracked_sparse_overflow_full_boundary_invalidates_the_other_target() {
        let (width, height) = (384_u32, 256_u32);
        let mut bridge = required_bridge(width, height);
        if !bridge.target.renderer.sparse_fine_supported() {
            return;
        }
        let pitch = usize::try_from(width).expect("test width") * 4;
        let damage = [vello::FineDamageRect {
            x: 144,
            y: 80,
            width: 96,
            height: 96,
        }];
        let identity = bridge
            .xrgb_history_identity(pitch, 2)
            .expect("test bridge identity");
        let mut history = XrgbTargetHistory::new(2);
        let mut left = vec![0; pitch * usize::try_from(height).expect("test height")];
        let mut right = left.clone();
        let seed = SceneDamage {
            revision: 1,
            previous_revision: None,
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            seed.revision,
            seed.previous_revision,
            seed.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &Scene::new(),
                seed,
                &transaction,
                &mut left,
                pitch,
            )
            .expect("seed first target");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let second = SceneDamage {
            revision: 2,
            previous_revision: Some(1),
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            second.revision,
            second.previous_revision,
            second.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &Scene::new(),
                second,
                &transaction,
                &mut right,
                pitch,
            )
            .expect("seed second target");
        transaction.commit(if write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });

        let overflow = SceneDamage {
            revision: 3,
            previous_revision: Some(2),
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            0,
            Some(identity),
            overflow.revision,
            overflow.previous_revision,
            overflow.regions,
            &[],
        );
        let write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &opaque_rect_stack_in_rect_scene(damage[0]),
                overflow,
                &transaction,
                &mut left,
                pitch,
            )
            .expect("dense overflow recovery");
        assert!(
            !write.damage_admitted,
            "overflow recovery cannot preserve age history"
        );
        assert_eq!(write.mode, XrgbCpuWriteMode::Full);
        transaction.commit(XrgbHistoryCommit::FullBoundary);

        let next = SceneDamage {
            revision: 4,
            previous_revision: Some(3),
            regions: Some(&damage),
        };
        let transaction = history.prepare(
            1,
            Some(identity),
            next.revision,
            next.previous_revision,
            next.regions,
            &[],
        );
        assert!(matches!(transaction.write_plan(), XrgbWritePlan::Full));
        let next_write = bridge
            .paint_damage_into_xrgb8888_with_history(
                &opaque_rect_stack_in_rect_scene(damage[0]),
                next,
                &transaction,
                &mut right,
                pitch,
            )
            .expect("other target must repaint fully after overflow boundary");
        assert_eq!(next_write.mode, XrgbCpuWriteMode::Full);
        transaction.commit(if next_write.damage_admitted {
            XrgbHistoryCommit::DamageAccepted
        } else {
            XrgbHistoryCommit::FullBoundary
        });
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn bridge_paint_fills_sized_buffers_and_xrgb_output_matches_swizzled_paint() {
        // Phase `bridge_paint_sizes_and_fills_its_buffer`.
        {
            // Phase `bridge_paint_returns_non_empty_buffer_for_non_empty_scene`.
            {
                let mut bridge = required_bridge(64, 64);
                // Fill with cyan so the buffer is definitely not the (dark) base color.
                let scene = rect_scene(64, 64, Color::from_rgb8(0, 255, 255));
                let bytes = bridge.paint(&scene).expect("paint");
                assert_eq!(bytes.len(), 64 * 64 * 4);
                // Find at least one non-background pixel (background is the bridge's default base color, ~0x1A,0x1B,0x26).
                let any_non_bg = bytes
                    .chunks_exact(4)
                    .any(|p| p[0] != 0x1A || p[1] != 0x1B || p[2] != 0x26);
                assert!(
                    any_non_bg,
                    "expected at least one non-background pixel after painting a cyan rect"
                );
                assert_eq!(bridge.last_phase_timing(), None, "profiling defaults off");
            }

            // Phase `bridge_resize_changes_buffer_size`.
            {
                let mut bridge = required_bridge(1600, 900);
                let bytes = bridge.paint(&Scene::new()).expect("paint at initial size");
                assert_eq!(bytes.len(), 1600 * 900 * 4);

                // Resize to 1280x720 and verify the next paint returns the new buffer size.
                bridge.resize(1280, 720).expect("resize");
                assert_eq!(bridge.size(), (1280, 720));
                let bytes = bridge.paint(&Scene::new()).expect("paint after resize");
                assert_eq!(bytes.len(), 1280 * 720 * 4);
            }

            // Phase `bridge_paint_uses_base_color_for_empty_scene`.
            {
                let mut bridge = required_bridge(32, 32);
                // Pick a distinctive base color so the empty background is distinguishable from any other clear.
                bridge.set_base_color(Color::from_rgb8(0x12, 0x34, 0x56));
                let bytes = bridge.paint(&Scene::new()).expect("paint empty");
                assert_eq!(bytes.len(), 32 * 32 * 4);
                for (i, p) in bytes.chunks_exact(4).enumerate() {
                    assert_eq!(
                        (p[0], p[1], p[2]),
                        (0x12, 0x34, 0x56),
                        "pixel {i} expected base color, got ({:#x}, {:#x}, {:#x})",
                        p[0],
                        p[1],
                        p[2]
                    );
                }
            }

            // Phase `bridge_handles_non_aligned_width_via_padding`.
            {
                // Width=33 → tight stride=132, padded to 256 — verifies the row-pitch stripping in `paint`.
                let mut bridge = required_bridge(33, 16);
                bridge.set_base_color(Color::from_rgb8(0xAA, 0xBB, 0xCC));
                let bytes = bridge.paint(&Scene::new()).expect("paint non-aligned");
                assert_eq!(bytes.len(), 33 * 16 * 4);
                for p in bytes.chunks_exact(4) {
                    assert_eq!((p[0], p[1], p[2]), (0xAA, 0xBB, 0xCC));
                }
            }
        }

        // Phase `xrgb_output_matches_swizzled_paint_and_honours_pitch_and_diagnostics`.
        {
            // Phase `paint_into_xrgb8888_matches_paint_swizzled`.
            {
                // The direct XRGB path must produce the same pixels as tight-RGBA `paint()` output
                // swizzled to XRGB8888.
                let mut bridge = required_bridge(40, 24);
                let scene = rect_scene(40, 24, Color::from_rgb8(10, 200, 90));

                let rgba = bridge.paint(&scene).expect("paint").to_vec();
                let mut expected = vec![0u8; 40 * 24 * 4];
                for (i, p) in rgba.chunks_exact(4).enumerate() {
                    let d = &mut expected[i * 4..i * 4 + 4];
                    d[0] = p[2]; // B
                    d[1] = p[1]; // G
                    d[2] = p[0]; // R
                    d[3] = 0xFF; // X
                }

                let pitch = 40 * 4;
                let mut dst = vec![0u8; pitch * 24];
                bridge
                    .paint_into_xrgb8888(&scene, &mut dst, pitch)
                    .expect("paint_into_xrgb8888");
                assert_eq!(
                    dst, expected,
                    "direct XRGB output must equal swizzled paint()"
                );
            }

            // Phase `paint_into_xrgb8888_respects_destination_pitch`.
            {
                // A destination whose pitch exceeds width*4 (the common DRM dumb-buffer case) must leave trailing padding untouched and place each row at `row * dst_pitch`.
                let mut bridge = required_bridge(8, 4);
                bridge.set_base_color(Color::from_rgb8(0x11, 0x22, 0x33));
                let pitch = 64;
                let mut dst = vec![0xEEu8; pitch * 4];
                bridge
                    .paint_into_xrgb8888(&Scene::new(), &mut dst, pitch)
                    .expect("paint into padded dst");
                for row in 0..4usize {
                    for x in 0..8usize {
                        let o = row * pitch + x * 4;
                        assert_eq!(
                            (dst[o], dst[o + 1], dst[o + 2]),
                            (0x33, 0x22, 0x11),
                            "row {row} px {x} should be base color in BGR order"
                        );
                    }
                    assert_eq!(
                        dst[row * pitch + 8 * 4],
                        0xEE,
                        "row {row} padding untouched"
                    );
                }
            }

            // Phase `diagnostic_zero_pitch_records_readback_but_no_output_bytes`.
            {
                let mut bridge = required_bridge(8, 4);
                bridge.set_phase_timing_enabled(true);
                bridge
                    .paint_into_xrgb8888(&Scene::new(), &mut [], 0)
                    .expect("zero-pitch diagnostic paint");
                let timing = bridge
                    .last_phase_timing()
                    .expect("enabled diagnostic timing");
                assert!(timing.readback_bytes > 0, "the GPU copy still completed");
                assert_eq!(timing.cpu_output_bytes, 0, "no destination was writable");
            }

            // Phase `diagnostic_xrgb_counts_written_bytes_without_changing_pixels`.
            {
                let mut bridge = required_bridge(8, 4);
                bridge.set_phase_timing_enabled(true);
                let scene = rect_scene(8, 4, Color::from_rgb8(0x12, 0x34, 0x56));
                let mut destination = vec![0_u8; 8 * 4 * 4];
                bridge
                    .paint_into_xrgb8888(&scene, &mut destination, 8 * 4)
                    .expect("diagnostic XRGB paint");
                let timing = bridge
                    .last_phase_timing()
                    .expect("enabled diagnostic timing");
                assert_eq!(timing.cpu_output_bytes, 8 * 4 * 4);
                assert_eq!(timing.readback_bytes, 8 * 4 * 4);
                for pixel in destination.chunks_exact(4) {
                    assert_eq!(pixel, &[0x56, 0x34, 0x12, 0xFF]);
                }
            }

            // Phase `zero_destination_pitch_unmaps_before_the_next_frame`.
            {
                let mut bridge = required_bridge(8, 4);
                let scene = rect_scene(8, 4, Color::from_rgb8(0x12, 0x34, 0x56));
                bridge
                    .paint_into_xrgb8888(&scene, &mut [], 0)
                    .expect("zero-pitch paint");

                let mut destination = vec![0_u8; 8 * 4 * 4];
                bridge
                    .paint_into_xrgb8888(&scene, &mut destination, 8 * 4)
                    .expect("paint after zero-pitch paint");
                for pixel in destination.chunks_exact(4) {
                    assert_eq!(pixel, &[0x56, 0x34, 0x12, 0xFF]);
                }
            }

            // Phase `overflow_recovery_retries_before_xrgb_output`.
            {
                let (width, height) = (384, 256);
                let mut xrgb_bridge = required_bridge(width, height);
                let scene = opaque_rect_stack_scene(width, height);
                let pitch = width as usize * 4;
                let mut xrgb = vec![0_u8; pitch * height as usize];
                xrgb_bridge
                    .paint_into_xrgb8888(&scene, &mut xrgb, pitch)
                    .expect("cold opaque-stack XRGB paint");
                assert!(
                    xrgb_bridge.last_render_attempts() >= 2,
                    "512 opaque rects at 384x256 must exercise bounded bump recovery"
                );
                for (index, pixel) in xrgb.chunks_exact(4).enumerate() {
                    // Fill 511 is the visible opaque top layer: RGB=(0xFF, 0xFD, 0xA5).
                    assert_eq!(pixel, &[0xA5, 0xFD, 0xFF, 0xFF], "XRGB pixel {index}");
                }

                // Use a separate cold renderer so this comparison cannot inherit the XRGB bridge's grown
                // budget. Both public outputs must agree after their own recovery path.
                let mut rgba_bridge = required_bridge(width, height);
                let rgba = rgba_bridge
                    .paint(&scene)
                    .expect("cold opaque-stack RGBA paint")
                    .to_vec();
                assert!(rgba_bridge.last_render_attempts() >= 2);
                for (index, (rgba, xrgb)) in
                    rgba.chunks_exact(4).zip(xrgb.chunks_exact(4)).enumerate()
                {
                    assert_eq!(rgba, &[0xFF, 0xFD, 0xA5, 0xFF], "RGBA pixel {index}");
                    assert_eq!(xrgb, &[rgba[2], rgba[1], rgba[0], 0xFF], "pixel {index}");
                }
            }
        }
    }

    #[test]
    fn targets_past_the_bin_limit_render_in_bands_of_whole_bin_rows() {
        assert_eq!(RetainedGpuTarget::band_rows((1920, 1080)), None);
        assert_eq!(
            RetainedGpuTarget::band_rows((5120, 2880)),
            None,
            "a 5K panel alone is 240 bins and renders whole"
        );
        // 5120x2880 beside 1920x1080: 28 bins across, so nine bin rows per band.
        assert_eq!(RetainedGpuTarget::band_rows((7040, 2880)), Some(2304));
        assert_eq!(RetainedGpuTarget::band_rows((7680, 4320)), Some(2048));
        assert_eq!(RetainedGpuTarget::band_rows((16384, 16384)), Some(1024));
    }

    /// The multi-output presenter paints one scene spanning every output. For a 5120x2880
    /// panel beside a 1920x1080 one that is 7040x2880, 336 of Vello's 256x256 bins against the
    /// 256 its binning and coarse stages address: the bins past 255 were clamped onto others,
    /// and the bottom of the large panel drew somebody else's content. Every bin must show its
    /// own.
    #[test]
    fn a_combined_target_past_the_bin_limit_draws_every_bin() {
        let (width, height) = (7040_u32, 2880_u32);
        let mut bridge = required_bridge(width, height);
        bridge.wait_for_gpu_completion_for_test();
        let colour = |bx: u32, by: u32| {
            Color::from_rgb8(
                u8::try_from(bx * 9).expect("fits"),
                u8::try_from(by * 20).expect("fits"),
                0xC8,
            )
        };
        let mut scene = rect_scene(width, height, Color::BLACK);
        let bins = (width.div_ceil(256), height.div_ceil(256));
        for by in 0..bins.1 {
            for bx in 0..bins.0 {
                // The last column and row of bins are partial: 7040 and 2880 are not multiples
                // of 256, so each probe sits within the first 64 pixels of its bin.
                let (x, y) = (f64::from(bx * 256 + 16), f64::from(by * 256 + 16));
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    colour(bx, by),
                    None,
                    &vello::kurbo::Rect::new(x, y, x + 32.0, y + 32.0),
                );
            }
        }
        let rgba = bridge.paint(&scene).expect("combined paint").to_vec();
        let mut wrong = Vec::new();
        for by in 0..bins.1 {
            for bx in 0..bins.0 {
                let (x, y) = ((bx * 256 + 32) as usize, (by * 256 + 32) as usize);
                let at = (y * width as usize + x) * 4;
                let want = colour(bx, by).to_rgba8();
                if rgba[at..at + 4] != [want.r, want.g, want.b, 0xFF] {
                    wrong.push((bx, by, rgba[at..at + 4].to_vec()));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "bins drawn wrong (bx, by, pixel): {wrong:?}"
        );
        assert!(
            bridge.last_full_render_attempts() >= 2,
            "a 336-bin target renders as more than one band"
        );
    }

    /// The black-screen loop: a frame failed and poisoned the target, the display
    /// configuration changed, and every later frame asked the poisoned target to resize, was
    /// refused with "retained GPU target must be reconstructed", and presented nothing.
    #[test]
    fn a_poisoned_target_is_rebuilt_at_the_size_a_resize_asks_for() {
        let mut bridge = required_bridge(128, 128);
        let mut test = completion_clock();
        test.failure = Some(WaitPhase::Fence);
        bridge.target.completion_test = Some(test);
        let mut pixels = vec![0; 128 * 128 * 4];
        assert!(matches!(
            bridge.paint_into_xrgb8888(&rect_scene(128, 128, Color::WHITE), &mut pixels, 512),
            Err(BridgeError::CompletionTimeout {
                phase: CompletionPhase::Fence
            })
        ));
        assert!(
            bridge.target.poisoned,
            "the fixture must reach the poisoned state"
        );
        let generation = bridge.xrgb_history_generation;

        bridge
            .resize(256, 96)
            .expect("a poisoned target is rebuilt, not refused");
        assert_eq!(bridge.size(), (256, 96));
        assert!(!bridge.target.poisoned);
        assert_ne!(
            bridge.xrgb_history_generation, generation,
            "a rebuilt target is a new destination history"
        );
        bridge.wait_for_gpu_completion_for_test();
        bridge.recovery_not_before = Some(
            Instant::now()
                .checked_sub(Duration::from_millis(1))
                .expect("monotonic clock is past its first millisecond"),
        );
        let red = Color::from_rgb8(0xE0, 0x10, 0x20);
        let rgba = bridge
            .paint(&rect_scene(256, 96, red))
            .expect("the rebuilt target paints")
            .to_vec();
        assert_eq!(rgba.len(), 256 * 96 * 4);
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(pixel, &[0xE0, 0x10, 0x20, 0xFF], "pixel {index}");
        }
    }

    /// A stand-in for an imported scanout image: an SDR target the test can read back.
    #[cfg(target_os = "linux")]
    fn direct_target(
        bridge: &VelloDrmBridge,
        width: u32,
        height: u32,
    ) -> ui::dmabuf_import::ImportedRenderTarget {
        let (device, _) = bridge.gpu();
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("direct-scanout-test-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Bgra8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        ui::dmabuf_import::ImportedRenderTarget {
            view: texture.create_view(&wgpu::TextureViewDescriptor::default()),
            texture,
            format: wgpu::TextureFormat::Bgra8Unorm,
            width,
            height,
            stride: width.saturating_mul(4),
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_paint_phase_timing_observes_real_gpu_wait_without_pixel_readback() {
        let mut bridge = required_bridge(128, 96);
        let target = direct_target(&bridge, 128, 96);
        let scene = rect_scene(128, 96, Color::WHITE);
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
        bridge
            .paint_imported_until(
                &scene,
                version(1, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .unwrap();
        assert!(
            bridge.last_phase_timing().is_none(),
            "ordinary bridge profiling stays off"
        );
        bridge.set_phase_timing_enabled(true);
        let stats = bridge
            .paint_imported_until(
                &scene,
                version(2, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .unwrap();
        let timing = bridge
            .last_phase_timing()
            .expect("actual direct GPU paint must expose its phases");
        assert!(
            timing.submit_us > 0,
            "actual CPU encoding and queue submission"
        );
        assert!(
            timing.completed_readback_map_us > 0,
            "actual GPU completion poll"
        );
        assert_eq!(timing.readback_bytes, 0);
        assert_eq!(timing.cpu_output_bytes, 0);
        assert_eq!(timing.cpu_copy_swizzle_us, 0);
        assert_eq!(stats.framebuffer_copy_bytes, 0);
        assert_eq!(stats.completion_markers, 1);
        assert_eq!(bridge.take_phase_timing(), Some(timing));
        assert!(
            bridge.take_phase_timing().is_none(),
            "timing is consumed exactly once"
        );
        assert!(
            bridge
                .paint_imported_until(
                    &scene,
                    version(3, None, None),
                    &target,
                    Instant::now()
                        .checked_sub(Duration::from_millis(1))
                        .expect("monotonic clock permits an expired deadline"),
                    white,
                    &[]
                )
                .is_err()
        );
        assert!(
            bridge.last_phase_timing().is_none(),
            "early refusal never reuses preceding frame timing"
        );
        bridge.set_phase_timing_enabled(false);
        bridge
            .paint_imported_until(
                &scene,
                version(4, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .unwrap();
        assert!(bridge.last_phase_timing().is_none());
    }

    /// Runs the ordinary direct paint, not a diagnostic helper or a fake GPU sample.
    #[cfg(target_os = "linux")]
    #[test]
    fn direct_gpu_stage_records_keep_actual_submission_identity_and_coverage() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_PROFILE_GPU_STAGES", "0");
        let ordinary = required_bridge(128, 96);
        assert!(ordinary.dispatch_stage_observer.is_none());
        drop(ordinary);
        env.set("TILO_PROFILE_GPU_STAGES", "1");
        env.set("VELLO_GPU_TIMESTAMPS", "1");
        env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
        let mut bridge = required_bridge(128, 96);
        let target = direct_target(&bridge, 128, 96);
        let mut samples = 0;
        for revision in 1..=12 {
            bridge
                .paint_imported_until(
                    &rect_scene(128, 96, Color::WHITE),
                    version(revision, None, None),
                    &target,
                    tilo_test_env::hang_deadline(),
                    crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                    &[],
                )
                .expect("production direct GPU paint");
            let row = bridge
                .dispatch_stage_observer
                .as_ref()
                .unwrap()
                .last_record
                .as_ref()
                .unwrap();
            assert_eq!(row["process_id"], std::process::id());
            assert_eq!(
                row["renderer_generation"],
                bridge.xrgb_history_generation.unwrap()
            );
            assert_eq!(row["revision"], revision);
            assert_eq!(row["coverage"]["attempts"], revision);
            assert_eq!(row["render_succeeded"], true);
            assert_eq!(
                row["coverage"]["matched_current"].as_u64().unwrap()
                    + row["coverage"]["missing_current"].as_u64().unwrap(),
                revision
            );
            if let Some(sample_id) = row["sample_submission_id"].as_u64() {
                samples += 1;
                let latest = row["latest_attempted_submission_id"].as_u64().unwrap();
                assert!(sample_id <= latest);
                assert_eq!(row["matched_current"], sample_id == latest);
                assert_eq!(row["stale_sample"], sample_id != latest);
                assert!(!row["stages"].as_array().unwrap().is_empty());
            } else {
                assert_eq!(row["missing_current"], true);
                assert!(row["stages"].as_array().unwrap().is_empty());
            }
            bridge.wait_for_gpu_completion_for_test();
        }
        assert!(
            samples > 0,
            "the diagnostic must observe an actual GPU stage sample"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_gpu_stage_failed_paints_and_recovery_keep_renderer_identity() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("TILO_PROFILE_GPU_STAGES", "1");
        env.set("VELLO_GPU_TIMESTAMPS", "1");
        env.set("VELLO_GPU_DISPATCH_TIMESTAMPS", "1");
        let mut bridge = required_bridge(128, 96);
        let target = direct_target(&bridge, 128, 96);
        let scene = rect_scene(128, 96, Color::WHITE);
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
        bridge
            .paint_imported_until(
                &scene,
                version(1, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("initial ordinary direct paint");
        let old_generation = bridge.xrgb_history_generation;
        let expired = bridge.paint_imported_until(
            &scene,
            version(2, None, None),
            &target,
            Instant::now()
                .checked_sub(Duration::from_secs(1))
                .expect("monotonic clock permits an expired deadline"),
            white,
            &[],
        );
        assert!(expired.is_err(), "an expired ordinary paint must fail");
        let row = bridge
            .dispatch_stage_observer
            .as_ref()
            .unwrap()
            .last_record
            .as_ref()
            .unwrap();
        assert_eq!(row["revision"], 2);
        assert_eq!(row["render_succeeded"], false);
        assert!(row["expected_current_submission_id"].is_null());
        assert_eq!(row["matched_current"], false);
        assert_eq!(row["missing_current"], true);
        // An actual wgpu validation error enters the ordinary recovery path.
        let (device, _) = bridge.gpu();
        let refused = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("stage-observer-recovery-test"),
            size: device.limits().max_buffer_size.saturating_add(4),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        drop(refused);
        assert!(
            bridge
                .paint_imported_until(
                    &scene,
                    version(3, None, None),
                    &target,
                    tilo_test_env::hang_deadline(),
                    white,
                    &[],
                )
                .is_err()
        );
        bridge
            .paint_imported_until(
                &scene,
                version(4, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("ordinary paint reconstructs after the reported GPU error");
        let row = bridge
            .dispatch_stage_observer
            .as_ref()
            .unwrap()
            .last_record
            .as_ref()
            .unwrap();
        assert_eq!(row["revision"], 4);
        assert_ne!(bridge.xrgb_history_generation, old_generation);
        assert_eq!(
            row["renderer_generation"],
            bridge.xrgb_history_generation.unwrap()
        );
    }

    /// The direct swapchain alternates between two scanout images, so the image a frame lands in
    /// holds the revision before last. The renderer's retained target holds the previous one, and
    /// the image receives a whole copy of it: a typed character must repaint sparsely, not the
    /// full panel (a dense 2944x1840 frame took ~45 ms on a Snapdragon X Elite).
    #[cfg(target_os = "linux")]
    #[test]
    fn direct_frames_stay_sparse_across_the_swapchain_and_show_the_whole_scene() {
        let (width, height) = (128, 96);
        let mut bridge = required_bridge(width, height);
        bridge.wait_for_gpu_completion_for_test();
        let slots = [
            direct_target(&bridge, width, height),
            direct_target(&bridge, width, height),
        ];
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;
        let region = vello::FineDamageRect {
            x: 16,
            y: 16,
            width: 16,
            height: 16,
        };
        let red = Color::from_rgb8(0xE0, 0x10, 0x20);
        let mut typed = rect_scene(width, height, Color::WHITE);
        typed.append(&region_scene(region, red), None);

        bridge
            .paint_imported_until(
                &rect_scene(width, height, Color::WHITE),
                version(1, None, None),
                &slots[0],
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("the first direct frame renders");
        let stats = bridge
            .paint_imported_until(
                &typed,
                version(2, Some(1), Some(&[region])),
                &slots[1],
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("the sparse direct frame renders");
        if bridge.target.renderer.sparse_fine_supported() {
            assert!(
                stats.sparse_attempts > 0 && stats.full_attempts == 0,
                "a damaged region repaints sparsely: {stats:?}"
            );
        }

        let (device, queue) = bridge.gpu();
        let (buffer, stride) = VelloDrmBridge::allocate_staging(device, width, height);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        VelloDrmBridge::append_full_copy(
            &mut encoder,
            &slots[1].texture,
            &buffer,
            stride,
            width,
            height,
        );
        queue.submit([encoder.finish()]);
        let mapped = VelloDrmBridge::map_device_readback(device, &buffer).expect("slot readback");
        let pixel = |x: u32, y: u32| {
            let at = (y * stride + x * 4) as usize;
            mapped[at..at + 4].to_vec()
        };
        assert_eq!(
            pixel(20, 20),
            vec![0x20, 0x10, 0xE0, 0xFF],
            "the typed region"
        );
        assert_eq!(
            pixel(100, 80),
            vec![0xFF; 4],
            "the rest of the slot comes from the retained frame, not the slot's stale image"
        );
        drop(mapped);
        buffer.unmap();
    }

    /// A client frame kept on the GPU is drawn by the presenting renderer from its texture, and
    /// by a renderer on another device from pixels read back on demand: both show the client.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_client_frame_on_the_gpu_reaches_every_renderer() {
        let (width, height) = (64, 48);
        let mut presenting = required_bridge(width, height);
        presenting.wait_for_gpu_completion_for_test();
        let (device, queue) = presenting.gpu();
        let red = [0xE0, 0x10, 0x20, 0xFF];
        let generation = presenting
            .target
            .present_generation
            .expect("a presenting device");
        let frame = crate::shell::gpu_frames::frame_for_test(
            device,
            queue,
            generation,
            (40, 30),
            red,
            9_001,
        );
        let image = frame.image((4, 2, 32, 24));
        assert!(image.data.is_empty(), "the frame carries no CPU pixels");
        let mut scene = rect_scene(width, height, Color::WHITE);
        scene.fill(
            Fill::NonZero,
            Affine::translate((8.0, 8.0)),
            &vello::peniko::ImageBrush::new(image),
            None,
            &vello::kurbo::Rect::new(0.0, 0.0, 32.0, 24.0),
        );
        let target = direct_target(&presenting, width, height);
        presenting
            .paint_imported_until(
                &scene,
                version(1, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS,
                &[],
            )
            .expect("the presenting renderer draws the frame");
        let (device, queue) = presenting.gpu();
        let (buffer, stride) = VelloDrmBridge::allocate_staging(device, width, height);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        VelloDrmBridge::append_full_copy(
            &mut encoder,
            &target.texture,
            &buffer,
            stride,
            width,
            height,
        );
        queue.submit([encoder.finish()]);
        let mapped = VelloDrmBridge::map_device_readback(device, &buffer).expect("readback");
        let at = (20 * stride + 20 * 4) as usize;
        assert_eq!(
            &mapped[at..at + 4],
            &[0x20, 0x10, 0xE0, 0xFF],
            "BGRA target"
        );
        drop(mapped);
        buffer.unmap();

        let mut elsewhere = required_bridge(width, height);
        elsewhere.wait_for_gpu_completion_for_test();
        let rgba = elsewhere
            .paint(&scene)
            .expect("a renderer without the texture still draws the frame")
            .to_vec();
        let at = ((20 * width + 20) * 4) as usize;
        assert_eq!(&rgba[at..at + 4], &red);
    }

    /// One direct frame that missed its deadline -- a cold shader cache, a busy GPU -- poisons
    /// the target. The retry after the readback presenter stood in must render again, or a single
    /// slow frame keeps the output on the per-frame readback for the rest of the session.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_direct_frame_after_a_missed_deadline_renders_again() {
        let mut bridge = required_bridge(128, 96);
        let target = direct_target(&bridge, 128, 96);
        let scene = rect_scene(128, 96, Color::WHITE);
        let white = crate::shell::color_pipeline::SDR_REFERENCE_WHITE_NITS;

        let mut test = completion_clock();
        test.failure = Some(WaitPhase::Marker);
        let deadline = test.start + Duration::from_secs(1);
        bridge.target.completion_test = Some(test);
        assert!(matches!(
            bridge.paint_imported_until(
                &scene,
                version(1, None, None),
                &target,
                deadline,
                white,
                &[]
            ),
            Err(BridgeError::DeadlineExceeded)
        ));
        assert!(
            bridge.target.poisoned,
            "the fixture must reach the poisoned state"
        );

        let stats = bridge
            .paint_imported_until(
                &scene,
                version(2, None, None),
                &target,
                tilo_test_env::hang_deadline(),
                white,
                &[],
            )
            .expect("the retried direct frame renders");
        assert_eq!(stats.revision, 2);
        assert!(!bridge.target.poisoned);
    }

    /// wgpu answers an uncaptured validation error by panicking, and the compositor aborts on
    /// panic: one oversized binding on the present thread ended the session and every client
    /// in it. The bridge's device must hand such an error to the bridge instead, which rebuilds
    /// its renderer and keeps presenting.
    #[test]
    fn an_uncaptured_wgpu_error_costs_a_renderer_not_the_session() {
        let mut bridge = required_bridge(64, 64);
        bridge.wait_for_gpu_completion_for_test();
        let limits = bridge.target.device.limits();
        let refused = bridge.target.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("past max_buffer_size"),
            size: limits.max_buffer_size.saturating_add(4),
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        drop(refused);
        assert!(
            bridge.device_errors.0.lock().expect("latch").is_some(),
            "the validation error reached the bridge, not a panic"
        );
        let generation = bridge.xrgb_history_generation;
        let green = Color::from_rgb8(0x10, 0xC0, 0x40);
        let rgba = bridge
            .paint(&rect_scene(64, 64, green))
            .expect("the next frame renders on a rebuilt renderer")
            .to_vec();
        assert_ne!(bridge.xrgb_history_generation, generation, "rebuilt");
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            assert_eq!(pixel, &[0x10, 0xC0, 0x40, 0xFF], "pixel {index}");
        }
    }

    /// wgpu's default limits cap a storage binding at 128 MiB whatever the hardware binds; a
    /// 5120x2880 output's scratch buffers need more. The bridge's device must carry what its
    /// adapter supports.
    #[test]
    fn the_bridge_device_binds_what_its_adapter_supports() {
        let bridge = required_bridge(64, 64);
        let adapter = bridge.context.devices[bridge.dev_id].adapter.limits();
        let device = bridge.target.device.limits();
        assert_eq!(
            device.max_storage_buffer_binding_size,
            adapter.max_storage_buffer_binding_size
        );
        assert_eq!(device.max_buffer_size, adapter.max_buffer_size);
    }
}
