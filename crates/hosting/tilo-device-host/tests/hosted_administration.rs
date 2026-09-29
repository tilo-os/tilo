//! A member the operator makes an administrator manages the hosted
//! workspace's permission profiles through desktop commands: the owner applies
//! them with that member's authority and records assignments as governance.
tilo_automerge_isolate::test_worker!();

use ed25519_dalek::SigningKey;
use tilo_peer_state::desktop_command::*;
use tilo_peer_state::join::{
    JoinChallengeRequest, JoinCompleteRequest, MemberRole, sign_join_proof,
};
use tilo_peer_state::owner_commit::*;
use tilo_user_identity::{DeviceCapabilities, DeviceCert, sign_device_cert};
use tilo_workspace_host::{
    SessionOwner,
    collaboration::{
        CollaborationService, IssueInvitationRequest, RemoveMemberRequest, SetMemberRoleRequest,
    },
};

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .unwrap()
}

/// A workspace host with one editor, the machine owner's own session.
fn hosted_with_session() -> (
    tempfile::TempDir,
    SessionOwner,
    CollaborationService,
    DeviceCert,
) {
    let temp = tempfile::tempdir().unwrap();
    let mut owner =
        SessionOwner::initialize_workspace(&temp.path().join("owner"), "Computer").unwrap();
    let administrator = owner.peer().authority().device_public_key;
    let now = now_ns();
    let root = SigningKey::from_bytes(&[51; 32]);
    let device = SigningKey::from_bytes(&[52; 32]);
    let mut session = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "Session".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: now.saturating_sub(1),
        expires_at_ns: None,
        signature: [0; 64],
    };
    sign_device_cert(&mut session, &root).unwrap();
    let service = CollaborationService::new(vec![]);
    let issued = service
        .issue(
            &mut owner,
            administrator,
            IssueInvitationRequest {
                request_id: [61; 32],
                role: MemberRole::Editor,
                recipient_user_public_key: None,
                invitee_name: Some("Session".into()),
                expires_at_ns: now + 120_000_000_000,
            },
            now,
        )
        .unwrap();
    let challenge = service
        .challenge(
            &mut owner,
            session.device_public_key,
            JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert: session.clone(),
                request_id: [62; 32],
            },
            now + 1,
        )
        .unwrap()
        .challenge;
    service
        .complete(
            &mut owner,
            session.device_public_key,
            JoinCompleteRequest {
                proof: sign_join_proof(challenge, &device).unwrap(),
            },
            now + 2,
        )
        .unwrap();
    (temp, owner, service, session)
}

fn command(
    owner: &SessionOwner,
    caller: &DeviceCert,
    command: DesktopCommand,
) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: owner.peer().room().into(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: caller.clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: owner.peer().room().into(),
                    heads: Vec::new(),
                },
                command,
            },
        },
    }
}

fn save_user_files() -> DesktopCommand {
    DesktopCommand::SavePermissionProfile {
        profile_json: serde_json::json!({
            "name": "user-files",
            "description": "Apps read and write the user's files",
            "rules": [{"scope": {"FsPath": "/data/out"}, "effect": "Grant", "verb": "Write"}],
        })
        .to_string(),
    }
}

fn assign_to_notes() -> DesktopCommand {
    DesktopCommand::AssignPermissionProfile {
        profile: "user-files".into(),
        subject_kind: "app".into(),
        subject: "so.tilo.notes".into(),
        expires_unix: 0,
    }
}

fn make_administrator(
    owner: &mut SessionOwner,
    service: &CollaborationService,
    session: &DeviceCert,
) {
    let administrator = owner.peer().authority().device_public_key;
    service
        .set_member_administrator(
            owner,
            administrator,
            RemoveMemberRequest {
                request_id: [63; 32],
                user_public_key: session.user_public_key,
            },
            now_ns(),
        )
        .unwrap();
}

fn room_capabilities(owner: &SessionOwner) -> Vec<collab_types::caps::CapOverride> {
    tilo_peer_state::room::extract_governed_fields(owner.peer().admitted_room().unwrap()).2
}

#[test]
fn an_editor_may_not_manage_permission_profiles() {
    let (_temp, mut owner, _service, session) = hosted_with_session();
    let saving = command(&owner, &session, save_user_files());
    let refusal = owner.commit_desktop_command(&saving).unwrap_err();
    assert!(
        format!("{refusal:?}").contains("administrator")
            || format!("{refusal:?}").contains("administer"),
        "{refusal:?}"
    );
}

