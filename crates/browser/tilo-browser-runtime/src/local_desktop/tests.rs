use super::*;
use crate::desktop_host::{DesktopHandle, DesktopRoot, DesktopSources};
use std::sync::Arc;
use tilo_desktop_model::commands::CommandSource;
use ui::portable_storage::PortableStorage;
use ui::record::{RecordCheckout, RecordId, RecordOwners};

const ROOM: &str = "11111111-2222-4333-8444-555555555555";

fn seed(json: serde_json::Value) -> Seed {
    serde_json::from_value(json).unwrap()
}
fn window(app: &str) -> serde_json::Value {
    serde_json::json!({ "app": app })
}
fn tabs(tree: &DesktopTree, workspace: usize) -> Vec<(u64, String)> {
    tree.workspaces[workspace]
        .strips
        .iter()
        .flat_map(|s| &s.window_groups)
        .flat_map(|g| &g.tabs)
        .map(|t| (t.id, canonical_app_id(&t.tile).unwrap()))
        .collect()
}
fn room_of(tree: &DesktopTree, workspace: usize) -> String {
    tree.workspaces[workspace].room.to_string()
}
fn bound(room: String, command: DesktopCommand) -> BoundCommand {
    BoundCommand {
        source: CommandSource {
            room,
            heads: vec![[1; 32]],
        },
        command,
    }
}
fn open(desktop: &mut LocalDesktop, room: String, app: &str) {
    desktop
        .apply(
            &bound(
                room,
                DesktopCommand::OpenApp {
                    app: app.into(),
                    beside: None,
                },
            ),
            None,
        )
        .unwrap();
}

#[test]
fn a_seed_admits_its_workspaces_strips_windows_and_records() {
    let desktop = LocalDesktop::new(
        ROOM,
        "Visitor",
        &seed(serde_json::json!({"workspaces": [
            {"title": "Reading", "strips": [
                {"title": "Books", "windows": [
                    {"app": "so.tilo.ebook", "record": "tab:00000000000000aa", "focused": true},
                    window("so.tilo.notes"),
                ]},
            ]},
            {"label": "Writing", "strips": [{"windows": [window("so.tilo.word-processor")]}]},
            {"title": "Empty"},
        ]})),
    )
    .unwrap();
    let admission = desktop.admission().unwrap();
    assert!(admission.complete());
    let tree = &admission.tree;
    let names = tree.workspaces.iter().map(|w| w.name.as_str());
    assert_eq!(names.collect::<Vec<_>>(), ["Reading", "Writing", "Empty"]);
    assert_eq!(
        room_of(tree, 0),
        ROOM,
        "the first workspace is the mount's room"
    );
    assert_ne!(room_of(tree, 1), ROOM);
    assert!(tree.workspaces[2].strips.is_empty());
    assert_eq!(tree.workspaces[0].strips[0].name, "Books");
    assert_eq!(tree.workspaces[1].strips[0].name, "Main");
    let reading = tabs(tree, 0);
    let apps = reading.iter().map(|(_, app)| app.as_str());
    assert_eq!(apps.collect::<Vec<_>>(), ["so.tilo.ebook", "so.tilo.notes"]);
    let (book, notes) = (reading[0].0, reading[1].0);
    assert_eq!(admission.routes[&book], "tab:00000000000000aa");
    assert!(!desktop.is_fresh(book), "the page's record already exists");
    assert!(
        desktop.is_fresh(notes),
        "a new window's record is created on mount"
    );
    let writer = tabs(tree, 1)[0].0;
    assert_ne!(peer_salt_of(writer), peer_salt_of(notes));
    assert_eq!(admission.required.len(), 3);

    let mut tree = admission.tree.clone();
    let mut local = desktop;
    let focus = local.take_focus().unwrap();
    assert_eq!(focus, book);
    select(&mut tree, focus);
    assert_eq!(tree.active, tree.workspaces[0].id);
    let strip = tree.workspaces[0].strips[0].id;
    assert_eq!(
        tree.active_group[&strip],
        tree.workspaces[0].strips[0].window_groups[0].id
    );
}

