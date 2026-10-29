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

/// Each app's package with the packages it imports before it, and the app it is.
fn apps() -> Vec<(&'static str, Vec<(&'static str, &'static str)>)> {
    vec![
        ("so.tilo.lang.counter", vec![app!("counter")]),
        ("so.tilo.lang.tasks", vec![app!("tasks")]),
        ("so.tilo.lang.notes", vec![app!("notes")]),
        ("so.tilo.lang.mail", vec![app!("mail")]),
        ("so.tilo.lang.chat", vec![app!("chat")]),
        (
            "so.tilo.lang.today",
            vec![app!("notes"), app!("tasks"), app!("chat"), app!("today")],
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
        assert!(
            (1..=3).contains(&meta.scenarios.len()),
            "{app} declares one to three scenarios"
        );
        for (i, scenario) in meta.scenarios.iter().enumerate() {
            // Each scenario in a room of its own, as a shot starts from one.
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

/// Today reads the roots of the apps it uses by name: opened before they are made, it shows them
/// once their scenarios fill them.
#[test]
fn todays_cards_follow_the_roots_of_the_apps_it_uses() {
    let packages = vec![app!("notes"), app!("tasks"), app!("chat"), app!("today")];
    let (mut replica, loaded) = Replica::new("ada", AutoCommit::new(), &packages);
    replica.host.root_of("so.tilo.lang.today").unwrap();
    replica.commit();
    let mut view = replica.view("so.tilo.lang.today");
    for package in &loaded[..3] {
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
    // Each card counts what its app's scenario made: the open todos, the pinned notes and the
    // chat's channels and categories at the top of its sidebar.
    let counts: Vec<String> = format!("{:?}", view.tree())
        .split("\"count\": Int(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next().map(str::to_owned))
        .collect();
    assert_eq!(counts, ["9", "3", "4"]);
}
