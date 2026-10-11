//! Strip pans composited from retained layers.
//!
//! While a strip pans, the frame producer draws the desktop as three layers
//! ([`FrameLayers`]): what is behind every window, the strips in canvas
//! coordinates, and every piece of chrome above them. A pan then changes only
//! where each strip reads the canvas ([`StripBlit::scroll_x`]); the layers'
//! scenes stay byte for byte the same.
//!
//! [`StripLayers`] keeps each layer rasterised in a texture of the output's
//! size. The workspace texture holds the strips as the screen shows them
//! ([`append_panned`]): on a pure pan, the part of each strip still on screen
//! is copied to where it moved, and only the columns the pan uncovered are
//! rasterised. The output is then composited from the three textures (source
//! over, premultiplied, as vello blends), over the strips that moved. The
//! backdrop and the chrome are rasterised again only when their scenes change.
//!
//! The composite stores straight alpha, as vello's fine stage does, so a
//! composited output matches a single render of the same frame within a
//! channel step of rounding.

use vello::kurbo::{Affine, Rect, Shape as _};
use vello::peniko::Fill;
use vello::{FineDamageRect, Scene, wgpu};

use crate::shell::frame::{FrameCache, StripBlit};

/// More strips than this, or more uncovered bands than vello's sparse pass
/// takes, and the workspace layer is rasterised whole.
const MAX_BANDS: usize = 16;

/// The workspace layer as the screen shows it: inside each panned strip,
/// read `scroll_x` pixels to the right; everywhere else in place.
pub fn append_panned(scene: &mut Scene, workspace: &Scene, strips: &[StripBlit], size: (f64, f64)) {
    if strips.is_empty() {
        scene.append(workspace, None);
        return;
    }
    let dest = |strip: &StripBlit| {
        Rect::new(
            f64::from(strip.dest_x),
            f64::from(strip.dest_y),
            f64::from(strip.dest_x) + f64::from(strip.dest_w),
            f64::from(strip.dest_y) + f64::from(strip.dest_h),
        )
    };
    for strip in strips {
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &dest(strip));
        scene.append(
            workspace,
            Some(Affine::translate((-f64::from(strip.scroll_x), 0.0))),
        );
        scene.pop_layer();
    }
    let (w, h) = size;
    let mut rest = Rect::new(0.0, 0.0, w, h).to_path(0.1);
    for strip in strips {
        rest.extend(dest(strip).path_elements(0.1));
    }
    scene.push_clip_layer(Fill::EvenOdd, Affine::IDENTITY, &rest);
    scene.append(workspace, None);
    scene.pop_layer();
}

/// What a set of layer textures shows: the layers of the frame last
/// composited from them.
pub(crate) struct Shown {
    backdrop: Scene,
    workspace: Scene,
    chrome: Scene,
    width: u32,
    strips: Vec<StripBlit>,
    /// The frame whose composite the output target was last given.
    pub(crate) revision: u64,
}

impl Shown {
    pub(crate) fn of(cache: &FrameCache, revision: u64) -> Self {
        Self {
            backdrop: cache.layers.backdrop.clone(),
            workspace: cache.layers.workspace.clone(),
            chrome: cache.layers.chrome.clone(),
            width: cache.width,
            strips: cache.strips.clone(),
            revision,
        }
    }
}

/// Part of the workspace layer that a pan moves: `width × height` pixels at
/// `(src_x, y)` go to `(dst_x, y)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandCopy {
    pub(crate) src_x: u32,
    pub(crate) dst_x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// What the workspace layer needs for a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceWork {
    /// It already shows the frame.
    Unchanged,
    /// The strips only panned: copy what stays on screen, then rasterise
    /// what the pans uncovered.
    Pan {
        copies: Vec<BandCopy>,
        uncovered: Vec<FineDamageRect>,
    },
    /// Rasterise it whole.
    Whole,
}

/// The work a frame takes: which layers to rasterise, and where to
/// composite the output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LayerWork {
    pub(crate) backdrop: bool,
    pub(crate) chrome: bool,
    pub(crate) workspace: WorkspaceWork,
    /// The output rect to composite again; `None` when the output already
    /// shows the frame.
    pub(crate) composite: Option<FineDamageRect>,
}

impl LayerWork {
    /// Every layer and the whole output.
    fn whole(size: (u32, u32)) -> Self {
        Self {
            backdrop: true,
            chrome: true,
            workspace: WorkspaceWork::Whole,
            composite: Some(full_rect(size)),
        }
    }

