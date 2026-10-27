//! Rasterizing frames into one retained target: only a frame's damage (or
//! all of it), its scroll copies made first and its backdrops blurred. Every
//! host that keeps the pixels it presented draws through [`RetainedRaster`]:
//! the native app's render worker, the room's render service and the
//! browser desktop, so they produce the same pixels from the same frame.

use crate::damage::{Region, coalesce_regions};
use crate::offscreen::{
    damage_geometric_capacity_enabled, damage_texture_extent,
    damage_texture_geometric_needs_replacement, damage_texture_geometric_width_extent,
    damage_texture_height_extent, damage_texture_needs_replacement,
};
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::{RenderParams, Renderer, Scene};

/// The CPU raster a software adapter's frames are drawn with: the native
/// render worker's. The native feature is only ever enabled off the web.
#[cfg(feature = "native")]
use crate::render_thread::cpu_raster;

/// A host without the native CPU raster (the browser) never makes a raster
/// on the CPU: the canvas is a type with no values.
#[cfg(not(feature = "native"))]
mod cpu_raster {
    use super::FrameContent;

    pub(super) struct CpuCanvas(std::convert::Infallible);

    pub(super) fn canvas(_: bool) -> Option<CpuCanvas> {
        None
    }

    impl CpuCanvas {
        pub(super) fn invalidate(&self) {
            match self.0 {}
        }
        pub(super) fn rasterize(&self, _: &FrameContent<'_>) -> Result<u64, String> {
            match self.0 {}
        }
        pub(super) fn pixels(&self) -> &[u8] {
            match self.0 {}
        }
        pub(super) fn upload(&self, _: &vello::wgpu::Queue, _: &vello::wgpu::Texture) {
            match self.0 {}
        }
        #[expect(
            clippy::needless_pass_by_ref_mut,
            reason = "the same signature as the native canvas, which records the setting"
        )]
        pub(super) fn draw_damage_only(&mut self, _: bool) {
            match self.0 {}
        }
    }
}

struct DamageTarget {
    width: u32,
    height: u32,
    texture: vello::wgpu::Texture,
    view: vello::wgpu::TextureView,
}

#[derive(Default)]
pub(crate) struct DamageCache {
    targets: Vec<DamageTarget>,
    last_region_count: Option<usize>,
    geometric_capacity: bool,
    coarse_pool_high_water_pixels: u64,
    /// Scratch space for scroll copies within the retained target.
    copy_scratch: crate::scroll_copy::CopyScratch,
    /// The frame's scene holds only what its damage needs
    /// ([`RetainedRaster::rasterize_damage_only`]).
    damage_only: bool,
    /// A frame drawn for its damage only would have had to repaint more:
    /// nothing was drawn.
    refused: bool,
}

/// Why a frame drawn for its damage only was not rasterized.
pub const DAMAGE_ONLY_REFUSED: &str =
    "a scene drawn for its damage only cannot repaint more than the damage";

/// Whether a frame of `width` × `height` with `damage` (physical pixels,
/// not empty) is repainted region by region rather than whole: a damage
/// that covers half the target, or whose window does, is rendered as one
/// whole frame ([`repaint_damage`]).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "damage bounds are clamped to the u32 surface before narrowing"
)]
pub(crate) fn repaints_alone(width: u32, height: u32, damage: &[Rect]) -> bool {
    let full_pixels = u64::from(width).saturating_mul(u64::from(height));
    let mut damage_pixels = 0u64;
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for rect in damage {
        let rx0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
        let ry0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
        let rx1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
        let ry1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
        let (region_width, region_height) = (rx1.saturating_sub(rx0), ry1.saturating_sub(ry0));
        damage_pixels = damage_pixels
            .saturating_add(u64::from(region_width).saturating_mul(u64::from(region_height)));
        if region_width != 0 && region_height != 0 {
            (x0, y0, x1, y1) = (x0.min(rx0), y0.min(ry0), x1.max(rx1), y1.max(ry1));
        }
    }
    if damage_pixels == 0 || damage_pixels >= full_pixels.saturating_div(2) {
        return false;
    }
    let window_width = x1.saturating_sub(x0 / RASTER_TILE * RASTER_TILE);
    let window_height = y1.saturating_sub(y0 / RASTER_TILE * RASTER_TILE);
    u64::from(window_width).saturating_mul(u64::from(window_height)) < full_pixels.saturating_div(2)
}

