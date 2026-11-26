//! The blurred material floating chrome is made of. Floating chrome — the palette, menus, popovers,
//! quick settings, sheets, notifications and chrome over media — is drawn over a blurred copy of what
//! is behind it, tinted with the panel ground (`docs/design-language.md`, "Material and transparency").
//!
//! A fill asks the frame's backdrop recording for the sample it draws; the blur pass that renders and
//! blurs what lies below a surface is [`crate::backdrop`].
#![allow(
    clippy::arithmetic_side_effects,
    reason = "backdrop geometry is bounded floating-point pixel math"
)]

use std::cell::RefCell;

use vello::Scene;
use vello::kurbo::{Affine, Rect, RoundedRect, Shape, Vec2};
use vello::peniko::{Color, Extend, Fill, ImageBrush, ImageData, ImageQuality};

use super::Lens;
pub use super::Sampling;
use super::material::Material;
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

/// A theme's material stage as one region runs it (`crate::tokens::shader`): its WGSL, and the
/// uniforms the pipeline around it reads, in the order `shader::wrap` declares them.
#[derive(Clone, Debug, PartialEq)]
pub struct Stage {
    pub wgsl: std::sync::Arc<str>,
    pub uniforms: [f32; STAGE_FLOATS],
}

/// How many numbers a stage's uniforms hold: ten `vec4<f32>`.
pub const STAGE_FLOATS: usize = 40;

impl Stage {
    /// The stage `source` runs for a part of `size` (logical pixels) with `tokens`, the pointer at
    /// `pointer` in the part's coordinates (`z` 1 while there is one).
    #[must_use]
    pub(crate) fn of(
        source: &super::ShaderSource,
        tokens: &super::PartTokens,
        size: vello::kurbo::Size,
        pointer: [f32; 3],
    ) -> Self {
        let mut inputs = super::shader::Inputs::of(
            tokens,
            Rect::from_origin_size((0.0, 0.0), size),
            source.time_centis,
        );
        inputs.pointer = pointer;
        let mut uniforms = [0.0; STAGE_FLOATS];
        let rows: [[f32; 4]; 10] = [
            inputs.fill,
            inputs.ink,
            inputs.ground,
            inputs.raised,
            inputs.accent,
            inputs.light,
            inputs.params,
            [inputs.size[0], inputs.size[1], inputs.radius, inputs.time],
            [
                inputs.pointer[0],
                inputs.pointer[1],
                inputs.pointer[2],
                inputs.refraction,
            ],
            [
                inputs.light_dir[0],
                inputs.light_dir[1],
                inputs.light_dir[2],
                0.0,
            ],
        ];
        for (row, values) in uniforms.chunks_exact_mut(4).zip(rows) {
            row.copy_from_slice(&values);
        }
        Self {
            wgsl: source.wgsl.as_str().into(),
            uniforms,
        }
    }

    pub(crate) fn key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.wgsl.hash(&mut hasher);
        for value in self.uniforms {
            value.to_bits().hash(&mut hasher);
        }
        hasher.finish()
    }

    pub(crate) fn source_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.wgsl.hash(&mut hasher);
        hasher.finish()
    }
}

