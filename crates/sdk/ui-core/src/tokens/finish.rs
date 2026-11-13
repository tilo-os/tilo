//! Finishes: what each surface role is made of, how its edges are drawn and how it separates
//! from what is below it (`docs/architecture/theming.md`, "Material, shape and depth"). A
//! theme document sets one [`Finish`] per surface role ([`RoleFinishes`]: the window
//! ground, raised controls, floating chrome and bars), one [`Border`] and one [`Depth`].
//! Legibility never depends on a finish: whatever lies below a translucent or glass
//! surface is held inside a band around the role's ground before the tint is laid over it
//! (`crate::backdrop::material_band`), so ink keeps the contrast it has on the opaque
//! ground over the worst backdrop (`crate::theme::ThemeDocument::findings`).

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use serde::{Deserialize, Serialize};

use crate::part_paint::{PainterSet, Param, builtin};

/// What one surface role is made of: the painter set that draws it and that
/// set's parameters.
///
/// In a document a role names its painter set (`"painter"`) beside the
/// set's own parameters, the ones the set declares
/// ([`crate::part_paint::PartPainter::params`]):
///
/// | Document | Look |
/// |---|---|
/// | `{ "painter": "block" }` | flat, square to the border |
/// | `{ "painter": "default" }` | the design language's opaque surfaces |
/// | `{ "painter": "default", "blur": 24, "tint": 0.72, "saturation": 1 }` | the blurred backdrop under the ground at a tint |
/// | `{ "painter": "glass", "blur": …, "tint": …, "saturation": …, "refraction": …, "edge_highlight": …, "specular": … }` | the same, bent and lit |
///
/// A set this build does not have, a parameter the set does not declare and
/// a required one left out are not admitted.
///
/// The toolkit itself understands three parameters, whichever set states
/// them: `blur` (how far the backdrop is blurred, logical pixels), `tint`
/// (the ground's opacity over it) and `saturation`. A role that states any
/// of them lets what is below it through ([`Finish::translucent`]).
#[derive(Clone)]
pub struct Finish {
    set: PainterSet,
    params: Arc<[Param]>,
    shader: crate::tokens::shader::Source,
}

#[derive(Deserialize)]
struct FinishDocument {
    painter: String,
    #[serde(flatten)]
    params: BTreeMap<String, f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    shader: Option<String>,
    #[serde(default)]
    time: Option<f32>,
}

impl Serialize for Finish {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let extra = usize::from(!self.shader.is_empty());
        let mut map = serializer.serialize_map(Some(
            self.params.len().saturating_add(extra).saturating_add(1),
        ))?;
        map.serialize_entry("painter", self.set.name())?;
        for param in self.params.iter() {
            map.serialize_entry(&param.name, &param.value)?;
        }
        if !self.shader.is_empty() {
            map.serialize_entry("shader", &self.shader.wgsl)?;
            if self.shader.time != 0.0 {
                map.serialize_entry("time", &self.shader.time)?;
            }
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Finish {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let document = FinishDocument::deserialize(deserializer)?;
        let mut finish =
            Self::named(&document.painter, document.params).map_err(serde::de::Error::custom)?;
        if let Some(wgsl) = document.shader {
            finish.shader = crate::tokens::shader::Source {
                wgsl,
                time: document.time.unwrap_or(0.0),
            };
        } else if document.time.is_some() {
            return Err(serde::de::Error::custom(
                "a finish states time for a shader, and it states none",
            ));
        }
        Ok(finish)
    }
}

impl std::fmt::Debug for Finish {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("Finish");
        out.field("painter", &self.set.name());
        for param in self.params.iter() {
            out.field(&param.name, &param.value);
        }
        if !self.shader.is_empty() {
            out.field("shader", &self.shader);
        }
        out.finish()
    }
}

impl PartialEq for Finish {
    fn eq(&self, other: &Self) -> bool {
        self.set.name() == other.set.name()
            && self.params == other.params
            && self.shader == other.shader
    }
}

static SOLID: LazyLock<Finish> = LazyLock::new(|| Finish::builtin(builtin::default_set(), &[]));
static FLAT: LazyLock<Finish> = LazyLock::new(|| Finish::builtin(builtin::block_set(), &[]));