/// What a host keeps between frames to rasterize them into one retained
/// target: the damage scratch, the backdrop blur and, on a software adapter,
/// the CPU canvas. The app's own render worker draws with it, and so does the
/// room's render service for each surface it draws for an app
/// (`tilo-render-service`), so both produce the same pixels from the same
/// frame.
pub struct RetainedRaster {
    damage_cache: DamageCache,
    /// Built with the first frame that draws a floating surface over the
    /// blurred material.
    backdrop_blur: Option<crate::backdrop::BackdropBlur>,
    cpu: Option<cpu_raster::CpuCanvas>,
}

impl RetainedRaster {
    /// Scratch for one target; `on_cpu` rasterizes on the CPU and uploads the
    /// pixels (a software adapter, see
    /// `render_thread::rasterizes_on_cpu`), where the host has a CPU raster.
    pub fn new(on_cpu: bool) -> Self {
        Self {
            damage_cache: DamageCache {
                geometric_capacity: damage_geometric_capacity_enabled(),
                ..DamageCache::default()
            },
            backdrop_blur: None,
            cpu: cpu_raster::canvas(on_cpu),
        }
    }

    /// Whether frames are rasterized on the CPU.
    pub fn on_cpu(&self) -> bool {
        self.cpu.is_some()
    }

    /// The target was replaced (a resize): nothing retained is current.
    pub fn invalidate(&mut self) {
        self.damage_cache = DamageCache {
            geometric_capacity: self.damage_cache.geometric_capacity,
            ..DamageCache::default()
        };
        if let Some(canvas) = self.cpu.as_mut() {
            canvas.invalidate();
        }
    }

