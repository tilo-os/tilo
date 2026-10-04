//! The themes a person is writing: the app's own resource.
//!
//! Each theme is one theme document (`ui::theme::ThemeDocument`) stored role
//! by role, so two people tuning different roles of the same theme merge
//! instead of overwriting each other. Every edit is an action, so the
//! palette, an agent or a script shapes a theme exactly as the controls do.
//! An edit that would take a role outside the band the document admits is
//! refused; legibility is measured on the whole document
//! ([`Theme::findings`]) and gates applying and exporting, not editing.

use ui::prelude::*;
use ui::theme::{
    Preset, THEME_SCHEMA_VERSION, ThemeDensity, ThemeDocument, ThemeMaterial, ThemeMotion,
    ThemeShape, ThemeType,
};
use ui::tokens::{Emphasis, Palette, Tone};

/// One theme being written.
#[derive(Model, Debug, Clone, PartialEq)]
pub struct Theme {
    pub name: String,
    pub palette: Json<Palette>,
    pub emphasis: Json<Emphasis>,
    pub presence: Json<Tone>,
    pub window_radius: f32,
    pub density: f32,
    pub face: String,
    pub base_size: f32,
    pub motion: f32,
    pub blur: f32,
    pub tint_opacity: f32,
}

impl Theme {
    /// A theme with every role of `document`.
    #[must_use]
    pub fn from_document(document: &ThemeDocument) -> Self {
        Self {
            name: document.name.clone(),
            palette: Json(document.palette),
            emphasis: Json(document.emphasis),
            presence: Json(document.presence),
            window_radius: document.shape.window_radius,
            density: document.density.spacing,
            face: document.type_.face.clone(),
            base_size: document.type_.base_size,
            motion: document.motion.character,
            blur: document.material.blur,
            tint_opacity: document.material.tint_opacity,
        }
    }

    /// The theme document this theme is, under `id`.
    #[must_use]
    pub fn document(&self, id: &str) -> ThemeDocument {
        ThemeDocument {
            schema_version: THEME_SCHEMA_VERSION,
            id: id.to_owned(),
            name: if self.name.trim().is_empty() {
                String::from("Untitled theme")
            } else {
                self.name.trim().to_owned()
            },
            palette: self.palette.0,
            emphasis: self.emphasis.0,
            presence: self.presence.0,
            shape: ThemeShape {
                window_radius: self.window_radius,
            },
            density: ThemeDensity {
                spacing: self.density,
            },
            type_: ThemeType {
                face: self.face.clone(),
                base_size: self.base_size,
            },
            motion: ThemeMotion {
                character: self.motion,
            },
            material: ThemeMaterial {
                blur: self.blur,
                tint_opacity: self.tint_opacity,
            },
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
}

/// A rung of the surface ladder, as actions name it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    Base,
    Content,
    Sidebar,
    Inspector,
    Raised,
    Overlay,
    Primary,
    Secondary,
    Tertiary,
}

impl Rung {
    pub const SURFACES: [Self; 6] = [
        Self::Base,
        Self::Sidebar,
        Self::Content,
        Self::Inspector,
        Self::Raised,
        Self::Overlay,
    ];
    pub const INK: [Self; 3] = [Self::Primary, Self::Secondary, Self::Tertiary];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Content => "content",
            Self::Sidebar => "sidebar",
            Self::Inspector => "inspector",
            Self::Raised => "raised",
            Self::Overlay => "overlay",
            Self::Primary => "primary",
            Self::Secondary => "secondary",
            Self::Tertiary => "tertiary",
        }
    }

    #[must_use]
    pub fn from_id(id: &str) -> Option<Self> {
        Self::SURFACES
            .into_iter()
            .chain(Self::INK)
            .find(|rung| rung.id() == id)
    }

    /// The lightness this rung holds in `palette`.
    #[must_use]
    pub fn of(self, palette: &Palette) -> f32 {
        match self {
            Self::Base => palette.surfaces.base,
            Self::Content => palette.surfaces.content,
            Self::Sidebar => palette.surfaces.sidebar,
            Self::Inspector => palette.surfaces.inspector,
            Self::Raised => palette.surfaces.raised,
            Self::Overlay => palette.surfaces.overlay,
            Self::Primary => palette.ink.primary,
            Self::Secondary => palette.ink.secondary,
            Self::Tertiary => palette.ink.tertiary,
        }
    }

    fn slot(self, palette: &mut Palette) -> &mut f32 {
        match self {
            Self::Base => &mut palette.surfaces.base,
            Self::Content => &mut palette.surfaces.content,
            Self::Sidebar => &mut palette.surfaces.sidebar,
            Self::Inspector => &mut palette.surfaces.inspector,
            Self::Raised => &mut palette.surfaces.raised,
            Self::Overlay => &mut palette.surfaces.overlay,
            Self::Primary => &mut palette.ink.primary,
            Self::Secondary => &mut palette.ink.secondary,
            Self::Tertiary => &mut palette.ink.tertiary,
        }
    }
}

