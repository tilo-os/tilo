//! Rasterizing frames into one retained target: only a frame's damage (or all of it), its scroll copies
//! made first and its backdrops blurred. Every host that keeps the pixels it presented draws through
//! [`RetainedRaster`]: the native app's render worker, the workspace's render service and the browser
//! shell, so they produce the same pixels from the same frame.

use crate::cpu_raster;
#[cfg(feature = "native")]
use tilo_ui_base::damage::coalesce_regions;
use tilo_ui_base::{damage::FineDamageRect, damage::area, damage::pixel_rect};
use tilo_ui_base::{
    offscreen::damage_geometric_capacity_enabled, offscreen::damage_texture_extent,
    offscreen::damage_texture_geometric_needs_replacement,
    offscreen::damage_texture_geometric_width_extent, offscreen::damage_texture_height_extent,
    offscreen::damage_texture_needs_replacement,
};
use vello::kurbo::Rect;
use vello::peniko::Color;
use vello::{RenderParams, Renderer, Scene};

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
    /// Damage textures grow in geometric buckets ([`DamageCache::geometric`]).
    geometric_capacity: bool,
    coarse_pool_high_water_pixels: u64,
    copy_scratch: tilo_ui_base::scroll_copy::CopyScratch,
    /// The frame's scene holds only what its damage needs ([`RetainedRaster::rasterize_damage_only`]).
    damage_only: bool,
    /// A frame drawn for its damage only would have had to repaint more: nothing was drawn.
    refused: bool,
}

/// Why a frame drawn for its damage only was not rasterized.
pub const DAMAGE_ONLY_REFUSED: &str =
    "a scene drawn for its damage only cannot repaint more than the damage";

/// Whether a frame of `width` × `height` with `damage` (physical pixels, not empty) is repainted region
/// by region rather than whole: a damage that covers half the target, or whose windows do, is rendered
/// as one whole frame ([`repaint_damage`]).
pub(crate) fn repaints_alone(width: u32, height: u32, damage: &[Rect]) -> bool {
    damage_plan(width, height, damage).is_some_and(|plan| plan.alone)
}

/// How a frame's damage is repainted: its regions, the windows they are rendered through, and how
/// many rectangles the damage named.
struct DamagePlan {
    count: usize,
    regions: Vec<FineDamageRect>,
    windows: Vec<FineDamageRect>,
    /// The windows cover less than half the target, so they cost less than the whole frame.
    alone: bool,
}

/// [`DamagePlan`] of `damage` on a `width` × `height` target; `None` when the damage covers no
/// pixel or half the target, which is rendered whole.
#[inline(never)]
fn damage_plan(width: u32, height: u32, damage: &[Rect]) -> Option<DamagePlan> {
    let half = u64::from(width)
        .saturating_mul(u64::from(height))
        .saturating_div(2);
    let regions: Vec<FineDamageRect> = damage
        .iter()
        .filter_map(|rect| pixel_rect(*rect, width, height))
        .collect();
    let damage_pixels = window_pixels(&regions);
    if damage_pixels == 0 || damage_pixels >= half {
        return None;
    }
    let count = regions.len();
    // The windows hold every region, so regions that overlap are copied out of them as they are:
    // a pixel copied twice is the same pixel.
    let windows = damage_windows(&regions);
    let alone = window_pixels(&windows) < half;
    Some(DamagePlan {
        count,
        regions,
        windows,
        alone,
    })
}

/// What one more window of damage costs beyond its own pixels, in pixels: each window is a Vello
/// render of its own over the frame's scene, so two regions close enough are cheaper as one window
/// than as two.
const WINDOW_COST_PIXELS: u64 = 128 * 128;

/// `region`'s window: from the tile corner at or before it, so its tiles are the full frame's and
/// its pixels the same.
fn tile_window(region: FineDamageRect) -> FineDamageRect {
    let (x, y) = (
        region.x / RASTER_TILE * RASTER_TILE,
        region.y / RASTER_TILE * RASTER_TILE,
    );
    FineDamageRect {
        x,
        y,
        width: region.x.saturating_add(region.width).saturating_sub(x),
        height: region.y.saturating_add(region.height).saturating_sub(y),
    }
}

