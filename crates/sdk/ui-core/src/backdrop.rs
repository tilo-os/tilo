//! The blurred material floating chrome is made of. Floating chrome — the palette, menus, popovers,
//! quick settings, sheets, notifications and chrome over media — is drawn over a blurred copy of what
//! is behind it, tinted with the panel ground (`docs/design-language.md`, "Material and transparency").
#![allow(
    clippy::arithmetic_side_effects,
    reason = "backdrop geometry is bounded floating-point pixel math"
)]

use std::cell::RefCell;

use vello::kurbo::{Affine, Rect, RoundedRect, Shape, Vec2};
use vello::peniko::{Color, Extend, Fill, ImageBrush, ImageData, ImageQuality};
use vello::{AaConfig, RenderParams, Renderer, Scene, wgpu};

use crate::floating_surface::Material;
use crate::part_paint::Lens;
pub use crate::part_paint::Sampling;
use crate::theme::Theme;
use crate::tokens::{Finish, SurfaceRole};

/// What a backdrop brush asks of the pass: how blurred, through what lens, at what corner radius.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    /// Blur standard deviation, logical pixels; `0` shows what is below
    /// straight.
    pub blur: f32,
    /// The lens: refraction at the rim, and whether the sample is shown as cells.
    pub lens: Lens,
    /// The part's corner radius, whose outline the lens bends along.
    pub radius: f64,
}

/// How a region's backdrop is bent, in the pixels of the space it is recorded or drawn in: a convex
/// lens along the rim of a rounded rect.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Refraction {
    /// Displacement at the outline.
    pub strength: f64,
    /// Width of the band along the outline the displacement eases across.
    pub band: f64,
    /// Corner radius of the rounded rect the region is.
    pub radius: f64,
    /// Red and blue set apart by this fraction of the displacement (blue
    /// bent further, as glass bends it); `0` none.
    pub dispersion: f64,
}

impl Refraction {
    /// No bend: the backdrop shows straight.
    pub const NONE: Self = Self {
        strength: 0.0,
        band: 0.0,
        radius: 0.0,
        dispersion: 0.0,
    };

    /// The bend `lens` asks for over a body of `radius`, in its logical pixels.
    #[must_use]
    pub fn of(lens: Lens, radius: f64) -> Self {
        let strength = f64::from(lens.refraction);
        let band = if lens.band > 0.0 {
            f64::from(lens.band)
        } else {
            strength * 2.5
        };
        Self {
            strength,
            band,
            radius: radius.max(0.0),
            dispersion: f64::from(lens.dispersion),
        }
    }

    /// Whether anything is bent.
    #[must_use]
    pub fn bends(&self) -> bool {
        self.strength.is_finite() && self.band.is_finite() && self.strength > 0.0 && self.band > 0.0
    }

    /// Whether the lens asks for what lies below even unblurred: it bends,
    /// or it states a band, as a Liquid lens at no strength does (clear
    /// glass, never its opaque fallback). A lens stating neither leaves an
    /// unblurred surface its fallback, as before lenses had bands.
    #[must_use]
    pub fn shows_through(&self) -> bool {
        self.bends() || (self.band.is_finite() && self.band > 0.0)
    }

    /// This bend in a space `scale` pixels per pixel of it.
    #[must_use]
    pub fn scaled(self, scale: f64) -> Self {
        Self {
            strength: self.strength * scale,
            band: self.band * scale,
            radius: self.radius * scale,
            dispersion: self.dispersion,
        }
    }

    /// Where the backdrop a pixel at `point` of a region of `size` shows
    /// lies, relative to it: toward the region's inside along its rounded
    /// outline's normal, by `strength` at the outline, easing to nothing
    /// `band` in (the band held to half the region's shorter side, so the
    /// two sides of a bar never cross). The backdrop pass's `bend` computes
    /// the same.
    #[must_use]
    pub fn displacement(&self, point: vello::kurbo::Point, size: vello::kurbo::Size) -> Vec2 {
        if !self.bends() || size.width <= 0.0 || size.height <= 0.0 {
            return Vec2::ZERO;
        }
        let half = Vec2::new(size.width / 2.0, size.height / 2.0);
        let shorter = half.x.min(half.y);
        let radius = self.radius.clamp(0.0, shorter);
        let p = point.to_vec2() - half;
        let sign = |v: f64| if v < 0.0 { -1.0 } else { 1.0 };
        let q = Vec2::new(p.x.abs() - (half.x - radius), p.y.abs() - (half.y - radius));
        let outside = Vec2::new(q.x.max(0.0), q.y.max(0.0));
        let distance = outside.hypot() + q.x.max(q.y).min(0.0) - radius;
        let normal = if q.x > 0.0 && q.y > 0.0 {
            let n = outside / outside.hypot();
            Vec2::new(n.x * sign(p.x), n.y * sign(p.y))
        } else if q.x > q.y {
            Vec2::new(sign(p.x), 0.0)
        } else {
            Vec2::new(0.0, sign(p.y))
        };
        let band = self.band.min(shorter);
        let t = (1.0 + distance / band).clamp(0.0, 1.0);
        -normal * (self.strength.min(shorter) * t * t)
    }
}

/// The luminance range what shows through a translucent surface is
/// compressed into, so ink chosen against the surface's ground stays
/// legible over anything below it: the material's band
/// ([`material_band`]) as relative luminances. The backdrop pass maps what
/// it sampled into it ([`Self::tone`] is the same arithmetic): luminance
/// is compressed evenly in contrast (a contrast ratio below becomes a
/// smaller, fixed power of itself), and each colour keeps its hue: dimmed,
/// its channels scale with its luminance; lifted, it keeps how far its
/// channels stand off its grey, so a near-grey stays near-grey. A bright
/// photo shows dimmed but in its colours and shapes, a dark page lifted
/// without a cast, never as one flat colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Legible {
    /// Relative luminance black maps to.
    pub dark: f64,
    /// Relative luminance white maps to.
    pub light: f64,
}

impl Default for Legible {
    fn default() -> Self {
        Self::ANY
    }
}

impl Legible {
    /// No compression: what lies below shows as it is.
    pub const ANY: Self = Self {
        dark: 0.0,
        light: 1.0,
    };

    /// The range a surface of `ground` under `theme` has what shows through it in.
    #[must_use]
    pub fn of(theme: &Theme, ground: Color) -> Self {
        let (dark, light) = material_band(theme, ground);
        let (dark, light) = (
            crate::color::relative_luminance(dark),
            crate::color::relative_luminance(light),
        );
        Self {
            dark: dark.min(light),
            light: dark.max(light),
        }
    }

    /// Whether anything is compressed.
    #[must_use]
    pub fn compresses(&self) -> bool {
        self.dark > 0.0 || self.light < 1.0
    }

    /// `rgb` (sRGB-encoded, 0–1) as it shows through: its relative
    /// luminance `y` mapped so `(y + 0.05) / 0.05` becomes that ratio to
    /// the power the band allows over the whole 21:1 range; its linear
    /// channels' offsets from `y` carried to the new luminance, scaled down
    /// with it where it falls and kept where it rises; and pulled toward
    /// grey only where a channel would leave the gamut. The backdrop pass's
    /// `tone` computes the same.
    #[must_use]
    pub fn tone(&self, rgb: [f64; 3]) -> [f64; 3] {
        if !self.compresses() {
            return rgb;
        }
        let linear = rgb.map(|c| {
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        });
        let y = 0.0722f64.mul_add(linear[2], 0.2126f64.mul_add(linear[0], 0.7152 * linear[1]));
        let power = ((self.light + 0.05) / (self.dark + 0.05)).ln() / 21f64.ln();
        let target = (self.dark + 0.05) * ((y + 0.05) / 0.05).powf(power) - 0.05;
        let keep = if y <= 1e-5 {
            1.0
        } else {
            (target / y).min(1.0)
        };
        let mut out = linear.map(|c| (c - y).mul_add(keep, target));
        let top = out[0].max(out[1]).max(out[2]);
        if top > 1.0 {
            let pull = (1.0 - target) / (top - target);
            out = out.map(|c| (c - target).mul_add(pull, target));
        }
        out.map(|c| {
            let c = c.clamp(0.0, 1.0);
            if c <= 0.003_130_8 {
                c * 12.92
            } else {
                1.055f64.mul_add(c.powf(1.0 / 2.4), -0.055)
            }
        })
    }
}

