//! A shortcut's triggers start it without anyone invoking it: a change a
//! person's app committed to a record in the workspace fires the shortcut
//! saved there, on their own device, once, and a schedule fires once per
//! fire time and keeps its place across a restart.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion"
)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use settings_store::notification_policy::NotificationPolicy;
use tilo_automation::{
    CatchUp, FOCUS_DO_NOT_DISTURB, FocusTransition, SavedShortcut, Schedule, Shortcut, TriggerSpec,
};
use tilo_compositor::{InputEvent, OutputConfig};
use tilo_desktop_model::{RoomId, Workspace, WorkspaceId};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The workspace the desk shows, its room, and the app whose records it
/// holds.
const ROOM: &str = "5c07c075-0000-4000-8000-000000000042";
const PACKAGE: &str = "so.tilo.chat";
const KIND: &str = "so.tilo.chat.message";
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

/// What a key sends, evdev codes.
const KEY_J: u32 = 36;
const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_ALT: u32 = 56;

/// This computer: a device of the person's, certified for their own root key,
/// kept in an identity root of the test's own. Returns the root, which must
/// outlive the session, and the device's key and certificate, which sign what
/// the person saves.
fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (
    tempfile::TempDir,
    ed25519_dalek::SigningKey,
    tilo_user_identity::DeviceCert,
) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x21; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
    tilo_user_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
        },
    ))));
    (root, device, cert)
}

/// The person a device certificate proves, as the desktop names them.
fn person_of(cert: &tilo_user_identity::DeviceCert) -> String {
    cert.user_public_key
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// A session keeping this device's desk under `state`, with the action plane
/// the shortcuts run on at `socket`, for `person`.
fn session_keeping(state: &Path, socket: &Path, person: &str) -> Session {
    session_launching(state, socket, person, Arc::new(Launches::default()))
}

/// The same session, starting its apps from `launches`: the record of what it
/// started is the test's, so it can start one.
fn session_launching(
    state: &Path,
    socket: &Path,
    person: &str,
    launches: Arc<Launches>,
) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            action_plane: ActionPlane::At(socket.to_path_buf()),
            person: person.to_owned(),
            wake: LoopWake::default(),
            launches,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The shortcut named `Tidy` in [`ROOM`], with `trigger`, saved by this
/// computer's person.
fn tidy_with(
    trigger: &TriggerSpec,
    device: &ed25519_dalek::SigningKey,
    cert: &tilo_user_identity::DeviceCert,
) -> SavedShortcut {
    let mut shortcut: Shortcut = serde_json::from_value(serde_json::json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [trigger],
    }))
    .expect("shortcut JSON");
    tilo_peer_state::room::sign_saved_shortcut(ROOM, &mut shortcut, cert, device)
        .expect("the person's own device saved it");
    SavedShortcut::encode(&shortcut)
}

/// Put `workspace` on this session's desk, in place of the one it showed for
/// the same room.
fn show(session: &Session, workspace: Workspace) {
    let room = workspace.room();
    let mut tree = session.handle().read(Clone::clone);
    tree.workspaces.retain(|held| held.room() != room);
    tree.workspaces.push(workspace);
    session.handle().replace_tree(tree);
}

/// Put a workspace for [`ROOM`] holding `shortcuts` on this session's desk.
fn show_room(session: &Session, shortcuts: Vec<SavedShortcut>) {
    let room = RoomId::parse(ROOM).expect("a room id");
    let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Chat");
    workspace.shortcuts = shortcuts;
    show(session, workspace);
}

/// Put a workspace for [`ROOM`] on this session's desk whose `person` reads
/// the folder `folder` through the capability `capability`, holding
/// `shortcuts`.
fn show_room_sharing(
    session: &Session,
    person: &str,
    capability: &str,
    folder: &Path,
    shortcuts: Vec<SavedShortcut>,
) {
    use collab_types::caps::{Access, CapOverride, Scope, Subject};

    let room = RoomId::parse(ROOM).expect("a room id");
    let mut workspace = Workspace::in_room(WorkspaceId::of_room(room), room, "Chat");
    workspace.set_owner(person);
    workspace.sandbox.push(
        CapOverride::grant(
            Subject::person(person),
            Scope::FsPath(folder.to_string_lossy().into_owned()),
            Access::Read,
        )
        .with_id(capability),
    );
    workspace.shortcuts = shortcuts;
    show(session, workspace);
}

