//! Retained CPU canvases and a thread-confined GLES consumer.
//!
//! KMS remains owned by `DrmScanout`. GLES and its canvases never leave
//! their worker thread. Every composition waits for the whole GPU submission
//! before returning; neither shared CPU mappings nor per-canvas fences are used.
use std::os::fd::OwnedFd;
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use vello::FineDamageRect;
use vello::Scene;
use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;

use super::cpu_present::{CpuFrame, CpuFrameStats, CpuRenderer};
use super::gl_compose::{ComposeStats, GlCompositor, LayerView};
use crate::compositor::cursor::CursorBitmap;
use crate::shell::frame::FrameCache;

pub(super) const MAX_LAYER_WIDTH: u32 = 8192;

enum Command {
    Paint(Box<FrameCache>, Option<(Arc<CursorBitmap>, i32, i32)>),
    Accepted,
    Complete,
    Refused,
    Forget,
    Stop,
}

pub(super) struct Rendered {
    pub framebuffer: smithay::reexports::drm::control::framebuffer::Handle,
    pub stats: CpuFrameStats,
    pub compose: ComposeStats,
    pub gpu_wait: Duration,
}

/// Drop only after KMS has relinquished every framebuffer this worker owns.
pub(super) struct Consumer {
    commands: mpsc::Sender<Command>,
    frames: mpsc::Receiver<Result<Rendered, String>>,
    worker: Option<JoinHandle<()>>,
}

