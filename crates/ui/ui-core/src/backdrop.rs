//! The GPU blur pass behind the floating material. It renders what lies below a floating surface into
//! a texture, blurs it and substitutes the placeholder the scene drew for it
//! (`docs/design-language.md`, "Material and transparency"). What asks it for a sample, and the
//! recording that gathers the surfaces of a frame, are [`crate::part_paint::backdrop`].
#![allow(
    clippy::arithmetic_side_effects,
    reason = "backdrop geometry is bounded floating-point pixel math"
)]

use vello::kurbo::{Affine, Rect};
use vello::peniko::{Color, ImageData};
use vello::{AaConfig, RenderParams, Renderer, Scene, wgpu};

use crate::part_paint::Sampling;
use crate::part_paint::backdrop::{
    BackdropFrame, BackdropRegion, KERNEL_REACH, Legible, MAX_BACKDROP_REGIONS, MAX_HALVINGS,
    MAX_KERNEL_RADIUS, MAX_SIGMA_TEXELS, Refraction, Stage,
};

const SHADER_SOURCE: &str = r"
struct Pass {
    uv_offset: vec2<f32>,
    uv_scale: vec2<f32>,
    step: vec2<f32>,
    sigma: f32,
    radius: i32,
    // The lens, for the last pass (`Refraction`, in target pixels): the
    // region's size, its corner radius, the band, the displacement at the
    // rim and the dispersion. `strength` 0 bends nothing.
    size: vec2<f32>,
    corner: f32,
    band: f32,
    strength: f32,
    dispersion: f32,
    // The luminance range the last pass compresses what it shows into,
    // and the ground's luminance it keeps (`Legible`); `0`, `1` none.
    dark: f32,
    light: f32,
    ground: f32,
    pad0: f32,
    pad1: f32,
    pad2: f32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var bilinear: sampler;
@group(0) @binding(2) var<uniform> pass_: Pass;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn source_uv(uv: vec2<f32>) -> vec2<f32> {
    return pass_.uv_offset + uv * pass_.uv_scale;
}

// Each target texel centre falls on the corner shared by four source
// texels, so one bilinear tap is their box average.
@fragment
fn fs_halve(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, bilinear, source_uv(in.uv), 0.0);
}

fn blur_at(centre: vec2<f32>) -> vec4<f32> {
    let falloff = -0.5 / (pass_.sigma * pass_.sigma);
    var sum = textureSampleLevel(source, bilinear, centre, 0.0);
    var total = 1.0;
    for (var i = 1; i <= pass_.radius; i++) {
        let x = f32(i);
        let weight = exp(x * x * falloff);
        let offset = pass_.step * x;
        sum += weight * (textureSampleLevel(source, bilinear, centre + offset, 0.0)
            + textureSampleLevel(source, bilinear, centre - offset, 0.0));
        total += 2.0 * weight;
    }
    return sum / total;
}

fn away(v: f32) -> f32 {
    return select(1.0, -1.0, v < 0.0);
}

// Where the backdrop the region's pixel at `point` shows lies, relative to
// it: toward the inside along the rounded outline's normal, `strength` at
// the outline easing quadratically to nothing `band` in. The same
// arithmetic as `Refraction::displacement`.
fn bend(point: vec2<f32>) -> vec2<f32> {
    let half = pass_.size * 0.5;
    let shorter = min(half.x, half.y);
    let corner = clamp(pass_.corner, 0.0, shorter);
    let p = point - half;
    let q = abs(p) - (half - vec2<f32>(corner));
    let outside = max(q, vec2<f32>(0.0));
    let distance = length(outside) + min(max(q.x, q.y), 0.0) - corner;
    var normal: vec2<f32>;
    if (q.x > 0.0 && q.y > 0.0) {
        normal = outside / length(outside) * vec2<f32>(away(p.x), away(p.y));
    } else if (q.x > q.y) {
        normal = vec2<f32>(away(p.x), 0.0);
    } else {
        normal = vec2<f32>(0.0, away(p.y));
    }
    let t = clamp(1.0 + distance / min(pass_.band, shorter), 0.0, 1.0);
    return -normal * (min(pass_.strength, shorter) * t * t);
}

fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn encoded(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3<f32>(0.0031308));
}

// What shows through, compressed into the legible range: luminance mapped
// evenly in contrast on each side of the ground's, which stays, each
// channel's offset from it scaled down where it
// falls and kept where it rises, pulled toward grey only where a channel
// would leave the gamut. The same arithmetic as `Legible::tone`.
fn tone(c: vec4<f32>) -> vec4<f32> {
    if (pass_.dark <= 0.0 && pass_.light >= 1.0) {
        return c;
    }
    let lin = linear(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let y = dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
    let ground = pass_.ground + 0.05;
    var end = pass_.light + 0.05;
    var far = 1.05;
    if (y < pass_.ground) {
        end = pass_.dark + 0.05;
        far = 0.05;
    }
    let reach = log(far / ground);
    var aim = pass_.ground;
    if (abs(reach) >= 1e-9) {
        aim = ground * pow((y + 0.05) / ground, log(end / ground) / reach) - 0.05;
    }
    // Rounding may carry the band's end a hair past it; past 1 the gamut step
    // below would divide by nothing and turn white black.
    aim = clamp(aim, pass_.dark, pass_.light);
    var keep = 1.0;
    if (y > 1e-5) {
        keep = min(aim / y, 1.0);
    }
    var out = vec3<f32>(aim) + (lin - vec3<f32>(y)) * keep;
    let top = max(out.r, max(out.g, out.b));
    if (top > 1.0) {
        out = vec3<f32>(aim) + (out - vec3<f32>(aim)) * ((1.0 - aim) / (top - aim));
    }
    return vec4<f32>(encoded(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0))), c.a);
}

// The blur, and in the last pass the lens and the legible range: the
// kernel is taken about the point the lens shows, so the bend costs one
// displaced lookup (three where red and blue part).
@fragment
fn fs_blur(in: VertexOut) -> @location(0) vec4<f32> {
    if (pass_.strength <= 0.0 || pass_.band <= 0.0) {
        return tone(blur_at(source_uv(in.uv)));
    }
    let shift = bend(in.uv * pass_.size) / pass_.size;
    let seen = blur_at(source_uv(in.uv + shift));
    if (pass_.dispersion <= 0.0) {
        return tone(seen);
    }
    let red = blur_at(source_uv(in.uv + shift * (1.0 - pass_.dispersion)));
    let blue = blur_at(source_uv(in.uv + shift * (1.0 + pass_.dispersion)));
    return tone(vec4<f32>(red.r, seen.g, blue.b, seen.a));
}
";

const PASS_BYTES: u64 = 80;

/// What [`BackdropBlur::prepare`] did, for frame profiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackdropStats {
    /// Regions whose placeholder now shows the blurred backdrop.
    pub substituted: usize,
    /// Regions that stay opaque: off target, or their backdrop failed to render.
    pub skipped: usize,
    /// Of the substituted, those shown from the sample they kept: nothing below them changed since it
    /// was taken, so no pass ran.
    pub reused: usize,
    /// CPU time spent encoding and submitting, in microseconds.
    pub encode_us: u64,
}

struct Plan {
    /// The region's index in its frame, which picks its output texture.
    slot: usize,
    first_pass: u64,
    /// Target pixels rendered as the backdrop: the region plus the blur's reach, clipped to the target
    /// and rounded out.
    source: Rect,
    levels: Vec<(u32, u32)>,
    /// Target pixels per texel of the rendered backdrop: two, or one for a sample blurred less than two
    /// pixels (a sharp lens), which would show the half resolution as softness.
    ground_texel: f64,
    /// Target pixels per texel of the last level.
    texel: f64,
    /// The blur's deviation in texels of the last level; `0` none.
    sigma: f64,
}

struct Texture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Texture {
    fn size(&self) -> (u32, u32) {
        (self.texture.width(), self.texture.height())
    }
}

