// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Take an encoded scene and create a graph to render it

use crate::recording::{BufferProxy, ImageFormat, ImageProxy, Recording, ResourceProxy};
use crate::shaders::FullShaders;
use crate::{AaConfig, RenderParams};

#[cfg(feature = "wgpu")]
use crate::Scene;

use vello_encoding::{Encoding, Resolver, WorkgroupSize, make_mask_lut, make_mask_lut_16};

/// Sparse-coarse selector upload and the dispatch shape it represents.
///
/// Active lists retain native bin ids in the selector's unused trailing words;
/// the shader reconstructs their native coordinates before touching tile data.
#[derive(Clone, Copy)]
pub(crate) enum SparseCoarseDispatch<'a> {
    Legacy(&'a [u32]),
    Active {
        selector: &'a [u32],
        workgroups: u32,
        path_tile_bounds: Option<[u32; 4]>,
    },
}

impl<'a> SparseCoarseDispatch<'a> {
    fn selector(self) -> &'a [u32] {
        match self {
            Self::Legacy(selector) | Self::Active { selector, .. } => selector,
        }
    }
}

/// Whether this exact coarse dispatch can use its matching opaque-prune
/// permutation. Callers must keep the ordinary shader for the same mode when
/// this is false; falling through to a different sparse mode would change its
/// selector interpretation and, for active dispatches, its grid.
pub(crate) fn sparse_opaque_prune_shader_selected(
    shaders: &FullShaders,
    dispatch: Option<SparseCoarseDispatch<'_>>,
    requested: bool,
) -> bool {
    requested
        && match dispatch {
            Some(SparseCoarseDispatch::Active { .. }) => {
                shaders.coarse_sparse_active_opaque_prune.is_some()
            }
            Some(SparseCoarseDispatch::Legacy(_)) => shaders.coarse_sparse_opaque_prune.is_some(),
            None => shaders.coarse_opaque_prune.is_some(),
        }
}

/// Whether the sparse coarse and fine shaders can jointly consume the private
/// SOLID-fused PTCL stream. This is deliberately a pair check: an old fine
/// shader must never receive a stream carrying the private high tag bit.
pub(crate) fn sparse_solid_fuse_shader_selected(
    shaders: &FullShaders,
    dispatch: Option<SparseCoarseDispatch<'_>>,
    requested: bool,
    quad_skip_requested: bool,
    opaque_prune_requested: bool,
) -> bool {
    requested
        && !quad_skip_requested
        && !opaque_prune_requested
        && shaders.fine_sparse_area_solid_fuse.is_some()
        && match dispatch {
            Some(SparseCoarseDispatch::Active { .. }) => {
                shaders.coarse_sparse_active_solid_fuse.is_some()
            }
            Some(SparseCoarseDispatch::Legacy(_)) => shaders.coarse_sparse_solid_fuse.is_some(),
            None => shaders.coarse_solid_fuse.is_some(),
        }
}

/// Select the paired writer/reader row-mask permutations only when sparse
/// active-bin rendering has also actually narrowed path allocation. The
/// segment padding is otherwise uninitialized and must remain unread.
pub(crate) fn sparse_segment_row_mask_shader_selected(
    shaders: &FullShaders,
    dispatch: Option<SparseCoarseDispatch<'_>>,
    requested: bool,
    quad_skip_requested: bool,
    opaque_prune_requested: bool,
    solid_fuse_requested: bool,
    width: u32,
    height: u32,
) -> bool {
    requested
        && !quad_skip_requested
        && !opaque_prune_requested
        && !solid_fuse_requested
        && shaders.coarse_sparse_active.is_some()
        && shaders.tile_alloc_sparse.is_some()
        && shaders.fine_sparse_area_segment_row_mask.is_some()
        && match dispatch {
            Some(SparseCoarseDispatch::Active {
                path_tile_bounds: Some(bounds),
                ..
            }) if path_tile_bounds_are_valid(bounds, width, height) => {
                if shaders.direct_path_dispatch_enabled {
                    shaders.path_tiling_direct_segment_row_mask.is_some()
                } else {
                    shaders.path_tiling_segment_row_mask.is_some()
                }
            }
            _ => false,
        }
}

/// Select the specialised leaf only for the exact one-workgroup clip shape.
/// The shader repeats this guard so a malformed indirect recording remains a
/// no-op rather than treating an incomplete cross-workgroup prefix as valid.
fn clip_single_group_shader_selected(
    shaders: &FullShaders,
    n_clips: u32,
    clip_leaf_workgroups: WorkgroupSize,
) -> bool {
    matches!(n_clips, 1..=256)
        && clip_leaf_workgroups == (1, 1, 1)
        && shaders.clip_leaf_single_group.is_some()
}

/// Check an integer native-tile ROI before it reaches the tile allocator.
///
/// The allocation permutation must never be selected for a full or malformed
/// region, because the normal path then remains the safer and cheaper choice.
pub(crate) fn path_tile_bounds_are_valid(bounds: [u32; 4], width: u32, height: u32) -> bool {
    let tiles_x = width.div_ceil(16);
    let tiles_y = height.div_ceil(16);
    let Some(native_tiles) = tiles_x.checked_mul(tiles_y) else {
        return false;
    };
    if native_tiles == 0 || native_tiles > 65_536 {
        return false;
    }
    let [x0, y0, x1, y1] = bounds;
    if x0 >= x1 || y0 >= y1 || x1 > tiles_x || y1 > tiles_y {
        return false;
    }
    x1.checked_sub(x0)
        .and_then(|width| {
            y1.checked_sub(y0)
                .and_then(|height| width.checked_mul(height))
        })
        .is_some_and(|area| area.saturating_mul(2) <= native_tiles)
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "the guarded final tile-local coordinates are in the closed 0..=16 range"
)]
fn segment_row_mask_for_final_segment(point0: [f32; 2], point1: [f32; 2], y_edge: f32) -> u16 {
    const TILE: f32 = 16.0;
    const SENTINEL: f32 = 1.0e9;
    let in_range = |value: f32| (0.0..=TILE).contains(&value);
    if !in_range(point0[0])
        || !in_range(point0[1])
        || !in_range(point1[0])
        || !in_range(point1[1])
        || (y_edge != SENTINEL && !in_range(y_edge))
    {
        return u16::MAX;
    }
    let interval = |first: i32, last: i32| {
        let first = first.max(0);
        let last = last.min(15);
        if first > last {
            0_u16
        } else {
            let high = (1_u32 << u32::try_from(last.saturating_add(1)).unwrap_or(16)) - 1;
            let low = (1_u32 << u32::try_from(first).unwrap_or(0)) - 1;
            u16::try_from(high & !low).unwrap_or(u16::MAX)
        }
    };
    let dy_mask = interval(
        point0[1].min(point1[1]).floor() as i32 - 1,
        point0[1].max(point1[1]).ceil() as i32,
    );
    if point1[0] - point0[0] == 0.0 || y_edge == SENTINEL {
        return dy_mask;
    }
    dy_mask | interval(y_edge.floor() as i32 - 1, 15)
}

#[cfg(test)]
pub(crate) fn fine_row_has_original_area_contribution(
    point0: [f32; 2],
    point1: [f32; 2],
    y_edge: f32,
    row: u32,
) -> bool {
    // Native rows are bounded to 0..16 by the caller. Keep this conversion
    // explicit so the independent test oracle cannot accidentally depend on
    // host integer-to-float precision for an unbounded input.
    let row = f32::from(u16::try_from(row).unwrap_or(u16::MAX));
    // Keep the original fine operation boundaries. In particular, do not
    // rewrite y + delta_y as point1.y - row: rounding is observable here.
    let y = point0[1] - row;
    let delta_y = point1[1] - point0[1];
    let y0 = y.clamp(0.0, 1.0);
    let y1 = (y + delta_y).clamp(0.0, 1.0);
    let dy = y0 - y1;
    if dy != 0.0 {
        return true;
    }
    let delta_x = point1[0] - point0[0];
    let sign = if delta_x > 0.0 {
        1.0
    } else if delta_x < 0.0 {
        -1.0
    } else {
        0.0
    };
    sign * (row - y_edge + 1.0).clamp(0.0, 1.0) != 0.0
}

/// Mirror the opaque-prune shader guard for focused, CPU-only predicate tests.
/// `backdrop_clear` must already use the draw's nonzero/even-odd fill rule.
#[cfg(test)]
pub(crate) fn sparse_opaque_prune_eligible(
    clip_zero_depth: u32,
    clip_depth: u32,
    render_blend_depth: u32,
    segment_count_or_ix: u32,
    backdrop_clear: bool,
    rgba_color: u32,
) -> bool {
    clip_zero_depth == 0
        && clip_depth == 0
        && render_blend_depth == 0
        && segment_count_or_ix == 0
        && !backdrop_clear
        && (rgba_color >> 24) == 255
}

#[derive(Clone, Copy, Debug)]
enum AtlasProxyAction {
    Created,
    Reused,
    Resized,
}

/// State for a render in progress.
pub struct Render {
    fine_wg_count: Option<WorkgroupSize>,
    canonical_segment_order: bool,
    canonical_solid_fuse: bool,
    area_ppt8: bool,
    clip_single_group_allowed: bool,
    clip_single_group: bool,
    fine_resources: Option<FineResources>,
    mask_buf: Option<ResourceProxy>,

    #[cfg(feature = "debug_layers")]
    captured_buffers: Option<CapturedBuffers>,
}

#[cfg(feature = "debug_layers")]
impl Drop for Render {
    fn drop(&mut self) {
        if self.captured_buffers.is_some() {
            unreachable!("Render captured buffers without freeing them");
        }
    }
}

/// Resources produced by pipeline, needed for fine rasterization.
struct FineResources {
    aa_config: AaConfig,

    config_buf: ResourceProxy,
    bump_buf: ResourceProxy,
    tile_buf: ResourceProxy,
    segments_buf: ResourceProxy,
    ptcl_buf: ResourceProxy,
    gradient_image: ResourceProxy,
    gradient_image_is_transient: bool,
    info_bin_data_buf: ResourceProxy,
    image_atlas: ResourceProxy,
    blend_spill_buf: ResourceProxy,

    out_image: ImageProxy,
}

fn no_ramp_gradient_image(zero_gradient: Option<ImageProxy>) -> (ResourceProxy, bool) {
    let is_transient = zero_gradient.is_none();
    (
        zero_gradient
            .unwrap_or_else(|| ImageProxy::new(1, 1, ImageFormat::Rgba8))
            .into(),
        is_transient,
    )
}

fn free_transient_gradient(
    recording: &mut Recording,
    gradient_image: ResourceProxy,
    gradient_image_is_transient: bool,
) {
    if gradient_image_is_transient {
        recording.free_resource(gradient_image);
    }
}

/// A collection of internal buffers that are used for debug visualization when the
/// `debug_layers` feature is enabled. The contents of these buffers remain GPU resident
/// and must be freed directly by the caller.
///
/// Some of these buffers are also scheduled for a download to allow their contents to be
/// processed for CPU-side validation. These buffers are documented as such.
#[cfg(feature = "debug_layers")]
pub struct CapturedBuffers {
    pub sizes: vello_encoding::BufferSizes,

    /// Buffers that remain GPU-only
    pub path_bboxes: BufferProxy,

    /// Buffers scheduled for download
    pub lines: BufferProxy,
}

#[cfg(feature = "debug_layers")]
impl CapturedBuffers {
    pub fn release_buffers(self, recording: &mut Recording) {
        recording.free_buffer(self.path_bboxes);
        recording.free_buffer(self.lines);
    }
}

#[cfg(feature = "wgpu")]
pub(crate) fn render_full(
    scene: &Scene,
    resolver: &mut Resolver,
    shaders: &FullShaders,
    image_atlas: &mut Option<ImageProxy>,
    recording: &mut Recording,
    packed_scene: &mut Vec<u8>,
    params: &RenderParams,
    budget: &vello_encoding::BumpBudget,
    measure: bool,
    canonical_segment_order: bool,
    area_ppt8: bool,
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>, bool, bool, bool) {
    render_encoding_full(
        scene.encoding(),
        resolver,
        shaders,
        image_atlas,
        recording,
        packed_scene,
        params,
        budget,
        measure,
        canonical_segment_order,
        area_ppt8,
        defer_bump_readback,
        zero_gradient,
    )
}

#[cfg(feature = "wgpu")]
/// Create a single recording with both coarse and fine render stages.
///
/// `budget` is how much bump-allocated scratch to reserve. When
/// `measure` is set the recording also downloads the bump counters, so
/// the caller can size the next frame's budget from what this one used —
/// see [`crate::bump_budget`]. The returned [`BufferProxy`] is that
/// download, and the caller owns freeing it.
pub(crate) fn render_encoding_full(
    encoding: &Encoding,
    resolver: &mut Resolver,
    shaders: &FullShaders,
    image_atlas: &mut Option<ImageProxy>,
    recording: &mut Recording,
    packed_scene: &mut Vec<u8>,
    params: &RenderParams,
    budget: &vello_encoding::BumpBudget,
    measure: bool,
    canonical_segment_order: bool,
    area_ppt8: bool,
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>, bool, bool, bool) {
    let mut render = Render::new();
    render.render_encoding_coarse_with_dispatch(
        encoding,
        resolver,
        shaders,
        image_atlas,
        recording,
        packed_scene,
        params,
        budget,
        measure,
        false,
        None,
        defer_bump_readback,
        zero_gradient,
    );
    let out_image = render.out_image();
    let bump = render.bump_buf();
    let bump_buf = measure.then_some(bump);
    let canonical_segment_order =
        canonical_segment_order && shaders.canonical_segment_order_full.is_some();
    if canonical_segment_order {
        render.record_canonical_segment_order_full(shaders, recording);
    }
    render.area_ppt8 = area_ppt8
        && params.antialiasing_method == AaConfig::Area
        && !canonical_segment_order
        && shaders.fine_area_ppt8.is_some();
    render.record_fine(shaders, recording);
    if defer_bump_readback {
        finish_bump_readback(recording, bump, measure);
    }
    (
        out_image.into(),
        bump_buf,
        canonical_segment_order,
        render.area_ppt8,
        render.clip_single_group,
    )
}

