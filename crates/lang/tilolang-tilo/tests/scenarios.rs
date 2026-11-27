//! Every app's scenarios run against its root resource and fill its window: the seed data shots,
//! tests and benchmarks start from.

use crate::fixtures::Workspace;
use tilolang_runtime::Actor;
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest};

use tilolang_tilo::used::{UsedApp, answer};

use crate::fixtures::Replica;

/// The files a scenario's seed makes — a letter's papers, a note's picture — kept in memory, as a
/// content store keeps them: a scenario runs on a device, and a device that keeps no files cannot
/// stage one that holds a file.
#[derive(Default)]
struct Papers(std::cell::RefCell<std::collections::HashMap<String, Vec<u8>>>);

impl BlobHost for Papers {
    fn read(&self, hash: &str) -> Option<Vec<u8>> {
        self.0.borrow().get(hash).cloned()
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<(), String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        // The content's own address, as a content store keeps the same bytes once.
        let mut digest = 0xcbf2_9ce4_8422_2325_u64;
        for byte in bytes {
            digest = (digest ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
        let hash = format!("k{digest:x}");
        self.0.borrow_mut().insert(hash.clone(), bytes.to_vec());
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            hash,
        ))
    }
}

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
        ("so.tilo.tasks", crate::fixtures::TASKS_SET.to_vec()),
        (
            "so.tilo.notes",
            vec![crate::fixtures::DISCUSSION, crate::fixtures::NOTES],
        ),
        ("so.tilo.mail", crate::fixtures::MAIL_SET.to_vec()),
        ("so.tilo.chat", vec![crate::fixtures::CHAT]),
        ("so.tilo.lang.today", {
            let mut packages = crate::fixtures::TASKS_SET.to_vec();
            packages.push(crate::fixtures::CHAT);
            packages.push(app!("today"));
            packages
        }),
    ]
}

#[test]
fn every_scenario_of_every_app_seeds_its_root_and_the_first_fills_a_list_past_a_screen() {
    for (app, packages) in apps() {
        let meta = {
            let (replica, loaded) = Replica::new("ada", Workspace::new(), &packages);
            drop(replica);
            loaded.last().unwrap().meta.clone()
        };
        // A scenario per window worth seeing (a list, a task's page, a project's board): the
        // seed a shot or a layout check of that window starts from, so there is no upper bound.
        assert!(!meta.scenarios.is_empty(), "{app} declares a scenario");
        for (i, scenario) in meta.scenarios.iter().enumerate() {
            // Each scenario in a workspace of its own, as a shot starts from one.
            let (replica, loaded) = Replica::new("ada", Workspace::new(), &packages);
            // A scenario may attach the roots of the packages it uses (Today's apps).
            for package in &loaded {
                if package.meta.app != app {
                    let _ = replica.host.root_of(&package.meta.app);
                }
            }
            replica.host.root_of(app).unwrap();
            replica.host.runtime().set_blobs(Papers::default());
            replica.commit();
            replica
                .host
                .runtime()
                .invoke_action(
                    &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                    (scenario.action.as_str(), vec![]),
                )
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
    let packages = crate::fixtures::TASKS_SET.to_vec();
    let (replica, loaded) = Replica::new("ada", Workspace::new(), &packages);
    replica.host.root_of("so.tilo.tasks").unwrap();
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
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.action.as_str(), vec![]),
        )
        .unwrap();
    let members = replica.host.runtime().members();
    assert!(
        members.iter().any(|member| member.who().id() == "Mia"),
        "Mia is one of the workspace's: {members:?}"
    );
}