/// Most surfaces one frame blurs behind.
pub const MAX_BACKDROP_REGIONS: usize = 4;

const PLACEHOLDER_TEXEL: f64 = 2.0;

const MAX_PLACEHOLDER_EDGE: u32 = 2048;

/// The blur runs on a copy halved until its standard deviation is at most
/// this many texels, so the kernel stays short at any radius.
const MAX_SIGMA_TEXELS: f64 = 4.0;

/// A kernel reaches three standard deviations; past that a Gaussian's
/// weights are below 1 %.
const KERNEL_REACH: f64 = 3.0;

const MAX_HALVINGS: u32 = 6;

const MAX_KERNEL_RADIUS: i32 = 16;

/// Whether `material` is drawn blurred under `theme`.
pub(crate) fn blurs(material: Material, theme: &Theme) -> bool {
    matches!(material, Material::Floating) && samples(theme.finish(SurfaceRole::Floating))
}

/// Whether a surface of `finish` shows a blurred backdrop: a translucent
/// finish with a blur. Unblurred translucency is never drawn over a
/// backdrop pass (`docs/design-language.md`, "Material and
/// transparency"); such a finish is its tint over the ground it knows.
fn samples(finish: &Finish) -> bool {
    finish.translucent() && finish.blur() > 0.0
}

/// Fill `shape` as a surface of `role` is: its finish's painter set ([`Theme::painter`]) draws it over
/// `ground`.
pub fn fill_role(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    role: SurfaceRole,
    theme: &Theme,
    ground: Color,
) {
    fill_role_flush(
        scene,
        transform,
        shape,
        role,
        theme.finish(role),
        theme,
        ground,
        crate::part_paint::Sides::NONE,
    );
}

/// [`fill_role`] for a surface whose `flush` sides stand on the display edge: it draws no edge,
/// rim or hairline there, so no pixel of another colour shows between it and the screen's edge.
pub fn fill_role_flush(
    scene: &mut Scene,
    transform: Affine,
    shape: &RoundedRect,
    role: SurfaceRole,
    finish: &Finish,
    theme: &Theme,
    ground: Color,
    flush: crate::part_paint::Sides,
) {
    paint_body(
        scene,
        transform,
        shape,
        (finish, role),
        theme,
        ground,
        flush,
    );
}

fn paint_body(
    scene: &mut Scene,
    transform: Affine,
    shape: &impl Shape,
    (finish, role): (&Finish, SurfaceRole),
    theme: &Theme,
    ground: Color,
    flush: crate::part_paint::Sides,
) {
    use crate::part_paint::{Moment, Part, States, builtin, paint_part, part_tokens};
    let (rect, radius, clip) = body_of(shape);
    let part = match role {
        SurfaceRole::Window => Part::WindowGround,
        SurfaceRole::Control => Part::ControlFace,
        SurfaceRole::Floating => Part::FloatingPanel,
        SurfaceRole::Bar => Part::BarBody,
    };
    let mut tokens = part_tokens(
        theme,
        role,
        (ground, theme.tokens.colors.text_primary, Color::TRANSPARENT),
        ground,
    );
    tokens.radius = crate::num::f32_of_f64(radius);
    tokens.params = finish.params().clone();
    tokens.flush = flush;
    tokens.backdrop = matches!(
        role,
        SurfaceRole::Window | SurfaceRole::Floating | SurfaceRole::Bar
    ) && !(theme.reduce_transparency || theme.forced_colors)
        && takes_backdrop(scene);
    if let Some(clip) = &clip {
        scene.push_clip_layer(Fill::NonZero, transform, clip);
    }
    let rest = Moment::settled(States::REST);
    let painted = paint_part(
        scene,
        transform,
        rect,
        (finish.set(), part, role),
        rest,
        &tokens,
        theme,
    );
    if painted.is_none() {
        paint_part(
            scene,
            transform,
            rect,
            (builtin::default_set(), part, role),
            rest,
            &tokens,
            theme,
        );
    }
    if clip.is_some() {
        scene.pop_layer();
    }
}

fn body_of(shape: &impl Shape) -> (Rect, f64, Option<vello::kurbo::BezPath>) {
    if let Some(body) = shape.as_rounded_rect()
        && let Some(radius) = body.radii().as_single_radius()
    {
        return (body.rect(), radius, None);
    }
    if let Some(rect) = shape.as_rect() {
        return (rect, 0.0, None);
    }
    if let Some(circle) = shape.as_circle() {
        return (circle.bounding_box(), circle.radius, None);
    }
    (shape.bounding_box(), 0.0, Some(shape.to_path(0.1)))
}

/// Whether a surface drawn into `scene` now would be recorded for the backdrop pass: a recording is
/// active, `scene` is one of its layers and a region is left this frame.
fn takes_backdrop(scene: &Scene) -> bool {
    let address = std::ptr::from_ref(scene) as usize;
    RECORDING.with(|slot| {
        slot.borrow().as_ref().is_some_and(|recording| {
            (recording.layers.iter().any(|&(known, _)| known == address)
                || recording.nested.contains(&address))
                && recording.next_slot < MAX_BACKDROP_REGIONS
        })
    })
}

/// Fill `path` with what lies below it, for a painter's backdrop brush
/// (`crate::part_paint`): under an active [`BackdropRecording`], the
/// backdrop blurred by `blur` logical pixels and bent by the lens along the
/// rim of the `radius`-rounded box around the path ([`Refraction`]; the
/// pass bends it, the scene draws the result as it draws a blur). `tint`
/// is the colour the painter lays over it, which the placeholder shows
/// where no pass substitutes it. `false`, and nothing drawn, where nothing
/// may be translucent or nothing records: the caller draws the brush's
/// fallback.
///
/// The backdrop pass compresses what it samples into the material's band
/// ([`Legible`]). Light a painter glazes over the sample, and a backdrop
/// the host provided ready-made (not the pass's), the caller has in the
/// band with [`hold_in_band`] after the glaze.
pub(crate) fn fill_backdrop_sample(
    scene: &mut Scene,
    transform: Affine,
    path: &vello::kurbo::BezPath,
    sample: Sample,
    theme: &Theme,
    tint: Color,
) -> Shown {
    let Sample { blur, lens, radius } = sample;
    if theme.reduce_transparency || theme.forced_colors {
        return Shown::Fallback;
    }
    let refraction = Refraction::of(lens, radius);
    if lens.sampling == Sampling::Smooth && blur <= 0.0 && !refraction.shows_through() {
        return Shown::Fallback;
    }
    let legible = Legible::of(theme, tint);
    let recorded = record(
        scene,
        transform,
        path,
        (blur, lens, refraction, legible),
        tint,
    );
    let Some((image, brush_transform, provided)) = recorded else {
        return Shown::Fallback;
    };
    let quality = match lens.sampling {
        Sampling::Smooth => ImageQuality::Medium,
        Sampling::Blocky => ImageQuality::Low,
    };
    let brush = ImageBrush::new(image)
        .with_extend(Extend::Pad)
        .with_quality(quality);
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        path,
    );
    if provided {
        Shown::Provided
    } else {
        Shown::Compressed
    }
}

