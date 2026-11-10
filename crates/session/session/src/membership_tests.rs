//! The workspace's own governance takes the people the settings change,
//! and refuses what it refuses, whatever this desk holds; and what it has
//! of them is what the desk's copy shows.

tilo_automerge_isolate::test_worker!();

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use tilo_action_policy::{Change, Grant, Role, Subject, WorkspacePolicy};
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{Workspace, WorkspaceId, WorkspaceSlot};
use tilo_network_actor::HostIdentity;
use tilo_peer_state::join::MemberRole;
use tilo_peer_state::owner::testing::{self, governor::TestGovernor};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;
use tilo_session_client::engine::Engine;
use tilo_session_client::membership::{Membership, authorship, project};

const WORKSPACE: &str = testing::WORKSPACE;

/// The identity of the device certified as `cert` , signing with `key` .
fn identity(cert: &tilo_user_identity::DeviceCert, key: &SigningKey) -> HostIdentity {
    HostIdentity::kept(cert.clone(), key.clone())
}

/// This device's engine, holding the workspace it governs, and the desk's own view of it with
/// the person in it as its owner. `joiner` joins the workspace too, as their own device, and
/// comes back as the person the workspace knows them by.
fn workspace_here(joiner: &str) -> (Engine, Workspace, String, tempfile::TempDir) {
    let person_root = SigningKey::from_bytes(&[9; 32]);
    let person_key = SigningKey::from_bytes(&[10; 32]);
    let cert = testing::certificate(&person_root, &person_key, "This device");
    let directory = tempfile::TempDir::new().expect("a state directory");
    let engine = Engine::open(
        identity(&cert, &person_key),
        Some(directory.path()),
        std::iter::empty(),
        crate::platform::custody::of_this_desk(),
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
    // the workspace's own plane.
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace");
    let now = tilo_clock::SystemTime::now();
    for (bytes, _) in governor.outgoing() {
        engine
            .owner()
            .receive(
                workspace_id,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                postcard::from_bytes(bytes).expect("a workspace change"),
                None,
                now,
            )
            .expect("this device takes the workspace as its governor has it");
    }
    let mut workspace = Workspace::in_workspace(
        WorkspaceSlot::of_workspace(workspace_id),
        workspace_id,
        "Test",
    );
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
fn role_of(engine: &Engine, person: &str) -> Option<MemberRole> {
    engine
        .people(WORKSPACE)
        .expect("the workspace's people")
        .into_iter()
        .find(|held| held.person == person && !held.removed)
        .map(|held| held.role)
}

/// Two devices of one workspace: the owner's, which signs for the workspace, and this desk's,
/// both holding what the workspace admitted.
struct TwoDesks {
    workspace_id: WorkspaceId,
    owner: Engine,
    owner_cert: tilo_user_identity::DeviceCert,
    desk: Engine,
    desk_cert: tilo_user_identity::DeviceCert,
    person: String,
    now: tilo_clock::SystemTime,
    _states: (tempfile::TempDir, tempfile::TempDir, tempfile::TempDir),
}

/// The workspace's owner admits ada as their own device, and what that changed reaches both
/// devices' copies of the workspace, as the workspace's host carries it.
fn the_owner_admitted_ada() -> TwoDesks {
    let workspace_id = WorkspaceId::parse(WORKSPACE).expect("a workspace");
    let now = tilo_clock::SystemTime::now();
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
        crate::platform::custody::of_this_desk(),
    )
    .expect("the owner's engine opens");
    let mut governor = testing::enroll_into(governor, a_engine.owner(), &a_key, &a_cert)
        .expect("the workspace is the owner's");
    // The person at this desk joins it as their own device, and so does another member.
    let b_root = SigningKey::from_bytes(&[53; 32]);
    let b_key = SigningKey::from_bytes(&[54; 32]);
    let b_cert = testing::certificate(&b_root, &b_key, "This desk");
    let b_state = tempfile::TempDir::new().expect("this desk's state directory");
    let b_engine = Engine::open(
        identity(&b_cert, &b_key),
        Some(b_state.path()),
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
    )
    .expect("this desk's engine opens");
    governor = testing::enroll_into(governor, b_engine.owner(), &b_key, &b_cert)
        .expect("the workspace admits this desk");
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
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(a_cert.device_public_key),
                    message.clone(),
                    None,
                    now,
                )
                .expect("a device takes the workspace as its host has it");
        }
    }
    TwoDesks {
        workspace_id,
        owner: a_engine,
        owner_cert: a_cert,
        desk: b_engine,
        desk_cert: b_cert,
        person: hex::encode(her.user_public_key),
        now,
        _states: (a_state, b_state, hers_at),
    }
}