/// The GPU half of the material: renders, blurs and substitutes each region's backdrop on one device.
pub struct BackdropBlur {
    layout: wgpu::BindGroupLayout,
    halve: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    passes: wgpu::Buffer,
    pass_stride: u64,
    /// Vello's half-resolution backdrop target.
    ground: Option<Texture>,
    /// Halved copies, one per level below the backdrop.
    levels: Vec<Option<Texture>>,
    /// The horizontal pass's result.
    across: Option<Texture>,
    /// The blurred images vello reads in place of the placeholders, one per region slot.
    outputs: Vec<Option<Texture>>,
    /// Placeholders currently substituted on the renderer.
    substituted: Vec<ImageData>,
    /// What each region slot's output was taken of, so a region whose backdrop has not changed shows it
    /// again without a pass.
    taken: Vec<Option<Taken>>,
    /// The blurred sample a region's material stage reads, one per region slot.
    staged: Vec<Option<Texture>>,
    /// Material stages compiled so far, by their source: `None` for one that does not validate,
    /// which is never tried again.
    stages: std::collections::HashMap<u64, Option<wgpu::RenderPipeline>>,
    /// The layout of a stage's texture, sampler and uniforms.
    stage_layout: wgpu::BindGroupLayout,
}

struct Taken {
    below: Scene,
    rect: Rect,
    size: (u32, u32),
    target: (u32, u32),
    numbers: [u64; 12],
    colours: [[u32; 4]; 2],
    stage: Option<u64>,
}

impl Taken {
    fn of(region: &BackdropRegion, target: (u32, u32), base: Color) -> Self {
        let lens = region.refraction;
        Self {
            below: region.below.clone(),
            rect: region.rect,
            size: (region.image.width, region.image.height),
            target,
            numbers: [
                region.blur_px,
                region.cell_px,
                lens.strength,
                lens.band,
                lens.radius,
                lens.dispersion,
                region.legible.dark,
                region.legible.light,
                f64::from(u8::from(region.sampling == Sampling::Blocky)),
                region.legible.ground,
                0.0,
                0.0,
            ]
            .map(f64::to_bits),
            colours: [region.ground, base].map(|colour| colour.components.map(f32::to_bits)),
            stage: region.stage.as_ref().map(Stage::key),
        }
    }

    fn holds(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.size == other.size
            && self.target == other.target
            && self.numbers == other.numbers
            && self.colours == other.colours
            && self.stage == other.stage
            && self.below.encoding_eq(&other.below)
    }
}

/// Whether `below` draws any of `images`: another region's sample, taken anew this frame, which it
/// shows through itself.
fn shows_any(below: &Scene, images: &[ImageData]) -> bool {
    !images.is_empty()
        && below.encoding().resources.patches.iter().any(|patch| {
            matches!(patch, vello_encoding::Patch::Image { image, .. }
                if images.iter().any(|taken| taken.data.id() == image.data.id()))
        })
}

