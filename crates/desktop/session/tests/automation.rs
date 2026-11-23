//! A shortcut's device triggers start it on this device without anyone invoking it: a chord the
//! person presses, an app that starts, a focus mode switching on. Each runs it once. Triggers on the
//! workspace (records, schedules, folders) fire in the daemon, which its own tests cover.
//!
//! A shortcut is one of its workspace's settings: every test saves it the way a person's save does
//! — signed by this computer's own device and committed into the workspace's document — and what
//! the shell shows of it is that document, as a shell this device kept shows the shortcuts of the
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

use ed25519_dalek::SigningKey;
use neighbour_wire::Retention;
use settings_store::notification_policy::NotificationPolicy;
use tilo_automation::{
    FOCUS_DO_NOT_DISTURB, FocusTransition, SavedShortcut, Shortcut, ShortcutChange, TriggerSpec,
};
use tilo_compositor::InputEvent;
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, workspace::TestWorkspace};
use tilo_peer_state::owner::{self, NativeWorkspaceOwner};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Options, Records, SearchOptions, Session};
use tilo_user_identity::DeviceCert;

/// The workspace the shell shows, the id it has on every device, and the app whose records it holds.
const WORKSPACE: &str = "5c07c075-0000-4000-8000-000000000042";
const PACKAGE: &str = "so.tilo.chat";

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
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    let identities = home.config().join("tilo/identity");
    home.env()
        .set("TILO_IDENTITY_ROOT", root.path())
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity::provisioning::ensure_collaboration_identity(
        &identities,
        &paths,
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("this computer's identity");
    // The file store this device's keys are kept in: a device that keeps its workspace seals what
    // an app writes there, under the workspace's keys.
    home.env().set("TILO_SECRETS_DIR", secrets());
    let device =
        tilo_device_identity::persist::load_device_key(&paths).expect("this computer's device key");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this computer's certificate");
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: "Ada".to_owned(),
            avatar_color: [40, 80, 120],
            device_public_key: None,
        },
    ))));
    (root, device, cert)
}

/// The file store this process keeps the workspaces' keys in: a process opens the store
/// `TILO_SECRETS_DIR` names once, so every session of this test binary keeps its keys in the one
/// store, which outlives each test's home.
fn secrets() -> &'static std::path::Path {
    static STORE: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    STORE.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("tilo-test-secrets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("the secret store's directory");
        dir
    })
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
    owner::open_file(
        &tilo_session_client::engine::owner_path(state).expect("this device's state directory"),
    )
    .expect("this device's workspaces")
}

/// Found [`WORKSPACE`] in `owner` as this computer's device — its only member, the person its
/// owner — and take the checkpoint its host publishes: the workspace every other device joins.
fn found(owner: &NativeWorkspaceOwner, key: &SigningKey, cert: &DeviceCert) -> TestWorkspace {
    let governor = testing::enroll_interactive_in(owner, key, cert, WORKSPACE)
        .expect("this computer founds the workspace it holds");
    take(owner, &governor);
    governor
}

/// Take what `governor` published into `owner`, as a device of the workspace admits its host's
/// frames.
fn take(owner: &NativeWorkspaceOwner, governor: &TestWorkspace) {
    let host = governor.device().device_public_key;
    take_frames(owner, host, governor.changes());
}