thread_local! {
    /// Where the pointer is over this thread's window, in its physical pixels: what a material
    /// stage reads as `pointer`.
    static POINTER: std::cell::Cell<Option<(f64, f64)>> = const { std::cell::Cell::new(None) };
    /// Whether the last recorded frame drew a material stage, which a moving pointer redraws.
    static STAGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The pointer is at `at` over this thread's window, in its physical pixels, or has left it.
pub fn set_pointer(at: Option<(f64, f64)>) {
    POINTER.with(|pointer| pointer.set(at));
}

/// Whether the window shows a material stage, so a pointer that moves over it asks for a frame.
#[must_use]
pub fn follows_pointer() -> bool {
    STAGED.with(std::cell::Cell::get)
}

/// The pointer in the coordinates of a part `transform` maps into the scene, from the part's
/// `origin`; `z` 1 while there is one.
fn pointer_in(transform: Affine, origin: vello::kurbo::Point) -> [f32; 3] {
    let Some((x, y)) = POINTER.with(std::cell::Cell::get) else {
        return [0.0; 3];
    };
    if transform.determinant().abs() <= f64::EPSILON {
        return [0.0; 3];
    }
    let at = transform.inverse() * vello::kurbo::Point::new(x, y) - origin.to_vec2();
    [
        crate::num::f32_of_f64(at.x),
        crate::num::f32_of_f64(at.y),
        1.0,
    ]
}

/// The luminance range what shows through a translucent surface is
/// compressed into, so ink chosen against the surface's ground stays
/// legible over anything below it: the material's band
/// ([`material_band`]) as relative luminances, around the ground's own. The
/// backdrop pass maps what it sampled into it ([`Self::tone`] is the same
/// arithmetic): the ground's luminance shows as it is, and on each side of it
/// luminance is compressed evenly in contrast (a contrast ratio to the ground
/// becomes a smaller, fixed power of itself) so black and white land on the
/// band's ends. What lies near the ground, a page under a menu, so shows
/// nearly as it is and the surface's tint lifts it; only what stands far off
/// it is pulled in. Each colour keeps its hue: dimmed,
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
    /// Relative luminance that shows as it is: the surface's ground, between `dark` and `light`.
    pub ground: f64,
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
        ground: 0.0,
    };

    /// The range a surface of `ground` under `theme` has what shows through it in.
    #[must_use]
    pub fn of(theme: &Theme, ground: Color) -> Self {
        let (dark, light) = material_band(theme, ground);
        let (dark, light) = (
            crate::color::relative_luminance(dark),
            crate::color::relative_luminance(light),
        );
        let (dark, light) = (dark.min(light), dark.max(light));
        Self {
            dark,
            light,
            ground: crate::color::relative_luminance(ground).clamp(dark, light),
        }
    }

    /// Whether anything is compressed.
    #[must_use]
    pub fn compresses(&self) -> bool {
        self.dark > 0.0 || self.light < 1.0
    }

    /// `rgb` (sRGB-encoded, 0–1) as it shows through: its relative
    /// luminance `y` mapped so its contrast ratio to the ground becomes that
    /// ratio to the power the band allows on `y`'s side of the ground
    /// ([`Self::aim`]); its linear
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
        let target = self.aim(y);
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

    /// The luminance `y` shows at: the ground's own stays, and the contrast to the ground on
    /// `y`'s side is raised to the power that brings black to `dark` and white to `light`.
    #[must_use]
    pub fn aim(&self, y: f64) -> f64 {
        let ground = self.ground + 0.05;
        let (end, far) = if y < self.ground {
            (self.dark + 0.05, 0.05)
        } else {
            (self.light + 0.05, 1.05)
        };
        let reach = (far / ground).ln();
        if reach.abs() < 1e-9 {
            return self.ground;
        }
        let power = (end / ground).ln() / reach;
        (ground * ((y + 0.05) / ground).powf(power) - 0.05).clamp(self.dark, self.light)
    }
}

/// Most surfaces one frame blurs behind.
pub const MAX_BACKDROP_REGIONS: usize = 4;

const PLACEHOLDER_TEXEL: f64 = 2.0;

const MAX_PLACEHOLDER_EDGE: u32 = 2048;

/// The blur runs on a copy halved until its standard deviation is at most
/// this many texels, so the kernel stays short at any radius.
pub(crate) const MAX_SIGMA_TEXELS: f64 = 4.0;

/// A kernel reaches three standard deviations; past that a Gaussian's
/// weights are below 1 %.
pub(crate) const KERNEL_REACH: f64 = 3.0;

pub(crate) const MAX_HALVINGS: u32 = 6;

