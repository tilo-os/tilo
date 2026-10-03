// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Vello is a 2d graphics rendering engine written in Rust, using [`wgpu`].
//! It efficiently draws large 2d scenes with interactive or near-interactive performance.
//!
//! ![image](https://github.com/linebender/vello/assets/8573618/cc2b742e-2135-4b70-8051-c49aeddb5d19)
//!
//!
//! ## Motivation
//!
//! Vello is meant to fill the same place in the graphics stack as other vector graphics renderers like [Skia](https://skia.org/), [Cairo](https://www.cairographics.org/), and its predecessor project [Piet](https://www.cairographics.org/).
//! On a basic level, that means it provides tools to render shapes, images, gradients, texts, etc, using a PostScript-inspired API, the same that powers SVG files and [the browser `<canvas>` element](https://developer.mozilla.org/en-US/docs/Web/API/CanvasRenderingContext2D).
//!
//! Vello's selling point is that it gets better performance than other renderers by better leveraging the GPU.
//! In traditional PostScript-style renderers, some steps of the render process like sorting and clipping either need to be handled in the CPU or done through the use of intermediary textures.
//! Vello avoids this by using prefix-scan algorithms to parallelize work that usually needs to happen in sequence, so that work can be offloaded to the GPU with minimal use of temporary buffers.
//!
//! This means that Vello needs a GPU with support for compute shaders to run.
//!
//!
//! ## Getting started
//!
//! Vello is meant to be integrated deep in UI render stacks.
//! While drawing in a Vello [`Scene`] is easy, actually rendering that scene to a surface setting up a wgpu context, which is a non-trivial task.
//!
//! To use Vello as the renderer for your PDF reader / GUI toolkit / etc, your code will have to look roughly like this:
//!
//! ```ignore
//! // Initialize wgpu and get handles
//! let (width, height) = ...;
//! let device: wgpu::Device = ...;
//! let queue: wgpu::Queue = ...;
//! let mut renderer = Renderer::new(
//!    &device,
//!    RendererOptions {
//!       use_cpu: false,
//!       antialiasing_support: vello::AaSupport::all(),
//!       num_init_threads: NonZeroUsize::new(1),
//!    },
//! ).expect("Failed to create renderer");
//!
//! // Create scene and draw stuff in it
//! let mut scene = vello::Scene::new();
//! scene.fill(
//!    vello::peniko::Fill::NonZero,
//!    vello::Affine::IDENTITY,
//!    vello::Color::from_rgb8(242, 140, 168),
//!    None,
//!    &vello::Circle::new((420.0, 200.0), 120.0),
//! );
//!
//! // Draw more stuff
//! scene.push_layer(...);
//! scene.fill(...);
//! scene.stroke(...);
//! scene.pop_layer(...);
//!
//! let texture = device.create_texture(&...);
//! // Render to a wgpu Texture
//! renderer
//!    .render_to_texture(
//!       &device,
//!       &queue,
//!       &scene,
//!       &texture,
//!       &vello::RenderParams {
//!          base_color: palette::css::BLACK, // Background color
//!          width,
//!          height,
//!          antialiasing_method: AaConfig::Msaa16,
//!       },
//!    )
//!    .expect("Failed to render to a texture");
//! // Do things with surface texture, such as blitting it to the Surface using
//! // wgpu::util::TextureBlitter.
//! ```
//!
//! See the [`examples/`](https://github.com/linebender/vello/tree/main/examples) folder to see how that code integrates with frameworks like winit.

// LINEBENDER LINT SET - lib.rs - v2
// See https://linebender.org/wiki/canonical-lints/
// These lints aren't included in Cargo.toml because they
// shouldn't apply to examples and tests
#![warn(unused_crate_dependencies)]
#![warn(clippy::print_stdout, clippy::print_stderr)]
// Targeting e.g. 32-bit means structs containing usize can give false positives for 64-bit.
#![cfg_attr(target_pointer_width = "64", warn(clippy::trivially_copy_pass_by_ref))]
// END LINEBENDER LINT SET
#![cfg_attr(docsrs, feature(doc_cfg))]
// The following lints are part of the Linebender standard set,
// but resolving them has been deferred for now.
// Feel free to send a PR that solves one or more of these.
// Need to allow instead of expect until Rust 1.83 https://github.com/rust-lang/rust/pull/130025
#![allow(missing_docs, reason = "We have many as-yet undocumented items.")]
#![expect(
    missing_debug_implementations,
    clippy::cast_possible_truncation,
    clippy::missing_assert_message,
    reason = "Deferred"
)]
#![allow(
    clippy::todo,
    unreachable_pub,
    unnameable_types,
    reason = "Deferred, only apply in some feature sets so not expect"
)]

#[cfg(feature = "wgpu")]
mod bump_budget;
mod debug;
mod recording;
mod render;
mod scene;
mod shaders;

#[cfg(feature = "wgpu")]
pub mod util;
#[cfg(feature = "wgpu")]
mod wgpu_engine;

pub mod low_level {
    //! Utilities which can be used to create an alternative Vello renderer to [`Renderer`][crate::Renderer].
    //!
    //! These APIs have not been carefully designed, and might not be powerful enough for this use case.

    pub use crate::debug::DebugLayers;
    pub use crate::recording::{
        BindType, BufferProxy, Command, ImageFormat, ImageProxy, Recording, ResourceBindings,
        ResourceId, ResourceProxy, ShaderId,
    };
    pub use crate::render::Render;
    pub use crate::shaders::FullShaders;
    /// Temporary export, used in `with_winit` for stats
    pub use vello_encoding::BumpAllocators;
}
/// Styling and composition primitives.
pub use peniko;
/// 2D geometry, with a focus on curves.
pub use peniko::kurbo;

#[cfg(feature = "wgpu")]
use peniko::ImageData;
#[cfg(feature = "wgpu")]
pub use wgpu;

pub use scene::{DrawGlyphs, Scene};
pub use vello_encoding::{FontEmbolden, Glyph, NormalizedCoord};

use low_level::ShaderId;
#[cfg(feature = "wgpu")]
use low_level::{BumpAllocators, FullShaders, Recording, Render};
use thiserror::Error;

#[cfg(feature = "wgpu")]
use debug::DebugLayers;
#[cfg(feature = "wgpu")]
use vello_encoding::Resolver;
#[cfg(feature = "wgpu")]
use wgpu_engine::{ExternalResource, WgpuEngine, default_enabled_from_env};

#[cfg(feature = "wgpu")]
use std::{borrow::Cow, num::NonZeroUsize, sync::atomic::AtomicBool};
#[cfg(feature = "wgpu")]
use wgpu::{Device, Queue, TextureView};
#[cfg(all(feature = "wgpu", feature = "wgpu-profiler"))]
use wgpu_profiler::{GpuProfiler, GpuProfilerSettings};

/// Represents the anti-aliasing method to use during a render pass.
///
/// Can be configured for a render operation by setting [`RenderParams::antialiasing_method`].
/// Each value of this can only be used if the corresponding field on [`AaSupport`] was used.
///
/// This can be converted into an `AaSupport` using [`Iterator::collect`],
/// as `AaSupport` implements `FromIterator`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AaConfig {
    /// Area anti-aliasing, where the alpha value for a pixel is computed from integrating
    /// the winding number over its square area.
    ///
    /// This technique produces very accurate values when the shape has winding number of 0 or 1
    /// everywhere, but can result in conflation artifacts otherwise.
    /// It generally has better performance than the multi-sampling methods.
    ///
    /// Can only be used if [enabled][AaSupport::area] for the `Renderer`.
    Area,
    /// 8x Multisampling
    ///
    /// Can only be used if [enabled][AaSupport::msaa8] for the `Renderer`.
    Msaa8,
    /// 16x Multisampling
    ///
    /// Can only be used if [enabled][AaSupport::msaa16] for the `Renderer`.
    Msaa16,
}

/// Represents the set of anti-aliasing configurations to enable during pipeline creation.
///
/// This is configured at `Renderer` creation time ([`Renderer::new`]) by setting
/// [`RendererOptions::antialiasing_support`].
///
/// This can be created from a set of `AaConfig` using [`Iterator::collect`],
/// as `AaSupport` implements `FromIterator`.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct AaSupport {
    /// Support [`AaConfig::Area`].
    pub area: bool,
    /// Support [`AaConfig::Msaa8`].
    pub msaa8: bool,
    /// Support [`AaConfig::Msaa16`].
    pub msaa16: bool,
}

impl AaSupport {
    /// Support every anti-aliasing method.
    ///
    /// This might increase startup time, as more shader variations must be compiled.
    pub fn all() -> Self {
        Self {
            area: true,
            msaa8: true,
            msaa16: true,
        }
    }

    /// Support only [`AaConfig::Area`].
    ///
    /// This should be the default choice for most users.
    pub fn area_only() -> Self {
        Self {
            area: true,
            msaa8: false,
            msaa16: false,
        }
    }
}

impl FromIterator<AaConfig> for AaSupport {
    fn from_iter<T: IntoIterator<Item = AaConfig>>(iter: T) -> Self {
        let mut result = Self {
            area: false,
            msaa8: false,
            msaa16: false,
        };
        for config in iter {
            match config {
                AaConfig::Area => result.area = true,
                AaConfig::Msaa8 => result.msaa8 = true,
                AaConfig::Msaa16 => result.msaa16 = true,
            }
        }
        result
    }
}

/// Errors that can occur in Vello.
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum Error {
    /// Encoding completed, but the caller declined submission. Reconstruct this renderer.
    #[error("render submission declined after encoding; reconstruct renderer")]
    SubmissionAborted,

    #[cfg(feature = "wgpu")]
    #[error("diagnostic geometry capture {0} bytes exceeds {1} byte cap")]
    #[doc(hidden)]
    DiagnosticGeometryCaptureTooLarge(u64, u64),
    #[cfg(feature = "wgpu")]
    #[error("diagnostic geometry capture is missing {0}")]
    #[doc(hidden)]
    DiagnosticGeometryCaptureMissing(&'static str),
    #[cfg(feature = "wgpu")]
    #[error("segment-order replay transfer {0} bytes exceeds {1} byte cap")]
    #[doc(hidden)]
    SegmentOrderReplayTooLarge(u64, u64),
    #[cfg(feature = "wgpu")]
    #[error("segment-order replay input is invalid: {0}")]
    #[doc(hidden)]
    SegmentOrderReplayInvalid(&'static str),
    #[cfg(feature = "wgpu")]
    #[error("segment-order replay is missing {0}")]
    #[doc(hidden)]
    SegmentOrderReplayMissing(&'static str),
    /// There is no available device with the features required by Vello.
    #[cfg(feature = "wgpu")]
    #[error("Couldn't find suitable device")]
    NoCompatibleDevice,
    /// Failed to create surface.
    /// See [`wgpu::CreateSurfaceError`] for more information.
    #[cfg(feature = "wgpu")]
    #[error("Couldn't create wgpu surface")]
    WgpuCreateSurfaceError(#[from] wgpu::CreateSurfaceError),
    /// Surface doesn't support the required texture formats.
    /// Make sure that you have a surface which provides one of
    /// [`TextureFormat::Rgba8Unorm`][wgpu::TextureFormat::Rgba8Unorm]
    /// or [`TextureFormat::Bgra8Unorm`][wgpu::TextureFormat::Bgra8Unorm] as texture formats.
    // TODO: Why does this restriction exist?
    #[cfg(feature = "wgpu")]
    #[error("Couldn't find `Rgba8Unorm` or `Bgra8Unorm` texture formats for surface")]
    UnsupportedSurfaceFormat,

    /// Used a buffer inside a recording while it was not available.
    /// Check if you have created it and not freed before its last usage.
    #[cfg(feature = "wgpu")]
    #[error("Buffer '{0}' is not available but used for {1}")]
    UnavailableBufferUsed(&'static str, &'static str),
    /// Failed to async map a buffer.
    /// See [`wgpu::BufferAsyncError`] for more information.
    #[cfg(feature = "wgpu")]
    #[error("Failed to async map a buffer")]
    BufferAsyncError(#[from] wgpu::BufferAsyncError),
    /// Failed to download an internal buffer for debug visualization.
    #[cfg(feature = "wgpu")]
    #[cfg(feature = "debug_layers")]
    #[error("Failed to download internal buffer '{0}' for visualization")]
    DownloadError(&'static str),

    #[cfg(feature = "wgpu")]
    #[error("wgpu Error from scope")]
    WgpuErrorFromScope(#[from] wgpu::Error),

    /// Failed to create [`GpuProfiler`].
    /// See [`wgpu_profiler::CreationError`] for more information.
    #[cfg(feature = "wgpu-profiler")]
    #[error("Couldn't create wgpu profiler")]
    #[doc(hidden)] // End-users of Vello should not have `wgpu-profiler` enabled.
    ProfilerCreationError(#[from] wgpu_profiler::CreationError),

    /// Failed to compile the shaders.
    #[cfg(feature = "hot_reload")]
    #[error("Failed to compile shaders:\n{0}")]
    #[doc(hidden)] // End-users of Vello should not have `hot_reload` enabled.
    ShaderCompilation(#[from] vello_shaders::compile::ErrorVec),
}

#[cfg_attr(
    not(feature = "wgpu"),
    expect(dead_code, reason = "this can be unused when wgpu feature is not used")
)]
pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;

/// State of the outstanding adaptive bump-counter readbacks.
#[cfg(feature = "wgpu")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BumpBudgetPoll {
    /// At least one counter map callback has not completed.
    Pending,
    /// Counter work has settled; this does not establish GPU frame completion.
    Complete {
        /// At least one collected render dropped geometry.
        overflowed: bool,
    },
}

/// A counter readback could not establish whether the render fit its budget.
#[cfg(feature = "wgpu")]
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum BumpBudgetError {
    /// The counter buffer could not be mapped.
    #[error("bump counter mapping failed")]
    MapFailed,
    /// A requested counter download is absent.
    #[error("bump counter download is missing")]
    MissingDownload,
    /// The mapped counter data has an unexpected length.
    #[error("bump counter download has invalid length")]
    InvalidDownload,
    /// The device could not be polled.
    #[error("bump counter device poll failed: {0}")]
    Poll(String),
}

/// Renders a scene into a texture or surface.
///
/// Currently, each renderer only supports a single surface format, if it
/// supports drawing to surfaces at all.
/// This is an assumption which is known to be limiting, and is planned to change.
#[cfg(feature = "wgpu")]
pub struct Renderer {
    options: RendererOptions,
    engine: WgpuEngine,
    resolver: Resolver,
    image_atlas: Option<recording::ImageProxy>,
    recording: Recording,
    packed_scene: Vec<u8>,
    shaders: FullShaders,
    /// How much bump-allocated scratch to reserve, and the per-frame
    /// readback that decides it. See [`crate::bump_budget`].
    budget: bump_budget::BudgetTracker,
    /// Overflow observed while beginning a later submission, before a caller
    /// has explicitly settled the previous one. This matters when one logical
    /// frame uses several recordings (such as a damage atlas): all of their
    /// results must make the final settle reject incomplete pixels.
    unsettled_bump_overflow: OverflowLatch,
    sparse_masks: Vec<[u32; 8]>,
    sparse_records: Vec<u32>,
    sparse_coarse_tiles: Vec<u32>,
    /// Exact opt-in command-order experiment for synchronous GPU rendering.
    /// It moves the existing bump readback after fine; it never removes it.
    defer_bump_readback: bool,
    sparse_upload_reuse: bool,
    sparse_upload: Option<SparseTileUpload>,
    /// Created only by the explicit fixed-order diagnostic, never at normal renderer startup.
    segment_order_replay_shader: Option<ShaderId>,
    /// Recorded-pass telemetry for the latest synchronous ordinary submission.
    last_canonical_segment_order_selected: bool,
    /// Recorded-pass telemetry for the latest synchronous ordinary submission.
    last_area_ppt8_selected: bool,
    /// Recorded-pass telemetry for the latest synchronous ordinary submission.
    last_clip_single_group_selected: bool,
    #[cfg(feature = "debug_layers")]
    debug: debug::DebugRenderer,
    #[cfg(feature = "wgpu-profiler")]
    #[doc(hidden)] // End-users of Vello should not have `wgpu-profiler` enabled.
    /// The profiler used with events for this renderer. This is *not* treated as public API.
    pub profiler: GpuProfiler,
    #[cfg(feature = "wgpu-profiler")]
    #[doc(hidden)] // End-users of Vello should not have `wgpu-profiler` enabled.
    /// The results from profiling. This is *not* treated as public API.
    pub profile_result: Option<Vec<wgpu_profiler::GpuTimerQueryResult>>,
}

/// GPU allocations whose sizes Vello can account for directly. Driver,
/// pipeline-cache, and swapchain memory are intentionally excluded.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RendererMemoryStats {
    pub bump_budget_bytes: u64,
    pub pooled_buffer_bytes: u64,
    pub live_buffer_bytes: u64,
    pub image_bytes: u64,
    pub sparse_upload_bytes: u64,
}

/// State of the opt-in GPU compute-pass timestamp diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum GpuTimestampStatus {
    Disabled,
    Unsupported,
    Idle,
    Pending { submission_id: u64 },
    Ready { submission_id: u64, pass_count: u32 },
    MapFailed { submission_id: u64 },
    Invalid { submission_id: u64 },
    Overflowed { submission_id: u64, pass_count: u32 },
}

/// One completed opt-in GPU compute-pass timestamp measurement.
#[derive(Clone, Debug)]
#[doc(hidden)]
pub struct GpuTimestampSample {
    pub submission_id: u64,
    pub pass_count: u32,
    pub span_ticks: u64,
    pub summed_pass_ticks: u64,
    pub timestamp_period_ns: f32,
    pub span_ns: f64,
    pub summed_pass_ns: f64,
    pub timestamp_bytes: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub enum GpuDispatchTimestampStatus {
    Disabled,
    Unsupported,
    Idle,
    Pending {
        submission_id: u64,
    },
    Ready {
        submission_id: u64,
        stage_count: u32,
    },
    MapFailed {
        submission_id: u64,
    },
    Invalid {
        submission_id: u64,
    },
    Overflowed {
        submission_id: u64,
        stage_count: u32,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct GpuDispatchStageSample {
    pub stage_id: u16,
    pub dispatch_count: u32,
    pub duration_ns: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct GpuDispatchTimestampSample {
    pub submission_id: u64,
    pub stage_count: u32,
    pub stages: [GpuDispatchStageSample; 32],
}

/// Stable labels for [`GpuDispatchStageSample::stage_id`]. ID zero is the
/// explicit aggregate for an unrecognised or overflowed shader label.
#[doc(hidden)]
pub fn gpu_dispatch_timestamp_stage_name(stage_id: u16) -> Option<&'static str> {
    Some(match stage_id {
        0 => "unknown",
        1 => "pathtag_reduce",
        2 => "pathtag_reduce2",
        3 => "pathtag_scan1",
        4 => "pathtag_scan_small",
        5 => "pathtag_scan_large",
        6 => "bbox_clear",
        7 => "flatten",
        8 => "draw_reduce",
        9 => "draw_leaf",
        10 => "clip_reduce",
        11 => "clip_leaf",
        12 => "binning",
        13 => "tile_alloc",
        14 => "path_count_setup",
        15 => "path_count",
        16 => "backdrop_dyn",
        17 => "coarse",
        18 => "path_tiling_setup",
        19 => "path_tiling",
        20 => "fine_area",
        21 => "fine_msaa8",
        22 => "fine_msaa16",
        23 => "fine_sparse_area",
        24 => "coarse_sparse",
        25 => "draw_leaf_prefix",
        26 => "path_count_direct",
        27 => "path_tiling_direct",
        28 => "coarse_sparse_active",
        _ => return None,
    })
}

/// An exact native-pixel damage rectangle for the opt-in sparse fine pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[doc(hidden)]
pub struct FineDamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Work selected by the opt-in sparse fine pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct SparseRenderStats {
    pub fine_tiles: u32,
    pub fine_pixels: u64,
    pub written_pixels: u64,
    pub coarse_pixels: u64,
    /// Whether this submission used the opt-in native active-bin coarse shader.
    pub coarse_active_bins: bool,
    /// Whether the opt-in adaptive coarse policy was effective for this renderer.
    pub coarse_adaptive: bool,
    /// Native coarse-bin count when active-list packing succeeded.
    pub coarse_native_bins: u32,
    /// Unique selected native-bin count when active-list packing succeeded.
    pub coarse_selected_native_bins: u32,
    /// Whether the selected active sparse-coarse dispatch narrowed path tile allocation.
    pub path_tile_bounds_applied: bool,
    /// Area in integer native tiles of the selected path-tile bounding union.
    pub path_tile_bounds_tiles: u32,
    /// Whether the Area-only sparse fine quad-skip permutation was selected.
    pub fine_quad_skip_applied: bool,
    /// Native four-pixel invocations in the unfiltered sparse-tile dispatch.
    pub fine_quad_total_quads: u32,
    /// Native four-pixel invocations whose damage-mask nibble is nonzero.
    pub fine_quad_active_quads: u32,
    /// Whether this sparse submission selected an opaque full-tile coarse permutation.
    pub coarse_opaque_prune_selected: bool,
    /// Whether this sparse submission selected paired coarse/fine SOLID fusion.
    pub solid_fuse_selected: bool,
    /// Whether this sparse submission selected paired segment-row-mask writer and reader shaders.
    pub segment_row_mask_selected: bool,
    /// Whether this sparse submission recorded the opt-in canonical segment-order pass.
    pub canonical_segment_order_shader_selected: bool,
    /// Whether this sparse submission recorded the opt-in Area PPT8 shader.
    pub area_ppt8_shader_selected: bool,
    /// Whether this submission recorded the opt-in single-workgroup clip leaf shader.
    pub clip_single_group_shader_selected: bool,
}

/// Exact resolved-input metadata for the diagnostic geometry-prefix opportunity.
///
/// This describes inputs only; it is not evidence that any GPU geometry output can be reused.
#[cfg(feature = "wgpu")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GeometryInputDescriptor {
    pub n_draw_objects: u32,
    pub n_paths: u32,
    pub n_clips: u32,
    pub bin_data_start: u32,
    pub path_tag_base: u32,
    pub path_data_base: u32,
    pub draw_tag_base: u32,
    pub draw_data_base: u32,
    pub transform_base: u32,
    pub style_base: u32,
    pub target_width: u32,
    pub target_height: u32,
    pub width_in_tiles: u32,
    pub height_in_tiles: u32,
    pub lines_size: u32,
    pub binning_size: u32,
    pub tiles_size: u32,
    pub seg_counts_size: u32,
    pub segments_size: u32,
    pub blend_size: u32,
    pub ptcl_size: u32,
    pub cpu_route: bool,
    pub bump_readback_pending: bool,
}

/// Borrowed exact geometry inputs from the latest encoded recording upload.
#[cfg(feature = "wgpu")]
#[doc(hidden)]
pub struct GeometryInput<'a> {
    pub descriptor: GeometryInputDescriptor,
    pub path_tags: &'a [u8],
    pub path_data: &'a [u8],
    pub transforms: &'a [u8],
    pub styles: &'a [u8],
}

/// Reason a geometry-input diagnostic observation cannot be formed safely.
#[cfg(feature = "wgpu")]
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryInputError {
    MissingOrAmbiguousUpload,
    MalformedConfig,
    MalformedSpans,
    EmptyGeometry,
    CpuRoute,
}

#[cfg(feature = "wgpu")]
fn geometry_recording_uploads(recording: &Recording) -> Result<(&[u8], &[u8]), GeometryInputError> {
    let mut scene = None;
    let mut config = None;
    for command in &recording.commands {
        match command {
            recording::Command::Upload(buffer, bytes) if buffer.name == "vello.scene" => {
                if scene.replace(bytes.as_slice()).is_some() {
                    return Err(GeometryInputError::MissingOrAmbiguousUpload);
                }
            }
            recording::Command::UploadUniform(buffer, bytes) if buffer.name == "vello.config" => {
                if config.replace(bytes.as_slice()).is_some() {
                    return Err(GeometryInputError::MissingOrAmbiguousUpload);
                }
            }
            _ => {}
        }
    }
    scene
        .zip(config)
        .ok_or(GeometryInputError::MissingOrAmbiguousUpload)
}

#[cfg(feature = "wgpu")]
fn geometry_input_from_uploads<'a>(
    scene: &'a [u8],
    config_bytes: &[u8],
    cpu_route: bool,
    bump_readback_pending: bool,
) -> Result<GeometryInput<'a>, GeometryInputError> {
    if cpu_route {
        return Err(GeometryInputError::CpuRoute);
    }
    if config_bytes.len() != size_of::<vello_encoding::ConfigUniform>() {
        return Err(GeometryInputError::MalformedConfig);
    }
    let config = bytemuck::pod_read_unaligned::<vello_encoding::ConfigUniform>(config_bytes);
    let layout = config.layout;
    if scene.len() % size_of::<u32>() != 0 {
        return Err(GeometryInputError::MalformedSpans);
    }
    if layout.n_paths == 0 {
        return Err(GeometryInputError::EmptyGeometry);
    }
    let offset = |word: u32| usize::try_from(word).ok()?.checked_mul(size_of::<u32>());
    let [
        Some(path_tag),
        Some(path_data),
        Some(draw_tag),
        Some(draw_data),
        Some(transform),
        Some(style),
    ] = [
        offset(layout.path_tag_base),
        offset(layout.path_data_base),
        offset(layout.draw_tag_base),
        offset(layout.draw_data_base),
        offset(layout.transform_base),
        offset(layout.style_base),
    ]
    else {
        return Err(GeometryInputError::MalformedSpans);
    };
    if !(path_tag <= path_data
        && path_data <= draw_tag
        && draw_tag <= draw_data
        && draw_data <= transform
        && transform <= style
        && style <= scene.len())
    {
        return Err(GeometryInputError::MalformedSpans);
    }
    if path_tag == path_data && path_data == draw_tag && transform == style && style == scene.len()
    {
        return Err(GeometryInputError::EmptyGeometry);
    }
    Ok(GeometryInput {
        descriptor: GeometryInputDescriptor {
            n_draw_objects: layout.n_draw_objects,
            n_paths: layout.n_paths,
            n_clips: layout.n_clips,
            bin_data_start: layout.bin_data_start,
            path_tag_base: layout.path_tag_base,
            path_data_base: layout.path_data_base,
            draw_tag_base: layout.draw_tag_base,
            draw_data_base: layout.draw_data_base,
            transform_base: layout.transform_base,
            style_base: layout.style_base,
            target_width: config.target_width,
            target_height: config.target_height,
            width_in_tiles: config.width_in_tiles,
            height_in_tiles: config.height_in_tiles,
            lines_size: config.lines_size,
            binning_size: config.binning_size,
            tiles_size: config.tiles_size,
            seg_counts_size: config.seg_counts_size,
            segments_size: config.segments_size,
            blend_size: config.blend_size,
            ptcl_size: config.ptcl_size,
            cpu_route: false,
            bump_readback_pending,
        },
        path_tags: &scene[path_tag..path_data],
        path_data: &scene[path_data..draw_tag],
        transforms: &scene[transform..style],
        styles: &scene[style..],
    })
}

#[cfg(feature = "wgpu")]
#[doc(hidden)]
pub struct SharedCoarseGeometryDownload {
    pub config: Vec<u8>,
    pub ptcl: wgpu::Buffer,
    pub ptcl_bytes: u64,
    pub segments: wgpu::Buffer,
    pub segments_bytes: u64,
}

/// Fixed-size diagnostic input for replaying one native PTCL fill with two segment orders.
///
/// This is deliberately an internal capture format rather than a rendering API. The shader
/// validates every field against the live coarse buffers before writing either order.
#[cfg(feature = "wgpu")]
#[doc(hidden)]
pub struct SegmentOrderReplayInput {
    pub tile: u32,
    pub logical_command: u32,
    pub size_and_rule: u32,
    pub backdrop: u32,
    pub expected: [[u32; 5]; 49],
    pub left: [[u32; 5]; 49],
    pub right: [[u32; 5]; 49],
}

/// GPU witnesses for the two guarded order patches. Each contains five header words followed
/// by the actual 49 five-word segment tuples read after its write.
#[cfg(feature = "wgpu")]
#[doc(hidden)]
pub struct SegmentOrderReplayDownload {
    pub left: wgpu::Buffer,
    pub right: wgpu::Buffer,
    pub bytes_per_witness: u64,
}

#[cfg(feature = "wgpu")]
const SEGMENT_ORDER_REPLAY_COUNT: usize = 49;
#[cfg(feature = "wgpu")]
const SEGMENT_ORDER_REPLAY_WORDS: usize = SEGMENT_ORDER_REPLAY_COUNT * 5;
#[cfg(feature = "wgpu")]
const SEGMENT_ORDER_REPLAY_WITNESS_WORDS: usize = 5 + SEGMENT_ORDER_REPLAY_WORDS;
#[cfg(feature = "wgpu")]
const SEGMENT_ORDER_REPLAY_TRANSFER_CAP: u64 = 64 * 1024 * 1024;

// This shader is created only by the explicit replay diagnostic. It is intentionally not part
// of `FullShaders`: normal renderers do not compile, bind, or dispatch it.
#[cfg(feature = "wgpu")]
const SEGMENT_ORDER_REPLAY_WGSL: &str = r#"
struct Control {
    tile: u32,
    logical_command: u32,
    size_and_rule: u32,
    backdrop: u32,
    ptcl_words: u32,
    segment_words: u32,
    segment_records: u32,
    tile_count: u32,
}

@group(0) @binding(0) var<storage, read> ptcl: array<u32>;
@group(0) @binding(1) var<storage, read_write> segments: array<u32>;
@group(0) @binding(2) var<storage, read> expected: array<u32>;
@group(0) @binding(3) var<storage, read> requested: array<u32>;
@group(0) @binding(4) var<uniform> control: Control;
@group(0) @binding(5) var<storage, read_write> witness: array<u32>;

const COUNT: u32 = 49u;
const WORDS_PER_SEGMENT: u32 = 6u;
const INITIAL_ALLOC: u32 = 64u;
const CONTINUATION_ALLOC: u32 = 256u;

fn fail(code: u32) {
    witness[0] = code;
}

fn width(tag: u32) -> u32 {
    switch tag {
        case 1u { return 4u; }
        case 3u, 10u { return 1u; }
        case 5u, 9u { return 2u; }
        case 6u, 7u, 8u, 11u, 13u { return 3u; }
        default { return 0u; }
    }
}

@compute @workgroup_size(1)
fn main() {
    if (control.ptcl_words < INITIAL_ALLOC) {
        fail(2u);
        return;
    }
    if (arrayLength(&ptcl) < control.ptcl_words || arrayLength(&segments) < control.segment_words
        || arrayLength(&expected) < COUNT * 5u || arrayLength(&requested) < COUNT * 5u
        || arrayLength(&witness) < 5u + COUNT * 5u) {
        fail(15u);
        return;
    }
    let complete_records = control.segment_words / WORDS_PER_SEGMENT;
    if (control.segment_records > complete_records) {
        fail(17u);
        return;
    }
    if (control.tile >= control.tile_count) {
        fail(16u);
        return;
    }
    var command = control.tile * INITIAL_ALLOC + 1u;
    var block_end = (control.tile + 1u) * INITIAL_ALLOC;
    if (command >= control.ptcl_words || block_end > control.ptcl_words) {
        fail(3u);
        return;
    }
    var semantic = 0u;
    var jumps = 0u;
    for (var steps = 0u; steps < 512u; steps++) {
        if (command >= block_end || command >= control.ptcl_words) {
            fail(4u);
            return;
        }
        let tag = ptcl[command];
        if (tag == 0u) {
            fail(5u);
            return;
        }
        if (tag == 12u) {
            if (command + 1u >= block_end) {
                fail(6u);
                return;
            }
            let jump_target = ptcl[command + 1u];
            if (control.ptcl_words < CONTINUATION_ALLOC || jump_target > control.ptcl_words - CONTINUATION_ALLOC) {
                fail(7u);
                return;
            }
            command = jump_target;
            block_end = jump_target + CONTINUATION_ALLOC;
            jumps += 1u;
            if (jumps > 16u) {
                fail(8u);
                return;
            }
            continue;
        }
        let command_width = width(tag);
        if (command_width == 0u || command + command_width > block_end) {
            fail(9u);
            return;
        }
        if (semantic == control.logical_command) {
            if (tag != 1u || command + 3u >= block_end) {
                fail(10u);
                return;
            }
            let size_and_rule = ptcl[command + 1u];
            let segment_start = ptcl[command + 2u];
            let backdrop = ptcl[command + 3u];
            if (size_and_rule != control.size_and_rule || backdrop != control.backdrop || size_and_rule >> 1u != COUNT) {
                fail(11u);
                return;
            }
            if (control.segment_records < COUNT || segment_start > control.segment_records - COUNT) {
                fail(12u);
                return;
            }
            var used: array<u32, 49>;
            for (var actual_ix = 0u; actual_ix < COUNT; actual_ix++) {
                let actual_base = (segment_start + actual_ix) * WORDS_PER_SEGMENT;
                var found = false;
                for (var expected_ix = 0u; expected_ix < COUNT; expected_ix++) {
                    let expected_base = expected_ix * 5u;
                    let same = segments[actual_base] == expected[expected_base]
                        && segments[actual_base + 1u] == expected[expected_base + 1u]
                        && segments[actual_base + 2u] == expected[expected_base + 2u]
                        && segments[actual_base + 3u] == expected[expected_base + 3u]
                        && segments[actual_base + 4u] == expected[expected_base + 4u];
                    if (!found && used[expected_ix] == 0u && same) {
                        used[expected_ix] = 1u;
                        found = true;
                    }
                }
                if (!found) {
                    fail(13u);
                    return;
                }
            }
            for (var ix = 0u; ix < COUNT; ix++) {
                let segment_base = (segment_start + ix) * WORDS_PER_SEGMENT;
                let order_base = ix * 5u;
                segments[segment_base] = requested[order_base];
                segments[segment_base + 1u] = requested[order_base + 1u];
                segments[segment_base + 2u] = requested[order_base + 2u];
                segments[segment_base + 3u] = requested[order_base + 3u];
                segments[segment_base + 4u] = requested[order_base + 4u];
            }
            witness[0] = 1u;
            witness[1] = segment_start;
            witness[2] = semantic;
            witness[3] = size_and_rule;
            witness[4] = backdrop;
            for (var ix = 0u; ix < COUNT; ix++) {
                let segment_base = (segment_start + ix) * WORDS_PER_SEGMENT;
                let witness_base = 5u + ix * 5u;
                witness[witness_base] = segments[segment_base];
                witness[witness_base + 1u] = segments[segment_base + 1u];
                witness[witness_base + 2u] = segments[segment_base + 2u];
                witness[witness_base + 3u] = segments[segment_base + 3u];
                witness[witness_base + 4u] = segments[segment_base + 4u];
            }
            return;
        }
        semantic += 1u;
        command += command_width;
    }
    fail(14u);
}
"#;

#[cfg(feature = "wgpu")]
const DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP: u64 = 64 * 1024 * 1024;

#[cfg(feature = "wgpu")]
fn diagnostic_geometry_download_total(
    config_bytes: u64,
    ptcl_bytes: u64,
    segments_bytes: u64,
) -> u64 {
    config_bytes
        .checked_add(ptcl_bytes)
        .and_then(|total| total.checked_add(segments_bytes))
        .unwrap_or(u64::MAX)
}

#[cfg(feature = "wgpu")]
fn diagnostic_geometry_config(recording: &Recording) -> Result<Vec<u8>> {
    recording
        .commands
        .iter()
        .rev()
        .find_map(|command| match command {
            recording::Command::UploadUniform(buffer, bytes) if buffer.name == "vello.config" => {
                Some(bytes.clone())
            }
            _ => None,
        })
        .ok_or(Error::DiagnosticGeometryCaptureMissing("vello.config"))
}

#[cfg(feature = "wgpu")]
fn take_diagnostic_geometry_downloads(
    engine: &mut WgpuEngine,
    ptcl_proxy: recording::BufferProxy,
    segments_proxy: recording::BufferProxy,
) -> Result<(wgpu::Buffer, wgpu::Buffer)> {
    // Remove both entries before reporting a missing copy, so a failed
    // diagnostic capture cannot retain whichever buffer was present.
    let (ptcl, segments) = (
        engine.take_download(ptcl_proxy),
        engine.take_download(segments_proxy),
    );
    let ptcl = ptcl.ok_or(Error::DiagnosticGeometryCaptureMissing("PTCL download"))?;
    let segments = segments.ok_or(Error::DiagnosticGeometryCaptureMissing("segment download"))?;
    Ok((ptcl, segments))
}

#[cfg(feature = "wgpu")]
fn segment_order_replay_words(order: &[[u32; 5]; SEGMENT_ORDER_REPLAY_COUNT]) -> Vec<u8> {
    let mut words = Vec::with_capacity(SEGMENT_ORDER_REPLAY_WORDS);
    for segment in order {
        words.extend_from_slice(segment);
    }
    bytemuck::cast_slice(&words).to_vec()
}

#[cfg(feature = "wgpu")]
fn same_segment_multiset(
    left: &[[u32; 5]; SEGMENT_ORDER_REPLAY_COUNT],
    right: &[[u32; 5]; SEGMENT_ORDER_REPLAY_COUNT],
) -> bool {
    let mut used = [false; SEGMENT_ORDER_REPLAY_COUNT];
    left.iter().all(|segment| {
        right
            .iter()
            .enumerate()
            .find(|(index, candidate)| !used[*index] && *candidate == segment)
            .is_some_and(|(index, _)| {
                used[index] = true;
                true
            })
    })
}

#[cfg(feature = "wgpu")]
fn validate_segment_order_replay_input(input: &SegmentOrderReplayInput) -> Result<()> {
    if input.size_and_rule >> 1 != u32::try_from(SEGMENT_ORDER_REPLAY_COUNT).unwrap_or(u32::MAX) {
        return Err(Error::SegmentOrderReplayInvalid("segment count"));
    }
    if !same_segment_multiset(&input.expected, &input.left) {
        return Err(Error::SegmentOrderReplayInvalid("left order multiset"));
    }
    if !same_segment_multiset(&input.expected, &input.right) {
        return Err(Error::SegmentOrderReplayInvalid("right order multiset"));
    }
    Ok(())
}

#[cfg(feature = "wgpu")]
fn segment_order_replay_transfer_bytes(width: u32, height: u32) -> Result<u64> {
    let padded_row = u64::from(width)
        .checked_mul(4)
        .map(|bytes| bytes.next_multiple_of(256))
        .ok_or(Error::SegmentOrderReplayTooLarge(
            u64::MAX,
            SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
        ))?;
    let output = padded_row
        .checked_mul(u64::from(height))
        .and_then(|bytes| bytes.checked_mul(2))
        .ok_or(Error::SegmentOrderReplayTooLarge(
            u64::MAX,
            SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
        ))?;
    let witnesses = u64::try_from(SEGMENT_ORDER_REPLAY_WITNESS_WORDS)
        .unwrap_or(u64::MAX)
        .checked_mul(4)
        // Two zero-filled witness uploads and two MAP_READ downloads are explicit logical
        // diagnostic transfers. This is not a bus-traffic or VRAM accounting claim.
        .and_then(|bytes| bytes.checked_mul(4))
        .ok_or(Error::SegmentOrderReplayTooLarge(
            u64::MAX,
            SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
        ))?;
    let inputs = u64::try_from(SEGMENT_ORDER_REPLAY_WORDS)
        .unwrap_or(u64::MAX)
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_mul(3))
        .and_then(|bytes| bytes.checked_add(32))
        .ok_or(Error::SegmentOrderReplayTooLarge(
            u64::MAX,
            SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
        ))?;
    output
        .checked_add(witnesses)
        .and_then(|bytes| bytes.checked_add(inputs))
        .ok_or(Error::SegmentOrderReplayTooLarge(
            u64::MAX,
            SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
        ))
}

#[cfg(feature = "wgpu")]
fn segment_order_replay_native_tile_count(width: u32, height: u32) -> Result<u32> {
    if width == 0 || height == 0 {
        return Err(Error::SegmentOrderReplayInvalid("native extent"));
    }
    width
        .checked_add(15)
        .and_then(|value| value.checked_div(16))
        .and_then(|columns| {
            height
                .checked_add(15)
                .and_then(|value| value.checked_div(16))
                .and_then(|rows| columns.checked_mul(rows))
        })
        .ok_or(Error::SegmentOrderReplayInvalid("native tile count"))
}

#[cfg(feature = "wgpu")]
fn segment_order_replay_control(
    input: &SegmentOrderReplayInput,
    ptcl_bytes: u64,
    segment_bytes: u64,
    width: u32,
    height: u32,
) -> Result<[u32; 8]> {
    let ptcl_words = ptcl_bytes
        .checked_div(4)
        .and_then(|words| u32::try_from(words).ok())
        .ok_or(Error::SegmentOrderReplayInvalid("PTCL word range"))?;
    let segment_words = segment_bytes
        .checked_div(4)
        .and_then(|words| u32::try_from(words).ok())
        .ok_or(Error::SegmentOrderReplayInvalid("segment word range"))?;
    let segment_records = segment_words / 6;
    let tile_count = segment_order_replay_native_tile_count(width, height)?;
    Ok([
        input.tile,
        input.logical_command,
        input.size_and_rule,
        input.backdrop,
        ptcl_words,
        segment_words,
        segment_records,
        tile_count,
    ])
}

#[cfg(feature = "wgpu")]
fn take_segment_order_replay_downloads(
    engine: &mut WgpuEngine,
    witnesses: [recording::BufferProxy; 2],
) -> Result<(wgpu::Buffer, wgpu::Buffer)> {
    let (left, right) = (
        engine.take_download(witnesses[0]),
        engine.take_download(witnesses[1]),
    );
    let left = left.ok_or(Error::SegmentOrderReplayMissing("left witness"))?;
    let right = right.ok_or(Error::SegmentOrderReplayMissing("right witness"))?;
    Ok((left, right))
}
// This is not `Send` (or `Sync`) on WebAssembly as the
// underlying wgpu types are not. This can be enabled with the
// `fragile-send-sync-non-atomic-wasm` feature in wgpu.
// See https://github.com/gfx-rs/wgpu/discussions/4127 for
// further discussion of this topic.
#[cfg(all(feature = "wgpu", not(target_arch = "wasm32")))]
static_assertions::assert_impl_all!(Renderer: Send);

/// Parameters used in a single render that are configurable by the client.
///
/// These are used in [`Renderer::render_to_texture`].
pub struct RenderParams {
    /// The background color applied to the target. This value is only applicable to the full
    /// pipeline.
    pub base_color: peniko::Color,

    /// Dimensions of the rasterization target
    pub width: u32,
    pub height: u32,

    /// The anti-aliasing algorithm. The selected algorithm must have been initialized while
    /// constructing the `Renderer`.
    pub antialiasing_method: AaConfig,
}

#[cfg(feature = "wgpu")]
/// Options which are set at renderer creation time, used in [`Renderer::new`].
pub struct RendererOptions {
    /// If true, run all stages up to fine rasterization on the CPU.
    ///
    /// This is not a recommended configuration as it is expected to have poor performance,
    /// but it can be useful for debugging.
    // TODO: Consider evolving this so that the CPU stages can be configured dynamically via
    // `RenderParams`.
    pub use_cpu: bool,

    /// Represents the enabled set of AA configurations. This will be used to determine which
    /// pipeline permutations should be compiled at startup.
    ///
    /// By default this will be all modes, to support the widest range of.
    /// It is recommended that most users configure this.
    pub antialiasing_support: AaSupport,

    /// How many threads to use for initialisation of shaders.
    ///
    /// Use `Some(1)` to use a single thread. This is recommended when on macOS
    /// (see <https://github.com/bevyengine/bevy/pull/10812#discussion_r1496138004>)
    ///
    /// Set to `None` to use a heuristic which will use many but not all threads
    ///
    /// Has no effect on WebAssembly
    ///
    /// Will default to `None` on most platforms, `Some(1)` on macOS.
    pub num_init_threads: Option<NonZeroUsize>,

    /// The pipeline cache to use when creating the shaders.
    ///
    /// For much more discussion of expected usage patterns, see the documentation on that type.
    pub pipeline_cache: Option<wgpu::PipelineCache>,
}

#[cfg(feature = "wgpu")]
impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            use_cpu: false,
            antialiasing_support: AaSupport::all(),
            #[cfg(target_os = "macos")]
            num_init_threads: NonZeroUsize::new(1),
            #[cfg(not(target_os = "macos"))]
            num_init_threads: None,
            pipeline_cache: None,
        }
    }
}

