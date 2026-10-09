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
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
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
    const ICON: Option<&'static str> = Some("themes");
    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1280, 800)
    }

    fn view(cx: &mut ViewCx<Studio>) -> Element {
        locales::install();
        view::root(cx)
    }

    fn scenarios() -> &'static [&'static str] {
        &[GALLERY_SCENARIO, "editor", "tinted", "illegible"]
    }

    /// `editor` opens a theme written from the warm graphite preset;
    /// `tinted` gives it a tinted emphasis and a rounder, roomier shape;
    /// `illegible` lifts its tertiary ink until it misses its floor.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Studio>> {
        let theme = || Id::from_raw(SCENARIO_THEME);
        let start = Studio::NEW_THEME.with((SCENARIO_THEME.to_owned(), "dark-graphite".to_owned()));
        let rename = Studio::RENAME.with((theme(), String::from("Ember")));
        match name {
            Some("editor") => vec![start, rename],
            Some("tinted") => vec![
                start,
                rename,
                Studio::SET_TINT_HUE.with((theme(), 40.0)),
                Studio::SET_WINDOW_RADIUS.with((theme(), 22.0)),
                Studio::SET_DENSITY.with((theme(), 1.2)),
            ],
            Some("illegible") => vec![
                start,
                rename,
                Studio::SET_LIGHTNESS.with((theme(), String::from("tertiary"), 0.42)),
            ],
            _ => Vec::new(),
        }
    }
}

ui::dev_reload_entry!(App);