/// How [`fill_backdrop_sample`] filled a backdrop brush.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shown {
    /// Not at all: the caller draws the brush's fallback.
    Fallback,
    /// With the pass's sample, which the pass compresses into the band;
    /// a glaze over it is the caller's to hold.
    Compressed,
    /// With a backdrop the host provided, which the caller has in the
    /// band ([`hold_in_band`]).
    Provided,
}

/// Hold what was drawn in `path` over a backdrop inside the material's band around `tint`
/// ([`material_band`]): the last step of a backdrop brush, after the painter's glaze, so nothing a
/// painter draws there escapes it.
pub(crate) fn hold_in_band(
    scene: &mut Scene,
    transform: Affine,
    path: &vello::kurbo::BezPath,
    theme: &Theme,
    tint: Color,
) {
    settle(scene, transform, path, theme, tint);
}

/// Fill `shape` with `material`: under an active [`BackdropRecording`] the
/// blurred backdrop tinted with the panel ground, otherwise the opaque
/// panel ground. Surfaces reach it through
/// [`crate::floating_surface::paint_material`].
pub(crate) fn fill(
    scene: &mut Scene,
    transform: Affine,
    shape: &impl Shape,
    material: Material,
    theme: &Theme,
) {
    fill_with_ground(
        scene,
        transform,
        shape,
        material,
        theme,
        crate::chrome::panel_ground(theme),
    );
}

/// [`fill`] with a ground of the caller's choosing: a surface that lifts
/// itself off the desktop by its tone (a notification card) tints the
/// same blurred backdrop with that tone instead of the panel ground.
pub(crate) fn fill_with_ground(
    scene: &mut Scene,
    transform: Affine,
    shape: &impl Shape,
    material: Material,
    theme: &Theme,
    ground: Color,
) {
    if material == Material::SeeThrough {
        let tint = see_through_fill(theme, ground);
        if blurs(Material::Floating, theme)
            && tint.components[3] < 1.0
            && let Some((image, brush_transform, _)) = record(
                scene,
                transform,
                shape,
                (
                    theme.tokens.colors.surfaces.material_blur_px,
                    Lens::default(),
                    Refraction::NONE,
                    Legible::ANY,
                ),
                ground,
            )
        {
            let brush = ImageBrush::new(image)
                .with_extend(Extend::Pad)
                .with_quality(ImageQuality::Medium);
            scene.fill(
                Fill::NonZero,
                transform,
                &brush,
                Some(brush_transform),
                shape,
            );
        }
        scene.fill(Fill::NonZero, transform, tint, None, shape);
        return;
    }
    let solid = Finish::solid();
    let finish = match material {
        Material::Floating => theme.finish(SurfaceRole::Floating),
        Material::Opaque | Material::SeeThrough => &solid,
    };
    paint_body(
        scene,
        transform,
        shape,
        (finish, SurfaceRole::Floating),
        theme,
        ground,
        crate::part_paint::Sides::NONE,
    );
}

pub use crate::tokens::finish::MATERIAL_SPREAD;

/// The darkest and the lightest colour the floating material shows over
/// `ground`: the ground stepped toward each end of the theme's neutral ramp
/// until it is [`MATERIAL_SPREAD`] off it (or reaches the end).
#[must_use]
pub fn material_band(theme: &Theme, ground: Color) -> (Color, Color) {
    crate::tokens::finish::material_band(&theme.tokens.colors, ground)
}

fn settle(scene: &mut Scene, transform: Affine, shape: &impl Shape, theme: &Theme, ground: Color) {
    use vello::peniko::{BlendMode, Compose, Mix};
    let (dark_end, light_end) = material_band(theme, ground);
    for (mix, end) in [(Mix::Darken, light_end), (Mix::Lighten, dark_end)] {
        scene.push_layer(
            Fill::NonZero,
            BlendMode::new(mix, Compose::SrcOver),
            1.0,
            transform,
            shape,
        );
        scene.fill(Fill::NonZero, transform, end, None, shape);
        scene.pop_layer();
    }
}

/// The see-through material's fill over `ground`: half of it, unblurred,
/// opaque under reduced transparency and forced colours (and on high
/// contrast, whose token is `1`).
pub(crate) fn see_through_fill(theme: &Theme, ground: Color) -> Color {
    let opacity = if theme.reduce_transparency || theme.forced_colors {
        1.0
    } else {
        theme.tokens.colors.surfaces.see_through_opacity
    };
    ground.with_alpha(opacity)
}

/// One surface drawn in the blurred material while a recording was active, in the recording's
/// coordinates.
#[derive(Clone)]
pub struct RecordedBackdrop {
    /// The placeholder the scene draws for the blurred backdrop.
    pub image: ImageData,
    /// The surface's bounds.
    pub bounds: Rect,
    /// Blur standard deviation in recording pixels.
    pub blur_px: f64,
    /// How the sample is filtered after the blur.
    pub sampling: crate::part_paint::Sampling,
    /// The cell's side in recording pixels, for [`crate::part_paint::Sampling::Blocky`].
    pub cell_px: f64,
    /// How the backdrop is bent, in recording pixels.
    pub refraction: Refraction,
    /// The range what shows through is compressed into.
    pub legible: Legible,
    /// The layer of the scene the surface was drawn into (see [`layer_scene`]).
    pub layer: u32,
    /// What that scene held before the surface, with the layers still open there ended: a balanced
    /// scene of it.
    pub below: Scene,
    /// The opaque panel ground the material tints with.
    pub ground: Color,
}

impl RecordedBackdrop {
    /// This surface's backdrop as a region of a target that draws it at
    /// `rect`, `scale` target pixels per recording pixel, over `below`
    /// (handed what the recording saw below the surface). The one mapping
    /// from a recording to a region, so a field the pass reads is never
    /// left behind on the way.
    #[must_use]
    pub fn into_region(
        self,
        rect: Rect,
        scale: f64,
        below: impl FnOnce(Scene) -> Scene,
    ) -> BackdropRegion {
        BackdropRegion {
            image: self.image,
            rect,
            blur_px: self.blur_px * scale,
            sampling: self.sampling,
            cell_px: self.cell_px * scale,
            refraction: self.refraction.scaled(scale),
            legible: self.legible,
            below: below(self.below),
            ground: self.ground,
        }
    }

    /// How far outside its bounds the surface's blurred pixels depend on what is drawn: the
    /// kernel's reach, and what the halved passes' texels add to it (at most a quarter of the
    /// deviation, twice over, once a texel is that large).
    #[must_use]
    pub fn reach(&self) -> f64 {
        let sigma = match self.sampling {
            Sampling::Smooth => self.blur_px,
            Sampling::Blocky => self.blur_px.max(self.cell_px * 0.5),
        };
        (sigma.max(0.0) * (KERNEL_REACH + 1.0)).ceil() + 4.0
    }
}

struct Recording {
    /// How many recordings this one is nested in. Placeholders are keyed by
    /// it, so a surface recorded by an inner host (an app frame drawn while
    /// the shell records its) never shares an image with one of the
    /// outer host's, whose blur would otherwise be substituted into it.
    depth: usize,
    next_slot: usize,
    layers: Vec<(usize, u32)>,
    nested: Vec<usize>,
    /// Surfaces drawn into a nested scene, waiting for [`append_nested`] to
    /// say where that scene lands. Their `layer` is unset until then.
    pending: Vec<(usize, RecordedBackdrop)>,
    surfaces: Vec<RecordedBackdrop>,
}

type PlaceholderKey = (usize, usize);

thread_local! {
    static RECORDING: RefCell<Option<Recording>> = const { RefCell::new(None) };
    static PLACEHOLDERS: RefCell<Vec<(PlaceholderKey, ImageData)>> =
        const { RefCell::new(Vec::new()) };
    static PROVIDED: RefCell<Vec<(usize, ImageData)>> = const { RefCell::new(Vec::new()) };
}