#[cfg(feature = "wgpu")]
pub(crate) fn render_encoding_sparse_fine(
    encoding: &Encoding,
    resolver: &mut Resolver,
    shaders: &FullShaders,
    image_atlas: &mut Option<ImageProxy>,
    recording: &mut Recording,
    packed_scene: &mut Vec<u8>,
    params: &RenderParams,
    budget: &vello_encoding::BumpBudget,
    measure: bool,
    damage_tiles: &[u32],
    external_damage_tiles: Option<BufferProxy>,
    coarse_tiles: Option<SparseCoarseDispatch<'_>>,
    coarse_opaque_prune_requested: bool,
    fine_quad_skip: bool,
    solid_fuse_requested: bool,
    quad_skip_requested: bool,
    segment_row_mask: bool,
    canonical_segment_order: bool,
    area_ppt8: bool,
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>, bool, bool, bool) {
    let mut render = Render::new();
    render.render_encoding_coarse_with_dispatch_and_opaque_prune(
        encoding,
        resolver,
        shaders,
        image_atlas,
        recording,
        packed_scene,
        params,
        budget,
        measure,
        false,
        coarse_tiles,
        coarse_opaque_prune_requested,
        solid_fuse_requested,
        quad_skip_requested,
        segment_row_mask,
        defer_bump_readback,
        zero_gradient,
    );
    let out_image = render.out_image();
    let bump = render.bump_buf();
    let bump_buf = measure.then_some(bump);
    let solid_fuse = sparse_solid_fuse_shader_selected(
        shaders,
        coarse_tiles,
        solid_fuse_requested,
        quad_skip_requested,
        coarse_opaque_prune_requested,
    );
    let canonical_segment_order = canonical_segment_order
        && if solid_fuse {
            shaders.canonical_segment_order_sparse_solid_fuse.is_some()
        } else {
            shaders.canonical_segment_order_sparse.is_some()
        };
    if canonical_segment_order {
        render.canonical_segment_order = true;
        render.canonical_solid_fuse = solid_fuse;
    }
    render.area_ppt8 = area_ppt8
        && params.antialiasing_method == AaConfig::Area
        && !canonical_segment_order
        && !fine_quad_skip
        && !solid_fuse
        && !segment_row_mask
        && shaders.fine_sparse_area_ppt8.is_some();
    render.record_fine_sparse_with_quad_skip(
        shaders,
        recording,
        damage_tiles,
        external_damage_tiles,
        fine_quad_skip,
        solid_fuse,
        segment_row_mask,
    );
    if defer_bump_readback {
        finish_bump_readback(recording, bump, measure);
    }
    (
        out_image.into(),
        bump_buf,
        canonical_segment_order,
        render.area_ppt8,
        render.clip_single_group,
    )
}

/// Test-only paired capture for private PTCL encodings. Unlike the shared
/// coarse diagnostic, this never dispatches an ordinary fine shader against a
/// SOLID-fused command stream.
#[cfg(test)]
pub(crate) fn render_encoding_sparse_fine_with_ptcl_capture(
    encoding: &Encoding,
    resolver: &mut Resolver,
    shaders: &FullShaders,
    image_atlas: &mut Option<ImageProxy>,
    recording: &mut Recording,
    packed_scene: &mut Vec<u8>,
    params: &RenderParams,
    budget: &vello_encoding::BumpBudget,
    damage_tiles: &[u32],
    coarse_tiles: Option<SparseCoarseDispatch<'_>>,
    solid_fuse_requested: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, BufferProxy) {
    let mut render = Render::new();
    render.render_encoding_coarse_with_dispatch_and_opaque_prune(
        encoding,
        resolver,
        shaders,
        image_atlas,
        recording,
        packed_scene,
        params,
        budget,
        false,
        false,
        coarse_tiles,
        false,
        solid_fuse_requested,
        false,
        false,
        false,
        zero_gradient,
    );
    render.record_fine_sparse_with_ptcl_capture(
        shaders,
        recording,
        damage_tiles,
        sparse_solid_fuse_shader_selected(
            shaders,
            coarse_tiles,
            solid_fuse_requested,
            false,
            false,
        ),
    )
}

/// Finish the bump lifetime at the caller-selected recording point.
///
/// The fine shaders do not bind the bump allocator. Deferring the copy is an
/// opt-in command-order experiment: it preserves the same counters while
/// allowing a shared compute pass to include fine before the copy boundary.
#[cfg(feature = "wgpu")]
pub(crate) fn finish_bump_readback(
    recording: &mut Recording,
    bump_buf: BufferProxy,
    measure: bool,
) {
    if measure {
        recording.download(bump_buf);
    }
    recording.free_buffer(bump_buf);
}

impl Default for Render {
    fn default() -> Self {
        Self::new()
    }
}

impl Render {
    pub fn new() -> Self {
        Self {
            fine_wg_count: None,
            canonical_segment_order: false,
            canonical_solid_fuse: false,
            area_ppt8: false,
            clip_single_group_allowed: true,
            clip_single_group: false,
            fine_resources: None,
            mask_buf: None,
            #[cfg(feature = "debug_layers")]
            captured_buffers: None,
        }
    }

    /// Keep hidden differential and replay routes on the ordinary clip shader.
    /// Their paired outputs are not normal submissions and must not report or
    /// exercise this opt-in coarse permutation.
    pub(crate) fn set_clip_single_group_allowed(&mut self, allowed: bool) {
        self.clip_single_group_allowed = allowed;
    }

    /// Prepare a recording for the coarse rasterization phase.
    ///
    /// `budget` is how much bump-allocated scratch to reserve; see
    /// [`vello_encoding::BumpBudget`].
    ///
    /// `measure_bump` downloads the atomic bump counters used for dynamic
    /// memory sizing. `capture_debug` additionally retains the debug-layer
    /// buffers for the caller; it must later take and release those buffers.
    pub fn render_encoding_coarse(
        &mut self,
        encoding: &Encoding,
        resolver: &mut Resolver,
        shaders: &FullShaders,
        persistent_image_atlas: &mut Option<ImageProxy>,
        recording: &mut Recording,
        packed: &mut Vec<u8>,
        params: &RenderParams,
        budget: &vello_encoding::BumpBudget,
        measure_bump: bool,
        capture_debug: bool,
        coarse_tiles: Option<&[u32]>,
        defer_bump_readback: bool,
        zero_gradient: Option<ImageProxy>,
    ) {
        self.render_encoding_coarse_with_dispatch(
            encoding,
            resolver,
            shaders,
            persistent_image_atlas,
            recording,
            packed,
            params,
            budget,
            measure_bump,
            capture_debug,
            coarse_tiles.map(SparseCoarseDispatch::Legacy),
            defer_bump_readback,
            zero_gradient,
        );
    }

    pub(crate) fn render_encoding_coarse_with_dispatch(
        &mut self,
        encoding: &Encoding,
        resolver: &mut Resolver,
        shaders: &FullShaders,
        persistent_image_atlas: &mut Option<ImageProxy>,
        recording: &mut Recording,
        packed: &mut Vec<u8>,
        params: &RenderParams,
        budget: &vello_encoding::BumpBudget,
        measure_bump: bool,
        capture_debug: bool,
        coarse_tiles: Option<SparseCoarseDispatch<'_>>,
        defer_bump_readback: bool,
        zero_gradient: Option<ImageProxy>,
    ) {
        self.render_encoding_coarse_with_dispatch_and_opaque_prune(
            encoding,
            resolver,
            shaders,
            persistent_image_atlas,
            recording,
            packed,
            params,
            budget,
            measure_bump,
            capture_debug,
            coarse_tiles,
            false,
            false,
            false,
            false,
            defer_bump_readback,
            zero_gradient,
        );
    }

