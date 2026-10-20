//! Headless offscreen render harness: renders a Vello `Scene` into a GPU
//! texture with no window, reads the bytes back, optionally writes a PNG.
//!
//! `copy_texture_to_buffer` needs the buffer's row pitch aligned to
//! `COPY_BYTES_PER_ROW_ALIGNMENT` (256 today), so a non-aligned width is
//! padded and the padding stripped on readback.
//!
//! Two renders of the *same* `Scene` are not guaranteed bit-identical:
//! vello assigns path-segment order via `atomicAdd` (GPU scheduling), and
//! float addition isn't associative, so coverage on a quantisation
//! boundary can land ±1 in a channel. Measured here: ≤4/57 600 pixels,
//! always ±1, ink-count unchanged — a race between concurrent renders in
//! one process, not a single-frame artifact. The order also follows the
//! encoding: two scenes that differ only in a path far away render a pixel
//! one unit apart, though each renders bit-identically every time. Measured
//! in the block editor's retained-paint test: moving a 2 px caret in the
//! heading (x 76.5 to 91.2, y 8 to 40.5) changed one paragraph pixel 170 px
//! away, (246, 77), from blue 51 to 52, with every glyph, draw and transform
//! of the paragraph unchanged and repeated renders of either scene equal.
//! Retained paint, which keeps undamaged pixels from the earlier scene,
//! therefore matches a full render only within ±1. Compare with
//! [`OffscreenRenderer::pixels_differing_beyond_aa`], not byte equality. The
//! stored pixels use straight alpha: RGB in fully transparent pixels is
//! invisible, while if either pixel has nonzero alpha, every channel keeps the
//! ±1 comparison tolerance. Exact untouched-pixel assertions remain separate
//! checks for byte preservation.
//!
//! Never cache one in a `thread_local!`: dropping wgpu resources touches
//! wgpu's own thread-local, and TLS destructors run in reverse
//! registration order, so a cache registered before the first wgpu call is
//! torn down after wgpu's — `SIGABRT`, killing the process. Use a
//! process-wide `static` instead (never drops); this type is `Send` to
//! allow that.

use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use crate::damage::{Region, coalesce_regions};
use crate::gpu::RenderContext;
use pollster::FutureExt;
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::{AaConfig, AaSupport, FineDamageRect, RenderParams, Renderer, RendererOptions, Scene};

/// Device pixels per logical pixel a capture draws its frames at
/// (`--device-scale`); 1 is the size people see on a standard display.
static CAPTURE_DEVICE_SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// The largest device scale a capture draws at: a glyph sheet enlarged
/// from the outlines (16x); `--device-scale` itself stops at 4.
pub const MAX_CAPTURE_DEVICE_SCALE: u32 = 16;

/// Draw every following capture frame at `scale` device pixels per
/// logical pixel, as a HiDPI display draws it: the frame is laid out at its
/// logical size and drawn with the window scale transform the live host
/// uses, so text is rasterised from the glyph outlines at that size rather
/// than enlarged afterwards ([`crate::headless::FrameHarness`]). Clamped to
/// `1..=MAX_CAPTURE_DEVICE_SCALE`.
pub fn set_capture_device_scale(scale: u32) {
    CAPTURE_DEVICE_SCALE.store(
        scale.clamp(1, MAX_CAPTURE_DEVICE_SCALE),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The device scale captures draw at ([`set_capture_device_scale`]).
#[must_use]
pub fn capture_device_scale() -> u32 {
    CAPTURE_DEVICE_SCALE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Ensures only ONE `wgpu::Instance` per process — constructing several in
/// parallel (the default `cargo test` pattern) races inside
/// `vkCreateInstance` on at least radv + intel-iris and SIGSEGVs. `Arc` lets
/// `OffscreenRenderers` keep the context alive past process-stack frames;
/// `Mutex` because `RenderContext::device(...)` takes `&mut self`.
static SHARED_RENDER_CONTEXT: OnceLock<Arc<Mutex<RenderContext>>> = OnceLock::new();

/// Submissions a one-shot render may spend without growing Vello's bump
/// reservation: two clean ones to confirm the atlas warm-up, and room for a
/// transparent or overflowed one that taught nothing. Keeping this bounded
/// is important on software Vulkan: a full-size screenshot is otherwise able
/// to monopolize a CPU core for minutes before it can publish anything.
const RESILIENT_RENDER_ATTEMPTS: usize = 4;

/// Growths of the bump reservation a one-shot render may wait through on top
/// of [`RESILIENT_RENDER_ATTEMPTS`]. Vello's stages skip their work once an
/// earlier one has overflowed, so a cold renderer learns one stage's demand
/// per submission: lines, then tiles and binning, then segment counts, then
/// the command list, blend spill and segments. A dense scene (a feathered
/// mask traced from a photograph) needs most of them in turn. Every growth
/// at least doubles a buffer towards a finite ceiling, so each one is
/// progress, and a reservation that has stopped growing falls back to the
/// attempt limit above.
const RESILIENT_RENDER_GROWTHS: usize = 8;

/// The submissions a one-shot render has spent, and whether it may make
/// another.
#[derive(Debug, Default)]
struct ResilientAttempts {
    /// Submissions that did not grow the reservation.
    stalled: usize,
    /// Submissions that grew it.
    growths: usize,
}

impl ResilientAttempts {
    /// Count a submission that `grew` the bump reservation (or did not) and
    /// answer whether another one may follow.
    const fn another(&mut self, grew: bool) -> bool {
        if grew && self.growths < RESILIENT_RENDER_GROWTHS {
            self.growths += 1;
        } else {
            self.stalled += 1;
        }
        self.stalled < RESILIENT_RENDER_ATTEMPTS
    }

    const fn submissions(&self) -> usize {
        self.stalled + self.growths
    }
}

/// Decide whether a screenshot submission is safe to return. A non-empty
/// scene must fit twice in succession: the first clean submission may still
/// be populating Vello's atlas after a bump allocation changed.
const fn resilient_submission_accepted(scene_is_empty: bool, confirmed: bool) -> bool {
    scene_is_empty || confirmed
}

/// Damage targets change by a handful of pixels during animation. Keeping
/// their allocations on a small grid avoids a texture allocation for every
/// slightly different progress-bar segment while wasting at most 31 pixels
/// per dimension.
const DAMAGE_TEXTURE_GRANULARITY: u32 = 32;
const DAMAGE_TEXTURE_HEIGHT_GRANULARITY: u32 = 16;

fn env_enabled_by_default(name: &str) -> bool {
    !matches!(crate::env::var(name).as_deref(), Ok("0"))
}

pub(crate) fn damage_geometric_capacity_enabled() -> bool {
    env_enabled_by_default("TILO_UI_DAMAGE_GEOMETRIC")
}

pub(crate) fn damage_texture_extent(required: u32, device_limit: u32) -> u32 {
    required
        .div_ceil(DAMAGE_TEXTURE_GRANULARITY)
        .saturating_mul(DAMAGE_TEXTURE_GRANULARITY)
        .min(device_limit)
        .max(required)
}

/// Prefix targets are sampled/copied on Vello's 16px vertical tile grid, so
/// their height can use the smaller quantum without changing rendered pixels.
pub(crate) fn damage_texture_height_extent(required: u32, device_limit: u32) -> u32 {
    required
        .div_ceil(DAMAGE_TEXTURE_HEIGHT_GRANULARITY)
        .saturating_mul(DAMAGE_TEXTURE_HEIGHT_GRANULARITY)
        .min(device_limit)
        .max(required)
}

pub(crate) fn damage_texture_geometric_width_extent(
    required: u32,
    viewport_width: u32,
    device_limit: u32,
) -> u32 {
    let cap = viewport_width.min(device_limit).max(required);
    required
        .max(DAMAGE_TEXTURE_GRANULARITY)
        .checked_next_power_of_two()
        .unwrap_or(u32::MAX)
        .min(cap)
        .max(required)
}

pub(crate) fn damage_texture_geometric_needs_replacement(
    allocated_width: u32,
    allocated_height: u32,
    required_width: u32,
    required_height: u32,
    desired_width: u32,
    desired_height: u32,
) -> bool {
    if allocated_width < required_width || allocated_height < required_height {
        return true;
    }
    // Do not recreate a target merely because the raw area crossed half: its
    // quantized/geometric capacity may be identical. Shrink only after
    // fourfold capacity slack, providing hysteresis around growth edges.
    (desired_width < allocated_width && desired_width.saturating_mul(4) <= allocated_width)
        || (desired_height < allocated_height
            && desired_height.saturating_mul(4) <= allocated_height)
}

pub(crate) fn damage_texture_needs_replacement(
    allocated_width: u32,
    allocated_height: u32,
    required_width: u32,
    required_height: u32,
) -> bool {
    if allocated_width < required_width || allocated_height < required_height {
        return true;
    }

    // Release a one-off large target after a substantial topology-preserving
    // shrink, but keep ordinary animation jitter allocation-free.
    let allocated_pixels = u64::from(allocated_width).saturating_mul(u64::from(allocated_height));
    let required_pixels = u64::from(required_width).saturating_mul(u64::from(required_height));
    allocated_pixels > required_pixels.saturating_mul(2)
}

struct DamageTexture {
    width: u32,
    height: u32,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffscreenMemoryStats {
    pub output_target_bytes: u64,
    pub damage_target_bytes: u64,
    pub readback_bytes: u64,
    pub vello: vello::RendererMemoryStats,
}

/// Host-side phases of one completed offscreen render, populated only by
/// [`OffscreenRenderer::render_damage_regions_gpu_wait_profiled`].
///
/// `vello_submit_us` includes scene resolution, uploads, and Vello's shared
/// queue submission, excluding separately measured copy encoding.
/// `copy_submit_us` covers only dirty-copy command encoding in Vello's
/// post-render callback. `settle_wait_us` is the blocking GPU-complete wait
/// while collecting Vello's bump counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffscreenRenderTiming {
    pub vello_submit_us: u128,
    pub copy_submit_us: u128,
    pub settle_wait_us: u128,
    pub full_attempts: u32,
    pub damage_setup_us: u128,
    pub damage_target_creations: u32,
    pub sparse_submissions: u32,
    /// Sparse submissions that actually selected the opt-in native active-bin coarse shader.
    /// Capability alone is not counted because oversized selectors fall back to legacy coarse.
    pub sparse_coarse_active_bins_submissions: u32,
    /// Sparse submissions made while Vello's opt-in adaptive coarse policy was active.
    pub sparse_coarse_adaptive_submissions: u32,
    /// Adaptive sparse submissions which deliberately retained dense coarse work.
    pub sparse_coarse_adaptive_dense_submissions: u32,
    /// Sum of representable native coarse bins across adaptive sparse submissions.
    pub sparse_coarse_native_bins: u64,
    /// Sum of selected native coarse bins across adaptive sparse submissions.
    pub sparse_coarse_selected_native_bins: u64,
    /// Sparse submissions where Vello actually applied path-level native tile bounds.
    pub sparse_path_tile_bounds_submissions: u32,
    /// Sum of native bounding-box tile areas from actually bounded sparse submissions.
    /// This is diagnostic coverage accounting, not an allocation or memory-saving metric.
    pub sparse_path_tile_bounds_tiles: u64,
    /// Sparse submissions where the coarse opaque-prune shader was actually selected.
    /// This does not claim a number of tiles or commands pruned, or any resource savings.
    pub sparse_coarse_opaque_prune_submissions: u32,
    /// Sparse submissions where the solid-fuse producer/consumer shader path was selected.
    /// This does not claim command fusion counts or any resource savings.
    pub sparse_solid_fuse_submissions: u32,
    /// Sparse submissions where the segment-row-mask shader path was actually selected.
    /// This does not claim a number of rows skipped or any resource savings.
    pub sparse_segment_row_mask_submissions: u32,
    /// Sparse submissions where canonical segment ordering was actually recorded.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub canonical_segment_order_sparse_selected_attempts: u32,
    /// Full submissions where canonical segment ordering was actually recorded.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub canonical_segment_order_full_selected_attempts: u32,
    /// Sparse submissions where the Area PPT8 shader was actually selected.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub area_ppt8_sparse_selected_attempts: u32,
    /// Full submissions where the Area PPT8 shader was actually selected.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub area_ppt8_full_selected_attempts: u32,
    /// Sparse submissions where the single-workgroup clip leaf shader was selected.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub clip_single_group_sparse_selected_attempts: u32,
    /// Full submissions where the single-workgroup clip leaf shader was selected.
    /// This is route-attempt telemetry, not a completion or speed claim.
    pub clip_single_group_full_selected_attempts: u32,
    /// Sparse submissions where the Area fine shader actually skipped zero-mask quad invocations.
    pub sparse_fine_quad_skip_submissions: u32,
    /// Total fine invocation groups considered by the applied quad-skip path.
    pub sparse_fine_quad_total_quads: u64,
    /// Non-zero-mask fine invocation groups evaluated by the applied quad-skip path.
    pub sparse_fine_quad_active_quads: u64,
    /// Zero-mask fine invocation groups skipped by the applied quad-skip path.
    pub sparse_fine_quad_skipped_quads: u64,
    pub sparse_fine_tiles: u64,
    pub sparse_written_pixels: u64,
    pub coarse_prefix_pixels: u64,
    /// Rounded GPU timestamp span across verified submissions.
    pub gpu_timestamp_span_ns: u128,
    /// Rounded sum of individual GPU pass durations across verified submissions.
    pub gpu_timestamp_pass_ns: u128,
    /// Rounded duration of the final recorded compute pass in verified submissions.
    pub gpu_timestamp_last_pass_ns: u128,
    /// Rounded duration of all earlier recorded compute passes in verified submissions.
    pub gpu_timestamp_other_pass_ns: u128,
    pub gpu_timestamp_pass_count: u32,
    pub gpu_timestamp_expected_submissions: u32,
    pub gpu_timestamp_sampled_submissions: u32,
    /// Submissions for which timestamp data was disabled, unavailable, stale, or failed.
    pub gpu_timestamp_missing_submissions: u32,
    /// Matched submissions expected to have an opt-in per-dispatch sample.
    pub gpu_dispatch_timestamp_expected_submissions: u32,
    pub gpu_dispatch_timestamp_sampled_submissions: u32,
    /// Missing dispatch samples are not treated as zero GPU time.
    pub gpu_dispatch_timestamp_missing_submissions: u32,
    /// A completed sample belonging to an older submission was discarded.
    pub gpu_dispatch_timestamp_stale_submissions: u32,
    /// Bit `stage_id - 1` is set when that stable stage was present.
    pub gpu_dispatch_timestamp_present_stages: u32,
    pub gpu_dispatch_timestamp_stage_duration_ns: [u128; 32],
    pub gpu_dispatch_timestamp_stage_dispatch_count: [u32; 32],
    pub gpu_dispatch_timestamp_unknown_duration_ns: u128,
    pub gpu_dispatch_timestamp_unknown_dispatch_count: u32,
}

/// Lightweight accounting for actual sparse submissions. Unlike [`OffscreenRenderTiming`], this
/// is updated for every sparse attempt, including one that later overflows into dense recovery.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SparseSubmissionAccounting {
    pub sparse_submissions: u64,
    pub active_sparse_submissions: u64,
    pub path_tile_bounds_submissions: u64,
    pub path_tile_bounds_tiles: u64,
    pub coarse_opaque_prune_selected_submissions: u64,
    pub solid_fuse_selected_submissions: u64,
    pub segment_row_mask_selected_submissions: u64,
    pub canonical_segment_order_selected_sparse_attempts: u64,
    pub area_ppt8_selected_sparse_attempts: u64,
    pub clip_single_group_shader_selected: u64,
    pub fine_quad_skip_submissions: u64,
    pub fine_quad_total_quads: u64,
    pub fine_quad_active_quads: u64,
    pub fine_quad_skipped_quads: u64,
}

impl SparseSubmissionAccounting {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record(&mut self, stats: vello::SparseRenderStats) {
        self.sparse_submissions = self.sparse_submissions.saturating_add(1);
        if stats.coarse_active_bins {
            self.active_sparse_submissions = self.active_sparse_submissions.saturating_add(1);
        }
        if stats.path_tile_bounds_applied {
            self.path_tile_bounds_submissions = self.path_tile_bounds_submissions.saturating_add(1);
            self.path_tile_bounds_tiles = self
                .path_tile_bounds_tiles
                .saturating_add(u64::from(stats.path_tile_bounds_tiles));
        }
        if stats.coarse_opaque_prune_selected {
            self.coarse_opaque_prune_selected_submissions = self
                .coarse_opaque_prune_selected_submissions
                .saturating_add(1);
        }
        if stats.solid_fuse_selected {
            self.solid_fuse_selected_submissions =
                self.solid_fuse_selected_submissions.saturating_add(1);
        }
        if stats.segment_row_mask_selected {
            self.segment_row_mask_selected_submissions =
                self.segment_row_mask_selected_submissions.saturating_add(1);
        }
        if stats.canonical_segment_order_shader_selected {
            self.canonical_segment_order_selected_sparse_attempts = self
                .canonical_segment_order_selected_sparse_attempts
                .saturating_add(1);
        }
        if stats.area_ppt8_shader_selected {
            self.area_ppt8_selected_sparse_attempts =
                self.area_ppt8_selected_sparse_attempts.saturating_add(1);
        }
        if stats.clip_single_group_shader_selected {
            self.clip_single_group_shader_selected =
                self.clip_single_group_shader_selected.saturating_add(1);
        }
        if stats.fine_quad_skip_applied {
            self.fine_quad_skip_submissions = self.fine_quad_skip_submissions.saturating_add(1);
            self.fine_quad_total_quads = self
                .fine_quad_total_quads
                .saturating_add(u64::from(stats.fine_quad_total_quads));
            self.fine_quad_active_quads = self
                .fine_quad_active_quads
                .saturating_add(u64::from(stats.fine_quad_active_quads));
            self.fine_quad_skipped_quads = self.fine_quad_skipped_quads.saturating_add(u64::from(
                stats
                    .fine_quad_total_quads
                    .saturating_sub(stats.fine_quad_active_quads),
            ));
        }
    }
}

/// Actual canonical-segment-order passes recorded since an explicit measured-workload reset.
/// Counters describe synchronous route attempts only, including retries, not completion or speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CanonicalSegmentOrderAttemptAccounting {
    pub full_selected_attempts: u64,
    pub sparse_selected_attempts: u64,
}

/// Actual Area PPT8 passes recorded since an explicit measured-workload reset.
/// Counters describe synchronous route attempts only, including retries, not completion or speed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AreaPpt8AttemptAccounting {
    pub full_selected_attempts: u64,
    pub sparse_selected_attempts: u64,
}

/// Actual single-workgroup clip leaf passes recorded since an explicit measured-workload reset.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClipSingleGroupAttemptAccounting {
    pub full_selected_attempts: u64,
    pub sparse_selected_attempts: u64,
}

impl ClipSingleGroupAttemptAccounting {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record_full(&mut self, selected: bool) {
        if selected {
            self.full_selected_attempts = self.full_selected_attempts.saturating_add(1);
        }
    }

    fn record_sparse(&mut self, selected: bool) {
        if selected {
            self.sparse_selected_attempts = self.sparse_selected_attempts.saturating_add(1);
        }
    }
}

impl AreaPpt8AttemptAccounting {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record_full(&mut self, selected: bool) {
        if selected {
            self.full_selected_attempts = self.full_selected_attempts.saturating_add(1);
        }
    }

    fn record_sparse(&mut self, selected: bool) {
        if selected {
            self.sparse_selected_attempts = self.sparse_selected_attempts.saturating_add(1);
        }
    }
}

impl CanonicalSegmentOrderAttemptAccounting {
    fn reset(&mut self) {
        *self = Self::default();
    }

    fn record_full(&mut self, selected: bool) {
        if selected {
            self.full_selected_attempts = self.full_selected_attempts.saturating_add(1);
        }
    }