/// What this desk holds of a workspace two devices share after the owner
/// raised ada from their own device: the people the workspace has here,
/// the owner, this desk's person, and ada, as the owner's change left her.
fn the_owner_raised_ada() -> (WorkspaceId, WorkspacePolicy, String, String, String) {
    let desks = the_owner_admitted_ada();
    assert_eq!(
        role_of(&desks.desk, &desks.person),
        Some(MemberRole::Editor),
        "the workspace has her as an editor"
    );

    // The owner gives her moderation from their own device, and what they wrote reaches this
    // desk.
    Membership::of(&desks.owner, desks.workspace_id)
        .commit(&Change::SetRole {
            person: desks.person.clone(),
            role: Role::Moderator,
        })
        .expect("the workspace takes the change");
    let written = desks
        .owner
        .owner()
        .with_outgoing(desks.workspace_id, desks.now, |_, outgoing| {
            outgoing
                .entries()
                .map(|entry| postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string()))
                .collect::<Result<Vec<neighbour_wire::NeighbourMsg>, String>>()
        })
        .expect("the owner's device published the change");
    assert!(
        !written.is_empty(),
        "the owner's device published something"
    );
    for message in written {
        desks
            .desk
            .owner()
            .receive(
                desks.workspace_id,
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(
                    desks.owner_cert.device_public_key,
                ),
                message,
                None,
                desks.now,
            )
            .expect("this desk takes the change");
    }
    assert_eq!(
        role_of(&desks.desk, &desks.person),
        Some(MemberRole::Moderator),
        "this desk's workspace has her as a moderator"
    );
    let policy = desks
        .desk
        .policy(WORKSPACE)
        .expect("the workspace's people");
    (
        desks.workspace_id,
        policy,
        hex::encode(desks.owner_cert.user_public_key),
        hex::encode(desks.desk_cert.user_public_key),
        desks.person,
    )
}

/// A role one member's device sets reaches another member's desk: what this desk has of the
/// workspace's people — the roster the People tab shows — has her with it, though the device
/// that set it is not here.
#[test]
fn a_role_another_member_sets_reaches_this_desks_people() {
    let (workspace_id, policy, owner, desk, person) = the_owner_raised_ada();
    // What this desk had of the workspace's people before the change is the owner's and this
    // person's; after it, what the workspace has.
    let mut workspace = Workspace::in_workspace(
        WorkspaceSlot::of_workspace(workspace_id),
        workspace_id,
        "Test",
    );
    workspace.set_owner(owner.clone());
    workspace.policy.members.insert(desk.clone(), Role::Editor);
    assert!(
        project(&mut workspace, &policy),
        "the people the workspace has are projected on to this desk"
    );
    assert_eq!(
        workspace.role_of(&person),
        Some(Role::Moderator),
        "the People tab shows her with the role the owner set"
    );
    assert_eq!(
        workspace.role_of(&desk),
        Some(Role::Editor),
        "and this person as the workspace has her"
    );
    assert!(
        !workspace.policy.members.contains_key(&owner),
        "the owner is the workspace's owner, not one more of its members"
    );
    assert!(
        !project(&mut workspace, &policy),
        "a desk already showing them is not written again"
    );
}

/// A person named in the workspace's settings is given another role,
/// removed, and named again: each is a change the workspace's own
/// admission makes, so it reaches every member, not only this desk.
#[test]
fn the_workspace_takes_the_people_its_settings_change() {
    let (engine, mut workspace, person, _state) = workspace_here("ada");
    let membership = Membership::of(&engine, workspace.id());
    assert_eq!(
        role_of(&engine, &person),
        Some(MemberRole::Editor),
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
        vec![Change::SetRole {
            person: person.clone(),
            role: Role::Moderator
        }],
        "a person the workspace has has their role set"
    );
    membership
        .commit(&set[0])
        .expect("the workspace takes the change");
    assert_eq!(
        role_of(&engine, &person),
        Some(MemberRole::Moderator),
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
        vec![Change::RemoveMember {
            person: person.clone()
        }],
        "a person the settings drop is removed"
    );
    membership
        .commit(&removed[0])
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
        vec![Change::AddMember {
            person: person.clone(),
            role: Role::Editor
        }],
        "a person the workspace does not have is added"
    );
    membership
        .commit(&added[0])
        .expect("the workspace takes her back");
    assert_eq!(
        role_of(&engine, &person),
        Some(MemberRole::Editor),
        "the workspace has her again"
    );
}

