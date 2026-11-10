//! Part painters: the host side of the theme SDK (`tilo_theme_sdk`,
//! `docs/architecture/theming.md`, "Part painters").
//!
//! A theme's painter set ([`PainterSet`], a [`PartPainter`]) draws parts;
//! this module is everything around it that the toolkit owns:
//!
//! * the sets a document can name: the built-in Default, Glass and Block
//!   ([`builtin`]) and those a theme installs ([`register_painter_set`],
//!   [`painter_set`]), its own loaded from wasm ([`wasm`]); the set drawing
//!   each surface role is the role's [`crate::tokens::Finish`]
//!   ([`Theme::painter`]);
//! * the values a part is drawn with ([`part_tokens`]);
//! * drawing a painted part into the frame, with its backdrop filled by the
//!   backdrop pass and held legible ([`paint_part`]), and the cache that
//!   keeps a settled part's drawing ([`draw`]);
//! * the clock its transitions run on ([`PartMotion`]);
//! * the painted shape that takes the pointer ([`hit_contains`]).
//!
//! A set paints what it chooses to; a part it returns `None` for is drawn by
//! the toolkit's own painters, so a theme draws only the parts it changes.

pub mod builtin;
mod draw;
mod motion;
pub(crate) mod shader;
pub mod wasm;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wasm_tests;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

pub use draw::{Painted, cached_parts, paint_part};
pub use motion::{Moment, PartMotion};
pub use shader::{SURFACE_SHADER, SurfaceShaderCode, builtin_surface_shader};
pub use tilo_theme_sdk::{
    Brush, Easing, Lens, Op, PaintedPart, Param, ParamSpec, Part, PartImage, PartPainter,
    PartScene, PartState, PartTokens, Sampling, ShaderSource, Sides, States, Stop, Transition,
};

use vello::kurbo::{BezPath, Point, Shape};
use vello::peniko::Color;

use crate::theme::Theme;
use crate::tokens::SurfaceRole;

/// A painter set the toolkit draws with: its name, as a theme document
/// names it, and the painter.
#[derive(Clone)]
pub struct PainterSet {
    name: Arc<str>,
    /// Unique per set made in this process, so a cache entry of one set is
    /// never taken for another's of the same name.
    id: u64,
    painter: Arc<dyn PartPainter>,
}

impl PainterSet {
    /// The set `name`, drawn by `painter`.
    pub fn new(name: impl Into<Arc<str>>, painter: Arc<dyn PartPainter>) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            name: name.into(),
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            painter,
        }
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn painter(&self) -> &dyn PartPainter {
        self.painter.as_ref()
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }
}

impl std::fmt::Debug for PainterSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("PainterSet").field(&self.name).finish()
    }
}

impl PartialEq for PainterSet {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

static REGISTRY: RwLock<Vec<PainterSet>> = RwLock::new(Vec::new());

/// Make `set` one a theme document can name. A set registered under a name
/// already taken replaces it: a theme's author installing a fixed version.
/// The built-in sets' names are the toolkit's: `false`, and nothing
/// registered, for a set that claims one.
pub fn register_painter_set(set: PainterSet) -> bool {
    if builtin::builtin(&set.name).is_some() {
        return false;
    }
    let mut sets = REGISTRY.write().unwrap_or_else(PoisonError::into_inner);
    sets.retain(|known| known.name != set.name);
    sets.push(set);
    true
}

/// The set a document calls `name`: a built-in one, or one registered.
#[must_use]
pub fn painter_set(name: &str) -> Option<PainterSet> {
    if let Some(set) = builtin::builtin(name) {
        return Some(set.clone());
    }
    REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .find(|set| &*set.name == name)
        .cloned()
}

/// The values a part of `role` is drawn with under `theme`: `fill`, `ink`
/// and `edge` as its role and state resolved them, `ground` what it stands
/// on, and the parameters the role's finish states. No backdrop: the
/// toolkit says where a part shows one.
#[must_use]
pub fn part_tokens(
    theme: &Theme,
    role: SurfaceRole,
    (fill, ink, edge): (Color, Color, Color),
    ground: Color,
) -> PartTokens {
    let colors = &theme.tokens.colors;
    let (border_width, border) = crate::surface::border(theme)
        .map_or((0.0, Color::TRANSPARENT), |(width, ink)| {
            (crate::num::f32_of_f64(width), ink)
        });
    let radius = match role {
        SurfaceRole::Control => theme.tokens.radii.control,
        SurfaceRole::Window | SurfaceRole::Floating | SurfaceRole::Bar => theme.tokens.radii.panel,
    };
    PartTokens {
        fill,
        ink,
        edge,
        ground,
        raised: colors.lift_on(ground, 1.0),
        accent: colors.intent.accent,
        light: colors.light,
        radius,
        border_width,
        border,
        scale: 1.0,
        dark: colors.palette.dark,
        high_contrast: colors.palette.high_contrast,
        reduce_transparency: theme.reduce_transparency || theme.forced_colors,
        backdrop: false,
        params: theme.finish(role).params().clone(),
        shader: crate::tokens::shader_source(theme.finish(role).shader()),
        flush: Sides::NONE,
    }
}

/// Whether `point` (in the coordinates `hit` is in) lies in the painted
/// shape `hit`, by the non-zero rule the shape was filled with.
#[must_use]
pub fn hit_contains(hit: &BezPath, point: Point) -> bool {
    hit.winding(point) != 0
}