    /// Test-only coarse entry point for the paired row-mask proof. Production
    /// callers cannot select the writer without the normal sparse eligibility
    /// calculation in `Renderer`.
    #[cfg(test)]
    #[allow(
        clippy::too_many_arguments,
        reason = "keeps the test-only proof wrapper aligned with the coarse resource boundary"
    )]
    pub(crate) fn render_encoding_coarse_for_row_mask_capture(
        &mut self,
        encoding: &Encoding,
        resolver: &mut Resolver,
        shaders: &FullShaders,
        persistent_image_atlas: &mut Option<ImageProxy>,
        recording: &mut Recording,
        packed: &mut Vec<u8>,
        params: &RenderParams,
        budget: &vello_encoding::BumpBudget,
        coarse_tiles: Option<SparseCoarseDispatch<'_>>,
        zero_gradient: Option<ImageProxy>,
    ) {
        self.render_encoding_coarse_with_dispatch_and_opaque_prune(
            encoding,
            resolver,
            shaders,
            persistent_image_atlas,
            recording,
            packed,
            params,
            budget,
            false,
            false,
            coarse_tiles,
            false,
            false,
            false,
            true,
            false,
            zero_gradient,
        );
    }

    /// Private sparse-fine-only entry point for the opaque command-list
    /// experiment. All public/full/shared-coarse callers retain the ordinary
    /// coarse shaders through the wrapper above.
    fn render_encoding_coarse_with_dispatch_and_opaque_prune(
        &mut self,
        encoding: &Encoding,
        resolver: &mut Resolver,
        shaders: &FullShaders,
        persistent_image_atlas: &mut Option<ImageProxy>,
        recording: &mut Recording,
        packed: &mut Vec<u8>,
        params: &RenderParams,
        budget: &vello_encoding::BumpBudget,
        measure_bump: bool,
        capture_debug: bool,
        coarse_tiles: Option<SparseCoarseDispatch<'_>>,
        coarse_opaque_prune_requested: bool,
        solid_fuse_requested: bool,
        quad_skip_requested: bool,
        segment_row_mask: bool,
        defer_bump_readback: bool,
        zero_gradient: Option<ImageProxy>,
    ) {
        use vello_encoding::RenderConfig;
        let coarse_opaque_prune = sparse_opaque_prune_shader_selected(
            shaders,
            coarse_tiles,
            coarse_opaque_prune_requested,
        );
        let solid_fuse = sparse_solid_fuse_shader_selected(
            shaders,
            coarse_tiles,
            solid_fuse_requested,
            quad_skip_requested,
            coarse_opaque_prune_requested,
        );
        recording.reset_reusing_scene_upload(packed);
        packed.clear();
        // Bounds are only safe when the active coarse shader will actually
        // run. In particular, never narrow tile allocation and then recover
        // through legacy or dense coarse with altered path geometry.
        let sparse_tile_bounds = match (
            coarse_tiles,
            shaders.coarse_sparse_active,
            shaders.tile_alloc_sparse,
        ) {
            (
                Some(SparseCoarseDispatch::Active {
                    path_tile_bounds: Some(bounds),
                    ..
                }),
                Some(_),
                Some(shader),
            ) if path_tile_bounds_are_valid(bounds, params.width, params.height) => Some((
                shader,
                recording.upload_uniform("vello.sparse_tile_bounds", bytemuck::bytes_of(&bounds)),
            )),
            _ => None,
        };
        let coarse_tile_bits = coarse_tiles.map(|dispatch| {
            recording.upload_uniform(
                "vello.coarse_sparse_tiles",
                bytemuck::cast_slice(dispatch.selector()).to_vec(),
            )
        });

        let (layout, ramps, images) = resolver.resolve(encoding, packed);
        let (gradient_image, gradient_image_is_transient) = if ramps.height == 0 {
            no_ramp_gradient_image(zero_gradient)
        } else {
            let data: &[u8] = bytemuck::cast_slice(ramps.data);
            (
                ResourceProxy::Image(recording.upload_image(
                    ramps.width,
                    ramps.height,
                    ImageFormat::Rgba8,
                    data,
                )),
                true,
            )
        };
        let atlas_width = images.width.max(1);
        let atlas_height = images.height.max(1);
        let (image_atlas, atlas_proxy_action) = match persistent_image_atlas {
            Some(proxy) if proxy.width == atlas_width && proxy.height == atlas_height => {
                (*proxy, AtlasProxyAction::Reused)
            }
            Some(proxy) => {
                recording.free_image(*proxy);
                let new_proxy = ImageProxy::new(atlas_width, atlas_height, ImageFormat::Rgba8);
                *persistent_image_atlas = Some(new_proxy);
                (new_proxy, AtlasProxyAction::Resized)
            }
            None => {
                let proxy = ImageProxy::new(atlas_width, atlas_height, ImageFormat::Rgba8);
                *persistent_image_atlas = Some(proxy);
                (proxy, AtlasProxyAction::Created)
            }
        };
        if !matches!(atlas_proxy_action, AtlasProxyAction::Reused)
            || !images.images.is_empty()
            || !images.regions.is_empty()
            || images.evicted != 0
        {
            if images.evicted == 0 {
                log::debug!(
                    "image atlas {}x{}: {:?}, {} upload(s)",
                    atlas_width,
                    atlas_height,
                    atlas_proxy_action,
                    images.images.len()
                );
            } else {
                log::debug!(
                    "image atlas {}x{}: {:?}, {} upload(s), {} eviction(s)",
                    atlas_width,
                    atlas_height,
                    atlas_proxy_action,
                    images.images.len(),
                    images.evicted
                );
            }
        }
        // Warn on the first resolve pass that omits any images.
        if images.omitted != 0 && images.omitted_total == images.omitted as u64 {
            log::warn!(
                "image atlas {}x{} omitted {} image(s); further omissions will not be logged",
                atlas_width,
                atlas_height,
                images.omitted,
            );
        }
        for image in images.images {
            recording.write_image(image_atlas, image.1, image.2, image.0.clone());
        }
        for (image, x, y, region) in images.regions {
            recording.write_image_region(image_atlas, [*x, *y], image.clone(), *region);
        }
        let cpu_config = RenderConfig::new(
            &layout,
            params.width,
            params.height,
            &params.base_color,
            budget,
        );
        // HACK: The coarse workgroup counts is the number of active bins.
        if (cpu_config.workgroup_counts.coarse.0
            * cpu_config.workgroup_counts.coarse.1
            * cpu_config.workgroup_counts.coarse.2)
            > 256
        {
            log::warn!(
                "Trying to paint too large image. {}x{}.\n\
                See https://github.com/linebender/vello/issues/680 for details",
                params.width,
                params.height
            );
        }
        let buffer_sizes = &cpu_config.buffer_sizes;
        let wg_counts = &cpu_config.workgroup_counts;

        if packed.is_empty() {
            // HACK: wgpu doesn't allow empty buffers, so we make sure that the scene buffer we upload
            // can contain at least one array item.
            // The values passed here should never be read, because the scene size in config
            // is zero.
            packed.resize(size_of::<u32>(), u8::MAX);
        }
        let scene_buf =
            ResourceProxy::Buffer(recording.upload("vello.scene", std::mem::take(packed)));
        let config_buf = ResourceProxy::Buffer(
            recording.upload_uniform("vello.config", bytemuck::bytes_of(&cpu_config.gpu)),
        );
        let info_bin_data_buf = ResourceProxy::new_buf(
            buffer_sizes.bin_data.size_in_bytes() as u64,
            "vello.info_bin_data_buf",
        );
        let tile_buf =
            ResourceProxy::new_buf(buffer_sizes.tiles.size_in_bytes().into(), "vello.tile_buf");
        let segments_buf = ResourceProxy::new_buf(
            buffer_sizes.segments.size_in_bytes().into(),
            "vello.segments_buf",
        );
        let ptcl_buf =
            ResourceProxy::new_buf(buffer_sizes.ptcl.size_in_bytes().into(), "vello.ptcl_buf");
        let reduced_buf = ResourceProxy::new_buf(
            buffer_sizes.path_reduced.size_in_bytes().into(),
            "vello.reduced_buf",
        );
        // TODO: really only need pathtag_wgs - 1
        recording.dispatch(
            shaders.pathtag_reduce,
            wg_counts.path_reduce,
            [config_buf, scene_buf, reduced_buf],
        );
        let mut pathtag_parent = reduced_buf;
        let mut large_pathtag_bufs = None;
        let use_large_path_scan = wg_counts.use_large_path_scan && !shaders.pathtag_is_cpu;
        if use_large_path_scan {
            let reduced2_buf = ResourceProxy::new_buf(
                buffer_sizes.path_reduced2.size_in_bytes().into(),
                "vello.reduced2_buf",
            );
            recording.dispatch(
                shaders.pathtag_reduce2,
                wg_counts.path_reduce2,
                [reduced_buf, reduced2_buf],
            );
            let reduced_scan_buf = ResourceProxy::new_buf(
                buffer_sizes.path_reduced_scan.size_in_bytes().into(),
                "reduced_scan_buf",
            );
            recording.dispatch(
                shaders.pathtag_scan1,
                wg_counts.path_scan1,
                [reduced_buf, reduced2_buf, reduced_scan_buf],
            );
            pathtag_parent = reduced_scan_buf;
            large_pathtag_bufs = Some((reduced2_buf, reduced_scan_buf));
        }

        let tagmonoid_buf = ResourceProxy::new_buf(
            buffer_sizes.path_monoids.size_in_bytes().into(),
            "vello.tagmonoid_buf",
        );
        let pathtag_scan = if use_large_path_scan {
            shaders.pathtag_scan_large
        } else {
            shaders.pathtag_scan
        };
        recording.dispatch(
            pathtag_scan,
            wg_counts.path_scan,
            [config_buf, scene_buf, pathtag_parent, tagmonoid_buf],
        );
        recording.free_resource(reduced_buf);
        if let Some((reduced2, reduced_scan)) = large_pathtag_bufs {
            recording.free_resource(reduced2);
            recording.free_resource(reduced_scan);
        }
        let cpu_draw_prefix = shaders.cpu_draw_prefix_enabled
            && buffer_sizes.draw_monoids.size_in_bytes() <= 1024 * 1024;
        let cpu_draw_monoid_buf = if cpu_draw_prefix {
            let buffer = ResourceProxy::new_buf(
                buffer_sizes.draw_monoids.size_in_bytes().into(),
                "vello.draw_monoid_buf",
            );
            recording.dispatch(
                shaders.draw_prefix,
                (1, 1, 1),
                [config_buf, scene_buf, buffer],
            );
            Some(buffer)
        } else {
            None
        };
        // Selective CPU metadata only reads the host config/scene streams.
        // It must run before flatten first materializes those streams on GPU;
        // its output remains live through the later GPU draw_leaf dispatch.
        let cpu_draw_reduced_buf = if shaders.cpu_metadata_enabled && !cpu_draw_prefix {
            let buffer = ResourceProxy::new_buf(
                buffer_sizes.draw_reduced.size_in_bytes().into(),
                "vello.draw_reduced_buf",
            );
            recording.dispatch(
                shaders.draw_reduce,
                wg_counts.draw_reduce,
                [config_buf, scene_buf, buffer],
            );
            Some(buffer)
        } else {
            None
        };
        let path_bbox_buf = ResourceProxy::new_buf(
            buffer_sizes.path_bboxes.size_in_bytes().into(),
            "vello.path_bbox_buf",
        );
        recording.dispatch(
            shaders.bbox_clear,
            wg_counts.bbox_clear,
            [config_buf, path_bbox_buf],
        );
        let bump_buf = BufferProxy::new(
            buffer_sizes.bump_alloc.size_in_bytes().into(),
            "vello.bump_buf",
        );
        recording.clear_all(bump_buf);
        let bump_buf = ResourceProxy::Buffer(bump_buf);
        let lines_buf =
            ResourceProxy::new_buf(buffer_sizes.lines.size_in_bytes().into(), "vello.lines_buf");
        recording.dispatch(
            shaders.flatten,
            wg_counts.flatten,
            [
                config_buf,
                scene_buf,
                tagmonoid_buf,
                path_bbox_buf,
                bump_buf,
                lines_buf,
            ],
        );
        let draw_reduced_buf = if let Some(buffer) = cpu_draw_reduced_buf {
            Some(buffer)
        } else if cpu_draw_prefix {
            None
        } else {
            let buffer = ResourceProxy::new_buf(
                buffer_sizes.draw_reduced.size_in_bytes().into(),
                "vello.draw_reduced_buf",
            );
            recording.dispatch(
                shaders.draw_reduce,
                wg_counts.draw_reduce,
                [config_buf, scene_buf, buffer],
            );
            Some(buffer)
        };
        let draw_monoid_buf = cpu_draw_monoid_buf.unwrap_or_else(|| {
            ResourceProxy::new_buf(
                buffer_sizes.draw_monoids.size_in_bytes().into(),
                "vello.draw_monoid_buf",
            )
        });
        let clip_inp_buf = ResourceProxy::new_buf(
            buffer_sizes.clip_inps.size_in_bytes().into(),
            "vello.clip_inp_buf",
        );
        if cpu_draw_prefix {
            recording.dispatch(
                shaders.draw_leaf_prefix,
                wg_counts.draw_leaf,
                [
                    config_buf,
                    scene_buf,
                    path_bbox_buf,
                    draw_monoid_buf,
                    info_bin_data_buf,
                    clip_inp_buf,
                ],
            );
        } else {
            recording.dispatch(
                shaders.draw_leaf,
                wg_counts.draw_leaf,
                [
                    config_buf,
                    scene_buf,
                    draw_reduced_buf.expect("GPU draw leaf needs draw reduction"),
                    path_bbox_buf,
                    draw_monoid_buf,
                    info_bin_data_buf,
                    clip_inp_buf,
                ],
            );
        }
        if let Some(draw_reduced_buf) = draw_reduced_buf {
            recording.free_resource(draw_reduced_buf);
        }
        let clip_el_buf = ResourceProxy::new_buf(
            buffer_sizes.clip_els.size_in_bytes().into(),
            "vello.clip_el_buf",
        );
        let clip_bic_buf = ResourceProxy::new_buf(
            buffer_sizes.clip_bics.size_in_bytes().into(),
            "vello.clip_bic_buf",
        );
        if wg_counts.clip_reduce.0 > 0 {
            recording.dispatch(
                shaders.clip_reduce,
                wg_counts.clip_reduce,
                [clip_inp_buf, path_bbox_buf, clip_bic_buf, clip_el_buf],
            );
        }
        let clip_bbox_buf = ResourceProxy::new_buf(
            buffer_sizes.clip_bboxes.size_in_bytes().into(),
            "vello.clip_bbox_buf",
        );
        if wg_counts.clip_leaf.0 > 0 {
            self.clip_single_group = self.clip_single_group_allowed
                && clip_single_group_shader_selected(shaders, layout.n_clips, wg_counts.clip_leaf);
            recording.dispatch(
                if self.clip_single_group {
                    shaders
                        .clip_leaf_single_group
                        .expect("selected single-workgroup clip leaf shader was not configured")
                } else {
                    shaders.clip_leaf
                },
                wg_counts.clip_leaf,
                [
                    config_buf,
                    clip_inp_buf,
                    path_bbox_buf,
                    clip_bic_buf,
                    clip_el_buf,
                    draw_monoid_buf,
                    clip_bbox_buf,
                ],
            );
        }
        recording.free_resource(clip_inp_buf);
        recording.free_resource(clip_bic_buf);
        recording.free_resource(clip_el_buf);
        let draw_bbox_buf = ResourceProxy::new_buf(
            buffer_sizes.draw_bboxes.size_in_bytes().into(),
            "vello.draw_bbox_buf",
        );
        let bin_header_buf = ResourceProxy::new_buf(
            buffer_sizes.bin_headers.size_in_bytes().into(),
            "vello.bin_header_buf",
        );
        recording.dispatch(
            shaders.binning,
            wg_counts.binning,
            [
                config_buf,
                draw_monoid_buf,
                path_bbox_buf,
                clip_bbox_buf,
                draw_bbox_buf,
                bump_buf,
                info_bin_data_buf,
                bin_header_buf,
            ],
        );
        recording.free_resource(draw_monoid_buf);
        recording.free_resource(clip_bbox_buf);
        // Note: this only needs to be rounded up because of the workaround to store the tile_offset
        // in storage rather than workgroup memory.
        let path_buf =
            ResourceProxy::new_buf(buffer_sizes.paths.size_in_bytes().into(), "vello.path_buf");
        if let Some((shader, bounds)) = sparse_tile_bounds {
            recording.dispatch(
                shader,
                wg_counts.tile_alloc,
                [
                    config_buf,
                    scene_buf,
                    draw_bbox_buf,
                    bump_buf,
                    path_buf,
                    tile_buf,
                    bounds.into(),
                ],
            );
            recording.free_buffer(bounds);
        } else {
            recording.dispatch(
                shaders.tile_alloc,
                wg_counts.tile_alloc,
                [
                    config_buf,
                    scene_buf,
                    draw_bbox_buf,
                    bump_buf,
                    path_buf,
                    tile_buf,
                ],
            );
        }
        recording.free_resource(draw_bbox_buf);
        recording.free_resource(tagmonoid_buf);
        let indirect_count_buf = (!shaders.direct_path_dispatch_enabled).then(|| {
            let buffer = BufferProxy::new(
                buffer_sizes.indirect_count.size_in_bytes().into(),
                "vello.indirect_count",
            );
            recording.dispatch(
                shaders.path_count_setup,
                wg_counts.path_count_setup,
                [bump_buf, buffer.into()],
            );
            buffer
        });
        let seg_counts_buf = ResourceProxy::new_buf(
            buffer_sizes.seg_counts.size_in_bytes().into(),
            "vello.seg_counts_buf",
        );
        if shaders.direct_path_dispatch_enabled {
            recording.dispatch(
                shaders.path_count_direct,
                (shaders.direct_path_workgroups, 1, 1),
                [
                    config_buf,
                    bump_buf,
                    lines_buf,
                    path_buf,
                    tile_buf,
                    seg_counts_buf,
                ],
            );
        } else {
            recording.dispatch_indirect(
                shaders.path_count,
                indirect_count_buf.expect("indirect path count needs its count buffer"),
                0,
                [
                    config_buf,
                    bump_buf,
                    lines_buf,
                    path_buf,
                    tile_buf,
                    seg_counts_buf,
                ],
            );
        }
        recording.dispatch(
            shaders.backdrop,
            wg_counts.backdrop,
            [config_buf, bump_buf, path_buf, tile_buf],
        );
        if let (Some(SparseCoarseDispatch::Active { workgroups, .. }), Some(shader), Some(bits)) = (
            coarse_tiles,
            if solid_fuse {
                shaders
                    .coarse_sparse_active_solid_fuse
                    .or(shaders.coarse_sparse_active)
            } else if coarse_opaque_prune {
                shaders
                    .coarse_sparse_active_opaque_prune
                    .or(shaders.coarse_sparse_active)
            } else {
                shaders.coarse_sparse_active
            },
            coarse_tile_bits,
        ) {
            recording.dispatch(
                shader,
                (workgroups, 1, 1),
                [
                    config_buf,
                    scene_buf,
                    draw_monoid_buf,
                    bin_header_buf,
                    info_bin_data_buf,
                    path_buf,
                    tile_buf,
                    bump_buf,
                    ptcl_buf,
                    bits.into(),
                ],
            );
        } else if let (Some(shader), Some(bits)) = (
            if solid_fuse {
                shaders.coarse_sparse_solid_fuse.or(shaders.coarse_sparse)
            } else if coarse_opaque_prune {
                shaders.coarse_sparse_opaque_prune.or(shaders.coarse_sparse)
            } else {
                shaders.coarse_sparse
            },
            coarse_tile_bits,
        ) {
            recording.dispatch(
                shader,
                wg_counts.coarse,
                [
                    config_buf,
                    scene_buf,
                    draw_monoid_buf,
                    bin_header_buf,
                    info_bin_data_buf,
                    path_buf,
                    tile_buf,
                    bump_buf,
                    ptcl_buf,
                    bits.into(),
                ],
            );
        } else {
            recording.dispatch(
                if solid_fuse {
                    shaders.coarse_solid_fuse.unwrap_or(shaders.coarse)
                } else if coarse_opaque_prune {
                    shaders.coarse_opaque_prune.unwrap_or(shaders.coarse)
                } else {
                    shaders.coarse
                },
                wg_counts.coarse,
                [
                    config_buf,
                    scene_buf,
                    draw_monoid_buf,
                    bin_header_buf,
                    info_bin_data_buf,
                    path_buf,
                    tile_buf,
                    bump_buf,
                    ptcl_buf,
                ],
            );
        }
        if let Some(bits) = coarse_tile_bits {
            recording.free_buffer(bits);
        }
        if shaders.direct_path_dispatch_enabled {
            recording.dispatch(
                if segment_row_mask {
                    shaders
                        .path_tiling_direct_segment_row_mask
                        .expect("selected direct segment-row-mask writer was not configured")
                } else {
                    shaders.path_tiling_direct
                },
                (shaders.direct_path_workgroups, 1, 1),
                [
                    bump_buf,
                    seg_counts_buf,
                    lines_buf,
                    path_buf,
                    tile_buf,
                    segments_buf,
                    ptcl_buf,
                ],
            );
        } else {
            let indirect_count_buf =
                indirect_count_buf.expect("indirect path tiling needs its count buffer");
            recording.dispatch(
                shaders.path_tiling_setup,
                wg_counts.path_tiling_setup,
                [bump_buf, indirect_count_buf.into(), ptcl_buf],
            );
            recording.dispatch_indirect(
                if segment_row_mask {
                    shaders
                        .path_tiling_segment_row_mask
                        .expect("selected segment-row-mask writer was not configured")
                } else {
                    shaders.path_tiling
                },
                indirect_count_buf,
                0,
                [
                    bump_buf,
                    seg_counts_buf,
                    lines_buf,
                    path_buf,
                    tile_buf,
                    segments_buf,
                ],
            );
            recording.free_buffer(indirect_count_buf);
        }
        recording.free_resource(seg_counts_buf);
        recording.free_resource(scene_buf);
        recording.free_resource(draw_monoid_buf);
        recording.free_resource(bin_header_buf);
        recording.free_resource(path_buf);
        let out_image = ImageProxy::new(params.width, params.height, ImageFormat::Rgba8);
        let blend_spill_buf = BufferProxy::new(
            buffer_sizes.blend_spill.size_in_bytes().into(),
            "vello.blend_spill",
        );
        self.fine_wg_count = Some(wg_counts.fine);
        self.fine_resources = Some(FineResources {
            aa_config: params.antialiasing_method,
            config_buf,
            bump_buf,
            tile_buf,
            segments_buf,
            ptcl_buf,
            gradient_image,
            gradient_image_is_transient,
            info_bin_data_buf,
            blend_spill_buf: ResourceProxy::Buffer(blend_spill_buf),
            image_atlas: ResourceProxy::Image(image_atlas),
            out_image,
        });
        if !defer_bump_readback {
            finish_bump_readback(recording, *bump_buf.as_buf().unwrap(), measure_bump);
        }

        #[cfg(feature = "debug_layers")]
        {
            if capture_debug {
                let path_bboxes = *path_bbox_buf.as_buf().unwrap();
                let lines = *lines_buf.as_buf().unwrap();
                recording.download(lines);

                self.captured_buffers = Some(CapturedBuffers {
                    sizes: cpu_config.buffer_sizes,
                    path_bboxes,
                    lines,
                });
            } else {
                recording.free_resource(path_bbox_buf);
                recording.free_resource(lines_buf);
            }
        }
        #[cfg(not(feature = "debug_layers"))]
        {
            let _ = capture_debug;
            recording.free_resource(path_bbox_buf);
            recording.free_resource(lines_buf);
        }
    }

    /// Canonicalize every full fine tile after path tiling has produced its
    /// segment ranges and before ordinary Area fine consumes them.
    fn record_canonical_segment_order_full(
        &self,
        shaders: &FullShaders,
        recording: &mut Recording,
    ) {
        let fine = self.fine_resources.as_ref().unwrap();
        recording.dispatch(
            shaders
                .canonical_segment_order_full
                .expect("selected canonical full shader was not configured"),
            self.fine_wg_count.unwrap(),
            [
                fine.config_buf,
                fine.bump_buf,
                fine.segments_buf,
                fine.ptcl_buf,
            ],
        );
    }

    /// Canonicalize only the native tiles selected by the same sparse damage
    /// records that the following sparse Area fine dispatch consumes.
    fn record_canonical_segment_order_sparse(
        &self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: BufferProxy,
        tile_count: u32,
        solid_fuse: bool,
    ) {
        let fine = self.fine_resources.as_ref().unwrap();
        let shader = if solid_fuse {
            shaders
                .canonical_segment_order_sparse_solid_fuse
                .expect("selected canonical sparse SOLID-fuse shader was not configured")
        } else {
            shaders
                .canonical_segment_order_sparse
                .expect("selected canonical sparse shader was not configured")
        };
        recording.dispatch(
            shader,
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.bump_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                damage_tiles.into(),
            ],
        );
    }

    /// Run fine rasterization assuming the coarse phase succeeded.
    pub fn record_fine(&mut self, shaders: &FullShaders, recording: &mut Recording) {
        let fine_wg_count = self.fine_wg_count.take().unwrap();
        let fine = self.fine_resources.take().unwrap();
        match fine.aa_config {
            AaConfig::Area => {
                recording.dispatch(
                    if self.area_ppt8 {
                        shaders
                            .fine_area_ppt8
                            .expect("selected Area PPT8 shader was not configured")
                    } else {
                        shaders
                            .fine_area
                            .expect("shaders not configured to support AA mode: area")
                    },
                    fine_wg_count,
                    [
                        fine.config_buf,
                        fine.segments_buf,
                        fine.ptcl_buf,
                        fine.info_bin_data_buf,
                        fine.blend_spill_buf,
                        ResourceProxy::Image(fine.out_image),
                        fine.gradient_image,
                        fine.image_atlas,
                    ],
                );
            }
            _ => {
                if self.mask_buf.is_none() {
                    let mask_lut = match fine.aa_config {
                        AaConfig::Msaa16 => make_mask_lut_16(),
                        AaConfig::Msaa8 => make_mask_lut(),
                        _ => unreachable!(),
                    };
                    let buf = recording.upload("vello.mask_lut", mask_lut);
                    self.mask_buf = Some(buf.into());
                }
                let fine_shader = match fine.aa_config {
                    AaConfig::Msaa16 => shaders
                        .fine_msaa16
                        .expect("shaders not configured to support AA mode: msaa16"),
                    AaConfig::Msaa8 => shaders
                        .fine_msaa8
                        .expect("shaders not configured to support AA mode: msaa8"),
                    _ => unreachable!(),
                };
                recording.dispatch(
                    fine_shader,
                    fine_wg_count,
                    [
                        fine.config_buf,
                        fine.segments_buf,
                        fine.ptcl_buf,
                        fine.info_bin_data_buf,
                        fine.blend_spill_buf,
                        ResourceProxy::Image(fine.out_image),
                        fine.gradient_image,
                        fine.image_atlas,
                        self.mask_buf.unwrap(),
                    ],
                );
            }
        }
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(fine.blend_spill_buf);
        // TODO: make mask buf persistent
        if let Some(mask_buf) = self.mask_buf.take() {
            recording.free_resource(mask_buf);
        }
    }

    pub fn record_fine_sparse(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
        external_damage_tiles: Option<BufferProxy>,
    ) {
        self.record_fine_sparse_with_quad_skip(
            shaders,
            recording,
            damage_tiles,
            external_damage_tiles,
            false,
            false,
            false,
        );
    }

    fn record_fine_sparse_with_quad_skip(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
        external_damage_tiles: Option<BufferProxy>,
        fine_quad_skip: bool,
        solid_fuse: bool,
        segment_row_mask: bool,
    ) {
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let owns_damage_tiles = external_damage_tiles.is_none();
        let damage_tiles = external_damage_tiles.unwrap_or_else(|| {
            recording.upload(
                "vello.damage_tiles",
                bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
            )
        });
        if self.canonical_segment_order {
            // `sparse_tile_records` enumerates each native tile at most once,
            // so these one-workgroup tile walks own disjoint PTCL fills.
            self.record_canonical_segment_order_sparse(
                shaders,
                recording,
                damage_tiles,
                tile_count,
                self.canonical_solid_fuse,
            );
        }
        let fine = self.fine_resources.take().unwrap();
        let shader = self
            .area_ppt8
            .then_some(shaders.fine_sparse_area_ppt8)
            .flatten()
            .or_else(|| {
                segment_row_mask
                    .then_some(shaders.fine_sparse_area_segment_row_mask)
                    .flatten()
                    .or_else(|| {
                        solid_fuse
                            .then_some(shaders.fine_sparse_area_solid_fuse)
                            .flatten()
                    })
            })
            .or_else(|| {
                fine_quad_skip
                    .then_some(shaders.fine_sparse_area_quad_skip)
                    .flatten()
            })
            .or(shaders.fine_sparse_area)
            .expect("sparse fine shader was not configured");
        recording.dispatch(
            shader,
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                fine.blend_spill_buf,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
                damage_tiles.into(),
            ],
        );
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(fine.blend_spill_buf);
        if owns_damage_tiles {
            recording.free_resource(damage_tiles.into());
        }
    }

    #[cfg(test)]
    fn record_fine_sparse_with_ptcl_capture(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
        solid_fuse: bool,
    ) -> (ResourceProxy, BufferProxy) {
        let fine = self.fine_resources.take().unwrap();
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let damage_tiles = recording.upload(
            "vello.solid_fuse_capture_damage_tiles",
            bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
        );
        let shader = if solid_fuse {
            shaders
                .fine_sparse_area_solid_fuse
                .expect("paired solid-fuse fine shader was not configured")
        } else {
            shaders
                .fine_sparse_area
                .expect("sparse fine shader was not configured")
        };
        let ptcl = *fine.ptcl_buf.as_buf().unwrap();
        const PTCL_CAPTURE_MAX_BYTES: u64 = 64 * 1024 * 1024;
        assert!(
            ptcl.size <= PTCL_CAPTURE_MAX_BYTES,
            "solid-fuse PTCL capture exceeds its bounded test cap"
        );
        recording.dispatch(
            shader,
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                fine.blend_spill_buf,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
                damage_tiles.into(),
            ],
        );
        recording.download(ptcl);
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(fine.blend_spill_buf);
        recording.free_resource(damage_tiles.into());
        (ResourceProxy::Image(fine.out_image), ptcl)
    }

    /// Encode dense and sparse Area fine passes from one immutable coarse result.
    pub(crate) fn record_fine_area_pair(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
        geometry_download: bool,
    ) -> (ImageProxy, ImageProxy, Option<(BufferProxy, BufferProxy)>) {
        let fine_wg_count = self.fine_wg_count.take().unwrap();
        let fine = self.fine_resources.take().unwrap();
        assert_eq!(fine.aa_config, AaConfig::Area);
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let damage_tiles = recording.upload(
            "vello.shared_coarse_damage_tiles",
            bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
        );
        let sparse_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let full_spill = fine.blend_spill_buf;
        let sparse_spill = ResourceProxy::Buffer(BufferProxy::new(
            full_spill.as_buf().unwrap().size,
            "vello.shared_coarse_sparse_blend_spill",
        ));
        let fine_area = shaders
            .fine_area
            .expect("Area fine shader was not configured");
        recording.dispatch(
            fine_area,
            fine_wg_count,
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                full_spill,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
            ],
        );
        recording.dispatch(
            shaders
                .fine_sparse_area
                .expect("sparse Area fine shader was not configured"),
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                sparse_spill,
                ResourceProxy::Image(sparse_out),
                fine.gradient_image,
                fine.image_atlas,
                damage_tiles.into(),
            ],
        );
        // Both fine passes read the immutable coarse geometry. Capture its
        // final same-frame contents only after each has been encoded, before
        // the source resources are released.
        let downloads = geometry_download.then(|| {
            let ptcl = *fine.ptcl_buf.as_buf().unwrap();
            let segments = *fine.segments_buf.as_buf().unwrap();
            recording.download(ptcl);
            recording.download(segments);
            (ptcl, segments)
        });
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(full_spill);
        recording.free_resource(sparse_spill);
        recording.free_resource(damage_tiles.into());
        (fine.out_image, sparse_out, downloads)
    }

    /// Test-only four-way Area fine comparison using one immutable coarse result.
    /// Each pass has its own output and blend spill; only the ordinary coarse inputs and damage
    /// records are shared. Public shared-coarse diagnostics intentionally remain P4-only.
    #[cfg(test)]
    #[allow(
        clippy::too_many_lines,
        reason = "the four explicit binding lists are the test's shared-input and distinct-output proof"
    )]
    pub(crate) fn record_fine_area_ppt8_pair(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
    ) -> (ImageProxy, ImageProxy, ImageProxy, ImageProxy) {
        let fine_wg_count = self.fine_wg_count.take().unwrap();
        let fine = self.fine_resources.take().unwrap();
        assert_eq!(fine.aa_config, AaConfig::Area);
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let damage_tiles = recording.upload(
            "vello.shared_coarse_ppt8_damage_tiles",
            bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
        );
        let p8_full_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let p4_sparse_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let p8_sparse_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let p4_full_spill = fine.blend_spill_buf;
        let p8_full_spill = ResourceProxy::Buffer(BufferProxy::new(
            p4_full_spill.as_buf().unwrap().size,
            "vello.shared_coarse_ppt8_full_spill",
        ));
        let p4_sparse_spill = ResourceProxy::Buffer(BufferProxy::new(
            p4_full_spill.as_buf().unwrap().size,
            "vello.shared_coarse_ppt8_sparse_p4_spill",
        ));
        let p8_sparse_spill = ResourceProxy::Buffer(BufferProxy::new(
            p4_full_spill.as_buf().unwrap().size,
            "vello.shared_coarse_ppt8_sparse_p8_spill",
        ));
        let common = [
            fine.config_buf,
            fine.segments_buf,
            fine.ptcl_buf,
            fine.info_bin_data_buf,
        ];
        recording.dispatch(
            shaders
                .fine_area
                .expect("Area P4 fine shader was not configured"),
            fine_wg_count,
            [
                common[0],
                common[1],
                common[2],
                common[3],
                p4_full_spill,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
            ],
        );
        recording.dispatch(
            shaders
                .fine_area_ppt8
                .expect("Area PPT8 fine shader was not configured"),
            fine_wg_count,
            [
                common[0],
                common[1],
                common[2],
                common[3],
                p8_full_spill,
                ResourceProxy::Image(p8_full_out),
                fine.gradient_image,
                fine.image_atlas,
            ],
        );
        let sparse_common = [
            common[0],
            common[1],
            common[2],
            common[3],
            fine.gradient_image,
            fine.image_atlas,
        ];
        recording.dispatch(
            shaders
                .fine_sparse_area
                .expect("sparse Area P4 fine shader was not configured"),
            (tile_count, 1, 1),
            [
                sparse_common[0],
                sparse_common[1],
                sparse_common[2],
                sparse_common[3],
                p4_sparse_spill,
                ResourceProxy::Image(p4_sparse_out),
                sparse_common[4],
                sparse_common[5],
                damage_tiles.into(),
            ],
        );
        recording.dispatch(
            shaders
                .fine_sparse_area_ppt8
                .expect("sparse Area PPT8 fine shader was not configured"),
            (tile_count, 1, 1),
            [
                sparse_common[0],
                sparse_common[1],
                sparse_common[2],
                sparse_common[3],
                p8_sparse_spill,
                ResourceProxy::Image(p8_sparse_out),
                sparse_common[4],
                sparse_common[5],
                damage_tiles.into(),
            ],
        );
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(p4_full_spill);
        recording.free_resource(p8_full_spill);
        recording.free_resource(p4_sparse_spill);
        recording.free_resource(p8_sparse_spill);
        recording.free_resource(damage_tiles.into());
        (fine.out_image, p8_full_out, p4_sparse_out, p8_sparse_out)
    }

    /// Diagnostic-only order replay. Both ordinary Area passes consume one coarse result; the
    /// caller inserts a guarded GPU patch immediately before each pass.
    pub(crate) fn record_fine_area_order_replay(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        patch_shader: crate::ShaderId,
        patch_a: [BufferProxy; 4],
        patch_b: [BufferProxy; 4],
    ) -> (ImageProxy, ImageProxy, [BufferProxy; 2]) {
        let fine_wg_count = self.fine_wg_count.take().unwrap();
        let fine = self.fine_resources.take().unwrap();
        assert_eq!(fine.aa_config, AaConfig::Area);
        let right_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let left_spill = fine.blend_spill_buf;
        let right_spill = ResourceProxy::Buffer(BufferProxy::new(
            left_spill.as_buf().unwrap().size,
            "vello.segment_order_replay_right_spill",
        ));
        let fine_area = shaders
            .fine_area
            .expect("Area fine shader was not configured");
        // ptcl, segments, expected, requested-order, control, witness.
        recording.dispatch(
            patch_shader,
            (1, 1, 1),
            [
                fine.ptcl_buf,
                fine.segments_buf,
                patch_a[0].into(),
                patch_a[1].into(),
                patch_a[2].into(),
                patch_a[3].into(),
            ],
        );
        recording.dispatch(
            fine_area,
            fine_wg_count,
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                left_spill,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
            ],
        );
        recording.dispatch(
            patch_shader,
            (1, 1, 1),
            [
                fine.ptcl_buf,
                fine.segments_buf,
                patch_b[0].into(),
                patch_b[1].into(),
                patch_b[2].into(),
                patch_b[3].into(),
            ],
        );
        recording.dispatch(
            fine_area,
            fine_wg_count,
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                right_spill,
                ResourceProxy::Image(right_out),
                fine.gradient_image,
                fine.image_atlas,
            ],
        );
        recording.download(patch_a[3]);
        recording.download(patch_b[3]);
        for buffer in [
            patch_a[0], patch_a[1], patch_b[1], patch_a[2], patch_a[3], patch_b[3],
        ] {
            recording.free_resource(buffer.into());
        }
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(left_spill);
        recording.free_resource(right_spill);
        (fine.out_image, right_out, [patch_a[3], patch_b[3]])
    }

    /// Test-only paired sparse Area capture. One row-mask producer feeds both
    /// readers, so their pixel and clear-bit checks cannot be confused by a
    /// second coarse pass choosing a different segment order.
    #[cfg(test)]
    pub(crate) fn record_fine_sparse_area_pair_with_row_mask_capture(
        &mut self,
        shaders: &FullShaders,
        recording: &mut Recording,
        damage_tiles: &[u32],
    ) -> (ImageProxy, ImageProxy, (BufferProxy, BufferProxy)) {
        let fine = self.fine_resources.take().unwrap();
        assert_eq!(fine.aa_config, AaConfig::Area);
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let damage_tiles = recording.upload(
            "vello.segment_row_mask_damage_tiles",
            bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
        );
        let masked_out = ImageProxy::new(
            fine.out_image.width,
            fine.out_image.height,
            ImageFormat::Rgba8,
        );
        let ordinary_spill = fine.blend_spill_buf;
        let masked_spill = ResourceProxy::Buffer(BufferProxy::new(
            ordinary_spill.as_buf().unwrap().size,
            "vello.segment_row_mask_sparse_blend_spill",
        ));
        let ordinary_fine = shaders
            .fine_sparse_area
            .expect("ordinary sparse Area shader was not configured");
        let masked_fine = shaders
            .fine_sparse_area_segment_row_mask
            .expect("row-mask sparse Area shader was not configured");
        recording.dispatch(
            ordinary_fine,
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                ordinary_spill,
                ResourceProxy::Image(fine.out_image),
                fine.gradient_image,
                fine.image_atlas,
                damage_tiles.into(),
            ],
        );
        recording.dispatch(
            masked_fine,
            (tile_count, 1, 1),
            [
                fine.config_buf,
                fine.segments_buf,
                fine.ptcl_buf,
                fine.info_bin_data_buf,
                masked_spill,
                ResourceProxy::Image(masked_out),
                fine.gradient_image,
                fine.image_atlas,
                damage_tiles.into(),
            ],
        );
        let ptcl = *fine.ptcl_buf.as_buf().unwrap();
        let segments = *fine.segments_buf.as_buf().unwrap();
        // Capture only after both readers have consumed the one coarse result.
        recording.download(ptcl);
        recording.download(segments);
        recording.free_resource(fine.config_buf);
        recording.free_resource(fine.tile_buf);
        recording.free_resource(fine.segments_buf);
        recording.free_resource(fine.ptcl_buf);
        free_transient_gradient(
            recording,
            fine.gradient_image,
            fine.gradient_image_is_transient,
        );
        recording.free_resource(fine.info_bin_data_buf);
        recording.free_resource(ordinary_spill);
        recording.free_resource(masked_spill);
        recording.free_resource(damage_tiles.into());
        (fine.out_image, masked_out, (ptcl, segments))
    }

    /// Get the output image.
    ///
    /// This is going away, as the caller will add the output image to the bind
    /// map.
    pub fn out_image(&self) -> ImageProxy {
        self.fine_resources.as_ref().unwrap().out_image
    }

    pub fn bump_buf(&self) -> BufferProxy {
        *self
            .fine_resources
            .as_ref()
            .unwrap()
            .bump_buf
            .as_buf()
            .unwrap()
    }

    pub(crate) fn fine_geometry_bytes(&self) -> (u64, u64) {
        let fine = self.fine_resources.as_ref().unwrap();
        (
            fine.ptcl_buf.as_buf().unwrap().size,
            fine.segments_buf.as_buf().unwrap().size,
        )
    }

    #[cfg(feature = "debug_layers")]
    pub fn take_captured_buffers(&mut self) -> Option<CapturedBuffers> {
        self.captured_buffers.take()
    }
}

