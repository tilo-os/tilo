//! The theme document: everything a theme may say, as data
//! (`docs/design-language.md`, "Theming"; `docs/architecture/theming.md`).
//!
//! Built-in presets, distribution defaults, the user's theme and every
//! theme Theme Studio writes are one [`ThemeDocument`]. It names semantic
//! roles only — a palette, an emphasis, the presence tone, the shape
//! (window radius and border), density, type, motion, what each surface
//! role is made of and how surfaces stand off what is below them — and
//! never a widget, so it cannot restyle an app's internals, remove the lift
//! or the focus ring, or break a contrast floor that [`ThemeDocument::findings`]
//! measures and the runtime repairs.
//!
//! A document of any other schema version is not admitted: the layer it
//! sits on contributes nothing and the layers below it apply.

use serde::{Deserialize, Serialize};

use crate::tokens::colors::{Colors, PresenceHue};
use crate::tokens::contrast::{ContrastFinding, audit, audit_materials};
use crate::tokens::finish::{Border, Depth, Finish, RoleFinishes};
use crate::tokens::palette::{Emphasis, Palette, Tone};
use crate::tokens::selector::SelectorFinish;

/// The one schema this build reads and writes.
pub const THEME_SCHEMA_VERSION: u32 = 8;

/// The window tile's radius, from which every other radius derives
/// concentrically (`crate::tokens::Radii::from_window_radius`; `0` makes
/// every corner square), and how surfaces are outlined.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeShape {
    pub window_radius: f32,
    /// The outline every surface carries ([`Border`]); the stock border
    /// keeps the design language's hairlines.
    #[serde(default)]
    pub border: Border,
}

impl ThemeShape {
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=40.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 0.0..=28.0;
}

/// The spacing scale, as a multiple of the stock 4 px scale. Pointer
/// targets and control heights stay put.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDensity {
    pub spacing: f32,
}

impl ThemeDensity {
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=2.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 0.75..=1.5;
    /// The compact density Settings offers.
    pub const COMPACT: f32 = 0.85;
    /// The stock density.
    pub const REGULAR: f32 = 1.0;
    /// The roomy density Settings offers.
    pub const ROOMY: f32 = 1.2;
}

/// The UI face, the reading face and the body size every text role derives
/// from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeType {
    pub face: String,
    /// The Reading role's face ([`crate::tokens::ReadingFace`]), by name.
    pub reading: String,
    pub base_size: f32,
    /// "Always distinguish similar letters": set I, l, 1 and 0, O apart in
    /// all text, not only in identifiers, codes and links. `None` follows
    /// the face: on with Hyperlegible, off otherwise
    /// ([`Self::distinguishes_letters`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distinct_letters: Option<bool>,
    /// How far the role sizes stand apart: each role's size is the body
    /// size times its stock ratio to the body raised to this power. `1` is
    /// the design language's scale, below it titles come closer to body
    /// text, above it they stand further off.
    #[serde(default = "ThemeType::stock_contrast")]
    pub contrast: f32,
    /// The weight of each weight tier.
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
    /// The renderer's weight.
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

/// The weights of the three tiers the type roles are set in: body text
/// (body, caption, reading, code), labels (control labels and row titles)
/// and headings (display down to subheading).
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

    /// Whether the tiers keep their order: a heading is never lighter than
    /// a label, nor a label than body text, so hierarchy survives.
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
    /// Band of role-size contrast a document may state: below it a title
    /// no longer reads larger than body text.
    pub const CONTRAST_ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=1.5;

    const fn stock_contrast() -> f32 {
        Self::STOCK_CONTRAST
    }

    /// The stock body size.
    pub const BASE_SIZE: f32 = 14.0;
    /// Band a document may state.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 10.0..=24.0;
    /// Band a theme is built at; a value outside it is clamped.
    pub const USABLE: std::ops::RangeInclusive<f32> = 12.0..=18.0;

    /// The stock reading face.
    #[must_use]
    pub fn default_reading() -> String {
        crate::tokens::ReadingFace::Default.name().into()
    }

    /// Whether this type sets every look-alike letter apart: the
    /// document's own choice, else on exactly when the face is Hyperlegible
    /// — the face people choose to tell letters apart.
    #[must_use]
    pub fn distinguishes_letters(&self) -> bool {
        self.distinct_letters.unwrap_or_else(|| {
            crate::tokens::UiFace::from_name(&self.face)
                == Some(crate::tokens::UiFace::Hyperlegible)
        })
    }
}

