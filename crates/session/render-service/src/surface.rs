//! One surface of one app: its layers, the retained frame they are drawn
//! into, and the buffers that frame is delivered in.
//!
//! A frame updates layers (new content, new placement, gone), is composed
//! into one scene in layer order, and is rasterized into the surface's
//! retained target exactly as the app's own render worker would have done
//! it (`ui::render_thread::RetainedRaster`): damage only, scroll copies
//! first, backdrops blurred. Then what changed is copied into a buffer the
//! app is not holding — what changed since that buffer last held a frame, so
//! a buffer two frames old gets two frames of damage — and the app attaches
//! it.

use std::collections::{BTreeMap, VecDeque};

use tilo_render_protocol::control::DmabufFormat;
use tilo_render_protocol::frame::{Frame, LayerChange};
use tilo_render_protocol::scene::Resources;
use tilo_render_protocol::{ImageId, LayerId, WireError};
use ui::render_thread::{FrameContent, RetainedRaster, RetainedTarget};
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::{BlendMode, Color, Fill};
use vello::wgpu;

use crate::buffers::{BufferSet, Memory};
use crate::gpu::Gpu;

/// Frames of damage a surface remembers, for buffers that missed them.
const HISTORY: usize = 8;

/// A rectangle of the surface, in pixels, inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl Region {
    fn whole(width: u32, height: u32) -> Self {
        Self {
            x: 0,
            y: 0,
            width,
            height,
        }
    }

    /// `rect` rounded outward and clipped to a `width` × `height` surface;
    /// `None` when nothing of it is inside.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to the surface's u32 extent before narrowing"
    )]
    fn clipped(rect: Rect, width: u32, height: u32) -> Option<Self> {
        let x0 = rect.x0.floor().clamp(0.0, f64::from(width)) as u32;
        let y0 = rect.y0.floor().clamp(0.0, f64::from(height)) as u32;
        let x1 = rect.x1.ceil().clamp(0.0, f64::from(width)) as u32;
        let y1 = rect.y1.ceil().clamp(0.0, f64::from(height)) as u32;
        (x1 > x0 && y1 > y0).then(|| Self {
            x: x0,
            y: y0,
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        })
    }

    pub fn as_wire(self) -> [i32; 4] {
        let wide = |value: u32| i32::try_from(value).unwrap_or(i32::MAX);
        [
            wide(self.x),
            wide(self.y),
            wide(self.width),
            wide(self.height),
        ]
    }
}

struct Layer {
    content: Scene,
    transform: Affine,
    opacity: f32,
    clip: Option<Rect>,
}

impl Layer {
    fn plain(&self) -> bool {
        self.transform == Affine::IDENTITY && self.opacity >= 1.0 && self.clip.is_none()
    }
}

/// The surface's retained frame on the GPU.
struct Target {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

/// A frame drawn into the retained target whose buffer is still to come: the
/// app holds every buffer of the surface.
pub struct Pending {
    pub sequence: u64,
    pub damage: Vec<Region>,
    pub gpu_us: u32,
}

/// A frame copied into a buffer: what the app attaches.
pub struct Delivered {
    pub buffer: u32,
    pub generation: u32,
    pub damage: Vec<Region>,
}

pub struct Surface {
    pub opaque: bool,
    pub formats: Vec<DmabufFormat>,
    layers: BTreeMap<LayerId, Layer>,
    order: Vec<LayerId>,
    raster: RetainedRaster,
    target: Option<Target>,
    target_valid: bool,
    had_backdrop: bool,
    size: (u32, u32),
    pub buffers: Option<BufferSet>,
    generations: u32,
    /// Damage of the last frames, newest last, by frame index.
    history: VecDeque<(u64, Vec<Region>)>,
    frame_index: u64,
    pub pending: Option<Pending>,
    composed: Scene,
}

impl Surface {
    pub fn new(opaque: bool, formats: Vec<DmabufFormat>, on_cpu: bool) -> Self {
        Self {
            opaque,
            formats,
            layers: BTreeMap::new(),
            order: Vec::new(),
            raster: RetainedRaster::new(on_cpu),
            target: None,
            target_valid: false,
            had_backdrop: false,
            size: (0, 0),
            buffers: None,
            generations: 0,
            history: VecDeque::new(),
            frame_index: 0,
            pending: None,
            composed: Scene::new(),
        }
    }

