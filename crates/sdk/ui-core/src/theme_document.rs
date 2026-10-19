//! The theme document: everything a theme may say, as data
//! (`docs/design-language.md`, "Theming"; `docs/architecture/theming.md`).
//!
//! Built-in presets, distribution defaults, the user's theme and every
//! theme Theme Studio writes are one [`ThemeDocument`]. It names semantic
//! roles only — a palette, an emphasis, the presence tone, the window
//! radius, density, type, motion character and the floating material — and
//! never a widget, so it cannot restyle an app's internals, remove the lift
//! or the focus ring, or break a contrast floor that [`ThemeDocument::findings`]
//! measures and the runtime repairs.
//!
//! A document of any other schema version is not admitted: the layer it
//! sits on contributes nothing and the layers below it apply.

use serde::{Deserialize, Serialize};

use crate::tokens::colors::{Colors, PresenceHue};
use crate::tokens::contrast::{ContrastFinding, audit};
use crate::tokens::palette::{Emphasis, Palette, Tone};
use crate::tokens::selector::SelectorFinish;

/// The one schema this build reads and writes.
pub const THEME_SCHEMA_VERSION: u32 = 7;

/// The window tile's radius. Every other radius derives from it
/// concentrically (`crate::tokens::Radii::from_window_radius`).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeShape {
    pub window_radius: f32,
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
}

impl ThemeType {
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

/// The character of the shared UI springs: `0` calm, `1` the stock snappy
/// motion, `2` lively. Reduced motion still replaces movement with a fade.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMotion {
    pub character: f32,
}

impl ThemeMotion {
    /// Band a document may state, and is built at.
    pub const ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=2.0;
}

/// The floating material: how strongly the backdrop behind floating chrome
/// is blurred, how opaque the overlay tint over it is, and the finish of
/// selectors, whose docked strip is made of it.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeMaterial {
    /// Backdrop blur, in logical pixels. `0` keeps floating chrome opaque.
    pub blur: f32,
    /// Opacity of the overlay tint over the blurred backdrop.
    pub tint_opacity: f32,
    /// What the window group's tab strip stands on, and the lit rim and
    /// hover film of the selectors in it and everywhere else.
    pub selector: SelectorFinish,
}

impl ThemeMaterial {
    /// Blur band a document may state.
    pub const BLUR_ADMITTED: std::ops::RangeInclusive<f32> = 0.0..=64.0;
    /// Blur band a theme is built at.
    pub const BLUR_USABLE: std::ops::RangeInclusive<f32> = 0.0..=48.0;
    /// Tint opacity band: below 0.6 text on a busy backdrop stops reading.
    pub const TINT_ADMITTED: std::ops::RangeInclusive<f32> = 0.6..=1.0;
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
            },
            density: ThemeDensity {
                spacing: ThemeDensity::REGULAR,
            },
            type_: ThemeType {
                face: crate::text_render::UI_SANS_FAMILY.into(),
                reading: ThemeType::default_reading(),
                base_size: ThemeType::BASE_SIZE,
                distinct_letters: None,
            },
            motion: ThemeMotion { character: 1.0 },
            material: ThemeMaterial {
                blur: if opaque {
                    0.0
                } else {
                    crate::tokens::colors::MATERIAL_BLUR_PX
                },
                tint_opacity: if opaque {
                    1.0
                } else {
                    crate::tokens::colors::MATERIAL_ALPHA
                },
                selector: if opaque {
                    SelectorFinish::OPAQUE
                } else {
                    SelectorFinish::STOCK
                },
            },
        }
    }

    /// Structural check: the schema, the text fields, and every number
    /// finite and inside the band its role admits. A document that fails is
    /// not a theme. Legibility is [`Self::findings`].
    ///
    /// # Errors
    ///
    /// A static message naming the first field that is out of range.
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
        if !within(self.motion.character, &ThemeMotion::ADMITTED) {
            return Err("motion character outside 0..=2");
        }
        if !within(self.material.blur, &ThemeMaterial::BLUR_ADMITTED) {
            return Err("material blur outside 0..=64");
        }
        if !within(self.material.tint_opacity, &ThemeMaterial::TINT_ADMITTED) {
            return Err("material tint opacity outside 0.6..=1");
        }
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
            material: stock.material,
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
    /// Dark, every text pairing at WCAG AAA.
    HighContrast,
    /// Light, every text pairing at WCAG AAA.
    HighContrastLight,
}

impl Preset {
    pub const ALL: [Self; 7] = [
        Self::Dark,
        Self::DarkCool,
        Self::DarkGraphite,
        Self::DarkSoft,
        Self::Light,
        Self::HighContrast,
        Self::HighContrastLight,
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
        }
    }

    #[must_use]
    pub const fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette::dark(),
            Self::DarkCool => Palette::dark_cool(),
            Self::DarkGraphite => Palette::dark_graphite(),
            Self::DarkSoft => Palette::dark_soft(),
            Self::Light => Palette::light(),
            Self::HighContrast => Palette::high_contrast_dark(),
            Self::HighContrastLight => Palette::high_contrast_light(),
        }
    }

    #[must_use]
    pub fn document(self) -> ThemeDocument {
        ThemeDocument::stock(self.id(), self.name(), self.palette())
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
                Box::new(|d: &mut ThemeDocument| d.material.tint_opacity = 0.2),
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
        assert!(hc.material.blur.abs() < f32::EPSILON);
        assert_eq!(hc.material.selector, SelectorFinish::OPAQUE);
        assert_eq!(hc.findings(), Vec::new());
    }
}