/// A package the person opens authors its own records in the workspace through the workspace's
/// governance, and the desk's projection is what the launch reads its authorship from.
#[test]
fn the_app_the_person_opens_authors_its_records_through_the_workspace() {
    let (engine, mut workspace, _, _state) = workspace_here("ada");
    let app = "so.tilo.notes";
    let now = tilo_clock::unix_now();
    let make_up: Vec<Grant> = workspace
        .policy
        .grants
        .iter()
        .filter(|grant| tilo_action_policy::roles::is_session_app(grant))
        .cloned()
        .collect();
    assert!(
        !workspace.app_is_author_at(app, now),
        "a workspace an app was never opened in makes it no author"
    );

    Membership::of(&engine, workspace.id())
        .grant_app_authorship(app)
        .expect("the workspace admits its person's grant");

    let policy = engine
        .policy(WORKSPACE)
        .expect("the workspace's governance here");
    assert!(
        policy
            .grants
            .iter()
            .any(|grant| grant.id.as_str() == format!("app-author:{app}")
                && matches!(grant.subject, Subject::App(_))),
        "the workspace's governance holds the authorship, so every device has it"
    );
    assert!(
        project(&mut workspace, &policy),
        "the desk's authorship is the governance's projection"
    );
    assert!(
        workspace.app_is_author_at(app, now),
        "the launch reads the app's authorship off the desk"
    );
    assert!(
        workspace.launch_grants(now).authors(app),
        "and hands it the authorship when it starts"
    );
    assert!(
        !project(&mut workspace, &policy),
        "a projection of what the desk has already changes nothing"
    );
    assert_eq!(
        workspace
            .policy
            .grants
            .iter()
            .filter(|grant| tilo_action_policy::roles::is_session_app(grant))
            .cloned()
            .collect::<Vec<Grant>>(),
        make_up,
        "the session's own app grants are the desk's make-up and stay"
    );
}

/// A desk holding one workspace, as a launch finds it.
struct OneDesk(std::cell::RefCell<Vec<Workspace>>);

impl tilo_session_client::membership::Workspaces for OneDesk {
    fn edit(
        &self,
        edit: &mut dyn FnMut(&mut [Workspace]) -> Result<bool, String>,
    ) -> Result<(), String> {
        edit(&mut self.0.borrow_mut()).map(|_| ())
    }
}

#[test]
fn a_workspace_that_lives_only_on_the_desk_lets_its_owner_author_records() {
    let app = "so.tilo.notes";
    let now = tilo_clock::unix_now();
    let mut workspace = Workspace::create("Notes");
    workspace.set_owner("ada");
    let id = workspace.id().to_string();
    let desk = OneDesk(std::cell::RefCell::new(vec![workspace]));
    assert!(
        !desk.0.borrow()[0].launch_grants(now).authors(app),
        "an app no one opened started read-only before"
    );

    authorship(None, &desk, &id, app).expect("the owner may let it author");
    let shown = desk.0.borrow();
    assert!(
        shown[0].app_is_author_at(app, now),
        "the launch reads the app's authorship off the desk"
    );
    assert!(
        shown[0].launch_grants(now).authors(app),
        "and hands it the authorship when it starts"
    );
    let granted = shown[0].policy.grants.len();
    drop(shown);

    authorship(None, &desk, &id, app).expect("there is nothing left to ask");
    assert_eq!(
        desk.0.borrow()[0].policy.grants.len(),
        granted,
        "a second opening writes nothing"
    );
}