impl Consumer {
    pub fn new(card: OwnedFd, size: (u32, u32)) -> Result<Self, String> {
        let (commands, receive) = mpsc::channel();
        let (send, frames) = mpsc::channel();
        let (ready, initialized) = mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("tilo-retained-gles".into())
            .spawn(move || {
                let mut gl = match GlCompositor::new(card, size) {
                    Ok(gl) => gl,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let limit = gl.max_layer_width();
                if size.0 > limit || size.1 > limit {
                    let _ = ready.send(Err("output exceeds GLES texture limits".into()));
                    return;
                }
                let _ = ready.send(Ok(limit));
                let mut canvases = Canvases::new();
                let mut failed: Option<String> = None;
                while let Ok(command) = receive.recv() {
                    match command {
                        Command::Paint(cache, cursor) => {
                            let result = if let Some(error) = &failed {
                                Err(error.clone())
                            } else {
                                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                    render(&mut gl, &mut canvases, &cache, size, cursor.as_ref())
                                }))
                                .unwrap_or_else(|_| Err("retained GLES worker panicked; preserving scanout resources".into()))
                            };
                            if let Err(error) = &result {
                                failed = Some(error.clone());
                            }
                            if send.send(result).is_err() {
                                break;
                            }
                        }
                        Command::Accepted => {
                            if !gl.flip_accepted() {
                                failed = Some(
                                    "retained slot lost ownership after KMS acceptance".into(),
                                );
                            }
                        }
                        Command::Complete => gl.flip_completed(),
                        Command::Refused => gl.flip_refused(),
                        Command::Forget => gl.forget_scanout(),
                        Command::Stop => break,
                    }
                }
            })
            .map_err(|error| format!("retained GLES worker: {error}"))?;
        match initialized.recv().map_err(|error| error.to_string())? {
            Ok(_) => Ok(Self {
                commands,
                frames,
                worker: Some(worker),
            }),
            Err(error) => {
                let _ = worker.join();
                Err(error)
            }
        }
    }

    pub fn paint(
        &self,
        cache: &FrameCache,
        cursor: Option<(Arc<CursorBitmap>, i32, i32)>,
    ) -> Result<Rendered, String> {
        self.commands
            .send(Command::Paint(Box::new(cache.clone()), cursor))
            .map_err(|error| error.to_string())?;
        self.frames.recv().map_err(|error| error.to_string())?
    }

    pub fn accepted(&self) {
        let _ = self.commands.send(Command::Accepted);
    }

    pub fn complete(&self) {
        let _ = self.commands.send(Command::Complete);
    }
    pub fn refused(&self) {
        let _ = self.commands.send(Command::Refused);
    }
    pub fn forget(&self) {
        let _ = self.commands.send(Command::Forget);
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Canvases {
    // Backdrop, stationary workspace, one canvas per strip, wheel, chrome.
    layers: Vec<CpuRenderer>,
    empty: Vec<bool>,
    sizes: Vec<(u32, u32)>,
}

impl Canvases {
    fn new() -> Self {
        Self {
            layers: Vec::new(),
            empty: Vec::new(),
            sizes: Vec::new(),
        }
    }

    fn update(&mut self, cache: &FrameCache, size: (u32, u32)) -> CpuFrameStats {
        let count = cache.layers.strips.len() + 4;
        if self.layers.len() != count {
            self.layers = (0..count).map(|_| CpuRenderer::new()).collect();
            self.empty = vec![false; count];
            self.sizes = vec![(0, 0); count];
        }
        let mut wheel = Scene::new();
        if let Some(sprite) = &cache.layers.wheel {
            let (x, y) = (f64::from(sprite.x), f64::from(sprite.y));
            wheel.push_clip_layer(
                Fill::NonZero,
                Affine::IDENTITY,
                &Rect::new(
                    x,
                    y,
                    x + f64::from(sprite.width),
                    y + f64::from(sprite.height),
                ),
            );
            wheel.append(&sprite.scene, Some(Affine::translate((x, y))));
            wheel.pop_layer();
        }
        let scenes = std::iter::once(&cache.layers.backdrop)
            .chain(std::iter::once(&cache.layers.workspace))
            .chain(cache.layers.strips.iter())
            .chain(std::iter::once(&wheel))
            .chain(std::iter::once(&cache.layers.chrome));
        let mut stats = CpuFrameStats {
            fill: "layers-gles",
            ..CpuFrameStats::default()
        };
        for (index, (((layer, was_empty), last_size), scene)) in self
            .layers
            .iter_mut()
            .zip(&mut self.empty)
            .zip(&mut self.sizes)
            .zip(scenes)
            .enumerate()
        {
            let layer_size = if (2..2 + cache.layers.strips.len()).contains(&index) {
                (cache.width, size.1)
            } else {
                size
            };
            let empty = scene.encoding().draw_tags.is_empty();
            // The dense rasterizer has no decoded-draw diff for an empty
            // scene. Its already-cleared canvas remains valid until either
            // drawing commands or dimensions change.
            if empty && *was_empty && *last_size == layer_size {
                continue;
            }
            // Layer scenes have their own geometry. Screen damage cannot be
            // treated as canvas damage; compare decoded draws independently.
            layer.update(
                CpuFrame {
                    scene,
                    revision: 0,
                    previous_revision: None,
                    regions: None,
                },
                layer_size,
            );
            *was_empty = empty;
            *last_size = layer_size;
            let work = layer.stats();
            stats.decode = stats.decode.saturating_add(work.decode);
            stats.raster = stats.raster.saturating_add(work.raster);
            stats.raster_px = stats.raster_px.saturating_add(work.raster_px);
            stats.raster_regions = stats.raster_regions.saturating_add(work.raster_regions);
            stats.moved_px = stats.moved_px.saturating_add(work.moved_px);
            stats.move_time = stats.move_time.saturating_add(work.move_time);
        }
        stats
    }

    fn views(&self, cache: &FrameCache, size: (u32, u32)) -> Vec<LayerView<'_>> {
        let full = FineDamageRect {
            x: 0,
            y: 0,
            width: size.0,
            height: size.1,
        };
        self.layers
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                let strip = index.checked_sub(2).and_then(|at| cache.strips.get(at));
                let width = if strip.is_some() { cache.width } else { size.0 };
                let pieces = match strip {
                    Some(strip) => vec![(
                        FineDamageRect {
                            x: strip.dest_x,
                            y: strip.dest_y,
                            width: strip.dest_w,
                            height: strip.dest_h,
                        },
                        strip.scroll_x,
                    )],
                    None => vec![(full, 0)],
                };
                LayerView {
                    pixels: layer.screen_rgba(),
                    width,
                    height: size.1,
                    stride: usize::try_from(width).unwrap_or(0).saturating_mul(4),
                    generation: layer.generation(),
                    opaque: false,
                    pieces,
                    draw: Vec::new(),
                    // A full upload is represented by an empty slice here: the
                    // generation must invalidate its cached texture separately.
                    written: layer.pending().unwrap_or(&[]),
                }
            })
            .collect()
    }
}