#[test]
fn an_administrator_session_saves_and_assigns_a_permission_profile() {
    let (_temp, mut owner, service, session) = hosted_with_session();
    make_administrator(&mut owner, &service, &session);
    let member = owner
        .peer()
        .governed_members(std::time::SystemTime::now())
        .unwrap()
        .into_iter()
        .find(|member| member.user_public_key == session.user_public_key)
        .unwrap();
    assert!(!member.removed);
    assert!(member.administrator);

    let saving = command(&owner, &session, save_user_files());
    owner
        .commit_desktop_command(&saving)
        .unwrap()
        .validate_for(&saving)
        .unwrap();
    let profiles = tilo_desktop_model::projection::workspace_permission_profiles_of(
        owner.peer().admitted_room().unwrap(),
    );
    assert_eq!(
        profiles.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["user-files"]
    );

    let assigning = command(&owner, &session, assign_to_notes());
    owner
        .commit_desktop_command(&assigning)
        .unwrap()
        .validate_for(&assigning)
        .unwrap();
    let granted = room_capabilities(&owner)
        .into_iter()
        .filter(|cap| cap.subject == collab_types::caps::Subject::app("so.tilo.notes"))
        .collect::<Vec<_>>();
    assert_eq!(granted.len(), 1, "{granted:?}");
    assert_eq!(
        granted[0].scope,
        collab_types::caps::Scope::FsPath("/data/out".into())
    );

    // A restarted owner keeps both.
    let root = owner.root().to_owned();
    drop(owner);
    let owner = SessionOwner::open(&root).unwrap();
    assert!(
        room_capabilities(&owner)
            .iter()
            .any(|cap| cap.subject == collab_types::caps::Subject::app("so.tilo.notes"))
    );
}

#[test]
fn changing_the_role_demotes_an_administrator() {
    let (_temp, mut owner, service, session) = hosted_with_session();
    make_administrator(&mut owner, &service, &session);
    let administrator = owner.peer().authority().device_public_key;
    service
        .set_member_role(
            &mut owner,
            administrator,
            SetMemberRoleRequest {
                request_id: [64; 32],
                user_public_key: session.user_public_key,
                role: MemberRole::Editor,
            },
            now_ns(),
        )
        .unwrap();
    let saving = command(&owner, &session, save_user_files());
    assert!(owner.commit_desktop_command(&saving).is_err());
}

#[test]
fn an_administrator_opening_an_app_gives_it_authorship() {
    let (_temp, mut owner, service, session) = hosted_with_session();
    let authors = |owner: &SessionOwner| {
        room_capabilities(owner)
            .into_iter()
            .filter(|cap| cap.subject == collab_types::caps::Subject::app("so.tilo.notes"))
            .collect::<Vec<_>>()
    };
    let open = |owner: &mut SessionOwner| {
        let opening = command(
            owner,
            &session,
            DesktopCommand::OpenApp {
                app: "so.tilo.notes".into(),
            },
        );
        owner
            .commit_desktop_command(&opening)
            .unwrap()
            .validate_for(&opening)
            .unwrap();
    };
    open(&mut owner);
    assert!(authors(&owner).is_empty(), "an editor grants nothing");

    make_administrator(&mut owner, &service, &session);
    open(&mut owner);
    let granted = authors(&owner);
    assert_eq!(granted.len(), 1, "{granted:?}");
    assert_eq!(granted[0].scope, collab_types::caps::Scope::Workspace);
    assert_eq!(granted[0].verb, collab_types::caps::Verb::Write);
    open(&mut owner);
    assert_eq!(authors(&owner).len(), 1, "granted once");
}

#[test]
fn a_member_records_its_effect_receipt_once() {
    let (_temp, mut owner, _service, session) = hosted_with_session();
    let invocation = tilo_actions::InvocationId::new();
    let receipt = |status| {
        serde_json::to_string(&tilo_desktop_model::WorkspaceEffectReceipt {
            invocation,
            actor: "agent:bench".into(),
            context: tilo_actions::ActionContext::for_resource(
                "Rename page",
                "page:1",
                "notes.page",
                "Page",
            ),
            status,
            completed_at: 1,
        })
        .unwrap()
    };
    let record = |owner: &mut SessionOwner, receipt_json: String| {
        let recording = command(
            owner,
            &session,
            DesktopCommand::RecordEffectReceipt { receipt_json },
        );
        owner.commit_desktop_command(&recording)
    };
    let succeeded = receipt(tilo_actions::EffectReceiptStatus::Succeeded);
    record(&mut owner, succeeded.clone()).unwrap();
    let receipts = tilo_desktop_model::projection::workspace_effect_receipts_of(
        owner.peer().admitted_room().unwrap(),
    );
    assert_eq!(
        receipts.iter().map(|r| r.invocation).collect::<Vec<_>>(),
        [invocation]
    );
    record(&mut owner, succeeded).unwrap();
    assert!(
        record(
            &mut owner,
            receipt(tilo_actions::EffectReceiptStatus::Cancelled)
        )
        .is_err()
    );
}