/// A change the workspace's admission refuses leaves its people as they were, whatever this
/// desk's copy of them says.
#[test]
fn a_change_the_workspace_refuses_leaves_its_people_as_they_were() {
    let (engine, workspace, _, _state) = workspace_here("ada");
    let membership = Membership::of(&engine, workspace.id());
    // A person this desk's copy names and the workspace never took in: a role for her is
    // refused, because there is no membership of hers to give a role.
    let stranger = hex::encode(SigningKey::from_bytes(&[31; 32]).verifying_key().to_bytes());
    assert!(
        membership
            .commit(&Change::SetRole {
                person: stranger.clone(),
                role: Role::Editor,
            })
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

/// The desk this computer keeps under `state` , acting as `person` , with its own workspace
/// governed by this computer's device — the workspace's owner — and ada admitted by a device of
/// her own.
struct GovernedDesk {
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
    now: tilo_clock::SystemTime,
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
            action_plane: socket.map_or(crate::ActionPlane::Off, |socket| {
                crate::ActionPlane::At(socket.to_path_buf())
            }),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session")
}

fn a_governed_desk() -> GovernedDesk {
    governed_desk(None)
}

/// [`a_governed_desk`] whose session serves its apps' broker, so this computer's own device
/// changes the workspace through the session it runs.
fn a_governed_desk_serving() -> GovernedDesk {
    governed_desk(Some(private_directory()))
}

fn governed_desk(served: Option<tempfile::TempDir>) -> GovernedDesk {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let (identity, cert, key) = provisioned_identity(&mut home);
    let person = hex::encode(cert.user_public_key);
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = a_workspace_on_the_desk(state.path());
    let now = tilo_clock::SystemTime::now();
    let hers = tempfile::tempdir().expect("her state directory");
    let (her, _her_cert, _her_key, ada) =
        a_second_device(state.path(), hers.path(), workspace_id, &cert, &key, now);
    let socket = served
        .as_ref()
        .map(|served| served.path().join("collab.sock"));
    let mut session = session_as(state.path(), &person, socket.as_deref());
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    GovernedDesk {
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

impl GovernedDesk {
    /// The session this desk runs.
    fn session(&mut self) -> &mut crate::Session {
        self.session.as_mut().expect("the session runs")
    }

    /// The session's sheet as the desk holds it.
    fn settings(&mut self) -> tilo_sheets::workspace_settings::Settings {
        self.session()
            .workspace_settings()
            .expect("the session hosts workspace settings")
            .settings()
    }

    /// Run one frame of the session's desk.
    fn frames(&mut self) {
        self.session()
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
    }

    /// Close the session, as this computer does when its desk is not up.
    fn close(&mut self) {
        self.session = None;
    }
}

/// The sheet the session hosts, as the person at this desk presses it.
fn settings_sheet(
    desk: &mut GovernedDesk,
) -> tilo_app::framework::testing::HeadlessApp<tilo_sheets::workspace_settings::WorkspaceSettingsApp>
{
    desk.session()
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
            "workspace-settings",
            &serde_json::Map::new(),
        )
        .expect("settings opens on the workspace in front");
    desk.frames();
    let seat = desk
        .session()
        .workspace_settings()
        .expect("the session hosts workspace settings")
        .clone();
    let mut app = tilo_app::framework::testing::HeadlessApp::<
        tilo_sheets::workspace_settings::WorkspaceSettingsApp,
    >::for_person_with_lent(&desk.person, seat)
    .expect("the sheet");
    app.harness().resize((1280, 900)).expect("a screen");
    app.settle();
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::ShowTab {
            tab: "people".to_owned(),
        },
    )
    .expect("show the people");
    desk.frames();
    app.settle();
    app
}

/// The role the sheet shows for `who` .
fn shown_role(desk: &mut GovernedDesk, who: &str) -> Option<String> {
    let who = who.to_owned();
    desk.settings()
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
    let mut desk = a_governed_desk();
    let mut app = settings_sheet(&mut desk);
    let ada = desk.ada.clone();
    assert_eq!(
        shown_role(&mut desk, &ada).as_deref(),
        Some("editor"),
        "the People tab shows her as the workspace has her"
    );
    press(&mut app, &format!("ws-people-role-{ada}"));
    activate(&mut app, "Commenter");
    desk.frames();
    app.settle();
    assert_eq!(
        shown_role(&mut desk, &ada).as_deref(),
        Some("commenter"),
        "the sheet's row says what the workspace now has her as"
    );
    drop(app);
    desk.close();
    assert_eq!(
        her_device_takes(
            desk.state.path(),
            desk.workspace_id,
            &desk.cert,
            &desk.key,
            &desk.her,
            &desk.ada,
            desk.now
        ),
        Some(MemberRole::Commenter),
        "her device takes the role the person set here"
    );
}

/// A role change the workspace refuses says why in the sheet, and leaves the person with the
/// role they had.
#[test]
fn a_refused_role_change_says_why_and_keeps_the_role() {
    let _settled = ui::motion::testing::settled();
    let mut desk = a_governed_desk();
    let mut app = settings_sheet(&mut desk);
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::SetRole {
            member: desk.ada.clone(),
            role: "owner".to_owned(),
        },
    )
    .expect("the sheet asks for the role");
    desk.frames();
    app.settle();
    let notice = desk
        .settings()
        .notice
        .expect("the sheet says the workspace refused it");
    assert_eq!(notice.title, "The workspace refused that change");
    assert!(
        notice
            .detail
            .as_deref()
            .is_some_and(|detail| detail.contains("ownership is set when the workspace is made")),
        "the sheet says why: {:?}",
        notice.detail
    );
    let ada = desk.ada.clone();
    assert_eq!(
        shown_role(&mut desk, &ada).as_deref(),
        Some("editor"),
        "she keeps the role she had"
    );
    let governed = desk
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
    let mut desk = a_governed_desk();
    // The workspace grants an app the camera, as one of its devices wrote.
    let mut tree = desk.session().handle().read(Clone::clone);
    tree.active_workspace_mut()
        .expect("a workspace in front")
        .grant_app(
            "so.tilo.notes",
            &tilo_desktop_model::Capability::Camera {
                scope: tilo_desktop_model::GRANT_SESSION,
            },
        );
    desk.session().handle().replace_tree(tree);
    let mut app = settings_sheet(&mut desk);
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::ShowTab {
            tab: "permissions".to_owned(),
        },
    )
    .expect("show the permissions");
    desk.frames();
    app.settle();
    let granted = desk
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
    let mut tree = desk.session().handle().read(Clone::clone);
    tilo_desktop::workspace_settings::apply_revoke(
        tree.active_workspace_mut().expect("a workspace in front"),
        &desk.person.clone(),
        &granted.key,
    )
    .expect("another device takes the grant back");
    desk.session().handle().replace_tree(tree);
    press(&mut app, "ws-settings-decision.primary");
    desk.frames();
    app.settle();
    let notice = desk
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
        desk.session().handle().read(|tree| {
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
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = SigningKey::from_bytes(&[0x61; 32]);
    let device = SigningKey::from_bytes(&[0x62; 32]);
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
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    tilo_user_identity::persist::write_device_cert(&paths.device_cert, &cert)
        .expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    (root, device, cert)
}

/// The desk this computer keeps under `state` .
fn session_keeping(state: &std::path::Path) -> crate::Session {
    crate::Session::headless(
        crate::Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            state: Some(state.to_path_buf()),
            ..crate::Options::default()
        },
        &[tilo_compositor::OutputConfig::new("HEADLESS-1", 1280, 800)],
    )
    .expect("a session")
}

/// A bot a member asks the workspace's host for is listed in the settings sheet, and Remove
/// there takes the request back through the workspace: the sheet stops listing it and the host
/// stops being asked for it.
#[test]
fn a_bot_the_sheet_removes_is_no_longer_asked_of_the_host() {
    let _settled = ui::motion::testing::settled();
    let mut desk = a_governed_desk_serving();
    let entry = "so.tilo.bots.standup";
    let workspace_id = desk.workspace_id;
    let cert = desk.cert.clone();
    change_governance(
        desk.session(),
        workspace_id,
        &cert,
        &WorkspaceMembershipCommand::RequestBot(
            tilo_peer_state::workspace_membership::BotRequest {
                entry: entry.to_owned(),
                version: "1.2.0".to_owned(),
                inputs: BTreeMap::default(),
                program: None,
            },
        ),
    );
    let mut app = settings_sheet(&mut desk);
    // The bots a member asks for are listed on the Agents tab, beside the agents themselves.
    app.effect(
        &tilo_sheets::workspace_settings::WorkspaceSettingsEffect::ShowTab {
            tab: "agents".to_owned(),
        },
    )
    .expect("show the agents");
    desk.frames();
    app.settle();
    the_sheet_lists_bot(&mut desk, entry, true);
    press(&mut app, &format!("ws-bots-remove-{entry}"));
    desk.frames();
    the_sheet_lists_bot(&mut desk, entry, false);
    let asks = desk
        .session()
        .shared
        .engine
        .as_ref()
        .expect("this device holds the workspace")
        .owner()
        .with_current(workspace_id, |view| {
            Ok(view.peer().governance().bot_requests())
        })
        .expect("the workspace's governance");
    assert!(
        asks.is_empty(),
        "the workspace no longer asks its host for the bot: {asks:?}"
    );
}

/// A person another device of the workspace took in is on this desk's copy of it, in the part
/// the workspace gives them, when the desk comes back: the People tab shows the workspace's
/// people, not only those this desk put there.
#[test]
fn the_people_another_device_admits_are_on_this_desks_people() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, key, cert) = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    // A frame brings the desk and its own workspace up.
    let workspace_id = {
        let mut session = session_keeping(state.path());
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        session
            .handle()
            .read(|tree| tree.workspaces.first().map(Workspace::id))
            .expect("a workspace on the desk")
    };
    // The workspace, as this computer's device governs it, takes ada in: a change another
    // member's device makes while this desk is away.
    let now = tilo_clock::SystemTime::now();
    let her_root = SigningKey::from_bytes(&[0x63; 32]);
    let her_key = SigningKey::from_bytes(&[0x64; 32]);
    let her = testing::certificate(&her_root, &her_key, "ada");
    {
        let engine = Engine::open(
            identity(&cert, &key),
            Some(state.path()),
            [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
            crate::platform::custody::of_this_desk(),
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
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    None,
                    now,
                )
                .expect("this device takes the workspace's change");
        }
    }
    let person = hex::encode(her.user_public_key);
    // A new session of the same desk shows the workspace's people.
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
        "the desk's workspace has the person the workspace took in"
    );
    // The sheet shows her in the People tab.
    session
        .invoke(
            tilo_desktop::desk_verbs::CHROME_RESOURCE,
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
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    home.env()
        .set("TILO_IDENTITY_ROOT", root.path())
        .set(tilo_identity_host::provisioning::FILE_SECRETS_ENV, "1");
    tilo_identity_host::provisioning::ensure_collaboration_identity(
        &home.config().join("tilo").join("identity"),
        &paths,
        tilo_identity_host::provisioning::IdentityStorage::Files,
    )
    .expect("this computer is provisioned as a device");
    let key = tilo_user_identity::persist::load_device_key(&paths).expect("this computer's key");
    let cert = tilo_user_identity::persist::read_device_cert(&paths.device_cert)
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
            action_plane: crate::ActionPlane::At(socket.to_path_buf()),
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
    broker: &tilo_action_broker::collab_broker::AppActionBroker,
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

/// The workspace this computer's desk opens when it has none of its own: the id the session's
/// engine and the test's other devices act on.
fn a_workspace_on_the_desk(state: &std::path::Path) -> WorkspaceId {
    let mut session = session_keeping(state);
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    session
        .handle()
        .read(|tree| tree.workspaces.first().map(Workspace::id))
        .expect("a workspace on the desk")
}

/// Let go of the workspace the desk founded here, so a fixture host can be the one that governs
/// it: the fixture's own genesis is the workspace's then, and the devices it admits are the
/// workspace's.
fn the_fixture_governs(mine: &Engine, workspace_id: WorkspaceId) {
    mine.delete(workspace_id)
        .expect("this device lets go of the workspace it founded");
}

/// Another person the workspace admits, on a device of her own: her engine on `her_state` , her
/// certificate and key, and the id the workspace knows her by.
fn a_second_device(
    state: &std::path::Path,
    her_state: &std::path::Path,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
    now: tilo_clock::SystemTime,
) -> (Engine, tilo_user_identity::DeviceCert, SigningKey, String) {
    let her_key = SigningKey::from_bytes(&[0x72; 32]);
    let her = testing::certificate(&SigningKey::from_bytes(&[0x71; 32]), &her_key, "ada");
    let hers = Engine::open(
        identity(&her, &her_key),
        Some(her_state),
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
    )
    .expect("her engine opens");
    // This computer's device governs the workspace, and admits her.
    let mine = Engine::open(
        identity(cert, key),
        Some(state),
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
    )
    .expect("this computer's engine opens");
    // The workspace the desk made is this computer's own already: a fixture host governs it
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
    for (bytes, _) in governor.outgoing() {
        for engine in [&mine, &hers] {
            engine
                .owner()
                .receive(
                    workspace_id,
                    tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                    postcard::from_bytes(bytes).expect("a workspace change"),
                    None,
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
        .action_plane
        .as_ref()
        .and_then(crate::plane::Plane::action_broker)
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

/// Run frames until the sheet lists the bot whose entry is `entry` , or stops listing it.
#[track_caller]
fn the_sheet_lists_bot(desk: &mut GovernedDesk, entry: &str, listed: bool) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let shows = desk
            .settings()
            .agents
            .bots
            .iter()
            .any(|bot| bot.entry.as_deref() == Some(entry));
        if shows == listed {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the sheet never {} the bot {entry}",
            if listed { "listed" } else { "dropped" }
        );
        desk.frames();
    }
}

/// Run frames until the desk shows `person` in `expected` 's part of the workspace.
#[track_caller]
fn the_desk_shows(session: &mut crate::Session, person: &str, expected: Option<Role>) {
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
            "the desk never showed the change: {shown:?}"
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
    now: tilo_clock::SystemTime,
) -> Option<MemberRole> {
    let mine = Engine::open(
        identity(cert, key),
        Some(state),
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
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
                tilo_peer_state::TransportOrigin::RetainedRelayDelta(cert.device_public_key),
                message,
                None,
                now,
            )
            .expect("her device takes the workspace's change");
    }
    hers.people(&workspace_id.to_string())
        .expect("the workspace's people")
        .into_iter()
        .find(|held| held.person == person && !held.removed)
        .map(|held| held.role)
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
    let workspace_id = a_workspace_on_the_desk(state.path());
    let now = tilo_clock::SystemTime::now();
    let her_state = tempfile::tempdir().expect("her state directory");
    let (hers, _her, _her_key, her_person) = a_second_device(
        state.path(),
        her_state.path(),
        workspace_id,
        &cert,
        &key,
        now,
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
        // What the desk shows is the workspace the session's engine holds: the role is hers, or
        // she is gone from it.
        the_desk_shows(&mut session, &her_person, expected);
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
            expected.map(|role| match role {
                Role::Moderator => MemberRole::Moderator,
                other => panic!("the test set {other:?}"),
            }),
            "her device has her as the workspace does"
        );
    }
}

/// Whether asking this session's broker to re-role someone as `scribe` is refused: the person's
/// device handed the session to an agent, and a workspace's people change at its person's
/// device only.
#[track_caller]
fn an_agent_is_refused_a_role(
    broker: &tilo_action_broker::collab_broker::AppActionBroker,
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
            },
        )
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("the host answers");
    matches!(
        answer,
        Err(tilo_actions::remote::ActionHostError::NotPermitted)
    )
}