/// How the shared UI moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MotionCurve {
    /// Springs, at the document's `character`: the stock motion.
    Spring,
    /// Critically damped: every movement eases in and settles with no
    /// overshoot, whatever its character.
    Eased,
    /// Nothing moves and nothing fades: every change lands at once.
    None,
}

/// The motion of the shared UI: its curve, the character of its springs
/// (`0` calm, `1` the stock snappy motion, `2` lively) and how long timed
/// transitions take. Reduced motion still replaces movement with a fade.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMotion {
    pub character: f32,
    #[serde(default = "ThemeMotion::stock_curve")]
    pub curve: MotionCurve,
    /// Every timed transition's duration, as a multiple of the stock one.
    #[serde(default = "ThemeMotion::stock_duration_scale")]
    pub duration_scale: f32,
}

impl ThemeMotion {
    /// Band a document may state, and is built at.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;
    /// Duration band a document may state, and is built at.
    pub const DURATION_ADMITTED: std::ops::RangeInclusive<f32> = 0.5..=2.0;

    /// The stock motion.
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

/// What every surface role is made of ([`Finish`] per
/// [`crate::tokens::SurfaceRole`]), and the finish of selectors. A role a
/// document leaves out keeps its stock finish.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMaterial {
    /// The window ground every app paints on.
    #[serde(default = "ThemeMaterial::stock_window")]
    pub window: Finish,
    /// Raised controls: buttons, fields, tracks, cards.
    #[serde(default = "ThemeMaterial::stock_control")]
    pub control: Finish,
    /// Floating chrome: menus, popovers, the palette, sheets, notifications.
    #[serde(default = "ThemeMaterial::stock_floating")]
    pub floating: Finish,
    /// The top bar's body; absent, the bar stands on the desktop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bar: Option<Finish>,
    /// What the window group's tab strip stands on, and the lit rim and
    /// hover film of the selectors in it and everywhere else.
    #[serde(default = "ThemeMaterial::stock_selector")]
    pub selector: SelectorFinish,
}

impl ThemeMaterial {
    /// The design language's materials.
    pub const STOCK: Self = Self {
        window: RoleFinishes::STOCK.window,
        control: RoleFinishes::STOCK.control,
        floating: RoleFinishes::STOCK.floating,
        bar: RoleFinishes::STOCK.bar,
        selector: SelectorFinish::STOCK,
    };

    /// Everything opaque, as high contrast paints.
    pub const OPAQUE: Self = Self {
        window: RoleFinishes::OPAQUE.window,
        control: RoleFinishes::OPAQUE.control,
        floating: RoleFinishes::OPAQUE.floating,
        bar: RoleFinishes::OPAQUE.bar,
        selector: SelectorFinish::OPAQUE,
    };

    const fn stock_window() -> Finish {
        Self::STOCK.window
    }

    const fn stock_control() -> Finish {
        Self::STOCK.control
    }

    const fn stock_floating() -> Finish {
        Self::STOCK.floating
    }

    const fn stock_selector() -> SelectorFinish {
        Self::STOCK.selector
    }

    /// The finish of every role.
    #[must_use]
    pub const fn roles(&self) -> RoleFinishes {
        RoleFinishes {
            window: self.window,
            control: self.control,
            floating: self.floating,
            bar: self.bar,
        }
    }
}

