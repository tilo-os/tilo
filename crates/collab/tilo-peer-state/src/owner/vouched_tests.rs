//! A workspace held on its host's word: installed from the membership answer,
//! admitting the replay and members' certificates, and kept across a reopen.
use super::*;
use crate::join::JoinTransportResponse;

struct Vouched {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: NativeWorkspaceOwner,
    workspace: WorkspaceId,
    now: SystemTime,
}

/// Hold the fixture's workspace on its membership answer and admit its replay.
fn vouched() -> Vouched {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let owner = open_file(&path).unwrap();
    let (_, bootstrap, messages, governor, now) = raw_fixture();
    let device = bootstrap.device_cert.clone();
    let workspace = bootstrap.workspace_id;
    let answer =
        postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(bootstrap)).unwrap();
    assert!(
        owner
            .install_vouched(workspace, &answer, &device, now)
            .unwrap()
    );
    assert!(
        !owner
            .install_vouched(workspace, &answer, &device, now)
            .unwrap(),
        "a workspace held under the same authority stays as it is"
    );
    let workspace = WorkspaceId::parse(WORKSPACE).unwrap();
    for message in messages {
        owner
            .receive(
                workspace,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                Some(&crate::owner::testing::member_key()),
                now,
            )
            .unwrap();
    }
    Vouched {
        _directory: directory,
        path,
        owner,
        workspace,
        now,
    }
}

#[test]
fn a_workspace_its_host_vouches_for_admits_its_replay_and_reopens() {
    let held = vouched();
    let calls = |owner: &NativeWorkspaceOwner| {
        owner
            .with_current(held.workspace, |view| {
                Ok(view
                    .peer()
                    .validated_media_policy()?
                    .ok_or("missing admitted policy")?
                    .calls
                    .len())
            })
            .unwrap()
    };
    assert_eq!(calls(&held.owner), 1);
    assert!(
        held.owner
            .with_current(held.workspace, |view| view
                .enrollment_invitation()
                .map(drop))
            .is_err(),
        "a vouched workspace has no enrollment of its own"
    );
    drop(held.owner);
    let reopened = open_file(&held.path).unwrap();
    assert_eq!(reopened.workspaces(), vec![held.workspace]);
    assert_eq!(calls(&reopened), 1);
}

#[test]
fn a_membership_answer_for_another_device_or_workspace_is_refused() {
    let (_, bootstrap, _, _, now) = raw_fixture();
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
    let workspace = bootstrap.workspace_id;
    let stranger = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[71; 32]),
        &SigningKey::from_bytes(&[72; 32]),
        "Stranger",
    );
    let answer = postcard::to_stdvec(&JoinTransportResponse::MembershipComplete(
        bootstrap.clone(),
    ))
    .unwrap();
    assert!(
        owner
            .install_vouched(workspace, &answer, &stranger, now)
            .is_err()
    );
    assert!(
        owner
            .install_vouched(workspace ^ 1, &answer, &bootstrap.device_cert, now)
            .is_err()
    );
    let error = JoinTransportResponse::Error {
        code: 403,
        message: "no".into(),
    };
    assert!(
        owner
            .install_vouched(
                workspace,
                &postcard::to_stdvec(&error).unwrap(),
                &bootstrap.device_cert,
                now
            )
            .is_err()
    );
    assert!(owner.workspaces().is_empty());
}

#[test]
fn a_member_introduces_itself_and_an_unlisted_device_does_not() {
    let held = vouched();
    let hello = |cert: &tilo_user_identity::DeviceCert| NeighbourMsg::IdentityHello {
        protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
        cert: Box::new(cert.clone()),
        revocations: Vec::new(),
    };
    let receive = |cert: &tilo_user_identity::DeviceCert| {
        held.owner.receive(
            held.workspace,
            TransportOrigin::RelayDelta(cert.device_public_key),
            hello(cert),
            Some(&crate::owner::testing::member_key()),
            held.now,
        )
    };
    let member = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[64; 32]),
        &crate::owner::testing::member_key(),
        "Member",
    );
    receive(&member).unwrap();
    let unlisted = crate::owner::testing::certificate(
        &SigningKey::from_bytes(&[64; 32]),
        &SigningKey::from_bytes(&[70; 32]),
        "Second",
    );
    assert!(
        receive(&unlisted).is_err(),
        "the workspace's roster lists who it admits"
    );
}

/// A durable receipt is the workspace host's assertion: its generation counts the
/// workspace's commits, and it names the exact coverage that generation made
/// durable. A member's catch-up publishes its snapshots alone — each device's own
/// generation is a private count, so two devices stamping the same workspace's
/// receipts would leave every member with a set no other member can merge, and
/// their link would end refusing the other's state.
#[test]
fn a_members_catch_up_leaves_the_workspaces_durable_receipts_to_its_host() {
    let held = vouched();
    let receipts = |held: &Vouched| {
        held.owner
            .with_current(held.workspace, |view| {
                let workspace = view
                    .peer()
                    .admitted_workspace()
                    .ok_or("the workspace's governance is missing")?;
                crate::durability::entries(workspace)
            })
            .expect("the workspace's durable receipts")
    };
    let before = receipts(&held);
    assert!(
        !before.is_empty(),
        "the workspace's host published its receipts"
    );
    let generation = held
        .owner
        .with_current(held.workspace, |view| Ok(view.generation()))
        .expect("the workspace's record");
    let messages = held
        .owner
        .commit_bootstrap(
            held.workspace,
            &crate::owner::testing::member_key(),
            held.now,
        )
        .expect("the member publishes its snapshots");
    assert!(
        !messages.is_empty(),
        "the member's catch-up carries its snapshots"
    );
    assert_eq!(
        receipts(&held),
        before,
        "a member's catch-up leaves the workspace's durable receipts to its host"
    );
    assert!(
        held.owner
            .with_current(held.workspace, |view| Ok(view.generation()))
            .expect("the workspace's record")
            > generation,
        "the member's catch-up is still its own commit"
    );
}