#[cfg(all(test, feature = "wgpu"))]
mod tests {
    use super::*;
    use crate::{
        AaConfig, RenderParams,
        recording::{Command, ShaderId},
    };
    use std::cmp::Ordering;

    fn direct_shaders(workgroups: u32) -> FullShaders {
        let shader = ShaderId(0);
        FullShaders {
            pathtag_reduce: shader,
            pathtag_reduce2: shader,
            pathtag_scan1: shader,
            pathtag_scan: shader,
            pathtag_scan_large: shader,
            bbox_clear: shader,
            flatten: shader,
            draw_reduce: shader,
            draw_leaf: shader,
            draw_prefix: shader,
            draw_leaf_prefix: shader,
            clip_reduce: shader,
            clip_leaf: shader,
            clip_leaf_single_group: Some(ShaderId(25)),
            binning: shader,
            tile_alloc: shader,
            tile_alloc_sparse: Some(ShaderId(8)),
            backdrop: shader,
            path_count_setup: shader,
            path_count: shader,
            path_count_direct: ShaderId(1),
            coarse: ShaderId(5),
            coarse_sparse: Some(ShaderId(3)),
            coarse_sparse_active: Some(ShaderId(4)),
            coarse_opaque_prune: Some(ShaderId(10)),
            coarse_sparse_opaque_prune: Some(ShaderId(11)),
            coarse_sparse_active_opaque_prune: Some(ShaderId(12)),
            coarse_solid_fuse: Some(ShaderId(13)),
            coarse_sparse_solid_fuse: Some(ShaderId(14)),
            coarse_sparse_active_solid_fuse: Some(ShaderId(15)),
            path_tiling_setup: shader,
            path_tiling: shader,
            path_tiling_direct: ShaderId(2),
            path_tiling_segment_row_mask: Some(ShaderId(17)),
            path_tiling_direct_segment_row_mask: Some(ShaderId(18)),
            fine_area: Some(ShaderId(6)),
            fine_area_ppt8: Some(ShaderId(23)),
            fine_msaa8: None,
            fine_msaa16: None,
            fine_sparse_area: Some(ShaderId(7)),
            fine_sparse_area_ppt8: Some(ShaderId(24)),
            fine_sparse_area_quad_skip: Some(ShaderId(9)),
            fine_sparse_area_solid_fuse: Some(ShaderId(16)),
            fine_sparse_area_segment_row_mask: Some(ShaderId(19)),
            canonical_segment_order_full: Some(ShaderId(20)),
            canonical_segment_order_sparse: Some(ShaderId(21)),
            canonical_segment_order_sparse_solid_fuse: Some(ShaderId(22)),
            pathtag_is_cpu: false,
            cpu_metadata_enabled: false,
            cpu_draw_prefix_enabled: false,
            direct_path_dispatch_enabled: true,
            direct_path_workgroups: workgroups,
        }
    }

