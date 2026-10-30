//! The engine this window browses with: which runtime is live, and what the
//! page area shows a person when it did not start.
//!
//! There is no simulated page: live browsing runs the Chromium engine runtime,
//! and a window without one says so instead of inventing pixels. The engine
//! is a runtime Browser requires: it installs and updates with Browser, and
//! the launcher refuses to start Browser without it. So a window without a
//! running engine is an engine that failed to start, or a broken install;
//! either way what a developer needs goes to the log.

use super::{BrowsingMode, SharedEngine};
use ui::icons::Icon;
use ui::widget::{Column, EmptyState, Widget};

/// Live browsing runs the Chromium engine runtime
/// ([`ui::webview::chromium::runtime`]); there is no other engine and no
/// simulated page. `TILO_BROWSER_PAINT=1` draws its pages from the paint
/// stream (the engine's layout as a display list, drawn by this toolkit)
/// instead of its pixels: what a web client of this host sees. Either way the
/// session can share the page as that stream.
///
/// Returns the engine and whether it failed to start.
pub(super) fn default_engine(mode: BrowsingMode) -> (SharedEngine, bool) {
    #[cfg(test)]
    if let Some(engine) = test_window_engine::take() {
        return (engine, false);
    }
    let (engine, failed) = native_engine(mode);
    if failed {
        return (engine, true);
    }
    if std::env::var("TILO_BROWSER_PAINT").as_deref() == Ok("1") {
        return (ui::webview::paint::PaintEngine::new(engine), false);
    }
    // Drawn from pixels here; captured as a display list only while the
    // session shares the window with someone watching (a web member).
    (ui::webview::paint::PaintEngine::streaming(engine), false)
}

/// Deterministic rendering is confined to `new_headless`.
fn native_engine(mode: BrowsingMode) -> (SharedEngine, bool) {
    if let Err(missing) = ui::webview::chromium::runtime::locate() {
        // Browser requires the engine, so installing or updating it brought
        // the engine and the launcher would not have started it without:
        // this is a broken install or a run outside the launcher.
        tracing::error!(
            hint = %missing.developer_hint(),
            "the browser engine Browser requires is missing"
        );
        return (unavailable_engine(mode), true);
    }
    let started = if mode.has_own_profile() {
        own_profile_root(mode)
            .and_then(|root| ui::webview::chromium::ChromiumEngine::new_private(&root))
            .map(|engine| engine as SharedEngine)
    } else if let Some(root) = crate::storage::explicit_root() {
        ui::webview::chromium::ChromiumEngine::with_profile_extensions(
            &root.join("chromium"),
            &crate::extensions::startup_paths(),
        )
        .map(|engine| engine as SharedEngine)
    } else {
        ui::webview::chromium::ChromiumEngine::new_with_extensions(
            &crate::extensions::startup_paths(),
        )
        .map(|engine| engine as SharedEngine)
    };
    engine_or_error(started)
}

/// The fresh profile a window of `mode` browses on, its own and removed
/// with it: a private window's is the process's private root, which also
/// holds what else it keeps until it closes; a visitor window's is one made
/// for its engine alone, beside the person's own profile and sharing
/// nothing with it, so none of their cookies or sign-ins reach its pages.
fn own_profile_root(mode: BrowsingMode) -> Result<std::path::PathBuf, String> {
    match mode {
        BrowsingMode::Private => crate::storage::explicit_root()
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| "Private browser root was not configured".to_string()),
        BrowsingMode::Visitor => crate::storage::allocate_private_root(),
        BrowsingMode::Normal => Err("The person's window browses on their own profile".into()),
    }
}

/// The started engine, or one that answers with the failure, and whether it
/// failed.
pub(super) fn engine_or_error(result: Result<SharedEngine, String>) -> (SharedEngine, bool) {
    match result {
        Ok(engine) => (engine, false),
        Err(error) => {
            tracing::error!(%error, "browser engine failed to start");
            (failed_engine(), true)
        }
    }
}

fn failed_engine() -> SharedEngine {
    ui::webview::chromium::UnavailableEngine::new(
        ui::webview::chromium::runtime::FAILED_MESSAGE.to_owned(),
    )
}

/// An engine that renders nothing and answers navigation with the failure.
/// A private window's disposable root is released at once: nothing will
/// ever write to it.
pub(super) fn unavailable_engine(mode: BrowsingMode) -> SharedEngine {
    if mode.is_private()
        && let Some(root) = crate::storage::explicit_root()
    {
        ui::webview::chromium::release_private_root(root);
    }
    failed_engine()
}

/// The page area while the engine is not running: what happened, and what a
/// person can do about it.
pub(super) fn engine_failed_state() -> Box<dyn Widget> {
    let state = EmptyState::new(ui::webview::chromium::runtime::FAILED_MESSAGE)
        .with_icon(Icon::Info)
        .with_message(ui::i18n!("browser-engine-failed"));
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .with_fill_remaining_height()
            .push(Box::new(state)),
    )
}

/// The engine a test's next live window drives instead of the native one,
/// so a test can run Browser in its real app host without starting a
/// browser process.
#[cfg(test)]
pub(crate) mod test_window_engine {
    use std::cell::RefCell;

    use ui::webview::SharedEngine;

    thread_local! {
        static NEXT: RefCell<Option<SharedEngine>> = const { RefCell::new(None) };
    }

    /// The next live window on this thread drives `engine`.
    pub(crate) fn install(engine: SharedEngine) {
        NEXT.with_borrow_mut(|next| *next = Some(engine));
    }

    pub(super) fn take() -> Option<SharedEngine> {
        NEXT.with_borrow_mut(Option::take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window opened for an agent browses on a profile made for it alone:
    /// a fresh directory Browser owns and removes, never the person's
    /// profile, so none of their cookies or sign-ins reach its pages, and
    /// never another window's.
    #[test]
    fn a_visitor_window_browses_on_a_fresh_profile_of_its_own() {
        let first = own_profile_root(BrowsingMode::Visitor).expect("a visitor's profile");
        let second = own_profile_root(BrowsingMode::Visitor).expect("another visitor's profile");
        for root in [&first, &second] {
            assert!(
                ui::webview::chromium::is_owned_private_root(root),
                "{} is a root Browser owns and removes",
                root.display()
            );
            let empty = std::fs::read_dir(root)
                .unwrap()
                .flatten()
                .all(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()));
            assert!(empty, "nothing but its markers: no cookies, no sign-ins");
            if let Some(person) = crate::storage::data_dir() {
                assert!(!root.starts_with(&person), "{}", root.display());
            }
        }
        assert_ne!(first, second, "each window has its own");
        assert!(own_profile_root(BrowsingMode::Normal).is_err());
        for root in [first, second] {
            ui::webview::chromium::release_private_root(&root);
            assert!(!root.exists(), "removed once no engine uses it");
        }
    }
}
