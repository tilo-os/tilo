//! Staging a scenario answers whether it ran whole: a step that is refused or fails stops the
//! staging there, in words that name the scenario, the step's action and why, rather than leaving
//! a capture or a test to picture a workspace the scenario never wrote.

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tilo_render_protocol::patch::local::pair;
use tilolang_runtime::Actor;
use tilolang_tilo::background::Waker;
use tilolang_tilo::host::{Host, RootView};
use tilolang_tilo::instance::{Config, Instance, Mode, Platform};
use tilolang_tilo::runtime::NameHost;

const APP: &str = "t.staging";

/// A talk whose scenarios write lines as the person staging them and as others.
const SOURCE: &str = r#"app t.staging "Staging"

type Line {
    said: Text
}

type Talk {
    lines: Collection<Line>
}

root Talk

default view talk for Talk

view talk(t: Talk) {
    page { text("{t.lines.len()} lines") }
}

action write(t: Talk, said: Text) -> Line {
    t.lines.insert(Line(said: said))
}

scenario "a morning" {
    let t = root Talk
    invoke(write(t, "Morning, from me"))
    invoke(write(t, "Morning, from Mia"), by: person("Mia"))
}

scenario "a quiet morning" {
    let t = root Talk
    invoke(write(t, "Morning, from me"))
}
"#;

fn ids() -> &'static str {
    static IDS: OnceLock<String> = OnceLock::new();
    IDS.get_or_init(|| {
        tilolang::compile_package_with(
            SOURCE,
            &tilolang::IdsLock::new(),
            &[],
            &mut tilolang::SeededEntropy(23),
        )
        .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"))
        .ids
        .to_text()
    })
}

struct Talk;

impl Platform for Talk {
    fn load(&self, host: &Host) -> u64 {
        host.load_builtin(SOURCE, ids()).unwrap();
        0
    }

    fn phrase(&self, id: &str, _args: &[(String, String)]) -> String {
        id.to_owned()
    }

    fn now(&self) -> String {
        "2026-11-24T09:00:00Z".to_owned()
    }

    fn waker(&self) -> Waker {
        Arc::new(|| {})
    }
}

struct Alone;

impl NameHost for Alone {
    fn name(&self, who: &Actor) -> String {
        who.id().to_owned()
    }
}

/// Ada's window of the talk, opened.
fn window() -> Instance {
    let (app_end, _window_end) = pair();
    let mut instance = Instance::new(
        Config {
            app: APP.to_owned(),
            me: "ada".into(),
            mode: Mode::Live,
            device: None,
            store: tilolang_tilo::resources::ResourceStore::in_memory(),
            root: RootView::default(),
            sink: Some(Box::new(tilolang_tilo::runtime::MemorySink::new())),
            link_home: None,
        },
        Rc::new(Talk),
        Rc::new(Alone),
        Box::new(app_end),
    );
    instance.sync();
    instance
}

#[test]
fn a_scenario_that_runs_whole_stages() {
    window()
        .stage("a quiet morning")
        .expect("a scenario of the person's own steps stages");
}

/// In a workspace that decides by its members, a step run as someone who is not in it is
/// refused: staging answers that refusal, naming the scenario, the step's action, who it was to
/// run as and why, though the steps before it stand.
#[test]
fn a_scenario_whose_step_is_refused_answers_the_step_and_why() {
    let mut instance = window();
    instance
        .host()
        .expect("the window opened")
        .runtime()
        .decide_by_workspace();
    let why = instance
        .stage("a morning")
        .expect_err("a scenario stopped at a refused step did not stage whole");
    for said in [
        "\"a morning\"",
        "can't write as Mia",
        "Mia is not in this workspace",
    ] {
        assert!(why.contains(said), "the failure says {said}: {why}");
    }
}

#[test]
fn a_scenario_the_app_does_not_have_is_no_staging() {
    let why = window()
        .stage("an evening")
        .expect_err("a scenario the app lacks stages nothing");
    assert!(
        why.contains("has no scenario \"an evening\""),
        "the failure names the scenario: {why}"
    );
}
