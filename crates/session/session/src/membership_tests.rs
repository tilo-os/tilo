//! The workspace's own governance takes the people the settings change,
//! and refuses what it refuses, whatever this session holds; and what it has
//! of them is what the session's copy shows.

tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use pollster::FutureExt as _;
use tilo_action_policy::{Change, Grantee, Role, WorkspacePolicy};
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{Workspace, WorkspaceId};
use tilo_network_actor::DeviceIdentity;
use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};
use tilo_session_client::engine::Engine;
use tilo_session_client::membership::{Membership, project};

const WORKSPACE: &str = testing::WORKSPACE;

/// The identity of the device certified as `cert` , signing with `key` .
fn identity(cert: &tilo_user_identity::DeviceCert, key: &SigningKey) -> DeviceIdentity {
    DeviceIdentity::kept(cert.clone(), key.clone())
}

/// This device's engine, holding the workspace it governs, and the session's own view of it with
/// the person in it as its owner. `joiner` joins the workspace too, as their own device, and
/// comes back as the person the workspace knows them by.
fn workspace_here(joiner: &str) -> (Engine, Workspace, String, tempfile::TempDir) {
    workspace_signed_through(joiner, None)
}

/// When a signer that holds its signatures back answers: once the asking loop turned.
#[derive(Default)]
struct SignHold {
    held: std::sync::atomic::AtomicBool,
    waiting: std::sync::Mutex<Vec<std::task::Waker>>,
}

