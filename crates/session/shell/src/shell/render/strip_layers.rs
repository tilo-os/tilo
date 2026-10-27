//! Strip pans composited from retained layers.
//!
//! While a strip pans, the frame producer draws the desktop as layers
//! ([`FrameLayers`]): what is behind every window, each strip on its own in
//! canvas coordinates, and every piece of chrome above them, all in scene
//! (device) pixels. A pan then changes only where each strip reads its canvas
//! ([`StripBlit::scroll_x`]); the layers' scenes stay byte for byte the same.
//!
//! `StripLayers` keeps the backdrop, the strips and the chrome rasterised in
//! textures of the output's size. The strips texture holds the strips as the
//! screen shows them ([`append_panned`]). On a pure pan, a pixel whose strips all moved by the
//! same distance is copied from where it was, and only the rest is
//! rasterised: the columns the pan uncovered, and the bands where a moving
//! strip's drawing meets a strip that moved otherwise (each strip draws its
//! shadows past its row, into its neighbour's). The output is then composited
//! from the three textures (source over, premultiplied, as vello blends), over
//! what changed. The backdrop and the chrome are rasterised again only when
//! their scenes change.
//!
//! The composite stores straight alpha, as vello's fine stage does, so a
//! composited output matches a single render of the same frame within a
//! channel step of rounding.

use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;
use vello::{FineDamageRect, Scene, wgpu};

use crate::shell::frame::{FrameCache, FrameLayers, StripBlit};

/// More uncovered bands than this and the strips layer is rasterised whole:
/// a sparse render of that many rects costs about what a whole one does.
const MAX_BANDS: usize = 32;

/// `strip`'s destination as a rect in scene pixels.
fn dest_rect(strip: &StripBlit) -> Rect {
    Rect::new(
        f64::from(strip.dest_x),
        f64::from(strip.dest_y),
        f64::from(strip.dest_x) + f64::from(strip.dest_w),
        f64::from(strip.dest_y) + f64::from(strip.dest_h),
    )
}

/// The strips layer as the screen shows it: what no strip carries in place,
/// then each strip, top to bottom, inside its destination read `scroll_x`
/// pixels to the right of it.
pub fn append_panned(scene: &mut Scene, layers: &FrameLayers, strips: &[StripBlit]) {
    scene.append(&layers.workspace, None);
    for (strip, drawn) in strips.iter().zip(&layers.strips) {
        scene.push_clip_layer(Fill::NonZero, Affine::IDENTITY, &dest_rect(strip));
        scene.append(
            drawn,
            Some(Affine::translate((-f64::from(strip.scroll_x), 0.0))),
        );
        scene.pop_layer();
    }
}

/// The frame `cache` holds as one scene: its layers over each other, as
/// `StripLayers` composites them.
pub fn compose(cache: &FrameCache) -> Scene {
    let mut scene = cache.layers.backdrop.clone();
    append_panned(&mut scene, &cache.layers, &cache.strips);
    scene.append(&cache.layers.chrome, None);
    scene
}

/// What a set of layer textures shows: the layers of the frame last
/// composited from them, or warmed into them at rest
/// ([`crate::shell::render::paint::RetainedGpuTarget::warm_layers`]). A
/// layer is `None` while its texture holds nothing known: a warm stopped
/// while drawing it.
pub(crate) struct Shown {
    backdrop: Option<Scene>,
    /// The chrome, and the [`FrameCache::generation`] of the drawing it is
    /// of.
    chrome: Option<(Scene, u64)>,
    strips: Option<ShownStrips>,
    /// The frame whose composite the output target was last given; `None`
    /// when the output was not composited from what the textures hold.
    pub(crate) revision: Option<u64>,
}

/// What the strips texture shows.
struct ShownStrips {
    workspace: Scene,
    drawn: Vec<Scene>,
    /// The damage parts of `drawn`, in the same order.
    parts: Vec<Option<ui::scene_parts::SectionParts>>,
    width: u32,
    strips: Vec<StripBlit>,
}

impl ShownStrips {
    fn of(cache: &FrameCache) -> Self {
        Self {
            workspace: cache.layers.workspace.clone(),
            drawn: cache.layers.strips.clone(),
            parts: cache.layers.strip_parts.clone(),
            width: cache.width,
            strips: cache.strips.clone(),
        }
    }
}

impl Shown {
    /// Textures holding every layer of `cache`, composited into the output
    /// as `revision`.
    pub(crate) fn of(cache: &FrameCache, revision: u64) -> Self {
        Self {
            backdrop: Some(cache.layers.backdrop.clone()),
            chrome: Some((cache.layers.chrome.clone(), cache.generation)),
            strips: Some(ShownStrips::of(cache)),
            revision: Some(revision),
        }
    }

    /// Textures holding nothing known.
    pub(crate) fn unknown() -> Self {
        Self {
            backdrop: None,
            chrome: None,
            strips: None,
            revision: None,
        }
    }

    /// Every texture holds a known layer.
    fn complete(&self) -> bool {
        self.backdrop.is_some() && self.chrome.is_some() && self.strips.is_some()
    }

    /// Which drawing's chrome the chrome texture holds
    /// ([`FrameCache::generation`]), when it holds a known one.
    pub(crate) fn chrome_generation(&self) -> Option<u64> {
        self.chrome.as_ref().map(|(_, generation)| *generation)
    }

    /// `layer`'s texture is about to be drawn: until it is done it holds
    /// nothing known.
    pub(crate) fn forget(&mut self, layer: Layer) {
        match layer {
            Layer::Backdrop => self.backdrop = None,
            Layer::Workspace => self.strips = None,
            Layer::Chrome => self.chrome = None,
        }
    }

