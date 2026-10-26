//! Theme Studio: write theme documents and make one the system's.
//!
//! A theme is a `ui::theme::ThemeDocument` — palette, emphasis, presence
//! tone, window radius, density, type, motion and the floating material —
//! and nothing else, so a theme written here cannot restyle an app's
//! internals. The studio previews every preset and every theme you write,
//! measures each against the contrast floors as you edit, and refuses to
//! apply or export one that misses a floor.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it.
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

/// The application id.
pub const APP_ID: &str = "so.tilo.themes";
/// The window and launcher title.
pub const APP_TITLE: &str = "Theme Studio";

/// The `--shots` scenario that shows the preset gallery.
pub const GALLERY_SCENARIO: &str = "gallery";
/// The theme every other `--shots` scenario writes and opens.
pub const SCENARIO_THEME: &str = "ember";

/// The studio: the themes you write, over the presets Tilo ships.
pub struct App;

impl ResourceApp for App {
    type Resource = Studio;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("themes");
    fn view(cx: &mut ViewCx<Studio>) -> Element {
        view::root(cx)
    }

    fn scenarios() -> &'static [&'static str] {
        &[GALLERY_SCENARIO, "editor", "tinted", "illegible"]
    }

    /// `editor` opens a theme written from the warm graphite preset;
    /// `tinted` gives it a tinted emphasis and a rounder, roomier shape;
    /// `illegible` lifts its tertiary ink until it misses its floor.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Studio>> {
        let theme = Studio::paths().themes.item(&Id::from_raw(SCENARIO_THEME));
        let start = Studio::NEW_THEME.with((SCENARIO_THEME.to_owned(), "dark-graphite".to_owned()));
        let edits = match name {
            Some("editor") => vec![theme.name.replace("Ember")],
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

#[cfg(test)]
mod reach_tests {
    use ui::framework::ResourceApp as _;
    use ui::framework::testing::{TestApp, unreachable_actions};

    /// Every action the app publishes is reachable by address in a drive of
    /// each of its scenarios: none is advertised yet handled only by a
    /// click. The scenarios stage fixtures; the app someone opens acts on
    /// this machine (its files, its processes), so it is not walked.
    #[test]
    fn a_drive_of_every_scenario_reaches_every_published_action() {
        for scenario in super::App::scenarios() {
            let mut app = TestApp::<super::App>::drive(scenario)
                .unwrap_or_else(|error| panic!("{scenario}: {error}"));
            assert_eq!(
                unreachable_actions(&mut app),
                Vec::<String>::new(),
                "{scenario}: published actions a drive cannot invoke"
            );
        }
    }
}