impl Finish {
    /// `set` with `params`, as the set declares them: every parameter it requires stated,
    /// none it does not declare.
    pub fn new<N: Into<String>>(
        set: PainterSet,
        params: impl IntoIterator<Item = (N, f32)>,
    ) -> Result<Self, &'static str> {
        let mut given: Vec<(String, f32)> = params
            .into_iter()
            .map(|(name, value)| (name.into(), value))
            .collect();
        let mut ordered = Vec::with_capacity(given.len());
        for spec in set.painter().params() {
            match given.iter().position(|(name, _)| *name == spec.name) {
                Some(at) => {
                    let (name, value) = given.swap_remove(at);
                    ordered.push(Param { name, value });
                }
                None if spec.default.is_none() => return Err("painter parameter missing"),
                None => {}
            }
        }
        if !given.is_empty() {
            return Err("painter parameter the set does not take");
        }
        Ok(Self {
            set,
            params: ordered.into(),
            shader: crate::tokens::shader::Source::empty(),
        })
    }

    /// The set a document calls `name`, with `params`.
    pub fn named<N: Into<String>>(
        name: &str,
        params: impl IntoIterator<Item = (N, f32)>,
    ) -> Result<Self, &'static str> {
        let set = crate::part_paint::painter_set(name).ok_or("painter set not installed")?;
        Self::new(set, params)
    }

    fn builtin(set: &PainterSet, params: &[(&str, f32)]) -> Self {
        Self {
            set: set.clone(),
            params: params
                .iter()
                .map(|&(name, value)| Param {
                    name: name.to_owned(),
                    value,
                })
                .collect(),
            shader: crate::tokens::shader::Source::empty(),
        }
    }

    /// The theme's own surface shader, for the `shader` set; empty otherwise.
    #[must_use]
    pub fn shader(&self) -> &crate::tokens::shader::Source {
        &self.shader
    }

    /// This finish with `source` as its surface shader.
    #[must_use]
    pub fn with_shader(mut self, source: crate::tokens::shader::Source) -> Self {
        self.shader = source;
        self
    }

    /// The design language's opaque surface: the Default set as it is.
    #[must_use]
    pub fn solid() -> Self {
        SOLID.clone()
    }

    /// A flat surface: the Block set.
    #[must_use]
    pub fn flat() -> Self {
        FLAT.clone()
    }

    /// The Default set showing what is below, blurred by `blur` logical pixels, its
    /// colour scaled by `saturation`, under the ground at `tint` opacity.
    #[must_use]
    pub fn frosted(blur: f32, tint: f32, saturation: f32) -> Self {
        Self::builtin(
            builtin::default_set(),
            &[("blur", blur), ("tint", tint), ("saturation", saturation)],
        )
    }

    /// The Glass set with
    /// `[blur, tint, saturation, refraction, edge_highlight, specular]`, for tests.
    #[cfg(test)]
    pub(crate) fn glass(
        [blur, tint, saturation, refraction, edge_highlight, specular]: [f32; 6],
    ) -> Self {
        Self::builtin(
            builtin::glass_set(),
            &[
                ("blur", blur),
                ("tint", tint),
                ("saturation", saturation),
                ("refraction", refraction),
                ("edge_highlight", edge_highlight),
                ("specular", specular),
            ],
        )
    }

    /// The stock floating material: the design language's blurred panel.
    #[must_use]
    pub fn stock_floating() -> Self {
        Self::frosted(
            crate::tokens::colors::MATERIAL_BLUR_PX,
            crate::tokens::colors::MATERIAL_ALPHA,
            1.0,
        )
    }

    /// The stock glass material: what a bar and the floating chrome are made of where a
    /// person asks to see what lies below — the backdrop blurred, left mostly to show and
    /// a little bent and lit at the rim, so a glass bar reads as glass over any wallpaper
    /// rather than as its ground.
    #[must_use]
    pub fn stock_glass() -> Self {
        const TINT: f32 = 0.3;
        const SATURATION: f32 = 1.6;
        const REFRACTION: f32 = 10.0;
        const EDGE_HIGHLIGHT: f32 = 0.4;
        const SPECULAR: f32 = 0.14;
        Self::builtin(
            builtin::glass_set(),
            &[
                ("blur", crate::tokens::colors::MATERIAL_BLUR_PX),
                ("tint", TINT),
                ("saturation", SATURATION),
                ("refraction", REFRACTION),
                ("edge_highlight", EDGE_HIGHLIGHT),
                ("specular", SPECULAR),
            ],
        )
    }

    /// The set that draws this finish.
    #[must_use]
    pub fn set(&self) -> &PainterSet {
        &self.set
    }

    /// The set's name, as a document names it.
    #[must_use]
    pub fn painter(&self) -> &str {
        self.set.name()
    }

    /// The parameters the document states, in the set's order.
    #[must_use]
    pub fn params(&self) -> &Arc<[Param]> {
        &self.params
    }

    /// The parameter `name`, where the document states it.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<f32> {
        self.params
            .iter()
            .find(|param| param.name == name)
            .map(|param| param.value)
    }

    /// Whether the role lets what is below it through: it states a blur, a tint or a
    /// saturation.
    #[must_use]
    pub fn translucent(&self) -> bool {
        ["blur", "tint", "saturation"]
            .iter()
            .any(|name| self.param(name).is_some())
    }

    /// The backdrop blur, logical pixels; `0` where none is stated.
    #[must_use]
    pub fn blur(&self) -> f32 {
        self.param("blur").unwrap_or(0.0)
    }

    /// The tint's opacity over the backdrop; `1` where none is stated.
    #[must_use]
    pub fn tint(&self) -> f32 {
        self.param("tint").unwrap_or(1.0)
    }

    /// The backdrop's saturation; `1` where none is stated.
    #[must_use]
    pub fn saturation(&self) -> f32 {
        self.param("saturation").unwrap_or(1.0)
    }

    /// Whether the role draws the hairline the design language gives it: every set's
    /// surfaces but Block's, which carry the border alone.
    #[must_use]
    pub fn hairline(&self) -> bool {
        self.set != *builtin::block_set()
    }

    /// The finish actually painted where nothing may be translucent (reduced
    /// transparency, high contrast): the toolkit's own sets become solid, Block staying
    /// flat; a theme's own set keeps drawing, every backdrop it asks for shown as its
    /// fallback.
    #[must_use]
    pub fn opaque(&self) -> &Self {
        if self.set == *builtin::default_set() || self.set == *builtin::glass_set() {
            &SOLID
        } else {
            self
        }
    }

    /// The finish painted under forced colours: what they guarantee is not a theme's set
    /// to keep, so always the toolkit's own, solid (Block staying flat).
    #[must_use]
    pub fn forced(&self) -> &Self {
        if self.set == *builtin::block_set() {
            &FLAT
        } else {
            &SOLID
        }
    }

    /// Structural check: every parameter finite and in the band its set admits, and a
    /// shader a theme stated is a surface shader
    /// ([`crate::tokens::shader::Source::validate`]).
    pub fn validate(&self) -> Result<(), &'static str> {
        let specs = self.set.painter().params();
        for param in self.params.iter() {
            let spec = specs
                .iter()
                .find(|spec| spec.name == param.name)
                .ok_or("painter parameter the set does not take")?;
            if !(param.value.is_finite() && spec.admitted.contains(&param.value)) {
                return Err("painter parameter outside its band");
            }
        }
        if self.set == *builtin::shader_set() {
            self.shader
                .validate()
                .map_err(|_| "surface shader is not a shader a theme may draw")?;
        } else if !self.shader.is_empty() {
            return Err("a shader is only a shader-set parameter");
        }
        Ok(())
    }

    /// This finish with every parameter held to the band its set is built at.
    #[must_use]
    pub fn usable(&self) -> Self {
        let specs = self.set.painter().params();
        let params = self
            .params
            .iter()
            .map(|param| {
                let value = specs
                    .iter()
                    .find(|spec| spec.name == param.name)
                    .map_or(param.value, |spec| {
                        param.value.clamp(*spec.usable.start(), *spec.usable.end())
                    });
                Param {
                    name: param.name.clone(),
                    value,
                }
            })
            .collect();
        Self {
            set: self.set.clone(),
            params,
            shader: self.shader.clone(),
        }
    }

    /// This finish at `strength` of its translucency: `0` turns its blur off and its tint
    /// opaque (and a glass body's light with it), `1` leaves it as it is.
    #[must_use]
    pub fn with_translucency(&self, strength: f32) -> Self {
        let strength = strength.clamp(0.0, 1.0);
        if self.params.is_empty() {
            return self.clone();
        }
        let params = self
            .params
            .iter()
            .map(|param| {
                let value = match param.name.as_str() {
                    // The ground's opacity over the backdrop: `0` shows it all, `1` hides
                    // it completely, so a weaker translucency raises the tint toward
                    // opaque.
                    "tint" => param.value + (1.0 - strength) * (1.0 - param.value),
                    "saturation" => 1.0 - strength * (1.0 - param.value),
                    // A look's own grain, not its translucency: a cell, a pixel grid, a
                    // shader's pattern stays what it is at any strength, or a calm
                    // surface would lose the cells it is made of (and a `0` cell draws
                    // nothing).
                    "cell" | "grid" | "scale" | "phase" | "light_angle" => param.value,
                    _ => param.value * strength,
                };
                Param {
                    name: param.name.clone(),
                    value,
                }
            })
            .collect();
        Self {
            set: self.set.clone(),
            params,
            shader: self.shader.clone(),
        }
    }
}