/// An agent the person's device handed its session to cannot change a workspace's people: that
/// is the person's own act at their device.
#[test]
fn an_agent_cannot_change_a_workspaces_people() {
    let mut home = tilo_test_env::TestHome::new().expect("a home for this computer");
    let (_identity, cert, _key) = provisioned_identity(&mut home);
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = a_workspace_on_the_desk(state.path());
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
        .action_plane
        .as_ref()
        .and_then(crate::plane::Plane::action_broker)
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
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
    )
    .expect("this computer's engine opens");
    assert!(
        mine.people(&workspace_id.to_string())
            .expect("the workspace's people")
            .iter()
            .all(|held| held.person != hex::encode(stranger)),
        "the agent's change is not the workspace's"
    );
}

/// An invitation this device's person makes into `workspace_id` from their own device: the
/// workspace's record has it, and the link it signed.
fn invite_from_this_device(
    state: &std::path::Path,
    workspace_id: WorkspaceId,
    cert: &tilo_user_identity::DeviceCert,
    key: &SigningKey,
) -> tilo_peer_state::join::invitations::IssuedInvitation {
    use tilo_peer_state::join::invitations::IssueInvitationRequest;

    let engine = Engine::open(
        identity(cert, key),
        Some(state),
        [(WorkspaceSlot::of_workspace(workspace_id), workspace_id)],
        crate::platform::custody::of_this_desk(),
    )
    .expect("this computer's engine opens");
    let now = tilo_clock::SystemTime::now();
    engine
        .owner()
        .issue_invitation(
            workspace_id,
            cert,
            IssueInvitationRequest {
                request_id: [0x71; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: None,
                invitee_name: Some("Ada".into()),
                expires_at_ns: tilo_clock::unix_now_ns()
                    .checked_add(60_000_000_000)
                    .expect("an expiry"),
                inviter_name: None,
            },
            Vec::new(),
            key,
            now,
        )
        .expect("this device issues the link")
}

/// The invitations the desk's own action lists for the workspace `workspace` .
fn listed_invitations(session: &mut crate::Session, workspace: &str) -> Vec<serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("workspace".into(), workspace.into());
    let listed = session
        .invoke(
            tilo_session_core::shell::DESK_RESOURCE,
            "invitations",
            &arguments,
        )
        .expect("the desk lists the invitations it made")
        .completed()
        .expect("listing finishes on return")
        .value
        .expect("the listing's invitations");
    serde_json::from_value(listed).expect("a list of invitations")
}