impl BackdropBlur {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-backdrop-blur"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-backdrop-blur"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(PASS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tilo-backdrop-blur"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tilo-backdrop-blur"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let halve = pipeline("fs_halve");
        let blur = pipeline("fs_blur");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tilo-backdrop-blur"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1);
        let pass_stride = PASS_BYTES.div_ceil(alignment).saturating_mul(alignment);
        let max_passes = (MAX_HALVINGS as usize + 2) * MAX_BACKDROP_REGIONS;
        let passes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-backdrop-blur-passes"),
            size: pass_stride.saturating_mul(max_passes as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let stage_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-material-stage"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(
                            crate::tokens::shader::UNIFORM_BYTES,
                        ),
                    },
                    count: None,
                },
            ],
        });
        Self {
            stage_layout,
            staged: Vec::new(),
            stages: std::collections::HashMap::new(),
            layout,
            halve,
            blur,
            sampler,
            passes,
            pass_stride,
            ground: None,
            levels: Vec::new(),
            across: None,
            outputs: Vec::new(),
            taken: Vec::new(),
            substituted: Vec::new(),
        }
    }

    /// Render, blur and substitute every region of `frame` on `renderer`,
    /// whose next render draws a `target`-sized frame cleared to `base`.
    /// A region that cannot be prepared keeps its opaque placeholder.
    /// Call [`Self::release`] once that render is submitted.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        frame: &BackdropFrame,
        target: (u32, u32),
        base: Color,
    ) -> BackdropStats {
        self.release(renderer);
        let started = crate::time::now();
        let mut stats = BackdropStats::default();
        let max_edge = device.limits().max_texture_dimension_2d;
        let target_rect = Rect::new(0.0, 0.0, f64::from(target.0), f64::from(target.1));
        let mut pass_bytes: Vec<u8> = Vec::new();
        let mut planned = Vec::new();
        for (slot, region) in frame.regions.iter().take(MAX_BACKDROP_REGIONS).enumerate() {
            match plan(region, target_rect, max_edge) {
                Some(mut plan) => {
                    plan.slot = slot;
                    plan.first_pass = pass_bytes.len() as u64 / PASS_BYTES;
                    write_passes(&mut pass_bytes, &plan, region);
                    planned.push((region, plan));
                }
                None => stats.skipped = stats.skipped.saturating_add(1),
            }
        }
        stats.skipped = stats
            .skipped
            .saturating_add(frame.regions.len().saturating_sub(MAX_BACKDROP_REGIONS));
        if planned.is_empty() {
            return stats;
        }
        queue.write_buffer(&self.passes, 0, &self.strided(&pass_bytes));
        let mut fresh: Vec<ImageData> = Vec::new();
        for (region, plan) in planned {
            let slot = plan.slot;
            let now = Taken::of(region, target, base);
            if self.taken.len() <= slot {
                self.taken.resize_with(slot.saturating_add(1), || None);
            }
            let unchanged = self
                .taken
                .get(slot)
                .and_then(Option::as_ref)
                .is_some_and(|taken| taken.holds(&now))
                && !shows_any(&region.below, &fresh);
            if unchanged && self.show_kept(renderer, region, slot) {
                stats.substituted = stats.substituted.saturating_add(1);
                stats.reused = stats.reused.saturating_add(1);
                continue;
            }
            if self.render_one(device, queue, renderer, region, &plan, base) {
                stats.substituted = stats.substituted.saturating_add(1);
                fresh.push(region.image.clone());
                if let Some(taken) = self.taken.get_mut(slot) {
                    *taken = Some(now);
                }
            } else {
                stats.skipped = stats.skipped.saturating_add(1);
                if let Some(taken) = self.taken.get_mut(slot) {
                    *taken = None;
                }
            }
        }
        stats.encode_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats
    }

    fn show_kept(&mut self, renderer: &mut Renderer, region: &BackdropRegion, slot: usize) -> bool {
        let Some(Some(output)) = self.outputs.get(slot) else {
            return false;
        };
        renderer.override_image(
            &region.image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: output.texture.clone(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        self.substituted.push(region.image.clone());
        true
    }

    /// Drop the substitutions of the last [`Self::prepare`], so later
    /// renders on `renderer` draw the opaque placeholders again.
    pub fn release(&mut self, renderer: &mut Renderer) {
        for image in self.substituted.drain(..) {
            renderer.override_image(&image, None);
        }
    }

    fn strided(&self, packed: &[u8]) -> Vec<u8> {
        let stride = usize::try_from(self.pass_stride).unwrap_or(256);
        let mut out = Vec::new();
        for pass in packed.chunks(PASS_BYTES as usize) {
            let start = out.len();
            out.extend_from_slice(pass);
            out.resize(start.saturating_add(stride), 0);
        }
        out
    }

    fn render_one(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        region: &BackdropRegion,
        plan: &Plan,
        base: Color,
    ) -> bool {
        let slot = plan.slot;
        let Some(&ground_size) = plan.levels.first() else {
            return false;
        };
        let ground = ensure_texture(
            device,
            &mut self.ground,
            ground_size,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            Some(&mut *renderer),
        );
        let mut below = Scene::new();
        below.append(
            &region.below,
            Some(
                Affine::scale(1.0 / plan.ground_texel)
                    * Affine::translate((-plan.source.x0, -plan.source.y0)),
            ),
        );
        // The material is opaque: a target cleared to transparency (an
        // offscreen capture) must not let the scene under the placeholder
        // show through unblurred.
        let base = if base.components[3] < 1.0 {
            region.ground
        } else {
            base
        };
        let rendered = renderer.render_to_texture(
            device,
            queue,
            &below,
            &ground.view,
            &RenderParams {
                base_color: base,
                width: ground_size.0,
                height: ground_size.1,
                antialiasing_method: AaConfig::Area,
            },
        );
        if let Err(error) = rendered {
            tracing::warn!(?error, "backdrop render failed; the surface stays opaque");
            return false;
        }
        let halvings = plan.levels.len().saturating_sub(1);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tilo-backdrop-blur"),
        });
        let mut pass = plan.first_pass;
        for (index, &size) in plan.levels.iter().enumerate().skip(1) {
            let Some(level) = ensure_level(device, &mut self.levels, index.saturating_sub(1), size)
            else {
                return false;
            };
            let source = if index == 1 {
                self.ground.as_ref().map(|texture| texture.view.clone())
            } else {
                self.levels
                    .get(index.saturating_sub(2))
                    .and_then(Option::as_ref)
                    .map(|texture| texture.view.clone())
            };
            let Some(source) = source else {
                return false;
            };
            self.draw(device, &mut encoder, &self.halve, &source, pass, &level);
            pass = pass.saturating_add(1);
        }
        let last_size = plan.levels.last().copied().unwrap_or(ground_size);
        let last = if halvings == 0 {
            self.ground.as_ref().map(|texture| texture.view.clone())
        } else {
            self.levels
                .get(halvings.saturating_sub(1))
                .and_then(Option::as_ref)
                .map(|texture| texture.view.clone())
        };
        let Some(last) = last else {
            return false;
        };
        let across = if plan.sigma > 0.0 {
            let across = ensure_texture(
                device,
                &mut self.across,
                last_size,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                None,
            )
            .view
            .clone();
            self.draw(device, &mut encoder, &self.blur, &last, pass, &across);
            across
        } else {
            last
        };
        pass = pass.saturating_add(1);
        if self.outputs.len() <= slot {
            self.outputs.resize_with(slot.saturating_add(1), || None);
        }
        let Some(output_slot) = self.outputs.get_mut(slot) else {
            return false;
        };
        let output = ensure_texture(
            device,
            output_slot,
            (region.image.width, region.image.height),
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            None,
        );
        let output_view = output.view.clone();
        let output_texture = output.texture.clone();
        if let Some(stage) = &region.stage {
            let Some(pipeline) = self.stage_pipeline(device, stage) else {
                return false;
            };
            if self.staged.len() <= slot {
                self.staged.resize_with(slot.saturating_add(1), || None);
            }
            let Some(staged_slot) = self.staged.get_mut(slot) else {
                return false;
            };
            let staged = ensure_texture(
                device,
                staged_slot,
                (region.image.width, region.image.height),
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                None,
            )
            .view
            .clone();
            self.draw(device, &mut encoder, &self.blur, &across, pass, &staged);
            self.draw_stage(
                device,
                &mut encoder,
                &pipeline,
                stage,
                &staged,
                &output_view,
            );
        } else {
            self.draw(
                device,
                &mut encoder,
                &self.blur,
                &across,
                pass,
                &output_view,
            );
        }
        queue.submit([encoder.finish()]);
        renderer.override_image(
            &region.image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        self.substituted.push(region.image.clone());
        true
    }

    /// The pipeline `stage` runs in, compiled the first time its source is seen. A source that does
    /// not pass [`crate::tokens::shader::check`] (a client's frame carries what it likes) is never
    /// compiled: its region keeps the opaque placeholder.
    fn stage_pipeline(
        &mut self,
        device: &wgpu::Device,
        stage: &Stage,
    ) -> Option<wgpu::RenderPipeline> {
        let layout = &self.stage_layout;
        self.stages
            .entry(stage.source_key())
            .or_insert_with(|| {
                if let Err(reason) = crate::tokens::shader::check(&stage.wgsl) {
                    tracing::warn!(%reason, "a material stage was refused; the surface stays opaque");
                    return None;
                }
                let source = crate::tokens::shader::wrap(&crate::tokens::SurfaceShader {
                    wgsl: stage.wgsl.to_string(),
                    time: 0.0,
                });
                let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
                    label: Some("tilo-material-stage"),
                    source: wgpu::ShaderSource::Wgsl(source.into()),
                });
                let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                    label: Some("tilo-material-stage"),
                    bind_group_layouts: &[Some(layout)],
                    immediate_size: 0,
                });
                Some(device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some("tilo-material-stage"),
                    layout: Some(&pipeline_layout),
                    vertex: wgpu::VertexState {
                        module: &module,
                        entry_point: Some("vs_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &module,
                        entry_point: Some("stage_main"),
                        compilation_options: wgpu::PipelineCompilationOptions::default(),
                        targets: &[Some(wgpu::ColorTargetState {
                            format: wgpu::TextureFormat::Rgba8Unorm,
                            blend: None,
                            write_mask: wgpu::ColorWrites::ALL,
                        })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                }))
            })
            .clone()
    }

    fn draw_stage(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        stage: &Stage,
        behind: &wgpu::TextureView,
        target: &wgpu::TextureView,
    ) {
        use wgpu::util::DeviceExt as _;
        let bytes: Vec<u8> = stage
            .uniforms
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect();
        let uniforms = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("tilo-material-stage"),
            contents: &bytes,
            usage: wgpu::BufferUsages::UNIFORM,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-material-stage"),
            layout: &self.stage_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(behind),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: uniforms.as_entire_binding(),
                },
            ],
        });
        let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tilo-material-stage"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        render.set_pipeline(pipeline);
        render.set_bind_group(0, &bind_group, &[]);
        render.draw(0..3, 0..1);
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        source: &wgpu::TextureView,
        pass: u64,
        target: &wgpu::TextureView,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-backdrop-blur"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.passes,
                        offset: 0,
                        size: wgpu::BufferSize::new(PASS_BYTES),
                    }),
                },
            ],
        });
        let offset = u32::try_from(pass.saturating_mul(self.pass_stride)).unwrap_or(0);
        let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tilo-backdrop-blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        render.set_pipeline(pipeline);
        render.set_bind_group(0, &bind_group, &[offset]);
        render.draw(0..3, 0..1);
    }
}