    #[test]
    fn clip_single_group_selection_requires_exact_single_gpu_workgroup() {
        let shaders = direct_shaders(1);
        for n_clips in [1, 255, 256] {
            assert!(clip_single_group_shader_selected(
                &shaders,
                n_clips,
                (1, 1, 1)
            ));
        }
        for (n_clips, workgroups) in [
            (0, (0, 1, 1)),
            (0, (1, 1, 1)),
            (257, (2, 1, 1)),
            (1, (1, 2, 1)),
            (1, (1, 1, 2)),
        ] {
            assert!(
                !clip_single_group_shader_selected(&shaders, n_clips, workgroups),
                "n_clips={n_clips}, workgroups={workgroups:?} must use ordinary clip leaf"
            );
        }
        let mut unavailable = shaders;
        unavailable.clip_leaf_single_group = None;
        assert!(!clip_single_group_shader_selected(
            &unavailable,
            1,
            (1, 1, 1)
        ));
    }

    fn canonical_record_cmp(left: &[u32; 6], right: &[u32; 6]) -> Ordering {
        left.iter().take(5).cmp(right.iter().take(5))
    }

    fn canonical_record_cmp_at(records: &[[u32; 6]], left: usize, right: usize) -> Ordering {
        match (records.get(left), records.get(right)) {
            (Some(left), Some(right)) => canonical_record_cmp(left, right),
            _ => Ordering::Equal,
        }
    }

    fn canonical_sift_down(records: &mut [[u32; 6]], root_start: usize, end: usize) {
        let mut root = root_start;
        while let Some(child) = root
            .checked_mul(2)
            .and_then(|value| value.checked_add(1))
            .filter(|child| *child < end)
        {
            let mut candidate = root;
            if canonical_record_cmp_at(records, child, candidate).is_gt() {
                candidate = child;
            }
            if let Some(right) = child.checked_add(1).filter(|right| *right < end)
                && canonical_record_cmp_at(records, right, candidate).is_gt()
            {
                candidate = right;
            }
            if candidate == root {
                return;
            }
            records.swap(root, candidate);
            root = candidate;
        }
    }

    fn canonical_heapsort(records: &mut [[u32; 6]]) {
        let mut start = records.len() / 2;
        while let Some(next) = start.checked_sub(1) {
            start = next;
            canonical_sift_down(records, start, records.len());
        }
        let mut end = records.len();
        while let Some(next) = end.checked_sub(1).filter(|next| *next != 0) {
            end = next;
            records.swap(0, end);
            canonical_sift_down(records, 0, end);
        }
    }

    #[test]
    fn canonical_heapsort_orders_five_words_and_moves_sixth_word_with_each_record() {
        let mut records = [
            [2, 0, 0, 0, 0, 77],
            [1, u32::MAX, 0, 0, 0, 88],
            [1, 0, 0, 0, 1, 99],
            [1, 0, 0, 0, 0, 111],
            [1, 0, 0, 0, 0, 222],
        ];
        let mut expected_records = records.to_vec();
        expected_records.sort_unstable();
        canonical_heapsort(&mut records);
        assert!(records.windows(2).all(
            |pair| matches!(pair, [left, right] if canonical_record_cmp(left, right).is_le())
        ));
        let mut actual_records = records.to_vec();
        actual_records.sort_unstable();
        assert_eq!(actual_records, expected_records);
    }

    #[test]
    fn canonical_heapsort_handles_two_and_three_record_heaps() {
        let mut two = [[1, 0, 0, 0, 0, 2], [0, u32::MAX, 0, 0, 0, 1]];
        canonical_heapsort(&mut two);
        assert!(matches!(&two, [left, right] if canonical_record_cmp(left, right).is_le()));

        let mut three = [[3, 0, 0, 0, 0, 3], [2, 0, 0, 0, 0, 2], [1, 0, 0, 0, 0, 1]];
        canonical_heapsort(&mut three);
        assert!(three.windows(2).all(
            |pair| matches!(pair, [left, right] if canonical_record_cmp(left, right).is_le())
        ));
    }

    fn assert_canonical_heapsort_preserves_records(mut records: Vec<[u32; 6]>) {
        let mut expected_multiset = records.clone();
        expected_multiset.sort_unstable();
        canonical_heapsort(&mut records);
        assert!(records.windows(2).all(
            |pair| matches!(pair, [left, right] if canonical_record_cmp(left, right).is_le())
        ));
        let mut actual_multiset = records;
        actual_multiset.sort_unstable();
        assert_eq!(actual_multiset, expected_multiset);
    }

    #[test]
    fn canonical_heapsort_handles_native_and_larger_fill_lengths() {
        // These larger fills cover the same heap paths as native captures.
        // The dedicated tie fixture above covers equal five-word keys.
        let native_fill = (0_u32..49)
            .rev()
            .map(|value| [value % 7, value / 7, value % 3, 0, value % 5, value])
            .collect();
        assert_canonical_heapsort_preserves_records(native_fill);

        let larger_fill = (0_u32..257)
            .rev()
            .map(|value| {
                [
                    value % 11,
                    value / 11,
                    value % 7,
                    value % 5,
                    value % 3,
                    value ^ 0xa5a5_0000,
                ]
            })
            .collect();
        assert_canonical_heapsort_preserves_records(larger_fill);
    }

