//! A frame drawn as retained layers, and strip pans composited from them.
//!
//! A frame producer whose content holds horizontally panned strips (a wide
//! canvas seen through a viewport) can draw the frame as layers
//! ([`FrameLayers`]): what is behind every strip, each strip on its own in
//! canvas coordinates, and everything above them, all in scene (device)
//! pixels. A pan then changes only where each strip reads its canvas
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

use crate::damage::{area, damage_union};

mod present;
pub use present::{LayerPresenter, LayerRaster, QueuedRaster};

/// A strip drawn in canvas coordinates. The presenter shows the strips
/// layer `scroll_x` pixels to the right of `dest_*`, in scene pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StripBlit {
    pub dest_x: u32,
    pub dest_y: u32,
    pub dest_w: u32,
    pub dest_h: u32,
    pub scroll_x: u32,
}

/// A frame as layers a presenter retains between frames.
#[derive(Clone)]
pub struct FrameCache {
    /// Width of the widest strip's canvas in scene pixels: wider than the
    /// screen when a strip's content is wider than its viewport.
    pub width: u32,
    /// Where each strip of [`FrameLayers::strips`] is shown, and how far it
    /// is panned, in the same order.
    pub strips: Vec<StripBlit>,
    pub layers: FrameLayers,
    /// Tells this drawing as layers apart from every other one its producer
    /// made, so a presenter knows which drawing each of its layer textures
    /// holds.
    pub generation: u64,
    /// Where [`FrameLayers::chrome`] differs from an earlier drawing's
    /// chrome; `None` when that is not known.
    pub chrome_damage: Option<ChromeDamage>,
}

/// Where the chrome of a drawing as layers differs from the chrome of the
/// drawing `since` ([`FrameCache::generation`]), in scene pixels and whole
/// 16-pixel tiles: a presenter whose chrome texture still holds that
/// drawing's chrome brings it up to date by rasterising only these rects,
/// so a clock tick rasterises a few tiles, not the whole layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChromeDamage {
    pub since: u64,
    pub rects: Vec<FineDamageRect>,
}

/// A frame's sections as layers, bottom to top, in scene pixels. Drawing
/// them over each other in order (source over) gives the frame's scene
/// ([`compose`]), so a presenter can rasterize each into its own retained
/// buffer and composite the buffers: a strip's pan then moves that strip's
/// pixels over a backdrop and chrome that stay put, without drawing any of
/// them again.
#[derive(Clone)]
pub struct FrameLayers {
    /// Anything drawn behind every strip.
    pub backdrop: Scene,
    /// What no strip carries, in place, below the strips.
    pub workspace: Scene,
    /// Each strip on screen, top to bottom, drawn on its own in canvas
    /// coordinates: shown inside its [`FrameCache::strips`] destination,
    /// read at its pan. Strips whose drawings meet (each draws its shadows
    /// past its row) each draw over the other there, as on screen.
    pub strips: Vec<Scene>,
    /// The damage parts each of `strips` marked, in the same order, so a
    /// presenter rasterises only where a part's drawing changed. `None` for
    /// a strip drawn without well-nested parts.
    pub strip_parts: Vec<Option<crate::scene_parts::SectionParts>>,
    /// Everything above the strips.
    pub chrome: Scene,
}

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
/// (a presenter warming its textures between pans). A
/// layer is `None` while its texture holds nothing known: a warm stopped
/// while drawing it.
pub struct Shown {
    backdrop: Option<Scene>,
    /// The chrome, and the [`FrameCache::generation`] of the drawing it is
    /// of.
    chrome: Option<(Scene, u64)>,
    strips: Option<ShownStrips>,
    /// The frame whose composite the output target was last given; `None`
    /// when the output was not composited from what the textures hold.
    pub revision: Option<u64>,
}

/// What the strips texture shows.
struct ShownStrips {
    workspace: Scene,
    drawn: Vec<Scene>,
    /// The damage parts of `drawn`, in the same order.
    parts: Vec<Option<crate::scene_parts::SectionParts>>,
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
    pub fn of(cache: &FrameCache, revision: u64) -> Self {
        Self {
            backdrop: Some(cache.layers.backdrop.clone()),
            chrome: Some((cache.layers.chrome.clone(), cache.generation)),
            strips: Some(ShownStrips::of(cache)),
            revision: Some(revision),
        }
    }