/// Hand the next frames the blurred backdrops of their surfaces, by region
/// slot of the outermost recording: a host that cannot blur in its
/// renderer (an app rasterized on the CPU inside a compositor, which has
/// what lies below) has them made where that is known and drawn in place of
/// the placeholders, so the material's band and tint apply to them as to a
/// substituted blur. An image is used only while its size is the slot's
/// placeholder size ([`backdrop_texels`]); the rest stay opaque.
pub fn provide_backdrops(images: Vec<(usize, ImageData)>) {
    PROVIDED.with(|provided| *provided.borrow_mut() = images);
}

/// The size in texels of the image a surface with `bounds` (recording
/// pixels) is drawn with: what a provided backdrop must measure. `cell_px`
/// is the cell the surface shows as pixels ([`Sampling::Blocky`]), in the
/// same recording pixels, or `0` for a smoothly sampled surface.
///
/// One texel per cell, as [`Self::record`] takes for a blocky sample: a
/// host that blurs outside the renderer has to hand back an image the size
/// the placeholder takes, or the surface draws the opaque placeholder and
/// its look never shows.
#[must_use]
pub fn backdrop_texels(bounds: Rect, sampling: Sampling, cell_px: f64) -> (u32, u32) {
    match sampling {
        Sampling::Blocky => (
            cells(bounds.width(), cell_px.max(1.0)),
            cells(bounds.height(), cell_px.max(1.0)),
        ),
        Sampling::Smooth => (texels(bounds.width()), texels(bounds.height())),
    }
}

fn provided(slot: PlaceholderKey, width: u32, height: u32) -> Option<ImageData> {
    if slot.0 != 0 {
        return None;
    }
    PROVIDED.with(|provided| {
        provided
            .borrow()
            .iter()
            .find(|(index, image)| {
                *index == slot.1 && image.width == width && image.height == height
            })
            .map(|(_, image)| image.clone())
    })
}

/// Records the surfaces drawn in the blurred material on this thread until
/// [`Self::finish`]. Nests: an inner recording hides the outer one and
/// restores it when it ends.
#[must_use = "a recording that is dropped discards what it collected"]
pub struct BackdropRecording {
    previous: Option<Recording>,
    finished: bool,
}

impl BackdropRecording {
    pub fn begin() -> Self {
        Self::continuing(Vec::new())
    }

    /// Resume a recording whose [`Self::finish`] returned `surfaces`, for a frame painted in more than
    /// one pass (a main pass, then a top layer drawn later into the same scene).
    pub fn continuing(surfaces: Vec<RecordedBackdrop>) -> Self {
        let previous = RECORDING.with(|slot| {
            let mut slot = slot.borrow_mut();
            let depth = slot
                .as_ref()
                .map_or(0, |outer| outer.depth.saturating_add(1));
            slot.replace(Recording {
                depth,
                next_slot: surfaces.len(),
                layers: Vec::new(),
                nested: Vec::new(),
                pending: Vec::new(),
                surfaces,
            })
        });
        Self {
            previous,
            finished: false,
        }
    }

    /// The surfaces drawn since [`Self::begin`], bottom to top. A surface
    /// whose nested scene never reached a named one is not among them; its
    /// placeholder shows the opaque material.
    pub fn finish(mut self) -> Vec<RecordedBackdrop> {
        self.finished = true;
        let previous = self.previous.take();
        RECORDING
            .with(|slot| slot.replace(previous))
            .map(|recording| recording.surfaces)
            .unwrap_or_default()
    }
}

impl Drop for BackdropRecording {
    fn drop(&mut self) {
        if !self.finished {
            let previous = self.previous.take();
            RECORDING.with(|slot| slot.replace(previous));
        }
    }
}

/// Floating surfaces drawn into `scene` sit on `layer`. The host names
/// every scene it draws its frame into, so it can rebuild what lies below a
/// surface: the scenes it composes under that layer, plus
/// [`RecordedBackdrop::below`]. A host drawing one scene names it once.
///
/// A surface drawn into any other scene — a scratch scene, or a cache a
/// widget appends later without [`nest_scene`] — has no known backdrop and
/// stays opaque. `scene` must not move while the recording is active;
/// naming a layer again replaces its scene. A no-op outside a recording.
pub fn layer_scene(scene: &Scene, layer: u32) {
    let address = std::ptr::from_ref(scene) as usize;
    RECORDING.with(|slot| {
        if let Some(recording) = slot.borrow_mut().as_mut() {
            recording
                .layers
                .retain(|&(known, named)| known != address && named != layer);
            recording.layers.push((address, layer));
        }
    });
}

/// `child` is a scene a widget draws now and composes into another scene
/// afterwards with [`append_nested`] (a subtree cache). Floating surfaces
/// drawn into it wait there and take what lies below them once it is
/// appended. `child` must not move until then. A no-op outside a recording.
pub fn nest_scene(child: &Scene) {
    let address = std::ptr::from_ref(child) as usize;
    RECORDING.with(|slot| {
        if let Some(recording) = slot.borrow_mut().as_mut()
            && !recording.nested.contains(&address)
        {
            recording.nested.push(address);
        }
    });
}

/// Forget the floating surfaces drawn into `child` since [`nest_scene`]: it is about to be drawn again.
pub fn forget_nested(child: &Scene) {
    let address = std::ptr::from_ref(child) as usize;
    RECORDING.with(|slot| {
        if let Some(recording) = slot.borrow_mut().as_mut() {
            recording.pending.retain(|(known, _)| *known != address);
        }
    });
}

/// `scene.append(child, Some(transform))`, carrying the floating surfaces
/// drawn into `child` since [`nest_scene`] onto `scene`: what `scene` has
/// so far lies below them. Returns how many surfaces `child` carried. A
/// cache that carried any must be redrawn on every recorded frame: reused,
/// its surfaces would show the opaque material.
pub fn append_nested(scene: &mut Scene, child: &Scene, transform: Affine) -> usize {
    let carried = RECORDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(recording) = slot.as_mut() else {
            return 0;
        };
        let child_address = std::ptr::from_ref(child) as usize;
        let parent = std::ptr::from_ref(&*scene) as usize;
        recording.nested.retain(|&known| known != child_address);
        let parent_layer = recording
            .layers
            .iter()
            .find_map(|&(known, layer)| (known == parent).then_some(layer));
        let parent_nested = recording.nested.contains(&parent);
        let (carried, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut recording.pending)
            .into_iter()
            .partition(|(address, _)| *address == child_address);
        recording.pending = kept;
        let count = carried.len();
        for (_, mut surface) in carried {
            let mut below = scene.clone();
            below.append(&surface.below, Some(transform));
            end_open_layers(&mut below);
            surface.below = below;
            surface.bounds = transform.transform_rect_bbox(surface.bounds);
            let scale = transform.determinant().abs().sqrt();
            surface.blur_px *= scale;
            surface.cell_px *= scale;
            surface.refraction = surface.refraction.scaled(scale);
            if let Some(layer) = parent_layer {
                surface.layer = layer;
                recording.surfaces.push(surface);
            } else if parent_nested {
                recording.pending.push((parent, surface));
            }
        }
        count
    });
    scene.append(child, Some(transform));
    carried
}