    #[test]
    fn canonical_sparse_recording_reuses_damage_binding_and_frees_after_fine() {
        for external_damage in [None, Some(BufferProxy::new(36, "test.canonical_damage"))] {
            let shaders = direct_shaders(1);
            let mut render = Render::new();
            let mut resolver = Resolver::new();
            let mut recording = Recording::default();
            let mut packed = Vec::new();
            let mut image_atlas = None;
            render.render_encoding_coarse_with_dispatch(
                &Encoding::default(),
                &mut resolver,
                &shaders,
                &mut image_atlas,
                &mut recording,
                &mut packed,
                &RenderParams {
                    base_color: peniko::Color::TRANSPARENT,
                    width: 16,
                    height: 16,
                    antialiasing_method: AaConfig::Area,
                },
                &vello_encoding::BumpBudget::INITIAL,
                true,
                false,
                None,
                true,
                None,
            );
            render.canonical_segment_order = true;
            render.canonical_solid_fuse = false;
            let bump = render.bump_buf();
            render.record_fine_sparse_with_quad_skip(
                &shaders,
                &mut recording,
                &[0; 9],
                external_damage,
                false,
                false,
                false,
            );
            finish_bump_readback(&mut recording, bump, true);

            let path_tiling = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::Dispatch(shader, _, _) if *shader == ShaderId(2)))
                .expect("coarse must finish direct path tiling before canonical ordering");
            let canonical = recording
                .commands
                .iter()
                .enumerate()
                .find_map(|(index, command)| match command {
                    Command::Dispatch(shader, _, bindings) if *shader == ShaderId(21) => bindings
                        .get(0)
                        .and_then(ResourceProxy::as_buf)
                        .zip(bindings.get(1).and_then(ResourceProxy::as_buf))
                        .zip(bindings.get(2).and_then(ResourceProxy::as_buf))
                        .zip(bindings.get(3).and_then(ResourceProxy::as_buf))
                        .zip(bindings.get(4).and_then(ResourceProxy::as_buf))
                        .map(|((((config, bump), segments), ptcl), damage)| {
                            (index, *config, *bump, *segments, *ptcl, *damage)
                        }),
                    _ => None,
                })
                .expect("canonical sparse dispatch must bind damage records");
            let fine = recording
                .commands
                .iter()
                .enumerate()
                .find_map(|(index, command)| match command {
                    Command::Dispatch(shader, _, bindings) if *shader == ShaderId(7) => bindings
                        .get(0)
                        .and_then(ResourceProxy::as_buf)
                        .zip(bindings.get(1).and_then(ResourceProxy::as_buf))
                        .zip(bindings.get(2).and_then(ResourceProxy::as_buf))
                        .zip(bindings.get(8).and_then(ResourceProxy::as_buf))
                        .map(|(((config, segments), ptcl), damage)| {
                            (index, *config, *segments, *ptcl, *damage)
                        }),
                    _ => None,
                })
                .expect("sparse fine must bind the canonical dispatch's damage records");
            assert!(path_tiling < canonical.0 && canonical.0 < fine.0);
            assert!(canonical.1.id == fine.1.id);
            assert!(canonical.3.id == fine.2.id);
            assert!(canonical.4.id == fine.3.id);
            assert!(canonical.5.id == fine.4.id);

