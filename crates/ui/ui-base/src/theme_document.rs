//! The theme document: everything a theme may say, as data (`docs/design-language.md`,
//! "Theming"; `docs/architecture/theming.md`). Built-in presets, distribution defaults,
//! the user's theme and every theme a person writes are one [`ThemeDocument`]. It
//! names semantic roles only — a palette, an emphasis, the presence tone, the shape
//! (window radius and border), density, type, motion, what each surface role is made of
//! and how surfaces stand off what is below them — and never a widget, so it cannot
//! restyle an app's internals, remove the lift or the focus ring, or break a contrast
//! floor that [`ThemeDocument::findings`] measures and the runtime repairs.

use serde::{Deserialize, Serialize};

use crate::tokens::colors::{Colors, PresenceHue};
use crate::tokens::contrast::{ContrastFinding, audit, audit_materials};
use crate::tokens::finish::{Border, Depth, Finish, RoleFinishes};
use crate::tokens::palette::{Emphasis, Palette, Tone};
use crate::tokens::selector::SelectorFinish;
use settings_model::theme::{Look, Preset, ThemeDensity};

/// The schema this build writes.
pub const THEME_SCHEMA_VERSION: u32 = 8;

/// The window tile's radius, from which every other radius derives concentrically
/// (`crate::tokens::Radii::from_window_radius`; `0` makes every corner square), and how
/// surfaces are outlined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeShape {
    pub window_radius: f32,
    #[serde(default)]
    pub border: Border,
}

impl ThemeShape {
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=40.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 0.0..=28.0;
}

/// How icons are drawn ([`crate::icons`]): one icon set, at a weight the theme names.
/// Sizes stay the metrics' (a 16 px glyph in a row, 20 at a bar's head); a theme moves
/// the outline, never the size a layout is built against.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeIcons {
    /// The outline's width in logical pixels: 1.5 at stock (Lucide's own); a look with
    /// pixels wants 2, a whole-pixel stroke it can land on.
    pub stroke: f32,
}

impl ThemeIcons {
    /// Band a document may state: below one pixel an outline breaks up at 1x, and past
    /// two and a half it reads as a drawn shape, not a symbol.
    pub const STROKE_ADMITTED: std::ops::RangeInclusive<f32> = 1.0..=2.5;

    /// The design language's icons.
    pub const STOCK: Self = Self { stroke: 1.5 };
}

/// The UI face, the reading face and the body size every text role derives from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeType {
    pub face: String,
    pub reading: String,
    pub base_size: f32,
    /// "Always distinguish similar letters": set I, l, 1 and 0, O apart in all text, not
    /// only in identifiers, codes and links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_letters: Option<bool>,
    /// How far the role sizes stand apart: each role's size is the body size times its
    /// stock ratio to the body raised to this power.
    #[serde(default = "ThemeType::stock_contrast")]
    pub contrast: f32,
    #[serde(default)]
    pub weights: ThemeWeights,
}

/// A weight a theme may give a tier of text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeWeight {
    Regular,
    Medium,
    Semibold,
    Bold,
}

impl ThemeWeight {
    #[must_use]
    pub const fn text_weight(self) -> crate::text_render::TextWeight {
        use crate::text_render::TextWeight;
        match self {
            Self::Regular => TextWeight::Regular,
            Self::Medium => TextWeight::Medium,
            Self::Semibold => TextWeight::Semibold,
            Self::Bold => TextWeight::Bold,
        }
    }
}

/// The weights of the three tiers the type roles are set in: body text (body, caption,
/// reading, code), labels (control labels and row titles) and headings (display down to
/// subheading).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeWeights {
    pub body: ThemeWeight,
    pub label: ThemeWeight,
    pub heading: ThemeWeight,
}

impl ThemeWeights {
    /// The design language's weights: 400, 500, 600.
    pub const STOCK: Self = Self {
        body: ThemeWeight::Regular,
        label: ThemeWeight::Medium,
        heading: ThemeWeight::Semibold,
    };