fn record(
    scene: &Scene,
    transform: Affine,
    shape: &impl Shape,
    (blur_px, lens, refraction, legible): (f32, Lens, Refraction, Legible),
    ground: Color,
) -> Option<(ImageData, Affine, bool)> {
    RECORDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let recording = slot.as_mut()?;
        let address = std::ptr::from_ref(scene) as usize;
        let layer = recording
            .layers
            .iter()
            .find_map(|&(known, layer)| (known == address).then_some(layer));
        if layer.is_none() && !recording.nested.contains(&address) {
            return None;
        }
        let slot_index = recording.next_slot;
        if slot_index >= MAX_BACKDROP_REGIONS {
            return None;
        }
        let bounds = transform.transform_rect_bbox(shape.bounding_box());
        if !bounds.is_finite() || bounds.width() <= 0.0 || bounds.height() <= 0.0 {
            return None;
        }
        if transform.determinant().abs() <= f64::EPSILON {
            return None;
        }
        recording.next_slot = slot_index.saturating_add(1);
        let device_scale = transform.determinant().abs().sqrt();
        let (width, height) = match lens.sampling {
            // One texel per cell, at most the placeholder edge: the
            // image is the cells themselves.
            crate::part_paint::Sampling::Blocky => {
                let cell = f64::from(lens.cell.max(1.0));
                (cells(bounds.width(), cell), cells(bounds.height(), cell))
            }
            crate::part_paint::Sampling::Smooth
                if f64::from(blur_px) * device_scale < PLACEHOLDER_TEXEL =>
            {
                (cells(bounds.width(), 1.0), cells(bounds.height(), 1.0))
            }
            crate::part_paint::Sampling::Smooth => {
                (texels(bounds.width()), texels(bounds.height()))
            }
        };
        let cell_px = f64::from(lens.cell.max(1.0));
        let key = (recording.depth, slot_index);
        let given = provided(key, width, height);
        let ready_made = given.is_some();
        let image = given.unwrap_or_else(|| placeholder(key, width, height, ground));
        let brush_transform = transform.inverse()
            * Affine::translate((bounds.x0, bounds.y0))
            * Affine::scale_non_uniform(
                bounds.width() / f64::from(width),
                bounds.height() / f64::from(height),
            );
        let surface = RecordedBackdrop {
            image: image.clone(),
            bounds,
            blur_px: f64::from(blur_px) * device_scale,
            sampling: lens.sampling,
            cell_px: cell_px * device_scale,
            refraction: refraction.scaled(device_scale),
            legible,
            layer: layer.unwrap_or_default(),
            below: {
                let mut below = scene.clone();
                end_open_layers(&mut below);
                below
            },
            ground,
        };
        if layer.is_some() {
            recording.surfaces.push(surface);
        } else {
            recording.pending.push((address, surface));
        }
        Some((image, brush_transform, ready_made))
    })
}

/// End the layers `below` leaves open, so a copy of a scene taken part way
/// through drawing it is a scene of it.
///
/// A surface is often drawn inside layers its scene still has open: the clip
/// of the list or window it floats over, the alpha layer of it fade-in.
/// Those layers end after the surface, so what lies below it is everything
/// drawn so far with them ended where the copy stops, exactly as a renderer
/// ends a scene's open layers. The render service draws a backdrop's scene by
/// itself and refuses one that leaves a clip open, and with it the whole frame.
fn end_open_layers(below: &mut Scene) {
    for _ in 0..below.encoding().n_open_clips {
        below.pop_layer();
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=MAX_PLACEHOLDER_EDGE before the cast"
)]
fn texels(logical: f64) -> u32 {
    (logical / PLACEHOLDER_TEXEL)
        .ceil()
        .clamp(1.0, f64::from(MAX_PLACEHOLDER_EDGE)) as u32
}

/// Placeholder texels for a cell-sampled edge of `logical` pixels: one texel per cell of `cell`, so the
/// image's texels are the cells.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "clamped to 1..=MAX_PLACEHOLDER_EDGE before the cast"
)]
fn cells(logical: f64, cell: f64) -> u32 {
    (logical / cell.max(1.0))
        .ceil()
        .clamp(1.0, f64::from(MAX_PLACEHOLDER_EDGE)) as u32
}

fn placeholder(slot: PlaceholderKey, width: u32, height: u32, ground: Color) -> ImageData {
    let rgba = ground.to_rgba8().to_u8_array();
    PLACEHOLDERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((_, image)) = cache.iter().find(|(index, image)| {
            *index == slot
                && image.width == width
                && image.height == height
                && image.data.data().get(..4) == Some(&rgba[..])
        }) {
            return image.clone();
        }
        let texel_count = (width as usize).saturating_mul(height as usize);
        let bytes: Vec<u8> = rgba
            .iter()
            .copied()
            .cycle()
            .take(texel_count.saturating_mul(4))
            .collect();
        let image = crate::raster::rgba8(bytes, width, height);
        cache.retain(|(index, _)| *index != slot);
        cache.push((slot, image.clone()));
        image
    })
}

/// One surface's backdrop, in the pixel space of the target it renders to.
#[derive(Clone)]
pub struct BackdropRegion {
    /// The placeholder the frame draws; [`BackdropBlur`] substitutes it.
    pub image: ImageData,
    /// The target pixels the placeholder is mapped onto.
    pub rect: Rect,
    /// Blur standard deviation in target pixels.
    pub blur_px: f64,
    /// How the sample is filtered after the blur: smoothly, or at one colour per cell of `cell_px`
    /// target pixels (a surface shown as pixels).
    pub sampling: Sampling,
    /// The cell's side in target pixels, for [`Sampling::Blocky`].
    pub cell_px: f64,
    /// How the backdrop is bent, in target pixels.
    pub refraction: Refraction,
    /// The range what shows through is compressed into.
    pub legible: Legible,
    /// Everything composed below the surface, in target pixels.
    pub below: Scene,
    /// The opaque panel ground: what the backdrop shows where nothing below covers it and the target is
    /// cleared to transparency.
    pub ground: Color,
}

/// The backdrops one frame draws its floating surfaces over.
#[derive(Clone, Default)]
pub struct BackdropFrame {
    pub regions: Vec<BackdropRegion>,
}

impl BackdropFrame {
    /// The backdrops of a frame drawn into one scene (named with [`layer_scene`]), whose coordinates
    /// are already the target's: each surface's backdrop is what was drawn before it.
    pub fn from_single_scene(surfaces: Vec<RecordedBackdrop>) -> Self {
        Self {
            regions: surfaces
                .into_iter()
                .map(|surface| {
                    let rect = surface.bounds;
                    surface.into_region(rect, 1.0, |below| below)
                })
                .collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.regions.is_empty()
    }
}

const SHADER_SOURCE: &str = r"
struct Pass {
    uv_offset: vec2<f32>,
    uv_scale: vec2<f32>,
    step: vec2<f32>,
    sigma: f32,
    radius: i32,
    // The lens, for the last pass (`Refraction`, in target pixels): the
    // region's size, its corner radius, the band, the displacement at the
    // rim and the dispersion. `strength` 0 bends nothing.
    size: vec2<f32>,
    corner: f32,
    band: f32,
    strength: f32,
    dispersion: f32,
    // The luminance range the last pass compresses what it shows into
    // (`Legible`); `0`, `1` none.
    dark: f32,
    light: f32,
}

@group(0) @binding(0) var source: texture_2d<f32>;
@group(0) @binding(1) var bilinear: sampler;
@group(0) @binding(2) var<uniform> pass_: Pass;

struct VertexOut {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) index: u32) -> VertexOut {
    let uv = vec2<f32>(f32((index << 1u) & 2u), f32(index & 2u));
    var out: VertexOut;
    out.position = vec4<f32>(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
    out.uv = uv;
    return out;
}

fn source_uv(uv: vec2<f32>) -> vec2<f32> {
    return pass_.uv_offset + uv * pass_.uv_scale;
}

// Each target texel centre lands on the corner shared by four source
// texels, so one bilinear tap is their box average.
@fragment
fn fs_halve(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSampleLevel(source, bilinear, source_uv(in.uv), 0.0);
}

fn blur_at(centre: vec2<f32>) -> vec4<f32> {
    let falloff = -0.5 / (pass_.sigma * pass_.sigma);
    var sum = textureSampleLevel(source, bilinear, centre, 0.0);
    var total = 1.0;
    for (var i = 1; i <= pass_.radius; i++) {
        let x = f32(i);
        let weight = exp(x * x * falloff);
        let offset = pass_.step * x;
        sum += weight * (textureSampleLevel(source, bilinear, centre + offset, 0.0)
            + textureSampleLevel(source, bilinear, centre - offset, 0.0));
        total += 2.0 * weight;
    }
    return sum / total;
}

fn away(v: f32) -> f32 {
    return select(1.0, -1.0, v < 0.0);
}

// Where the backdrop the region's pixel at `point` shows lies, relative to
// it: toward the inside along the rounded outline's normal, `strength` at
// the outline easing quadratically to nothing `band` in. The same
// arithmetic as `Refraction::displacement`.
fn bend(point: vec2<f32>) -> vec2<f32> {
    let half = pass_.size * 0.5;
    let shorter = min(half.x, half.y);
    let corner = clamp(pass_.corner, 0.0, shorter);
    let p = point - half;
    let q = abs(p) - (half - vec2<f32>(corner));
    let outside = max(q, vec2<f32>(0.0));
    let distance = length(outside) + min(max(q.x, q.y), 0.0) - corner;
    var normal: vec2<f32>;
    if (q.x > 0.0 && q.y > 0.0) {
        normal = outside / length(outside) * vec2<f32>(away(p.x), away(p.y));
    } else if (q.x > q.y) {
        normal = vec2<f32>(away(p.x), 0.0);
    } else {
        normal = vec2<f32>(0.0, away(p.y));
    }
    let t = clamp(1.0 + distance / min(pass_.band, shorter), 0.0, 1.0);
    return -normal * (min(pass_.strength, shorter) * t * t);
}

fn linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3<f32>(2.4)), c / 12.92, c <= vec3<f32>(0.04045));
}