/// The windows `regions` are repainted through: regions far apart each through a window of their
/// own, so a change at the top of the screen and one at its bottom repaint two strips rather than
/// everything between them; windows whose union costs no more than both apart
/// ([`WINDOW_COST_PIXELS`]) are one. A list too long to group is one window around it all.
pub(crate) fn damage_windows(regions: &[FineDamageRect]) -> Vec<FineDamageRect> {
    let windows: Vec<FineDamageRect> = regions.iter().copied().map(tile_window).collect();
    if windows.len() > 128 {
        return Vec::from_iter(
            windows
                .into_iter()
                .reduce(tilo_ui_base::damage::damage_union),
        );
    }
    tilo_ui_base::damage::merge_regions(windows, WINDOW_COST_PIXELS)
}

/// The pixels `windows` cover, each counted once per window it is in.
fn window_pixels(windows: &[FineDamageRect]) -> u64 {
    windows
        .iter()
        .fold(0u64, |sum, window| sum.saturating_add(area(*window)))
}

/// Whether `outer` holds every pixel of `inner`.
fn contains(outer: FineDamageRect, inner: FineDamageRect) -> bool {
    tilo_ui_base::damage::damage_union(outer, inner) == outer
}

/// What a host keeps between frames to rasterize them into one retained target: the damage scratch, the
/// backdrop blur and, on a software adapter, the CPU canvas. The app's own render worker draws with it,
/// and so does the workspace's render service for each surface it draws for an app, so both produce the
/// same pixels from the same frame.
pub struct RetainedRaster {
    damage_cache: DamageCache,
    backdrop_blur: Option<tilo_ui_base::backdrop::BackdropBlur>,
    cpu: Option<cpu_raster::CpuCanvas>,
}

impl DamageCache {
    /// Whether damage textures grow in geometric buckets: the one sizing the environment
    /// chose, which a page that reads no environment always gets.
    fn geometric(&self) -> bool {
        self.geometric_capacity
    }
}

impl RetainedRaster {
    /// Scratch for one target; `on_cpu` rasterizes on the CPU and uploads the pixels (a software
    /// adapter, see `tilo_ui_base::gpu::rasterizes_on_cpu`).
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

    /// Rasterize `frame` on the CPU into this scratch, keeping its pixels for
    /// [`Self::cpu_frame`]: how a presenter with no GPU to draw on shows its
    /// frames (a page whose device keeps failing). Fails for a frame only the
    /// renderer can draw (its backdrops), and when this scratch draws on the
    /// GPU.
    pub fn rasterize_to_cpu(&mut self, frame: FrameContent<'_>) -> Result<u64, String> {
        if !frame.backdrop.is_empty() {
            return Err("a backdrop needs the renderer's pipelines".to_owned());
        }
        let canvas = self
            .cpu
            .as_mut()
            .ok_or("this rasterizer draws on the GPU")?;
        canvas.rasterize(&frame)
    }

