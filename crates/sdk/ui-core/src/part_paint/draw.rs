//! Drawing a painted part into the frame, and the cache of settled parts.

use std::cell::RefCell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

use vello::Scene;
use vello::kurbo::{Affine, BezPath, Rect, Stroke};
use vello::peniko::{
    BlendMode, Brush as VelloBrush, ColorStop, Extend, Fill, Gradient, ImageBrush, ImageData,
    ImageQuality,
};

use super::{Brush, Moment, Op, PainterSet, Part, PartScene, PartTokens};
use crate::theme::Theme;
use crate::tokens::SurfaceRole;

/// A part as the toolkit drew it.
#[derive(Clone, Debug, PartialEq)]
pub struct Painted {
    /// Where it takes the pointer, in the coordinates its rect was given
    /// in; `None`: its rect.
    pub hit: Option<BezPath>,
}

/// Settled parts kept per thread. A window's parts at rest are a few dozen
/// distinct sizes and states; past this the least recently drawn go.
const CACHE_ENTRIES: usize = 256;

/// What identifies a settled part's drawing: the set, the part, its size in
/// device pixels, its states and the tokens it was drawn with.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key {
    set: u64,
    part: Part,
    width: u64,
    height: u64,
    scale: u32,
    states: u16,
    tokens: u64,
}

/// A settled part's drawing, at the origin; `None` where its set left it to
/// the toolkit.
struct Entry {
    drawn: Option<(Scene, Option<BezPath>)>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<Key, (Rc<Entry>, u64)>,
    tick: u64,
    /// Pattern tiles by content, so a part redrawn while it moves uploads
    /// its tile once.
    tiles: HashMap<u64, ImageData>,
}

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// How many settled parts this thread holds drawn (for tests and the
/// performance overlay).
#[must_use]
pub fn cached_parts() -> usize {
    CACHE.with(|cache| cache.borrow().entries.len())
}

/// Draw `part` of `set` in `rect` (in the coordinates `transform` maps into
/// `scene`'s), at `moment` of its motion, with `tokens`. `None` when the set
/// leaves the part to the toolkit's own painters, which the caller then
/// draws.
///
/// A settled part is drawn once per size, state and tokens and replayed
/// from then on; a moving one is drawn every frame; one that shows the
/// backdrop is drawn every frame into `scene` itself, which the backdrop
/// pass records it against. Only floating chrome and bars show the
/// backdrop, as [`crate::backdrop::fill_role`] says; elsewhere a backdrop
/// brush is its fallback.
pub fn paint_part(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    (set, part, role): (&PainterSet, Part, SurfaceRole),
    moment: Moment,
    tokens: &PartTokens,
    theme: &Theme,
) -> Option<Painted> {
    let scale = transform.determinant().abs().sqrt();
    let tokens = PartTokens {
        scale: crate::num::f32_of_f64(scale),
        ..tokens.clone()
    };
    let placed = transform * Affine::translate(rect.origin().to_vec2());
    let to_caller = Affine::translate(rect.origin().to_vec2());
    let key = moment.settled.then(|| Key {
        set: set.id(),
        part,
        width: (rect.width() * scale).round().to_bits(),
        height: (rect.height() * scale).round().to_bits(),
        scale: tokens.scale.to_bits(),
        states: moment.state.now.bits(),
        tokens: tokens_hash(&tokens),
    });
    if let Some(entry) = key.and_then(cached) {
        let (drawn, hit) = entry.drawn.as_ref()?;
        scene.append(drawn, Some(placed));
        return Some(Painted {
            hit: hit.as_ref().map(|hit| to_caller * hit.clone()),
        });
    }
    let painted = set
        .painter()
        .paint(part, rect.size(), moment.state, moment.progress, &tokens);
    let Some(painted) = painted else {
        if let Some(key) = key {
            keep(key, Entry { drawn: None });
        }
        return None;
    };
    let hit = painted.hit.as_ref().map(|hit| to_caller * hit.clone());
    let sampled = matches!(role, SurfaceRole::Floating | SurfaceRole::Bar);
    if sampled && painted.scene.samples_backdrop() {
        encode(scene, placed, (&painted.scene, true), theme, &tokens);
        return Some(Painted { hit });
    }
    let mut drawn = Scene::new();
    encode(
        &mut drawn,
        Affine::IDENTITY,
        (&painted.scene, false),
        theme,
        &tokens,
    );
    scene.append(&drawn, Some(placed));
    if let Some(key) = key {
        keep(
            key,
            Entry {
                drawn: Some((drawn, painted.hit)),
            },
        );
    }
    Some(Painted { hit })
}

fn cached(key: Key) -> Option<Rc<Entry>> {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.tick += 1;
        let tick = cache.tick;
        let (entry, used) = cache.entries.get_mut(&key)?;
        *used = tick;
        Some(Rc::clone(entry))
    })
}