impl SignHold {
    fn held(&self) -> bool {
        self.held.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Hold every signature back from now on.
    fn hold(&self) {
        self.held.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// The loop turned: answer what was held back.
    fn open(&self) {
        self.held.store(false, std::sync::atomic::Ordering::SeqCst);
        let waiting = std::mem::take(&mut *self.waiting.lock().expect("the hold"));
        for waker in waiting {
            waker.wake();
        }
    }
}

/// This device's key behind a hold: what a page's key, which signs through a promise, or a signer
/// that answers through the session's own loop looks like to the code that asks it.
struct LateSigner {
    key: SigningKey,
    hold: std::sync::Arc<SignHold>,
}

impl collab_types::signer::DeviceSigner for LateSigner {
    fn device_public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    fn endpoint_secret(&self) -> Option<[u8; 32]> {
        Some(self.key.to_bytes())
    }

    fn sign(&self, message: Vec<u8>) -> collab_types::signer::SignFuture<'_> {
        let signature = ed25519_dalek::Signer::sign(&self.key, &message).to_bytes();
        let hold = std::sync::Arc::clone(&self.hold);
        Box::pin(std::future::poll_fn(move |context| {
            if hold.held() {
                hold.waiting
                    .lock()
                    .expect("the hold")
                    .push(context.waker().clone());
                if hold.held() {
                    return std::task::Poll::Pending;
                }
            }
            std::task::Poll::Ready(Ok(signature))
        }))
    }
}

/// [`workspace_here`] with this device signing through `hold` when one is given.
fn workspace_signed_through(
    joiner: &str,
    hold: Option<std::sync::Arc<SignHold>>,
) -> (Engine, Workspace, String, tempfile::TempDir) {
    let person_root = SigningKey::from_bytes(&[9; 32]);
    let person_key = SigningKey::from_bytes(&[10; 32]);
    let cert = testing::certificate(&person_root, &person_key, "This device");
    let directory = tempfile::TempDir::new().expect("a state directory");
    let identity = match hold {
        Some(hold) => DeviceIdentity::kept(
            cert.clone(),
            LateSigner {
                key: person_key.clone(),
                hold,
            },
        ),
        None => identity(&cert, &person_key),
    };
    let engine = Engine::open(
        identity,
        Some(directory.path()),
        std::iter::empty(),
        crate::platform::membership_check::of_this_session(),
    )
    .expect("this device's engine opens");
    let governor = TestGovernor::empty_interactive(cert.clone(), person_key.clone(), WORKSPACE)
        .expect("this device governs its workspace");
    let mut governor = testing::enroll_into(governor, engine.owner(), &person_key, &cert)
        .expect("this device's workspace is its own");
    // The person the settings change, as their own device of the same workspace: a person the
    // workspace has a device of is one it has.
    let her_root = SigningKey::from_bytes(&[21; 32]);
    let her_key = SigningKey::from_bytes(&[22; 32]);
    let her = testing::certificate(&her_root, &her_key, joiner);
    let joining = tempfile::TempDir::new().expect("her state directory");
    let hers = tilo_peer_state::owner::open_file(&joining.path().join("owner.json"))
        .expect("her owner opens");
    governor = testing::enroll_into(governor, &hers, &her_key, &her)
        .expect("the workspace admits her device");
    // What her admission changed reaches this device's copy of the workspace, as it would over
    // the workspace's own peer network.
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace");
    let now = tilo_clock::wall();
    for (bytes, _) in governor.outgoing() {
        engine
            .owner()
            .receive(
                workspace_id,
                tilo_peer_state::TransportOrigin::RetainedServerDelta(cert.device_public_key),
                postcard::from_bytes(bytes).expect("a workspace change"),
                now,
            )
            .expect("this device takes the workspace as its governor has it");
    }
    let mut workspace = Workspace::new(workspace_id, "Test");
    workspace
        .policy
        .members
        .insert(hex::encode(cert.user_public_key), Role::Owner);
    workspace
        .policy
        .members
        .insert(joiner.to_owned(), Role::Editor);
    (
        engine,
        workspace,
        hex::encode(her.user_public_key),
        directory,
    )
}

/// The role the workspace `WORKSPACE` gives `person` , as its governance here has it.
fn role_of(engine: &Engine, person: &str) -> Option<Role> {
    engine
        .people(WORKSPACE)
        .expect("the workspace's people")
        .into_iter()
        .find(|held| held.id.person_id() == Some(person) && !held.removed)
        .and_then(|held| held.role)
}

/// Two devices of one workspace: the owner's, which signs for the workspace, and this session's,
/// both holding what the workspace admitted.
struct TwoSessions {
    workspace_id: WorkspaceId,
    owner: Engine,
    owner_cert: tilo_user_identity::DeviceCert,
    session: Engine,
    session_cert: tilo_user_identity::DeviceCert,
    person: String,
    now: tilo_clock::time::SystemTime,
    _states: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
}

impl TwoSessions {
    /// What the owner's device published reaches this session, as the workspace's host carries it.
    fn deliver(&self) {
        let written = self
            .owner
            .owner()
            .with_outgoing(self.workspace_id, self.now, |_, outgoing| {
                outgoing
                    .entries()
                    .map(|entry| {
                        postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<neighbour_wire::NeighbourMsg>, String>>()
            })
            .expect("the owner's device published the change");
        assert!(
            !written.is_empty(),
            "the owner's device published something"
        );
        for message in written {
            self.session
                .owner()
                .receive(
                    self.workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(
                        self.owner_cert.device_public_key,
                    ),
                    message,
                    self.now,
                )
                .expect("this session takes the change");
        }
    }
}

/// The workspace's owner admits ada as their own device, and what that changed reaches both
/// devices' copies of the workspace, as the workspace's host carries it.
fn the_owner_admitted_ada() -> TwoSessions {
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace");
    let now = tilo_clock::wall();
    // The workspace's owner, whose own device signs for it.
    let a_root = SigningKey::from_bytes(&[51; 32]);
    let a_key = SigningKey::from_bytes(&[52; 32]);
    let a_cert = testing::certificate(&a_root, &a_key, "The owner's laptop");
    let governor = TestGovernor::empty_interactive(a_cert.clone(), a_key.clone(), WORKSPACE)
        .expect("the workspace's owner governs it");
    let a_state = tempfile::TempDir::new().expect("a state directory");
    let a_engine = Engine::open(
        identity(&a_cert, &a_key),
        Some(a_state.path()),
        std::iter::empty(),
        crate::platform::membership_check::of_this_session(),
    )
    .expect("the owner's engine opens");
    let mut governor = testing::enroll_into(governor, a_engine.owner(), &a_key, &a_cert)
        .expect("the workspace is the owner's");
    // The person at this session joins it as their own device, and so does another member.
    let b_root = SigningKey::from_bytes(&[53; 32]);
    let b_key = SigningKey::from_bytes(&[54; 32]);
    let b_cert = testing::certificate(&b_root, &b_key, "This session");
    let b_state = tempfile::TempDir::new().expect("this session's state directory");
    let b_engine = Engine::open(
        identity(&b_cert, &b_key),
        Some(b_state.path()),
        [workspace_id],
        crate::platform::membership_check::of_this_session(),
    )
    .expect("this session's engine opens");
    governor = testing::enroll_into(governor, b_engine.owner(), &b_key, &b_cert)
        .expect("the workspace admits this session");
    let her_root = SigningKey::from_bytes(&[55; 32]);
    let her_key = SigningKey::from_bytes(&[56; 32]);
    let her = testing::certificate(&her_root, &her_key, "ada");
    let hers_at = tempfile::TempDir::new().expect("her state directory");
    let hers = tilo_peer_state::owner::open_file(&hers_at.path().join("owner.json"))
        .expect("her owner opens");
    governor = testing::enroll_into(governor, &hers, &her_key, &her)
        .expect("the workspace admits her device");
    let admitted: Vec<neighbour_wire::NeighbourMsg> = governor
        .outgoing()
        .map(|(bytes, _)| postcard::from_bytes(bytes).expect("a workspace change"))
        .collect();
    for engine in [&a_engine, &b_engine] {
        for message in &admitted {
            engine
                .owner()
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(a_cert.device_public_key),
                    message.clone(),
                    now,
                )
                .expect("a device takes the workspace as its host has it");
        }
    }
    TwoSessions {
        workspace_id,
        owner: a_engine,
        owner_cert: a_cert,
        session: b_engine,
        session_cert: b_cert,
        person: hex::encode(her.user_public_key),
        now,
        _states: (a_state, b_state, hers_at),
    }
}

/// What this session holds of a workspace two devices share after the owner
/// raised ada from their own device: the people the workspace has here,
/// the owner, this session's person, and ada, as the owner's change left her.
fn the_owner_raised_ada() -> (WorkspaceId, WorkspacePolicy, String, String, String) {
    let sessions = the_owner_admitted_ada();
    assert_eq!(
        role_of(&sessions.session, &sessions.person),
        Some(Role::Editor),
        "the workspace has her as an editor"
    );

    // The owner gives her moderation from their own device, and what they wrote reaches this
    // session.
    Membership::of(&sessions.owner, sessions.workspace_id)
        .commit(&Change::SetRole {
            person: sessions.person.clone(),
            role: Role::Moderator,
        })
        .block_on()
        .expect("the workspace takes the change");
    sessions.deliver();
    assert_eq!(
        role_of(&sessions.session, &sessions.person),
        Some(Role::Moderator),
        "this session's workspace has her as a moderator"
    );
    let policy = sessions
        .session
        .policy(WORKSPACE)
        .expect("the workspace's people");
    (
        sessions.workspace_id,
        policy,
        hex::encode(sessions.owner_cert.user_public_key),
        hex::encode(sessions.session_cert.user_public_key),
        sessions.person,
    )
}

/// What the owner lets an app reach, on their own device, is the workspace's: it reaches another
/// member's session, so that member's device starts the app without asking its person again.
#[test]
fn what_one_member_lets_an_app_reach_reaches_every_members_session() {
    use collab_types::caps::{Access, CapOverride, Scope};
    let sessions = the_owner_admitted_ada();
    let owner = hex::encode(sessions.owner_cert.user_public_key);
    let mut workspace = Workspace::new(sessions.workspace_id, "Test");
    workspace.set_owner(owner.clone());
    let network = CapOverride::grant(
        Grantee::person(owner.clone()),
        Scope::NetHost("*".into()),
        Access::Read,
    )
    .through("so.tilo.mail");
    let edit = workspace
        .grant_sandbox_as_at(&owner, network, tilo_clock::unix_now())
        .expect("the owner may let an app reach the network");
    // A member who may not grant in the workspace lets no app reach anything for everyone: the
    // workspace refuses it, here and on every device.
    let member = hex::encode(sessions.session_cert.user_public_key);
    let theirs = CapOverride::grant(
        Grantee::person(member.clone()),
        Scope::NetHost("*".into()),
        Access::Read,
    )
    .through("so.tilo.mail")
    .granted_by(tilo_action_policy::MemberId::person(member));
    assert!(
        Membership::of(&sessions.session, sessions.workspace_id)
            .write(&[WorkspaceMembershipCommand::Settings(
                SettingsChange::GrantSandbox { capability: theirs },
            )])
            .block_on()
            .is_err(),
        "the workspace refuses a member's network grant for its apps"
    );
    let membership = Membership::of(&sessions.owner, sessions.workspace_id);
    membership
        .write(&membership.changes(&workspace, &edit))
        .block_on()
        .expect("the workspace takes it");
    sessions.deliver();

    // This member's shell takes it from the workspace: a capability that lasts only while this
    // session does stays, one the workspace took back goes.
    let governed = sessions
        .session
        .owner()
        .with_current(sessions.workspace_id, |view| {
            view.peer().governance().governed_sandbox()
        })
        .expect("this session holds the workspace");
    let mut shown = Workspace::new(sessions.workspace_id, "Test");
    let session_only = CapOverride::grant(
        Grantee::person(owner.clone()),
        Scope::NetHost("example.test".into()),
        Access::Read,
    )
    .through("so.tilo.notes")
    .lasting(tilo_action_policy::Lifetime::Session);
    let taken_back = CapOverride::grant(
        Grantee::person(owner),
        Scope::NetHost("old.example.test".into()),
        Access::Read,
    )
    .through("so.tilo.notes");
    shown.sandbox = vec![session_only.clone(), taken_back];
    assert!(super::project_sandbox(&mut shown, governed));
    assert!(
        shown
            .sandbox
            .iter()
            .any(|cap| cap.names_app("so.tilo.mail") && matches!(cap.scope, Scope::NetHost(_))),
        "this member's shell lets Mail reach the network"
    );
    assert!(shown.sandbox.contains(&session_only));
    assert_eq!(
        shown.sandbox.len(),
        2,
        "what the workspace took back is gone"
    );
}

/// A role one member's device sets reaches another member's session: what this session has of the
/// workspace's people — the roster the People tab shows — has her with it, though the device
/// that set it is not here.
#[test]
fn a_role_another_member_sets_reaches_this_shells_people() {
    let (workspace_id, policy, owner, session, person) = the_owner_raised_ada();
    // What this session had of the workspace's people before the change is the owner's and this
    // person's; after it, what the workspace has.
    let mut workspace = Workspace::new(workspace_id, "Test");
    workspace.set_owner(owner.clone());
    workspace
        .policy
        .members
        .insert(session.clone(), Role::Editor);
    assert!(
        project(&mut workspace, &policy),
        "the people the workspace has are projected on to this session"
    );
    assert_eq!(
        workspace.role_of(&person),
        Some(Role::Moderator),
        "the People tab shows her with the role the owner set"
    );
    assert_eq!(
        workspace.role_of(&session),
        Some(Role::Editor),
        "and this person as the workspace has her"
    );
    assert!(
        !workspace.policy.members.contains_key(&owner),
        "the owner is the workspace's owner, not one more of its members"
    );
    assert!(
        !project(&mut workspace, &policy),
        "a session already showing them is not written again"
    );
}

/// A person named in the workspace's settings is given another role,
/// removed, and named again: each is a change the workspace's own
/// admission makes, so it reaches every member, not only this session.
#[test]
fn the_workspace_takes_the_people_its_settings_change() {
    let (engine, mut workspace, person, _state) = workspace_here("ada");
    let membership = Membership::of(&engine, workspace.id());
    assert_eq!(
        role_of(&engine, &person),
        Some(Role::Editor),
        "the workspace has her as an editor"
    );

    // The person gives her moderation.
    workspace
        .policy
        .members
        .insert(person.clone(), Role::Moderator);
    let set = membership.changes(
        &workspace,
        &PolicyEdit {
            members: vec![person.clone()],
            ..PolicyEdit::default()
        },
    );
    assert_eq!(
        set,
        vec![WorkspaceMembershipCommand::Member {
            change: Change::SetRole {
                person: person.clone(),
                role: Role::Moderator
            }
        }],
        "a person the workspace has has their role set"
    );
    membership
        .write(&set)
        .block_on()
        .expect("the workspace takes the change");
    assert_eq!(
        role_of(&engine, &person),
        Some(Role::Moderator),
        "the workspace has made her a moderator"
    );

    workspace.policy.members.remove(&person);
    let removed = membership.changes(
        &workspace,
        &PolicyEdit {
            members: vec![person.clone()],
            ..PolicyEdit::default()
        },
    );
    assert_eq!(
        removed,
        vec![WorkspaceMembershipCommand::Member {
            change: Change::RemoveMember {
                person: person.clone()
            }
        }],
        "a person the settings drop is removed"
    );
    membership
        .write(&removed)
        .block_on()
        .expect("the workspace takes the removal");
    assert_eq!(
        role_of(&engine, &person),
        None,
        "the workspace has her no more"
    );

    // And the settings name her again, as a person the workspace does not have: that takes her
    // back in.
    workspace
        .policy
        .members
        .insert(person.clone(), Role::Editor);
    let added = membership.changes(
        &workspace,
        &PolicyEdit {
            members: vec![person.clone()],
            ..PolicyEdit::default()
        },
    );
    assert_eq!(
        added,
        vec![WorkspaceMembershipCommand::Member {
            change: Change::AddMember {
                person: person.clone(),
                role: Role::Editor
            }
        }],
        "a person the workspace does not have is added"
    );
    membership
        .write(&added)
        .block_on()
        .expect("the workspace takes her back");
    assert_eq!(
        role_of(&engine, &person),
        Some(Role::Editor),
        "the workspace has her again"
    );
}

/// A change the workspace's admission refuses leaves its people as they were, whatever this
/// session's copy of them says.
#[test]
fn a_change_the_workspace_refuses_leaves_its_people_as_they_were() {
    let (engine, workspace, _, _state) = workspace_here("ada");
    let membership = Membership::of(&engine, workspace.id());
    // A person this session's copy names and the workspace never took in: a role for her is
    // refused, because there is no membership of hers to give a role.
    let stranger = hex::encode(SigningKey::from_bytes(&[31; 32]).verifying_key().to_bytes());
    assert!(
        membership
            .commit(&Change::SetRole {
                person: stranger.clone(),
                role: Role::Editor,
            })
            .block_on()
            .is_err(),
        "the workspace refuses a role for someone it does not have"
    );
    assert_eq!(
        role_of(&engine, &stranger),
        None,
        "the workspace never had her"
    );
    let owner = workspace
        .policy
        .members
        .iter()
        .find(|(_, role)| **role == Role::Owner)
        .map(|(person, _)| person.clone())
        .expect("the workspace's owner");
    let before = role_of(&engine, &owner);
    assert!(
        membership
            .commit(&Change::RemoveMember {
                person: owner.clone(),
            })
            .block_on()
            .is_err(),
        "the workspace refuses to remove its owner"
    );
    assert_eq!(
        role_of(&engine, &owner),
        before,
        "the workspace's owner stays its owner"
    );
    assert!(
        before.is_some(),
        "the workspace had its owner to begin with"
    );
}

/// The session this computer keeps under `state` , acting as `person` , with its own workspace
/// governed by this computer's device — the workspace's owner — and ada admitted by a device of
/// her own.
struct GovernedSession {
    session: Option<crate::Session>,
    state: tempfile::TempDir,
    _served: Option<tempfile::TempDir>,
    _hers: tempfile::TempDir,
    _identity: tempfile::TempDir,
    _home: tilo_test_env::TestHome,
    workspace_id: WorkspaceId,
    person: String,
    ada: String,
    her: Engine,
    cert: tilo_user_identity::DeviceCert,
    key: SigningKey,
    now: tilo_clock::time::SystemTime,
}

/// [`session_keeping`], acting as `person` instead of the default one, and serving its apps'
/// broker on `socket` when the test gives it one.
fn session_as(
    state: &std::path::Path,
    person: &str,
    socket: Option<&std::path::Path>,
) -> crate::Session {
    crate::Session::headless(
        crate::Options {
            person: person.to_owned(),
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            app_socket: socket.map_or(crate::AppSocket::Off, |socket| {
                crate::AppSocket::At(socket.to_path_buf())
            }),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session")
}

fn a_governed_session() -> GovernedSession {
    governed_session(None)
}

/// [`a_governed_session`] whose session serves its apps' broker, so this computer's own device
/// changes the workspace through the session it runs.
fn governed_session(served: Option<tempfile::TempDir>) -> GovernedSession {
    governed_session_seeded(served, |_, _, _| ())
}

/// `governed_session`, with what `seed` gives the governor to publish once
/// the workspace has both devices in it.
fn governed_session_seeded(
    served: Option<tempfile::TempDir>,
    seed: impl FnOnce(&mut TestGovernor, &tilo_user_identity::DeviceCert, &SigningKey),
) -> GovernedSession {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let (identity, cert, key) = provisioned_identity(&mut home);
    let person = hex::encode(cert.user_public_key);
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = a_workspace_on_the_shell(state.path());
    let now = tilo_clock::wall();
    let hers = tempfile::tempdir().expect("her state directory");
    let (her, _her_cert, _her_key, ada) = a_second_device(
        state.path(),
        hers.path(),
        workspace_id,
        &cert,
        &key,
        now,
        seed,
    );
    let socket = served
        .as_ref()
        .map(|served| served.path().join("collab.sock"));
    let mut session = session_as(state.path(), &person, socket.as_deref());
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    GovernedSession {
        session: Some(session),
        state,
        _served: served,
        _hers: hers,
        _identity: identity,
        _home: home,
        workspace_id,
        person,
        ada,
        her,
        cert,
        key,
        now,
    }
}

impl GovernedSession {
    /// The session this session runs.
    fn session(&mut self) -> &mut crate::Session {
        self.session.as_mut().expect("the session runs")
    }

    /// The session's sheet as the session holds it.
    fn settings(&mut self) -> tilo_sheets::workspace_settings::Settings {
        self.session()
            .workspace_settings()
            .expect("the session hosts workspace settings")
            .settings()
    }

    /// Run one frame of the session's session.
    fn frames(&mut self) {
        self.session()
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
    }

    /// Step the session until `done` says so, or the time a test may hang ran out: what the
    /// workspace answers a change lands on a pass after the one that asked.
    fn frames_until(&mut self, mut done: impl FnMut(&mut Self) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(self) && std::time::Instant::now() < deadline {
            self.frames();
        }
    }

    /// Close the session, as this computer does when its session is not up.
    fn close(&mut self) {
        self.session = None;
    }
}

/// The sheet the session hosts, as the person at this session presses it.
fn settings_sheet(
    session: &mut GovernedSession,
) -> tilo_app::framework::testing::HeadlessApp<tilo_sheets::workspace_settings::WorkspaceSettingsApp>
{
    session
        .session()
        .invoke(
            tilo_desktop::verbs::CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("settings opens on the workspace in front");
    session.frames();
    let state = session
        .session()
        .workspace_settings()
        .expect("the session hosts workspace settings")
        .clone();
    let mut app = tilo_app::framework::testing::HeadlessApp::<
        tilo_sheets::workspace_settings::WorkspaceSettingsApp,
    >::for_person_with_lent(&session.person, state)
    .expect("the sheet");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned(),
        },
    )
    .expect("show the people");
    session.frames();
    app.settle();
    app
}

/// The role the sheet shows for `who` .
fn shown_role(session: &mut GovernedSession, who: &str) -> Option<String> {
    let who = who.to_owned();
    session
        .settings()
        .people
        .people
        .into_iter()
        .find(|listed| listed.key == who)
        .and_then(|listed| listed.role_key)
}

/// Press the control `id` of `app` , as a person's own gesture.
fn press(
    app: &mut tilo_app::framework::testing::HeadlessApp<
        tilo_sheets::workspace_settings::WorkspaceSettingsApp,
    >,
    id: &str,
) {
    app.harness()
        .perform_named(id, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("press {id}: {error}"));
    app.settle();
}

/// Activate the element `name` says, as a screen reader does.
fn activate(
    app: &mut tilo_app::framework::testing::HeadlessApp<
        tilo_sheets::workspace_settings::WorkspaceSettingsApp,
    >,
    name: &str,
) {
    let node = app
        .harness()
        .a11y_node_named(name)
        .unwrap_or_else(|| panic!("no element says {name:?}"));
    app.harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|error| panic!("activate {name}: {error}"));
    app.settle();
}

/// The People tab shows the workspace's people as its governance has them,
/// and the role the person gives there is the workspace's own change: her
/// device takes it as it takes every change of the workspace.
#[test]
fn a_role_the_people_tab_sets_reaches_the_other_members_device() {
    let _settled = ui::motion::testing::settled();
    let mut session = a_governed_session();
    let mut app = settings_sheet(&mut session);
    let ada = session.ada.clone();
    assert_eq!(
        shown_role(&mut session, &ada).as_deref(),
        Some("editor"),
        "the People tab shows her as the workspace has her"
    );
    press(&mut app, &format!("ws-people-role-{ada}"));
    activate(&mut app, "Commenter");
    session.frames_until(|session| shown_role(session, &ada).as_deref() == Some("commenter"));
    app.settle();
    assert_eq!(
        shown_role(&mut session, &ada).as_deref(),
        Some("commenter"),
        "the sheet's row says what the workspace now has her as"
    );
    drop(app);
    session.close();
    assert_eq!(
        her_device_takes(
            session.state.path(),
            session.workspace_id,
            &session.cert,
            &session.key,
            &session.her,
            &session.ada,
            session.now
        ),
        Some(Role::Commenter),
        "her device takes the role the person set here"
    );
}

/// A role change the workspace refuses says why in the sheet, and leaves the person with the
/// role they had.
#[test]
fn a_refused_role_change_says_why_and_keeps_the_role() {
    let _settled = ui::motion::testing::settled();
    let mut session = a_governed_session();
    let mut app = settings_sheet(&mut session);
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::SetRole {
            member: session.ada.clone(),
            role: "owner".to_owned(),
        },
    )
    .expect("the sheet asks for the role");
    session.frames();
    app.settle();
    let notice = session
        .settings()
        .notice
        .expect("the sheet says the workspace refused it");
    assert_eq!(notice.title, "The workspace refused that change");
    assert!(
        notice.detail.as_deref().is_some_and(|detail| {
            detail.contains("ownership is handed over by the workspace's owner")
        }),
        "the sheet says why: {:?}",
        notice.detail
    );
    let ada = session.ada.clone();
    assert_eq!(
        shown_role(&mut session, &ada).as_deref(),
        Some("editor"),
        "she keeps the role she had"
    );
    let governed = session
        .session()
        .handle()
        .read(|tree| tree.workspaces.first().and_then(|ws| ws.role_of(&ada)));
    assert_eq!(
        governed,
        Some(Role::Editor),
        "and the workspace never had her as anything else"
    );
}

/// A grant change the workspace refuses says why in the sheet, and the workspace keeps what the
/// person asked to change.
#[test]
fn a_refused_grant_change_says_why_and_keeps_the_grant() {
    let _settled = ui::motion::testing::settled();
    let mut session = a_governed_session();
    // The workspace grants an app the camera, as one of its devices wrote.
    let mut tree = session.session().handle().read(Clone::clone);
    tree.active_workspace_mut()
        .expect("a workspace in front")
        .grant_sandbox(
            collab_types::caps::CapOverride::grant(
                tilo_action_policy::Grantee::person("ada"),
                collab_types::caps::Scope::Device(collab_types::caps::DeviceCap::Camera),
                collab_types::caps::Access::Read,
            )
            .through("so.tilo.notes")
            .lasting(tilo_action_policy::Lifetime::Session),
        );
    session.session().handle().replace_tree(tree);
    let mut app = settings_sheet(&mut session);
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::ShowTab {
            tab: "permissions".to_owned(),
        },
    )
    .expect("show the permissions");
    session.frames();
    app.settle();
    let granted = session
        .settings()
        .permissions
        .sections
        .into_iter()
        .flat_map(|section| section.permissions)
        .next()
        .expect("the sheet lists the permission the workspace gives");
    // The confirmation is open when another of the workspace's devices takes the grant back, so
    // what this sheet has is out of date.
    press(&mut app, &format!("ws-permission-revoke-{}", granted.key));
    let mut tree = session.session().handle().read(Clone::clone);
    tilo_desktop::workspace_settings::apply_revoke(
        tree.active_workspace_mut().expect("a workspace in front"),
        &session.person.clone(),
        &granted.key,
    )
    .expect("another device takes the grant back");
    session.session().handle().replace_tree(tree);
    press(&mut app, "ws-settings-decision.primary");
    session.frames();
    app.settle();
    let notice = session
        .settings()
        .notice
        .expect("the sheet says the workspace refused it");
    assert_eq!(notice.title, "The workspace refused that change");
    assert!(
        notice
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("no grant")),
        "the sheet says why: {:?}",
        notice.detail
    );
    assert!(
        session.session().handle().read(|tree| {
            tree.workspaces
                .first()
                .is_some_and(|workspace| workspace.sandbox.iter().all(|cap| cap.id != granted.key))
        }),
        "the workspace keeps the grant as the other device left it"
    );
}