fn encoded(c: vec3<f32>) -> vec3<f32> {
    return select(1.055 * pow(c, vec3<f32>(1.0 / 2.4)) - 0.055, c * 12.92, c <= vec3<f32>(0.0031308));
}

// What shows through, compressed into the legible range: luminance mapped
// evenly in contrast, each channel's offset from it scaled down where it
// falls and kept where it rises, pulled toward grey only where a channel
// would leave the gamut. The same arithmetic as `Legible::tone`.
fn tone(c: vec4<f32>) -> vec4<f32> {
    if (pass_.dark <= 0.0 && pass_.light >= 1.0) {
        return c;
    }
    let lin = linear(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)));
    let y = dot(lin, vec3<f32>(0.2126, 0.7152, 0.0722));
    let power = log((pass_.light + 0.05) / (pass_.dark + 0.05)) / log(21.0);
    let aim = (pass_.dark + 0.05) * pow((y + 0.05) / 0.05, power) - 0.05;
    var keep = 1.0;
    if (y > 1e-5) {
        keep = min(aim / y, 1.0);
    }
    var out = vec3<f32>(aim) + (lin - vec3<f32>(y)) * keep;
    let top = max(out.r, max(out.g, out.b));
    if (top > 1.0) {
        out = vec3<f32>(aim) + (out - vec3<f32>(aim)) * ((1.0 - aim) / (top - aim));
    }
    return vec4<f32>(encoded(clamp(out, vec3<f32>(0.0), vec3<f32>(1.0))), c.a);
}

// The blur, and in the last pass the lens and the legible range: the
// kernel is taken about the point the lens shows, so the bend costs one
// displaced lookup (three where red and blue part).
@fragment
fn fs_blur(in: VertexOut) -> @location(0) vec4<f32> {
    if (pass_.strength <= 0.0 || pass_.band <= 0.0) {
        return tone(blur_at(source_uv(in.uv)));
    }
    let shift = bend(in.uv * pass_.size) / pass_.size;
    let seen = blur_at(source_uv(in.uv + shift));
    if (pass_.dispersion <= 0.0) {
        return tone(seen);
    }
    let red = blur_at(source_uv(in.uv + shift * (1.0 - pass_.dispersion)));
    let blue = blur_at(source_uv(in.uv + shift * (1.0 + pass_.dispersion)));
    return tone(vec4<f32>(red.r, seen.g, blue.b, seen.a));
}
";

const PASS_BYTES: u64 = 64;

/// What [`BackdropBlur::prepare`] did, for frame profiling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BackdropStats {
    /// Regions whose placeholder now shows the blurred backdrop.
    pub substituted: usize,
    /// Regions that stay opaque: off target, or their backdrop failed to render.
    pub skipped: usize,
    /// Of the substituted, those shown from the sample they kept: nothing below them changed since it
    /// was taken, so no pass ran.
    pub reused: usize,
    /// CPU time spent encoding and submitting, in microseconds.
    pub encode_us: u64,
}

struct Plan {
    /// The region's index in its frame, which picks its output texture.
    slot: usize,
    first_pass: u64,
    /// Target pixels rendered as the backdrop: the region plus the blur's reach, clipped to the target
    /// and rounded out.
    source: Rect,
    levels: Vec<(u32, u32)>,
    /// Target pixels per texel of the rendered backdrop: two, or one for a sample blurred less than two
    /// pixels (a sharp lens), which would show the half resolution as softness.
    ground_texel: f64,
    /// Target pixels per texel of the last level.
    texel: f64,
    /// The blur's deviation in texels of the last level; `0` none.
    sigma: f64,
}

struct Texture {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
}

impl Texture {
    fn size(&self) -> (u32, u32) {
        (self.texture.width(), self.texture.height())
    }
}

/// The GPU half of the material: renders, blurs and substitutes each region's backdrop on one device.
pub struct BackdropBlur {
    layout: wgpu::BindGroupLayout,
    halve: wgpu::RenderPipeline,
    blur: wgpu::RenderPipeline,
    sampler: wgpu::Sampler,
    passes: wgpu::Buffer,
    pass_stride: u64,
    /// Vello's half-resolution backdrop target.
    ground: Option<Texture>,
    /// Halved copies, one per level below the backdrop.
    levels: Vec<Option<Texture>>,
    /// The horizontal pass's result.
    across: Option<Texture>,
    /// The blurred images vello reads in place of the placeholders, one per region slot.
    outputs: Vec<Option<Texture>>,
    /// Placeholders currently substituted on the renderer.
    substituted: Vec<ImageData>,
    /// What each region slot's output was taken of, so a region whose backdrop has not changed shows it
    /// again without a pass.
    taken: Vec<Option<Taken>>,
}

struct Taken {
    below: Scene,
    rect: Rect,
    size: (u32, u32),
    target: (u32, u32),
    numbers: [u64; 12],
    colours: [[u32; 4]; 2],
}

impl Taken {
    fn of(region: &BackdropRegion, target: (u32, u32), base: Color) -> Self {
        let lens = region.refraction;
        Self {
            below: region.below.clone(),
            rect: region.rect,
            size: (region.image.width, region.image.height),
            target,
            numbers: [
                region.blur_px,
                region.cell_px,
                lens.strength,
                lens.band,
                lens.radius,
                lens.dispersion,
                region.legible.dark,
                region.legible.light,
                f64::from(u8::from(region.sampling == Sampling::Blocky)),
                0.0,
                0.0,
                0.0,
            ]
            .map(f64::to_bits),
            colours: [region.ground, base].map(|colour| colour.components.map(f32::to_bits)),
        }
    }

    fn holds(&self, other: &Self) -> bool {
        self.rect == other.rect
            && self.size == other.size
            && self.target == other.target
            && self.numbers == other.numbers
            && self.colours == other.colours
            && self.below.encoding_eq(&other.below)
    }
}

/// Whether `below` draws any of `images`: another region's sample, taken anew this frame, which it
/// shows through itself.
fn shows_any(below: &Scene, images: &[ImageData]) -> bool {
    !images.is_empty()
        && below.encoding().resources.patches.iter().any(|patch| {
            matches!(patch, vello_encoding::Patch::Image { image, .. }
                if images.iter().any(|taken| taken.data.id() == image.data.id()))
        })
}

