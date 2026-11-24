//! A theme as the person's appearance record holds it: every role a field with its own
//! address (`settings/appearance/themes/<id>/surfaces/base`), so a person's controls, an
//! agent and a script change a theme by the same edits, with history and undo. The roles
//! the fields do not name (each finish's painter and shader, borders, depth, the reading
//! face, the motion curve) are kept from the document the theme was made from (`origin`),
//! so a theme round-trips whole. `docs/designs/theme-resource.md`.

use serde::{Deserialize, Serialize};
use tilo_resource::{Model, Refusal};

use crate::theme::{
    Preset, PresetDesign, THEME_SCHEMA_VERSION, ThemeDensity, ThemeDocument, ThemeMaterial,
    ThemeMotion, ThemeShape, ThemeType,
};
use crate::tokens::{
    Emphasis, InkLadder, Palette, SelectorFinish, StatusTones, StripMaterial, SurfaceLadder, Tone,
};

/// OKLCH lightness of every surface, `0` black to `1` white.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Surfaces {
    /// Desktop
    pub base: f64,
    /// Window
    pub content: f64,
    /// Sidebar
    pub sidebar: f64,
    /// Inspector
    pub inspector: f64,
    /// Raised
    pub raised: f64,
    /// Floating chrome
    pub overlay: f64,
}

impl Surfaces {
    /// Every rung's lightness, the desktop's first.
    fn lightnesses(self) -> [f64; 6] {
        let Self {
            base,
            content,
            sidebar,
            inspector,
            raised,
            overlay,
        } = self;
        [base, content, sidebar, inspector, raised, overlay]
    }

    /// The ladder a palette is built from.
    fn ladder(self) -> SurfaceLadder {
        let [base, content, sidebar, inspector, raised, overlay] = self.lightnesses().map(narrow);
        SurfaceLadder {
            base,
            content,
            sidebar,
            inspector,
            raised,
            overlay,
        }
    }
}

impl From<SurfaceLadder> for Surfaces {
    fn from(ladder: SurfaceLadder) -> Self {
        let SurfaceLadder {
            base,
            content,
            sidebar,
            inspector,
            raised,
            overlay,
        } = ladder;
        Self {
            base: f64::from(base),
            content: f64::from(content),
            sidebar: f64::from(sidebar),
            inspector: f64::from(inspector),
            raised: f64::from(raised),
            overlay: f64::from(overlay),
        }
    }
}

/// OKLCH lightness of the three ink tiers, on the content surface.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Ink {
    /// Primary ink
    pub primary: f64,
    /// Secondary ink
    pub secondary: f64,
    /// Tertiary ink
    pub tertiary: f64,
}

/// The presence colours' OKLCH lightness and chroma.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Presence {
    /// Presence lightness
    pub lightness: f64,
    /// Presence chroma
    pub chroma: f64,
}

/// One status colour's OKLCH lightness and chroma; its hue is the status's.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StatusTone {
    pub lightness: f64,
    pub chroma: f64,
}

impl StatusTone {
    fn of(tone: Tone) -> Self {
        Self {
            lightness: f64::from(tone.lightness),
            chroma: f64::from(tone.chroma),
        }
    }

    fn tone(self) -> Tone {
        Tone::new(narrow(self.lightness), narrow(self.chroma))
    }
}

/// The four status tones.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub danger: StatusTone,
    pub warning: StatusTone,
    pub success: StatusTone,
    pub info: StatusTone,
}

