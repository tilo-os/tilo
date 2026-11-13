//! A shortcut's triggers start it without anyone invoking it: a change a person's app committed to
//! a record in the workspace fires the shortcut saved there, on their own device, once, and a
//! schedule fires once per fire time and keeps its place across a restart.
//!
//! A shortcut is one of its workspace's settings: every test saves it the way a person's save does
//! — signed by this computer's own device and committed into the workspace's document — and what
//! the desk shows of it is that document, as a desk this device kept shows the shortcuts of the
//! session before.

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
use std::time::{Duration, SystemTime};

use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use settings_store::notification_policy::NotificationPolicy;
use tilo_automation::{
    CatchUp, FOCUS_DO_NOT_DISTURB, FocusTransition, SavedShortcut, Schedule, Shortcut,
    ShortcutChange, TriggerSpec,
};
use tilo_compositor::InputEvent;
use tilo_desktop_model::{DesktopTree, Workspace, WorkspaceId, WorkspaceSlot};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
use tilo_peer_state::owner::{self, NativeWorkspaceOwner};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_service::LoopWake;
use tilo_session::{ActionPlane, Launches, Options, Session};
use tilo_user_identity::DeviceCert;

/// The workspace the desk shows, the id it has on every device, and the app whose records it holds.
const WORKSPACE: &str = "5c07c075-0000-4000-8000-000000000042";
const PACKAGE: &str = "so.tilo.chat";
const KIND: &str = "so.tilo.chat.message";
const RECORD: &str = "uuid:5c07c075-0000-4000-8000-0000000000aa";

/// What a key sends, evdev codes.
const KEY_J: u32 = 36;
const KEY_LEFT_CTRL: u32 = 29;
const KEY_LEFT_ALT: u32 = 56;

/// The workspace as its own id.
fn workspace_id() -> WorkspaceId {
    WorkspaceId::parse(WORKSPACE).expect("a workspace id")
}

/// This computer: the identity store's provisioned identity, and the device it enrolled here.
/// Returns the root, which must outlive the session, and the device's key and certificate, which
/// sign what the person saves — a shortcut runs under the certificate this device acts with, so an
/// identity that only looks like one would leave nothing to run.
fn this_computer(
    home: &mut tilo_test_env::TestHome,
) -> (tempfile::TempDir, SigningKey, DeviceCert) {
    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    let identities = home.config().join("tilo/identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", root.path())
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &identities,
        &paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    let device =
        tilo_user_identity::persist::load_device_key(&paths).expect("this computer's device key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this computer's certificate");
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
fn person_of(cert: &DeviceCert) -> String {
    cert.user_public_key
        .iter()
        .fold(String::new(), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

/// This device's workspaces as it keeps them under `state`: the record the session's own engine
/// opens, written by the same commits a session writes.
fn held(state: &Path) -> NativeWorkspaceOwner {
    let record = state.join("tilo").join("rooms.json");
    std::fs::create_dir_all(record.parent().expect("the state's workspace directory"))
        .expect("this device's state directory");
    owner::open_file(&owner::owner_file_of(&record)).expect("this device's workspaces")
}

/// Found [`WORKSPACE`] in `owner` as this computer's device — its only member, the person its
/// owner — and take the checkpoint its host publishes: the workspace every other device joins.
fn found(owner: &NativeWorkspaceOwner, key: &SigningKey, cert: &DeviceCert) -> TestGovernor {
    let governor = testing::enroll_interactive_governor_in(owner, key, cert, WORKSPACE)
        .expect("this computer founds the workspace it holds");
    take(owner, &governor);
    governor
}

/// Take what `governor` published into `owner`, as a device of the workspace admits its host's
/// frames.
fn take(owner: &NativeWorkspaceOwner, governor: &TestGovernor) {
    let host = governor.workspace_host().device_public_key;
    take_frames(owner, host, governor.changes());
}

/// Take what the device `from` holds of the workspace into `owner`, which is what the workspace's
/// other devices do with a member's published changes.
fn take_published(owner: &NativeWorkspaceOwner, from: &NativeWorkspaceOwner, device: [u8; 32]) {
    let messages = from
        .with_outgoing(workspace_id(), SystemTime::now(), |_, outgoing| {
            let messages: Result<Vec<_>, String> = outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string()))
                .collect();
            messages
        })
        .expect("what this device published");
    for message in messages {
        owner
            .receive(
                workspace_id(),
                TransportOrigin::RelayDelta(device),
                message,
                SystemTime::now(),
            )
            .expect("a member's change is admitted");
    }
}

/// Admit `frames`, published by the device `publisher`, into `owner`'s workspace.
fn take_frames<'a>(
    owner: &NativeWorkspaceOwner,
    publisher: [u8; 32],
    frames: impl Iterator<Item = (&'a [u8], Retention)>,
) {
    let frames: Vec<(Vec<u8>, Retention)> = frames
        .map(|(bytes, retention)| (bytes.to_vec(), retention))
        .collect();
    for (bytes, retention) in frames {
        let origin = match retention {
            Retention::Checkpoint => TransportOrigin::RetainedRelayDelta(publisher),
            Retention::Delta => TransportOrigin::RelayDelta(publisher),
        };
        owner
            .receive(
                workspace_id(),
                origin,
                postcard::from_bytes(&bytes).expect("a publication"),
                SystemTime::now(),
            )
            .expect("the workspace admits what its device published");
    }
}

/// Commit `settings` into the workspace as this computer's device: the call the session's own save
/// makes when a person saves a shortcut, which the workspace's admission decides here and on every
/// device that receives it.
fn commit(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    cert: &DeviceCert,
    settings: SettingsChange,
) {
    let request = OwnerCommitRequest {
        workspace: WORKSPACE.to_owned(),
        operation: CommitOperation::WorkspaceMembershipCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: cert.clone(),
            command: WorkspaceMembershipCommand::Settings(settings),
        },
    };
    pollster::block_on(owner.commit_workspace_membership_command(request, key, SystemTime::now()))
        .expect("the workspace admits this device's save");
}

