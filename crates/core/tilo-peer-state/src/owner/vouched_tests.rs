//! A workspace held on its host's word: installed from the membership answer,
//! admitting the replay and members' certificates, and kept across a reopen.
use super::*;
use crate::join::JoinTransportResponse;

use crate::fixture::primary_cert;
struct Vouched {
    _directory: tempfile::TempDir,
    path: PathBuf,
    owner: DeviceWorkspaces,
    workspace: WorkspaceId,
    now: SystemTime,
}

/// Hold the fixture's workspace on its membership answer and admit its replay.
fn vouched() -> Vouched {
    let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
    let path = directory.path().join("joined-workspaces.json");
    let owner = open_file(&path).unwrap();
    let (_, bootstrap, messages, owner_copy, now) = raw_fixture();
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
                TransportOrigin::RetainedServerDelta(owner_copy),
                message,
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

tilo_test_env::scenario! {
    fn a_host_vouched_workspace_admits_only_its_own_members_answers;

    phase a_workspace_its_host_vouches_for_admits_its_replay_and_reopens;
    {
        let held = vouched();
        let basis = |owner: &DeviceWorkspaces| {
            owner
                .with_current(held.workspace, |view| {
                    view.peer()
                        .security_basis()
                        .ok_or("missing admitted workspace")?
                        .map_err(|error| error.to_string())
                })
                .unwrap()
        };
        let admitted = basis(&held.owner);
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
        assert_eq!(basis(&reopened), admitted);
    }

    phase a_membership_answer_for_another_device_or_workspace_is_refused;
    {
        let (_, bootstrap, _, _, now) = raw_fixture();
        let directory = tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap();
        let owner = open_file(&directory.path().join("joined-workspaces.json")).unwrap();
        let workspace = bootstrap.workspace_id;
        let stranger = primary_cert(
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
                .install_vouched(
                    WorkspaceId::from_u128(41),
                    &answer,
                    &bootstrap.device_cert,
                    now
                )
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

    phase a_member_introduces_itself_and_an_unlisted_device_does_not;
    {
        let held = vouched();
        let hello = |cert: &tilo_user_identity::DeviceCert| NeighbourMsg::IdentityHello {
            protocol: neighbour_wire::PEER_PROTOCOL_VERSION,
            cert: Box::new(cert.clone()),
            revocations: Vec::new(),
        };
        let receive = |cert: &tilo_user_identity::DeviceCert| {
            held.owner.receive(
                held.workspace,
                TransportOrigin::ServerDelta(cert.device_public_key),
                hello(cert),
                held.now,
            )
        };
        let member = primary_cert(
            &SigningKey::from_bytes(&[64; 32]),
            &crate::owner::testing::member_key(),
            "Member",
        );
        receive(&member).unwrap();
        let unlisted = primary_cert(
            &SigningKey::from_bytes(&[64; 32]),
            &SigningKey::from_bytes(&[70; 32]),
            "Second",
        );
        assert!(
            receive(&unlisted).is_err(),
            "the workspace's roster lists who it admits"
        );
    }
}