/// One theme of the person's: every role it edits. Its id is its key in the person's
/// themes, which a mode names as `custom:<id>`.
#[derive(Model, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[model(check = ThemeRecord::admitted)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each bool is one independent switch of the theme document; folding them into a \
              state enum would make combinations a person can set unrepresentable"
)]
pub struct ThemeRecord {
    #[model(label)]
    pub name: String,
    #[model(readonly)]
    pub dark: bool,
    pub high_contrast: bool,
    pub hue: Option<f64>,
    pub chroma: f64,
    pub surfaces: Surfaces,
    pub ink: Ink,
    pub status: Status,
    pub tint: Option<f64>,
    #[model(default = false)]
    #[serde(default)]
    pub monochrome: bool,
    pub presence: Presence,
    pub window_radius: f64,
    pub density: f64,
    pub face: String,
    pub base_size: f64,
    pub distinct_letters: Option<bool>,
    pub motion: f64,
    pub blur: f64,
    pub tint_opacity: f64,
    pub frosted_strip: bool,
    pub rim_span: f64,
    pub rim_alpha: f64,
    pub hover_alpha: f64,
    /// How far the glass bends what lies behind floating chrome at its rim, logical pixels: the
    /// finish's `refraction`, which the Glass and Liquid sets and a material stage read. Windows and
    /// bars bend in the same proportion to it as the theme had them. `0` bends nothing.
    #[model(default = 0.0)]
    #[serde(default)]
    pub refraction: f64,
    /// The document the theme was made from, as JSON: the roles no field names.
    #[model(readonly)]
    #[model(default = None)]
    #[serde(default)]
    pub origin: Option<String>,
}

impl ThemeRecord {
    /// A theme with every role of `document`.
    #[must_use]
    pub fn from_document(document: &ThemeDocument) -> Self {
        let palette = &document.palette;
        let surfaces = palette.surfaces;
        let ink = palette.ink;
        Self {
            name: document.name.clone(),
            dark: palette.dark,
            high_contrast: palette.high_contrast,
            hue: palette.hue.map(f64::from),
            chroma: f64::from(palette.chroma),
            surfaces: Surfaces::from(surfaces),
            ink: Ink {
                primary: f64::from(ink.primary),
                secondary: f64::from(ink.secondary),
                tertiary: f64::from(ink.tertiary),
            },
            status: Status {
                danger: StatusTone::of(palette.status.danger),
                warning: StatusTone::of(palette.status.warning),
                success: StatusTone::of(palette.status.success),
                info: StatusTone::of(palette.status.info),
            },
            tint: match document.emphasis {
                Emphasis::Inverted | Emphasis::You => None,
                Emphasis::Tinted { hue } => Some(f64::from(hue)),
            },
            monochrome: document.emphasis == Emphasis::Inverted,
            presence: Presence {
                lightness: f64::from(document.presence.lightness),
                chroma: f64::from(document.presence.chroma),
            },
            window_radius: f64::from(document.shape.window_radius),
            density: f64::from(document.density.spacing),
            face: document.type_.face.clone(),
            base_size: f64::from(document.type_.base_size),
            distinct_letters: document.type_.distinct_letters,
            motion: f64::from(document.motion.character),
            blur: f64::from(document.material.floating.blur()),
            tint_opacity: f64::from(document.material.floating.tint()),
            frosted_strip: document.material.selector.strip == StripMaterial::Floating,
            rim_span: f64::from(document.material.selector.rim_span),
            rim_alpha: f64::from(document.material.selector.rim_alpha),
            hover_alpha: f64::from(document.material.selector.hover_alpha),
            refraction: f64::from(
                document
                    .material
                    .floating
                    .param("refraction")
                    .unwrap_or(0.0),
            ),
            origin: serde_json::to_string(document).ok(),
        }
    }

    /// A new theme starting from the theme `base`, named as the base's copy.
    #[must_use]
    pub fn draft(base: &ThemeDocument) -> Self {
        let mut theme = Self::from_document(base);
        theme.name = format!("{} copy", theme.name);
        theme
    }

    /// The preset `id` names, as a theme document.
    #[must_use]
    pub fn preset(id: &str) -> Option<ThemeDocument> {
        Preset::find(id).map(|preset| preset.document())
    }

    /// The document this theme was made from, where it was kept and still reads.
    fn origin(&self) -> Option<ThemeDocument> {
        serde_json::from_str(self.origin.as_deref()?).ok()
    }