/// Every theme on this person's workbench.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Studio {
    pub themes: Collection<Theme>,
}

fn theme_mut<'a>(studio: &'a mut Studio, theme: &Id) -> Result<&'a mut Theme, Refusal> {
    studio
        .themes
        .get_mut(theme)
        .ok_or_else(|| Refusal::from("there is no such theme"))
}

/// Apply `edit` to a copy of `theme` and keep it only if the document it
/// makes is still admitted.
fn edit(studio: &mut Studio, theme: &Id, edit: impl FnOnce(&mut Theme)) -> Result<(), Refusal> {
    let current = theme_mut(studio, theme)?;
    let mut next = current.clone();
    edit(&mut next);
    next.document(theme.as_str())
        .validate()
        .map_err(Refusal::from)?;
    *current = next;
    Ok(())
}

fn finite(value: f64, what: &str) -> Result<f32, Refusal> {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "theme roles are small numbers; f32 holds them exactly enough"
    )]
    let value = value as f32;
    if value.is_finite() {
        Ok(value)
    } else {
        Err(Refusal::new(format!("{what} must be a number")))
    }
}

/// The theme a new one starts from: a preset id, or another theme's id.
fn base_document(studio: &Studio, base: &str) -> Result<ThemeDocument, Refusal> {
    if let Some(preset) = Preset::find(base) {
        return Ok(preset.document());
    }
    let id = Id::from_raw(base);
    studio
        .themes
        .get(&id)
        .map(|theme| theme.document(id.as_str()))
        .ok_or_else(|| Refusal::new(format!("no preset or theme is called {base:?}")))
}

#[resource(kind = "so.tilo.themes.studio", label = "Themes")]
impl Studio {
    /// Start a theme from a preset or from one of your themes.
    #[action]
    pub fn new_theme(
        &mut self,
        #[param("Id for the new theme")] id: String,
        #[param("The preset or theme to start from")] base: String,
    ) -> Result<(), Refusal> {
        let id = Id::from_raw(id);
        if self.themes.contains(&id) || Preset::find(id.as_str()).is_some() {
            return Err(Refusal::from("a theme with that id already exists"));
        }
        let mut theme = Theme::from_document(&base_document(self, &base)?);
        theme.name = format!("{} copy", theme.name);
        self.themes.push(id, theme);
        Ok(())
    }

    /// Rename a theme.
    #[action]
    pub fn rename(&mut self, theme: Id, #[param("New name")] name: String) -> Result<(), Refusal> {
        edit(self, &theme, |t| t.name = name)
    }

    /// Delete a theme.
    #[action(destructive)]
    pub fn delete(&mut self, theme: Id) -> Result<(), Refusal> {
        self.themes
            .remove(&theme)
            .map(|_| ())
            .ok_or_else(|| Refusal::from("there is no such theme"))
    }

    /// Make a theme dark (ink light on its surfaces) or light. The ladder
    /// flips to the stock ladder of that polarity, keeping hue and chroma.
    #[action]
    pub fn set_dark(
        &mut self,
        theme: Id,
        #[param("Whether the theme is dark")] dark: bool,
    ) -> Result<(), Refusal> {
        edit(self, &theme, |t| {
            if t.palette.0.dark == dark {
                return;
            }
            let stock = if dark {
                Palette::dark()
            } else {
                Palette::light()
            };
            let chroma = t.palette.0.chroma;
            t.palette = Json(Palette {
                hue: t.palette.0.hue,
                chroma,
                ..stock
            });
            t.presence = Json(stock.stock_presence());
        })
    }

    /// Set the hue every neutral shares, in degrees.
    #[action]
    pub fn set_hue(
        &mut self,
        theme: Id,
        #[param("Hue in degrees, 0 to 360")] hue: f64,
    ) -> Result<(), Refusal> {
        let hue = finite(hue, "a hue")?.rem_euclid(360.0);
        edit(self, &theme, |t| t.palette.0.hue = Some(hue))
    }