#[cfg(feature = "wgpu")]
struct RenderResult {
    bump: Option<BumpAllocators>,
    #[cfg(feature = "debug_layers")]
    captured: Option<render::CapturedBuffers>,
}

#[cfg(feature = "wgpu")]
struct SparseTileUpload {
    buffer: wgpu::Buffer,
    capacity_bytes: u64,
}

/// Carries overflow from a completed readback until the caller explicitly
/// settles the logical frame that contained it.
#[cfg(feature = "wgpu")]
#[derive(Default)]
struct OverflowLatch(bool);

#[cfg(feature = "wgpu")]
impl OverflowLatch {
    fn record(&mut self, overflowed: bool) {
        self.0 |= overflowed;
    }

    fn take(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
}

#[cfg(feature = "wgpu")]
fn sparse_tile_records(
    masks: &mut Vec<[u32; 8]>,
    records: &mut Vec<u32>,
    width: u32,
    height: u32,
    damage: &[FineDamageRect],
    max_tiles: u32,
) -> Option<SparseRenderStats> {
    const TILE: u32 = 16;
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let tile_count = tiles_x.checked_mul(tiles_y)?;
    if tile_count == 0 || tile_count > max_tiles || tile_count > 65_536 {
        return None;
    }
    let tile_count = usize::try_from(tile_count).ok()?;
    masks.resize(tile_count, [0; 8]);
    masks.fill([0; 8]);
    for rect in damage {
        let x0 = rect.x.min(width);
        let y0 = rect.y.min(height);
        let x1 = rect.x.saturating_add(rect.width).min(width);
        let y1 = rect.y.saturating_add(rect.height).min(height);
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        let tile_y0 = y0 / TILE;
        let tile_y1 = y1.saturating_sub(1) / TILE;
        let tile_x0 = x0 / TILE;
        let tile_x1 = x1.saturating_sub(1) / TILE;
        for tile_y in tile_y0..=tile_y1 {
            let local_y0 = y0.saturating_sub(tile_y.saturating_mul(TILE));
            let local_y1 = y1.saturating_sub(tile_y.saturating_mul(TILE)).min(TILE);
            for tile_x in tile_x0..=tile_x1 {
                let local_x0 = x0.saturating_sub(tile_x.saturating_mul(TILE));
                let local_x1 = x1.saturating_sub(tile_x.saturating_mul(TILE)).min(TILE);
                let width_mask = ((1u32 << local_x1.saturating_sub(local_x0)) - 1) << local_x0;
                let tile = tile_y.checked_mul(tiles_x)?.checked_add(tile_x)?;
                let mask = &mut masks[usize::try_from(tile).ok()?];
                for word in local_y0 / 2..local_y1.saturating_sub(1) / 2 + 1 {
                    let row = word * 2;
                    if row >= local_y0 && row < local_y1 {
                        mask[usize::try_from(word).ok()?] |= width_mask;
                    }
                    if row.saturating_add(1) >= local_y0 && row.saturating_add(1) < local_y1 {
                        mask[usize::try_from(word).ok()?] |= width_mask << 16;
                    }
                }
            }
        }
    }
    records.clear();
    let mut written_pixels = 0u64;
    let mut fine_pixels = 0u64;
    for (tile, mask) in masks.iter().enumerate() {
        if mask.iter().all(|word| *word == 0) {
            continue;
        }
        written_pixels = written_pixels.saturating_add(
            mask.iter()
                .map(|word| u64::from(word.count_ones()))
                .sum::<u64>(),
        );
        records.push(u32::try_from(tile).ok()?);
        records.extend(*mask);
        let tile_x = u32::try_from(tile).ok()? % tiles_x;
        let tile_y = u32::try_from(tile).ok()? / tiles_x;
        fine_pixels = fine_pixels.saturating_add(
            u64::from(width.saturating_sub(tile_x.saturating_mul(TILE)).min(TILE)).saturating_mul(
                u64::from(height.saturating_sub(tile_y.saturating_mul(TILE)).min(TILE)),
            ),
        );
    }
    let fine_tiles = u32::try_from(records.len() / 9).ok()?;
    if fine_tiles == 0 {
        return None;
    }
    Some(SparseRenderStats {
        fine_tiles,
        fine_pixels,
        written_pixels,
        coarse_pixels: u64::from(width).saturating_mul(u64::from(height)),
        coarse_active_bins: false,
        coarse_adaptive: false,
        coarse_native_bins: 0,
        coarse_selected_native_bins: 0,
        path_tile_bounds_applied: false,
        path_tile_bounds_tiles: 0,
        fine_quad_skip_applied: false,
        fine_quad_total_quads: 0,
        fine_quad_active_quads: 0,
        coarse_opaque_prune_selected: false,
        solid_fuse_selected: false,
        segment_row_mask_selected: false,
        canonical_segment_order_shader_selected: false,
        area_ppt8_shader_selected: false,
        clip_single_group_shader_selected: false,
    })
}

#[cfg(feature = "wgpu")]
fn sparse_fine_quad_counts(records: &[u32], width: u32, height: u32) -> Option<(u32, u32, u64)> {
    if records.is_empty() || records.len() % 9 != 0 {
        return None;
    }
    let tiles_x = width.div_ceil(16);
    let tiles_y = height.div_ceil(16);
    let tile_count = tiles_x.checked_mul(tiles_y)?;
    if tile_count == 0 || tile_count > 65_536 {
        return None;
    }
    let total = u32::try_from(records.len() / 9).ok()?.checked_mul(64)?;
    let mut active = 0u32;
    let mut pixels = 0u64;
    for record in records.chunks_exact(9) {
        let tile = record[0];
        if tile >= tile_count {
            return None;
        }
        let tile_x = tile % tiles_x;
        let tile_y = tile / tiles_x;
        // Full native tiles do not need per-nibble coordinate clipping. Fold
        // each nibble to its low bit and count eight active quads at once.
        if tile_x.checked_mul(16)?.checked_add(16)? <= width
            && tile_y.checked_mul(16)?.checked_add(16)? <= height
        {
            for word in 0..8u32 {
                let mask = record[usize::try_from(word + 1).ok()?];
                let nibble_bits = (mask | (mask >> 1) | (mask >> 2) | (mask >> 3)) & 0x1111_1111;
                let count = nibble_bits.count_ones();
                active = active.checked_add(count)?;
                pixels = pixels.checked_add(u64::from(count) * 4)?;
            }
            continue;
        }
        for word in 0..8u32 {
            let mask = record[usize::try_from(word + 1).ok()?];
            for nibble in 0..8u32 {
                if ((mask >> (nibble * 4)) & 0xf) == 0 {
                    continue;
                }
                let local_pixel = word * 32 + nibble * 4;
                let x = tile_x.checked_mul(16)?.checked_add(local_pixel % 16)?;
                let y = tile_y.checked_mul(16)?.checked_add(local_pixel / 16)?;
                if x >= width || y >= height {
                    return None;
                }
                active = active.checked_add(1)?;
                pixels = pixels.checked_add(u64::from(width.saturating_sub(x).min(4)))?;
            }
        }
    }
    Some((total, active, pixels))
}

/// Return a bounded integer native-tile union for valid sparse damage records.
///
/// The union intentionally preserves native tile coordinates: it only limits
/// path allocation, while the existing coarse and fine passes retain their
/// original configuration and identifiers.
#[cfg(feature = "wgpu")]
pub(crate) fn sparse_path_tile_bounds(
    records: &[u32],
    width: u32,
    height: u32,
) -> Option<[u32; 4]> {
    let tiles_x = width.div_ceil(16);
    let tiles_y = height.div_ceil(16);
    let native_tiles = tiles_x.checked_mul(tiles_y)?;
    if native_tiles == 0 || native_tiles > 65_536 || records.is_empty() || records.len() % 9 != 0 {
        return None;
    }

    let mut x0 = tiles_x;
    let mut y0 = tiles_y;
    let mut x1 = 0;
    let mut y1 = 0;
    for record in records.chunks_exact(9) {
        let tile = record[0];
        if tile >= native_tiles {
            return None;
        }
        let tile_x = tile % tiles_x;
        let tile_y = tile / tiles_x;
        x0 = x0.min(tile_x);
        y0 = y0.min(tile_y);
        x1 = x1.max(tile_x.checked_add(1)?);
        y1 = y1.max(tile_y.checked_add(1)?);
    }
    let bounds = [x0, y0, x1, y1];
    render::path_tile_bounds_are_valid(bounds, width, height).then_some(bounds)
}

/// Build the fixed 8 KiB uniform tile selector consumed by `coarse_sparse`.
#[cfg(feature = "wgpu")]
pub(crate) fn sparse_coarse_tile_bits(
    bits: &mut Vec<u32>,
    records: &[u32],
    width: u32,
    height: u32,
) -> bool {
    const WORDS: usize = 2_048;
    let tiles_x = width.div_ceil(16);
    let tiles_y = height.div_ceil(16);
    let Some(tile_count) = tiles_x.checked_mul(tiles_y) else {
        return false;
    };
    if tile_count == 0 || tile_count > 65_536 || records.is_empty() || records.len() % 9 != 0 {
        return false;
    }
    bits.resize(WORDS, 0);
    bits.fill(0);
    for record in records.chunks_exact(9) {
        let tile = record[0];
        if tile >= tile_count {
            return false;
        }
        let word = usize::try_from(tile >> 5).ok();
        let Some(word) = word.filter(|word| *word < WORDS) else {
            return false;
        };
        bits[word] |= 1u32 << (tile & 31);
    }
    true
}

#[cfg(feature = "wgpu")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Native-bin occupancy calculated while packing an active selector.
pub(crate) struct SparseCoarseActiveBins {
    pub native_bins: u32,
    pub selected_bins: u32,
}

#[cfg(feature = "wgpu")]
pub(crate) fn sparse_coarse_native_bin_count(width: u32, height: u32) -> Option<u32> {
    let tiles_x = width.div_ceil(16);
    let tiles_y = height.div_ceil(16);
    let tile_count = tiles_x.checked_mul(tiles_y)?;
    if tile_count == 0 || tile_count > 65_536 {
        return None;
    }
    let native_bins = tiles_x.div_ceil(16).checked_mul(tiles_y.div_ceil(16))?;
    (native_bins <= 256).then_some(native_bins)
}

#[cfg(feature = "wgpu")]
/// Pack a unique native-bin list after the used selector words.
///
/// The fixed selector supports at most 65,536 tiles. The existing binning
/// stage supports at most 256 native bins. The bitmap is indexed by native
/// bin id because row-major tile ids repeat bin ids across a bin band.
pub(crate) fn sparse_coarse_active_bin_list(
    bits: &mut Vec<u32>,
    records: &[u32],
    width: u32,
    height: u32,
) -> Option<SparseCoarseActiveBins> {
    const SELECTOR_WORDS: usize = 2_048;
    const MAX_NATIVE_BINS: usize = 256;
    const TILE: u32 = 16;

    if !sparse_coarse_tile_bits(bits, records, width, height) {
        return None;
    }
    let tiles_x = width.div_ceil(TILE);
    let tiles_y = height.div_ceil(TILE);
    let tile_count = tiles_x.checked_mul(tiles_y)?;
    let selector_words = usize::try_from(tile_count.div_ceil(32)).ok()?;
    let width_in_bins = tiles_x.div_ceil(TILE);
    let native_bin_count = usize::try_from(sparse_coarse_native_bin_count(width, height)?).ok()?;
    if native_bin_count > MAX_NATIVE_BINS || selector_words >= SELECTOR_WORDS {
        return None;
    }

    let mut native_bins = [0u64; MAX_NATIVE_BINS / 64];
    for record in records.chunks_exact(9) {
        let tile = record[0];
        if tile >= tile_count {
            return None;
        }
        let tile_x = tile % tiles_x;
        let tile_y = tile / tiles_x;
        let bin = (tile_y / TILE)
            .checked_mul(width_in_bins)?
            .checked_add(tile_x / TILE)?;
        let bin = usize::try_from(bin).ok()?;
        if bin >= native_bin_count {
            return None;
        }
        native_bins[bin / 64] |= 1u64 << (bin % 64);
    }

    let active_count = native_bins
        .iter()
        .map(|word| word.count_ones())
        .sum::<u32>();
    let active_count = usize::try_from(active_count).ok()?;
    if active_count == 0 || active_count > SELECTOR_WORDS - selector_words {
        return None;
    }
    let mut next = selector_words;
    for bin in 0..native_bin_count {
        if native_bins[bin / 64] & (1u64 << (bin % 64)) != 0 {
            bits[next] = u32::try_from(bin).ok()?;
            next += 1;
        }
    }
    Some(SparseCoarseActiveBins {
        native_bins: u32::try_from(native_bin_count).ok()?,
        selected_bins: u32::try_from(active_count).ok()?,
    })
}

#[cfg(feature = "wgpu")]
pub(crate) fn sparse_coarse_active_coverage(native_bins: u32, selected_bins: u32) -> bool {
    selected_bins != 0
        && selected_bins.saturating_mul(2) <= native_bins
        && native_bins.saturating_sub(selected_bins) >= 32
}

#[cfg(feature = "wgpu")]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SparseCoarseAdaptiveSelection {
    pub bins: Option<SparseCoarseActiveBins>,
    pub active: bool,
}

#[cfg(feature = "wgpu")]
pub(crate) fn sparse_coarse_adaptive_selection(
    bits: &mut Vec<u32>,
    records: &[u32],
    width: u32,
    height: u32,
) -> SparseCoarseAdaptiveSelection {
    let Some(native_bins) = sparse_coarse_native_bin_count(width, height) else {
        return SparseCoarseAdaptiveSelection::default();
    };
    if native_bins <= 32 {
        return SparseCoarseAdaptiveSelection::default();
    }
    let bins = sparse_coarse_active_bin_list(bits, records, width, height);
    SparseCoarseAdaptiveSelection {
        active: bins.is_some_and(|bins| {
            sparse_coarse_active_coverage(bins.native_bins, bins.selected_bins)
        }),
        bins,
    }
}

#[cfg(feature = "wgpu")]
impl Renderer {
    /// Creates a new renderer for the specified device.
    pub fn new(device: &Device, options: RendererOptions) -> Result<Self> {
        let mut engine = WgpuEngine::new(options.use_cpu, options.pipeline_cache.clone());
        engine.initialize_gpu_timestamps(device);
        // If we are running in parallel (i.e. the number of threads is not 1)
        if options.num_init_threads != NonZeroUsize::new(1) {
            #[cfg(not(target_arch = "wasm32"))]
            engine.use_parallel_initialisation();
        }
        let shaders = shaders::full_shaders(device, &mut engine, &options)?;
        #[cfg(not(target_arch = "wasm32"))]
        engine.build_shaders_if_needed(device, options.num_init_threads);
        #[cfg(feature = "debug_layers")]
        let debug = debug::DebugRenderer::new(device, wgpu::TextureFormat::Rgba8Unorm, &mut engine);
        let mut budget = bump_budget::BudgetTracker::new();
        budget.set_device_limits(&device.limits());

        Ok(Self {
            options,
            engine,
            resolver: Resolver::new(),
            image_atlas: None,
            recording: Recording::default(),
            packed_scene: Vec::new(),
            shaders,
            budget,
            unsettled_bump_overflow: OverflowLatch::default(),
            sparse_masks: Vec::new(),
            sparse_records: Vec::new(),
            sparse_coarse_tiles: Vec::new(),
            defer_bump_readback: std::env::var_os("VELLO_DEFER_BUMP_READBACK")
                .is_some_and(|value| value == "1"),
            sparse_upload_reuse: default_enabled_from_env("VELLO_SPARSE_UPLOAD_REUSE"),
            sparse_upload: None,
            segment_order_replay_shader: None,
            last_canonical_segment_order_selected: false,
            last_area_ppt8_selected: false,
            last_clip_single_group_selected: false,
            #[cfg(feature = "debug_layers")]
            debug,
            #[cfg(feature = "wgpu-profiler")]
            profiler: GpuProfiler::new(device, GpuProfilerSettings::default())?,
            #[cfg(feature = "wgpu-profiler")]
            profile_result: None,
        })
    }

    /// Return the GPU allocations whose byte sizes are directly known to the
    /// renderer. This is intended for performance budgets and diagnostics.
    pub fn memory_stats(&self) -> RendererMemoryStats {
        let (pooled_buffer_bytes, live_buffer_bytes, image_bytes) =
            self.engine.tracked_memory_bytes();
        RendererMemoryStats {
            bump_budget_bytes: self.budget.effective_size_in_bytes(),
            pooled_buffer_bytes,
            live_buffer_bytes,
            image_bytes,
            sparse_upload_bytes: self
                .sparse_upload
                .as_ref()
                .map_or(0, |upload| upload.capacity_bytes),
        }
    }

    /// Limit the image atlas to `max_size` pixels per side (8192 by
    /// default). Images that do not fit after eviction are omitted and
    /// reported, instead of a 256 MB atlas taking a browser's GPU process
    /// past its memory limit.
    pub fn set_image_atlas_max(&mut self, max_size: u32) {
        self.resolver.set_image_atlas_max(max_size);
    }

    /// Grow the bump buffers at most once, straight to the
    /// [`Self::set_bump_budget_limit`] total, instead of doubling as a scene
    /// fills up. Each growth reallocates every buffer, which a browser's GPU
    /// process briefly holds twice.
    pub fn set_bump_budget_grow_once(&mut self, grow_once: bool) {
        self.budget.set_grow_once(grow_once);
    }

    /// Limit the adaptive bump buffers to `bytes` in total, whatever frames
    /// report using. Scenes past it drop geometry, which the shaders report,
    /// instead of taking a browser's GPU process past its memory limit.
    /// Viewport floors come on top.
    pub fn set_bump_budget_limit(&mut self, bytes: u64) {
        self.budget.set_max_total_bytes(bytes);
        self.engine.clear_pool();
    }

    /// Drop transient GPU buffers held only for reuse. In-flight command
    /// buffers keep their own references, so this is safe at a damage-shape
    /// transition and prevents obsolete size classes living forever.
    pub fn trim_pooled_buffers(&mut self) {
        self.engine.clear_pool();
    }

    /// Whether shared compute-pass encoding is effective for future recordings.
    ///
    /// This is enabled by default in release GPU rendering. Full CPU and
    /// profiler builds retain their unbatched fallback paths.
    #[doc(hidden)]
    pub fn compute_pass_batching_enabled(&self) -> bool {
        self.engine.compute_pass_batching_enabled()
    }

    /// Enable or disable shared compute-pass encoding for future recordings.
    /// Intended for A/B verification only.
    #[doc(hidden)]
    pub fn set_compute_pass_batching(&mut self, enabled: bool) {
        self.engine.set_compute_pass_batching(enabled);
    }

    /// Whether the default GPU direct path-count/tiling dispatch is active.
    #[doc(hidden)]
    pub fn direct_path_dispatch_enabled(&self) -> bool {
        !self.options.use_cpu && self.engine.direct_path_dispatch_enabled()
    }

    /// Effective initialization-time grid for direct path-count and path-tiling dispatches.
    #[doc(hidden)]
    pub fn direct_path_workgroups(&self) -> u32 {
        self.engine.direct_path_workgroups()
    }

    /// Whether no-ramp fine passes reuse one engine-owned zero gradient.
    #[doc(hidden)]
    pub fn zero_gradient_reuse_enabled(&self) -> bool {
        self.engine.zero_gradient_reuse_enabled()
    }

    /// Toggle direct GPU path-count/tiling dispatch for A/B comparison.
    #[doc(hidden)]
    pub fn set_direct_path_dispatch_enabled(&mut self, enabled: bool) {
        self.engine.set_direct_path_dispatch_enabled(enabled);
        self.shaders.direct_path_dispatch_enabled =
            !self.options.use_cpu && self.engine.direct_path_dispatch_enabled();
    }

    /// Whether synchronous GPU recording defers its existing bump readback
    /// until after fine rasterization. This is an opt-in command-order A/B;
    /// it does not apply to full CPU or asynchronous rendering.
    #[doc(hidden)]
    pub fn defer_bump_readback_enabled(&self) -> bool {
        !self.options.use_cpu && self.defer_bump_readback
    }

    /// Sum every tile's segments in a fixed order, so repeated renders of one
    /// scene are bit-identical regardless of GPU scheduling. Same as
    /// `VELLO_CANONICAL_SEGMENT_ORDER=1`; recompiles the shader set.
    ///
    /// # Errors
    /// Returns an error if the shader set fails to compile.
    #[doc(hidden)]
    pub fn enable_canonical_segment_order(&mut self, device: &Device) -> Result<()> {
        if self.engine.canonical_segment_order_enabled() {
            return Ok(());
        }
        self.engine.enable_canonical_segment_order();
        self.shaders = shaders::full_shaders(device, &mut self.engine, &self.options)?;
        #[cfg(not(target_arch = "wasm32"))]
        self.engine
            .build_shaders_if_needed(device, self.options.num_init_threads);
        Ok(())
    }

    /// Override deferred synchronous bump readback for a differential oracle.
    #[doc(hidden)]
    pub fn set_defer_bump_readback_enabled(&mut self, enabled: bool) {
        self.defer_bump_readback = enabled;
    }

    /// Return the state of the opt-in `VELLO_GPU_TIMESTAMPS=1` diagnostic.
    #[doc(hidden)]
    pub fn gpu_timestamp_status(&mut self) -> GpuTimestampStatus {
        self.engine.gpu_timestamp_status()
    }

    /// Consume the ready opt-in GPU timestamp sample, if its map callback has
    /// completed. This also unmaps its bounded readback buffer for reuse.
    #[doc(hidden)]
    pub fn take_gpu_timestamp_sample(&mut self) -> Option<GpuTimestampSample> {
        self.engine.take_gpu_timestamp_sample()
    }