/// This computer's identity, kept under a root of the test's own, and the device certificate a
/// session started here signs with.
fn this_computer(
    env: &mut tilo_test_env::ScopedEnv,
) -> (
    tempfile::TempDir,
    SigningKey,
    tilo_user_identity::DeviceCert,
) {
    let root = tempfile::tempdir().expect("an identity root");
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let device = SigningKey::from_bytes(&[0x62; 32]);
    tilo_device_identity::persist::write_device_key(&paths.device_privkey, &device)
        .expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_device_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    (root, device, cert)
}

/// The session this computer keeps under `state` .
fn session_keeping(state: &std::path::Path) -> crate::Session {
    let session = crate::Session::headless(
        crate::Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session");
    if let Some(engine) = &session.shared.engine {
        crate::founding_fixture::serve(engine);
    }
    session
}

/// A person another device of the workspace took in is on this session's copy of it, in the part
/// the workspace gives them, when the session comes back: the People tab shows the workspace's
/// people, not only those this session put there.
#[test]
fn the_people_another_device_admits_are_on_this_shells_people() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, key, cert) = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    // A frame brings the session and its own workspace up.
    let workspace_id = {
        let mut session = session_keeping(state.path());
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        session
            .handle()
            .read(|tree| tree.workspaces.first().map(Workspace::id))
            .expect("a workspace on the session")
    };
    // The workspace, as this computer's device governs it, takes ada in: a change another
    // member's device makes while this session is away.
    let now = tilo_clock::wall();
    let her_root = SigningKey::from_bytes(&[0x63; 32]);
    let her_key = SigningKey::from_bytes(&[0x64; 32]);
    let her = testing::certificate(&her_root, &her_key, "ada");
    {
        let engine = Engine::open(
            identity(&cert, &key),
            Some(state.path()),
            [workspace_id],
            crate::platform::membership_check::of_this_session(),
        )
        .expect("this computer's engine opens");
        the_fixture_governs(&engine, workspace_id);
        let governor =
            TestGovernor::empty_interactive(cert.clone(), key.clone(), &workspace_id.to_string())
                .expect("this computer's device governs the workspace");
        let mut governor = testing::enroll_into(governor, engine.owner(), &key, &cert)
            .expect("the workspace is this computer's");
        let hers_at = tempfile::TempDir::new().expect("her state directory");
        let hers = tilo_peer_state::owner::open_file(&hers_at.path().join("owner.json"))
            .expect("her owner opens");
        governor = testing::enroll_into(governor, &hers, &her_key, &her)
            .expect("the workspace admits her device");
        for (bytes, _) in governor.outgoing() {
            engine
                .owner()
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    now,
                )
                .expect("this device takes the workspace's change");
        }
    }
    let person = hex::encode(her.user_public_key);
    // A new session of the same session shows the workspace's people.
    let mut session = session_keeping(state.path());
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    let role = session
        .handle()
        .read(|tree| tree.workspaces.first().and_then(|ws| ws.role_of(&person)));
    assert_eq!(
        role,
        Some(Role::Editor),
        "the session's workspace has the person the workspace took in"
    );
    // The sheet shows her in the People tab.
    session
        .invoke(
            tilo_desktop::verbs::CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("settings opens on the workspace in front");
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    let row = session
        .workspace_settings()
        .expect("the session hosts workspace settings")
        .settings()
        .people
        .people
        .into_iter()
        .find(|listed| listed.key == person)
        .expect("the People tab lists her");
    assert_eq!(
        row.role_key.as_deref(),
        Some("editor"),
        "the People tab says which part the workspace gives her"
    );
}