    /// Set the neutrals' chroma: 0 is pure grey, 0.01 the most temperature
    /// a neutral may carry.
    #[action]
    pub fn set_chroma(
        &mut self,
        theme: Id,
        #[param("Chroma, 0 to 0.01")] chroma: f64,
    ) -> Result<(), Refusal> {
        let chroma = finite(chroma, "a chroma")?;
        edit(self, &theme, |t| t.palette.0.chroma = chroma)
    }

    /// Set the lightness of one surface or ink tier: `base`, `content`,
    /// `sidebar`, `inspector`, `raised`, `overlay`, `primary`, `secondary`
    /// or `tertiary`.
    #[action]
    pub fn set_lightness(
        &mut self,
        theme: Id,
        #[param("Surface or ink tier")] rung: String,
        #[param("OKLCH lightness, 0 to 1")] lightness: f64,
    ) -> Result<(), Refusal> {
        let rung = Rung::from_id(&rung).ok_or_else(|| Refusal::new(format!("no tier {rung:?}")))?;
        let lightness = finite(lightness, "a lightness")?;
        edit(self, &theme, |t| *rung.slot(&mut t.palette.0) = lightness)
    }

    /// Show primary actions, selection and on states inverted (no colour),
    /// or tinted with one hue.
    #[action]
    pub fn set_tinted(
        &mut self,
        theme: Id,
        #[param("Whether actions and selection are tinted")] tinted: bool,
    ) -> Result<(), Refusal> {
        edit(self, &theme, |t| {
            t.emphasis = Json(match (tinted, t.emphasis.0) {
                (false, _) => Emphasis::Inverted,
                (true, Emphasis::Tinted { hue }) => Emphasis::Tinted { hue },
                (true, Emphasis::Inverted) => Emphasis::Tinted { hue: 150.0 },
            });
        })
    }

    /// Set the tint of a tinted emphasis, in degrees.
    #[action]
    pub fn set_tint_hue(
        &mut self,
        theme: Id,
        #[param("Hue in degrees, 0 to 360")] hue: f64,
    ) -> Result<(), Refusal> {
        let hue = finite(hue, "a hue")?.rem_euclid(360.0);
        edit(self, &theme, |t| {
            t.emphasis = Json(Emphasis::Tinted { hue });
        })
    }

    /// Set the lightness of the twelve presence colours.
    #[action]
    pub fn set_presence_lightness(
        &mut self,
        theme: Id,
        #[param("OKLCH lightness, 0.35 to 0.9")] lightness: f64,
    ) -> Result<(), Refusal> {
        let lightness = finite(lightness, "a lightness")?;
        edit(self, &theme, |t| t.presence.0.lightness = lightness)
    }

    /// Set the chroma of the twelve presence colours.
    #[action]
    pub fn set_presence_chroma(
        &mut self,
        theme: Id,
        #[param("OKLCH chroma, 0.04 to 0.2")] chroma: f64,
    ) -> Result<(), Refusal> {
        let chroma = finite(chroma, "a chroma")?;
        edit(self, &theme, |t| t.presence.0.chroma = chroma)
    }

    /// Set the window radius; every other radius follows from it.
    #[action]
    pub fn set_window_radius(
        &mut self,
        theme: Id,
        #[param("Window radius in pixels, 0 to 28")] radius: f64,
    ) -> Result<(), Refusal> {
        let radius = finite(radius, "a radius")?;
        edit(self, &theme, |t| t.window_radius = radius)
    }

    /// Set the spacing scale, as a multiple of the stock scale.
    #[action]
    pub fn set_density(
        &mut self,
        theme: Id,
        #[param("Spacing, 0.75 to 1.5")] density: f64,
    ) -> Result<(), Refusal> {
        let density = finite(density, "a density")?;
        edit(self, &theme, |t| t.density = density)
    }

    /// Set the interface typeface.
    #[action]
    pub fn set_face(
        &mut self,
        theme: Id,
        #[param("Font family")] face: String,
    ) -> Result<(), Refusal> {
        edit(self, &theme, |t| face.trim().clone_into(&mut t.face))
    }

    /// Set the body text size every text role derives from.
    #[action]
    pub fn set_base_size(
        &mut self,
        theme: Id,
        #[param("Body size in pixels, 12 to 18")] size: f64,
    ) -> Result<(), Refusal> {
        let size = finite(size, "a size")?;
        edit(self, &theme, |t| t.base_size = size)
    }

