//! CPU-fallback Vello → XRGB bridge for the DRM backend.
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
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

use pollster::FutureExt;
use vello::peniko::Color;
// `ui::gpu`'s context, not `vello::util`'s: this device is shared with every other client on the machine, so it needs the same memory hint — see the header of `ui/src/gpu.rs`.
use ui::gpu::RenderContext;
use vello::{AaConfig, AaSupport, RenderParams, Renderer, RendererOptions, Scene, wgpu};

use super::xrgb_history::XrgbWritePlan;
pub use super::xrgb_history::{
    XrgbHistoryCommit, XrgbHistoryIdentity, XrgbHistoryTransaction, XrgbTargetHistory,
};

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

    #[error("XRGB destination-history transaction does not match this scene damage")]
    XrgbHistoryMismatch,
}

/// One-shot latch so the "CPU fallback active" warning fires once per process; shared across all bridges since the message is process-global.
static WARNED_CPU_FALLBACK: AtomicBool = AtomicBool::new(false);

/// Process-unique bridge identities prevent a replacement renderer from inheriting a stale
/// caller destination claim. Exhaustion makes destination tracking unavailable rather than
/// wrapping; ordinary bridge construction and full rendering remain available.
static NEXT_XRGB_HISTORY_GENERATION: AtomicU64 = AtomicU64::new(1);

/// A complex first frame can require Vello to grow its bump allocation before it produces every
/// pixel. Retrying is deliberately bounded: an exhausted recovery reports an error rather than
/// presenting incomplete geometry as a successful frame.
const RESILIENT_RENDER_ATTEMPTS: usize = 4;

/// Identity of the adapter actually opened by this bridge. This comes from the wgpu adapter
/// that owns the renderer, rather than from a separate GL probe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeAdapterInfo {
    pub name: String,
    pub backend: String,
    pub device_type: String,
    pub driver: String,
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

/// Result of attempting the opt-in sparse path. An overflow has already submitted and mapped
/// work, but those pixels are not safe to expose; its caller immediately starts a dense recovery.
enum SparseReadback {
    Unavailable,
    Complete {
        mapped: wgpu::BufferView,
        stats: vello::SparseRenderStats,
    },
    Overflow {
        stats: vello::SparseRenderStats,
    },
}