impl BackdropBlur {
    pub fn new(device: &wgpu::Device) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("tilo-backdrop-blur"),
            source: wgpu::ShaderSource::Wgsl(SHADER_SOURCE.into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tilo-backdrop-blur"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(PASS_BYTES),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tilo-backdrop-blur"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = |entry_point: &str| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("tilo-backdrop-blur"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &module,
                    entry_point: Some("vs_main"),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &module,
                    entry_point: Some(entry_point),
                    compilation_options: wgpu::PipelineCompilationOptions::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format: wgpu::TextureFormat::Rgba8Unorm,
                        blend: None,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let halve = pipeline("fs_halve");
        let blur = pipeline("fs_blur");
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("tilo-backdrop-blur"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let alignment = u64::from(device.limits().min_uniform_buffer_offset_alignment).max(1);
        let pass_stride = PASS_BYTES.div_ceil(alignment).saturating_mul(alignment);
        let max_passes = (MAX_HALVINGS as usize + 2) * MAX_BACKDROP_REGIONS;
        let passes = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("tilo-backdrop-blur-passes"),
            size: pass_stride.saturating_mul(max_passes as u64),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            layout,
            halve,
            blur,
            sampler,
            passes,
            pass_stride,
            ground: None,
            levels: Vec::new(),
            across: None,
            outputs: Vec::new(),
            taken: Vec::new(),
            substituted: Vec::new(),
        }
    }

    /// Render, blur and substitute every region of `frame` on `renderer`,
    /// whose next render draws a `target`-sized frame cleared to `base`.
    /// A region that cannot be prepared keeps its opaque placeholder.
    /// Call [`Self::release`] once that render is submitted.
    pub fn prepare(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        frame: &BackdropFrame,
        target: (u32, u32),
        base: Color,
    ) -> BackdropStats {
        self.release(renderer);
        let started = crate::time::now();
        let mut stats = BackdropStats::default();
        let max_edge = device.limits().max_texture_dimension_2d;
        let target_rect = Rect::new(0.0, 0.0, f64::from(target.0), f64::from(target.1));
        let mut pass_bytes: Vec<u8> = Vec::new();
        let mut planned = Vec::new();
        for (slot, region) in frame.regions.iter().take(MAX_BACKDROP_REGIONS).enumerate() {
            match plan(region, target_rect, max_edge) {
                Some(mut plan) => {
                    plan.slot = slot;
                    plan.first_pass = pass_bytes.len() as u64 / PASS_BYTES;
                    write_passes(&mut pass_bytes, &plan, region);
                    planned.push((region, plan));
                }
                None => stats.skipped = stats.skipped.saturating_add(1),
            }
        }
        stats.skipped = stats
            .skipped
            .saturating_add(frame.regions.len().saturating_sub(MAX_BACKDROP_REGIONS));
        if planned.is_empty() {
            return stats;
        }
        queue.write_buffer(&self.passes, 0, &self.strided(&pass_bytes));
        let mut fresh: Vec<ImageData> = Vec::new();
        for (region, plan) in planned {
            let slot = plan.slot;
            let now = Taken::of(region, target, base);
            if self.taken.len() <= slot {
                self.taken.resize_with(slot.saturating_add(1), || None);
            }
            let unchanged = self
                .taken
                .get(slot)
                .and_then(Option::as_ref)
                .is_some_and(|taken| taken.holds(&now))
                && !shows_any(&region.below, &fresh);
            if unchanged && self.show_kept(renderer, region, slot) {
                stats.substituted = stats.substituted.saturating_add(1);
                stats.reused = stats.reused.saturating_add(1);
                continue;
            }
            if self.render_one(device, queue, renderer, region, &plan, base) {
                stats.substituted = stats.substituted.saturating_add(1);
                fresh.push(region.image.clone());
                if let Some(taken) = self.taken.get_mut(slot) {
                    *taken = Some(now);
                }
            } else {
                stats.skipped = stats.skipped.saturating_add(1);
                if let Some(taken) = self.taken.get_mut(slot) {
                    *taken = None;
                }
            }
        }
        stats.encode_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        stats
    }

    fn show_kept(&mut self, renderer: &mut Renderer, region: &BackdropRegion, slot: usize) -> bool {
        let Some(Some(output)) = self.outputs.get(slot) else {
            return false;
        };
        renderer.override_image(
            &region.image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: output.texture.clone(),
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        self.substituted.push(region.image.clone());
        true
    }

    /// Drop the substitutions of the last [`Self::prepare`], so later
    /// renders on `renderer` draw the opaque placeholders again.
    pub fn release(&mut self, renderer: &mut Renderer) {
        for image in self.substituted.drain(..) {
            renderer.override_image(&image, None);
        }
    }

    fn strided(&self, packed: &[u8]) -> Vec<u8> {
        let stride = usize::try_from(self.pass_stride).unwrap_or(256);
        let mut out = Vec::new();
        for pass in packed.chunks(PASS_BYTES as usize) {
            let start = out.len();
            out.extend_from_slice(pass);
            out.resize(start.saturating_add(stride), 0);
        }
        out
    }

    fn render_one(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut Renderer,
        region: &BackdropRegion,
        plan: &Plan,
        base: Color,
    ) -> bool {
        let slot = plan.slot;
        let Some(&ground_size) = plan.levels.first() else {
            return false;
        };
        let ground = ensure_texture(
            device,
            &mut self.ground,
            ground_size,
            wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING,
            Some(&mut *renderer),
        );
        let mut below = Scene::new();
        below.append(
            &region.below,
            Some(
                Affine::scale(1.0 / plan.ground_texel)
                    * Affine::translate((-plan.source.x0, -plan.source.y0)),
            ),
        );
        // The material is opaque: a target cleared to transparency (an
        // offscreen capture) must not let the scene under the placeholder
        // show through unblurred.
        let base = if base.components[3] < 1.0 {
            region.ground
        } else {
            base
        };
        let rendered = renderer.render_to_texture(
            device,
            queue,
            &below,
            &ground.view,
            &RenderParams {
                base_color: base,
                width: ground_size.0,
                height: ground_size.1,
                antialiasing_method: AaConfig::Area,
            },
        );
        if let Err(error) = rendered {
            tracing::warn!(?error, "backdrop render failed; the surface stays opaque");
            return false;
        }
        let halvings = plan.levels.len().saturating_sub(1);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tilo-backdrop-blur"),
        });
        let mut pass = plan.first_pass;
        for (index, &size) in plan.levels.iter().enumerate().skip(1) {
            let Some(level) = ensure_level(device, &mut self.levels, index.saturating_sub(1), size)
            else {
                return false;
            };
            let source = if index == 1 {
                self.ground.as_ref().map(|texture| texture.view.clone())
            } else {
                self.levels
                    .get(index.saturating_sub(2))
                    .and_then(Option::as_ref)
                    .map(|texture| texture.view.clone())
            };
            let Some(source) = source else {
                return false;
            };
            self.draw(device, &mut encoder, &self.halve, &source, pass, &level);
            pass = pass.saturating_add(1);
        }
        let last_size = plan.levels.last().copied().unwrap_or(ground_size);
        let last = if halvings == 0 {
            self.ground.as_ref().map(|texture| texture.view.clone())
        } else {
            self.levels
                .get(halvings.saturating_sub(1))
                .and_then(Option::as_ref)
                .map(|texture| texture.view.clone())
        };
        let Some(last) = last else {
            return false;
        };
        let across = if plan.sigma > 0.0 {
            let across = ensure_texture(
                device,
                &mut self.across,
                last_size,
                wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                None,
            )
            .view
            .clone();
            self.draw(device, &mut encoder, &self.blur, &last, pass, &across);
            across
        } else {
            last
        };
        pass = pass.saturating_add(1);
        if self.outputs.len() <= slot {
            self.outputs.resize_with(slot.saturating_add(1), || None);
        }
        let Some(output_slot) = self.outputs.get_mut(slot) else {
            return false;
        };
        let output = ensure_texture(
            device,
            output_slot,
            (region.image.width, region.image.height),
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            None,
        );
        let output_view = output.view.clone();
        let output_texture = output.texture.clone();
        self.draw(
            device,
            &mut encoder,
            &self.blur,
            &across,
            pass,
            &output_view,
        );
        queue.submit([encoder.finish()]);
        renderer.override_image(
            &region.image,
            Some(wgpu::TexelCopyTextureInfoBase {
                texture: output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            }),
        );
        self.substituted.push(region.image.clone());
        true
    }

