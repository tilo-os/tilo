//! A workspace's administrators govern it: a member given the administrator
//! grant holds every governance action of the workspace, on a node the Editor's
//! own rule on its resource does not outrank, so workspaces that held the grant
//! where it did are rewritten to govern through it.
use super::*;
use tilo_action_policy::KindSet;
use tilo_action_policy::roles::{self, governance};

/// How many of the workspace's governance actions the member with device
/// `device` may run, as `peer`'s workspace policy decides it now.
fn governs(peer: &StatePeer, device: [u8; 32], now: SystemTime) -> usize {
    let member = peer.permissions(device, now).unwrap();
    let bound = roles::foreign_device_bound();
    let authority = member.authority(bound);
    let cx = tilo_action_policy::Context::at_time(now);
    governance::ALL
        .into_iter()
        .filter(|action| {
            member
                .policy
                .check_action(&authority, action, &cx, false)
                .is_ok()
        })
        .count()
}

const EVERY: usize = governance::ALL.len();

fn is_administrator(peer: &StatePeer, user: [u8; 32], now: SystemTime) -> bool {
    peer.governance()
        .governed_members(now)
        .unwrap()
        .iter()
        .any(|member| member.user_public_key == user && member.administrator)
}

#[test]
fn an_administrator_governs_and_an_editor_does_not() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    assert_eq!(governs(&peer, editor.device_public_key, now), 0);

    assert!(
        peer.governance_mut()
            .set_member_administrator(user, &host_key, now)
            .unwrap()
    );
    assert!(is_administrator(&peer, user, now));
    assert_eq!(governs(&peer, editor.device_public_key, now), EVERY);
    let held = peer.permissions(editor.device_public_key, now).unwrap();
    assert_eq!(held.policy.role_of(&held.person), Some(Role::Editor));

    // Setting a role takes the standing back with the grant.
    peer.governance_mut()
        .set_member_role(user, crate::join::MemberRole::Editor, &host_key, now)
        .unwrap();
    assert!(!is_administrator(&peer, user, now));
    assert_eq!(governs(&peer, editor.device_public_key, now), 0);
}

#[test]
fn an_administrator_grant_on_the_workspaces_resource_is_rewritten_once() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let person = hex::encode(user);
    let owner = Subject::Person(hex::encode(peer.owner()));
    // The grant as workspaces held it before: on the workspace's resource, where the
    // Editor's Deny at the same node outranks it.
    let legacy = Grant::new(
        roles::administrator_grant_id(&person).as_str(),
        Subject::Person(person.clone()),
        roles::workspace_resource(peer.workspace_id()),
        KindSet::ALL,
        Answer::Allow,
        owner.clone(),
    );
    assert!(roles::is_legacy_administrator_grant(
        peer.workspace_id(),
        &legacy
    ));
    peer.governance_mut()
        .put_governed_grants(std::slice::from_ref(&legacy), &host_key, now)
        .unwrap();
    assert!(is_administrator(&peer, user, now));
    assert_eq!(
        governs(&peer, editor.device_public_key, now),
        0,
        "the Editor's Deny outranks the grant where it sits"
    );

    assert!(
        peer.governance_mut()
            .reconcile_administrator_grants(&host_key, now)
            .unwrap()
    );
    assert_eq!(governs(&peer, editor.device_public_key, now), EVERY);
    assert!(is_administrator(&peer, user, now));
    let held = peer.permissions(editor.device_public_key, now).unwrap();
    let rewritten = held
        .policy
        .grants
        .iter()
        .filter(|grant| grant.id == legacy.id)
        .collect::<Vec<_>>();
    assert_eq!(
        rewritten,
        [&roles::administrator_grant(
            peer.workspace_id(),
            &person,
            owner
        )]
    );
    assert!(
        !peer
            .governance_mut()
            .reconcile_administrator_grants(&host_key, now)
            .unwrap()
    );
}

#[test]
fn reconciling_a_workspace_without_old_grants_writes_nothing_and_needs_no_writer() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let stranger = SigningKey::from_bytes(&[77; 32]);
    assert!(
        !peer
            .governance_mut()
            .reconcile_administrator_grants(&stranger, now)
            .unwrap()
    );
}