    /// Pixels this frame rasterises with vello, of an output of `size`.
    pub(crate) fn raster_pixels(&self, size: (u32, u32)) -> u64 {
        let whole = area(full_rect(size));
        let layers = u64::from(self.backdrop).saturating_add(u64::from(self.chrome));
        let workspace = match &self.workspace {
            WorkspaceWork::Unchanged => 0,
            WorkspaceWork::Pan { uncovered, .. } => uncovered
                .iter()
                .copied()
                .map(area)
                .fold(0, u64::saturating_add),
            WorkspaceWork::Whole => whole,
        };
        whole.saturating_mul(layers).saturating_add(workspace)
    }

    /// Pixels this frame composites.
    pub(crate) fn composite_pixels(&self) -> u64 {
        self.composite.map_or(0, area)
    }
}

fn full_rect(size: (u32, u32)) -> FineDamageRect {
    FineDamageRect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }
}

fn area(rect: FineDamageRect) -> u64 {
    u64::from(rect.width).saturating_mul(u64::from(rect.height))
}

fn union(a: Option<FineDamageRect>, b: FineDamageRect) -> FineDamageRect {
    let Some(a) = a else { return b };
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    let x1 = a.x.saturating_add(a.width).max(b.x.saturating_add(b.width));
    let y1 =
        a.y.saturating_add(a.height)
            .max(b.y.saturating_add(b.height));
    FineDamageRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    }
}

/// `strip`'s destination clamped to an output of `size`, as `(x0, y0, x1, y1)`.
fn band(strip: &StripBlit, size: (u32, u32)) -> (u32, u32, u32, u32) {
    let x0 = strip.dest_x.min(size.0);
    let y0 = strip.dest_y.min(size.1);
    let x1 = strip.dest_x.saturating_add(strip.dest_w).min(size.0);
    let y1 = strip.dest_y.saturating_add(strip.dest_h).min(size.1);
    (x0, y0, x1, y1)
}

fn overlaps(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 < b.2 && b.0 < a.2 && a.1 < b.3 && b.1 < a.3
}

/// The work that brings layer textures showing `shown` to `cache`, on an
/// output of `size`. `output_current` says the output target still holds
/// `shown`'s composite, so only what changed needs compositing again.
pub(crate) fn plan(
    shown: Option<&Shown>,
    cache: &FrameCache,
    size: (u32, u32),
    output_current: bool,
) -> LayerWork {
    let Some(shown) = shown else {
        return LayerWork::whole(size);
    };
    let backdrop = !shown.backdrop.encoding_eq(&cache.layers.backdrop);
    let chrome = !shown.chrome.encoding_eq(&cache.layers.chrome);
    let workspace = workspace_work(shown, cache, size);
    let mut composite = None;
    if backdrop || chrome || workspace == WorkspaceWork::Whole || !output_current {
        composite = Some(full_rect(size));
    } else if let WorkspaceWork::Pan { copies, uncovered } = &workspace {
        for rect in copies
            .iter()
            .map(|copy| FineDamageRect {
                x: copy.src_x.min(copy.dst_x),
                y: copy.y,
                width: copy.width.saturating_add(copy.src_x.abs_diff(copy.dst_x)),
                height: copy.height,
            })
            .chain(uncovered.iter().copied())
        {
            composite = Some(union(composite, rect));
        }
    }
    LayerWork {
        backdrop,
        chrome,
        workspace,
        composite,
    }
}