    fn record_sparse(&mut self, selected: bool) {
        if selected {
            self.sparse_selected_attempts = self.sparse_selected_attempts.saturating_add(1);
        }
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    reason = "validated non-negative timestamp durations are rounded down for compact benchmark aggregates"
)]
fn timestamp_last_and_other_pass_ns(sample: &vello::GpuTimestampSample) -> Option<(u128, u128)> {
    if sample.pass_count == 0
        || !sample.timestamp_period_ns.is_finite()
        || sample.timestamp_period_ns <= 0.0
        || !sample.summed_pass_ns.is_finite()
        || sample.summed_pass_ns < 0.0
    {
        return None;
    }
    let byte_len = usize::try_from(sample.pass_count).ok()?.checked_mul(16)?;
    if sample.timestamp_bytes.len() != byte_len {
        return None;
    }
    let mut final_pair = None;
    for pair in sample.timestamp_bytes.chunks_exact(16) {
        let beginning = u64::from_le_bytes(pair.get(..8)?.try_into().ok()?);
        let end = u64::from_le_bytes(pair.get(8..)?.try_into().ok()?);
        // Do not accept a valid final pair from an otherwise malformed raw
        // sample: the split is meaningful only for a complete submission.
        end.checked_sub(beginning)?;
        final_pair = Some(pair);
    }
    let final_pair = final_pair?;
    let beginning = u64::from_le_bytes(final_pair.get(..8)?.try_into().ok()?);
    let end = u64::from_le_bytes(final_pair.get(8..)?.try_into().ok()?);
    let last_ns = end.checked_sub(beginning)? as f64 * f64::from(sample.timestamp_period_ns);
    if !last_ns.is_finite() || last_ns < 0.0 {
        return None;
    }
    let total_ns = sample.summed_pass_ns as u128;
    let last_ns = last_ns as u128;
    Some((last_ns, total_ns.checked_sub(last_ns)?))
}

impl OffscreenMemoryStats {
    pub fn known_gpu_bytes(self) -> u64 {
        self.output_target_bytes
            .saturating_add(self.damage_target_bytes)
            .saturating_add(self.vello.pooled_buffer_bytes)
            .saturating_add(self.vello.live_buffer_bytes)
            .saturating_add(self.vello.image_bytes)
            .saturating_add(self.vello.sparse_upload_bytes)
    }
}

fn shared_render_context() -> Arc<Mutex<RenderContext>> {
    SHARED_RENDER_CONTEXT
        .get_or_init(|| Arc::new(Mutex::new(RenderContext::new())))
        .clone()
}

/// Handles to the process-wide shared wgpu device — the very same one
/// every [`OffscreenRenderer`] draws through.
///
/// Apps that run their own GPU work headlessly (the video editor
/// composites its timeline frames with a wgpu compute pipeline) must
/// borrow these rather than stand up a second `wgpu::Instance`: parallel
/// `vkCreateInstance` calls race and SIGSEGV on several Mesa drivers,
/// which is the whole reason the shared context exists.
///
/// Both handles are `Arc`-backed inside wgpu, so cloning them out of the
/// context is cheap and they stay valid once the lock is released.
pub struct SharedGpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

/// Borrow the shared device/queue, initialising the process-wide context
/// on first call. Fails with the same [`OffscreenError`]s as
/// [`OffscreenRenderer::try_new`] — a machine with no usable adapter
/// gets a `NoAdapter`, never a panic.
///
/// # Errors
///
/// Returns [`OffscreenError::NoAdapter`] when no usable GPU adapter exists or the
/// device cannot be waited for (a browser page), or
/// [`OffscreenError::ContextPoisoned`] when another thread panicked while
/// holding the shared render-context lock.
pub fn shared_gpu() -> Result<SharedGpu, OffscreenError> {
    // Acquiring the device waits on it, which a browser page cannot do: the
    // caller gets the same answer as a machine without an adapter and falls
    // back, instead of the page panicking.
    if !tilo_rt::available(tilo_rt::Feature::BlockOn) {
        return Err(OffscreenError::NoAdapter);
    }
    let context = shared_render_context();
    let (device, queue) = {
        let mut ctx = context
            .lock()
            .map_err(|_| OffscreenError::ContextPoisoned)?;
        let dev_id = ctx
            .device(None)
            .block_on()
            .ok_or(OffscreenError::NoAdapter)?;
        let handle = ctx.devices.get(dev_id).ok_or(OffscreenError::NoAdapter)?;
        let device = handle.device.clone();
        let queue = handle.queue.clone();
        drop(ctx);
        (device, queue)
    };
    Ok(SharedGpu { device, queue })
}

/// Why an [`OffscreenRenderer`] could not be built.
///
/// Every variant is an *environment* failure. Bad arguments (a zero width
/// or height) stay an `assert!` in the constructors — that's a caller bug.
#[derive(Debug)]
pub enum OffscreenError {
    /// On a headless machine this usually means no software rasteriser is
    /// installed (llvmpipe / lavapipe).
    NoAdapter,
    Renderer(vello::Error),
    /// Only reachable under `panic = "unwind"`; the shipping release
    /// profile aborts instead of poisoning.
    ContextPoisoned,
}

impl std::fmt::Display for OffscreenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoAdapter => f.write_str("no wgpu adapter available — is llvmpipe installed?"),
            Self::Renderer(e) => write!(f, "vello could not build a renderer: {e}"),
            Self::ContextPoisoned => f.write_str("the shared wgpu RenderContext is poisoned"),
        }
    }
}

impl std::error::Error for OffscreenError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Renderer(e) => Some(e),
            Self::NoAdapter | Self::ContextPoisoned => None,
        }
    }
}

/// An offscreen wgpu + vello pipeline sized at `width × height`. Reuse
/// a single instance for multiple renders to amortise adapter / device
/// creation (which is the expensive part).
pub struct OffscreenRenderer {
    pub width: u32,
    pub height: u32,
    /// Shared with every other `OffscreenRenderer` in the process — see
    /// [`SHARED_RENDER_CONTEXT`].
    context: Arc<Mutex<RenderContext>>,
    dev_id: usize,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    /// Allocated only for screenshot/pixel callers. GPU timing benchmarks
    /// never read the target back and therefore should not carry a second
    /// full-resolution host-visible allocation.
    readback_buf: Option<wgpu::Buffer>,
    padded_bytes_per_row: u32,
    renderer: Renderer,
    damage_textures: Vec<DamageTexture>,
    damage_geometric_capacity: bool,
    /// Zero means the retained target was last updated by a full render;
    /// non-zero values are the number of active damage passes. Exact animated
    /// dimensions are deliberately excluded so they cannot flush GPU caches.
    last_damage_region_count: Option<usize>,
    coarse_pool_high_water_pixels: u64,
    sparse_submission_accounting: SparseSubmissionAccounting,
    canonical_segment_order_attempt_accounting: CanonicalSegmentOrderAttemptAccounting,
    area_ppt8_attempt_accounting: AreaPpt8AttemptAccounting,
    clip_single_group_attempt_accounting: ClipSingleGroupAttemptAccounting,
    target_valid: bool,
    /// Backdrops the scenes rendered next draw floating surfaces over; see
    /// [`Self::set_backdrop`].
    backdrop: Option<Arc<crate::backdrop::BackdropFrame>>,
    /// Built with the first frame that has a backdrop.
    backdrop_blur: Option<crate::backdrop::BackdropBlur>,
    /// Scratch space for scroll copies within the target.
    copy_scratch: crate::scroll_copy::CopyScratch,
}