    /// Identity assigned to the latest Vello recording when timestamp
    /// diagnostics are enabled. It advances even if capture was skipped
    /// because the one bounded readback slot was busy.
    #[doc(hidden)]
    pub fn gpu_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.engine.gpu_timestamp_latest_submission_id()
    }

    /// Bytes held by the opt-in timestamp resolve and readback buffers.
    #[doc(hidden)]
    pub fn gpu_timestamp_buffer_bytes(&self) -> u64 {
        self.engine.gpu_timestamp_buffer_bytes()
    }

    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_status(&mut self) -> GpuDispatchTimestampStatus {
        self.engine.gpu_dispatch_timestamp_status()
    }

    #[doc(hidden)]
    pub fn take_gpu_dispatch_timestamp_sample(&mut self) -> Option<GpuDispatchTimestampSample> {
        self.engine.take_gpu_dispatch_timestamp_sample()
    }

    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_latest_submission_id(&self) -> Option<u64> {
        self.engine.gpu_dispatch_timestamp_latest_submission_id()
    }

    /// Bytes held by the opt-in per-dispatch timestamp resolve and readback
    /// buffers, excluding opaque query metadata.
    #[doc(hidden)]
    pub fn gpu_dispatch_timestamp_buffer_bytes(&self) -> u64 {
        self.engine.gpu_dispatch_timestamp_buffer_bytes()
    }

    /// Returns the exact packed scene bytes in the most recently encoded recording.
    ///
    /// The bytes are the resolver output submitted as `vello.scene`, not the
    /// source [`Scene`] encoding. They remain borrowed from this renderer only
    /// until its next render begins; their layout is diagnostic-only and not a
    /// stable public format. Calling this performs no rendering work or copy.
    #[doc(hidden)]
    pub fn diagnostic_packed_scene(&self) -> &[u8] {
        self.recording
            .commands
            .iter()
            .rev()
            .find_map(|command| match command {
                recording::Command::Upload(buffer, bytes) if buffer.name == "vello.scene" => {
                    Some(bytes.as_slice())
                }
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Borrow the four resolver-expanded input spans from the latest encoded recording.
    ///
    /// This is a profile-only opportunity diagnostic. It reads existing recording uploads and
    /// never retains GPU resources, changes budget state, or claims a reusable line count.
    #[doc(hidden)]
    pub fn diagnostic_geometry_input(&self) -> Result<GeometryInput<'_>, GeometryInputError> {
        let (scene, config_bytes) = geometry_recording_uploads(&self.recording)?;
        geometry_input_from_uploads(
            scene,
            config_bytes,
            self.options.use_cpu,
            self.budget.has_pending_readback(),
        )
    }

    /// Whether default sparse Area-AA fine shaders are available for
    /// [`Self::render_to_texture_with_damage`].
    #[doc(hidden)]
    pub fn sparse_fine_supported(&self) -> bool {
        self.engine.sparse_fine_enabled()
            && !self.options.use_cpu
            && self.shaders.fine_sparse_area.is_some()
    }

    /// Whether the exact opt-in eight-pixel Area fine experiment was requested.
    #[doc(hidden)]
    pub fn area_ppt8_requested(&self) -> bool {
        self.engine.area_ppt8_enabled()
    }

    /// Whether the ordinary synchronous GPU Area full route has a PPT8 shader available.
    #[doc(hidden)]
    pub fn area_ppt8_available(&self) -> bool {
        self.area_ppt8_requested() && !self.options.use_cpu && self.shaders.fine_area_ppt8.is_some()
    }

    /// Whether the latest synchronous ordinary submission recorded a PPT8 fine shader.
    #[doc(hidden)]
    pub fn last_area_ppt8_selected(&self) -> bool {
        self.last_area_ppt8_selected
    }

    /// Whether the exact opt-in single-workgroup clip leaf experiment was requested.
    #[doc(hidden)]
    pub fn clip_single_group_requested(&self) -> bool {
        self.engine.clip_single_group_enabled()
    }

    /// Whether the GPU single-workgroup clip leaf permutation was configured.
    #[doc(hidden)]
    pub fn clip_single_group_available(&self) -> bool {
        self.clip_single_group_requested()
            && !self.options.use_cpu
            && self.shaders.clip_leaf_single_group.is_some()
    }

    /// Whether the latest synchronous submission recorded the specialised clip leaf shader.
    #[doc(hidden)]
    pub fn last_clip_single_group_selected(&self) -> bool {
        self.last_clip_single_group_selected
    }

    fn area_ppt8_selected_for(&self, params: &RenderParams) -> bool {
        params.antialiasing_method == AaConfig::Area
            && self.area_ppt8_available()
            && !self.sparse_fine_quad_skip_requested()
            && !self.sparse_opaque_prune_requested()
            && !self.sparse_solid_fuse_requested()
            && !self.sparse_segment_row_mask_requested()
            && !self.canonical_segment_order_requested()
    }

    fn sparse_area_ppt8_selected_for(&self, params: &RenderParams) -> bool {
        self.area_ppt8_selected_for(params) && self.shaders.fine_sparse_area_ppt8.is_some()
    }

    /// Whether canonical raw-bit segment ordering was requested for synchronous GPU Area paths.
    #[doc(hidden)]
    pub fn canonical_segment_order_requested(&self) -> bool {
        self.engine.canonical_segment_order_enabled()
    }

    /// Whether the GPU renderer has the ordinary full canonical-order shader.
    ///
    /// Sparse selection additionally requires its matching sparse permutation
    /// at recording time and otherwise falls back without claiming selection.
    #[doc(hidden)]
    pub fn canonical_segment_order_available(&self) -> bool {
        self.canonical_segment_order_requested()
            && !self.options.use_cpu
            && self.shaders.fine_area.is_some()
            && self.shaders.canonical_segment_order_full.is_some()
    }

    /// Whether the latest synchronous submission actually recorded the canonical-order pass.
    #[doc(hidden)]
    pub fn last_canonical_segment_order_selected(&self) -> bool {
        self.last_canonical_segment_order_selected
    }

    fn canonical_segment_order_selected_for(&self, params: &RenderParams, measure: bool) -> bool {
        measure
            && params.antialiasing_method == AaConfig::Area
            && self.canonical_segment_order_available()
    }

    /// Whether the Area sparse fine quad-skip experiment was requested.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_requested(&self) -> bool {
        self.engine.sparse_fine_quad_skip_enabled()
    }

    /// Whether the Area sparse fine quad-skip permutation can be selected.
    #[doc(hidden)]
    pub fn sparse_fine_quad_skip_available(&self) -> bool {
        self.sparse_fine_quad_skip_requested()
            && self.sparse_fine_supported()
            && self.shaders.fine_sparse_area_quad_skip.is_some()
    }

    /// Whether the opaque full-tile coarse pruning experiment was requested.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_requested(&self) -> bool {
        self.engine.sparse_opaque_prune_enabled()
    }

    /// Whether the normal dense-grid opaque-prune permutation is available.
    ///
    /// Sparse coarse modes validate their matching permutation at dispatch
    /// selection time and otherwise retain their ordinary mode.
    #[doc(hidden)]
    pub fn sparse_opaque_prune_available(&self) -> bool {
        self.sparse_opaque_prune_requested()
            && self.sparse_fine_supported()
            && self.shaders.coarse_opaque_prune.is_some()
    }

    /// Whether the sparse SOLID-fuse experiment was requested.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_requested(&self) -> bool {
        self.engine.sparse_solid_fuse_enabled()
    }

    /// Whether the normal paired sparse coarse/fine SOLID-fuse path is available.
    #[doc(hidden)]
    pub fn sparse_solid_fuse_available(&self) -> bool {
        self.sparse_solid_fuse_requested()
            && !self.sparse_fine_quad_skip_requested()
            && !self.sparse_opaque_prune_requested()
            && self.sparse_fine_supported()
            && self.shaders.coarse_solid_fuse.is_some()
            && self.shaders.fine_sparse_area_solid_fuse.is_some()
    }

    /// Whether the sparse segment-row-mask experiment was requested.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_requested(&self) -> bool {
        self.engine.sparse_segment_row_mask_enabled()
    }

    /// Whether matching sparse segment-row-mask writer and reader permutations exist.
    #[doc(hidden)]
    pub fn sparse_segment_row_mask_available(&self) -> bool {
        self.sparse_segment_row_mask_requested()
            && !self.sparse_fine_quad_skip_requested()
            && !self.sparse_opaque_prune_requested()
            && !self.sparse_solid_fuse_requested()
            && self.sparse_tile_bounds_available()
            && self.shaders.fine_sparse_area_segment_row_mask.is_some()
            && if self.shaders.direct_path_dispatch_enabled {
                self.shaders.path_tiling_direct_segment_row_mask.is_some()
            } else {
                self.shaders.path_tiling_segment_row_mask.is_some()
            }
    }

    #[cfg(feature = "wgpu")]
    fn sparse_opaque_prune_selected_for(
        &self,
        dispatch: Option<render::SparseCoarseDispatch<'_>>,
    ) -> bool {
        render::sparse_opaque_prune_shader_selected(
            &self.shaders,
            dispatch,
            self.sparse_opaque_prune_requested(),
        )
    }

    #[cfg(feature = "wgpu")]
    fn sparse_solid_fuse_selected_for(
        &self,
        dispatch: Option<render::SparseCoarseDispatch<'_>>,
    ) -> bool {
        render::sparse_solid_fuse_shader_selected(
            &self.shaders,
            dispatch,
            self.sparse_solid_fuse_requested(),
            self.sparse_fine_quad_skip_requested(),
            self.sparse_opaque_prune_requested(),
        )
    }

    #[cfg(feature = "wgpu")]
    fn sparse_segment_row_mask_selected_for(
        &self,
        dispatch: Option<render::SparseCoarseDispatch<'_>>,
        width: u32,
        height: u32,
    ) -> bool {
        render::sparse_segment_row_mask_shader_selected(
            &self.shaders,
            dispatch,
            self.sparse_segment_row_mask_requested(),
            self.sparse_fine_quad_skip_requested(),
            self.sparse_opaque_prune_requested(),
            self.sparse_solid_fuse_requested(),
            width,
            height,
        )
    }

    #[doc(hidden)]
    pub fn sparse_coarse_supported(&self) -> bool {
        self.sparse_fine_supported() && self.shaders.coarse_sparse.is_some()
    }

    /// Whether the opt-in active native-bin sparse-coarse permutation is available.
    ///
    /// A particular frame can still use the legacy sparse-coarse shader when
    /// its unique native-bin list cannot fit the fixed selector uniform.
    #[doc(hidden)]
    pub fn sparse_coarse_active_bins_available(&self) -> bool {
        self.sparse_coarse_supported() && self.shaders.coarse_sparse_active.is_some()
    }

    #[doc(hidden)]
    pub fn sparse_coarse_adaptive_enabled(&self) -> bool {
        self.engine.sparse_coarse_adaptive_enabled() && self.sparse_coarse_active_bins_available()
    }

    /// Whether the bounded path-tile allocation experiment was requested.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_requested(&self) -> bool {
        self.engine.sparse_tile_bounds_enabled()
    }

    /// Whether the bounded path-tile allocation permutation can be selected.
    #[doc(hidden)]
    pub fn sparse_tile_bounds_available(&self) -> bool {
        self.sparse_tile_bounds_requested()
            && self.sparse_coarse_active_bins_available()
            && self.shaders.tile_alloc_sparse.is_some()
    }

    /// Whether selective CPU path-tag processing is active (not full CPU rendering).
    #[doc(hidden)]
    pub fn cpu_pathtag_enabled(&self) -> bool {
        !self.options.use_cpu && self.engine.cpu_pathtag_enabled()
    }

    /// Toggle selective CPU path-tag processing while retaining GPU coarse/fine work.
    #[doc(hidden)]
    pub fn set_cpu_pathtag_enabled(&mut self, enabled: bool) {
        self.engine.set_cpu_pathtag_enabled(enabled);
        self.shaders.pathtag_is_cpu = self.options.use_cpu || self.engine.cpu_pathtag_enabled();
        self.shaders.cpu_metadata_enabled =
            !self.options.use_cpu && self.engine.cpu_metadata_enabled();
        self.shaders.cpu_draw_prefix_enabled =
            !self.options.use_cpu && self.engine.cpu_draw_prefix_enabled();
    }

    /// Whether bounded selective CPU path-tag host-buffer reuse is active.
    #[doc(hidden)]
    pub fn cpu_pathtag_reuse_enabled(&self) -> bool {
        !self.options.use_cpu && self.engine.cpu_pathtag_reuse_enabled()
    }

    /// Retained host allocation capacity for selective CPU path-tag reuse.
    /// This is deliberately separate from [`RendererMemoryStats`], which is GPU-only.
    #[doc(hidden)]
    pub fn cpu_pathtag_pool_retained_bytes(&self) -> u64 {
        self.engine.cpu_pathtag_pool_retained_bytes()
    }

    /// Whether selective CPU bbox-clear and draw-reduce metadata is active.
    #[doc(hidden)]
    pub fn cpu_metadata_enabled(&self) -> bool {
        !self.options.use_cpu && self.engine.cpu_metadata_enabled()
    }

    /// Toggle selective CPU metadata while keeping flatten and later stages on GPU.
    #[doc(hidden)]
    pub fn set_cpu_metadata_enabled(&mut self, enabled: bool) {
        self.engine.set_cpu_metadata_enabled(enabled);
        self.shaders.cpu_metadata_enabled =
            !self.options.use_cpu && self.engine.cpu_metadata_enabled();
    }

    /// Retained host capacity for selective CPU metadata, excluded from GPU stats.
    #[doc(hidden)]
    pub fn cpu_metadata_pool_retained_bytes(&self) -> u64 {
        self.engine.cpu_metadata_pool_retained_bytes()
    }

    /// Whether the bounded selective CPU draw-prefix path is active.
    #[doc(hidden)]
    pub fn cpu_draw_prefix_enabled(&self) -> bool {
        !self.options.use_cpu && self.engine.cpu_draw_prefix_enabled()
    }

    /// Toggle the bounded selective CPU draw-prefix path.
    #[doc(hidden)]
    pub fn set_cpu_draw_prefix_enabled(&mut self, enabled: bool) {
        self.engine.set_cpu_draw_prefix_enabled(enabled);
        self.shaders.cpu_draw_prefix_enabled =
            !self.options.use_cpu && self.engine.cpu_draw_prefix_enabled();
    }

    /// Retained host capacity for bounded selective CPU draw-prefix reuse.
    #[doc(hidden)]
    pub fn cpu_draw_prefix_pool_retained_bytes(&self) -> u64 {
        self.engine.cpu_draw_prefix_pool_retained_bytes()
    }

    /// Whether a draw-prefix table of `bytes` fits the bounded CPU path.
    #[doc(hidden)]
    pub fn cpu_draw_prefix_supported(&self, bytes: u64) -> bool {
        !self.options.use_cpu && self.engine.cpu_draw_prefix_supported(bytes)
    }

    /// Enable or disable sparse fine recording after construction. Enabling is
    /// effective only when the renderer was initially created with Area-AA
    /// support and therefore compiled the default sparse shader.
    #[doc(hidden)]
    pub fn set_sparse_fine_enabled(&mut self, enabled: bool) {
        self.engine.set_sparse_fine_enabled(enabled);
    }

    #[doc(hidden)]
    pub fn sparse_upload_reuse_enabled(&self) -> bool {
        self.sparse_upload_reuse && self.sparse_fine_supported()
    }

    fn prepare_sparse_upload(
        &mut self,
        device: &Device,
        queue: &Queue,
    ) -> Option<recording::BufferProxy> {
        if !self.sparse_upload_reuse {
            return None;
        }
        let byte_len = u64::try_from(self.sparse_records.len())
            .ok()?
            .checked_mul(4)?;
        if byte_len == 0 || byte_len > 9 * 65_536 * 4 {
            return None;
        }
        let needs_replacement = self
            .sparse_upload
            .as_ref()
            .is_none_or(|upload| upload.capacity_bytes < byte_len);
        if needs_replacement {
            let capacity_bytes = byte_len.checked_next_power_of_two()?;
            if capacity_bytes > 9 * 65_536 * 4
                || capacity_bytes > u64::from(device.limits().max_storage_buffer_binding_size)
            {
                return None;
            }
            if let Some(upload) = self.sparse_upload.as_ref() {
                self.engine.invalidate_buffer_bindings(&upload.buffer);
            }
            self.sparse_upload = Some(SparseTileUpload {
                buffer: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("vello.damage_tiles.persistent"),
                    size: capacity_bytes,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                }),
                capacity_bytes,
            });
        }
        let upload = self.sparse_upload.as_ref()?;
        queue.write_buffer(
            &upload.buffer,
            0,
            bytemuck::cast_slice::<u32, u8>(&self.sparse_records),
        );
        Some(recording::BufferProxy::new(byte_len, "vello.damage_tiles"))
    }

    /// Release cached bind groups referencing a scratch texture that its
    /// owner is about to replace. Other bind groups and GPU buffer pools stay
    /// warm, so animated damage resizing does not become a global cache flush.
    pub fn invalidate_texture_bindings(&mut self, texture: &TextureView) {
        self.engine.invalidate_texture_bindings(texture);
    }

    /// Renders a scene to the target texture.
    ///
    /// The texture is assumed to be of the specified dimensions and have been created with
    /// the [`wgpu::TextureFormat::Rgba8Unorm`] format and the [`wgpu::TextureUsages::STORAGE_BINDING`]
    /// flag set.
    ///
    /// If you want to render Vello content to a surface (such as in a UI toolkit), you have two options:
    /// 1) Render to an intermediate texture, which is the same size as the surface.
    ///    You would then use [`TextureBlitter`][wgpu::util::TextureBlitter] to blit the rendered result from
    ///    that texture to the surface.
    ///    This pattern is supported by the [`util`] module.
    /// 2) Call `render_to_texture` directly on the [`SurfaceTexture`][wgpu::SurfaceTexture]'s texture, if
    ///    it has the right usages. This should generally be avoided, as some GPUs assume that you will not
    ///    be rendering to the surface using a compute pipeline, and optimise accordingly.
    pub fn render_to_texture(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
    ) -> Result<()> {
        self.render_to_texture_with_post_render(device, queue, scene, texture, params, |_| {})
    }

    /// Render a scene and append caller commands to Vello's command encoder
    /// before its single queue submission.
    ///
    /// This is intended for tightly coupled composition such as copying an
    /// offscreen Vello target into a retained texture. The callback runs only
    /// after Vello has encoded the whole recording and before submission, so
    /// its commands execute after the render without requiring another queue
    /// submission.
    /// Encode with an optional final submission gate. A rejected recording requires
    /// renderer reconstruction; no counter/timestamp readback is started for it.
    #[doc(hidden)]
    pub fn render_to_texture_with_post_render_checked<F, G>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        post_render: F,
        before_submit: G,
    ) -> Result<()>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
        G: FnOnce() -> bool,
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        // Collect the previous frame's bump counters before choosing
        // this frame's reservation, so a scene that has grown is covered
        // by the frame after the one that grew it. When the reservation
        // changes, the pooled buffers at the old sizes are dropped
        // rather than kept for a size nothing will ask for again.
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        // Downloads inside a recording are not supported by the CPU-dispatch
        // engine. The CPU path still uses the GPU for the fine pass, but its
        // coarse buffers are host allocations and do not need GPU-side bump
        // measurement.
        let measure = !self.options.use_cpu && self.budget.wants_readback();
        let canonical_segment_order = self.canonical_segment_order_selected_for(params, measure);
        let area_ppt8 = self.area_ppt8_selected_for(params);
        let defer_bump_readback = self.defer_bump_readback_enabled() || canonical_segment_order;
        let zero_gradient = if self.engine.zero_gradient_reuse_enabled() {
            Some(self.engine.ensure_zero_gradient(device, queue))
        } else {
            None
        };
        let (
            target,
            bump_buf,
            canonical_segment_order_selected,
            area_ppt8_selected,
            clip_single_group_selected,
        ) = render::render_full(
            scene,
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            canonical_segment_order,
            area_ppt8,
            defer_bump_readback,
            zero_gradient,
        );
        let external_resources = [ExternalResource::Image(
            *target.as_image().unwrap(),
            texture,
        )];
        self.engine.run_recording_with_post_render_checked(
            device,
            queue,
            &self.recording,
            &external_resources,
            "render_to_texture",
            post_render,
            before_submit,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        self.last_canonical_segment_order_selected = canonical_segment_order_selected;
        self.last_area_ppt8_selected = area_ppt8_selected;
        self.last_clip_single_group_selected = clip_single_group_selected;
        // N.B. This is horrible; this integration of wgpu-profiler really needs some work...
        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }

        Ok(())
    }

    /// As [`Self::render_to_texture_with_post_render_checked`], returning the
    /// exact queue submission that contains both Vello and `post_render` work.
    #[doc(hidden)]
    pub fn render_to_texture_with_post_render_checked_indexed<F, G>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        post_render: F,
        before_submit: G,
    ) -> Result<wgpu::SubmissionIndex>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
        G: FnOnce() -> bool,
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = !self.options.use_cpu && self.budget.wants_readback();
        let canonical_segment_order = self.canonical_segment_order_selected_for(params, measure);
        let area_ppt8 = self.area_ppt8_selected_for(params);
        let defer_bump_readback = self.defer_bump_readback_enabled() || canonical_segment_order;
        let zero_gradient = self
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| self.engine.ensure_zero_gradient(device, queue));
        let (target, bump_buf, canonical, ppt8, clip_single) = render::render_full(
            scene,
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            canonical_segment_order,
            area_ppt8,
            defer_bump_readback,
            zero_gradient,
        );
        let external_resources = [ExternalResource::Image(
            *target.as_image().unwrap(),
            texture,
        )];
        let submission = self
            .engine
            .run_recording_with_post_render_checked_indexed(
                device,
                queue,
                &self.recording,
                &external_resources,
                "render_to_texture",
                post_render,
                before_submit,
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        self.last_canonical_segment_order_selected = canonical;
        self.last_area_ppt8_selected = ppt8;
        self.last_clip_single_group_selected = clip_single;
        Ok(submission)
    }

    /// Append post-render commands to the same unconditional Vello submission.
    #[doc(hidden)]
    pub fn render_to_texture_with_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        post_render: F,
    ) -> Result<()>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.render_to_texture_with_post_render_checked(
            device,
            queue,
            scene,
            texture,
            params,
            post_render,
            || true,
        )
    }

    /// Render only exact native-pixel damage through the opt-in sparse Area
    /// fine pass. `None` means no command was submitted and callers must use
    /// their ordinary dense path.
    #[doc(hidden)]
    pub fn render_to_texture_with_damage(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
    ) -> Result<Option<SparseRenderStats>> {
        self.render_to_texture_with_damage_and_post_render(
            device,
            queue,
            scene,
            texture,
            params,
            damage,
            |_| {},
        )
    }

    /// Diagnostic: compare dense and sparse Area fine passes from one coarse recording.
    ///
    /// Both outputs are native textures; sparse pixels outside `damage` are
    /// unspecified and must not be compared. `None` means no submission was
    /// made and the callback was not invoked. Overflow follows the ordinary
    /// renderer settlement contract and must be settled by the caller.
    #[doc(hidden)]
    pub fn render_to_textures_shared_coarse_area_with_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        full_texture: &TextureView,
        sparse_texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        post_render: F,
    ) -> Result<Option<SparseRenderStats>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if !self.sparse_fine_supported() || params.antialiasing_method != AaConfig::Area {
            return Ok(None);
        }
        let Some(stats) = sparse_tile_records(
            &mut self.sparse_masks,
            &mut self.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            return Ok(None);
        };
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = self.budget.wants_readback();
        let defer_bump_readback = self.defer_bump_readback_enabled();
        let zero_gradient = self
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| self.engine.ensure_zero_gradient(device, queue));
        let mut render = Render::new();
        render.set_clip_single_group_allowed(false);
        render.render_encoding_coarse_with_dispatch(
            scene.encoding(),
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            false,
            None,
            defer_bump_readback,
            zero_gradient,
        );
        let bump = render.bump_buf();
        let bump_buf = measure.then_some(bump);
        let (full_proxy, sparse_proxy, _) = render.record_fine_area_pair(
            &self.shaders,
            &mut self.recording,
            &self.sparse_records,
            false,
        );
        if defer_bump_readback {
            render::finish_bump_readback(&mut self.recording, bump, measure);
        }
        let external_resources = [
            ExternalResource::Image(full_proxy, full_texture),
            ExternalResource::Image(sparse_proxy, sparse_texture),
        ];
        self.engine.run_recording_with_post_render(
            device,
            queue,
            &self.recording,
            &external_resources,
            "render_to_textures_shared_coarse_area",
            post_render,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }
        Ok(Some(stats))
    }

    /// Test-only P4/PPT8 differential capture. It shares exactly one coarse recording and keeps
    /// the public shared-coarse diagnostics on their ordinary P4 route.
    #[cfg(test)]
    fn render_to_textures_shared_coarse_area_ppt8_with_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        p4_full_texture: &TextureView,
        p8_full_texture: &TextureView,
        p4_sparse_texture: &TextureView,
        p8_sparse_texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        post_render: F,
    ) -> Result<Option<SparseRenderStats>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if !self.sparse_fine_supported()
            || params.antialiasing_method != AaConfig::Area
            || self.shaders.fine_area_ppt8.is_none()
            || self.shaders.fine_sparse_area_ppt8.is_none()
        {
            return Ok(None);
        }
        let Some(stats) = sparse_tile_records(
            &mut self.sparse_masks,
            &mut self.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            return Ok(None);
        };
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = self.budget.wants_readback();
        let defer_bump_readback = self.defer_bump_readback_enabled();
        let zero_gradient = self
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| self.engine.ensure_zero_gradient(device, queue));
        let mut render = Render::new();
        render.set_clip_single_group_allowed(false);
        render.render_encoding_coarse_with_dispatch(
            scene.encoding(),
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            false,
            None,
            defer_bump_readback,
            zero_gradient,
        );
        let bump = render.bump_buf();
        let bump_buf = measure.then_some(bump);
        let (p4_full, p8_full, p4_sparse, p8_sparse) = render.record_fine_area_ppt8_pair(
            &self.shaders,
            &mut self.recording,
            &self.sparse_records,
        );
        if defer_bump_readback {
            render::finish_bump_readback(&mut self.recording, bump, measure);
        }
        let resources = [
            ExternalResource::Image(p4_full, p4_full_texture),
            ExternalResource::Image(p8_full, p8_full_texture),
            ExternalResource::Image(p4_sparse, p4_sparse_texture),
            ExternalResource::Image(p8_sparse, p8_sparse_texture),
        ];
        self.engine.run_recording_with_post_render(
            device,
            queue,
            &self.recording,
            &resources,
            "render_to_textures_shared_coarse_area_ppt8",
            post_render,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        Ok(Some(stats))
    }

    /// Diagnostic shared-coarse rendering with bounded PTCL and segment downloads.
    #[doc(hidden)]
    pub fn render_to_textures_shared_coarse_area_with_geometry_and_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        full_texture: &TextureView,
        sparse_texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        post_render: F,
    ) -> Result<Option<(SparseRenderStats, SharedCoarseGeometryDownload)>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if !self.sparse_fine_supported() || params.antialiasing_method != AaConfig::Area {
            return Ok(None);
        }
        let Some(stats) = sparse_tile_records(
            &mut self.sparse_masks,
            &mut self.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            return Ok(None);
        };
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = self.budget.wants_readback();
        let defer = self.defer_bump_readback_enabled();
        // The capture cap is checked after coarse encoding determines the exact logical buffer
        // sizes. Keep the prior persistent atlas handle so an unsent recording is transactional.
        let prior_image_atlas = self.image_atlas;
        let zero = self
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| self.engine.ensure_zero_gradient(device, queue));
        let mut render = Render::new();
        render.set_clip_single_group_allowed(false);
        render.render_encoding_coarse_with_dispatch(
            scene.encoding(),
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            false,
            None,
            defer,
            zero,
        );
        let config = match diagnostic_geometry_config(&self.recording) {
            Ok(config) => config,
            Err(error) => {
                self.abort_unsent_geometry_capture(prior_image_atlas);
                return Err(error);
            }
        };
        let (ptcl_bytes, segments_bytes) = render.fine_geometry_bytes();
        let total = diagnostic_geometry_download_total(
            u64::try_from(config.len()).unwrap_or(u64::MAX),
            ptcl_bytes,
            segments_bytes,
        );
        if total > DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP {
            self.abort_unsent_geometry_capture(prior_image_atlas);
            return Err(Error::DiagnosticGeometryCaptureTooLarge(
                total,
                DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP,
            ));
        }
        let bump = render.bump_buf();
        let bump_buf = measure.then_some(bump);
        let (full_proxy, sparse_proxy, downloads) = render.record_fine_area_pair(
            &self.shaders,
            &mut self.recording,
            &self.sparse_records,
            true,
        );
        let (ptcl_proxy, segments_proxy) = downloads.ok_or(
            Error::DiagnosticGeometryCaptureMissing("geometry downloads"),
        )?;
        if defer {
            render::finish_bump_readback(&mut self.recording, bump, measure);
        }
        let resources = [
            ExternalResource::Image(full_proxy, full_texture),
            ExternalResource::Image(sparse_proxy, sparse_texture),
        ];
        let run_result = self.engine.run_recording_with_post_render(
            device,
            queue,
            &self.recording,
            &resources,
            "render_to_textures_shared_coarse_area_geometry",
            post_render,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        );
        if let Err(error) = run_result {
            // A recording error can happen after one of the copies was
            // materialized. Neither diagnostic buffer may outlive an error.
            let _ =
                take_diagnostic_geometry_downloads(&mut self.engine, ptcl_proxy, segments_proxy);
            return Err(error);
        }
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }
        let (ptcl, segments) =
            take_diagnostic_geometry_downloads(&mut self.engine, ptcl_proxy, segments_proxy)?;
        Ok(Some((
            stats,
            SharedCoarseGeometryDownload {
                config,
                ptcl,
                ptcl_bytes,
                segments,
                segments_bytes,
            },
        )))
    }

    /// Validate replay input and the bounded extra transfer budget before a caller allocates its
    /// second diagnostic target.
    #[doc(hidden)]
    pub fn segment_order_replay_preflight(
        width: u32,
        height: u32,
        input: &SegmentOrderReplayInput,
    ) -> Result<()> {
        validate_segment_order_replay_input(input)?;
        let tile_count = segment_order_replay_native_tile_count(width, height)?;
        if input.tile >= tile_count {
            return Err(Error::SegmentOrderReplayInvalid("native tile index"));
        }
        let transfer_bytes = segment_order_replay_transfer_bytes(width, height)?;
        if transfer_bytes > SEGMENT_ORDER_REPLAY_TRANSFER_CAP {
            return Err(Error::SegmentOrderReplayTooLarge(
                transfer_bytes,
                SEGMENT_ORDER_REPLAY_TRANSFER_CAP,
            ));
        }
        Ok(())
    }

    /// Diagnostic-only causal replay of two fixed segment orders through ordinary Area fine.
    ///
    /// The two passes share one freshly encoded coarse result, including its config, PTCL,
    /// information buffer, gradients, and image atlas. A bounded GPU guard verifies the current
    /// target fill's metadata and full segment multiset before it writes either supplied order.
    #[doc(hidden)]
    pub fn render_to_textures_segment_order_replay_with_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        left_texture: &TextureView,
        right_texture: &TextureView,
        params: &RenderParams,
        input: &SegmentOrderReplayInput,
        post_render: F,
    ) -> Result<Option<SegmentOrderReplayDownload>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if self.options.use_cpu || params.antialiasing_method != AaConfig::Area {
            return Ok(None);
        }
        Self::segment_order_replay_preflight(params.width, params.height, input)?;
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = self.budget.wants_readback();
        let defer = self.defer_bump_readback_enabled();
        let prior_image_atlas = self.image_atlas;
        let zero = self
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| self.engine.ensure_zero_gradient(device, queue));
        let mut render = Render::new();
        render.set_clip_single_group_allowed(false);
        render.render_encoding_coarse_with_dispatch(
            scene.encoding(),
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            false,
            None,
            defer,
            zero,
        );
        let (ptcl_bytes, segments_bytes) = render.fine_geometry_bytes();
        let control = match segment_order_replay_control(
            input,
            ptcl_bytes,
            segments_bytes,
            params.width,
            params.height,
        ) {
            Ok(control) => control,
            Err(error) => {
                self.abort_unsent_geometry_capture(prior_image_atlas);
                return Err(error);
            }
        };
        let patch_shader = self.segment_order_replay_shader(device);
        let expected = self.recording.upload(
            "vello.segment_order_replay_expected",
            segment_order_replay_words(&input.expected),
        );
        let left = self.recording.upload(
            "vello.segment_order_replay_left",
            segment_order_replay_words(&input.left),
        );
        let right = self.recording.upload(
            "vello.segment_order_replay_right",
            segment_order_replay_words(&input.right),
        );
        let control = self.recording.upload_uniform(
            "vello.segment_order_replay_control",
            bytemuck::cast_slice(&control).to_vec(),
        );
        let witness_bytes = u64::try_from(SEGMENT_ORDER_REPLAY_WITNESS_WORDS)
            .unwrap_or(u64::MAX)
            .saturating_mul(4);
        let left_witness = self.recording.upload(
            "vello.segment_order_replay_left_witness",
            vec![0; usize::try_from(witness_bytes).unwrap_or(0)],
        );
        let right_witness = self.recording.upload(
            "vello.segment_order_replay_right_witness",
            vec![0; usize::try_from(witness_bytes).unwrap_or(0)],
        );
        let bump = render.bump_buf();
        let bump_buf = measure.then_some(bump);
        let (left_proxy, right_proxy, witnesses) = render.record_fine_area_order_replay(
            &self.shaders,
            &mut self.recording,
            patch_shader,
            [expected, left, control, left_witness],
            [expected, right, control, right_witness],
        );
        if defer {
            render::finish_bump_readback(&mut self.recording, bump, measure);
        }
        let resources = [
            ExternalResource::Image(left_proxy, left_texture),
            ExternalResource::Image(right_proxy, right_texture),
        ];
        let run_result = self.engine.run_recording_with_post_render(
            device,
            queue,
            &self.recording,
            &resources,
            "render_to_textures_segment_order_replay",
            post_render,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        );
        if let Err(error) = run_result {
            let _ = take_segment_order_replay_downloads(&mut self.engine, witnesses);
            return Err(error);
        }
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }
        let (left, right) = take_segment_order_replay_downloads(&mut self.engine, witnesses)?;
        Ok(Some(SegmentOrderReplayDownload {
            left,
            right,
            bytes_per_witness: witness_bytes,
        }))
    }

    fn segment_order_replay_shader(&mut self, device: &Device) -> ShaderId {
        if let Some(shader) = self.segment_order_replay_shader {
            return shader;
        }
        let shader = self.engine.add_compute_shader(
            device,
            "vello.segment_order_replay_patch",
            Cow::Borrowed(SEGMENT_ORDER_REPLAY_WGSL),
            &[
                recording::BindType::BufReadOnly,
                recording::BindType::Buffer,
                recording::BindType::BufReadOnly,
                recording::BindType::BufReadOnly,
                recording::BindType::Uniform,
                recording::BindType::Buffer,
            ],
            wgpu_engine::CpuShaderType::Missing,
        );
        self.segment_order_replay_shader = Some(shader);
        shader
    }

    /// Discard a diagnostic recording before it reaches the queue. Resolver state can otherwise
    /// refer to a newly allocated atlas that was never uploaded; recreating it makes the next
    /// ordinary render resolve and upload its complete image set again.
    fn abort_unsent_geometry_capture(&mut self, prior_image_atlas: Option<recording::ImageProxy>) {
        self.image_atlas = prior_image_atlas;
        self.resolver = Resolver::new();
        self.recording = Recording::default();
    }

    /// Render exact native-pixel damage and append work to the same sparse
    /// submission after Vello has encoded its passes.
    ///
    /// The callback is not invoked when sparse rendering is unavailable or
    /// `damage` is empty, because those cases submit no command buffer.
    /// Encode with an optional final submission gate. A rejected recording requires
    /// renderer reconstruction; no counter/timestamp readback is started for it.
    #[doc(hidden)]
    pub fn render_to_texture_with_damage_and_post_render_checked<F, G>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        post_render: F,
        before_submit: G,
    ) -> Result<Option<SparseRenderStats>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
        G: FnOnce() -> bool,
    {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if !self.sparse_fine_supported() || params.antialiasing_method != AaConfig::Area {
            return Ok(None);
        }
        let Some(mut stats) = sparse_tile_records(
            &mut self.sparse_masks,
            &mut self.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            return Ok(None);
        };
        self.budget.set_viewport(params.width, params.height);
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        let measure = self.budget.wants_readback();
        let canonical_segment_order = self.canonical_segment_order_selected_for(params, measure);
        let area_ppt8 = self.sparse_area_ppt8_selected_for(params);
        let defer_bump_readback = self.defer_bump_readback_enabled() || canonical_segment_order;
        let zero_gradient = if self.engine.zero_gradient_reuse_enabled() {
            Some(self.engine.ensure_zero_gradient(device, queue))
        } else {
            None
        };
        let external_damage_tiles = self.prepare_sparse_upload(device, queue);
        let fine_quad_skip = self.sparse_fine_quad_skip_available();
        if fine_quad_skip {
            if let Some((total, active, pixels)) =
                sparse_fine_quad_counts(&self.sparse_records, params.width, params.height)
            {
                stats.fine_quad_skip_applied = true;
                stats.fine_quad_total_quads = total;
                stats.fine_quad_active_quads = active;
                stats.fine_pixels = pixels;
            }
        }
        stats.coarse_adaptive = self.sparse_coarse_adaptive_enabled();
        let sparse_tile_bounds_available = self.sparse_tile_bounds_available();
        let coarse_tiles = if self.engine.sparse_coarse_adaptive_enabled() {
            let selection = self.sparse_coarse_active_bins_available().then(|| {
                sparse_coarse_adaptive_selection(
                    &mut self.sparse_coarse_tiles,
                    &self.sparse_records,
                    params.width,
                    params.height,
                )
            });
            if let Some(selection) = selection {
                let active_bins = selection.bins;
                if let Some(active_bins) = active_bins {
                    stats.coarse_native_bins = active_bins.native_bins;
                    stats.coarse_selected_native_bins = active_bins.selected_bins;
                    if selection.active {
                        stats.coarse_active_bins = true;
                        Some(render::SparseCoarseDispatch::Active {
                            selector: self.sparse_coarse_tiles.as_slice(),
                            workgroups: active_bins.selected_bins,
                            path_tile_bounds: sparse_tile_bounds_available
                                .then(|| {
                                    sparse_path_tile_bounds(
                                        &self.sparse_records,
                                        params.width,
                                        params.height,
                                    )
                                })
                                .flatten(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else if self.sparse_coarse_supported() {
            let active_bins = self
                .sparse_coarse_active_bins_available()
                .then(|| {
                    sparse_coarse_active_bin_list(
                        &mut self.sparse_coarse_tiles,
                        &self.sparse_records,
                        params.width,
                        params.height,
                    )
                })
                .flatten();
            if let Some(active_bins) = active_bins {
                stats.coarse_native_bins = active_bins.native_bins;
                stats.coarse_selected_native_bins = active_bins.selected_bins;
                stats.coarse_active_bins = true;
                Some(render::SparseCoarseDispatch::Active {
                    selector: self.sparse_coarse_tiles.as_slice(),
                    workgroups: active_bins.selected_bins,
                    path_tile_bounds: sparse_tile_bounds_available
                        .then(|| {
                            sparse_path_tile_bounds(
                                &self.sparse_records,
                                params.width,
                                params.height,
                            )
                        })
                        .flatten(),
                })
            } else if sparse_coarse_tile_bits(
                &mut self.sparse_coarse_tiles,
                &self.sparse_records,
                params.width,
                params.height,
            ) {
                Some(render::SparseCoarseDispatch::Legacy(
                    self.sparse_coarse_tiles.as_slice(),
                ))
            } else {
                None
            }
        } else {
            None
        };
        if let Some(render::SparseCoarseDispatch::Active {
            path_tile_bounds: Some(bounds),
            ..
        }) = coarse_tiles
        {
            stats.path_tile_bounds_applied = true;
            stats.path_tile_bounds_tiles = bounds[2]
                .saturating_sub(bounds[0])
                .saturating_mul(bounds[3].saturating_sub(bounds[1]));
        }
        let coarse_opaque_prune_requested = self.sparse_opaque_prune_requested();
        stats.coarse_opaque_prune_selected = self.sparse_opaque_prune_selected_for(coarse_tiles);
        let sparse_solid_fuse_requested = self.sparse_solid_fuse_requested();
        stats.solid_fuse_selected = self.sparse_solid_fuse_selected_for(coarse_tiles);
        let sparse_fine_quad_skip_requested = self.sparse_fine_quad_skip_requested();
        stats.segment_row_mask_selected =
            self.sparse_segment_row_mask_selected_for(coarse_tiles, params.width, params.height);
        let (
            target,
            bump_buf,
            canonical_segment_order_selected,
            area_ppt8_selected,
            clip_single_group_selected,
        ) = render::render_encoding_sparse_fine(
            scene.encoding(),
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            &self.sparse_records,
            external_damage_tiles,
            coarse_tiles,
            coarse_opaque_prune_requested,
            stats.fine_quad_skip_applied,
            sparse_solid_fuse_requested,
            sparse_fine_quad_skip_requested,
            stats.segment_row_mask_selected,
            canonical_segment_order,
            area_ppt8,
            defer_bump_readback,
            zero_gradient,
        );
        if let Some(proxy) = external_damage_tiles {
            let upload = self
                .sparse_upload
                .as_ref()
                .expect("persistent sparse upload was prepared");
            let external_resources = [
                ExternalResource::Image(*target.as_image().unwrap(), texture),
                ExternalResource::Buffer(proxy, &upload.buffer),
            ];
            self.engine.run_recording_with_post_render_checked(
                device,
                queue,
                &self.recording,
                &external_resources,
                "render_to_texture_sparse",
                post_render,
                before_submit,
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
        } else {
            let external_resources = [ExternalResource::Image(
                *target.as_image().unwrap(),
                texture,
            )];
            self.engine.run_recording_with_post_render_checked(
                device,
                queue,
                &self.recording,
                &external_resources,
                "render_to_texture_sparse",
                post_render,
                before_submit,
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
        }
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
        stats.canonical_segment_order_shader_selected = canonical_segment_order_selected;
        stats.area_ppt8_shader_selected = area_ppt8_selected;
        stats.clip_single_group_shader_selected = clip_single_group_selected;
        self.last_canonical_segment_order_selected = canonical_segment_order_selected;
        self.last_area_ppt8_selected = area_ppt8_selected;
        self.last_clip_single_group_selected = clip_single_group_selected;
        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }
        Ok(Some(stats))
    }

    /// Append post-render commands to the same unconditional Vello submission.
    #[doc(hidden)]
    pub fn render_to_texture_with_damage_and_post_render<F>(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        post_render: F,
    ) -> Result<Option<SparseRenderStats>>
    where
        F: FnOnce(&mut wgpu::CommandEncoder),
    {
        self.render_to_texture_with_damage_and_post_render_checked(
            device,
            queue,
            scene,
            texture,
            params,
            damage,
            post_render,
            || true,
        )
    }

    /// Poll outstanding counter readbacks without waiting for the GPU.
    ///
    /// Pending work retains any overflow collected by earlier renders. Only a
    /// completed poll consumes that overflow. Readback errors remain visible on
    /// subsequent polls, including errors found by ordinary render collection;
    /// replacing the renderer is required to discard a failed observation.
    ///
    /// An idle renderer, or one with measurement deliberately disabled, has no
    /// outstanding counter work and reports complete. This method does not prove
    /// GPU submission or frame completion: callers must establish that separately.
    pub fn poll_bump_budget(&mut self, device: &Device) -> Result<BumpBudgetPoll, BumpBudgetError> {
        let (changed, overflowed) = self.budget.collect(device, &mut self.engine);
        self.unsettled_bump_overflow.record(overflowed);
        if changed {
            self.engine.clear_pool();
        }
        self.budget.check_readback()?;
        if self.budget.has_pending_readback() {
            return Ok(BumpBudgetPoll::Pending);
        }
        Ok(BumpBudgetPoll::Complete {
            overflowed: self.unsettled_bump_overflow.take(),
        })
    }

    /// Collect the bump counters submitted since the previous call to this
    /// method. Returns `true` when any of them overflowed and therefore
    /// dropped geometry. One-shot/readback renderers call this after waiting
    /// for the GPU so they can resubmit immediately; interactive renderers
    /// can keep their normal next-frame recovery.
    #[doc(hidden)]
    pub fn settle_bump_budget(&mut self, device: &Device) -> bool {
        let mut overflowed = self.unsettled_bump_overflow.take();
        // A one-shot readback has already waited for the rendered texture,
        // but the tiny counter buffer's map callback can become observable a
        // poll later. Give it a bounded blocking settle so "not ready yet" is
        // never mistaken for "the frame fit".
        for _ in 0..4 {
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let (changed, frame_overflowed) = self.budget.collect(device, &mut self.engine);
            overflowed |= frame_overflowed;
            if changed {
                self.engine.clear_pool();
            }
            if !self.budget.has_pending_readback() {
                break;
            }
        }
        overflowed
    }

    /// Overwrite `image` with `texture`.
    ///
    /// Most users should prefer [`register_texture`](Self::register_texture), which
    /// ergonomically encapulates these requirements.
    ///
    /// `texture` must have the [`wgpu::TextureFormat::Rgba8Unorm`] format and
    /// the [`wgpu::TextureUsages::COPY_SRC`] flag set. The `Rgba8UnormSrgb` format
    /// might also be supported, but this is not tested.
    ///
    /// The given `Texture`'s data will be copied into the slot in Vello's image
    /// atlas where the image would be placed each frame.
    /// This has the effect that wherever you request the image to be drawn (e.g.
    /// through [`Scene::draw_image`]), the data from the texture will be used instead.
    ///
    /// Correct behaviour is not guaranteed if the texture does not have the same
    /// dimensions as the image, nor if an image which uses the same [blob] but different
    /// dimensions would be rendered.
    ///
    /// [blob]: peniko::ImageData::data
    pub fn override_image(
        &mut self,
        image: &ImageData,
        texture: Option<wgpu::TexelCopyTextureInfoBase<wgpu::Texture>>,
    ) -> Option<wgpu::TexelCopyTextureInfoBase<wgpu::Texture>> {
        self.resolver.mark_image_dirty(image);
        match texture {
            Some(texture) => self.engine.image_overrides.insert(image.data.id(), texture),
            None => self.engine.image_overrides.remove(&image.data.id()),
        }
    }

    /// Marks `image` as dirty in the atlas cache, so its override texture will be recopied on
    /// the next render that uses it.
    ///
    /// Call this for any `image` whose existing override texture has changed since the last render.
    /// Otherwise, stale image data from the atlas may be used.
    pub fn mark_override_image_dirty(&mut self, image: &ImageData) {
        self.resolver.mark_image_dirty(image);
    }

    /// Register a [`wgpu::Texture`] with Vello, to allow drawing GPU-resident data.
    ///
    /// The returned `Image` can be used in [`Scene`]s (only those rendered with this `Renderer`)
    /// Rendering Scenes which use this `Image` with other `Renderer`s will panic.
    ///
    /// `texture` must have the [`wgpu::TextureFormat::Rgba8Unorm`] format and
    /// the [`wgpu::TextureUsages::COPY_SRC`] flag set. This is because the data will
    /// be copied into Vello's image atlas at the start of each frame.
    /// The `Rgba8UnormSrgb` format might also be supported, but this is not tested.
    /// The texture is assumed to have unpremultiplied alpha.
    ///
    /// This is a utility wrapper around [`override_image`](Self::override_image).
    /// For greater control, use that method.
    ///
    /// If the texture is no longer active then it should be unregistered using [`unregister_texture`](Self::unregister_texture)
    pub fn register_texture(&mut self, texture: wgpu::Texture) -> ImageData {
        // Create a fake, empty blob which will be used to back the returned image
        // This image data will never be read by Vello, due to being added to
        // image_overrides, below.
        let fake_blob = peniko::Blob::new(std::sync::Arc::new(&[]));

        let image = ImageData {
            data: fake_blob,
            format: peniko::ImageFormat::Rgba8,
            alpha_type: peniko::ImageAlphaType::Alpha,
            width: texture.width(),
            height: texture.height(),
        };

        // For this utility API, we take the full texture and use the base layer and mip level
        let texture_base = wgpu::TexelCopyTextureInfoBase {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        };

        // We overwrite any attempt to use the fake blob, instead reading from the texture.
        self.override_image(&image, Some(texture_base));

        image
    }

    /// Unregister a [`wgpu::Texture`] that was registered with [`register_texture`](Self::register_texture).
    pub fn unregister_texture(&mut self, handle: ImageData) {
        self.override_image(&handle, None);
    }

    /// Reload the shaders. This should only be used during `vello` development
    #[cfg(feature = "hot_reload")]
    #[doc(hidden)] // End-users of Vello should not have `hot_reload` enabled.
    pub async fn reload_shaders(&mut self, device: &Device) -> Result<(), Error> {
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut engine = WgpuEngine::new(self.options.use_cpu, self.options.pipeline_cache.clone());
        // We choose not to initialise these shaders in parallel, to ensure the error scope works correctly
        let shaders = shaders::full_shaders(device, &mut engine, &self.options)?;
        #[cfg(feature = "debug_layers")]
        let debug = debug::DebugRenderer::new(device, wgpu::TextureFormat::Rgba8Unorm, &mut engine);
        let error = scope.pop().await;
        if let Some(error) = error {
            return Err(error.into());
        }
        self.engine = engine;
        self.image_atlas = None;
        self.shaders = shaders;
        #[cfg(feature = "debug_layers")]
        {
            self.debug = debug;
        }
        Ok(())
    }

    /// Renders a scene to the target texture using an async pipeline.
    ///
    /// Almost all consumers should prefer [`Self::render_to_texture`].
    ///
    /// The return value is the value of the `BumpAllocators` in this rendering, which is currently used
    /// for debug output.
    ///
    /// This return type is not stable, and will likely be changed when a more principled way to access
    /// relevant statistics is implemented
    #[cfg_attr(docsrs, doc(hidden))]
    #[deprecated(
        note = "render_to_texture should be preferred, as the _async version has no stability guarantees"
    )]
    pub async fn render_to_texture_async(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        debug_layers: DebugLayers,
    ) -> Result<Option<BumpAllocators>> {
        self.last_canonical_segment_order_selected = false;
        self.last_area_ppt8_selected = false;
        self.last_clip_single_group_selected = false;
        if cfg!(not(feature = "debug_layers")) && !debug_layers.is_empty() {
            static HAS_WARNED: AtomicBool = AtomicBool::new(false);
            if !HAS_WARNED.swap(true, std::sync::atomic::Ordering::Release) {
                log::warn!(
                    "Requested debug layers {debug_layers:?} but `debug_layers` feature is not enabled"
                );
            }
        }

        let result = self
            .render_to_texture_async_internal(device, queue, scene, texture, params)
            .await?;

        #[cfg(feature = "debug_layers")]
        {
            let mut recording = Recording::default();
            let target_proxy = recording::ImageProxy::new(
                params.width,
                params.height,
                recording::ImageFormat::Rgba8,
            );
            if let Some(captured) = result.captured {
                let bump = result.bump.as_ref().unwrap();
                // TODO: We could avoid this download if `DebugLayers::VALIDATION` is unset.
                let downloads = DebugDownloads::map(&self.engine, &captured, bump).await?;
                self.debug.render(
                    &mut recording,
                    target_proxy,
                    &captured,
                    bump,
                    params,
                    &downloads,
                    debug_layers,
                );

                // TODO: this sucks. better to release everything in a helper
                // TODO: it would be much better to have a way to safely destroy a buffer.
                self.engine.free_download(captured.lines);
                captured.release_buffers(&mut recording);
            }
            let external_resources = [ExternalResource::Image(target_proxy, texture)];
            self.engine.run_recording(
                device,
                queue,
                &recording,
                &external_resources,
                "render_to_texture_async debug layers",
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
        }

        #[cfg(feature = "wgpu-profiler")]
        {
            self.profiler.end_frame().unwrap();
            if let Some(result) = self
                .profiler
                .process_finished_frame(queue.get_timestamp_period())
            {
                self.profile_result = Some(result);
            }
        }

        Ok(result.bump)
    }

    async fn render_to_texture_async_internal(
        &mut self,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
    ) -> Result<RenderResult> {
        let mut render = Render::new();
        render.set_clip_single_group_allowed(false);
        let mut recording = Recording::default();
        let mut packed_scene = Vec::new();
        let encoding = scene.encoding();
        self.budget.set_viewport(params.width, params.height);
        let capture_debug = cfg!(feature = "debug_layers");
        // Debug layers require these counters. Otherwise only the adaptive
        // GPU path pays for the awaited readback; pinned and CPU-dispatch
        // rendering do not need it for sizing.
        let measure_bump =
            capture_debug || (!self.options.use_cpu && self.budget.wants_measurement());
        let zero_gradient = if self.engine.zero_gradient_reuse_enabled() {
            Some(self.engine.ensure_zero_gradient(device, queue))
        } else {
            None
        };
        render.render_encoding_coarse(
            encoding,
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut recording,
            &mut packed_scene,
            params,
            self.budget.budget(),
            measure_bump,
            capture_debug,
            None,
            false,
            zero_gradient,
        );
        let target = render.out_image();
        let bump_buf = render.bump_buf();
        #[cfg(feature = "debug_layers")]
        let captured = render.take_captured_buffers();
        self.engine.run_recording(
            device,
            queue,
            &recording,
            &[],
            "t_async_coarse",
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;

        let mut bump: Option<BumpAllocators> = None;
        let mapped_and_unmapped = if let Some(download) = self.engine.get_download(bump_buf) {
            let buf_slice = download.slice(..);
            let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
            buf_slice.map_async(wgpu::MapMode::Read, move |v| sender.send(v).unwrap());
            receiver.receive().await.expect("channel was closed")?;
            let mapped = buf_slice.get_mapped_range();
            bump = Some(bytemuck::pod_read_unaligned(&mapped));
            drop(mapped);
            download.unmap();
            true
        } else {
            false
        };
        // Size later frames from this one. Re-running coarse for the current
        // frame is not attempted; a failure can therefore affect this output,
        // and convergence can require more than one subsequent render.
        if let Some(bump) = bump.as_ref()
            && self.budget.observe_now(bump)
        {
            self.engine.clear_pool();
        }
        if mapped_and_unmapped {
            self.engine.recycle_bump_download(bump_buf);
        } else {
            self.engine.free_download(bump_buf);
        }
        // Maybe clear to reuse allocation?
        let mut recording = Recording::default();
        render.record_fine(&self.shaders, &mut recording);
        let external_resources = [ExternalResource::Image(target, texture)];
        self.engine.run_recording(
            device,
            queue,
            &recording,
            &external_resources,
            "t_async_fine",
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;
        Ok(RenderResult {
            bump,
            #[cfg(feature = "debug_layers")]
            captured,
        })
    }
}
#[cfg(all(feature = "debug_layers", feature = "wgpu"))]
pub(crate) struct DebugDownloads<'a> {
    pub lines: wgpu::BufferSlice<'a>,
}

#[cfg(all(feature = "debug_layers", feature = "wgpu"))]
impl<'a> DebugDownloads<'a> {
    pub async fn map(
        engine: &'a WgpuEngine,
        captured: &render::CapturedBuffers,
        bump: &BumpAllocators,
    ) -> Result<DebugDownloads<'a>> {
        use vello_encoding::LineSoup;

        let Some(lines_buf) = engine.get_download(captured.lines) else {
            return Err(Error::DownloadError("linesoup"));
        };

        let lines = lines_buf.slice(..bump.lines as u64 * size_of::<LineSoup>() as u64);
        let (sender, receiver) = futures_intrusive::channel::shared::oneshot_channel();
        lines.map_async(wgpu::MapMode::Read, move |v| sender.send(v).unwrap());
        receiver.receive().await.expect("channel was closed")?;
        Ok(Self { lines })
    }
}

#[cfg(all(test, feature = "wgpu"))]
mod tests {
    use std::{
        future::Future,
        mem::size_of,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
        thread,
    };

    use super::*;
    use crate::{
        kurbo::{Affine, BezPath, Point, Rect},
        peniko::{
            BlendMode, Blob, Color, ColorStop, Compose, Fill, Gradient, ImageAlphaType,
            ImageFormat, Mix,
        },
    };

    fn checked_bump_device() -> (Device, Queue) {
        let (device, queue) =
            headless_device().expect("checked bump tests require a real GPU adapter");
        eprintln!("checked bump adapter: {:?}", device.adapter_info());
        (device, queue)
    }

    fn checked_bump_wait(device: &Device) {
        device
            .poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(std::time::Duration::from_secs(2)),
            })
            .expect("counter fixture must complete within two seconds");
    }

    fn checked_bump_download(
        renderer: &mut Renderer,
        device: &Device,
        queue: &Queue,
        bytes: Vec<u8>,
    ) -> recording::BufferProxy {
        let mut recording = Recording::default();
        let proxy = recording.upload("vello.bump_buf", bytes);
        recording.download(proxy);
        recording.free_buffer(proxy);
        renderer
            .engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "checked bump fixture",
                #[cfg(feature = "wgpu-profiler")]
                &mut renderer.profiler,
            )
            .expect("real counter download must submit");
        renderer.engine.clear_pool();
        proxy
    }

    fn checked_bump_fixture(device: &Device, queue: &Queue, used: BumpAllocators) -> Renderer {
        let mut renderer = Renderer::new(device, RendererOptions::default()).unwrap();
        let proxy = checked_bump_download(
            &mut renderer,
            device,
            queue,
            bytemuck::bytes_of(&used).to_vec(),
        );
        renderer.budget.begin_readback(&renderer.engine, proxy);
        checked_bump_wait(device);
        renderer
    }

    fn checked_bump_params() -> RenderParams {
        RenderParams {
            base_color: Color::BLACK,
            width: 16,
            height: 16,
            antialiasing_method: AaConfig::Area,
        }
    }

    #[test]
    fn checked_bump_pending_retains_download_and_overflow() {
        use bump_budget::test_helpers::{pending_proxy, set_ready};
        let (device, queue) = checked_bump_device();
        let mut renderer = checked_bump_fixture(
            &device,
            &queue,
            BumpAllocators {
                failed: 1,
                lines: 1_000_000,
                ..Default::default()
            },
        );
        // The ordinary render collects the overflow before checked settlement.
        renderer
            .render_to_texture(
                &device,
                &queue,
                &solid_scene(),
                &target(&device),
                &checked_bump_params(),
            )
            .unwrap();
        checked_bump_wait(&device);
        let proxy = pending_proxy(&renderer.budget);
        set_ready(&mut renderer.budget, None);
        for _ in 0..3 {
            assert_eq!(
                renderer.poll_bump_budget(&device),
                Ok(BumpBudgetPoll::Pending)
            );
            assert!(renderer.engine.get_download(proxy).is_some());
            assert!(!renderer.budget.wants_readback());
        }
        set_ready(&mut renderer.budget, Some(true));
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: true })
        );
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: false })
        );
    }

    #[test]
    fn checked_bump_idle_renderer_has_no_outstanding_counter_work() {
        let (device, _) = checked_bump_device();
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        assert!(renderer.budget.wants_measurement());
        assert!(!renderer.budget.has_pending_readback());
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: false })
        );
    }

    #[test]
    fn checked_bump_map_failure_is_not_completion() {
        use bump_budget::test_helpers::{pending_proxy, set_ready};
        let (device, queue) = checked_bump_device();
        let mut renderer = checked_bump_fixture(&device, &queue, Default::default());
        let proxy = pending_proxy(&renderer.budget);
        renderer.engine.get_download(proxy).unwrap().unmap();
        set_ready(&mut renderer.budget, Some(false));
        // Exercise failure discovered by the real ordinary render's collection.
        renderer
            .render_to_texture(
                &device,
                &queue,
                &solid_scene(),
                &target(&device),
                &checked_bump_params(),
            )
            .unwrap();
        checked_bump_wait(&device);
        assert!(renderer.engine.get_download(proxy).is_none());
        for _ in 0..2 {
            assert_eq!(
                renderer.poll_bump_budget(&device),
                Err(BumpBudgetError::MapFailed)
            );
        }
    }

    #[test]
    fn checked_bump_failed_map_download_never_returns_to_pool() {
        use bump_budget::test_helpers::{pending_proxy, set_ready};
        let (device, queue) = checked_bump_device();
        let mut renderer = checked_bump_fixture(&device, &queue, Default::default());
        let proxy = pending_proxy(&renderer.budget);
        renderer.engine.get_download(proxy).unwrap().unmap();
        set_ready(&mut renderer.budget, Some(false));
        assert_eq!(
            renderer.budget.collect(&device, &mut renderer.engine),
            (false, false)
        );
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 0);
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Err(BumpBudgetError::MapFailed)
        );
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 0);
    }

    #[test]
    fn checked_bump_missing_download_is_error() {
        use bump_budget::test_helpers::{pending_proxy, set_ready};
        let (device, queue) = checked_bump_device();
        for (missing_at_begin, callback_pending) in [(true, false), (false, false), (false, true)] {
            let mut renderer = checked_bump_fixture(&device, &queue, Default::default());
            let proxy = pending_proxy(&renderer.budget);
            renderer.engine.get_download(proxy).unwrap().unmap();
            renderer.engine.free_download(proxy);
            if missing_at_begin {
                renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
                renderer.budget.begin_readback(&renderer.engine, proxy);
            }
            if callback_pending {
                set_ready(&mut renderer.budget, None);
            }
            let pooled = renderer.memory_stats().pooled_buffer_bytes;
            assert_eq!(
                renderer.poll_bump_budget(&device),
                Err(BumpBudgetError::MissingDownload)
            );
            assert_eq!(
                renderer.poll_bump_budget(&device),
                Err(BumpBudgetError::MissingDownload)
            );
            assert_eq!(renderer.memory_stats().pooled_buffer_bytes, pooled);
        }
    }

    #[test]
    fn checked_bump_poll_failure_remains_error_after_later_success() {
        use bump_budget::test_helpers::{fail_poll, pending_proxy};
        let (device, queue) = checked_bump_device();
        let mut renderer = checked_bump_fixture(&device, &queue, Default::default());
        let proxy = pending_proxy(&renderer.budget);
        // Replace only the external poll outcome, while collection operates on
        // the same actual mapped download and tracker as normal rendering.
        fail_poll(&mut renderer.budget, &mut renderer.engine);
        let expected = Err(BumpBudgetError::Poll(wgpu::PollError::Timeout.to_string()));
        assert_eq!(renderer.poll_bump_budget(&device), expected);
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 0);
        let proxy = checked_bump_download(&mut renderer, &device, &queue, vec![0; 32]);
        renderer.budget.begin_readback(&renderer.engine, proxy);
        checked_bump_wait(&device);
        assert_eq!(renderer.poll_bump_budget(&device), expected);
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 32);
    }

    #[test]
    fn checked_bump_invalid_download_is_error_and_not_recycled() {
        let (device, queue) = checked_bump_device();
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        let proxy = checked_bump_download(&mut renderer, &device, &queue, vec![0; 16]);
        renderer.budget.begin_readback(&renderer.engine, proxy);
        checked_bump_wait(&device);
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Err(BumpBudgetError::InvalidDownload)
        );
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 0);
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Err(BumpBudgetError::InvalidDownload)
        );
    }

    #[test]
    fn checked_bump_collection_preserves_growth_and_recycles_once() {
        use bump_budget::test_helpers::pending_proxy;
        let (device, queue) = checked_bump_device();
        let mut renderer = checked_bump_fixture(
            &device,
            &queue,
            BumpAllocators {
                failed: 1,
                lines: 1_000_000,
                ..Default::default()
            },
        );
        let proxy = pending_proxy(&renderer.budget);
        assert_eq!(
            renderer.budget.collect(&device, &mut renderer.engine),
            (true, true)
        );
        assert_eq!(renderer.budget.budget().lines, 2_000_000);
        assert!(renderer.engine.get_download(proxy).is_none());
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 32);
        assert_eq!(
            renderer.budget.collect(&device, &mut renderer.engine),
            (false, false)
        );
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 32);
        // Checked collection must clear obsolete pool classes on growth too.
        let proxy = checked_bump_download(
            &mut renderer,
            &device,
            &queue,
            bytemuck::bytes_of(&BumpAllocators {
                lines: 2_000_000,
                ..Default::default()
            })
            .to_vec(),
        );
        renderer.budget.begin_readback(&renderer.engine, proxy);
        checked_bump_wait(&device);
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: false })
        );
        assert_eq!(renderer.budget.budget().lines, 4_000_000);
        assert_eq!(renderer.memory_stats().pooled_buffer_bytes, 0);
    }

    #[test]
    fn checked_bump_strict_gpu_render_matches_completed_dense_counters() {
        use bump_budget::test_helpers::pending_proxy;
        let (device, queue) = checked_bump_device();
        let mut strict = Renderer::new(&device, RendererOptions::default()).unwrap();
        let mut dense = Renderer::new(&device, RendererOptions::default()).unwrap();
        let scene = gradient_scene();
        let params = checked_bump_params();
        strict
            .render_to_texture(&device, &queue, &scene, &target(&device), &params)
            .unwrap();
        dense
            .render_to_texture(&device, &queue, &scene, &target(&device), &params)
            .unwrap();
        checked_bump_wait(&device);
        let counters = |renderer: &Renderer| {
            renderer
                .engine
                .get_download(pending_proxy(&renderer.budget))
                .unwrap()
                .slice(..)
                .get_mapped_range()
                .to_vec()
        };
        let strict_counters = counters(&strict);
        assert_eq!(strict_counters, counters(&dense));
        assert!(
            strict_counters[4..].iter().any(|value| *value != 0),
            "real scene must use bump allocations"
        );
        // The completed dense reference uses the existing tracker collection;
        // this small scene stays below the unchanged effective viewport floors.
        assert_eq!(
            dense.budget.collect(&device, &mut dense.engine),
            (false, false)
        );
        assert_eq!(
            strict.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: false })
        );
        assert_eq!(strict.budget.budget(), dense.budget.budget());
        assert!(strict.budget.budget().lines > 0);
        assert_eq!(strict.memory_stats(), dense.memory_stats());
    }

    #[test]
    fn checked_bump_disabled_measurement_reports_no_counter_work() {
        let (device, _) = checked_bump_device();
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        bump_budget::test_helpers::disable_measurement(&mut renderer.budget);
        assert!(!renderer.budget.wants_measurement());
        assert!(!renderer.budget.wants_readback());
        assert!(!renderer.budget.has_pending_readback());
        let before = renderer.memory_stats();
        assert_eq!(
            renderer.poll_bump_budget(&device),
            Ok(BumpBudgetPoll::Complete { overflowed: false })
        );
        assert_eq!(renderer.memory_stats(), before);
    }

    #[test]
    fn geometry_recording_uploads_requires_one_scene_and_one_config() {
        let mut recording = Recording::default();
        recording.upload("vello.scene", [1_u8, 2, 3, 4]);
        recording.upload_uniform("vello.config", [5_u8; 4]);
        let (scene, config) = geometry_recording_uploads(&recording).unwrap();
        assert_eq!(scene, [1, 2, 3, 4]);
        assert_eq!(config, [5; 4]);

        recording.upload("vello.scene", [9_u8; 4]);
        assert_eq!(
            geometry_recording_uploads(&recording),
            Err(GeometryInputError::MissingOrAmbiguousUpload)
        );

        let mut missing_scene = Recording::default();
        missing_scene.upload_uniform("vello.config", [5_u8; 4]);
        assert_eq!(
            geometry_recording_uploads(&missing_scene),
            Err(GeometryInputError::MissingOrAmbiguousUpload)
        );
        let mut missing_config = Recording::default();
        missing_config.upload("vello.scene", [1_u8; 4]);
        assert_eq!(
            geometry_recording_uploads(&missing_config),
            Err(GeometryInputError::MissingOrAmbiguousUpload)
        );
        let mut duplicate_config = Recording::default();
        duplicate_config.upload("vello.scene", [1_u8; 4]);
        duplicate_config.upload_uniform("vello.config", [5_u8; 4]);
        duplicate_config.upload_uniform("vello.config", [6_u8; 4]);
        assert_eq!(
            geometry_recording_uploads(&duplicate_config),
            Err(GeometryInputError::MissingOrAmbiguousUpload)
        );
    }

    fn geometry_input_config() -> vello_encoding::ConfigUniform {
        vello_encoding::ConfigUniform {
            width_in_tiles: 2,
            height_in_tiles: 3,
            target_width: 31,
            target_height: 47,
            base_color: 0x0102_0304,
            layout: vello_encoding::Layout {
                n_draw_objects: 7,
                n_paths: 1,
                n_clips: 2,
                bin_data_start: 3,
                path_tag_base: 0,
                path_data_base: 1,
                draw_tag_base: 2,
                draw_data_base: 3,
                transform_base: 4,
                style_base: 5,
            },
            lines_size: 11,
            binning_size: 12,
            tiles_size: 13,
            seg_counts_size: 14,
            segments_size: 15,
            blend_size: 16,
            ptcl_size: 17,
        }
    }

    fn geometry_input_bytes(config: &vello_encoding::ConfigUniform) -> Vec<u8> {
        bytemuck::bytes_of(config).to_vec()
    }

    #[test]
    fn geometry_input_parser_slices_exact_word_spans_without_copying() {
        let config = geometry_input_config();
        let scene = (0_u8..24).collect::<Vec<_>>();
        let input =
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&config), false, false)
                .unwrap();
        assert_eq!(input.path_tags, &scene[0..4]);
        assert_eq!(input.path_data, &scene[4..8]);
        assert_eq!(input.transforms, &scene[16..20]);
        assert_eq!(input.styles, &scene[20..24]);
        assert_eq!(input.path_tags.as_ptr(), scene.as_ptr());
        assert_eq!(input.path_data.as_ptr(), scene[4..].as_ptr());
        assert_eq!(input.transforms.as_ptr(), scene[16..].as_ptr());
        assert_eq!(input.styles.as_ptr(), scene[20..].as_ptr());
        assert_eq!(
            input.descriptor,
            GeometryInputDescriptor {
                n_draw_objects: 7,
                n_paths: 1,
                n_clips: 2,
                bin_data_start: 3,
                path_tag_base: 0,
                path_data_base: 1,
                draw_tag_base: 2,
                draw_data_base: 3,
                transform_base: 4,
                style_base: 5,
                target_width: 31,
                target_height: 47,
                width_in_tiles: 2,
                height_in_tiles: 3,
                lines_size: 11,
                binning_size: 12,
                tiles_size: 13,
                seg_counts_size: 14,
                segments_size: 15,
                blend_size: 16,
                ptcl_size: 17,
                cpu_route: false,
                bump_readback_pending: false,
            }
        );
    }

    #[test]
    fn geometry_input_parser_rejects_bad_config_scene_and_bases() {
        let config = geometry_input_config();
        let bytes = geometry_input_bytes(&config);
        let scene = vec![0_u8; 24];
        for bad in [&bytes[..bytes.len() - 1], &[0_u8; 1][..]] {
            assert!(matches!(
                geometry_input_from_uploads(&scene, bad, false, false),
                Err(GeometryInputError::MalformedConfig)
            ));
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(matches!(
            geometry_input_from_uploads(&scene, &long, false, false),
            Err(GeometryInputError::MalformedConfig)
        ));
        let mut unaligned = vec![0_u8];
        unaligned.extend_from_slice(&bytes);
        assert!(geometry_input_from_uploads(&scene, &unaligned[1..], false, false).is_ok());
        assert!(matches!(
            geometry_input_from_uploads(&scene[..23], &bytes, false, false),
            Err(GeometryInputError::MalformedSpans)
        ));
        let mut zero_paths = config;
        zero_paths.layout.n_paths = 0;
        assert!(matches!(
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&zero_paths), false, false),
            Err(GeometryInputError::EmptyGeometry)
        ));
        assert!(matches!(
            geometry_input_from_uploads(&[], &geometry_input_bytes(&zero_paths), false, false),
            Err(GeometryInputError::EmptyGeometry)
        ));
        let mut empty_spans = config;
        empty_spans.layout.path_data_base = 0;
        empty_spans.layout.draw_tag_base = 0;
        empty_spans.layout.draw_data_base = 0;
        empty_spans.layout.transform_base = 0;
        empty_spans.layout.style_base = 0;
        assert!(matches!(
            geometry_input_from_uploads(&[], &geometry_input_bytes(&empty_spans), false, false),
            Err(GeometryInputError::EmptyGeometry)
        ));
        for (field, value) in [
            ("path_tag", 2),
            ("path_data", 0),
            ("draw_tag", 0),
            ("draw_data", 1),
            ("transform", 2),
            ("style", 3),
            ("style", 7),
            ("style", u32::MAX),
        ] {
            let mut bad = config;
            match field {
                "path_tag" => bad.layout.path_tag_base = value,
                "path_data" => {
                    bad.layout.path_tag_base = 1;
                    bad.layout.path_data_base = value;
                }
                "draw_tag" => bad.layout.draw_tag_base = value,
                "draw_data" => bad.layout.draw_data_base = value,
                "transform" => bad.layout.transform_base = value,
                "style" => bad.layout.style_base = value,
                _ => unreachable!(),
            }
            assert!(matches!(
                geometry_input_from_uploads(&scene, &geometry_input_bytes(&bad), false, false),
                Err(GeometryInputError::MalformedSpans)
            ));
        }
    }

    #[test]
    fn geometry_input_parser_excludes_draw_and_color_but_includes_geometry() {
        let config = geometry_input_config();
        let scene = (0_u8..24).collect::<Vec<_>>();
        let original =
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&config), false, false)
                .unwrap();
        for index in [8, 12] {
            let mut changed = scene.clone();
            changed[index] ^= 1;
            let candidate =
                geometry_input_from_uploads(&changed, &geometry_input_bytes(&config), false, false)
                    .unwrap();
            assert_eq!(original.path_tags, candidate.path_tags);
            assert_eq!(original.path_data, candidate.path_data);
            assert_eq!(original.transforms, candidate.transforms);
            assert_eq!(original.styles, candidate.styles);
        }
        for (index, expected_changed_span) in [(0, 0), (4, 1), (16, 2), (20, 3)] {
            let mut changed = scene.clone();
            changed[index] ^= 1;
            let candidate =
                geometry_input_from_uploads(&changed, &geometry_input_bytes(&config), false, false)
                    .unwrap();
            let original_spans = [
                original.path_tags,
                original.path_data,
                original.transforms,
                original.styles,
            ];
            let candidate_spans = [
                candidate.path_tags,
                candidate.path_data,
                candidate.transforms,
                candidate.styles,
            ];
            assert_ne!(
                original_spans[expected_changed_span],
                candidate_spans[expected_changed_span]
            );
            assert_ne!(original_spans, candidate_spans);
        }
        let mut color_only = config;
        color_only.base_color = 0x1234_5678;
        let candidate =
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&color_only), false, false)
                .unwrap();
        assert_eq!(original.path_tags, candidate.path_tags);
        assert_eq!(original.descriptor, candidate.descriptor);
    }

    #[test]
    fn geometry_input_parser_keeps_cpu_and_pending_separate() {
        let config = geometry_input_config();
        let scene = vec![0_u8; 24];
        assert!(matches!(
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&config), true, false),
            Err(GeometryInputError::CpuRoute)
        ));
        let input =
            geometry_input_from_uploads(&scene, &geometry_input_bytes(&config), false, true)
                .unwrap();
        assert!(!input.descriptor.cpu_route);
        assert!(input.descriptor.bump_readback_pending);
    }

    struct TestThreadWaker(thread::Thread);

    impl Wake for TestThreadWaker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        let waker = Waker::from(Arc::new(TestThreadWaker(thread::current())));
        let mut context = Context::from_waker(&waker);
        let mut future = std::pin::pin!(future);
        loop {
            match future.as_mut().poll(&mut context) {
                Poll::Ready(value) => return value,
                Poll::Pending => thread::park(),
            }
        }
    }

    #[test]
    fn diagnostic_geometry_download_cap_accepts_native_baseline_and_rejects_overflow() {
        const NATIVE_PTCL_BYTES: u64 = 49_766_400;
        const NATIVE_SEGMENTS_BYTES: u64 = 6_220_800;
        let native_total =
            diagnostic_geometry_download_total(512, NATIVE_PTCL_BYTES, NATIVE_SEGMENTS_BYTES);
        assert_eq!(native_total, 55_987_712);
        assert!(native_total <= DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP);
        assert_eq!(
            diagnostic_geometry_download_total(1, DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP, 0),
            DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP + 1
        );
        assert_eq!(diagnostic_geometry_download_total(u64::MAX, 1, 0), u64::MAX);
    }

    #[test]
    fn segment_order_replay_preflight_caps_and_preserves_duplicate_multisets() {
        let mut expected = [[0_u32; 5]; SEGMENT_ORDER_REPLAY_COUNT];
        expected[0] = [7, 8, 9, 10, 11];
        expected[1] = expected[0];
        let mut right = expected;
        right.swap(0, 1);
        let input = SegmentOrderReplayInput {
            tile: 0,
            logical_command: 0,
            size_and_rule: 98,
            backdrop: 0,
            expected,
            left: expected,
            right,
        };
        assert!(validate_segment_order_replay_input(&input).is_ok());
        let native = segment_order_replay_transfer_bytes(3840, 2160).expect("native cap math");
        assert_eq!(native, 66_362_172);
        assert!(native <= SEGMENT_ORDER_REPLAY_TRANSFER_CAP);
        assert!(
            segment_order_replay_transfer_bytes(4096, 2160).expect("larger cap math")
                > SEGMENT_ORDER_REPLAY_TRANSFER_CAP
        );
        assert!(matches!(
            segment_order_replay_transfer_bytes(u32::MAX, u32::MAX),
            Err(Error::SegmentOrderReplayTooLarge(..))
        ));
        let mut missing_duplicate = input.right;
        missing_duplicate[1] = [12, 13, 14, 15, 16];
        let invalid = SegmentOrderReplayInput {
            right: missing_duplicate,
            ..input
        };
        assert!(matches!(
            validate_segment_order_replay_input(&invalid),
            Err(Error::SegmentOrderReplayInvalid("right order multiset"))
        ));
    }

    #[test]
    fn segment_order_replay_control_accepts_tail_and_bounds_native_tiles() {
        let input = SegmentOrderReplayInput {
            tile: 1,
            logical_command: 0,
            size_and_rule: 98,
            backdrop: 0,
            expected: [[0; 5]; SEGMENT_ORDER_REPLAY_COUNT],
            left: [[0; 5]; SEGMENT_ORDER_REPLAY_COUNT],
            right: [[0; 5]; SEGMENT_ORDER_REPLAY_COUNT],
        };
        let control = segment_order_replay_control(&input, 64 * 4, 49 * 24 + 4, 16, 16)
            .expect("trailing segment bytes are not a stride failure");
        assert_eq!(control[6], 49);
        assert_eq!(control[7], 1);
        assert!(
            input.tile >= control[7],
            "fixture is an invalid native tile"
        );
        assert!(matches!(
            Renderer::segment_order_replay_preflight(16, 16, &input),
            Err(Error::SegmentOrderReplayInvalid("native tile index"))
        ));
        let valid = SegmentOrderReplayInput { tile: 0, ..input };
        assert!(matches!(
            Renderer::segment_order_replay_preflight(0, 16, &valid),
            Err(Error::SegmentOrderReplayInvalid("native extent"))
        ));
    }

    #[test]
    fn segment_order_replay_patch_preserves_padding_and_unrelated_records_on_gpu() {
        let (device, queue) = headless_device().expect("headless GPU device for replay proof");
        let mut engine = WgpuEngine::new(false, None);
        let shader = engine.add_compute_shader(
            &device,
            "vello.segment_order_replay_patch_test",
            Cow::Borrowed(SEGMENT_ORDER_REPLAY_WGSL),
            &[
                recording::BindType::BufReadOnly,
                recording::BindType::Buffer,
                recording::BindType::BufReadOnly,
                recording::BindType::BufReadOnly,
                recording::BindType::Uniform,
                recording::BindType::Buffer,
            ],
            wgpu_engine::CpuShaderType::Missing,
        );
        let mut expected = [[0_u32; 5]; SEGMENT_ORDER_REPLAY_COUNT];
        for (index, segment) in expected.iter_mut().enumerate() {
            let index = u32::try_from(index).expect("fixture index fits u32");
            *segment = [
                index,
                index.saturating_add(100),
                index.saturating_add(200),
                index.saturating_add(300),
                index.saturating_add(400),
            ];
        }
        let mut requested = expected;
        requested.rotate_left(1);
        let mut ptcl = [0_u32; 64];
        ptcl[1..5].copy_from_slice(&[1, 98, 0, 0]);
        let mut original_segments = vec![0_u32; (SEGMENT_ORDER_REPLAY_COUNT + 2) * 6 + 1];
        for (index, segment) in expected.iter().enumerate() {
            let base = index * 6;
            original_segments[base..base + 5].copy_from_slice(segment);
            original_segments[base + 5] = 0xdead_0000_u32
                .saturating_add(u32::try_from(index).expect("fixture index fits u32"));
        }
        original_segments[SEGMENT_ORDER_REPLAY_COUNT * 6..].fill(0xa5a5_a5a5);
        let mut recording = Recording::default();
        let ptcl = recording.upload(
            "vello.segment_order_replay_test_ptcl",
            bytemuck::cast_slice(&ptcl).to_vec(),
        );
        let segments = recording.upload(
            "vello.segment_order_replay_test_segments",
            bytemuck::cast_slice(&original_segments).to_vec(),
        );
        let expected_buffer = recording.upload(
            "vello.segment_order_replay_test_expected",
            segment_order_replay_words(&expected),
        );
        let requested_buffer = recording.upload(
            "vello.segment_order_replay_test_requested",
            segment_order_replay_words(&requested),
        );
        let control = recording.upload_uniform(
            "vello.segment_order_replay_test_control",
            bytemuck::cast_slice(&[
                0_u32,
                0,
                98,
                0,
                64,
                u32::try_from(original_segments.len()).expect("fixture words fit u32"),
                u32::try_from(original_segments.len() / 6).expect("fixture records fit u32"),
                1,
            ])
            .to_vec(),
        );
        let witness = recording.upload(
            "vello.segment_order_replay_test_witness",
            vec![0; SEGMENT_ORDER_REPLAY_WITNESS_WORDS * 4],
        );
        recording.dispatch(
            shader,
            (1, 1, 1),
            [
                ptcl,
                segments,
                expected_buffer,
                requested_buffer,
                control,
                witness,
            ],
        );
        recording.download(segments);
        recording.download(witness);
        for buffer in [
            ptcl,
            segments,
            expected_buffer,
            requested_buffer,
            control,
            witness,
        ] {
            recording.free_buffer(buffer);
        }
        #[cfg(feature = "wgpu-profiler")]
        let mut profiler =
            GpuProfiler::new(&device, GpuProfilerSettings::default()).expect("test profiler");
        engine
            .run_recording(
                &device,
                &queue,
                &recording,
                &[],
                "segment_order_replay_patch_test",
                #[cfg(feature = "wgpu-profiler")]
                &mut profiler,
            )
            .expect("test patch must submit");
        let segments_after = readback_words(
            &device,
            &engine
                .take_download(segments)
                .expect("segment test download"),
        );
        let witness_after = readback_words(
            &device,
            &engine
                .take_download(witness)
                .expect("witness test download"),
        );
        assert_eq!(witness_after[0], 1, "GPU guard must write only on success");
        for (index, segment) in requested.iter().enumerate() {
            let base = index * 6;
            assert_eq!(&segments_after[base..base + 5], segment);
            assert_eq!(segments_after[base + 5], original_segments[base + 5]);
            let witness_base = 5 + index * 5;
            assert_eq!(&witness_after[witness_base..witness_base + 5], segment);
        }
        assert_eq!(
            &segments_after[SEGMENT_ORDER_REPLAY_COUNT * 6..original_segments.len()],
            &original_segments[SEGMENT_ORDER_REPLAY_COUNT * 6..],
            "the bounded GPU patch must not modify records outside the fill"
        );
    }

    fn headless_device() -> Option<(Device, Queue)> {
        let mut context = util::RenderContext::new();
        let device_id = block_on(context.device(None))?;
        let device = context.devices.swap_remove(device_id);
        Some((device.device, device.queue))
    }

    fn canonical_segment_record_cmp(left: &[u32], right: &[u32]) -> std::cmp::Ordering {
        left[..5].cmp(&right[..5])
    }

    struct CanonicalSegmentOrderGpuFixture<'a> {
        width_in_tiles: u32,
        height_in_tiles: u32,
        ptcl_words: &'a [u32],
        ptcl_size: u32,
        segment_words: &'a [u32],
        segments_size: u32,
        bump_words: [u32; 8],
        damage_tiles: Option<&'a [u32]>,
    }

    fn canonical_segment_order_gpu_fixture(
        device: &Device,
        queue: &Queue,
        fixture: CanonicalSegmentOrderGpuFixture<'_>,
    ) -> (Vec<u32>, Vec<u32>, Vec<u32>) {
        let mut engine = WgpuEngine::new(false, None);
        let (source, bindings) = if fixture.damage_tiles.is_some() {
            (
                vello_shaders::SHADERS
                    .canonical_segment_order_sparse
                    .wgsl
                    .code
                    .as_ref(),
                vec![
                    recording::BindType::Uniform,
                    recording::BindType::Buffer,
                    recording::BindType::Buffer,
                    recording::BindType::Buffer,
                    recording::BindType::BufReadOnly,
                ],
            )
        } else {
            (
                vello_shaders::SHADERS
                    .canonical_segment_order_full
                    .wgsl
                    .code
                    .as_ref(),
                vec![
                    recording::BindType::Uniform,
                    recording::BindType::Buffer,
                    recording::BindType::Buffer,
                    recording::BindType::Buffer,
                ],
            )
        };
        let shader = engine.add_compute_shader(
            device,
            "vello.canonical_segment_order_guard_test",
            Cow::Borrowed(source),
            &bindings,
            wgpu_engine::CpuShaderType::Missing,
        );
        let config = vello_encoding::ConfigUniform {
            width_in_tiles: fixture.width_in_tiles,
            height_in_tiles: fixture.height_in_tiles,
            segments_size: fixture.segments_size,
            ptcl_size: fixture.ptcl_size,
            ..vello_encoding::ConfigUniform::default()
        };
        let mut recording = Recording::default();
        let config = recording.upload_uniform(
            "vello.canonical_segment_order_guard_config",
            bytemuck::bytes_of(&config).to_vec(),
        );
        let bump = recording.upload(
            "vello.canonical_segment_order_guard_bump",
            bytemuck::cast_slice(&fixture.bump_words).to_vec(),
        );
        let segments = recording.upload(
            "vello.canonical_segment_order_guard_segments",
            bytemuck::cast_slice(fixture.segment_words).to_vec(),
        );
        let ptcl = recording.upload(
            "vello.canonical_segment_order_guard_ptcl",
            bytemuck::cast_slice(fixture.ptcl_words).to_vec(),
        );
        let damage = fixture.damage_tiles.map(|tiles| {
            recording.upload(
                "vello.canonical_segment_order_guard_damage",
                bytemuck::cast_slice(tiles).to_vec(),
            )
        });
        if let Some(damage) = damage {
            let workgroups = u32::try_from(fixture.damage_tiles.unwrap().len() / 9)
                .expect("damage fixture workgroups fit u32");
            recording.dispatch(
                shader,
                (workgroups, 1, 1),
                [config, bump, segments, ptcl, damage],
            );
        } else {
            recording.dispatch(shader, (1, 1, 1), [config, bump, segments, ptcl]);
        }
        recording.download(bump);
        recording.download(segments);
        recording.download(ptcl);
        for buffer in [config, bump, segments, ptcl] {
            recording.free_buffer(buffer);
        }
        if let Some(damage) = damage {
            recording.free_buffer(damage);
        }
        #[cfg(feature = "wgpu-profiler")]
        let mut profiler = GpuProfiler::new(device, GpuProfilerSettings::default())
            .expect("canonical guard fixture profiler");
        engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "canonical_segment_order_guard_test",
                #[cfg(feature = "wgpu-profiler")]
                &mut profiler,
            )
            .expect("canonical guard fixture submits");
        let bump = readback_proxy_words(
            device,
            &engine
                .take_download(bump)
                .expect("canonical guard bump download"),
            bump.size,
        );
        let segments = readback_proxy_words(
            device,
            &engine
                .take_download(segments)
                .expect("canonical guard segment download"),
            segments.size,
        );
        let ptcl = readback_proxy_words(
            device,
            &engine
                .take_download(ptcl)
                .expect("canonical guard PTCL download"),
            ptcl.size,
        );
        (bump, ptcl, segments)
    }

    fn canonical_segment_sort_gpu_fixture(
        device: &Device,
        queue: &Queue,
        count: usize,
    ) -> Vec<u32> {
        let mut engine = WgpuEngine::new(false, None);
        let shader = engine.add_compute_shader(
            device,
            "vello.canonical_segment_order_test",
            Cow::Borrowed(
                vello_shaders::SHADERS
                    .canonical_segment_order_full
                    .wgsl
                    .code
                    .as_ref(),
            ),
            &[
                recording::BindType::Uniform,
                recording::BindType::Buffer,
                recording::BindType::Buffer,
                recording::BindType::Buffer,
            ],
            wgpu_engine::CpuShaderType::Missing,
        );
        let mut config = vello_encoding::ConfigUniform::default();
        config.width_in_tiles = 1;
        config.height_in_tiles = 1;
        config.segments_size = u32::try_from(count).expect("fixture record count fits u32");
        config.ptcl_size = 64;
        // The second initial-size block is physical pool tail only. It must
        // not make an otherwise valid tile walk into allocation capacity.
        let mut ptcl_words = vec![u32::MAX; 128];
        ptcl_words[..64].fill(0);
        ptcl_words[1..6].copy_from_slice(&[
            1,
            u32::try_from(count).expect("fixture count fits PTCL") << 1,
            0,
            0,
            0,
        ]);
        // The one-word tail is deliberately not part of a complete physical
        // segment record. The shader must validate logical record limits,
        // rather than demanding an allocation be an exact 24-byte multiple.
        let mut segment_words = vec![0_u32; count * 6 + 1];
        for (index, record) in segment_words[..count * 6].chunks_exact_mut(6).enumerate() {
            let value = u32::try_from(count - index).expect("fixture value fits u32");
            record.copy_from_slice(&[value % 7, 0, 0, 0, 0, value ^ 0x5a5a_0000]);
        }
        segment_words[count * 6] = 0xa5a5_a5a5;
        let original = segment_words.clone();
        let mut recording = Recording::default();
        let config = recording.upload_uniform(
            "vello.canonical_segment_order_test_config",
            bytemuck::bytes_of(&config).to_vec(),
        );
        let bump = recording.upload(
            "vello.canonical_segment_order_test_bump",
            bytemuck::cast_slice(&[
                0_u32,
                0,
                0,
                0,
                0,
                u32::try_from(count).expect("fixture record count fits bump allocator"),
                0,
                0,
            ])
            .to_vec(),
        );
        let segments = recording.upload(
            "vello.canonical_segment_order_test_segments",
            bytemuck::cast_slice(&segment_words).to_vec(),
        );
        let ptcl = recording.upload(
            "vello.canonical_segment_order_test_ptcl",
            bytemuck::cast_slice(&ptcl_words).to_vec(),
        );
        recording.dispatch(shader, (1, 1, 1), [config, bump, segments, ptcl]);
        recording.download(bump);
        recording.download(segments);
        recording.download(ptcl);
        for buffer in [config, bump, segments, ptcl] {
            recording.free_buffer(buffer);
        }
        #[cfg(feature = "wgpu-profiler")]
        let mut profiler = GpuProfiler::new(device, GpuProfilerSettings::default())
            .expect("canonical sort fixture profiler");
        engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "canonical_segment_order_test",
                #[cfg(feature = "wgpu-profiler")]
                &mut profiler,
            )
            .expect("canonical sort fixture submits");
        let bump_after = readback_proxy_words(
            device,
            &engine
                .take_download(bump)
                .expect("canonical sort bump download"),
            bump.size,
        );
        assert_eq!(bump_after[0], 0, "valid canonical fixture must not fail");
        let ptcl_after = readback_proxy_words(
            device,
            &engine
                .take_download(ptcl)
                .expect("canonical sort PTCL download"),
            ptcl.size,
        );
        assert_eq!(
            ptcl_after[0], 0,
            "valid canonical fixture must not poison fine"
        );
        let actual = readback_proxy_words(
            device,
            &engine
                .take_download(segments)
                .expect("canonical sort segment download"),
            segments.size,
        );
        assert_eq!(
            actual[count * 6],
            original[count * 6],
            "tail must be untouched"
        );
        let mut expected_records = original[..count * 6]
            .chunks_exact(6)
            .map(<[u32; 6]>::try_from)
            .collect::<Result<Vec<_>, _>>()
            .expect("complete fixture records");
        expected_records.sort_unstable();
        let mut actual_records = actual[..count * 6]
            .chunks_exact(6)
            .map(<[u32; 6]>::try_from)
            .collect::<Result<Vec<_>, _>>()
            .expect("complete GPU records");
        assert!(actual_records.windows(2).all(|pair| {
            canonical_segment_record_cmp(pair[0].as_slice(), pair[1].as_slice()).is_le()
        }));
        actual_records.sort_unstable();
        assert_eq!(actual_records, expected_records);
        actual
    }

    #[test]
    fn canonical_segment_order_gpu_heapsorts_all_fixture_lengths_and_preserves_tail() {
        let (device, queue) = headless_device().expect("headless GPU device for canonical sort");
        for count in [2, 3, 49, 257] {
            let words = canonical_segment_sort_gpu_fixture(&device, &queue, count);
            assert_eq!(words.len(), count * 6 + 1);
        }
    }

    #[test]
    fn canonical_segment_order_gpu_guards_poison_fine_without_writing_segments() {
        let (device, queue) = headless_device().expect("headless GPU device for canonical guards");
        let segments = vec![
            2,
            0,
            0,
            0,
            0,
            22, // record 0
            1,
            0,
            0,
            0,
            0,
            11,          // record 1
            0xa5a5_a5a5, // incomplete physical tail
        ];
        let mut valid = vec![0_u32; 128];
        valid[1..6].copy_from_slice(&[1, 4, 0, 0, 0]);
        let valid_bump = [0, 0, 0, 0, 0, 2, 0, 0];
        let assert_rejected = |name: &str,
                               ptcl: Vec<u32>,
                               ptcl_size: u32,
                               bump: [u32; 8],
                               damage: Option<Vec<u32>>| {
            let (after_bump, after_ptcl, after_segments) = canonical_segment_order_gpu_fixture(
                &device,
                &queue,
                CanonicalSegmentOrderGpuFixture {
                    width_in_tiles: 1,
                    height_in_tiles: 1,
                    ptcl_words: &ptcl,
                    ptcl_size,
                    segment_words: &segments,
                    segments_size: 2,
                    bump_words: bump,
                    damage_tiles: damage.as_deref(),
                },
            );
            if bump[0] == 0 {
                assert_ne!(
                    after_bump[0] & 0x20,
                    0,
                    "{name} must set the canonical parser failure bit"
                );
            }
            assert_eq!(after_ptcl[0], u32::MAX, "{name} must poison fine");
            assert_eq!(
                after_segments, segments,
                "{name} must not sort before validation"
            );
        };

        let mut unknown = valid.clone();
        unknown[1] = 99;
        assert_rejected("unknown command", unknown, 64, valid_bump, None);

        let mut truncated = vec![0_u32; 128];
        truncated[1..63].fill(3); // 62 one-word CMD_SOLID commands.
        truncated[63] = 1; // A FILL cannot cross the initial 64-word block.
        assert_rejected("truncated fill", truncated, 64, valid_bump, None);

        let mut unaligned = vec![0_u32; 320];
        unaligned[1..3].copy_from_slice(&[12, 65]);
        assert_rejected(
            "unaligned continuation",
            unaligned,
            320,
            [0, 0, 256, 0, 0, 2, 0, 0],
            None,
        );

        let mut backward = vec![0_u32; 320];
        backward[1..3].copy_from_slice(&[12, 64]);
        backward[64..66].copy_from_slice(&[12, 64]);
        assert_rejected(
            "backward continuation",
            backward,
            320,
            [0, 0, 256, 0, 0, 2, 0, 0],
            None,
        );

        let mut allocation_tail = vec![0_u32; 320];
        allocation_tail[1..3].copy_from_slice(&[12, 64]);
        allocation_tail[64..69].copy_from_slice(&[1, 4, 0, 0, 0]);
        assert_rejected("physical PTCL tail", allocation_tail, 64, valid_bump, None);

        let mut unused_logical_tail = vec![0_u32; 320];
        unused_logical_tail[1..3].copy_from_slice(&[12, 64]);
        unused_logical_tail[64..69].copy_from_slice(&[1, 4, 0, 0, 0]);
        assert_rejected(
            "unused logical PTCL tail",
            unused_logical_tail,
            320,
            valid_bump,
            None,
        );

        assert_rejected(
            "actual segment bump bound",
            valid.clone(),
            64,
            [0, 0, 0, 0, 0, 1, 0, 0],
            None,
        );

        let mut overlap = vec![0_u32; 128];
        overlap[1..10].copy_from_slice(&[1, 4, 0, 0, 1, 2, 1, 0, 0]);
        assert_rejected("overlapping fills", overlap, 64, valid_bump, None);

        assert_rejected(
            "prior bump failure",
            valid.clone(),
            64,
            [1, 0, 0, 0, 0, 2, 0, 0],
            None,
        );

        let mut wrong_sparse_tile = vec![0_u32; 9];
        wrong_sparse_tile[0] = 1;
        assert_rejected(
            "sparse tile outside native range",
            valid.clone(),
            64,
            valid_bump,
            Some(wrong_sparse_tile),
        );

        let mut continuation = vec![0_u32; 320];
        continuation[1..3].copy_from_slice(&[12, 64]);
        continuation[64..69].copy_from_slice(&[1, 4, 0, 0, 0]);
        let (bump, ptcl, sorted) = canonical_segment_order_gpu_fixture(
            &device,
            &queue,
            CanonicalSegmentOrderGpuFixture {
                width_in_tiles: 1,
                height_in_tiles: 1,
                ptcl_words: &continuation,
                ptcl_size: 320,
                segment_words: &segments,
                segments_size: 2,
                bump_words: [0, 0, 256, 0, 0, 2, 0, 0],
                damage_tiles: None,
            },
        );
        assert_eq!(bump[0], 0, "valid continuation must remain accepted");
        assert_eq!(ptcl[0], 0, "valid continuation must not poison fine");
        assert_eq!(&sorted[..12], &[1, 0, 0, 0, 0, 11, 2, 0, 0, 0, 0, 22]);
        assert_eq!(sorted[12], segments[12], "sub-record tail stays untouched");

        let mut two_tiles = vec![0_u32; 128];
        two_tiles[1..6].copy_from_slice(&[1, 4, 0, 0, 0]);
        two_tiles[65..70].copy_from_slice(&[1, 4, 2, 0, 0]);
        let two_tile_segments = vec![
            2, 0, 0, 0, 0, 22, 1, 0, 0, 0, 0, 11, // selected tile
            4, 0, 0, 0, 0, 44, 3, 0, 0, 0, 0, 33, // unselected tile
        ];
        let mut one_sparse_tile = vec![0_u32; 9];
        one_sparse_tile[0] = 0;
        let (bump, ptcl, sorted) = canonical_segment_order_gpu_fixture(
            &device,
            &queue,
            CanonicalSegmentOrderGpuFixture {
                width_in_tiles: 2,
                height_in_tiles: 1,
                ptcl_words: &two_tiles,
                ptcl_size: 128,
                segment_words: &two_tile_segments,
                segments_size: 4,
                bump_words: [0, 0, 0, 0, 0, 4, 0, 0],
                damage_tiles: Some(&one_sparse_tile),
            },
        );
        assert_eq!(bump[0], 0, "valid sparse tile must remain accepted");
        assert_eq!(ptcl[0], 0, "valid sparse tile must not poison fine");
        assert_eq!(&sorted[..12], &[1, 0, 0, 0, 0, 11, 2, 0, 0, 0, 0, 22]);
        assert_eq!(
            &sorted[12..],
            &two_tile_segments[12..],
            "sparse canonicalization must not touch an unselected native tile"
        );
    }

    fn replay_guard_fixture(
        device: &Device,
        queue: &Queue,
        ptcl_words: &[u32],
        segment_words: &[u32],
        expected: &[[u32; 5]; SEGMENT_ORDER_REPLAY_COUNT],
        control_words: [u32; 8],
    ) -> (Vec<u32>, Vec<u32>) {
        let mut engine = WgpuEngine::new(false, None);
        let shader = engine.add_compute_shader(
            device,
            "vello.segment_order_replay_guard_fixture",
            Cow::Borrowed(SEGMENT_ORDER_REPLAY_WGSL),
            &[
                recording::BindType::BufReadOnly,
                recording::BindType::Buffer,
                recording::BindType::BufReadOnly,
                recording::BindType::BufReadOnly,
                recording::BindType::Uniform,
                recording::BindType::Buffer,
            ],
            wgpu_engine::CpuShaderType::Missing,
        );
        let mut recording = Recording::default();
        let ptcl = recording.upload(
            "replay.guard.ptcl",
            bytemuck::cast_slice(ptcl_words).to_vec(),
        );
        let segments = recording.upload(
            "replay.guard.segments",
            bytemuck::cast_slice(segment_words).to_vec(),
        );
        let expected_buffer = recording.upload(
            "replay.guard.expected",
            segment_order_replay_words(expected),
        );
        let requested = recording.upload(
            "replay.guard.requested",
            segment_order_replay_words(expected),
        );
        let control = recording.upload_uniform(
            "replay.guard.control",
            bytemuck::cast_slice(&control_words).to_vec(),
        );
        let witness = recording.upload(
            "replay.guard.witness",
            vec![0; SEGMENT_ORDER_REPLAY_WITNESS_WORDS * 4],
        );
        recording.dispatch(
            shader,
            (1, 1, 1),
            [ptcl, segments, expected_buffer, requested, control, witness],
        );
        recording.download(segments);
        recording.download(witness);
        for buffer in [ptcl, segments, expected_buffer, requested, control, witness] {
            recording.free_buffer(buffer);
        }
        #[cfg(feature = "wgpu-profiler")]
        let mut profiler = GpuProfiler::new(device, GpuProfilerSettings::default())
            .expect("guard fixture profiler");
        engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "segment_order_replay_guard_fixture",
                #[cfg(feature = "wgpu-profiler")]
                &mut profiler,
            )
            .expect("guard fixture submits");
        (
            readback_words(
                device,
                &engine
                    .take_download(segments)
                    .expect("guard segments download"),
            ),
            readback_words(
                device,
                &engine
                    .take_download(witness)
                    .expect("guard witness download"),
            ),
        )
    }

    #[test]
    fn segment_order_replay_gpu_guards_reject_bad_metadata_and_ptcl_without_writes() {
        let (device, queue) = headless_device().expect("headless GPU device for replay guards");
        let mut expected = [[0_u32; 5]; SEGMENT_ORDER_REPLAY_COUNT];
        let mut segments = vec![0_u32; SEGMENT_ORDER_REPLAY_COUNT * 6 + 1];
        for (index, tuple) in expected.iter_mut().enumerate() {
            let value = u32::try_from(index).expect("fixture index fits u32");
            *tuple = [
                value,
                value.saturating_add(1),
                value.saturating_add(2),
                value.saturating_add(3),
                value.saturating_add(4),
            ];
            segments[index * 6..index * 6 + 5].copy_from_slice(tuple);
            segments[index * 6 + 5] = 0xfeed_0000_u32.saturating_add(value);
        }
        let mut valid_ptcl = vec![0_u32; 64];
        valid_ptcl[1..5].copy_from_slice(&[1, 98, 0, 0]);
        let base_control = [
            0,
            0,
            98,
            0,
            u32::try_from(valid_ptcl.len()).expect("ptcl words fit u32"),
            u32::try_from(segments.len()).expect("segment words fit u32"),
            u32::try_from(segments.len() / 6).expect("segment records fit u32"),
            1,
        ];
        let mut cases = Vec::new();
        let mut multiplicity_segments = segments.clone();
        let duplicate = [
            multiplicity_segments[0],
            multiplicity_segments[1],
            multiplicity_segments[2],
            multiplicity_segments[3],
            multiplicity_segments[4],
        ];
        multiplicity_segments[6..11].copy_from_slice(&duplicate);
        cases.push((
            "multiplicity",
            valid_ptcl.clone(),
            multiplicity_segments,
            base_control,
            13,
        ));
        let mut bad_metadata = valid_ptcl.clone();
        bad_metadata[4] = 1;
        cases.push(("metadata", bad_metadata, segments.clone(), base_control, 11));
        let mut bad_range = valid_ptcl.clone();
        bad_range[3] = 1;
        cases.push(("range", bad_range, segments.clone(), base_control, 12));
        let mut unknown = valid_ptcl.clone();
        unknown[1] = 99;
        cases.push(("unknown", unknown, segments.clone(), base_control, 9));
        let mut edge_fill = vec![0_u32; 128];
        edge_fill[1..63].fill(3);
        edge_fill[63..67].copy_from_slice(&[1, 98, 0, 0]);
        let edge_control = [0, 62, 98, 0, 128, base_control[5], base_control[6], 1];
        cases.push((
            "truncated fill at initial edge",
            edge_fill,
            segments.clone(),
            edge_control,
            9,
        ));
        let mut edge_jump = vec![0_u32; 128];
        edge_jump[1..63].fill(3);
        edge_jump[63..65].copy_from_slice(&[12, 64]);
        cases.push((
            "truncated jump at initial edge",
            edge_jump,
            segments.clone(),
            edge_control,
            6,
        ));
        let mut truncated = vec![0_u32; 128];
        truncated[1..64].fill(3);
        truncated[64..68].copy_from_slice(&[1, 98, 0, 0]);
        let truncated_control = [0, 63, 98, 0, 128, base_control[5], base_control[6], 1];
        cases.push((
            "block fallthrough",
            truncated,
            segments.clone(),
            truncated_control,
            4,
        ));
        let mut cyclic = vec![0_u32; 320];
        cyclic[1..3].copy_from_slice(&[12, 64]);
        cyclic[64..66].copy_from_slice(&[12, 64]);
        let cyclic_control = [0, 0, 98, 0, 320, base_control[5], base_control[6], 1];
        cases.push(("cycle", cyclic, segments.clone(), cyclic_control, 8));
        let mut surplus_segments = vec![0_u32; 51_usize.saturating_mul(6)];
        let target_words = SEGMENT_ORDER_REPLAY_COUNT.saturating_mul(6);
        surplus_segments[..target_words].copy_from_slice(&segments[..target_words]);
        let mut logical_record_bound = valid_ptcl.clone();
        logical_record_bound[3] = 1;
        let logical_record_control = [
            0,
            0,
            98,
            0,
            u32::try_from(logical_record_bound.len()).expect("ptcl words fit u32"),
            u32::try_from(surplus_segments.len()).expect("segment words fit u32"),
            49,
            1,
        ];
        cases.push((
            "logical record bound",
            logical_record_bound,
            surplus_segments,
            logical_record_control,
            12,
        ));
        let mut inconsistent_records = base_control;
        inconsistent_records[6] = inconsistent_records[6].saturating_add(1);
        cases.push((
            "inconsistent records",
            valid_ptcl.clone(),
            segments.clone(),
            inconsistent_records,
            17,
        ));
        let mut invalid_tile_control = base_control;
        invalid_tile_control[0] = 1;
        invalid_tile_control[4] = 128;
        let mut invalid_tile_ptcl = vec![0_u32; 128];
        invalid_tile_ptcl[65..69].copy_from_slice(&[1, 98, 0, 0]);
        cases.push((
            "native tile",
            invalid_tile_ptcl,
            segments.clone(),
            invalid_tile_control,
            16,
        ));
        for (label, ptcl, original_segments, control, status) in cases {
            let (after, witness) = replay_guard_fixture(
                &device,
                &queue,
                &ptcl,
                &original_segments,
                &expected,
                control,
            );
            assert_eq!(witness[0], status, "{label} guard status");
            assert_eq!(
                &after[..original_segments.len()],
                original_segments.as_slice(),
                "{label} guard changed segment data"
            );
        }
    }

    fn target(device: &Device) -> TextureView {
        device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("vello.zero_gradient_test_target"),
                size: wgpu::Extent3d {
                    width: 16,
                    height: 16,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::STORAGE_BINDING,
                view_formats: &[],
            })
            .create_view(&wgpu::TextureViewDescriptor::default())
    }

    fn damage_target(device: &Device, queue: &Queue) -> (wgpu::Texture, TextureView) {
        damage_target_with_size(device, queue, 16, 16)
    }

    fn damage_target_with_size(
        device: &Device,
        queue: &Queue,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, TextureView) {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("vello.sparse_post_render_test_target"),
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
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &vec![0; usize::try_from(width).unwrap() * usize::try_from(height).unwrap() * 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(width * 4),
                rows_per_image: Some(height),
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        (texture, view)
    }

    fn copy_damage_tile(
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        readback: &wgpu::Buffer,
        x: u32,
        y: u32,
    ) {
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d { x, y, z: 0 },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(256),
                    rows_per_image: Some(16),
                },
            },
            wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
        );
    }

    fn readback_tile(device: &Device, readback: &wgpu::Buffer) -> Vec<u8> {
        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range().to_vec();
        readback.unmap();
        pixels
    }

    fn readback_words(device: &Device, readback: &wgpu::Buffer) -> Vec<u32> {
        let bytes = readback_tile(device, readback);
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect()
    }

    fn readback_proxy_words(
        device: &Device,
        readback: &wgpu::Buffer,
        logical_bytes: u64,
    ) -> Vec<u32> {
        let mut bytes = readback_tile(device, readback);
        let logical_bytes =
            usize::try_from(logical_bytes).expect("logical readback bytes fit usize");
        assert_eq!(logical_bytes % 4, 0, "word fixture has whole-word length");
        assert!(
            bytes.len() >= logical_bytes,
            "pooled readback must include every logical byte"
        );
        bytes.truncate(logical_bytes);
        bytes
            .chunks_exact(4)
            .map(|word| u32::from_le_bytes(word.try_into().unwrap()))
            .collect()
    }

    fn reachable_ptcl_tags(ptcl: &[u32], mut offset: usize) -> Vec<u32> {
        let mut tags = Vec::new();
        for _ in 0..256 {
            assert!(offset < ptcl.len(), "PTCL walk left its captured buffer");
            let tag = ptcl[offset] & !0x8000_0000;
            tags.push(tag);
            match tag {
                0 => return tags,
                1 => offset += 4,  // CMD_FILL
                3 => offset += 1,  // CMD_SOLID
                5 => offset += 2,  // CMD_COLOR
                10 => offset += 1, // CMD_BEGIN_CLIP
                11 => offset += 3, // CMD_END_CLIP
                12 => offset = usize::try_from(ptcl[offset + 1]).unwrap(), // CMD_JUMP
                unknown => panic!("unexpected non-fixture PTCL command {unknown}"),
            }
        }
        panic!("PTCL walk did not reach CMD_END within its bounded fixture list");
    }

    fn reachable_ptcl_words(ptcl: &[u32], mut offset: usize) -> (Vec<u32>, usize) {
        let mut tags = Vec::new();
        let mut words = 0;
        for _ in 0..512 {
            assert!(offset < ptcl.len(), "PTCL walk left its captured buffer");
            let raw = ptcl[offset];
            let tag = raw & !0x8000_0000;
            tags.push(raw);
            match tag {
                0 => return (tags, words + 1),
                1 => {
                    offset += 4;
                    words += 4;
                }
                3 | 10 | 13 => {
                    offset += if tag == 13 { 3 } else { 1 };
                    words += if tag == 13 { 3 } else { 1 };
                }
                5 | 9 => {
                    offset += 2;
                    words += 2;
                }
                6 | 7 | 8 | 11 => {
                    offset += 3;
                    words += 3;
                }
                12 => {
                    offset = usize::try_from(ptcl[offset + 1]).unwrap();
                    words += 2;
                }
                unknown => panic!("unexpected solid-fuse PTCL command {unknown}"),
            }
        }
        panic!("PTCL walk did not reach CMD_END within its bounded fixture list");
    }

    fn capture_sparse_ptcl(
        renderer: &mut Renderer,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
        solid_fuse: bool,
    ) -> Vec<u32> {
        let Some(_) = sparse_tile_records(
            &mut renderer.sparse_masks,
            &mut renderer.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            panic!("capture requires sparse records");
        };
        renderer.budget.set_viewport(params.width, params.height);
        let zero = renderer
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| renderer.engine.ensure_zero_gradient(device, queue));
        let (target, ptcl_proxy) = render::render_encoding_sparse_fine_with_ptcl_capture(
            scene.encoding(),
            &mut renderer.resolver,
            &renderer.shaders,
            &mut renderer.image_atlas,
            &mut renderer.recording,
            &mut renderer.packed_scene,
            params,
            renderer.budget.budget(),
            &renderer.sparse_records,
            None,
            solid_fuse,
            zero,
        );
        renderer
            .engine
            .run_recording(
                device,
                queue,
                &renderer.recording,
                &[ExternalResource::Image(
                    *target.as_image().unwrap(),
                    texture,
                )],
                "solid_fuse_ptcl_capture",
                #[cfg(feature = "wgpu-profiler")]
                &mut renderer.profiler,
            )
            .unwrap();
        readback_words(
            device,
            &renderer
                .engine
                .take_download(ptcl_proxy)
                .expect("paired capture PTCL download must exist"),
        )
    }

    struct SegmentRowMaskCapture {
        ptcl: Vec<u32>,
        segments: Vec<u8>,
    }

    fn capture_sparse_row_mask_pair(
        renderer: &mut Renderer,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        ordinary: &TextureView,
        masked: &TextureView,
        params: &RenderParams,
        damage: &[FineDamageRect],
    ) -> SegmentRowMaskCapture {
        let Some(_) = sparse_tile_records(
            &mut renderer.sparse_masks,
            &mut renderer.sparse_records,
            params.width,
            params.height,
            damage,
            device.limits().max_compute_workgroups_per_dimension,
        ) else {
            panic!("row-mask capture requires sparse records");
        };
        let active = sparse_coarse_active_bin_list(
            &mut renderer.sparse_coarse_tiles,
            &renderer.sparse_records,
            params.width,
            params.height,
        )
        .expect("row-mask capture requires active sparse bins");
        let bounds = sparse_path_tile_bounds(&renderer.sparse_records, params.width, params.height)
            .expect("row-mask capture requires valid bounded native tiles");
        let coarse_tiles = Some(render::SparseCoarseDispatch::Active {
            selector: renderer.sparse_coarse_tiles.as_slice(),
            workgroups: active.selected_bins,
            path_tile_bounds: Some(bounds),
        });
        assert!(render::sparse_segment_row_mask_shader_selected(
            &renderer.shaders,
            coarse_tiles,
            true,
            false,
            false,
            false,
            params.width,
            params.height,
        ));
        renderer.budget.set_viewport(params.width, params.height);
        let zero_gradient = renderer
            .engine
            .zero_gradient_reuse_enabled()
            .then(|| renderer.engine.ensure_zero_gradient(device, queue));
        let mut render = Render::new();
        render.render_encoding_coarse_for_row_mask_capture(
            scene.encoding(),
            &mut renderer.resolver,
            &renderer.shaders,
            &mut renderer.image_atlas,
            &mut renderer.recording,
            &mut renderer.packed_scene,
            params,
            renderer.budget.budget(),
            coarse_tiles,
            zero_gradient,
        );
        let (ptcl_bytes, segment_bytes) = render.fine_geometry_bytes();
        assert!(
            diagnostic_geometry_download_total(0, ptcl_bytes, segment_bytes)
                <= DIAGNOSTIC_GEOMETRY_DOWNLOAD_CAP,
            "test-only row-mask downloads must remain bounded"
        );
        let (ordinary_proxy, masked_proxy, (ptcl_proxy, segments_proxy)) = render
            .record_fine_sparse_area_pair_with_row_mask_capture(
                &renderer.shaders,
                &mut renderer.recording,
                &renderer.sparse_records,
            );
        renderer
            .engine
            .run_recording(
                device,
                queue,
                &renderer.recording,
                &[
                    ExternalResource::Image(ordinary_proxy, ordinary),
                    ExternalResource::Image(masked_proxy, masked),
                ],
                "segment_row_mask_pair_capture",
                #[cfg(feature = "wgpu-profiler")]
                &mut renderer.profiler,
            )
            .expect("row-mask pair capture must submit");
        let ptcl = readback_words(
            device,
            &renderer
                .engine
                .take_download(ptcl_proxy)
                .expect("row-mask PTCL download must exist"),
        );
        let segments = readback_tile(
            device,
            &renderer
                .engine
                .take_download(segments_proxy)
                .expect("row-mask segment download must exist"),
        );
        SegmentRowMaskCapture { ptcl, segments }
    }

    fn readback_texture_rows(
        device: &Device,
        queue: &Queue,
        texture: &wgpu::Texture,
        width: u32,
        height: u32,
    ) -> (Vec<u8>, usize) {
        let tight = width.checked_mul(4).expect("test texture row size fits");
        let stride = tight.next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT);
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.segment_row_mask_pixels"),
            size: u64::from(stride) * u64::from(height),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("vello.segment_row_mask_pixels_copy"),
        });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &readback,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(stride),
                    rows_per_image: Some(height),
                },
            },
            wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit([encoder.finish()]);
        (
            readback_tile(device, &readback),
            usize::try_from(stride).unwrap(),
        )
    }

    fn texture_has_nonzero_and_varied_pixels(
        bytes: &[u8],
        stride: usize,
        width: u32,
        height: u32,
    ) -> bool {
        let width = usize::try_from(width).unwrap();
        let height = usize::try_from(height).unwrap();
        let mut first = None;
        let mut nonzero = false;
        for y in 0..height {
            let row = &bytes[y * stride..y * stride + width * 4];
            for pixel in row.chunks_exact(4) {
                nonzero |= pixel.iter().any(|byte| *byte != 0);
                if let Some(first) = first {
                    if pixel != first {
                        return nonzero;
                    }
                } else {
                    first = Some(pixel);
                }
            }
        }
        false
    }

    fn read_captured_segment_f32(segments: &[u8], byte_offset: usize) -> f32 {
        let bytes: [u8; 4] = segments[byte_offset..byte_offset + 4]
            .try_into()
            .expect("captured segment word is in range");
        f32::from_bits(u32::from_le_bytes(bytes))
    }

    fn assert_captured_row_mask_clear_bits(records: &[u32], ptcl: &[u32], segments: &[u8]) -> u32 {
        const TILE_PTCL_WORDS: usize = 64;
        const SEGMENT_STRIDE: usize = 24;
        assert_eq!(records.len() % 9, 0);
        assert!(
            !records.is_empty(),
            "row-mask proof requires at least one selected native tile"
        );
        let mut clear_bits = 0_u32;
        let mut reachable_segments = 0_u32;
        for record in records.chunks_exact(9) {
            let tile = usize::try_from(record[0]).expect("native tile index fits usize");
            let mut command = tile
                .checked_mul(TILE_PTCL_WORDS)
                .and_then(|offset| offset.checked_add(1))
                .expect("PTCL tile start fits");
            let mut reached_end = false;
            for _ in 0..512 {
                let tag = *ptcl
                    .get(command)
                    .expect("reachable PTCL command stays in captured range")
                    & !0x8000_0000;
                match tag {
                    0 => {
                        reached_end = true;
                        break;
                    }
                    1 => {
                        let size_and_rule = *ptcl
                            .get(command + 1)
                            .expect("FILL size stays in captured range");
                        let segment_base = usize::try_from(
                            *ptcl
                                .get(command + 2)
                                .expect("FILL segment offset stays in captured range"),
                        )
                        .expect("FILL segment offset fits usize");
                        let segment_count = usize::try_from(size_and_rule >> 1)
                            .expect("FILL segment count fits usize");
                        let segment_end = segment_base
                            .checked_add(segment_count)
                            .expect("FILL segment range fits usize");
                        reachable_segments = reachable_segments
                            .saturating_add(u32::try_from(segment_count).unwrap_or(u32::MAX));
                        for segment in segment_base..segment_end {
                            let offset = segment
                                .checked_mul(SEGMENT_STRIDE)
                                .expect("segment byte offset fits");
                            assert!(
                                offset.saturating_add(SEGMENT_STRIDE) <= segments.len(),
                                "PTCL FILL segment must remain in captured 24-byte storage"
                            );
                            let point0 = [
                                read_captured_segment_f32(segments, offset),
                                read_captured_segment_f32(segments, offset.saturating_add(4)),
                            ];
                            let point1 = [
                                read_captured_segment_f32(segments, offset.saturating_add(8)),
                                read_captured_segment_f32(segments, offset.saturating_add(12)),
                            ];
                            let y_edge =
                                read_captured_segment_f32(segments, offset.saturating_add(16));
                            let mask = u32::from_le_bytes(
                                segments[offset.saturating_add(20)..offset.saturating_add(24)]
                                    .try_into()
                                    .expect("captured mask word is in range"),
                            );
                            for row in 0..16 {
                                if mask & (1_u32 << row) == 0 {
                                    clear_bits = clear_bits.saturating_add(1);
                                    assert!(
                                        !render::fine_row_has_original_area_contribution(
                                            point0, point1, y_edge, row,
                                        ),
                                        "captured clear row {row} omitted a nonzero original Area contribution: {point0:?} {point1:?} edge={y_edge:?} mask={mask:#06x}"
                                    );
                                }
                            }
                        }
                        command = command.saturating_add(4);
                    }
                    3 | 10 => command = command.saturating_add(1),
                    5 | 9 => command = command.saturating_add(2),
                    6 | 7 | 8 | 11 | 13 => command = command.saturating_add(3),
                    12 => {
                        command = usize::try_from(
                            *ptcl
                                .get(command + 1)
                                .expect("JUMP target stays in captured range"),
                        )
                        .expect("JUMP target fits usize");
                    }
                    unexpected => panic!("unexpected reachable PTCL tag {unexpected}"),
                }
            }
            assert!(
                reached_end,
                "reachable PTCL walk exceeded its command bound"
            );
        }
        assert!(
            reachable_segments > 0,
            "row-mask proof requires reachable FILL segment records"
        );
        clear_bits
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_segment_row_mask_gpu_pair_proves_clear_bits_and_exact_pixels() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 65;
        const HEIGHT: u32 = 33;
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let damage = [FineDamageRect {
            x: 48,
            y: 16,
            width: 17,
            height: 17,
        }];
        let mut scene = Scene::new();
        scene.push_clip_layer(
            Fill::EvenOdd,
            Affine::IDENTITY,
            &Rect::new(-4.0, -4.0, 69.0, 37.0),
        );
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(14.5, -1.0, 66.0, 34.0),
        );
        let mut path = BezPath::new();
        path.move_to((12.0, -2.0));
        path.line_to((66.0, 4.25));
        path.line_to((48.25, 33.5));
        path.line_to((15.75, 18.5));
        path.close_path();
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::from_rgba8(220, 40, 180, 144),
            None,
            &path,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(30, 180, 255, 192),
            None,
            &Rect::new(47.5, 15.5, 65.0, 33.0),
        );
        scene.pop_layer();
        scene.pop_layer();

        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut dense = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(&mut dense, &device, &queue, &scene, &dense_target, &params);
        let (dense_pixels, dense_stride) =
            readback_texture_rows(&device, &queue, &dense_texture, WIDTH, HEIGHT);

        for direct in [true, false] {
            let (ordinary_texture, ordinary_target) =
                damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
            let (masked_texture, masked_target) =
                damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
            let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
            renderer
                .engine
                .enable_sparse_segment_row_mask_fixture(direct);
            renderer.shaders =
                shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
                    .expect("row-mask fixture permutations must compile");
            renderer
                .engine
                .build_shaders_if_needed(&device, renderer.options.num_init_threads);
            assert!(renderer.sparse_segment_row_mask_requested());
            assert!(renderer.sparse_segment_row_mask_available());
            let capture = capture_sparse_row_mask_pair(
                &mut renderer,
                &device,
                &queue,
                &scene,
                &ordinary_target,
                &masked_target,
                &params,
                &damage,
            );
            let clear_bits = assert_captured_row_mask_clear_bits(
                &renderer.sparse_records,
                &capture.ptcl,
                &capture.segments,
            );
            assert!(clear_bits > 0, "fixture must capture nontrivial row masks");
            let (ordinary_pixels, ordinary_stride) =
                readback_texture_rows(&device, &queue, &ordinary_texture, WIDTH, HEIGHT);
            let (masked_pixels, masked_stride) =
                readback_texture_rows(&device, &queue, &masked_texture, WIDTH, HEIGHT);
            assert_eq!(ordinary_stride, dense_stride);
            assert_eq!(masked_stride, dense_stride);
            for y in 0..HEIGHT {
                for x in 0..WIDTH {
                    let offset = usize::try_from(y).unwrap() * dense_stride
                        + usize::try_from(x).unwrap() * 4;
                    let ordinary = &ordinary_pixels[offset..offset + 4];
                    let masked = &masked_pixels[offset..offset + 4];
                    let dense = &dense_pixels[offset..offset + 4];
                    let requested = x >= damage[0].x
                        && x < damage[0].x.saturating_add(damage[0].width)
                        && y >= damage[0].y
                        && y < damage[0].y.saturating_add(damage[0].height);
                    if requested {
                        assert_eq!(ordinary, dense, "ordinary sparse pixel {x},{y}");
                        assert_eq!(masked, dense, "masked sparse pixel {x},{y}");
                    } else {
                        assert_eq!(ordinary, &[0; 4], "ordinary sentinel pixel {x},{y}");
                        assert_eq!(masked, &[0; 4], "masked sentinel pixel {x},{y}");
                    }
                }
            }
            let capture_bytes = capture
                .ptcl
                .len()
                .saturating_mul(size_of::<u32>())
                .saturating_add(capture.segments.len());
            let requested_pixels =
                u64::from(damage[0].width).saturating_mul(u64::from(damage[0].height));
            eprintln!(
                "segment-row-mask GPU proof direct={direct} captured_bytes={capture_bytes} checked_clear_bits={clear_bits} requested_pixels={requested_pixels}"
            );
        }
    }

    fn render_dense_with_recovery(
        renderer: &mut Renderer,
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        texture: &TextureView,
        params: &RenderParams,
    ) {
        for _ in 0..4 {
            renderer
                .render_to_texture(device, queue, scene, texture, params)
                .unwrap();
            if !renderer.settle_bump_budget(device) {
                return;
            }
        }
        panic!("dense reference remained overflowed after bounded recovery");
    }

    fn solid_scene() -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, 16.0, 16.0),
        );
        scene
    }

    fn gradient_scene() -> Scene {
        let mut scene = Scene::new();
        let gradient = Gradient::new_linear(Point::new(0.0, 0.0), Point::new(16.0, 16.0))
            .with_stops(
                &[
                    ColorStop {
                        offset: 0.0,
                        color: Color::BLACK.into(),
                    },
                    ColorStop {
                        offset: 1.0,
                        color: Color::WHITE.into(),
                    },
                ][..],
            );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &gradient,
            None,
            &Rect::new(0.0, 0.0, 16.0, 16.0),
        );
        scene
    }

    fn ppt8_differential_scene(width: u32, height: u32) -> Scene {
        let mut scene = Scene::new();
        let full = Rect::new(
            -1.25,
            -0.75,
            f64::from(width) + 0.5,
            f64::from(height) + 0.25,
        );
        for shade in 0..40u8 {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(shade.wrapping_mul(7), 30, 200, 180),
                None,
                &full,
            );
        }
        let stops = [
            ColorStop {
                offset: 0.0,
                color: Color::from_rgb8(30, 60, 90).into(),
            },
            ColorStop {
                offset: 1.0,
                color: Color::from_rgb8(220, 150, 40).into(),
            },
        ];
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            &Gradient::new_linear(
                Point::new(-1.0, 0.25),
                Point::new(16.75, f64::from(height) - 0.25),
            )
            .with_stops(stops.as_slice()),
            None,
            &Rect::new(-1.25, -0.75, 16.75, f64::from(height) + 0.25),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &Gradient::new_two_point_radial(
                Point::new(20.125, 8.25),
                0.75,
                Point::new(24.625, 8.75),
                12.5,
            )
            .with_stops(stops.as_slice()),
            None,
            &Rect::new(15.25, 0.25, 33.75, f64::from(height) - 0.25),
        );
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            &Gradient::new_sweep(
                Point::new(f64::from(width) - 8.25, f64::from(height) * 0.5),
                0.0,
                std::f32::consts::TAU,
            )
            .with_stops(stops.as_slice()),
            None,
            &Rect::new(
                32.25,
                0.25,
                f64::from(width) + 0.5,
                f64::from(height) - 0.25,
            ),
        );
        let image = ImageData {
            data: Blob::new(Arc::new(vec![
                255, 20, 120, 200, 20, 240, 90, 144, 30, 60, 255, 176, 250, 210, 40, 224,
            ])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 2,
            height: 2,
        };
        scene.draw_image(
            &image,
            Affine::translate((39.25, 8.25)) * Affine::scale_non_uniform(4.5, 4.0),
        );
        scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            Rect::new(20.25, 3.5, 29.75, f64::from(height) - 3.25),
            Color::from_rgba8(20, 240, 90, 160),
            2.25,
            1.0,
        );
        for depth in 0..9u8 {
            scene.push_layer(
                Fill::EvenOdd,
                if depth == 8 {
                    BlendMode::new(Mix::Multiply, Compose::SrcOver)
                } else {
                    BlendMode::default()
                },
                0.75 - f32::from(depth) * 0.02,
                Affine::IDENTITY,
                &Rect::new(1.25, 1.0, 14.75, f64::from(height) - 1.25),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 180),
            None,
            &Rect::new(1.25, 1.0, 14.75, f64::from(height) - 1.25),
        );
        for _ in 0..9 {
            scene.pop_layer();
        }
        let mut triangle = BezPath::new();
        triangle.move_to((1.25, 1.5));
        triangle.line_to((f64::from(width) - 1.5, 2.25));
        triangle.line_to((20.75, f64::from(height) - 1.5));
        triangle.close_path();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(80, 30, 255, 170),
            None,
            &triangle,
        );
        scene
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the test intentionally leaves compact GPU proof evidence in the test log"
    )]
    fn area_ppt8_shared_coarse_preserves_exact_area_bytes() {
        let (device, queue) =
            headless_device().expect("PPT8 shared-coarse proof requires GPU adapter");
        const WIDTH: u32 = 49;
        const HEIGHT: u32 = 17;
        const DAMAGE_X: u32 = 15;
        const DAMAGE_WIDTH: u32 = 34;
        const DAMAGE_X_USIZE: usize = 15;
        let (p4_full_texture, p4_full) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (p8_full_texture, p8_full) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (p4_sparse_texture, p4_sparse) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (p8_sparse_texture, p8_sparse) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_area_ppt8_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("PPT8 shader set must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        assert!(renderer.area_ppt8_requested());
        assert!(renderer.area_ppt8_available());
        let scene = ppt8_differential_scene(WIDTH, HEIGHT);
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let stats = renderer
            .render_to_textures_shared_coarse_area_ppt8_with_post_render(
                &device,
                &queue,
                &scene,
                &p4_full,
                &p8_full,
                &p4_sparse,
                &p8_sparse,
                &params,
                &[FineDamageRect {
                    // Begins one pixel before a native boundary and runs through the partial
                    // final column, so every P4 subgroup boundary is exercised while sparse
                    // stores still leave a sentinel region outside requested damage.
                    x: DAMAGE_X,
                    y: 0,
                    width: DAMAGE_WIDTH,
                    height: HEIGHT,
                }],
                |_| {},
            )
            .expect("shared P4/PPT8 recording must submit")
            .expect("full native damage must be represented");
        assert_eq!(stats.fine_tiles, 8);
        let (p4_full_bytes, p4_full_stride) =
            readback_texture_rows(&device, &queue, &p4_full_texture, WIDTH, HEIGHT);
        let (p8_full_bytes, p8_full_stride) =
            readback_texture_rows(&device, &queue, &p8_full_texture, WIDTH, HEIGHT);
        let (p4_sparse_bytes, p4_sparse_stride) =
            readback_texture_rows(&device, &queue, &p4_sparse_texture, WIDTH, HEIGHT);
        let (p8_sparse_bytes, p8_sparse_stride) =
            readback_texture_rows(&device, &queue, &p8_sparse_texture, WIDTH, HEIGHT);
        let tight = usize::try_from(WIDTH).unwrap() * 4;
        for y in 0..usize::try_from(HEIGHT).unwrap() {
            let p4_full_row = &p4_full_bytes[y * p4_full_stride..y * p4_full_stride + tight];
            let p8_full_row = &p8_full_bytes[y * p8_full_stride..y * p8_full_stride + tight];
            let p4_sparse_row =
                &p4_sparse_bytes[y * p4_sparse_stride..y * p4_sparse_stride + tight];
            let p8_sparse_row =
                &p8_sparse_bytes[y * p8_sparse_stride..y * p8_sparse_stride + tight];
            assert_eq!(
                p8_full_row, p4_full_row,
                "full PPT8 byte mismatch on row {y}"
            );
            for x in 0..usize::try_from(WIDTH).unwrap() {
                let offset = x * 4;
                if x >= DAMAGE_X_USIZE {
                    assert_eq!(
                        &p4_sparse_row[offset..offset + 4],
                        &p4_full_row[offset..offset + 4],
                        "sparse P4 byte mismatch at {x},{y}"
                    );
                    assert_eq!(
                        &p8_sparse_row[offset..offset + 4],
                        &p4_full_row[offset..offset + 4],
                        "sparse PPT8 byte mismatch at {x},{y}"
                    );
                } else {
                    assert_eq!(&p4_sparse_row[offset..offset + 4], &[0; 4]);
                    assert_eq!(&p8_sparse_row[offset..offset + 4], &[0; 4]);
                }
            }
        }
        assert!(
            texture_has_nonzero_and_varied_pixels(&p4_full_bytes, p4_full_stride, WIDTH, HEIGHT),
            "PPT8 differential fixture must produce nonempty varied P4 bytes"
        );
        assert!(
            texture_has_nonzero_and_varied_pixels(
                &p4_sparse_bytes,
                p4_sparse_stride,
                WIDTH,
                HEIGHT,
            ),
            "PPT8 sparse differential fixture must produce nonempty varied P4 bytes"
        );
        assert!(
            !renderer.settle_bump_budget(&device),
            "PPT8 differential fixture must complete without bump overflow"
        );
        eprintln!(
            "area-ppt8 shared proof: extent={WIDTH}x{HEIGHT} route=shared-coarse-p4-ppt8 fine_tiles={} compared_bytes={} overflow=false",
            stats.fine_tiles,
            u64::from(WIDTH)
                .saturating_mul(u64::from(HEIGHT))
                .saturating_mul(4),
        );
    }

    fn ppt8_native_scene() -> Scene {
        let mut scene = Scene::new();
        let stops = [
            ColorStop {
                offset: 0.0,
                color: Color::from_rgba8(20, 180, 255, 192).into(),
            },
            ColorStop {
                offset: 1.0,
                color: Color::from_rgba8(255, 40, 80, 144).into(),
            },
        ];
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            &Gradient::new_linear(Point::new(3753.25, 79.75), Point::new(3839.5, 111.25))
                .with_stops(stops.as_slice()),
            None,
            &Rect::new(3753.25, 79.75, 3839.5, 111.25),
        );
        let mut triangle = BezPath::new();
        triangle.move_to((3761.125, 82.5));
        triangle.line_to((3839.75, 96.25));
        triangle.line_to((3770.375, 110.5));
        triangle.close_path();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 176),
            None,
            &triangle,
        );
        scene
    }

    #[test]
    #[ignore = "native 4K PPT8 shared-coarse GPU proof"]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the ignored native proof intentionally leaves compact GPU evidence in the test log"
    )]
    fn area_ppt8_shared_coarse_native_4k_preserves_exact_bytes() {
        let (device, queue) =
            headless_device().expect("native PPT8 shared-coarse proof requires GPU adapter");
        const WIDTH: u32 = 3_840;
        const HEIGHT: u32 = 2_160;
        let (p4_full_texture, p4_full) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (p8_full_texture, p8_full) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (_p4_sparse_texture, p4_sparse) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (_p8_sparse_texture, p8_sparse) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_area_ppt8_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("native PPT8 shader set must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let stats = renderer
            .render_to_textures_shared_coarse_area_ppt8_with_post_render(
                &device,
                &queue,
                &ppt8_native_scene(),
                &p4_full,
                &p8_full,
                &p4_sparse,
                &p8_sparse,
                &params,
                &[FineDamageRect {
                    x: 3_760,
                    y: 80,
                    width: 80,
                    height: 32,
                }],
                |_| {},
            )
            .expect("native shared P4/PPT8 recording must submit")
            .expect("native sparse damage must be represented");
        assert!(stats.fine_tiles > 0);
        let (p4_bytes, p4_stride) =
            readback_texture_rows(&device, &queue, &p4_full_texture, WIDTH, HEIGHT);
        let (p8_bytes, p8_stride) =
            readback_texture_rows(&device, &queue, &p8_full_texture, WIDTH, HEIGHT);
        let tight = usize::try_from(WIDTH).unwrap() * 4;
        for y in 0..usize::try_from(HEIGHT).unwrap() {
            assert_eq!(
                &p8_bytes[y * p8_stride..y * p8_stride + tight],
                &p4_bytes[y * p4_stride..y * p4_stride + tight],
                "native full PPT8 byte mismatch on row {y}"
            );
        }
        assert!(
            texture_has_nonzero_and_varied_pixels(&p4_bytes, p4_stride, WIDTH, HEIGHT),
            "native PPT8 fixture must produce nonempty varied P4 bytes"
        );
        assert!(
            !renderer.settle_bump_budget(&device),
            "native PPT8 fixture must complete without bump overflow"
        );
        eprintln!(
            "area-ppt8 native proof: extent={WIDTH}x{HEIGHT} route=shared-coarse-p4-ppt8 fine_tiles={} compared_bytes={} overflow=false",
            stats.fine_tiles,
            u64::from(WIDTH)
                .saturating_mul(u64::from(HEIGHT))
                .saturating_mul(4),
        );
    }

    fn render_params() -> RenderParams {
        RenderParams {
            base_color: Color::TRANSPARENT,
            width: 16,
            height: 16,
            antialiasing_method: AaConfig::Area,
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_damage_post_render_shares_submission_and_preserves_exact_pixels() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let (_target_texture, target) = damage_target(&device, &queue);
        let mut renderer = Renderer::new(
            &device,
            RendererOptions {
                num_init_threads: NonZeroUsize::new(1),
                ..Default::default()
            },
        )
        .unwrap();
        renderer.sparse_upload_reuse = true;
        if !renderer.sparse_fine_supported() {
            return;
        }
        let scene = solid_scene();
        let params = render_params();
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.sparse_post_render_test_readback"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut callback_count = 0;
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &target,
                &params,
                &[FineDamageRect {
                    x: 4,
                    y: 4,
                    width: 8,
                    height: 8,
                }],
                |encoder| {
                    callback_count += 1;
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo {
                            texture: &_target_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(256),
                                rows_per_image: Some(16),
                            },
                        },
                        wgpu::Extent3d {
                            width: 16,
                            height: 16,
                            depth_or_array_layers: 1,
                        },
                    );
                },
            )
            .unwrap();
        assert!(stats.is_some());
        assert_eq!(callback_count, 1);

        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range();
        let pixel = |x: usize, y: usize| &pixels[y * 256 + x * 4..y * 256 + x * 4 + 4];
        assert_eq!(pixel(7, 7), &[255, 255, 255, 255]);
        assert_eq!(pixel(3, 3), &[0, 0, 0, 0]);
        assert_eq!(pixel(12, 12), &[0, 0, 0, 0]);
        drop(pixels);
        readback.unmap();

        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &_target_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &[0; 16 * 16 * 4],
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(16 * 4),
                rows_per_image: Some(16),
            },
            wgpu::Extent3d {
                width: 16,
                height: 16,
                depth_or_array_layers: 1,
            },
        );
        renderer.sparse_upload_reuse = false;
        callback_count = 0;
        assert!(
            renderer
                .render_to_texture_with_damage_and_post_render(
                    &device,
                    &queue,
                    &scene,
                    &target,
                    &params,
                    &[FineDamageRect {
                        x: 4,
                        y: 4,
                        width: 8,
                        height: 8,
                    }],
                    |encoder| {
                        callback_count += 1;
                        encoder.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture: &_target_texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: &readback,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(256),
                                    rows_per_image: Some(16),
                                },
                            },
                            wgpu::Extent3d {
                                width: 16,
                                height: 16,
                                depth_or_array_layers: 1,
                            },
                        );
                    },
                )
                .unwrap()
                .is_some()
        );
        assert_eq!(callback_count, 1);
        let slice = readback.slice(..);
        let (sender, receiver) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |result| {
            sender.send(result).unwrap()
        });
        device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
        receiver.recv().unwrap().unwrap();
        let pixels = slice.get_mapped_range();
        let pixel = |x: usize, y: usize| &pixels[y * 256 + x * 4..y * 256 + x * 4 + 4];
        assert_eq!(pixel(7, 7), &[255, 255, 255, 255]);
        assert_eq!(pixel(3, 3), &[0, 0, 0, 0]);
        drop(pixels);
        readback.unmap();
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_damage_post_render_skips_unsupported_and_empty_submissions() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let (_target_texture, target) = damage_target(&device, &queue);
        let mut renderer = Renderer::new(
            &device,
            RendererOptions {
                num_init_threads: NonZeroUsize::new(1),
                ..Default::default()
            },
        )
        .unwrap();
        let scene = solid_scene();
        let mut callback_count = 0;
        let unsupported = RenderParams {
            antialiasing_method: AaConfig::Msaa8,
            ..render_params()
        };
        assert!(
            renderer
                .render_to_texture_with_damage_and_post_render(
                    &device,
                    &queue,
                    &scene,
                    &target,
                    &unsupported,
                    &[FineDamageRect {
                        x: 0,
                        y: 0,
                        width: 1,
                        height: 1,
                    }],
                    |_| callback_count += 1,
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(callback_count, 0);

        assert!(
            renderer
                .render_to_texture_with_damage_and_post_render(
                    &device,
                    &queue,
                    &scene,
                    &target,
                    &render_params(),
                    &[],
                    |_| callback_count += 1,
                )
                .unwrap()
                .is_none()
        );
        assert_eq!(callback_count, 0);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_tile_bounds_active_gpu_fixture_preserves_left_of_roi_winding() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 3_840;
        const HEIGHT: u32 = 2_160;
        const DAMAGE_X: u32 = 1_920;
        const DAMAGE_Y: u32 = 1_072;
        const SECOND_DAMAGE_X: u32 = 2_192;
        let (texture, target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = Renderer::new(
            &device,
            RendererOptions {
                num_init_threads: NonZeroUsize::new(1),
                ..Default::default()
            },
        )
        .unwrap();
        // Configure the isolated fixture directly instead of changing global
        // environment variables while other renderer tests may be running.
        renderer.engine.enable_sparse_tile_bounds_fixture(true);
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("fixture permutations must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        assert!(renderer.sparse_tile_bounds_available());

        let mut scene = Scene::new();
        // Nested NonZero/EvenOdd clips and a negative source extent keep this
        // fixture on the path-bbox/winding boundary rather than a trivial quad.
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(-16.0, -16.0, f64::from(WIDTH + 16), f64::from(HEIGHT + 16)),
        );
        scene.push_clip_layer(
            Fill::EvenOdd,
            Affine::IDENTITY,
            &Rect::new(-8.0, -8.0, f64::from(WIDTH + 8), f64::from(HEIGHT + 8)),
        );
        for _ in 0..3 {
            scene.push_clip_layer(
                Fill::NonZero,
                Affine::IDENTITY,
                &Rect::new(-4.0, -4.0, f64::from(WIDTH + 4), f64::from(HEIGHT + 4)),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(-32.0, -32.0, f64::from(WIDTH + 32), f64::from(HEIGHT + 32)),
        );
        for _ in 0..5 {
            scene.pop_layer();
        }
        let mut dense_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(
            &mut dense_renderer,
            &device,
            &queue,
            &scene,
            &dense_target,
            &RenderParams {
                base_color: Color::TRANSPARENT,
                width: WIDTH,
                height: HEIGHT,
                antialiasing_method: AaConfig::Area,
            },
        );
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.sparse_tile_bounds_left_winding"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.dense_tile_bounds_left_winding"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let second_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.sparse_tile_bounds_second_damage"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_second_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.dense_tile_bounds_second_damage"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &target,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: WIDTH,
                    height: HEIGHT,
                    antialiasing_method: AaConfig::Area,
                },
                &[
                    FineDamageRect {
                        x: DAMAGE_X,
                        y: DAMAGE_Y,
                        width: 16,
                        height: 16,
                    },
                    FineDamageRect {
                        x: SECOND_DAMAGE_X,
                        y: DAMAGE_Y,
                        width: 16,
                        height: 16,
                    },
                ],
                |encoder| {
                    copy_damage_tile(encoder, &texture, &readback, DAMAGE_X, DAMAGE_Y);
                    copy_damage_tile(encoder, &dense_texture, &dense_readback, DAMAGE_X, DAMAGE_Y);
                    copy_damage_tile(
                        encoder,
                        &texture,
                        &second_readback,
                        SECOND_DAMAGE_X,
                        DAMAGE_Y,
                    );
                    copy_damage_tile(
                        encoder,
                        &dense_texture,
                        &dense_second_readback,
                        SECOND_DAMAGE_X,
                        DAMAGE_Y,
                    );
                },
            )
            .unwrap()
            .expect("sparse fixture must submit");
        assert!(stats.coarse_active_bins);
        assert!(stats.path_tile_bounds_applied);
        assert_eq!(stats.path_tile_bounds_tiles, 18);
        let sparse = readback_tile(&device, &readback);
        assert_eq!(sparse, readback_tile(&device, &dense_readback));
        assert_eq!(
            readback_tile(&device, &second_readback),
            readback_tile(&device, &dense_second_readback)
        );
        assert_eq!(&sparse[7 * 256 + 7 * 4..7 * 256 + 7 * 4 + 4], &[255; 4]);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_tile_bounds_forced_active_preserves_partial_native_edge() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 529;
        const HEIGHT: u32 = 257;
        let (texture, target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_sparse_tile_bounds_fixture(false);
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("fixture permutations must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        assert!(renderer.sparse_tile_bounds_available());
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT)),
        );
        let mut dense_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(
            &mut dense_renderer,
            &device,
            &queue,
            &scene,
            &dense_target,
            &RenderParams {
                base_color: Color::TRANSPARENT,
                width: WIDTH,
                height: HEIGHT,
                antialiasing_method: AaConfig::Area,
            },
        );
        let readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.sparse_tile_bounds_partial_edge"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.dense_tile_bounds_partial_edge"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &target,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: WIDTH,
                    height: HEIGHT,
                    antialiasing_method: AaConfig::Area,
                },
                &[FineDamageRect {
                    x: WIDTH - 1,
                    y: HEIGHT - 1,
                    width: 1,
                    height: 1,
                }],
                |encoder| {
                    copy_damage_tile(encoder, &texture, &readback, WIDTH - 16, HEIGHT - 16);
                    copy_damage_tile(
                        encoder,
                        &dense_texture,
                        &dense_readback,
                        WIDTH - 16,
                        HEIGHT - 16,
                    );
                },
            )
            .unwrap()
            .expect("partial-edge sparse fixture must submit");
        assert!(stats.coarse_active_bins);
        assert!(stats.path_tile_bounds_applied);
        assert_eq!(stats.path_tile_bounds_tiles, 1);
        assert!(stats.fine_quad_skip_applied);
        assert_eq!(stats.fine_quad_total_quads, 64);
        assert_eq!(stats.fine_quad_active_quads, 1);
        let pixels = readback_tile(&device, &readback);
        let dense_pixels = readback_tile(&device, &dense_readback);
        assert_eq!(&pixels[15 * 256 + 15 * 4..15 * 256 + 15 * 4 + 4], &[255; 4]);
        assert_eq!(
            &pixels[15 * 256 + 15 * 4..15 * 256 + 15 * 4 + 4],
            &dense_pixels[15 * 256 + 15 * 4..15 * 256 + 15 * 4 + 4]
        );
        assert_eq!(&pixels[..4], &[0; 4]);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_opaque_prune_preserves_pixels_and_restarts_the_tile_ptcl_head() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 48;
        const HEIGHT: u32 = 16;
        let (sparse_texture, sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut scene = Scene::new();
        // The middle native tile has no edges for these full-width fills.
        // Enough prior commands force a dynamic PTCL jump before the final
        // opaque white fill restarts the fixed head.
        for shade in 0..24u8 {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(shade, 0, 0, 128),
                None,
                &Rect::new(-1.0, -1.0, f64::from(WIDTH + 1), f64::from(HEIGHT + 1)),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(-1.0, -1.0, f64::from(WIDTH + 1), f64::from(HEIGHT + 1)),
        );
        // This later translucent paint must remain after the restarted list.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 255, 128),
            None,
            &Rect::new(17.25, 2.5, 30.75, 13.5),
        );
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let mut dense_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(
            &mut dense_renderer,
            &device,
            &queue,
            &scene,
            &dense_target,
            &params,
        );
        let (baseline_full_texture, baseline_full_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (baseline_sparse_texture, baseline_sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut baseline_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        let baseline = baseline_renderer
            .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                &device,
                &queue,
                &scene,
                &baseline_full_target,
                &baseline_sparse_target,
                &params,
                &[FineDamageRect {
                    x: 16,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
                |_encoder| {},
            )
            .unwrap()
            .expect("baseline geometry capture must submit");
        let baseline_tags = reachable_ptcl_tags(&readback_words(&device, &baseline.1.ptcl), 65);
        assert!(
            baseline_tags.contains(&12),
            "lower translucent commands need a reachable jump"
        );
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_sparse_opaque_prune_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("opaque-prune shader must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        assert!(renderer.sparse_opaque_prune_available());
        let sparse_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_sparse"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_dense"),
            size: 256 * 16,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &sparse_target,
                &params,
                &[FineDamageRect {
                    x: 16,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
                |encoder| {
                    copy_damage_tile(encoder, &sparse_texture, &sparse_readback, 16, 0);
                    copy_damage_tile(encoder, &dense_texture, &dense_readback, 16, 0);
                },
            )
            .unwrap()
            .expect("sparse opaque-prune fixture must submit");
        assert!(stats.coarse_opaque_prune_selected);
        assert!(!stats.fine_quad_skip_applied);
        assert_eq!(
            readback_tile(&device, &sparse_readback),
            readback_tile(&device, &dense_readback)
        );

        // The public shared-coarse diagnostic remains unpruned. This bounded
        // test-only substitution selects the already compiled normal coarse
        // permutation so its PTCL head can be inspected without a production
        // readback or an opaque-prune-specific buffer.
        let (full_texture, full_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (geometry_sparse_texture, geometry_sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut geometry_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        geometry_renderer
            .engine
            .enable_sparse_opaque_prune_fixture();
        geometry_renderer.shaders = shaders::full_shaders(
            &device,
            &mut geometry_renderer.engine,
            &geometry_renderer.options,
        )
        .expect("opaque-prune diagnostic shader must compile");
        geometry_renderer
            .engine
            .build_shaders_if_needed(&device, geometry_renderer.options.num_init_threads);
        let prune = geometry_renderer
            .shaders
            .coarse_opaque_prune
            .expect("normal opaque-prune permutation must be available");
        geometry_renderer.shaders.coarse = prune;
        let result = geometry_renderer
            .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                &device,
                &queue,
                &scene,
                &full_target,
                &geometry_sparse_target,
                &params,
                &[FineDamageRect {
                    x: 16,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
                |_encoder| {},
            )
            .unwrap()
            .expect("geometry capture must submit");
        assert!(!result.0.coarse_opaque_prune_selected);
        let ptcl = readback_words(&device, &result.1.ptcl);
        // Tile one begins at word 64. Its preserved header is followed by the
        // opaque solid/color pair, proving the prior chain was discarded.
        assert_eq!(ptcl[65], 3);
        assert_eq!(ptcl[66], 5);
        assert_eq!(ptcl[67], 0xffff_ffff);
        assert_ne!(ptcl[65], 12);
        let pruned_tags = reachable_ptcl_tags(&ptcl, 65);
        assert!(!pruned_tags.contains(&12));
        assert!(pruned_tags.len() < baseline_tags.len());
        drop((
            baseline_full_texture,
            baseline_sparse_texture,
            full_texture,
            geometry_sparse_texture,
        ));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_opaque_prune_gpu_guard_matrix_preserves_deep_clips_and_unsafe_tiles() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 144;
        const HEIGHT: u32 = 16;
        let (sparse_texture, sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut scene = Scene::new();
        // A closed stack deeper than the fine spill threshold must leave its
        // historical maximum untouched before the eligible opaque replacement.
        for _ in 0..5 {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                0.5,
                Affine::IDENTITY,
                &Rect::new(-1.0, -1.0, 17.0, 17.0),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(-1.0, -1.0, 17.0, 17.0),
        );
        for _ in 0..5 {
            scene.pop_layer();
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(-1.0, -1.0, 17.0, 17.0),
        );
        // Alpha 254, a crossing edge, and a live clip must each retain their
        // previous command history even though the coarse permutation is on.
        for _ in 0..5 {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                0.5,
                Affine::IDENTITY,
                &Rect::new(31.0, -1.0, 49.0, 17.0),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(31.0, -1.0, 49.0, 17.0),
        );
        for _ in 0..5 {
            scene.pop_layer();
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 255, 0, 254),
            None,
            &Rect::new(31.0, -1.0, 49.0, 17.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(63.0, -1.0, 81.0, 17.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(65.25, -1.0, 78.5, 17.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(95.0, -1.0, 113.0, 17.0),
        );
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(95.25, 0.25, 112.75, 15.75),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(95.0, -1.0, 113.0, 17.0),
        );
        scene.pop_layer();
        // A doubled EvenOdd path has a clear backdrop in the last tile.
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(127.0, -1.0, 145.0, 17.0),
        );
        let mut doubled = BezPath::new();
        for _ in 0..2 {
            doubled.move_to((127.0, -1.0));
            doubled.line_to((145.0, -1.0));
            doubled.line_to((145.0, 17.0));
            doubled.line_to((127.0, 17.0));
            doubled.close_path();
        }
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &doubled,
        );
        // A later fractional source clip plus translucent accent verifies the
        // restarted list still appends commands in original order.
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(0.25, 0.25, 143.5, 15.5),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(0, 0, 255, 128),
            None,
            &Rect::new(-1.0, -1.0, 145.0, 17.0),
        );
        scene.pop_layer();
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let mut dense_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(
            &mut dense_renderer,
            &device,
            &queue,
            &scene,
            &dense_target,
            &params,
        );
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_sparse_opaque_prune_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("opaque-prune shader must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        let sparse_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_guard_sparse"),
            size: 768 * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_guard_dense"),
            size: 768 * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let damage = FineDamageRect {
            x: 0,
            y: 2,
            width: WIDTH,
            height: 12,
        };
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &sparse_target,
                &params,
                &[damage],
                |encoder| {
                    for (texture, readback) in [
                        (&sparse_texture, &sparse_readback),
                        (&dense_texture, &dense_readback),
                    ] {
                        encoder.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d::ZERO,
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: readback,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(768),
                                    rows_per_image: Some(HEIGHT),
                                },
                            },
                            wgpu::Extent3d {
                                width: WIDTH,
                                height: HEIGHT,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                },
            )
            .unwrap()
            .expect("guard matrix sparse render must submit");
        assert!(stats.coarse_opaque_prune_selected);
        assert!(!stats.fine_quad_skip_applied);
        let sparse = readback_tile(&device, &sparse_readback);
        let dense = readback_tile(&device, &dense_readback);
        let mut nontransparent_requested = false;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = usize::try_from(y * 768 + x * 4).unwrap();
                if y >= damage.y && y < damage.y + damage.height {
                    assert_eq!(&sparse[offset..offset + 4], &dense[offset..offset + 4]);
                    nontransparent_requested |= sparse[offset + 3] != 0;
                } else {
                    assert_eq!(&sparse[offset..offset + 4], &[0; 4]);
                }
            }
        }
        assert!(nontransparent_requested);

        // Keep the production shared-coarse API ordinary. This test-only
        // substitution captures the prune PTCL so unsafe tiles can be checked
        // for retained command history, while the two deep tiles ensure at
        // least one nonzero blend spill offset survives a post-close prune.
        let (geometry_full_texture, geometry_full_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (geometry_sparse_texture, geometry_sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut geometry_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        geometry_renderer
            .engine
            .enable_sparse_opaque_prune_fixture();
        geometry_renderer.shaders = shaders::full_shaders(
            &device,
            &mut geometry_renderer.engine,
            &geometry_renderer.options,
        )
        .expect("opaque-prune guard shader must compile");
        let prune = geometry_renderer
            .shaders
            .coarse_opaque_prune
            .expect("guard normal prune permutation must be available");
        geometry_renderer.shaders.coarse = prune;
        geometry_renderer
            .engine
            .build_shaders_if_needed(&device, geometry_renderer.options.num_init_threads);
        let geometry = geometry_renderer
            .render_to_textures_shared_coarse_area_with_geometry_and_post_render(
                &device,
                &queue,
                &scene,
                &geometry_full_target,
                &geometry_sparse_target,
                &params,
                &[damage],
                |_encoder| {},
            )
            .unwrap()
            .expect("guard geometry capture must submit");
        let ptcl = readback_words(&device, &geometry.1.ptcl);
        assert!(
            ptcl[0] != 0 || ptcl[128] != 0,
            "one of the two deep tiles must retain its nonzero blend spill offset"
        );
        // Tile zero is the eligible post-close opaque target: it resets the
        // cursor but keeps its precomputed blend-spill header intact.
        assert_eq!(ptcl[1], 3);
        assert_eq!(ptcl[2], 5);
        assert_eq!(ptcl[3], 0xffff_ffff);
        let alpha254_tags = reachable_ptcl_tags(&ptcl, 129);
        assert!(alpha254_tags.contains(&10) && alpha254_tags.contains(&11));
        for tile_base in [256usize, 384, 512] {
            assert_eq!(ptcl[tile_base + 1], 3);
            assert_eq!(ptcl[tile_base + 2], 5);
            assert_eq!(ptcl[tile_base + 3] >> 24, 128);
        }
        drop((geometry_full_texture, geometry_sparse_texture));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_opaque_prune_preserves_partial_native_edge_damage() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 49;
        const HEIGHT: u32 = 17;
        let (sparse_texture, sparse_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(-1.0, -1.0, 50.0, 18.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 0, 0, 128),
            None,
            &Rect::new(48.25, 16.25, 49.0, 17.0),
        );
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let mut dense_renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(
            &mut dense_renderer,
            &device,
            &queue,
            &scene,
            &dense_target,
            &params,
        );
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_sparse_opaque_prune_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("opaque-prune edge shader must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        let sparse_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_partial_sparse"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.opaque_prune_partial_dense"),
            size: 256,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stats = renderer
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &sparse_target,
                &params,
                &[FineDamageRect {
                    x: WIDTH - 1,
                    y: HEIGHT - 1,
                    width: 1,
                    height: 1,
                }],
                |encoder| {
                    for (texture, readback) in [
                        (&sparse_texture, &sparse_readback),
                        (&dense_texture, &dense_readback),
                    ] {
                        encoder.copy_texture_to_buffer(
                            wgpu::TexelCopyTextureInfo {
                                texture,
                                mip_level: 0,
                                origin: wgpu::Origin3d {
                                    x: WIDTH - 1,
                                    y: HEIGHT - 1,
                                    z: 0,
                                },
                                aspect: wgpu::TextureAspect::All,
                            },
                            wgpu::TexelCopyBufferInfo {
                                buffer: readback,
                                layout: wgpu::TexelCopyBufferLayout {
                                    offset: 0,
                                    bytes_per_row: Some(256),
                                    rows_per_image: Some(1),
                                },
                            },
                            wgpu::Extent3d {
                                width: 1,
                                height: 1,
                                depth_or_array_layers: 1,
                            },
                        );
                    }
                },
            )
            .unwrap()
            .expect("partial-edge sparse render must submit");
        assert!(stats.coarse_opaque_prune_selected);
        assert!(!stats.fine_quad_skip_applied);
        let sparse = readback_tile(&device, &sparse_readback);
        let dense = readback_tile(&device, &dense_readback);
        assert_eq!(&sparse[..4], &dense[..4]);
        assert_ne!(&sparse[..4], &[0; 4]);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "test-only PTCL evidence is intentionally retained in the test log"
    )]
    fn sparse_solid_fuse_gpu_preserves_paints_clips_jumps_and_partial_damage() {
        let Some((device, queue)) = headless_device() else {
            eprintln!("solid-fuse PTCL proof skipped: no headless GPU device");
            return;
        };
        const WIDTH: u32 = 49;
        const HEIGHT: u32 = 17;
        const STRIDE: u32 = 256;
        let (dense_texture, dense_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (plain_texture, plain_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (fused_texture, fused_target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let stops = [
            ColorStop {
                offset: 0.0,
                color: Color::from_rgb8(30, 60, 90).into(),
            },
            ColorStop {
                offset: 1.0,
                color: Color::from_rgb8(220, 150, 40).into(),
            },
        ];
        let mut scene = Scene::new();
        let full = Rect::new(0.0, 0.0, f64::from(WIDTH), f64::from(HEIGHT));
        // More than one initial block remains after fusion, so the paired
        // stream crosses a reachable dynamic JUMP as well as all fused paint
        // successor kinds below.
        for i in 0..40u8 {
            scene.fill(
                Fill::NonZero,
                Affine::IDENTITY,
                Color::from_rgba8(i.wrapping_mul(7), 30, 200, 180),
                None,
                &full,
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &Gradient::new_linear(Point::new(0.0, 0.0), Point::new(49.0, 17.0))
                .with_stops(stops.as_slice()),
            None,
            &full,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &Gradient::new_two_point_radial(
                Point::new(20.0, 8.0),
                1.0,
                Point::new(24.0, 9.0),
                24.0,
            )
            .with_stops(stops.as_slice()),
            None,
            &full,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            &Gradient::new_sweep(Point::new(24.0, 8.0), 0.0, std::f32::consts::TAU)
                .with_stops(stops.as_slice()),
            None,
            &full,
        );
        let image = ImageData {
            data: Blob::new(Arc::new(vec![255, 20, 120, 200])),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width: 1,
            height: 1,
        };
        scene.draw_image(
            &image,
            Affine::scale_non_uniform(f64::from(WIDTH), f64::from(HEIGHT)),
        );
        scene.draw_blurred_rounded_rect(
            Affine::IDENTITY,
            Rect::new(-3.0, -3.0, 52.0, 20.0),
            Color::from_rgba8(20, 240, 90, 160),
            3.0,
            1.0,
        );
        for depth in 0..9u8 {
            scene.push_layer(
                Fill::EvenOdd,
                // Keep a non-default source-over alpha at every level, and
                // force spill depth beyond the in-workgroup blend stack.
                if depth == 8 {
                    BlendMode::new(Mix::Multiply, Compose::SrcOver)
                } else {
                    BlendMode::default()
                },
                0.75 - f32::from(depth) * 0.02,
                Affine::IDENTITY,
                &Rect::new(0.0, 0.0, 49.0, 17.0),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(255, 255, 255, 180),
            None,
            &full,
        );
        for _ in 0..9 {
            scene.pop_layer();
        }
        // This path remains segmented and must retain CMD_FILL.
        let mut triangle = BezPath::new();
        triangle.move_to((1.25, 1.5));
        triangle.line_to((47.5, 2.25));
        triangle.line_to((20.75, 15.5));
        triangle.close_path();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(80, 30, 255, 170),
            None,
            &triangle,
        );
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let mut dense = Renderer::new(&device, RendererOptions::default()).unwrap();
        render_dense_with_recovery(&mut dense, &device, &queue, &scene, &dense_target, &params);
        let mut plain = Renderer::new(&device, RendererOptions::default()).unwrap();
        let mut fused = Renderer::new(&device, RendererOptions::default()).unwrap();
        fused.engine.enable_sparse_solid_fuse_fixture();
        fused.shaders = shaders::full_shaders(&device, &mut fused.engine, &fused.options)
            .expect("solid-fuse shader must compile");
        fused
            .engine
            .build_shaders_if_needed(&device, fused.options.num_init_threads);
        assert!(fused.sparse_solid_fuse_available());
        let plain_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.solid_fuse_plain"),
            size: u64::from(STRIDE * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let fused_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.solid_fuse_fused"),
            size: u64::from(STRIDE * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let dense_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.solid_fuse_dense"),
            size: u64::from(STRIDE * HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let damage = [
            FineDamageRect {
                x: 0,
                y: 0,
                width: 16,
                height: 16,
            },
            FineDamageRect {
                x: 32,
                y: 0,
                width: 16,
                height: 16,
            },
            FineDamageRect {
                x: 48,
                y: 16,
                width: 1,
                height: 1,
            },
        ];
        let copy = |encoder: &mut wgpu::CommandEncoder,
                    texture: &wgpu::Texture,
                    readback: &wgpu::Buffer| {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer: readback,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(STRIDE),
                        rows_per_image: Some(HEIGHT),
                    },
                },
                wgpu::Extent3d {
                    width: WIDTH,
                    height: HEIGHT,
                    depth_or_array_layers: 1,
                },
            );
        };
        let plain_stats = plain
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &plain_target,
                &params,
                &damage,
                |encoder| copy(encoder, &plain_texture, &plain_readback),
            )
            .unwrap()
            .expect("plain sparse render must submit");
        let fused_stats = fused
            .render_to_texture_with_damage_and_post_render(
                &device,
                &queue,
                &scene,
                &fused_target,
                &params,
                &damage,
                |encoder| {
                    copy(encoder, &fused_texture, &fused_readback);
                    copy(encoder, &dense_texture, &dense_readback);
                },
            )
            .unwrap()
            .expect("fused sparse render must submit");
        assert!(!plain_stats.solid_fuse_selected);
        assert!(fused_stats.solid_fuse_selected);
        assert!(!fused_stats.fine_quad_skip_applied);
        let plain_pixels = readback_tile(&device, &plain_readback);
        let fused_pixels = readback_tile(&device, &fused_readback);
        let dense_pixels = readback_tile(&device, &dense_readback);
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = usize::try_from(y * STRIDE + x * 4).unwrap();
                let requested =
                    (x < 16 && y < 16) || (x >= 32 && x < 48 && y < 16) || (x == 48 && y == 16);
                if requested {
                    assert_eq!(
                        &fused_pixels[offset..offset + 4],
                        &dense_pixels[offset..offset + 4]
                    );
                    assert_eq!(
                        &plain_pixels[offset..offset + 4],
                        &dense_pixels[offset..offset + 4]
                    );
                } else {
                    assert_eq!(&fused_pixels[offset..offset + 4], &[0; 4]);
                    assert_eq!(&plain_pixels[offset..offset + 4], &[0; 4]);
                }
            }
        }

        // The narrow paired capture never invokes full/shared ordinary fine
        // on the fused stream. It proves reachable tag protocol changes, not
        // merely identical pixels.
        let (capture_plain_texture, capture_plain_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (capture_fused_texture, capture_fused_target) =
            damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut capture_plain = Renderer::new(&device, RendererOptions::default()).unwrap();
        let mut capture_fused = Renderer::new(&device, RendererOptions::default()).unwrap();
        capture_fused.engine.enable_sparse_solid_fuse_fixture();
        capture_fused.shaders =
            shaders::full_shaders(&device, &mut capture_fused.engine, &capture_fused.options)
                .expect("solid-fuse capture shader must compile");
        capture_fused
            .engine
            .build_shaders_if_needed(&device, capture_fused.options.num_init_threads);
        let capture_damage = [FineDamageRect {
            x: 0,
            y: 0,
            width: 16,
            height: 16,
        }];
        let plain_ptcl = capture_sparse_ptcl(
            &mut capture_plain,
            &device,
            &queue,
            &scene,
            &capture_plain_target,
            &params,
            &capture_damage,
            false,
        );
        let fused_ptcl = capture_sparse_ptcl(
            &mut capture_fused,
            &device,
            &queue,
            &scene,
            &capture_fused_target,
            &params,
            &capture_damage,
            true,
        );
        let (plain_tags, plain_words) = reachable_ptcl_words(&plain_ptcl, 1);
        let (fused_tags, fused_words) = reachable_ptcl_words(&fused_ptcl, 1);
        assert!(plain_tags.iter().any(|tag| *tag == 3));
        assert!(!fused_tags.iter().any(|tag| (*tag & !0x8000_0000) == 3));
        for successor in [5, 6, 7, 8, 9, 11, 13] {
            assert!(
                fused_tags
                    .iter()
                    .any(|tag| { (*tag & 0x8000_0000) != 0 && (*tag & !0x8000_0000) == successor })
            );
        }
        let plain_fill_count = plain_tags.iter().filter(|tag| **tag == 1).count();
        let fused_fill_count = fused_tags
            .iter()
            .filter(|tag| (**tag & !0x8000_0000) == 1)
            .count();
        assert!(plain_fill_count > 0);
        assert_eq!(fused_fill_count, plain_fill_count);
        for structural in [0, 1, 10, 12] {
            assert!(plain_tags.iter().any(|tag| *tag == structural));
            assert!(
                fused_tags
                    .iter()
                    .any(|tag| (*tag & !0x8000_0000) == structural)
            );
            assert!(
                !fused_tags.iter().any(|tag| {
                    (*tag & 0x8000_0000) != 0 && (*tag & !0x8000_0000) == structural
                })
            );
        }
        assert!(plain_words > fused_words);
        let flagged_successors: Vec<u32> = fused_tags
            .iter()
            .filter_map(|tag| ((*tag & 0x8000_0000) != 0).then_some(*tag & !0x8000_0000))
            .collect();
        eprintln!(
            "solid-fuse PTCL proof: plain commands={} words={plain_words}; fused commands={} words={fused_words}; flagged={flagged_successors:?}; jump=true fill=true captured_bytes={}",
            plain_tags.len(),
            fused_tags.len(),
            fused_ptcl.len() * size_of::<u32>(),
        );
        drop((capture_plain_texture, capture_fused_texture));
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_solid_fuse_stays_unavailable_for_default_and_full_cpu_renderers() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let plain = Renderer::new(&device, RendererOptions::default()).unwrap();
        assert!(!plain.sparse_solid_fuse_requested());
        assert!(!plain.sparse_solid_fuse_available());

        let mut cpu = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: true,
                ..RendererOptions::default()
            },
        )
        .unwrap();
        cpu.engine.enable_sparse_solid_fuse_fixture();
        cpu.shaders = shaders::full_shaders(&device, &mut cpu.engine, &cpu.options)
            .expect("CPU fallback shader set must compile");
        assert!(!cpu.sparse_fine_supported());
        assert!(!cpu.sparse_solid_fuse_available());
        assert!(cpu.shaders.coarse_solid_fuse.is_none());
        assert!(cpu.shaders.coarse_sparse_solid_fuse.is_none());
        assert!(cpu.shaders.coarse_sparse_active_solid_fuse.is_none());
        assert!(cpu.shaders.fine_sparse_area_solid_fuse.is_none());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_segment_row_mask_stays_unavailable_for_default_and_full_cpu_renderers() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let plain = Renderer::new(&device, RendererOptions::default()).unwrap();
        assert!(!plain.sparse_segment_row_mask_requested());
        assert!(!plain.sparse_segment_row_mask_available());
        assert!(!SparseRenderStats::default().segment_row_mask_selected);

        let mut cpu = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: true,
                ..RendererOptions::default()
            },
        )
        .unwrap();
        cpu.engine.enable_sparse_segment_row_mask_fixture(true);
        cpu.shaders = shaders::full_shaders(&device, &mut cpu.engine, &cpu.options)
            .expect("CPU fallback shader set must compile");
        assert!(cpu.sparse_segment_row_mask_requested());
        assert!(!cpu.sparse_fine_supported());
        assert!(!cpu.sparse_segment_row_mask_available());
        assert!(cpu.shaders.path_tiling_segment_row_mask.is_none());
        assert!(cpu.shaders.path_tiling_direct_segment_row_mask.is_none());
        assert!(cpu.shaders.fine_sparse_area_segment_row_mask.is_none());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn canonical_segment_order_defaults_off_and_excludes_cpu_dispatch() {
        let Some((device, _queue)) = headless_device() else {
            return;
        };
        let plain = Renderer::new(&device, RendererOptions::default()).unwrap();
        assert!(!plain.canonical_segment_order_requested());
        assert!(!plain.canonical_segment_order_available());
        assert!(!plain.last_canonical_segment_order_selected());
        assert!(!SparseRenderStats::default().canonical_segment_order_shader_selected);

        let mut cpu = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: true,
                ..RendererOptions::default()
            },
        )
        .unwrap();
        cpu.engine.enable_canonical_segment_order_fixture();
        cpu.shaders = shaders::full_shaders(&device, &mut cpu.engine, &cpu.options)
            .expect("CPU canonical-order shader set must compile");
        assert!(cpu.canonical_segment_order_requested());
        assert!(!cpu.canonical_segment_order_available());
        assert!(cpu.shaders.canonical_segment_order_full.is_none());
        assert!(cpu.shaders.canonical_segment_order_sparse.is_none());
        assert!(
            cpu.shaders
                .canonical_segment_order_sparse_solid_fuse
                .is_none()
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn canonical_segment_order_requires_an_unpinned_measurement_and_area() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_canonical_segment_order_fixture();
        renderer.engine.enable_sparse_tile_bounds_fixture(false);
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("canonical-order shader set must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        renderer.set_defer_bump_readback_enabled(false);
        let area = RenderParams {
            base_color: Color::TRANSPARENT,
            width: 16,
            height: 16,
            antialiasing_method: AaConfig::Area,
        };
        assert!(renderer.canonical_segment_order_available());
        assert!(renderer.canonical_segment_order_selected_for(&area, true));
        // Pending and pinned budgets both yield `measure == false`; the
        // experiment records no pass rather than adding a fresh transfer.
        assert!(!renderer.canonical_segment_order_selected_for(&area, false));
        let msaa = RenderParams {
            antialiasing_method: AaConfig::Msaa8,
            ..area
        };
        assert!(!renderer.canonical_segment_order_selected_for(&msaa, true));

        let (_target_texture, target) = damage_target(&device, &queue);
        let scene = Scene::new();
        renderer
            .render_to_texture(&device, &queue, &scene, &target, &area)
            .expect("canonical full fixture submits");
        assert!(renderer.last_canonical_segment_order_selected());
        assert!(!renderer.settle_bump_budget(&device));
        let sparse = renderer
            .render_to_texture_with_damage(
                &device,
                &queue,
                &scene,
                &target,
                &area,
                &[FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
            )
            .expect("canonical sparse fixture submits")
            .expect("nonempty sparse damage must submit");
        assert!(sparse.canonical_segment_order_shader_selected);
        assert!(renderer.last_canonical_segment_order_selected());
        assert!(!renderer.settle_bump_budget(&device));
        assert!(
            renderer
                .render_to_texture_with_damage(&device, &queue, &scene, &target, &msaa, &[])
                .expect("unsupported sparse AA has no submission")
                .is_none()
        );
        assert!(!renderer.last_canonical_segment_order_selected());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the test intentionally leaves compact GPU route evidence in the test log"
    )]
    fn area_ppt8_fixture_selects_area_routes_and_resets_on_fallback() {
        let (device, queue) =
            headless_device().expect("Area PPT8 routing proof requires GPU adapter");
        assert!(!SparseRenderStats::default().area_ppt8_shader_selected);

        let mut cpu = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: true,
                ..RendererOptions::default()
            },
        )
        .unwrap();
        cpu.engine.enable_area_ppt8_fixture();
        cpu.shaders = shaders::full_shaders(&device, &mut cpu.engine, &cpu.options)
            .expect("CPU Area PPT8 fallback shader set must compile");
        assert!(cpu.area_ppt8_requested());
        assert!(!cpu.area_ppt8_available());
        assert!(cpu.shaders.fine_area_ppt8.is_none());
        assert!(cpu.shaders.fine_sparse_area_ppt8.is_none());

        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_area_ppt8_fixture();
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("Area PPT8 shader set must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        let area = RenderParams {
            base_color: Color::TRANSPARENT,
            width: 16,
            height: 16,
            antialiasing_method: AaConfig::Area,
        };
        let msaa = RenderParams {
            antialiasing_method: AaConfig::Msaa8,
            ..area
        };
        assert!(renderer.area_ppt8_requested());
        assert!(renderer.area_ppt8_available());
        assert!(renderer.area_ppt8_selected_for(&area));
        assert!(renderer.sparse_area_ppt8_selected_for(&area));
        assert!(!renderer.area_ppt8_selected_for(&msaa));
        let (_texture, target) = damage_target(&device, &queue);
        let scene = solid_scene();
        renderer
            .render_to_texture(&device, &queue, &scene, &target, &area)
            .expect("Area PPT8 full fixture submits");
        assert!(renderer.last_area_ppt8_selected());
        assert!(!renderer.settle_bump_budget(&device));
        let sparse = renderer
            .render_to_texture_with_damage(
                &device,
                &queue,
                &scene,
                &target,
                &area,
                &[FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
            )
            .expect("Area PPT8 sparse fixture submits")
            .expect("nonempty sparse damage must submit");
        assert!(sparse.area_ppt8_shader_selected);
        assert!(renderer.last_area_ppt8_selected());
        assert!(!renderer.settle_bump_budget(&device));
        assert!(
            renderer
                .render_to_texture_with_damage(&device, &queue, &scene, &target, &msaa, &[])
                .expect("unsupported sparse AA has no submission")
                .is_none()
        );
        assert!(!renderer.last_area_ppt8_selected());
        let (_shared_full_texture, shared_full) = damage_target(&device, &queue);
        let (_shared_sparse_texture, shared_sparse) = damage_target(&device, &queue);
        let shared = renderer
            .render_to_textures_shared_coarse_area_with_post_render(
                &device,
                &queue,
                &scene,
                &shared_full,
                &shared_sparse,
                &area,
                &[FineDamageRect {
                    x: 0,
                    y: 0,
                    width: 16,
                    height: 16,
                }],
                |_| {},
            )
            .expect("ordinary shared diagnostic must submit")
            .expect("ordinary shared diagnostic has native damage");
        assert!(!shared.area_ppt8_shader_selected);
        assert!(!renderer.last_area_ppt8_selected());
        assert!(!renderer.settle_bump_budget(&device));
        // Requested incompatible sparse permutations preempt PPT8 before either fine route is
        // recorded, even if a caller has already prepared its PPT8 shader set.
        renderer.engine.enable_sparse_tile_bounds_fixture(false);
        assert!(!renderer.area_ppt8_selected_for(&area));
        assert!(!renderer.sparse_area_ppt8_selected_for(&area));
        eprintln!(
            "area-ppt8 route proof: extent=16x16 routes=full,sparse,msaa-fallback shared=ordinary quad-preempted overflow=false"
        );
    }

    fn clip_single_group_scene(clips: u32, width: u32, height: u32) -> Scene {
        let mut scene = Scene::new();
        for depth in 0..clips {
            let inset = f64::from(depth % 3) * 0.125;
            let right = if depth % 5 == 0 {
                f64::from(width) - 8.0 - inset
            } else {
                f64::from(width) + 0.5 - inset
            };
            scene.push_clip_layer(
                if depth % 2 == 0 {
                    Fill::NonZero
                } else {
                    Fill::EvenOdd
                },
                Affine::IDENTITY,
                &Rect::new(
                    -1.25 + inset,
                    -0.75 + inset,
                    right,
                    f64::from(height) + 0.25 - inset,
                ),
            );
        }
        let mut triangle = BezPath::new();
        triangle.move_to((-0.5, 1.25));
        triangle.line_to((f64::from(width) - 0.25, 2.75));
        triangle.line_to((f64::from(width) * 0.5, f64::from(height) - 0.5));
        triangle.close_path();
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::from_rgba8(40, 170, 240, 176),
            None,
            &triangle,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::from_rgba8(245, 90, 35, 156),
            None,
            &Rect::new(
                f64::from(width) - 48.25,
                f64::from(height) - 24.5,
                f64::from(width) + 1.0,
                f64::from(height) + 1.0,
            ),
        );
        scene.fill(
            Fill::EvenOdd,
            Affine::IDENTITY,
            Color::from_rgba8(35, 215, 150, 192),
            None,
            &Rect::new(
                f64::from(width) - 31.75,
                f64::from(height) - 16.25,
                f64::from(width) - 0.25,
                f64::from(height) - 0.25,
            ),
        );
        // Deliberately leave these clips open. Resolver emits the matching
        // end-clip draw tags while preserving the exact logical clip count.
        assert_eq!(scene.encoding().n_clips, clips);
        scene
    }

    fn clip_single_group_renderer(device: &Device) -> Renderer {
        let mut renderer = Renderer::new(device, RendererOptions::default()).unwrap();
        renderer.engine.enable_clip_single_group_fixture();
        renderer.shaders = shaders::full_shaders(device, &mut renderer.engine, &renderer.options)
            .expect("clip single-group shader set must compile");
        renderer
            .engine
            .build_shaders_if_needed(device, renderer.options.num_init_threads);
        renderer
    }

    fn assert_texture_rows_match(
        actual: &[u8],
        actual_stride: usize,
        expected: &[u8],
        expected_stride: usize,
        width: u32,
        height: u32,
        label: &str,
    ) {
        let tight = usize::try_from(width).unwrap().saturating_mul(4);
        for y in 0..usize::try_from(height).unwrap() {
            assert_eq!(
                &actual[y * actual_stride..y * actual_stride + tight],
                &expected[y * expected_stride..y * expected_stride + tight],
                "{label} logical row {y}",
            );
        }
    }

    fn shared_clip_leaf_fixture(
        device: &Device,
        queue: &Queue,
        n_clips: u32,
        mixed_push_end: bool,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        use vello_encoding::{ClipElement, ConfigUniform, DrawMonoid, Layout, PathBbox};

        let mut renderer = clip_single_group_renderer(device);
        let mut config = ConfigUniform::default();
        config.target_width = 49;
        config.target_height = 17;
        config.layout = Layout {
            n_clips,
            n_draw_objects: n_clips,
            n_paths: n_clips,
            ..Layout::default()
        };
        let mut recording = Recording::default();
        let config = recording.upload_uniform("clip.shared.config", bytemuck::bytes_of(&config));
        let mut clip_inp = Vec::with_capacity(usize::try_from(n_clips).unwrap().saturating_mul(2));
        for index in 0..n_clips {
            // An odd record closes the immediately preceding push. An odd
            // count therefore leaves one valid open clip, while preserving
            // the requested logical record count.
            let end_clip = mixed_push_end && index % 2 == 1;
            clip_inp.extend([index, if end_clip { !index } else { index }]);
        }
        let clip_inp = recording.upload("clip.shared.inp", bytemuck::cast_slice(&clip_inp));
        let paths = (0..n_clips)
            .map(|index| PathBbox {
                x0: i32::try_from(index % 7).unwrap() - 3,
                y0: i32::try_from(index % 5).unwrap() - 2,
                x1: 45 + i32::try_from(index % 4).unwrap(),
                y1: 14 + i32::try_from(index % 3).unwrap(),
                draw_flags: 0,
                trans_ix: 0,
            })
            .collect::<Vec<_>>();
        let paths = recording.upload("clip.shared.paths", bytemuck::cast_slice(&paths));
        let reduced = recording.upload("clip.shared.reduced", [0; 8]);
        let clip_els = recording.upload(
            "clip.shared.els",
            bytemuck::bytes_of(&ClipElement::default()),
        );
        let initial_draw = (0..n_clips)
            .map(|index| DrawMonoid {
                path_ix: index,
                clip_ix: index,
                scene_offset: 0x1111_0000 | index,
                info_offset: 0x2222_0000 | index,
            })
            .collect::<Vec<_>>();
        let initial_bbox = vec![0xa5_u8; usize::try_from(n_clips).unwrap().saturating_mul(16)];
        let ordinary_draw = recording.upload(
            "clip.shared.ordinary.draw",
            bytemuck::cast_slice(&initial_draw),
        );
        let ordinary_bbox = recording.upload("clip.shared.ordinary.bbox", initial_bbox.clone());
        let selected_draw = recording.upload(
            "clip.shared.selected.draw",
            bytemuck::cast_slice(&initial_draw),
        );
        let selected_bbox = recording.upload("clip.shared.selected.bbox", initial_bbox.clone());
        let grid = (n_clips.div_ceil(256), 1, 1);
        recording.dispatch(
            renderer.shaders.clip_leaf,
            grid,
            [
                config,
                clip_inp,
                paths,
                reduced,
                clip_els,
                ordinary_draw,
                ordinary_bbox,
            ],
        );
        let special = renderer
            .shaders
            .clip_leaf_single_group
            .expect("fixture shader exists");
        recording.dispatch(
            special,
            grid,
            [
                config,
                clip_inp,
                paths,
                reduced,
                clip_els,
                selected_draw,
                selected_bbox,
            ],
        );
        let dispatches = recording
            .commands
            .iter()
            .filter_map(|command| match command {
                recording::Command::Dispatch(shader, _, bindings)
                    if *shader == renderer.shaders.clip_leaf || *shader == special =>
                {
                    Some(bindings)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            dispatches.len(),
            2,
            "fixture must dispatch ordinary then specialised leaf"
        );
        let resource_id = |resource: recording::ResourceProxy| match resource {
            recording::ResourceProxy::Buffer(proxy)
            | recording::ResourceProxy::BufferRange { proxy, .. } => proxy.id,
            recording::ResourceProxy::Image(proxy) => proxy.id,
        };
        for binding in 0..5 {
            assert!(resource_id(dispatches[0][binding]) == resource_id(dispatches[1][binding]));
        }
        assert!(resource_id(dispatches[0][5]) != resource_id(dispatches[1][5]));
        assert!(resource_id(dispatches[0][6]) != resource_id(dispatches[1][6]));
        recording.download(ordinary_draw);
        recording.download(ordinary_bbox);
        recording.download(selected_draw);
        recording.download(selected_bbox);
        renderer
            .engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "clip_leaf_shared_fixture",
                #[cfg(feature = "wgpu-profiler")]
                &mut renderer.profiler,
            )
            .expect("shared clip fixture submits");
        let read = |engine: &mut WgpuEngine, proxy: recording::BufferProxy| {
            let mut bytes = readback_tile(
                device,
                &engine
                    .take_download(proxy)
                    .expect("fixture download exists"),
            );
            bytes.truncate(usize::try_from(proxy.size).unwrap());
            bytes
        };
        (
            read(&mut renderer.engine, ordinary_draw),
            read(&mut renderer.engine, ordinary_bbox),
            read(&mut renderer.engine, selected_draw),
            read(&mut renderer.engine, selected_bbox),
        )
    }

    fn assert_clip_leaf_outputs_match_for_count(
        device: &Device,
        queue: &Queue,
        clips: u32,
        mixed_push_end: bool,
    ) {
        let (ordinary_draw, ordinary_bbox, selected_draw, selected_bbox) =
            shared_clip_leaf_fixture(device, queue, clips, mixed_push_end);
        assert_eq!(
            selected_draw, ordinary_draw,
            "draw monoids for {clips} clips"
        );
        assert_eq!(
            selected_bbox, ordinary_bbox,
            "clip bboxes for {clips} clips"
        );
        if mixed_push_end {
            use vello_encoding::DrawMonoid;

            let mut observed_end_fixup = false;
            for end_index in (1..clips).step_by(2) {
                let byte_offset = usize::try_from(end_index)
                    .unwrap()
                    .saturating_mul(std::mem::size_of::<DrawMonoid>());
                let end = bytemuck::pod_read_unaligned::<DrawMonoid>(
                    &selected_draw[byte_offset..byte_offset + std::mem::size_of::<DrawMonoid>()],
                );
                let parent_index = end_index - 1;
                assert_eq!(end.path_ix, parent_index, "end clip {end_index} path fixup");
                assert_eq!(
                    end.scene_offset,
                    0x1111_0000 | parent_index,
                    "end clip {end_index} scene fixup"
                );
                assert_eq!(
                    end.info_offset,
                    0x2222_0000 | parent_index,
                    "end clip {end_index} info fixup"
                );
                assert_ne!(end.path_ix, end_index, "end clip {end_index} path changed");
                assert_ne!(
                    end.scene_offset,
                    0x1111_0000 | end_index,
                    "end clip {end_index} scene changed"
                );
                assert_ne!(
                    end.info_offset,
                    0x2222_0000 | end_index,
                    "end clip {end_index} info changed"
                );
                observed_end_fixup = true;
            }
            assert!(
                observed_end_fixup,
                "mixed fixture must contain at least one valid EndClip fixup"
            );
        }
    }

    fn clip_single_group_guard_fixture(
        device: &Device,
        queue: &Queue,
        n_clips: u32,
    ) -> (Vec<u8>, Vec<u8>) {
        use vello_encoding::{ClipElement, ConfigUniform, DrawMonoid, Layout, PathBbox};

        let mut renderer = clip_single_group_renderer(device);
        let mut config = ConfigUniform::default();
        config.target_width = 49;
        config.target_height = 17;
        config.layout = Layout {
            n_clips,
            n_draw_objects: n_clips,
            n_paths: n_clips,
            ..Layout::default()
        };
        let mut recording = Recording::default();
        let config = recording.upload_uniform("clip.guard.config", bytemuck::bytes_of(&config));
        let clip_inp = recording.upload("clip.guard.inp", [0; 8]);
        let paths = recording.upload("clip.guard.paths", bytemuck::bytes_of(&PathBbox::default()));
        let reduced = recording.upload("clip.guard.reduced", [0; 8]);
        let clip_els = recording.upload(
            "clip.guard.els",
            bytemuck::bytes_of(&ClipElement::default()),
        );
        let initial_draw = (0..n_clips)
            .map(|index| DrawMonoid {
                path_ix: index,
                clip_ix: index,
                scene_offset: 0x1111_0000 | index,
                info_offset: 0x2222_0000 | index,
            })
            .collect::<Vec<_>>();
        let initial_bbox = vec![0xa5_u8; usize::try_from(n_clips).unwrap().saturating_mul(16)];
        let draw = recording.upload("clip.guard.draw", bytemuck::cast_slice(&initial_draw));
        let bboxes = recording.upload("clip.guard.bboxes", initial_bbox.clone());
        recording.dispatch(
            renderer
                .shaders
                .clip_leaf_single_group
                .expect("fixture shader exists"),
            (n_clips.div_ceil(256), 1, 1),
            [config, clip_inp, paths, reduced, clip_els, draw, bboxes],
        );
        recording.download(draw);
        recording.download(bboxes);
        renderer
            .engine
            .run_recording(
                device,
                queue,
                &recording,
                &[],
                "clip_leaf_single_group_guard_fixture",
                #[cfg(feature = "wgpu-profiler")]
                &mut renderer.profiler,
            )
            .expect("guard fixture submits");
        let read = |engine: &mut WgpuEngine, proxy: recording::BufferProxy| {
            let mut bytes = readback_tile(
                device,
                &engine
                    .take_download(proxy)
                    .expect("guard fixture download exists"),
            );
            bytes.truncate(usize::try_from(proxy.size).unwrap());
            bytes
        };
        (
            read(&mut renderer.engine, draw),
            read(&mut renderer.engine, bboxes),
        )
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn clip_single_group_gpu_guard_leaves_oversized_outputs_at_sentinels() {
        let (device, queue) =
            headless_device().expect("clip single-group guard proof requires GPU adapter");
        let n_clips = 257;
        let (draw, bboxes) = clip_single_group_guard_fixture(&device, &queue, n_clips);
        let expected_draw = (0..n_clips)
            .flat_map(|index| [index, index, 0x1111_0000 | index, 0x2222_0000 | index])
            .flat_map(u32::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(draw, expected_draw, "oversized guarded draw monoids");
        assert_eq!(bboxes, vec![0xa5; usize::try_from(n_clips).unwrap() * 16]);
    }

    fn assert_clip_single_group_full_and_sparse_exact(
        device: &Device,
        queue: &Queue,
        scene: &Scene,
        params: &RenderParams,
    ) {
        let (ordinary_full_texture, ordinary_full) =
            damage_target_with_size(device, queue, params.width, params.height);
        let (selected_full_texture, selected_full) =
            damage_target_with_size(device, queue, params.width, params.height);
        let mut ordinary = Renderer::new(device, RendererOptions::default()).unwrap();
        ordinary.shaders.clip_leaf_single_group = None;
        let mut selected = clip_single_group_renderer(device);
        render_dense_with_recovery(&mut ordinary, device, queue, scene, &ordinary_full, params);
        assert!(!ordinary.last_clip_single_group_selected());
        render_dense_with_recovery(&mut selected, device, queue, scene, &selected_full, params);
        assert!(selected.last_clip_single_group_selected());
        let (ordinary_bytes, ordinary_stride) = readback_texture_rows(
            device,
            queue,
            &ordinary_full_texture,
            params.width,
            params.height,
        );
        let (selected_bytes, selected_stride) = readback_texture_rows(
            device,
            queue,
            &selected_full_texture,
            params.width,
            params.height,
        );
        assert!(
            texture_has_nonzero_and_varied_pixels(
                &ordinary_bytes,
                ordinary_stride,
                params.width,
                params.height,
            ),
            "clip single-group differential scene must produce nonempty varied pixels"
        );
        assert_texture_rows_match(
            &selected_bytes,
            selected_stride,
            &ordinary_bytes,
            ordinary_stride,
            params.width,
            params.height,
            "single-group full",
        );
        let (sparse_texture, sparse) =
            damage_target_with_size(device, queue, params.width, params.height);
        let damage_x = params.width.saturating_sub(34);
        let damage_y = params.height.saturating_sub(17);
        let damage = [FineDamageRect {
            x: damage_x,
            y: damage_y,
            width: params.width.saturating_sub(damage_x),
            height: params.height.saturating_sub(damage_y),
        }];
        let stats = selected
            .render_to_texture_with_damage(device, queue, scene, &sparse, params, &damage)
            .expect("clip single-group sparse fixture submits")
            .expect("nonempty clip damage must submit");
        assert!(stats.clip_single_group_shader_selected);
        assert!(!selected.settle_bump_budget(device));
        let (sparse_bytes, sparse_stride) =
            readback_texture_rows(device, queue, &sparse_texture, params.width, params.height);
        assert!(
            texture_has_nonzero_and_varied_pixels(
                &sparse_bytes,
                sparse_stride,
                params.width,
                params.height,
            ),
            "single-group sparse damage must contain visible varied clipped pixels"
        );
        for y in 0..usize::try_from(params.height).unwrap() {
            for x in 0..usize::try_from(params.width).unwrap() {
                let offset = x.saturating_mul(4);
                let sparse_pixel = &sparse_bytes[y * sparse_stride + offset..][..4];
                if x >= usize::try_from(damage_x).unwrap()
                    && y >= usize::try_from(damage_y).unwrap()
                {
                    assert_eq!(
                        sparse_pixel,
                        &ordinary_bytes[y * ordinary_stride + offset..][..4]
                    );
                } else {
                    assert_eq!(
                        sparse_pixel, &[0; 4],
                        "single-group sparse sentinel {x},{y}"
                    );
                }
            }
        }
    }

    fn assert_clip_single_group_route_boundary(
        device: &Device,
        queue: &Queue,
        params: &RenderParams,
        clip_count: u32,
        expected_selected: bool,
    ) {
        let (_texture, target) =
            damage_target_with_size(device, queue, params.width, params.height);
        let mut renderer = clip_single_group_renderer(device);
        render_dense_with_recovery(
            &mut renderer,
            device,
            queue,
            &clip_single_group_scene(clip_count, params.width, params.height),
            &target,
            params,
        );
        assert_eq!(
            renderer.last_clip_single_group_selected(),
            expected_selected
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the test intentionally leaves compact GPU route evidence in the test log"
    )]
    fn clip_single_group_fixture_selects_only_bounded_gpu_clip_routes() {
        let (device, queue) =
            headless_device().expect("clip single-group routing proof requires GPU adapter");
        const WIDTH: u32 = 49;
        const HEIGHT: u32 = 17;
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        let mut cpu = Renderer::new(
            &device,
            RendererOptions {
                use_cpu: true,
                ..RendererOptions::default()
            },
        )
        .unwrap();
        cpu.engine.enable_clip_single_group_fixture();
        cpu.shaders = shaders::full_shaders(&device, &mut cpu.engine, &cpu.options)
            .expect("CPU clip single-group shader set must compile");
        assert!(cpu.clip_single_group_requested());
        assert!(!cpu.clip_single_group_available());
        let scene = clip_single_group_scene(9, WIDTH, HEIGHT);
        assert_clip_single_group_full_and_sparse_exact(&device, &queue, &scene, &params);
        for clip_count in [1, 255, 256] {
            assert_clip_single_group_route_boundary(&device, &queue, &params, clip_count, true);
        }
        assert_clip_single_group_route_boundary(&device, &queue, &params, 0, false);
        assert_clip_single_group_route_boundary(&device, &queue, &params, 257, false);
        let (_texture, target) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = clip_single_group_renderer(&device);
        assert!(
            renderer
                .render_to_texture_with_damage(&device, &queue, &scene, &target, &params, &[])
                .expect("empty damage is a supported no-op")
                .is_none()
        );
        assert!(!renderer.last_clip_single_group_selected());
        eprintln!(
            "clip-single-group GPU route proof: extent={WIDTH}x{HEIGHT} clips=1,9,255,256 selected=true clips=0,257 selected=false sparse_sentinel=true settled_overflow=false"
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the test intentionally leaves compact GPU proof evidence in the test log"
    )]
    fn clip_single_group_gpu_clip_outputs_match_ordinary_for_bounded_depths() {
        let (device, queue) =
            headless_device().expect("clip single-group output proof requires GPU adapter");
        for clips in [1, 255, 256] {
            assert_clip_leaf_outputs_match_for_count(&device, &queue, clips, false);
        }
        // A one-record clip stream cannot contain a valid EndClip. These
        // larger mixed cases prove the EndClip fixups on shared input.
        for clips in [255, 256] {
            assert_clip_leaf_outputs_match_for_count(&device, &queue, clips, true);
        }
        eprintln!(
            "clip-single-group output proof: extent=49x17 all_push=1,255,256 mixed_push_end=255,256 route=ordinary+single-group logical_draw_monoids_and_clip_bboxes=exact"
        );
    }

    #[test]
    #[ignore = "native 4K single-workgroup clip leaf full/sparse GPU proof"]
    #[cfg(not(target_arch = "wasm32"))]
    #[allow(
        clippy::print_stderr,
        reason = "the ignored native proof intentionally leaves compact GPU evidence in the test log"
    )]
    fn clip_single_group_native_4k_preserves_full_and_sparse_clip_pixels() {
        const WIDTH: u32 = 3_840;
        const HEIGHT: u32 = 2_160;
        let (device, queue) =
            headless_device().expect("native clip single-group proof requires GPU adapter");
        let params = RenderParams {
            base_color: Color::TRANSPARENT,
            width: WIDTH,
            height: HEIGHT,
            antialiasing_method: AaConfig::Area,
        };
        assert_clip_single_group_full_and_sparse_exact(
            &device,
            &queue,
            &clip_single_group_scene(9, WIDTH, HEIGHT),
            &params,
        );
        eprintln!(
            "clip-single-group native proof: extent={WIDTH}x{HEIGHT} route=full+sparse compared_bytes={} overflow=false",
            u64::from(WIDTH)
                .saturating_mul(u64::from(HEIGHT))
                .saturating_mul(4),
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn sparse_fine_quad_skip_shared_coarse_preserves_deep_unaligned_damage() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        const WIDTH: u32 = 64;
        const HEIGHT: u32 = 32;
        let (full_texture, full) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let (sparse_texture, sparse) = damage_target_with_size(&device, &queue, WIDTH, HEIGHT);
        let mut renderer = Renderer::new(&device, RendererOptions::default()).unwrap();
        renderer.engine.enable_sparse_tile_bounds_fixture(false);
        renderer.shaders = shaders::full_shaders(&device, &mut renderer.engine, &renderer.options)
            .expect("quad-skip shader must compile");
        renderer
            .engine
            .build_shaders_if_needed(&device, renderer.options.num_init_threads);
        let quad = renderer
            .shaders
            .fine_sparse_area_quad_skip
            .expect("quad-skip variant must be available");
        // This is test-only substitution: production shared-coarse diagnostics
        // retain fine_sparse_area through the public wrapper.
        renderer.shaders.fine_sparse_area = Some(quad);

        let mut scene = Scene::new();
        for depth in 0..5 {
            let inset = f64::from(depth) + 0.25;
            scene.push_layer(
                Fill::EvenOdd,
                BlendMode::default(),
                0.5,
                Affine::IDENTITY,
                &Rect::new(
                    inset,
                    inset,
                    f64::from(WIDTH) - inset,
                    f64::from(HEIGHT) - inset,
                ),
            );
        }
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::WHITE,
            None,
            &Rect::new(-2.5, 1.25, 62.75, 30.5),
        );
        for _ in 0..5 {
            scene.pop_layer();
        }
        let damage = [
            FineDamageRect {
                x: 17,
                y: 4,
                width: 3,
                height: 2,
            },
            FineDamageRect {
                x: 39,
                y: 21,
                width: 2,
                height: 3,
            },
        ];
        let full_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.quad_skip_shared_full"),
            size: 256 * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let sparse_readback = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vello.quad_skip_shared_sparse"),
            size: 256 * u64::from(HEIGHT),
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let stats = renderer
            .render_to_textures_shared_coarse_area_with_post_render(
                &device,
                &queue,
                &scene,
                &full,
                &sparse,
                &RenderParams {
                    base_color: Color::TRANSPARENT,
                    width: WIDTH,
                    height: HEIGHT,
                    antialiasing_method: AaConfig::Area,
                },
                &damage,
                |encoder| {
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo {
                            texture: &full_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &full_readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(256),
                                rows_per_image: Some(HEIGHT),
                            },
                        },
                        wgpu::Extent3d {
                            width: WIDTH,
                            height: HEIGHT,
                            depth_or_array_layers: 1,
                        },
                    );
                    encoder.copy_texture_to_buffer(
                        wgpu::TexelCopyTextureInfo {
                            texture: &sparse_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d::ZERO,
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyBufferInfo {
                            buffer: &sparse_readback,
                            layout: wgpu::TexelCopyBufferLayout {
                                offset: 0,
                                bytes_per_row: Some(256),
                                rows_per_image: Some(HEIGHT),
                            },
                        },
                        wgpu::Extent3d {
                            width: WIDTH,
                            height: HEIGHT,
                            depth_or_array_layers: 1,
                        },
                    );
                },
            )
            .unwrap()
            .expect("shared-coarse fixture must submit");
        assert!(!stats.fine_quad_skip_applied);
        assert_eq!(stats.fine_quad_total_quads, 0);
        assert_eq!(stats.fine_quad_active_quads, 0);
        let (total_quads, active_quads, _) =
            sparse_fine_quad_counts(&renderer.sparse_records, WIDTH, HEIGHT)
                .expect("fixture damage records must be valid");
        assert!(active_quads > 0 && active_quads < total_quads);
        let full_pixels = readback_tile(&device, &full_readback);
        let sparse_pixels = readback_tile(&device, &sparse_readback);
        let mut nontransparent_requested = false;
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                let offset = usize::try_from(y * 256 + x * 4).unwrap();
                let requested = damage.iter().any(|rect| {
                    x >= rect.x
                        && x < rect.x + rect.width
                        && y >= rect.y
                        && y < rect.y + rect.height
                });
                if requested {
                    assert_eq!(
                        &sparse_pixels[offset..offset + 4],
                        &full_pixels[offset..offset + 4]
                    );
                    nontransparent_requested |= sparse_pixels[offset + 3] != 0;
                } else {
                    assert_eq!(&sparse_pixels[offset..offset + 4], &[0; 4]);
                }
            }
        }
        assert!(nontransparent_requested);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn zero_gradient_reuse_preserves_one_image_and_fine_cache_binding() {
        let Some((device, queue)) = headless_device() else {
            return;
        };
        let target = target(&device);
        let mut renderer = Renderer::new(
            &device,
            RendererOptions {
                num_init_threads: NonZeroUsize::new(1),
                ..Default::default()
            },
        )
        .unwrap();
        renderer.engine.set_zero_gradient_reuse_for_test(true);
        let solid = solid_scene();
        let params = render_params();

        assert!(renderer.zero_gradient_reuse_enabled());
        renderer
            .render_to_texture(&device, &queue, &solid, &target, &params)
            .unwrap();
        let first_images = renderer.memory_stats().image_bytes;
        assert_eq!(first_images, 8, "one-pixel atlas plus zero gradient");
        assert_eq!(renderer.engine.zero_gradient_test_stats().0, 1);
        let (fallback_id, fallback_bytes, image_count, atlas_bytes) =
            renderer.engine.zero_gradient_test_image_state();
        assert!(fallback_id.is_some());
        assert_eq!(fallback_bytes, 4);
        assert_eq!(image_count, 2);
        assert_eq!(first_images, atlas_bytes + fallback_bytes);

        renderer
            .render_to_texture(&device, &queue, &solid, &target, &params)
            .unwrap();
        let (allocations, cache_hits) = renderer.engine.zero_gradient_test_stats();
        assert_eq!(allocations, 1);
        assert!(cache_hits > 0, "second fine pass must reuse its binding");
        assert_eq!(renderer.memory_stats().image_bytes, first_images);

        renderer.trim_pooled_buffers();
        assert_eq!(renderer.memory_stats().image_bytes, first_images);
        renderer
            .render_to_texture(&device, &queue, &solid, &target, &params)
            .unwrap();
        assert_eq!(renderer.engine.zero_gradient_test_stats().0, 1);
        assert_eq!(renderer.memory_stats().image_bytes, first_images);

        let gradient = gradient_scene();
        renderer
            .render_to_texture(&device, &queue, &gradient, &target, &params)
            .unwrap();
        assert_eq!(renderer.engine.zero_gradient_test_stats().0, 1);
        let (gradient_fallback_id, gradient_fallback_bytes, gradient_image_count, gradient_atlas) =
            renderer.engine.zero_gradient_test_image_state();
        assert!(gradient_fallback_id == fallback_id);
        assert_eq!(gradient_fallback_bytes, fallback_bytes);
        assert_eq!(gradient_image_count, image_count);
        assert_eq!(
            renderer.memory_stats().image_bytes,
            gradient_atlas + gradient_fallback_bytes,
            "the uploaded gradient ramp must be released after fine"
        );

        let mut disabled = Renderer::new(
            &device,
            RendererOptions {
                num_init_threads: NonZeroUsize::new(1),
                ..Default::default()
            },
        )
        .unwrap();
        disabled.engine.set_zero_gradient_reuse_for_test(false);
        assert!(!disabled.zero_gradient_reuse_enabled());
        disabled
            .render_to_texture(&device, &queue, &solid, &target, &params)
            .unwrap();
        assert_eq!(disabled.engine.zero_gradient_test_stats(), (0, 0));
        let (disabled_fallback, disabled_bytes, disabled_image_count, disabled_atlas) =
            disabled.engine.zero_gradient_test_image_state();
        assert!(disabled_fallback.is_none());
        assert_eq!(disabled_bytes, 0);
        assert_eq!(disabled_image_count, 1);
        assert_eq!(disabled.memory_stats().image_bytes, disabled_atlas);
        assert_eq!(disabled_atlas, 4);
    }

    #[test]
    fn overflow_latch_survives_a_later_non_overflow_and_is_consumed_once() {
        let mut latch = OverflowLatch::default();
        // This models atlas batch N completing while batch N + 1 begins.
        latch.record(true);
        latch.record(false);

        assert!(latch.take());
        assert!(!latch.take());
    }

    #[test]
    fn sparse_tile_records_union_overlapping_native_damage() {
        let damage = [
            FineDamageRect {
                x: 0,
                y: 0,
                width: 2,
                height: 1,
            },
            FineDamageRect {
                x: 1,
                y: 0,
                width: 2,
                height: 1,
            },
        ];
        let mut masks = Vec::new();
        let mut records = Vec::new();
        let stats = sparse_tile_records(&mut masks, &mut records, 17, 17, &damage, 4).unwrap();
        assert_eq!(records.len(), 9);
        assert_eq!(records[0], 0);
        assert_eq!(records[1] & 0b111, 0b111);
        assert_eq!(stats.fine_tiles, 1);
        assert_eq!(stats.fine_pixels, 256);
        assert_eq!(stats.written_pixels, 3);
        assert_eq!(stats.coarse_pixels, 289);
    }

    #[test]
    fn sparse_tile_records_clamp_and_reject_empty_damage() {
        let mut masks = Vec::new();
        let mut records = Vec::new();
        assert!(sparse_tile_records(&mut masks, &mut records, 16, 16, &[], 1).is_none());
        let damage = [FineDamageRect {
            x: 15,
            y: 15,
            width: 9,
            height: 9,
        }];
        let stats = sparse_tile_records(&mut masks, &mut records, 16, 16, &damage, 1).unwrap();
        assert_eq!(stats.written_pixels, 1);
        assert_eq!(stats.fine_pixels, 256);
    }

    #[test]
    fn sparse_tile_row_masks_match_a_pixel_oracle_across_boundaries() {
        let width: u32 = 33;
        let height: u32 = 35;
        let mut seed = 0x1234_5678u32;
        let mut damage = vec![FineDamageRect {
            x: 15,
            y: 1,
            width: 3,
            height: 3,
        }];
        for _ in 0..32 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let x = seed % 40;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let y = seed % 40;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let rect_width = seed % 20;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            damage.push(FineDamageRect {
                x,
                y,
                width: rect_width,
                height: seed % 20,
            });
        }
        let mut masks = Vec::new();
        let mut records = Vec::new();
        sparse_tile_records(&mut masks, &mut records, width, height, &damage, 9).unwrap();

        let mut expected = vec![false; usize::try_from(width * height).unwrap()];
        for rect in &damage {
            for y in rect.y.min(height)..rect.y.saturating_add(rect.height).min(height) {
                for x in rect.x.min(width)..rect.x.saturating_add(rect.width).min(width) {
                    expected[usize::try_from(y * width + x).unwrap()] = true;
                }
            }
        }
        for y in 0..height {
            for x in 0..width {
                let tile = (y / 16) * width.div_ceil(16) + x / 16;
                let pixel = (y % 16) * 16 + x % 16;
                let actual = masks[usize::try_from(tile).unwrap()]
                    [usize::try_from(pixel / 32).unwrap()]
                    & (1 << (pixel % 32))
                    != 0;
                assert_eq!(actual, expected[usize::try_from(y * width + x).unwrap()]);
            }
        }
    }

    #[test]
    fn sparse_coarse_tile_bits_deduplicate_and_clear_reused_words() {
        let mut bits = Vec::new();
        let mut records = vec![0; 27];
        records[0] = 0;
        records[9] = 65_535;
        records[18] = 65_535;
        assert!(sparse_coarse_tile_bits(&mut bits, &records, 4_096, 4_096));
        assert_eq!(bits.len(), 2_048);
        assert_eq!(bits[0], 1);
        assert_eq!(bits[2_047], 1 << 31);

        assert!(sparse_coarse_tile_bits(&mut bits, &[0; 9], 4_096, 4_096));
        assert_eq!(bits[0], 1);
        assert_eq!(bits[2_047], 0);
    }

    #[test]
    fn sparse_coarse_tile_bits_reject_malformed_or_out_of_range_records() {
        let mut bits = Vec::new();
        assert!(!sparse_coarse_tile_bits(&mut bits, &[], 32, 32));
        assert!(!sparse_coarse_tile_bits(&mut bits, &[0; 8], 32, 32));
        let mut out_of_range = [0; 9];
        out_of_range[0] = 4;
        assert!(!sparse_coarse_tile_bits(&mut bits, &out_of_range, 32, 32));
        assert!(!sparse_coarse_tile_bits(&mut bits, &[0; 9], 0, 32));
        assert!(!sparse_coarse_tile_bits(&mut bits, &[0; 9], 32, 0));
        assert!(!sparse_coarse_tile_bits(&mut bits, &[0; 9], 8_192, 8_192));
    }

    #[test]
    fn sparse_path_tile_bounds_unions_disjoint_tiles_without_translating_native_ids() {
        let mut records = vec![0; 18];
        // 64 native tiles per row. These records remain their original
        // row-major native ids; only their enclosing path ROI is narrowed.
        records[0] = 64 + 2;
        records[9] = 4 * 64 + 5;
        assert_eq!(
            sparse_path_tile_bounds(&records, 1_024, 1_024),
            Some([2, 1, 6, 5])
        );
    }

    #[test]
    fn sparse_fine_quad_counts_follow_nonzero_nibbles_and_native_edges() {
        let mut records = [0; 9];
        // Unaligned damage in tile zero: lanes 0 and 8 are the only active quads.
        records[1] = 1 << 1;
        records[2] = 1 << 3;
        assert_eq!(sparse_fine_quad_counts(&records, 16, 16), Some((64, 2, 8)));

        let mut edge = [0; 9];
        edge[0] = 1;
        // Final partial 17x1 target: only the first quad exists on tile one.
        edge[1] = 0xf;
        assert_eq!(sparse_fine_quad_counts(&edge, 17, 1), Some((64, 1, 1)));
        assert_eq!(sparse_fine_quad_counts(&[0; 8], 16, 16), None);
    }

    #[test]
    fn sparse_fine_quad_fast_path_matches_pixel_quad_oracle() {
        fn oracle(records: &[u32], width: u32, height: u32) -> (u32, u64) {
            let tiles_x = width.div_ceil(16);
            let mut active = 0;
            let mut pixels = 0;
            for record in records.chunks_exact(9) {
                let tile_x = record[0] % tiles_x;
                let tile_y = record[0] / tiles_x;
                for pixel in (0..256u32).step_by(4) {
                    let mask = record[usize::try_from(pixel / 32 + 1).unwrap()];
                    if ((mask >> (pixel & 31)) & 0xf) == 0 {
                        continue;
                    }
                    let x = tile_x * 16 + pixel % 16;
                    let y = tile_y * 16 + pixel / 16;
                    if x < width && y < height {
                        active += 1;
                        pixels += u64::from((width - x).min(4));
                    }
                }
            }
            (active, pixels)
        }

        let mut state = 0x6d2b_79f5u32;
        for &(width, height, tile) in &[(16, 16, 0), (32, 32, 0)] {
            for _ in 0..16 {
                let mut record = [0; 9];
                record[0] = tile;
                for word in &mut record[1..] {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    *word = state;
                }
                let (active, pixels) = oracle(&record, width, height);
                assert_eq!(
                    sparse_fine_quad_counts(&record, width, height),
                    Some((64, active, pixels))
                );
            }
        }
    }

    #[test]
    fn sparse_path_tile_bounds_handles_partial_native_edges_and_rejects_bad_unions() {
        let mut edge = [0; 9];
        edge[0] = 240 * 135 - 1;
        assert_eq!(
            sparse_path_tile_bounds(&edge, 3_840, 2_160),
            Some([239, 134, 240, 135])
        );

        assert_eq!(sparse_path_tile_bounds(&[], 1_024, 1_024), None);
        assert_eq!(sparse_path_tile_bounds(&[0; 8], 1_024, 1_024), None);
        let mut offscreen = [0; 9];
        offscreen[0] = 4_096;
        assert_eq!(sparse_path_tile_bounds(&offscreen, 1_024, 1_024), None);

        // The bounding union covers the complete native target, so this
        // experiment deliberately declines it rather than adding a variant.
        let mut full = [0; 18];
        full[9] = 64 * 64 - 1;
        assert_eq!(sparse_path_tile_bounds(&full, 1_024, 1_024), None);
    }

    #[test]
    fn sparse_coarse_active_bins_deduplicate_repeated_row_major_bin_ids() {
        // A 32 by 32 tile target contains four native 16 by 16 tile bins.
        // Tile 0 and tile 32 are in separate rows but the same native bin.
        let mut records = vec![0; 45];
        for (record, tile) in records.chunks_exact_mut(9).zip([0, 32, 16, 512, 528]) {
            record[0] = tile;
        }
        let mut bits = Vec::new();
        assert_eq!(
            sparse_coarse_active_bin_list(&mut bits, &records, 512, 512),
            Some(SparseCoarseActiveBins {
                native_bins: 4,
                selected_bins: 4,
            })
        );
        let selector_words = 32;
        assert_eq!(&bits[selector_words..selector_words + 4], &[0, 1, 2, 3]);
    }

    #[test]
    fn sparse_coarse_active_bins_fall_back_when_selector_has_insufficient_list_space() {
        // This target stays within the existing 256-native-bin renderer
        // bound. Its 65,280 tiles use 2,040 selector words, leaving eight
        // list words; these nine selected bins must retain legacy coarse.
        let mut records = vec![0; 81];
        for (record, tile) in records.chunks_exact_mut(9).zip((0..9).map(|bin| bin * 16)) {
            record[0] = tile;
        }
        let mut bits = Vec::new();
        assert!(sparse_coarse_active_bin_list(&mut bits, &records, 4_080, 4_096).is_none());
        assert_eq!(bits.len(), 2_048);
        assert_eq!(bits[0], 0x0001_0001);
    }

    #[test]
    fn sparse_coarse_active_bins_preserve_the_final_partial_selector_word() {
        // 33 native tiles consume two selector words; tile 32 occupies the
        // first bit of that final partial word. The native-bin list must
        // begin at word two rather than overwrite it.
        let mut records = [0; 9];
        records[0] = 32;
        let mut bits = Vec::new();
        assert_eq!(
            sparse_coarse_active_bin_list(&mut bits, &records, 528, 16),
            Some(SparseCoarseActiveBins {
                native_bins: 3,
                selected_bins: 1,
            })
        );
        assert_eq!(&bits[..3], &[0, 1, 2]);
    }

    #[test]
    fn sparse_coarse_active_bins_reject_existing_over_256_native_bin_limit() {
        // The selector itself can represent this tall target, but upstream
        // binning has only 256 native-bin workgroup slots.
        let mut bits = Vec::new();
        assert!(sparse_coarse_active_bin_list(&mut bits, &[0; 9], 16, 65_792).is_none());
        assert_eq!(bits.len(), 2_048);
        assert_eq!(bits[0], 1);
    }

    #[test]
    fn active_sparse_coarse_timestamp_stage_name_is_stable() {
        assert_eq!(
            gpu_dispatch_timestamp_stage_name(28),
            Some("coarse_sparse_active")
        );
    }

    #[test]
    fn sparse_coarse_active_coverage_requires_ratio_and_absolute_savings() {
        assert!(sparse_coarse_active_coverage(64, 32));
        assert!(!sparse_coarse_active_coverage(64, 33));
        assert!(!sparse_coarse_active_coverage(32, 1));
        assert!(sparse_coarse_active_coverage(33, 1));
        assert!(!sparse_coarse_active_coverage(64, 0));
        assert!(!sparse_coarse_active_coverage(64, 65));
    }

    #[test]
    fn sparse_coarse_adaptive_selection_skips_unprofitable_or_unpacked_lists() {
        let mut sentinel = vec![0xdead_beef; 3];
        assert_eq!(
            sparse_coarse_adaptive_selection(&mut sentinel, &[0; 9], 256, 256),
            SparseCoarseAdaptiveSelection::default()
        );
        assert_eq!(sentinel, vec![0xdead_beef; 3]);

        let mut bits = Vec::new();
        // 33 native bins with two selected saves 31 bins, below the floor.
        let mut records = vec![0; 18];
        records[9] = 16 * 32;
        let selection = sparse_coarse_adaptive_selection(&mut bits, &records, 8_448, 256);
        assert_eq!(selection.bins.unwrap().native_bins, 33);
        assert!(!selection.active);

        let mut bits = Vec::new();
        let selection = sparse_coarse_adaptive_selection(&mut bits, &[0; 9], 8_448, 256);
        assert!(selection.active);

        let mut bits = Vec::new();
        let mut records = vec![0; 81];
        for (record, tile) in records.chunks_exact_mut(9).zip((0..9).map(|bin| bin * 16)) {
            record[0] = tile;
        }
        assert_eq!(
            sparse_coarse_adaptive_selection(&mut bits, &records, 4_080, 4_096),
            SparseCoarseAdaptiveSelection::default()
        );
    }
}
