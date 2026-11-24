//! The app on the session resource: `diffmerge-model` keeps the model and its
//! verbs; here the app draws it and stages its samples.

use std::rc::Rc;

use ui::framework::{ResourceApp, ViewCx};
use ui::i18n::EmbeddedCatalog;
use ui::tilo_resource::Invoke;
use ui::widget::{Element, NavShell};

use crate::app::DiffMergeApp;
use crate::chrome::Chrome;

pub use diffmerge_model::actions::diff_block_fingerprint;
pub use diffmerge_model::{DiffmergeEffect, HunkChoice, Session, session};

pub fn sample_invocations() -> [Invoke<Session>; 2] {
    let (left, right) = crate::samples::diff_pair();
    let (base, ours, theirs) = crate::samples::merge_triple();
    [
        Session::OPEN_COMPARISON.with(session::OpenComparison {
            left_label: left.label,
            left: left.text,
            right_label: right.label,
            right: right.text,
        }),
        Session::OPEN_MERGE.with(session::OpenMerge {
            base_label: base.label,
            base: base.text,
            ours_label: ours.label,
            ours: ours.text,
            theirs_label: theirs.label,
            theirs: theirs.text,
        }),
    ]
}

pub struct DiffmergeShell;

impl ResourceApp for DiffmergeShell {
    type Resource = Session;
    const ID: &'static str = ui::apps::DIFFMERGE;
    // Each window shows what it opened, never another window's.
    const WINDOW_RESOURCE: bool = true;
    const TITLE: &'static str = "Diff & Merge";
    const TITLE_KEY: &'static str = "diffmerge-app-title";
    const ICON: Option<&'static str> = Some("diffmerge");
    fn label(state: &Session) -> String {
        state.title()
    }

    fn view(cx: &mut ViewCx<Session>) -> Element {
        let chrome = Chrome::new();
        let mut comparison = DiffMergeApp::new();
        comparison.attach_chrome(chrome.clone());
        comparison.attach(
            cx.live().clone(),
            cx.dispatcher(),
            cx.waker(),
            cx.read_only_state(),
        );
        let store = comparison.shared();
        let effects = Rc::clone(&store);
        cx.effects(move |effect: DiffmergeEffect, cx| crate::app::answer(&effects, effect, cx));
        cx.on_stage(move |scenario| {
            store
                .borrow_mut()
                .ask(crate::app::Asked::Stage(scenario.map(str::to_owned)));
        });
        Box::new(
            NavShell::new()
                .content_kind(ui::widget::ContentKind::Document)
                .content(Box::new(comparison))
                .content_header("", chrome.actions()),
        )
    }

    fn catalog() -> Option<&'static EmbeddedCatalog> {
        Some(crate::i18n::catalogs())
    }

    fn scenarios() -> &'static [&'static str] {
        &["empty", "diff", "merge", "folders", "error", "status"]
    }

    /// Every scenario but the empty one shows the bundled samples; the folder listing
    /// shows the sample tree comparison instead.
    fn scenario(name: Option<&str>) -> Vec<Invoke<Session>> {
        if name == Some("empty") {
            return Vec::new();
        }
        sample_invocations().into()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
tilo_app_test_host::reach_tests!(DiffmergeShell);