#[test]
fn unknown_apps_invalid_rooms_and_oversized_seeds_are_refused() {
    let error = |json| {
        LocalDesktop::new(ROOM, "Visitor", &seed(json))
            .err()
            .unwrap()
    };
    assert_eq!(
        error(
            serde_json::json!({"workspaces":[{"strips":[{"windows":[window("so.tilo.nope")]}]}]})
        ),
        "application \"so.tilo.nope\" is not available"
    );
    assert_eq!(
        error(serde_json::json!({"apps":["writer"]})),
        "application \"writer\" is not available",
        "the host canonicalizes aliases; the core resolves exact ids"
    );
    let many = vec![serde_json::json!({}); MAX_WORKSPACES + 1];
    assert!(error(serde_json::json!({ "workspaces": many })).contains("workspaces"));
    assert!(LocalDesktop::new("not-a-room", "Visitor", &Seed::default()).is_err());
    assert!(
        serde_json::from_value::<Seed>(serde_json::json!({"workspace": []})).is_err(),
        "a misspelt seed is refused, not ignored"
    );
    let empty = LocalDesktop::new(ROOM, "Visitor", &Seed::default()).unwrap();
    let admission = empty.admission().unwrap();
    assert_eq!(admission.tree.workspaces.len(), 1);
    assert_eq!(admission.tree.workspaces[0].name, "Workspace 1");
}

#[test]
fn the_owner_reduces_opens_closes_and_renames_in_their_own_room() {
    let mut desktop = LocalDesktop::new(
        ROOM,
        "Visitor",
        &seed(serde_json::json!({"workspaces": [
            {"strips": [{"windows": [window("so.tilo.notes")]}]},
            {"title": "Second"},
        ]})),
    )
    .unwrap();
    let before = desktop.admission().unwrap();
    let second = room_of(&before.tree, 1);
    open(&mut desktop, second.clone(), "so.tilo.calculator");
    let after = desktop.admission().unwrap();
    assert_eq!(tabs(&after.tree, 0), tabs(&before.tree, 0));
    let opened = tabs(&after.tree, 1);
    assert_eq!(opened.len(), 1);
    assert_eq!(opened[0].1, "so.tilo.calculator");
    let tab = opened[0].0;
    assert!(desktop.is_fresh(tab));
    assert_eq!(desktop.take_focus(), Some(tab));
    assert!(
        after
            .required
            .iter()
            .any(|r| r.app_id == "so.tilo.calculator")
    );

    let strip = after.tree.workspaces[1].strips[0].clone();
    let group = strip.window_groups[0].id;
    let close = DesktopCommand::CloseTab {
        location: tilo_desktop_model::commands::TabLocation {
            strip: strip.id.0,
            group,
            tab,
        },
    };
    assert_eq!(
        desktop
            .apply(
                &bound(second.clone(), close.clone()),
                Some("tab:00000000000000aa")
            )
            .unwrap_err(),
        "only an open names a record",
    );
    desktop.apply(&bound(second.clone(), close), None).unwrap();
    desktop
        .apply(
            &bound(
                second.clone(),
                DesktopCommand::RenameWorkspace {
                    name: "Renamed".into(),
                },
            ),
            None,
        )
        .unwrap();
    let closed = desktop.admission().unwrap();
    assert!(tabs(&closed.tree, 1).is_empty());
    assert_eq!(closed.tree.workspaces[1].name, "Renamed");
    assert!(desktop.room(closed.tree.workspaces[1].room).is_some());

    let refused = desktop.apply(
        &bound(
            second,
            DesktopCommand::OpenApp {
                app: "so.tilo.nope".into(),
                beside: None,
            },
        ),
        None,
    );
    assert_eq!(refused.unwrap_err(), "application cannot be opened here");
}