/// Press `keys` together and let go of them in reverse, as a person does.
fn chord(session: &mut Session, keys: &[u32]) {
    for keycode in keys {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: *keycode,
                pressed: true,
            })
            .expect("the key");
        session.step(Duration::from_millis(1)).expect("step");
    }
    for keycode in keys.iter().rev() {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: *keycode,
                pressed: false,
            })
            .expect("the key");
        session.step(Duration::from_millis(1)).expect("step");
    }
}

/// Step the session past `span` of its passing time.
fn pass(session: &mut Session, span: Duration) {
    let started = std::time::Instant::now();
    while started.elapsed() < span {
        session.step(Duration::from_millis(10)).expect("step");
    }
}

/// Save `rows` (`(id, kind)`) as `package`'s index of `room` holds them, the
/// way the app of a workspace writes it: published in one rename.
fn write_index(home: &tilo_test_env::TestHome, rows: &[(&str, &str)]) {
    let directory =
        app_state::room_index_directory(home.data(), PACKAGE, ROOM).expect("an index directory");
    let staged = directory.with_file_name("staging-chat");
    for old in [&staged, &directory] {
        if old.exists() {
            std::fs::remove_dir_all(old).expect("the index it replaces");
        }
    }
    std::fs::create_dir_all(&staged).expect("the index's directory");
    let mut index = automerge::AutoCommit::new();
    for (id, kind) in rows {
        let id = ui::record::RecordId::parse(id).expect("a record id");
        assert!(
            ui::record::room::reference(&mut index, &id, &ui::record::RecordKind::new(*kind)),
            "the index names {id}"
        );
    }
    std::fs::write(staged.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");
    std::fs::rename(&staged, &directory).expect("the index published");
}

/// Where the record document lives.
fn record_path(home: &tilo_test_env::TestHome) -> std::path::PathBuf {
    app_state::DocStore::room_record_dir_at(home.data(), PACKAGE, ROOM, RECORD)
        .expect("the record's directory")
        .join(app_state::DOCUMENT_FILE)
}

/// The record as an app of the workspace wrote it before the session started.
fn seed_record(home: &tilo_test_env::TestHome) {
    let mut doc = automerge::AutoCommit::new();
    doc.put(automerge::ROOT, "seed", 1).expect("a field");
    doc.commit();
    let path = record_path(home);
    std::fs::create_dir_all(path.parent().expect("the record's directory")).expect("the directory");
    std::fs::write(&path, doc.save()).expect("the record");
}

/// What the app commits when its person sends a message: a change whose
/// history entry names the verb that made it.
fn a_posted_message_change() -> String {
    use tilo_actions::{ActionContext, ActionInvoker, ActionKey, Described, Did};
    ActionContext::for_app(PACKAGE, "Chat")
        .with_invoker(ActionInvoker::actor("bob"))
        .with_described(Described::new(Did::verb(
            ActionKey::new(PACKAGE, "chat.workspace", "post"),
            [],
        )))
        .change_message()
        .expect("a change message")
}

/// The app commits one message to the record, as the person's app does.
fn post_a_message(home: &tilo_test_env::TestHome) {
    let path = record_path(home);
    let bytes = std::fs::read(&path).expect("the record");
    let mut doc = automerge::AutoCommit::load(&bytes).expect("the record document");
    let before = doc.length(&automerge::ROOT);
    doc.put(automerge::ROOT, format!("message-{before}"), "hi")
        .expect("a field");
    doc.commit_with(
        automerge::transaction::CommitOptions::default().with_message(a_posted_message_change()),
    )
    .expect("a committed change");
    std::fs::write(&path, doc.save()).expect("the record written");
}

/// Step the session until it started a shortcut `wanted` run.
#[track_caller]
fn until_a_run(session: &mut Session, wanted: usize) -> Vec<String> {
    let started = std::time::Instant::now();
    loop {
        session.step(Duration::from_millis(2)).expect("step");
        let runs = session.take_finished_shortcut_runs();
        if runs.len() >= wanted {
            return runs.iter().map(|run| run.shortcut.id.clone()).collect();
        }
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "no shortcut run started"
        );
    }
}