fn workspace_work(shown: &Shown, cache: &FrameCache, size: (u32, u32)) -> WorkspaceWork {
    let same_strips = shown.strips.len() == cache.strips.len()
        && shown.strips.iter().zip(&cache.strips).all(|(old, new)| {
            (old.dest_x, old.dest_y, old.dest_w, old.dest_h)
                == (new.dest_x, new.dest_y, new.dest_w, new.dest_h)
        });
    if shown.width != cache.width
        || !same_strips
        || cache.strips.len() > MAX_BANDS
        || !shown.workspace.encoding_eq(&cache.layers.workspace)
    {
        return WorkspaceWork::Whole;
    }
    let mut copies = Vec::new();
    let mut uncovered = Vec::new();
    for (index, (old, new)) in shown.strips.iter().zip(&cache.strips).enumerate() {
        if old.scroll_x == new.scroll_x {
            continue;
        }
        let (x0, y0, x1, y1) = band(new, size);
        let (width, height) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        if width == 0 || height == 0 {
            continue;
        }
        // Where two strips' destinations meet, the screen shows both reads
        // of the canvas: a copy cannot move one of them alone.
        if cache
            .strips
            .iter()
            .enumerate()
            .any(|(other, strip)| other != index && overlaps(band(strip, size), (x0, y0, x1, y1)))
        {
            return WorkspaceWork::Whole;
        }
        // A pixel at x shows the canvas at x + scroll_x: after the pan it
        // shows what x + (new - old) showed before.
        let forward = new.scroll_x > old.scroll_x;
        let shift = new.scroll_x.abs_diff(old.scroll_x);
        if shift >= width {
            uncovered.push(FineDamageRect {
                x: x0,
                y: y0,
                width,
                height,
            });
            continue;
        }
        let kept = width.saturating_sub(shift);
        let (src_x, dst_x, gap_x) = if forward {
            (x0.saturating_add(shift), x0, x0.saturating_add(kept))
        } else {
            (x0, x0.saturating_add(shift), x0)
        };
        copies.push(BandCopy {
            src_x,
            dst_x,
            y: y0,
            width: kept,
            height,
        });
        uncovered.push(FineDamageRect {
            x: gap_x,
            y: y0,
            width: shift,
            height,
        });
    }
    if copies.is_empty() && uncovered.is_empty() {
        WorkspaceWork::Unchanged
    } else {
        WorkspaceWork::Pan { copies, uncovered }
    }
}

/// One layer's texture: what vello rasterises into and the composite reads.
pub(crate) struct LayerTexture {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
}

impl LayerTexture {
    fn new(device: &wgpu::Device, size: (u32, u32), label: &'static str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Self { texture, view }
    }
}

/// Which layer texture a render goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Layer {
    Backdrop,
    Workspace,
    Chrome,
}

/// The retained layers of one output, and the pass that composites them.
pub(crate) struct StripLayers {
    pub(crate) size: (u32, u32),
    backdrop: LayerTexture,
    workspace: LayerTexture,
    chrome: LayerTexture,
    /// Where a moving band is staged: a texture cannot be copied onto itself.
    scratch: Option<(LayerTexture, (u32, u32))>,
    composite: CompositePass,
    pub(crate) shown: Option<Shown>,
}

impl StripLayers {
    pub(crate) fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        Self {
            size,
            backdrop: LayerTexture::new(device, size, "tilo-strip-layer-backdrop"),
            workspace: LayerTexture::new(device, size, "tilo-strip-layer-workspace"),
            chrome: LayerTexture::new(device, size, "tilo-strip-layer-chrome"),
            scratch: None,
            composite: CompositePass::new(device),
            shown: None,
        }
    }

    pub(crate) fn texture_mut(&mut self, layer: Layer) -> &mut LayerTexture {
        match layer {
            Layer::Backdrop => &mut self.backdrop,
            Layer::Workspace => &mut self.workspace,
            Layer::Chrome => &mut self.chrome,
        }
    }

    /// Move the workspace bands `copies` names, through the scratch texture.
    pub(crate) fn encode_copies(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        copies: &[BandCopy],
    ) {
        let need = copies.iter().fold((0, 0), |(w, h), copy| {
            (w.max(copy.width), h.max(copy.height))
        });
        if need.0 == 0 || need.1 == 0 {
            return;
        }
        if self
            .scratch
            .as_ref()
            .is_none_or(|(_, size)| size.0 < need.0 || size.1 < need.1)
        {
            self.scratch = Some((
                LayerTexture::new(device, need, "tilo-strip-layer-scratch"),
                need,
            ));
        }
        let Some((scratch, _)) = self.scratch.as_ref() else {
            return;
        };
        for copy in copies {
            let extent = wgpu::Extent3d {
                width: copy.width,
                height: copy.height,
                depth_or_array_layers: 1,
            };
            encoder.copy_texture_to_texture(
                texel(&self.workspace.texture, copy.src_x, copy.y),
                texel(&scratch.texture, 0, 0),
                extent,
            );
            encoder.copy_texture_to_texture(
                texel(&scratch.texture, 0, 0),
                texel(&self.workspace.texture, copy.dst_x, copy.y),
                extent,
            );
        }
    }

    /// Composite `rect` of `output` from the three layers.
    pub(crate) fn encode_composite(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        output: &wgpu::TextureView,
        rect: FineDamageRect,
    ) {
        self.composite.encode(
            device,
            queue,
            encoder,
            [
                &self.backdrop.view,
                &self.workspace.view,
                &self.chrome.view,
                output,
            ],
            rect,
        );
    }
}