impl OffscreenRenderer {
    /// This renderer's device and queue, cloned out of the shared context so
    /// its lock (host-side bookkeeping only) is not held while rendering.
    #[allow(
        clippy::expect_used,
        reason = "a poisoned shared context or a missing device entry is an environment failure the headless harness has no fallback for"
    )]
    fn device_queue(&self) -> (wgpu::Device, wgpu::Queue) {
        let ctx = self.context.lock().expect("shared RenderContext poisoned");
        let handle = ctx
            .devices
            .get(self.dev_id)
            .expect("this renderer's device entry is missing from the shared RenderContext");
        (handle.device.clone(), handle.queue.clone())
    }

    /// Render all of `scene` over `bg` into the target texture.
    #[allow(
        clippy::expect_used,
        reason = "the harness cannot produce a meaningful frame after vello fails to render"
    )]
    fn render_full(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        scene: &Scene,
        bg: Color,
    ) {
        self.renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &self.view,
                &RenderParams {
                    base_color: bg,
                    width: self.width,
                    height: self.height,
                    antialiasing_method: AaConfig::Area,
                },
            )
            .expect("render_to_texture");
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

    fn timestamp_submission_id(&self, profiled: bool) -> Option<u64> {
        if !profiled {
            return None;
        }
        self.renderer.gpu_timestamp_latest_submission_id()
    }

    fn dispatch_timestamp_submission_id(&self, profiled: bool) -> Option<u64> {
        profiled.then(|| self.renderer.gpu_dispatch_timestamp_latest_submission_id())?
    }

    fn record_dispatch_timestamp(
        &mut self,
        expected_submission: Option<u64>,
        timing: &mut Option<OffscreenRenderTiming>,
    ) {
        let Some(timing) = timing.as_mut() else {
            return;
        };
        if matches!(
            self.renderer.gpu_dispatch_timestamp_status(),
            vello::GpuDispatchTimestampStatus::Disabled
                | vello::GpuDispatchTimestampStatus::Unsupported
        ) {
            return;
        }
        timing.gpu_dispatch_timestamp_expected_submissions = timing
            .gpu_dispatch_timestamp_expected_submissions
            .saturating_add(1);
        let Some(sample) = self.renderer.take_gpu_dispatch_timestamp_sample() else {
            timing.gpu_dispatch_timestamp_missing_submissions = timing
                .gpu_dispatch_timestamp_missing_submissions
                .saturating_add(1);
            return;
        };
        if Some(sample.submission_id) != expected_submission {
            timing.gpu_dispatch_timestamp_stale_submissions = timing
                .gpu_dispatch_timestamp_stale_submissions
                .saturating_add(1);
            timing.gpu_dispatch_timestamp_missing_submissions = timing
                .gpu_dispatch_timestamp_missing_submissions
                .saturating_add(1);
            return;
        }
        timing.gpu_dispatch_timestamp_sampled_submissions = timing
            .gpu_dispatch_timestamp_sampled_submissions
            .saturating_add(1);
        let stage_count = usize::try_from(sample.stage_count)
            .unwrap_or(sample.stages.len())
            .min(sample.stages.len());
        for stage in sample.stages.into_iter().take(stage_count) {
            if stage.dispatch_count == 0 {
                continue;
            }
            if stage.stage_id == 0 || stage.stage_id > 32 {
                timing.gpu_dispatch_timestamp_unknown_duration_ns = timing
                    .gpu_dispatch_timestamp_unknown_duration_ns
                    .saturating_add(stage.duration_ns);
                timing.gpu_dispatch_timestamp_unknown_dispatch_count = timing
                    .gpu_dispatch_timestamp_unknown_dispatch_count
                    .saturating_add(stage.dispatch_count);
                continue;
            }
            let Some(index) = usize::from(stage.stage_id).checked_sub(1) else {
                timing.gpu_dispatch_timestamp_unknown_duration_ns = timing
                    .gpu_dispatch_timestamp_unknown_duration_ns
                    .saturating_add(stage.duration_ns);
                timing.gpu_dispatch_timestamp_unknown_dispatch_count = timing
                    .gpu_dispatch_timestamp_unknown_dispatch_count
                    .saturating_add(stage.dispatch_count);
                continue;
            };
            let (Some(duration), Some(dispatch_count)) = (
                timing
                    .gpu_dispatch_timestamp_stage_duration_ns
                    .get_mut(index),
                timing
                    .gpu_dispatch_timestamp_stage_dispatch_count
                    .get_mut(index),
            ) else {
                timing.gpu_dispatch_timestamp_unknown_duration_ns = timing
                    .gpu_dispatch_timestamp_unknown_duration_ns
                    .saturating_add(stage.duration_ns);
                timing.gpu_dispatch_timestamp_unknown_dispatch_count = timing
                    .gpu_dispatch_timestamp_unknown_dispatch_count
                    .saturating_add(stage.dispatch_count);
                continue;
            };
            let stage_bit = 1u32
                .checked_shl(u32::try_from(index).unwrap_or(u32::BITS))
                .unwrap_or(0);
            timing.gpu_dispatch_timestamp_present_stages |= stage_bit;
            *duration = duration.saturating_add(stage.duration_ns);
            *dispatch_count = dispatch_count.saturating_add(stage.dispatch_count);
        }
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "timestamp nanoseconds are non-negative finite GPU durations rounded down for compact benchmark aggregates"
    )]
    fn record_timestamp(
        &mut self,
        expected_submission: Option<u64>,
        timing: &mut Option<OffscreenRenderTiming>,
    ) {
        let Some(timing) = timing.as_mut() else {
            return;
        };
        timing.gpu_timestamp_expected_submissions =
            timing.gpu_timestamp_expected_submissions.saturating_add(1);
        let Some(sample) = self.renderer.take_gpu_timestamp_sample() else {
            timing.gpu_timestamp_missing_submissions =
                timing.gpu_timestamp_missing_submissions.saturating_add(1);
            return;
        };
        if Some(sample.submission_id) != expected_submission {
            timing.gpu_timestamp_missing_submissions =
                timing.gpu_timestamp_missing_submissions.saturating_add(1);
            return;
        }
        let Some((last_pass_ns, other_pass_ns)) = timestamp_last_and_other_pass_ns(&sample) else {
            // A Vello sample should already have validated this layout. If it
            // ever does not, keep the complete-frame accounting truthful
            // rather than silently reporting a partial duration breakdown.
            timing.gpu_timestamp_missing_submissions =
                timing.gpu_timestamp_missing_submissions.saturating_add(1);
            return;
        };
        timing.gpu_timestamp_span_ns = timing
            .gpu_timestamp_span_ns
            .saturating_add(sample.span_ns.max(0.0) as u128);
        timing.gpu_timestamp_pass_ns = timing
            .gpu_timestamp_pass_ns
            .saturating_add(sample.summed_pass_ns.max(0.0) as u128);
        timing.gpu_timestamp_last_pass_ns = timing
            .gpu_timestamp_last_pass_ns
            .saturating_add(last_pass_ns);
        timing.gpu_timestamp_other_pass_ns = timing
            .gpu_timestamp_other_pass_ns
            .saturating_add(other_pass_ns);
        timing.gpu_timestamp_pass_count = timing
            .gpu_timestamp_pass_count
            .saturating_add(sample.pass_count);
        timing.gpu_timestamp_sampled_submissions =
            timing.gpu_timestamp_sampled_submissions.saturating_add(1);
    }

    /// Overrides Vello's compute-pass batching for a differential reference.
    #[doc(hidden)]
    pub fn set_compute_pass_batching(&mut self, enabled: bool) {
        self.renderer.set_compute_pass_batching(enabled);
    }

    /// Whether geometric retained-damage texture capacity is enabled.
    #[doc(hidden)]
    pub fn damage_geometric_capacity_enabled(&self) -> bool {
        self.damage_geometric_capacity
    }

    /// Overrides direct GPU path count/tiling dispatch for a differential oracle.
    #[doc(hidden)]
    pub fn set_direct_path_dispatch_enabled(&mut self, enabled: bool) {
        self.renderer.set_direct_path_dispatch_enabled(enabled);
    }

    /// Overrides deferred bump-readback ordering for an independent oracle.
    #[doc(hidden)]
    pub fn set_defer_bump_readback_enabled(&mut self, enabled: bool) {
        self.renderer.set_defer_bump_readback_enabled(enabled);
    }

    /// Reset no-allocation sparse-submission accounting at a measured-workload boundary.
    #[doc(hidden)]
    pub fn reset_sparse_submission_accounting(&mut self) {
        self.sparse_submission_accounting.reset();
    }

    /// Actual sparse submissions recorded since the latest explicit reset.
    #[doc(hidden)]
    pub fn sparse_submission_accounting(&self) -> SparseSubmissionAccounting {
        self.sparse_submission_accounting
    }

    /// Reset canonical segment-order route-attempt counters at a measured-workload boundary.
    #[doc(hidden)]
    pub fn reset_canonical_segment_order_attempt_accounting(&mut self) {
        self.canonical_segment_order_attempt_accounting.reset();
    }

    /// Actual canonical segment-order passes recorded since the latest explicit reset.
    #[doc(hidden)]
    pub fn canonical_segment_order_attempt_accounting(
        &self,
    ) -> CanonicalSegmentOrderAttemptAccounting {
        self.canonical_segment_order_attempt_accounting
    }

    /// Reset Area PPT8 route-attempt counters at a measured-workload boundary.
    #[doc(hidden)]
    pub fn reset_area_ppt8_attempt_accounting(&mut self) {
        self.area_ppt8_attempt_accounting.reset();
    }

    /// Actual Area PPT8 passes recorded since the latest explicit reset.
    #[doc(hidden)]
    pub fn area_ppt8_attempt_accounting(&self) -> AreaPpt8AttemptAccounting {
        self.area_ppt8_attempt_accounting
    }

    /// Reset single-workgroup clip leaf route-attempt counters at a workload boundary.
    #[doc(hidden)]
    pub fn reset_clip_single_group_attempt_accounting(&mut self) {
        self.clip_single_group_attempt_accounting.reset();
    }

    /// Actual single-workgroup clip leaf passes recorded since the latest explicit reset.
    #[doc(hidden)]
    pub fn clip_single_group_attempt_accounting(&self) -> ClipSingleGroupAttemptAccounting {
        self.clip_single_group_attempt_accounting
    }

    /// Overrides sparse fine recording for differential tests and diagnostics.
    #[doc(hidden)]
    pub fn set_sparse_fine_enabled(&mut self, enabled: bool) {
        self.renderer.set_sparse_fine_enabled(enabled);
    }

    /// Overrides the opt-in CPU path-tag reduce/scan path for differential tests.
    #[doc(hidden)]
    pub fn set_cpu_pathtag_enabled(&mut self, enabled: bool) {
        self.renderer.set_cpu_pathtag_enabled(enabled);
    }

    /// Overrides selective CPU metadata for differential tests.
    #[doc(hidden)]
    pub fn set_cpu_metadata_enabled(&mut self, enabled: bool) {
        self.renderer.set_cpu_metadata_enabled(enabled);
    }

    /// Overrides bounded selective CPU draw-prefix processing for differential tests.
    #[doc(hidden)]
    pub fn set_cpu_draw_prefix_enabled(&mut self, enabled: bool) {
        self.renderer.set_cpu_draw_prefix_enabled(enabled);
    }

    /// Whether a draw-prefix table fits the bounded selective CPU path.
    #[doc(hidden)]
    pub fn cpu_draw_prefix_supported(&self, bytes: u64) -> bool {
        self.renderer.cpu_draw_prefix_supported(bytes)
    }

    pub fn memory_stats(&self) -> OffscreenMemoryStats {
        let rgba_bytes = |width: u32, height: u32| {
            u64::from(width)
                .saturating_mul(u64::from(height))
                .saturating_mul(4)
        };
        OffscreenMemoryStats {
            output_target_bytes: rgba_bytes(self.width, self.height),
            damage_target_bytes: self
                .damage_textures
                .iter()
                .map(|target| rgba_bytes(target.width, target.height))
                .sum(),
            readback_bytes: self.readback_buf.as_ref().map_or(0, wgpu::Buffer::size),
            vello: self.renderer.memory_stats(),
        }
    }

    /// Panics if no suitable adapter exists. **Do not try to soften that
    /// with `std::panic::catch_unwind`** — the shipping release profile
    /// sets `panic = "abort"`, under which a panic kills the process at the
    /// panic site and `catch_unwind` never sees it. Callers that need a
    /// soft-fail path must use [`try_new`](Self::try_new).
    ///
    /// # Panics
    ///
    /// Panics if [`try_new`](Self::try_new) returns an [`OffscreenError`] —
    /// see its variants for exactly when that happens.
    #[allow(
        clippy::panic,
        reason = "new() is the deliberate panicking convenience constructor paired with the fallible try_new(); removing the panic would defeat the reason both exist"
    )]
    pub fn new(width: u32, height: u32) -> Self {
        match Self::try_new(width, height) {
            Ok(renderer) => renderer,
            Err(e) => panic!("OffscreenRenderer::new: {e}"),
        }
    }

    /// Identity of the adapter actually selected by this renderer's shared context.
    ///
    /// Returns `None` if that context is inaccessible or no longer contains this renderer's
    /// selected device. This only snapshots adapter metadata; it neither changes selection nor
    /// performs GPU work.
    #[doc(hidden)]
    pub fn adapter_info(&self) -> Option<wgpu::AdapterInfo> {
        let context = self.context.lock().ok()?;
        context
            .devices
            .get(self.dev_id)
            .map(|handle| handle.adapter.get_info())
    }

    /// For any code path that has to survive a GPU-less machine, since
    /// `catch_unwind` can't (see [`new`](Self::new)). Still asserts on a
    /// zero width or height — that's a caller bug, not an environment one.
    ///
    /// # Errors
    ///
    /// Returns [`OffscreenError::NoAdapter`] when no wgpu adapter is
    /// available (a headless machine typically needs llvmpipe/lavapipe
    /// installed), [`OffscreenError::Renderer`] when vello fails to build a
    /// renderer for the adapter, and [`OffscreenError::ContextPoisoned`]
    /// when the shared `RenderContext` mutex was poisoned by a panic on
    /// another thread.
    ///
    /// # Panics
    ///
    /// Panics if `width` or `height` is zero — that's a caller bug, not a
    /// recoverable environment failure.
    pub fn try_new(width: u32, height: u32) -> Result<Self, OffscreenError> {
        Self::try_new_with_preprocessing(
            width,
            height,
            crate::env::var_os("TILO_UI_VELLO_CPU").is_some(),
        )
    }

    /// Use stable CPU segment ordering with Vello's production GPU fine
    /// rasterizer. Exact pixel comparisons can select this per instance
    /// without changing process-wide renderer settings.
    ///
    /// # Errors
    /// Returns the same adapter and renderer errors as [`Self::try_new`].
    ///
    /// # Panics
    /// Panics on zero dimensions, as [`Self::try_new`] does.
    pub fn try_new_with_cpu_preprocessing(width: u32, height: u32) -> Result<Self, OffscreenError> {
        Self::try_new_with_preprocessing(width, height, true)
    }

    /// Exact pixel oracles can use stable CPU segment ordering while keeping
    /// Vello's fine rasterization on the GPU. Avoid process-global env mutation.
    #[cfg(test)]
    pub(crate) fn try_new_for_pixel_test(
        width: u32,
        height: u32,
        use_cpu: bool,
    ) -> Result<Self, OffscreenError> {
        Self::try_new_with_preprocessing(width, height, use_cpu)
    }

    #[allow(
        clippy::panic_in_result_fn,
        reason = "zero dimensions are a caller bug"
    )]
    fn try_new_with_preprocessing(
        width: u32,
        height: u32,
        use_cpu: bool,
    ) -> Result<Self, OffscreenError> {
        assert!(width > 0 && height > 0, "width and height must be > 0");

        let context = shared_render_context();
        let mut ctx = context
            .lock()
            .map_err(|_| OffscreenError::ContextPoisoned)?;
        let dev_id = ctx
            .device(None)
            .block_on()
            .ok_or(OffscreenError::NoAdapter)?;
        let handle = ctx.devices.get(dev_id).ok_or(OffscreenError::NoAdapter)?;
        let device = &handle.device;

        let format = wgpu::TextureFormat::Rgba8Unorm;
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("quick-ui-offscreen-render-target"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());

        // See the row-pitch alignment note in the module docs.
        let bytes_per_pixel = 4u32;
        let unpadded = width.saturating_mul(bytes_per_pixel);
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = unpadded.div_ceil(align).saturating_mul(align);
        let renderer = Renderer::new(
            device,
            RendererOptions {
                use_cpu,
                // Only `Area` is ever asked for; `all()` would also build
                // the msaa8/msaa16 pipelines nothing selects.
                antialiasing_support: AaSupport::area_only(),
                num_init_threads: None,
                pipeline_cache: None,
            },
        )
        .map_err(OffscreenError::Renderer)?;

        drop(ctx);

        Ok(Self {
            width,
            height,
            context,
            dev_id,
            texture,
            view,
            readback_buf: None,
            padded_bytes_per_row,
            renderer,
            damage_textures: Vec::with_capacity(4),
            damage_geometric_capacity: damage_geometric_capacity_enabled(),
            last_damage_region_count: None,
            coarse_pool_high_water_pixels: 0,
            sparse_submission_accounting: SparseSubmissionAccounting::default(),
            canonical_segment_order_attempt_accounting:
                CanonicalSegmentOrderAttemptAccounting::default(),
            area_ppt8_attempt_accounting: AreaPpt8AttemptAccounting::default(),
            clip_single_group_attempt_accounting: ClipSingleGroupAttemptAccounting::default(),
            target_valid: false,
            backdrop: None,
            backdrop_blur: None,
            copy_scratch: crate::scroll_copy::CopyScratch::default(),
        })
    }

    /// Draw the floating surfaces of the scenes rendered from now on over
    /// `backdrop`, the frame their host recorded with them (for the shell,
    /// `FrameBuilder::take_backdrop`). `None` draws them opaque.
    pub fn set_backdrop(&mut self, backdrop: Option<Arc<crate::backdrop::BackdropFrame>>) {
        self.backdrop = backdrop;
    }

    #[allow(
        clippy::expect_used,
        reason = "pixel-test readback cannot recover from a failed GPU mapping"
    )]
    fn read_target(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<u8> {
        let readback_buf = self.readback_buf.get_or_insert_with(|| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("quick-ui-offscreen-readback"),
                size: u64::from(self.padded_bytes_per_row.saturating_mul(self.height)),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        });
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &self.texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback_buf,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_bytes_per_row),
                    rows_per_image: Some(self.height),
                },
            },
            wgpu::Extent3d {
                width: self.width,
                height: self.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(std::iter::once(encoder.finish()));

        let slice = readback_buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            tx.send(result).ok();
        });
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv()
            .expect("map_async channel")
            .expect("buffer map failed");

        let mapped = slice.get_mapped_range();
        let row_bytes = (self.width as usize).saturating_mul(4);
        let mut tight = Vec::with_capacity((self.height as usize).saturating_mul(row_bytes));
        for row in 0..self.height as usize {
            let start = row.saturating_mul(self.padded_bytes_per_row as usize);
            let end = start.saturating_add(row_bytes);
            let row_slice = mapped
                .get(start..end)
                .expect("readback row out of range of the mapped buffer");
            tight.extend_from_slice(row_slice);
        }
        drop(mapped);
        readback_buf.unmap();
        tight
    }

    /// Returns raw RGBA pixels, tight-packed at `4 * width` bytes per row
    /// (the texture format is `Rgba8Unorm`, so **R, G, B, A** order, not
    /// BGRA).
    ///
    /// # Panics
    ///
    /// Panics if the shared `RenderContext` mutex is poisoned, if this
    /// renderer's device entry has gone missing from it, if vello fails to
    /// render the scene, or if the GPU readback fails (the `map_async`
    /// channel drops or the mapping itself errors). Every one of these is
    /// an environment failure this headless test/screenshot harness has no
    /// meaningful fallback for — it fails loudly rather than handing back a
    /// silently-empty buffer a comparison could misread as a real frame.
    fn render_with_overflow(&mut self, scene: &Scene, bg: Color) -> (Vec<u8>, bool) {
        // The lock only guards host-side DeviceHandle bookkeeping, not
        // rendering — vello's kernels run GPU-side. Clone the cheap,
        // Arc-backed `Device`/`Queue` handles out and drop the guard
        // immediately, rather than holding the shared context locked for
        // every other `OffscreenRenderer` through the whole render + readback.
        let (device, queue) = self.device_queue();

        if let Some(backdrop) = self.backdrop.as_deref().filter(|frame| !frame.is_empty()) {
            self.backdrop_blur
                .get_or_insert_with(|| crate::backdrop::BackdropBlur::new(&device))
                .prepare(
                    &device,
                    &queue,
                    &mut self.renderer,
                    backdrop,
                    (self.width, self.height),
                    bg,
                );
        }
        self.render_full(&device, &queue, scene, bg);
        if let Some(blur) = self.backdrop_blur.as_mut() {
            blur.release(&mut self.renderer);
        }
        self.target_valid = true;
        if self
            .last_damage_region_count
            .is_some_and(|count| count != 0)
        {
            self.renderer.trim_pooled_buffers();
        }
        while let Some(target) = self.damage_textures.pop() {
            self.renderer.invalidate_texture_bindings(&target.view);
        }
        self.last_damage_region_count = Some(0);

        let tight = self.read_target(&device, &queue);
        let overflowed = self.renderer.settle_bump_budget(&device);
        (tight, overflowed)
    }

    pub fn render(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        self.render_with_overflow(scene, bg).0
    }

    /// Read the already-rendered retained target without submitting another
    /// Vello frame. Used to verify partial-damage composition pixel-for-pixel.
    ///
    /// # Panics
    ///
    /// Panics if the shared GPU context or target readback fails.
    pub fn readback_current(&mut self) -> Vec<u8> {
        let (device, queue) = self.device_queue();
        self.read_target(&device, &queue)
    }

    /// Submit a render and wait for its GPU work without copying the target
    /// back to CPU memory.
    ///
    /// Performance harnesses use this to measure Vello rather than PCIe/shared
    /// memory readback plus row repacking. Screenshot and visual-regression
    /// callers should continue to use [`Self::render`].
    ///
    /// # Panics
    /// If shared GPU state is poisoned/missing or Vello rejects the render.
    pub fn render_gpu_wait(&mut self, scene: &Scene, bg: Color) {
        let mut timing = None;
        let _ = self.render_gpu_wait_count(scene, bg, &mut timing);
    }

    /// Record canonical segment-order selection immediately after one completed full recording.
    /// This is an attempt counter: later bump settlement can still overflow and retry.
    fn record_canonical_segment_order_full_selection(
        &mut self,
        timing: &mut Option<OffscreenRenderTiming>,
    ) {
        if !self.renderer.last_canonical_segment_order_selected() {
            return;
        }
        self.canonical_segment_order_attempt_accounting
            .record_full(true);
        if let Some(profile) = timing.as_mut() {
            profile.canonical_segment_order_full_selected_attempts = profile
                .canonical_segment_order_full_selected_attempts
                .saturating_add(1);
        }
    }

    /// Record Area PPT8 selection immediately after one completed full recording.
    /// This is an attempt counter: later bump settlement can still overflow and retry.
    fn record_area_ppt8_full_selection(&mut self, timing: &mut Option<OffscreenRenderTiming>) {
        if !self.renderer.last_area_ppt8_selected() {
            return;
        }
        self.area_ppt8_attempt_accounting.record_full(true);
        if let Some(profile) = timing.as_mut() {
            profile.area_ppt8_full_selected_attempts =
                profile.area_ppt8_full_selected_attempts.saturating_add(1);
        }
    }

    /// Record single-workgroup clip leaf selection after a completed full recording.
    fn record_clip_single_group_full_selection(
        &mut self,
        timing: &mut Option<OffscreenRenderTiming>,
    ) {
        if !self.renderer.last_clip_single_group_selected() {
            return;
        }
        self.clip_single_group_attempt_accounting.record_full(true);
        if let Some(profile) = timing.as_mut() {
            profile.clip_single_group_full_selected_attempts = profile
                .clip_single_group_full_selected_attempts
                .saturating_add(1);
        }
    }

    /// The same completed full render as [`Self::render_gpu_wait`], returning
    /// every full-target pass issued while Vello grows a bump allocation.
    fn render_gpu_wait_count(
        &mut self,
        scene: &Scene,
        bg: Color,
        timing: &mut Option<OffscreenRenderTiming>,
    ) -> u64 {
        let (device, queue) = self.device_queue();

        let mut overflowed;
        let mut attempts = 0u64;
        let mut budget = ResilientAttempts::default();
        loop {
            attempts = attempts.saturating_add(1);
            let reserved = self.renderer.memory_stats().bump_budget_bytes;
            let start = timing.is_some().then(crate::time::now);
            self.render_full(&device, &queue, scene, bg);
            if let (Some(start), Some(profile)) = (start, timing.as_mut()) {
                profile.vello_submit_us = profile
                    .vello_submit_us
                    .saturating_add(start.elapsed().as_micros());
                profile.full_attempts = profile.full_attempts.saturating_add(1);
                profile.coarse_prefix_pixels = profile
                    .coarse_prefix_pixels
                    .saturating_add(u64::from(self.width).saturating_mul(u64::from(self.height)));
            }
            self.record_canonical_segment_order_full_selection(timing);
            self.record_area_ppt8_full_selection(timing);
            self.record_clip_single_group_full_selection(timing);
            let timestamp_submission = self.timestamp_submission_id(timing.is_some());
            let dispatch_timestamp_submission =
                self.dispatch_timestamp_submission_id(timing.is_some());
            if timing.is_some() {
                let start = crate::time::now();
                overflowed = self.renderer.settle_bump_budget(&device);
                if let Some(profile) = timing.as_mut() {
                    profile.settle_wait_us = profile
                        .settle_wait_us
                        .saturating_add(start.elapsed().as_micros());
                }
                self.record_timestamp(timestamp_submission, timing);
                self.record_dispatch_timestamp(dispatch_timestamp_submission, timing);
            } else {
                overflowed = self.renderer.settle_bump_budget(&device);
            }
            let grew = self.renderer.memory_stats().bump_budget_bytes > reserved;
            if !overflowed || !budget.another(grew) {
                break;
            }
        }
        assert!(
            !overflowed,
            "Vello still overflowed after {attempts} full render attempts"
        );
        self.target_valid = true;
        if self
            .last_damage_region_count
            .is_some_and(|count| count != 0)
        {
            self.renderer.trim_pooled_buffers();
        }
        while let Some(target) = self.damage_textures.pop() {
            self.renderer.invalidate_texture_bindings(&target.view);
        }
        self.last_damage_region_count = Some(0);
        self.coarse_pool_high_water_pixels =
            u64::from(self.width).saturating_mul(u64::from(self.height));
        u64::from(self.width)
            .saturating_mul(u64::from(self.height))
            .saturating_mul(attempts)
    }

    /// Update one native-resolution rectangle in the retained target and
    /// wait for completion. Pixels outside `damage` are preserved exactly;
    /// no resizing or upscaling is involved.
    ///
    /// Returns the number of pixels actually rasterized. An invalid or
    /// full-size damage rectangle falls back to a complete target render.
    ///
    /// # Panics
    ///
    /// Panics if the shared GPU context is unavailable or Vello rejects the
    /// render. This is a benchmark/test primitive and cannot produce a valid
    /// timing sample after either failure.
    pub fn render_damage_gpu_wait(&mut self, scene: &Scene, bg: Color, damage: Rect) -> u64 {
        self.render_damage_regions_gpu_wait(scene, bg, std::slice::from_ref(&damage))
    }

    /// Batched form of [`Self::render_damage_gpu_wait`]. Render the scene once
    /// at its original coordinates into a scratch target extending from the
    /// native origin through the damage bounds, then copy only dirty regions.
    /// Keeping native coordinates preserves full-render text coverage. If
    /// either the damage or this scratch extent covers at least half the
    /// output, use a full render to bound scratch memory. The final wait and
    /// reported raster work include any overflow-recovery passes.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`Self::render_damage_gpu_wait`].
    pub fn render_damage_regions_gpu_wait(
        &mut self,
        scene: &Scene,
        bg: Color,
        damage: &[Rect],
    ) -> u64 {
        let mut timing = None;
        self.render_damage_regions_gpu_wait_inner(scene, bg, damage, &mut timing)
    }

    /// [`Self::render_damage_regions_gpu_wait`] after moving pixels within
    /// the retained target by `copies`, in order, as a window presents a
    /// scroll ([`crate::scroll_copy`]). With no valid target, or a copy that
    /// would leave it, the frame renders whole.
    ///
    /// # Panics
    ///
    /// Panics under the same conditions as [`Self::render_damage_gpu_wait`].
    pub fn render_copies_and_damage_gpu_wait(
        &mut self,
        scene: &Scene,
        bg: Color,
        copies: &[crate::scroll_copy::DeviceCopy],
        damage: &[Rect],
    ) -> u64 {
        if !copies.is_empty() {
            let (device, queue) = self.device_queue();
            let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("offscreen scroll copies"),
            });
            if !self.target_valid
                || !crate::scroll_copy::encode_copies(
                    &device,
                    &mut encoder,
                    &self.texture,
                    &mut self.copy_scratch,
                    copies,
                )
            {
                let mut timing = None;
                return self.render_gpu_wait_count(scene, bg, &mut timing);
            }
            queue.submit([encoder.finish()]);
        }
        if damage.is_empty() {
            return 0;
        }
        self.render_damage_regions_gpu_wait(scene, bg, damage)
    }

    /// Profile the existing damage render without adding GPU work or readback.
    /// This opt-in path alone reads host clocks around submission and the
    /// existing final settle wait.
    pub fn render_damage_regions_gpu_wait_profiled(
        &mut self,
        scene: &Scene,
        bg: Color,
        damage: &[Rect],
    ) -> (u64, OffscreenRenderTiming) {
        let mut timing = Some(OffscreenRenderTiming::default());
        let pixels = self.render_damage_regions_gpu_wait_inner(scene, bg, damage, &mut timing);
        (pixels, timing.unwrap_or_default())
    }

    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::expect_used,
        clippy::too_many_lines,
        reason = "damage bounds are clamped to non-negative u32 target dimensions before narrowing; the benchmark renderer cannot produce a meaningful sample after a GPU submission failure"
    )]
    fn render_damage_regions_gpu_wait_inner(
        &mut self,
        scene: &Scene,
        bg: Color,
        damage: &[Rect],
        timing: &mut Option<OffscreenRenderTiming>,
    ) -> u64 {
        let full_pixels = u64::from(self.width).saturating_mul(u64::from(self.height));
        let mut damage_pixels = 0u64;
        let mut damage_region_count = 0usize;
        for rect in damage {
            let x0 = rect.x0.floor().max(0.0).min(f64::from(self.width)) as u32;
            let y0 = rect.y0.floor().max(0.0).min(f64::from(self.height)) as u32;
            let x1 = rect.x1.ceil().max(0.0).min(f64::from(self.width)) as u32;
            let y1 = rect.y1.ceil().max(0.0).min(f64::from(self.height)) as u32;
            let width = x1.saturating_sub(x0);
            let height = y1.saturating_sub(y0);
            damage_pixels =
                damage_pixels.saturating_add(u64::from(width).saturating_mul(u64::from(height)));
            damage_region_count =
                damage_region_count.saturating_add(usize::from(width != 0 && height != 0));
        }
        if !self.target_valid
            || damage_pixels == 0
            || damage_pixels >= full_pixels.saturating_div(2)
        {
            return self.render_gpu_wait_count(scene, bg, timing);
        }

        let (device, queue) = self.device_queue();
        let setup_start = timing.is_some().then(crate::time::now);
        let texture_limit = device.limits().max_texture_dimension_2d;
        let regions = coalesce_regions(
            damage
                .iter()
                .filter_map(|rect| {
                    let x0 = rect.x0.floor().max(0.0).min(f64::from(self.width)) as u32;
                    let y0 = rect.y0.floor().max(0.0).min(f64::from(self.height)) as u32;
                    let x1 = rect.x1.ceil().max(0.0).min(f64::from(self.width)) as u32;
                    let y1 = rect.y1.ceil().max(0.0).min(f64::from(self.height)) as u32;
                    let width = x1.saturating_sub(x0);
                    let height = y1.saturating_sub(y0);
                    (width != 0 && height != 0).then_some(Region {
                        x: x0,
                        y: y0,
                        width,
                        height,
                    })
                })
                .collect(),
        );
        let prefix_width = regions
            .iter()
            .map(|r| r.x.saturating_add(r.width))
            .max()
            .unwrap_or(0);
        let prefix_height = regions
            .iter()
            .map(|r| r.y.saturating_add(r.height))
            .max()
            .unwrap_or(0);
        let raster_pixels = u64::from(prefix_width).saturating_mul(u64::from(prefix_height));
        if self.renderer.sparse_fine_supported() {
            if self.coarse_pool_high_water_pixels >= full_pixels.saturating_div(2)
                && raster_pixels.saturating_mul(4) <= self.coarse_pool_high_water_pixels
            {
                self.renderer.trim_pooled_buffers();
                self.coarse_pool_high_water_pixels = 0;
            }
            let fine_damage: Vec<FineDamageRect> = regions
                .iter()
                .map(|region| FineDamageRect {
                    x: region.x,
                    y: region.y,
                    width: region.width,
                    height: region.height,
                })
                .collect();
            let sparse_setup_us = setup_start.map(|start| start.elapsed().as_micros());
            let sparse_start = timing.is_some().then(crate::time::now);
            if let Some(stats) = self
                .renderer
                .render_to_texture_with_damage(
                    &device,
                    &queue,
                    scene,
                    &self.view,
                    &RenderParams {
                        base_color: bg,
                        width: prefix_width,
                        height: prefix_height,
                        antialiasing_method: AaConfig::Area,
                    },
                    &fine_damage,
                )
                .expect("render_to_texture_with_damage")
            {
                self.sparse_submission_accounting.record(stats);
                self.canonical_segment_order_attempt_accounting
                    .record_sparse(stats.canonical_segment_order_shader_selected);
                self.area_ppt8_attempt_accounting
                    .record_sparse(stats.area_ppt8_shader_selected);
                self.clip_single_group_attempt_accounting
                    .record_sparse(stats.clip_single_group_shader_selected);
                if let (Some(profile), Some(setup_us)) = (timing.as_mut(), sparse_setup_us) {
                    profile.damage_setup_us = profile.damage_setup_us.saturating_add(setup_us);
                }
                let timestamp_submission = self.timestamp_submission_id(timing.is_some());
                let dispatch_timestamp_submission =
                    self.dispatch_timestamp_submission_id(timing.is_some());
                if let (Some(profile), Some(start)) = (timing.as_mut(), sparse_start) {
                    profile.vello_submit_us = profile
                        .vello_submit_us
                        .saturating_add(start.elapsed().as_micros());
                    profile.sparse_submissions = profile.sparse_submissions.saturating_add(1);
                    if stats.coarse_active_bins {
                        profile.sparse_coarse_active_bins_submissions = profile
                            .sparse_coarse_active_bins_submissions
                            .saturating_add(1);
                    }
                    if stats.coarse_adaptive {
                        profile.sparse_coarse_adaptive_submissions =
                            profile.sparse_coarse_adaptive_submissions.saturating_add(1);
                        if !stats.coarse_active_bins {
                            profile.sparse_coarse_adaptive_dense_submissions = profile
                                .sparse_coarse_adaptive_dense_submissions
                                .saturating_add(1);
                        }
                        profile.sparse_coarse_native_bins = profile
                            .sparse_coarse_native_bins
                            .saturating_add(u64::from(stats.coarse_native_bins));
                        profile.sparse_coarse_selected_native_bins = profile
                            .sparse_coarse_selected_native_bins
                            .saturating_add(u64::from(stats.coarse_selected_native_bins));
                    }
                    if stats.path_tile_bounds_applied {
                        profile.sparse_path_tile_bounds_submissions = profile
                            .sparse_path_tile_bounds_submissions
                            .saturating_add(1);
                        profile.sparse_path_tile_bounds_tiles = profile
                            .sparse_path_tile_bounds_tiles
                            .saturating_add(u64::from(stats.path_tile_bounds_tiles));
                    }
                    if stats.coarse_opaque_prune_selected {
                        profile.sparse_coarse_opaque_prune_submissions = profile
                            .sparse_coarse_opaque_prune_submissions
                            .saturating_add(1);
                    }
                    if stats.solid_fuse_selected {
                        profile.sparse_solid_fuse_submissions =
                            profile.sparse_solid_fuse_submissions.saturating_add(1);
                    }
                    if stats.segment_row_mask_selected {
                        profile.sparse_segment_row_mask_submissions = profile
                            .sparse_segment_row_mask_submissions
                            .saturating_add(1);
                    }
                    if stats.canonical_segment_order_shader_selected {
                        profile.canonical_segment_order_sparse_selected_attempts = profile
                            .canonical_segment_order_sparse_selected_attempts
                            .saturating_add(1);
                    }
                    if stats.area_ppt8_shader_selected {
                        profile.area_ppt8_sparse_selected_attempts =
                            profile.area_ppt8_sparse_selected_attempts.saturating_add(1);
                    }
                    if stats.clip_single_group_shader_selected {
                        profile.clip_single_group_sparse_selected_attempts = profile
                            .clip_single_group_sparse_selected_attempts
                            .saturating_add(1);
                    }
                    if stats.fine_quad_skip_applied {
                        profile.sparse_fine_quad_skip_submissions =
                            profile.sparse_fine_quad_skip_submissions.saturating_add(1);
                        profile.sparse_fine_quad_total_quads = profile
                            .sparse_fine_quad_total_quads
                            .saturating_add(u64::from(stats.fine_quad_total_quads));
                        profile.sparse_fine_quad_active_quads = profile
                            .sparse_fine_quad_active_quads
                            .saturating_add(u64::from(stats.fine_quad_active_quads));
                        profile.sparse_fine_quad_skipped_quads = profile
                            .sparse_fine_quad_skipped_quads
                            .saturating_add(u64::from(
                                stats
                                    .fine_quad_total_quads
                                    .saturating_sub(stats.fine_quad_active_quads),
                            ));
                    }
                    profile.sparse_fine_tiles = profile
                        .sparse_fine_tiles
                        .saturating_add(u64::from(stats.fine_tiles));
                    profile.sparse_written_pixels = profile
                        .sparse_written_pixels
                        .saturating_add(stats.written_pixels);
                    profile.coarse_prefix_pixels = profile
                        .coarse_prefix_pixels
                        .saturating_add(stats.coarse_pixels);
                }
                let overflowed = if let Some(profile) = timing.as_mut() {
                    let start = crate::time::now();
                    let overflowed = self.renderer.settle_bump_budget(&device);
                    profile.settle_wait_us = profile
                        .settle_wait_us
                        .saturating_add(start.elapsed().as_micros());
                    overflowed
                } else {
                    self.renderer.settle_bump_budget(&device)
                };
                self.record_timestamp(timestamp_submission, timing);
                self.record_dispatch_timestamp(dispatch_timestamp_submission, timing);
                while let Some(target) = self.damage_textures.pop() {
                    self.renderer.invalidate_texture_bindings(&target.view);
                }
                self.last_damage_region_count = Some(damage_region_count);
                self.coarse_pool_high_water_pixels =
                    self.coarse_pool_high_water_pixels.max(stats.coarse_pixels);
                if overflowed {
                    return stats
                        .fine_pixels
                        .saturating_add(self.render_gpu_wait_count(scene, bg, timing));
                }
                return stats.fine_pixels;
            }
        }
        if raster_pixels >= full_pixels.saturating_div(2) {
            return self.render_gpu_wait_count(scene, bg, timing);
        }
        // Region topology changes every animation tick. Vello's pools are
        // kept warm across partial frames, but a full target's scratch pool
        // is released before entering the smaller prefix path.
        if self.last_damage_region_count == Some(0) {
            self.renderer.trim_pooled_buffers();
        }
        self.last_damage_region_count = Some(damage_region_count);
        let allocated_width = if self.damage_geometric_capacity {
            damage_texture_geometric_width_extent(prefix_width, self.width, texture_limit)
        } else {
            damage_texture_extent(prefix_width, texture_limit)
        };
        let allocated_height = damage_texture_height_extent(prefix_height, texture_limit);
        let recreate = self.damage_textures.first().is_none_or(|target| {
            if self.damage_geometric_capacity {
                damage_texture_geometric_needs_replacement(
                    target.width,
                    target.height,
                    prefix_width,
                    prefix_height,
                    allocated_width,
                    allocated_height,
                )
            } else {
                damage_texture_needs_replacement(
                    target.width,
                    target.height,
                    prefix_width,
                    prefix_height,
                )
            }
        });
        if recreate {
            if let Some(target) = self.damage_textures.first() {
                self.renderer.invalidate_texture_bindings(&target.view);
            }
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("quick-ui-native-damage-target"),
                size: wgpu::Extent3d {
                    width: allocated_width,
                    height: allocated_height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            let replacement = DamageTexture {
                width: allocated_width,
                height: allocated_height,
                texture,
                view,
            };
            if let Some(target) = self.damage_textures.first_mut() {
                *target = replacement;
            } else {
                self.damage_textures.push(replacement);
            }
            if let Some(timing) = timing.as_mut() {
                timing.damage_target_creations = timing.damage_target_creations.saturating_add(1);
            }
        }
        let target = self
            .damage_textures
            .first()
            .expect("damage target was inserted above");
        if let (Some(timing), Some(setup_start)) = (timing.as_mut(), setup_start) {
            timing.damage_setup_us = timing
                .damage_setup_us
                .saturating_add(setup_start.elapsed().as_micros());
        }
        let render_start = timing.is_some().then(crate::time::now);
        let mut copy_encode_us = 0u128;
        self.renderer
            .render_to_texture_with_post_render(
                &device,
                &queue,
                scene,
                &target.view,
                &RenderParams {
                    base_color: bg,
                    width: prefix_width,
                    height: prefix_height,
                    antialiasing_method: AaConfig::Area,
                },
                |encoder| {
                    let copy_start = timing.is_some().then(crate::time::now);
                    for region in &regions {
                        encoder.copy_texture_to_texture(
                            wgpu::TexelCopyTextureInfo {
                                texture: &target.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: region.x,
                                    y: region.y,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyTextureInfo {
                                texture: &self.texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: region.x,
                                    y: region.y,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::Extent3d {
                                width: region.width,
                                height: region.height,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                    if let Some(copy_start) = copy_start {
                        copy_encode_us = copy_start.elapsed().as_micros();
                    }
                },
            )
            .expect("render_to_texture");
        self.record_canonical_segment_order_full_selection(timing);
        self.record_area_ppt8_full_selection(timing);
        self.record_clip_single_group_full_selection(timing);
        let timestamp_submission = self.timestamp_submission_id(timing.is_some());
        let dispatch_timestamp_submission = self.dispatch_timestamp_submission_id(timing.is_some());
        if let (Some(timing), Some(render_start)) = (timing.as_mut(), render_start) {
            let combined_us = render_start.elapsed().as_micros();
            timing.vello_submit_us = timing
                .vello_submit_us
                .saturating_add(combined_us.saturating_sub(copy_encode_us));
            timing.copy_submit_us = timing.copy_submit_us.saturating_add(copy_encode_us);
            timing.coarse_prefix_pixels = timing.coarse_prefix_pixels.saturating_add(raster_pixels);
        }
        while self.damage_textures.len() > 1 {
            let target = self.damage_textures.pop().expect("length checked above");
            self.renderer.invalidate_texture_bindings(&target.view);
        }
        self.coarse_pool_high_water_pixels = self.coarse_pool_high_water_pixels.max(raster_pixels);
        let overflowed = if let Some(timing) = timing.as_mut() {
            let start = crate::time::now();
            let overflowed = self.renderer.settle_bump_budget(&device);
            timing.settle_wait_us = timing
                .settle_wait_us
                .saturating_add(start.elapsed().as_micros());
            overflowed
        } else {
            self.renderer.settle_bump_budget(&device)
        };
        self.record_timestamp(timestamp_submission, timing);
        self.record_dispatch_timestamp(dispatch_timestamp_submission, timing);
        if overflowed {
            // Account for Vello bump growth before claiming completion.
            // The full retry is conservative and restores every target pixel.
            return raster_pixels.saturating_add(self.render_gpu_wait_count(scene, bg, timing));
        }
        raster_pixels
    }

    /// [`render`](Self::render) with a transparent background.
    pub fn render_rgba(&mut self, scene: &Scene) -> Vec<u8> {
        self.render(scene, Color::TRANSPARENT)
    }

    /// Render until Vello confirms that no bump-allocated geometry was
    /// dropped, up to a strict attempt limit. This checks the renderer's GPU
    /// counters, so partially missing glyphs are caught as reliably as a
    /// completely transparent failed frame.
    ///
    /// # Panics
    ///
    /// Panics when `Vello` cannot produce two consecutive valid submissions
    /// within the bounded retry count. Returning partial screenshot pixels
    /// would make visual-regression output silently untrustworthy.
    #[allow(
        clippy::panic,
        reason = "exhausting bounded screenshot retries is a fatal renderer invariant"
    )]
    pub fn render_resilient(&mut self, scene: &Scene, bg: Color) -> Vec<u8> {
        self.try_render_resilient(scene, bg)
            .unwrap_or_else(|problem| panic!("{problem}"))
    }

    /// [`Self::render_resilient`] for a caller that can answer a failure
    /// instead of stopping: an app rendering a picture someone asked for.
    ///
    /// # Errors
    /// Returns why no two consecutive overflow-free renders came within the
    /// bounded retry count.
    pub fn try_render_resilient(&mut self, scene: &Scene, bg: Color) -> Result<Vec<u8>, String> {
        let mut confirmed = false;
        let mut attempts = ResilientAttempts::default();
        loop {
            let reserved = self.renderer.memory_stats().bump_budget_bytes;
            let (pixels, overflowed) = self.render_with_overflow(scene, bg);
            let grew = self.renderer.memory_stats().bump_budget_bytes > reserved;
            let unexpectedly_transparent = bg == Color::TRANSPARENT
                && !scene.encoding().is_empty()
                && pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel.get(3).copied().unwrap_or(0) == 0);
            if overflowed || unexpectedly_transparent {
                // Growth invalidates every earlier target. A new atlas or
                // bump allocation can make a non-empty first retry look
                // plausible while still omitting the earliest paths, so do
                // not let it count as the confirmation frame.
                confirmed = false;
            } else if resilient_submission_accepted(scene.encoding().is_empty(), confirmed) {
                return Ok(pixels);
            } else {
                // A scene that just fit is still not sufficient: Vello can use
                // that submission to populate its glyph/image atlas. Require a
                // second consecutive clean frame before writing a screenshot.
                confirmed = true;
            }
            if !attempts.another(grew) {
                return Err(format!(
                    "Vello did not produce two consecutive overflow-free screenshot renders after {} attempts ({} of them growing its buffers)",
                    attempts.submissions(),
                    attempts.growths,
                ));
            }
        }
    }

    /// Render a one-shot scene without accepting Vello's transparent
    /// bump-buffer warm-up frame. Interactive rendering naturally gets a next
    /// frame after the reservation grows; screenshots, first recording frames
    /// and deterministic visual snapshots do not, so they retry here.
    pub fn render_rgba_resilient(&mut self, scene: &Scene) -> Vec<u8> {
        self.render_resilient(scene, Color::TRANSPARENT)
    }

    /// Encode a complete PNG before publishing it. Screenshot tools are often
    /// interrupted by a new iteration or CI timeout; writing the final path
    /// directly made those interruptions look like successful, corrupt PNGs.
    #[allow(
        clippy::expect_used,
        reason = "PNG encoding is internal to the explicitly infallible render_png debug helper"
    )]
    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        crate::png::encode_rgba8(width, height, rgba).expect("png encode")
    }

    /// Convenience: render `scene` and write the result to `path` as a
    /// PNG. Creates parent dirs if they don't exist.
    ///
    /// # Panics
    ///
    /// Panics if `path`'s file cannot be created or PNG encoding fails —
    /// both are dev/test-environment problems (disk full, bad permissions)
    /// with no recoverable action for this infallible, shots/test-only
    /// helper to take.
    #[allow(
        clippy::expect_used,
        reason = "render_png is a debug/test screenshot helper called as infallible from shots and test call sites across the workspace; a write/encode failure is a dev-environment problem worth crashing loudly on, not a Result this signature can propagate without a wide API change"
    )]
    pub fn render_png<P: AsRef<Path>>(&mut self, scene: &Scene, bg: Color, path: P) {
        let rgba = self.render_resilient(scene, bg);
        tilo_files::native::atomic::write_creating_parent(
            path.as_ref(),
            &Self::encode_png(self.width, self.height, &rgba),
        )
        .expect("atomically publish png");
    }

    /// Sample the RGBA at `(x, y)` from a tight-packed buffer (4 *
    /// width bytes per row). Returns `(r, g, b, a)`.
    ///
    /// # Panics
    ///
    /// Panics if `(x, y)` is outside `rgba`. Every real call site is test
    /// code asserting against known-valid coordinates (see the crate's
    /// render tests), so an out-of-range sample is a test bug that should
    /// fail loudly rather than silently read back `0`.
    #[allow(
        clippy::indexing_slicing,
        reason = "sample() exists only for test pixel assertions; an out-of-range coordinate is a test bug that should panic, not silently return 0"
    )]
    pub fn sample(rgba: &[u8], width: u32, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let idx = (y.saturating_mul(width).saturating_add(x)).saturating_mul(4) as usize;
        (
            rgba[idx],
            rgba[idx.saturating_add(1)],
            rgba[idx.saturating_add(2)],
            rgba[idx.saturating_add(3)],
        )
    }

    /// Compares stored straight-alpha RGBA8 pixels with the ±1 antialiasing
    /// tolerance described in the module docs. RGB in fully transparent
    /// pixels is invisible; if either pixel has nonzero alpha, every channel
    /// participates in the tolerance. Buffers of different lengths count as
    /// differing everywhere they don't overlap.
    pub fn pixels_differing_beyond_aa(a: &[u8], b: &[u8]) -> usize {
        let overlapping = a
            .chunks_exact(4)
            .zip(b.chunks_exact(4))
            .filter(|(x, y)| differs_beyond_aa(x, y))
            .count();
        let unmatched = a.len().abs_diff(b.len()).div_ceil(4);
        overlapping.saturating_add(unmatched)
    }

    /// Where [`Self::pixels_differing_beyond_aa`] finds two equally long
    /// frames `width` pixels wide differ, as `(x, y)`.
    pub fn positions_differing_beyond_aa(a: &[u8], b: &[u8], width: usize) -> Vec<(usize, usize)> {
        a.chunks_exact(4)
            .zip(b.chunks_exact(4))
            .enumerate()
            .filter(|(_, (x, y))| differs_beyond_aa(x, y))
            .map(|(index, _)| (index % width.max(1), index / width.max(1)))
            .collect()
    }
}