#[test]
fn an_edit_that_matches_a_rule_fires_its_action_once() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    write_index(&home, &[(RECORD, KIND)]);
    seed_record(&home);

    let mut session = session_keeping(state.path(), &socket, &person);
    session.step(Duration::from_millis(1)).expect("a frame");
    show_room(
        &session,
        vec![tidy_with(
            &TriggerSpec::RecordChanged {
                kind: KIND.to_owned(),
                actions: Vec::new(),
                fields: Vec::new(),
                by: tilo_automation::ActorFilter::default(),
            },
            &device,
            &cert,
        )],
    );
    // The pass that shows the shortcut catches up with the record: the first
    // look at a record only learns its heads, so a message posted before the
    // shortcut is on the desk is not one it fires on.
    session.step(Duration::from_millis(1)).expect("a frame");
    post_a_message(&home);
    let runs = until_a_run(&mut session, 1);
    // Many more passes over the same change start nothing: a fire time is
    // started once, by its run's id.
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    let again = session.take_finished_shortcut_runs();
    assert!(again.is_empty(), "the same change started another run");
    assert_eq!(runs, vec!["tidy".to_string()], "the shortcut ran once");
}

#[test]
fn a_shortcut_saved_by_someone_elses_device_fires_nothing_here() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, _device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    write_index(&home, &[(RECORD, KIND)]);
    seed_record(&home);

    // Another person's device saved the shortcut in this room.
    let other_person = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
    let other_device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    let mut other_cert = tilo_user_identity::DeviceCert {
        user_public_key: other_person.verifying_key().to_bytes(),
        device_public_key: other_device.verifying_key().to_bytes(),
        device_label: "Their laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut other_cert, &other_person).expect("their device");

    let mut session = session_keeping(state.path(), &socket, &person);
    session.step(Duration::from_millis(1)).expect("a frame");
    show_room(
        &session,
        vec![tidy_with(
            &TriggerSpec::RecordChanged {
                kind: KIND.to_owned(),
                actions: Vec::new(),
                fields: Vec::new(),
                by: tilo_automation::ActorFilter::default(),
            },
            &other_device,
            &other_cert,
        )],
    );
    post_a_message(&home);
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "a shortcut fires on the behalf of the person who saved it, never someone else's"
    );
}

/// The schedule ledger this device keeps.
fn ledger_path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join("automation").join("schedules.json")
}

/// The one schedule the ledger holds, as (its key, its trigger, its next fire
/// time).
fn only_schedule(state: &Path) -> (String, serde_json::Value, i64) {
    let text = std::fs::read_to_string(ledger_path(state)).expect("the schedule ledger");
    let ledger: serde_json::Value = serde_json::from_str(&text).expect("the ledger's JSON");
    let schedules = ledger["schedules"].as_object().expect("the schedules");
    assert_eq!(schedules.len(), 1, "one schedule is armed");
    let (key, entry) = schedules.iter().next().expect("the schedule");
    (
        key.clone(),
        entry["trigger"].clone(),
        entry["next_fire_ms"].as_i64().expect("a fire time"),
    )
}

/// The ledger a device that was off when a fire time came finds: the
/// schedule it kept, its next fire `due` milliseconds past the epoch.
fn keep_a_schedule_due_at(state: &Path, trigger: &TriggerSpec, due: i64) {
    let ledger = serde_json::json!({
        "schedules": {
            format!("{ROOM}/tidy/0"): {
                "trigger": serde_json::to_value(trigger).expect("the trigger"),
                "next_fire_ms": due,
            }
        }
    });
    let path = ledger_path(state);
    std::fs::create_dir_all(path.parent().expect("the ledger's directory"))
        .expect("the ledger's directory");
    std::fs::write(path, ledger.to_string()).expect("the ledger written");
}

#[test]
fn a_schedule_fires_once_per_fire_time_and_keeps_its_place_across_a_restart() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let scheduled = |device: &ed25519_dalek::SigningKey, cert: &tilo_user_identity::DeviceCert| {
        tidy_with(
            &TriggerSpec::Schedule {
                schedule: Schedule::Interval { seconds: 3600 },
                catch_up: CatchUp::RunOnce,
            },
            device,
            cert,
        )
    };

    // The hour came and went while the session was not running: the fire time
    // it kept is now in the past, and the session it starts in runs it.
    let trigger = TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds: 3600 },
        catch_up: CatchUp::RunOnce,
    };
    let due = i64::try_from(tilo_clock::unix_now_ms()).unwrap() - 1_000;
    keep_a_schedule_due_at(state.path(), &trigger, due);
    let mut session = session_keeping(state.path(), &socket, &person);
    // The room is on the desk before the first pass, as it is kept with the
    // desk across a restart: a schedule no workspace shows is one this device
    // no longer keeps a fire time for.
    show_room(&session, vec![scheduled(&device, &cert)]);
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "the missed fire time ran once"
    );
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    let (key, entry, next) = only_schedule(state.path());
    assert!(
        key.ends_with("/tidy/0"),
        "the ledger names the shortcut's trigger: {key}"
    );
    assert!(
        next > due,
        "the ledger moved past the fire time it ran: {next} is after {due}"
    );
    assert_eq!(entry["schedule"]["every"], "interval");
    assert_eq!(entry["schedule"]["seconds"], 3600);
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "one fire time, one run"
    );
    drop(session);

    // A third session finds the fire time it moved to and runs nothing.
    let mut session = session_keeping(state.path(), &socket, &person);
    show_room(&session, vec![scheduled(&device, &cert)]);
    // Past a tick, so a fire time the ledger kept would have come up.
    for _ in 0..60 {
        session.step(Duration::from_millis(10)).expect("step");
    }
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "the fire time the ledger kept ran once, across both restarts"
    );
}