/// The texel at `(x, y)` of `texture`, as a copy's corner.
fn texel(texture: &wgpu::Texture, x: u32, y: u32) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d { x, y, z: 0 },
        aspect: wgpu::TextureAspect::All,
    }
}

const COMPOSITE_SHADER: &str = r"
struct Params {
    origin: vec2<u32>,
    extent: vec2<u32>,
};

@group(0) @binding(0) var backdrop: texture_2d<f32>;
@group(0) @binding(1) var workspace: texture_2d<f32>;
@group(0) @binding(2) var chrome: texture_2d<f32>;
@group(0) @binding(3) var output: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(4) var<uniform> params: Params;

fn premultiplied(c: vec4<f32>) -> vec4<f32> {
    return vec4<f32>(c.rgb * c.a, c.a);
}

@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= params.extent.x || id.y >= params.extent.y) {
        return;
    }
    let p = vec2<i32>(params.origin + id.xy);
    let below = premultiplied(textureLoad(backdrop, p, 0));
    let strips = premultiplied(textureLoad(workspace, p, 0));
    let above = premultiplied(textureLoad(chrome, p, 0));
    let under = strips + below * (1.0 - strips.a);
    let out = above + under * (1.0 - above.a);
    // Straight alpha, as vello's fine stage stores it.
    let a_inv = 1.0 / max(out.a, 1e-6);
    textureStore(output, p, vec4<f32>(out.rgb * a_inv, out.a));
}
";

/// Bytes of the composite shader's `Params` uniform.
const PARAMS_BYTES: u64 = 16;

struct CompositePass {
    pipeline: wgpu::ComputePipeline,
    layout: wgpu::BindGroupLayout,
    params: wgpu::Buffer,
}