/// Whether two stored straight-alpha pixels differ beyond the ±1 the module
/// docs describe; RGB under zero alpha on both sides is invisible.
fn differs_beyond_aa(a: &[u8], b: &[u8]) -> bool {
    if a.get(3) == Some(&0) && b.get(3) == Some(&0) {
        return false;
    }
    a.iter().zip(b.iter()).any(|(p, q)| p.abs_diff(*q) > 1)
}

#[cfg(test)]
mod tests {

    use super::{
        AreaPpt8AttemptAccounting, CanonicalSegmentOrderAttemptAccounting,
        ClipSingleGroupAttemptAccounting, OffscreenRenderTiming, OffscreenRenderer,
        RESILIENT_RENDER_ATTEMPTS, RESILIENT_RENDER_GROWTHS, ResilientAttempts,
        SparseSubmissionAccounting, damage_texture_extent,
        damage_texture_geometric_needs_replacement, damage_texture_geometric_width_extent,
        damage_texture_height_extent, damage_texture_needs_replacement, env_enabled_by_default,
        resilient_submission_accepted, shared_gpu, timestamp_last_and_other_pass_ns,
    };
    use vello::Scene;
    use vello::kurbo::{Affine, BezPath, Rect};
    use vello::peniko::{BlendMode, Color, Fill, ImageBrush, ImageData};