    /// The floating finish: the origin's as it is while the blur and tint are its own, else
    /// its painter with the edited blur and tint (the stock frosted or solid surface where
    /// that painter cannot take them).
    fn floating(&self, origin: Option<&crate::tokens::Finish>) -> crate::tokens::Finish {
        let (blur, tint) = (narrow(self.blur), narrow(self.tint_opacity));
        if let Some(origin) = origin {
            if origin.blur().to_bits() == blur.to_bits()
                && origin.tint().to_bits() == tint.to_bits()
            {
                return origin.clone();
            }
            let mut params: Vec<(String, f32)> = origin
                .params()
                .iter()
                .filter(|param| !matches!(param.name.as_str(), "blur" | "tint"))
                .map(|param| (param.name.clone(), param.value))
                .collect();
            params.extend([("blur".to_owned(), blur), ("tint".to_owned(), tint)]);
            if let Ok(edited) = crate::tokens::Finish::new(origin.set().clone(), params) {
                return edited.with_shader(origin.shader().clone());
            }
        }
        if blur > 0.0 {
            crate::tokens::Finish::frosted(blur, tint, 1.0)
        } else {
            crate::tokens::Finish::solid()
        }
    }

    /// The palette this theme's roles make.
    #[must_use]
    pub fn palette(&self) -> Palette {
        Palette {
            dark: self.dark,
            high_contrast: self.high_contrast,
            hue: self.hue.map(narrow),
            chroma: narrow(self.chroma),
            surfaces: self.surfaces.ladder(),
            ink: InkLadder {
                primary: narrow(self.ink.primary),
                secondary: narrow(self.ink.secondary),
                tertiary: narrow(self.ink.tertiary),
            },
            status: StatusTones {
                danger: self.status.danger.tone(),
                warning: self.status.warning.tone(),
                success: self.status.success.tone(),
                info: self.status.info.tone(),
            },
        }
    }

    /// The theme document this theme is, under `id`.
    #[must_use]
    pub fn document(&self, id: &str) -> ThemeDocument {
        let origin = self.origin();
        let origin = origin.as_ref();
        ThemeDocument {
            icons: origin.and_then(|origin| origin.icons),
            schema_version: THEME_SCHEMA_VERSION,
            id: id.to_owned(),
            name: if self.name.trim().is_empty() {
                String::from("Untitled theme")
            } else {
                self.name.trim().to_owned()
            },
            palette: self.palette(),
            emphasis: if self.monochrome {
                Emphasis::Inverted
            } else {
                self.tint
                    .map_or(Emphasis::You, |hue| Emphasis::Tinted { hue: narrow(hue) })
            },
            presence: Tone::new(
                narrow(self.presence.lightness),
                narrow(self.presence.chroma),
            ),
            shape: ThemeShape {
                window_radius: narrow(self.window_radius),
                border: origin.map_or(crate::tokens::Border::STOCK, |origin| origin.shape.border),
            },
            density: ThemeDensity {
                spacing: narrow(self.density),
            },
            type_: ThemeType {
                face: self.face.clone(),
                reading: origin.map_or_else(ThemeType::default_reading, |origin| {
                    origin.type_.reading.clone()
                }),
                base_size: narrow(self.base_size),
                distinct_letters: self.distinct_letters,
                contrast: origin.map_or(ThemeType::STOCK_CONTRAST, |origin| origin.type_.contrast),
                weights: origin.map_or(crate::theme::ThemeWeights::STOCK, |origin| {
                    origin.type_.weights
                }),
            },
            motion: ThemeMotion {
                character: narrow(self.motion),
                ..origin.map_or(ThemeMotion::STOCK, |origin| origin.motion)
            },
            material: ThemeMaterial {
                floating: self.floating(origin.map(|origin| &origin.material.floating)),
                selector: SelectorFinish {
                    strip: if self.frosted_strip {
                        StripMaterial::Floating
                    } else {
                        StripMaterial::Opaque
                    },
                    rim_span: narrow(self.rim_span),
                    rim_alpha: narrow(self.rim_alpha),
                    hover_alpha: narrow(self.hover_alpha),
                },
                ..origin.map_or_else(ThemeMaterial::stock, |origin| origin.material.clone())
            }
            .refracting(narrow(self.refraction)),
            depth: origin.map_or(crate::tokens::Depth::STOCK, |origin| origin.depth),
            response: origin.and_then(|origin| origin.response),
        }
    }