/// A theme, as data. See the module documentation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeDocument {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub palette: Palette,
    #[serde(default)]
    pub emphasis: Emphasis,
    /// Lightness and chroma of the twelve presence colours.
    pub presence: Tone,
    pub shape: ThemeShape,
    pub density: ThemeDensity,
    #[serde(rename = "type")]
    pub type_: ThemeType,
    pub motion: ThemeMotion,
    pub material: ThemeMaterial,
    /// How surfaces stand off what is below them.
    #[serde(default)]
    pub depth: Depth,
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
                ThemeMaterial::OPAQUE
            } else {
                ThemeMaterial::STOCK
            },
            depth: Depth::STOCK,
        }
    }

    /// Structural check: the schema, the text fields, and every number
    /// finite and inside the band its role admits. A document that fails is
    /// not a theme. Legibility is [`Self::findings`].
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

    /// Every contrast floor this document misses as authored, whoever wears
    /// it (each presence hue as the focus ring), worst ratio first per pair.
    /// Theme Studio refuses to save a document with findings; the runtime
    /// repairs one that arrives anyway.
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

    /// The same document as the high-contrast palette of its polarity:
    /// shape, density, type and motion stay; the palette, presence tone and
    /// emphasis become the AAA ones and the material turns opaque.
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

/// The themes Tilo ships.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Preset {
    /// The default: pure neutral dark.
    Dark,
    DarkCool,
    DarkGraphite,
    DarkSoft,
    Light,
    HighContrast,
    /// Light, every text pairing at WCAG AAA.
    HighContrastLight,
    /// Everything glass: translucent windows, controls, bars and floating
    /// chrome that bend and catch the light. A plain theme document
    /// (`themes/glass.json`).
    Glass,
    /// [`Self::Glass`] on the light ladder (`themes/glass-light.json`).
    GlassLight,
    /// Everything flat and square: opaque, bordered, no shadows, no motion
    /// (`themes/block.json`).
    Block,
    /// [`Self::Block`] on the light ladder (`themes/block-light.json`).
    BlockLight,
}

/// The shipped themes that are plain documents, by preset.
const SHIPPED_DOCUMENTS: [(Preset, &str); 4] = [
    (Preset::Glass, include_str!("../themes/glass.json")),
    (
        Preset::GlassLight,
        include_str!("../themes/glass-light.json"),
    ),
    (Preset::Block, include_str!("../themes/block.json")),
    (
        Preset::BlockLight,
        include_str!("../themes/block-light.json"),
    ),
];

impl Preset {
    pub const ALL: [Self; 11] = [
        Self::Dark,
        Self::DarkCool,
        Self::DarkGraphite,
        Self::DarkSoft,
        Self::Light,
        Self::HighContrast,
        Self::HighContrastLight,
        Self::Glass,
        Self::GlassLight,
        Self::Block,
        Self::BlockLight,
    ];

