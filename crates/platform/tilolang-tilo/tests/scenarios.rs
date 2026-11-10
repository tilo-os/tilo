//! Every app's scenarios run against its root resource and fill its window: the seed data shots,
//! tests and benchmarks start from.

use automerge::AutoCommit;
use tilolang_runtime::Principal;

use crate::fixtures::Replica;

macro_rules! app {
    ($name:literal) => {
        (
            include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// An app whose scenarios are a file of their own (`scenarios.tl`), as its package compiles them.
macro_rules! app_and_scenarios {
    ($name:literal) => {
        (
            concat!(
                include_str!(concat!("../../../../apps/tl/", $name, "/app.tl")),
                "\n",
                include_str!(concat!("../../../../apps/tl/", $name, "/scenarios.tl")),
            ),
            include_str!(concat!("../../../../apps/tl/", $name, "/app.ids")),
        )
    };
}

/// Each app's package with the packages it imports before it, and the app it is.
fn apps() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("so.tilo.lang.counter", vec![app!("counter")]),
        (
            "so.tilo.lang.tasks",
            vec![app!("discussion"), app_and_scenarios!("tasks")],
        ),
        (
            "so.tilo.lang.notes",
            vec![app!("discussion"), app!("notes")],
        ),
        ("so.tilo.lang.mail", vec![app!("mail")]),
        ("so.tilo.lang.chat", vec![app!("chat")]),
        (
            "so.tilo.lang.today",
            vec![
                app!("discussion"),
                app!("notes"),
                app_and_scenarios!("tasks"),
                app!("chat"),
                app!("today"),
            ],
        ),
    ]
}

#[test]
fn every_scenario_of_every_app_seeds_its_root_and_the_first_fills_a_list_past_a_screen() {
    for (app, packages) in apps() {
        let meta = {
            let (replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
            drop(replica);
            loaded.last().unwrap().meta.clone()
        };
        // A scenario per window worth seeing (a list, a task's page, a project's board): the
        // seed a shot or a layout check of that window starts from, so there is no upper bound.
        assert!(!meta.scenarios.is_empty(), "{app} declares a scenario");
        for (i, scenario) in meta.scenarios.iter().enumerate() {
            // Each scenario in a workspace of its own, as a shot starts from one.
            let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
            // A scenario may attach the roots of the packages it uses (Today's apps).
            for package in &loaded {
                if package.meta.app != app {
                    let _ = replica.host.root_of(&package.meta.app);
                }
            }
            replica.host.root_of(app).unwrap();
            replica.commit();
            replica
                .host
                .runtime()
                .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
                .unwrap_or_else(|e| panic!("{app} {:?}: {e}", scenario.name));
            replica.commit();
            let view = replica.view(app);
            if i == 0 && view.tree().kind == "list" {
                // Its rows, which render as the list shows them.
                let rows = view.list_len(&view.tree().key).unwrap_or(0);
                assert!(
                    rows >= 12,
                    "{app}'s first scenario has enough rows to scroll: {rows}"
                );
            }
        }
    }
}

/// A scenario's `person("Mia")` is someone in the workspace it stages: a view that offers the
/// members offers them too, by the name the scenario gave.
#[test]
fn a_scenarios_named_person_is_a_member_of_the_workspace() {
    let packages = vec![app!("discussion"), app_and_scenarios!("tasks")];
    let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
    replica.host.root_of("so.tilo.lang.tasks").unwrap();
    replica.commit();
    let scenario = loaded
        .last()
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a task's subtasks")
        .expect("Tasks stages a task's subtasks");
    replica
        .host
        .runtime()
        .invoke_action(&Principal::new("ada"), (scenario.action.as_str(), vec![]))
        .unwrap();
    let members = replica.host.runtime().members();
    assert!(
        members.iter().any(|member| member.who.0 == "scenario:Mia"),
        "Mia is one of the workspace's: {members:?}"
    );
}

/// Today reads the roots of the apps it uses by name: opened before they are made, it shows them
/// once their scenarios fill them.
#[test]
fn todays_cards_follow_the_roots_of_the_apps_it_uses() {
    let packages = vec![
        app!("discussion"),
        app!("notes"),
        app_and_scenarios!("tasks"),
        app!("chat"),
        app!("today"),
    ];
    let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
    // The day the scenarios' data is about, which a capture pins: Tasks' week is dated around
    // it, and Today's place is what is due on the day it is read.
    replica.host.set_clock(|| "2026-06-30T10:42:00Z".to_owned());
    replica.host.root_of("so.tilo.lang.today").unwrap();
    replica.commit();
    let mut view = replica.view("so.tilo.lang.today");
    for package in loaded.iter().filter(|p| {
        matches!(
            p.meta.app.as_str(),
            "so.tilo.lang.notes" | "so.tilo.lang.tasks" | "so.tilo.lang.chat"
        )
    }) {
        replica.host.root_of(&package.meta.app).unwrap();
        replica.commit();
        let first = &package.meta.scenarios[0];
        replica
            .host
            .runtime()
            .invoke_action(&Principal::new("ada"), (first.action.as_str(), vec![]))
            .unwrap();
        replica.commit();
    }
    view.poll_patches().unwrap();
    // Each card counts what its app's place holds for the person looking: the open tasks their
    // Today holds — six, the five the week dates that day, the thirtieth, and the haircut it
    // dates the day before; the five it dates later and the five it leaves undated and nobody
    // holds wait elsewhere — the pinned notes, two, and Chat's Unread, the conversations
    // holding a message the reader has not seen: design, general and the direct conversation
    // with Leo, three. Not the channels nothing was said in, not the categories, which say
    // nothing themselves, and not the notes to yourself, whose one message is the reader's own.
    let counts: Vec<String> = format!("{:?}", view.tree())
        .split("\"count\": Int(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next().map(str::to_owned))
        .collect();
    assert_eq!(counts, ["6", "2", "3"]);
}