/// The surface roles a theme finishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SurfaceRole {
    Window,
    Control,
    Floating,
    Bar,
}

/// What the shell's chrome — its bar and its dock — is made of, as the person chose it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeMaterial {
    #[default]
    Bar,
    Glass,
    Calm,
}

crate::component::choice!(ChromeMaterial, "ChromeMaterial", {
    Bar => "bar",
    Glass => "glass",
    Calm => "calm",
});

/// One finish per surface role: the theme document's `material` role
/// (`crate::theme::ThemeMaterial`) as the toolkit paints it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleFinishes {
    /// The window ground. Windows never overlap, so a translucent window shows the
    /// desktop behind it.
    pub window: Finish,
    pub control: Finish,
    pub floating: Finish,
    pub bar: Option<Finish>,
}

impl RoleFinishes {
    /// The design language's finishes: opaque windows and controls, the blurred floating
    /// material, and a glass bar.
    #[must_use]
    pub fn stock() -> Self {
        Self {
            window: Finish::solid(),
            control: Finish::solid(),
            floating: Finish::stock_floating(),
            bar: Some(Finish::stock_glass()),
        }
    }

    /// Everything opaque: high contrast and forced colours.
    #[must_use]
    pub fn solid() -> Self {
        Self {
            window: Finish::solid(),
            control: Finish::solid(),
            floating: Finish::solid(),
            bar: Some(Finish::solid()),
        }
    }

