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
pub mod proposal;
#[cfg(feature = "samples")]
mod samples;
mod view;

use std::cell::RefCell;
use std::rc::Rc;

use ui::prelude::*;

pub use model::{Draft, Program, Workbench};

/// What the app was handed that shows repeated work: the actions the person
/// invoked in this window, in order ([`proposal::Occurrence`]). Code driving
/// the app hands it a log; a window the person opened sees none, because
/// nothing else keeps one (Tilo has no hidden memory store).
#[derive(Clone, Default)]
pub struct Observed {
    invocations: Rc<RefCell<Vec<proposal::Occurrence>>>,
}

impl Observed {
    /// Read `text` as the log of the actions taken, one action per line:
    /// `app/resource-kind/action`, then ` on <resource>` and ` as <label>`
    /// when known. Lines that do not read are skipped.
    #[must_use]
    pub fn from_log(text: &str) -> Self {
        let seen = Self::default();
        seen.record(text);
        seen
    }

    /// Add `text` to the log, one action per line.
    pub fn record(&self, text: &str) {
        let mut invocations = self.invocations.borrow_mut();
        for line in text.lines() {
            if let Some(occurrence) = parse_occurrence(line) {
                invocations.push(occurrence);
            }
        }
        let excess = invocations.len().saturating_sub(proposal::MAX_OBSERVED);
        invocations.drain(..excess);
    }

    /// Every action taken, oldest first.
    #[must_use]
    pub fn invocations(&self) -> Vec<proposal::Occurrence> {
        self.invocations.borrow().clone()
    }
}

/// One line of an action log:
/// `app/resource-kind/action[ on <resource>][ as <label>]`.
fn parse_occurrence(line: &str) -> Option<proposal::Occurrence> {
    let line = line.trim();
    let (key, rest) = line.split_once(" on ").unwrap_or((line, ""));
    let mut parts = key.split('/');
    let (app, kind, action) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some()
        || [app, kind, action]
            .iter()
            .any(|part| part.trim().is_empty())
    {
        return None;
    }
    let (resource, label) = rest.split_once(" as ").unwrap_or((rest, ""));
    let word = |text: &str| {
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    };
    Some(proposal::Occurrence {
        app: app.trim().to_owned(),
        kind: kind.trim().to_owned(),
        action: action.trim().to_owned(),
        resource: word(resource),
        label: word(label),
    })
}

/// The log a drive of the app was handed, when the scenario says the person
/// has been repeating work: `--shots <scenario>` reads
/// `TILO_SHORTCUTS_OBSERVED` as its lines. A window the person opens is
/// handed nothing.
#[must_use]
pub fn observed_from_environment(name: Option<&str>) -> Observed {
    let _ = name;
    Observed::default()
}

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

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        samples::SCENARIOS
    }

    #[cfg(feature = "samples")]
    fn scenario(name: Option<&str>) -> Vec<Invoke<Workbench>> {
        samples::drafts(name)
    }
}

ui::reach_tests!(App);