    /// Every contrast floor this theme misses, as a person reads them.
    #[must_use]
    pub fn findings(&self, id: &str) -> Vec<String> {
        let document = self.document(id);
        match document.validate() {
            Err(reason) => vec![reason.to_owned()],
            Ok(()) => document
                .findings()
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }

    /// This theme turned dark (ink light on its surfaces) or light, keeping its hue,
    /// chroma and name.
    #[must_use]
    pub fn with_polarity(&self, dark: bool) -> Self {
        if self.dark == dark {
            return self.clone();
        }
        let stock = if dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        let mut document = self.document("theme");
        document.palette = Palette {
            hue: document.palette.hue,
            chroma: document.palette.chroma,
            ..stock
        };
        document.presence = stock.stock_presence();
        Self {
            name: self.name.clone(),
            ..Self::from_document(&document)
        }
    }

    /// Every role is a number inside the band the document admits, and every pair of ink
    /// and surface keeps its contrast floor: an edit that would make the theme illegible is
    /// refused with the pairs it would break.
    fn admitted(&self) -> Result<(), Refusal> {
        let numbers = [
            self.chroma,
            self.ink.primary,
            self.ink.secondary,
            self.ink.tertiary,
            self.presence.lightness,
            self.presence.chroma,
            self.window_radius,
            self.density,
            self.base_size,
            self.motion,
            self.blur,
            self.tint_opacity,
            self.rim_span,
            self.rim_alpha,
            self.hover_alpha,
            self.refraction,
        ];
        if numbers
            .into_iter()
            .chain(self.surfaces.lightnesses())
            .chain(self.hue)
            .chain(self.tint)
            .any(|value| !value.is_finite())
        {
            return Err(Refusal::new("every role of a theme is a number"));
        }
        let document = self.document("theme");
        document.validate().map_err(Refusal::from)?;
        legible(&document)
    }
}

/// A theme document read from JSON, refused unless it is admitted and keeps every contrast
/// floor: what importing a theme takes.
///
/// # Errors
/// What is wrong with the document, as a person reads it.
pub fn admit_document(json: &str) -> Result<ThemeDocument, Refusal> {
    let document: ThemeDocument = serde_json::from_str(json)
        .map_err(|error| Refusal::new(format!("not a theme document: {error}")))?;
    if let Some(reason) = crate::Theme::refusal(&document) {
        return Err(Refusal::new(reason));
    }
    legible(&document)?;
    Ok(document)
}

/// Refused with the pairs that miss their contrast floor, if any do.
fn legible(document: &ThemeDocument) -> Result<(), Refusal> {
    let findings = document.findings();
    if findings.is_empty() {
        return Ok(());
    }
    Err(Refusal::new(format!(
        "Too little contrast: {}",
        findings
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("; ")
    )))
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "theme roles are small numbers; f32 holds them exactly enough"
)]
fn narrow(value: f64) -> f32 {
    value as f32
}

impl ThemeMaterial {
    /// This material with floating chrome bending what lies behind it by `refraction`, and windows
    /// and bars in the proportion to it the material had them (each at `refraction` where floating
    /// chrome bent nothing). A finish whose set takes no refraction keeps its look.
    fn refracting(mut self, refraction: f32) -> Self {
        let before = self.floating.param("refraction").unwrap_or(0.0);
        // Unchanged, the material is the theme's own, windows that bend while floating chrome
        // does not included.
        if refraction.to_bits() == before.to_bits() {
            return self;
        }
        let scaled = |finish: &crate::tokens::Finish| -> crate::tokens::Finish {
            // A finish that states no refraction takes one where its set declares it, and
            // keeps its look where it does not (`Finish::new` refuses the parameter).
            let own = finish.param("refraction").unwrap_or(0.0);
            let value = if before > 0.0 {
                own * refraction / before
            } else {
                refraction
            };
            if value.to_bits() == own.to_bits() {
                return finish.clone();
            }
            let mut params = finish
                .params()
                .iter()
                .filter(|param| param.name != "refraction")
                .map(|param| (param.name.clone(), param.value))
                .collect::<Vec<_>>();
            params.push(("refraction".to_owned(), value));
            crate::tokens::Finish::new(finish.set().clone(), params).map_or_else(
                |_| finish.clone(),
                |edited| edited.with_shader(finish.shader().clone()),
            )
        };
        self.window = scaled(&self.window);
        self.floating = scaled(&self.floating);
        self.bar = self.bar.as_ref().map(scaled);
        self
    }
}