/// Today reads the roots of the apps it uses by name, each from that app's own document: opened
/// before they are made, it shows them once their scenarios fill them. Each app keeps its
/// document, as installed apps do, and Today's runtime loads them as other apps' packages, so
/// nothing of theirs is in Today's document for its cards to read by mistake.
#[test]
fn todays_cards_follow_the_roots_of_the_apps_it_uses() {
    const TODAY: &str = "so.tilo.lang.today";
    // The day the scenarios' data is about, which a capture pins: Tasks' week is dated around
    // it, and Today's place is what is due on the day it is read.
    let clock = || "2026-06-30T10:42:00Z".to_owned();
    let mut owners: Vec<(Replica, Vec<tilolang_tilo::host::Package>)> = [
        vec![crate::fixtures::DISCUSSION, crate::fixtures::NOTES],
        crate::fixtures::TASKS_SET.to_vec(),
        vec![crate::fixtures::CHAT],
    ]
    .iter()
    .map(|packages| {
        let (replica, loaded) = Replica::new("ada", Workspace::new(), packages);
        replica.host.set_clock(clock);
        (replica, loaded)
    })
    .collect();
    let (today, discussion) = Replica::new("ada", Workspace::new(), &[crate::fixtures::DISCUSSION]);
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
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                (first.action.as_str(), vec![]),
            )
            .unwrap();
        owner.commit();
    }
    // Today's runtime reads each app from its document, through that app's own package.
    let mut used: Vec<UsedApp> = owners
        .iter()
        .map(|(owner, loaded)| {
            let packages: Vec<(&[u8], &tilolang_tilo::runtime::Meta)> = loaded
                .iter()
                .map(|package| (&*package.wasm, &package.meta))
                .collect();
            UsedApp::new(owner.doc.store(), &packages, "ada").unwrap()
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
                !today
                    .host
                    .store()
                    .contains(&tilolang_runtime::value::ResourceRef::root(
                        format!("root-{root}"),
                        root.clone()
                    )),
                "Today's document holds no root of {}",
                app.app
            );
        }
    }
    view.poll_patches().unwrap();
    // Each card counts what its app's place holds for the person looking: the open tasks their
    // Today holds — six, the five the week dates that day, the thirtieth, and the haircut it
    // dates the day before; the five it dates later and the five it leaves undated and nobody
    // holds wait elsewhere — the pinned notes, two, and Chat's Unread, the conversations
    // holding a message the reader has not seen: design and general, two, the only ones the
    // staged week says anything in. Not the channels nothing was said in, not the categories,
    // which say nothing themselves, and not the notes to yourself, whose one message is the
    // reader's own.
    let counts: Vec<String> = format!("{:?}", view.tree())
        .split("\"count\": Int(")
        .skip(1)
        .filter_map(|rest| rest.split(')').next().map(str::to_owned))
        .collect();
    assert_eq!(counts, ["6", "2", "2"]);
}

/// A scenario is a fixture: a seed runs it straight on the runtime, and nothing else may. Through
/// the API, as the person at the window, the app's own code or an agent calls an action, it is
/// no action, so none of them stages a workspace's data.
#[test]
fn only_a_seed_runs_a_scenario() {
    let packages = vec![crate::fixtures::DISCUSSION, crate::fixtures::NOTES];
    let (replica, loaded) = Replica::new("ada", Workspace::new(), &packages);
    let notebook = replica.host.root_of("so.tilo.notes").unwrap();
    replica.commit();
    let meta = &loaded.last().unwrap().meta;
    let scenario = meta
        .scenarios
        .first()
        .expect("Notes stages a week of notes");
    let kind = tilolang_tilo::meta::MetaExt::kind_of(meta, "Notebook");
    let args = serde_json::Map::new();
    let agent = tilolang_tilo::publish::Caller {
        actor: tilolang_tilo::runtime::ids::MemberId::agent("task-1", "ada").into(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
        handed: false,
        app: None,
    };
    for caller in [None, Some(&agent)] {
        let refused = replica
            .host
            .plan(&tilolang_tilo::publish::PlanRequest {
                app: "so.tilo.notes",
                kind: &kind,
                action_id: &scenario.action,
                resource: Some(&notebook.doc),
                args: &args,
                caller,
                from: None,
            })
            .unwrap_err();
        assert!(refused.contains("no action"), "{refused}");
    }
    assert!(
        meta.callable_actions()
            .all(|action| action.id != scenario.action),
        "another app's code cannot name it"
    );
    // The seed itself still runs it.
    replica
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (scenario.action.as_str(), vec![]),
        )
        .unwrap();
}
