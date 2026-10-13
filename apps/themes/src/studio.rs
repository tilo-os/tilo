//! The themes a person is writing: the app's own resource.
//!
//! Each theme is one theme document (`ui::theme::ThemeDocument`) stored role
//! by role, so two people tuning different roles of the same theme merge
//! instead of overwriting each other. Every role is a field edited at its
//! path (`Studio::paths().themes.item(&id).density.set(1.2)`), so the
//! palette, an agent or a script shapes a theme exactly as the controls do.
//! A change that would take a role outside the band the document admits is
//! refused by the theme's invariant, whichever way it comes; legibility is
//! measured on the whole document ([`Theme::findings`]) and gates applying
//! and exporting, not editing.

use ui::prelude::*;
use ui::theme::{
    Preset, THEME_SCHEMA_VERSION, ThemeDensity, ThemeDocument, ThemeMaterial, ThemeMotion,
    ThemeShape, ThemeType,
};
use ui::tilo_resource::{Path, Refusal};
use ui::tokens::{Emphasis, InkLadder, Palette, StatusTones, SurfaceLadder, Tone};

/// OKLCH lightness of every surface, `0` black to `1` white.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq)]
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
#[derive(Model, Default, Debug, Clone, Copy, PartialEq)]
pub struct Ink {
    /// Primary ink
    pub primary: f64,
    /// Secondary ink
    pub secondary: f64,
    /// Tertiary ink
    pub tertiary: f64,
}

/// The presence colours' OKLCH lightness and chroma.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq)]
pub struct Presence {
    /// Presence lightness
    pub lightness: f64,
    /// Presence chroma
    pub chroma: f64,
}

/// One status colour's OKLCH lightness and chroma; its hue is the status's.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq)]
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
#[derive(Model, Default, Debug, Clone, Copy, PartialEq)]
pub struct Status {
    pub danger: StatusTone,
    pub warning: StatusTone,
    pub success: StatusTone,
    pub info: StatusTone,
}

/// One theme being written.
#[derive(Model, Debug, Clone, PartialEq)]
#[model(check = Theme::admitted)]
pub struct Theme {
    /// Name
    #[model(label)]
    pub name: String,
    /// Ink is light on the surfaces. Switching flips the ladder
    /// ([`Studio::switch_polarity`]), so it is not set on its own.
    #[model(readonly)]
    pub dark: bool,
    /// Every text pairing holds AAA.
    pub high_contrast: bool,
    /// The hue every neutral shares, in degrees; none until something gives
    /// the material a temperature.
    pub hue: Option<f64>,
    /// Neutral chroma
    pub chroma: f64,
    pub surfaces: Surfaces,
    pub ink: Ink,
    pub status: Status,
    /// The hue primary actions, selection and on states are tinted with;
    /// none shows them inverted.
    pub tint: Option<f64>,
    pub presence: Presence,
    /// Window radius
    pub window_radius: f64,
    /// Density
    pub density: f64,
    /// Typeface
    pub face: String,
    /// Body size
    pub base_size: f64,
    /// "Always distinguish similar letters": `Some(true)` always, `None`
    /// with the typeface (on with Hyperlegible).
    pub distinct_letters: Option<bool>,
    /// Motion
    pub motion: f64,
    /// Blur
    pub blur: f64,
    /// Tint opacity
    pub tint_opacity: f64,
}

impl Theme {
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
                Emphasis::Inverted => None,
                Emphasis::Tinted { hue } => Some(f64::from(hue)),
            },
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
            blur: f64::from(document.material.blur),
            tint_opacity: f64::from(document.material.tint_opacity),
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
        ThemeDocument {
            schema_version: THEME_SCHEMA_VERSION,
            id: id.to_owned(),
            name: if self.name.trim().is_empty() {
                String::from("Untitled theme")
            } else {
                self.name.trim().to_owned()
            },
            palette: self.palette(),
            emphasis: self
                .tint
                .map_or(Emphasis::Inverted, |hue| Emphasis::Tinted {
                    hue: narrow(hue),
                }),
            presence: Tone::new(
                narrow(self.presence.lightness),
                narrow(self.presence.chroma),
            ),
            shape: ThemeShape {
                window_radius: narrow(self.window_radius),
            },
            density: ThemeDensity {
                spacing: narrow(self.density),
            },
            type_: ThemeType {
                face: self.face.clone(),
                reading: ThemeType::default_reading(),
                base_size: narrow(self.base_size),
                distinct_letters: self.distinct_letters,
            },
            motion: ThemeMotion {
                character: narrow(self.motion),
            },
            material: ThemeMaterial {
                blur: narrow(self.blur),
                tint_opacity: narrow(self.tint_opacity),
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

    /// Every role is a number inside the band the document admits.
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
        self.document("theme").validate().map_err(Refusal::from)
    }
}