/// This computer provisioned as one of its person's devices: where the session reads the device
/// it signs with, and the fleet that admits it.
fn provisioned_identity(
    home: &mut tilo_test_env::TestHome,
) -> (
    tempfile::TempDir,
    tilo_user_identity::DeviceCert,
    SigningKey,
) {
    let root = tempfile::tempdir().expect("a root for this computer's device");
    let paths = tilo_device_identity::IdentityPaths::under(root.path());
    home.env()
        .set("TILO_IDENTITY_ROOT", root.path())
        .set(tilo_identity::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity::provisioning::ensure_collaboration_identity(
        &home.config().join("tilo").join("identity"),
        &paths,
        tilo_identity::provisioning::IdentityStorage::Files,
    )
    .expect("this computer is provisioned as a device");
    let key = tilo_device_identity::persist::load_device_key(&paths).expect("this computer's key");
    let cert = tilo_device_identity::persist::read_device_cert(&paths.device_cert)
        .expect("this computer's certificate");
    (root, cert, key)
}

/// A directory for the session's socket that only this user reaches: the broker refuses to
/// serve one anybody else could open.
fn private_directory() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the broker in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the broker's directory is this user's");
    root
}

/// The session this computer runs, keeping `state` and serving its broker on `socket` : where
/// its person changes a workspace's people.
fn session_serving(state: &std::path::Path, socket: &std::path::Path) -> crate::Session {
    crate::Session::headless(
        crate::Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            app_socket: crate::AppSocket::At(socket.to_path_buf()),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session")
}

/// Wait for `broker` to publish the workspace's people actions, on the clock it re-reads what
/// this device may change.
#[track_caller]
fn wait_for_people(
    broker: &tilo_workspace_service::apps::collab_broker::AppActionBroker,
    workspace_id: WorkspaceId,
) {
    let resource = tilo_actions::remote::workspace_governance_resource(&workspace_id.to_string());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let published = broker.surfaces().into_iter().any(|surface| {
            surface.route.cluster_id == workspace_id.to_string()
                && surface
                    .actions
                    .iter()
                    .any(|action| action.resource == resource && action.action == "set-role")
        });
        if published {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the workspace's people actions are not published"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// The workspace this computer's session opens when it has none of its own: the id the session's
/// engine and the test's other devices act on.
fn a_workspace_on_the_shell(state: &std::path::Path) -> WorkspaceId {
    let mut session = session_keeping(state);
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    session
        .handle()
        .read(|tree| tree.workspaces.first().map(Workspace::id))
        .expect("a workspace on the session")
}

/// Let go of the workspace the session founded here, so a fixture host can be the one that governs
/// it: the fixture's own genesis is the workspace's then, and the devices it admits are the
/// workspace's.
fn the_fixture_governs(mine: &Engine, workspace_id: WorkspaceId) {
    mine.delete(workspace_id)
        .expect("this device lets go of the workspace it founded");
}

/// Another person the workspace admits, on a device of her own: her engine on `her_state` , her
/// certificate and key, and the id the workspace knows her by. `seed` makes what the governor
/// publishes to the devices once the workspace has both of them.
fn a_second_device(
    state: &std::path::Path,
    her_state: &std::path::Path,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    now: tilo_clock::time::SystemTime,
    seed: impl FnOnce(&mut TestGovernor, &tilo_user_identity::DeviceCert, &SigningKey),
) -> (Engine, tilo_user_identity::DeviceCert, SigningKey, String) {
    let her_key = SigningKey::from_bytes(&[0x72; 32]);
    let her = testing::certificate(&SigningKey::from_bytes(&[0x71; 32]), &her_key, "ada");
    let hers = Engine::open(
        identity(&her, &her_key),
        Some(her_state),
        [workspace_id],
        crate::platform::membership_check::of_this_session(),
    )
    .expect("her engine opens");
    // This computer's device governs the workspace, and admits her.
    let mine = Engine::open(
        identity(cert, key),
        Some(state),
        [workspace_id],
        crate::platform::membership_check::of_this_session(),
    )
    .expect("this computer's engine opens");
    // The workspace the session made is this computer's own already: a fixture host governs it
    // from here, and admits her device into it.
    the_fixture_governs(&mine, workspace_id);
    let governor =
        TestGovernor::empty_interactive(cert.clone(), key.clone(), &workspace_id.to_string())
            .expect("this computer's device governs the workspace");
    let mut governor = testing::enroll_into(governor, mine.owner(), key, cert)
        .expect("the workspace is this computer's");
    governor = testing::enroll_into(governor, hers.owner(), &her_key, &her)
        .expect("the workspace admits her device");
    let person = hex::encode(her.user_public_key);
    seed(&mut governor, cert, key);
    for (bytes, _) in governor.outgoing() {
        for engine in [&mine, &hers] {
            engine
                .owner()
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedServerDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    now,
                )
                .expect("the workspace's change is taken");
        }
    }
    (hers, her, her_key, person)
}

/// This session's broker writes `command` into the workspace's people as this computer's own
/// device asks it, and the workspace answers as taken.
#[track_caller]
fn change_governance(
    session: &crate::Session,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    command: &WorkspaceMembershipCommand,
) {
    let broker = session
        .app_wire
        .as_ref()
        .and_then(crate::app_wire::AppWire::action_broker)
        .expect("the session serves its broker");
    wait_for_people(&broker, workspace_id);
    let job = broker
        .change_workspace_membership(&workspace_id.to_string(), command, cert)
        .expect("this device's person may change the workspace's people");
    let completed = job
        .completion
        .recv_timeout(tilo_test_env::hang_bound())
        .expect("the invocation answers");
    assert!(completed.code.is_empty(), "{}", completed.message);
}

/// [`change_governance`] one of the people changes the settings sheet makes.
#[track_caller]
fn change_people(
    session: &crate::Session,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    change: &Change,
) {
    change_governance(
        session,
        workspace_id,
        cert,
        &WorkspaceMembershipCommand::Member {
            change: change.clone(),
        },
    );
}

/// Run frames until the session shows `person` in `expected` 's part of the workspace.
#[track_caller]
fn the_shell_shows(session: &mut crate::Session, person: &str, expected: Option<Role>) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let shown = session
            .handle()
            .read(|tree| tree.workspaces.first().and_then(|ws| ws.role_of(person)));
        if shown == expected {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the session never showed the change: {shown:?}"
        );
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
    }
}

