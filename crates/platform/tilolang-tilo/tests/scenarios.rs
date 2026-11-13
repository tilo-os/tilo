//! Every app's scenarios run against its root resource and fill its window: the seed data shots,
//! tests and benchmarks start from.

use automerge::AutoCommit;
use tilolang_runtime::Principal;

use tilolang_tilo::used::{UsedApp, answer};

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

/// Today reads the roots of the apps it uses by name, each from that app's own record: opened
/// before they are made, it shows them once their scenarios fill them. Each app keeps its
/// record, as installed apps do, and Today's runtime loads them as other apps' packages, so
/// nothing of theirs is in Today's record for its cards to read by mistake.
#[test]
fn todays_cards_follow_the_roots_of_the_apps_it_uses() {
    const TODAY: &str = "so.tilo.lang.today";
    // The day the scenarios' data is about, which a capture pins: Tasks' week is dated around
    // it, and Today's place is what is due on the day it is read.
    let clock = || "2026-06-30T10:42:00Z".to_owned();
    let mut owners: Vec<(Replica, Vec<tilolang_tilo::host::Package>)> = [
        vec![app!("discussion"), app!("notes")],
        vec![app!("discussion"), app_and_scenarios!("tasks")],
        vec![app!("chat")],
    ]
    .iter()
    .map(|packages| {
        let (replica, loaded) = Replica::new("ada", AutoCommit::new(), packages);
        replica.host.set_clock(clock);
        (replica, loaded)
    })
    .collect();
    let (mut today, discussion) = Replica::new("ada", AutoCommit::new(), &[app!("discussion")]);
    today.host.set_clock(clock);
    let mut metas = vec![discussion[0].meta.clone()];
    for (_, loaded) in &owners {
        let app = loaded.last().unwrap();
        today
            .host
            .load_foreign_package(&app.wasm, &app.meta)
            .unwrap();
        metas.push(app.meta.clone());
    }
    let (source, ids) = app!("today");
    let compiled =
        tilolang::compile_package(source, &tilolang::IdsLock::parse(ids).unwrap(), &metas)
            .unwrap_or_else(|diagnostics| panic!("{diagnostics:#?}"));
    today.host.load_compiled(&compiled).unwrap();
    today.host.root_of(TODAY).unwrap();
    today.commit();
    let mut view = today.view(TODAY);
    for (owner, loaded) in &mut owners {
        let package = loaded.last().unwrap().clone();
        owner.host.root_of(&package.meta.app).unwrap();
        owner.commit();
        let first = &package.meta.scenarios[0];
        owner
            .host
            .runtime()
            .invoke_action(&Principal::new("ada"), (first.action.as_str(), vec![]))
            .unwrap();
        owner.commit();
    }
    // Today's runtime reads each app from its record, through that app's own package.
    let mut used: Vec<UsedApp> = owners
        .iter()
        .map(|(owner, loaded)| {
            let packages: Vec<(&[u8], &tilolang_tilo::runtime::Meta)> = loaded
                .iter()
                .map(|package| (&*package.wasm, &package.meta))
                .collect();
            let used = UsedApp::new(&packages, "ada").unwrap();
            used.host().sync_from(&owner.doc);
            used
        })
        .collect();
    for _ in 0..16 {
        view.poll_patches().unwrap();
        let mut reading: Vec<&mut UsedApp> = used.iter_mut().collect();
        if !answer(today.host.runtime(), &mut reading) {
            break;
        }
    }
    for (_, loaded) in &owners {
        let app = &loaded.last().unwrap().meta;
        for root in &app.roots {
            assert!(
                !today.host.store().contains(&format!("root-{root}")),
                "Today's record holds no root of {}",
                app.app
            );
        }
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