/// Whether the desk's own action cancelling `invite` changed the workspace.
fn cancelled_at_the_desk(session: &mut crate::Session, workspace: &str, invite: &str) -> bool {
    let mut arguments = serde_json::Map::new();
    arguments.insert("workspace".into(), workspace.into());
    arguments.insert("invite".into(), invite.into());
    let answer = session
        .invoke(
            tilo_session_core::shell::DESK_RESOURCE,
            "cancel-invitation",
            &arguments,
        )
        .expect("the desk cancels the link this device made")
        .completed()
        .expect("cancelling finishes on return")
        .value
        .expect("whether the workspace changed");
    answer["cancelled"]
        .as_bool()
        .expect("whether the workspace changed")
}

/// An invitation this device's person made into a workspace is listed by the desk's own action
/// and cancelled there, and the desk lists none afterwards: inviting and taking a link back
/// need no host, only the device that made it.
#[test]
fn the_desk_lists_and_cancels_the_invitations_this_device_made() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let (_identity, key, cert) = this_computer(&mut env);
    let state = tempfile::tempdir().expect("a state directory");
    let workspace_id = {
        let mut session = session_keeping(state.path());
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
        session
            .handle()
            .read(|tree| tree.workspaces.first().map(Workspace::id))
            .expect("a workspace on the desk")
    };
    let invitation = invite_from_this_device(state.path(), workspace_id, &cert, &key);
    let mut session = session_keeping(state.path());
    session
        .step(std::time::Duration::from_millis(1))
        .expect("a frame");
    let workspace = session
        .handle()
        .read(|tree| {
            tree.workspaces
                .first()
                .map(|workspace| workspace.slot.0.to_string())
        })
        .expect("a workspace on the desk");
    let listed = listed_invitations(&mut session, &workspace);
    assert_eq!(listed.len(), 1, "the one link this device made");
    assert_eq!(listed[0]["invite"], hex::encode(invitation.invite_id));
    assert_eq!(listed[0]["role"], "editor");
    assert_eq!(listed[0]["invitee_name"], "Ada");
    let invite = hex::encode(invitation.invite_id);
    assert!(
        cancelled_at_the_desk(&mut session, &workspace, &invite),
        "the link is cancelled"
    );
    assert!(
        !cancelled_at_the_desk(&mut session, &workspace, &invite),
        "cancelling it again changes nothing"
    );
    assert!(
        listed_invitations(&mut session, &workspace).is_empty(),
        "the cancelled link is not listed"
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
    use tilo_action_policy::{ActionKind, Answer, Authority, Context, TreePath};
    use tilo_actions::remote::{Pending, PendingPoll};
    use tilo_broker_core::services::WorkspaceNodeAsk;

    fn answered<T: std::fmt::Debug>(mut pending: Pending<T>) -> T {
        match pending.poll_ready() {
            PendingPoll::Ready(value) => value,
            other => panic!("the workspace answers at once here: {other:?}"),
        }
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
    assert_eq!(share["grants"], 4, "one grant per role below the owner");
    let grants = engine
        .policy(WORKSPACE)
        .expect("the workspace's policy")
        .grants;
    assert_eq!(
        grants
            .iter()
            .filter(|grant| is_of_share(grant, "m1"))
            .count(),
        4,
        "the share is the workspace's own grants"
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
        unshared["revoked"], 4,
        "every grant of the share is taken back"
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