/// A role's value as the theme document holds it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "theme roles are small numbers; f32 holds them exactly enough"
)]
fn narrow(value: f64) -> f32 {
    value as f32
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

    /// The path of this rung's lightness in `theme`.
    #[must_use]
    pub fn path<R>(self, theme: &ThemePaths<R>) -> Path<R, f64> {
        let SurfacesPaths {
            base,
            content,
            sidebar,
            inspector,
            raised,
            overlay,
            ..
        } = &theme.surfaces;
        match self {
            Self::Base => base.clone(),
            Self::Content => content.clone(),
            Self::Sidebar => sidebar.clone(),
            Self::Inspector => inspector.clone(),
            Self::Raised => raised.clone(),
            Self::Overlay => overlay.clone(),
            Self::Primary => theme.ink.primary.clone(),
            Self::Secondary => theme.ink.secondary.clone(),
            Self::Tertiary => theme.ink.tertiary.clone(),
        }
    }
}

/// Every theme on this person's workbench.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Studio {
    pub themes: Collection<Theme>,
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

#[resource(kind = "themes.studio", label = "Themes")]
impl Studio {
    /// Start a theme from a preset or from one of your themes.
    #[action]
    pub fn new_theme(
        &mut self,
        #[param("Id for the new theme")] id: String,
        #[param("The preset or theme to start from", example = "dark-graphite")] base: String,
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

    /// Reset the editable properties to the graphite preset, preserving its name.
    #[action]
    pub fn reset(&mut self, theme: &Id) -> Result<(), Refusal> {
        let document = base_document(self, "dark-graphite")?;
        let current = theme_mut(self, theme)?;
        *current = Theme {
            name: current.name.clone(),
            ..Theme::from_document(&document)
        };
        Ok(())
    }

    /// Delete a theme.
    #[action(destructive)]
    pub fn delete(&mut self, theme: &Id) -> Result<(), Refusal> {
        self.themes
            .remove(theme)
            .map(|_| ())
            .ok_or_else(|| Refusal::from("there is no such theme"))
    }

    /// Switch a theme between dark (ink light on its surfaces) and light.
    /// The ladder flips to the stock ladder of that polarity, keeping hue and
    /// chroma.
    #[action]
    pub fn switch_polarity(
        &mut self,
        theme: &Id,
        #[param("Whether the theme is dark")] dark: bool,
    ) -> Result<(), Refusal> {
        let current = theme_mut(self, theme)?;
        if current.dark == dark {
            return Ok(());
        }
        let stock = if dark {
            Palette::dark()
        } else {
            Palette::light()
        };
        let mut document = current.document(theme.as_str());
        document.palette = Palette {
            hue: document.palette.hue,
            chroma: document.palette.chroma,
            ..stock
        };
        document.presence = stock.stock_presence();
        let name = current.name.clone();
        *current = Theme {
            name,
            ..Theme::from_document(&document)
        };
        Ok(())
    }
}

fn theme_mut<'a>(studio: &'a mut Studio, theme: &Id) -> Result<&'a mut Theme, Refusal> {
    studio
        .themes
        .get_mut(theme)
        .ok_or_else(|| Refusal::from("there is no such theme"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::tilo_resource::{Edit, Replica};

    fn studio_with(base: &str) -> (Replica<Studio>, Id) {
        let mut studio = Replica::<Studio>::new();
        studio
            .apply(Studio::NEW_THEME.with(("mine".to_owned(), base.to_owned())))
            .expect("a preset is a base");
        (studio, Id::from_raw("mine"))
    }

    fn edit(studio: &mut Replica<Studio>, edit: &Edit<Studio>) -> Result<(), String> {
        let invoke = Invoke::edit(edit).map_err(|error| error.to_string())?;
        studio
            .apply(invoke)
            .map(drop)
            .map_err(|error| error.to_string())
    }

    fn theme(studio: &Replica<Studio>, id: &Id) -> Theme {
        studio.state().unwrap().themes[id].clone()
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
        assert_eq!(theme(&studio, &id).name, "Dark soft copy");
        let paths = Studio::paths().themes.item(&id);
        edit(&mut studio, &paths.hue.set(Some(40.0))).unwrap();
        assert_eq!(theme(&studio, &id).hue, Some(40.0));
        assert!(
            edit(&mut studio, &paths.chroma.set(0.2)).is_err(),
            "too much colour"
        );
        assert!(edit(&mut studio, &paths.window_radius.set(90.0)).is_err());
        assert!(edit(&mut studio, &paths.density.set(f64::NAN)).is_err());
        assert!(
            edit(&mut studio, &Rung::Tertiary.path(&paths).set(0.1)).is_err(),
            "ink may not cross to the surfaces' side"
        );
        edit(&mut studio, &Rung::Content.path(&paths).set(0.24)).unwrap();
        assert!((theme(&studio, &id).surfaces.content - 0.24).abs() < 1e-6);
        assert!(
            edit(&mut studio, &paths.dark.set(false)).is_err(),
            "polarity is switched, never set alone"
        );
        let nobody = Studio::paths().themes.item(&Id::from_raw("nobody"));
        assert!(edit(&mut studio, &nobody.name.replace("x")).is_err());
    }

    #[test]
    fn emphasis_polarity_and_findings_follow_the_edits() {
        let (mut studio, id) = studio_with("dark");
        let paths = Studio::paths().themes.item(&id);
        edit(&mut studio, &paths.tint.set(Some(30.0))).unwrap();
        assert_eq!(
            theme(&studio, &id).document("mine").emphasis,
            Emphasis::Tinted { hue: 30.0 }
        );
        edit(&mut studio, &paths.tint.set(None)).unwrap();
        assert_eq!(
            theme(&studio, &id).document("mine").emphasis,
            Emphasis::Inverted
        );

        studio
            .apply(Studio::SWITCH_POLARITY.with((id.clone(), false)))
            .unwrap();
        let light = theme(&studio, &id);
        assert!(!light.dark);
        assert_eq!(light.name, "Dark copy", "switching keeps the name");
        assert_eq!(light.findings("mine"), Vec::<String>::new());

        edit(&mut studio, &Rung::Tertiary.path(&paths).set(0.80)).unwrap();
        let findings = theme(&studio, &id).findings("mine");
        assert!(
            findings.iter().any(|f| f.starts_with("text_tertiary")),
            "{findings:?}"
        );
    }

    #[test]
    fn two_people_tuning_different_roles_merge() {
        let (mut ours, id) = studio_with("dark");
        let mut theirs = ours.fork();
        let paths = Studio::paths().themes.item(&id);
        edit(&mut ours, &paths.density.set(1.2)).unwrap();
        edit(&mut theirs, &Rung::Content.path(&paths).set(0.24)).unwrap();
        ours.sync_with(&mut theirs).unwrap();
        let merged = theme(&ours, &id);
        assert!((merged.density - 1.2).abs() < 1e-6);
        assert!((merged.surfaces.content - 0.24).abs() < 1e-6);
        assert_eq!(ours.state().unwrap(), theirs.state().unwrap());
    }

    #[test]
    fn a_new_theme_starts_from_a_preset_or_a_theme_and_ids_are_unique() {
        let (mut studio, id) = studio_with("light");
        edit(
            &mut studio,
            &Studio::paths().themes.item(&id).density.set(1.2),
        )
        .unwrap();
        studio
            .apply(Studio::NEW_THEME.with(("second".to_owned(), "mine".to_owned())))
            .unwrap();
        let second = theme(&studio, &Id::from_raw("second"));
        assert!((second.density - 1.2).abs() < 1e-6);
        for (new, base) in [("mine", "dark"), ("dark", "dark"), ("third", "nord")] {
            assert!(
                studio
                    .apply(Studio::NEW_THEME.with((new.to_owned(), base.to_owned())))
                    .is_err(),
                "{new} from {base}"
            );
        }
        studio.apply_confirmed(Studio::DELETE.with(id)).unwrap();
        assert_eq!(studio.state().unwrap().themes.len(), 1);
    }
}
