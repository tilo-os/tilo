//! The blurred material floating chrome is made of.
//!
//! Floating chrome — the palette, menus, popovers, quick settings, sheets,
//! notifications and chrome over media — is drawn over a blurred copy of
//! what is behind it, tinted with the panel ground
//! (`docs/design-language.md`, "Material and transparency"). The blur
//! radius and the tint's opacity are theme roles
//! (`surfaces.material_blur_px`, `surfaces.material_opacity`, set by the
//! theme document's `material`). A surface asks for the material with
//! [`crate::floating_surface::Material::Floating`]; nothing outside the
//! theme and this module picks a blur radius or a tint.
//!
//! Vello cannot sample what it has already drawn, so the blur is a pass
//! that runs before the frame:
//!
//! 1. While a host holds a [`BackdropRecording`], [`fill`] draws a surface's
//!    body as a placeholder image under the tint and records what lies
//!    behind it: what the scene it is drawn into held so far, which of the
//!    host's scenes that is (see [`layer_scene`]), the surface's bounds and
//!    its blur radius.
//! 2. The host turns those records into a [`BackdropFrame`] in its target's
//!    pixel space, each region carrying the complete scene composed below
//!    the surface.
//! 3. A renderer that composites materials calls [`BackdropBlur::prepare`]:
//!    each region's backdrop is rendered at half resolution, halved until
//!    the blur fits a short kernel, blurred separably, and substituted for
//!    the placeholder. The frame then renders as usual, and
//!    [`BackdropBlur::release`] drops the substitutions.
//!
//! The placeholder's own pixels are the opaque panel ground, so any path
//! that does not substitute it — no blur pass in the renderer, a failed
//! backdrop render, a capture of the scene — shows the opaque material.
//! Reduced transparency, forced colours and a theme without blur draw the
//! opaque material directly.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "backdrop geometry is bounded floating-point pixel math"
)]

use std::cell::RefCell;
use std::sync::Arc;

use vello::kurbo::{Affine, Rect, Shape};
use vello::peniko::{
    Blob, Color, Extend, Fill, ImageAlphaType, ImageBrush, ImageData, ImageFormat, ImageQuality,
};
use vello::{AaConfig, RenderParams, Renderer, Scene, wgpu};

use crate::floating_surface::Material;
use crate::theme::Theme;

/// Most surfaces one frame blurs behind. Later ones in the same frame are
/// opaque: each region costs one extra render of everything below it.
pub const MAX_BACKDROP_REGIONS: usize = 4;

/// Logical pixels per texel of the blurred image a surface is drawn with.
/// The backdrop is a low-frequency signal; two pixels per texel is
/// indistinguishable from full resolution once blurred.
const PLACEHOLDER_TEXEL: f64 = 2.0;

/// Longest edge of a placeholder, in texels.
const MAX_PLACEHOLDER_EDGE: u32 = 2048;

/// The blur runs on a copy halved until its standard deviation is at most
/// this many texels, so the kernel stays short at any radius.
const MAX_SIGMA_TEXELS: f64 = 4.0;

/// A kernel reaches three standard deviations; past that a Gaussian's
/// weights are below 1 %.
const KERNEL_REACH: f64 = 3.0;

/// Upper bound on the halvings of one backdrop.
const MAX_HALVINGS: u32 = 6;

/// Upper bound on kernel taps on each side of the centre.
const MAX_KERNEL_RADIUS: i32 = 16;

/// Whether `material` is drawn blurred under `theme`. Reduced transparency,
/// forced colours and a theme that declares no blur (the high-contrast
/// track) keep every surface opaque.
pub(crate) fn blurs(material: Material, theme: &Theme) -> bool {
    matches!(material, Material::Floating)
        && !theme.reduce_transparency
        && !theme.forced_colors
        && theme.tokens.colors.surfaces.material_blur_px > 0.0
}

/// Fill `shape` with `material`: under an active [`BackdropRecording`] the
/// blurred backdrop tinted with the panel ground, otherwise the opaque
/// panel ground. Surfaces reach it through
/// [`crate::floating_surface::paint_material`].
pub(crate) fn fill(
    scene: &mut Scene,
    transform: Affine,
    shape: &impl Shape,
    material: Material,
    theme: &Theme,
) {
    let ground = crate::chrome::panel_ground(theme);
    if blurs(material, theme)
        && let Some((image, brush_transform)) = record(scene, transform, shape, theme, ground)
    {
        let brush = ImageBrush::new(image)
            .with_extend(Extend::Pad)
            .with_quality(ImageQuality::Medium);
        scene.fill(
            Fill::NonZero,
            transform,
            &brush,
            Some(brush_transform),
            shape,
        );
        scene.fill(
            Fill::NonZero,
            transform,
            ground.with_alpha(theme.tokens.colors.surfaces.material_opacity),
            None,
            shape,
        );
        return;
    }
    scene.fill(Fill::NonZero, transform, ground, None, shape);
}