    /// The pixels [`Self::rasterize_to_cpu`] last drew, while this scratch
    /// draws on the CPU.
    #[must_use]
    pub fn cpu_frame(&self) -> Option<CpuFrame<'_>> {
        let canvas = self.cpu.as_ref()?;
        let (width, height) = canvas.size();
        Some(CpuFrame {
            pixels: canvas.pixels(),
            width,
            height,
            dirty: canvas.dirty(),
        })
    }

    /// Bring `target` up to date with `frame`: its backdrops blurred and substituted, its scroll copies
    /// made, its damage (or all of it) repainted. Returns the pixels rasterized, and the backdrop
    /// pass's statistics when the frame had backdrops.
    pub fn rasterize(
        &mut self,
        (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
        mut renderer: Option<&mut Renderer>,
        target: RetainedTarget<'_>,
        frame: FrameContent<'_>,
    ) -> (
        Result<u64, String>,
        Option<tilo_ui_base::backdrop::BackdropStats>,
    ) {
        // Floating surfaces over the blurred material: render, blur and
        // substitute each one's backdrop before the frame that draws it.
        let backdrop_stats = if frame.backdrop.is_empty() {
            None
        } else {
            let Some(gpu) = renderer.as_deref_mut() else {
                return (
                    Err("a backdrop needs the renderer's pipelines".to_owned()),
                    None,
                );
            };
            Some(
                self.backdrop_blur
                    .get_or_insert_with(|| tilo_ui_base::backdrop::BackdropBlur::new(device))
                    .prepare(
                        device,
                        queue,
                        gpu,
                        frame.backdrop,
                        (frame.width, frame.height),
                        frame.base_color,
                    ),
            )
        };
        let raster_frame = FrameContent {
            backdrop: &tilo_ui_base::part_paint::backdrop::BackdropFrame::default(),
            ..frame
        };
        let rasterized =
            if let Some(canvas) = self.cpu.as_mut().filter(|_| frame.backdrop.is_empty()) {
                canvas.rasterize(&raster_frame).inspect(|_| {
                    canvas.upload(queue, target.texture);
                })
            } else {
                if let Some(canvas) = self.cpu.as_mut() {
                    canvas.invalidate();
                }
                let Some(renderer) = renderer.as_deref_mut() else {
                    return (
                        Err("this frame draws on the GPU, whose pipelines are not \
                             compiled yet"
                            .to_owned()),
                        None,
                    );
                };
                rasterize_damage(
                    (device, queue),
                    target,
                    renderer,
                    &mut self.damage_cache,
                    &raster_frame,
                )
                .map_err(|error| error.to_string())
            };
        if let (Some(blur), Some(renderer)) = (self.backdrop_blur.as_mut(), renderer) {
            blur.release(renderer);
        }
        (rasterized, backdrop_stats)
    }

    /// [`Self::rasterize`] for a frame whose scene holds only what its damage needs
    /// (`tilo_ui_base::draw_cull`): it is never repainted beyond its damage. `Err` with
    /// [`DAMAGE_ONLY_REFUSED`] when it would have to be (a whole frame, a backdrop, a copy that cannot
    /// be made, a damage too large to repaint alone); the target then still holds the last frame.
    pub fn rasterize_damage_only(
        &mut self,
        gpu: (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        frame: FrameContent<'_>,
    ) -> (
        Result<u64, String>,
        Option<tilo_ui_base::backdrop::BackdropStats>,
    ) {
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
        let (rasterized, stats) = self.rasterize(gpu, Some(renderer), target, frame);
        self.damage_cache.damage_only = false;
        if let Some(canvas) = self.cpu.as_mut() {
            canvas.draw_damage_only(false);
        }
        if std::mem::take(&mut self.damage_cache.refused) {
            return (Err(DAMAGE_ONLY_REFUSED.to_owned()), stats);
        }
        (rasterized, stats)
    }
}

/// The retained frame the native host rasterizes into and presents from. Pixels outside a frame's
/// damage are whatever the previous frame left, so it must only ever be written through
/// [`RetainedRaster::rasterize`].
#[derive(Clone, Copy)]
pub struct RetainedTarget<'a> {
    pub texture: &'a vello::wgpu::Texture,
    pub view: &'a vello::wgpu::TextureView,
}

/// A frame rasterized on the CPU ([`RetainedRaster::rasterize_to_cpu`]): what a presenter with no
/// GPU to draw on puts on its canvas.
#[derive(Clone, Copy)]
pub struct CpuFrame<'a> {
    /// Rows of `width * 4` bytes, premultiplied RGBA8.
    pub pixels: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// The rectangles the frame wrote, in pixels: outside them the canvas still holds the frames
    /// before it.
    pub dirty: &'a [tilo_cpu_raster::Region],
}

/// [`CpuFrame`]'s premultiplied pixels as a canvas's `ImageData` holds them,
/// straight alpha, in place: a pixel of full alpha is its own colour over
/// nothing, and no other colour is more than its alpha, so no step leaves the
/// byte.
pub fn straighten(pixels: &mut [u8]) {
    for pixel in pixels.chunks_exact_mut(4) {
        let [red, green, blue, a] = pixel else {
            continue;
        };
        let alpha = u32::from(*a);
        if alpha == 255 {
            continue;
        }
        for value in [red, green, blue] {
            *value = if alpha == 0 {
                0
            } else {
                u8::try_from(
                    u32::from(*value)
                        .saturating_mul(255)
                        .saturating_add(alpha / 2)
                        .checked_div(alpha)
                        .unwrap_or(0),
                )
                .unwrap_or(u8::MAX)
            };
        }
    }
}