/// An administrator moderates the workspace as a Moderator does, an Editor not:
/// the grant comes and goes with the administrator's.
#[test]
fn an_administrator_moderates_and_an_editor_does_not() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let node = TreePath::workspace(peer.workspace_id())
        .app("so.tilo.chat")
        .kind("chat.message")
        .resource("welcome")
        .field(roles::MODERATION_FIELD)
        .action("remove");
    let moderates = |peer: &StatePeer| {
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .allows(&node, tilo_action_policy::ActionKind::Undoable, now)
    };
    assert!(!moderates(&peer));
    assert!(
        peer.governance_mut()
            .set_member_administrator(user, &host_key, now)
            .unwrap()
    );
    assert!(moderates(&peer));
    peer.governance_mut()
        .set_member_role(user, crate::join::MemberRole::Editor, &host_key, now)
        .unwrap();
    assert!(!moderates(&peer));
}

/// A member writes the workspace's membership from their own device, as far as
/// their grants reach: an administrator's device changes a role itself, an
/// editor's cannot.
#[test]
fn an_administrators_own_device_changes_a_role_and_an_editors_cannot() {
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    assert!(
        peer.clone()
            .governance_mut()
            .set_member_role(user, crate::join::MemberRole::Viewer, &editor_key, now)
            .is_err()
    );
    peer.governance_mut()
        .set_member_administrator(user, &host_key, now)
        .unwrap();
    assert!(
        peer.governance_mut()
            .set_member_role(user, crate::join::MemberRole::Viewer, &editor_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .role,
        Role::Viewer
    );
    assert!(!is_administrator(&peer, user, now));
}

#[test]
fn a_members_membership_command_is_signed_by_their_own_device_and_decided_by_their_grants() {
    use crate::workspace_membership::WorkspaceMembershipCommand;
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let demote = WorkspaceMembershipCommand::Member {
        change: tilo_action_policy::Change::SetRole {
            person: hex::encode(user),
            role: Role::Viewer,
        },
    };
    assert!(
        peer.clone()
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &demote, &editor_key, now)
            .is_err(),
        "an editor's grants change no role"
    );
    peer.governance_mut()
        .set_member_administrator(user, &host_key, now)
        .unwrap();
    assert!(
        peer.clone()
            .workspace_mut()
            .apply_workspace_membership_command(
                &editor,
                &demote,
                &SigningKey::from_bytes(&[7; 32]),
                now
            )
            .is_err(),
        "another key does not sign for the caller's device"
    );
    assert!(
        peer.workspace_mut()
            .apply_workspace_membership_command(&editor, &demote, &editor_key, now)
            .unwrap()
    );
    assert_eq!(
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .role,
        Role::Viewer
    );
}

/// An administrator's own device lets an app author the workspace, as a grant
/// of their own; an editor's cannot, and letting it again changes nothing.
/// A member leaves the workspace on their own device: leaving is nobody's
/// grant, and the workspace they leave takes their device rows with them.
#[test]
fn a_members_own_device_leaves_the_workspace() {
    use crate::workspace_membership::WorkspaceMembershipCommand;
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let leave = WorkspaceMembershipCommand::Member {
        change: tilo_action_policy::Change::RemoveMember {
            person: hex::encode(user),
        },
    };
    assert!(peer.permissions(editor.device_public_key, now).is_ok());
    assert!(
        peer.workspace_mut()
            .apply_workspace_membership_command(&editor, &leave, &editor_key, now)
            .unwrap(),
        "the editor left"
    );
    assert!(
        peer.permissions(editor.device_public_key, now).is_err(),
        "a device whose person left is no member's"
    );
    assert!(
        peer.governance()
            .governed_members(now)
            .unwrap()
            .iter()
            .all(|member| member.user_public_key != user),
        "the member and their device rows are off the roster"
    );
}

