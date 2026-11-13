//! Theme Studio: write theme documents and make one the system's.

// Apps run isolated, so app-local arithmetic checks are relaxed; a fault in a shared crate reaches the session, so its lints stay.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local arithmetic; see the note above"
)]
pub mod apply;
pub mod draw;
pub mod effects;
pub mod locales;
pub mod preview;
pub mod studio;
mod view;

use ui::prelude::*;

pub use studio::Studio;

pub const APP_ID: &str = "so.tilo.themes";
pub const APP_TITLE: &str = "Theme Studio";

pub const GALLERY_SCENARIO: &str = "gallery";
pub const SCENARIO_THEME: &str = "ember";

/// The studio: the themes you write, over the presets Tilo ships.
pub struct App;

impl ResourceApp for App {
    type Resource = Studio;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = APP_TITLE;
    const TITLE_KEY: &'static str = "themes-app-title";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("themes");
    fn view(cx: &mut ViewCx<Studio>) -> Element {
        view::root(cx)
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            GALLERY_SCENARIO,
            "editor",
            "editor-list",
            "editor-dialog",
            "tinted",
            "illegible",
        ]
    }

    /// `editor` opens a theme written from the warm graphite preset; `tinted` gives it a
    /// tinted emphasis and a rounder, roomier shape; `illegible` lifts its tertiary ink
    /// until it misses its floor.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Studio>> {
        let theme = Studio::paths().themes.item(&Id::from_raw(SCENARIO_THEME));
        let start = Studio::NEW_THEME.with((SCENARIO_THEME.to_owned(), "dark-graphite".to_owned()));
        let edits = match name {
            Some("editor" | "editor-list" | "editor-dialog") => {
                vec![theme.name.replace("Ember")]
            }
            Some("tinted") => vec![
                theme.name.replace("Ember"),
                theme.tint.set(Some(40.0)),
                theme.window_radius.set(22.0),
                theme.density.set(1.2),
            ],
            Some("illegible") => vec![
                theme.name.replace("Ember"),
                studio::Rung::Tertiary.path(&theme).set(0.42),
            ],
            _ => return Vec::new(),
        };
        std::iter::once(start)
            .chain(edits.iter().filter_map(|edit| Invoke::edit(edit).ok()))
            .collect()
    }
}

ui::dev_reload_entry!(App);

ui::reach_tests!(App);

/// German is one of the languages the studio ships, and its window is named there too: the
/// key the framework resolves the title through reads "Design-Studio".
#[cfg(test)]
mod window_name_tests {
    #[test]
    fn the_window_is_named_in_german() {
        let catalog = &crate::locales::CATALOG;
        catalog.set_locale("de-DE");
        assert_eq!(
            catalog.t(<super::App as ui::framework::ResourceApp>::TITLE_KEY),
            "Design-Studio",
            "a German window is titled in German"
        );
        catalog.set_locale("en-US");
        assert_eq!(
            catalog.t(<super::App as ui::framework::ResourceApp>::TITLE_KEY),
            "Theme Studio"
        );
    }
}
