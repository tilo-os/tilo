// Copyright 2022 the Vello Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Load rendering shaders.

#[cfg(feature = "wgpu")]
use wgpu::Device;

use crate::ShaderId;

#[cfg(feature = "wgpu")]
use crate::{
    Error, RendererOptions,
    recording::{BindType, ImageFormat},
    wgpu_engine::WgpuEngine,
};

// Shaders for the full pipeline
pub struct FullShaders {
    pub pathtag_reduce: ShaderId,
    pub pathtag_reduce2: ShaderId,
    pub pathtag_scan1: ShaderId,
    pub pathtag_scan: ShaderId,
    pub pathtag_scan_large: ShaderId,
    pub bbox_clear: ShaderId,
    pub flatten: ShaderId,
    pub draw_reduce: ShaderId,
    pub draw_leaf: ShaderId,
    pub draw_prefix: ShaderId,
    pub draw_leaf_prefix: ShaderId,
    pub clip_reduce: ShaderId,
    pub clip_leaf: ShaderId,
    /// Single-workgroup GPU clip leaf that omits only the empty cross-group prefix.
    pub clip_leaf_single_group: Option<ShaderId>,
    pub binning: ShaderId,
    pub tile_alloc: ShaderId,
    pub tile_alloc_sparse: Option<ShaderId>,
    pub backdrop: ShaderId,
    pub path_count_setup: ShaderId,
    pub path_count: ShaderId,
    pub path_count_direct: ShaderId,
    pub coarse: ShaderId,
    pub coarse_sparse: Option<ShaderId>,
    pub coarse_sparse_active: Option<ShaderId>,
    pub coarse_opaque_prune: Option<ShaderId>,
    pub coarse_sparse_opaque_prune: Option<ShaderId>,
    pub coarse_sparse_active_opaque_prune: Option<ShaderId>,
    pub coarse_solid_fuse: Option<ShaderId>,
    pub coarse_sparse_solid_fuse: Option<ShaderId>,
    pub coarse_sparse_active_solid_fuse: Option<ShaderId>,
    pub path_tiling_setup: ShaderId,
    pub path_tiling: ShaderId,
    pub path_tiling_direct: ShaderId,
    pub path_tiling_segment_row_mask: Option<ShaderId>,
    pub path_tiling_direct_segment_row_mask: Option<ShaderId>,
    pub fine_area: Option<ShaderId>,
    pub fine_area_ppt8: Option<ShaderId>,
    pub fine_msaa8: Option<ShaderId>,
    pub fine_msaa16: Option<ShaderId>,
    pub fine_sparse_area: Option<ShaderId>,
    pub fine_sparse_area_ppt8: Option<ShaderId>,
    pub fine_sparse_area_quad_skip: Option<ShaderId>,
    pub fine_sparse_area_solid_fuse: Option<ShaderId>,
    pub fine_sparse_area_segment_row_mask: Option<ShaderId>,
    pub canonical_segment_order_full: Option<ShaderId>,
    pub canonical_segment_order_sparse: Option<ShaderId>,
    pub canonical_segment_order_sparse_solid_fuse: Option<ShaderId>,
    // 2-level dispatch works for CPU pathtag scan even for large
    // inputs, 3-level is not yet implemented.
    pub pathtag_is_cpu: bool,
    /// Selective CPU bbox/draw metadata must run before GPU flatten.
    pub cpu_metadata_enabled: bool,
    /// Bounded selective CPU draw-prefix with a GPU leaf permutation.
    pub cpu_draw_prefix_enabled: bool,
    /// Fixed-grid GPU path-count/tiling replaces indirect setup when enabled.
    pub direct_path_dispatch_enabled: bool,
    /// Initialization-time fixed grid shared by both direct path stages.
    pub direct_path_workgroups: u32,
}