    fn draw(
        &self,
        device: &wgpu::Device,
        encoder: &mut wgpu::CommandEncoder,
        pipeline: &wgpu::RenderPipeline,
        source: &wgpu::TextureView,
        pass: u64,
        target: &wgpu::TextureView,
    ) {
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("tilo-backdrop-blur"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(source),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &self.passes,
                        offset: 0,
                        size: wgpu::BufferSize::new(PASS_BYTES),
                    }),
                },
            ],
        });
        let offset = u32::try_from(pass.saturating_mul(self.pass_stride)).unwrap_or(0);
        let mut render = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("tilo-backdrop-blur"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        render.set_pipeline(pipeline);
        render.set_bind_group(0, &bind_group, &[offset]);
        render.draw(0..3, 0..1);
    }
}

/// The blur `region` is drawn with, in target pixels: its, and for a
/// cell-sampled backdrop at least half a cell, so each cell (one texel of
/// the placeholder the last pass resamples onto) is the average of what
/// lies below it rather than one point of it.
fn region_sigma(region: &BackdropRegion) -> f64 {
    match region.sampling {
        Sampling::Smooth => region.blur_px,
        Sampling::Blocky => region.blur_px.max(region.cell_px * 0.5),
    }
}

const SHARP_SIGMA: f64 = 2.0;

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "sizes are rounded, positive and bounded by the device's texture limit before the cast"
)]
fn plan(region: &BackdropRegion, target: Rect, max_edge: u32) -> Option<Plan> {
    let sigma = region_sigma(region);
    if !sigma.is_finite()
        || sigma < 0.0
        || (sigma == 0.0 && !region.refraction.shows_through())
        || region.image.width == 0
        || region.image.height == 0
    {
        return None;
    }
    if region.rect.intersect(target).area() <= 0.0 {
        return None;
    }
    let reach = (sigma * KERNEL_REACH).ceil();
    let source = region.rect.inflate(reach, reach).intersect(target);
    let source = Rect::new(
        source.x0.floor(),
        source.y0.floor(),
        source.x1.ceil(),
        source.y1.ceil(),
    );
    if source.width() < 1.0 || source.height() < 1.0 {
        return None;
    }
    let ground_texel = if sigma < SHARP_SIGMA { 1.0 } else { 2.0 };
    let rendered = |edge: f64| ((edge / ground_texel).ceil().max(1.0) as u32).min(max_edge.max(1));
    let mut size = (rendered(source.width()), rendered(source.height()));
    let mut levels = vec![size];
    let mut texel = ground_texel;
    while sigma / texel > MAX_SIGMA_TEXELS
        && size.0.min(size.1) >= 8
        && levels.len() <= MAX_HALVINGS as usize
    {
        size = (size.0.div_ceil(2), size.1.div_ceil(2));
        levels.push(size);
        texel *= 2.0;
    }
    Some(Plan {
        slot: 0,
        first_pass: 0,
        source,
        levels,
        ground_texel,
        texel,
        sigma: sigma / texel,
    })
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "shader parameters are f32 by definition; the values are small texture coordinates"
)]
fn write_passes(bytes: &mut Vec<u8>, plan: &Plan, region: &BackdropRegion) {
    let mut push = |offset: [f64; 2],
                    scale: [f64; 2],
                    step: [f64; 2],
                    (sigma, radius): (f64, i32),
                    (lens, legible): (Refraction, Legible)| {
        for value in [
            offset[0], offset[1], scale[0], scale[1], step[0], step[1], sigma,
        ] {
            bytes.extend_from_slice(&(value as f32).to_le_bytes());
        }
        bytes.extend_from_slice(&radius.to_le_bytes());
        let bends = lens.bends();
        for value in [
            region.rect.width(),
            region.rect.height(),
            lens.radius,
            lens.band,
            if bends { lens.strength } else { 0.0 },
            lens.dispersion.max(0.0),
            legible.dark,
            legible.light,
        ] {
            bytes.extend_from_slice(&(value as f32).to_le_bytes());
        }
    };
    for pair in plan.levels.windows(2) {
        if let [source, target] = pair {
            let scale = [
                2.0 * f64::from(target.0) / f64::from(source.0),
                2.0 * f64::from(target.1) / f64::from(source.1),
            ];
            push(
                [0.0, 0.0],
                scale,
                [0.0, 0.0],
                (1.0, 0),
                (Refraction::NONE, Legible::ANY),
            );
        }
    }
    let (width, height) = plan.levels.last().copied().unwrap_or((1, 1));
    let (width, height) = (f64::from(width), f64::from(height));
    let kernel = if plan.sigma > 0.0 {
        let radius = (plan.sigma * KERNEL_REACH)
            .ceil()
            .clamp(1.0, f64::from(MAX_KERNEL_RADIUS));
        #[allow(
            clippy::cast_possible_truncation,
            reason = "clamped to 1..=MAX_KERNEL_RADIUS"
        )]
        let radius = radius as i32;
        (plan.sigma, radius)
    } else {
        (1.0, 0)
    };
    push(
        [0.0, 0.0],
        [1.0, 1.0],
        [1.0 / width, 0.0],
        kernel,
        (Refraction::NONE, Legible::ANY),
    );
    let offset = [
        (region.rect.x0 - plan.source.x0) / plan.texel / width,
        (region.rect.y0 - plan.source.y0) / plan.texel / height,
    ];
    let scale = [
        region.rect.width() / plan.texel / width,
        region.rect.height() / plan.texel / height,
    ];
    push(
        offset,
        scale,
        [0.0, 1.0 / height],
        kernel,
        (region.refraction, region.legible),
    );
}

fn ensure_texture<'a>(
    device: &wgpu::Device,
    slot: &'a mut Option<Texture>,
    size: (u32, u32),
    usage: wgpu::TextureUsages,
    renderer: Option<&mut Renderer>,
) -> &'a Texture {
    if slot.as_ref().is_some_and(|texture| texture.size() != size)
        && let (Some(old), Some(renderer)) = (slot.take(), renderer)
    {
        renderer.invalidate_texture_bindings(&old.view);
    }
    if slot.as_ref().is_some_and(|texture| texture.size() != size) {
        *slot = None;
    }
    slot.get_or_insert_with(|| {
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("tilo-backdrop"),
            size: wgpu::Extent3d {
                width: size.0.max(1),
                height: size.1.max(1),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        Texture { texture, view }
    })
}

fn ensure_level(
    device: &wgpu::Device,
    levels: &mut Vec<Option<Texture>>,
    index: usize,
    size: (u32, u32),
) -> Option<wgpu::TextureView> {
    if levels.len() <= index {
        levels.resize_with(index.saturating_add(1), || None);
    }
    let slot = levels.get_mut(index)?;
    Some(
        ensure_texture(
            device,
            slot,
            size,
            wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            None,
        )
        .view
        .clone(),
    )
}

#[cfg(test)]
mod tests;

pub mod desktop;