/// Bring `retained` up to date with `frame`: its scroll copies made, then its damage (or all of it)
/// repainted.
pub(crate) fn rasterize_damage(
    (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
    retained: RetainedTarget<'_>,
    renderer: &mut Renderer,
    damage_cache: &mut DamageCache,
    frame: &FrameContent<'_>,
) -> Result<u64, vello::Error> {
    let mut frame = *frame;
    if !frame.full_redraw && !frame.scroll_copies.is_empty() {
        let mut encoder = device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor {
            label: Some("ui-native-scroll-copies"),
        });
        if tilo_ui_base::scroll_copy::encode_copies(
            device,
            &mut encoder,
            retained.texture,
            &mut damage_cache.copy_scratch,
            frame.scroll_copies,
        ) {
            queue.submit([encoder.finish()]);
        } else if damage_cache.damage_only {
            damage_cache.refused = true;
            return Ok(0);
        } else {
            frame.full_redraw = true;
        }
    }
    repaint_damage((device, queue), retained, renderer, damage_cache, &frame)
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

    // A large union costs less as one Vello pass and must not retain a second
    // near-window-sized texture. This is both the time and memory bound.
    let Some(DamagePlan {
        count: damage_region_count,
        regions,
        windows,
        alone,
    }) = damage_plan(width, height, damage_regions)
    else {
        return render_full(renderer, damage_cache);
    };
    // Vello's sparse fine pass is a second render path the web build would
    // carry for this alone; it repaints through the windows below instead.
    // The native feature is only ever enabled off the web.
    #[cfg(feature = "native")]
    if renderer.sparse_fine_supported() {
        let regions = coalesce_regions(regions.clone());
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
        let raster_pixels = u64::from(prefix_width).saturating_mul(u64::from(prefix_height));
        if damage_cache.coarse_pool_high_water_pixels >= full_pixels.saturating_div(2)
            && raster_pixels.saturating_mul(4) <= damage_cache.coarse_pool_high_water_pixels
        {
            renderer.trim_pooled_buffers();
            damage_cache.coarse_pool_high_water_pixels = 0;
        }
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
            &regions,
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
    // Each window of the damage, from a tile corner so its tiles are the full frame's and its
    // pixels the same: rendered alone, shifted to the target's origin, then copied into place
    // region by region. Regions far apart are windows of their own: one window around both would
    // repaint everything between them.
    if !alone {
        return render_full(renderer, damage_cache);
    }
    if damage_cache.last_region_count == Some(0) {
        renderer.trim_pooled_buffers();
    }
    damage_cache.last_region_count = Some(damage_region_count);
    // One scratch texture every window renders into in turn: as wide and as tall as the widest and
    // the tallest of them.
    let (largest, window_width, window_height) =
        windows
            .iter()
            .fold((0, 0, 0), |(largest, width, height), window| {
                (
                    largest.max(area(*window)),
                    width.max(window.width),
                    height.max(window.height),
                )
            });
    damage_cache.coarse_pool_high_water_pixels =
        damage_cache.coarse_pool_high_water_pixels.max(largest);
    let texture_limit = device.limits().max_texture_dimension_2d;
    let allocated_width = if damage_cache.geometric() {
        damage_texture_geometric_width_extent(window_width, width, texture_limit)
    } else {
        damage_texture_extent(window_width, texture_limit)
    };
    let allocated_height = damage_texture_height_extent(window_height, texture_limit);
    let recreate = damage_cache.targets.first().is_none_or(|target| {
        if damage_cache.geometric() {
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
    for window in &windows {
        let (origin_x, origin_y) = (window.x, window.y);
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
        renderer.render_to_texture(
            device,
            queue,
            window_scene,
            &target.view,
            &RenderParams {
                base_color,
                width: window.width,
                height: window.height,
                antialiasing_method: vello::AaConfig::Area,
            },
        )?;
        // The copies are submitted before the next window renders into the same texture, and the
        // queue keeps that order.
        let mut encoder =
            device.create_command_encoder(&vello::wgpu::CommandEncoderDescriptor::default());
        // A region is copied out of each window that holds it: windows that overlap repaint it
        // with the same pixels.
        for region in regions.iter().filter(|region| contains(*window, **region)) {
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
    }
    while damage_cache.targets.len() > 1 {
        let target = damage_cache.targets.pop().expect("length checked above");
        renderer.invalidate_texture_bindings(&target.view);
    }
    Ok(window_pixels(&windows))
}

/// Vello's tile edge in pixels: a window rendered from a multiple of it bins its paths into the same
/// tiles as the full frame.
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
    pub scroll_copies: &'a [tilo_ui_base::scroll_copy::DeviceCopy],
    /// Repaint everything: the target is not current, or the frame says so.
    pub full_redraw: bool,
    pub backdrop: &'a tilo_ui_base::part_paint::backdrop::BackdropFrame,
}

/// What a target holds and how a scene is placed on it: a frame drawn for another key cannot be
/// repainted over the last one.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TargetKey {
    pub width: u32,
    pub height: u32,
    /// From scene coordinates to the target's pixels (the display scale).
    pub to_device: vello::kurbo::Affine,
    pub base_color: Color,
    /// Changed by the host whenever the target's pixels may no longer be the last frame's: a new
    /// texture or device, a theme, a frame drawn but not presented.
    pub generation: u64,
}

/// What of `scene`, drawn with `parts`, repainting `damage` (target pixels) on the target `key` names
/// takes: the scene without the parts that cannot reach the damage ([`tilo_ui_base::scene_parts::culled`]), or
/// `None` when it takes all of it: no parts, every part reaching the damage, or a damage repainted as
/// one whole frame ([`repaints_alone`]), which draws every pixel.
pub fn culled_for_damage(
    scene: &Scene,
    parts: Option<&tilo_ui_base::scene_parts::SectionParts>,
    key: TargetKey,
    damage: &[Rect],
) -> Option<Scene> {
    if !repaints_alone(key.width, key.height, damage) {
        return None;
    }
    tilo_ui_base::scene_parts::culled(scene, parts?, |bounds| {
        bounds.iter().any(|rect| {
            let rect = key.to_device.transform_rect_bbox(*rect).inflate(1.0, 1.0);
            damage.iter().any(|region| region.overlaps(rect))
        })
    })
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
    Unproven(tilo_ui_base::scene_parts::Unproven),
    /// The renderer dropped geometry from a frame drawn since the target was last repainted whole
    /// ([`RetainedFrames::note_scratch`]).
    LostGeometry,
}

/// The frame a host presented last, to compare the next with. Where a scene drawn with damage parts
/// ([`tilo_ui_base::scene_parts`]) changed is the parts whose commands differ (or what a hinted part says
/// changed); every other pixel is drawn by the same commands as before. Anything that cannot be proven
/// (no parts, a change outside every part, another target) repaints the whole frame.
#[derive(Default)]
pub struct RetainedFrames {
    last: Option<(Scene, tilo_ui_base::scene_parts::SectionParts, TargetKey)>,
    shown: bool,
    /// The renderer's scratch ([`crate::render_bump_recovery::Scratch`]) when the target was last
    /// brought up to date.
    scratch: Option<crate::render_bump_recovery::Scratch>,
    /// The target may show a frame with geometry missing.
    lost: bool,
    bounds: tilo_ui_base::scene_parts::DrawBounds,
}

impl RetainedFrames {
    /// What to repaint of `scene`, drawn with `parts`, onto a target that shows the frame last
    /// [`Self::presented`].
    pub fn repaint(
        &self,
        scene: &Scene,
        parts: Option<&tilo_ui_base::scene_parts::SectionParts>,
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
        let changed = match tilo_ui_base::scene_parts::damage(last, last_parts, scene, parts) {
            Ok(changed) => changed,
            Err(unproven) => return Repaint::Full(Unknown::Unproven(unproven)),
        };
        let target = Rect::new(0.0, 0.0, f64::from(key.width), f64::from(key.height));
        Repaint::Regions(
            changed
                .into_iter()
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

    /// [`Self::repaint`] for a host that moves the pixels of the target before it repaints: what hinted
    /// parts say moved ([`tilo_ui_base::scene_parts::DamageHint::copies`]) is copied, in order, and then the
    /// rest repainted. A copy that cannot be made exactly (its content drawn through a fading layer,
    /// something unbounded drawn over it) repaints where it lands instead.
    pub fn repaint_copying(
        &mut self,
        scene: &Scene,
        parts: Option<&tilo_ui_base::scene_parts::SectionParts>,
        key: TargetKey,
    ) -> (Repaint, Vec<tilo_ui_base::scroll_copy::DeviceCopy>) {
        let full = |unknown| (Repaint::Full(unknown), Vec::new());
        if self.lost {
            return full(Unknown::LostGeometry);
        }
        let Some(parts) = parts else {
            return full(Unknown::NoParts);
        };
        let Some((last, last_parts, last_key)) = &self.last else {
            return full(Unknown::NoFrame);
        };
        if *last_key != key {
            return full(Unknown::Target);
        }
        let section = match tilo_ui_base::scene_parts::damage_with_copies(
            last,
            last_parts,
            scene,
            parts,
            &mut self.bounds,
        ) {
            Ok(section) => section,
            Err(unproven) => return full(Unknown::Unproven(unproven)),
        };
        let target = Rect::new(0.0, 0.0, f64::from(key.width), f64::from(key.height));
        let damage = section
            .rects
            .into_iter()
            .map(|rect| {
                key.to_device
                    .transform_rect_bbox(rect)
                    .expand()
                    .inflate(1.0, 1.0)
                    .intersect(target)
            })
            .filter(|rect| !rect.is_zero_area())
            .collect();
        let presented = tilo_ui_base::scene_parts::present_copies(
            &section.copies,
            damage,
            &[],
            key.to_device,
            (key.width, key.height),
        );
        let raster = presented
            .raster
            .into_iter()
            .map(|rect| rect.expand().intersect(target))
            .filter(|rect| !rect.is_zero_area())
            .collect();
        (Repaint::Regions(raster), presented.copies)
    }

    /// `scene`, drawn with `parts`, is what the target with `key` shows now.
    pub fn presented(
        &mut self,
        scene: &Scene,
        parts: Option<&tilo_ui_base::scene_parts::SectionParts>,
        key: TargetKey,
    ) {
        self.last = parts.map(|parts| (scene.clone(), parts.clone(), key));
        self.shown = true;
        self.lost = false;
    }

    /// Whether the renderer's scratch is `scratch` without the target having been brought up to date
    /// since it changed: a frame the target shows may have geometry missing.
    #[must_use]
    pub fn shows_lost_geometry(&self, scratch: crate::render_bump_recovery::Scratch) -> bool {
        self.lost || self.scratch.is_some_and(|recorded| recorded != scratch)
    }

    /// The renderer's scratch is `scratch` now: once it changed since the target was brought up to
    /// date, the next frame repaints the whole target. Pixels drawn with geometry missing stay on it
    /// otherwise, since later frames repaint only what changes.
    pub fn note_scratch(&mut self, scratch: crate::render_bump_recovery::Scratch) {
        if self.scratch.is_none_or(|recorded| recorded != scratch) {
            self.lost = self.scratch.is_some();
            self.scratch = Some(scratch);
        }
    }

    /// The target no longer shows the last frame presented.
    pub fn forget(&mut self) {
        self.last = None;
        self.shown = false;
    }

    /// Whether the target still holds the frame last presented: a host that acquired a surface texture
    /// for a frame it then skipped puts that on it again ([`tilo_ui_base::gpu::submit_target_to_surface`])
    /// rather than presenting a cleared one.
    #[must_use]
    pub const fn shows_frame(&self) -> bool {
        self.shown
    }
}

/// A check that a retained target shows what a full render of the same frame does: a frame repainted
/// only where it changed must leave no stale pixel.
pub struct RasterCheck {
    retained: vello::wgpu::Buffer,
    full: vello::wgpu::Buffer,
    width: u32,
    height: u32,
    row: u32,
    /// The colour the target was cleared with: a pixel missing geometry shows it, so a difference that
    /// has one side at it names the side that dropped the geometry.
    base: [u8; 4],
    /// The renderer's scratch when the check started ([`crate::render_bump_recovery::Scratch`]): one
    /// that changed before the comparison means a render in it, or the frame it compares, may have
    /// dropped geometry, and a difference then names neither.
    scratch: crate::render_bump_recovery::Scratch,
    mapped: std::sync::Arc<std::sync::atomic::AtomicU32>,
    /// Why the readback failed, when it did: a check that could not read the frames back says so,
    /// rather than claiming they differ.
    unreadable: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl RasterCheck {
    /// Render `scene` in full beside `target`, which must already show it, and start reading both back.
    pub fn start(
        (device, queue): (&vello::wgpu::Device, &vello::wgpu::Queue),
        renderer: &mut Renderer,
        target: RetainedTarget<'_>,
        scene: &Scene,
        (width, height): (u32, u32),
        base_color: Color,
        scratch: crate::render_bump_recovery::Scratch,
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
        let copied = [(target.texture, &retained), (&full, &full_pixels)];
        let buffers = [&retained, &full_pixels];
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("ui-retained-check"),
        });
        for (texture, buffer) in copied {
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
        let unreadable = std::sync::Arc::new(std::sync::Mutex::new(None));
        for buffer in buffers {
            let mapped = mapped.clone();
            let unreadable = unreadable.clone();
            buffer
                .slice(..)
                .map_async(wgpu::MapMode::Read, move |result| {
                    // A failed map counts as a check that could not run,
                    // never as a pass.
                    if let Err(error) = result {
                        let mut why = unreadable.lock().unwrap_or_else(|e| e.into_inner());
                        if why.is_none() {
                            *why = Some(error.to_string());
                        }
                        mapped.fetch_add(100, std::sync::atomic::Ordering::AcqRel);
                        return;
                    }
                    mapped.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                });
        }
        Ok(Self {
            retained,
            full: full_pixels,
            width,
            height,
            row,
            base: base_color.to_rgba8().to_u8_array(),
            scratch,
            mapped,
            unreadable,
        })
    }

    /// What reading the frames back said: how the pixels that differ fall on the frame, so a stale
    /// repaint is found on the screen rather than only counted, or why the frames could not be read.
    pub fn differences(&self, scratch: crate::render_bump_recovery::Scratch) -> Option<Readback> {
        let mapped = self.mapped.load(std::sync::atomic::Ordering::Acquire);
        if mapped < 2 {
            return None;
        }
        if mapped != 2 {
            let why = self
                .unreadable
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
                .unwrap_or_else(|| "its map failed".to_owned());
            return Some(Readback::Unreadable(why));
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
        let width = usize::try_from(self.width).unwrap_or(0);
        let (kept, rendered) = (tight(&self.retained), tight(&self.full));
        let blank = (
            kept.iter().all(|byte| *byte == 0),
            rendered.iter().all(|byte| *byte == 0),
        );
        // A full render that read back empty was not drawn: the check has
        // nothing to compare with and says so rather than blaming the frame.
        if blank.1 {
            self.unmap_all();
            return Some(Readback::Unreadable(
                "the full render read back empty".to_owned(),
            ));
        }
        let (count, bounds) = tilo_ui_base::offscreen::OffscreenRenderer::differing_beyond_aa(
            &kept, &rendered, width,
        );
        let (inside, rows, kept_base, render_base) = if count == 0 {
            (0, Vec::new(), 0, 0)
        } else {
            let positions =
                tilo_ui_base::offscreen::OffscreenRenderer::positions_differing_beyond_aa(
                    &kept, &rendered, width,
                );
            let inside = bounds.map_or(0, |(x0, y0, x1, y1)| {
                let (left, top) = (x0.saturating_add(8), y0.saturating_add(8));
                let (right, bottom) = (x1.saturating_sub(8), y1.saturating_sub(8));
                positions
                    .iter()
                    .filter(|(x, y)| *x >= left && *x < right && *y >= top && *y < bottom)
                    .count()
            });
            let mut rows: Vec<(u32, Vec<(u32, u32)>)> = Vec::new();
            for (x, y) in &positions {
                let (Ok(x), Ok(y)) = (u32::try_from(*x), u32::try_from(*y)) else {
                    continue;
                };
                if rows.last().is_none_or(|(at, _)| *at != y) {
                    if rows.len() == DIFF_ROWS {
                        continue;
                    }
                    rows.push((y, Vec::new()));
                }
                let Some((_, runs)) = rows.last_mut() else {
                    continue;
                };
                let extends = runs.last().is_some_and(|(_, end)| *end == x);
                if extends {
                    if let Some((_, end)) = runs.last_mut() {
                        *end = x.saturating_add(1);
                    }
                } else if runs.len() < DIFF_RUNS {
                    runs.push((x, x.saturating_add(1)));
                }
            }
            // Which side is missing what: a pixel cleared to the base colour
            // and drawn on nothing shows it, so a differing pixel that is the
            // base colour on one side only names the side that dropped it.
            let near = |colour: [u8; 4]| {
                colour[..3]
                    .iter()
                    .zip(&self.base[..3])
                    .all(|(c, base)| c.abs_diff(*base) <= 1)
            };
            let (mut kept_base, mut render_base) = (0, 0);
            for (kept_at, rendered_at) in kept.chunks_exact(4).zip(rendered.chunks_exact(4)) {
                if kept_at
                    .iter()
                    .zip(rendered_at)
                    .all(|(a, b)| a.abs_diff(*b) <= 1)
                {
                    continue;
                }
                let kept_at = [kept_at[0], kept_at[1], kept_at[2], kept_at[3]];
                let rendered_at = [
                    rendered_at[0],
                    rendered_at[1],
                    rendered_at[2],
                    rendered_at[3],
                ];
                kept_base += usize::from(near(kept_at));
                render_base += usize::from(near(rendered_at));
            }
            (inside, rows, kept_base, render_base)
        };
        let first = kept
            .chunks_exact(4)
            .zip(rendered.chunks_exact(4))
            .enumerate()
            .find(|(_, (kept, rendered))| {
                kept.iter().zip(*rendered).any(|(p, q)| p.abs_diff(*q) > 1)
            })
            .and_then(|(index, (kept, rendered))| {
                let at = u32::try_from(index).ok()?;
                Some((
                    at % self.width.max(1),
                    at / self.width.max(1),
                    [kept[0], kept[1], kept[2], kept[3]],
                    [rendered[0], rendered[1], rendered[2], rendered[3]],
                ))
            });
        self.unmap_all();
        // A renderer that lost geometry may have lost it from the frame the
        // target shows or from this check's render of it: the difference
        // says which side is at the base colour, but not which one is right.
        // Its reservation changing is that loss too, and it is known before
        // an overflow readback is collected.
        if count != 0 && scratch != self.scratch {
            return Some(Readback::Unreadable(format!(
                "the renderer's scratch changed while this frame was drawn: {} to {} bytes, {} \
                 overflow(s) collected, and the {count} pixels that differ ({kept_base} at the \
                 base colour in the target, {render_base} in the full render) name neither side",
                self.scratch.bump_bytes,
                scratch.bump_bytes,
                scratch.collected.saturating_sub(self.scratch.collected),
            )));
        }
        Some(Readback::Differing(Differences {
            count,
            inside,
            rows,
            kept_base,
            render_base,
            first,
            bounds: bounds.map(|(x0, y0, x1, y1)| {
                (
                    u32::try_from(x0).unwrap_or(u32::MAX),
                    u32::try_from(y0).unwrap_or(u32::MAX),
                    u32::try_from(x1).unwrap_or(u32::MAX),
                    u32::try_from(y1).unwrap_or(u32::MAX),
                )
            }),
        }))
    }

    fn unmap_all(&self) {
        self.retained.unmap();
        self.full.unmap();
    }
}

/// What reading a frame repainted in part and a full render of it back said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readback {
    /// Both frames were read: how one differs from the other.
    Differing(Differences),
    /// The frames could not be read, so there is nothing to compare: why not. A check that could not
    /// run never passes.
    Unreadable(String),
}

/// How a frame repainted in part differed from a full render of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Differences {
    /// How many pixels differ.
    pub count: usize,
    /// How many of them lie more than eight pixels inside [`Self::bounds`]: `0` says the difference is
    /// the outline of a rectangle, not its fill.
    pub inside: usize,
    /// For each row that differs, up to [`DIFF_ROWS`] of them, the runs of columns that differ, up to
    /// [`DIFF_RUNS`] a row.
    pub rows: Vec<(u32, Vec<(u32, u32)>)>,
    /// How many of them are the colour the target was cleared with in the frame repainted in part:
    /// geometry missing from what the target shows.
    pub kept_base: usize,
    /// How many of them are that colour in the full render: geometry the check's own render dropped,
    /// which names no stale frame.
    pub render_base: usize,
    /// The first pixel that differs beyond the ±1 two renders of one frame may differ by: where it is,
    /// then its colour in the frame repainted in part and in the full render.
    pub first: Option<(u32, u32, [u8; 4], [u8; 4])>,
    /// The left, top, right and bottom of the pixels that differ, the far edges exclusive, or `None`
    /// where none do.
    pub bounds: Option<(u32, u32, u32, u32)>,
}

const DIFF_ROWS: usize = 16;
const DIFF_RUNS: usize = 8;

#[cfg(test)]
#[path = "retained_raster_tests.rs"]
mod tests;