    /// `layer`'s texture now holds `cache`'s.
    pub(crate) fn record(&mut self, layer: Layer, cache: &FrameCache) {
        match layer {
            Layer::Backdrop => self.backdrop = Some(cache.layers.backdrop.clone()),
            Layer::Workspace => self.strips = Some(ShownStrips::of(cache)),
            Layer::Chrome => self.chrome = Some((cache.layers.chrome.clone(), cache.generation)),
        }
    }
}

/// Part of the strips layer that a pan moves: `width × height` pixels at
/// `(src_x, y)` go to `(dst_x, y)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BandCopy {
    pub(crate) src_x: u32,
    pub(crate) dst_x: u32,
    pub(crate) y: u32,
    pub(crate) width: u32,
    pub(crate) height: u32,
}

/// What the strips layer needs for a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum WorkspaceWork {
    /// It already shows the frame.
    Unchanged,
    /// The strips only panned, or one changed: copy what stays on screen,
    /// then rasterise what the pans uncovered and what changed.
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
    /// Where the chrome is rasterised again when it is: only these tiles
    /// ([`chrome_damage`]), or the whole layer with `None`.
    pub(crate) chrome_tiles: Option<Vec<FineDamageRect>>,
    pub(crate) workspace: WorkspaceWork,
    /// The output rect to composite again; `None` when the output already
    /// shows the frame.
    pub(crate) composite: Option<FineDamageRect>,
}

impl LayerWork {
    fn whole(size: (u32, u32)) -> Self {
        Self {
            backdrop: true,
            chrome: true,
            chrome_tiles: None,
            workspace: WorkspaceWork::Whole,
            composite: Some(full_rect(size)),
        }
    }

    /// Pixels this frame rasterises with vello, of an output of `size`.
    pub(crate) fn raster_pixels(&self, size: (u32, u32)) -> u64 {
        let whole = area(full_rect(size));
        let chrome = match (&self.chrome_tiles, self.chrome) {
            (_, false) => 0,
            (Some(tiles), true) => tiles.iter().copied().map(area).fold(0, u64::saturating_add),
            (None, true) => whole,
        };
        let layers = u64::from(self.backdrop);
        let workspace = match &self.workspace {
            WorkspaceWork::Unchanged => 0,
            WorkspaceWork::Pan { uncovered, .. } => uncovered
                .iter()
                .copied()
                .map(area)
                .fold(0, u64::saturating_add),
            WorkspaceWork::Whole => whole,
        };
        whole
            .saturating_mul(layers)
            .saturating_add(chrome)
            .saturating_add(workspace)
    }

    /// Pixels this frame composites.
    pub(crate) fn composite_pixels(&self) -> u64 {
        self.composite.map_or(0, area)
    }