    /// Whether the tiers keep their order: a heading is never lighter than a label, nor a
    /// label than body text, so hierarchy survives.
    #[must_use]
    pub fn ordered(&self) -> bool {
        let rank = |w: ThemeWeight| w.text_weight().to_u16();
        rank(self.body) <= rank(self.label) && rank(self.label) <= rank(self.heading)
    }
}

impl Default for ThemeWeights {
    fn default() -> Self {
        Self::STOCK
    }
}

impl ThemeType {
    /// The stock spread of role sizes.
    pub const STOCK_CONTRAST: f32 = 1.0;
    /// Band of role-size contrast a document may state: below it a title no longer reads
    /// larger than body text.
    pub const CONTRAST_ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=1.5;

    const fn stock_contrast() -> f32 {
        Self::STOCK_CONTRAST
    }

    pub const BASE_SIZE: f32 = 14.0;
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 10.0..=24.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 12.0..=18.0;

    #[must_use]
    pub fn default_reading() -> String {
        crate::tokens::ReadingFace::Default.name().into()
    }

    /// Whether this type sets every look-alike letter apart: the document's own choice,
    /// else on exactly when the face is Hyperlegible — the face people choose to tell
    /// letters apart.
    #[must_use]
    pub fn distinguishes_letters(&self) -> bool {
        self.distinct_letters.unwrap_or_else(|| {
            crate::tokens::UiFace::from_name(&self.face)
                == Some(crate::tokens::UiFace::Hyperlegible)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionCurve {
    Spring,
    Eased,
    None,
}

/// The motion of the shared UI: its curve, the character of its springs (`0` calm, `1`
/// the stock snappy motion, `2` lively) and how long timed transitions take.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMotion {
    pub character: f32,
    #[serde(default = "ThemeMotion::stock_curve")]
    pub curve: MotionCurve,
    #[serde(default = "ThemeMotion::stock_duration_scale")]
    pub duration_scale: f32,
}

impl ThemeMotion {
    /// Band a document may state, and is built at.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;
    /// Duration band a document may state, and is built at.
    pub const DURATION_ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=2.0;

    pub const STOCK: Self = Self {
        character: 1.0,
        curve: MotionCurve::Spring,
        duration_scale: 1.0,
    };

    const fn stock_curve() -> MotionCurve {
        MotionCurve::Spring
    }

    const fn stock_duration_scale() -> f32 {
        1.0
    }
}

/// What every surface role is made of ([`Finish`] per [`crate::tokens::SurfaceRole`]),
/// and the finish of selectors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMaterial {
    #[serde(default = "ThemeMaterial::stock_window")]
    pub window: Finish,
    #[serde(default = "ThemeMaterial::stock_control")]
    pub control: Finish,
    #[serde(default = "ThemeMaterial::stock_floating")]
    pub floating: Finish,
    #[serde(default = "ThemeMaterial::stock_bar")]
    pub bar: Option<Finish>,
    #[serde(default = "ThemeMaterial::stock_selector")]
    pub selector: SelectorFinish,
}

impl ThemeMaterial {
    /// The design language's materials.
    #[must_use]
    pub fn stock() -> Self {
        Self::of(RoleFinishes::stock(), SelectorFinish::STOCK)
    }

    /// Everything opaque, as high contrast paints.
    #[must_use]
    pub fn opaque() -> Self {
        Self::of(RoleFinishes::solid(), SelectorFinish::OPAQUE)
    }

    fn of(roles: RoleFinishes, selector: SelectorFinish) -> Self {
        let RoleFinishes {
            window,
            control,
            floating,
            bar,
        } = roles;
        Self {
            window,
            control,
            floating,
            bar,
            selector,
        }
    }

    fn stock_window() -> Finish {
        RoleFinishes::stock().window
    }

    fn stock_control() -> Finish {
        RoleFinishes::stock().control
    }

    fn stock_floating() -> Finish {
        RoleFinishes::stock().floating
    }

    fn stock_bar() -> Option<Finish> {
        RoleFinishes::stock().bar
    }

    const fn stock_selector() -> SelectorFinish {
        SelectorFinish::STOCK
    }

    #[must_use]
    pub fn roles(&self) -> RoleFinishes {
        RoleFinishes {
            window: self.window.clone(),
            control: self.control.clone(),
            floating: self.floating.clone(),
            bar: self.bar.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDocument {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub palette: Palette,
    #[serde(default)]
    pub emphasis: Emphasis,
    pub presence: Tone,
    pub shape: ThemeShape,
    pub density: ThemeDensity,
    #[serde(rename = "type")]
    pub type_: ThemeType,
    pub motion: ThemeMotion,
    pub material: ThemeMaterial,
    #[serde(default)]
    pub depth: Depth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response: Option<crate::tokens::Response>,
    /// How icons are drawn: the outline's width.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icons: Option<ThemeIcons>,
}

fn text_field_ok(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn within(value: f32, band: &std::ops::RangeInclusive<f32>) -> bool {
    value.is_finite() && band.contains(&value)
}

impl ThemeDocument {
    /// A document with the stock roles around `palette`.
    #[must_use]
    pub fn stock(id: &str, name: &str, palette: Palette) -> Self {
        let opaque = palette.high_contrast;
        Self {
            schema_version: THEME_SCHEMA_VERSION,
            id: id.into(),
            name: name.into(),
            palette,
            emphasis: Emphasis::stock_for(&palette),
            presence: palette.stock_presence(),
            shape: ThemeShape {
                window_radius: crate::tokens::Radii::WINDOW,
                border: Border::STOCK,
            },
            density: ThemeDensity {
                spacing: ThemeDensity::REGULAR,
            },
            type_: ThemeType {
                face: crate::text_render::UI_SANS_FAMILY.into(),
                reading: ThemeType::default_reading(),
                base_size: ThemeType::BASE_SIZE,
                distinct_letters: None,
                contrast: ThemeType::STOCK_CONTRAST,
                weights: ThemeWeights::STOCK,
            },
            motion: ThemeMotion::STOCK,
            material: if opaque {
                ThemeMaterial::opaque()
            } else {
                ThemeMaterial::stock()
            },
            depth: Depth::STOCK,
            response: None,
            icons: None,
        }
    }

    /// Structural check: the schema, the text fields, and every number finite and inside
    /// the band its role admits.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.schema_version != THEME_SCHEMA_VERSION {
            return Err("unsupported theme schema");
        }
        if !text_field_ok(&self.id) {
            return Err("invalid theme id");
        }
        if !text_field_ok(&self.name) {
            return Err("invalid theme name");
        }
        self.palette.validate()?;
        self.emphasis.validate()?;
        self.presence.validate_presence()?;
        if !within(self.shape.window_radius, &ThemeShape::ADMITTED) {
            return Err("window radius outside 0..=40");
        }
        if !within(self.density.spacing, &ThemeDensity::ADMITTED) {
            return Err("density outside 0.5..=2");
        }
        if !text_field_ok(&self.type_.face) {
            return Err("invalid type face");
        }
        if !text_field_ok(&self.type_.reading) {
            return Err("invalid reading face");
        }
        if !within(self.type_.base_size, &ThemeType::ADMITTED) {
            return Err("base type size outside 10..=24");
        }
        if !within(self.type_.contrast, &ThemeType::CONTRAST_ADMITTED) {
            return Err("type contrast outside 0.5..=1.5");
        }
        if !self.type_.weights.ordered() {
            return Err("type weights out of order: heading ≥ label ≥ body");
        }
        if !within(self.motion.character, &ThemeMotion::ADMITTED) {
            return Err("motion character outside 0..=2");
        }
        if !within(self.motion.duration_scale, &ThemeMotion::DURATION_ADMITTED) {
            return Err("motion duration scale outside 0.5..=2");
        }
        self.shape.border.validate()?;
        self.depth.validate()?;
        if let Some(response) = &self.response {
            response.validate()?;
        }
        if let Some(icons) = &self.icons
            && !within(icons.stroke, &ThemeIcons::STROKE_ADMITTED)
        {
            return Err("icon stroke outside 1..=2.5");
        }
        self.material.roles().validate()?;
        let selector = &self.material.selector;
        if !within(selector.rim_span, &SelectorFinish::RIM_SPAN_ADMITTED) {
            return Err("selector rim span outside 0..=1");
        }
        if !within(selector.rim_alpha, &SelectorFinish::RIM_ALPHA_ADMITTED) {
            return Err("selector rim opacity outside 0..=0.5");
        }
        if !within(selector.hover_alpha, &SelectorFinish::HOVER_ALPHA_ADMITTED) {
            return Err("selector hover opacity outside 0.7..=1");
        }
        Ok(())
    }

    /// Every contrast floor this document misses as authored, whoever wears it (each
    /// presence hue as the focus ring), worst ratio first per pair.
    #[must_use]
    pub fn findings(&self) -> Vec<ContrastFinding> {
        let mut worst: Vec<ContrastFinding> = Vec::new();
        for you in PresenceHue::ALL {
            let colors = Colors::as_authored(&self.palette, self.emphasis, self.presence, you);
            for finding in audit(&colors) {
                match worst
                    .iter_mut()
                    .find(|f| f.foreground == finding.foreground && f.ground == finding.ground)
                {
                    Some(seen) if seen.ratio > finding.ratio => *seen = finding,
                    Some(_) => {}
                    None => worst.push(finding),
                }
            }
        }
        let colors = Colors::as_authored(
            &self.palette,
            self.emphasis,
            self.presence,
            PresenceHue::Blue,
        );
        worst.extend(audit_materials(&colors, &self.material.roles()));
        worst
    }

    /// Whether ink is light on this document's surfaces.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        self.palette.dark
    }

    /// The same document as the high-contrast palette of its polarity: shape, density,
    /// type and motion stay; the palette, presence tone and emphasis become the AAA ones
    /// and the material turns opaque.
    #[must_use]
    pub fn in_high_contrast(&self) -> Self {
        let preset = if self.palette.dark {
            Preset::HighContrast
        } else {
            Preset::HighContrastLight
        };
        let stock = preset.document();
        Self {
            id: stock.id,
            name: stock.name,
            palette: stock.palette,
            emphasis: Emphasis::Inverted,
            presence: stock.presence,
            material: {
                let roles = self.material.roles().opaque();
                ThemeMaterial {
                    window: roles.window,
                    control: roles.control,
                    floating: roles.floating,
                    bar: roles.bar,
                    selector: SelectorFinish::OPAQUE,
                }
            },
            ..self.clone()
        }
    }
}

/// What a preset looks like: its palette, and the document built around it.
pub trait PresetDesign {
    #[must_use]
    fn palette(self) -> Palette;

    /// The preset's document: the stock roles around its palette, with its look's accent.
    #[must_use]
    fn document(self) -> ThemeDocument;
}

impl PresetDesign for Preset {
    fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette::dark(),
            Self::Light => Palette::light(),
            Self::PaperDark => Palette::paper_dark(),
            Self::PaperLight => Palette::paper_light(),
            Self::MossDark => Palette::moss_dark(),
            Self::MossLight => Palette::moss_light(),
            Self::DuskDark => Palette::dusk_dark(),
            Self::DuskLight => Palette::dusk_light(),
            Self::MonoDark => Palette::mono_dark(),
            Self::MonoLight => Palette::mono_light(),
            Self::HighContrast => Palette::high_contrast_dark(),
            Self::HighContrastLight => Palette::high_contrast_light(),
        }
    }

    fn document(self) -> ThemeDocument {
        let mut document = ThemeDocument::stock(self.id(), self.name(), self.palette());
        if let Some(look) = self.look() {
            document.emphasis = look.emphasis();
        }
        document
    }
}

/// The accent a look comes with.
pub trait LookDesign {
    /// The accent the look comes with, until the person picks one.
    #[must_use]
    fn emphasis(self) -> Emphasis;
}

impl LookDesign for Look {
    fn emphasis(self) -> Emphasis {
        match self {
            Self::Tilo => Emphasis::You,
            Self::Paper => Emphasis::Tinted { hue: 55.0 },
            Self::Moss => Emphasis::Tinted { hue: 85.0 },
            Self::Dusk => Emphasis::Tinted { hue: 325.0 },
            Self::Mono => Emphasis::Inverted,
        }
    }
}

/// Theme documents that exercise each surface finish (glass, block, pixelated, liquid
/// glass), for the finish painters' own tests: no shipped theme wears them.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::panic,
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]
pub(crate) fn finish_document(id: &str) -> ThemeDocument {
    let json = match id {
        "glass" => include_str!("../tests/fixtures/finishes/glass.json"),
        "glass-light" => include_str!("../tests/fixtures/finishes/glass-light.json"),
        "block" => include_str!("../tests/fixtures/finishes/block.json"),
        "block-light" => include_str!("../tests/fixtures/finishes/block-light.json"),
        "pixelated" => include_str!("../tests/fixtures/finishes/pixelated.json"),
        "liquid-glass" => include_str!("../tests/fixtures/finishes/liquid-glass.json"),
        "liquid-glass-light" => include_str!("../tests/fixtures/finishes/liquid-glass-light.json"),
        other => panic!("no finish fixture {other}"),
    };
    serde_json::from_str(json).expect("a finish fixture parses")
}

