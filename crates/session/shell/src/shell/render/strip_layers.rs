//! Strip pans composited from retained layers.
//!
//! While a strip pans, the frame producer draws the desktop as layers
//! ([`FrameLayers`]): what is behind every window, each strip on its own in
//! canvas coordinates, the docked wheel on its own, and every piece of chrome
//! above them, all in scene (device) pixels. A pan then changes only where
//! each strip reads its canvas ([`StripBlit::scroll_x`]) and where the wheel
//! sits; the layers' scenes stay byte for byte the same.
//!
//! [`StripLayers`] keeps the backdrop, the strips and the chrome rasterised in
//! textures of the output's size, and the wheel in a texture of its own size.
//! The strips texture holds the strips as the screen shows them
//! ([`append_panned`]). On a pure pan, a pixel whose strips all moved by the
//! same distance is copied from where it was, and only the rest is
//! rasterised: the columns the pan uncovered, and the bands where a moving
//! strip's drawing meets a strip that moved otherwise (each strip draws its
//! shadows past its row, into its neighbour's). The output is then composited
//! from the four textures (source over, premultiplied, as vello blends), over
//! what changed. The backdrop, the chrome and the wheel are rasterised again
//! only when their scenes change; a moving wheel is only composited at its new
//! place.
//!
//! The composite stores straight alpha, as vello's fine stage does, so a
//! composited output matches a single render of the same frame within a
//! channel step of rounding.

use vello::kurbo::{Affine, Rect};
use vello::peniko::Fill;
use vello::{FineDamageRect, Scene, wgpu};

use crate::shell::frame::{FrameCache, FrameLayers, StripBlit, WheelSprite};

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

/// The wheel where it sits this frame, clipped to what its texture holds.
pub fn append_wheel(scene: &mut Scene, wheel: &WheelSprite) {
    let (x, y) = (f64::from(wheel.x), f64::from(wheel.y));
    scene.push_clip_layer(
        Fill::NonZero,
        Affine::IDENTITY,
        &Rect::new(
            x,
            y,
            x + f64::from(wheel.width),
            y + f64::from(wheel.height),
        ),
    );
    scene.append(&wheel.scene, Some(Affine::translate((x, y))));
    scene.pop_layer();
}

/// The frame `cache` holds as one scene: its layers over each other, as
/// [`StripLayers`] composites them.
pub fn compose(cache: &FrameCache) -> Scene {
    let mut scene = cache.layers.backdrop.clone();
    append_panned(&mut scene, &cache.layers, &cache.strips);
    if let Some(wheel) = &cache.layers.wheel {
        append_wheel(&mut scene, wheel);
    }
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
    chrome: Option<Scene>,
    wheel: Option<ShownWheel>,
    strips: Option<ShownStrips>,
    /// The frame whose composite the output target was last given; `None`
    /// when the output was not composited from what the textures hold.
    pub(crate) revision: Option<u64>,
}

/// What the wheel's texture shows: the wheel, or no wheel.
struct ShownWheel(Option<WheelSprite>);

/// What the strips texture shows.
struct ShownStrips {
    workspace: Scene,
    drawn: Vec<Scene>,
    width: u32,
    strips: Vec<StripBlit>,
}

impl ShownStrips {
    fn of(cache: &FrameCache) -> Self {
        Self {
            workspace: cache.layers.workspace.clone(),
            drawn: cache.layers.strips.clone(),
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
            chrome: Some(cache.layers.chrome.clone()),
            wheel: Some(ShownWheel(cache.layers.wheel.clone())),
            strips: Some(ShownStrips::of(cache)),
            revision: Some(revision),
        }
    }

    /// Textures holding nothing known.
    pub(crate) fn unknown() -> Self {
        Self {
            backdrop: None,
            chrome: None,
            wheel: None,
            strips: None,
            revision: None,
        }
    }

    /// Every texture holds a known layer.
    fn complete(&self) -> bool {
        self.backdrop.is_some()
            && self.chrome.is_some()
            && self.wheel.is_some()
            && self.strips.is_some()
    }