#[test]
fn an_administrators_own_device_lets_an_app_author_and_an_editors_cannot() {
    use crate::workspace_membership::WorkspaceMembershipCommand;
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let editor_key = SigningKey::from_bytes(&[6; 32]);
    let notes = WorkspaceMembershipCommand::GrantAppAuthorship {
        app: "so.tilo.notes".into(),
    };
    let authors = |peer: &StatePeer| {
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .policy
            .app_authors("so.tilo.notes", &tilo_action_policy::Context::at_time(now))
    };
    assert!(
        peer.clone()
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &notes, &editor_key, now)
            .is_err(),
        "an editor lets no app author the workspace"
    );
    peer.governance_mut()
        .set_member_administrator(user, &host_key, now)
        .unwrap();
    assert!(
        peer.workspace_mut()
            .apply_workspace_membership_command(&editor, &notes, &editor_key, now)
            .unwrap()
    );
    assert!(authors(&peer));
    let grant = peer
        .permissions(editor.device_public_key, now)
        .unwrap()
        .policy
        .grants
        .into_iter()
        .find(|grant| grant.id.as_str() == "app-author:so.tilo.notes")
        .unwrap();
    assert_eq!(
        grant.granted_by,
        tilo_action_policy::Subject::Person(hex::encode(user))
    );
    assert!(
        !peer
            .workspace_mut()
            .apply_workspace_membership_command(&editor, &notes, &editor_key, now)
            .unwrap(),
        "an app already authoring is no change"
    );
}

/// The workspace's own settings an editor's device writes as `change`.
fn change_settings(
    peer: &mut StatePeer,
    editor: &DeviceCert,
    change: crate::workspace_membership::SettingsChange,
    now: SystemTime,
) -> Result<bool, String> {
    peer.workspace_mut().apply_workspace_membership_command(
        editor,
        &crate::workspace_membership::WorkspaceMembershipCommand::Settings(change),
        &SigningKey::from_bytes(&[6; 32]),
        now,
    )
}

fn readers() -> tilo_action_policy::Preset {
    tilo_action_policy::Preset::new(
        "Readers",
        "Read notes.",
        vec![tilo_action_policy::Rule::new(
            tilo_action_policy::TreePath::root().app("so.tilo.notes"),
            KindSet::READ,
            tilo_action_policy::Answer::Allow,
        )],
    )
}

/// A member changes the workspace's own settings from their own device as far as
/// their grants reach: an administrator renames the workspace, saves and assigns
/// its grant sets and grants and takes back its apps' sandbox capabilities;
/// an editor does none of it.
#[test]
fn an_administrators_own_device_changes_the_workspaces_settings_and_an_editors_cannot() {
    use crate::workspace_membership::SettingsChange;
    use collab_types::caps::{Access, CapOverride, Principal, Scope, Subject as CapSubject};
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let rename = SettingsChange::Rename {
        name: "Plans".into(),
    };
    let capability = CapOverride::grant(
        CapSubject::app("so.tilo.reader"),
        Scope::NetHost("example.org".into()),
        Access::Read,
    );
    let sandbox = SettingsChange::GrantSandbox { capability };
    let save = SettingsChange::SavePreset { preset: readers() };
    for change in [rename.clone(), sandbox.clone(), save.clone()] {
        assert!(
            change_settings(&mut peer.clone(), &editor, change.clone(), now).is_err(),
            "an editor does not {change:?}"
        );
    }

    peer.governance_mut()
        .set_member_administrator(user, &host_key, now)
        .unwrap();
    assert!(change_settings(&mut peer, &editor, rename, now).unwrap());
    assert_eq!(peer.governance().workspace_name().as_deref(), Some("Plans"));
    assert!(change_settings(&mut peer, &editor, sandbox, now).unwrap());
    let granted = peer.governance().governed_sandbox().unwrap();
    let [held] = granted.as_slice() else {
        panic!("one capability: {granted:?}");
    };
    assert_eq!(held.granted_by, Some(Principal::Person(hex::encode(user))));
    assert!(change_settings(&mut peer, &editor, save, now).unwrap());
    assert!(
        change_settings(
            &mut peer,
            &editor,
            SettingsChange::AssignPreset {
                preset: "Readers".into(),
                subject: tilo_action_policy::Subject::agent("scribe"),
                lifetime: tilo_action_policy::Lifetime::UntilRevoked,
            },
            now,
        )
        .unwrap()
    );
    let policy = peer
        .permissions(editor.device_public_key, now)
        .unwrap()
        .policy;
    assert!(
        policy
            .grants
            .iter()
            .any(|grant| grant.subject == tilo_action_policy::Subject::agent("scribe"))
    );
    assert!(
        change_settings(
            &mut peer,
            &editor,
            SettingsChange::RevokeSandbox {
                capability: held.id.clone(),
            },
            now,
        )
        .unwrap()
    );
    assert!(peer.governance().governed_sandbox().unwrap().is_empty());
}

