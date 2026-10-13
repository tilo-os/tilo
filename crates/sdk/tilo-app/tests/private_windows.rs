#![allow(clippy::unwrap_used, reason = "test assertions")]

//! A private window: an app that offers them opens one with `--private`, on
//! a profile of its own thrown away with the process, attached to no
//! runtime and holding none of the person's own records.

use std::cell::RefCell;

use tilo_app::launch::LaunchArgs;
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tabs {
    pub open: Vec<String>,
}

#[resource(kind = "test.tabs", label = "Tabs")]
impl Tabs {
    /// Open a tab.
    #[action]
    pub fn open(&mut self, url: String) {
        self.open.push(url);
    }
}

/// What the person visited: theirs, kept beside every window.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Visits {
    pub urls: Vec<String>,
}

#[resource(kind = "test.visits", label = "Visits")]
impl Visits {
    /// Remember a visit.
    #[action]
    pub fn visit(&mut self, url: String) {
        self.urls.push(url);
    }
}

thread_local! {
    /// What the last view built saw: whether it was private, and whether it
    /// was handed the person's records.
    static SEEN: RefCell<Option<(bool, bool)>> = const { RefCell::new(None) };
}

struct Browsing;

impl ResourceApp for Browsing {
    type Resource = Tabs;
    const ID: &'static str = "so.tilo.test-browsing";
    const TITLE: &'static str = "Browsing";
    const PRIVATE_WINDOWS: bool = true;

    fn view(cx: &mut ViewCx<Tabs>) -> Element {
        let person = cx.person::<Visits>();
        SEEN.with(|seen| *seen.borrow_mut() = Some((cx.is_private(), person.is_some())));
        if let Some(visits) = person {
            visits
                .invoke(&Visits::VISIT.with("https://kept.test"))
                .unwrap();
        }
        Box::new(cx.text(|tabs| tabs.open.join(", ")))
    }
}

struct Notes;

impl ResourceApp for Notes {
    type Resource = Tabs;
    const ID: &'static str = "so.tilo.test-notes";
    const TITLE: &'static str = "Notes";

    fn view(cx: &mut ViewCx<Tabs>) -> Element {
        Box::new(cx.text(|tabs| tabs.open.join(", ")))
    }
}

#[test]
fn a_private_window_attaches_to_no_runtime() {
    let private = LaunchArgs::component().with_flag("--private");
    let spec = tilo_app::framework::launch_spec::<Browsing>();
    assert!(!spec.attaches_runtime(&private));
    assert!(spec.attaches_runtime(&LaunchArgs::component()));
    // An app that offers no private windows takes the flag for nothing.
    assert!(tilo_app::framework::launch_spec::<Notes>().attaches_runtime(&private));
}

#[test]
fn a_private_window_runs_on_a_throwaway_profile_without_the_persons_records() {
    let args = LaunchArgs::component().with_flag("--private");
    let spec = tilo_app::framework::launch_spec::<Browsing>();
    let prepared = spec.prepare_hosted(&args).unwrap();
    assert!(prepared.context.is_private());
    let settings = prepared
        .context
        .storage()
        .settings_document()
        .path()
        .expect("a private window's settings are a file of its profile");
    let profile = settings.parent().unwrap().to_path_buf();
    assert!(
        profile.starts_with(std::env::temp_dir()),
        "the profile is not the person's: {}",
        profile.display()
    );

    let component = spec
        .prepare_component(&args, prepared.context, prepared.target, prepared.restore)
        .unwrap();
    let mut app = tilo_app::App::from_component(component, prepared.runner_inputs).unwrap();
    assert_eq!(
        SEEN.with(|seen| *seen.borrow()),
        Some((true, false)),
        "the view knows it is private and holds none of the person's records"
    );
    assert!(profile.exists(), "the profile lives while the window does");
    app.close_components();
    drop(app);
    assert!(
        !profile.exists(),
        "the profile is thrown away with the window: {}",
        profile.display()
    );
}