/// [`finish_document`] as the theme it builds.
#[cfg(any(test, feature = "test-support"))]
#[allow(
    clippy::expect_used,
    reason = "test support: failing the calling test is the point"
)]
pub fn finish_theme(id: &str) -> crate::theme::Theme {
    crate::theme::Theme::from_document(&finish_document(id)).expect("a finish fixture is usable")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tinted_emphasis_hue_keeps_every_floor_on_every_preset() {
        for preset in Preset::ALL {
            for step in 0..72_u16 {
                let mut doc = preset.document();
                doc.emphasis = Emphasis::Tinted {
                    hue: f32::from(step) * 5.0,
                };
                let findings = doc.findings();
                assert!(
                    findings.is_empty(),
                    "{} tinted {}: {}",
                    preset.name(),
                    f32::from(step) * 5.0,
                    findings
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
    }

    tilo_test_env::scenario! {
        /// A theme document through its JSON at this schema.
        /// Each phase was a test of its own and builds its own fixture.
        fn documents_round_trip_and_other_schemas_are_refused;

        phase a_document_round_trips_and_other_schemas_are_not_admitted;
        {
            let mut doc = Preset::DuskDark.document();
            doc.emphasis = Emphasis::Tinted { hue: 30.0 };
            let json = serde_json::to_value(&doc).unwrap();
            assert_eq!(json["type"]["base_size"], 14.0);
            assert_eq!(json["emphasis"]["kind"], "tinted");
            let back: ThemeDocument = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(back, doc);

            let mut old = doc.clone();
            old.schema_version = 4;
            assert_eq!(old.validate(), Err("unsupported theme schema"));
            let mut legacy = json;
            legacy["accent"] = serde_json::json!("#3b82f6");
            assert!(
                serde_json::from_value::<ThemeDocument>(legacy).is_err(),
                "roles this schema does not have are not admitted"
            );
        }

        phase the_liquid_glass_document_round_trips_with_its_lens;
        // The Liquid Glass document round-trips with its lens: the set's name and every
        // parameter it states come back, a band, light angle or dispersion out of its
        // admitted range is refused, and the theme built from it hands the lens to every
        // role's parts.
        {
            use crate::tokens::SurfaceRole;
            let mut doc = finish_document("liquid-glass");
            doc.material.floating = Finish::named(
                "liquid",
                [
                    ("tint", 0.2),
                    ("refraction", 10.0),
                    ("band", 16.0),
                    ("dispersion", 0.25),
                    ("light_angle", 60.0),
                ],
            )
            .unwrap();
            let json = serde_json::to_value(&doc).unwrap();
            assert_eq!(json["material"]["floating"]["painter"], "liquid");
            assert_eq!(json["material"]["floating"]["band"], 16.0);
            assert_eq!(json["material"]["floating"]["dispersion"], 0.25);
            let back: ThemeDocument = serde_json::from_value(json.clone()).unwrap();
            assert_eq!(back, doc);
            assert_eq!(back.validate(), Ok(()));
            let theme = crate::theme::Theme::from_document(&back).unwrap();
            for role in [
                SurfaceRole::Window,
                SurfaceRole::Control,
                SurfaceRole::Floating,
                SurfaceRole::Bar,
            ] {
                assert_eq!(theme.finish(role).painter(), "liquid", "{role:?}");
            }
            assert_eq!(
                theme.finish(SurfaceRole::Floating).param("band"),
                Some(16.0)
            );
            assert_eq!(
                theme.finish(SurfaceRole::Floating).param("light_angle"),
                Some(60.0)
            );
            for (name, value) in [("band", 80.0), ("light_angle", 400.0), ("dispersion", 0.9)] {
                let mut out = json.clone();
                out["material"]["floating"][name] = serde_json::json!(value);
                let refused = serde_json::from_value::<ThemeDocument>(out)
                    .map_or(true, |doc| doc.validate().is_err());
                assert!(refused, "{name} {value} was admitted");
            }
        }

        phase a_document_naming_only_the_old_roles_takes_the_stock_new_ones;
        {
            let json = serde_json::json!({
                "schema_version": THEME_SCHEMA_VERSION,
                "id": "terse", "name": "Terse",
                "palette": serde_json::to_value(Palette::dark()).unwrap(),
                "presence": { "lightness": 0.74, "chroma": 0.14 },
                "shape": { "window_radius": 14.0 },
                "density": { "spacing": 1.0 },
                "type": { "face": "Default", "reading": "Literata", "base_size": 14.0 },
                "motion": { "character": 1.0 },
                "material": {}
            });
            let doc: ThemeDocument = serde_json::from_value(json).unwrap();
            assert_eq!(doc.material, ThemeMaterial::stock());
            assert_eq!(doc.depth, Depth::STOCK);
            assert_eq!(doc.shape.border, Border::STOCK);
            assert_eq!(doc.motion, ThemeMotion::STOCK);
            assert_eq!(doc.type_.weights, ThemeWeights::STOCK);
            assert_eq!(doc.validate(), Ok(()));
        }
    }

    tilo_test_env::scenario! {
        /// What a theme document must hold to be admitted.
        /// Each phase was a test of its own and builds its own fixture.
        fn documents_are_valid_and_legible_or_refused_by_name;

        phase every_preset_is_a_valid_legible_document;
        {
            for preset in Preset::ALL {
                let doc = preset.document();
                assert_eq!(doc.validate(), Ok(()), "{}", preset.name());
                assert_eq!(doc.findings(), Vec::new(), "{}", preset.name());
                assert_eq!(Preset::find(preset.id()), Some(preset));
                assert_eq!(Preset::find(preset.name()), Some(preset));
            }
            assert_eq!(Preset::find("Nord"), None);
        }

        phase out_of_band_values_are_refused_and_failing_contrast_is_named;
        {
            let base = Preset::Light.document();
            for (field, edit) in [
                (
                    "radius",
                    Box::new(|d: &mut ThemeDocument| d.shape.window_radius = 80.0)
                        as Box<dyn Fn(&mut ThemeDocument)>,
                ),
                (
                    "density",
                    Box::new(|d: &mut ThemeDocument| d.density.spacing = 0.1),
                ),
                (
                    "size",
                    Box::new(|d: &mut ThemeDocument| d.type_.base_size = f32::NAN),
                ),
                (
                    "face",
                    Box::new(|d: &mut ThemeDocument| d.type_.face = " ".into()),
                ),
                (
                    "motion",
                    Box::new(|d: &mut ThemeDocument| d.motion.character = 3.0),
                ),
                (
                    "tint",
                    Box::new(|d: &mut ThemeDocument| {
                        d.material.floating = Finish::frosted(24.0, 1.4, 1.0);
                    }),
                ),
                (
                    "glass",
                    Box::new(|d: &mut ThemeDocument| {
                        d.material.window = Finish::glass([24.0, 0.4, 1.0, 90.0, 0.2, 0.1]);
                    }),
                ),
                (
                    "border",
                    Box::new(|d: &mut ThemeDocument| d.shape.border.width = 12.0),
                ),
                (
                    "depth",
                    Box::new(|d: &mut ThemeDocument| d.depth = Depth::Shadow { strength: -1.0 }),
                ),
                (
                    "type contrast",
                    Box::new(|d: &mut ThemeDocument| d.type_.contrast = 4.0),
                ),
                (
                    "weights",
                    Box::new(|d: &mut ThemeDocument| {
                        d.type_.weights.heading = ThemeWeight::Regular;
                        d.type_.weights.label = ThemeWeight::Bold;
                    }),
                ),
                (
                    "duration",
                    Box::new(|d: &mut ThemeDocument| d.motion.duration_scale = 0.0),
                ),
                (
                    "hover",
                    Box::new(|d: &mut ThemeDocument| d.material.selector.hover_alpha = 0.3),
                ),
                (
                    "rim",
                    Box::new(|d: &mut ThemeDocument| d.material.selector.rim_alpha = f32::NAN),
                ),
                (
                    "chroma",
                    Box::new(|d: &mut ThemeDocument| d.palette.chroma = 0.2),
                ),
            ] {
                let mut doc = base.clone();
                edit(&mut doc);
                assert!(doc.validate().is_err(), "{field} must be refused");
            }

            let mut murky = base;
            murky.palette.ink.tertiary = 0.80;
            assert_eq!(murky.validate(), Ok(()), "structurally fine");
            let findings = murky.findings();
            assert!(
                findings
                    .iter()
                    .any(|f| f.foreground == crate::tokens::contrast::Role::TextTertiary),
                "{findings:?}"
            );
        }

        phase high_contrast_keeps_shape_density_type_and_motion;
        {
            let mut doc = Preset::Light.document();
            doc.shape.window_radius = 6.0;
            doc.density.spacing = 1.2;
            doc.emphasis = Emphasis::Tinted { hue: 30.0 };
            let hc = doc.in_high_contrast();
            assert!(hc.palette.high_contrast && !hc.palette.dark);
            assert_eq!(hc.name, "High contrast light");
            assert_eq!(hc.emphasis, Emphasis::Inverted);
            assert!((hc.shape.window_radius - 6.0).abs() < f32::EPSILON);
            assert!((hc.density.spacing - 1.2).abs() < f32::EPSILON);
            assert_eq!(hc.material, ThemeMaterial::opaque());
            assert_eq!(hc.material.selector, SelectorFinish::OPAQUE);
            assert_eq!(hc.findings(), Vec::new());
        }

        phase every_look_is_designed_twice_and_its_sides_differ;
        {
            for look in Look::ALL {
                let dark = look.preset(true);
                let light = look.preset(false);
                assert_eq!(dark.look(), Some(look));
                assert_eq!(light.look(), Some(look));
                assert!(dark.document().is_dark(), "{}", dark.name());
                assert!(!light.document().is_dark(), "{}", light.name());
                assert_eq!(dark.document().emphasis, look.emphasis());
                assert_eq!(Look::find(look.name()), Some(look));
                assert_eq!(Preset::find(look.id()), Some(dark));
            }
            // Two looks of one polarity are told apart at a glance: their desktop and
            // their windows together differ by more than a rounding step. (Light windows
            // are all near white; the desktop around them carries the temperature.)
            for dark in [true, false] {
                let grounds: Vec<_> = Look::ALL
                    .iter()
                    .map(|look| {
                        let surfaces =
                            crate::theme::Theme::preset(look.preset(dark)).tokens.colors.surfaces;
                        [surfaces.base.to_rgba8(), surfaces.content.to_rgba8()]
                    })
                    .collect();
                for (i, a) in grounds.iter().enumerate() {
                    for b in grounds.iter().skip(i + 1) {
                        let distance: u32 = a
                            .iter()
                            .zip(b)
                            .map(|(a, b)| {
                                i32::from(a.r).abs_diff(i32::from(b.r))
                                    + i32::from(a.g).abs_diff(i32::from(b.g))
                                    + i32::from(a.b).abs_diff(i32::from(b.b))
                            })
                            .sum();
                        assert!(distance >= 8, "{a:?} and {b:?} are too alike");
                    }
                }
            }
            for preset in Preset::ALL {
                assert_eq!(preset.document().icons, None, "{}", preset.name());
            }
            let pixelated = finish_document("pixelated");
            assert_eq!(pixelated.icons, Some(ThemeIcons { stroke: 2.0 }));
            assert!((finish_theme("pixelated").tokens.icon_stroke_px - 2.0).abs() < f32::EPSILON);
            let glass = finish_document("glass");
            for role in [
                glass.material.window,
                glass.material.control,
                glass.material.floating,
            ] {
                assert_eq!(role.painter(), "glass", "{role:?}");
            }
            let block = finish_document("block");
            assert_eq!(block.shape.window_radius, 0.0);
            assert_eq!(block.depth, Depth::None);
            assert_eq!(block.motion.curve, MotionCurve::None);
            assert!(block.shape.border.outlines());
            assert_eq!(block.material.roles().opaque(), block.material.roles());
        }

        phase the_backdrop_band_narrows_to_what_the_ink_can_afford;
        {
            use crate::tokens::SurfaceRole;
            use crate::tokens::finish::{MATERIAL_SPREAD, material_spread, worst_grounds};
            let mut doc = Preset::Dark.document();
            doc.palette.ink.secondary = 0.64;
            doc.material.window = Finish::glass([24.0, 0.0, 1.0, 8.0, 0.2, 0.1]);
            assert_eq!(doc.validate(), Ok(()));
            let colors =
                Colors::as_authored(&doc.palette, doc.emphasis, doc.presence, PresenceHue::Blue);
            let spread = material_spread(&colors, colors.surfaces.content);
            assert!((1.0..MATERIAL_SPREAD).contains(&spread), "{spread}");
            for ground in worst_grounds(&colors, &doc.material.roles(), SurfaceRole::Window) {
                for (ink, floor) in [(colors.text_dim, 4.5), (colors.text_tertiary, 3.0)] {
                    let ratio = crate::color::contrast_ratio(ink, ground);
                    assert!(ratio >= floor - 1e-6, "{ratio} under {floor}");
                }
            }
            assert!(
                !doc.findings()
                    .iter()
                    .any(|f| matches!(f.ground, crate::tokens::contrast::Role::Material(_))),
                "no ink misses its floor on the material"
            );
            let stock = Colors::as_authored(
                &Palette::dark(),
                Emphasis::You,
                Palette::dark().stock_presence(),
                PresenceHue::Blue,
            );
            assert!(
                (material_spread(&stock, stock.surfaces.overlay) - MATERIAL_SPREAD).abs() < 1e-9,
                "Default's floating material keeps its band"
            );
        }
    }
}