/// A Moderator names the workspace by their role; a grant on one saved set's
/// assign node lets an editor assign that set, and nothing else.
#[test]
fn a_moderator_renames_the_workspace_and_a_grant_on_a_set_assigns_only_it() {
    use crate::workspace_membership::SettingsChange;
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    peer.governance_mut()
        .put_governed_presets(&[readers()], &host_key, now)
        .unwrap();
    let assign = SettingsChange::AssignPreset {
        preset: "Readers".into(),
        subject: tilo_action_policy::Subject::agent("scribe"),
        lifetime: tilo_action_policy::Lifetime::UntilRevoked,
    };
    assert!(change_settings(&mut peer.clone(), &editor, assign.clone(), now).is_err());
    let owner = hex::encode(peer.owner());
    let on_set = tilo_action_policy::Grant::new(
        "assign-readers",
        tilo_action_policy::Subject::person(hex::encode(user)),
        tilo_desktop_model::preset_assign_node(peer.workspace_id(), "Readers"),
        KindSet::OUTSIDE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Subject::person(owner),
    );
    peer.governance_mut()
        .put_governed_grants(&[on_set], &host_key, now)
        .unwrap();
    assert!(change_settings(&mut peer, &editor, assign, now).unwrap());
    assert!(
        change_settings(
            &mut peer.clone(),
            &editor,
            SettingsChange::SavePreset { preset: readers() },
            now
        )
        .is_err(),
        "assigning one set saves none"
    );

    let rename = SettingsChange::Rename {
        name: "Plans".into(),
    };
    assert!(change_settings(&mut peer.clone(), &editor, rename.clone(), now).is_err());
    peer.governance_mut()
        .set_member_role(user, crate::join::MemberRole::Moderator, &host_key, now)
        .unwrap();
    assert!(change_settings(&mut peer, &editor, rename, now).unwrap());
    assert_eq!(peer.governance().workspace_name().as_deref(), Some("Plans"));
}

/// An administrator lets one app use another app's action: the grant the consent
/// sheet's "Always allow" writes, named by the person here — recorded once however
/// often it is given, and taken back by the same four names.
#[test]
fn an_administrator_lets_one_app_use_another_apps_action_and_takes_it_back() {
    use crate::workspace_membership::{AppAction, SettingsChange};
    use tilo_action_policy::{Answer, KindSet, Subject, TreePath};
    let (mut peer, host_key, _, _, now) = crate::admission_tests::seeded();
    let (editor, user) = crate::admission_tests::add_editor(&mut peer, &host_key, now);
    let named = AppAction::new(
        "so.tilo.lang.today",
        "so.tilo.lang.notes",
        "tl:so.tilo.lang.notes:root",
        "file-note",
    )
    .expect("an app's action, named");
    let grant = SettingsChange::GrantAppAction(named.clone());
    assert!(
        change_settings(&mut peer.clone(), &editor, grant.clone(), now).is_err(),
        "an editor lets no app act"
    );
    peer.governance_mut()
        .set_member_administrator(user, &host_key, now)
        .unwrap();
    let held = |peer: &mut StatePeer| -> Vec<tilo_action_policy::Grant> {
        peer.permissions(editor.device_public_key, now)
            .unwrap()
            .policy
            .grants
            .into_iter()
            .filter(|grant| grant.subject == Subject::app("so.tilo.lang.today"))
            .collect()
    };
    assert!(change_settings(&mut peer, &editor, grant.clone(), now).unwrap());
    let once = held(&mut peer);
    let [granted] = once.as_slice() else {
        panic!("one grant: {once:?}");
    };
    assert_eq!(granted.id, named.grant_id(), "the action is the grant's id");
    assert_eq!(
        granted.node,
        named.node(&TreePath::workspace(peer.workspace_id().to_string())),
        "it names the action wherever the resource sits in the app"
    );
    assert_eq!(granted.kinds, KindSet::READ_UNDOABLE);
    assert_eq!(granted.answer, Answer::Allow);
    assert_eq!(granted.granted_by, Subject::person(hex::encode(user)));
    // Given again, it is the grant already there, not a second one.
    change_settings(&mut peer, &editor, grant, now).unwrap();
    assert_eq!(
        held(&mut peer).len(),
        1,
        "one standing grant for one action"
    );
    assert!(
        change_settings(
            &mut peer,
            &editor,
            SettingsChange::RevokeAppAction(named),
            now
        )
        .unwrap()
    );
    assert!(
        held(&mut peer).is_empty(),
        "taken back by the same four names"
    );
}