    /// The layer textures already hold the frame: nothing to rasterise.
    pub(crate) fn rasterises_nothing(&self) -> bool {
        !self.backdrop && !self.chrome && self.workspace == WorkspaceWork::Unchanged
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

/// Most pixels a warm at rest rasterises in one render: a frame that comes
/// while the layers warm waits for one band, not for a whole layer. A band
/// of a million pixels is a fifth of the 2944x1840 panel, a few
/// milliseconds of raster on its GPU, so a keystroke arriving mid-warm
/// waits that long rather than for half a dense frame.
const WARM_BAND_PIXELS: u64 = 1 << 20;

/// Rows of a warm band are a multiple of vello's tile height, so no tile is
/// drawn by two bands.
const WARM_BAND_ROWS: u32 = 16;

/// The bands a warm rasterises a whole layer of an output of `size` in, top
/// to bottom; `None` when one render takes the whole layer.
pub(crate) fn warm_bands(size: (u32, u32)) -> Option<Vec<FineDamageRect>> {
    let count = area(full_rect(size)).div_ceil(WARM_BAND_PIXELS);
    if count <= 1 {
        return None;
    }
    let count = u32::try_from(count).ok()?;
    let rows = size
        .1
        .div_ceil(count)
        .div_ceil(WARM_BAND_ROWS)
        .saturating_mul(WARM_BAND_ROWS)
        .max(WARM_BAND_ROWS);
    let mut bands = Vec::new();
    let mut y = 0_u32;
    while y < size.1 {
        let height = rows.min(size.1.saturating_sub(y));
        bands.push(FineDamageRect {
            x: 0,
            y,
            width: size.0,
            height,
        });
        y = y.saturating_add(height);
    }
    Some(bands)
}

/// Where the chrome texture, holding `shown`'s chrome, must be rasterised
/// again to hold `cache`'s, on an output of `size`: the rects
/// [`FrameCache::chrome_damage`] names, when the texture holds the drawing
/// they were measured from. `None` when that is not known: the whole layer.
pub(crate) fn chrome_damage(
    shown: &Shown,
    cache: &FrameCache,
    size: (u32, u32),
) -> Option<Vec<FineDamageRect>> {
    let damage = cache.chrome_damage.as_ref()?;
    if shown.chrome_generation() != Some(damage.since) {
        return None;
    }
    Some(
        damage
            .rects
            .iter()
            .filter_map(|rect| {
                let x1 = rect.x.saturating_add(rect.width).min(size.0);
                let y1 = rect.y.saturating_add(rect.height).min(size.1);
                (x1 > rect.x && y1 > rect.y).then(|| FineDamageRect {
                    x: rect.x,
                    y: rect.y,
                    width: x1.saturating_sub(rect.x),
                    height: y1.saturating_sub(rect.y),
                })
            })
            .collect(),
    )
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
    let differs =
        |layer: Option<&Scene>, scene: &Scene| layer.is_none_or(|layer| !layer.encoding_eq(scene));
    let backdrop = differs(shown.backdrop.as_ref(), &cache.layers.backdrop);
    let chrome = differs(
        shown.chrome.as_ref().map(|(chrome, _)| chrome),
        &cache.layers.chrome,
    );
    let workspace = shown
        .strips
        .as_ref()
        .map_or(WorkspaceWork::Whole, |strips| {
            workspace_work(strips, cache, size)
        });
    // A chrome texture holding the drawing the chrome changed from takes
    // only the tiles that changed.
    let chrome_tiles = chrome.then(|| chrome_damage(shown, cache, size)).flatten();
    // A texture that holds nothing known was never composited into the
    // output.
    let output_current = output_current && shown.complete();
    let composite = if backdrop
        || (chrome && chrome_tiles.is_none())
        || workspace == WorkspaceWork::Whole
        || !output_current
    {
        Some(full_rect(size))
    } else {
        let moved = match &workspace {
            WorkspaceWork::Pan { copies, uncovered } => copies
                .iter()
                .map(|copy| FineDamageRect {
                    x: copy.src_x.min(copy.dst_x),
                    y: copy.y,
                    width: copy.width.saturating_add(copy.src_x.abs_diff(copy.dst_x)),
                    height: copy.height,
                })
                .chain(uncovered.iter().copied())
                .collect(),
            WorkspaceWork::Unchanged | WorkspaceWork::Whole => Vec::new(),
        };
        let tiles = chrome_tiles.iter().flatten().copied();
        moved
            .into_iter()
            .chain(tiles)
            .fold(None, |composite, rect| Some(union(composite, rect)))
    };
    LayerWork {
        backdrop,
        chrome,
        chrome_tiles,
        workspace,
        composite,
    }
}

/// One strip as the planner sees it: where it is shown, how far it moved
/// since the shown frame (positive: its content moved left), and how its
/// drawing changed.
#[derive(Clone)]
struct Piece {
    band: (u32, u32, u32, u32),
    delta: i64,
    change: Change,
}

/// How far inside an output's edges a changed window group's columns are
/// looked for ([`cover_lane`]): the columns a change can round onto a tile
/// just past the group it belongs to, where the output's own edge is not.
const COLUMN_EDGE: u32 = 16;

/// How a strip's drawing changed since the shown frame.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Change {
    Unchanged,
    /// Only inside these output rects, where the strip shows them now: the
    /// old and new bounds of the window groups whose drawing changed, in
    /// whole 16-pixel tiles, within the strip's band. Empty when what
    /// changed is nowhere on screen.
    Within(Vec<FineDamageRect>),
    /// Anywhere: the change is not bounded by the strip's damage parts.
    Anywhere,
}

/// Vello rasterises in tiles of this many device pixels a side
/// (`TILE_WIDTH`/`TILE_HEIGHT` in `vello_encoding`'s config); a changed edge can
/// round its tile's other pixels by a unit, so a change is repaired in whole
/// tiles. A raster unit, not a spacing.
const TILE_EDGE: u32 = 16;

/// How `new_drawn` (with `new_parts`), shown through `blit` on an output of
/// `size`, changed from `old_drawn` (with `old_parts`).
fn strip_change(
    (old_drawn, old_parts): (&Scene, Option<&ui::scene_parts::SectionParts>),
    (new_drawn, new_parts): (&Scene, Option<&ui::scene_parts::SectionParts>),
    blit: &StripBlit,
    size: (u32, u32),
) -> Change {
    if old_drawn.encoding_eq(new_drawn) {
        return Change::Unchanged;
    }
    let (Some(old_parts), Some(new_parts)) = (old_parts, new_parts) else {
        return Change::Anywhere;
    };
    let Ok(rects) = ui::scene_parts::damage(old_drawn, old_parts, new_drawn, new_parts) else {
        return Change::Anywhere;
    };
    let (bx0, by0, bx1, by1) = band(blit, size);
    let scroll = f64::from(blit.scroll_x);
    let mut within = Vec::new();
    // The strip is drawn in canvas coordinates and shown `scroll_x` pixels to
    // the left ([`append_panned`]), clipped to its band.
    let edge = f64::from(TILE_EDGE);
    let tile = |value: f64, round: fn(f64) -> f64| round(value / edge) * edge;
    let clamp = |value: f64, low: u32, high: u32| -> u32 {
        ui::num::u32_of_f64(value.clamp(f64::from(low), f64::from(high)))
    };
    for rect in rects {
        let x0 = clamp(tile(rect.x0 - scroll, f64::floor), bx0, bx1);
        let x1 = clamp(tile(rect.x1 - scroll, f64::ceil), bx0, bx1);
        let y0 = clamp(tile(rect.y0, f64::floor), by0, by1);
        let y1 = clamp(tile(rect.y1, f64::ceil), by0, by1);
        if x1 > x0 && y1 > y0 {
            within.push(FineDamageRect {
                x: x0,
                y: y0,
                width: x1.saturating_sub(x0),
                height: y1.saturating_sub(y0),
            });
        }
    }
    if within.is_empty() {
        Change::Unchanged
    } else {
        Change::Within(within)
    }
}

/// `rect` cut to the run `x0..x1` of rows `y0..y1`, if any of it is there.
fn clipped(
    rect: &FineDamageRect,
    (x0, y0, x1, y1): (u32, u32, u32, u32),
) -> Option<FineDamageRect> {
    let left = rect.x.max(x0);
    let top = rect.y.max(y0);
    let right = rect.x.saturating_add(rect.width).min(x1);
    let bottom = rect.y.saturating_add(rect.height).min(y1);
    (right > left && bottom > top).then(|| FineDamageRect {
        x: left,
        y: top,
        width: right.saturating_sub(left),
        height: bottom.saturating_sub(top),
    })
}

fn workspace_work(shown: &ShownStrips, cache: &FrameCache, size: (u32, u32)) -> WorkspaceWork {
    let same_strips = shown.strips.len() == cache.strips.len()
        && shown.drawn.len() == cache.layers.strips.len()
        && cache.strips.len() == cache.layers.strips.len()
        && shown.strips.iter().zip(&cache.strips).all(|(old, new)| {
            (old.dest_x, old.dest_y, old.dest_w, old.dest_h)
                == (new.dest_x, new.dest_y, new.dest_w, new.dest_h)
        });
    if shown.width != cache.width
        || !same_strips
        || !shown.workspace.encoding_eq(&cache.layers.workspace)
    {
        return WorkspaceWork::Whole;
    }
    let pieces: Vec<Piece> = shown
        .strips
        .iter()
        .zip(&cache.strips)
        .zip(shown.drawn.iter().zip(&cache.layers.strips))
        .enumerate()
        .map(|(index, ((old, new), (old_drawn, new_drawn)))| {
            let change = strip_change(
                (old_drawn, shown.parts.get(index).and_then(Option::as_ref)),
                (
                    new_drawn,
                    cache.layers.strip_parts.get(index).and_then(Option::as_ref),
                ),
                new,
                size,
            );
            Piece {
                band: band(new, size),
                delta: i64::from(new.scroll_x).saturating_sub(i64::from(old.scroll_x)),
                change,
            }
        })
        .collect();
    // What no strip carries is drawn in place under the strips: where it
    // draws, a strip's pixels cannot be moved without it.
    let in_place_drawn = !cache.layers.workspace.encoding().is_empty();
    let (copies, uncovered) = pan_work(&pieces, in_place_drawn, cache.width);
    if uncovered.len() > MAX_BANDS {
        WorkspaceWork::Whole
    } else if copies.is_empty() && uncovered.is_empty() {
        WorkspaceWork::Unchanged
    } else {
        WorkspaceWork::Pan { copies, uncovered }
    }
}

/// What a frame copies and what it rasterises on the strips layer.
///
/// The screen is cut into rows at every strip's top and bottom edge, and
/// each row into runs of pixels covered by the same strips. A run whose
/// strips all moved by `d` shows what the pixels `d` to its right showed,
/// which the same strips covered: those are copied, the `d` columns the move
/// uncovered are rasterised. A run whose strips moved by different distances
/// (where two strips' drawings meet) is rasterised whole, and so is a run a
/// strip changed in anywhere but its drawing; a run nothing moved in and
/// nothing changed in is left as it is.
///
/// A pan moves what the strips drew; what changed is drawn where it stands,
/// not where the move would have brought it. So a run that moved is copied
/// whole, and the tiles a change stands on are rasterised over the copy.
fn pan_work(
    pieces: &[Piece],
    in_place_drawn: bool,
    width: u32,
) -> (Vec<BandCopy>, Vec<FineDamageRect>) {
    let mut ys: Vec<u32> = pieces
        .iter()
        .flat_map(|piece| [piece.band.1, piece.band.3])
        .collect();
    ys.sort_unstable();
    ys.dedup();
    let mut copies: Vec<BandCopy> = Vec::new();
    let mut uncovered: Vec<FineDamageRect> = Vec::new();
    for (y0, y1) in ys.iter().copied().zip(ys.iter().copied().skip(1)) {
        if y1 <= y0 {
            continue;
        }
        let height = y1.saturating_sub(y0);
        for (x0, x1, cover) in runs(pieces, y0, y1, width) {
            let width = x1.saturating_sub(x0);
            let Some((first, rest)) = cover.split_first() else {
                continue;
            };
            let delta = first.delta;
            let uniform = rest.iter().all(|piece| piece.delta == delta);
            let anywhere = cover.iter().any(|piece| piece.change == Change::Anywhere);
            // What changed within the run, where the strips show it now.
            let within: Vec<FineDamageRect> = cover
                .iter()
                .filter_map(|piece| match &piece.change {
                    Change::Within(rects) => Some(rects),
                    Change::Unchanged | Change::Anywhere => None,
                })
                .flatten()
                .filter_map(|rect| clipped(rect, (x0, y0, x1, y1)))
                .collect();
            let shift = u32::try_from(delta.unsigned_abs()).unwrap_or(u32::MAX);
            if anywhere || !uniform || (delta != 0 && (in_place_drawn || shift >= width)) {
                push_rect(&mut uncovered, x0, y0, width, height);
                continue;
            }
            // A row the strips did not move in and did not change shows as it
            // shows: nothing to copy, nothing to draw.
            if delta == 0 && within.is_empty() {
                continue;
            }
            let kept = width.saturating_sub(shift);
            let (src_x, dst_x, gap_x) = if delta > 0 {
                (x0.saturating_add(shift), x0, x0.saturating_add(kept))
            } else {
                (x0, x0.saturating_add(shift), x0)
            };
            let kept = BandCopy {
                src_x,
                dst_x,
                y: y0,
                width: kept,
                height,
            };
            // What a strip shows here and where the pan puts it are the same
            // pixels, so the row is copied where it moves to; a change drawn
            // on top of them is not moved with them, and is drawn again.
            push_copy(&mut copies, kept);
            push_rect(&mut uncovered, gap_x, y0, shift, height);
            for rect in &within {
                push_rect(&mut uncovered, rect.x, rect.y, rect.width, rect.height);
            }
        }
    }
    (copies, uncovered)
}

/// The strips that show in the row `y0..y1`: every strip whose band holds
/// the row. Each strip between two others is drawn over the ones before it
/// ([`append_panned`]), so only the last of them shows; several strips over
/// the same row are told apart by the widths of their bands, which the
/// planner cuts into runs at. A change inside [`COLUMN_EDGE`] of an output's
/// own edge is left out: it was rounded onto a tile just past the group it
/// belongs to, and rasterising it costs that edge the cut the output makes
/// there anyway.
fn cover_lane(pieces: &[Piece], y0: u32, y1: u32, width: u32) -> Vec<Piece> {
    pieces
        .iter()
        .filter(|piece| piece.band.1 <= y0 && piece.band.3 >= y1 && piece.band.0 < piece.band.2)
        .map(|piece| Piece {
            change: match &piece.change {
                Change::Within(rects) => Change::Within(
                    rects
                        .iter()
                        .filter(|rect| {
                            rect.x >= COLUMN_EDGE && rect.x < width.saturating_sub(COLUMN_EDGE)
                        })
                        .copied()
                        .collect(),
                ),
                other => other.clone(),
            },
            ..piece.clone()
        })
        .collect()
}

/// The runs of the row `y0..y1` covered by the same strips, left to right,
/// with those strips: cut at every strip's edges, and at the columns an
/// output's edges are near, so a run there is decided apart from the run the
/// pan really moves. Pixels no strip covers are left out.
fn runs(pieces: &[Piece], y0: u32, y1: u32, width: u32) -> Vec<(u32, u32, Vec<Piece>)> {
    let in_row = cover_lane(pieces, y0, y1, width);
    let mut xs: Vec<u32> = in_row
        .iter()
        .flat_map(|piece| [piece.band.0, piece.band.2])
        .collect();
    xs.push(COLUMN_EDGE.min(width));
    xs.push(width.saturating_sub(COLUMN_EDGE));
    xs.sort_unstable();
    xs.dedup();
    let mut runs: Vec<(u32, u32, Vec<usize>)> = Vec::new();
    for (x0, x1) in xs.iter().copied().zip(xs.iter().copied().skip(1)) {
        let cover: Vec<usize> = in_row
            .iter()
            .enumerate()
            .filter(|(_, piece)| piece.band.0 <= x0 && piece.band.2 >= x1)
            .map(|(index, _)| index)
            .collect();
        match runs.last_mut() {
            Some(last) if last.1 == x0 && last.2 == cover => {
                last.1 = x1;
            }
            _ => runs.push((x0, x1, cover)),
        }
    }
    runs.into_iter()
        .map(|(x0, x1, cover)| {
            let pieces = cover
                .into_iter()
                .filter_map(|index| in_row.get(index).cloned())
                .collect();
            (x0, x1, pieces)
        })
        .collect()
}

/// Add a rect to rasterise, joining it to the one above when they line up.
fn push_rect(rects: &mut Vec<FineDamageRect>, x: u32, y: u32, width: u32, height: u32) {
    if width == 0 || height == 0 {
        return;
    }
    if let Some(above) = rects.iter_mut().rev().find(|above| {
        above.x == x && above.width == width && above.y.saturating_add(above.height) == y
    }) {
        above.height = above.height.saturating_add(height);
        return;
    }
    rects.push(FineDamageRect {
        x,
        y,
        width,
        height,
    });
}

/// Add a band copy, joining it to the one above when they line up.
fn push_copy(copies: &mut Vec<BandCopy>, copy: BandCopy) {
    if copy.width == 0 || copy.height == 0 {
        return;
    }
    if let Some(above) = copies.iter_mut().rev().find(|above| {
        above.src_x == copy.src_x
            && above.dst_x == copy.dst_x
            && above.width == copy.width
            && above.y.saturating_add(above.height) == copy.y
    }) {
        above.height = above.height.saturating_add(copy.height);
        return;
    }
    copies.push(copy);
}

/// One layer's texture: what vello rasterises into and the composite reads.
pub(crate) struct LayerTexture {
    pub(crate) texture: wgpu::Texture,
    pub(crate) view: wgpu::TextureView,
    pub(crate) size: (u32, u32),
}

impl LayerTexture {
    fn new(device: &wgpu::Device, size: (u32, u32), label: &'static str) -> Self {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
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
        Self {
            texture,
            view,
            size,
        }
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
    /// The composite has run once ([`Self::prime_composite`]).
    primed: bool,
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
            primed: false,
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

    /// Run the composite once, over one pixel of a throwaway target: a
    /// driver that compiles a pipeline on its first dispatch (lavapipe)
    /// then does so while the desktop rests, not in a pan's first frame.
    pub(crate) fn prime_composite(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if std::mem::replace(&mut self.primed, true) {
            return;
        }
        let target = LayerTexture::new(device, (1, 1), "tilo-strip-layer-prime");
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tilo-strip-layer-prime"),
        });
        self.encode_composite(
            CompositeTarget {
                device,
                queue,
                encoder: &mut encoder,
                output: &target.view,
            },
            FineDamageRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            },
        );
        queue.submit([encoder.finish()]);
    }

    /// Move the strips layer's bands `copies` names, through the scratch
    /// texture.
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

    pub(crate) fn encode_composite(&self, target: CompositeTarget<'_>, rect: FineDamageRect) {
        self.composite.encode(
            target,
            [&self.backdrop.view, &self.workspace.view, &self.chrome.view],
            rect,
        );
    }
}