pub(crate) const MAX_KERNEL_RADIUS: i32 = 16;

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
        super::Sides::NONE,
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
    flush: super::Sides,
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
    flush: super::Sides,
) {
    use super::{Moment, Part, States, builtin, paint_part, part_tokens};
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
        (tint, None),
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

/// Fill `path` (part coordinates, the part's box `rect`) with a theme's material stage over what
/// lies below it: under an active [`BackdropRecording`], a region the backdrop pass blurs by `blur`
/// logical pixels and then runs the stage over on the GPU; where a host provided the backdrop
/// ready-made, the stage evaluated on the CPU over it. `false`, and nothing drawn, where nothing may
/// be translucent or nothing records: the caller evaluates the stage over the part's ground.
/// What is drawn is then held in the material's band around `tokens.fill`.
pub(crate) fn fill_stage(
    scene: &mut Scene,
    transform: Affine,
    (path, rect): (&vello::kurbo::BezPath, Rect),
    blur: f32,
    (source, tokens): (&super::ShaderSource, &super::PartTokens),
    theme: &Theme,
) -> bool {
    if theme.reduce_transparency || theme.forced_colors || source.wgsl.trim().is_empty() {
        return false;
    }
    let tint = tokens.fill;
    let stage = Stage::of(
        source,
        tokens,
        rect.size(),
        pointer_in(transform, rect.origin()),
    );
    let recorded = record(
        scene,
        transform,
        &rect,
        (
            blur,
            Lens::default(),
            Refraction::NONE,
            Legible::of(theme, tint),
        ),
        (tint, Some(stage)),
    );
    let Some((image, brush_transform, provided)) = recorded else {
        return false;
    };
    let (image, brush_transform) = if provided {
        let behind = super::shader::Behind { image: &image };
        let staged = super::shader::evaluate(source, rect, tokens, Some(&behind), tint);
        let placed = Affine::translate(rect.origin().to_vec2())
            * Affine::scale_non_uniform(
                rect.width() / f64::from(staged.width.max(1)),
                rect.height() / f64::from(staged.height.max(1)),
            );
        (staged, placed)
    } else {
        (image, brush_transform)
    };
    let brush = ImageBrush::new(image)
        .with_extend(Extend::Pad)
        .with_quality(ImageQuality::Medium);
    scene.fill(
        Fill::NonZero,
        transform,
        &brush,
        Some(brush_transform),
        path,
    );
    settle(scene, transform, path, theme, tint);
    true
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
/// [`super::material::paint_material`].
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
/// itself off the shell by its tone (a notification card) tints the
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
                (ground, None),
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
        super::Sides::NONE,
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
pub fn see_through_fill(theme: &Theme, ground: Color) -> Color {
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
    pub sampling: super::Sampling,
    /// The cell's side in recording pixels, for [`super::Sampling::Blocky`].
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
    /// The theme's material stage run over the blurred sample, if the surface's material has one.
    pub stage: Option<Stage>,
}

impl RecordedBackdrop {
    /// The size in texels of the image this surface is drawn with: what a host that blurs outside
    /// the renderer is asked for and has to hand back ([`provide_backdrops`]), or the surface draws
    /// its opaque placeholder and its look never shows. It is the placeholder's own size, so it
    /// follows whatever the recording took: one texel per cell for a surface shown as pixels, one
    /// per pixel for a material stage or a blur too small to halve, coarser texels otherwise.
    #[must_use]
    pub fn texels(&self) -> (u32, u32) {
        (self.image.width, self.image.height)
    }

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
            stage: self.stage,
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
    /// say where that scene is placed. Their `layer` is unset until then.
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
/// placeholder size ([`RecordedBackdrop::texels`]); the rest stay opaque.
pub fn provide_backdrops(images: Vec<(usize, ImageData)>) {
    PROVIDED.with(|provided| *provided.borrow_mut() = images);
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
        if RECORDING.with(|slot| slot.borrow().is_none()) {
            STAGED.with(|staged| staged.set(false));
        }
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
    (ground, stage): (Color, Option<Stage>),
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
            // A stage draws the surface's own light and edges: one texel per target pixel, so
            // they stay as sharp as the scene around them.
            super::Sampling::Smooth if stage.is_some() => {
                (cells(bounds.width(), 1.0), cells(bounds.height(), 1.0))
            }
            // One texel per cell, at most the placeholder edge: the
            // image is the cells themselves.
            super::Sampling::Blocky => {
                let cell = f64::from(lens.cell.max(1.0));
                (cells(bounds.width(), cell), cells(bounds.height(), cell))
            }
            super::Sampling::Smooth if f64::from(blur_px) * device_scale < PLACEHOLDER_TEXEL => {
                (cells(bounds.width(), 1.0), cells(bounds.height(), 1.0))
            }
            super::Sampling::Smooth => (texels(bounds.width()), texels(bounds.height())),
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
            stage,
        };
        if surface.stage.is_some() {
            STAGED.with(|staged| staged.set(true));
        }
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
    /// The placeholder the frame draws; the blur pass substitutes it.
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
    /// The theme's material stage, run over the blurred sample on the GPU; `None` shows the sample.
    pub stage: Option<Stage>,
}

/// The backdrops one frame draws its floating surfaces over.
#[derive(Clone, Default)]
pub struct BackdropFrame {
    pub regions: Vec<BackdropRegion>,
}

impl BackdropRegion {
    /// The size in texels of the image the region is drawn with ([`RecordedBackdrop::texels`]).
    #[must_use]
    pub fn texels(&self) -> (u32, u32) {
        (self.image.width, self.image.height)
    }
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

#[cfg(test)]
mod tests;