/// One surface drawn in the blurred material while a recording was active,
/// in the recording's coordinates.
#[derive(Clone)]
pub struct RecordedBackdrop {
    /// The placeholder the scene draws for the blurred backdrop.
    pub image: ImageData,
    /// The surface's bounds.
    pub bounds: Rect,
    /// Blur standard deviation in recording pixels.
    pub blur_px: f64,
    /// The layer of the scene the surface was drawn into (see
    /// [`layer_scene`]).
    pub layer: u32,
    /// What that scene held before the surface.
    pub below: Scene,
    /// The opaque panel ground the material tints with.
    pub ground: Color,
}

#[derive(Default)]
struct Recording {
    /// The host's scenes, by address, and their layers.
    layers: Vec<(usize, u32)>,
    surfaces: Vec<RecordedBackdrop>,
}

thread_local! {
    static RECORDING: RefCell<Option<Recording>> = const { RefCell::new(None) };
    /// One placeholder per region slot, reused while its size and ground
    /// hold so an unchanged surface encodes identically frame to frame.
    static PLACEHOLDERS: RefCell<Vec<(usize, ImageData)>> = const { RefCell::new(Vec::new()) };
}

/// Records the surfaces drawn in the blurred material on this thread until
/// [`Self::finish`]. Nests: an inner recording hides the outer one and
/// restores it when it ends.
#[must_use = "a recording that is dropped discards what it collected"]
pub struct BackdropRecording {
    previous: Option<Recording>,
    finished: bool,
}

impl BackdropRecording {
    pub fn begin() -> Self {
        Self {
            previous: RECORDING.with(|slot| slot.replace(Some(Recording::default()))),
            finished: false,
        }
    }

    /// The surfaces drawn since [`Self::begin`], bottom to top.
    pub fn finish(mut self) -> Vec<RecordedBackdrop> {
        self.finished = true;
        let previous = self.previous.take();
        RECORDING
            .with(|slot| slot.replace(previous))
            .map(|recording| recording.surfaces)
            .unwrap_or_default()
    }
}

impl Drop for BackdropRecording {
    fn drop(&mut self) {
        if !self.finished {
            let previous = self.previous.take();
            RECORDING.with(|slot| slot.replace(previous));
        }
    }
}

/// Floating surfaces drawn into `scene` sit on `layer`. The host names
/// every scene it draws its frame into, so it can rebuild what lies below a
/// surface: the scenes it composes under that layer, plus
/// [`RecordedBackdrop::below`]. A host drawing one scene names it once.
///
/// A surface drawn into any other scene — a cache or scratch scene a
/// widget appends later — has no known backdrop and stays opaque. `scene`
/// must not move while the recording is active; naming a layer again
/// replaces its scene. A no-op outside a recording.
pub fn layer_scene(scene: &Scene, layer: u32) {
    let address = std::ptr::from_ref(scene) as usize;
    RECORDING.with(|slot| {
        if let Some(recording) = slot.borrow_mut().as_mut() {
            recording
                .layers
                .retain(|&(known, named)| known != address && named != layer);
            recording.layers.push((address, layer));
        }
    });
}

/// Register a surface with the active recording and return the placeholder
/// to draw with its brush transform, or `None` when nothing records it.
fn record(
    scene: &Scene,
    transform: Affine,
    shape: &impl Shape,
    theme: &Theme,
    ground: Color,
) -> Option<(ImageData, Affine)> {
    RECORDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let recording = slot.as_mut()?;
        let address = std::ptr::from_ref(scene) as usize;
        let layer = recording
            .layers
            .iter()
            .find_map(|&(known, layer)| (known == address).then_some(layer))?;
        let slot_index = recording.surfaces.len();
        if slot_index >= MAX_BACKDROP_REGIONS {
            return None;
        }
        let bounds = transform.transform_rect_bbox(shape.bounding_box());
        if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return None;
        }
        if transform.determinant().abs() <= f64::EPSILON {
            return None;
        }
        let (width, height) = (texels(bounds.width()), texels(bounds.height()));
        let image = placeholder(slot_index, width, height, ground);
        // Brush space is the fill's space: undo `transform`, then map the
        // image onto the surface's bounds in recording coordinates, so the
        // backdrop stays put while the surface scales or slides in.
        let brush_transform = transform.inverse()
            * Affine::translate((bounds.x0, bounds.y0))
            * Affine::scale_non_uniform(
                bounds.width() / f64::from(width),
                bounds.height() / f64::from(height),
            );
        recording.surfaces.push(RecordedBackdrop {
            image: image.clone(),
            bounds,
            blur_px: f64::from(theme.tokens.colors.surfaces.material_blur_px),
            layer,
            below: scene.clone(),
            ground,
        });
        Some((image, brush_transform))
    })
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=MAX_PLACEHOLDER_EDGE before the cast"
)]
fn texels(logical: f64) -> u32 {
    (logical / PLACEHOLDER_TEXEL)
        .ceil()
        .clamp(1.0, f64::from(MAX_PLACEHOLDER_EDGE)) as u32
}