    /// Pixels the surface's target and buffers cover.
    pub fn pixels(&self) -> u64 {
        u64::from(self.size.0).saturating_mul(u64::from(self.size.1))
    }

    /// Take a frame's layer changes.
    ///
    /// # Errors
    ///
    /// Over the layer limit.
    pub fn apply_layers(&mut self, frame: &mut Frame, max_layers: u32) -> Result<(), WireError> {
        for update in frame.layers.drain(..) {
            let transform = update.transform.to_kurbo();
            let clip = update.clip.map(kurbo_rect);
            match update.change {
                LayerChange::Content(content) => {
                    if !self.layers.contains_key(&update.id) {
                        if self.layers.len() >= max_layers as usize {
                            return Err(WireError::OverLimit("layers"));
                        }
                        self.order.push(update.id);
                    }
                    self.layers.insert(
                        update.id,
                        Layer {
                            content: Scene::from(*content),
                            transform,
                            opacity: update.opacity,
                            clip,
                        },
                    );
                }
                LayerChange::Keep => {
                    if let Some(layer) = self.layers.get_mut(&update.id) {
                        layer.transform = transform;
                        layer.opacity = update.opacity;
                        layer.clip = clip;
                    }
                }
                LayerChange::Remove => {
                    self.layers.remove(&update.id);
                    self.order.retain(|id| *id != update.id);
                }
            }
        }
        if let Some(order) = frame.order.take() {
            // Layers the order names that exist, then any it forgot, so a
            // layer is never silently hidden.
            let mut next: Vec<LayerId> = order
                .into_iter()
                .filter(|id| self.layers.contains_key(id))
                .collect();
            next.dedup();
            for id in &self.order {
                if !next.contains(id) {
                    next.push(*id);
                }
            }
            self.order = next;
        }
        Ok(())
    }

    /// Draw `frame` (its layers already applied) into the retained target and
    /// return what changed. `tenant_changed`: the renderer last drew another
    /// app, so its pooled scratch is dropped before this one draws (fresh
    /// buffers are zeroed: nothing of the other app is left to read).
    pub fn rasterize(
        &mut self,
        gpu: Option<&mut Gpu>,
        frame: &mut Frame,
        resources: &dyn Resources,
        tenant_changed: bool,
    ) -> Result<Vec<Region>, String> {
        let (width, height) = (frame.width, frame.height);
        if self.size != (width, height) {
            self.size = (width, height);
            self.target = None;
            self.target_valid = false;
            self.raster.invalidate();
            self.history.clear();
        }
        let backdrop = backdrop_frame(frame, resources).map_err(|error| error.to_string())?;
        let full_redraw =
            frame.full || !self.target_valid || !backdrop.is_empty() || self.had_backdrop;
        let damage_rects: Vec<Rect> = frame.damage.iter().map(|rect| kurbo_rect(*rect)).collect();
        let scroll_copies: Vec<ui::scroll_copy::DeviceCopy> = frame
            .scroll_copies
            .iter()
            .map(|copy| ui::scroll_copy::DeviceCopy {
                x: copy.x,
                y: copy.y,
                width: copy.width,
                height: copy.height,
                dx: copy.dx,
                dy: copy.dy,
            })
            .collect();
        let Self {
            layers,
            order,
            composed,
            target,
            raster,
            ..
        } = self;
        let scene = compose(layers, order, (width, height), composed);
        let content = FrameContent {
            scene,
            width,
            height,
            base_color: Color::new(frame.base_color),
            damage_regions: &damage_rects,
            scroll_copies: &scroll_copies,
            full_redraw,
            backdrop: &backdrop,
        };
        let result = match gpu {
            Some(gpu) => {
                if tenant_changed {
                    gpu.renderer.trim_pooled_buffers();
                }
                let target = target.get_or_insert_with(|| new_target(&gpu.device, width, height));
                raster
                    .rasterize(
                        (&gpu.device, &gpu.queue),
                        &mut gpu.renderer,
                        RetainedTarget {
                            texture: &target.texture,
                            view: &target.view,
                        },
                        content,
                    )
                    .0
                    .map(|_| ())
            }
            None => raster.rasterize_cpu(content).map(|_| ()),
        };
        self.had_backdrop = !backdrop.is_empty();
        if let Err(error) = result {
            self.target_valid = false;
            return Err(error);
        }
        self.target_valid = true;
        let changed = if full_redraw {
            vec![Region::whole(width, height)]
        } else {
            frame
                .damage
                .iter()
                .filter_map(|rect| Region::clipped(kurbo_rect(*rect), width, height))
                .chain(frame.scroll_copies.iter().filter_map(|copy| {
                    let x = copy.x.checked_add_signed(copy.dx)?;
                    let y = copy.y.checked_add_signed(copy.dy)?;
                    Some(Region {
                        x,
                        y,
                        width: copy.width,
                        height: copy.height,
                    })
                }))
                .collect()
        };
        self.frame_index = self.frame_index.saturating_add(1);
        self.history.push_back((self.frame_index, changed.clone()));
        while self.history.len() > HISTORY {
            self.history.pop_front();
        }
        Ok(changed)
    }