fn render(
    gl: &mut GlCompositor,
    canvases: &mut Canvases,
    cache: &FrameCache,
    size: (u32, u32),
    cursor: Option<&(Arc<CursorBitmap>, i32, i32)>,
) -> Result<Rendered, String> {
    validate(cache, size, gl.max_layer_width())?;
    let started = Instant::now();
    let mut stats = canvases.update(cache, size);
    let canvas_full = [FineDamageRect {
        x: 0,
        y: 0,
        width: cache.width,
        height: size.1,
    }];
    let screen_full = [FineDamageRect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }];
    let mut views = canvases.views(cache, size);
    // Pending `None` means full invalidation, including dropped publication.
    // Supply a full rectangle even when the allocation generation is unchanged.
    for (view, layer) in views.iter_mut().zip(&canvases.layers) {
        if layer.pending().is_none() {
            view.written = if view.width == cache.width && view.width != size.0 {
                &canvas_full
            } else {
                &screen_full
            };
        }
    }
    let (framebuffer, compose, sync) =
        gl.compose(&views, cursor.map(|(bitmap, x, y)| (bitmap, *x, *y)))?;
    let wait = Instant::now();
    // Fail closed: never return a framebuffer or permit a later canvas write
    // after a failed global completion wait.
    sync.wait()
        .map_err(|error| format!("retained GLES completion: {error}"))?;
    let gpu_wait = wait.elapsed();
    for layer in &mut canvases.layers {
        layer.mark_presented(layer.presented_revision().wrapping_add(1));
    }
    stats.total = started.elapsed();
    Ok(Rendered {
        framebuffer,
        stats,
        compose,
        gpu_wait,
    })
}