    /// Set the character of motion: 0 is calm, 1 the stock motion, 2 lively.
    #[action]
    pub fn set_motion(
        &mut self,
        theme: Id,
        #[param("Motion character, 0 to 2")] character: f64,
    ) -> Result<(), Refusal> {
        let character = finite(character, "a motion character")?;
        edit(self, &theme, |t| t.motion = character)
    }

    /// Set how strongly floating chrome blurs what is behind it.
    #[action]
    pub fn set_blur(
        &mut self,
        theme: Id,
        #[param("Blur in pixels, 0 to 48")] blur: f64,
    ) -> Result<(), Refusal> {
        let blur = finite(blur, "a blur")?;
        edit(self, &theme, |t| t.blur = blur)
    }

    /// Set how opaque floating chrome's tint is over the blurred backdrop.
    #[action]
    pub fn set_tint_opacity(
        &mut self,
        theme: Id,
        #[param("Opacity, 0.6 to 1")] opacity: f64,
    ) -> Result<(), Refusal> {
        let opacity = finite(opacity, "an opacity")?;
        edit(self, &theme, |t| t.tint_opacity = opacity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn studio_with(base: &str) -> (Studio, Id) {
        let mut studio = Studio::default();
        studio
            .new_theme("mine".into(), base.into())
            .expect("a preset is a base");
        (studio, Id::from_raw("mine"))
    }

    #[test]
    fn a_theme_round_trips_its_document_role_by_role() {
        for preset in Preset::ALL {
            let document = preset.document();
            let theme = Theme::from_document(&document);
            assert_eq!(theme.document(preset.id()), document);
            assert_eq!(theme.findings(preset.id()), Vec::<String>::new());
        }
    }

    #[test]
    fn edits_stay_inside_the_admitted_bands() {
        let (mut studio, id) = studio_with("dark-soft");
        assert_eq!(studio.themes[&id].name, "Dark soft copy");
        studio.set_hue(id.clone(), 400.0).unwrap();
        assert!(
            studio.themes[&id]
                .palette
                .0
                .hue
                .is_some_and(|hue| (hue - 40.0).abs() < 1e-4)
        );
        assert!(
            studio.set_chroma(id.clone(), 0.2).is_err(),
            "too much colour"
        );
        assert!(studio.set_window_radius(id.clone(), 90.0).is_err());
        assert!(studio.set_density(id.clone(), f64::NAN).is_err());
        assert!(
            studio
                .set_lightness(id.clone(), "tertiary".into(), 0.1)
                .is_err(),
            "ink may not cross to the surfaces' side"
        );
        studio
            .set_lightness(id.clone(), "content".into(), 0.24)
            .unwrap();
        assert!((studio.themes[&id].palette.0.surfaces.content - 0.24).abs() < 1e-6);
        assert!(studio.rename(Id::from_raw("nobody"), "x".into()).is_err());
    }

    #[test]
    fn emphasis_polarity_and_findings_follow_the_edits() {
        let (mut studio, id) = studio_with("dark");
        studio.set_tinted(id.clone(), true).unwrap();
        studio.set_tint_hue(id.clone(), 30.0).unwrap();
        assert_eq!(
            studio.themes[&id].emphasis.0,
            Emphasis::Tinted { hue: 30.0 }
        );
        studio.set_tinted(id.clone(), false).unwrap();
        assert_eq!(studio.themes[&id].emphasis.0, Emphasis::Inverted);

        studio.set_dark(id.clone(), false).unwrap();
        let theme = &studio.themes[&id];
        assert!(!theme.palette.0.dark);
        assert_eq!(theme.findings("mine"), Vec::<String>::new());

        studio
            .set_lightness(id.clone(), "tertiary".into(), 0.80)
            .unwrap();
        let findings = studio.themes[&id].findings("mine");
        assert!(
            findings.iter().any(|f| f.starts_with("text_tertiary")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_new_theme_starts_from_a_preset_or_a_theme_and_ids_are_unique() {
        let (mut studio, id) = studio_with("light");
        studio.set_density(id.clone(), 1.2).unwrap();
        studio.new_theme("second".into(), "mine".into()).unwrap();
        let second = &studio.themes[&Id::from_raw("second")];
        assert!((second.density - 1.2).abs() < 1e-6);
        assert!(studio.new_theme("mine".into(), "dark".into()).is_err());
        assert!(studio.new_theme("dark".into(), "dark".into()).is_err());
        assert!(studio.new_theme("third".into(), "nord".into()).is_err());
        studio.delete(id).unwrap();
        assert_eq!(studio.themes.len(), 1);
    }
}