    /// Textures holding nothing known.
    pub fn unknown() -> Self {
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
    pub fn chrome_generation(&self) -> Option<u64> {
        self.chrome.as_ref().map(|(_, generation)| *generation)
    }

    /// `layer`'s texture is about to be drawn: until it is done it holds
    /// nothing known.
    pub fn forget(&mut self, layer: Layer) {
        match layer {
            Layer::Backdrop => self.backdrop = None,
            Layer::Workspace => self.strips = None,
            Layer::Chrome => self.chrome = None,
        }
    }

    /// `layer`'s texture now holds `cache`'s.
    pub fn record(&mut self, layer: Layer, cache: &FrameCache) {
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
pub struct BandCopy {
    pub src_x: u32,
    pub dst_x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl BandCopy {
    /// The copy as a scroll within a retained target makes it.
    #[must_use]
    pub fn device_copy(self) -> crate::scroll_copy::DeviceCopy {
        let dx = i64::from(self.dst_x).saturating_sub(i64::from(self.src_x));
        crate::scroll_copy::DeviceCopy {
            x: self.src_x,
            y: self.y,
            width: self.width,
            height: self.height,
            dx: i32::try_from(dx).unwrap_or(i32::MAX),
            dy: 0,
        }
    }
}

/// What the strips layer needs for a frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceWork {
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
pub struct LayerWork {
    pub backdrop: bool,
    pub chrome: bool,
    /// Where the chrome is rasterised again when it is: only these tiles
    /// ([`chrome_damage`]), or the whole layer with `None`.
    pub chrome_tiles: Option<Vec<FineDamageRect>>,
    pub workspace: WorkspaceWork,
    /// The output rect to composite again; `None` when the output already
    /// shows the frame.
    pub composite: Option<FineDamageRect>,
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
    pub fn raster_pixels(&self, size: (u32, u32)) -> u64 {
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
    pub fn composite_pixels(&self) -> u64 {
        self.composite.map_or(0, area)
    }

    /// The layer textures already hold the frame: nothing to rasterise.
    pub fn rasterises_nothing(&self) -> bool {
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
pub fn warm_bands(size: (u32, u32)) -> Option<Vec<FineDamageRect>> {
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
pub fn chrome_damage(
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
pub fn plan(
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
        moved.into_iter().chain(tiles).reduce(damage_union)
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
    (old_drawn, old_parts): (&Scene, Option<&crate::scene_parts::SectionParts>),
    (new_drawn, new_parts): (&Scene, Option<&crate::scene_parts::SectionParts>),
    blit: &StripBlit,
    size: (u32, u32),
) -> Change {
    if old_drawn.encoding_eq(new_drawn) {
        return Change::Unchanged;
    }
    let (Some(old_parts), Some(new_parts)) = (old_parts, new_parts) else {
        return Change::Anywhere;
    };
    let Ok(rects) = crate::scene_parts::damage(old_drawn, old_parts, new_drawn, new_parts) else {
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
        crate::num::u32_of_f64(value.clamp(f64::from(low), f64::from(high)))
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
pub struct LayerTexture {
    pub texture: wgpu::Texture,
    pub view: wgpu::TextureView,
    pub size: (u32, u32),
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
pub enum Layer {
    Backdrop,
    Workspace,
    Chrome,
}

/// The retained layers of one output, and the pass that composites them.
pub struct StripLayers {
    pub size: (u32, u32),
    backdrop: LayerTexture,
    workspace: LayerTexture,
    chrome: LayerTexture,
    /// Where a moving band is staged: a texture cannot be copied onto itself.
    scratch: crate::scroll_copy::CopyScratch,
    composite: CompositePass,
    /// The composite has run once ([`Self::prime_composite`]).
    primed: bool,
    pub shown: Option<Shown>,
}

impl StripLayers {
    pub fn new(device: &wgpu::Device, size: (u32, u32)) -> Self {
        Self {
            size,
            backdrop: LayerTexture::new(device, size, "tilo-strip-layer-backdrop"),
            workspace: LayerTexture::new(device, size, "tilo-strip-layer-workspace"),
            chrome: LayerTexture::new(device, size, "tilo-strip-layer-chrome"),
            scratch: crate::scroll_copy::CopyScratch::default(),
            composite: CompositePass::new(device),
            primed: false,
            shown: None,
        }
    }

    pub fn texture_mut(&mut self, layer: Layer) -> &mut LayerTexture {
        match layer {
            Layer::Backdrop => &mut self.backdrop,
            Layer::Workspace => &mut self.workspace,
            Layer::Chrome => &mut self.chrome,
        }
    }

    /// Run the composite once, over one pixel of a throwaway target: a
    /// driver that compiles a pipeline on its first dispatch (lavapipe)
    /// then does so while the desktop rests, not in a pan's first frame.
    pub fn prime_composite(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
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

    /// Move the strips layer's bands `copies` names, as a scroll within a
    /// retained target moves ([`crate::scroll_copy::encode_copies`]).
    /// Returns `false`, recording nothing, when a copy would leave the
    /// layer: the caller then rasterises the layer whole.
    #[must_use]
    pub fn encode_copies(
        &mut self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        copies: &[BandCopy],
    ) -> bool {
        let copies: Vec<crate::scroll_copy::DeviceCopy> =
            copies.iter().map(|copy| copy.device_copy()).collect();
        crate::scroll_copy::encode_copies(
            device,
            encoder,
            &self.workspace.texture,
            &mut self.scratch,
            &copies,
        )
    }

    pub fn encode_composite(&self, target: CompositeTarget<'_>, rect: FineDamageRect) {
        self.composite.encode(
            target,
            [&self.backdrop.view, &self.workspace.view, &self.chrome.view],
            rect,
        );
    }
}

/// Where a composite is encoded and what it writes.
pub struct CompositeTarget<'a> {
    pub device: &'a wgpu::Device,
    pub queue: &'a wgpu::Queue,
    pub encoder: &'a mut wgpu::CommandEncoder,
    pub output: &'a wgpu::TextureView,
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
mod tests;
