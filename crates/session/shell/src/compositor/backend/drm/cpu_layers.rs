//! The desktop's layers, each kept by the CPU in a canvas of its own, for
//! the GLES compositor ([`super::gl_compose`]).
//!
//! A frame drawn as layers ([`crate::shell::frame::FrameCache`]) carries the
//! backdrop, the workspace behind the strips, each strip in canvas
//! coordinates and the chrome above. Each is kept in its own
//! [`RetainedCanvas`]: a layer whose scene is byte for byte the one it holds
//! is not looked at again, and one that changed is compared with it, so only
//! what changed is rasterized. A strip's canvas spans everything its windows
//! reach; only the part its pan shows is rasterized, and a pan only moves
//! where the compositor reads it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "screen and canvas rectangles whose sides fit u16; damage rects are clamped at zero before they become pixels"
)]

use tilo_cpu_raster::Region;
use vello::FineDamageRect;
use vello::Scene;

use super::cpu_present::CpuFrameStats;
use super::gl_compose::{LayerDraw, LayerKey, Placement};
use super::retained_canvas::{Damage, RetainedCanvas};
use crate::shell::frame::{FrameCache, StripBlit};
use ui::scene_parts::SectionParts;

/// Widest strip canvas kept: wider strips pan in the scene instead. A
/// canvas this wide and as tall as a 1080p screen holds 35 MB.
pub const MAX_STRIP_CANVAS: u32 = 8192;

/// Strip canvases grow in steps of this many pixels.
const STRIP_WIDTH_STEP: u32 = 1024;

/// One layer's canvas, the scene it holds and what the compositor has not
/// uploaded yet.
struct LayerCanvas {
    canvas: RetainedCanvas,
    scene: Option<Scene>,
    /// The damage parts `scene` was drawn with, when it was.
    parts: Option<SectionParts>,
    unuploaded: Vec<Region>,
}

/// What the producer says about where a layer changed.
enum Hint<'a> {
    /// Nothing: the decoded draws decide.
    None,
    /// The parts the new scene was drawn with: compared with the held
    /// scene's, they say which parts changed.
    Parts(Option<&'a SectionParts>),
    /// Where the scene differs from the one the canvas holds, when known.
    Rects(Option<Vec<FineDamageRect>>),
}

/// Rects in layer pixels, rounded out to whole pixels.
fn whole_pixels(rects: &[vello::kurbo::Rect]) -> Vec<FineDamageRect> {
    rects
        .iter()
        .filter_map(|rect| {
            let rect = rect.expand();
            let x0 = rect.x0.max(0.0);
            let y0 = rect.y0.max(0.0);
            (rect.x1 > x0 && rect.y1 > y0).then_some(FineDamageRect {
                x: x0 as u32,
                y: y0 as u32,
                width: (rect.x1 - x0) as u32,
                height: (rect.y1 - y0) as u32,
            })
        })
        .collect()
}

impl LayerCanvas {
    fn new(threads: u16) -> Self {
        Self {
            canvas: RetainedCanvas::new(threads),
            scene: None,
            parts: None,
            unuploaded: Vec::new(),
        }
    }

    /// Bring the canvas up to `scene` at `size`, rasterizing only within
    /// `shown`.
    fn update(
        &mut self,
        scene: &Scene,
        hint: Hint<'_>,
        size: (u32, u32),
        shown: FineDamageRect,
        stats: &mut CpuFrameStats,
    ) {
        if self.canvas.size() != size {
            self.canvas.resize(size.0, size.1);
            self.scene = None;
            self.unuploaded.clear();
        }
        let new_parts = match &hint {
            Hint::Parts(parts) => parts.cloned(),
            _ => None,
        };
        let rects = match (&self.scene, hint) {
            (Some(held), Hint::Parts(Some(parts))) => self
                .parts
                .as_ref()
                .and_then(|old| ui::scene_parts::damage(held, old, scene, parts).ok())
                .map(|rects| whole_pixels(&rects)),
            (Some(_), Hint::Rects(rects)) => rects,
            _ => None,
        };
        let damage = match (&self.scene, &rects) {
            (Some(held), _) if held.encoding_eq(scene) => Damage::None,
            (Some(_), Some(rects)) => Damage::Within(rects),
            _ => Damage::Unknown,
        };
        let moved = self.canvas.apply(scene, damage, stats);
        if !matches!(damage, Damage::None) {
            self.scene = Some(scene.clone());
        }
        self.parts = new_parts;
        let drawn = self.canvas.rasterize(Some(shown), stats);
        self.unuploaded.extend(moved);
        self.unuploaded.extend(drawn);
        // Past this many rectangles one upload of their bounds is cheaper.
        if self.unuploaded.len() > 64 {
            let bounds = self
                .unuploaded
                .iter()
                .copied()
                .reduce(|a, b| {
                    let x0 = a.x.min(b.x);
                    let y0 = a.y.min(b.y);
                    let x1 = (a.x + a.width).max(b.x + b.width);
                    let y1 = (a.y + a.height).max(b.y + b.height);
                    Region::new(x0, y0, x1 - x0, y1 - y0)
                })
                .unwrap_or_default();
            self.unuploaded = vec![bounds];
        }
    }

