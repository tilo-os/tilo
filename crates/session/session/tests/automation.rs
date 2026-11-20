//! A shortcut's triggers start it without anyone invoking it: a change a person's app committed to
//! a record in the workspace fires the shortcut saved there, on their own device, once, and a
//! schedule fires once per fire time and keeps its place across a restart.
//!
//! A shortcut is one of its workspace's settings: every test saves it the way a person's save does
//! — signed by this computer's own device and committed into the workspace's document — and what
//! the shell shows of it is that document, as a shell this device kept shows the shortcuts of the
//! session before.
//!
//! An app's resource is the workspace's: a test's app is a window on the app wire, joined to its
//! resource's route and writing there, so its change reaches the workspace as every app's write
//! does — committed by the broker, which notes it in the change stream.

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
use tilo_desktop_model::{Layout, Workspace, WorkspaceId};
use tilo_peer_state::TransportOrigin;
use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
use tilo_peer_state::owner::{self, NativeWorkspaceOwner};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_service::LoopWake;
use tilo_session::{AppSocket, Launches, Options, Records, SearchOptions, Session};
use tilo_user_identity::DeviceCert;

/// The workspace the shell shows, the id it has on every device, and the app whose records it holds.
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
                TransportOrigin::ServerDelta(device),
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
    let request = OwnerCommitRequest {
        workspace: workspace.to_owned(),
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

/// The workspace this desktop starts from: the person's own, placed on this device's own workspace
/// service and held as a member, as every made workspace is. The session after it starts on the
/// shell this one kept, so the workspace is held here before its engine opens — which is what makes
/// the session's own roles, the resources a shortcut follows among them, read what the workspace
/// holds.
fn made_here(
    data: &Path,
    state: &Path,
    socket: &Path,
    person: &str,
    launches: Arc<Launches>,
) -> String {
    let mut first = session_launching(data, state, socket, person, launches);
    // The workspace a person's device starts from is theirs: the session founds it on this device's
    // own workspace service, off its loop, so a pass after the founding holds it.
    let workspace = tilo_test_env::wait_for("the workspace the first session holds", || {
        first.step(Duration::from_millis(2)).expect("a frame");
        let held = first.held_workspaces();
        assert!(
            held.len() <= 1,
            "a device that starts for the first time holds one workspace: {held:?}"
        );
        held.into_iter().next()
    });
    drop(first);
    workspace
}

/// A word one side of the window says and the other waits for, as long as the test's suite allows.
#[derive(Default)]
struct Said {
    said: std::sync::Mutex<bool>,
    changed: std::sync::Condvar,
}

impl Said {
    /// Say it.
    fn say(&self) {
        *self.said.lock().expect("the word") = true;
        self.changed.notify_all();
    }

    /// Wait for it, up to `within`; whether it was said.
    fn taken(&self, within: Duration) -> bool {
        let said = self.said.lock().expect("the word");
        let (_said, waited) = self
            .changed
            .wait_timeout_while(said, within, |said| !*said)
            .expect("the word");
        !waited.timed_out()
    }
}

/// An app's window on the app wire: the app's process at this test, joined to its resource's route
/// in [`WORKSPACE`] and holding the resource's document, as a window of the app holds it. Its
/// founding write reaches the workspace when it joins, and the edit it is told to make reaches it
/// through the same wire when the test says so — what the broker commits into the workspace and
/// notes in the change stream, as it does for every app write.
struct Window {
    /// The app's edit, made and sent when the test says.
    edit: Arc<Said>,
    /// The test's last word: the window ends once it is said.
    stop: Arc<Said>,
    /// The window's ending: what it came to, once it was told to stop.
    ended: Arc<Said>,
    outcome: Arc<std::sync::Mutex<Option<Result<(), String>>>>,
    handle: std::thread::JoinHandle<()>,
}

impl Window {
    /// This app's window on the wire at `socket`, in `workspace` , which this device's service
    /// holds: joined to its resource's route as the app's own, the resource founded with what the
    /// window keeps in it, waiting to make the app's edit.
    fn open(socket: &Path, workspace: &str) -> Self {
        let socket = socket.to_path_buf();
        let workspace = workspace.to_owned();
        let (edit, stop) = (Arc::new(Said::default()), Arc::new(Said::default()));
        let ended = Arc::new(Said::default());
        let outcome = Arc::new(std::sync::Mutex::new(None));
        let (editing, stopping, ending, said) = (
            Arc::clone(&edit),
            Arc::clone(&stop),
            Arc::clone(&ended),
            Arc::clone(&outcome),
        );
        let handle = std::thread::spawn(move || {
            *said.lock().expect("the outcome") =
                Some(window_writes(&socket, &workspace, &editing, &stopping));
            ending.say();
        });
        Self {
            edit,
            stop,
            ended,
            outcome,
            handle,
        }
    }

    /// Make the app's edit to its resource: the change the window's person commits to it.
    fn commit_edit(&self) {
        self.edit.say();
    }

    /// Why the window ended before the test was done with it, if it did.
    fn ended_early(&self) -> Option<String> {
        self.ended.taken(Duration::from_millis(1)).then(|| {
            let outcome = self.outcome.lock().expect("the outcome").take();
            outcome.map_or_else(
                || "the window ended before the edit".to_owned(),
                |outcome| match outcome {
                    Ok(()) => "the window ended before the edit".to_owned(),
                    Err(error) => error,
                },
            )
        })
    }

    /// The window's ending, which is what the app's edit came to: what the wire made of it.
    fn finish(self) -> Result<(), String> {
        self.stop.say();
        if !self.ended.taken(tilo_test_env::hang_bound()) {
            return Err("the window never answered".to_owned());
        }
        self.handle.join().expect("the window's thread");
        self.outcome
            .lock()
            .expect("the outcome")
            .take()
            .expect("the window's outcome")
    }
}

/// The app's window on the app wire at `socket`: joined to [`RECORD`]'s route in [`WORKSPACE`] as
/// the app's own document, the resource founded with what the window keeps in it, and the app's
/// edit committed to the resource once `editing` says — each write the app's, on the wire the
/// broker commits into the workspace.
fn window_writes(
    socket: &Path,
    workspace: &str,
    editing: &Said,
    stopping: &Said,
) -> Result<(), String> {
    let cluster = uuid::Uuid::parse_str(workspace).map_err(|error| error.to_string())?;
    let id = ui::resource::DocumentId::parse(RECORD).ok_or("a resource id")?;
    let kind = ui::resource::ResourceType::new(KIND);
    let mut app_wire = std::os::unix::net::UnixStream::connect(socket)
        .map_err(|error| format!("the app wire's socket: {error}"))?;
    launcher::services::write_collab_handoff(&mut app_wire, PACKAGE, workspace)
        .map_err(|error| error.to_string())?;
    let mut identity =
        std::os::unix::net::UnixStream::connect(launcher::services::identity_socket())
            .map_err(|error| format!("the identity service's socket: {error}"))?;
    launcher::services::write_identity_handshake(&mut identity, PACKAGE, workspace)
        .map_err(|error| error.to_string())?;
    // The window's own path to its resource: its connection and identity, as the launcher hands
    // an app both, and the manager a native window joins its resource with.
    let mut joined = collaboration::CollaborationManager::join_instance(
        &kind,
        std::os::fd::OwnedFd::from(app_wire),
        std::os::fd::AsFd::as_fd(&identity),
        collaboration::RouteSpec::document(PACKAGE, cluster, RECORD),
    )
    .map_err(|error| format!("the window joins: {error:?}"))?
    .finish_join()
    .map_err(|error| format!("the window's resource is joined: {error:?}"))?;

    // The resource as the app keeps it: stamped with what it is, and what the app holds in it
    // before the person's edit.
    let mut doc = automerge::AutoCommit::new();
    ui::resource::init(&mut doc, &id, &kind, workspace);
    doc.put(automerge::ROOT, "seed", 1)
        .map_err(|error| error.to_string())?;
    doc.commit();
    let founded = doc.save();
    // What the person's edit is made on: the resource as the window holds it, once it founded it.
    let founded_heads = doc.get_heads();
    send(
        &mut joined,
        collaboration::CollabRequest::document_snapshot(founded),
    )?;

    // Nothing of the app's is written until the person edits the resource.
    if !editing.taken(tilo_test_env::hang_bound()) {
        return Err("the test never made the edit".to_owned());
    }
    doc.put(
        automerge::ROOT,
        format!("message-{}", doc.length(automerge::ROOT)),
        "hi",
    )
    .map_err(|error| error.to_string())?;
    doc.commit_with(
        automerge::transaction::CommitOptions::default().with_message(a_posted_message_change()),
    );
    let change = doc.save_after(&founded_heads);
    send(
        &mut joined,
        collaboration::CollabRequest::document_incremental(change),
    )?;

    // The window stays up, serving its connection, until the test is done with it: a write the
    // connection carried but did not flush would be lost with it.
    // The window keeps serving its connection until the test is done with it: a write the
    // connection carried but did not flush would be lost with it.
    while !stopping.taken(Duration::from_millis(5)) {
        let _ = joined.poll();
    }
    Ok(())
}

/// Send `request` on the window's resource route, again until the connection takes it: the window
/// polls its connection as it goes, as a window does while its route is still joining.
fn send(
    joined: &mut collaboration::CollaborationManager,
    request: collaboration::CollabRequest,
) -> Result<(), String> {
    let started = std::time::Instant::now();
    let mut refused = String::new();
    while started.elapsed() < tilo_test_env::hang_bound() {
        let _ = joined.poll();
        match joined.try_send(request.clone()) {
            Ok(()) => return Ok(()),
            Err(error) => refused = error.to_string(),
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Err(format!("the window's write was never taken: {refused}"))
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

/// What the app commits when its person sends a message: a change whose history entry names the
/// verb that made it.
fn a_posted_message_change() -> String {
    use tilo_actions::{ActionContext, ActionInvoker, ActionKey, Described, Did};
    ActionContext::for_app(PACKAGE, "Chat")
        .with_invoker(ActionInvoker::person("bob"))
        .with_described(Described::new(Did::verb(
            ActionKey::new(PACKAGE, "chat.workspace", "post"),
            [],
        )))
        .change_message()
        .expect("a change message")
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
fn an_edit_that_matches_a_rule_fires_its_action_once() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
    // The app's window is this test's process: the session admits its connection as the launch it
    // recorded.
    let launches = Arc::new(Launches::default());
    // This desktop starts from the person's own workspace, which its first session placed here.
    let workspace = made_here(
        home.data(),
        state.path(),
        &socket,
        &person,
        Arc::clone(&launches),
    );
    launches.started(std::process::id(), PACKAGE, &workspace, true);
    let owner = held(state.path());
    let tidy = save_in(
        &owner,
        &workspace,
        &device,
        &cert,
        tidy_with(&TriggerSpec::ResourceChanged {
            kind: KIND.to_owned(),
            actions: Vec::new(),
            fields: Vec::new(),
            by: tilo_automation::ActorFilter::default(),
        }),
    );
    drop(owner);

    // The shell starts with the workspace before the person's shortcut is on it: the resource is
    // the app's to found, and the rule arms against the resource as it stands.
    let mut session = showing_launching(
        home.data(),
        state.path(),
        &socket,
        &person,
        Arc::clone(&launches),
        kept_in(&person, &workspace, &[]),
    );
    assert!(
        session
            .held_workspaces()
            .iter()
            .any(|held| held == &workspace),
        "the session holds the workspace its shell kept"
    );
    let window = Window::open(&socket, &workspace);
    until_the_resource(&mut session, &window, &workspace);

    // The person's shortcut arrives on the shell: the rule arms, and the first look at the
    // resource only learns its heads, so the app's write of what it already holds never fires it.
    show(
        &session,
        kept_in(&person, &workspace, std::slice::from_ref(&tidy)),
    );
    session.step(Duration::from_millis(2)).expect("a frame");

    // The person's app commits a message to the resource: one change, on the wire its window
    // writes on, which the broker commits into the workspace and notes in the change stream.
    window.commit_edit();
    let runs = until_a_run(&mut session, 1);
    // Many more passes over the same change start nothing: a fire time is started once, by its
    // run's id.
    for _ in 0..50 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    let again = session.take_finished_invocations();
    assert!(again.is_empty(), "the same change started another run");
    assert_eq!(runs, vec!["tidy".to_string()], "the shortcut ran once");
    window.finish().expect("the app's window wrote its edit");
}

#[test]
fn a_shortcut_saved_by_someone_elses_device_fires_nothing_here() {
    let mut home = tilo_test_env::TestHome::new().expect("a home");
    let (_identity, device, cert) = this_computer(&mut home);
    let person = person_of(&cert);
    let state = tempfile::tempdir().expect("a state directory");
    let socket = home.runtime().join("action.sock");
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
        tidy_with(&TriggerSpec::ResourceChanged {
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

    let launches = Arc::new(Launches::default());
    launches.started(std::process::id(), PACKAGE, WORKSPACE, true);
    let mut session = showing_launching(
        home.data(),
        state.path(),
        &socket,
        &person,
        Arc::clone(&launches),
        kept(&person, &[]),
    );
    let window = Window::open(&socket, WORKSPACE);
    until_the_resource(&mut session, &window, WORKSPACE);
    show(&session, kept(&person, std::slice::from_ref(&theirs_saved)));
    session.step(Duration::from_millis(2)).expect("a frame");
    // The app's own change to the resource, on its wire, as the test above makes one.
    window.commit_edit();
    for _ in 0..500 {
        session.step(Duration::from_millis(2)).expect("step");
    }
    assert!(
        session.take_finished_invocations().is_empty(),
        "a shortcut fires on the behalf of the person who saved it, never someone else's"
    );
    window.finish().expect("the app's window wrote its edit");
}

/// Wait, stepping `session` , until the workspace holds the app's resource of the kind the rule
/// names: the app's window founded it on the wire, and this session's engine took the broker's
/// write of it.
fn until_the_resource(session: &mut Session, window: &Window, workspace: &str) {
    tilo_test_env::wait_for("the app's resource in the workspace", || {
        if let Some(error) = window.ended_early() {
            panic!("the app's window ended: {error}");
        }
        session.step(Duration::from_millis(2)).expect("step");
        let held = session
            .read_resource(workspace, PACKAGE, RECORD)
            .ok()
            .flatten();
        held.is_some().then_some(())
    });
    assert!(
        ui::resource::identity(
            &automerge::AutoCommit::load(
                &session
                    .read_resource(workspace, PACKAGE, RECORD)
                    .expect("the resource reads")
                    .expect("the workspace holds the resource")
            )
            .expect("the resource is a document")
        )
        .is_some_and(|checkout| checkout.kind.as_str() == KIND),
        "the workspace holds the app's resource, of the kind the rule names"
    );
}

/// The schedule log this device keeps.
fn log_path(state: &Path) -> std::path::PathBuf {
    state.join("tilo").join("automation").join("schedules.json")
}

/// The one schedule the log holds, as (its key, its trigger, its next fire time).
fn only_schedule(state: &Path) -> (String, serde_json::Value, i64) {
    let text = std::fs::read_to_string(log_path(state)).expect("the schedule log");
    let log: serde_json::Value = serde_json::from_str(&text).expect("the log's JSON");
    let schedules = log["schedules"].as_object().expect("the schedules");
    assert_eq!(schedules.len(), 1, "one schedule is armed");
    let (key, entry) = schedules.iter().next().expect("the schedule");
    (
        key.clone(),
        entry["trigger"].clone(),
        entry["next_fire_ms"].as_i64().expect("a fire time"),
    )
}

/// The log a device that was off when a fire time came finds: the schedule it kept, its next
/// fire `due` milliseconds past the epoch.
fn keep_a_schedule_due_at(state: &Path, trigger: &TriggerSpec, due: i64) {
    let log = serde_json::json!({
        "schedules": {
            format!("{WORKSPACE}/tidy/0"): {
                "trigger": serde_json::to_value(trigger).expect("the trigger"),
                "next_fire_ms": due,
            }
        }
    });
    let path = log_path(state);
    std::fs::create_dir_all(path.parent().expect("the log's directory"))
        .expect("the log's directory");
    std::fs::write(path, log.to_string()).expect("the log written");
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
        home.data(),
        state.path(),
        &socket,
        &person,
        kept(&person, std::slice::from_ref(&tidy)),
    );
    let runs = until_a_run(&mut session, 1);
    assert_eq!(
        runs,
        vec!["tidy".to_string()],
        "the missed fire time ran once"
    );
    // The log's move past the fire time is a write of the session's to disk: a counted number of
    // frames is not that write landing.
    let (key, entry, next) =
        tilo_test_env::wait_for("the log to move past the fire time it ran", || {
            session.step(Duration::from_millis(2)).expect("step");
            let (key, entry, next) = only_schedule(state.path());
            (next > due).then_some((key, entry, next))
        });
    assert!(
        key.ends_with("/tidy/0"),
        "the log names the shortcut's trigger: {key}"
    );
    assert!(
        next > due,
        "the log moved past the fire time it ran: {next} is after {due}"
    );
    assert_eq!(entry["schedule"]["every"], "interval");
    assert_eq!(entry["schedule"]["seconds"], 3600);
    assert!(
        session.take_finished_invocations().is_empty(),
        "one fire time, one run"
    );
    drop(session);

    // A third session finds the fire time it moved to and runs nothing.
    let mut session = showing(
        home.data(),
        state.path(),
        &socket,
        &person,
        kept(&person, std::slice::from_ref(&tidy)),
    );
    // Past a tick, so a fire time the log kept would have come up.
    for _ in 0..60 {
        session.step(Duration::from_millis(10)).expect("step");
    }
    assert!(
        session.take_finished_invocations().is_empty(),
        "the fire time the log kept ran once, across both restarts"
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
    // shell keeps it.
    let mut workspace = kept(&person, std::slice::from_ref(&tidy));
    workspace
        .sandbox
        .push(sharing(&person, "notes", folder.path()));
    let mut session = showing(home.data(), state.path(), &socket, &person, workspace);
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
        session.take_finished_invocations().is_empty(),
        "one change, one run"
    );
}

/// The workspace's capability `id` letting `person` read the folder at `folder`.
fn sharing(person: &str, id: &str, folder: &Path) -> collab_types::caps::CapOverride {
    use collab_types::caps::{Access, CapOverride, Scope};
    CapOverride::grant(
        tilo_action_policy::Grantee::person(person),
        Scope::FsPath(folder.to_string_lossy().into_owned()),
        Access::Read,
    )
    .with_id(id)
}