    /// The buffers for the current size, made when there are none of this
    /// size. Returns whether they are new (the app must be told).
    ///
    /// # Errors
    ///
    /// When buffers cannot be allocated.
    pub fn ensure_buffers(
        &mut self,
        gpu: Option<&Gpu>,
    ) -> Result<bool, crate::buffers::BufferError> {
        let (width, height) = self.size;
        if self
            .buffers
            .as_ref()
            .is_some_and(|set| (set.width, set.height) == (width, height))
        {
            return Ok(false);
        }
        self.generations = self.generations.wrapping_add(1);
        let set = match gpu {
            #[cfg(target_os = "linux")]
            Some(gpu) if gpu.dmabufs() => {
                let format = crate::buffers::dmabuf_format(self.opaque, &self.formats)
                    .ok_or(crate::buffers::BufferError::NoFormat)?;
                let allocator = gpu
                    .allocator
                    .as_ref()
                    .ok_or(crate::buffers::BufferError::NoFormat)?;
                BufferSet::dmabufs(
                    (&gpu.device, allocator),
                    (width, height),
                    format,
                    self.generations,
                )?
            }
            _ => BufferSet::shm((width, height), self.opaque, self.generations)?,
        };
        self.buffers = Some(set);
        Ok(true)
    }

    /// The app released `buffer` of `generation`.
    pub fn release(&mut self, generation: u32, buffer: u32) {
        if let Some(set) = self
            .buffers
            .as_mut()
            .filter(|set| set.generation == generation)
            && let Some(buffer) = set.buffers.get_mut(buffer as usize)
        {
            buffer.busy = false;
        }
    }

    /// Copy the retained frame into a free buffer and hand it to the app.
    /// `None` when the app holds every buffer: the frame waits as
    /// [`Self::pending`] for a release. On a GPU the copy is submitted, not
    /// waited for; the caller waits once for every surface it delivered.
    pub fn deliver(&mut self, gpu: Option<&Gpu>, damage: &[Region]) -> Option<Delivered> {
        let set = self.buffers.as_mut()?;
        let index = set.free()?;
        let (width, height) = (set.width, set.height);
        let frame_index = self.frame_index;
        let buffer = set.buffers.get_mut(index)?;
        // What changed since the buffer last held a frame; all of it when it
        // never did or that frame left the history.
        let copy: Vec<Region> = match buffer.holds {
            Some(held)
                if self
                    .history
                    .front()
                    .is_some_and(|(first, _)| *first <= held.saturating_add(1)) =>
            {
                self.history
                    .iter()
                    .filter(|(index, _)| *index > held)
                    .flat_map(|(_, regions)| regions.iter().copied())
                    .collect()
            }
            _ => vec![Region::whole(width, height)],
        };
        match (&mut buffer.memory, gpu, self.target.as_ref()) {
            #[cfg(target_os = "linux")]
            (Memory::Dmabuf(dmabuf), Some(gpu), Some(target)) => {
                let imported = &dmabuf.target;
                let mut encoder =
                    gpu.device
                        .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                            label: Some("render-service-deliver"),
                        });
                for region in &copy {
                    encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &target.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: region.x,
                                y: region.y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &imported.texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: region.x,
                                y: region.y,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: region.width,
                            height: region.height,
                            depth_or_array_layers: 1,
                        },
                    );
                }
                gpu.queue.submit([encoder.finish()]);
            }
            (Memory::Shm(memory), _, _) => {
                let pixels = self.raster.cpu_pixels()?;
                copy_swizzled(pixels, memory.as_mut_slice(), width, &copy);
            }
            #[cfg(target_os = "linux")]
            _ => return None,
        }
        buffer.busy = true;
        buffer.holds = Some(frame_index);
        Some(Delivered {
            buffer: u32::try_from(index).ok()?,
            generation: set.generation,
            damage: damage.to_vec(),
        })
    }
}