    /// The finish of `role`; a bar without a body reads as flat.
    #[must_use]
    pub fn of(&self, role: SurfaceRole) -> &Finish {
        match role {
            SurfaceRole::Window => &self.window,
            SurfaceRole::Control => &self.control,
            SurfaceRole::Floating => &self.floating,
            SurfaceRole::Bar => self.bar.as_ref().unwrap_or(&*FLAT),
        }
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        self.window.validate()?;
        self.control.validate()?;
        self.floating.validate()?;
        if let Some(bar) = &self.bar {
            bar.validate()?;
        }
        Ok(())
    }

    /// These finishes with every parameter held to its usable band.
    #[must_use]
    pub fn usable(&self) -> Self {
        Self {
            window: self.window.usable(),
            control: self.control.usable(),
            floating: self.floating.usable(),
            bar: self.bar.as_ref().map(Finish::usable),
        }
    }

    /// These finishes where nothing may be translucent.
    #[must_use]
    pub fn opaque(&self) -> Self {
        Self {
            window: self.window.opaque().clone(),
            control: self.control.opaque().clone(),
            floating: self.floating.opaque().clone(),
            bar: self.bar.as_ref().map(|bar| bar.opaque().clone()),
        }
    }
}

impl Default for RoleFinishes {
    fn default() -> Self {
        Self::stock()
    }
}

/// Which ink a border is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BorderInk {
    Hairline,
    Standard,
    /// Every surface of every role outlined in the strong ink: the tertiary text tier,
    /// which clears 3:1 on every surface (WCAG 1.4.11).
    Strong,
}

/// How surfaces are outlined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Border {
    /// Width of an outline, logical pixels.
    pub width: f32,
    pub ink: BorderInk,
}