    fn draw(&self, key: LayerKey, placements: Vec<Placement>) -> LayerDraw<'_> {
        LayerDraw {
            key,
            canvas: &self.canvas,
            written: &self.unuploaded,
            placements,
        }
    }
}

/// The canvases of every layer of the desktop.
pub struct CpuLayers {
    threads: u16,
    backdrop: LayerCanvas,
    workspace: LayerCanvas,
    strips: Vec<LayerCanvas>,
    chrome: LayerCanvas,
    /// The drawing ([`FrameCache::generation`]) whose chrome the chrome
    /// canvas holds.
    chrome_generation: Option<u64>,
    /// The screen the layers were last drawn for, and where each strip shows.
    screen: (u32, u32),
    blits: Vec<StripBlit>,
}

impl std::fmt::Debug for CpuLayers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CpuLayers")
            .field("screen", &self.screen)
            .field("strips", &self.strips.len())
            .finish_non_exhaustive()
    }
}

fn full(size: (u32, u32)) -> FineDamageRect {
    FineDamageRect {
        x: 0,
        y: 0,
        width: size.0,
        height: size.1,
    }
}

/// The canvas pixels strip `blit` shows: its destination, read `scroll_x`
/// pixels to the right.
fn strip_source(blit: &StripBlit) -> FineDamageRect {
    FineDamageRect {
        x: blit.dest_x + blit.scroll_x,
        y: blit.dest_y,
        width: blit.dest_w,
        height: blit.dest_h,
    }
}

impl CpuLayers {
    pub fn new(threads: u16) -> Self {
        Self {
            threads,
            backdrop: LayerCanvas::new(threads),
            workspace: LayerCanvas::new(threads),
            strips: Vec::new(),
            chrome: LayerCanvas::new(threads),
            chrome_generation: None,
            screen: (0, 0),
            blits: Vec::new(),
        }
    }

    /// Bring every layer up to `cache` on a `screen`-sized output.
    pub fn update(&mut self, cache: &FrameCache, screen: (u32, u32), stats: &mut CpuFrameStats) {
        let layers = &cache.layers;
        // Pixels each layer rasterizes, for the frame's log line.
        let mut drawn = Vec::new();
        let mut update = |layer: &mut LayerCanvas,
                          scene: &Scene,
                          hint: Hint<'_>,
                          size: (u32, u32),
                          shown: FineDamageRect,
                          stats: &mut CpuFrameStats| {
            let before = stats.raster_px;
            layer.update(scene, hint, size, shown, stats);
            drawn.push(stats.raster_px.saturating_sub(before).to_string());
        };
        update(
            &mut self.backdrop,
            &layers.backdrop,
            Hint::None,
            screen,
            full(screen),
            stats,
        );
        update(
            &mut self.workspace,
            &layers.workspace,
            Hint::None,
            screen,
            full(screen),
            stats,
        );
        let threads = self.threads;
        self.strips
            .resize_with(layers.strips.len(), || LayerCanvas::new(threads));
        // Rounded up, so a strip that grows by a window seldom reallocates
        // (and rasterizes again) its canvas.
        let width = cache
            .width
            .max(screen.0)
            .div_ceil(STRIP_WIDTH_STEP)
            .saturating_mul(STRIP_WIDTH_STEP)
            .min(MAX_STRIP_CANVAS.max(screen.0));
        let canvas = (width, screen.1);
        for (index, ((strip, scene), blit)) in self
            .strips
            .iter_mut()
            .zip(&layers.strips)
            .zip(&cache.strips)
            .enumerate()
        {
            let parts = layers.strip_parts.get(index).and_then(Option::as_ref);
            update(
                strip,
                scene,
                Hint::Parts(parts),
                canvas,
                strip_source(blit),
                stats,
            );
        }
        // The chrome's damage holds against the drawing it was measured
        // from; only a canvas holding that drawing may use it.
        let chrome_rects = cache
            .chrome_damage
            .as_ref()
            .filter(|damage| self.chrome_generation == Some(damage.since))
            .map(|damage| damage.rects.clone());
        update(
            &mut self.chrome,
            &layers.chrome,
            Hint::Rects(chrome_rects),
            screen,
            full(screen),
            stats,
        );
        self.chrome_generation = Some(cache.generation);
        let scrolls: Vec<String> = cache
            .strips
            .iter()
            .map(|blit| blit.scroll_x.to_string())
            .collect();
        stats.layers = format!("{} scroll={}", drawn.join(","), scrolls.join("/"));
        self.screen = screen;
        self.blits.clone_from(&cache.strips);
    }