            if external_damage.is_none() {
                let free = recording
                    .commands
                    .iter()
                    .position(
                        |command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == canonical.5.id),
                    )
                    .expect("transient damage upload must be freed");
                assert!(free > fine.0);
            } else {
                assert!(!recording.commands.iter().any(
                    |command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == canonical.5.id)
                ));
            }
            let bump_download = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::Download(buffer) if buffer.id == canonical.2.id))
                .expect("selected canonical recording must download its existing bump counter after fine");
            let bump_free = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == canonical.2.id))
                .expect("selected canonical recording must free its bump counter after download");
            assert!(fine.0 < bump_download && bump_download < bump_free);
            for shared in [canonical.1, canonical.3, canonical.4] {
                let free = recording
                    .commands
                    .iter()
                    .position(|command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == shared.id))
                    .expect("shared fine resource must be released");
                assert!(free > fine.0);
            }
        }
    }

    #[test]
    fn segment_row_mask_clear_bits_preserve_original_area_terms() {
        let coordinates = [
            0.0,
            -0.0,
            f32::from_bits(1),
            f32::MIN_POSITIVE,
            1.0e-6,
            0.5,
            1.0,
            7.5,
            15.0,
            15.999_999,
            16.0,
        ];
        let edges = [0.0, -0.0, 1.0e-6, 0.5, 1.0, 7.5, 15.0, 16.0, 1.0e9];
        for x0 in coordinates {
            for x1 in coordinates {
                for y0 in coordinates {
                    for y1 in coordinates {
                        for y_edge in edges {
                            let point0 = [x0, y0];
                            let point1 = [x1, y1];
                            let mask = segment_row_mask_for_final_segment(point0, point1, y_edge);
                            for row in 0..16 {
                                if mask & (1_u16 << row) == 0 {
                                    assert!(
                                        !fine_row_has_original_area_contribution(
                                            point0, point1, y_edge, row
                                        ),
                                        "clear row {row} must preserve the original Area equations: {point0:?} {point1:?} edge={y_edge:?} mask={mask:#06x}"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        for (point0, point1, edge) in [
            ([f32::NAN, 0.0], [1.0, 1.0], 1.0e9),
            ([0.0, f32::INFINITY], [1.0, 1.0], 1.0e9),
            ([0.0, 0.0], [16.1, 1.0], 1.0e9),
            ([0.0, 0.0], [1.0, 1.0], f32::NAN),
            ([0.0, 0.0], [1.0, 1.0], 16.1),
        ] {
            assert_eq!(
                segment_row_mask_for_final_segment(point0, point1, edge),
                u16::MAX
            );
        }
    }

    #[test]
    fn segment_row_mask_selection_requires_bounded_active_pair_and_preserves_modes() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let mut direct = direct_shaders(128);
        assert!(sparse_segment_row_mask_shader_selected(
            &direct, active, true, false, false, false, 257, 257
        ));
        for (quad, opaque, fused) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert!(!sparse_segment_row_mask_shader_selected(
                &direct, active, true, quad, opaque, fused, 257, 257
            ));
        }
        assert!(!sparse_segment_row_mask_shader_selected(
            &direct,
            Some(SparseCoarseDispatch::Legacy(&selector)),
            true,
            false,
            false,
            false,
            257,
            257,
        ));
        assert!(!sparse_segment_row_mask_shader_selected(
            &direct,
            Some(SparseCoarseDispatch::Active {
                selector: &selector,
                workgroups: 1,
                path_tile_bounds: Some([0, 0, 17, 17]),
            }),
            true,
            false,
            false,
            false,
            257,
            257,
        ));
        direct.path_tiling_direct_segment_row_mask = None;
        assert!(!sparse_segment_row_mask_shader_selected(
            &direct, active, true, false, false, false, 257, 257
        ));

        let mut missing_reader = direct_shaders(128);
        missing_reader.fine_sparse_area_segment_row_mask = None;
        assert!(!sparse_segment_row_mask_shader_selected(
            &missing_reader,
            active,
            true,
            false,
            false,
            false,
            257,
            257,
        ));

        let mut indirect = direct_shaders(128);
        indirect.direct_path_dispatch_enabled = false;
        assert!(sparse_segment_row_mask_shader_selected(
            &indirect, active, true, false, false, false, 257, 257
        ));
        indirect.path_tiling_segment_row_mask = None;
        assert!(!sparse_segment_row_mask_shader_selected(
            &indirect, active, true, false, false, false, 257, 257
        ));
    }

    fn segment_row_mask_recording(
        direct: bool,
        dispatch: Option<SparseCoarseDispatch<'_>>,
        missing_writer: bool,
        missing_reader: bool,
    ) -> (Recording, bool) {
        let mut shaders = direct_shaders(128);
        shaders.direct_path_dispatch_enabled = direct;
        if missing_writer {
            if direct {
                shaders.path_tiling_direct_segment_row_mask = None;
            } else {
                shaders.path_tiling_segment_row_mask = None;
            }
        }
        if missing_reader {
            shaders.fine_sparse_area_segment_row_mask = None;
        }
        let selected = sparse_segment_row_mask_shader_selected(
            &shaders, dispatch, true, false, false, false, 257, 257,
        );
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch_and_opaque_prune(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            false,
            false,
            false,
            selected,
            false,
            None,
        );
        render.record_fine_sparse_with_quad_skip(
            &shaders,
            &mut recording,
            &[0; 9],
            None,
            false,
            false,
            selected,
        );
        (recording, selected)
    }

    #[test]
    fn segment_row_mask_recording_uses_only_matched_writer_reader_and_bounds() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        for (direct, producer) in [(true, ShaderId(18)), (false, ShaderId(17))] {
            let (recording, selected) = segment_row_mask_recording(direct, active, false, false);
            assert!(selected);
            let producer_bindings = if direct { 7 } else { 6 };
            if direct {
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(shader, _, bindings) if *shader == producer && bindings.len() == producer_bindings)
                }));
            } else {
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::DispatchIndirect(shader, _, _, bindings) if *shader == producer && bindings.len() == producer_bindings)
                }));
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(ShaderId(0), _, bindings) if bindings.len() == 3)
                }));
            }
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(ShaderId(19), _, bindings) if bindings.len() == 9)
            }));
        }

        let invalid = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([0, 0, 17, 17]),
        });
        for (direct, dispatch, missing_writer, missing_reader) in [
            (true, invalid, false, false),
            (true, active, true, false),
            (true, active, false, true),
            (false, active, true, false),
            (false, active, false, true),
        ] {
            let (recording, selected) =
                segment_row_mask_recording(direct, dispatch, missing_writer, missing_reader);
            assert!(!selected);
            let bounds_uploads = recording
                .commands
                .iter()
                .filter(|command| {
                    matches!(command, Command::UploadUniform(buffer, _) if buffer.name == "vello.sparse_tile_bounds")
                })
                .count();
            assert_eq!(
                bounds_uploads,
                if matches!(
                    dispatch,
                    Some(SparseCoarseDispatch::Active {
                        path_tile_bounds: Some([0, 0, 17, 17]),
                        ..
                    })
                ) {
                    0
                } else {
                    1
                },
                "missing row-mask partners must preserve the ordinary allocation bounds decision"
            );
            let ordinary_producer = if direct { ShaderId(2) } else { ShaderId(0) };
            let producer_bindings = if direct { 7 } else { 6 };
            if direct {
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(shader, _, bindings) if *shader == ordinary_producer && bindings.len() == producer_bindings)
                }));
            } else {
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::DispatchIndirect(shader, _, _, bindings) if *shader == ordinary_producer && bindings.len() == producer_bindings)
                }));
            }
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(ShaderId(7), _, bindings) if bindings.len() == 9)
            }));
            assert!(!recording.commands.iter().any(|command| {
                matches!(
                    command,
                    Command::Dispatch(ShaderId(18) | ShaderId(19), _, _)
                        | Command::DispatchIndirect(ShaderId(17), _, _, _)
                )
            }));
        }
    }

    #[test]
    fn segment_row_mask_pair_records_one_geometry_for_both_readers_before_downloads() {
        let selector = vec![0; 2_048];
        let dispatch = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let shaders = direct_shaders(128);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch_and_opaque_prune(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            false,
            false,
            false,
            true,
            false,
            None,
        );
        let (_, _, (ptcl, segments)) = render.record_fine_sparse_area_pair_with_row_mask_capture(
            &shaders,
            &mut recording,
            &[0; 9],
        );
        let fine = recording
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| match command {
                Command::Dispatch(ShaderId(7) | ShaderId(19), _, bindings) => {
                    Some((index, bindings))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(fine.len(), 2);
        assert_eq!(
            fine[0].1[1].as_buf().unwrap().id.0,
            fine[1].1[1].as_buf().unwrap().id.0
        );
        assert_eq!(
            fine[0].1[2].as_buf().unwrap().id.0,
            fine[1].1[2].as_buf().unwrap().id.0
        );
        assert_ne!(
            fine[0].1[4].as_buf().unwrap().id.0,
            fine[1].1[4].as_buf().unwrap().id.0
        );
        assert_ne!(
            fine[0].1[5].as_image().unwrap().id.0,
            fine[1].1[5].as_image().unwrap().id.0
        );
        let downloads = [ptcl, segments].map(|buffer| {
            recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::Download(candidate) if candidate.id == buffer.id))
                .expect("captured geometry must be downloaded")
        });
        assert!(downloads.into_iter().all(|download| download > fine[1].0));
        for buffer in [ptcl, segments] {
            let free = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::FreeBuffer(candidate) if candidate.id == buffer.id))
                .expect("captured geometry must be released");
            assert!(free > downloads[1]);
        }
    }

    fn direct_dispatch_grids(workgroups: u32) -> Vec<(usize, WorkgroupSize)> {
        let shaders = direct_shaders(workgroups);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 16,
                height: 16,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            None,
            false,
            None,
        );
        recording
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Dispatch(shader, grid, _)
                    if *shader == shaders.path_count_direct
                        || *shader == shaders.path_tiling_direct =>
                {
                    Some((shader.0, *grid))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn direct_path_dispatch_records_the_same_nonzero_grid_for_both_stages() {
        for workgroups in [1, 8, 128] {
            let grids = direct_dispatch_grids(workgroups);
            assert_eq!(
                grids,
                vec![(1, (workgroups, 1, 1)), (2, (workgroups, 1, 1)),],
                "both stride-loop stages must retain the configured nonzero grid"
            );
        }
    }

    fn sparse_coarse_dispatches(
        dispatch: Option<SparseCoarseDispatch<'_>>,
        width: u32,
        height: u32,
        active_shader_available: bool,
    ) -> Vec<(usize, WorkgroupSize)> {
        let mut shaders = direct_shaders(128);
        if !active_shader_available {
            shaders.coarse_sparse_active = None;
        }
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width,
                height,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            false,
            None,
        );
        if dispatch.is_none() {
            assert!(!recording.commands.iter().any(|command| {
                matches!(command, Command::UploadUniform(buffer, _) if buffer.name == "vello.coarse_sparse_tiles")
            }));
        }
        recording
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Dispatch(shader, grid, _)
                    if *shader == ShaderId(3)
                        || *shader == ShaderId(4)
                        || *shader == ShaderId(5) =>
                {
                    Some((shader.0, *grid))
                }
                _ => None,
            })
            .collect()
    }

    fn sparse_tile_bounds_recording(
        dispatch: Option<SparseCoarseDispatch<'_>>,
        active_shader_available: bool,
        allocation_shader_available: bool,
    ) -> Recording {
        let mut shaders = direct_shaders(128);
        if !active_shader_available {
            shaders.coarse_sparse_active = None;
        }
        if !allocation_shader_available {
            shaders.tile_alloc_sparse = None;
        }
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            false,
            None,
        );
        recording
    }

    fn sparse_fine_shader_recording(quad_skip: bool, public_wrapper: bool) -> Recording {
        let shaders = direct_shaders(128);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        let params = RenderParams {
            base_color: peniko::Color::TRANSPARENT,
            width: 16,
            height: 16,
            antialiasing_method: AaConfig::Area,
        };
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &params,
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            None,
            false,
            None,
        );
        if public_wrapper {
            render.record_fine_sparse(&shaders, &mut recording, &[0; 9], None);
        } else {
            render.record_fine_sparse_with_quad_skip(
                &shaders,
                &mut recording,
                &[0; 9],
                None,
                quad_skip,
                false,
                false,
            );
        }
        recording
    }

    #[test]
    fn sparse_fine_quad_skip_selects_only_the_private_opt_in_path() {
        let selected = sparse_fine_shader_recording(true, false);
        assert!(selected.commands.iter().any(|command| {
            matches!(command, Command::Dispatch(ShaderId(9), _, bindings) if bindings.len() == 9)
        }));
        for recording in [
            sparse_fine_shader_recording(false, false),
            sparse_fine_shader_recording(true, true),
        ] {
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(ShaderId(7), _, bindings) if bindings.len() == 9)
            }));
            assert!(
                !recording
                    .commands
                    .iter()
                    .any(|command| { matches!(command, Command::Dispatch(ShaderId(9), _, _)) })
            );
        }
    }

    fn opaque_prune_recording(
        dispatch: Option<SparseCoarseDispatch<'_>>,
        requested: bool,
        missing_variant: bool,
    ) -> (Recording, bool) {
        let mut shaders = direct_shaders(128);
        if missing_variant {
            match dispatch {
                Some(SparseCoarseDispatch::Active { .. }) => {
                    shaders.coarse_sparse_active_opaque_prune = None;
                }
                Some(SparseCoarseDispatch::Legacy(_)) => {
                    shaders.coarse_sparse_opaque_prune = None;
                }
                None => shaders.coarse_opaque_prune = None,
            }
        }
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        let selected = sparse_opaque_prune_shader_selected(&shaders, dispatch, requested);
        render.render_encoding_coarse_with_dispatch_and_opaque_prune(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            requested,
            false,
            false,
            false,
            false,
            None,
        );
        (recording, selected)
    }

    #[test]
    fn sparse_opaque_prune_selects_only_matching_coarse_variants() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let cases = [
            (None, ShaderId(10), 9usize),
            (
                Some(SparseCoarseDispatch::Legacy(&selector)),
                ShaderId(11),
                10,
            ),
            (active, ShaderId(12), 10),
        ];
        for (dispatch, shader, bindings) in cases {
            let (recording, selected) = opaque_prune_recording(dispatch, true, false);
            assert!(selected);
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(candidate, _, resources) if *candidate == shader && resources.len() == bindings)
            }));
        }
    }

    #[test]
    fn sparse_opaque_prune_unrequested_and_missing_variants_use_original_modes() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let cases = [
            (None, ShaderId(5)),
            (Some(SparseCoarseDispatch::Legacy(&selector)), ShaderId(3)),
            (active, ShaderId(4)),
        ];
        for (dispatch, expected) in cases {
            let active_mode = matches!(dispatch, Some(SparseCoarseDispatch::Active { .. }));
            for (recording, selected) in [
                opaque_prune_recording(dispatch, false, false),
                opaque_prune_recording(dispatch, true, true),
            ] {
                assert!(!selected);
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(shader, _, _) if *shader == expected)
                }));
                assert!(!recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(ShaderId(10 | 11 | 12), _, _))
                }));
                if active_mode {
                    assert!(recording.commands.iter().any(|command| {
                        matches!(command, Command::Dispatch(ShaderId(8), _, bindings) if bindings.len() == 7)
                    }));
                }
            }
        }
    }

    fn solid_fuse_recording(
        dispatch: Option<SparseCoarseDispatch<'_>>,
        requested: bool,
        missing_coarse: bool,
        missing_fine: bool,
        quad_skip_requested: bool,
        opaque_prune_requested: bool,
    ) -> (Recording, bool) {
        let mut shaders = direct_shaders(128);
        if missing_coarse {
            match dispatch {
                Some(SparseCoarseDispatch::Active { .. }) => {
                    shaders.coarse_sparse_active_solid_fuse = None;
                }
                Some(SparseCoarseDispatch::Legacy(_)) => {
                    shaders.coarse_sparse_solid_fuse = None;
                }
                None => shaders.coarse_solid_fuse = None,
            }
        }
        if missing_fine {
            shaders.fine_sparse_area_solid_fuse = None;
        }
        let selected = sparse_solid_fuse_shader_selected(
            &shaders,
            dispatch,
            requested,
            quad_skip_requested,
            opaque_prune_requested,
        );
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch_and_opaque_prune(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            dispatch,
            opaque_prune_requested,
            requested,
            quad_skip_requested,
            false,
            false,
            None,
        );
        render.record_fine_sparse_with_quad_skip(
            &shaders,
            &mut recording,
            &[0; 9],
            None,
            quad_skip_requested,
            selected,
            false,
        );
        (recording, selected)
    }

    #[test]
    fn sparse_solid_fuse_selects_only_complete_matching_pairs() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let cases = [
            (None, ShaderId(13), 9usize),
            (
                Some(SparseCoarseDispatch::Legacy(&selector)),
                ShaderId(14),
                10,
            ),
            (active, ShaderId(15), 10),
        ];
        for (dispatch, coarse, bindings) in cases {
            let (recording, selected) =
                solid_fuse_recording(dispatch, true, false, false, false, false);
            assert!(selected);
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(shader, _, resources) if *shader == coarse && resources.len() == bindings)
            }));
            assert!(recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(ShaderId(16), _, resources) if resources.len() == 9)
            }));
            if matches!(dispatch, Some(SparseCoarseDispatch::Active { .. })) {
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(ShaderId(15), (1, 1, 1), bindings) if bindings.len() == 10)
                }));
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(ShaderId(8), _, bindings) if bindings.len() == 7)
                }));
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::UploadUniform(buffer, bytes) if buffer.name == "vello.sparse_tile_bounds" && bytes == bytemuck::bytes_of(&[1u32, 1, 4, 4]))
                }));
            }
        }
    }

    #[test]
    fn sparse_solid_fuse_falls_back_to_same_mode_and_preserves_other_experiments() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 1, 4, 4]),
        });
        let cases = [
            (None, ShaderId(5)),
            (Some(SparseCoarseDispatch::Legacy(&selector)), ShaderId(3)),
            (active, ShaderId(4)),
        ];
        for (dispatch, expected) in cases {
            for (recording, selected) in [
                solid_fuse_recording(dispatch, false, false, false, false, false),
                solid_fuse_recording(dispatch, true, true, false, false, false),
                solid_fuse_recording(dispatch, true, false, true, false, false),
            ] {
                assert!(!selected);
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(shader, _, _) if *shader == expected)
                }));
                assert!(recording.commands.iter().any(|command| {
                    matches!(command, Command::Dispatch(ShaderId(7), _, resources) if resources.len() == 9)
                }));
                assert!(!recording.commands.iter().any(|command| {
                    matches!(
                        command,
                        Command::Dispatch(ShaderId(13 | 14 | 15 | 16), _, _)
                    )
                }));
                if matches!(dispatch, Some(SparseCoarseDispatch::Active { .. })) {
                    assert!(recording.commands.iter().any(|command| {
                        matches!(command, Command::Dispatch(ShaderId(4), (1, 1, 1), bindings) if bindings.len() == 10)
                    }));
                    assert!(recording.commands.iter().any(|command| {
                        matches!(command, Command::UploadUniform(buffer, bytes) if buffer.name == "vello.sparse_tile_bounds" && bytes == bytemuck::bytes_of(&[1u32, 1, 4, 4]))
                    }));
                }
            }
        }
        let (quad, selected) = solid_fuse_recording(None, true, false, false, true, false);
        assert!(!selected);
        assert!(
            quad.commands
                .iter()
                .any(|command| { matches!(command, Command::Dispatch(ShaderId(9), _, _)) })
        );
        let (opaque, selected) = solid_fuse_recording(None, true, false, false, false, true);
        assert!(!selected);
        assert!(
            opaque
                .commands
                .iter()
                .any(|command| { matches!(command, Command::Dispatch(ShaderId(10), _, _)) })
        );
    }

    #[test]
    fn sparse_opaque_prune_predicate_rejects_every_unsafe_boundary() {
        let opaque = 0xff00_00ff;
        assert!(sparse_opaque_prune_eligible(0, 0, 0, 0, false, opaque));
        for args in [
            (1, 0, 0, 0, false, opaque),
            (0, 1, 0, 0, false, opaque),
            (0, 0, 1, 0, false, opaque),
            (0, 0, 0, 1, false, opaque),
            (0, 0, 0, 0, true, opaque),
            (0, 0, 0, 0, false, 0xfe00_00ff),
        ] {
            assert!(!sparse_opaque_prune_eligible(
                args.0, args.1, args.2, args.3, args.4, args.5
            ));
        }
    }

    #[test]
    fn sparse_tile_bounds_records_dedicated_uniform_binding_and_free() {
        let selector = vec![0; 2_048];
        let bounds = [1, 2, 5, 6];
        let recording = sparse_tile_bounds_recording(
            Some(SparseCoarseDispatch::Active {
                selector: &selector,
                workgroups: 1,
                path_tile_bounds: Some(bounds),
            }),
            true,
            true,
        );
        let (uniform, bytes) = recording
            .commands
            .iter()
            .find_map(|command| match command {
                Command::UploadUniform(buffer, bytes)
                    if buffer.name == "vello.sparse_tile_bounds" =>
                {
                    Some((*buffer, bytes.as_slice()))
                }
                _ => None,
            })
            .expect("selected bounds need one dedicated uniform");
        assert_eq!(bytes, bytemuck::bytes_of(&bounds));
        assert_eq!(uniform.size, 16);
        let dispatch = recording
            .commands
            .iter()
            .enumerate()
            .find_map(|(ix, command)| match command {
                Command::Dispatch(ShaderId(8), _, bindings) => Some((ix, bindings)),
                _ => None,
            })
            .expect("selected bounds need the allocation permutation");
        assert_eq!(dispatch.1.len(), 7);
        assert!(dispatch.1[6].as_buf().unwrap().id == uniform.id);
        let free = recording
            .commands
            .iter()
            .position(
                |command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == uniform.id),
            )
            .expect("bounds uniform must be released");
        assert!(free > dispatch.0);
    }

    #[test]
    fn sparse_tile_bounds_leaves_dense_legacy_and_missing_permutations_untouched() {
        let selector = vec![0; 2_048];
        let active = Some(SparseCoarseDispatch::Active {
            selector: &selector,
            workgroups: 1,
            path_tile_bounds: Some([1, 2, 5, 6]),
        });
        let cases = [
            (None, true, true),
            (Some(SparseCoarseDispatch::Legacy(&selector)), true, true),
            (active, false, true),
            (active, true, false),
        ];
        for (dispatch, active_shader, allocation_shader) in cases {
            let recording =
                sparse_tile_bounds_recording(dispatch, active_shader, allocation_shader);
            assert!(!recording.commands.iter().any(|command| {
                matches!(command, Command::UploadUniform(buffer, _) if buffer.name == "vello.sparse_tile_bounds")
            }));
            assert!(
                !recording
                    .commands
                    .iter()
                    .any(|command| { matches!(command, Command::Dispatch(ShaderId(8), _, _)) })
            );
        }
    }

    #[test]
    fn sparse_tile_bounds_refuses_zero_area_offscreen_and_full_recording_bounds() {
        let selector = vec![0; 2_048];
        for bounds in [[4, 4, 4, 5], [18, 0, 19, 1], [0, 0, 17, 17]] {
            let recording = sparse_tile_bounds_recording(
                Some(SparseCoarseDispatch::Active {
                    selector: &selector,
                    workgroups: 1,
                    path_tile_bounds: Some(bounds),
                }),
                true,
                true,
            );
            assert!(!recording.commands.iter().any(|command| {
                matches!(command, Command::UploadUniform(buffer, _) if buffer.name == "vello.sparse_tile_bounds")
            }));
            assert!(
                !recording
                    .commands
                    .iter()
                    .any(|command| { matches!(command, Command::Dispatch(ShaderId(8), _, _)) })
            );
        }
    }

    #[test]
    fn sparse_coarse_active_records_native_bin_list_grid() {
        let mut selector = vec![0; 2_048];
        // The 257x257-pixel target uses ten selector words and four native
        // bins. Its list names two disconnected native bins.
        selector[0] = 1;
        selector[10] = 0;
        selector[11] = 3;
        assert_eq!(
            sparse_coarse_dispatches(
                Some(SparseCoarseDispatch::Active {
                    selector: &selector,
                    workgroups: 2,
                    path_tile_bounds: None,
                }),
                257,
                257,
                true
            ),
            vec![(4, (2, 1, 1))]
        );
    }

    #[test]
    fn sparse_coarse_active_falls_back_to_legacy_full_native_bin_grid() {
        let mut selector = vec![0; 2_048];
        selector[0] = 1;
        selector[10] = 0;
        selector[11] = 3;
        assert_eq!(
            sparse_coarse_dispatches(
                Some(SparseCoarseDispatch::Active {
                    selector: &selector,
                    workgroups: 2,
                    path_tile_bounds: None,
                }),
                257,
                257,
                false,
            ),
            vec![(3, (2, 2, 1))]
        );
    }

    #[test]
    fn sparse_coarse_dense_fallback_records_no_sparse_shader() {
        let mut selector = Vec::new();
        let mut records = vec![0; 18];
        records[9] = 16 * 32;
        let selection =
            crate::sparse_coarse_adaptive_selection(&mut selector, &records, 8_448, 256);
        assert!(!selection.active);
        assert!(selection.bins.is_some());
        assert_eq!(
            sparse_coarse_dispatches(None, 257, 257, true),
            vec![(5, (2, 2, 1))]
        );
    }

    fn shared_coarse_pair_recording(
        defer: bool,
        geometry_download: bool,
    ) -> (Recording, Option<(BufferProxy, BufferProxy)>) {
        let shaders = direct_shaders(128);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            true,
            false,
            None,
            defer,
            None,
        );
        let bump = render.bump_buf();
        let (_, _, downloads) =
            render.record_fine_area_pair(&shaders, &mut recording, &[0; 9], geometry_download);
        if defer {
            finish_bump_readback(&mut recording, bump, true);
        }
        (recording, downloads)
    }

    #[test]
    fn shared_coarse_ppt8_pair_records_distinct_p4_and_ppt8_shaders() {
        let shaders = direct_shaders(128);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 17,
                height: 17,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            None,
            false,
            None,
        );
        render.record_fine_area_ppt8_pair(&shaders, &mut recording, &[0; 9]);
        let fine_dispatches = recording
            .commands
            .iter()
            .filter_map(|command| match command {
                Command::Dispatch(shader, _, bindings)
                    if *shader == ShaderId(6)
                        || *shader == ShaderId(7)
                        || *shader == ShaderId(23)
                        || *shader == ShaderId(24) =>
                {
                    Some((*shader, bindings))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(fine_dispatches.len(), 4);
        assert!(fine_dispatches[0].0 == ShaderId(6));
        assert!(fine_dispatches[1].0 == ShaderId(23));
        assert!(fine_dispatches[2].0 == ShaderId(7));
        assert!(fine_dispatches[3].0 == ShaderId(24));
        let resource_id = |resource: ResourceProxy| match resource {
            ResourceProxy::Buffer(proxy) | ResourceProxy::BufferRange { proxy, .. } => proxy.id,
            ResourceProxy::Image(proxy) => proxy.id,
        };
        for binding in [0, 1, 2, 3, 6, 7] {
            let p4 = resource_id(fine_dispatches[0].1[binding]);
            let p8 = resource_id(fine_dispatches[1].1[binding]);
            assert!(p4 == p8, "full P4/PPT8 binding {binding} must be shared");
        }
        for binding in [0, 1, 2, 3, 6, 7, 8] {
            let p4 = resource_id(fine_dispatches[2].1[binding]);
            let p8 = resource_id(fine_dispatches[3].1[binding]);
            assert!(p4 == p8, "sparse P4/PPT8 binding {binding} must be shared");
        }
        assert!(resource_id(fine_dispatches[0].1[4]) != resource_id(fine_dispatches[1].1[4]));
        assert!(resource_id(fine_dispatches[0].1[5]) != resource_id(fine_dispatches[1].1[5]));
        assert!(resource_id(fine_dispatches[2].1[4]) != resource_id(fine_dispatches[3].1[4]));
        assert!(resource_id(fine_dispatches[2].1[5]) != resource_id(fine_dispatches[3].1[5]));
    }

    #[test]
    fn segment_order_replay_records_guarded_patches_before_shared_ordinary_fines() {
        let shaders = direct_shaders(128);
        let mut render = Render::new();
        let mut resolver = Resolver::new();
        let mut recording = Recording::default();
        let mut packed = Vec::new();
        let mut image_atlas = None;
        render.render_encoding_coarse_with_dispatch(
            &Encoding::default(),
            &mut resolver,
            &shaders,
            &mut image_atlas,
            &mut recording,
            &mut packed,
            &RenderParams {
                base_color: peniko::Color::TRANSPARENT,
                width: 257,
                height: 257,
                antialiasing_method: AaConfig::Area,
            },
            &vello_encoding::BumpBudget::INITIAL,
            false,
            false,
            None,
            false,
            None,
        );
        let expected = recording.upload("replay.expected", vec![0; 980]);
        let left = recording.upload("replay.left", vec![0; 980]);
        let right = recording.upload("replay.right", vec![0; 980]);
        let control = recording.upload_uniform("replay.control", vec![0; 32]);
        let left_witness = recording.upload("replay.left_witness", vec![0; 1000]);
        let right_witness = recording.upload("replay.right_witness", vec![0; 1000]);
        let (_, _, witnesses) = render.record_fine_area_order_replay(
            &shaders,
            &mut recording,
            ShaderId(99),
            [expected, left, control, left_witness],
            [expected, right, control, right_witness],
        );
        let dispatches = recording
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| match command {
                Command::Dispatch(shader, _, bindings)
                    if *shader == ShaderId(99) || *shader == ShaderId(6) =>
                {
                    Some((index, *shader, bindings))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(dispatches.len(), 4);
        assert!(dispatches[0].1 == ShaderId(99));
        assert!(dispatches[1].1 == ShaderId(6));
        assert!(dispatches[2].1 == ShaderId(99));
        assert!(dispatches[3].1 == ShaderId(6));
        let left_fine = dispatches[1].2;
        let right_fine = dispatches[3].2;
        let left_patch = dispatches[0].2;
        let right_patch = dispatches[2].2;
        assert!(left_patch[0].as_buf().unwrap().id == left_fine[2].as_buf().unwrap().id);
        assert!(right_patch[0].as_buf().unwrap().id == right_fine[2].as_buf().unwrap().id);
        assert!(left_patch[1].as_buf().unwrap().id == left_fine[1].as_buf().unwrap().id);
        assert!(right_patch[1].as_buf().unwrap().id == right_fine[1].as_buf().unwrap().id);
        assert!(left_patch[2].as_buf().unwrap().id == right_patch[2].as_buf().unwrap().id);
        assert!(left_patch[4].as_buf().unwrap().id == right_patch[4].as_buf().unwrap().id);
        assert!(left_patch[3].as_buf().unwrap().id != right_patch[3].as_buf().unwrap().id);
        assert!(left_patch[5].as_buf().unwrap().id != right_patch[5].as_buf().unwrap().id);
        for binding in [0, 1, 2, 3] {
            assert!(
                left_fine[binding].as_buf().unwrap().id == right_fine[binding].as_buf().unwrap().id
            );
        }
        for binding in [6, 7] {
            assert!(
                left_fine[binding].as_image().unwrap().id
                    == right_fine[binding].as_image().unwrap().id
            );
        }
        assert!(left_fine[4].as_buf().unwrap().id != right_fine[4].as_buf().unwrap().id);
        assert!(left_fine[5].as_image().unwrap().id != right_fine[5].as_image().unwrap().id);
        let downloads = witnesses.map(|buffer| {
            recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::Download(candidate) if candidate.id == buffer.id))
                .expect("witness download")
        });
        assert!(downloads.iter().all(|index| *index > dispatches[3].0));
        for (buffer, download) in witnesses.into_iter().zip(downloads) {
            let free = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::FreeBuffer(candidate) if candidate.id == buffer.id))
                .expect("witness free");
            assert!(free > download);
        }
        let coarse = recording
            .commands
            .iter()
            .enumerate()
            .filter(|(_, command)| matches!(command, Command::Dispatch(ShaderId(5), _, _)))
            .count();
        assert_eq!(coarse, 1);
        for binding in [&left_fine[0], &left_fine[1], &left_fine[2], &left_fine[3]] {
            let id = binding.as_buf().unwrap().id;
            let free = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::FreeBuffer(candidate) if candidate.id == id))
                .expect("shared fine resource free");
            assert!(free > dispatches[3].0);
        }
    }

    #[test]
    fn shared_coarse_pair_records_shared_inputs_and_delayed_frees() {
        for defer in [false, true] {
            let (recording, downloads) = shared_coarse_pair_recording(defer, false);
            assert!(downloads.is_none());
            assert_eq!(
                recording
                    .commands
                    .iter()
                    .filter(|command| matches!(command, Command::Dispatch(ShaderId(5), _, _)))
                    .count(),
                1
            );
            assert!(!recording.commands.iter().any(|command| {
                matches!(command, Command::Dispatch(ShaderId(3) | ShaderId(4), _, _))
            }));
            let dispatches = recording
                .commands
                .iter()
                .enumerate()
                .filter_map(|(ix, command)| {
                    if let Command::Dispatch(shader, _, bindings) = command
                        && (*shader == ShaderId(6) || *shader == ShaderId(7))
                    {
                        Some((ix, *shader, bindings))
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(dispatches.len(), 2);
            let (full_ix, full_shader, full) = dispatches[0];
            let (sparse_ix, sparse_shader, sparse) = dispatches[1];
            assert!(full_shader == ShaderId(6));
            assert!(sparse_shader == ShaderId(7));
            assert_eq!(full_ix + 1, sparse_ix);
            assert_eq!(full.len(), 8);
            assert_eq!(sparse.len(), 9);
            for ix in [0, 1, 2, 3] {
                assert!(full[ix].as_buf().unwrap().id == sparse[ix].as_buf().unwrap().id);
            }
            assert!(full[6].as_image().unwrap().id == sparse[6].as_image().unwrap().id);
            assert!(full[7].as_image().unwrap().id == sparse[7].as_image().unwrap().id);
            assert!(full[4].as_buf().unwrap().id != sparse[4].as_buf().unwrap().id);
            assert!(full[5].as_image().unwrap().id != sparse[5].as_image().unwrap().id);
            assert_eq!(recording.commands.iter().filter(|command| matches!(command, Command::Upload(buffer, _) if buffer.name == "vello.shared_coarse_damage_tiles")).count(), 1);
            for binding in [&full[0], &full[1], &full[2], &full[3]] {
                let id = binding.as_buf().unwrap().id;
                let free_ix = recording
                    .commands
                    .iter()
                    .position(
                        |command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == id),
                    )
                    .unwrap();
                assert!(free_ix > sparse_ix);
            }
            let gradient = full[6].as_image().unwrap().id;
            let gradient_free = recording
                .commands
                .iter()
                .position(
                    |command| matches!(command, Command::FreeImage(image) if image.id == gradient),
                )
                .unwrap();
            assert!(gradient_free > sparse_ix);
            let bump_download = recording
                .commands
                .iter()
                .position(|command| matches!(command, Command::Download(_)))
                .unwrap();
            let bump = match &recording.commands[bump_download] {
                Command::Download(buffer) => buffer.id,
                _ => unreachable!(),
            };
            assert_eq!(
                recording
                    .commands
                    .iter()
                    .filter(
                        |command| matches!(command, Command::Download(buffer) if buffer.id == bump)
                    )
                    .count(),
                1
            );
            assert_eq!(
                recording
                    .commands
                    .iter()
                    .filter(|command| matches!(command, Command::Download(_)))
                    .count(),
                1,
                "the normal paired path must not request geometry downloads"
            );
            let bump_free = recording
                .commands
                .iter()
                .position(
                    |command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == bump),
                )
                .unwrap();
            assert_eq!(recording.commands.iter().filter(|command| matches!(command, Command::FreeBuffer(buffer) if buffer.id == bump)).count(), 1);
            assert!(bump_free > bump_download);
            assert!(if defer {
                bump_download > sparse_ix
            } else {
                bump_download < full_ix
            });
        }
    }

    #[test]
    fn shared_coarse_geometry_downloads_follow_both_fine_passes_before_frees() {
        let (recording, downloads) = shared_coarse_pair_recording(false, true);
        let (ptcl, segments) = downloads.expect("geometry capture must request both downloads");
        let fine_dispatches = recording
            .commands
            .iter()
            .enumerate()
            .filter_map(|(ix, command)| match command {
                Command::Dispatch(ShaderId(6) | ShaderId(7), _, _) => Some(ix),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(fine_dispatches.len(), 2);
        let ptcl_download = recording
            .commands
            .iter()
            .position(
                |command| matches!(command, Command::Download(buffer) if buffer.id == ptcl.id),
            )
            .unwrap();
        let segments_download = recording
            .commands
            .iter()
            .position(
                |command| matches!(command, Command::Download(buffer) if buffer.id == segments.id),
            )
            .unwrap();
        for download in [ptcl_download, segments_download] {
            assert!(download > fine_dispatches[1]);
        }
        assert_eq!(segments_download, ptcl_download + 1);
        for buffer in [ptcl, segments] {
            let free = recording
                .commands
                .iter()
                .position(
                    |command| matches!(command, Command::FreeBuffer(candidate) if candidate.id == buffer.id),
                )
                .unwrap();
            assert!(free > segments_download);
        }
    }

    #[test]
    fn finish_bump_readback_appends_download_then_free() {
        let bump = BufferProxy::new(16, "test.bump");
        let mut recording = Recording::default();
        // The helper preserves the ordering after its existing preceding commands.
        recording.clear_all(bump);
        finish_bump_readback(&mut recording, bump, true);

        assert!(matches!(
            recording.commands.first(),
            Some(Command::Clear(..))
        ));
        assert!(
            matches!(recording.commands.get(1), Some(Command::Download(buf)) if buf.id == bump.id)
        );
        assert!(
            matches!(recording.commands.get(2), Some(Command::FreeBuffer(buf)) if buf.id == bump.id)
        );
    }

    #[test]
    fn finish_unmeasured_bump_readback_only_frees() {
        let bump = BufferProxy::new(16, "test.bump");
        let mut recording = Recording::default();
        finish_bump_readback(&mut recording, bump, false);

        assert!(
            matches!(recording.commands.as_slice(), [Command::FreeBuffer(buf)] if buf.id == bump.id)
        );
    }

    #[test]
    fn zero_gradient_is_stable_and_not_freed_with_the_frame() {
        let zero_gradient = ImageProxy::new(1, 1, ImageFormat::Rgba8);
        let (gradient, is_transient) = no_ramp_gradient_image(Some(zero_gradient));
        assert!(!is_transient);
        assert!(gradient.as_image().unwrap().id == zero_gradient.id);

        let mut recording = Recording::default();
        free_transient_gradient(&mut recording, gradient, is_transient);
        assert!(recording.commands.is_empty());
    }

    #[test]
    fn disabled_zero_gradient_reuse_keeps_the_transient_lifetime() {
        let (gradient, is_transient) = no_ramp_gradient_image(None);
        assert!(is_transient);

        let mut recording = Recording::default();
        free_transient_gradient(&mut recording, gradient, is_transient);
        assert!(
            matches!(recording.commands.as_slice(), [Command::FreeImage(image)] if image.id == gradient.as_image().unwrap().id)
        );
    }
}
