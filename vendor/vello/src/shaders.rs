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
    pub binning: ShaderId,
    pub tile_alloc: ShaderId,
    pub backdrop: ShaderId,
    pub path_count_setup: ShaderId,
    pub path_count: ShaderId,
    pub path_count_direct: ShaderId,
    pub coarse: ShaderId,
    pub coarse_sparse: Option<ShaderId>,
    pub path_tiling_setup: ShaderId,
    pub path_tiling: ShaderId,
    pub path_tiling_direct: ShaderId,
    pub fine_area: Option<ShaderId>,
    pub fine_msaa8: Option<ShaderId>,
    pub fine_msaa16: Option<ShaderId>,
    pub fine_sparse_area: Option<ShaderId>,
    // 2-level dispatch works for CPU pathtag scan even for large
    // inputs, 3-level is not yet implemented.
    pub pathtag_is_cpu: bool,
    /// Selective CPU bbox/draw metadata must run before GPU flatten.
    pub cpu_metadata_enabled: bool,
    /// Bounded selective CPU draw-prefix with a GPU leaf permutation.
    pub cpu_draw_prefix_enabled: bool,
    /// Fixed-grid GPU path-count/tiling replaces indirect setup when enabled.
    pub direct_path_dispatch_enabled: bool,
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

    let pathtag_reduce = add_shader!(pathtag_reduce, [Uniform, BufReadOnly, Buffer]);
    let pathtag_reduce2 = add_shader!(
        pathtag_reduce2,
        [BufReadOnly, Buffer],
        CpuShaderType::Skipped
    );
    let pathtag_scan1 = add_shader!(
        pathtag_scan1,
        [BufReadOnly, BufReadOnly, Buffer],
        CpuShaderType::Skipped
    );
    let pathtag_scan = add_shader!(
        pathtag_scan_small,
        [Uniform, BufReadOnly, BufReadOnly, Buffer],
        CpuShaderType::Present(vello_shaders::cpu::pathtag_scan)
    );
    let pathtag_scan_large = add_shader!(
        pathtag_scan_large,
        [Uniform, BufReadOnly, BufReadOnly, Buffer],
        CpuShaderType::Skipped
    );
    let bbox_clear = add_shader!(bbox_clear, [Uniform, Buffer]);
    let flatten = add_shader!(
        flatten,
        [Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer, Buffer]
    );
    let draw_reduce = add_shader!(draw_reduce, [Uniform, BufReadOnly, Buffer]);
    let draw_leaf = add_shader!(
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
    );
    let draw_prefix = engine.add_cpu_shader("vello.draw_prefix", vello_shaders::cpu::draw_prefix);
    let draw_leaf_prefix = add_shader!(
        draw_leaf_prefix,
        [Uniform, BufReadOnly, BufReadOnly, Buffer, Buffer, Buffer,],
        CpuShaderType::Missing
    );
    let clip_reduce = add_shader!(clip_reduce, [BufReadOnly, BufReadOnly, Buffer, Buffer]);
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
    let path_count_setup = add_shader!(path_count_setup, [Buffer, Buffer]);
    let path_count = add_shader!(
        path_count,
        [Uniform, Buffer, BufReadOnly, BufReadOnly, Buffer, Buffer]
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
    let path_tiling_setup = add_shader!(path_tiling_setup, [Buffer, Buffer, Buffer]);
    let path_tiling = add_shader!(
        path_tiling,
        [
            Buffer,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            BufReadOnly,
            Buffer,
        ]
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
        Some(add_shader!(
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
        ))
    } else {
        None
    };

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
        binning,
        tile_alloc,
        path_count_setup,
        path_count,
        path_count_direct,
        backdrop,
        coarse,
        coarse_sparse,
        path_tiling_setup,
        path_tiling,
        path_tiling_direct,
        fine_area,
        fine_msaa8,
        fine_msaa16,
        fine_sparse_area,
        pathtag_is_cpu: options.use_cpu || engine.cpu_pathtag_enabled(),
        cpu_metadata_enabled: !options.use_cpu && engine.cpu_metadata_enabled(),
        cpu_draw_prefix_enabled: !options.use_cpu && engine.cpu_draw_prefix_enabled(),
        direct_path_dispatch_enabled: !options.use_cpu && engine.direct_path_dispatch_enabled(),
    })
}