/// Admit `frames`, published by the device `publisher`, into `owner`'s workspace.
fn take_frames(
    owner: &NativeWorkspaceOwner,
    publisher: [u8; 32],
    frames: impl IntoIterator<Item = (Vec<u8>, Retention)>,
) {
    let frames: Vec<(Vec<u8>, Retention)> = frames.into_iter().collect();
    for (bytes, retention) in frames {
        let origin = match retention {
            Retention::Checkpoint => TransportOrigin::RetainedServerDelta(publisher),
            Retention::Delta => TransportOrigin::ServerDelta(publisher),
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
    workspace: &str,
    key: &SigningKey,
    cert: &DeviceCert,
    settings: SettingsChange,
) {
    let command = WorkspaceMembershipCommand::Settings(settings);
    let workspace = tilo_desktop_model::WorkspaceId::parse(workspace)
        .expect("the fixture's workspace is a workspace id");
    pollster::block_on(owner.commit_membership(workspace, cert, &command, key, SystemTime::now()))
        .expect("the workspace admits this device's save");
}

/// Save `shortcut` in the workspace, signed by `cert`'s device with `key`: one of the workspace's
/// settings, which every device of it reads from its document. Answers the shortcut as the
/// document holds it, which is also what a shell kept across a restart shows of it.
fn save(
    owner: &NativeWorkspaceOwner,
    key: &SigningKey,
    cert: &DeviceCert,
    shortcut: Shortcut,
) -> SavedShortcut {
    save_in(owner, WORKSPACE, key, cert, shortcut)
}

/// [`save`] in `workspace` : the shortcuts of the workspace the session made as well as those of the
/// one this test founded.
fn save_in(
    owner: &NativeWorkspaceOwner,
    workspace: &str,
    key: &SigningKey,
    cert: &DeviceCert,
    mut shortcut: Shortcut,
) -> SavedShortcut {
    tilo_peer_state::workspace::sign_saved_shortcut(workspace, &mut shortcut, cert, key)
        .expect("the person's own device saved it");
    let saved = SavedShortcut::encode(&shortcut);
    commit(
        owner,
        workspace,
        key,
        cert,
        SettingsChange::Shortcuts(ShortcutChange::Save(Box::new(shortcut))),
    );
    saved
}

/// [`WORKSPACE`] as `person` 's own, showing the `shortcuts` its document holds: what the shell this
/// device keeps shows of the workspace, as the session that kept it left it.
fn kept(person: &str, shortcuts: &[SavedShortcut]) -> Workspace {
    kept_in(person, WORKSPACE, shortcuts)
}

/// [`kept`] for the workspace `workspace` : the one a session of this device made, as well as the
/// one this test founded.
fn kept_in(person: &str, workspace: &str, shortcuts: &[SavedShortcut]) -> Workspace {
    let mut workspace = Workspace::new(
        WorkspaceId::parse(workspace).expect("a workspace id"),
        "Mine",
    );
    workspace.set_owner(person.to_owned());
    workspace.shortcuts = shortcuts.to_vec();
    workspace
}

/// A session keeping this device's shell under `state` , with the app wire the shortcuts run on
/// at `socket` , for `person` , starting its apps from `launches` : the log of what it started is
/// the test's, so an app may speak on its wire as the process it started.
fn session_launching(
    data: &Path,
    state: &Path,
    socket: &Path,
    person: &str,
    launches: Arc<Launches>,
) -> Session {
    crate::common::headless(Options {
        collab: false,
        state: Some(state.to_path_buf()),
        app_socket: AppSocket::At(socket.to_path_buf()),
        person: person.to_owned(),
        wake: LoopWake::default(),
        launches,
        search: SearchOptions {
            records: Some(Records::of(data.to_path_buf())),
            ..SearchOptions::default()
        },
        ..Options::default()
    })
}

/// Put `workspace` on the session's shell, in place of the one it showed for the same workspace.
fn show(session: &Session, workspace: Workspace) {
    let id = workspace.id();
    let mut tree: Layout = session.handle().read(Clone::clone);
    tree.workspaces.retain(|held| held.id() != id);
    tree.active = workspace.id();
    tree.workspaces.push(workspace);
    session.handle().replace_tree(tree);
}

/// The session on the shell showing `workspace` : the shell a device kept, before the first pass
/// reads it, whose governance this device holds — and the passes in which that governance's
/// shortcuts arm the triggers and the workspace's records are followed.
fn showing(
    data: &Path,
    state: &Path,
    socket: &Path,
    person: &str,
    workspace: Workspace,
) -> Session {
    showing_launching(
        data,
        state,
        socket,
        person,
        Arc::new(Launches::default()),
        workspace,
    )
}

/// [`showing`], starting its apps from `launches` : the log of what it started is the test's, so an
/// app may speak on its wire as the process it started.
fn showing_launching(
    data: &Path,
    state: &Path,
    socket: &Path,
    person: &str,
    launches: Arc<Launches>,
    workspace: Workspace,
) -> Session {
    let mut session = session_launching(data, state, socket, person, launches);
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

/// Step the session until it started a shortcut `wanted` run.
#[track_caller]
fn until_a_run(session: &mut Session, wanted: usize) -> Vec<String> {
    let mut seen = Vec::new();
    // A firing waits for the log that records it to be on disk, and a writer busy with the rest
    // of the suite is what makes it take a while: the bound is the suite's own, not a second count.
    tilo_test_env::wait_for("a shortcut run to start", || {
        session.step(Duration::from_millis(2)).expect("step");
        seen.extend(session.take_finished_invocations());
        (seen.len() >= wanted).then_some(())
    });
    seen.iter().map(|run| run.shortcut.id.clone()).collect()
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
        home.data(),
        state.path(),
        &socket,
        &person,
        kept(&person, std::slice::from_ref(&tidy)),
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
        session.take_finished_invocations().is_empty(),
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
    let mut session = session_launching(
        home.data(),
        state.path(),
        &socket,
        &person,
        Arc::clone(&launches),
    );
    show(&session, kept(&person, std::slice::from_ref(&tidy)));
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
        session.take_finished_invocations().is_empty(),
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
        home.data(),
        state.path(),
        &socket,
        &person,
        kept(&person, std::slice::from_ref(&tidy)),
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
        session.take_finished_invocations().is_empty(),
        "one switch, one run"
    );
}