#[test]
fn an_open_on_a_record_shows_that_record_in_a_new_selected_window() {
    let mut desktop = LocalDesktop::new(ROOM, "Visitor", &Seed::default()).unwrap();
    let room = room_of(&desktop.admission().unwrap().tree, 0);
    let open = DesktopCommand::OpenApp {
        app: "so.tilo.ebook".into(),
        beside: None,
    };
    desktop
        .apply(
            &bound(room.clone(), open.clone()),
            Some("tab:00000000000000aa"),
        )
        .unwrap();
    let admission = desktop.admission().unwrap();
    let (tab, app) = tabs(&admission.tree, 0)[0].clone();
    assert_eq!(app, "so.tilo.ebook");
    assert_eq!(admission.routes[&tab], "tab:00000000000000aa");
    assert!(
        !desktop.is_fresh(tab),
        "the record exists; nothing creates it"
    );
    assert_eq!(desktop.take_focus(), Some(tab));
    assert!(
        admission
            .required
            .iter()
            .any(|r| r.record_id == "tab:00000000000000aa")
    );
    assert_eq!(
        desktop
            .apply(&bound(room, open), Some("not a record"))
            .unwrap_err(),
        "invalid canonical ordinary record route"
    );
    assert_eq!(
        tabs(&desktop.admission().unwrap().tree, 0).len(),
        1,
        "a refused open changes nothing"
    );
}

#[test]
fn every_room_change_carries_the_time_it_was_made() {
    let before = ui::time::try_unix_now().unwrap() as i64;
    let mut desktop = LocalDesktop::new(
        ROOM,
        "Visitor",
        &seed(serde_json::json!({"workspaces": [{"strips": [{"windows": [window("so.tilo.notes")]}]}]})),
    )
    .unwrap();
    let id = desktop.admission().unwrap().tree.workspaces[0].room;
    open(&mut desktop, id.to_string(), "so.tilo.calculator");
    let doc = desktop.room(id).unwrap();
    let changes = doc.clone().get_changes(&[]);
    assert!(changes.len() >= 2);
    for change in changes {
        assert!(
            change.timestamp() >= before,
            "a seeded or reduced change is stamped when it was committed, not at the epoch"
        );
    }
}

#[test]
fn seeded_members_and_app_grants_are_the_workspaces_capabilities() {
    let desktop = LocalDesktop::new(
        ROOM,
        "Visitor",
        &seed(serde_json::json!({"workspaces": [{"title": "Shared",
            "members": [{"name": "Ada", "access": "comment"}, {"name": "Scribe", "agent": true}],
            "grants": [{"app": "so.tilo.word-processor", "access": "read", "over": "so.tilo.ebook"}],
        }]})),
    )
    .unwrap();
    let tree = desktop.admission().unwrap().tree;
    let workspace = &tree.workspaces[0];
    assert_eq!(workspace.owner.as_deref(), Some("Visitor"));
    assert_eq!(workspace.capabilities.len(), 3);
    let roster = workspace.roster_at(0);
    let people = roster
        .iter()
        .map(|(who, _)| who.as_str())
        .collect::<Vec<_>>();
    assert_eq!(people, ["Visitor", "Ada"], "the owner first, agents apart");
    let agents = workspace.agent_members_at(0);
    assert_eq!(
        agents.iter().map(|(a, _)| a.as_str()).collect::<Vec<_>>(),
        ["Scribe"]
    );
    let grant = &workspace.capabilities[2];
    assert_eq!(grant.scope, Scope::App("so.tilo.ebook".into()));
    assert_eq!(grant.verb, Verb::Read);
    assert!(
        serde_json::from_value::<Seed>(
            serde_json::json!({"workspaces":[{"members":[{"name":"x","access":"owner"}]}]})
        )
        .is_err()
    );
}

fn mount(seed_json: serde_json::Value, storage: &PortableStorage) -> (DesktopRoot, DesktopHandle) {
    let local = LocalDesktop::new(ROOM, "author", &seed(seed_json)).unwrap();
    let permissions = ui::permissions::PermissionContext::from_capabilities(
        ui::permissions::Actor::person(Some(ui::permissions::Role::Owner), "author"),
        vec![],
    );
    let (root, handle) = DesktopRoot::new(
        DesktopSources {
            admission: local.admission().unwrap(),
            storage: storage.clone(),
            permissions,
            content: None,
            local: Some(local),
        },
        ROOM,
        "device",
        "author",
        1,
        Arc::new(|| {}),
    )
    .unwrap();
    assert!(
        handle.errors.borrow().is_empty(),
        "{:?}",
        handle.errors.borrow()
    );
    (root, handle)
}
fn frame(root: DesktopRoot) -> ui::headless::FrameHarness {
    let mut frame = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(root),
        (1280, 720),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("author".into()),
    );
    frame.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));
    frame.poll_frame().unwrap();
    frame
}
fn navigation(handle: &DesktopHandle) -> serde_json::Value {
    handle.navigation.borrow().clone()
}

