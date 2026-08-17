//! Feeds — a CRDT-native RSS/Atom/Gemini reader for qOS.
//!
//! Architecture (mirrors the notes app):
//!   * [`model`]  — pure data + the in-memory [`model::Store`].
//!   * [`doc`]    — Automerge mapping; converging state lives here.
//!   * [`sample`] — bundled offline fixture used by `--shots` + first run.
//!   * [`fetch`]  — off-thread HTTP (reqwest + feed-rs) and gemini (TLS).
//!   * [`html`] / [`gemtext`] — body → renderable [`model::ContentBlock`]s.
//!   * [`opml`]   — subscription import/export.
//!   * [`app`]    — the three-pane root [`app::FeedsApp`] widget.
//!
//! `app_root()` is the single integration point: it returns the root widget
//! used by both the live `ui::App` event loop and the `--shots` offscreen
//! renderer.

pub mod actions;
pub mod app;
pub mod doc;
pub mod fetch;
pub mod gemtext;
pub mod html;
pub mod icons;
pub mod modal;
pub mod model;
pub mod opml;
pub mod portal;
pub mod reader_theme;
pub mod sample;
pub mod search_provider;

pub use app::FeedsApp;
pub use model::{Article, Feed, FeedKind, Filter, Folder, Store};

use ui::widget::Widget;

/// Build and return the app's root widget tree.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(FeedsApp::new())
}

/// Like [`app_root`] but staged for a named `--shots` scenario. Used to
/// capture interaction states the single-frame headless renderer can't reach
/// through live input:
///
/// * `"sidebar-open"` — pre-open the subscriptions sidebar, so a narrow
///   viewport (`--viewport 560x800`) captures the overlay flap floating over
///   the dimmed content with its close affordance.
/// * `"detail"` — push the reader (detail) in, so a narrow viewport captures
///   the single-pane master→detail reader with its Back affordance.
///
/// Any other (or empty) scenario yields the same default frame as
/// [`app_root`].
pub fn app_root_for_shot(scenario: &str) -> Box<dyn Widget> {
    use crate::model::{ReadFont, ReadTheme};
    // The `empty` scenario renders the real live app (empty on a fresh machine)
    // so the first-run onboarding state can be screenshotted — no sample data.
    if scenario == "empty" {
        return Box::new(FeedsApp::new());
    }
    let mut app = FeedsApp::new_demo();
    match scenario {
        "sidebar-open" => app.snap_sidebar_open(true),
        "detail" => app.force_detail_pushed(),
        // Reading-settings popover open over a serif, sepia reader page so the
        // shot shows the font/size/theme controls and the serif body together.
        "reader-settings" => {
            app.set_read_font(ReadFont::Serif);
            app.set_read_theme(ReadTheme::Sepia);
            app.force_reader_settings_open();
        }
        // A "Refresh all" mid-flight: the per-feed progress strip + bar.
        "refresh" => app.force_refresh_in_progress(),
        // Long feed titles to verify the sidebar elides to a single line with a
        // fade instead of wrapping into the next row.
        "long-titles" => app.stage_long_sidebar_titles(),
        // The per-feed right-click context menu open over the sidebar (the
        // single-frame renderer can't reach it through live input).
        "context-menu" => app.force_feed_menu_open(),
        // The per-article right-click context menu (copy / open / read /
        // favourite) open over the article list.
        "article-menu" => app.force_article_menu_open(),
        // The reader pane holding keyboard focus (its accent focus ring).
        "reader-focus" => app.force_reader_focus(),
        // The "mark all as read" confirmation dialog before it runs.
        "mark-all-read" => app.force_mark_all_read_confirm(),
        // The subscriptions list after an OPML import folds new folders + feeds
        // into the sidebar.
        "post-import" => app.stage_post_import(),
        // The Add-subscription dialog open over the reader, with a URL typed
        // into the real TextInput (shared caret / clipboard core).
        "add-subscription" => app.stage_add_subscription("https://blog.rust-lang.org/feed.xml"),
        // The Add-subscription dialog showing an honest inline error for a bad
        // entry (the field stays usable).
        "add-error" => {
            app.stage_add_subscription_error("not a real url", "That doesn't look like a URL — try https://example.com/feed.xml")
        }
        _ => {}
    }
    Box::new(app)
}