/// The blur `region` is drawn with, in target pixels: its, and for a
/// cell-sampled backdrop at least half a cell, so each cell (one texel of
/// the placeholder the last pass resamples onto) is the average of what
/// lies below it rather than one point of it.
fn region_sigma(region: &BackdropRegion) -> f64 {
    match region.sampling {
        Sampling::Smooth => region.blur_px,
        Sampling::Blocky => region.blur_px.max(region.cell_px * 0.5),
    }
}

const SHARP_SIGMA: f64 = 2.0;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sizes are rounded, positive and bounded by the device's texture limit before the cast"
)]
fn plan(region: &BackdropRegion, target: Rect, max_edge: u32) -> Option<Plan> {
    let sigma = region_sigma(region);
    if !sigma.is_finite()
        || sigma < 0.0
        || (sigma == 0.0 && !region.refraction.shows_through() && region.stage.is_none())
        || region.image.width == 0
        || region.image.height == 0
    {
        return None;
    }
    if region.rect.intersect(target).area() <= 0.0 {
        return None;
    }
    let reach = (sigma * KERNEL_REACH).ceil();
    let source = region.rect.inflate(reach, reach).intersect(target);
    let source = Rect::new(
        source.x0.floor(),
        source.y0.floor(),
        source.x1.ceil(),
        source.y1.ceil(),
    );
    if source.width() < 1.0 || source.height() < 1.0 {
        return None;
    }
    let ground_texel = if sigma < SHARP_SIGMA { 1.0 } else { 2.0 };
    let rendered = |edge: f64| ((edge / ground_texel).ceil().max(1.0) as u32).min(max_edge.max(1));
    let mut size = (rendered(source.width()), rendered(source.height()));
    let mut levels = vec![size];
    let mut texel = ground_texel;
    while sigma / texel > MAX_SIGMA_TEXELS
        && size.0.min(size.1) >= 8
        && levels.len() <= MAX_HALVINGS as usize
    {
        size = (size.0.div_ceil(2), size.1.div_ceil(2));
        levels.push(size);
        texel *= 2.0;
    }
    Some(Plan {
        slot: 0,
        first_pass: 0,
        source,
        levels,
        ground_texel,
        texel,
        sigma: sigma / texel,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "shader parameters are f32 by definition; the values are small texture coordinates"
)]
fn write_passes(bytes: &mut Vec<u8>, plan: &Plan, region: &BackdropRegion) {
    let mut push = |offset: [f64; 2],
                    scale: [f64; 2],
                    step: [f64; 2],
                    (sigma, radius): (f64, i32),
                    (lens, legible): (Refraction, Legible)| {
        for value in [
            offset[0], offset[1], scale[0], scale[1], step[0], step[1], sigma,
        ] {
            bytes.extend_from_slice(&(value as f32).to_le_bytes());
        }
        bytes.extend_from_slice(&radius.to_le_bytes());
        let bends = lens.bends();
        for value in [
            region.rect.width(),
            region.rect.height(),
            lens.radius,
            lens.band,
            if bends { lens.strength } else { 0.0 },
            lens.dispersion.max(0.0),
            legible.dark,
            legible.light,
            legible.ground,
            0.0,
            0.0,
            0.0,
        ] {
            bytes.extend_from_slice(&(value as f32).to_le_bytes());
        }
    };
    for pair in plan.levels.windows(2) {
        if let [source, target] = pair {
            let scale = [
                2.0 * f64::from(target.0) / f64::from(source.0),
                2.0 * f64::from(target.1) / f64::from(source.1),
            ];
            push(
                [0.0, 0.0],
                scale,
                [0.0, 0.0],
                (1.0, 0),
                (Refraction::NONE, Legible::ANY),
            );
        }
    }
    let (width, height) = plan.levels.last().copied().unwrap_or((1, 1));
    let (width, height) = (f64::from(width), f64::from(height));
    let kernel = if plan.sigma > 0.0 {
        let radius = (plan.sigma * KERNEL_REACH)
            .ceil()
            .clamp(1.0, f64::from(MAX_KERNEL_RADIUS));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to 1..=MAX_KERNEL_RADIUS"
        )]
        let radius = radius as i32;
        (plan.sigma, radius)
    } else {
        (1.0, 0)
    };
    push(
        [0.0, 0.0],
        [1.0, 1.0],
        [1.0 / width, 0.0],
        kernel,
        (Refraction::NONE, Legible::ANY),
    );
    let offset = [
        (region.rect.x0 - plan.source.x0) / plan.texel / width,
        (region.rect.y0 - plan.source.y0) / plan.texel / height,
    ];
    let scale = [
        region.rect.width() / plan.texel / width,
        region.rect.height() / plan.texel / height,
    ];
    push(
        offset,
        scale,
        [0.0, 1.0 / height],
        kernel,
        (region.refraction, region.legible),
    );
}

fn ensure_texture<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Texture>,
    size: (u32, u32),
    usage: wgpu::TextureUsages,
    renderer: Option<&mut Renderer>,
) -> &'a Texture {
    if slot.as_ref().is_some_and(|texture| texture.size() != size)
        && let (Some(old), Some(renderer)) = (slot.take(), renderer)
    {
        renderer.invalidate_texture_bindings(&old.view);
    }
    if slot.as_ref().is_some_and(|texture| texture.size() != size) {
        *slot = None;
    }
    slot.get_or_insert_with(|| {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-backdrop"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Texture { texture, view }
    })
}

fn ensure_level(
    device: &wgpu::Device,
    levels: &mut Vec<Option<Texture>>,
    index: usize,
    size: (u32, u32),
) -> Option<wgpu::TextureView> {
    if levels.len() <= index {
        levels.resize_with(index.saturating_add(1), || None);
    }
    let slot = levels.get_mut(index)?;
    Some(
        ensure_texture(
            device,
            slot,
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            None,
        )
        .view
        .clone(),
    )
}
