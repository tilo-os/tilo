//! Painting a decoded video frame on the GPU.
//!
//! A hardware decoder produces YUV, not RGB — NV12 almost always, P010 for
//! 10-bit. Two paths bring that to the screen without the CPU touching a
//! pixel:
//!
//! * **Zero-copy.** The picture never left the GPU: [`crate::dmabuf_import`]
//!   aliases its dmabuf as textures. Reachable only for a linear buffer (see
//!   that module for why).
//! * **Plane upload.** The decoder produced planes in system memory — what
//!   NVDEC yields, since `nvcodec` exports CUDA memory rather than a dmabuf.
//!   [`upload_planar`] pushes the two planes to the GPU as they are: 1.5 bytes
//!   per pixel with no conversion, against 4 bytes per pixel *after* a
//!   full-frame CPU colour conversion, which is what asking the pipeline for
//!   RGBA costs.
//!
//! Either way the result is a set of YUV planes, and [`YuvConverter`] turns
//! them into one sampleable RGBA texture with a fullscreen pass. That is the
//! same arithmetic [`media::frame::planar_to_rgba`] does per pixel on the CPU,
//! moved to the GPU.
//!
//! [`upload_planar`]: crate::gpu_video::upload_planar
//! [`YuvConverter`]: crate::gpu_video::YuvConverter

use media::dmabuf::{ColorRange, Colorimetry};
use media::frame::{PlanarFormat, PlanarImage};
use vello::wgpu;

#[cfg(all(feature = "dmabuf-import", target_os = "linux"))]
use crate::dmabuf_import::PlaneLayout;

#[cfg(not(all(feature = "dmabuf-import", target_os = "linux")))]
/// How the planes are laid out. Mirrors `dmabuf_import::PlaneLayout` for
/// builds that upload planes but do not import dmabufs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaneLayout {
    Packed,
    BiPlanarYuv,
    PackedYuv422,
}

/// YUV planes living on the GPU, ready for conversion.
pub struct VideoPlanes {
    /// Luma then interleaved chroma for [`PlaneLayout::BiPlanarYuv`]; a single
    /// texture otherwise.
    pub planes: Vec<wgpu::Texture>,
    pub layout: PlaneLayout,
    pub colorimetry: Colorimetry,
    pub width: u32,
    pub height: u32,
}

/// Upload a planar system-memory picture to the GPU as-is.
///
/// No colour conversion happens here — the planes go up in their native
/// format and [`YuvConverter`] does the arithmetic on the GPU. For 1080p NV12
/// that is 3.1 MB uploaded per frame against 8.3 MB for RGBA, and the 2
/// million-pixel conversion never runs on the CPU at all.
pub fn upload_planar(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &PlanarImage,
    width: u32,
    height: u32,
) -> Option<VideoPlanes> {
    if img.planes.len() < 2 {
        tracing::warn!(
            "gpu_video: planar frame has {} planes, need 2",
            img.planes.len()
        );
        return None;
    }
    // NV12's chroma is one Rg8 texel per two pixels in each axis; P010 the
    // same shape at 16 bits per component.
    let (luma_fmt, chroma_fmt) = match img.format {
        PlanarFormat::Nv12 => (wgpu::TextureFormat::R8Unorm, wgpu::TextureFormat::Rg8Unorm),
        PlanarFormat::P010 => (
            wgpu::TextureFormat::R16Unorm,
            wgpu::TextureFormat::Rg16Unorm,
        ),
    };
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);

    let mut planes = Vec::with_capacity(2);
    for (i, (src, (fmt, (w, h)))) in img
        .planes
        .iter()
        .zip([(luma_fmt, (width, height)), (chroma_fmt, (cw, ch))])
        .enumerate()
    {
        let tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(if i == 0 { "video-luma" } else { "video-chroma" }),
            size: wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: fmt,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        // The decoder pads rows; `bytes_per_row` is the source stride, not the
        // packed width, or every row after the first lands offset.
        let Some(needed) = (src.stride as usize).checked_mul(h as usize) else {
            tracing::warn!(
                "gpu_video: plane {i} dimensions overflow for {w}x{h} at stride {}",
                src.stride
            );
            return None;
        };
        let Some(bytes) = src.bytes.get(..needed) else {
            tracing::warn!(
                "gpu_video: plane {i} has {} bytes, needs {needed} for {}x{h} at stride {}",
                src.bytes.len(),
                w,
                src.stride
            );
            return None;
        };
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &tex,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            bytes,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(src.stride),
                rows_per_image: Some(h),
            },
            wgpu::Extent3d {
                width: w,
                height: h,
                depth_or_array_layers: 1,
            },
        );
        planes.push(tex);
    }

    Some(VideoPlanes {
        planes,
        layout: PlaneLayout::BiPlanarYuv,
        colorimetry: img.colorimetry,
        width,
        height,
    })
}

