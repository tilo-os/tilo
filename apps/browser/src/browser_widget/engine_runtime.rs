//! The engine this window browses with: which runtime is live, why it is not,
//! and what the page area offers a person when it is missing or failed.
//!
//! There is no simulated page: live browsing runs the Chromium engine runtime,
//! and a window without one says so instead of inventing pixels.

use super::{BrowsingMode, SharedEngine, megabytes};
use ui::icons::Icon;
use ui::tilo_actions::ActionBinding;
use ui::widget::{Column, EmptyState, Widget};

/// Why live browsing has no running engine, in the terms the page area
/// shows a person. What a developer needs goes to the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EngineProblem {
    /// The Chromium engine runtime is not installed.
    NotInstalled,
    /// The engine is installed but did not start.
    Failed,
}

/// Exceptional repair of the offline engine baseline. Store resolves the
/// installed Browser root and reviews its exact declared optional component;
/// opening this handoff never consents to a payload download.
pub(crate) const ENGINE_INSTALL_URI: &str =
    "tilo-store:runtime/so.tilo.browser/tilo-runtime/2026.10.23-chromium";

/// Live browsing runs the Chromium engine runtime
/// ([`ui::webview::chromium::runtime`]); there is no other engine and no
/// simulated page. `TILO_BROWSER_PAINT=1` draws its pages from the paint
/// stream (the engine's layout as a display list, drawn by this toolkit)
/// instead of its pixels: what a web client of this host sees. Either way the
/// session can share the page as that stream.
pub(super) fn default_engine(mode: BrowsingMode) -> (SharedEngine, Option<EngineProblem>) {
    #[cfg(test)]
    if let Some(engine) = test_window_engine::take() {
        return (engine, None);
    }
    let (engine, problem) = native_engine(mode);
    if problem.is_some() {
        return (engine, problem);
    }
    if std::env::var("TILO_BROWSER_PAINT").as_deref() == Ok("1") {
        return (ui::webview::paint::PaintEngine::new(engine), None);
    }
    // Drawn from pixels here; captured as a display list only while the
    // session shares the window with someone watching (a web member).
    (ui::webview::paint::PaintEngine::streaming(engine), None)
}

/// Deterministic rendering is confined to `new_headless`.
fn native_engine(mode: BrowsingMode) -> (SharedEngine, Option<EngineProblem>) {
    if let Err(missing) = ui::webview::chromium::runtime::locate() {
        tracing::warn!(hint = %missing.developer_hint(), "browser engine is not installed");
        return (
            unavailable_engine(mode, EngineProblem::NotInstalled),
            Some(EngineProblem::NotInstalled),
        );
    }
    let started = if mode.is_private() {
        crate::storage::explicit_root()
            .ok_or_else(|| "Private browser root was not configured".to_string())
            .and_then(ui::webview::chromium::ChromiumEngine::new_private)
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

pub(super) fn engine_or_error(
    result: Result<SharedEngine, String>,
) -> (SharedEngine, Option<EngineProblem>) {
    match result {
        Ok(engine) => (engine, None),
        Err(error) => {
            tracing::error!(%error, "browser engine failed to start");
            (
                ui::webview::chromium::UnavailableEngine::new(user_message(EngineProblem::Failed)),
                Some(EngineProblem::Failed),
            )
        }
    }
}

/// An engine that renders nothing and answers navigation with the problem.
/// A private window's disposable root is released at once: nothing will
/// ever write to it.
pub(super) fn unavailable_engine(mode: BrowsingMode, problem: EngineProblem) -> SharedEngine {
    if mode.is_private()
        && let Some(root) = crate::storage::explicit_root()
    {
        ui::webview::chromium::release_private_root(root);
    }
    ui::webview::chromium::UnavailableEngine::new(user_message(problem))
}

fn user_message(problem: EngineProblem) -> String {
    match problem {
        EngineProblem::NotInstalled => ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE,
        EngineProblem::Failed => ui::webview::chromium::runtime::FAILED_MESSAGE,
    }
    .to_owned()
}

/// The page area while the engine is missing or stopped: what happened,
/// and the one thing a person can do about it.
pub(super) fn engine_problem_state(
    problem: EngineProblem,
    install: Option<ActionBinding<()>>,
    setup: Option<&crate::engine_setup::Setup>,
) -> Box<dyn Widget> {
    if let (EngineProblem::NotInstalled, Some(setup)) = (problem, setup) {
        return engine_setup_state(setup, install);
    }
    let state = match problem {
        EngineProblem::NotInstalled => {
            let state = EmptyState::new(ui::webview::chromium::runtime::NOT_INSTALLED_MESSAGE)
                .with_icon(Icon::Download)
                .with_message(ui::i18n!("browser-engine-repair-message"))
                .with_action(ui::i18n!("browser-engine-repair-action"))
                .with_action_id("browser.install-engine");
            match install {
                Some(binding) => state.on_action(binding),
                None => state,
            }
        }
        EngineProblem::Failed => EmptyState::new(ui::webview::chromium::runtime::FAILED_MESSAGE)
            .with_icon(Icon::Info)
            .with_message(ui::i18n!("browser-engine-reopen")),
    };
    Box::new(
        Column::new()
            .with_no_padding()
            .with_gap(0.0)
            .with_fill_remaining_height()
            .push(Box::new(state)),
    )
}

/// The page area while the host fetches the engine: what is happening and
/// how far along, in the toolkit's own empty state and progress bar.
pub(super) fn engine_setup_state(
    setup: &crate::engine_setup::Setup,
    retry: Option<ActionBinding<()>>,
) -> Box<dyn Widget> {
    use crate::engine_setup::Setup;
    use ui::webview::chromium::install::InstallPhase;
    let mut column = Column::new()
        .with_no_padding()
        .with_gap(0.0)
        .with_fill_remaining_height();
    match setup {
        Setup::Fetching {
            phase,
            received,
            total,
        } => {
            let message = match (phase, total) {
                (InstallPhase::Preparing, _) => "Preparing…".to_owned(),
                (InstallPhase::Installing, _) => "Installing…".to_owned(),
                (InstallPhase::Downloading, Some(total)) => format!(
                    "Downloading, {} of {}",
                    megabytes(*received),
                    megabytes(*total)
                ),
                (InstallPhase::Downloading, None) => {
                    format!("Downloading, {}", megabytes(*received))
                }
            };
            column = column.push(Box::new(
                EmptyState::new("Getting the browser engine")
                    .with_icon(Icon::Download)
                    .with_message(message)
                    .busy(),
            ));
            let bar = setup.fraction().map_or_else(
                ui::widget::ProgressBar::indeterminate,
                ui::widget::ProgressBar::new,
            );
            column = column.push(Box::new(bar.with_label("Browser engine")));
        }
        Setup::Reopened => {
            column = column.push(Box::new(
                EmptyState::new("The browser engine is ready")
                    .with_icon(Icon::Info)
                    .with_message("Browser opened in a new window. You can close this one."),
            ));
        }
        Setup::Failed(error) => {
            let state = EmptyState::error("Couldn’t get the browser engine", error)
                .with_icon(Icon::Download)
                .with_action("Try again")
                .with_action_id("browser.install-engine");
            column = column.push(Box::new(match retry {
                Some(binding) => state.on_action(binding),
                None => state,
            }));
        }
    }
    Box::new(column)
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