/// What her device holds of `person` once it took the changes this computer's engine wrote.
fn her_device_takes(
    state: &std::path::Path,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    hers: &Engine,
    person: &str,
    now: tilo_clock::time::SystemTime,
) -> Option<Role> {
    let mine = Engine::open(
        identity(cert, key),
        Some(state),
        [workspace_id],
        crate::platform::membership_check::of_this_session(),
    )
    .expect("this computer's engine opens");
    let written = mine
        .owner()
        .with_outgoing(workspace_id, now, |_, outgoing| {
            outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, String>>()
        })
        .expect("this computer's device published the change");
    assert!(!written.is_empty(), "the change is the workspace's to send");
    for message in written {
        hers.owner()
            .receive(
                workspace_id,
                tilo_peer_state::TransportOrigin::RetainedServerDelta(cert.device_public_key),
                message,
                now,
            )
            .expect("her device takes the workspace's change");
    }
    hers.people(&workspace_id.to_string())
        .expect("the workspace's people")
        .into_iter()
        .find(|held| held.id.person_id() == Some(person) && !held.removed)
        .and_then(|held| held.role)
}

/// A person's own device changes a workspace's people through the session this computer runs:
/// the role they give and the member they remove are the workspace's own change, signed with
/// this device's key, so the workspace the session's engine holds has them, and the person's
/// other device takes them as it takes any change of the workspace.
#[test]
fn a_person_changes_a_workspaces_people_through_the_session_this_computer_runs() {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let (_identity, cert, key) = provisioned_identity(&mut home);
    let me = crate::sync::device_identity().expect("this computer is a device");
    assert_eq!(
        me.device_cert, cert,
        "the session reads this computer's own device"
    );
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = a_workspace_on_the_shell(state.path());
    let now = tilo_clock::wall();
    let her_state = tempfile::tempdir().expect("her state directory");
    let (hers, _her, _her_key, her_person) = a_second_device(
        state.path(),
        her_state.path(),
        workspace_id,
        &cert,
        &key,
        now,
        |_, _, _| (),
    );
    let socket_dir = private_directory();
    let socket = socket_dir.path().join("collab.sock");
    // The role the person gives her, and then the member they remove: each through this
    // session's broker, as the person's own device asks it.
    for change in [
        Change::SetRole {
            person: her_person.clone(),
            role: Role::Moderator,
        },
        Change::RemoveMember {
            person: her_person.clone(),
        },
    ] {
        let expected = match &change {
            Change::SetRole { .. } => Some(Role::Moderator),
            _ => None,
        };
        let mut session = session_serving(state.path(), &socket);
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        change_people(&session, workspace_id, &cert, &change);
        // What the session shows is the workspace the session's engine holds: the role is hers, or
        // she is gone from it.
        the_shell_shows(&mut session, &her_person, expected);
        drop(session);
        // The change this computer's device wrote, carried to her device as the workspace's
        // host carries every change.
        assert_eq!(
            her_device_takes(
                state.path(),
                workspace_id,
                &cert,
                &key,
                &hers,
                &her_person,
                now
            ),
            expected,
            "her device has her as the workspace does"
        );
    }
}

