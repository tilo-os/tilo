//! Shortcuts: build and run pipelines of the actions your apps publish.
//!
//! A shortcut is a small program whose every step is an ordinary published
//! action (see `tilo-automation`). This app lists the saved shortcuts of
//! the workspace, runs them, and edits drafts: an ordered list of steps
//! chosen from the live action catalogue, each argument bound to a typed
//! value, an input, a variable or an earlier step's result — only the
//! choices the shortcut checker accepts are offered. Saving shows the
//! permissions the shortcut will use before it is stored.
//!
//! Drafts are this app's resource ([`model::Workbench`]); saving, running and
//! reloading are its published effects ([`door`]); both reach every caller
//! of the action door, not only the buttons.

// Apps run in isolated processes, so app-local arithmetic checks are relaxed.
#![allow(
    clippy::arithmetic_side_effects,
    reason = "app-local counters and indices; see the note above"
)]

pub mod door;
pub mod drafting;
pub mod editor;
pub mod locales;
pub mod model;
mod samples;
mod view;

use ui::prelude::*;

pub use model::{Draft, Program, Workbench};

/// The application id. The session publishes the shortcut library under
/// the same id, which is what makes it this app's own.
pub const APP_ID: &str = tilo_automation::SHORTCUTS_APP_ID;
/// The window and launcher title.
pub const APP_TITLE: &str = "Shortcuts";

pub struct App;

/// The app as every host mounts it.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(App)
}

impl ResourceApp for App {
    type Resource = Workbench;
    const ID: &'static str = APP_ID;
    const TITLE: &'static str = APP_TITLE;

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (1180, 760)
    }

    fn view(cx: &mut ViewCx<Workbench>) -> Element {
        view::root(cx)
    }

    fn scenarios() -> &'static [&'static str] {
        samples::SCENARIOS
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Workbench>> {
        samples::drafts(name)
    }
}

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
