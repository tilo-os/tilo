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
use std::{num::NonZeroUsize, sync::atomic::AtomicBool};
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
    })
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

        Ok(Self {
            options,
            engine,
            resolver: Resolver::new(),
            image_atlas: None,
            recording: Recording::default(),
            packed_scene: Vec::new(),
            shaders,
            budget: bump_budget::BudgetTracker::new(),
            unsettled_bump_overflow: OverflowLatch::default(),
            sparse_masks: Vec::new(),
            sparse_records: Vec::new(),
            sparse_coarse_tiles: Vec::new(),
            defer_bump_readback: std::env::var_os("VELLO_DEFER_BUMP_READBACK")
                .is_some_and(|value| value == "1"),
            sparse_upload_reuse: default_enabled_from_env("VELLO_SPARSE_UPLOAD_REUSE"),
            sparse_upload: None,
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

    /// Whether default sparse Area-AA fine shaders are available for
    /// [`Self::render_to_texture_with_damage`].
    #[doc(hidden)]
    pub fn sparse_fine_supported(&self) -> bool {
        self.engine.sparse_fine_enabled()
            && !self.options.use_cpu
            && self.shaders.fine_sparse_area.is_some()
    }

    #[doc(hidden)]
    pub fn sparse_coarse_supported(&self) -> bool {
        self.sparse_fine_supported() && self.shaders.coarse_sparse.is_some()
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
        let defer_bump_readback = self.defer_bump_readback_enabled();
        let (target, bump_buf) = render::render_full(
            scene,
            &mut self.resolver,
            &self.shaders,
            &mut self.image_atlas,
            &mut self.recording,
            &mut self.packed_scene,
            params,
            self.budget.budget(),
            measure,
            defer_bump_readback,
        );
        let external_resources = [ExternalResource::Image(
            *target.as_image().unwrap(),
            texture,
        )];
        self.engine.run_recording_with_post_render(
            device,
            queue,
            &self.recording,
            &external_resources,
            "render_to_texture",
            post_render,
            #[cfg(feature = "wgpu-profiler")]
            &mut self.profiler,
        )?;
        if let Some(bump_buf) = bump_buf {
            self.budget.begin_readback(&self.engine, bump_buf);
        }
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
        let external_damage_tiles = self.prepare_sparse_upload(device, queue);
        let coarse_tiles = self
            .sparse_coarse_supported()
            .then(|| {
                sparse_coarse_tile_bits(
                    &mut self.sparse_coarse_tiles,
                    &self.sparse_records,
                    params.width,
                    params.height,
                )
            })
            .filter(|valid| *valid)
            .map(|_| self.sparse_coarse_tiles.as_slice());
        let (target, bump_buf) = render::render_encoding_sparse_fine(
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
            defer_bump_readback,
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
            self.engine.run_recording(
                device,
                queue,
                &self.recording,
                &external_resources,
                "render_to_texture_sparse",
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
        } else {
            let external_resources = [ExternalResource::Image(
                *target.as_image().unwrap(),
                texture,
            )];
            self.engine.run_recording(
                device,
                queue,
                &self.recording,
                &external_resources,
                "render_to_texture_sparse",
                #[cfg(feature = "wgpu-profiler")]
                &mut self.profiler,
            )?;
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
        Ok(Some(stats))
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
    use super::{FineDamageRect, OverflowLatch, sparse_coarse_tile_bits, sparse_tile_records};

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
}