/// The opaque `ground`-coloured image a surface draws in `slot`.
fn placeholder(slot: usize, width: u32, height: u32, ground: Color) -> ImageData {
    let rgba = ground.to_rgba8().to_u8_array();
    PLACEHOLDERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, image)) = cache.iter().find(|(index, image)| {
            *index == slot
                && image.width == width
                && image.height == height
                && image.data.data().get(..4) == Some(&rgba[..])
        }) {
            return image.clone();
        }
        let texel_count = (width as usize).saturating_mul(height as usize);
        let bytes: Vec<u8> = rgba
            .iter()
            .copied()
            .cycle()
            .take(texel_count.saturating_mul(4))
            .collect();
        let image = ImageData {
            data: Blob::new(Arc::new(bytes)),
            format: ImageFormat::Rgba8,
            alpha_type: ImageAlphaType::Alpha,
            width,
            height,
        };
        cache.retain(|(index, _)| *index != slot);
        cache.push((slot, image.clone()));
        image
    })
}

/// One surface's backdrop, in the pixel space of the target it renders to.
#[derive(Clone)]
pub struct BackdropRegion {
    /// The placeholder the frame draws; [`BackdropBlur`] substitutes it.
    pub image: ImageData,
    /// The target pixels the placeholder is mapped onto.
    pub rect: Rect,
    /// Blur standard deviation in target pixels.
    pub blur_px: f64,
    /// Everything composed below the surface, in target pixels.
    pub below: Scene,
    /// The opaque panel ground: what the backdrop shows where nothing below
    /// covers it and the target is cleared to transparency.
    pub ground: Color,
}

/// The backdrops one frame draws its floating surfaces over.
#[derive(Clone, Default)]
pub struct BackdropFrame {
    pub regions: Vec<BackdropRegion>,
}