    /// The layers bottom to top, as the compositor draws them.
    pub fn draws(&self) -> Vec<LayerDraw<'_>> {
        let whole = || {
            vec![Placement {
                src: full(self.screen),
                dst: (0, 0),
            }]
        };
        let mut draws = vec![
            self.backdrop.draw(LayerKey::Backdrop, whole()),
            self.workspace.draw(LayerKey::Workspace, whole()),
        ];
        for (index, (strip, blit)) in self.strips.iter().zip(&self.blits).enumerate() {
            let placement = Placement {
                src: strip_source(blit),
                dst: (
                    i32::try_from(blit.dest_x).unwrap_or(i32::MAX),
                    i32::try_from(blit.dest_y).unwrap_or(i32::MAX),
                ),
            };
            draws.push(strip.draw(LayerKey::Strip(index), vec![placement]));
        }
        draws.push(self.chrome.draw(LayerKey::Chrome, whole()));
        draws
    }

    /// A canvas changed since the compositor last uploaded it.
    pub fn unuploaded(&self) -> bool {
        [&self.backdrop, &self.workspace, &self.chrome]
            .into_iter()
            .chain(&self.strips)
            .any(|layer| !layer.unuploaded.is_empty())
    }

    /// The compositor has every layer as the canvases hold it.
    pub fn uploaded(&mut self) {
        for layer in [&mut self.backdrop, &mut self.workspace, &mut self.chrome]
            .into_iter()
            .chain(self.strips.iter_mut())
        {
            layer.unuploaded.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::frame::FrameLayers;
    use vello::kurbo::{Affine, Rect};
    use vello::peniko::{Color, Fill};

    fn filled(rects: &[(Rect, Color)]) -> Scene {
        let mut scene = Scene::new();
        for (rect, color) in rects {
            scene.fill(Fill::NonZero, Affine::IDENTITY, *color, None, rect);
        }
        scene
    }

    fn cache(scroll_x: u32) -> FrameCache {
        let white = Color::from_rgb8(255, 255, 255);
        // A strip canvas three screens wide, a window on each screen.
        let strip = filled(&[
            (Rect::new(16.0, 32.0, 112.0, 96.0), white),
            (Rect::new(144.0, 32.0, 240.0, 96.0), white),
            (Rect::new(272.0, 32.0, 368.0, 96.0), white),
        ]);
        FrameCache {
            width: 384,
            strips: vec![StripBlit {
                dest_x: 0,
                dest_y: 16,
                dest_w: 128,
                dest_h: 96,
                scroll_x,
            }],
            layers: FrameLayers {
                backdrop: filled(&[(
                    Rect::new(0.0, 0.0, 128.0, 128.0),
                    Color::from_rgb8(0, 0, 80),
                )]),
                workspace: Scene::new(),
                strips: vec![strip],
                strip_parts: vec![None],
                chrome: filled(&[(
                    Rect::new(0.0, 0.0, 128.0, 16.0),
                    Color::from_rgb8(30, 30, 30),
                )]),
            },
            generation: 1,
            chrome_damage: None,
        }
    }

    #[test]
    fn a_pan_rasterizes_only_what_it_reveals_and_moves_where_the_strip_is_read() {
        let mut layers = CpuLayers::new(0);
        let mut stats = CpuFrameStats::default();
        layers.update(&cache(0), (128, 128), &mut stats);
        let first = stats.raster_px;
        assert!(first > 0);
        let strip_source = |layers: &CpuLayers| {
            layers
                .draws()
                .into_iter()
                .find(|draw| draw.key == LayerKey::Strip(0))
                .map(|draw| draw.placements[0])
        };
        assert_eq!(
            strip_source(&layers),
            Some(Placement {
                src: FineDamageRect {
                    x: 0,
                    y: 16,
                    width: 128,
                    height: 96
                },
                dst: (0, 16)
            })
        );
        layers.uploaded();

        let mut stats = CpuFrameStats::default();
        layers.update(&cache(0), (128, 128), &mut stats);
        assert_eq!(stats.raster_px, 0, "the same layers again");
        assert!(!layers.unuploaded());

        // Panned by 32 pixels: only the columns that came into view are
        // drawn, in the strip's canvas.
        let mut stats = CpuFrameStats::default();
        layers.update(&cache(32), (128, 128), &mut stats);
        assert!(stats.raster_px > 0, "{stats:?}");
        assert!(
            stats.raster_px <= 32 * 96,
            "only the revealed columns: {stats:?}"
        );
        assert_eq!(strip_source(&layers).map(|p| p.src.x), Some(32));

        // Panned back: everything shown is still in the canvas.
        let mut stats = CpuFrameStats::default();
        layers.update(&cache(0), (128, 128), &mut stats);
        assert_eq!(stats.raster_px, 0, "{stats:?}");
    }
}