#[test]
fn the_real_desktop_creates_new_windows_records_and_opens_the_pages_own() {
    let storage = PortableStorage::new("local-desktop-test", "device").unwrap();
    // A record a standalone Writer mount of this profile, device and room made.
    let writer = crate::desktop::definition("so.tilo.word-processor").unwrap();
    let record = RecordCheckout::new(RecordId::minted(), writer.record_kind.clone());
    let store = RecordOwners::portable(storage.clone())
        .open(writer.app_id, ROOM)
        .unwrap();
    store.create_record(&record).unwrap();
    store
        .with(&record.id, |doc| {
            word_processor::initialize_empty_document(doc).unwrap();
        })
        .unwrap();
    let (root, handle) = mount(
        serde_json::json!({"workspaces": [{"strips": [{"windows": [
            {"app": "so.tilo.word-processor", "record": record.id.as_str(), "focused": true},
            window("so.tilo.notes"),
        ]}]}]}),
        &storage,
    );
    let mut frame = frame(root);
    assert!(
        handle.errors.borrow().is_empty(),
        "{:?}",
        handle.errors.borrow()
    );
    let records = handle.records.borrow();
    let ids = records.keys().cloned().collect::<Vec<_>>();
    assert!(ids.contains(&("so.tilo.word-processor".into(), record.id.as_str().into())));
    let notes = records.values().find(|r| r.app == "so.tilo.notes").unwrap();
    assert!(
        notes.store.is_ready(&notes.checkout.id),
        "created by Notes itself"
    );
    drop(records);
    assert!(handle.local.as_ref().unwrap().borrow().fresh.is_empty());
    // The page scripts the focused window's app through its action door.
    let focused = navigation(&handle)["active"]["tab"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let resources = frame.bound_resources_of(Some(focused), None).unwrap();
    assert!(
        resources
            .iter()
            .any(|r| r.kind == "word_processor.document")
    );
    assert!(!resources.iter().any(|r| r.kind.starts_with("notes.")));

    // Every window reports its app and record, so the page can mount the
    // record elsewhere, and opens another window on it.
    let snapshot = navigation(&handle);
    let entries = snapshot["entries"].as_array().unwrap();
    let writer_entry = entries
        .iter()
        .find(|e| e["app"] == "so.tilo.word-processor")
        .unwrap();
    assert_eq!(writer_entry["record"], record.id.as_str());
    let notes_entry = entries
        .iter()
        .find(|e| e["app"] == "so.tilo.notes")
        .unwrap();
    assert!(
        notes_entry["record"]
            .as_str()
            .is_some_and(|r| !r.is_empty())
    );
    let generation = snapshot["generation"].as_str().unwrap().to_owned();
    let opened = handle
        .navigate(
            &mut frame,
            serde_json::from_value(serde_json::json!({"kind":"open","generation":generation,
                "app":"so.tilo.word-processor","record":record.id.as_str()}))
            .unwrap(),
        )
        .unwrap();
    assert!(opened.is_some());
    frame.poll_frame().unwrap();
    let snapshot = navigation(&handle);
    let windows = snapshot["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["record"] == record.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 2, "a second window on the same record");
    assert_eq!(
        snapshot["active"], windows[1]["route"],
        "and it is selected"
    );
    assert!(
        handle.errors.borrow().is_empty(),
        "{:?}",
        handle.errors.borrow()
    );
    for record in ["", "not a record"] {
        assert!(
            handle
                .navigate(
                    &mut frame,
                    serde_json::from_value(
                        serde_json::json!({"kind":"open","generation":generation,
                        "app":"so.tilo.word-processor","record":record})
                    )
                    .unwrap(),
                )
                .is_err(),
            "{record:?}"
        );
    }
    frame.close_components();
    let _ = frame.take_component_close_reports();
}

#[test]
fn palette_opens_and_workspace_switches_apply_locally_and_open_history() {
    let storage = PortableStorage::new("local-desktop-test", "device").unwrap();
    let (root, handle) = mount(
        serde_json::json!({"workspaces": [
            {"title": "One", "strips": [{"windows": [window("so.tilo.notes")]}]},
            {"title": "Two"},
        ]}),
        &storage,
    );
    let mut frame = frame(root);
    let snapshot = navigation(&handle);
    assert_eq!(snapshot["capabilities"]["open"], true);
    let generation = snapshot["generation"].as_str().unwrap().to_owned();
    let two = snapshot["workspaces"][1]["id"].as_str().unwrap().to_owned();

    frame.set_modifiers(&["ctrl".into()]);
    frame.key("k");
    frame.set_modifiers(&[]);
    frame.type_text("Open Calculator");
    frame.key("Enter");
    frame.poll_frame().unwrap();
    assert!(
        handle.session_requests.borrow().is_empty(),
        "nothing waits for a peer"
    );
    let entries = navigation(&handle)["entries"].as_array().unwrap().clone();
    assert_eq!(entries.len(), 2);
    let calculator = entries.iter().find(|e| e["label"] == "Calculator").unwrap();
    assert_eq!(
        navigation(&handle)["active"],
        calculator["route"],
        "the open is selected"
    );

    handle
        .navigate(
            &mut frame,
            serde_json::from_value(
                serde_json::json!({"kind":"workspace","generation":generation,"workspace":two}),
            )
            .unwrap(),
        )
        .unwrap();
    frame.poll_frame().unwrap();
    assert_eq!(navigation(&handle)["activeWorkspace"], two);
    let opened = handle
        .navigate(
            &mut frame,
            serde_json::from_value(
                serde_json::json!({"kind":"open","generation":generation,"app":"so.tilo.notes"}),
            )
            .unwrap(),
        )
        .unwrap();
    assert!(opened.is_some());
    frame.poll_frame().unwrap();
    let snapshot = navigation(&handle);
    let in_two = snapshot["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["route"]["workspace"] == two)
        .count();
    assert_eq!(in_two, 1, "the open lands in the active workspace's room");
    assert_eq!(snapshot["capabilities"]["workspaceHistory"], true);
    handle
        .navigate(
            &mut frame,
            serde_json::from_value(serde_json::json!({"kind":"workspace-activity","generation":generation,"workspace":two,"view":"explore"})).unwrap(),
        )
        .unwrap();
    frame.poll_frame().unwrap();
    assert_eq!(navigation(&handle)["workspaceActivity"]["workspace"], two);
    // The workspace settings draw over the desktop: opening them retires Activity.
    let settings = |kind: &str| {
        serde_json::from_value(serde_json::json!({"kind":kind,"generation":generation,"workspace":two,"tab":"permissions"}))
            .unwrap()
    };
    handle
        .navigate(&mut frame, settings("workspace-settings"))
        .unwrap();
    frame.poll_frame().unwrap();
    let snapshot = navigation(&handle);
    assert_eq!(snapshot["workspaceActivity"], serde_json::Value::Null);
    assert_eq!(
        snapshot["workspaceSettings"],
        serde_json::json!({"workspace": two, "tab": "permissions"})
    );
    handle
        .navigate(
            &mut frame,
            serde_json::from_value(serde_json::json!({"kind":"close-workspace-settings","generation":generation,"workspace":two})).unwrap(),
        )
        .unwrap();
    frame.poll_frame().unwrap();
    assert_eq!(
        navigation(&handle)["workspaceSettings"],
        serde_json::Value::Null
    );
    assert!(
        handle.errors.borrow().is_empty(),
        "{:?}",
        handle.errors.borrow()
    );
    frame.close_components();
    let _ = frame.take_component_close_reports();
}
