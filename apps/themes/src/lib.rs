//! Themes — qOS native theme studio (CRDT-native, built on the `ui`
//! toolkit). Browse preset themes, customise the OS token set live, and
//! create / duplicate / export your own.
//!
//! `app_root()` is the single integration point used by both the live
//! winit event loop and the `--shots` offscreen renderer.

pub mod actions;
pub mod app;
pub mod customizer;
pub mod doc;
pub mod draw;
pub mod gallery;
pub mod model;
pub mod preview;
pub mod store;
pub mod widgets;

pub use app::ThemesApp;

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(ThemesApp::new())
}

/// Build the root widget tree for a headless `--shots` capture at the given
/// viewport width. The single-frame renderer has no live first frame to settle
/// the responsive layout, so the layout class (docked vs. overlay) is seeded
/// from `width` here and the gallery flap is snapped to its endpoint. A narrow
/// width therefore captures the collapsed gallery floating over its scrim.
pub fn app_root_for_shot(width: u32) -> Box<dyn Widget> {
    // Optional scenario for the capture (sidebar menu, narrow scroll, font
    // change), selected via the `QOS_THEMES_SHOT` env var so a single binary
    // renders every review still. `live` captures the honest live default (the
    // presets-only catalog, no fabricated custom theme); every other scenario
    // uses the seeded demo catalog so the customizer shows real editing.
    let scenario = std::env::var("QOS_THEMES_SHOT").unwrap_or_default();
    let mut app = if scenario == "live" {
        ThemesApp::new()
    } else {
        ThemesApp::new_demo()
    };
    app.seed_shot(width as f64, &scenario);
    Box::new(app)
}