    #[test]
    fn an_image_after_text_and_an_empty_sub_scene_lands_where_it_is_painted() {
        // Chat paints a line of text, appends a sub-scene that holds nothing,
        // then an attachment. The text's glyph run leaves its own transform
        // last in the stream; the empty append must not make the picture
        // inherit it (that moved it off its card, so it never showed).
        use std::sync::Arc;
        use vello::peniko::{Blob, ImageAlphaType, ImageFormat};
        const W: u32 = 256;
        const H: u32 = 192;
        let image = ImageData {
            data: Blob::new(Arc::new(
                [230_u8, 40, 40, 255]
                    .into_iter()
                    .cycle()
                    .take(8 * 8 * 4)
                    .collect::<Vec<u8>>(),
            )),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 8,
            height: 8,
        };
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(0, 0, 1),
            None,
            &Rect::new(0.0, 0.0, 4.0, 4.0),
        );
        crate::text_render::TextRenderer::new().draw_text(
            &mut scene,
            "Screen recording",
            14.0,
            Color::from_rgb8(0, 0, 2),
            Affine::translate((10.0, 100.0)),
            None,
        );
        scene.append(&Scene::new(), None);
        let card = Rect::new(100.0, 20.0, 160.0, 80.0);
        crate::image_store::paint_image_cover(&mut scene, Affine::IDENTITY, card, &image, 6.0);
        let pixels = OffscreenRenderer::new(W, H).render(&scene, Color::BLACK);
        let (r, g, b, _) = OffscreenRenderer::sample(&pixels, W, 130, 50);
        assert!(
            r > 200 && g < 80 && b < 80,
            "the picture on its card: {r},{g},{b}"
        );
        let (r, ..) = OffscreenRenderer::sample(&pixels, W, 150, 170);
        assert!(r < 20, "nothing under the text's transform: {r}");
    }

    #[test]
    fn offscreen_publication_is_send_resilient_and_decodable() {
        // Case `offscreen_renderer_stays_send`:
        // A `static Mutex<Option<OffscreenRenderer>>` (see the module docs)
        // only compiles while this holds, so a field that quietly gave up
        // `Send` should fail here, where the reason is written down.
        {
            fn assert_send<T: Send>() {}
            assert_send::<OffscreenRenderer>();
        }

        // Case `resilient_render_requires_a_second_clean_nonempty_submission`.
        {
            // A first clean output may have warmed Vello's atlas, so only the
            // next clean submission may leave the offscreen renderer.
            assert!(!resilient_submission_accepted(false, false));
            assert!(resilient_submission_accepted(false, true));

            // Intentionally empty transparent scenes need no atlas warm-up.
            assert!(resilient_submission_accepted(true, false));
        }

        // Case `resilient_render_waits_through_every_stage_growing_in_turn`.
        {
            // Vello's later stages skip their work once an earlier one has
            // overflowed, so a cold renderer grows one stage per submission.
            // Five growths in a row then two clean frames must be allowed:
            // a flat four-attempt limit refused a feathered photo mask.
            let mut attempts = ResilientAttempts::default();
            for _ in 0..5 {
                assert!(attempts.another(true), "a growth is progress");
            }
            assert!(attempts.another(false), "the first clean frame");
            assert_eq!(attempts.submissions(), 6);

            // A reservation that cannot grow any more still gives up after
            // the bounded attempts, however many growths came before.
            let mut stuck = ResilientAttempts::default();
            let mut submissions = 0;
            while stuck.another(true) {
                submissions += 1;
                assert!(submissions < 64, "growth alone must not retry forever");
            }
            assert_eq!(
                stuck.submissions(),
                RESILIENT_RENDER_GROWTHS + RESILIENT_RENDER_ATTEMPTS
            );
            let mut stalled = ResilientAttempts::default();
            let allowed = (0..RESILIENT_RENDER_ATTEMPTS)
                .filter(|_| stalled.another(false))
                .count();
            assert_eq!(allowed, RESILIENT_RENDER_ATTEMPTS - 1);
        }

        // Case `png_publication_is_complete_and_decodable`.
        {
            let dir = tempfile::tempdir().expect("temporary screenshot directory");
            let path = dir.path().join("frame.png");
            let rgba = [0x12, 0x34, 0x56, 0xff];
            tilo_files::native::atomic::write_creating_parent(
                &path,
                &OffscreenRenderer::encode_png(1, 1, &rgba),
            )
            .expect("publish png");

            let decoded = crate::png::read_rgba8(&path).expect("published png is complete");
            assert_eq!((decoded.width, decoded.height), (1, 1));
            assert_eq!(decoded.rgba, rgba);
        }
    }

    /// A cold renderer learns one Vello stage's demand per overflowing
    /// submission, because every later stage skips its work once an earlier
    /// one has overflowed: here the flattened lines, then the segment counts,
    /// then the nested layers' blend spill. A one-shot render must wait
    /// through every growth. A flat four-attempt limit refused Canvas's
    /// measure of a photograph behind a feathered mask.
    #[test]
    fn resilient_render_grows_through_cascading_stage_overflows() {
        let Ok(mut renderer) = OffscreenRenderer::try_new(320, 320) else {
            eprintln!("skipping cascading overflow test: no GPU");
            return;
        };
        let side = 320.0;
        let full = Rect::new(0.0, 0.0, side, side);
        let mut scene = Scene::new();
        // Deeper than Vello's on-chip blend stack on every tile.
        for _ in 0..8 {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                0.95,
                Affine::IDENTITY,
                &full,
            );
        }
        // Thousands of loops across the whole target: far more flattened
        // lines, and line/tile crossings, than a fresh renderer reserves.
        let mut loops = BezPath::new();
        for i in 0..20_000_u32 {
            let y = f64::from(i % 320);
            loops.move_to((0.0, y));
            loops.curve_to((side * 1.5, side - y), (-side * 0.5, side - y), (side, y));
            loops.close_path();
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(200, 60, 40, 255),
            None,
            &loops,
        );
        for _ in 0..8 {
            scene.pop_layer();
        }
        let pixels = renderer
            .try_render_resilient(&scene, Color::TRANSPARENT)
            .expect("growing through every stage's overflow, then two clean renders");
        assert!(
            pixels
                .chunks_exact(4)
                .any(|pixel| pixel.get(3).copied().unwrap_or(0) != 0),
            "the loops are drawn"
        );
    }

    #[test]
    fn optimization_attempt_accounting_resets_and_separates_routes() {
        // Case `sparse_submission_accounting_resets_and_counts_only_applied_optimizations`.
        {
            let mut accounting = SparseSubmissionAccounting::default();
            assert_eq!(accounting, SparseSubmissionAccounting::default());
            assert_eq!(accounting.solid_fuse_selected_submissions, 0);
            assert_eq!(accounting.segment_row_mask_selected_submissions, 0);
            assert_eq!(
                accounting.canonical_segment_order_selected_sparse_attempts,
                0
            );
            assert_eq!(accounting.area_ppt8_selected_sparse_attempts, 0);
            assert_eq!(accounting.clip_single_group_shader_selected, 0);
            accounting.record(vello::SparseRenderStats::default());
            accounting.record(vello::SparseRenderStats {
                coarse_active_bins: true,
                path_tile_bounds_applied: true,
                path_tile_bounds_tiles: 24,
                coarse_opaque_prune_selected: true,
                solid_fuse_selected: true,
                segment_row_mask_selected: true,
                canonical_segment_order_shader_selected: true,
                area_ppt8_shader_selected: true,
                clip_single_group_shader_selected: true,
                fine_quad_skip_applied: true,
                fine_quad_total_quads: 64,
                fine_quad_active_quads: 16,
                ..vello::SparseRenderStats::default()
            });
            // A sparse fallback may report a candidate area but must not be counted as applied.
            accounting.record(vello::SparseRenderStats {
                path_tile_bounds_applied: false,
                path_tile_bounds_tiles: 99,
                coarse_opaque_prune_selected: false,
                solid_fuse_selected: false,
                segment_row_mask_selected: false,
                canonical_segment_order_shader_selected: false,
                area_ppt8_shader_selected: false,
                clip_single_group_shader_selected: false,
                fine_quad_skip_applied: false,
                fine_quad_total_quads: 64,
                fine_quad_active_quads: 1,
                ..vello::SparseRenderStats::default()
            });
            assert_eq!(accounting.sparse_submissions, 3);
            assert_eq!(accounting.active_sparse_submissions, 1);
            assert_eq!(accounting.path_tile_bounds_submissions, 1);
            assert_eq!(accounting.path_tile_bounds_tiles, 24);
            assert_eq!(accounting.coarse_opaque_prune_selected_submissions, 1);
            assert_eq!(accounting.solid_fuse_selected_submissions, 1);
            assert_eq!(accounting.segment_row_mask_selected_submissions, 1);
            assert_eq!(
                accounting.canonical_segment_order_selected_sparse_attempts,
                1
            );
            assert_eq!(accounting.area_ppt8_selected_sparse_attempts, 1);
            assert_eq!(accounting.clip_single_group_shader_selected, 1);
            assert_eq!(accounting.fine_quad_skip_submissions, 1);
            assert_eq!(accounting.fine_quad_total_quads, 64);
            assert_eq!(accounting.fine_quad_active_quads, 16);
            assert_eq!(accounting.fine_quad_skipped_quads, 48);
            accounting.reset();
            assert_eq!(accounting, SparseSubmissionAccounting::default());
        }

        // Case `canonical_segment_order_attempt_accounting_resets_and_separates_routes`.
        {
            let mut accounting = CanonicalSegmentOrderAttemptAccounting::default();
            accounting.record_full(true);
            accounting.record_full(false);
            accounting.record_sparse(true);
            accounting.record_sparse(true);
            assert_eq!(accounting.full_selected_attempts, 1);
            assert_eq!(accounting.sparse_selected_attempts, 2);
            accounting.reset();
            assert_eq!(
                accounting,
                CanonicalSegmentOrderAttemptAccounting::default()
            );
        }

        // Case `area_ppt8_attempt_accounting_resets_and_separates_routes`.
        {
            let mut accounting = AreaPpt8AttemptAccounting::default();
            accounting.record_full(true);
            accounting.record_full(false);
            accounting.record_sparse(true);
            accounting.record_sparse(true);
            assert_eq!(accounting.full_selected_attempts, 1);
            assert_eq!(accounting.sparse_selected_attempts, 2);
            accounting.reset();
            assert_eq!(accounting, AreaPpt8AttemptAccounting::default());
        }

        // Case `clip_single_group_attempt_accounting_resets_and_separates_routes`.
        {
            let mut accounting = ClipSingleGroupAttemptAccounting::default();
            accounting.record_full(true);
            accounting.record_full(false);
            accounting.record_sparse(true);
            accounting.record_sparse(false);
            accounting.record_sparse(true);
            assert_eq!(accounting.full_selected_attempts, 1);
            assert_eq!(accounting.sparse_selected_attempts, 2);
            accounting.reset();
            assert_eq!(accounting, ClipSingleGroupAttemptAccounting::default());
        }
    }

    #[test]
    fn damage_and_geometric_capacity_grow_in_bounded_hysteretic_buckets() {
        // Case `damage_texture_extent_is_quantized_and_device_bounded`.
        {
            assert_eq!(damage_texture_extent(1, 8_192), 32);
            assert_eq!(damage_texture_extent(801, 8_192), 832);
            assert_eq!(damage_texture_extent(8_191, 8_192), 8_192);
            assert_eq!(damage_texture_height_extent(172, 8_192), 176);
            assert_eq!(damage_texture_height_extent(8_191, 8_192), 8_192);
        }

        // Case `damage_texture_reuses_animation_jitter_but_releases_large_outliers`.
        {
            assert!(!damage_texture_needs_replacement(832, 128, 809, 96));
            assert!(damage_texture_needs_replacement(832, 128, 400, 96));
            assert!(damage_texture_needs_replacement(832, 128, 833, 96));
            assert!(damage_texture_needs_replacement(1_024, 1_024, 128, 128));
        }

        // Case `geometric_width_capacity_grows_in_bounded_buckets`.
        {
            assert_eq!(damage_texture_geometric_width_extent(33, 3_840, 8_192), 64);
            assert_eq!(
                damage_texture_geometric_width_extent(1_025, 3_840, 8_192),
                2_048
            );
            assert_eq!(
                damage_texture_geometric_width_extent(2_049, 3_840, 8_192),
                3_840
            );
            assert_eq!(
                damage_texture_geometric_width_extent(8_191, 8_192, 8_192),
                8_192
            );
            assert_eq!(damage_texture_geometric_width_extent(16, 16, 8_192), 16);
            assert_eq!(
                damage_texture_geometric_width_extent(4_095, 8_192, 4_096),
                4_096
            );
        }

        // Case `geometric_capacity_uses_hysteresis_and_never_recreates_identical_size`.
        {
            assert!(!damage_texture_geometric_needs_replacement(
                2_048, 176, 1_026, 172, 2_048, 176,
            ));
            assert!(!damage_texture_geometric_needs_replacement(
                2_048, 176, 1_024, 172, 1_024, 176,
            ));
            assert!(damage_texture_geometric_needs_replacement(
                2_048, 176, 2_049, 172, 3_840, 176,
            ));
            assert!(damage_texture_geometric_needs_replacement(
                2_048, 176, 512, 44, 512, 48,
            ));
        }

        // Case `geometric_capacity_avoids_forward_animation_recreation_churn`.
        {
            let mut baseline_capacity = 0;
            let mut geometric_capacity = 0;
            let mut baseline_creations = 0u32;
            let mut geometric_creations = 0u32;
            for required in (1_025..=2_048).step_by(32) {
                if baseline_capacity < required {
                    baseline_capacity = damage_texture_extent(required, 3_840);
                    baseline_creations = baseline_creations.saturating_add(1);
                }
                let desired = damage_texture_geometric_width_extent(required, 3_840, 8_192);
                if geometric_capacity < required {
                    geometric_capacity = desired;
                    geometric_creations = geometric_creations.saturating_add(1);
                }
            }
            assert!(baseline_creations > geometric_creations);
            assert_eq!(geometric_creations, 1);
        }
    }

    fn timestamp_sample(
        timestamps: &[u64],
        pass_count: u32,
        period_ns: f32,
        summed_ns: f64,
    ) -> vello::GpuTimestampSample {
        vello::GpuTimestampSample {
            submission_id: 1,
            pass_count,
            span_ticks: 0,
            summed_pass_ticks: 0,
            timestamp_period_ns: period_ns,
            span_ns: 0.0,
            summed_pass_ns: summed_ns,
            timestamp_bytes: timestamps
                .iter()
                .flat_map(|tick| tick.to_le_bytes())
                .collect(),
        }
    }

    #[test]
    fn timestamp_split_and_frame_comparison_accept_only_valid_input() {
        // Case `timestamp_last_pass_split_handles_one_and_many_passes`.
        {
            let single = timestamp_sample(&[10, 16], 1, 0.5, 3.0);
            assert_eq!(timestamp_last_and_other_pass_ns(&single), Some((3, 0)));

            // (16 - 10) * .5 = 3 and (29 - 20) * .5 = 4.5, rounded down.
            let multiple = timestamp_sample(&[10, 16, 20, 29], 2, 0.5, 7.5);
            assert_eq!(timestamp_last_and_other_pass_ns(&multiple), Some((4, 3)));
        }

        // Case `timestamp_last_pass_split_rejects_incomplete_or_backwards_pairs`.
        {
            let incomplete = timestamp_sample(&[10, 16], 2, 1.0, 6.0);
            assert_eq!(timestamp_last_and_other_pass_ns(&incomplete), None);

            // Even a good final pair cannot make an earlier backwards pair a
            // complete measurement.
            let backwards = timestamp_sample(&[16, 10, 20, 24], 2, 1.0, 4.0);
            assert_eq!(timestamp_last_and_other_pass_ns(&backwards), None);
        }

        // Case `frame_comparison_tolerates_one_step_and_no_more`:
        // Without this guard the tolerance could be widened later — turning
        // "these are the same frame" into an assertion that passes on frames
        // that are visibly different — with no test noticing.
        {
            let base = [10u8, 20, 30, 255, 40, 50, 60, 255];

            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&base, &base),
                0,
                "identical buffers must not report a difference"
            );

            // Every channel off by one, in both directions: still one frame.
            let wobbled = [11u8, 19, 31, 254, 39, 51, 59, 255];
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&base, &wobbled),
                0,
                "a one-step antialiasing wobble must read as the same frame"
            );

            // Two steps on a single channel of a single pixel is a real
            // difference.
            let two_steps = [12u8, 20, 30, 255, 40, 50, 60, 255];
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&base, &two_steps),
                1,
                "a two-step difference must be reported"
            );

            // A truncated buffer is never a match, however similar its
            // overlapping prefix is.
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&base, &base[..4]),
                1,
                "a size mismatch must count the pixels that have no counterpart"
            );
        }

        // Case `frame_comparison_ignores_rgb_only_when_both_pixels_are_fully_transparent`.
        {
            for (actual, expected, differences) in [
                ([0, 0, 0, 0], [81, 209, 119, 0], 0),
                ([10, 20, 30, 1], [12, 20, 30, 1], 1),
                ([10, 20, 30, 0], [12, 20, 30, 1], 1),
                ([12, 20, 30, 1], [10, 20, 30, 0], 1),
                ([10, 20, 30, 0], [10, 20, 30, 2], 1),
                ([10, 20, 30, 254], [10, 20, 30, 252], 1),
            ] {
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(&actual, &expected),
                    differences,
                    "actual={actual:?}, expected={expected:?}"
                );
            }

            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &[0, 0, 0, 0, 81, 209, 119, 0],
                    &[255, 0, 255, 0],
                ),
                1,
                "transparent pixels without a counterpart remain a size mismatch"
            );
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "end-to-end retained renderer oracle; end-to-end geometric damage capacity oracle"
    )]
    #[test]
    fn native_and_retained_damage_match_full_renders() {
        // Case `native_damage_matches_a_full_render_and_preserves_untouched_pixels`.
        {
            const W: u32 = 192;
            const H: u32 = 128;
            let mut initial = Scene::new();
            initial.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(24, 32, 48),
                None,
                &Rect::new(8.0, 8.0, 88.0, 56.0),
            );
            let mut updated = initial.clone();
            updated.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(80, 220, 120),
                None,
                &Rect::new(28.0, 20.0, 68.0, 44.0),
            );
            // Deliberately disconnected from the progress-like centre update:
            // this exercises one native-origin prefix target with two independent
            // dirty-region copies.
            updated.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(230, 100, 40),
                None,
                &Rect::new(4.0, 4.0, 16.0, 14.0),
            );

            let mut retained = OffscreenRenderer::new(W, H);
            retained.set_sparse_fine_enabled(false);
            let before = retained.render(&initial, Color::BLACK);
            let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &updated,
                Color::BLACK,
                &[
                    Rect::new(26.0, 18.0, 70.0, 46.0),
                    Rect::new(2.0, 2.0, 18.0, 16.0),
                ],
            );
            assert_eq!(raster_pixels, 70 * 46);
            assert_eq!(
                timing.full_attempts, 0,
                "small damage must avoid full recovery"
            );
            let partial = retained.readback_current();

            let mut reference = OffscreenRenderer::new(W, H);
            let full = reference.render(&updated, Color::BLACK);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&partial, &full),
                0,
                "native damage composition must match a complete frame",
            );
            // (80, 12) is outside both [26,70)×[18,46) and [2,18)×[2,16).
            assert_eq!(
                OffscreenRenderer::sample(&before, W, 80, 12),
                OffscreenRenderer::sample(&partial, W, 80, 12),
                "pixels outside damage stay byte-identical",
            );
        }

        // Case `native_damage_tracks_disconnected_transparent_clipped_motion_and_resize`.
        {
            const W: u32 = 256;
            const H: u32 = 192;
            let scene_for = |left: Rect, right: Rect| {
                let mut scene = Scene::new();
                let outer = Rect::new(12.0, 10.0, 116.0, 86.0);
                let inner = Rect::new(22.0, 18.0, 106.0, 80.0);
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &outer);
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &inner);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(70, 150, 250, 170),
                    None,
                    &left,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(250, 110, 60, 145),
                    None,
                    &right,
                );
                scene.pop_layer();
                scene.pop_layer();
                scene
            };
            let first_left = Rect::new(26.0, 26.0, 42.0, 38.0);
            let first_right = Rect::new(88.0, 64.0, 101.0, 76.0);
            let second_left = Rect::new(34.0, 24.0, 56.0, 42.0);
            let second_right = Rect::new(78.0, 61.0, 104.0, 78.0);
            let third_left = Rect::new(24.0, 30.0, 62.0, 48.0);
            let third_right = Rect::new(91.0, 52.0, 103.0, 79.0);

            let mut retained = OffscreenRenderer::new(W, H);
            retained.render(&scene_for(first_left, first_right), Color::TRANSPARENT);
            for (previous_left, previous_right, next_left, next_right) in [
                (first_left, first_right, second_left, second_right),
                (second_left, second_right, third_left, third_right),
            ] {
                let next = scene_for(next_left, next_right);
                // Old and new bounds are separate regions: clearing transparent
                // old pixels is as important as painting the moved pixels.
                retained.render_damage_regions_gpu_wait(
                    &next,
                    Color::TRANSPARENT,
                    &[
                        previous_left.inflate(1.0, 1.0),
                        previous_right.inflate(1.0, 1.0),
                        next_left.inflate(1.0, 1.0),
                        next_right.inflate(1.0, 1.0),
                    ],
                );
                let partial = retained.readback_current();
                let mut reference = OffscreenRenderer::new(W, H);
                let full = reference.render(&next, Color::TRANSPARENT);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(&partial, &full),
                    0,
                    "clipped transparent native-damage frame must equal a full render",
                );
                assert_eq!(
                    OffscreenRenderer::sample(&partial, W, 4, 4).3,
                    0,
                    "transparent pixels outside nested clips must remain transparent",
                );
            }
        }

        // Case `retained_damage_matches_full_with_clips_overlaps_and_image_updates`.
        {
            const W: u32 = 256;
            const H: u32 = 192;
            let image = |rgba: [u8; 4]| {
                crate::raster::rgba8(
                    rgba.into_iter()
                        .cycle()
                        .take(4 * 4 * 4)
                        .collect::<Vec<u8>>(),
                    4,
                    4,
                )
            };
            let blue = image([20, 120, 255, 190]);
            let orange = image([255, 100, 30, 170]);
            let scene_for = |image: &ImageData, image_rect: Rect, overlap: Rect| {
                let mut scene = Scene::new();
                let clip = Rect::new(16.0, 12.0, 132.0, 104.0);
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(100, 40, 220, 120),
                    None,
                    &overlap,
                );
                let brush = ImageBrush::new(image.clone());
                let scale = Affine::translate((image_rect.x0, image_rect.y0))
                    * Affine::scale(image_rect.width() / 4.0);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    &brush,
                    Some(scale),
                    &image_rect,
                );
                scene.pop_layer();
                scene
            };
            let old = Rect::new(28.0, 26.0, 52.0, 50.0);
            let moved = Rect::new(64.0, 34.0, 92.0, 62.0);
            let replaced = Rect::new(44.0, 56.0, 78.0, 90.0);
            let overlap_a = Rect::new(42.0, 30.0, 76.0, 64.0);
            let overlap_b = Rect::new(54.0, 50.0, 88.0, 84.0);
            let first = scene_for(&blue, old, overlap_a);
            let second = scene_for(&blue, moved, overlap_a);
            let third = scene_for(&orange, replaced, overlap_b);
            let mut retained = OffscreenRenderer::new(W, H);
            retained.render_rgba_resilient(&first);
            let sparse_supported = retained.vello().sparse_fine_supported();
            let mut reference = OffscreenRenderer::new(W, H);
            reference.set_compute_pass_batching(false);
            reference.set_direct_path_dispatch_enabled(false);
            reference.set_defer_bump_readback_enabled(false);
            reference.set_sparse_fine_enabled(false);
            reference.set_cpu_metadata_enabled(false);
            for (frame, (next, dirty)) in [
                (
                    &second,
                    vec![
                        old.inflate(1.0, 1.0),
                        moved.inflate(1.0, 1.0),
                        old.inflate(1.0, 1.0),
                    ],
                ),
                (
                    &third,
                    vec![
                        moved.inflate(1.0, 1.0),
                        replaced.inflate(1.0, 1.0),
                        overlap_a.inflate(1.0, 1.0),
                        overlap_b.inflate(1.0, 1.0),
                    ],
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let before = retained.readback_current();
                let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    next,
                    Color::TRANSPARENT,
                    &dirty,
                );
                assert!(raster_pixels < u64::from(W).saturating_mul(u64::from(H)));
                if sparse_supported {
                    assert_eq!(timing.sparse_submissions, 1);
                    assert!(timing.sparse_fine_tiles > 0);
                    assert!(timing.sparse_written_pixels > 0);
                }
                let partial = retained.readback_current();
                let full = reference.render_rgba_resilient(next);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(&partial, &full),
                    0
                );
                assert_eq!(
                    OffscreenRenderer::sample(&before, W, 8, 8),
                    OffscreenRenderer::sample(&partial, W, 8, 8)
                );
                let image_only = if frame == 0 {
                    OffscreenRenderer::sample(&partial, W, 82, 40)
                } else {
                    OffscreenRenderer::sample(&partial, W, 48, 86)
                };
                assert!(image_only.3 > 0, "image-only pixel must not be transparent");
                if frame == 0 {
                    assert!(
                        image_only.2 > image_only.0,
                        "reused blue image must remain blue"
                    );
                } else {
                    assert!(
                        image_only.0 > image_only.2,
                        "replacement image must be orange"
                    );
                }
                // Exercise the next partial update after a full retained-target refresh.
                if frame == 0 {
                    retained.render_damage_regions_gpu_wait(
                        &second,
                        Color::TRANSPARENT,
                        &[Rect::new(0.0, 0.0, f64::from(W), f64::from(H))],
                    );
                }
            }
        }

        // Case `geometric_damage_capacity_grows_and_shrinks_without_changing_native_pixels`.
        {
            const W: u32 = 512;
            const H: u32 = 128;
            let scene_for = |rect: Rect, color: Color| {
                let mut scene = Scene::new();
                scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
                scene
            };
            // The native prefix widths for these shapes are 40, 104, and 216,
            // which map to 64, 128, and 256 pixel geometric capacities.
            let narrow = Rect::new(24.0, 20.0, 40.0, 44.0);
            let middle = Rect::new(88.0, 20.0, 104.0, 44.0);
            let wide = Rect::new(200.0, 20.0, 216.0, 44.0);
            let first = scene_for(narrow, Color::from_rgba8(40, 120, 250, 180));
            let second = scene_for(middle, Color::from_rgba8(250, 90, 50, 180));
            let third = scene_for(wide, Color::from_rgba8(70, 220, 130, 180));

            let mut retained = OffscreenRenderer::new(W, H);
            // Tests do not mutate the process environment. Enable this renderer's
            // sampled mode explicitly so this covers the opt-in production path.
            retained.damage_geometric_capacity = true;
            retained.set_sparse_fine_enabled(false);
            retained.render_rgba_resilient(&first);

            let mut reference = OffscreenRenderer::new(W, H);
            reference.set_compute_pass_batching(false);
            reference.set_defer_bump_readback_enabled(false);
            let initial_reference = reference.render_rgba_resilient(&first);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &initial_reference,
                ),
                0,
            );

            let assert_partial = |retained: &mut OffscreenRenderer,
                                  reference: &mut OffscreenRenderer,
                                  scene: &Scene,
                                  dirty: &[Rect],
                                  expected_capacity: u32,
                                  expected_creations: u32| {
                let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    scene,
                    Color::TRANSPARENT,
                    dirty,
                );
                assert!(
                    raster_pixels < u64::from(W).saturating_mul(u64::from(H)),
                    "these small prefix updates must not take the full fallback",
                );
                assert_eq!(timing.damage_target_creations, expected_creations);
                if retained.vello().gpu_timestamp_buffer_bytes() != 0 {
                    assert!(
                        timing.gpu_timestamp_expected_submissions > 0,
                        "timestamp-enabled partial render must record its submission",
                    );
                    assert_eq!(
                        timing.gpu_timestamp_sampled_submissions,
                        timing.gpu_timestamp_expected_submissions,
                        "timestamp-enabled partial render must read every submitted sample",
                    );
                    assert_eq!(timing.gpu_timestamp_missing_submissions, 0);
                    assert!(
                        timing.gpu_timestamp_pass_count > 0,
                        "timestamp-enabled partial render must report encoded GPU passes",
                    );
                    let split_ns = timing
                        .gpu_timestamp_last_pass_ns
                        .saturating_add(timing.gpu_timestamp_other_pass_ns);
                    assert!(
                        split_ns.abs_diff(timing.gpu_timestamp_pass_ns)
                            <= u128::from(timing.gpu_timestamp_sampled_submissions),
                        "per-sample rounding is the only allowed difference between the split and total GPU pass time",
                    );
                }
                assert_eq!(
                    retained.damage_textures.first().map(|target| target.width),
                    Some(expected_capacity),
                );
                let full = reference.render_rgba_resilient(scene);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &full,
                    ),
                    0,
                    "geometric prefix capacity must retain full-render native pixels",
                );
            };

            // 64 -> 128 -> 256. The first update deliberately has unchanged
            // content: it exercises the first native-prefix allocation itself.
            assert_partial(&mut retained, &mut reference, &first, &[narrow], 64, 1);
            assert_partial(
                &mut retained,
                &mut reference,
                &second,
                &[narrow, middle],
                128,
                1,
            );
            assert_eq!(
                OffscreenRenderer::sample(&retained.readback_current(), W, 30, 30).3,
                0,
                "moving off the narrow rect must clear its transparent old pixels",
            );
            assert_partial(
                &mut retained,
                &mut reference,
                &third,
                &[middle, wide],
                256,
                1,
            );
            assert_eq!(
                OffscreenRenderer::sample(&retained.readback_current(), W, 96, 30).3,
                0,
                "moving off the middle rect must clear its transparent old pixels",
            );

            // 256 remains warm while moved content is cleared. Once only the
            // narrow prefix remains, fourfold slack permits a real 256 -> 64
            // shrink without leaving stale transparent pixels behind.
            assert_partial(
                &mut retained,
                &mut reference,
                &second,
                &[wide, middle],
                256,
                0,
            );
            assert_partial(
                &mut retained,
                &mut reference,
                &first,
                &[middle, narrow],
                256,
                0,
            );
            assert_eq!(
                OffscreenRenderer::sample(&retained.readback_current(), W, 96, 30).3,
                0,
                "the final narrow frame must not retain stale middle pixels",
            );
            assert_partial(&mut retained, &mut reference, &first, &[narrow], 64, 1);
        }
    }

    #[allow(
        clippy::items_after_statements,
        clippy::too_many_lines,
        reason = "local fixed dimensions document the second phase of this renderer oracle; end-to-end sparse renderer oracle with local fixed geometry; end-to-end sparse upload allocation and pixel oracle"
    )]
    #[test]
    fn sparse_coarse_rendering_matches_the_dense_oracle() {
        // Case `sparse_damage_masks_same_tile_and_handles_a_tall_native_prefix`.
        'sparse_damage_masks_same_tile_and_handles_a_tall_native_prefix: {
            const W: u32 = 256;
            const H: u32 = 192;
            let mut retained = OffscreenRenderer::new(64, 64);
            if !retained.vello().sparse_fine_supported() {
                let requested = env_enabled_by_default("VELLO_SPARSE_FINE");
                assert!(
                    !requested || std::env::var_os("TILO_UI_VELLO_CPU").is_some(),
                    "default sparse fine must be available unless the explicit CPU fallback is selected",
                );
                eprintln!("skipping sparse GPU mask test: sparse fine is unavailable");
                break 'sparse_damage_masks_same_tile_and_handles_a_tall_native_prefix;
            }
            let full_rect = Rect::new(0.0, 0.0, 64.0, 64.0);
            let mut blue = Scene::new();
            blue.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(20, 60, 220),
                None,
                &full_rect,
            );
            retained.render_rgba_resilient(&blue);
            let mut red = Scene::new();
            red.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(230, 40, 30),
                None,
                &full_rect,
            );
            let dirty = Rect::new(5.0, 7.0, 7.0, 9.0);
            let (pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &red,
                Color::TRANSPARENT,
                &[dirty],
            );
            assert!(pixels < 64 * 64);
            assert_eq!(timing.sparse_submissions, 1);
            assert!(timing.sparse_fine_tiles > 0);
            assert_eq!(timing.sparse_written_pixels, 4);
            assert!(retained.damage_textures.is_empty());
            let bytes = retained.readback_current();
            for y in 0..64 {
                for x in 0..64 {
                    let pixel = OffscreenRenderer::sample(&bytes, 64, x, y);
                    if (5..7).contains(&x) && (7..9).contains(&y) {
                        assert!(pixel.0 > pixel.2, "masked pixel must be red");
                    } else {
                        assert!(pixel.2 > pixel.0, "same-tile untouched pixel changed");
                    }
                }
            }

            let far = Rect::new(238.0, 174.0, 250.0, 186.0);
            let mut sparse = OffscreenRenderer::new(W, H);
            assert!(sparse.vello().sparse_fine_supported());
            sparse.render_rgba_resilient(&Scene::new());
            let mut changed = Scene::new();
            changed.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(80, 210, 120, 190),
                None,
                &far,
            );
            let (pixels, timing) = sparse.render_damage_regions_gpu_wait_profiled(
                &changed,
                Color::TRANSPARENT,
                &[far],
            );
            assert!(pixels < u64::from(W).saturating_mul(u64::from(H)).saturating_div(2));
            assert!(
                timing.coarse_prefix_pixels
                    >= u64::from(W).saturating_mul(u64::from(H)).saturating_div(2)
            );
            assert_eq!(timing.sparse_submissions, 1);
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);
            let full = oracle.render_rgba_resilient(&changed);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(&sparse.readback_current(), &full),
                0
            );
        }

        // Case `sparse_coarse_matches_dense_oracle_across_disjoint_masked_frames`.
        'sparse_coarse_matches_dense_oracle_across_disjoint_masked_frames: {
            if !matches!(std::env::var("VELLO_SPARSE_COARSE").as_deref(), Ok("1")) {
                eprintln!("skipping sparse coarse test: coarse selection is not requested");
                break 'sparse_coarse_matches_dense_oracle_across_disjoint_masked_frames;
            }
            let Ok(_gpu) = shared_gpu() else {
                eprintln!("skipping sparse coarse test: no shared GPU");
                break 'sparse_coarse_matches_dense_oracle_across_disjoint_masked_frames;
            };
            const W: u32 = 544;
            const H: u32 = 528;
            let mut retained = OffscreenRenderer::new(W, H);
            if !retained.vello().sparse_coarse_supported() {
                let sparse_fine_requested = env_enabled_by_default("VELLO_SPARSE_FINE");
                assert!(
                    !sparse_fine_requested || std::env::var_os("TILO_UI_VELLO_CPU").is_some(),
                    "coarse support is required only for the requested GPU sparse-fine path"
                );
                break 'sparse_coarse_matches_dense_oracle_across_disjoint_masked_frames;
            }
            assert!(retained.vello().sparse_coarse_supported());
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);

            let image = crate::raster::rgba8(
                [30_u8, 180, 250, 180]
                    .into_iter()
                    .cycle()
                    .take(4 * 4 * 4)
                    .collect::<Vec<_>>(),
                4,
                4,
            );
            let scene_for = |accent: Rect, image_rect: Rect| {
                let mut scene = Scene::new();
                let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
                let clip = Rect::new(12.0, 12.0, 532.0, 516.0);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(18, 28, 48),
                    None,
                    &full,
                );
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(240, 70, 50, 180),
                    None,
                    &accent,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    &ImageBrush::new(image.clone()),
                    Some(
                        Affine::translate((image_rect.x0, image_rect.y0))
                            * Affine::scale(image_rect.width() / 4.0),
                    ),
                    &image_rect,
                );
                scene.pop_layer();
                scene
            };
            let base = scene_for(
                Rect::new(20.0, 20.0, 20.0, 20.0),
                Rect::new(24.0, 24.0, 24.0, 24.0),
            );
            retained.render_rgba_resilient(&base);
            for (frame, (next, dirty)) in [
                (
                    scene_for(
                        Rect::new(28.0, 28.0, 52.0, 54.0),
                        Rect::new(34.0, 30.0, 46.0, 42.0),
                    ),
                    vec![
                        Rect::new(28.0, 28.0, 52.0, 54.0),
                        Rect::new(34.0, 30.0, 46.0, 42.0),
                    ],
                ),
                (
                    scene_for(
                        Rect::new(492.0, 490.0, 520.0, 518.0),
                        Rect::new(450.0, 450.0, 478.0, 478.0),
                    ),
                    vec![
                        Rect::new(28.0, 28.0, 52.0, 54.0),
                        Rect::new(34.0, 30.0, 46.0, 42.0),
                        Rect::new(492.0, 490.0, 520.0, 518.0),
                        Rect::new(450.0, 450.0, 478.0, 478.0),
                    ],
                ),
            ]
            .into_iter()
            .enumerate()
            {
                let before = retained.readback_current();
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &next,
                    Color::TRANSPARENT,
                    &dirty,
                );
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&next),
                    ),
                    0,
                    "sparse coarse output must equal the unbatched sparse-disabled oracle"
                );
                assert_eq!(
                    OffscreenRenderer::sample(&before, W, 150, 150),
                    OffscreenRenderer::sample(&retained.readback_current(), W, 150, 150),
                    "an untouched sample in the same coarse bin must remain unchanged"
                );
                if frame == 1 {
                    assert_eq!(
                        OffscreenRenderer::sample(&before, W, 150, 300),
                        OffscreenRenderer::sample(&retained.readback_current(), W, 150, 300),
                        "an empty coarse bin inside the far native prefix must remain untouched"
                    );
                }
                if matches!(
                    std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").as_deref(),
                    Ok("1")
                ) && retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0
                {
                    let coarse_sparse_bit = 1_u32.checked_shl(23).unwrap_or(0);
                    let dense_coarse_bit = 1_u32.checked_shl(16).unwrap_or(0);
                    assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_sampled_submissions,
                        timing.gpu_dispatch_timestamp_expected_submissions,
                    );
                    assert_ne!(
                        timing.gpu_dispatch_timestamp_present_stages & coarse_sparse_bit,
                        0
                    );
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_present_stages & dense_coarse_bit,
                        0
                    );
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                }
                let batched_reuse_profile =
                    !matches!(
                        std::env::var("VELLO_COMPUTE_BATCH").as_deref(),
                        Ok("0" | "1")
                    ) && env_enabled_by_default("VELLO_SPARSE_UPLOAD_REUSE");
                if batched_reuse_profile && retained.vello().gpu_timestamp_buffer_bytes() != 0 {
                    assert_eq!(timing.sparse_submissions, 1);
                    assert_eq!(timing.full_attempts, 0);
                    assert!(timing.gpu_timestamp_pass_count > 0);
                    assert!(
                        timing.gpu_timestamp_pass_count <= 3,
                        "the sparse upload must be recorded before the batched compute passes"
                    );
                    assert_eq!(
                        timing.gpu_timestamp_sampled_submissions,
                        timing.gpu_timestamp_expected_submissions,
                    );
                    assert_eq!(timing.gpu_timestamp_missing_submissions, 0);
                }
            }
        }

        // Case `adaptive_sparse_coarse_257_uses_dense_coarse_and_matches_dense`.
        'adaptive_sparse_coarse_257_uses_dense_coarse_and_matches_dense: {
            const W: u32 = 257;
            const H: u32 = 257;
            let Some(mut retained) =
                adaptive_sparse_coarse_renderer_or_skip(W, H, "adaptive sparse coarse 257 dense")
            else {
                break 'adaptive_sparse_coarse_257_uses_dense_coarse_and_matches_dense;
            };
            let mut oracle = dense_sparse_coarse_oracle(W, H);
            let before = (
                Rect::new(229.25, 17.5, 245.75, 37.25),
                Rect::new(17.0, 226.5, 43.0, 242.0),
                Rect::new(111.5, 113.25, 130.75, 130.5),
                Rect::new(244.0, 244.0, 250.0, 250.0),
            );
            retained.render_rgba_resilient(&active_sparse_coarse_257_scene(
                before.0, before.1, before.2, before.3,
            ));
            let after = (
                Rect::new(242.25, 17.5, 257.0, 41.75),
                Rect::new(16.5, 241.25, 47.75, 257.0),
                Rect::new(121.5, 120.5, 139.25, 137.0),
                Rect::new(252.25, 252.5, 257.0, 257.0),
            );
            let next = active_sparse_coarse_257_scene(after.0, after.1, after.2, after.3);
            let damage = [
                before.0.inflate(1.0, 1.0),
                before.1.inflate(1.0, 1.0),
                before.2.inflate(1.0, 1.0),
                before.3.inflate(1.0, 1.0),
                after.0.inflate(1.0, 1.0),
                after.1.inflate(1.0, 1.0),
                after.2.inflate(1.0, 1.0),
                after.3.inflate(1.0, 1.0),
            ];
            let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &next,
                Color::TRANSPARENT,
                &damage,
            );
            assert!(raster_pixels < u64::from(W).saturating_mul(u64::from(H)));
            assert_eq!(timing.sparse_submissions, 1);
            assert_eq!(timing.full_attempts, 0);
            assert_eq!(timing.sparse_coarse_adaptive_submissions, 1);
            assert_eq!(timing.sparse_coarse_adaptive_dense_submissions, 1);
            assert_eq!(timing.sparse_coarse_active_bins_submissions, 0);
            assert_eq!(
                timing.sparse_coarse_native_bins, 0,
                "small native targets must skip the adaptive selector packing precheck"
            );
            assert_eq!(timing.sparse_coarse_selected_native_bins, 0);
            let partial = retained.readback_current();
            let dense = oracle.render_rgba_resilient(&next);
            assert_exact_fixture_pixels(&partial, &dense, "adaptive sparse coarse 257", 0);
            assert_adaptive_coarse_timestamp(
                &mut retained,
                &timing,
                17,
                "adaptive sparse coarse 257",
            );
        }

        // Case `adaptive_sparse_coarse_2049_uses_active_native_bins_and_matches_dense`.
        'adaptive_sparse_coarse_2049_uses_active_native_bins_and_matches_dense: {
            const W: u32 = 2_049;
            const H: u32 = 2_049;
            let Some(mut retained) =
                adaptive_sparse_coarse_renderer_or_skip(W, H, "adaptive sparse coarse 2049 active")
            else {
                break 'adaptive_sparse_coarse_2049_uses_active_native_bins_and_matches_dense;
            };
            let mut oracle = dense_sparse_coarse_oracle(W, H);
            let before = (
                Rect::new(229.25, 17.5, 245.75, 37.25),
                Rect::new(17.0, 226.5, 43.0, 242.0),
                Rect::new(111.5, 113.25, 130.75, 130.5),
                Rect::new(244.0, 244.0, 250.0, 250.0),
            );
            retained.render_rgba_resilient(&adaptive_sparse_coarse_2049_scene(
                before.0, before.1, before.2, before.3,
            ));
            let after = (
                Rect::new(242.25, 17.5, 257.0, 41.75),
                Rect::new(16.5, 241.25, 47.75, 257.0),
                Rect::new(121.5, 120.5, 139.25, 137.0),
                Rect::new(252.25, 252.5, 257.0, 257.0),
            );
            let next = adaptive_sparse_coarse_2049_scene(after.0, after.1, after.2, after.3);
            let damage = [
                before.0, before.1, before.2, before.3, after.0, after.1, after.2, after.3,
            ]
            .map(|rect| translated_damage(rect.inflate(1.0, 1.0)));
            let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &next,
                Color::TRANSPARENT,
                &damage,
            );
            assert!(raster_pixels < u64::from(W).saturating_mul(u64::from(H)));
            assert_eq!(timing.sparse_submissions, 1);
            assert_eq!(timing.full_attempts, 0);
            assert_eq!(timing.sparse_coarse_adaptive_submissions, 1);
            assert_eq!(timing.sparse_coarse_adaptive_dense_submissions, 0);
            assert_eq!(timing.sparse_coarse_active_bins_submissions, 1);
            assert_eq!(timing.sparse_coarse_native_bins, 81);
            assert_eq!(timing.sparse_coarse_selected_native_bins, 4);
            assert_eq!(
                timing.coarse_prefix_pixels,
                u64::from(W).saturating_mul(u64::from(H)),
                "the translated edge damage must retain the real 2049px native prefix"
            );
            let partial = retained.readback_current();
            let dense = oracle.render_rgba_resilient(&next);
            assert_exact_fixture_pixels(&partial, &dense, "adaptive sparse coarse 2049", 0);
            assert_adaptive_coarse_timestamp(
                &mut retained,
                &timing,
                28,
                "adaptive sparse coarse 2049",
            );
        }

        // Case `active_sparse_coarse_257_native_edges_clips_and_translucency_match_dense`.
        'active_sparse_coarse_257_native_edges_clips_and_translucency_match_dense: {
            const W: u32 = 257;
            const H: u32 = 257;
            let Some(mut retained) =
                active_sparse_coarse_renderer_or_skip(W, H, "active sparse coarse 257 parity")
            else {
                break 'active_sparse_coarse_257_native_edges_clips_and_translucency_match_dense;
            };
            let mut oracle = dense_sparse_coarse_oracle(W, H);
            let right_a = Rect::new(229.25, 17.5, 245.75, 37.25);
            let bottom_a = Rect::new(17.0, 226.5, 43.0, 242.0);
            let crossing_a = Rect::new(111.5, 113.25, 130.75, 130.5);
            let corner_a = Rect::new(244.0, 244.0, 250.0, 250.0);
            retained.render_rgba_resilient(&active_sparse_coarse_257_scene(
                right_a, bottom_a, crossing_a, corner_a,
            ));

            let right_b = Rect::new(242.25, 17.5, 257.0, 41.75);
            let bottom_b = Rect::new(16.5, 241.25, 47.75, 257.0);
            let crossing_b = Rect::new(121.5, 120.5, 139.25, 137.0);
            let corner_b = Rect::new(252.25, 252.5, 257.0, 257.0);
            let right_c = Rect::new(236.5, 39.25, 257.0, 58.5);
            let bottom_c = Rect::new(43.5, 244.0, 66.0, 257.0);
            let crossing_c = Rect::new(126.25, 109.75, 146.5, 129.25);
            let corner_c = Rect::new(254.5, 251.0, 257.0, 257.0);
            let frames = [
                (
                    active_sparse_coarse_257_scene(right_b, bottom_b, crossing_b, corner_b),
                    vec![
                        right_a.inflate(1.0, 1.0),
                        bottom_a.inflate(1.0, 1.0),
                        crossing_a.inflate(1.0, 1.0),
                        corner_a.inflate(1.0, 1.0),
                        right_b.inflate(1.0, 1.0),
                        bottom_b.inflate(1.0, 1.0),
                        crossing_b.inflate(1.0, 1.0),
                        corner_b.inflate(1.0, 1.0),
                    ],
                ),
                (
                    active_sparse_coarse_257_scene(right_c, bottom_c, crossing_c, corner_c),
                    vec![
                        right_b.inflate(1.0, 1.0),
                        bottom_b.inflate(1.0, 1.0),
                        crossing_b.inflate(1.0, 1.0),
                        corner_b.inflate(1.0, 1.0),
                        right_c.inflate(1.0, 1.0),
                        bottom_c.inflate(1.0, 1.0),
                        crossing_c.inflate(1.0, 1.0),
                        corner_c.inflate(1.0, 1.0),
                    ],
                ),
            ];
            for (frame, (next, damage)) in frames.into_iter().enumerate() {
                let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &next,
                    Color::TRANSPARENT,
                    &damage,
                );
                assert!(raster_pixels < u64::from(W).saturating_mul(u64::from(H)));
                assert_eq!(timing.sparse_submissions, 1);
                assert_eq!(timing.full_attempts, 0);
                assert_eq!(
                    timing.sparse_coarse_active_bins_submissions, 1,
                    "frame {frame} must use the active native-bin permutation, not only expose it"
                );
                let partial = retained.readback_current();
                let dense = oracle.render_rgba_resilient(&next);
                assert_exact_fixture_pixels(&partial, &dense, "active sparse coarse 257", frame);
                if matches!(
                    std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").as_deref(),
                    Ok("1")
                ) && retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0
                {
                    let active_coarse = 1_u32.checked_shl(27).expect("stage id 28 fits mask");
                    assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_sampled_submissions,
                        timing.gpu_dispatch_timestamp_expected_submissions,
                    );
                    assert_ne!(
                        timing.gpu_dispatch_timestamp_present_stages & active_coarse,
                        0
                    );
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                }
            }
        }

        // Case `active_sparse_coarse_overflow_recovers_dense_before_publication`.
        'active_sparse_coarse_overflow_recovers_dense_before_publication: {
            const W: u32 = 384;
            const H: u32 = 256;
            let Some(mut retained) = active_sparse_coarse_renderer_or_skip(
                W,
                H,
                "active sparse coarse overflow recovery",
            ) else {
                break 'active_sparse_coarse_overflow_recovers_dense_before_publication;
            };
            let mut oracle = dense_sparse_coarse_oracle(W, H);
            let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
            // This 16×16 bin-1 accent keeps the copied damage small while its native-origin prefix
            // is 368×240. The 512 same-blue full-target fills therefore exceed Vello's initial tile
            // reservation before the accent can be published.
            let accent = Rect::new(352.0, 224.0, 368.0, 240.0);
            let blue = Color::from_rgb8(24, 96, 188);
            let mut seed = Scene::new();
            seed.fill(Fill::NonZero, Affine::IDENTITY, blue, None, &full);
            retained.render_rgba_resilient(&seed);

            let mut overflowing = Scene::new();
            for _ in 0..512_u32 {
                overflowing.fill(Fill::NonZero, Affine::IDENTITY, blue, None, &full);
            }
            overflowing.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgb8(230, 76, 48),
                None,
                &accent,
            );
            let (raster_pixels, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &overflowing,
                Color::TRANSPARENT,
                &[accent],
            );
            assert!(raster_pixels >= u64::from(W).saturating_mul(u64::from(H)));
            assert_eq!(timing.sparse_submissions, 1);
            assert_eq!(timing.sparse_coarse_active_bins_submissions, 1);
            assert!(
                timing.full_attempts >= 1,
                "the actual sparse active-bin submission must overflow and recover densely"
            );
            let partial = retained.readback_current();
            let dense = oracle.render_rgba_resilient(&overflowing);
            assert_exact_fixture_pixels(
                &partial,
                &dense,
                "active sparse coarse overflow recovery",
                0,
            );
        }

        // Case `sparse_upload_reuse_grows_once_and_matches_an_unbatched_dense_oracle`.
        'sparse_upload_reuse_grows_once_and_matches_an_unbatched_dense_oracle: {
            if !env_enabled_by_default("VELLO_SPARSE_UPLOAD_REUSE") {
                eprintln!("skipping sparse upload reuse test: reuse is not requested");
                break 'sparse_upload_reuse_grows_once_and_matches_an_unbatched_dense_oracle;
            }

            const W: u32 = 128;
            const H: u32 = 96;
            let mut retained = OffscreenRenderer::new(W, H);
            if !retained.vello().sparse_fine_supported()
                || !retained.vello().sparse_upload_reuse_enabled()
            {
                eprintln!("skipping sparse upload reuse test: sparse GPU path is unavailable");
                break 'sparse_upload_reuse_grows_once_and_matches_an_unbatched_dense_oracle;
            }
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);

            let scene_for = |rect: Rect| {
                let mut scene = Scene::new();
                let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(20, 60, 220),
                    None,
                    &full,
                );
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(230, 40, 30),
                    None,
                    &rect,
                );
                scene
            };
            let base = scene_for(Rect::new(0.0, 0.0, 0.0, 0.0));
            retained.render_rgba_resilient(&base);

            let small = Rect::new(5.0, 7.0, 7.0, 9.0);
            let small_scene = scene_for(small);
            let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &small_scene,
                Color::TRANSPARENT,
                &[small],
            );
            let deferred_bump_profile = retained.vello().defer_bump_readback_enabled()
                && matches!(std::env::var("VELLO_COMPUTE_BATCH").as_deref(), Ok("2"))
                && retained.vello().cpu_metadata_enabled()
                && retained.vello().gpu_timestamp_buffer_bytes() != 0;
            if deferred_bump_profile {
                assert_eq!(
                    timing.gpu_timestamp_pass_count, 1,
                    "persistent sparse upload plus the deferred bump copy must retain one compute pass"
                );
                assert_eq!(
                    timing.gpu_timestamp_sampled_submissions,
                    timing.gpu_timestamp_expected_submissions,
                );
                assert_eq!(timing.gpu_timestamp_missing_submissions, 0);
            }
            let dispatch_requested = matches!(
                std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").as_deref(),
                Ok("1")
            );
            if dispatch_requested && retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
                assert_eq!(
                    timing.gpu_dispatch_timestamp_sampled_submissions,
                    timing.gpu_dispatch_timestamp_expected_submissions,
                );
                assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                let sparse_stage_bit = 1u32 << 22;
                assert_ne!(
                    timing.gpu_dispatch_timestamp_present_stages & sparse_stage_bit,
                    0
                );
                assert!(timing.gpu_dispatch_timestamp_stage_duration_ns[22] > 0);
                assert!(timing.gpu_dispatch_timestamp_stage_dispatch_count[22] > 0);
            } else if dispatch_requested {
                assert!(matches!(
                    retained.vello_mut().gpu_dispatch_timestamp_status(),
                    vello::GpuDispatchTimestampStatus::Unsupported
                        | vello::GpuDispatchTimestampStatus::Disabled
                ));
            }
            let first_memory = retained.memory_stats();
            let first_upload = first_memory.vello.sparse_upload_bytes;
            assert!(
                first_upload > 0,
                "reuse must retain the first tile upload buffer"
            );
            assert!(first_memory.known_gpu_bytes() >= first_upload);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&small_scene),
                ),
                0,
                "reused sparse upload must equal the unbatched dense reference",
            );

            retained.render_rgba_resilient(&base);
            retained.render_damage_regions_gpu_wait(&small_scene, Color::TRANSPARENT, &[small]);
            assert_eq!(
                retained.memory_stats().vello.sparse_upload_bytes,
                first_upload,
                "same tile list must reuse its persistent allocation",
            );

            retained.render_rgba_resilient(&base);
            let grown = Rect::new(40.0, 20.0, 89.0, 69.0);
            let grown_scene = scene_for(grown);
            retained.render_damage_regions_gpu_wait(&grown_scene, Color::TRANSPARENT, &[grown]);
            let grown_memory = retained.memory_stats();
            assert!(
                grown_memory.vello.sparse_upload_bytes > first_upload,
                "a larger tile list must replace the bounded persistent upload",
            );
            assert!(grown_memory.known_gpu_bytes() >= grown_memory.vello.sparse_upload_bytes);
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&grown_scene),
                ),
                0,
                "grown reused upload must equal the unbatched dense reference",
            );
        }
    }

    fn active_sparse_coarse_renderer_or_skip(
        width: u32,
        height: u32,
        test_name: &str,
    ) -> Option<OffscreenRenderer> {
        let coarse_requested = matches!(std::env::var("VELLO_SPARSE_COARSE").as_deref(), Ok("1"));
        let active_requested = matches!(
            std::env::var("VELLO_SPARSE_COARSE_ACTIVE_BINS").as_deref(),
            Ok("1")
        );
        if !coarse_requested || !active_requested {
            eprintln!(
                "skipping {test_name}: VELLO_SPARSE_COARSE=1 and VELLO_SPARSE_COARSE_ACTIVE_BINS=1 are required"
            );
            return None;
        }
        if shared_gpu().is_err() {
            eprintln!("skipping {test_name}: no shared GPU");
            return None;
        }
        let renderer = OffscreenRenderer::new(width, height);
        if !renderer.vello().sparse_coarse_active_bins_available() {
            eprintln!("skipping {test_name}: active native-bin coarse is unavailable");
            return None;
        }
        Some(renderer)
    }

    fn adaptive_sparse_coarse_renderer_or_skip(
        width: u32,
        height: u32,
        test_name: &str,
    ) -> Option<OffscreenRenderer> {
        let auto_requested = matches!(
            std::env::var("VELLO_SPARSE_COARSE_AUTO").as_deref(),
            Ok("1")
        );
        let forced_mode = std::env::var_os("VELLO_SPARSE_COARSE").is_some()
            || std::env::var_os("VELLO_SPARSE_COARSE_ACTIVE_BINS").is_some();
        if !auto_requested || forced_mode {
            eprintln!(
                "skipping {test_name}: VELLO_SPARSE_COARSE_AUTO=1 with the legacy coarse flags unset is required"
            );
            return None;
        }
        if shared_gpu().is_err() {
            eprintln!("skipping {test_name}: no shared GPU");
            return None;
        }
        let renderer = OffscreenRenderer::new(width, height);
        if !renderer.vello().sparse_coarse_adaptive_enabled() {
            eprintln!("skipping {test_name}: adaptive sparse coarse is unavailable");
            return None;
        }
        Some(renderer)
    }

    fn dense_sparse_coarse_oracle(width: u32, height: u32) -> OffscreenRenderer {
        let mut oracle = OffscreenRenderer::new(width, height);
        oracle.set_compute_pass_batching(false);
        oracle.set_direct_path_dispatch_enabled(false);
        oracle.set_defer_bump_readback_enabled(false);
        oracle.set_sparse_fine_enabled(false);
        oracle.set_cpu_metadata_enabled(false);
        oracle.set_cpu_pathtag_enabled(false);
        oracle.set_cpu_draw_prefix_enabled(false);
        oracle
    }

    fn assert_exact_fixture_pixels(actual: &[u8], expected: &[u8], fixture: &str, frame: usize) {
        assert_eq!(
            actual.len(),
            expected.len(),
            "{fixture} frame {frame} changed the pixel buffer length"
        );
        let first_mismatch = actual
            .iter()
            .zip(expected)
            .position(|(actual, expected)| actual != expected);
        assert_eq!(
            first_mismatch, None,
            "{fixture} frame {frame} must be byte-identical to its dense oracle"
        );
    }

    fn assert_adaptive_coarse_timestamp(
        renderer: &mut OffscreenRenderer,
        timing: &OffscreenRenderTiming,
        expected_stage: u32,
        fixture: &str,
    ) {
        if !matches!(
            std::env::var("VELLO_GPU_DISPATCH_TIMESTAMPS").as_deref(),
            Ok("1")
        ) {
            return;
        }
        if renderer.vello().gpu_dispatch_timestamp_buffer_bytes() == 0 {
            assert!(matches!(
                renderer.vello_mut().gpu_dispatch_timestamp_status(),
                vello::GpuDispatchTimestampStatus::Unsupported
                    | vello::GpuDispatchTimestampStatus::Disabled
            ));
            return;
        }
        let expected = 1_u32
            .checked_shl(expected_stage.saturating_sub(1))
            .expect("coarse stage id must fit the timestamp mask");
        let legacy = 1_u32.checked_shl(23).expect("stage id 24 fits mask");
        let dense = 1_u32.checked_shl(16).expect("stage id 17 fits mask");
        let active = 1_u32.checked_shl(27).expect("stage id 28 fits mask");
        assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
        assert_eq!(
            timing.gpu_dispatch_timestamp_sampled_submissions,
            timing.gpu_dispatch_timestamp_expected_submissions,
            "{fixture} must match every dispatch timestamp submission"
        );
        assert_ne!(
            timing.gpu_dispatch_timestamp_present_stages & expected,
            0,
            "{fixture} must encode its selected coarse stage"
        );
        assert_eq!(
            timing.gpu_dispatch_timestamp_present_stages & legacy,
            0,
            "{fixture} must not select legacy sparse coarse"
        );
        if expected_stage == 17 {
            assert_eq!(timing.gpu_dispatch_timestamp_present_stages & active, 0);
        } else {
            assert_eq!(timing.gpu_dispatch_timestamp_present_stages & dense, 0);
        }
        assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
        assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
    }

    fn active_sparse_coarse_257_scene(
        right: Rect,
        bottom: Rect,
        crossing: Rect,
        corner: Rect,
    ) -> Scene {
        const W: u32 = 257;
        const H: u32 = 257;
        let mut scene = Scene::new();
        let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
        let outer_clip = Rect::new(4.0, 5.0, f64::from(W), f64::from(H));
        let inner_clip = Rect::new(9.0, 11.0, f64::from(W), f64::from(H));
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(17, 27, 43),
            None,
            &full,
        );
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &outer_clip);
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &inner_clip);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(240, 78, 54, 178),
            None,
            &right,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(52, 184, 244, 151),
            None,
            &bottom,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(160, 84, 236, 133),
            None,
            &crossing,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(84, 228, 166, 197),
            None,
            &corner,
        );
        scene.pop_layer();
        scene.pop_layer();
        scene
    }

    fn adaptive_sparse_coarse_2049_scene(
        right: Rect,
        bottom: Rect,
        crossing: Rect,
        corner: Rect,
    ) -> Scene {
        const W: u32 = 2_049;
        const H: u32 = 2_049;
        const OFFSET: f64 = 1_792.0;
        let mut scene = Scene::new();
        let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
        let translate = Affine::translate((OFFSET, OFFSET));
        let outer_clip = Rect::new(4.0, 5.0, 257.0, 257.0);
        let inner_clip = Rect::new(9.0, 11.0, 257.0, 257.0);
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgb8(17, 27, 43),
            None,
            &full,
        );
        scene.push_clip_layer(Fill::NonZero, translate, &outer_clip);
        scene.push_clip_layer(Fill::NonZero, translate, &inner_clip);
        scene.fill(
            Fill::NonZero,
            translate,
            Color::from_rgba8(240, 78, 54, 178),
            None,
            &right,
        );
        scene.fill(
            Fill::NonZero,
            translate,
            Color::from_rgba8(52, 184, 244, 151),
            None,
            &bottom,
        );
        scene.fill(
            Fill::NonZero,
            translate,
            Color::from_rgba8(160, 84, 236, 133),
            None,
            &crossing,
        );
        scene.fill(
            Fill::NonZero,
            translate,
            Color::from_rgba8(84, 228, 166, 197),
            None,
            &corner,
        );
        scene.pop_layer();
        scene.pop_layer();
        scene
    }

    fn translated_damage(rect: Rect) -> Rect {
        Rect::new(
            rect.x0 + 1_792.0,
            rect.y0 + 1_792.0,
            rect.x1 + 1_792.0,
            rect.y1 + 1_792.0,
        )
    }

    #[allow(
        clippy::too_many_lines,
        clippy::items_after_statements,
        reason = "end-to-end CPU/GPU path equivalence oracle with local fixed geometry; end-to-end direct/indirect renderer equivalence oracle; local fixed dimensions document the renderer recovery geometry"
    )]
    #[test]
    fn gpu_path_dispatch_matches_its_oracles_and_recovers_overflow() {
        // Case `cpu_pathtag_matches_gpu_oracle_and_restores_gpu_stages_when_disabled`.
        'cpu_pathtag_matches_gpu_oracle_and_restores_gpu_stages_when_disabled: {
            if !env_enabled_by_default("VELLO_CPU_PATHTAG") {
                eprintln!("skipping CPU path-tag test: selective path tags are not requested");
                break 'cpu_pathtag_matches_gpu_oracle_and_restores_gpu_stages_when_disabled;
            }
            let Ok(_gpu) = shared_gpu() else {
                eprintln!("skipping CPU path-tag test: no shared GPU");
                break 'cpu_pathtag_matches_gpu_oracle_and_restores_gpu_stages_when_disabled;
            };
            const W: u32 = 384;
            const H: u32 = 256;
            let mut retained = OffscreenRenderer::new(W, H);
            if !retained.vello().cpu_pathtag_enabled() {
                assert!(
                    std::env::var_os("TILO_UI_VELLO_CPU").is_some(),
                    "default selective CPU path tags must activate on a GPU renderer"
                );
                break 'cpu_pathtag_matches_gpu_oracle_and_restores_gpu_stages_when_disabled;
            }
            let draw_prefix_requested = env_enabled_by_default("VELLO_CPU_DRAW_PREFIX");
            let direct_path_requested = env_enabled_by_default("VELLO_DIRECT_PATH_DISPATCH");
            if direct_path_requested {
                assert!(
                    retained.vello().direct_path_dispatch_enabled(),
                    "default direct GPU path dispatch must activate"
                );
            }
            if draw_prefix_requested {
                assert!(retained.vello().cpu_draw_prefix_enabled());
                assert!(retained.cpu_draw_prefix_supported(16));
                assert!(retained.cpu_draw_prefix_supported(1024 * 1024));
                assert!(!retained.cpu_draw_prefix_supported(1024 * 1024 + 16));
            }
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_direct_path_dispatch_enabled(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);
            oracle.set_cpu_metadata_enabled(false);
            oracle.set_cpu_pathtag_enabled(false);
            oracle.set_cpu_draw_prefix_enabled(false);
            let image = crate::raster::rgba8(
                [30_u8, 170, 240, 180]
                    .into_iter()
                    .cycle()
                    .take(64)
                    .collect::<Vec<_>>(),
                4,
                4,
            );
            let scene_for = |offset: f64, add_tag: bool| {
                let mut scene = Scene::new();
                let clip = Rect::new(12.0, 10.0, 180.0, 150.0);
                let mut curve = BezPath::new();
                curve.move_to((20.0, 118.0));
                curve.curve_to((42.0, 12.0), (118.0, 150.0), (158.0, 34.0));
                curve.line_to((132.0, 132.0));
                curve.close_path();
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                scene.fill(
                    Fill::NonZero,
                    Affine::translate((offset, 0.0)),
                    Color::from_rgba8(235, 70, 45, 185),
                    None,
                    &curve,
                );
                if add_tag {
                    scene.fill(
                        Fill::NonZero,
                        Affine::IDENTITY,
                        Color::from_rgba8(80, 220, 120, 170),
                        None,
                        &Rect::new(48.0, 64.0, 82.0, 96.0),
                    );
                }
                let image_rect = Rect::new(96.0 + offset, 46.0, 124.0 + offset, 74.0);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    &ImageBrush::new(image.clone()),
                    Some(
                        Affine::translate((image_rect.x0, image_rect.y0))
                            * Affine::scale(image_rect.width() / 4.0),
                    ),
                    &image_rect,
                );
                scene.pop_layer();
                scene
            };
            let initial = scene_for(0.0, false);
            retained.render_rgba_resilient(&initial);
            let bounded_damage = Rect::new(12.0, 12.0, 184.0, 150.0);
            let reuse_requested = env_enabled_by_default("VELLO_CPU_PATHTAG_REUSE");
            let deferred_bump_profile = retained.vello().defer_bump_readback_enabled()
                && matches!(std::env::var("VELLO_COMPUTE_BATCH").as_deref(), Ok("2"))
                && !retained.vello().sparse_fine_supported()
                && retained.vello().gpu_timestamp_buffer_bytes() != 0;
            let mut retained_pool_bytes = 0;
            for next in [scene_for(9.0, false), scene_for(9.0, true)] {
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &next,
                    Color::TRANSPARENT,
                    &[bounded_damage],
                );
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&next),
                    ),
                    0
                );
                if reuse_requested {
                    assert!(retained.vello().cpu_pathtag_reuse_enabled());
                    let current_pool_bytes = retained.vello().cpu_pathtag_pool_retained_bytes();
                    assert!(current_pool_bytes > 0);
                    assert!(current_pool_bytes >= retained_pool_bytes);
                    retained_pool_bytes = current_pool_bytes;
                }
                if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                    assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_sampled_submissions,
                        timing.gpu_dispatch_timestamp_expected_submissions,
                    );
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                    let reduce = 1_u32.checked_shl(0).unwrap_or(0);
                    let scan_small = 1_u32.checked_shl(3).unwrap_or(0);
                    let flatten = 1_u32.checked_shl(6).unwrap_or(0);
                    let sparse_fine_requested = env_enabled_by_default("VELLO_SPARSE_FINE");
                    let fine = 1_u32
                        .checked_shl(if sparse_fine_requested { 22 } else { 19 })
                        .unwrap_or(0);
                    assert_eq!(timing.gpu_dispatch_timestamp_present_stages & reduce, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_present_stages & scan_small, 0);
                    if retained.vello().cpu_metadata_enabled() {
                        let bbox_clear = 1_u32.checked_shl(5).unwrap_or(0);
                        let draw_reduce = 1_u32.checked_shl(7).unwrap_or(0);
                        assert_eq!(timing.gpu_dispatch_timestamp_present_stages & bbox_clear, 0);
                        assert_eq!(
                            timing.gpu_dispatch_timestamp_present_stages & draw_reduce,
                            0
                        );
                    }
                    if retained.vello().cpu_draw_prefix_enabled() {
                        let prefix_leaf = 1_u32.checked_shl(24).unwrap_or(0);
                        let draw_leaf = 1_u32.checked_shl(8).unwrap_or(0);
                        assert_ne!(
                            timing.gpu_dispatch_timestamp_present_stages & prefix_leaf,
                            0
                        );
                        assert_eq!(timing.gpu_dispatch_timestamp_present_stages & draw_leaf, 0);
                    }
                    assert_ne!(timing.gpu_dispatch_timestamp_present_stages & flatten, 0);
                    assert_ne!(timing.gpu_dispatch_timestamp_present_stages & fine, 0);
                    let path_count_direct = 1_u32.checked_shl(25).unwrap_or(0);
                    let path_tiling_direct = 1_u32.checked_shl(26).unwrap_or(0);
                    let path_count_setup = 1_u32.checked_shl(13).unwrap_or(0);
                    let path_count = 1_u32.checked_shl(14).unwrap_or(0);
                    let path_tiling_setup = 1_u32.checked_shl(17).unwrap_or(0);
                    let path_tiling = 1_u32.checked_shl(18).unwrap_or(0);
                    if retained.vello().direct_path_dispatch_enabled() {
                        assert_ne!(
                            timing.gpu_dispatch_timestamp_present_stages & path_count_direct,
                            0
                        );
                        assert_ne!(
                            timing.gpu_dispatch_timestamp_present_stages & path_tiling_direct,
                            0
                        );
                        assert_eq!(
                            timing.gpu_dispatch_timestamp_present_stages
                                & (path_count_setup | path_count | path_tiling_setup | path_tiling),
                            0
                        );
                    }
                }
                if deferred_bump_profile {
                    let expected_pass_count = if retained.vello().cpu_metadata_enabled() {
                        1
                    } else {
                        // GPU bbox-clear precedes the bump clear and remains
                        // a separate pass when CPU metadata is disabled.
                        2
                    };
                    assert_eq!(
                        timing.gpu_timestamp_pass_count, expected_pass_count,
                        "the deferred synchronous bump copy must not add a compute-pass split"
                    );
                    assert_eq!(
                        timing.gpu_timestamp_sampled_submissions,
                        timing.gpu_timestamp_expected_submissions,
                    );
                    assert_eq!(timing.gpu_timestamp_missing_submissions, 0);
                }
            }
            if direct_path_requested {
                retained.set_direct_path_dispatch_enabled(false);
                let indirect_scene = scene_for(10.5, true);
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &indirect_scene,
                    Color::TRANSPARENT,
                    &[bounded_damage],
                );
                assert!(!retained.vello().direct_path_dispatch_enabled());
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&indirect_scene),
                    ),
                    0
                );
                if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                    let path_count_direct = 1_u32.checked_shl(25).unwrap_or(0);
                    let path_tiling_direct = 1_u32.checked_shl(26).unwrap_or(0);
                    let path_count_setup = 1_u32.checked_shl(13).unwrap_or(0);
                    let path_count = 1_u32.checked_shl(14).unwrap_or(0);
                    let path_tiling_setup = 1_u32.checked_shl(17).unwrap_or(0);
                    let path_tiling = 1_u32.checked_shl(18).unwrap_or(0);
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_present_stages
                            & (path_count_direct | path_tiling_direct),
                        0
                    );
                    assert_ne!(
                        timing.gpu_dispatch_timestamp_present_stages
                            & (path_count_setup | path_count | path_tiling_setup | path_tiling),
                        0
                    );
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                }
                retained.set_direct_path_dispatch_enabled(true);
            }
            if deferred_bump_profile {
                retained.set_defer_bump_readback_enabled(false);
                let restored_scene = scene_for(10.0, true);
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &restored_scene,
                    Color::TRANSPARENT,
                    &[bounded_damage],
                );
                assert!(!retained.vello().defer_bump_readback_enabled());
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&restored_scene),
                    ),
                    0
                );
                assert!(
                    timing.gpu_timestamp_pass_count >= 2,
                    "the original synchronous bump copy must restore its compute-pass boundary"
                );
                assert_eq!(
                    timing.gpu_timestamp_sampled_submissions,
                    timing.gpu_timestamp_expected_submissions,
                );
                assert_eq!(timing.gpu_timestamp_missing_submissions, 0);
                retained.set_defer_bump_readback_enabled(true);
            }
            if draw_prefix_requested {
                let prefix_pool = retained.vello().cpu_draw_prefix_pool_retained_bytes();
                assert!(prefix_pool > 0 && retained.cpu_draw_prefix_supported(prefix_pool));
                retained.set_cpu_draw_prefix_enabled(false);
                let prefix_gpu_scene = scene_for(11.0, true);
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &prefix_gpu_scene,
                    Color::TRANSPARENT,
                    &[bounded_damage],
                );
                assert!(retained.vello().cpu_pathtag_enabled());
                assert!(!retained.vello().cpu_draw_prefix_enabled());
                assert_eq!(retained.vello().cpu_draw_prefix_pool_retained_bytes(), 0);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&prefix_gpu_scene),
                    ),
                    0
                );
                if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                    let prefix_leaf = 1_u32.checked_shl(24).unwrap_or(0);
                    let draw_leaf = 1_u32.checked_shl(8).unwrap_or(0);
                    assert_eq!(
                        timing.gpu_dispatch_timestamp_present_stages & prefix_leaf,
                        0
                    );
                    assert_ne!(timing.gpu_dispatch_timestamp_present_stages & draw_leaf, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                }
            }
            if env_enabled_by_default("VELLO_CPU_METADATA") {
                assert!(retained.vello().cpu_metadata_enabled());
                retained.set_cpu_metadata_enabled(false);
                let metadata_gpu_scene = scene_for(12.0, true);
                let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                    &metadata_gpu_scene,
                    Color::TRANSPARENT,
                    &[bounded_damage],
                );
                assert!(retained.vello().cpu_pathtag_enabled());
                assert!(!retained.vello().cpu_metadata_enabled());
                assert_eq!(retained.vello().cpu_metadata_pool_retained_bytes(), 0);
                assert_eq!(
                    OffscreenRenderer::pixels_differing_beyond_aa(
                        &retained.readback_current(),
                        &oracle.render_rgba_resilient(&metadata_gpu_scene),
                    ),
                    0
                );
                if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                    let bbox_clear = 1_u32.checked_shl(5).unwrap_or(0);
                    let draw_reduce = 1_u32.checked_shl(7).unwrap_or(0);
                    assert_ne!(timing.gpu_dispatch_timestamp_present_stages & bbox_clear, 0);
                    assert_ne!(
                        timing.gpu_dispatch_timestamp_present_stages & draw_reduce,
                        0
                    );
                    assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                    assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                }
            }
            retained.set_cpu_pathtag_enabled(false);
            let gpu_scene = scene_for(14.0, true);
            let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &gpu_scene,
                Color::TRANSPARENT,
                &[bounded_damage],
            );
            assert!(!retained.vello().cpu_pathtag_enabled());
            if reuse_requested {
                assert_eq!(retained.vello().cpu_pathtag_pool_retained_bytes(), 0);
            }
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&gpu_scene),
                ),
                0
            );
            if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                assert!(timing.gpu_dispatch_timestamp_expected_submissions > 0);
                assert_eq!(
                    timing.gpu_dispatch_timestamp_sampled_submissions,
                    timing.gpu_dispatch_timestamp_expected_submissions,
                );
                assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                let reduce = 1_u32.checked_shl(0).unwrap_or(0);
                let scan_small = 1_u32.checked_shl(3).unwrap_or(0);
                assert_ne!(timing.gpu_dispatch_timestamp_present_stages & reduce, 0);
                assert_ne!(timing.gpu_dispatch_timestamp_present_stages & scan_small, 0);
            }
        }

        // Case `direct_path_dispatch_matches_an_indirect_gpu_oracle_and_restores_stages`.
        'direct_path_dispatch_matches_an_indirect_gpu_oracle_and_restores_stages: {
            if !env_enabled_by_default("VELLO_DIRECT_PATH_DISPATCH") {
                eprintln!("skipping direct path-dispatch test: experiment is not requested");
                break 'direct_path_dispatch_matches_an_indirect_gpu_oracle_and_restores_stages;
            }
            let Ok(_gpu) = shared_gpu() else {
                eprintln!("skipping direct path-dispatch test: no shared GPU");
                break 'direct_path_dispatch_matches_an_indirect_gpu_oracle_and_restores_stages;
            };
            const W: u32 = 384;
            const H: u32 = 256;
            let mut retained = OffscreenRenderer::new(W, H);
            if !retained.vello().direct_path_dispatch_enabled() {
                assert!(
                    std::env::var_os("TILO_UI_VELLO_CPU").is_some(),
                    "default direct GPU path dispatch must activate"
                );
                break 'direct_path_dispatch_matches_an_indirect_gpu_oracle_and_restores_stages;
            }
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_direct_path_dispatch_enabled(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);

            let empty = Scene::new();
            let empty_pixels = retained.render_rgba_resilient(&empty);
            assert!(
                empty_pixels
                    .chunks_exact(4)
                    .all(|pixel| pixel.get(3).copied().unwrap_or_default() == 0),
                "the fixed direct grid must preserve an empty scene's success sentinel"
            );
            let mut stride_scene = Scene::new();
            // Non-overlapping quarter-pixel rectangles are fully visible and stay
            // within one 16px tile. Each contributes four lines and segments, so
            // 8,193 * 4 = 32,772 crosses both direct-grid loops without hidden
            // later-stride geometry or tile-boundary culling.
            for i in 0_u32..8_193 {
                let x = f64::from((i % 128).saturating_mul(2)) + 0.25;
                let y = f64::from((i / 128).saturating_mul(2)) + 0.25;
                let color = if i == 8_192 {
                    Color::from_rgb8(250, 220, 35)
                } else if i % 2 == 0 {
                    Color::from_rgb8(45, 115, 215)
                } else {
                    Color::from_rgb8(190, 55, 125)
                };
                stride_scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    color,
                    None,
                    &Rect::new(x, y, x + 1.0, y + 1.0),
                );
            }
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.render_rgba_resilient(&stride_scene),
                    &oracle.render_rgba_resilient(&stride_scene),
                ),
                0,
                "direct path-count/tiling must retain later grid-stride iterations"
            );
            let scene_for = |x: f64| {
                let mut scene = Scene::new();
                let clip = Rect::new(8.0, 8.0, 180.0, 120.0);
                let mut curve = BezPath::new();
                curve.move_to((x, 112.0));
                curve.curve_to((x + 28.0, 12.0), (x + 90.0, 118.0), (x + 128.0, 28.0));
                curve.close_path();
                scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &clip);
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgba8(220, 70, 40, 190),
                    None,
                    &curve,
                );
                scene.pop_layer();
                scene
            };
            // The stride fixture deliberately touched the whole retained target;
            // restore a known full baseline before testing bounded damage.
            retained.render_rgba_resilient(&scene_for(8.0));
            let first = scene_for(16.0);
            let damage = Rect::new(8.0, 8.0, 180.0, 120.0);
            let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &first,
                Color::TRANSPARENT,
                &[damage],
            );
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&first),
                ),
                0
            );
            if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                let direct =
                    (1_u32.checked_shl(25).unwrap_or(0)) | (1_u32.checked_shl(26).unwrap_or(0));
                let indirect = (1_u32.checked_shl(13).unwrap_or(0))
                    | (1_u32.checked_shl(14).unwrap_or(0))
                    | (1_u32.checked_shl(17).unwrap_or(0))
                    | (1_u32.checked_shl(18).unwrap_or(0));
                assert_eq!(
                    timing.gpu_dispatch_timestamp_sampled_submissions,
                    timing.gpu_dispatch_timestamp_expected_submissions,
                );
                assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
                assert_eq!(
                    timing.gpu_dispatch_timestamp_present_stages & direct,
                    direct
                );
                assert_eq!(timing.gpu_dispatch_timestamp_present_stages & indirect, 0);
            }

            retained.set_direct_path_dispatch_enabled(false);
            let second = scene_for(24.0);
            let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &second,
                Color::TRANSPARENT,
                &[damage],
            );
            assert!(!retained.vello().direct_path_dispatch_enabled());
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&second),
                ),
                0
            );
            if retained.vello().gpu_dispatch_timestamp_buffer_bytes() != 0 {
                let direct =
                    (1_u32.checked_shl(25).unwrap_or(0)) | (1_u32.checked_shl(26).unwrap_or(0));
                let indirect = (1_u32.checked_shl(13).unwrap_or(0))
                    | (1_u32.checked_shl(14).unwrap_or(0))
                    | (1_u32.checked_shl(17).unwrap_or(0))
                    | (1_u32.checked_shl(18).unwrap_or(0));
                assert_eq!(timing.gpu_dispatch_timestamp_present_stages & direct, 0);
                assert_ne!(timing.gpu_dispatch_timestamp_present_stages & indirect, 0);
                assert_eq!(timing.gpu_dispatch_timestamp_missing_submissions, 0);
                assert_eq!(timing.gpu_dispatch_timestamp_stale_submissions, 0);
            }
        }

        // Case `gpu_path_experiments_keep_overflow_recovery_complete`.
        'gpu_path_experiments_keep_overflow_recovery_complete: {
            let deferred_bump_requested = matches!(
                std::env::var("VELLO_DEFER_BUMP_READBACK").as_deref(),
                Ok("1")
            );
            let direct_path_requested = env_enabled_by_default("VELLO_DIRECT_PATH_DISPATCH");
            if !deferred_bump_requested && !direct_path_requested {
                eprintln!("skipping GPU path recovery test: no experiment is requested");
                break 'gpu_path_experiments_keep_overflow_recovery_complete;
            }
            if shared_gpu().is_err() {
                eprintln!("skipping deferred bump recovery test: no shared GPU");
                break 'gpu_path_experiments_keep_overflow_recovery_complete;
            }

            const W: u32 = 384;
            const H: u32 = 256;
            let mut retained = OffscreenRenderer::new(W, H);
            if !retained.vello().defer_bump_readback_enabled()
                && !retained.vello().direct_path_dispatch_enabled()
            {
                assert!(std::env::var_os("TILO_UI_VELLO_CPU").is_some());
                eprintln!("skipping GPU path recovery test: global CPU rendering is active");
                break 'gpu_path_experiments_keep_overflow_recovery_complete;
            }
            if deferred_bump_requested {
                assert!(retained.vello().defer_bump_readback_enabled());
            }
            if direct_path_requested {
                assert!(retained.vello().direct_path_dispatch_enabled());
            }
            let mut oracle = OffscreenRenderer::new(W, H);
            oracle.set_compute_pass_batching(false);
            oracle.set_direct_path_dispatch_enabled(false);
            oracle.set_defer_bump_readback_enabled(false);
            oracle.set_sparse_fine_enabled(false);

            let mut scene = Scene::new();
            let full = Rect::new(0.0, 0.0, f64::from(W), f64::from(H));
            // 512 viewport-covering opaque paths request 196,608 tiles, beyond
            // the 122,880-tile initial floor. Their final pixels remain simple,
            // so this specifically exercises bump overflow/retry rather than an
            // image-comparison edge case.
            for _ in 0..512 {
                scene.fill(
                    Fill::NonZero,
                    Affine::IDENTITY,
                    Color::from_rgb8(24, 96, 188),
                    None,
                    &full,
                );
            }
            let (_, timing) = retained.render_damage_regions_gpu_wait_profiled(
                &scene,
                Color::TRANSPARENT,
                &[full],
            );
            assert!(
                timing.full_attempts >= 2,
                "the fixture must overflow the initial tile reservation and retry"
            );
            assert_eq!(
                OffscreenRenderer::pixels_differing_beyond_aa(
                    &retained.readback_current(),
                    &oracle.render_rgba_resilient(&scene),
                ),
                0
            );
        }
    }
}