impl SparseReadback {
    fn stats(&self) -> Option<vello::SparseRenderStats> {
        match self {
            Self::Unavailable => None,
            Self::Complete { stats, .. } | Self::Overflow { stats } => Some(*stats),
        }
    }
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
    dev_id: usize,
    /// The vello renderer parented to the same wgpu device as `texture`, cached across frames so we don't pay the pipeline-compile cost on every paint.
    renderer: Renderer,
    /// Offscreen render target. Recreated on resize.
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Readback buffer (padded to the wgpu row alignment requirement).
    readback_buf: wgpu::Buffer,
    /// Bytes-per-row in the padded readback buffer; may exceed `width * 4` when the tight stride doesn't divide 256.
    padded_bytes_per_row: u32,
    /// Tight (no padding) RGBA buffer the public `paint` returns.
    readback: Vec<u8>,
    /// Current size; resize triggers reallocation of `texture` + `readback_buf`.
    size: (u32, u32),
    /// Base color used to clear when the caller's scene is empty; matches the winit backend's fallback so the host window doesn't flash white.
    base_color: Color,
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
    /// Profiling is off in production; when on, timings describe only the latest paint.
    phase_timing_enabled: bool,
    last_phase_timing: Option<BridgePhaseTiming>,
    /// The target and staging buffer contain one complete, settled frame. Both must be true
    /// before a sparse render is allowed to preserve pixels outside its damage list.
    retained_target_valid: bool,
    retained_staging_valid: bool,
    /// Revision associated with the complete retained frame. Legacy full-render APIs do not
    /// carry a revision, so they intentionally clear this even though their target is complete.
    last_successful_revision: Option<u64>,
    /// Process-unique identity and monotonic content epoch for opt-in destination histories.
    xrgb_history_generation: Option<u64>,
    xrgb_history_epoch: Option<u64>,
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
        let mut context = RenderContext::new();
        let dev_id = context.device(None).block_on().ok_or_else(|| {
            BridgeError::NoAdapter(
                "no wgpu adapter (install vulkan/llvmpipe drivers in the runtime)".to_owned(),
            )
        })?;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;

        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);

        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu: false,
                antialiasing_support: AaSupport::all(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(|e| BridgeError::Renderer(format!("{e:?}")))?;

        Ok(Self {
            context,
            dev_id,
            renderer,
            texture,
            view,
            readback_buf,
            padded_bytes_per_row,
            readback: Vec::new(),
            size: (width, height),
            base_color: Color::from_rgb8(0x1A, 0x1B, 0x26),
            last_render_attempts: 0,
            last_sparse_render_attempts: 0,
            last_full_render_attempts: 0,
            last_canonical_segment_order_full_attempts: 0,
            last_canonical_segment_order_sparse_attempts: 0,
            last_area_ppt8_full_attempts: 0,
            last_area_ppt8_sparse_attempts: 0,
            last_clip_single_group_full_attempts: 0,
            last_clip_single_group_sparse_attempts: 0,
            last_sparse_render_stats: None,
            phase_timing_enabled: false,
            last_phase_timing: None,
            retained_target_valid: false,
            retained_staging_valid: false,
            last_successful_revision: None,
            xrgb_history_generation,
            xrgb_history_epoch: Some(1),
            #[cfg(test)]
            fail_next_tracked_swizzle: false,
        })
    }

    /// Override the per-frame clear color; matches the winit backend API so callers can swap themes uniformly.
    pub fn set_base_color(&mut self, color: Color) {
        self.base_color = color;
        // Sparse rendering never clears pixels outside its rectangles. A new clear color is a
        // full-frame change even if the scene recording itself is unchanged.
        self.invalidate_retained();
        self.bump_xrgb_history_epoch();
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
        let (width, height) = self.size;
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
        })
    }

    /// Latest diagnostic timing. `None` means profiling was not enabled for that paint.
    pub fn last_phase_timing(&self) -> Option<BridgePhaseTiming> {
        self.last_phase_timing
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
        self.renderer.gpu_dispatch_timestamp_status()
    }

    /// Consume the one bounded Vello per-dispatch timestamp sample, if its asynchronous map has
    /// completed. Diagnostic callers must match [`vello::GpuDispatchTimestampSample::submission_id`]
    /// to [`Self::gpu_dispatch_timestamp_latest_submission_id`].
    #[doc(hidden)]
    pub fn take_gpu_dispatch_timestamp_sample(
        &mut self,
    ) -> Option<vello::GpuDispatchTimestampSample> {
        self.renderer.take_gpu_dispatch_timestamp_sample()
    }

    /// Identity assigned to the latest Vello recording when opt-in per-dispatch timestamps are
    /// available. The id advances even when the bounded diagnostic readback slot was busy, so a
    /// missing sample must remain missing rather than being attributed to a newer paint.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.renderer.gpu_dispatch_timestamp_latest_submission_id()
    }

    /// Bytes allocated exclusively for opt-in per-dispatch timestamp resolve/readback buffers.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_buffer_bytes(&self) -> u64 {
        self.renderer.gpu_dispatch_timestamp_buffer_bytes()
    }

    /// Effective initialization-time direct path workgroup count, exposed only for an explicit
    /// diagnostic benchmark header. Normal paint behavior does not query or alter it.
    #[doc(hidden)]
    pub fn direct_path_workgroups(&self) -> u32 {
        self.renderer.direct_path_workgroups()
    }

    /// Exact CPU upload bytes from the most recently encoded Vello recording. This borrows
    /// renderer-owned diagnostic state and is valid only until the next render starts.
    #[doc(hidden)]
    pub fn diagnostic_packed_scene(&self) -> &[u8] {
        self.renderer.diagnostic_packed_scene()
    }

    /// Exact resolver-expanded geometry inputs from the latest ordinary recording.
    ///
    /// Profile-only callers use this as an input-opportunity diagnostic; it does not retain GPU
    /// geometry or infer current bump counters from the ordinary settle result.
    #[doc(hidden)]
    pub fn diagnostic_geometry_input(
        &self,
    ) -> Result<vello::GeometryInput<'_>, vello::GeometryInputError> {
        self.renderer.diagnostic_geometry_input()
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

        let (width, height) = self.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let mapped = Self::render_full_to_readback(
            &mut self.renderer,
            &self.context,
            self.dev_id,
            &self.texture,
            &self.view,
            &self.readback_buf,
            padded_bytes_per_row,
            (width, height),
            self.base_color,
            &mut self.last_render_attempts,
            &mut self.last_full_render_attempts,
            Some(&mut self.last_canonical_segment_order_full_attempts),
            Some(&mut self.last_area_ppt8_full_attempts),
            Some(&mut self.last_clip_single_group_full_attempts),
            timing.as_mut(),
            scene,
        )?;

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
        self.invalidate_retained();
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (direct mapped RGBA→XRGB swizzle); \
                 zero-copy DMA-BUF interop remains a follow-up. \
                 Expect ~several ms per frame for a 1080p output."
            );
        }

        let (width, height) = self.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let mapped = Self::render_full_to_readback(
            &mut self.renderer,
            &self.context,
            self.dev_id,
            &self.texture,
            &self.view,
            &self.readback_buf,
            padded_bytes_per_row,
            (width, height),
            self.base_color,
            &mut self.last_render_attempts,
            &mut self.last_full_render_attempts,
            Some(&mut self.last_canonical_segment_order_full_attempts),
            Some(&mut self.last_area_ppt8_full_attempts),
            Some(&mut self.last_clip_single_group_full_attempts),
            timing.as_mut(),
            scene,
        )?;

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
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let Some(damage) = Self::validate_diagnostic_damage(damage, self.size) else {
            return Ok(None);
        };

        let (width, height) = self.size;
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

        self.render_shared_coarse_diagnostic_attempts(
            scene,
            &damage,
            full_dst,
            sparse_dst,
            dst_pitch,
            sparse_target,
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
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let Some(damage) = Self::validate_diagnostic_damage(damage, self.size) else {
            return Ok(None);
        };

        let (width, height) = self.size;
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

        self.render_shared_coarse_geometry_diagnostic_attempts(
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
        self.last_phase_timing = None;
        self.invalidate_retained();
        self.reset_render_attempts();
        let (width, height) = self.size;
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
                .renderer
                .render_to_textures_segment_order_replay_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.view,
                    &right_view,
                    &RenderParams {
                        base_color: self.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    input,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.texture,
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
            self.last_render_attempts = self.last_render_attempts.saturating_add(1);
            let buffers = [
                (&self.readback_buf, self.readback_buf.size()),
                (&right_readback, right_readback.size()),
                (&replay.left, replay.bytes_per_witness),
                (&replay.right, replay.bytes_per_witness),
            ];
            let mapped = Self::map_replay_buffers(&self.context, self.dev_id, &buffers)?;
            if self.renderer.settle_bump_budget(&device) {
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
            attempts: self.last_render_attempts,
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
        let (width, height) = self.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .renderer
                .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.texture,
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
            self.last_render_attempts = self.last_render_attempts.saturating_add(1);
            self.last_sparse_render_attempts = self.last_sparse_render_attempts.saturating_add(1);
            self.last_sparse_render_stats = Some(stats);

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
            attempts: self.last_render_attempts,
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
        let (width, height) = self.size;
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = dev_handle.device.clone();
        let queue = dev_handle.queue.clone();
        for _ in 0..RESILIENT_RENDER_ATTEMPTS {
            let sparse = self
                .renderer
                .render_to_textures_shared_coarse_area_with_post_render(
                    &device,
                    &queue,
                    scene,
                    &self.view,
                    sparse_target.view,
                    &RenderParams {
                        base_color: self.base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    damage,
                    |encoder| {
                        Self::append_full_copy(
                            encoder,
                            &self.texture,
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
            self.last_render_attempts = self.last_render_attempts.saturating_add(1);
            self.last_sparse_render_attempts = self.last_sparse_render_attempts.saturating_add(1);
            self.last_sparse_render_stats = Some(stats);

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
            attempts: self.last_render_attempts,
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
        let (width, height) = self.size;
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
        if self.renderer.settle_bump_budget(device) {
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

    #[allow(
        clippy::too_many_arguments,
        reason = "The diagnostic owns paired targets, destination slices, and same-frame geometry."
    )]
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
        let (width, height) = self.size;
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
        if self.renderer.settle_bump_budget(device) {
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
            Self::validate_complete_xrgb_destination(dst, dst_pitch, self.size)?;
        }
        self.last_phase_timing = None;
        if !WARNED_CPU_FALLBACK.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                "DRM paint: CPU fallback active — vello → wgpu readback → XRGB8888 \
                 (retained sparse damage when version-safe); zero-copy DMA-BUF interop remains \
                 a follow-up."
            );
        }

        // Decide against the last *complete* frame before invalidating it. From this point until
        // render, copy, map, and bump settlement all succeed, no frame may be reused as sparse
        // backing state.
        let sparse_damage = self.eligible_sparse_damage(damage);
        let mut damage_admitted = sparse_damage.is_some();
        self.invalidate_retained();

        let (width, height) = self.size;
        let padded_bytes_per_row = self.padded_bytes_per_row;
        let mut timing = self.phase_timing_enabled.then(BridgePhaseTiming::default);
        self.reset_render_attempts();
        let mapped = if let Some(regions) = sparse_damage.as_deref() {
            let sparse_readback = Self::render_sparse_to_readback(
                &mut self.renderer,
                &self.context,
                self.dev_id,
                &self.texture,
                &self.view,
                &self.readback_buf,
                padded_bytes_per_row,
                (width, height),
                self.base_color,
                &mut self.last_render_attempts,
                &mut self.last_sparse_render_attempts,
                Some(&mut self.last_canonical_segment_order_sparse_attempts),
                Some(&mut self.last_area_ppt8_sparse_attempts),
                Some(&mut self.last_clip_single_group_sparse_attempts),
                timing.as_mut(),
                scene,
                regions,
            )?;
            self.last_sparse_render_stats = sparse_readback.stats();
            match sparse_readback {
                SparseReadback::Complete { mapped, .. } => mapped,
                // Sparse overflow can leave incomplete pixels in both retained resources. The
                // dense retry begins from the scene and copies every pixel before publication.
                SparseReadback::Unavailable | SparseReadback::Overflow { .. } => {
                    // A dense recovery publishes complete pixels, but it is intentionally a
                    // destination-history boundary rather than proof that other targets still
                    // have every skipped intermediate change.
                    damage_admitted = false;
                    Self::render_full_to_readback(
                        &mut self.renderer,
                        &self.context,
                        self.dev_id,
                        &self.texture,
                        &self.view,
                        &self.readback_buf,
                        padded_bytes_per_row,
                        (width, height),
                        self.base_color,
                        &mut self.last_render_attempts,
                        &mut self.last_full_render_attempts,
                        Some(&mut self.last_canonical_segment_order_full_attempts),
                        Some(&mut self.last_area_ppt8_full_attempts),
                        Some(&mut self.last_clip_single_group_full_attempts),
                        timing.as_mut(),
                        scene,
                    )?
                }
            }
        } else {
            Self::render_full_to_readback(
                &mut self.renderer,
                &self.context,
                self.dev_id,
                &self.texture,
                &self.view,
                &self.readback_buf,
                padded_bytes_per_row,
                (width, height),
                self.base_color,
                &mut self.last_render_attempts,
                &mut self.last_full_render_attempts,
                Some(&mut self.last_canonical_segment_order_full_attempts),
                Some(&mut self.last_area_ppt8_full_attempts),
                Some(&mut self.last_clip_single_group_full_attempts),
                timing.as_mut(),
                scene,
            )?
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

    /// Select sparse rendering only when every unchanged pixel is backed by the exact preceding
    /// complete frame. `FineDamageRect` is integer-native already, so "normalization" here means
    /// rejecting degenerate/out-of-bounds rectangles and removing only exact/contained duplicates.
    fn eligible_sparse_damage(
        &self,
        damage: SceneDamage<'_>,
    ) -> Option<Vec<vello::FineDamageRect>> {
        let previous = damage.previous_revision?;
        if self.last_successful_revision != Some(previous)
            || !self.retained_target_valid
            || !self.retained_staging_valid
            || !self.renderer.sparse_fine_supported()
        {
            return None;
        }
        Self::normalize_damage(damage.regions?, self.size)
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
        self.retained_target_valid = false;
        self.retained_staging_valid = false;
        self.last_successful_revision = None;
    }

    #[inline]
    fn mark_retained_complete(&mut self, revision: Option<u64>) {
        self.retained_target_valid = true;
        self.retained_staging_valid = true;
        self.last_successful_revision = revision;
    }

    #[inline]
    fn reset_render_attempts(&mut self) {
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
    fn swizzle_xrgb_row(dst: &mut [u8], source: &[u8]) {
        for (d, s) in dst.chunks_exact_mut(4).zip(source.chunks_exact(4)) {
            if let ([db, dg, dr, dx], [sr, sg, sb, _sa]) = (d, s) {
                *db = *sb;
                *dg = *sg;
                *dr = *sr;
                *dx = 0xFF;
            }
        }
    }

    /// Render `scene` densely, append the full texture→`readback_buf` copy to Vello's own
    /// submission, then map the completed copy. A failed bump-budget attempt is unmapped and
    /// retried before its pixels can reach either public output path.
    #[allow(
        clippy::too_many_arguments,
        reason = "the owned mapped view keeps paint's destination and tight RGBA buffer independently available to its caller"
    )]
    fn render_full_to_readback(
        renderer: &mut Renderer,
        context: &RenderContext,
        dev_id: usize,
        texture: &wgpu::Texture,
        view: &wgpu::TextureView,
        readback_buf: &wgpu::Buffer,
        padded_bytes_per_row: u32,
        size: (u32, u32),
        base_color: Color,
        last_render_attempts: &mut usize,
        last_full_render_attempts: &mut usize,
        mut canonical_segment_order_full_attempts: Option<&mut usize>,
        mut area_ppt8_full_attempts: Option<&mut usize>,
        mut clip_single_group_full_attempts: Option<&mut usize>,
        mut phase_timing: Option<&mut BridgePhaseTiming>,
        scene: &Scene,
    ) -> Result<wgpu::BufferView, BridgeError> {
        let (width, height) = size;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let queue = &dev_handle.queue;

        // A sparse submission that overflowed has already consumed one bounded attempt. Do not
        // turn that recovery into an unbounded fifth submission.
        let remaining_attempts = RESILIENT_RENDER_ATTEMPTS.saturating_sub(*last_render_attempts);
        for _ in 0..remaining_attempts {
            *last_render_attempts = (*last_render_attempts).saturating_add(1);
            *last_full_render_attempts = (*last_full_render_attempts).saturating_add(1);
            let submit_started = phase_timing.as_ref().map(|_| Instant::now());
            renderer
                .render_to_texture_with_post_render(
                    device,
                    queue,
                    scene,
                    view,
                    &RenderParams {
                        base_color,
                        width,
                        height,
                        antialiasing_method: AaConfig::Area,
                    },
                    |encoder| {
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
                    },
                )
                .map_err(|e| BridgeError::Render(format!("{e:?}")))?;
            if renderer.last_canonical_segment_order_selected()
                && let Some(selected_attempts) =
                    canonical_segment_order_full_attempts.as_deref_mut()
            {
                *selected_attempts = selected_attempts.saturating_add(1);
            }
            if renderer.last_area_ppt8_selected()
                && let Some(selected_attempts) = area_ppt8_full_attempts.as_deref_mut()
            {
                *selected_attempts = selected_attempts.saturating_add(1);
            }
            if renderer.last_clip_single_group_selected()
                && let Some(selected_attempts) = clip_single_group_full_attempts.as_deref_mut()
            {
                *selected_attempts = selected_attempts.saturating_add(1);
            }

            if let (Some(timing), Some(started)) = (phase_timing.as_deref_mut(), submit_started) {
                timing.submit_us = timing
                    .submit_us
                    .saturating_add(started.elapsed().as_micros());
            }
            if let Some(timing) = phase_timing.as_deref_mut() {
                let copied = u64::from(width)
                    .saturating_mul(u64::from(height))
                    .saturating_mul(4);
                timing.readback_bytes = timing.readback_bytes.saturating_add(copied);
            }

            // `map_readback` is the only blocking wait: its one-second fence deadline covers
            // Vello rendering and this appended copy, so this is GPU-completion-plus-map time,
            // not a readback-copy-only measurement. Only once that work completed do we consume the
            // bump counter; its internal poll is then an already-complete, non-serial settle.
            let map_started = phase_timing.as_ref().map(|_| Instant::now());
            let mapped = Self::map_readback(context, dev_id, readback_buf)?;
            if let (Some(timing), Some(started)) = (phase_timing.as_deref_mut(), map_started) {
                timing.completed_readback_map_us = timing
                    .completed_readback_map_us
                    .saturating_add(started.elapsed().as_micros());
            }
            let settle_started = phase_timing.as_ref().map(|_| Instant::now());
            let settled = renderer.settle_bump_budget(device);
            if let (Some(timing), Some(started)) = (phase_timing.as_deref_mut(), settle_started) {
                timing.settle_us = timing
                    .settle_us
                    .saturating_add(started.elapsed().as_micros());
            }
            if !settled {
                return Ok(mapped);
            }
            drop(mapped);
            readback_buf.unmap();
        }
        Err(BridgeError::RecoveryExhausted {
            attempts: *last_render_attempts,
        })
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

    /// Submit an exact sparse render and append one texture→staging copy for every normalized
    /// damage rectangle. All copies share the staging buffer's native padded stride, with each
    /// rectangle placed at `y * stride + x * 4`; untouched bytes therefore remain the previous
    /// complete frame. No command is submitted when Vello cannot use sparse Area rendering.
    #[allow(
        clippy::too_many_arguments,
        reason = "the sparse path mirrors the full path so its retained-resource transaction is explicit"
    )]
    fn render_sparse_to_readback(
        renderer: &mut Renderer,
        context: &RenderContext,
        dev_id: usize,
        texture: &wgpu::Texture,
        view: &wgpu::TextureView,
        readback_buf: &wgpu::Buffer,
        padded_bytes_per_row: u32,
        size: (u32, u32),
        base_color: Color,
        last_render_attempts: &mut usize,
        last_sparse_render_attempts: &mut usize,
        canonical_segment_order_sparse_attempts: Option<&mut usize>,
        area_ppt8_sparse_attempts: Option<&mut usize>,
        clip_single_group_sparse_attempts: Option<&mut usize>,
        mut phase_timing: Option<&mut BridgePhaseTiming>,
        scene: &Scene,
        damage: &[vello::FineDamageRect],
    ) -> Result<SparseReadback, BridgeError> {
        let (width, height) = size;
        let dev_handle = context.devices.get(dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {dev_id} out of range"))
        })?;
        let device = &dev_handle.device;
        let queue = &dev_handle.queue;
        let submit_started = phase_timing.as_ref().map(|_| Instant::now());
        let sparse = renderer
            .render_to_texture_with_damage_and_post_render(
                device,
                queue,
                scene,
                view,
                &RenderParams {
                    base_color,
                    width,
                    height,
                    antialiasing_method: AaConfig::Area,
                },
                damage,
                |encoder| {
                    for rect in damage {
                        let offset = u64::from(rect.y)
                            .saturating_mul(u64::from(padded_bytes_per_row))
                            .saturating_add(u64::from(rect.x).saturating_mul(4));
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
                                buffer: readback_buf,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset,
                                    bytes_per_row: Some(padded_bytes_per_row),
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
                },
            )
            .map_err(|e| BridgeError::Render(format!("{e:?}")))?;
        let Some(stats) = sparse else {
            return Ok(SparseReadback::Unavailable);
        };
        *last_render_attempts = (*last_render_attempts).saturating_add(1);
        *last_sparse_render_attempts = (*last_sparse_render_attempts).saturating_add(1);
        Self::record_sparse_shader_selected_attempts(
            &stats,
            canonical_segment_order_sparse_attempts,
            area_ppt8_sparse_attempts,
            clip_single_group_sparse_attempts,
        );
        if let (Some(timing), Some(started)) = (phase_timing.as_deref_mut(), submit_started) {
            timing.submit_us = timing
                .submit_us
                .saturating_add(started.elapsed().as_micros());
        }
        if let Some(timing) = phase_timing.as_deref_mut() {
            let copied = Self::sparse_damage_readback_bytes(damage);
            timing.readback_bytes = timing.readback_bytes.saturating_add(copied);
        }

        let map_started = phase_timing.as_ref().map(|_| Instant::now());
        let mapped = Self::map_readback(context, dev_id, readback_buf)?;
        if let (Some(timing), Some(started)) = (phase_timing.as_deref_mut(), map_started) {
            timing.completed_readback_map_us = timing
                .completed_readback_map_us
                .saturating_add(started.elapsed().as_micros());
        }
        let settle_started = phase_timing.as_ref().map(|_| Instant::now());
        let overflowed = renderer.settle_bump_budget(device);
        if let (Some(timing), Some(started)) = (phase_timing, settle_started) {
            timing.settle_us = timing
                .settle_us
                .saturating_add(started.elapsed().as_micros());
        }
        if overflowed {
            drop(mapped);
            readback_buf.unmap();
            Ok(SparseReadback::Overflow { stats })
        } else {
            Ok(SparseReadback::Complete { mapped, stats })
        }
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
        if let Err(e) = waited {
            // Cancels an in-flight map request (or releases a map that raced to completion) so
            // the next production frame can retry instead of seeing this buffer as pending.
            readback_buf.unmap();
            return Err(BridgeError::Map(format!("fence wait: {e:?}")));
        }
        match rx.recv_timeout(std::time::Duration::from_secs(1)) {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(format!("{e:?}")));
            }
            Err(e) => {
                readback_buf.unmap();
                return Err(BridgeError::Map(format!("map_async channel: {e}")));
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
        self.bump_xrgb_history_epoch();
        if width == 0 || height == 0 {
            return Err(BridgeError::InvalidSize(format!(
                "VelloDrmBridge::resize: width/height must be > 0, got {width}x{height}"
            )));
        }
        if (width, height) == self.size {
            return Ok(());
        }
        let dev_handle = self.context.devices.get(self.dev_id).ok_or_else(|| {
            BridgeError::InvalidDevice(format!("wgpu device id {} out of range", self.dev_id))
        })?;
        let device = &dev_handle.device;
        let (texture, view, readback_buf, padded_bytes_per_row) =
            Self::allocate_targets(device, width, height);
        self.texture = texture;
        self.view = view;
        self.readback_buf = readback_buf;
        self.padded_bytes_per_row = padded_bytes_per_row;
        self.size = (width, height);
        // Reset the tight buffer for the next `paint`; the new size may be larger, so clearing is cleaner than keeping capacity.
        self.readback.clear();
        Ok(())
    }

    /// Current (width, height); useful in tests and for comparing against the connector's mode size before resizing.
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    /// Vello submissions used by the latest paint, including any sparse attempt and bounded
    /// dense bump-budget recovery retries.
    pub fn last_render_attempts(&self) -> usize {
        self.last_render_attempts
    }

    /// Sparse Vello submissions used by the latest paint. This remains zero for every legacy
    /// full-render API and for conservative versioned fallbacks.
    pub fn last_sparse_render_attempts(&self) -> usize {
        self.last_sparse_render_attempts
    }

    /// Dense Vello submissions used by the latest paint, including bump-budget retries.
    pub fn last_full_render_attempts(&self) -> usize {
        self.last_full_render_attempts
    }

    /// Full render attempts from the latest paint which actually recorded canonical segment
    /// ordering. A selected pass is not evidence of completion or performance improvement.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_full_attempts(&self) -> usize {
        self.last_canonical_segment_order_full_attempts
    }

    /// Sparse render attempts from the latest paint which actually recorded canonical segment
    /// ordering, including an overflowed sparse attempt before dense recovery.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_sparse_attempts(&self) -> usize {
        self.last_canonical_segment_order_sparse_attempts
    }

    /// Full render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_full_attempts(&self) -> usize {
        self.last_area_ppt8_full_attempts
    }

    /// Sparse render attempts from the latest paint that actually selected Area PPT8.
    #[doc(hidden)]
    pub fn last_area_ppt8_sparse_attempts(&self) -> usize {
        self.last_area_ppt8_sparse_attempts
    }

    /// Full render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_full_attempts(&self) -> usize {
        self.last_clip_single_group_full_attempts
    }

    /// Sparse render attempts from the latest paint that selected single-workgroup clip leaf.
    #[doc(hidden)]
    pub fn last_clip_single_group_sparse_attempts(&self) -> usize {
        self.last_clip_single_group_sparse_attempts
    }

    /// Actual sparse coarse-dispatch selection from the latest paint, if sparse submission was
    /// reached. This is present even when a sparse overflow forced a later dense recovery.
    pub fn last_sparse_render_stats(&self) -> Option<vello::SparseRenderStats> {
        self.last_sparse_render_stats
    }

    /// Whether path-level sparse tile bounds were requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_requested(&self) -> bool {
        self.renderer.sparse_tile_bounds_requested()
    }

    /// Whether this renderer can apply path-level sparse tile bounds.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_available(&self) -> bool {
        self.renderer.sparse_tile_bounds_available()
    }

    /// Whether sparse Area fine quad skipping was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_requested(&self) -> bool {
        self.renderer.sparse_fine_quad_skip_requested()
    }

    /// Whether sparse Area fine quad skipping can be selected for this renderer.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_available(&self) -> bool {
        self.renderer.sparse_fine_quad_skip_available()
    }

    /// Whether sparse coarse opaque pruning was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_requested(&self) -> bool {
        self.renderer.sparse_opaque_prune_requested()
    }

    /// Whether sparse coarse opaque pruning can select its shader for this renderer.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_available(&self) -> bool {
        self.renderer.sparse_opaque_prune_available()
    }

    /// Whether sparse solid-fuse selection was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_requested(&self) -> bool {
        self.renderer.sparse_solid_fuse_requested()
    }

    /// Whether sparse solid-fuse selection can be used by this renderer.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_available(&self) -> bool {
        self.renderer.sparse_solid_fuse_available()
    }

    /// Whether sparse segment-row masking was requested for this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_requested(&self) -> bool {
        self.renderer.sparse_segment_row_mask_requested()
    }

    /// Whether sparse segment-row masking can be selected by this renderer.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_available(&self) -> bool {
        self.renderer.sparse_segment_row_mask_available()
    }

    /// Whether canonical segment ordering was requested for this renderer.
    #[doc(hidden)]
    pub fn canonical_segment_order_requested(&self) -> bool {
        self.renderer.canonical_segment_order_requested()
    }

    /// Whether this renderer has the canonical segment-ordering pass available.
    #[doc(hidden)]
    pub fn canonical_segment_order_available(&self) -> bool {
        self.renderer.canonical_segment_order_available()
    }

    /// Whether Area PPT8 was requested for this renderer.
    #[doc(hidden)]
    pub fn area_ppt8_requested(&self) -> bool {
        self.renderer.area_ppt8_requested()
    }

    /// Whether this renderer has the ordinary Area PPT8 full shader available.
    #[doc(hidden)]
    pub fn area_ppt8_available(&self) -> bool {
        self.renderer.area_ppt8_available()
    }

    /// Whether single-workgroup clip leaf was requested for this renderer.
    #[doc(hidden)]
    pub fn clip_single_group_requested(&self) -> bool {
        self.renderer.clip_single_group_requested()
    }

    /// Whether the single-workgroup clip leaf permutation is available.
    #[doc(hidden)]
    pub fn clip_single_group_available(&self) -> bool {
        self.renderer.clip_single_group_available()
    }

    /// Allocate (texture, view, `readback_buf`, `padded_bytes_per_row`) for the given size; shared by `new` and `resize`.
    fn allocate_targets(
        device: &wgpu::Device,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, wgpu::TextureView, wgpu::Buffer, u32) {
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quick-desktop-drm-paint-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let readback_size = u64::from(padded_bytes_per_row).saturating_mul(u64::from(height));
        let readback_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("quick-desktop-drm-paint-readback"),
            size: readback_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        (texture, view, readback_buf, padded_bytes_per_row)
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
            if let ([db, dg, dr, dx], [sr, sg, sb, _sa]) = (dpix, spix) {
                *db = *sb;
                *dg = *sg;
                *dr = *sr;
                *dx = 0xFF;
            }
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
    fn whole_buffer_swizzles_like_paint_into_xrgb8888() {
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

    #[test]
    fn offset_subrect_picks_the_right_source_region() {
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

    #[test]
    fn destination_pitch_padding_is_untouched() {
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

    #[test]
    fn overhanging_output_is_clamped_not_panicking() {
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

    #[test]
    fn rotate_90_maps_portrait_scene_to_landscape_scanout() {
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

    #[test]
    fn profiled_path_copies_only_the_output_viewport() {
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

#[cfg(test)]
mod tests {
    //! These tests need a working wgpu adapter (llvmpipe inside the distrobox); soft-skip on
    //! adapter-acquisition failure so a CI host without Vulkan doesn't flake. Run inside `my-distrobox`, where llvmpipe is available.
    use super::*;
    use crate::compositor::frame::FrameBuilder;
    use automerge::{AutoCommit, ROOT};
    use ui::taffy::NodeId;
    use vello::kurbo::Affine;
    use vello::peniko::Fill;
    use winit::dpi::PhysicalPosition;
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton, WindowEvent};

    fn bridge_or_skip(w: u32, h: u32) -> Option<VelloDrmBridge> {
        match VelloDrmBridge::new(w, h) {
            Ok(b) => Some(b),
            Err(BridgeError::NoAdapter(msg)) => {
                eprintln!("skipping paint test: {msg}");
                None
            }
            Err(other) => panic!("unexpected bridge error: {other:?}"),
        }
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
            Self::validate_complete_xrgb_destination(dst, dst_pitch, self.size)?;
            let mapped = Self::map_readback(&self.context, self.dev_id, &self.readback_buf)?;
            let result = Self::validate_complete_mapped_xrgb_source(MappedXrgbSource {
                bytes: &mapped,
                padded_bytes_per_row: self.padded_bytes_per_row as usize,
                width: self.size.0 as usize,
                height: self.size.1 as usize,
            })
            .map(|()| {
                Self::swizzle_mapped_xrgb8888(
                    MappedXrgbSource {
                        bytes: &mapped,
                        padded_bytes_per_row: self.padded_bytes_per_row as usize,
                        width: self.size.0 as usize,
                        height: self.size.1 as usize,
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
        let _ = frame.dispatch_event(&cursor, &modifiers, document, &ROOT, focus);
        let pressed = frame.dispatch_event(&press, &modifiers, document, &ROOT, focus);
        let released = frame.dispatch_event(&release, &modifiers, document, &ROOT, focus);
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
    fn tracked_full_swizzle_rejects_truncated_source_without_mutating_destination() {
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

    #[test]
    fn tracked_partial_swizzle_validates_every_rect_before_mutating_destination() {
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

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    fn tracked_short_destination_and_prepared_damage_mismatch_abort_without_writes() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    fn tracked_swizzle_error_unmaps_before_the_next_paint() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    fn saturated_bridge_history_epoch_disables_tracking_but_not_full_paint() {
        let Some(mut bridge) = bridge_or_skip(16, 12) else {
            return;
        };
        bridge.xrgb_history_epoch = Some(u64::MAX);
        bridge.set_base_color(Color::from_rgb8(0x20, 0x30, 0x40));
        assert!(!bridge.xrgb_history_available());
        let mut destination = vec![0xA7; 16 * 12 * 4];
        bridge
            .paint_into_xrgb8888(&Scene::new(), &mut destination, 16 * 4)
            .expect("ordinary full paint remains available after tracking epoch saturation");
    }

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    fn history_bridge_accepts_gap_and_oversized_damage_as_real_full_boundaries() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        reason = "fixed test dimensions are bounded by their literal fixture values"
    )]
    fn history_bridge_accepts_nonzero_scanout_epoch_across_two_targets() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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
            .paint_damage_into_xrgb8888_with_history(&scene, first, &transaction, &mut left, pitch)
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

    #[test]
    #[allow(
        clippy::arithmetic_side_effects,
        clippy::too_many_lines,
        reason = "the two-target fixture keeps each ordered history transaction and exact staging comparison together"
    )]
    fn history_two_targets_repair_padded_destination_from_current_staging() {
        let (width, height) = (64_u32, 48_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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
        if bridge.renderer.sparse_fine_supported() {
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
        let Some(mut one_bridge) = bridge_or_skip(WIDTH, HEIGHT) else {
            return;
        };
        let Some(mut two_bridge) = bridge_or_skip(WIDTH, HEIGHT) else {
            return;
        };
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
        let mut frame = FrameBuilder::new((WIDTH, HEIGHT));
        let mut document = AutoCommit::new();
        let mut focus = None;
        let mut one_partial = 0_u64;
        let mut two_partial = 0_u64;

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
    fn l_shaped_damage_does_not_expand_to_a_bounding_box() {
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

    #[test]
    fn shared_coarse_diagnostic_accepts_full_damage_without_relaxing_sparse_policy() {
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

    #[test]
    fn versioned_damage_retains_full_pixels_and_copies_only_dirty_rectangles() {
        let (width, height) = (64_u32, 48_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

        let sparse_supported = bridge.renderer.sparse_fine_supported();
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

        let Some(mut oracle) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    fn overlapping_disjoint_and_edge_damage_matches_dense_oracle() {
        let (width, height) = (64_u32, 48_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

        let Some(mut oracle) = bridge_or_skip(width, height) else {
            return;
        };
        let mut expected = vec![0; pitch * height as usize];
        oracle
            .paint_into_xrgb8888(&second, &mut expected, pitch)
            .expect("dense oracle");
        assert_xrgb_pixels_eq(&actual, &expected, width as usize, height as usize, pitch);
    }

    #[test]
    fn revision_gap_and_empty_damage_force_full_readback() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    fn failed_versioned_paint_invalidates_sparse_backing_before_retry() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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
            !bridge.retained_target_valid
                && !bridge.retained_staging_valid
                && bridge.last_successful_revision.is_none(),
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

    #[test]
    fn sparse_overflow_recovers_dense_without_publishing_partial_pixels() {
        let (width, height) = (384_u32, 256_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
        if !bridge.renderer.sparse_fine_supported() {
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

        let Some(mut oracle) = bridge_or_skip(width, height) else {
            return;
        };
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
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
        if !bridge.renderer.sparse_fine_supported() {
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
    fn base_color_and_resize_invalidate_versioned_sparse_backing() {
        let (width, height) = (32_u32, 24_u32);
        let Some(mut bridge) = bridge_or_skip(width, height) else {
            return;
        };
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

    #[test]
    fn bridge_paint_returns_non_empty_buffer_for_non_empty_scene() {
        let Some(mut bridge) = bridge_or_skip(64, 64) else {
            return;
        };
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

    #[test]
    fn diagnostic_zero_pitch_records_readback_but_no_output_bytes() {
        let Some(mut bridge) = bridge_or_skip(8, 4) else {
            return;
        };
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

    #[test]
    fn diagnostic_xrgb_counts_written_bytes_without_changing_pixels() {
        let Some(mut bridge) = bridge_or_skip(8, 4) else {
            return;
        };
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

    #[test]
    fn bridge_resize_changes_buffer_size() {
        let Some(mut bridge) = bridge_or_skip(1600, 900) else {
            return;
        };
        let bytes = bridge.paint(&Scene::new()).expect("paint at initial size");
        assert_eq!(bytes.len(), 1600 * 900 * 4);

        // Resize to 1280x720 and verify the next paint returns the new buffer size.
        bridge.resize(1280, 720).expect("resize");
        assert_eq!(bridge.size(), (1280, 720));
        let bytes = bridge.paint(&Scene::new()).expect("paint after resize");
        assert_eq!(bytes.len(), 1280 * 720 * 4);
    }

    #[test]
    fn bridge_paint_uses_base_color_for_empty_scene() {
        let Some(mut bridge) = bridge_or_skip(32, 32) else {
            return;
        };
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

    #[test]
    fn paint_into_xrgb8888_matches_paint_swizzled() {
        // The direct XRGB path must produce the same pixels as tight-RGBA `paint()` output
        // swizzled to XRGB8888.
        let Some(mut bridge) = bridge_or_skip(40, 24) else {
            return;
        };
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

    #[test]
    fn paint_into_xrgb8888_respects_destination_pitch() {
        // A destination whose pitch exceeds width*4 (the common DRM dumb-buffer case) must leave trailing padding untouched and place each row at `row * dst_pitch`.
        let Some(mut bridge) = bridge_or_skip(8, 4) else {
            return;
        };
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

    #[test]
    fn bridge_handles_non_aligned_width_via_padding() {
        // Width=33 → tight stride=132, padded to 256 — verifies the row-pitch stripping in `paint`.
        let Some(mut bridge) = bridge_or_skip(33, 16) else {
            return;
        };
        bridge.set_base_color(Color::from_rgb8(0xAA, 0xBB, 0xCC));
        let bytes = bridge.paint(&Scene::new()).expect("paint non-aligned");
        assert_eq!(bytes.len(), 33 * 16 * 4);
        for p in bytes.chunks_exact(4) {
            assert_eq!((p[0], p[1], p[2]), (0xAA, 0xBB, 0xCC));
        }
    }

    #[test]
    fn overflow_recovery_retries_before_xrgb_output() {
        let (width, height) = (384, 256);
        let Some(mut xrgb_bridge) = bridge_or_skip(width, height) else {
            return;
        };
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
        let Some(mut rgba_bridge) = bridge_or_skip(width, height) else {
            return;
        };
        let rgba = rgba_bridge
            .paint(&scene)
            .expect("cold opaque-stack RGBA paint")
            .to_vec();
        assert!(rgba_bridge.last_render_attempts() >= 2);
        for (index, (rgba, xrgb)) in rgba.chunks_exact(4).zip(xrgb.chunks_exact(4)).enumerate() {
            assert_eq!(rgba, &[0xFF, 0xFD, 0xA5, 0xFF], "RGBA pixel {index}");
            assert_eq!(xrgb, &[rgba[2], rgba[1], rgba[0], 0xFF], "pixel {index}");
        }
    }

    #[test]
    fn zero_destination_pitch_unmaps_before_the_next_frame() {
        let Some(mut bridge) = bridge_or_skip(8, 4) else {
            return;
        };
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
}
