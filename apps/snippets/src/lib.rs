//! Snippets — a text & code snippet manager for qOS.
//!
//! A searchable, taggable library of reusable snippets. Each snippet has a
//! title, language, tags and a body that may contain `${placeholder}`
//! variables; the detail pane shows a syntax-highlighted preview, and the
//! expand-with-variables flow fills the placeholders before copying to the
//! clipboard. The library is a CRDT object (see [`doc`]) so it syncs across
//! devices and collaborators converge on one shared set of snippets.
//!
//! The single integration point is [`app_root`], used by both the live
//! event loop and the `--shots` offscreen renderer in `main.rs`.

pub mod actions;
pub mod app;
pub mod doc;
pub mod draw;
pub mod expand;
pub mod highlight;
pub mod model;
pub mod seed;

pub use app::SnippetsApp;
pub use model::{Language, Library, Selection, Snippet, SnippetId, SmartFolder};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(SnippetsApp::new())
}

/// Like [`app_root`] but with a named interaction scenario staged for a
/// single headless screenshot (the live `--shots` renderer paints one frame
/// and can't reach toggled states through input). Recognised scenarios:
///
/// * `"narrow"` — pair with a sub-breakpoint `--viewport` to show the
///   primary sidebar as an open overlay flap over the content + scrim.
/// * `"detail"` — pair with a very-narrow `--viewport` to show the
///   single-pane push stack on the detail with its Back affordance.
///
/// An empty / unknown name yields the same frame as [`app_root`].
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    // The `empty` scenario captures the honest live zero-state (a fresh
    // install has no snippets); every other scenario stages the seeded demo
    // library so the capture shows the manager doing real work.
    let mut app = if scenario == "empty" {
        SnippetsApp::new()
    } else {
        SnippetsApp::new_demo()
    };
    app.seed_shot_scenario(scenario);
    Box::new(app)
}