/// Whether asking this session's broker to re-role someone as `scribe` is refused: the person's
/// device handed the session to an agent, and a workspace's people change at its person's
/// device only.
#[track_caller]
fn an_agent_is_refused_a_role(
    broker: &tilo_workspace_service::apps::collab_broker::AppActionBroker,
    cert: &tilo_user_identity::DeviceCert,
    workspace_id: WorkspaceId,
    person: &str,
) -> bool {
    let agent =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace_id.to_string())
            .in_new_execution()
            .delegated_to("scribe");
    let answer = broker
        .device_host()
        .invoke(
            &agent,
            tilo_actions::remote::InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                on_behalf_of: None,
                target: tilo_actions::remote::RemoteActionTarget {
                    provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.into(),
                    address: tilo_actions::ActionAddress::new(
                        tilo_actions::remote::workspace_governance_resource(
                            &workspace_id.to_string(),
                        ),
                        "set-role",
                    ),
                },
                argument: tilo_actions::remote::InvokeArgument::Json(
                    serde_json::json!({
                        "person": person,
                        "role": "moderator",
                    })
                    .as_object()
                    .expect("an object")
                    .clone(),
                ),
                context: None,
                confirmation: None,
                revision: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the host answers");
    matches!(
        answer,
        Err(tilo_actions::remote::ActionCallError::NotPermitted)
    )
}