impl BackdropFrame {
    /// The backdrops of a frame drawn into one scene (named with
    /// [`layer_scene`]), whose coordinates are already the target's: each
    /// surface's backdrop is what was drawn before it.
    pub fn from_single_scene(surfaces: Vec<RecordedBackdrop>) -> Self {
        Self {
            regions: surfaces
                .into_iter()
                .map(|surface| BackdropRegion {
                    image: surface.image,
                    rect: surface.bounds,
                    blur_px: surface.blur_px,
                    below: surface.below,
                    ground: surface.ground,
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

const SHADER_SOURCE: &str = r"
struct Pass {
    uv_offset: vec2<f32>,
    uv_scale: vec2<f32>,
    step: vec2<f32>,
    sigma: f32,
    radius: i32,
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

// Each target texel centre lands on the corner shared by four source
// texels, so one bilinear tap is their box average.
@fragment
fn fs_halve(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, bilinear, source_uv(in.uv), 0.0);
}

@fragment
fn fs_blur(in: VertexOut) -> @location(0) vec4<f32> {
    let centre = source_uv(in.uv);
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
";

/// Bytes of the shader's `Pass`.
const PASS_BYTES: u64 = 32;

/// What [`BackdropBlur::prepare`] did, for frame profiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackdropStats {
    /// Regions whose placeholder now shows the blurred backdrop.
    pub substituted: usize,
    /// Regions that stay opaque: off target, or their backdrop failed to render.
    pub skipped: usize,
    /// CPU time spent encoding and submitting, in microseconds. The GPU
    /// work runs ahead of the frame's own render on the same queue.
    pub encode_us: u64,
}

/// One region's work, planned before anything is encoded.
struct Plan {
    /// The region's index in its frame, which picks its output texture.
    slot: usize,
    /// Index of the region's first `Pass` in the uniform buffer.
    first_pass: u64,
    /// Target pixels rendered as the backdrop: the region plus the blur's
    /// reach, clipped to the target and rounded out.
    source: Rect,
    /// Texture sizes from the half-resolution backdrop down to the level
    /// the blur runs on.
    levels: Vec<(u32, u32)>,
    /// Target pixels per texel of the last level.
    texel: f64,
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

/// The GPU half of the material: renders, blurs and substitutes each
/// region's backdrop on one device. Built once per renderer.
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
    /// The blurred images vello reads in place of the placeholders, one per
    /// region slot.
    outputs: Vec<Option<Texture>>,
    /// Placeholders currently substituted on the renderer.
    substituted: Vec<ImageData>,
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
        Self {
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
        for (region, plan) in planned {
            if self.render_one(device, queue, renderer, region, &plan, base) {
                stats.substituted = stats.substituted.saturating_add(1);
            } else {
                stats.skipped = stats.skipped.saturating_add(1);
            }
        }
        stats.encode_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats
    }

    /// Drop the substitutions of the last [`Self::prepare`], so later
    /// renders on `renderer` draw the opaque placeholders again.
    pub fn release(&mut self, renderer: &mut Renderer) {
        for image in self.substituted.drain(..) {
            renderer.override_image(&image, None);
        }
    }

    /// Lay the tightly packed pass parameters out at the device's uniform
    /// offset alignment.
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
            Some(Affine::scale(0.5) * Affine::translate((-plan.source.x0, -plan.source.y0))),
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
        self.draw(
            device,
            &mut encoder,
            &self.blur,
            &across,
            pass,
            &output_view,
        );
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

/// Decide the source rectangle, the halvings and the kernel for `region`,
/// or `None` when it has nothing to blur.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sizes are rounded, positive and bounded by the device's texture limit before the cast"
)]
fn plan(region: &BackdropRegion, target: Rect, max_edge: u32) -> Option<Plan> {
    let sigma = region.blur_px;
    if !sigma.is_finite() || sigma <= 0.0 || region.image.width == 0 || region.image.height == 0 {
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
    let half = |edge: f64| ((edge / 2.0).ceil().max(1.0) as u32).min(max_edge.max(1));
    let mut size = (half(source.width()), half(source.height()));
    let mut levels = vec![size];
    let mut texel = 2.0;
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
        texel,
        sigma: sigma / texel,
    })
}

/// Append the `Pass` parameters of `plan`: one per halving, the horizontal
/// blur, then the vertical blur that resamples onto the placeholder.
#[allow(
    clippy::cast_possible_truncation,
    reason = "shader parameters are f32 by definition; the values are small texture coordinates"
)]
fn write_passes(bytes: &mut Vec<u8>, plan: &Plan, region: &BackdropRegion) {
    let mut push = |offset: [f64; 2], scale: [f64; 2], step: [f64; 2], sigma: f64, radius: i32| {
        for value in [
            offset[0], offset[1], scale[0], scale[1], step[0], step[1], sigma,
        ] {
            bytes.extend_from_slice(&(value as f32).to_le_bytes());
        }
        bytes.extend_from_slice(&radius.to_le_bytes());
    };
    for pair in plan.levels.windows(2) {
        if let [source, target] = pair {
            let scale = [
                2.0 * f64::from(target.0) / f64::from(source.0),
                2.0 * f64::from(target.1) / f64::from(source.1),
            ];
            push([0.0, 0.0], scale, [0.0, 0.0], 1.0, 0);
        }
    }
    let (width, height) = plan.levels.last().copied().unwrap_or((1, 1));
    let (width, height) = (f64::from(width), f64::from(height));
    let radius = (plan.sigma * KERNEL_REACH)
        .ceil()
        .clamp(1.0, f64::from(MAX_KERNEL_RADIUS));
    #[allow(
        clippy::cast_possible_truncation,
        reason = "clamped to 1..=MAX_KERNEL_RADIUS"
    )]
    let radius = radius as i32;
    push(
        [0.0, 0.0],
        [1.0, 1.0],
        [1.0 / width, 0.0],
        plan.sigma,
        radius,
    );
    // The level's texel `j` covers target pixels `source.x0 + j * texel`
    // onward, so the region's rectangle is this sub-rectangle of it.
    let offset = [
        (region.rect.x0 - plan.source.x0) / plan.texel / width,
        (region.rect.y0 - plan.source.y0) / plan.texel / height,
    ];
    let scale = [
        region.rect.width() / plan.texel / width,
        region.rect.height() / plan.texel / height,
    ];
    push(offset, scale, [0.0, 1.0 / height], plan.sigma, radius);
}

/// `slot`'s texture at exactly `size`, reallocated when it differs. A
/// texture vello renders into is unbound from `renderer` first.
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
        // Vello caches bind groups by view; a stale one would outlive the
        // texture it names.
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

#[cfg(test)]
mod tests;