#[test]
fn a_shortcut_armed_on_a_chord_runs_once_when_the_person_presses_it() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let mut session = session_keeping(state.path(), &socket, &person);
    session.step(Duration::from_millis(1)).expect("a frame");
    show_room(
        &session,
        vec![tidy_with(
            &TriggerSpec::Keyboard {
                chord: "Ctrl+Alt+J".to_owned(),
            },
            &device,
            &cert,
        )],
    );
    // The pass that shows the shortcut takes its chord for the automation,
    // so the keys reach it rather than the desktop.
    session.step(Duration::from_millis(1)).expect("a frame");
    chord(&mut session, &[KEY_LEFT_CTRL, KEY_LEFT_ALT, KEY_J]);
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "the chord the person pressed started the shortcut"
    );
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "one press, one run"
    );
}

#[test]
fn a_shortcut_armed_on_an_app_runs_once_when_it_starts() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let launches = Arc::new(Launches::default());
    let mut session = session_launching(state.path(), &socket, &person, Arc::clone(&launches));
    show_room(
        &session,
        vec![tidy_with(
            &TriggerSpec::AppOpened {
                app_id: PACKAGE.to_owned(),
            },
            &device,
            &cert,
        )],
    );
    // A tick arms the shortcut and learns the apps running, of which there
    // are none.
    pass(&mut session, Duration::from_millis(700));
    // The session starts the app: its own process runs it.
    launches.started(std::process::id(), PACKAGE, ROOM, false);
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "the app starting started the shortcut"
    );
    // The app keeps running, which starts nothing more.
    pass(&mut session, Duration::from_millis(1200));
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "one start, one run"
    );
}

#[test]
fn a_shortcut_armed_on_a_focus_mode_runs_once_when_it_switches_on() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let mut session = session_keeping(state.path(), &socket, &person);
    show_room(
        &session,
        vec![tidy_with(
            &TriggerSpec::FocusMode {
                mode: FOCUS_DO_NOT_DISTURB.to_owned(),
                transition: FocusTransition::Enter,
            },
            &device,
            &cert,
        )],
    );
    // A tick arms the shortcut and learns that Do Not Disturb is off.
    pass(&mut session, Duration::from_millis(700));
    // The person turns Do Not Disturb on, from quick settings or the menu.
    NotificationPolicy::save_do_not_disturb(true).expect("do not disturb saved");
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "Do Not Disturb switching on started the shortcut"
    );
    pass(&mut session, Duration::from_millis(1200));
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "one switch, one run"
    );
}

#[test]
fn a_shortcut_armed_on_a_shared_folder_runs_once_when_a_file_in_it_changes() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(home.env());
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let folder = tempfile::tempdir().expect("a shared folder");
    let mut session = session_keeping(state.path(), &socket, &person);
    show_room_sharing(
        &session,
        &person,
        "notes",
        folder.path(),
        vec![tidy_with(
            &TriggerSpec::FileChanged {
                capability: "notes".to_owned(),
            },
            &device,
            &cert,
        )],
    );
    // A tick arms the shortcut and starts watching the folder it shares.
    pass(&mut session, Duration::from_millis(1200));
    std::fs::write(folder.path().join("groceries.md"), "milk").expect("a file in the folder");
    // The change is reported once the file stops being written.
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "a file changing in the shared folder started the shortcut"
    );
    pass(&mut session, Duration::from_millis(1200));
    assert!(
        session.take_finished_shortcut_runs().is_empty(),
        "one change, one run"
    );
}