impl Border {
    pub const STOCK: Self = Self {
        width: 1.0,
        ink: BorderInk::Hairline,
    };

    /// Width band a document may state.
    pub const WIDTH_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=4.0;

    /// Whether every surface carries an outline of its own.
    #[must_use]
    pub fn outlines(&self) -> bool {
        self.ink != BorderInk::Hairline && self.width > 0.0
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if !(self.width.is_finite() && Self::WIDTH_ADMITTED.contains(&self.width)) {
            return Err("border width outside 0..=4");
        }
        Ok(())
    }
}

impl Default for Border {
    fn default() -> Self {
        Self::STOCK
    }
}

/// How a surface separates from what is below it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Depth {
    /// Soft shadows, the design language's elevation, their strength scaled by `strength`
    /// (`1` stock).
    Shadow {
        strength: f32,
    },
    Outline,
    None,
}

impl Depth {
    pub const STOCK: Self = Self::Shadow { strength: 1.0 };

    /// Shadow strength band a document may state.
    pub const STRENGTH_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;

    /// The factor every shadow's opacity is scaled by.
    #[must_use]
    pub const fn shadow_strength(self) -> f32 {
        match self {
            Self::Shadow { strength } => strength,
            Self::Outline | Self::None => 0.0,
        }
    }

    pub fn validate(self) -> Result<(), &'static str> {
        if let Self::Shadow { strength } = self
            && !(strength.is_finite() && Self::STRENGTH_ADMITTED.contains(&strength))
        {
            return Err("shadow strength outside 0..=2");
        }
        Ok(())
    }
}

impl Default for Depth {
    fn default() -> Self {
        Self::STOCK
    }
}

/// How far, as a contrast ratio, a translucent surface may stand off its ground anywhere,
/// whatever is behind it.
pub const MATERIAL_SPREAD: f64 = 1.5;

/// How far what shows through a translucent surface of `ground` may stand off it:
/// [`MATERIAL_SPREAD`], narrowed to what the theme's inks can afford. Contrast ratios on
/// one side of a ground divide exactly, so ink with ratio `r` on the ground keeps
/// `r / spread` on the band's far end; the spread is the least of `r / floor` over the
/// three ink tiers (body text for primary and secondary, large text for tertiary, AAA
/// text on a high-contrast palette), never below `1` (an opaque surface).
#[must_use]
pub fn material_spread(colors: &crate::tokens::Colors, ground: vello::peniko::Color) -> f64 {
    use crate::tokens::contrast::{BODY_TEXT_MIN_CONTRAST, HIGH_CONTRAST_TEXT_MIN_CONTRAST};
    let text = if colors.palette.high_contrast {
        HIGH_CONTRAST_TEXT_MIN_CONTRAST
    } else {
        BODY_TEXT_MIN_CONTRAST
    };
    let large = if colors.palette.high_contrast {
        HIGH_CONTRAST_TEXT_MIN_CONTRAST
    } else {
        crate::tokens::colors::NON_TEXT_MIN_CONTRAST
    };
    [
        (colors.text_primary, text),
        (colors.text_dim, text),
        (colors.text_tertiary, large),
    ]
    .into_iter()
    .map(|(ink, floor)| crate::color::contrast_ratio(ink, ground) / floor * (1.0 - 1e-6))
    .fold(MATERIAL_SPREAD, f64::min)
    .max(1.0)
}