/// An agent the person's device handed its session to cannot change a workspace's people: that
/// is the person's own act at their device.
#[test]
fn an_agent_cannot_change_a_workspaces_people() {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let (_identity, cert, _key) = provisioned_identity(&mut home);
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = a_workspace_on_the_shell(state.path());
    let stranger = SigningKey::from_bytes(&[0x73; 32])
        .verifying_key()
        .to_bytes();
    let socket_dir = private_directory();
    let socket = socket_dir.path().join("collab.sock");
    let mut session = session_serving(state.path(), &socket);
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    let broker = session
        .app_wire
        .as_ref()
        .and_then(crate::app_wire::AppWire::action_broker)
        .expect("the session serves its broker");
    wait_for_people(&broker, workspace_id);
    assert!(
        an_agent_is_refused_a_role(&broker, &cert, workspace_id, &hex::encode(stranger)),
        "an agent is refused the workspace's people"
    );
    // And nobody was added to the workspace it asked for.
    drop((session, broker));
    let mine = Engine::open(
        crate::sync::device_identity().expect("this computer is a device"),
        Some(state.path()),
        [workspace_id],
        crate::platform::membership_check::of_this_session(),
    )
    .expect("this computer's engine opens");
    assert!(
        mine.people(&workspace_id.to_string())
            .expect("the workspace's people")
            .iter()
            .all(|held| held.id.person_id() != Some(hex::encode(stranger).as_str())),
        "the agent's change is not the workspace's"
    );
}

/// An app's ask about one node of the workspace — Chat sharing what a window shows with the
/// members, or asking what they may do at one — is the workspace's own governance: the share's
/// grants are written there, its end takes them back, and what the asker may do is read from
/// the same policy every device of the workspace admits.
#[test]
fn an_apps_share_of_a_node_is_the_workspaces_own_grants() {
    use crate::workspace_nodes::WorkspaceNodes;
    use std::sync::Arc;
    use tilo_action_policy::share::is_of_share;
    use tilo_action_policy::{ActionKind, Answer, Authority, Context, Grantee, Role, TreePath};
    use tilo_actions::remote::Pending;
    use tilo_broker_core::services::WorkspaceNodeAsk;

    fn answered<T>(pending: Pending<T>) -> T {
        landed(pending)
    }

    let (engine, _, joiner, _state) = workspace_here("ada");
    let engine = Arc::new(engine);
    let asks = WorkspaceNodes::service(Some(&engine)).expect("this session holds a workspace");
    let node = TreePath::workspace(WORKSPACE)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .resource("plan");
    let text = node.to_string();
    let ask = |ask: WorkspaceNodeAsk| {
        asks.ask(WORKSPACE, &text, ask)
            .expect("a node of this workspace")
    };
    let share = answered(ask(WorkspaceNodeAsk::Share {
        share: "m1".into(),
        access: "view".into(),
    }))
    .expect("the workspace's owner shares what they hold");
    assert_eq!(
        share["grants"], 1,
        "a share is one grant, to the role every other role reaches"
    );
    let grants = engine
        .policy(WORKSPACE)
        .expect("the workspace's policy")
        .grants;
    let of_share: Vec<_> = grants
        .iter()
        .filter(|grant| is_of_share(grant, "m1"))
        .collect();
    assert_eq!(of_share.len(), 1, "the share is the workspace's own grants");
    assert_eq!(
        of_share[0].grantee,
        Grantee::role(Role::Viewer),
        "the grant is the viewer role's, and every role above it reads through it"
    );
    let permits = answered(ask(WorkspaceNodeAsk::Permits)).expect("what they may do there");
    assert_eq!(permits["read"], true);
    assert_eq!(permits["undoable"], true, "the owner holds the workspace");
    let member = Authority::person(
        engine
            .policy(WORKSPACE)
            .expect("the workspace's policy")
            .party(&joiner),
    )
    .decide(&node, ActionKind::Read, &Context::at_secs(0))
    .answer;
    assert_eq!(member, Answer::Allow, "a member reads what is shared");
    let unshared =
        answered(ask(WorkspaceNodeAsk::Unshare { share: "m1".into() })).expect("the share ends");
    assert_eq!(
        unshared["revoked"], 1,
        "the share's one grant is taken back"
    );
    assert!(
        engine
            .policy(WORKSPACE)
            .expect("the workspace's policy")
            .grants
            .iter()
            .all(|grant| !is_of_share(grant, "m1")),
        "nothing of the share is left in the workspace"
    );
}