    /// `layer`'s texture is about to be drawn: until it is done it holds
    /// nothing known.
    pub(crate) fn forget(&mut self, layer: Layer) {
        match layer {
            Layer::Backdrop => self.backdrop = None,
            Layer::Workspace => self.strips = None,
            Layer::Wheel => self.wheel = None,
            Layer::Chrome => self.chrome = None,
        }
    }

    /// `layer`'s texture now holds `cache`'s.
    pub(crate) fn record(&mut self, layer: Layer, cache: &FrameCache) {
        match layer {
            Layer::Backdrop => self.backdrop = Some(cache.layers.backdrop.clone()),
            Layer::Workspace => self.strips = Some(ShownStrips::of(cache)),
            Layer::Wheel => self.wheel = Some(ShownWheel(cache.layers.wheel.clone())),
            Layer::Chrome => self.chrome = Some(cache.layers.chrome.clone()),
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
    pub(crate) workspace: WorkspaceWork,
    /// Rasterise the wheel's texture (of this many pixels) again.
    pub(crate) wheel: Option<u64>,
    /// The output rect to composite again; `None` when the output already
    /// shows the frame.
    pub(crate) composite: Option<FineDamageRect>,
}

impl LayerWork {
    /// Every layer and the whole output.
    fn whole(cache: &FrameCache, size: (u32, u32)) -> Self {
        Self {
            backdrop: true,
            chrome: true,
            workspace: WorkspaceWork::Whole,
            wheel: cache.layers.wheel.as_ref().map(wheel_area),
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
        whole
            .saturating_mul(layers)
            .saturating_add(workspace)
            .saturating_add(self.wheel.unwrap_or(0))
    }

    /// Pixels this frame composites.
    pub(crate) fn composite_pixels(&self) -> u64 {
        self.composite.map_or(0, area)
    }

    /// The layer textures already hold the frame: nothing to rasterise.
    pub(crate) fn rasterises_nothing(&self) -> bool {
        !self.backdrop
            && !self.chrome
            && self.wheel.is_none()
            && self.workspace == WorkspaceWork::Unchanged
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
/// while the layers warm waits for one band, not for a whole layer.
const WARM_BAND_PIXELS: u64 = 4 << 20;

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

fn wheel_area(wheel: &WheelSprite) -> u64 {
    u64::from(wheel.width).saturating_mul(u64::from(wheel.height))
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

/// The output pixels the wheel covers, clamped to an output of `size`;
/// `None` when it is off screen.
fn wheel_on_screen(wheel: &WheelSprite, size: (u32, u32)) -> Option<FineDamageRect> {
    let clamp = |value: i64, limit: u32| -> u32 {
        u32::try_from(value.clamp(0, i64::from(limit))).unwrap_or(limit)
    };
    let x0 = i64::from(wheel.x);
    let y0 = i64::from(wheel.y);
    let x1 = x0.saturating_add(i64::from(wheel.width));
    let y1 = y0.saturating_add(i64::from(wheel.height));
    let (x0, x1) = (clamp(x0, size.0), clamp(x1, size.0));
    let (y0, y1) = (clamp(y0, size.1), clamp(y1, size.1));
    (x1 > x0 && y1 > y0).then(|| FineDamageRect {
        x: x0,
        y: y0,
        width: x1.saturating_sub(x0),
        height: y1.saturating_sub(y0),
    })
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
        return LayerWork::whole(cache, size);
    };
    let differs =
        |layer: Option<&Scene>, scene: &Scene| layer.is_none_or(|layer| !layer.encoding_eq(scene));
    let backdrop = differs(shown.backdrop.as_ref(), &cache.layers.backdrop);
    let chrome = differs(shown.chrome.as_ref(), &cache.layers.chrome);
    let workspace = shown
        .strips
        .as_ref()
        .map_or(WorkspaceWork::Whole, |strips| {
            workspace_work(strips, cache, size)
        });
    let (wheel, wheel_rects) = match shown.wheel.as_ref() {
        Some(ShownWheel(old)) => wheel_work(old.as_ref(), cache.layers.wheel.as_ref(), size),
        None => (cache.layers.wheel.as_ref().map(wheel_area), Vec::new()),
    };
    // A texture that holds nothing known was never composited into the
    // output.
    let output_current = output_current && shown.complete();
    let mut composite = None;
    if backdrop || chrome || workspace == WorkspaceWork::Whole || !output_current {
        composite = Some(full_rect(size));
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
        for rect in moved.into_iter().chain(wheel_rects) {
            composite = Some(union(composite, rect));
        }
    }
    LayerWork {
        backdrop,
        chrome,
        workspace,
        wheel,
        composite,
    }
}

/// Whether the wheel's texture is rasterised again (its pixel count), and
/// the output rects it leaves and covers when it moved, appeared or went.
fn wheel_work(
    shown: Option<&WheelSprite>,
    wheel: Option<&WheelSprite>,
    size: (u32, u32),
) -> (Option<u64>, Vec<FineDamageRect>) {
    let same_pixels = |a: &WheelSprite, b: &WheelSprite| {
        a.width == b.width && a.height == b.height && a.scene.encoding_eq(&b.scene)
    };
    match (shown, wheel) {
        (None, None) => (None, Vec::new()),
        (Some(old), Some(new)) if same_pixels(old, new) && (old.x, old.y) == (new.x, new.y) => {
            (None, Vec::new())
        }
        (old, new) => {
            let raster = new
                .filter(|new| old.is_none_or(|old| !same_pixels(old, new)))
                .map(wheel_area);
            let rects = old
                .into_iter()
                .chain(new)
                .filter_map(|sprite| wheel_on_screen(sprite, size))
                .collect();
            (raster, rects)
        }
    }
}

/// One strip as the planner sees it: where it is shown, how far it moved
/// since the shown frame (positive: its content moved left), and whether its
/// drawing changed.
#[derive(Clone, Copy)]
struct Piece {
    band: (u32, u32, u32, u32),
    delta: i64,
    changed: bool,
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
        .map(|((old, new), (old_drawn, new_drawn))| Piece {
            band: band(new, size),
            delta: i64::from(new.scroll_x).saturating_sub(i64::from(old.scroll_x)),
            changed: !old_drawn.encoding_eq(new_drawn),
        })
        .collect();
    // What no strip carries is drawn in place under the strips: where it
    // draws, a strip's pixels cannot be moved without it.
    let in_place_drawn = !cache.layers.workspace.encoding().is_empty();
    let (copies, uncovered) = pan_work(&pieces, in_place_drawn);
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
/// (where two strips' drawings meet), or one with a changed strip, is
/// rasterised whole; a run nothing moved in is left as it is.
fn pan_work(pieces: &[Piece], in_place_drawn: bool) -> (Vec<BandCopy>, Vec<FineDamageRect>) {
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
        for (x0, x1, cover) in runs(pieces, y0, y1) {
            let width = x1.saturating_sub(x0);
            let Some((first, rest)) = cover.split_first() else {
                continue;
            };
            let delta = first.delta;
            let uniform = rest.iter().all(|piece| piece.delta == delta);
            let changed = cover.iter().any(|piece| piece.changed);
            if !changed && uniform && delta == 0 {
                continue;
            }
            let shift = u32::try_from(delta.unsigned_abs()).unwrap_or(u32::MAX);
            if changed || !uniform || in_place_drawn || shift >= width {
                push_rect(&mut uncovered, x0, y0, width, height);
                continue;
            }
            let kept = width.saturating_sub(shift);
            let (src_x, dst_x, gap_x) = if delta > 0 {
                (x0.saturating_add(shift), x0, x0.saturating_add(kept))
            } else {
                (x0, x0.saturating_add(shift), x0)
            };
            push_copy(
                &mut copies,
                BandCopy {
                    src_x,
                    dst_x,
                    y: y0,
                    width: kept,
                    height,
                },
            );
            push_rect(&mut uncovered, gap_x, y0, shift, height);
        }
    }
    (copies, uncovered)
}

/// The runs of the row `y0..y1` covered by the same strips, left to right,
/// with those strips; pixels no strip covers are left out.
fn runs(pieces: &[Piece], y0: u32, y1: u32) -> Vec<(u32, u32, Vec<Piece>)> {
    let in_row: Vec<Piece> = pieces
        .iter()
        .copied()
        .filter(|piece| piece.band.1 <= y0 && piece.band.3 >= y1 && piece.band.0 < piece.band.2)
        .collect();
    let mut xs: Vec<u32> = in_row
        .iter()
        .flat_map(|piece| [piece.band.0, piece.band.2])
        .collect();
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
            Some(last) if last.1 == x0 && last.2 == cover => last.1 = x1,
            _ => runs.push((x0, x1, cover)),
        }
    }
    runs.into_iter()
        .map(|(x0, x1, cover)| {
            let pieces = cover
                .into_iter()
                .filter_map(|index| in_row.get(index).copied())
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
    Wheel,
    Chrome,
}

/// The retained layers of one output, and the pass that composites them.
pub(crate) struct StripLayers {
    pub(crate) size: (u32, u32),
    backdrop: LayerTexture,
    workspace: LayerTexture,
    /// The docked wheel at its own size; one pixel, unused, without it.
    wheel: LayerTexture,
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
            wheel: LayerTexture::new(device, (1, 1), "tilo-strip-layer-wheel"),
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
            Layer::Wheel => &mut self.wheel,
            Layer::Chrome => &mut self.chrome,
        }
    }

    /// Give the wheel's texture `size`, keeping it when it already has it.
    pub(crate) fn size_wheel(&mut self, device: &wgpu::Device, size: (u32, u32)) {
        if self.wheel.size != size {
            self.wheel = LayerTexture::new(device, size, "tilo-strip-layer-wheel");
        }
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

    /// Composite `rect` of `output` from the layers, the wheel where `wheel`
    /// puts it.
    pub(crate) fn encode_composite(
        &self,
        target: CompositeTarget<'_>,
        wheel: Option<&WheelSprite>,
        rect: FineDamageRect,
    ) {
        let wheel = wheel.map_or(([0, 0], [0, 0]), |wheel| {
            ([wheel.x, wheel.y], [wheel.width, wheel.height])
        });
        self.composite.encode(
            target,
            [
                &self.backdrop.view,
                &self.workspace.view,
                &self.wheel.view,
                &self.chrome.view,
            ],
            wheel,
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
    wheel_origin: vec2<i32>,
    wheel_extent: vec2<u32>,
};

@group(0) @binding(0) var backdrop: texture_2d<f32>;
@group(0) @binding(1) var workspace: texture_2d<f32>;
@group(0) @binding(2) var wheel: texture_2d<f32>;
@group(0) @binding(3) var chrome: texture_2d<f32>;
@group(0) @binding(4) var output: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(5) var<uniform> params: Params;

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
    let q = p - params.wheel_origin;
    var rider = vec4<f32>(0.0);
    if (q.x >= 0 && q.y >= 0
        && u32(q.x) < params.wheel_extent.x && u32(q.y) < params.wheel_extent.y) {
        rider = premultiplied(textureLoad(wheel, q, 0));
    }
    let above = premultiplied(textureLoad(chrome, p, 0));
    let under = strips + below * (1.0 - strips.a);
    let middle = rider + under * (1.0 - rider.a);
    let out = above + middle * (1.0 - above.a);
    // Straight alpha, as vello's fine stage stores it.
    let a_inv = 1.0 / max(out.a, 1e-6);
    textureStore(output, p, vec4<f32>(out.rgb * a_inv, out.a));
}
";

/// Bytes of the composite shader's `Params` uniform.
const PARAMS_BYTES: u64 = 32;

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
                read(3),
                wgpu::BindGroupLayoutEntry {
                    binding: 4,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 5,
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
        [backdrop, workspace, wheel, chrome]: [&wgpu::TextureView; 4],
        ([wheel_x, wheel_y], [wheel_width, wheel_height]): ([i32; 2], [u32; 2]),
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
        let mut bytes = [0_u8; 32];
        let words = [
            rect.x.to_le_bytes(),
            rect.y.to_le_bytes(),
            rect.width.to_le_bytes(),
            rect.height.to_le_bytes(),
            wheel_x.to_le_bytes(),
            wheel_y.to_le_bytes(),
            wheel_width.to_le_bytes(),
            wheel_height.to_le_bytes(),
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
                texture(2, wheel),
                texture(3, chrome),
                texture(4, output),
                wgpu::BindGroupEntry {
                    binding: 5,
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
        let strips_drawn = strips.iter().map(|_| scene(drawn)).collect();
        FrameCache {
            width: 3000,
            strips,
            layers: FrameLayers {
                backdrop: scene(Color::BLACK),
                workspace: Scene::new(),
                strips: strips_drawn,
                wheel: None,
                chrome: scene(Color::WHITE),
            },
        }
    }

    fn wheel(x: i32, color: Color) -> WheelSprite {
        WheelSprite {
            scene: scene(color),
            width: 120,
            height: 500,
            x,
            y: 40,
        }
    }

    #[test]
    fn nothing_shown_renders_every_layer_and_the_whole_output() {
        let work = plan(None, &cache(Color::BLACK, vec![strip(40)]), SIZE, true);
        assert_eq!(work, LayerWork::whole(&cache(Color::BLACK, vec![]), SIZE));
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
        assert!(!work.backdrop && !work.chrome && work.wheel.is_none());
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
    fn a_wheel_that_moves_is_composited_without_being_rasterised() {
        let mut before = cache(Color::BLACK, vec![strip(40)]);
        before.layers.wheel = Some(wheel(30, Color::WHITE));
        let shown = Shown::of(&before, 1);
        let mut moved = cache(Color::BLACK, vec![strip(52)]);
        moved.layers.wheel = Some(wheel(18, Color::WHITE));
        let work = plan(Some(&shown), &moved, SIZE, true);
        assert_eq!(work.wheel, None, "the wheel's pixels are kept");
        assert_eq!(
            work.composite,
            Some(FineDamageRect {
                x: 18,
                y: 40,
                width: 882,
                height: 500,
            }),
            "the wheel's old and new places are composited with the strip"
        );
        let mut redrawn = cache(Color::BLACK, vec![strip(40)]);
        redrawn.layers.wheel = Some(wheel(30, Color::BLACK));
        let work = plan(Some(&shown), &redrawn, SIZE, true);
        assert_eq!(work.wheel, Some(120 * 500));
        let mut gone = cache(Color::BLACK, vec![strip(40)]);
        gone.layers.wheel = Some(wheel(-200, Color::WHITE));
        let work = plan(Some(&shown), &gone, SIZE, true);
        assert_eq!(work.wheel, None);
        assert_eq!(
            work.composite,
            Some(FineDamageRect {
                x: 30,
                y: 40,
                width: 120,
                height: 500,
            }),
            "a wheel panned off screen leaves its old place"
        );
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
        assert_eq!(work, LayerWork::whole(&frame, SIZE));
        for layer in [
            Layer::Backdrop,
            Layer::Chrome,
            Layer::Wheel,
            Layer::Workspace,
        ] {
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

    /// A whole layer is warmed in bands of at most about four million
    /// pixels, a whole number of tile rows each, that cover the output once.
    #[test]
    fn warm_bands_cover_the_output_once_in_tile_rows() {
        assert_eq!(warm_bands((1920, 1080)), None);
        for size in [(3840, 2160), (5120, 2880), (7680, 4320)] {
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
        let mut frame = cache(Color::BLACK, vec![strip(40)]);
        frame.layers.wheel = Some(wheel(30, Color::WHITE));
        let mut expected = frame.layers.backdrop.clone();
        append_panned(&mut expected, &frame.layers, &frame.strips);
        if let Some(wheel) = &frame.layers.wheel {
            append_wheel(&mut expected, wheel);
        }
        expected.append(&frame.layers.chrome, None);
        assert!(compose(&frame).encoding_eq(&expected));
    }
}