/// The darkest and the lightest colour a translucent surface of `ground` shows, whatever
/// lies below it: on the side of the ground its ink stands on, the ground stepped toward
/// that end of the theme's neutral ramp until it stands [`material_spread`] off it (or
/// reaches the end); on the other side, the ramp's end itself.
#[must_use]
pub fn material_band(
    colors: &crate::tokens::Colors,
    ground: vello::peniko::Color,
) -> (vello::peniko::Color, vello::peniko::Color) {
    use vello::peniko::Color;
    let spread = material_spread(colors, ground);
    let toward = |end: Color| {
        let at = |t: f32| {
            let [r, g, b, _] = ground.components;
            let [er, eg, eb, _] = end.components;
            Color::new([r + (er - r) * t, g + (eg - g) * t, b + (eb - b) * t, 1.0])
        };
        if crate::color::contrast_ratio(at(1.0), ground) <= spread {
            return at(1.0);
        }
        let (mut within, mut beyond) = (0.0_f32, 1.0_f32);
        for _ in 0..24 {
            let mid = (within + beyond) / 2.0;
            if crate::color::contrast_ratio(at(mid), ground) <= spread {
                within = mid;
            } else {
                beyond = mid;
            }
        }
        at(within)
    };
    let stops = &colors.neutral.stops;
    // Ink stands on one side of its ground; what shows through on the other side only
    // parts further from it, so that side runs to the ramp's end and keeps more of what
    // lies below.
    let luminance = crate::color::relative_luminance;
    let ink_lighter = luminance(colors.text_primary) > luminance(ground);
    let far = |end: Color| {
        let [r, g, b, _] = end.components;
        Color::new([r, g, b, 1.0])
    };
    let dark_first = luminance(stops[0]) <= luminance(stops[10]);
    let (first, last) = match (ink_lighter, dark_first) {
        (true, true) | (false, false) => (far(stops[0]), toward(stops[10])),
        (true, false) | (false, true) => (toward(stops[0]), far(stops[10])),
    };
    if crate::color::relative_luminance(first) <= crate::color::relative_luminance(last) {
        (first, last)
    } else {
        (last, first)
    }
}

/// The opaque ground a surface of `role` is tinted with.
#[must_use]
pub fn role_ground(colors: &crate::tokens::Colors, role: SurfaceRole) -> vello::peniko::Color {
    match role {
        SurfaceRole::Window => colors.surfaces.content,
        SurfaceRole::Control => colors.surfaces.raised,
        SurfaceRole::Floating | SurfaceRole::Bar => colors.surfaces.overlay,
    }
}