/// What `pending` answers once it landed, within the time a test may hang.
fn landed<T>(pending: tilo_actions::remote::Pending<T>) -> T {
    let within =
        tilo_test_env::hang_deadline().saturating_duration_since(std::time::Instant::now());
    pending
        .wait_timeout(within)
        .unwrap_or_else(|_| panic!("the work did not answer"))
        .expect("the work was given up before it answered")
}

/// A share written through a signer that answers only once the loop that asked turned (a page's
/// key, or a signer answering through the session's own loop): the ask hands back its answer at
/// once and holds nothing, and the share lands once the signer answered. A loop that waited for
/// the signature here would never turn, and the signer would never answer.
#[test]
fn a_share_signed_late_never_holds_the_loop_that_asked() {
    use crate::workspace_nodes::WorkspaceNodes;
    use std::sync::Arc;
    use tilo_action_policy::TreePath;
    use tilo_actions::remote::PendingPoll;
    use tilo_broker_core::services::WorkspaceNodeAsk;

    let hold = Arc::new(SignHold::default());
    let (engine, _, _, _state) = workspace_signed_through("ada", Some(Arc::clone(&hold)));
    let engine = Arc::new(engine);
    let asks = WorkspaceNodes::service(Some(&engine)).expect("this session holds a workspace");
    let node = TreePath::workspace(WORKSPACE)
        .app("so.tilo.notes")
        .kind("notes.notebook")
        .resource("plan")
        .to_string();
    hold.hold();
    let mut share = asks
        .ask(
            WORKSPACE,
            &node,
            WorkspaceNodeAsk::Share {
                share: "m1".into(),
                access: "view".into(),
            },
        )
        .expect("a node of this workspace");
    assert!(
        matches!(share.poll_ready(), PendingPoll::Waiting),
        "nothing is shared before the signer answered"
    );
    // The loop that asked goes on, and its turn is what lets the signer answer.
    hold.open();
    let shared = landed(share).expect("the workspace's owner shares what they hold");
    assert_eq!(
        shared["grants"], 1,
        "a share is one grant, to the role every other role reaches"
    );
}

/// "Allow for this issue" on the sheet a run's question asked is a grant in the workspace's
/// governance, given by the person at this device, and in force for the broker once the session the
/// loop publishes carries it: the call it was given for waits for exactly that. A grant given
/// once is taken back the same way once spent.
#[test]
fn a_grant_the_person_gives_is_in_force_once_the_shell_carries_it() {
    use tilo_action_policy::agent_answers::{AnswerScope, answer_grant};
    use tilo_action_policy::{ActionKind, Answer};
    use tilo_workspace_service::apps::services::WorkspaceGrants as _;
    let (engine, mut workspace, _, _state) = workspace_here("ada");
    let engine = std::sync::Arc::new(engine);
    let person = engine
        .owner_of(WORKSPACE)
        .expect("the workspace's owner here");
    let tree = |workspace: &Workspace| tilo_desktop_model::Layout {
        workspaces: vec![workspace.clone()],
        ..tilo_desktop_model::Layout::default()
    };
    let session = std::sync::Arc::new(crate::app_wire::LayoutAuthority::new(
        tree(&workspace),
        crate::engine::revocations_of(None),
        crate::app_wire::Names {
            person: person.clone(),
            earlier: Vec::new(),
        },
    ));
    let grants = crate::app_wire::SessionGrants::new(
        std::sync::Arc::clone(&engine),
        std::sync::Arc::clone(&session),
    );
    let today = "so.tilo.lang.today";
    // Today's "Always" for Tasks' action: a grant naming the app, given by the person.
    let scope = AnswerScope::action("so.tilo.tasks", "tasks", "clear_done");
    let (kind, allow) = (ActionKind::Undoable, Answer::Allow);
    let grant = answer_grant(
        WORKSPACE,
        Grantee::app(today),
        (&scope, kind),
        allow,
        Grantee::person(&person),
    );
    let id = grant.id.as_str().to_owned();
    let request = tilo_workspace_service::apps::app_actions::AppActionConsentRequest {
        handle: "app-action:i1".into(),
        workspace: WORKSPACE.into(),
        person,
        app_id: today.into(),
        agent_session: None,
        target_app: "so.tilo.tasks".into(),
        resource_label: "Tasks".into(),
        action_label: "Delete finished tasks".into(),
        kind,
        risk: tilo_actions::RiskLevel::Destructive,
        confirming: false,
        moderation: None,
        scope: None,
        draft: None,
        ask: None,
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let recorded = runtime.block_on(async {
        let mut recording = grants.record(
            &request,
            grant,
        );
        // Written into the governance, the grant is not yet in force: the session does not carry it.
        let held = |engine: &crate::engine::Engine| {
            engine.policy(WORKSPACE).is_ok_and(|policy| {
                policy
                    .grants
                    .iter()
                    .any(|grant| grant.id.as_str() == id)
            })
        };
        // A loop that never sees the grant is a hang the suite only cuts off, so the governance
        // the engine holds is reported instead.
        let mut turns = 0;
        while !held(&engine) {
            assert!(
                turns < 4096,
                "the governance never carries the grant the session was given: {:?}",
                engine.policy(WORKSPACE)
            );
            turns += 1;
            tokio::select! {
                recorded = &mut recording => panic!("in force before the session carries it: {recorded:?}"),
                () = tokio::task::yield_now() => {}
            }
        }
        // The loop's pass: the governance projected into the session, and the session published.
        let policy = engine.policy(WORKSPACE).expect("the workspace's governance here");
        assert!(project(&mut workspace, &policy), "the session takes the grant");
        session.publish(tree(&workspace));
        recording.await
    });
    assert_eq!(recorded.as_deref(), Ok(id.as_str()));

    runtime.block_on(async {
        grants.revoke(WORKSPACE, &id);
        while engine
            .policy(WORKSPACE)
            .is_ok_and(|policy| policy.grants.iter().any(|grant| grant.id.as_str() == id))
        {
            tokio::task::yield_now().await;
        }
    });
}