/// Where a composite is encoded and what it writes.
pub(crate) struct CompositeTarget<'a> {
    pub(crate) device: &'a wgpu::Device,
    pub(crate) queue: &'a wgpu::Queue,
    pub(crate) encoder: &'a mut wgpu::CommandEncoder,
    pub(crate) output: &'a wgpu::TextureView,
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

/// Bytes of the composite shader's `Params` uniform: two `vec2<u32>`, a
/// whole 16-byte uniform block.
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
        target: CompositeTarget<'_>,
        [backdrop, workspace, chrome]: [&wgpu::TextureView; 3],
        rect: FineDamageRect,
    ) {
        let CompositeTarget {
            device,
            queue,
            encoder,
            output,
        } = target;
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let mut bytes = [0_u8; 16];
        let words = [
            rect.x.to_le_bytes(),
            rect.y.to_le_bytes(),
            rect.width.to_le_bytes(),
            rect.height.to_le_bytes(),
        ];
        for (slot, value) in bytes.chunks_exact_mut(4).zip(words) {
            slot.copy_from_slice(&value);
        }
        queue.write_buffer(&self.params, 0, &bytes);
        let texture = |binding, view| wgpu::BindGroupEntry {
            binding,
            resource: wgpu::BindingResource::TextureView(view),
        };
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-strip-layer-composite"),
            layout: &self.layout,
            entries: &[
                texture(0, backdrop),
                texture(1, workspace),
                texture(2, chrome),
                texture(3, output),
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

    fn cache(drawn: Color, strips: Vec<StripBlit>) -> FrameCache {
        let strips_drawn: Vec<Scene> = strips.iter().map(|_| scene(drawn)).collect();
        let strip_parts = strips_drawn.iter().map(|_| None).collect();
        FrameCache {
            width: 3000,
            strips,
            generation: 1,
            chrome_damage: None,
            layers: FrameLayers {
                backdrop: scene(Color::BLACK),
                workspace: Scene::new(),
                strips: strips_drawn,
                strip_parts,
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
    fn a_changed_strip_rasterises_its_destination_and_geometry_the_layer() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
        let content = plan(
            Some(&shown),
            &cache(Color::WHITE, vec![strip(52)]),
            SIZE,
            true,
        );
        assert_eq!(
            content.workspace,
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
        let mut taller = strip(52);
        taller.dest_h = 420;
        let geometry = plan(Some(&shown), &cache(Color::BLACK, vec![taller]), SIZE, true);
        assert_eq!(geometry.workspace, WorkspaceWork::Whole);
        assert_eq!(geometry.composite, Some(full_rect(SIZE)));
    }

    /// A strip drawn as two window groups, each a damage part: one near the
    /// canvas's left edge, one far right of the screen.
    fn grouped(near: Color, far: Color) -> (Scene, Option<ui::scene_parts::SectionParts>) {
        let mut drawn = Scene::new();
        let ((), parts) = ui::scene_parts::record(&mut drawn, |drawn| {
            for (index, (x, color)) in [(130.0, near), (2400.0, far)].into_iter().enumerate() {
                let bounds = Rect::new(x, 60.0, x + 300.0, 300.0);
                let key = ui::scene_parts::PartKey::new("test.group", index);
                ui::scene_parts::part(drawn, key, vec![bounds], |drawn| {
                    drawn.fill(Fill::NonZero, Affine::IDENTITY, color, None, &bounds);
                });
            }
        });
        (drawn, parts)
    }

    fn grouped_cache(near: Color, far: Color, scroll_x: u32) -> FrameCache {
        let (drawn, parts) = grouped(near, far);
        let mut frame = cache(Color::BLACK, vec![strip(scroll_x)]);
        frame.layers.strips = vec![drawn];
        frame.layers.strip_parts = vec![parts];
        frame
    }

    /// A window group off screen that changed while the strip rested (an app
    /// presenting its first content unseen) rasterises nothing; one on
    /// screen rasterises its own bounds, in whole tiles inside the strip's
    /// band, and nothing else of the strip. Panning while it changes copies
    /// the strip as any pan does and rasterises the uncovered columns and
    /// the group. A strip drawn without parts still rasterises its band.
    #[test]
    fn a_changed_window_group_rasterises_only_where_it_is_on_screen() {
        let shown = Shown::of(&grouped_cache(Color::WHITE, Color::WHITE, 40), 1);
        let far = plan(
            Some(&shown),
            &grouped_cache(Color::WHITE, Color::BLACK, 40),
            SIZE,
            true,
        );
        assert_eq!(far.workspace, WorkspaceWork::Unchanged);
        assert_eq!(far.composite, None);

        let near = plan(
            Some(&shown),
            &grouped_cache(Color::BLACK, Color::WHITE, 40),
            SIZE,
            true,
        );
        // The group changed without the strip moving: every row the band
        // holds is copied where it stands, and the group is drawn again on
        // the rows and columns it was drawn on.
        assert_eq!(
            near.workspace,
            WorkspaceWork::Pan {
                copies: vec![BandCopy {
                    src_x: 100,
                    dst_x: 100,
                    y: 50,
                    width: 800,
                    height: 400,
                }],
                uncovered: vec![FineDamageRect {
                    x: 100,
                    y: 50,
                    width: 300,
                    height: 254,
                }],
            }
        );

        let panned = plan(
            Some(&shown),
            &grouped_cache(Color::BLACK, Color::WHITE, 52),
            SIZE,
            true,
        );
        let WorkspaceWork::Pan { copies, uncovered } = &panned.workspace else {
            panic!("a pan with a changed group copies: {:?}", panned.workspace);
        };
        // The run past the changed group's own columns copies every row the
        // pan keeps: that change is drawn where it stands, and the rows a run
        // copies are the run's.
        assert_eq!(
            copies,
            &vec![BandCopy {
                src_x: 112,
                dst_x: 100,
                y: 50,
                width: 788,
                height: 400,
            }],
            "{uncovered:?}"
        );
        // Canvas 130..430 shown 52 px left is 78..378, in tiles 64..384.
        assert!(
            uncovered
                .iter()
                .any(|rect| rect.x == 100 && rect.width == 284),
            "{uncovered:?}"
        );

        let mut unparted = grouped_cache(Color::BLACK, Color::WHITE, 40);
        unparted.layers.strip_parts = vec![None];
        let whole = plan(Some(&shown), &unparted, SIZE, true);
        assert_eq!(
            whole.workspace,
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

    /// A group at the strip's left edge changed while the strip panned: the
    /// pan copies every row it keeps and draws the group again where it
    /// stands. A change nowhere on screen is not drawn at all: no row moved
    /// because of it, and nothing shows it.
    #[test]
    fn a_pan_copies_its_rows_and_draws_a_changed_group_where_it_stands() {
        // A group drawn at the canvas's left edge (110..410, shown at 10..310
        // with the strip 100 px right of the canvas), so the columns a pan
        // brings it to are the run a move of at most a strip's width leaves.
        let edge = |near: Color, far: Color| {
            let mut drawn = Scene::new();
            let ((), parts) = ui::scene_parts::record(&mut drawn, |drawn| {
                for (index, (x, color)) in [(10.0, near), (2400.0, far)].into_iter().enumerate() {
                    let bounds = Rect::new(x, 60.0, x + 300.0, 300.0);
                    let key = ui::scene_parts::PartKey::new("test.group", index);
                    ui::scene_parts::part(drawn, key, vec![bounds], |drawn| {
                        drawn.fill(Fill::NonZero, Affine::IDENTITY, color, None, &bounds);
                    });
                }
            });
            let mut frame = cache(Color::BLACK, vec![strip(0)]);
            frame.layers.strips = vec![drawn];
            frame.layers.strip_parts = vec![parts];
            frame
        };
        let shown = Shown::of(&edge(Color::WHITE, Color::WHITE), 1);
        // The strip moved 4 px left with the group at its edge changed: the
        // pan copies the rows it keeps and draws the group where it stands.
        let work = plan(Some(&shown), &edge(Color::BLACK, Color::WHITE), SIZE, true);
        let WorkspaceWork::Pan { copies, uncovered } = &work.workspace else {
            panic!("a pan: {work:?}");
        };
        assert_eq!(
            copies,
            &vec![BandCopy {
                src_x: 100,
                dst_x: 100,
                y: 50,
                width: 800,
                height: 400,
            }],
            "copies, with {uncovered:?}"
        );
        // The rows the change stands on are drawn again where it stands.
        assert_eq!(
            uncovered,
            &vec![FineDamageRect {
                x: 100,
                y: 50,
                width: 220,
                height: 254,
            }]
        );
        assert!(work.raster_pixels(SIZE) < 900 * 400);

        // A group the move does not reach changed with the strips beside it,
        // and nothing on screen shows that change: no row is copied and none
        // is drawn.
        let near = plan(Some(&shown), &edge(Color::WHITE, Color::BLACK), SIZE, true).workspace;
        assert_eq!(near, WorkspaceWork::Unchanged);
    }

    /// Two strips whose destinations meet (each draws its shadows past its
    /// row): the upper one pans. Its own rows are copied; the band both
    /// cover, where the lower strip's drawing stays put, is rasterised; the
    /// lower strip's own rows are left alone.
    #[test]
    fn where_a_panning_strip_meets_a_still_one_the_shared_band_is_rasterised() {
        let mut lower = strip(0);
        lower.dest_y = 430;
        lower.dest_h = 150;
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40), lower]), 1);
        let work = plan(
            Some(&shown),
            &cache(Color::BLACK, vec![strip(52), lower]),
            SIZE,
            true,
        );
        let WorkspaceWork::Pan { copies, uncovered } = &work.workspace else {
            panic!("a pan: {work:?}");
        };
        assert_eq!(
            copies,
            &vec![BandCopy {
                src_x: 112,
                dst_x: 100,
                y: 50,
                width: 788,
                height: 380,
            }]
        );
        assert_eq!(
            uncovered,
            &vec![
                FineDamageRect {
                    x: 888,
                    y: 50,
                    width: 12,
                    height: 380,
                },
                FineDamageRect {
                    x: 100,
                    y: 430,
                    width: 800,
                    height: 20,
                },
            ]
        );
        assert_eq!(work.raster_pixels(SIZE), 12 * 380 + 800 * 20);
        let both = plan(
            Some(&shown),
            &cache(
                Color::BLACK,
                vec![strip(52), {
                    let mut moved = lower;
                    moved.scroll_x = 12;
                    moved
                }],
            ),
            SIZE,
            true,
        );
        let WorkspaceWork::Pan { copies, .. } = &both.workspace else {
            panic!("a pan: {both:?}");
        };
        assert_eq!(
            copies.iter().map(|copy| copy.height).sum::<u32>(),
            530,
            "strips moving together are copied through the band they share: {copies:?}"
        );
    }

    #[test]
    fn what_no_strip_carries_drawn_in_place_keeps_strip_pixels_from_moving() {
        let shown_cache = {
            let mut cache = cache(Color::BLACK, vec![strip(40)]);
            cache.layers.workspace = scene(Color::WHITE);
            cache
        };
        let shown = Shown::of(&shown_cache, 1);
        let mut next = cache(Color::BLACK, vec![strip(52)]);
        next.layers.workspace = scene(Color::WHITE);
        let work = plan(Some(&shown), &next, SIZE, true);
        let WorkspaceWork::Pan { copies, uncovered } = work.workspace else {
            panic!("a pan: {work:?}");
        };
        assert!(copies.is_empty());
        assert_eq!(uncovered.len(), 1);
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

    /// Chrome that changed only in the tiles its damage names, from the
    /// drawing the chrome texture holds, rasterises and composites those
    /// tiles; measured from another drawing, it rasterises the whole layer.
    #[test]
    fn changed_chrome_from_the_drawing_held_rasterises_only_its_tiles() {
        let shown = Shown::of(&cache(Color::BLACK, vec![strip(40)]), 1);
        let tiles = vec![
            FineDamageRect {
                x: 16,
                y: 0,
                width: 48,
                height: 32,
            },
            FineDamageRect {
                x: 900,
                y: 16,
                width: 64,
                height: 16,
            },
        ];
        let mut next = cache(Color::BLACK, vec![strip(40)]);
        next.generation = 2;
        next.layers.chrome = scene(Color::BLACK);
        next.chrome_damage = Some(crate::shell::frame::ChromeDamage {
            since: 1,
            rects: tiles.clone(),
        });
        let work = plan(Some(&shown), &next, SIZE, true);
        assert!(work.chrome && !work.backdrop);
        assert_eq!(work.chrome_tiles, Some(tiles));
        assert_eq!(work.workspace, WorkspaceWork::Unchanged);
        assert_eq!(
            work.composite,
            Some(FineDamageRect {
                x: 16,
                y: 0,
                width: 948,
                height: 32,
            })
        );
        assert_eq!(work.raster_pixels(SIZE), 48 * 32 + 64 * 16);

        next.chrome_damage = Some(crate::shell::frame::ChromeDamage {
            since: 7,
            rects: vec![],
        });
        let other = plan(Some(&shown), &next, SIZE, true);
        assert_eq!(other.chrome_tiles, None);
        assert_eq!(other.composite, Some(full_rect(SIZE)));
    }

    /// A warm that stopped while drawing a layer leaves that texture holding
    /// nothing known: the next frame rasterises it and composites the whole
    /// output, and what the warm finished stays as it is.
    #[test]
    fn a_layer_a_warm_left_unknown_is_rasterised_and_the_output_composited_whole() {
        let frame = cache(Color::BLACK, vec![strip(40)]);
        let mut shown = Shown::of(&frame, 3);
        shown.forget(Layer::Chrome);
        let work = plan(Some(&shown), &frame, SIZE, true);
        assert!(work.chrome && !work.backdrop);
        assert_eq!(work.workspace, WorkspaceWork::Unchanged);
        assert_eq!(work.composite, Some(full_rect(SIZE)));
        shown.record(Layer::Chrome, &frame);
        let warm = plan(Some(&shown), &frame, SIZE, true);
        assert!(warm.rasterises_nothing());
        assert_eq!(warm.composite, None);

        let mut nothing = Shown::unknown();
        let work = plan(Some(&nothing), &frame, SIZE, true);
        assert_eq!(work, LayerWork::whole(SIZE));
        for layer in [Layer::Backdrop, Layer::Chrome, Layer::Workspace] {
            nothing.record(layer, &frame);
        }
        let warmed = plan(Some(&nothing), &frame, SIZE, false);
        assert!(warmed.rasterises_nothing());
        assert_eq!(
            warmed.composite,
            Some(full_rect(SIZE)),
            "layers warmed at rest were never composited into the output"
        );
    }

    #[test]
    fn warm_bands_cover_the_output_once_in_tile_rows() {
        assert_eq!(warm_bands((1024, 1024)), None);
        for size in [(1920, 1080), (2944, 1840), (3840, 2160), (7680, 4320)] {
            let bands = warm_bands(size).expect("a large output is warmed in bands");
            let mut y = 0_u32;
            for band in &bands {
                let end = band.y.saturating_add(band.height);
                assert_eq!((band.x, band.y, band.width), (0, y, size.0));
                assert!(band.height.is_multiple_of(WARM_BAND_ROWS) || end == size.1);
                assert!(
                    area(*band)
                        <= WARM_BAND_PIXELS.saturating_add(
                            u64::from(size.0).saturating_mul(u64::from(WARM_BAND_ROWS))
                        )
                );
                y = end;
            }
            assert_eq!(y, size.1, "{size:?}: the bands cover the output");
        }
    }

    #[test]
    fn a_composed_frame_draws_the_layers_in_order() {
        let frame = cache(Color::BLACK, vec![strip(40)]);
        let mut expected = frame.layers.backdrop.clone();
        append_panned(&mut expected, &frame.layers, &frame.strips);
        expected.append(&frame.layers.chrome, None);
        assert!(compose(&frame).encoding_eq(&expected));
    }
}