    /// The stored id (`appearance.dark_theme`, `appearance.light_theme`).
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::DarkCool => "dark-cool",
            Self::DarkGraphite => "dark-graphite",
            Self::DarkSoft => "dark-soft",
            Self::Light => "light",
            Self::HighContrast => "high-contrast",
            Self::HighContrastLight => "high-contrast-light",
            Self::Glass => "glass",
            Self::GlassLight => "glass-light",
            Self::Block => "block",
            Self::BlockLight => "block-light",
        }
    }

    /// The name a person reads.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Dark => "Dark",
            Self::DarkCool => "Dark cool",
            Self::DarkGraphite => "Dark graphite",
            Self::DarkSoft => "Dark soft",
            Self::Light => "Light",
            Self::HighContrast => "High contrast",
            Self::HighContrastLight => "High contrast light",
            Self::Glass => "Glass",
            Self::GlassLight => "Glass light",
            Self::Block => "Block",
            Self::BlockLight => "Block light",
        }
    }

    #[must_use]
    pub const fn palette(self) -> Palette {
        match self {
            Self::Dark | Self::Glass | Self::Block => Palette::dark(),
            Self::DarkCool => Palette::dark_cool(),
            Self::DarkGraphite => Palette::dark_graphite(),
            Self::DarkSoft => Palette::dark_soft(),
            Self::Light | Self::GlassLight | Self::BlockLight => Palette::light(),
            Self::HighContrast => Palette::high_contrast_dark(),
            Self::HighContrastLight => Palette::high_contrast_light(),
        }
    }

    /// The preset's document: the stock roles around its palette, or for a
    /// shipped document its file, parsed once.
    #[must_use]
    pub fn document(self) -> ThemeDocument {
        static PARSED: std::sync::LazyLock<Vec<(Preset, ThemeDocument)>> = std::sync::LazyLock::new(
            || {
                SHIPPED_DOCUMENTS
                    .iter()
                    .filter_map(|(preset, json)| {
                        match serde_json::from_str::<ThemeDocument>(json) {
                            Ok(document) => Some((*preset, document)),
                            Err(error) => {
                                // `shipped_documents_are_valid_and_legible`
                                // keeps this from ever reaching a build.
                                tracing::error!(preset = preset.id(), %error, "a shipped theme document does not parse");
                                None
                            }
                        }
                    })
                    .collect()
            },
        );
        PARSED
            .iter()
            .find(|(preset, _)| *preset == self)
            .map_or_else(
                || ThemeDocument::stock(self.id(), self.name(), self.palette()),
                |(_, document)| document.clone(),
            )
    }

    /// A preset by id or name, loosely: case, spaces, hyphens and
    /// underscores are ignored (`"high-contrast"`, `"High contrast"`).
    #[must_use]
    pub fn find(key: &str) -> Option<Self> {
        let key: String = key
            .chars()
            .filter(|c| c.is_alphanumeric())
            .flat_map(char::to_lowercase)
            .collect();
        Some(match key.as_str() {
            "dark" => Self::Dark,
            "darkcool" | "cool" => Self::DarkCool,
            "darkgraphite" | "graphite" | "darkwarm" | "warm" => Self::DarkGraphite,
            "darksoft" | "soft" => Self::DarkSoft,
            "light" => Self::Light,
            "highcontrast" | "contrast" | "highcontrastdark" => Self::HighContrast,
            "highcontrastlight" => Self::HighContrastLight,
            "glass" | "glassdark" => Self::Glass,
            "glasslight" => Self::GlassLight,
            "block" | "blockdark" => Self::Block,
            "blocklight" => Self::BlockLight,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_a_valid_legible_document() {
        for preset in Preset::ALL {
            let doc = preset.document();
            assert_eq!(doc.validate(), Ok(()), "{}", preset.name());
            assert_eq!(doc.findings(), Vec::new(), "{}", preset.name());
            assert_eq!(Preset::find(preset.id()), Some(preset));
            assert_eq!(Preset::find(preset.name()), Some(preset));
        }
        assert_eq!(Preset::find("Nord"), None);
    }

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

    #[test]
    fn a_document_round_trips_and_other_schemas_are_not_admitted() {
        let mut doc = Preset::DarkSoft.document();
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

    #[test]
    fn out_of_band_values_are_refused_and_failing_contrast_is_named() {
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
                    d.material.floating = Finish::Translucent {
                        blur: 24.0,
                        tint: 1.4,
                        saturation: 1.0,
                    };
                }),
            ),
            (
                "glass",
                Box::new(|d: &mut ThemeDocument| {
                    d.material.window = Finish::Glass {
                        blur: 24.0,
                        tint: 0.4,
                        saturation: 1.0,
                        refraction: 90.0,
                        edge_highlight: 0.2,
                        specular: 0.1,
                    };
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

    #[test]
    fn high_contrast_keeps_shape_density_type_and_motion() {
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
        assert_eq!(hc.material, ThemeMaterial::OPAQUE);
        assert_eq!(hc.material.selector, SelectorFinish::OPAQUE);
        assert_eq!(hc.findings(), Vec::new());
    }

    #[test]
    fn shipped_documents_are_valid_and_legible_and_parse_from_their_files() {
        for (preset, json) in SHIPPED_DOCUMENTS {
            let parsed: ThemeDocument = serde_json::from_str(json).unwrap();
            assert_eq!(parsed.id, preset.id());
            assert_eq!(parsed.name, preset.name());
            assert_eq!(parsed.palette, preset.palette(), "{}", preset.name());
            assert_eq!(preset.document(), parsed);
        }
        let glass = Preset::Glass.document();
        for role in [
            glass.material.window,
            glass.material.control,
            glass.material.floating,
        ] {
            assert!(matches!(role, Finish::Glass { .. }), "{role:?}");
        }
        let block = Preset::Block.document();
        assert_eq!(block.shape.window_radius, 0.0);
        assert_eq!(block.depth, Depth::None);
        assert_eq!(block.motion.curve, MotionCurve::None);
        assert!(block.shape.border.outlines());
        assert_eq!(block.material.roles().opaque(), block.material.roles());
    }

    #[test]
    fn every_material_round_trips_on_a_whole_document() {
        let glass = Finish::Glass {
            blur: 20.0,
            tint: 0.2,
            saturation: 1.8,
            refraction: 12.0,
            edge_highlight: 0.4,
            specular: 0.2,
        };
        let translucent = Finish::Translucent {
            blur: 30.0,
            tint: 0.4,
            saturation: 0.5,
        };
        for finish in [Finish::Flat, Finish::Solid, translucent, glass] {
            let mut doc = Preset::Dark.document();
            doc.material.window = finish;
            doc.material.control = finish;
            doc.material.floating = finish;
            doc.material.bar = Some(finish);
            doc.shape.border = Border {
                width: 2.0,
                ink: crate::tokens::BorderInk::Standard,
            };
            doc.depth = Depth::Outline;
            doc.type_.contrast = 1.25;
            doc.type_.weights = ThemeWeights {
                body: ThemeWeight::Medium,
                label: ThemeWeight::Semibold,
                heading: ThemeWeight::Bold,
            };
            doc.motion = ThemeMotion {
                character: 0.5,
                curve: MotionCurve::Eased,
                duration_scale: 1.5,
            };
            assert_eq!(doc.validate(), Ok(()), "{finish:?}");
            let json = serde_json::to_string(&doc).unwrap();
            let back: ThemeDocument = serde_json::from_str(&json).unwrap();
            assert_eq!(back, doc, "{finish:?}");
            assert_eq!(doc.findings(), Vec::new(), "{finish:?}");
        }
    }

    #[test]
    fn a_document_naming_only_the_old_roles_takes_the_stock_new_ones() {
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
        assert_eq!(doc.material, ThemeMaterial::STOCK);
        assert_eq!(doc.depth, Depth::STOCK);
        assert_eq!(doc.shape.border, Border::STOCK);
        assert_eq!(doc.motion, ThemeMotion::STOCK);
        assert_eq!(doc.type_.weights, ThemeWeights::STOCK);
        assert_eq!(doc.validate(), Ok(()));
    }

    #[test]
    fn the_backdrop_band_narrows_to_what_the_ink_can_afford() {
        use crate::tokens::SurfaceRole;
        use crate::tokens::finish::{MATERIAL_SPREAD, material_spread, worst_grounds};
        // Ink that clears its floor on the opaque ground with little to
        // spare: the band a translucent window shows through narrows so it
        // still clears it over the darkest and the lightest backdrop.
        let mut doc = Preset::Dark.document();
        doc.palette.ink.secondary = 0.64;
        doc.material.window = Finish::Glass {
            blur: 24.0,
            tint: 0.0,
            saturation: 1.0,
            refraction: 8.0,
            edge_highlight: 0.2,
            specular: 0.1,
        };
        assert_eq!(doc.validate(), Ok(()));
        let colors =
            Colors::as_authored(&doc.palette, doc.emphasis, doc.presence, PresenceHue::Blue);
        let spread = material_spread(&colors, colors.surfaces.content);
        assert!(spread < MATERIAL_SPREAD && spread >= 1.0, "{spread}");
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
        // The stock dark ink affords the whole band.
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