fn kurbo_rect(rect: tilo_render_protocol::frame::Rect) -> Rect {
    Rect::new(
        f64::from(rect.x0),
        f64::from(rect.y0),
        f64::from(rect.x1),
        f64::from(rect.y1),
    )
}

/// A surface's layers as one scene: the one plain layer itself, or the
/// layers composed into `composed` in order.
fn compose<'a>(
    layers: &'a BTreeMap<LayerId, Layer>,
    order: &[LayerId],
    (width, height): (u32, u32),
    composed: &'a mut Scene,
) -> &'a Scene {
    if let [only] = order
        && let Some(layer) = layers.get(only).filter(|layer| layer.plain())
    {
        return &layer.content;
    }
    composed.reset();
    for layer in order.iter().filter_map(|id| layers.get(id)) {
        let isolated = layer.opacity < 1.0 || layer.clip.is_some();
        if isolated {
            let clip = layer
                .clip
                .unwrap_or_else(|| Rect::new(0.0, 0.0, f64::from(width), f64::from(height)));
            composed.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                layer.opacity,
                Affine::IDENTITY,
                &clip,
            );
        }
        composed.append(
            &layer.content,
            (layer.transform != Affine::IDENTITY).then_some(layer.transform),
        );
        if isolated {
            composed.pop_layer();
        }
    }
    composed
}

fn new_target(device: &wgpu::Device, width: u32, height: u32) -> Target {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("render-service-retained-target"),
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
            | wgpu::TextureUsages::COPY_DST
            | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
    Target { texture, view }
}

/// Premultiplied RGBA rows into a `wl_shm` (A|X)RGB8888 buffer, which is
/// B, G, R, A in memory.
fn copy_swizzled(source: &[u8], target: &mut [u8], width: u32, regions: &[Region]) {
    let stride = (width as usize).saturating_mul(4);
    for region in regions {
        for row in region.y..region.y.saturating_add(region.height) {
            let start = (row as usize)
                .saturating_mul(stride)
                .saturating_add((region.x as usize).saturating_mul(4));
            let end = start.saturating_add((region.width as usize).saturating_mul(4));
            let (Some(from), Some(to)) = (source.get(start..end), target.get_mut(start..end))
            else {
                continue;
            };
            for (pixel_in, pixel_out) in from.chunks_exact(4).zip(to.chunks_exact_mut(4)) {
                if let ([r, g, b, a], [ob, og, or, oa]) = (pixel_in, pixel_out) {
                    *ob = *b;
                    *og = *g;
                    *or = *r;
                    *oa = *a;
                }
            }
        }
    }
}

/// The frame's backdrops, with their placeholder images resolved.
fn backdrop_frame(
    frame: &mut Frame,
    resources: &dyn Resources,
) -> Result<ui::backdrop::BackdropFrame, WireError> {
    let regions = std::mem::take(&mut frame.backdrops)
        .into_iter()
        .map(|backdrop| {
            let image = resources
                .image(ImageId(backdrop.image.0))
                .ok_or(WireError::UnknownResource("a backdrop image"))?;
            Ok(ui::backdrop::BackdropRegion {
                image,
                rect: kurbo_rect(backdrop.rect),
                blur_px: f64::from(backdrop.blur_px),
                below: Scene::from(backdrop.below),
                ground: Color::new(backdrop.ground),
            })
        })
        .collect::<Result<_, WireError>>()?;
    Ok(ui::backdrop::BackdropFrame { regions })
}