/// The per-frame constants the conversion shader needs: the inverse colour
/// matrix and the range expansion, derived from the stream's colorimetry.
///
/// Laid out as four `vec4`s so the alignment rules are trivially satisfied.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
struct ConvertUniform {
    /// `r_cr`, `g_cb`, `g_cr`, `b_cb`.
    coeffs: [f32; 4],
    /// `y_scale`, `y_offset`, `c_scale`, `is_yuv422`.
    range: [f32; 4],
}

impl ConvertUniform {
    fn for_colorimetry(c: Colorimetry, layout: PlaneLayout) -> Self {
        let (kr, kg, kb) = c.matrix.luma_coefficients();
        let (y_scale, y_offset, c_scale) = match c.range {
            // 16..235 luma and 16..240 chroma, expanded to 0..1.
            ColorRange::Limited => (255.0 / 219.0, 16.0 / 255.0, 255.0 / 224.0),
            ColorRange::Full => (1.0, 0.0, 1.0),
        };
        Self {
            coeffs: [
                2.0 * (1.0 - kr),
                -2.0 * kb * (1.0 - kb) / kg,
                -2.0 * kr * (1.0 - kr) / kg,
                2.0 * (1.0 - kb),
            ],
            range: [
                y_scale,
                y_offset,
                c_scale,
                if layout == PlaneLayout::PackedYuv422 {
                    1.0
                } else {
                    0.0
                },
            ],
        }
    }

    fn as_bytes(&self) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (slot, value) in out
            .chunks_exact_mut(std::mem::size_of::<f32>())
            .zip(self.coeffs.iter().chain(&self.range))
        {
            slot.copy_from_slice(&value.to_ne_bytes());
        }
        out
    }
}

const SHADER: &str = r"
struct Convert {
    // r_cr, g_cb, g_cr, b_cb
    coeffs: vec4<f32>,
    // y_scale, y_offset, c_scale, is_yuv422
    range: vec4<f32>,
};

@group(0) @binding(0) var<uniform> cv: Convert;
@group(0) @binding(1) var luma: texture_2d<f32>;
@group(0) @binding(2) var chroma: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

