//! Live-render test harness — drive a `ui` widget's REAL frame cycle headlessly
//! so a test can exercise the production data path (network → decode → draw),
//! not a mock and not sample data, and then inspect the resulting pixels.
//!
//! This is the reusable pattern for proving a backend/render-critical app
//! actually shows real content. Hermetic `--shots` prove *layout* only (they
//! render offline/sample data); this harness proves the *live* path:
//!
//!   * [`Harness::pump_frame`] runs one real frame — `sync` → `build_layout` →
//!     `compute_layout` → `apply_layout` → `draw` — exactly like `ui::App` and
//!     the `--shots` renderer. `draw` is what issues real tile requests; the
//!     next frame's `sync` drains the decoded results. No GPU needed.
//!   * [`pump_frames_until`] loops that until a caller predicate holds (e.g.
//!     "tiles resident") or a timeout elapses, sleeping between frames so the
//!     background worker can fetch + decode.
//!   * [`render_live_to_png`] runs one full frame AND rasterises it on the GPU
//!     (lavapipe is fine) to a PNG plus a tight RGBA buffer.
//!   * [`Frame`] wraps that buffer with content assertions (`distinct_colors`,
//!     `is_uniform`, `changed_fraction`) so a test can prove the map area shows
//!     real, varied imagery rather than a uniform fill or the placeholder.
//!
//! The module is generic over `ui::widget::Widget`, so any app can reuse it;
//! the map-specific predicates live in the app's own live tests.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use taffy::prelude::*;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::Color;

/// Per-run context reused across pumped frames: the theme, presence/permission
/// contexts, the text-shaping cache, the CRDT document and the viewport size.
/// One `Harness` drives one widget instance through many frames.
pub struct Harness {
    pub theme: Theme,
    pub presence: PresenceCtx,
    pub permission: PermissionContext,
    pub text: TextRenderer,
    pub state: ui::State,
    pub viewport: (u32, u32),
}

impl Harness {
    /// A harness at `viewport` size with the OS dark theme (matching `--shots`).
    pub fn new(viewport: (u32, u32)) -> Self {
        Self {
            theme: Theme::dark(),
            presence: PresenceCtx::empty(),
            permission: PermissionContext::permissive(),
            text: TextRenderer::new(),
            state: ui::State::new(),
            viewport,
        }
    }

    /// Run ONE real frame of `widget` into `scene`. Mirrors `ui::App`'s cycle
    /// (and `main.rs`'s `--shots` renderer) precisely: `sync` folds in any
    /// worker results, layout is recomputed, and `draw_with_presence` paints —
    /// which, for the map, computes `visible_tiles` and issues the real tile
    /// `request()`s. Passing a throwaway `scene` still drives those draw-side
    /// side effects, so the plumbing runs whether or not the frame is
    /// rasterised.
    pub fn pump_frame(&mut self, widget: &mut dyn Widget, scene: &mut Scene) {
        let (w, h) = self.viewport;
        widget.sync(&mut self.state.doc, &self.state.root);

        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let child = widget.build_layout(&mut taffy, &self.theme, &mut node_map);
        let Ok(root) = taffy.new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(w as f32),
                    height: Dimension::length(h as f32),
                },
                ..Default::default()
            },
            &[child],
        ) else {
            return;
        };
        {
            let measure_map = node_map.clone();
            let tr = &mut self.text;
            let laid_out = taffy
                .compute_layout_with_measure(
                    root,
                    taffy::Size {
                        width: AvailableSpace::Definite(w as f32),
                        height: AvailableSpace::Definite(h as f32),
                    },
                    |known, available, node_id, _, _| {
                        if let Some(&ptr) = measure_map.get(&node_id) {
                            unsafe { (*ptr).measure(known, available, tr) }
                        } else {
                            taffy::Size::ZERO
                        }
                    },
                )
                .is_ok();
            if !laid_out {
                return;
            }
        }
        widget.apply_layout(&taffy, child, Point::ORIGIN);
        widget.draw_with_presence(
            scene,
            Affine::IDENTITY,
            &mut self.text,
            &self.theme,
            &self.presence,
            &self.permission,
        );
    }
}

/// Outcome of a [`pump_frames_until`] loop.
#[derive(Clone, Copy, Debug)]
pub struct PumpStats {
    /// How many real frames were pumped.
    pub frames: u32,
    /// Wall-clock time the loop ran.
    pub elapsed: Duration,
    /// `true` if the predicate fired, `false` if it timed out.
    pub stopped: bool,
}

/// Pump real frames of `widget` until `stop(widget)` is true or `max_wait`
/// elapses, sleeping `poll` between frames so the background worker can make
/// progress. Draws into a throwaway `Scene` each frame purely to drive the
/// draw-side plumbing (tile requests) — no GPU is touched here, so this is the
/// cheap way to exercise a full data path and assert its in-memory outcome.
pub fn pump_frames_until<W, F>(
    h: &mut Harness,
    widget: &mut W,
    max_wait: Duration,
    poll: Duration,
    mut stop: F,
) -> PumpStats
where
    W: Widget,
    F: FnMut(&W) -> bool,
{
    let start = Instant::now();
    let mut frames = 0u32;
    loop {
        let mut scene = Scene::new();
        h.pump_frame(widget, &mut scene);
        frames += 1;
        if stop(widget) {
            return PumpStats {
                frames,
                elapsed: start.elapsed(),
                stopped: true,
            };
        }
        if start.elapsed() >= max_wait {
            return PumpStats {
                frames,
                elapsed: start.elapsed(),
                stopped: false,
            };
        }
        std::thread::sleep(poll);
    }
}