/// What a surface of `role` can look like at its darkest and its lightest under
/// `finishes`, over any backdrop: the role's ground when it is opaque; the band around it
/// ([`material_band`]) when it shows a blurred backdrop; and for a translucent control,
/// which is never blurred, its ground at its tint over the darkest and lightest window it
/// can stand on.
#[must_use]
pub fn worst_grounds(
    colors: &crate::tokens::Colors,
    finishes: &RoleFinishes,
    role: SurfaceRole,
) -> [vello::peniko::Color; 2] {
    let ground = role_ground(colors, role);
    let finish = finishes.of(role);
    if !finish.translucent() {
        return [ground, ground];
    }
    if role == SurfaceRole::Control {
        let below = worst_grounds(colors, finishes, SurfaceRole::Window);
        return below.map(|under| crate::tokens::colors::over(under, ground, finish.tint()));
    }
    let (dark, light) = material_band(colors, ground);
    [dark, light]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glass_with(refraction: f32, specular: f32, saturation: f32) -> Finish {
        Finish::named(
            "glass",
            [
                ("blur", 18.0),
                ("tint", 0.25),
                ("saturation", saturation),
                ("refraction", refraction),
                ("edge_highlight", 0.4),
                ("specular", specular),
            ],
        )
        .unwrap()
    }

    #[test]
    fn every_finish_round_trips_and_out_of_band_values_are_refused() {
        let glass = glass_with(10.0, 0.15, 1.6);
        for finish in [
            Finish::flat(),
            Finish::solid(),
            Finish::frosted(24.0, 0.5, 1.2),
            glass.clone(),
        ] {
            let json = serde_json::to_value(&finish).unwrap();
            assert_eq!(serde_json::from_value::<Finish>(json).unwrap(), finish);
            assert_eq!(finish.validate(), Ok(()));
        }
        let json = serde_json::to_value(&glass).unwrap();
        assert_eq!(json["painter"], serde_json::json!("glass"));
        assert_eq!(json["refraction"], serde_json::json!(10.0));
        assert_eq!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "block"})).unwrap(),
            Finish::flat()
        );
        assert_eq!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "default"})).unwrap(),
            Finish::solid()
        );
        assert_eq!(
            serde_json::from_value::<Finish>(serde_json::json!({
                "painter": "default", "saturation": 1.2, "tint": 0.5, "blur": 24.0
            }))
            .unwrap(),
            Finish::frosted(24.0, 0.5, 1.2)
        );
        assert!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "block", "blur": 3.0}))
                .is_err(),
            "the block set takes no blur"
        );
        assert!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "glass", "blur": 3.0}))
                .is_err(),
            "glass states every parameter"
        );
        assert!(
            serde_json::from_value::<Finish>(serde_json::json!({"painter": "chrome"})).is_err(),
            "a painter set this build does not have is not admitted"
        );
        for bad in [
            Finish::frosted(80.0, 0.5, 1.0),
            Finish::frosted(8.0, f32::NAN, 1.0),
            glass_with(64.0, 0.1, 1.0),
            glass_with(8.0, 0.9, 1.0),
            glass_with(8.0, 0.1, 3.0),
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert!((Finish::frosted(60.0, 0.5, 1.0).usable().blur() - 48.0).abs() < 1e-6);
    }

    #[test]
    fn opaque_keeps_flat_flat_and_makes_the_rest_solid() {
        let block = RoleFinishes {
            window: Finish::flat(),
            control: Finish::flat(),
            floating: Finish::flat(),
            bar: Some(Finish::flat()),
        };
        assert_eq!(block.opaque(), block);
        assert_eq!(RoleFinishes::stock().opaque().floating, Finish::solid());
        assert_eq!(*glass_with(4.0, 0.1, 1.0).opaque(), Finish::solid());
        assert_eq!(*glass_with(4.0, 0.1, 1.0).forced(), Finish::solid());
        assert_eq!(*Finish::flat().forced(), Finish::flat());
        assert_eq!(
            *RoleFinishes {
                bar: None,
                ..RoleFinishes::stock()
            }
            .of(SurfaceRole::Bar),
            Finish::flat(),
            "a theme without a bar body draws it flat"
        );
        let stock = RoleFinishes::stock();
        let stock_bar = stock.of(SurfaceRole::Bar);
        assert!(stock_bar.translucent() && stock_bar.blur() > 0.0);
        assert!(stock_bar.hairline(), "the stock bar keeps its hairline");
        assert_eq!(
            stock.opaque().bar.as_ref(),
            Some(&Finish::solid()),
            "nothing may be translucent"
        );
        assert!(!Finish::flat().hairline());
        assert!(Finish::solid().hairline());
        assert!(!Finish::solid().translucent());
        assert!(Finish::stock_floating().translucent());
    }

    /// A calm surface is the glass with its translucency off: its light falls away with
    /// the strength, but a look's own grain — a pixel cell, the cell grid, a shader's
    /// pattern size — is what the look is made of, so it stays.
    #[test]
    fn calm_keeps_the_looks_own_grain_and_drops_the_light() {
        let pixelated = Finish::named(
            "pixelated",
            [("cell", 4.0), ("cover", 0.8), ("blur", 0.0), ("grid", 0.2)],
        )
        .unwrap();
        let calm = pixelated.with_translucency(0.0);
        assert_eq!(calm.painter(), "pixelated");
        assert_eq!(calm.param("cell"), Some(4.0), "the cells stay a look");
        assert_eq!(calm.param("cover"), Some(0.0), "nothing shows through");
        assert_eq!(calm.param("grid"), Some(0.2), "the grid stays");
        let glass = glass_with(16.0, 0.2, 1.5);
        let calm = glass.with_translucency(0.0);
        assert!((calm.param("refraction").unwrap_or(0.0)).abs() < 1e-6);
        assert!((calm.param("specular").unwrap_or(0.0)).abs() < 1e-6);
        assert!((calm.tint() - 1.0).abs() < 1e-6, "the ground is opaque");
    }

    #[test]
    fn depth_and_border_round_trip() {
        for depth in [Depth::STOCK, Depth::Outline, Depth::None] {
            let json = serde_json::to_value(depth).unwrap();
            assert_eq!(serde_json::from_value::<Depth>(json).unwrap(), depth);
        }
        assert!(Depth::Shadow { strength: 3.0 }.validate().is_err());
        let border = Border {
            width: 2.0,
            ink: BorderInk::Strong,
        };
        let json = serde_json::to_value(border).unwrap();
        assert_eq!(serde_json::from_value::<Border>(json).unwrap(), border);
        assert!(border.outlines());
        assert!(!Border::STOCK.outlines());
        assert!(
            Border {
                width: 9.0,
                ..border
            }
            .validate()
            .is_err()
        );
    }
}