#[cfg(feature = "wgpu")]
pub(crate) fn full_shaders(
    device: &Device,
    engine: &mut WgpuEngine,
    options: &RendererOptions,
) -> Result<FullShaders, Error> {
    use crate::wgpu_engine::CpuShaderType;
    use BindType::*;

    let mut force_gpu = false;
    let force_gpu_from: Option<&str> = None;
    // Uncomment this to force use of GPU shaders from the specified shader and later even
    // if `engine.use_cpu` is specified.
    //let force_gpu_from = Some("binning");

    #[cfg(feature = "hot_reload")]
    let mut shaders = vello_shaders::compile::ShaderInfo::from_default()?;
    #[cfg(not(feature = "hot_reload"))]
    let shaders = vello_shaders::SHADERS;

    macro_rules! add_shader {
        ($name:ident, $label:expr, $bindings:expr, $cpu:expr) => {{
            if force_gpu_from == Some(stringify!($name)) {
                force_gpu = true;
            }
            #[cfg(feature = "hot_reload")]
            let source = shaders
                .remove(stringify!($name))
                .expect(stringify!($name))
                .source
                .into();
            #[cfg(not(feature = "hot_reload"))]
            let source = shaders.$name.wgsl.code;
            engine.add_compute_shader(
                device,
                concat!("vello.", $label),
                source,
                &$bindings,
                if force_gpu {
                    CpuShaderType::Missing
                } else {
                    $cpu
                },
            )
        }};
        ($name:ident, $bindings:expr, $cpu:expr) => {{ add_shader!($name, stringify!($name), $bindings, $cpu) }};
        ($name:ident, $bindings:expr) => {
            add_shader!(
                $name,
                $bindings,
                CpuShaderType::Present(vello_shaders::cpu::$name)
            )
        };
    }

    // Registrations a full frame on the default path never dispatches defer
    // their pipeline under `PipelineDeferral::Unused` (Tilo patch): the
    // three-level path-tag scan (large scenes only), the GPU draw reduce and
    // leaf while the CPU draw prefix serves, the indirect path stages while
    // the direct ones serve, the clip reduce (many clips only) and the sparse
    // fine stage (damage-limited repaints).
    macro_rules! unused {
        ($when:expr, $registration:expr) => {{
            engine.defer_unused($when);
            let id = $registration;
            engine.defer_unused(false);
            id
        }};
    }
    let direct_paths = engine.direct_path_dispatch_enabled() && !options.use_cpu;
    let draw_prefix = engine.cpu_draw_prefix_enabled() && !options.use_cpu;

    let pathtag_reduce = add_shader!(pathtag_reduce, [Uniform, BufReadOnly, Buffer]);
    let pathtag_reduce2 = unused!(
        true,
        add_shader!(
            pathtag_reduce2,
            [BufReadOnly, Buffer],
            CpuShaderType::Skipped
        )
    );
    let pathtag_scan1 = unused!(
        true,
        add_shader!(
            pathtag_scan1,
            [BufReadOnly, BufReadOnly, Buffer],
            CpuShaderType::Skipped
        )
    );
    let pathtag_scan = add_shader!(
        pathtag_scan_small,
        [Uniform, BufReadOnly, BufReadOnly, Buffer],
        CpuShaderType::Present(vello_shaders::cpu::pathtag_scan)
    );
    let pathtag_scan_large = unused!(
        true,
        add_shader!(
            pathtag_scan_large,
            [Uniform, BufReadOnly, BufReadOnly, Buffer],
            CpuShaderType::Skipped
        )
    );
    let bbox_clear = add_shader!(bbox_clear, [Uniform, Buffer]);
    let flatten = add_shader!(
        flatten,
        [Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer, Buffer]
    );
    let draw_reduce = unused!(
        draw_prefix,
        add_shader!(draw_reduce, [Uniform, BufReadOnly, Buffer])
    );
    let draw_leaf = unused!(
        draw_prefix,
        add_shader!(
            draw_leaf,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
            ]
        )
    );
    let draw_prefix = engine.add_cpu_shader("vello.draw_prefix", vello_shaders::cpu::draw_prefix);
    let draw_leaf_prefix = add_shader!(
        draw_leaf_prefix,
        [Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer, Buffer,],
        CpuShaderType::Missing
    );
    let clip_reduce = unused!(
        true,
        add_shader!(clip_reduce, [BufReadOnly, BufReadOnly, Buffer, Buffer])
    );
    let clip_leaf = add_shader!(
        clip_leaf,
        [
            Uniform,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            Buffer,
            Buffer,
        ]
    );
    let clip_leaf_single_group =
        (engine.clip_single_group_enabled() && !options.use_cpu).then(|| {
            add_shader!(
                clip_leaf_single_group,
                [
                    Uniform,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                ],
                CpuShaderType::Missing
            )
        });
    let binning = add_shader!(
        binning,
        [
            Uniform,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            Buffer,
            Buffer,
            Buffer,
            Buffer,
        ]
    );
    let tile_alloc = add_shader!(
        tile_alloc,
        [Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer, Buffer]
    );
    let tile_alloc_sparse = if engine.sparse_tile_bounds_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area
    {
        Some(add_shader!(
            tile_alloc_sparse,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
                Uniform
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let path_count_setup = unused!(
        direct_paths,
        add_shader!(path_count_setup, [Buffer, Buffer])
    );
    let path_count = unused!(
        direct_paths,
        add_shader!(
            path_count,
            [Uniform, Buffer, BufReadOnly, BufReadOnly, Buffer, Buffer]
        )
    );
    let path_count_direct = add_shader!(
        path_count_direct,
        [Uniform, Buffer, BufReadOnly, BufReadOnly, Buffer, Buffer],
        CpuShaderType::Missing
    );
    let backdrop = add_shader!(
        backdrop_dyn,
        [Uniform, Buffer, BufReadOnly, Buffer],
        CpuShaderType::Present(vello_shaders::cpu::backdrop)
    );
    let coarse = add_shader!(
        coarse,
        [
            Uniform,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            Buffer,
            Buffer,
            Buffer,
        ]
    );
    let coarse_sparse = if engine.sparse_fine_enabled()
        && engine.sparse_coarse_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area
    {
        Some(add_shader!(
            coarse_sparse,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
                Uniform,
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let coarse_sparse_active = if engine.sparse_fine_enabled()
        && engine.sparse_coarse_active_bins_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area
    {
        Some(add_shader!(
            coarse_sparse_active,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
                Uniform,
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let sparse_opaque_prune = engine.sparse_opaque_prune_enabled()
        && engine.sparse_fine_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area;
    let coarse_opaque_prune = sparse_opaque_prune.then(|| {
        add_shader!(
            coarse_opaque_prune,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
            ],
            CpuShaderType::Missing
        )
    });
    let coarse_sparse_opaque_prune =
        (sparse_opaque_prune && engine.sparse_coarse_enabled()).then(|| {
            add_shader!(
                coarse_sparse_opaque_prune,
                [
                    Uniform,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                    Buffer,
                    Uniform,
                ],
                CpuShaderType::Missing
            )
        });
    let coarse_sparse_active_opaque_prune =
        (sparse_opaque_prune && engine.sparse_coarse_active_bins_enabled()).then(|| {
            add_shader!(
                coarse_sparse_active_opaque_prune,
                [
                    Uniform,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                    Buffer,
                    Uniform,
                ],
                CpuShaderType::Missing
            )
        });
    let sparse_solid_fuse = engine.sparse_solid_fuse_enabled()
        && engine.sparse_fine_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area;
    let coarse_solid_fuse = sparse_solid_fuse.then(|| {
        add_shader!(
            coarse_solid_fuse,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Buffer,
                Buffer,
            ],
            CpuShaderType::Missing
        )
    });
    let coarse_sparse_solid_fuse =
        (sparse_solid_fuse && engine.sparse_coarse_enabled()).then(|| {
            add_shader!(
                coarse_sparse_solid_fuse,
                [
                    Uniform,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                    Buffer,
                    Uniform,
                ],
                CpuShaderType::Missing
            )
        });
    let coarse_sparse_active_solid_fuse =
        (sparse_solid_fuse && engine.sparse_coarse_active_bins_enabled()).then(|| {
            add_shader!(
                coarse_sparse_active_solid_fuse,
                [
                    Uniform,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                    Buffer,
                    Uniform,
                ],
                CpuShaderType::Missing
            )
        });
    let path_tiling_setup = unused!(
        direct_paths,
        add_shader!(path_tiling_setup, [Buffer, Buffer, Buffer])
    );
    let path_tiling = unused!(
        direct_paths,
        add_shader!(
            path_tiling,
            [
                Buffer,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
            ]
        )
    );
    let path_tiling_direct = add_shader!(
        path_tiling_direct,
        [
            Buffer,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            Buffer,
            Buffer
        ],
        CpuShaderType::Missing
    );
    let sparse_segment_row_mask = engine.sparse_segment_row_mask_enabled()
        && engine.sparse_fine_enabled()
        && !options.use_cpu
        && options.antialiasing_support.area;
    let path_tiling_segment_row_mask =
        (!engine.direct_path_dispatch_enabled() && sparse_segment_row_mask).then(|| {
            add_shader!(
                path_tiling_segment_row_mask,
                [
                    Buffer,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                ],
                CpuShaderType::Missing
            )
        });
    let path_tiling_direct_segment_row_mask =
        (engine.direct_path_dispatch_enabled() && sparse_segment_row_mask).then(|| {
            add_shader!(
                path_tiling_direct_segment_row_mask,
                [
                    Buffer,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    BufReadOnly,
                    Buffer,
                    Buffer,
                ],
                CpuShaderType::Missing
            )
        });
    let fine_resources = [
        Uniform,
        BufReadOnly,
        BufReadOnly,
        BufReadOnly,
        Buffer,
        Image(ImageFormat::Rgba8),
        ImageRead(ImageFormat::Rgba8),
        ImageRead(ImageFormat::Rgba8),
        // Mask LUT buffer, used only when MSAA is enabled.
        BufReadOnly,
    ];

    let aa_support = &options.antialiasing_support;
    let fine_area = if aa_support.area {
        Some(add_shader!(
            fine_area,
            fine_resources[..fine_resources.len() - 1],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let area_ppt8 = engine.area_ppt8_enabled() && !options.use_cpu && aa_support.area;
    let fine_area_ppt8 = area_ppt8.then(|| {
        add_shader!(
            fine_area_ppt8,
            fine_resources[..fine_resources.len() - 1],
            CpuShaderType::Missing
        )
    });
    let fine_msaa8 = if aa_support.msaa8 {
        Some(add_shader!(
            fine_msaa8,
            fine_resources,
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let fine_msaa16 = if aa_support.msaa16 {
        Some(add_shader!(
            fine_msaa16,
            fine_resources,
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let fine_sparse_area = if engine.sparse_fine_enabled() && !options.use_cpu && aa_support.area {
        Some(unused!(true, add_shader!(
            fine_sparse_area,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Image(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                BufReadOnly,
            ],
            CpuShaderType::Missing
        )))
    } else {
        None
    };
    let fine_sparse_area_ppt8 = (area_ppt8 && engine.sparse_fine_enabled()).then(|| {
        add_shader!(
            fine_sparse_area_ppt8,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Image(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                BufReadOnly,
            ],
            CpuShaderType::Missing
        )
    });
    let fine_sparse_area_quad_skip = if engine.sparse_fine_quad_skip_enabled()
        && engine.sparse_fine_enabled()
        && !options.use_cpu
        && aa_support.area
    {
        Some(add_shader!(
            fine_sparse_area_quad_skip,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Image(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                BufReadOnly,
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let fine_sparse_area_solid_fuse = if sparse_solid_fuse {
        Some(add_shader!(
            fine_sparse_area_solid_fuse,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Image(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                BufReadOnly,
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let fine_sparse_area_segment_row_mask = if sparse_segment_row_mask {
        Some(add_shader!(
            fine_sparse_area_segment_row_mask,
            [
                Uniform,
                BufReadOnly,
                BufReadOnly,
                BufReadOnly,
                Buffer,
                Image(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                ImageRead(ImageFormat::Rgba8),
                BufReadOnly,
            ],
            CpuShaderType::Missing
        ))
    } else {
        None
    };
    let canonical_segment_order =
        engine.canonical_segment_order_enabled() && !options.use_cpu && aa_support.area;
    let canonical_segment_order_full = canonical_segment_order.then(|| {
        add_shader!(
            canonical_segment_order_full,
            [Uniform, Buffer, Buffer, Buffer],
            CpuShaderType::Missing
        )
    });
    let canonical_segment_order_sparse = canonical_segment_order.then(|| {
        add_shader!(
            canonical_segment_order_sparse,
            [Uniform, Buffer, Buffer, Buffer, BufReadOnly],
            CpuShaderType::Missing
        )
    });
    let canonical_segment_order_sparse_solid_fuse = (canonical_segment_order && sparse_solid_fuse)
        .then(|| {
            add_shader!(
                canonical_segment_order_sparse_solid_fuse,
                [Uniform, Buffer, Buffer, Buffer, BufReadOnly],
                CpuShaderType::Missing
            )
        });

    Ok(FullShaders {
        pathtag_reduce,
        pathtag_reduce2,
        pathtag_scan,
        pathtag_scan1,
        pathtag_scan_large,
        bbox_clear,
        flatten,
        draw_reduce,
        draw_leaf,
        draw_prefix,
        draw_leaf_prefix,
        clip_reduce,
        clip_leaf,
        clip_leaf_single_group,
        binning,
        tile_alloc,
        tile_alloc_sparse,
        path_count_setup,
        path_count,
        path_count_direct,
        backdrop,
        coarse,
        coarse_sparse,
        coarse_sparse_active,
        coarse_opaque_prune,
        coarse_sparse_opaque_prune,
        coarse_sparse_active_opaque_prune,
        coarse_solid_fuse,
        coarse_sparse_solid_fuse,
        coarse_sparse_active_solid_fuse,
        path_tiling_setup,
        path_tiling,
        path_tiling_direct,
        path_tiling_segment_row_mask,
        path_tiling_direct_segment_row_mask,
        fine_area,
        fine_area_ppt8,
        fine_msaa8,
        fine_msaa16,
        fine_sparse_area,
        fine_sparse_area_ppt8,
        fine_sparse_area_quad_skip,
        fine_sparse_area_solid_fuse,
        fine_sparse_area_segment_row_mask,
        canonical_segment_order_full,
        canonical_segment_order_sparse,
        canonical_segment_order_sparse_solid_fuse,
        pathtag_is_cpu: options.use_cpu || engine.cpu_pathtag_enabled(),
        cpu_metadata_enabled: !options.use_cpu && engine.cpu_metadata_enabled(),
        cpu_draw_prefix_enabled: !options.use_cpu && engine.cpu_draw_prefix_enabled(),
        direct_path_dispatch_enabled: !options.use_cpu && engine.direct_path_dispatch_enabled(),
        direct_path_workgroups: engine.direct_path_workgroups(),
    })
}