    /// Bring `target` up to date with `frame`: its backdrops blurred and
    /// substituted, its scroll copies made, its damage (or all of it)
    /// repainted. Returns the pixels rasterized, and the backdrop pass's
    /// statistics when the frame had backdrops.
    pub fn rasterize(
        &mut self,
        (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        frame: FrameContent<'_>,
    ) -> (Result<u64, String>, Option<crate::backdrop::BackdropStats>) {
        // Floating surfaces over the blurred material: render, blur and
        // substitute each one's backdrop before the frame that draws it.
        let backdrop_stats = (!frame.backdrop.is_empty()).then(|| {
            self.backdrop_blur
                .get_or_insert_with(|| crate::backdrop::BackdropBlur::new(device))
                .prepare(
                    device,
                    queue,
                    renderer,
                    frame.backdrop,
                    (frame.width, frame.height),
                    frame.base_color,
                )
        });
        let raster_frame = FrameContent {
            backdrop: &crate::backdrop::BackdropFrame::default(),
            ..frame
        };
        let rasterized =
            if let Some(canvas) = self.cpu.as_mut().filter(|_| frame.backdrop.is_empty()) {
                canvas.rasterize(&raster_frame).inspect(|_| {
                    canvas.upload(queue, target.texture);
                })
            } else {
                // Backdrop substitutions currently belong to Vello's GPU renderer.
                // Invalidate retained CPU pixels when that path writes the target.
                if let Some(canvas) = self.cpu.as_mut() {
                    canvas.invalidate();
                }
                rasterize_damage(
                    (device, queue),
                    target,
                    renderer,
                    &mut self.damage_cache,
                    &raster_frame,
                )
                .map_err(|error| error.to_string())
            };
        if let Some(blur) = self.backdrop_blur.as_mut() {
            blur.release(renderer);
        }
        (rasterized, backdrop_stats)
    }

    /// [`Self::rasterize`] for a frame whose scene holds only what its
    /// damage needs (`crate::draw_cull`): it is never repainted beyond its
    /// damage. `Err` with [`DAMAGE_ONLY_REFUSED`] when it would have to be
    /// (a whole frame, a backdrop, a copy that cannot be made, a damage too
    /// large to repaint alone); the target then still holds the last frame.
    pub fn rasterize_damage_only(
        &mut self,
        gpu: (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        frame: FrameContent<'_>,
    ) -> (Result<u64, String>, Option<crate::backdrop::BackdropStats>) {
        if frame.full_redraw
            || !frame.backdrop.is_empty()
            || !repaints_alone(frame.width, frame.height, frame.damage_regions)
        {
            return (Err(DAMAGE_ONLY_REFUSED.to_owned()), None);
        }
        self.damage_cache.damage_only = true;
        self.damage_cache.refused = false;
        if let Some(canvas) = self.cpu.as_mut() {
            canvas.draw_damage_only(true);
        }
        let (rasterized, stats) = self.rasterize(gpu, renderer, target, frame);
        self.damage_cache.damage_only = false;
        if let Some(canvas) = self.cpu.as_mut() {
            canvas.draw_damage_only(false);
        }
        if std::mem::take(&mut self.damage_cache.refused) {
            return (Err(DAMAGE_ONLY_REFUSED.to_owned()), stats);
        }
        (rasterized, stats)
    }

    /// Bring `target` up to date with `frame` on the GPU, repainting only
    /// its damage (or all of it): for a host whose frames have no scroll
    /// copies or backdrops and that never rasterizes on the CPU (the
    /// browser), which then carries none of that code.
    ///
    /// # Errors
    ///
    /// When the frame cannot be rendered; what the target holds is then
    /// unknown.
    pub fn repaint(
        &mut self,
        gpu: (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        frame: FrameContent<'_>,
    ) -> Result<u64, String> {
        debug_assert!(
            frame.scroll_copies.is_empty() && frame.backdrop.is_empty() && self.cpu.is_none(),
            "repaint makes no scroll copy, blurs no backdrop and draws on the GPU"
        );
        repaint_damage(gpu, target, renderer, &mut self.damage_cache, &frame)
            .map_err(|error| error.to_string())
    }

    /// The CPU canvas after [`Self::rasterize_cpu`]: premultiplied RGBA8
    /// rows of `width * 4` bytes, or `None` on a GPU raster.
    pub fn cpu_pixels(&self) -> Option<&[u8]> {
        self.cpu.as_ref().map(cpu_raster::CpuCanvas::pixels)
    }

    /// [`Self::rasterize`] for a host without a GPU, on a raster made
    /// [`on_cpu`](Self::on_cpu): the canvas itself is the target, returned as
    /// premultiplied RGBA8 rows of `width * 4` bytes. Backdrops are not
    /// blurred; their placeholders show the opaque material.
    ///
    /// # Errors
    ///
    /// When the raster is not on the CPU or the frame cannot be rasterized.
    pub fn rasterize_cpu(&mut self, frame: FrameContent<'_>) -> Result<&[u8], String> {
        let canvas = self
            .cpu
            .as_mut()
            .ok_or_else(|| "this raster draws on a GPU".to_owned())?;
        canvas.rasterize(&FrameContent {
            backdrop: &crate::backdrop::BackdropFrame::default(),
            ..frame
        })?;
        Ok(canvas.pixels())
    }
}

/// The retained frame the native host rasterizes into and presents from.
/// Pixels outside a frame's damage are whatever the previous frame left, so
/// it must only ever be written through [`RetainedRaster::rasterize`].
/// `Rgba8Unorm` with storage, copy-source and copy-destination usage.
#[derive(Clone, Copy)]
pub struct RetainedTarget<'a> {
    pub texture: &'a vello::wgpu::Texture,
    pub view: &'a vello::wgpu::TextureView,
}

/// Bring `retained` up to date with `frame`: its scroll copies made, then
/// its damage (or all of it) repainted ([`repaint_damage`]).
pub(crate) fn rasterize_damage(
    (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
    retained: RetainedTarget<'_>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: &FrameContent<'_>,
) -> Result<u64, vello::Error> {
    // What a scroll keeps on screen moves before the damage around it is
    // repainted; the target holds the last presented frame.
    if !frame.full_redraw && !frame.scroll_copies.is_empty() {
        let mut encoder = device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
            label: Some("ui-native-scroll-copies"),
        });
        if !crate::scroll_copy::encode_copies(
            device,
            &mut encoder,
            retained.texture,
            &mut damage_cache.copy_scratch,
            frame.scroll_copies,
        ) {
            if damage_cache.damage_only {
                damage_cache.refused = true;
                return Ok(0);
            }
            let full = FrameContent {
                full_redraw: true,
                ..*frame
            };
            return repaint_damage((device, queue), retained, renderer, damage_cache, &full);
        }
        queue.submit([encoder.finish()]);
    }
    repaint_damage((device, queue), retained, renderer, damage_cache, frame)
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::expect_used,
    clippy::too_many_lines,
    reason = "damage bounds are clamped to the u32 surface before narrowing; one bounded GPU transaction keeps retained-target correctness visible in one place"
)]
fn repaint_damage(
    (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
    retained: RetainedTarget<'_>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: &FrameContent<'_>,
) -> Result<u64, vello::Error> {
    let FrameContent {
        scene,
        width,
        height,
        base_color,
        damage_regions,
        full_redraw,
        ..
    } = *frame;
    let full_pixels = u64::from(width).saturating_mul(u64::from(height));
    // The whole frame, after the partial targets and pools are let go. A
    // scene drawn for its damage only is not repainted whole.
    let render_full = |renderer: &mut Renderer, damage_cache: &mut DamageCache| {
        if damage_cache.damage_only {
            damage_cache.refused = true;
            return Ok(0);
        }
        if damage_cache
            .last_region_count
            .is_some_and(|previous| previous != 0)
        {
            renderer.trim_pooled_buffers();
        }
        while let Some(target) = damage_cache.targets.pop() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        damage_cache.last_region_count = Some(0);
        damage_cache.coarse_pool_high_water_pixels = full_pixels;
        renderer.render_to_texture(
            device,
            queue,
            scene,
            retained.view,
            &RenderParams {
                base_color,
                width,
                height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )?;
        Ok(full_pixels)
    };
    if full_redraw {
        return render_full(renderer, damage_cache);
    }
    if damage_regions.is_empty() {
        return Ok(0);
    }

    let mut damage_pixels = 0u64;
    let mut damage_region_count = 0usize;
    for rect in damage_regions {
        let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
        let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
        let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
        let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
        let region_width = x1.saturating_sub(x0);
        let region_height = y1.saturating_sub(y0);
        damage_pixels = damage_pixels
            .saturating_add(u64::from(region_width).saturating_mul(u64::from(region_height)));
        damage_region_count = damage_region_count
            .saturating_add(usize::from(region_width != 0 && region_height != 0));
    }
    // A large union costs less as one Vello pass and must not retain a second
    // near-window-sized texture. This is both the time and memory bound.
    if damage_pixels == 0 || damage_pixels >= full_pixels.saturating_div(2) {
        return render_full(renderer, damage_cache);
    }

    let regions = coalesce_regions(
        damage_regions
            .iter()
            .filter_map(|rect| {
                let x0 = rect.x0.floor().max(0.0).min(f64::from(width)) as u32;
                let y0 = rect.y0.floor().max(0.0).min(f64::from(height)) as u32;
                let x1 = rect.x1.ceil().max(0.0).min(f64::from(width)) as u32;
                let y1 = rect.y1.ceil().max(0.0).min(f64::from(height)) as u32;
                let region_width = x1.saturating_sub(x0);
                let region_height = y1.saturating_sub(y0);
                (region_width != 0 && region_height != 0).then_some(Region {
                    x: x0,
                    y: y0,
                    width: region_width,
                    height: region_height,
                })
            })
            .collect(),
    );
    let prefix_width = regions
        .iter()
        .map(|r| r.x.saturating_add(r.width))
        .max()
        .unwrap_or(0);
    let prefix_height = regions
        .iter()
        .map(|r| r.y.saturating_add(r.height))
        .max()
        .unwrap_or(0);
    // Vello's sparse fine pass is a second render path the web build would
    // carry for this alone; it repaints through the window below instead.
    // The native feature is only ever enabled off the web.
    #[cfg(feature = "native")]
    if renderer.sparse_fine_supported() {
        let raster_pixels = u64::from(prefix_width).saturating_mul(u64::from(prefix_height));
        if damage_cache.coarse_pool_high_water_pixels >= full_pixels.saturating_div(2)
            && raster_pixels.saturating_mul(4) <= damage_cache.coarse_pool_high_water_pixels
        {
            renderer.trim_pooled_buffers();
            damage_cache.coarse_pool_high_water_pixels = 0;
        }
        let fine_damage: Vec<vello::FineDamageRect> = regions
            .iter()
            .map(|region| vello::FineDamageRect {
                x: region.x,
                y: region.y,
                width: region.width,
                height: region.height,
            })
            .collect();
        if let Some(stats) = renderer.render_to_texture_with_damage(
            device,
            queue,
            scene,
            retained.view,
            &RenderParams {
                base_color,
                width: prefix_width,
                height: prefix_height,
                antialiasing_method: vello::AaConfig::Area,
            },
            &fine_damage,
        )? {
            while let Some(target) = damage_cache.targets.pop() {
                renderer.invalidate_texture_bindings(&target.view);
            }
            damage_cache.last_region_count = Some(damage_region_count);
            damage_cache.coarse_pool_high_water_pixels = damage_cache
                .coarse_pool_high_water_pixels
                .max(stats.coarse_pixels);
            return Ok(stats.fine_pixels);
        }
    }
    // The window around the damage, from a tile corner so its tiles are the
    // full frame's and its pixels the same: rendered alone, shifted to the
    // target's origin, then copied into place region by region.
    let origin = |at: fn(&Region) -> u32| {
        regions.iter().map(at).min().unwrap_or(0) / RASTER_TILE * RASTER_TILE
    };
    let (origin_x, origin_y) = (origin(|r| r.x), origin(|r| r.y));
    let window_width = prefix_width.saturating_sub(origin_x);
    let window_height = prefix_height.saturating_sub(origin_y);
    let raster_pixels = u64::from(window_width).saturating_mul(u64::from(window_height));
    if raster_pixels >= full_pixels.saturating_div(2) {
        return render_full(renderer, damage_cache);
    }
    // Damage topology is volatile; preserve warm partial pools, but discard
    // full-target scratch before switching to the smaller window target.
    if damage_cache.last_region_count == Some(0) {
        renderer.trim_pooled_buffers();
    }
    damage_cache.last_region_count = Some(damage_region_count);
    damage_cache.coarse_pool_high_water_pixels = damage_cache
        .coarse_pool_high_water_pixels
        .max(raster_pixels);
    let texture_limit = device.limits().max_texture_dimension_2d;
    let allocated_width = if damage_cache.geometric_capacity {
        damage_texture_geometric_width_extent(window_width, width, texture_limit)
    } else {
        damage_texture_extent(window_width, texture_limit)
    };
    let allocated_height = damage_texture_height_extent(window_height, texture_limit);
    let recreate = damage_cache.targets.first().is_none_or(|target| {
        if damage_cache.geometric_capacity {
            damage_texture_geometric_needs_replacement(
                target.width,
                target.height,
                window_width,
                window_height,
                allocated_width,
                allocated_height,
            )
        } else {
            damage_texture_needs_replacement(
                target.width,
                target.height,
                window_width,
                window_height,
            )
        }
    });
    if recreate {
        if let Some(target) = damage_cache.targets.first() {
            renderer.invalidate_texture_bindings(&target.view);
        }
        let texture = device.create_texture(&vello::wgpu::TextureDescriptor {
            label: Some("ui-native-damage-target"),
            size: vello::wgpu::Extent3d {
                width: allocated_width,
                height: allocated_height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: vello::wgpu::TextureDimension::D2,
            format: vello::wgpu::TextureFormat::Rgba8Unorm,
            usage: vello::wgpu::TextureUsages::STORAGE_BINDING
                | vello::wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&vello::wgpu::TextureViewDescriptor::default());
        let replacement = DamageTarget {
            width: allocated_width,
            height: allocated_height,
            texture,
            view,
        };
        if let Some(target) = damage_cache.targets.first_mut() {
            *target = replacement;
        } else {
            damage_cache.targets.push(replacement);
        }
    }
    let target = damage_cache
        .targets
        .first()
        .expect("damage target was inserted above");
    let shifted;
    let window_scene = if origin_x == 0 && origin_y == 0 {
        scene
    } else {
        let mut scene_at_origin = Scene::new();
        scene_at_origin.append(
            scene,
            Some(vello::kurbo::Affine::translate((
                -f64::from(origin_x),
                -f64::from(origin_y),
            ))),
        );
        shifted = scene_at_origin;
        &shifted
    };
    // Rendered as a full frame is, then copied in a submission of its own:
    // one render path for both, which the web build carries once.
    renderer.render_to_texture(
        device,
        queue,
        window_scene,
        &target.view,
        &RenderParams {
            base_color,
            width: window_width,
            height: window_height,
            antialiasing_method: vello::AaConfig::Area,
        },
    )?;
    let mut encoder =
        device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor::default());
    for region in &regions {
        encoder.copy_texture_to_texture(
            vello::wgpu::TexelCopyTextureInfo {
                texture: &target.texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d {
                    x: region.x - origin_x,
                    y: region.y - origin_y,
                    z: 0,
                },
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::TexelCopyTextureInfo {
                texture: retained.texture,
                mip_level: 0,
                origin: vello::wgpu::Origin3d {
                    x: region.x,
                    y: region.y,
                    z: 0,
                },
                aspect: vello::wgpu::TextureAspect::All,
            },
            vello::wgpu::Extent3d {
                width: region.width,
                height: region.height,
                depth_or_array_layers: 1,
            },
        );
    }
    queue.submit([encoder.finish()]);
    while damage_cache.targets.len() > 1 {
        let target = damage_cache.targets.pop().expect("length checked above");
        renderer.invalidate_texture_bindings(&target.view);
    }
    Ok(raster_pixels)
}

/// Vello's tile edge in pixels: a window rendered from a multiple of it
/// bins its paths into the same tiles as the full frame.
const RASTER_TILE: u32 = 16;

/// One frame's scene, surface size and damage, as the worker received it.
#[derive(Clone, Copy)]
pub struct FrameContent<'a> {
    pub scene: &'a Scene,
    pub width: u32,
    pub height: u32,
    pub base_color: Color,
    /// Physical-pixel rectangles that changed.
    pub damage_regions: &'a [Rect],
    /// Copies within the target before the damage is repainted.
    pub scroll_copies: &'a [crate::scroll_copy::DeviceCopy],
    /// Repaint everything: the target is not current, or the frame says so.
    pub full_redraw: bool,
    pub backdrop: &'a crate::backdrop::BackdropFrame,
}

/// What a target holds and how a scene is placed on it: a frame drawn for
/// another key cannot be repainted over the last one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetKey {
    pub width: u32,
    pub height: u32,
    /// From scene coordinates to the target's pixels (the display scale).
    pub to_device: vello::kurbo::Affine,
    pub base_color: Color,
    /// Changed by the host whenever the target's pixels may no longer be
    /// the last frame's: a new texture or device, a theme, a frame drawn
    /// but not presented.
    pub generation: u64,
}

/// What of a frame to repaint over the one presented before.
#[derive(Clone, Debug, PartialEq)]
pub enum Repaint {
    /// Everything: there is no frame before, or what changed is not known.
    Full(Unknown),
    /// Only these target pixels (none: nothing changed).
    Regions(Vec<Rect>),
}

/// Why a frame is repainted in full.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unknown {
    /// The target shows no frame drawn with parts.
    NoFrame,
    /// The frame was drawn without parts.
    NoParts,
    /// The target or the scene's placement on it changed.
    Target,
    /// The frames cannot be compared part by part.
    Unproven(crate::scene_parts::Unproven),
    /// The renderer dropped geometry from a frame drawn since the target
    /// was last repainted whole ([`RetainedFrames::note_overflows`]).
    LostGeometry,
}

/// The frame a host presented last, to compare the next with. Where a scene
/// drawn with damage parts ([`crate::scene_parts`]) changed is the parts
/// whose commands differ (or what a hinted part says changed); every other
/// pixel is drawn by the same commands as before. Anything that cannot be
/// proven (no parts, a change outside every part, another target) repaints
/// the whole frame.
#[derive(Default)]
pub struct RetainedFrames {
    last: Option<(Scene, crate::scene_parts::SectionParts, TargetKey)>,
    /// The target holds the frame last presented, parts or not.
    shown: bool,
    /// The renderer's overflows ([`crate::render_bump_recovery::Overflows`])
    /// the target was last repainted whole for.
    overflows: u64,
    /// The target may show a frame with geometry missing.
    lost: bool,
}

impl RetainedFrames {
    /// What to repaint of `scene`, drawn with `parts`, onto a target that
    /// shows the frame last [`Self::presented`].
    pub fn repaint(
        &self,
        scene: &Scene,
        parts: Option<&crate::scene_parts::SectionParts>,
        key: TargetKey,
    ) -> Repaint {
        if self.lost {
            return Repaint::Full(Unknown::LostGeometry);
        }
        let Some(parts) = parts else {
            return Repaint::Full(Unknown::NoParts);
        };
        let Some((last, last_parts, last_key)) = &self.last else {
            return Repaint::Full(Unknown::NoFrame);
        };
        if *last_key != key {
            return Repaint::Full(Unknown::Target);
        }
        let changed = match crate::scene_parts::damage(last, last_parts, scene, parts) {
            Ok(changed) => changed,
            Err(unproven) => return Repaint::Full(Unknown::Unproven(unproven)),
        };
        let target = Rect::new(0.0, 0.0, f64::from(key.width), f64::from(key.height));
        Repaint::Regions(
            changed
                .into_iter()
                // Part bounds cover antialiasing; whole pixels plus one
                // absorb rounding.
                .map(|rect| {
                    key.to_device
                        .transform_rect_bbox(rect)
                        .expand()
                        .inflate(1.0, 1.0)
                        .intersect(target)
                })
                .filter(|rect| !rect.is_zero_area())
                .collect(),
        )
    }

    /// `scene`, drawn with `parts`, is what the target with `key` shows now.
    pub fn presented(
        &mut self,
        scene: &Scene,
        parts: Option<&crate::scene_parts::SectionParts>,
        key: TargetKey,
    ) {
        self.last = parts.map(|parts| (scene.clone(), parts.clone(), key));
        self.shown = true;
        self.lost = false;
    }

    /// Whether the renderer has collected an overflow, `collected` in all,
    /// that the target was not yet repainted whole for: a frame it shows may
    /// have geometry missing.
    #[must_use]
    pub const fn shows_lost_geometry(&self, collected: u64) -> bool {
        self.lost || self.overflows != collected
    }

    /// The renderer has collected `collected` overflows in all: after a new
    /// one, the next frame repaints the whole target. Pixels drawn with
    /// geometry missing stay on it otherwise, since later frames repaint
    /// only what changes.
    pub fn note_overflows(&mut self, collected: u64) {
        if self.overflows != collected {
            self.overflows = collected;
            self.lost = true;
        }
    }

    /// The target no longer shows the last frame presented.
    pub fn forget(&mut self) {
        self.last = None;
        self.shown = false;
    }

    /// Whether the target still holds the frame last presented: a host
    /// that acquired a surface texture for a frame it then skipped puts
    /// that on it again ([`crate::gpu::submit_target_to_surface`]) rather
    /// than presenting a cleared one.
    #[must_use]
    pub const fn shows_frame(&self) -> bool {
        self.shown
    }
}

/// A check that a retained target shows what a full render of the same
/// frame does: a frame repainted only where it changed must leave no stale
/// pixel. Debug and test builds run it on the frames they repaint in part.
/// The two images are read back asynchronously; [`Self::differing`] answers
/// once both are in.
pub struct RasterCheck {
    retained: vello::wgpu::Buffer,
    full: vello::wgpu::Buffer,
    width: u32,
    height: u32,
    row: u32,
    mapped: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

impl RasterCheck {
    /// Render `scene` in full beside `target`, which must already show it,
    /// and start reading both back.
    ///
    /// # Errors
    ///
    /// When the full render fails.
    pub fn start(
        (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        scene: &Scene,
        (width, height): (u32, u32),
        base_color: Color,
    ) -> Result<Self, String> {
        use vello::wgpu;
        let full = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("ui-retained-check"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = full.create_view(&wgpu::TextureViewDescriptor::default());
        renderer
            .render_to_texture(
                device,
                queue,
                scene,
                &view,
                &RenderParams {
                    base_color,
                    width,
                    height,
                    antialiasing_method: vello::AaConfig::Area,
                },
            )
            .map_err(|error| error.to_string())?;
        renderer.invalidate_texture_bindings(&view);
        let row = (width * 4).div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buffer = || {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("ui-retained-check-readback"),
                size: u64::from(row) * u64::from(height),
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            })
        };
        let (retained, full_pixels) = (buffer(), buffer());
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ui-retained-check"),
        });
        for (texture, buffer) in [(target.texture, &retained), (&full, &full_pixels)] {
            encoder.copy_texture_to_buffer(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d::ZERO,
                    aspect: wgpu::TextureAspect::All,
                },
                wgpu::TexelCopyBufferInfo {
                    buffer,
                    layout: wgpu::TexelCopyBufferLayout {
                        offset: 0,
                        bytes_per_row: Some(row),
                        rows_per_image: Some(height),
                    },
                },
                wgpu::Extent3d {
                    width,
                    height,
                    depth_or_array_layers: 1,
                },
            );
        }
        queue.submit([encoder.finish()]);
        let mapped = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        for buffer in [&retained, &full_pixels] {
            let mapped = mapped.clone();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    // A failed map counts as a mismatch, never as a pass.
                    let step = if result.is_ok() { 1 } else { 100 };
                    mapped.fetch_add(step, std::sync::atomic::Ordering::AcqRel);
                });
        }
        Ok(Self {
            retained,
            full: full_pixels,
            width,
            height,
            row,
            mapped,
        })
    }

    /// The pixels that differ beyond antialiasing once both images are
    /// read back; `None` while they are not. A failed readback differs
    /// everywhere.
    pub fn differing(&self) -> Option<usize> {
        let mapped = self.mapped.load(std::sync::atomic::Ordering::Acquire);
        if mapped < 2 {
            return None;
        }
        let everywhere =
            usize::try_from(u64::from(self.width) * u64::from(self.height)).unwrap_or(usize::MAX);
        if mapped != 2 {
            return Some(everywhere);
        }
        let tight = |buffer: &vello::wgpu::Buffer| {
            let range = buffer.slice(..).get_mapped_range();
            let width = usize::try_from(self.width * 4).unwrap_or(0);
            let row = usize::try_from(self.row).unwrap_or(0);
            let rows = usize::try_from(self.height).unwrap_or(0);
            let mut pixels = Vec::with_capacity(width * rows);
            for y in 0..rows {
                if let Some(line) = range.get(y * row..y * row + width) {
                    pixels.extend_from_slice(line);
                }
            }
            pixels
        };
        let differing = crate::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(
            &tight(&self.retained),
            &tight(&self.full),
        );
        self.retained.unmap();
        self.full.unmap();
        Some(differing)
    }
}