impl CompositePass {
    fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-strip-layer-composite"),
            source: wgpu::ShaderSource::Wgsl(COMPOSITE_SHADER.into()),
        });
        let read = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-strip-layer-composite"),
            entries: &[
                read(0),
                read(1),
                read(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: wgpu::BufferSize::new(PARAMS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tilo-strip-layer-composite"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("tilo-strip-layer-composite"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("main"),
            compilation_options: wgpu::PipelineCompilationOptions::default(),
            cache: None,
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-strip-layer-composite-params"),
            size: PARAMS_BYTES,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            layout,
            params,
        }
    }

    fn encode(
        &self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        encoder: &mut wgpu::CommandEncoder,
        [backdrop, workspace, chrome, output]: [&wgpu::TextureView; 4],
        rect: FineDamageRect,
    ) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let mut bytes = [0_u8; 16];
        for (slot, value) in
            bytes
                .chunks_exact_mut(4)
                .zip([rect.x, rect.y, rect.width, rect.height])
        {
            slot.copy_from_slice(&value.to_le_bytes());
        }
        queue.write_buffer(&self.params, 0, &bytes);
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-strip-layer-composite"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(backdrop),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::TextureView(workspace),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(chrome),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(output),
                },
                wgpu::BindGroupEntry {
                    binding: 4,
                    resource: self.params.as_entire_binding(),
                },
            ],
        });
        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
            label: Some("tilo-strip-layer-composite"),
            timestamp_writes: None,
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &bind_group, &[]);
        pass.dispatch_workgroups(rect.width.div_ceil(8), rect.height.div_ceil(8), 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::frame::FrameLayers;
    use vello::peniko::Color;

    const SIZE: (u32, u32) = (1000, 600);

    fn scene(color: Color) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            color,
            None,
            &Rect::new(10.0, 10.0, 50.0, 50.0),
        );
        scene
    }

    fn strip(scroll_x: u32) -> StripBlit {
        StripBlit {
            dest_x: 100,
            dest_y: 50,
            dest_w: 800,
            dest_h: 400,
            scroll_x,
        }
    }

    fn cache(workspace: Color, strips: Vec<StripBlit>) -> FrameCache {
        FrameCache {
            width: 3000,
            strips,
            layers: FrameLayers {
                backdrop: scene(Color::BLACK),
                workspace: scene(workspace),
                chrome: scene(Color::WHITE),
            },
        }
    }

    #[test]
    fn nothing_shown_renders_every_layer_and_the_whole_output() {
        let work = plan(None, &cache(Color::BLACK, vec![strip(40)]), SIZE, true);
        assert_eq!(work, LayerWork::whole(SIZE));
        assert_eq!(work.raster_pixels(SIZE), 1_800_000);
    }

    #[test]
    fn a_pan_copies_what_stays_and_rasterises_what_it_uncovered() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 7);
        let work = plan(
            Some(&shown),
            &cache(Color::BLACK, vec![strip(52)]),
            SIZE,
            true,
        );
        assert!(!work.backdrop && !work.chrome);
        assert_eq!(
            work.workspace,
            WorkspaceWork::Pan {
                copies: vec![BandCopy {
                    src_x: 112,
                    dst_x: 100,
                    y: 50,
                    width: 788,
                    height: 400,
                }],
                uncovered: vec![FineDamageRect {
                    x: 888,
                    y: 50,
                    width: 12,
                    height: 400,
                }],
            }
        );
        assert_eq!(
            work.composite,
            Some(FineDamageRect {
                x: 100,
                y: 50,
                width: 800,
                height: 400,
            })
        );
        assert_eq!(work.raster_pixels(SIZE), 4_800);

        let back = plan(
            Some(&shown),
            &cache(Color::BLACK, vec![strip(30)]),
            SIZE,
            true,
        );
        let WorkspaceWork::Pan { copies, uncovered } = back.workspace else {
            panic!("a pan back is a pan: {back:?}");
        };
        assert_eq!(
            copies,
            vec![BandCopy {
                src_x: 100,
                dst_x: 110,
                y: 50,
                width: 790,
                height: 400,
            }]
        );
        assert_eq!(
            uncovered,
            vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 10,
                height: 400,
            }]
        );
    }

    #[test]
    fn a_pan_wider_than_the_strip_rasterises_the_strip() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(0)]), 1);
        let work = plan(
            Some(&shown),
            &cache(Color::BLACK, vec![strip(900)]),
            SIZE,
            true,
        );
        assert_eq!(
            work.workspace,
            WorkspaceWork::Pan {
                copies: Vec::new(),
                uncovered: vec![FineDamageRect {
                    x: 100,
                    y: 50,
                    width: 800,
                    height: 400,
                }],
            }
        );
    }

    #[test]
    fn changed_strip_content_or_geometry_rasterises_the_workspace_whole() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
        let content = plan(
            Some(&shown),
            &cache(Color::WHITE, vec![strip(52)]),
            SIZE,
            true,
        );
        assert_eq!(content.workspace, WorkspaceWork::Whole);
        assert_eq!(content.composite, Some(full_rect(SIZE)));
        let mut taller = strip(52);
        taller.dest_h = 420;
        let geometry = plan(Some(&shown), &cache(Color::BLACK, vec![taller]), SIZE, true);
        assert_eq!(geometry.workspace, WorkspaceWork::Whole);
    }

    #[test]
    fn overlapping_strips_are_never_moved_by_copy() {
        let mut lower = strip(0);
        lower.dest_y = 440;
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40), lower]), 1);
        let work = plan(
            Some(&shown),
            &cache(Color::BLACK, vec![strip(52), lower]),
            SIZE,
            true,
        );
        assert_eq!(work.workspace, WorkspaceWork::Whole);
    }

    #[test]
    fn an_unchanged_frame_composites_nothing_unless_the_output_lost_it() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
        let same = cache(Color::BLACK, vec![strip(40)]);
        let work = plan(Some(&shown), &same, SIZE, true);
        assert_eq!(work.workspace, WorkspaceWork::Unchanged);
        assert_eq!(work.composite, None);
        assert_eq!(work.raster_pixels(SIZE), 0);
        let lost = plan(Some(&shown), &same, SIZE, false);
        assert_eq!(lost.composite, Some(full_rect(SIZE)));
        assert_eq!(lost.raster_pixels(SIZE), 0);
    }

    #[test]
    fn changed_chrome_rasterises_the_chrome_and_composites_the_output() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
        let mut next = cache(Color::BLACK, vec![strip(52)]);
        next.layers.chrome = scene(Color::BLACK);
        let work = plan(Some(&shown), &next, SIZE, true);
        assert!(work.chrome && !work.backdrop);
        assert_eq!(work.composite, Some(full_rect(SIZE)));
        assert!(matches!(work.workspace, WorkspaceWork::Pan { .. }));
    }
}