/// Save `shortcut` in the workspace, signed by `cert`'s device with `key`: one of the workspace's
/// settings, which every device of it reads from its document. Answers the shortcut as the
/// document holds it, which is also what a desk kept across a restart shows of it.
fn save(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    cert: &DeviceCert,
    mut shortcut: Shortcut,
) -> SavedShortcut {
    tilo_peer_state::workspace::sign_saved_shortcut(WORKSPACE, &mut shortcut, cert, key)
        .expect("the person's own device saved it");
    let saved = SavedShortcut::encode(&shortcut);
    commit(
        owner,
        key,
        cert,
        SettingsChange::Shortcuts(ShortcutChange::Save(Box::new(shortcut))),
    );
    saved
}

/// [`WORKSPACE`] as `person` 's own, showing the `shortcuts` its document holds: what the desk this
/// device keeps shows of the workspace, as the session that kept it left it.
fn room(person: &str, shortcuts: &[SavedShortcut]) -> Workspace {
    let mut workspace = Workspace::in_workspace(
        WorkspaceSlot::of_workspace(workspace_id()),
        workspace_id(),
        "Chat",
    );
    workspace.set_owner(person.to_owned());
    workspace.shortcuts = shortcuts.to_vec();
    workspace
}

/// A session keeping this device's desk under `state` , with the action plane the shortcuts run on
/// at `socket` , for `person` , showing the workspace its governance holds.
fn session_keeping(state: &Path, socket: &Path, person: &str) -> Session {
    session_launching(state, socket, person, Arc::new(Launches::default()))
}

/// The same session, starting its apps from `launches` : the record of what it started is the
/// test's, so it can start one.
fn session_launching(
    state: &Path,
    socket: &Path,
    person: &str,
    launches: Arc<Launches>,
) -> Session {
    crate::common::headless(Options {
        collab: false,
        state: Some(state.to_path_buf()),
        action_plane: ActionPlane::At(socket.to_path_buf()),
        person: person.to_owned(),
        wake: LoopWake::default(),
        launches,
        ..Options::default()
    })
}