/// Run one real frame of `widget` and rasterise it on the GPU to `path`
/// (opaque-black background so "did anything paint" is unambiguous), returning
/// the tight RGBA buffer for pixel inspection. Requires a working offscreen
/// GPU; lavapipe (software Vulkan) is sufficient.
pub fn render_live_to_png<W: Widget>(h: &mut Harness, widget: &mut W, path: &Path) -> Frame {
    let (w, ht) = h.viewport;
    let mut scene = Scene::new();
    h.pump_frame(widget, &mut scene);
    let mut renderer = OffscreenRenderer::new(w, ht);
    let rgba = renderer.render(&scene, Color::BLACK);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    write_png(path, &rgba, w, ht);
    Frame {
        rgba,
        width: w,
        height: ht,
    }
}

/// Best-effort PNG write for debug inspection. A failure here (disk full,
/// permissions) must never fail the caller's in-memory pixel assertions, so
/// this silently gives up rather than panicking.
fn write_png(path: &Path, rgba: &[u8], w: u32, h: u32) {
    let Ok(file) = std::fs::File::create(path) else {
        return;
    };
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let Ok(mut writer) = enc.write_header() else {
        return;
    };
    let _ = writer.write_image_data(rgba);
}

/// A rasterised frame: tight-packed RGBA (`4 * width` bytes per row) plus its
/// dimensions, with content assertions used to prove a region shows real,
/// varied imagery rather than a placeholder or a uniform fill.
pub struct Frame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Frame {
    /// RGBA at `(x, y)`.
    pub fn px(&self, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let i = ((y * self.width + x) * 4) as usize;
        (
            self.rgba.get(i).copied().unwrap_or(0),
            self.rgba.get(i + 1).copied().unwrap_or(0),
            self.rgba.get(i + 2).copied().unwrap_or(0),
            self.rgba.get(i + 3).copied().unwrap_or(0),
        )
    }

    /// Clamp a region to the frame and yield integer pixel bounds.
    fn bounds(&self, region: Rect) -> (u32, u32, u32, u32) {
        let x0 = region.x0.max(0.0) as u32;
        let y0 = region.y0.max(0.0) as u32;
        let x1 = (region.x1.min(f64::from(self.width)) as u32).max(x0 + 1);
        let y1 = (region.y1.min(f64::from(self.height)) as u32).max(y0 + 1);
        (x0, y0, x1, y1)
    }

    /// Number of DISTINCT colours (quantised to 5 bits/channel to ignore
    /// anti-aliasing noise) sampled on a `step`-pixel grid inside `region`. A
    /// real tiled map — roads, parks, water, buildings, labels — has many
    /// dozens; a uniform fill or the flat "imagery unavailable" placeholder has
    /// a handful.
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y are the pixel-grid cursor and r/g/b the quantised channel values, the \
                  standard vocabulary for pixel sampling"
    )]
    pub fn distinct_colors(&self, region: Rect, step: u32) -> usize {
        let step = step.max(1);
        let (x0, y0, x1, y1) = self.bounds(region);
        let mut set = std::collections::HashSet::new();
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let (r, g, b, _) = self.px(x, y);
                set.insert((r >> 3, g >> 3, b >> 3));
                x += step;
            }
            y += step;
        }
        set.len()
    }

    /// Whether `region` is a single flat colour (≤ 1 distinct quantised colour
    /// on the sample grid). The honest "no imagery" states are near-uniform;
    /// real tiles never are.
    pub fn is_uniform(&self, region: Rect) -> bool {
        self.distinct_colors(region, 4) <= 1
    }

    /// Fraction of sampled pixels in `region` that differ from `other` by more
    /// than `tol` on any channel. Used to prove the map area actually *changed*
    /// once tiles loaded — i.e. real imagery replaced the placeholder — rather
    /// than trusting colour-diversity alone.
    #[allow(
        clippy::many_single_char_names,
        reason = "x/y are the pixel-grid cursor and a/b/d the two sampled pixels and their \
                  difference, the standard vocabulary for pixel sampling"
    )]
    pub fn changed_fraction(&self, other: &Frame, region: Rect, step: u32, tol: u8) -> f64 {
        let step = step.max(1);
        let (x0, y0, x1, y1) = self.bounds(region);
        let mut total = 0u64;
        let mut changed = 0u64;
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                let a = self.px(x, y);
                let b = other.px(x, y);
                let d =
                    a.0.abs_diff(b.0)
                        .max(a.1.abs_diff(b.1))
                        .max(a.2.abs_diff(b.2));
                if d > tol {
                    changed += 1;
                }
                total += 1;
                x += step;
            }
            y += step;
        }
        if total == 0 {
            0.0
        } else {
            changed as f64 / total as f64
        }
    }
}
