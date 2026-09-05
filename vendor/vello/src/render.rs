// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Take an encoded scene and create a graph to render it

use crate::recording::{BufferProxy, ImageFormat, ImageProxy, Recording, ResourceProxy};
use crate::shaders::FullShaders;
use crate::{AaConfig, RenderParams};

#[cfg(feature = "wgpu")]
use crate::Scene;

use vello_encoding::{Encoding, Resolver, WorkgroupSize, make_mask_lut, make_mask_lut_16};

#[derive(Clone, Copy, Debug)]
enum AtlasProxyAction {
    Created,
    Reused,
    Resized,
}

/// State for a render in progress.
pub struct Render {
    fine_wg_count: Option<WorkgroupSize>,
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
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>) {
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
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>) {
    let mut render = Render::new();
    render.render_encoding_coarse(
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
    render.record_fine(shaders, recording);
    if defer_bump_readback {
        finish_bump_readback(recording, bump, measure);
    }
    (out_image.into(), bump_buf)
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
    coarse_tiles: Option<&[u32]>,
    defer_bump_readback: bool,
    zero_gradient: Option<ImageProxy>,
) -> (ResourceProxy, Option<BufferProxy>) {
    let mut render = Render::new();
    render.render_encoding_coarse(
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
        defer_bump_readback,
        zero_gradient,
    );
    let out_image = render.out_image();
    let bump = render.bump_buf();
    let bump_buf = measure.then_some(bump);
    render.record_fine_sparse(shaders, recording, damage_tiles, external_damage_tiles);
    if defer_bump_readback {
        finish_bump_readback(recording, bump, measure);
    }
    (out_image.into(), bump_buf)
}

/// Finish the bump lifetime at the caller-selected recording point.
///
/// The fine shaders do not bind the bump allocator. Deferring the copy is an
/// opt-in command-order experiment: it preserves the same counters while
/// allowing a shared compute pass to include fine before the copy boundary.
#[cfg(feature = "wgpu")]
fn finish_bump_readback(recording: &mut Recording, bump_buf: BufferProxy, measure: bool) {
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
            fine_resources: None,
            mask_buf: None,
            #[cfg(feature = "debug_layers")]
            captured_buffers: None,
        }
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
        use vello_encoding::RenderConfig;
        recording.reset_reusing_scene_upload(packed);
        packed.clear();
        let coarse_tile_bits = coarse_tiles.map(|bits| {
            recording.upload_uniform(
                "vello.coarse_sparse_tiles",
                bytemuck::cast_slice(bits).to_vec(),
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
            recording.dispatch(
                shaders.clip_leaf,
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
                (128, 1, 1),
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
        if let (Some(shader), Some(bits)) = (shaders.coarse_sparse, coarse_tile_bits) {
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
                shaders.coarse,
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
                shaders.path_tiling_direct,
                (128, 1, 1),
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
                shaders.path_tiling,
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

    /// Run fine rasterization assuming the coarse phase succeeded.
    pub fn record_fine(&mut self, shaders: &FullShaders, recording: &mut Recording) {
        let fine_wg_count = self.fine_wg_count.take().unwrap();
        let fine = self.fine_resources.take().unwrap();
        match fine.aa_config {
            AaConfig::Area => {
                recording.dispatch(
                    shaders
                        .fine_area
                        .expect("shaders not configured to support AA mode: area"),
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
        let fine = self.fine_resources.take().unwrap();
        let tile_count = u32::try_from(damage_tiles.len() / 9).unwrap_or(0);
        let owns_damage_tiles = external_damage_tiles.is_none();
        let damage_tiles = external_damage_tiles.unwrap_or_else(|| {
            recording.upload(
                "vello.damage_tiles",
                bytemuck::cast_slice::<u32, u8>(damage_tiles).to_vec(),
            )
        });
        recording.dispatch(
            shaders
                .fine_sparse_area
                .expect("sparse fine shader was not configured"),
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

    #[cfg(feature = "debug_layers")]
    pub fn take_captured_buffers(&mut self) -> Option<CapturedBuffers> {
        self.captured_buffers.take()
    }
}

#[cfg(all(test, feature = "wgpu"))]
mod tests {
    use super::*;
    use crate::recording::Command;

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