/// Put `workspace` on the session's desk, in place of the one it showed for the same workspace.
fn show(session: &Session, workspace: Workspace) {
    let id = workspace.id();
    let mut tree: DesktopTree = session.handle().read(Clone::clone);
    tree.workspaces.retain(|held| held.id() != id);
    tree.active = workspace.slot;
    tree.workspaces.push(workspace);
    session.handle().replace_tree(tree);
}

/// The session on the desk showing `workspace` : the desk a device kept, before the first pass
/// reads it, whose governance this device holds — and the passes in which that governance's
/// shortcuts arm the triggers and the workspace's records are followed.
fn showing(state: &Path, socket: &Path, person: &str, workspace: Workspace) -> Session {
    let mut session = session_keeping(state, socket, person);
    show(&session, workspace);
    for _ in 0..2 {
        session.step(Duration::from_millis(1)).expect("a frame");
    }
    session
}

/// The shortcut named `Tidy` in [`WORKSPACE`], with `trigger` .
fn tidy_with(trigger: &TriggerSpec) -> Shortcut {
    serde_json::from_value(serde_json::json!({
        "id": "tidy",
        "name": "Tidy",
        "steps": [],
        "triggers": [trigger],
    }))
    .expect("shortcut JSON")
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

/// Save `rows` (`(id, kind)`) as `package`'s index of `workspace` holds them,
/// the way the app of a workspace writes it: published in one rename.
fn write_index(home: &tilo_test_env::TestHome, rows: &[(&str, &str)]) {
    let directory = app_state::workspace_index_directory(home.data(), PACKAGE, WORKSPACE)
        .expect("an index directory");
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
            ui::record::index::reference(&mut index, &id, &ui::record::RecordKind::new(*kind)),
            "the index names {id}"
        );
    }
    std::fs::write(staged.join(app_state::DOCUMENT_FILE), index.save()).expect("the index");
    std::fs::rename(&staged, &directory).expect("the index published");
}

/// Where the record document lives.
fn record_path(home: &tilo_test_env::TestHome) -> std::path::PathBuf {
    app_state::DocStore::workspace_document_dir_at(home.data(), PACKAGE, WORKSPACE, RECORD)
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

/// What the app commits when its person sends a message: a change whose history entry names the
/// verb that made it.
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
    let mut seen = Vec::new();
    // A firing waits for the ledger that records it to be on disk, and a writer busy with the rest
    // of the suite is what makes it take a while: the bound is the suite's own, not a second count.
    tilo_test_env::wait_for("a shortcut run to start", || {
        session.step(Duration::from_millis(2)).expect("step");
        seen.extend(session.take_finished_shortcut_runs());
        (seen.len() >= wanted).then_some(())
    });
    seen.iter().map(|run| run.shortcut.id.clone()).collect()
}

#[test]
fn an_edit_that_matches_a_rule_fires_its_action_once() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    write_index(&home, &[(RECORD, KIND)]);
    seed_record(&home);
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(
        &owner,
        &device,
        &cert,
        tidy_with(&TriggerSpec::RecordChanged {
            kind: KIND.to_owned(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: tilo_automation::ActorFilter::default(),
        }),
    );
    drop(owner);

    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&tidy)),
    );
    // The pass that shows the shortcut catches up with the record: the first look at a record only
    // learns its heads, so a message posted before the shortcut is on the desk is not one it fires
    // on.
    session.step(Duration::from_millis(1)).expect("a frame");
    post_a_message(&home);
    let runs = until_a_run(&mut session, 1);
    // Many more passes over the same change start nothing: a fire time is started once, by its
    // run's id.
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    write_index(&home, &[(RECORD, KIND)]);
    seed_record(&home);
    let owner = held(state.path());
    let governor = found(&owner, &device, &cert);

    // Another person joined this workspace, and their own device saved a shortcut of theirs in it.
    let their_root = SigningKey::from_bytes(&[0x31; 32]);
    let their_device = SigningKey::from_bytes(&[0x32; 32]);
    let theirs = testing::certificate(&their_root, &their_device, "Their laptop");
    let their_state = tempfile::tempdir().expect("their state directory");
    let their_owner = held(their_state.path());
    let governor = testing::enroll_into(governor, &their_owner, &their_device, &theirs)
        .expect("the workspace admits their device");
    // This device takes the admission of their device, as it takes every member's.
    take(&owner, &governor);
    let theirs_saved = save(
        &their_owner,
        &their_device,
        &theirs,
        tidy_with(&TriggerSpec::RecordChanged {
            kind: KIND.to_owned(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: tilo_automation::ActorFilter::default(),
        }),
    );
    take_published(
        &owner,
        &their_owner,
        their_device.verifying_key().to_bytes(),
    );
    drop(their_owner);
    drop(owner);

    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&theirs_saved)),
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