fn keep(key: Key, entry: Entry) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.tick += 1;
        let tick = cache.tick;
        if cache.entries.len() >= CACHE_ENTRIES
            && let Some(oldest) = cache
                .entries
                .iter()
                .min_by_key(|(_, (_, used))| *used)
                .map(|(key, _)| *key)
        {
            cache.entries.remove(&oldest);
        }
        cache.entries.insert(key, (Rc::new(entry), tick));
    });
}

fn tokens_hash(tokens: &PartTokens) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for color in [
        tokens.fill,
        tokens.ink,
        tokens.edge,
        tokens.ground,
        tokens.raised,
        tokens.accent,
        tokens.border,
    ] {
        for channel in color.components {
            channel.to_bits().hash(&mut hasher);
        }
    }
    for value in [tokens.radius, tokens.border_width, tokens.scale] {
        value.to_bits().hash(&mut hasher);
    }
    (
        tokens.dark,
        tokens.high_contrast,
        tokens.reduce_transparency,
    )
        .hash(&mut hasher);
    for param in &tokens.params {
        param.name.hash(&mut hasher);
        param.value.to_bits().hash(&mut hasher);
    }
    hasher.finish()
}

/// Encode `part` into `out`, in the coordinates `transform` maps into its.
/// `sampled`: whether a backdrop brush shows the backdrop or its fallback.
fn encode(
    out: &mut Scene,
    transform: Affine,
    (part, sampled): (&PartScene, bool),
    theme: &Theme,
    tokens: &PartTokens,
) {
    let mut open = 0_usize;
    for op in &part.ops {
        match op {
            Op::Fill {
                path,
                brush:
                    Brush::Backdrop {
                        blur,
                        lens,
                        fallback,
                    },
                ..
            } if sampled => crate::backdrop::fill_painted_backdrop(
                out,
                transform,
                path,
                (*blur, lens.refraction, f64::from(tokens.radius)),
                theme,
                (tokens.fill, *fallback),
            ),
            Op::Fill {
                path,
                brush,
                brush_transform,
            } => {
                let (brush, placed) = vello_brush(brush);
                out.fill(
                    Fill::NonZero,
                    transform,
                    &brush,
                    Some(*brush_transform * placed),
                    path,
                );
            }
            Op::Stroke { path, width, brush } => {
                let (brush, placed) = vello_brush(brush);
                out.stroke(
                    &Stroke::new(f64::from(*width)),
                    transform,
                    &brush,
                    Some(placed),
                    path,
                );
            }
            Op::PushLayer { clip, alpha } => {
                open += 1;
                out.push_layer(
                    Fill::NonZero,
                    BlendMode::default(),
                    alpha.clamp(0.0, 1.0),
                    transform,
                    clip,
                );
            }
            Op::PopLayer => {
                // A pop with nothing open is the painter's mistake; it must
                // not close a layer of the frame around the part.
                if open > 0 {
                    open -= 1;
                    out.pop_layer();
                }
            }
        }
    }
    for _ in 0..open {
        out.pop_layer();
    }
}

/// `brush` as vello draws it, and the transform that places the brush's
/// own space (a pattern's texels) in the part's.
fn vello_brush(brush: &Brush) -> (VelloBrush, Affine) {
    let stops = |stops: &[super::Stop]| -> Vec<ColorStop> {
        stops
            .iter()
            .map(|stop| ColorStop {
                offset: stop.offset,
                color: stop.color.into(),
            })
            .collect()
    };
    match brush {
        Brush::Solid(color) => (VelloBrush::Solid(*color), Affine::IDENTITY),
        Brush::Linear {
            start,
            end,
            stops: s,
        } => (
            Gradient::new_linear(*start, *end)
                .with_stops(&stops(s)[..])
                .into(),
            Affine::IDENTITY,
        ),
        Brush::Radial {
            center,
            radius,
            stops: s,
        } => (
            Gradient::new_radial(*center, *radius)
                .with_stops(&stops(s)[..])
                .into(),
            Affine::IDENTITY,
        ),
        Brush::Pattern { image, scale } => {
            if !image.is_well_formed() {
                return (
                    VelloBrush::Solid(vello::peniko::Color::TRANSPARENT),
                    Affine::IDENTITY,
                );
            }
            let tile = tile(image);
            (
                VelloBrush::Image(
                    ImageBrush::new(tile)
                        .with_extend(Extend::Repeat)
                        .with_quality(ImageQuality::Low),
                ),
                Affine::scale(f64::from(scale.max(f32::MIN_POSITIVE))),
            )
        }
        Brush::Backdrop { fallback, .. } => (VelloBrush::Solid(*fallback), Affine::IDENTITY),
    }
}

/// The image of a pattern tile, made once per content on this thread.
fn tile(image: &super::PartImage) -> ImageData {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (image.width, image.height, &image.rgba).hash(&mut hasher);
    let id = hasher.finish();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.tiles.len() > CACHE_ENTRIES {
            cache.tiles.clear();
        }
        cache
            .tiles
            .entry(id)
            .or_insert_with(|| crate::raster::rgba8(image.rgba.clone(), image.width, image.height))
            .clone()
    })
}