// A single oversized triangle covers the target with no vertex buffer.
@vertex
fn vs(@builtin(vertex_index) idx: u32) -> VsOut {
    var out: VsOut;
    let x = f32((idx << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(idx & 2u) * 2.0 - 1.0;
    out.pos = vec4<f32>(x, y, 0.0, 1.0);
    // Flip Y: texture space runs downwards, clip space upwards.
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}

@fragment
fn fs(in: VsOut) -> @location(0) vec4<f32> {
    var y: f32;
    var cb: f32;
    var cr: f32;

    if (cv.range.w > 0.5) {
        // 4:2:2 packed as Y0 Cb Y1 Cr in one RGBA texel per two pixels. Which
        // luma of the pair applies depends on the pixel's parity within the
        // texel, so sample the texel and pick.
        let dims = vec2<f32>(textureDimensions(luma));
        let texel = textureSample(luma, samp, in.uv);
        let px = in.uv.x * dims.x * 2.0;
        let even = fract(px * 0.5) < 0.5;
        y = select(texel.z, texel.x, even);
        cb = texel.y;
        cr = texel.w;
    } else {
        y = textureSample(luma, samp, in.uv).r;
        let c = textureSample(chroma, samp, in.uv).rg;
        cb = c.r;
        cr = c.g;
    }

    // Expand the studio range and remove the chroma bias, then apply the
    // inverse of the RGB->YCbCr matrix the stream was encoded with.
    let yy = (y - cv.range.y) * cv.range.x;
    let u = (cb - 0.5) * cv.range.z;
    let v = (cr - 0.5) * cv.range.z;

    let r = yy + cv.coeffs.x * v;
    let g = yy + cv.coeffs.y * u + cv.coeffs.z * v;
    let b = yy + cv.coeffs.w * u;
    return vec4<f32>(clamp(vec3<f32>(r, g, b), vec3<f32>(0.0), vec3<f32>(1.0)), 1.0);
}
";

/// Turns YUV planes into one RGBA texture with a fullscreen pass.
///
/// Build once and reuse: the pipeline and sampler are the expensive part, and
/// nothing in them depends on the frame.
pub struct YuvConverter {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    uniform: wgpu::Buffer,
    /// The output texture, rebuilt when the frame size changes.
    target: Option<(wgpu::Texture, u32, u32)>,
    format: wgpu::TextureFormat,
}

impl YuvConverter {
    fn create_target(
        device: &wgpu::Device,
        format: wgpu::TextureFormat,
        width: u32,
        height: u32,
    ) -> (wgpu::Texture, u32, u32) {
        (
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some("video-rgba"),
                size: wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            }),
            width,
            height,
        )
    }

    /// Build a converter producing `format` (use the surface's format so the
    /// result can be composited directly).
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuv-to-rgba"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv-to-rgba"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuv-to-rgba"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuv-to-rgba"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                buffers: &[],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                targets: &[Some(format.into())],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            layout,
            sampler: device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some("yuv-to-rgba"),
                mag_filter: wgpu::FilterMode::Linear,
                min_filter: wgpu::FilterMode::Linear,
                // Chroma is half resolution; clamping stops the edge texels
                // wrapping and tinting the opposite border.
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                ..Default::default()
            }),
            uniform: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("yuv-to-rgba"),
                size: 32,
                usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            }),
            target: None,
            format,
        }
    }

    /// Convert `planes` into an RGBA texture and return a view of it.
    ///
    /// The output texture is reused across calls while the frame size holds,
    /// so a steady stream of same-sized frames allocates nothing.
    ///
    /// # Panics
    ///
    /// Panics if `planes` contains no textures. Every supported plane layout
    /// has at least one texture, and the constructors in this module enforce
    /// that invariant.
    pub fn convert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        planes: &VideoPlanes,
    ) -> &wgpu::Texture {
        let (w, h) = (planes.width.max(1), planes.height.max(1));
        let stale = !matches!(&self.target, Some((_, tw, th)) if *tw == w && *th == h);
        if stale {
            self.target = Some(Self::create_target(device, self.format, w, h));
        }

        queue.write_buffer(
            &self.uniform,
            0,
            &ConvertUniform::for_colorimetry(planes.colorimetry, planes.layout).as_bytes(),
        );

        // A packed 4:2:2 frame has one texture; bind it twice so the layout is
        // satisfied without a second pipeline.
        #[allow(clippy::expect_used, reason = "documented VideoPlanes invariant")]
        let luma_texture = planes
            .planes
            .first()
            .expect("video planes must contain a luma or packed texture");
        let luma = luma_texture.create_view(&wgpu::TextureViewDescriptor::default());
        let chroma = planes
            .planes
            .get(1)
            .unwrap_or(luma_texture)
            .create_view(&wgpu::TextureViewDescriptor::default());
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv-to-rgba"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: self.uniform.as_entire_binding(),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(&luma),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&chroma),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        });

        let (target, _, _) = self
            .target
            .get_or_insert_with(|| Self::create_target(device, self.format, w, h));
        let view = target.create_view(&wgpu::TextureViewDescriptor::default());
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("yuv-to-rgba"),
        });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv-to-rgba"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    depth_slice: None,
                    ops: wgpu::Operations {
                        // This is an off-screen conversion target, not an app
                        // surface. A transparent clear is deterministic and
                        // the fullscreen pass replaces it with opaque video.
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &bind, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit(Some(enc.finish()));
        target
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media::dmabuf::YuvMatrix;

    #[test]
    fn yuv_conversion_constants_map_range_and_chroma_like_the_cpu_path() {
        // Case `limited_range_constants_expand_video_black_and_white_to_the_full_scale`.
        {
            let u = ConvertUniform::for_colorimetry(
                Colorimetry {
                    matrix: YuvMatrix::Bt709,
                    range: ColorRange::Limited,
                },
                PlaneLayout::BiPlanarYuv,
            );
            let [y_scale, y_offset, ..] = u.range;
            // The shader computes (y - offset) * scale.
            let black = (16.0 / 255.0 - y_offset) * y_scale;
            let white = (235.0 / 255.0 - y_offset) * y_scale;
            assert!(
                black.abs() < 1e-4,
                "limited-range 16 must map to 0.0, got {black}"
            );
            assert!(
                (white - 1.0).abs() < 1e-4,
                "limited-range 235 must map to 1.0, got {white}"
            );
        }

        // Case `full_range_constants_are_the_identity`.
        {
            let u = ConvertUniform::for_colorimetry(
                Colorimetry {
                    matrix: YuvMatrix::Bt709,
                    range: ColorRange::Full,
                },
                PlaneLayout::BiPlanarYuv,
            );
            assert_eq!(u.range[0], 1.0, "no scaling");
            assert_eq!(u.range[1], 0.0, "no offset");
            assert_eq!(u.range[2], 1.0, "no chroma scaling");
        }

        // Case `neutral_chroma_stays_grey_under_every_matrix`.
        {
            // A sign error or a swapped coefficient shows up here immediately.
            for matrix in [YuvMatrix::Bt601, YuvMatrix::Bt709, YuvMatrix::Bt2020] {
                let u = ConvertUniform::for_colorimetry(
                    Colorimetry {
                        matrix,
                        range: ColorRange::Full,
                    },
                    PlaneLayout::BiPlanarYuv,
                );
                let [r, g, b] = convert_one(&u, 0.5, 0.5, 0.5);
                assert!((r - 0.5).abs() < 1e-5, "{matrix:?}: r drifted to {r}");
                assert!((g - 0.5).abs() < 1e-5, "{matrix:?}: g drifted to {g}");
                assert!((b - 0.5).abs() < 1e-5, "{matrix:?}: b drifted to {b}");
            }
        }

        // Case `pure_chroma_pushes_the_expected_channel`.
        {
            let u = ConvertUniform::for_colorimetry(
                Colorimetry {
                    matrix: YuvMatrix::Bt709,
                    range: ColorRange::Full,
                },
                PlaneLayout::BiPlanarYuv,
            );
            // Cr above neutral is red; Cb above neutral is blue. Swapping the two
            // is the classic YUV bug and this is what catches it.
            let [r, _, b] = convert_one(&u, 0.5, 0.5, 1.0);
            assert!(r > b, "high Cr must raise red above blue, got r={r} b={b}");
            let [r2, _, b2] = convert_one(&u, 0.5, 1.0, 0.5);
            assert!(
                b2 > r2,
                "high Cb must raise blue above red, got r={r2} b={b2}"
            );
        }

        // Case `the_gpu_matrix_agrees_with_the_cpu_fallback`.
        {
            // `media::frame::planar_to_rgba` is the CPU path for consumers that
            // cannot use a GPU. The two must not diverge, or a frame would change
            // colour depending on which path served it.
            use media::frame::{PlanarImage, PlaneData};
            use std::sync::Arc;

            let colorimetry = Colorimetry {
                matrix: YuvMatrix::Bt709,
                range: ColorRange::Limited,
            };
            for (y, cb, cr) in [
                (128u8, 128u8, 128u8),
                (200, 90, 160),
                (60, 170, 100),
                (235, 128, 128),
            ] {
                let img = PlanarImage {
                    format: PlanarFormat::Nv12,
                    planes: vec![
                        PlaneData {
                            bytes: Arc::new(vec![y; 4]),
                            stride: 2,
                            width: 2,
                            height: 2,
                        },
                        PlaneData {
                            bytes: Arc::new(vec![cb, cr]),
                            stride: 2,
                            width: 1,
                            height: 1,
                        },
                    ],
                    colorimetry,
                };
                let cpu = media::frame::planar_to_rgba(&img, 2, 2);

                let u = ConvertUniform::for_colorimetry(colorimetry, PlaneLayout::BiPlanarYuv);
                let gpu = convert_one(
                    &u,
                    f32::from(y) / 255.0,
                    f32::from(cb) / 255.0,
                    f32::from(cr) / 255.0,
                );

                for (i, name) in ["r", "g", "b"].iter().enumerate() {
                    let gpu_u8 = (gpu[i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    let delta = i32::from(gpu_u8) - i32::from(cpu[i]);
                    assert!(
                        delta.abs() <= 1,
                        "YUV({y},{cb},{cr}) {name}: GPU {gpu_u8} vs CPU {} — the two \
                     conversion paths disagree",
                        cpu[i]
                    );
                }
            }
        }
    }

    /// Run the shader's arithmetic on the CPU so the constants can be checked
    /// against known colours without a GPU.
    fn convert_one(u: &ConvertUniform, y: f32, cb: f32, cr: f32) -> [f32; 3] {
        let yy = (y - u.range[1]) * u.range[0];
        let uu = (cb - 0.5) * u.range[2];
        let vv = (cr - 0.5) * u.range[2];
        [
            yy + u.coeffs[0] * vv,
            yy + u.coeffs[1] * uu + u.coeffs[2] * vv,
            yy + u.coeffs[3] * uu,
        ]
    }

    #[test]
    fn video_uniform_is_packed_as_the_shader_declares() {
        // Case `the_uniform_is_packed_as_the_shader_declares_it`.
        {
            // Two vec4s, 32 bytes; a mismatch here reads garbage constants in the
            // shader and produces a plausible-but-wrong picture.
            assert_eq!(std::mem::size_of::<ConvertUniform>(), 32);
            let u = ConvertUniform::for_colorimetry(
                Colorimetry::BT709_LIMITED,
                PlaneLayout::BiPlanarYuv,
            );
            let bytes = u.as_bytes();
            assert_eq!(&bytes[0..4], &u.coeffs[0].to_ne_bytes(), "coeffs lead");
            assert_eq!(
                &bytes[16..20],
                &u.range[0].to_ne_bytes(),
                "range starts at the second vec4"
            );
        }

        // Case `the_packed_422_flag_is_set_only_for_that_layout`.
        {
            let bi = ConvertUniform::for_colorimetry(
                Colorimetry::BT709_LIMITED,
                PlaneLayout::BiPlanarYuv,
            );
            assert_eq!(bi.range[3], 0.0);
            let packed = ConvertUniform::for_colorimetry(
                Colorimetry::BT709_LIMITED,
                PlaneLayout::PackedYuv422,
            );
            assert_eq!(packed.range[3], 1.0, "the shader branches on this");
        }
    }
}