fn validate(cache: &FrameCache, size: (u32, u32), limit: u32) -> Result<(), String> {
    if size.0 == 0 || size.1 == 0 || cache.width < size.0 || cache.width > limit || size.1 > limit {
        return Err("retained canvas dimensions exceed output or texture limits".into());
    }
    if cache.layers.strips.len() != cache.strips.len()
        || cache.layers.strip_parts.len() != cache.strips.len()
    {
        return Err("retained strip scenes do not match destinations".into());
    }
    if tilo_cpu_raster::reads_backdrop(cache.layers.workspace.encoding())
        || cache
            .layers
            .strips
            .iter()
            .any(|strip| tilo_cpu_raster::reads_backdrop(strip.encoding()))
        || cache
            .layers
            .wheel
            .as_ref()
            .is_some_and(|wheel| tilo_cpu_raster::reads_backdrop(wheel.scene.encoding()))
        || tilo_cpu_raster::reads_backdrop(cache.layers.chrome.encoding())
    {
        return Err("retained layers require independent backdrop sampling".into());
    }
    for (index, strip) in cache.strips.iter().enumerate() {
        if strip
            .dest_x
            .checked_add(strip.dest_w)
            .is_none_or(|x| x > size.0)
            || strip
                .dest_y
                .checked_add(strip.dest_h)
                .is_none_or(|y| y > size.1)
            || strip
                .dest_x
                .checked_add(strip.scroll_x)
                .and_then(|x| x.checked_add(strip.dest_w))
                .is_none_or(|x| x > cache.width)
        {
            return Err(format!(
                "retained strip lies outside its canvas: destination=({},{},{},{}), scroll_x={}, canvas_width={}, output={}x{}",
                strip.dest_x,
                strip.dest_y,
                strip.dest_w,
                strip.dest_h,
                strip.scroll_x,
                cache.width,
                size.0,
                size.1
            ));
        }
        if cache.strips.iter().take(index).any(|other| {
            strip.dest_x < other.dest_x.saturating_add(other.dest_w)
                && other.dest_x < strip.dest_x.saturating_add(strip.dest_w)
                && strip.dest_y < other.dest_y.saturating_add(other.dest_h)
                && other.dest_y < strip.dest_y.saturating_add(strip.dest_h)
        }) {
            return Err("retained strips overlap".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::frame::{FrameLayers, StripBlit, WheelSprite};
    use vello::Scene;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    fn fill(scene: &mut Scene, rect: Rect, color: Color) {
        scene.fill(Fill::NonZero, Affine::IDENTITY, color, None, &rect);
    }

    fn cache(scroll_x: u32) -> FrameCache {
        let mut backdrop = Scene::new();
        fill(
            &mut backdrop,
            Rect::new(0.0, 0.0, 64.0, 32.0),
            Color::from_rgb8(0, 0, 255),
        );
        let mut strip_scene = Scene::new();
        fill(
            &mut strip_scene,
            Rect::new(80.0, 0.0, 112.0, 32.0),
            Color::from_rgb8(255, 0, 0),
        );
        FrameCache {
            width: 128,
            strips: vec![StripBlit {
                dest_x: 0,
                dest_y: 0,
                dest_w: 64,
                dest_h: 32,
                scroll_x,
            }],
            generation: 1,
            chrome_damage: None,
            layers: FrameLayers {
                backdrop,
                workspace: Scene::new(),
                strips: vec![strip_scene],
                strip_parts: vec![None],
                wheel: None,
                chrome: Scene::new(),
            },
        }
    }

    #[test]
    fn retained_views_include_the_current_per_strip_window_scene() {
        let mut cache = cache(0);
        cache.layers.workspace = Scene::new();
        fill(
            &mut cache.layers.strips[0],
            Rect::new(20.0, 0.0, 52.0, 32.0),
            Color::from_rgb8(255, 0, 0),
        );
        let mut canvases = Canvases::new();
        canvases.update(&cache, (64, 32));
        let views = canvases.views(&cache, (64, 32));
        let shown_red = views.iter().any(|view| {
            view.pieces.iter().any(|(dest, scroll_x)| {
                let (x, y) = (25, 4);
                if x < dest.x
                    || x >= dest.x.saturating_add(dest.width)
                    || y < dest.y
                    || y >= dest.y.saturating_add(dest.height)
                {
                    return false;
                }
                let source_x = usize::try_from(x.saturating_add(*scroll_x)).unwrap();
                let source_y = usize::try_from(y).unwrap();
                let at = source_y
                    .saturating_mul(view.stride)
                    .saturating_add(source_x.saturating_mul(4));
                view.pixels.get(at..at + 4) == Some(&[255, 0, 0, 255][..])
            })
        });
        assert!(
            shown_red,
            "the retained GLES input must carry strip windows"
        );
    }

    #[test]
    fn retained_wheel_layer_moves_without_changing_strip_canvas() {
        let mut cache = cache(0);
        let mut wheel = Scene::new();
        fill(
            &mut wheel,
            Rect::new(0.0, 0.0, 8.0, 8.0),
            Color::from_rgb8(0, 255, 0),
        );
        cache.layers.wheel = Some(WheelSprite {
            scene: wheel,
            width: 8,
            height: 8,
            x: 10,
            y: 4,
        });
        let mut canvases = Canvases::new();
        canvases.update(&cache, (64, 32));
        let before = canvases.views(&cache, (64, 32));
        let at = |x: usize, y: usize| y * before[3].stride + x * 4;
        assert_eq!(
            &before[3].pixels[at(11, 5)..at(11, 5) + 4],
            &[0, 255, 0, 255]
        );
        assert_eq!(&before[3].pixels[at(9, 5)..at(9, 5) + 4], &[0, 0, 0, 0]);
        let strip_generation = canvases.layers[2].generation();
        cache.layers.wheel.as_mut().unwrap().x = 13;
        canvases.update(&cache, (64, 32));
        let after = canvases.views(&cache, (64, 32));
        let at = |x: usize, y: usize| y * after[3].stride + x * 4;
        assert_eq!(
            &after[3].pixels[at(14, 5)..at(14, 5) + 4],
            &[0, 255, 0, 255]
        );
        assert_eq!(&after[3].pixels[at(11, 5)..at(11, 5) + 4], &[0, 0, 0, 0]);
        assert_eq!(canvases.layers[2].generation(), strip_generation);
    }

    #[test]
    fn retained_view_order_matches_the_dense_frame_with_two_strips_and_wheel() {
        let mut cache = cache(24);
        cache.strips[0].dest_h = 16;
        cache.strips.push(StripBlit {
            dest_x: 0,
            dest_y: 16,
            dest_w: 64,
            dest_h: 16,
            scroll_x: 48,
        });
        let mut second = Scene::new();
        fill(
            &mut second,
            Rect::new(60.0, 16.0, 80.0, 32.0),
            Color::from_rgb8(255, 0, 255),
        );
        cache.layers.strips.push(second);
        cache.layers.strip_parts.push(None);
        fill(
            &mut cache.layers.workspace,
            Rect::new(0.0, 0.0, 8.0, 32.0),
            Color::from_rgb8(0, 255, 0),
        );
        let mut wheel = Scene::new();
        fill(
            &mut wheel,
            Rect::new(0.0, 0.0, 8.0, 8.0),
            Color::from_rgb8(255, 255, 0),
        );
        cache.layers.wheel = Some(WheelSprite {
            scene: wheel,
            width: 8,
            height: 8,
            x: 14,
            y: 16,
        });
        fill(
            &mut cache.layers.chrome,
            Rect::new(15.0, 17.0, 17.0, 19.0),
            Color::WHITE,
        );
        validate(&cache, (64, 32), 128).expect("two disjoint strip layers");

        let dense = crate::shell::render::strip_layers::compose(&cache);
        let mut oracle = CpuRenderer::new();
        oracle.update(
            CpuFrame {
                scene: &dense,
                revision: 0,
                previous_revision: None,
                regions: None,
            },
            (64, 32),
        );
        let mut canvases = Canvases::new();
        canvases.update(&cache, (64, 32));
        let views = canvases.views(&cache, (64, 32));
        assert_eq!(views.len(), 6);
        for (x, y) in (0..32_u32).flat_map(|y| (0..64_u32).map(move |x| (x, y))) {
            let mut actual = [0, 0, 0, 255];
            for view in &views {
                for (dest, scroll_x) in &view.pieces {
                    if x < dest.x
                        || x >= dest.x.saturating_add(dest.width)
                        || y < dest.y
                        || y >= dest.y.saturating_add(dest.height)
                    {
                        continue;
                    }
                    let at = usize::try_from(y).unwrap() * view.stride
                        + usize::try_from(x.saturating_add(*scroll_x)).unwrap() * 4;
                    let pixel = &view.pixels[at..at + 4];
                    if pixel[3] == 255 {
                        actual.copy_from_slice(pixel);
                    }
                }
            }
            let at = (usize::try_from(y).unwrap() * 64 + usize::try_from(x).unwrap()) * 4;
            assert_eq!(actual, oracle.screen_rgba()[at..at + 4], "pixel {x},{y}");
        }
    }

    #[test]
    fn preserved_strip_pan_reuses_pixels_and_matches_the_dense_scene() {
        let mut canvases = Canvases::new();
        let first = cache(0);
        assert!(canvases.update(&first, (64, 32)).raster_px > 0);
        for layer in &mut canvases.layers {
            layer.mark_presented(1);
        }
        // Regression from the physical Pi stack: pan reveals the red window
        // over the fixed blue wallpaper without rasterizing any layer again.
        for scroll in [24, 40, 48, 64] {
            let panned = cache(scroll);
            assert_eq!(
                canvases.update(&panned, (64, 32)).raster_px,
                0,
                "pan {scroll}"
            );
            let views = canvases.views(&panned, (64, 32));
            let pieces = &views[2].pieces;
            assert_eq!(
                pieces,
                &vec![(
                    FineDamageRect {
                        x: 0,
                        y: 0,
                        width: 64,
                        height: 32
                    },
                    scroll
                )]
            );
            let mut dense = Scene::new();
            fill(
                &mut dense,
                Rect::new(0.0, 0.0, 64.0, 32.0),
                Color::from_rgb8(0, 0, 255),
            );
            let x = 80.0 - f64::from(scroll);
            fill(
                &mut dense,
                Rect::new(x, 0.0, x + 32.0, 32.0),
                Color::from_rgb8(255, 0, 0),
            );
            let mut oracle = CpuRenderer::new();
            oracle.update(
                CpuFrame {
                    scene: &dense,
                    revision: 0,
                    previous_revision: None,
                    regions: None,
                },
                (64, 32),
            );
            for x in 0..64_usize {
                let canvas_x = x + scroll as usize;
                let foreground = &views[2].pixels[canvas_x * 4..canvas_x * 4 + 4];
                let actual = if foreground[3] == 0 {
                    &views[0].pixels[x * 4..x * 4 + 4]
                } else {
                    foreground
                };
                assert_eq!(
                    actual,
                    &oracle.screen_rgba()[x * 4..x * 4 + 4],
                    "pan {scroll}, pixel {x}"
                );
            }
        }
    }

    #[test]
    fn multiple_strip_views_cover_only_their_destinations_above_workspace() {
        let mut cached = cache(24);
        cached.strips[0].dest_w = 20;
        cached.strips.push(StripBlit {
            dest_x: 30,
            dest_y: 8,
            dest_w: 20,
            dest_h: 16,
            scroll_x: 48,
        });
        cached.layers.strips.push(Scene::new());
        cached.layers.strip_parts.push(None);
        validate(&cached, (64, 32), 128).expect("disjoint strips");
        let full = FineDamageRect {
            x: 0,
            y: 0,
            width: 64,
            height: 32,
        };
        let mut canvases = Canvases::new();
        canvases.update(&cached, (64, 32));
        let views = canvases.views(&cached, (64, 32));
        assert_eq!(views[1].pieces, vec![(full, 0)]);
        for y in 0..32 {
            for x in 0..64 {
                let offsets: Vec<_> = views[2..4]
                    .iter()
                    .flat_map(|view| &view.pieces)
                    .filter(|(rect, _)| {
                        x >= rect.x
                            && x < rect.x + rect.width
                            && y >= rect.y
                            && y < rect.y + rect.height
                    })
                    .map(|(_, offset)| *offset)
                    .collect();
                let expected = if x < 20 {
                    vec![24]
                } else if (30..50).contains(&x) && (8..24).contains(&y) {
                    vec![48]
                } else {
                    vec![]
                };
                assert_eq!(offsets, expected, "pixel {x},{y}");
            }
        }
    }

    #[test]
    fn invalid_bounds_overlap_and_backdrop_reads_refuse_layer_composition() {
        let mut cached = cache(64);
        cached.strips[0].scroll_x = u32::MAX;
        assert!(validate(&cached, (64, 32), 128).is_err());
        cached.strips[0].scroll_x = 0;
        cached.strips.push(cached.strips[0]);
        cached.layers.strips.push(Scene::new());
        cached.layers.strip_parts.push(None);
        assert!(validate(&cached, (64, 32), 128).is_err());
        cached.strips.pop();
        cached.layers.strips.pop();
        cached.layers.strip_parts.pop();
        cached.width = 129;
        assert!(validate(&cached, (64, 32), 128).is_err());
        cached.width = 128;
        cached.layers.chrome.push_layer(
            Fill::NonZero,
            vello::peniko::BlendMode::new(
                vello::peniko::Mix::SoftLight,
                vello::peniko::Compose::SrcOver,
            ),
            1.0,
            Affine::IDENTITY,
            &Rect::new(0.0, 0.0, 64.0, 32.0),
        );
        fill(
            &mut cached.layers.chrome,
            Rect::new(0.0, 0.0, 64.0, 32.0),
            Color::from_rgb8(128, 0, 0),
        );
        cached.layers.chrome.pop_layer();
        assert!(validate(&cached, (64, 32), 128).is_err());
    }

    #[test]
    fn chrome_empty_transitions_clear_pixels_and_invalidate_uploads() {
        let mut cached = cache(24);
        let mut canvases = Canvases::new();
        canvases.update(&cached, (64, 32));
        let chrome = canvases.layers.len() - 1;
        for layer in &mut canvases.layers {
            layer.mark_presented(1);
        }
        fill(
            &mut cached.layers.chrome,
            Rect::new(0.0, 0.0, 64.0, 32.0),
            Color::from_rgb8(0, 255, 0),
        );
        assert!(canvases.update(&cached, (64, 32)).raster_px > 0);
        assert_eq!(
            &canvases.layers[chrome].screen_rgba()[..4],
            &[0, 255, 0, 255]
        );
        for layer in &mut canvases.layers {
            layer.mark_presented(2);
        }
        cached.layers.chrome = Scene::new();
        assert!(canvases.update(&cached, (64, 32)).raster_px > 0);
        assert!(canvases.layers[chrome].has_pending());
        assert!(
            canvases.layers[chrome]
                .screen_rgba()
                .iter()
                .all(|byte| *byte == 0)
        );
        for layer in &mut canvases.layers {
            layer.mark_presented(3);
        }
        assert_eq!(canvases.update(&cached, (64, 32)).raster_px, 0);
        assert!(!canvases.layers[chrome].has_pending());
    }
}