/// The one schedule the ledger holds, as (its key, its trigger, its next fire time).
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

/// The ledger a device that was off when a fire time came finds: the schedule it kept, its next
/// fire `due` milliseconds past the epoch.
fn keep_a_schedule_due_at(state: &Path, trigger: &TriggerSpec, due: i64) {
    let ledger = serde_json::json!({
        "schedules": {
            format!("{WORKSPACE}/tidy/0"): {
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let trigger = TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds: 3600 },
        catch_up: CatchUp::RunOnce,
    };
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(&owner, &device, &cert, tidy_with(&trigger));
    drop(owner);

    // The hour came and went while the session was not running: the fire time it kept is now in the
    // past, and the session it starts in runs it.
    let due = i64::try_from(tilo_clock::unix_now_ms()).unwrap() - 1_000;
    keep_a_schedule_due_at(state.path(), &trigger, due);
    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&tidy)),
    );
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "the missed fire time ran once"
    );
    // The ledger's move past the fire time is a write of the session's to disk: a counted number of
    // frames is not that write landing.
    let (key, entry, next) =
        tilo_test_env::wait_for("the ledger to move past the fire time it ran", || {
            session.step(Duration::from_millis(2)).expect("step");
            let (key, entry, next) = only_schedule(state.path());
            (next > due).then_some((key, entry, next))
        });
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
    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&tidy)),
    );
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(
        &owner,
        &device,
        &cert,
        tidy_with(&TriggerSpec::Keyboard {
            chord: "Ctrl+Alt+J".to_owned(),
        }),
    );
    drop(owner);

    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&tidy)),
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(
        &owner,
        &device,
        &cert,
        tidy_with(&TriggerSpec::AppOpened {
            app_id: PACKAGE.to_owned(),
        }),
    );
    drop(owner);

    let launches = Arc::new(Launches::default());
    let mut session = session_launching(state.path(), &socket, &person, Arc::clone(&launches));
    show(&session, room(&person, std::slice::from_ref(&tidy)));
    // A tick arms the shortcut and learns the apps running, of which there are none.
    pass(&mut session, Duration::from_millis(700));
    // The session starts the app: its own process runs it.
    launches.started(std::process::id(), PACKAGE, WORKSPACE, false);
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(
        &owner,
        &device,
        &cert,
        tidy_with(&TriggerSpec::FocusMode {
            mode: FOCUS_DO_NOT_DISTURB.to_owned(),
            transition: FocusTransition::Enter,
        }),
    );
    drop(owner);

    let mut session = showing(
        state.path(),
        &socket,
        &person,
        room(&person, std::slice::from_ref(&tidy)),
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
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    let folder = tempfile::tempdir().expect("a shared folder");
    let owner = held(state.path());
    found(&owner, &device, &cert);
    let tidy = save(
        &owner,
        &device,
        &cert,
        tidy_with(&TriggerSpec::FileChanged {
            capability: "notes".to_owned(),
        }),
    );
    drop(owner);

    // The folder the shortcut watches is the one the workspace shares with this person, as the
    // desk keeps it.
    let mut workspace = room(&person, std::slice::from_ref(&tidy));
    workspace
        .sandbox
        .push(sharing(&person, "notes", folder.path()));
    let mut session = showing(state.path(), &socket, &person, workspace);
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

/// The workspace's capability `id` letting `person` read the folder at `folder`.
fn sharing(person: &str, id: &str, folder: &Path) -> collab_types::caps::CapOverride {
    use collab_types::caps::{Access, CapOverride, Scope, Subject};
    CapOverride::grant(
        Subject::person(person),
        Scope::FsPath(folder.to_string_lossy().into_owned()),
        Access::Read,
    )
    .with_id(id)
}
