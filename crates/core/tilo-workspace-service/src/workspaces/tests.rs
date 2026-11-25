#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use super::*;
use crate::service::{ServiceConfig, ServiceIdentity, WorkspaceService};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

pub(crate) use super::fixtures::{
    device_cert, entry_join, governor, governor_with, restricted_until, signed_pre_admission,
    test_host, test_host_with_grants,
};

pub(crate) mod admission_cost;
mod catch_up;
#[cfg(feature = "agents")]
mod host_address;
mod invitation_read;
pub(crate) mod large_document;
mod link_windows;
#[cfg(feature = "agents")]
mod lookup;
mod naming;
mod one_copy_two_writers;
mod replacing;

#[tokio::test]
async fn a_pre_admission_for_one_person_issues_an_invitation_only_they_can_redeem() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = PreAdmitRequest {
        request_id: [3; 32],
        grant: signed_pre_admission(&created.workspace, b"one-time"),
        recipient_user_public_key: Some(guest),
        inviter_name: Some("Plaza".into()),
    };
    let issued = workspaces.pre_admit(&cert, request.clone()).await.unwrap();
    // The same request again is a retry, not a second invitation.
    assert_eq!(workspaces.pre_admit(&cert, request).await.unwrap(), issued);
    let pending = workspaces.pending(&created.workspace, &cert).await.unwrap();
    let invitation = pending
        .iter()
        .find(|pending| pending.invite_id == issued.invite_id)
        .unwrap();
    assert_eq!(invitation.recipient_user_public_key, Some(guest));
    server.shutdown().await;
}

pub(crate) use super::fixtures::guest_device;

#[tokio::test]
async fn a_crowd_of_pre_admitted_guests_signs_its_own_way_in_and_a_bad_token_is_refused_alone() {
    use tilo_server_protocol::workspaces::JoinReply;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Public").await.unwrap();
    let mut redemptions = Vec::new();
    let mut people = Vec::new();
    for index in 0..6_u8 {
        let token = [index + 1; 8];
        let issued = workspaces
            .pre_admit(
                &cert,
                PreAdmitRequest {
                    request_id: [index + 1; 32],
                    grant: signed_pre_admission(&created.workspace, &token),
                    recipient_user_public_key: None,
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let (device_key, device_cert) = guest_device();
        people.push(device_cert.user_public_key);
        // The last guest presents someone else's token.
        let token = if index == 5 { [1; 8] } else { token };
        redemptions.push((
            issued.signed_invitation,
            (device_key, device_cert),
            format!("Guest {index}"),
            token,
        ));
    }
    // The changes this host's own device made to the workspace.
    let own_changes = || async {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let governed = state.lock().await;
        let own = governed.peer().workspace_actor_for(&cert.device_public_key);
        let mut held = governed.peer().admitted_workspace().unwrap().clone();
        held.get_changes(&[])
            .iter()
            .filter(|change| change.actor_id() == &own)
            .count()
    };
    let before = own_changes().await;
    let answers = n0_future::join_all(redemptions.into_iter().map(
        |(invitation, (device_key, device_cert), name, token)| {
            let workspaces = workspaces.clone();
            async move {
                workspaces
                    .admit_proven(
                        invitation,
                        (device_cert, &device_key),
                        rand::random(),
                        &name,
                        Some(token.as_slice()),
                    )
                    .await
            }
        },
    ))
    .await;
    for answer in answers.iter().take(5) {
        assert!(matches!(answer, JoinReply::Admitted { .. }), "{answer:?}");
    }
    assert!(
        matches!(answers.last(), Some(JoinReply::Refused(reason)) if reason.contains("token")),
        "{:?}",
        answers.last()
    );
    assert_eq!(
        own_changes().await,
        before,
        "each guest's device signs its own admission: this host signs none of them"
    );
    let summary = workspaces.summary(&created.workspace).await.unwrap();
    let members: Vec<_> = summary
        .members
        .iter()
        .filter_map(tilo_peer_state::Member::root)
        .collect();
    for person in people.iter().take(5) {
        assert!(members.contains(person));
    }
    assert!(!members.contains(&people[5]));
    // Each is named as they named themself coming in, in that same change.
    for (index, person) in people.iter().take(5).enumerate() {
        let member = summary
            .members
            .iter()
            .find(|member| member.root() == Some(*person))
            .unwrap();
        assert_eq!(member.name, format!("Guest {index}"));
    }
    server.shutdown().await;
}

/// Concurrent pre-admissions share their workspace's writes, and every one of
/// them is written before it is answered: after a restart each invitation
/// still admits its guest.
#[tokio::test]
async fn a_crowds_shared_writes_hold_each_invitation_grant_across_a_restart() {
    use tilo_server_protocol::workspaces::JoinReply;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Public").await.unwrap();
    let issued = n0_future::join_all((0..8_u8).map(|index| {
        let workspaces = workspaces.clone();
        let workspace = created.workspace.clone();
        let cert = cert.clone();
        async move {
            workspaces
                .pre_admit(
                    &cert,
                    PreAdmitRequest {
                        request_id: [index + 1; 32],
                        grant: signed_pre_admission(&workspace, &[index + 1; 8]),
                        recipient_user_public_key: None,
                        inviter_name: None,
                    },
                )
                .await
                .unwrap()
        }
    }))
    .await;
    let guests: Vec<_> = issued.iter().map(|_| guest_device()).collect();
    server.shutdown().await;
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    for (index, (issued, (device_key, device_cert))) in issued.iter().zip(&guests).enumerate() {
        let token = u8::try_from(index + 1).unwrap();
        let answer = workspaces
            .admit_proven(
                issued.signed_invitation.clone(),
                (device_cert.clone(), device_key),
                rand::random(),
                &format!("Guest {index}"),
                Some(&[token; 8][..]),
            )
            .await;
        assert!(matches!(answer, JoinReply::Admitted { .. }), "{answer:?}");
    }
    server.shutdown().await;
}

/// An entry cut off between committing a device's admission and settling it
/// (the page that asked closed meanwhile) leaves governance holding the
/// device while the server decides its changes against what devices may do, without it.
/// The device's next entry finds it admitted already, and settles what
/// governance holds before answering, so its changes are decided with it.
#[tokio::test]
async fn an_entry_cut_off_before_its_settle_is_settled_by_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let page_key = SigningKey::from_bytes(&[9; 32]);
    let page = {
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut page = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: page_key.verifying_key().to_bytes(),
            device_label: "browser".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut page, &root).unwrap();
        page
    };
    // The first entry commits the admission and stops there.
    {
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        let mut governed = governed.lock().await;
        let now = SystemTime::now();
        let mut candidate = governed.peer().clone();
        assert!(
            candidate
                .governance_mut()
                .admit_member_device(&page, &page_key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .keep(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
            .unwrap();
    }
    assert!(
        !workspaces
            .decides_for(created.workspace_id, page.device_public_key)
            .await,
        "the cut-off entry left the device out of the device_access changes are decided against"
    );

    assert!(
        !workspaces
            .enter(
                &created.workspace,
                (&page, &SigningKey::from_bytes(&[9; 32]))
            )
            .await
            .unwrap(),
        "the device is admitted already"
    );
    assert!(
        workspaces
            .decides_for(created.workspace_id, page.device_public_key)
            .await,
        "the next entry settled the admission"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn every_settled_decision_is_announced_with_its_workspace() {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, Role};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let mut changes = workspaces.governance_changes();
    let deny_outside = Grant::new(
        "public-guests-no-outside",
        Grantee::role(Role::Editor),
        tilo_action_policy::TreePath::workspace(&created.workspace),
        KindSet::OUTSIDE,
        Answer::Deny,
        Grantee::person(hex::encode(cert.user_public_key)),
    );
    workspaces
        .put_workspace_grants(&created.workspace, &cert, [4; 32], vec![deny_outside])
        .await
        .unwrap();
    let announced = tokio::time::timeout(std::time::Duration::from_secs(5), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(announced, created.workspace);
    assert_eq!(
        workspaces
            .summary(&created.workspace)
            .await
            .unwrap()
            .workspace,
        created.workspace
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_view_reader_refused(
    workspaces: &Workspaces,
    workspace: WorkspaceId,
    caller: &tilo_actions::remote::Caller,
    source: &tilo_peer_state::document_frame::DocumentRoute,
) {
    assert!(
        workspaces
            .resource_view_policy(workspace, caller, &source.app_id, &source.document)
            .await
            .is_none()
    );
    assert_eq!(
        workspaces
            .resource_view_document(workspace, caller, source)
            .await
            .err()
            .unwrap(),
        "resource read not permitted",
        "a currently unauthorized caller must be refused before existing-key access"
    );
}

/// Publish `revoked` while a reader of `source` waits on the governed workspace
/// lock: the reader must recheck admission behind that same lock and be refused.
#[cfg(feature = "agents")]
async fn publish_a_revocation_with_a_reader_waiting(
    workspaces: &Workspaces,
    workspace: &str,
    workspace_id: WorkspaceId,
    caller: &tilo_actions::remote::Caller,
    source: &tilo_peer_state::document_frame::DocumentRoute,
    revoked: tilo_user_identity::RevocationEntry,
    key: &SigningKey,
) {
    let queued_read = workspaces.resource_view_document(workspace_id, caller, source);
    tokio::pin!(queued_read);
    let state = workspaces.workspace(workspace).await.unwrap();
    let mut workspace = state.lock().await;
    // Poll the actual reader to Pending while the governed workspace lock is held.
    // It must recheck admission after acquiring this same lock.
    assert!(
        std::future::poll_fn(|cx| {
            use std::future::Future;
            std::task::Poll::Ready(queued_read.as_mut().poll(cx).is_pending())
        })
        .await
    );
    let now = SystemTime::now();
    let mut candidate = workspace.peer().clone();
    candidate
        .governance_mut()
        .publish_governed_revocations(&[revoked], key, now)
        .unwrap();
    let collaboration = workspace.collaboration().clone();
    workspace
        .keep(candidate, key, now, |_, _| Ok(collaboration))
        .unwrap();
    drop(workspace);
    drop(state);
    assert_eq!(
        queued_read.await.err().unwrap(),
        "resource read not permitted"
    );
}

#[cfg(feature = "agents")]
#[tokio::test]
async fn view_read_policy_requires_current_certified_person_and_observes_revocation() {
    use tilo_actions::remote::Caller;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces
        .create("Read-only resource discovery")
        .await
        .unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .enter(&created.workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    let caller = Caller::authenticated(member.user_public_key, member.device_public_key);
    let policy = workspaces
        .resource_view_policy(created.workspace_id, &caller, "so.tilo.chat", "")
        .await
        .unwrap();
    assert_eq!(policy.workspace, created.workspace);
    assert_eq!(
        policy
            .access
            .policy
            .role_of(policy.access.person_id().unwrap_or_default()),
        Some(collab_types::Role::Owner)
    );
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        document: "@tilo:package-primary".into(),
    };
    assert_eq!(
        workspaces
            .resource_view_document(created.workspace_id, &caller, &source)
            .await
            .err()
            .unwrap(),
        "source channel keys unavailable",
        "the authorized reader reaches the existing-key boundary without creating channel state"
    );
    assert_eq!(
        workspaces
            .resource_view_document(WorkspaceId::from_u128(0), &caller, &source)
            .await
            .err()
            .unwrap(),
        "source workspace mismatch"
    );
    for denied in [
        Caller::authenticated([9; 32], cert.device_public_key),
        Caller::authenticated(cert.user_public_key, [9; 32]),
        Caller::local_person(hex::encode(cert.user_public_key)),
        Caller::local_agent(
            "agent-with-no-delegation",
            hex::encode(cert.user_public_key).clone(),
        ),
        Caller::local_app("so.tilo.chat"),
    ] {
        assert_view_reader_refused(&workspaces, created.workspace_id, &denied, &source).await;
    }
    assert!(
        workspaces
            .resource_view_policy(WorkspaceId::from_u128(0), &caller, "so.tilo.chat", "")
            .await
            .is_none()
    );
    let identity_key = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: cert.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &identity_key).unwrap();
    publish_a_revocation_with_a_reader_waiting(
        &workspaces,
        &created.workspace,
        created.workspace_id,
        &caller,
        &source,
        revoked,
        &key,
    )
    .await;
    assert_view_reader_refused(&workspaces, created.workspace_id, &caller, &source).await;
    drop(workspaces);
    server.shutdown().await;
}

/// What a workspace's records service answers an agent with follows from who
/// the workspace admits: a member's agent, asking as the member on their
/// device, reads the workspace; a stranger and a revoked device read nothing.
#[cfg(feature = "agents")]
#[tokio::test]
async fn admits_a_members_agent_never_a_stranger_and_never_a_revoked_device() {
    use tilo_actions::remote::Caller;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("A workspace's people").await.unwrap();
    let member_key = SigningKey::from_bytes(&[5; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .enter(&created.workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    // The member's agent asks as the member, on the member's own device.
    let agent = Caller::authenticated(member.user_public_key, member.device_public_key);
    assert!(
        workspaces.admits(created.workspace_id, &agent).await,
        "a member's agent reads the workspace its person is in"
    );
    for stranger in [
        Caller::authenticated([9; 32], member.device_public_key),
        Caller::authenticated(member.user_public_key, [9; 32]),
        Caller::local_agent("agent-with-no-delegation", "ada"),
        Caller::local_app("so.tilo.chat"),
    ] {
        assert!(
            !workspaces.admits(created.workspace_id, &stranger).await,
            "{:?} is nobody this workspace admits",
            stranger.acting()
        );
    }
    assert!(
        !workspaces.admits(WorkspaceId::from_u128(0), &agent).await,
        "a workspace this host does not govern admits nobody"
    );
    // Revoking the device ends it, though the workspace still lists its
    // certificate: the authority a member holds here is checked, not the list.
    let identity_key = SigningKey::from_bytes(&[1; 32]);
    let mut revoked = tilo_user_identity::RevocationEntry {
        user_public_key: member.user_public_key,
        revoked_device_public_key: member.device_public_key,
        revoked_at_ns: tilo_clock::unix_now_ns(),
        reason: tilo_user_identity::RevocationReason::Compromised,
        signature: [0; 64],
    };
    tilo_user_identity::sign_revocation_entry(&mut revoked, &identity_key).unwrap();
    let state = workspaces.workspace(&created.workspace).await.unwrap();
    {
        let mut workspace = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        candidate
            .governance_mut()
            .publish_governed_revocations(&[revoked], &key, now)
            .unwrap();
        let collaboration = workspace.collaboration().clone();
        workspace
            .keep(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    drop(state);
    assert!(
        !workspaces.admits(created.workspace_id, &agent).await,
        "a revoked device reads nothing, its certificate in the list or not"
    );
    drop(workspaces);
    server.shutdown().await;
}

#[test]
fn a_created_workspace_restores_with_its_governance_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        Some(&crate::workspaces::fixtures::founding_device()),
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    // The founding is this device's own write, carried once it is kept.
    assert!(matches!(
        created.unsent.as_slice(),
        [neighbour_wire::NeighbourMsg::SyncResp { changes, .. }] if !changes.is_empty()
    ));
    let restored = GovernedWorkspace::open(created.path.clone(), None).unwrap();
    assert_eq!(restored.record.workspace, created.record.workspace);
    assert_eq!(
        restored.peer().governance().generation(),
        created.peer().governance().generation()
    );
    assert_eq!(restored.summary().name, "Team");
    assert!(restored.peer().has_governance());
}

/// The peer state the workspace file at `path` names, read back from its parts.
fn kept_peer_state(path: &std::path::Path) -> (document_format::StoredResource, Vec<u8>) {
    let bytes = std::fs::read(path).unwrap();
    let stored: document_format::StoredResource = postcard::from_bytes(&bytes).unwrap();
    let peer = peer_parts::PeerParts::open(path)
        .unwrap()
        .load(&stored.peer)
        .unwrap();
    (stored, peer)
}

/// A workspace an earlier build kept with its peer state in an older durable layout opens
/// with everything it held, and its file names the state in today's layout from then on.
#[test]
fn a_peer_state_of_an_older_layout_opens_and_is_written_again_in_todays() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        Some(&crate::workspaces::fixtures::founding_device()),
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    let path = created.path.clone();
    drop(created);
    // The same state as the version before kept it: what an earlier build wrote.
    let (mut stored, peer) = kept_peer_state(&path);
    let peer = kept_before(&peer);
    assert_eq!(
        postcard::take_from_bytes::<u32>(&peer).unwrap().0,
        tilo_peer_state::DURABLE_VERSION - 1
    );
    stored.peer = vec![peer_parts::PeerPart::Inline(peer)];
    std::fs::write(&path, postcard::to_stdvec(&stored).unwrap()).unwrap();

    let restored = GovernedWorkspace::open(path.clone(), None).unwrap();
    assert_eq!(restored.summary().name, "Team");
    assert!(restored.peer().has_governance());
    drop(restored);
    let (_, peer) = kept_peer_state(&path);
    assert_eq!(
        postcard::take_from_bytes::<u32>(&peer).unwrap().0,
        tilo_peer_state::DURABLE_VERSION,
        "the file names the state in today's layout"
    );
    let reopened = GovernedWorkspace::open(path, None).unwrap();
    assert_eq!(reopened.summary().name, "Team");
}

/// `bytes`, a durable state, as the version before this build's kept it: its layout is the same.
fn kept_before(bytes: &[u8]) -> Vec<u8> {
    let mut bytes = bytes.to_vec();
    let today = u8::try_from(tilo_peer_state::DURABLE_VERSION).unwrap();
    assert_eq!(
        bytes.first(),
        Some(&today),
        "a state names its version first"
    );
    bytes[0] = today.saturating_sub(1);
    bytes
}

#[test]
fn a_workspace_name_must_be_printable_and_short() {
    let (key, cert) = governor();
    let longest = "x".repeat(80);
    let too_long = "x".repeat(81);
    let longest_wide = "\u{e9}".repeat(80);
    let too_long_wide = "\u{e9}".repeat(81);
    let padded = format!("  {longest}\n");
    // Each name, whether a workspace is founded with it, and what it is.
    let names: [(&str, bool, &str); 13] = [
        // A workspace nobody named is founded with no name: every reader
        // shows it in their own language.
        ("", true, "no name"),
        ("   ", true, "only spaces, which is no name"),
        ("x", true, "one character"),
        (&longest, true, "80 characters, the longest"),
        (&too_long, false, "81 characters"),
        (&longest_wide, true, "80 characters of two bytes each"),
        (&too_long_wide, false, "81 characters of two bytes each"),
        (&padded, true, "80 characters inside space that is trimmed"),
        ("\tTeam\n", true, "a tab and a newline around a name"),
        ("a\u{7}b", false, "a bell inside"),
        ("a\nb", false, "a newline inside"),
        ("a\u{7f}b", false, "a delete inside"),
        ("a\u{85}b", false, "a next-line control inside"),
    ];
    for (name, founded, what) in names {
        let dir = tempfile::tempdir().unwrap();
        let created = GovernedWorkspace::create(
            dir.path(),
            name,
            &cert,
            &key,
            Some(&crate::workspaces::fixtures::founding_device()),
            (&[], &[]),
            SystemTime::now(),
        );
        assert_eq!(
            created.is_ok(),
            founded,
            "{what} ({name:?}): {:?}",
            created.err()
        );
    }
}

/// The workspace's copy of `record` of `app`, when it has one.
pub(crate) async fn is_banned(
    workspaces: &Workspaces,
    workspace: &str,
    host: &DeviceCert,
    person: [u8; 32],
) -> bool {
    let state = workspaces.workspace(workspace).await.unwrap();
    let permissions = state
        .lock()
        .await
        .peer()
        .permissions(host.device_public_key, SystemTime::now())
        .unwrap();
    permissions.policy.is_banned(&hex::encode(person))
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn a_renamed_workspace_shows_its_new_name_to_members_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("plaza").await.unwrap();
    let before = created.generation;
    let renamed = workspaces
        .rename(&created.workspace, " Plaza ")
        .await
        .unwrap();
    assert_eq!(renamed.name, "Plaza");
    assert!(
        renamed.generation > before,
        "the rename is a governance change"
    );
    // Every reader of the name on the host reads the governance's one name.
    let listed = workspaces.list().await;
    let listed = listed.iter().find(|s| s.workspace == created.workspace);
    assert_eq!(listed.unwrap().name, "Plaza", "the host's list");
    let hosted = workspaces.workspace_name(created.workspace_id).await;
    assert_eq!(hosted.as_deref(), Some("Plaza"), "a hosted window's");
    let again = workspaces
        .rename(&created.workspace, "Plaza")
        .await
        .unwrap();
    assert_eq!(
        again.generation, renamed.generation,
        "the same name changes nothing"
    );
    assert!(workspaces.rename(&created.workspace, " ").await.is_err());
    // Each name a workspace cannot take is refused, and changes nothing.
    let too_long = "x".repeat(81);
    for (name, what) in [
        ("", "no name"),
        (too_long.as_str(), "81 characters"),
        ("a\u{7}b", "a bell inside"),
        ("a\nb", "a newline inside"),
    ] {
        let refused = workspaces.rename(&created.workspace, name).await;
        assert!(refused.is_err(), "{what} ({name:?}) renamed the workspace");
    }
    let kept = workspaces
        .rename(&created.workspace, "Plaza")
        .await
        .unwrap();
    assert_eq!(kept.name, "Plaza");
    assert_eq!(
        kept.generation, renamed.generation,
        "a refused name changes nothing"
    );
    let path = {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let governed = state.lock().await;
        assert_eq!(
            governed.peer().governance().workspace_name().as_deref(),
            Some("Plaza")
        );
        governed.path.clone()
    };
    drop(workspaces);
    server.shutdown().await;
    let restored = GovernedWorkspace::open(path, None).unwrap();
    assert_eq!(restored.summary().name, "Plaza");
    assert_eq!(
        restored.peer().governance().workspace_name().as_deref(),
        Some("Plaza")
    );
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays live until its consuming shutdown"
)]
async fn the_owner_restricts_until_a_time_and_lifts_it() {
    use tilo_server_protocol::workspaces::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = |request_id| RemoveMemberRequest {
        request_id,
        user_public_key: guest,
    };
    let until = tilo_clock::unix_now_ms() + 30 * 60_000;
    workspaces
        .restrict_member(&created.workspace, &cert, request([1; 32]), until)
        .await
        .unwrap();
    assert_eq!(
        restricted_until(workspaces, &created.workspace, &cert, guest).await,
        Some(until)
    );
    assert!(
        !is_banned(workspaces, &created.workspace, &cert, guest).await,
        "a restriction is not a ban"
    );
    workspaces
        .unrestrict_member(&created.workspace, &cert, request([2; 32]))
        .await
        .unwrap();
    assert_eq!(
        restricted_until(workspaces, &created.workspace, &cert, guest).await,
        None
    );
    let owner = RemoveMemberRequest {
        request_id: [3; 32],
        user_public_key: cert.user_public_key,
    };
    assert!(
        workspaces
            .restrict_member(&created.workspace, &cert, owner, until)
            .await
            .is_err(),
        "the owner is never restricted"
    );
    server.shutdown().await;
}

/// A server certified with `capabilities`, a workspace it governs, and one of
/// the workspace's member devices, with the channel route that member asks for.
struct AskedForKeys {
    /// The server's state.
    state: tempfile::TempDir,
    server: crate::workspaces::fixtures::TestHost,
    workspaces: Workspaces,
    workspace: String,
    channel: collab_types::WorkspaceId,
}

/// Start a server certified with `capabilities`, create a workspace on it, admit
/// a member device of the person, and hand the workspace that device's MLS
/// access request for the workspace's group, as its runtime sends one.
async fn a_member_asks_for_a_channel(capabilities: DeviceCapabilities) -> AskedForKeys {
    a_member_asks_for_a_channel_through(capabilities, super::Carried::Unkept).await
}

/// [`a_member_asks_for_a_channel`], the request reaching the host as `carried`
/// says.
async fn a_member_asks_for_a_channel_through(
    capabilities: DeviceCapabilities,
    carried: super::Carried,
) -> AskedForKeys {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(capabilities);
    let server = test_host(&dir.path().join("server"), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Sealed").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .enter(&created.workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    let channel = collab_types::WorkspaceId::parse(&created.workspace).unwrap();
    let vault = key_group::VaultKey::new([3; 32]);
    let member_dir = dir.path().join("member");
    std::fs::create_dir_all(&member_dir).unwrap();
    let request = key_group::GroupPeer::waiting(
        Some(std::sync::Arc::new(key_group::Files::new(&member_dir))),
        channel,
        Box::new(member_key.clone()),
        &vault,
    )
    .unwrap()
    .access_request_now(1, false, &vault)
    .unwrap()
    .expect("a member waiting for the channel asks for it");
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::MlsAccessRequest {
        workspace: created.workspace.clone(),
        request: Box::new(request),
        delivery: 1,
    })
    .unwrap();
    workspaces
        .absorb_carried(&created.workspace, member.device_public_key, &body, carried)
        .await
        .unwrap();
    AskedForKeys {
        state: dir,
        server,
        workspaces,
        workspace: created.workspace,
        channel,
    }
}

impl AskedForKeys {
    /// Whether the workspace kept the member's request among the records it
    /// retains for the devices that take part in its channels.
    async fn retains_the_request(&self) -> bool {
        let workspace_id = WorkspaceId::parse(&self.workspace).unwrap();
        self.server
            .workspace_documents()
            .retained(workspace_id).await
            .is_ok_and(|retained| {
                retained.iter().any(|retained| {
                    postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body).is_ok_and(
                        |message| {
                            matches!(
                                message,
                                neighbour_wire::NeighbourMsg::MlsAccessRequest {
                                    workspace,
                                    request,
                                    ..
                                } if workspace == self.workspace && request.workspace == self.channel
                            )
                        },
                    )
                })
            })
    }

    /// Shut the server down, then remove its state.
    async fn shut_down(self) {
        let Self {
            state,
            server,
            workspaces,
            ..
        } = self;
        drop(workspaces);
        server.shutdown().await;
        drop(state);
    }
}

/// A host keeps what the workspace's channels are made of like any other
/// governance, for whoever takes part: a member's request for a channel's group
/// is kept for the devices that link later.
#[tokio::test]
async fn a_host_keeps_what_its_members_send_for_a_channels_group() {
    let asked = a_member_asks_for_a_channel(DeviceCapabilities::member()).await;
    assert!(
        asked.retains_the_request().await,
        "the workspace retained the member's request for its channels"
    );
    asked.shut_down().await;
}

/// A server another person keeps is named among a workspace's holders as its store:
/// what it sends for a channel's group is refused like anything a device the
/// workspace does not govern sends, and no key of the workspace is handed to it,
/// so the frames it keeps open nowhere on it.
#[tokio::test]
async fn another_persons_server_keeps_the_ciphertext_and_is_refused_a_channels_keys() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(DeviceCapabilities::member());
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Sealed").await.unwrap();
    let workspace_id = WorkspaceId::parse(&created.workspace).unwrap();

    // The other person's server, named to keep the workspace's ciphertext alone.
    let site = SigningKey::from_bytes(&[9; 32]);
    let (site_key, site_cert) = device_cert(&site, "site", DeviceCapabilities::member());
    let site_dir = tempfile::tempdir().unwrap();
    let site_host = test_host(site_dir.path(), &site_key, &site_cert).await;
    let hint = site_host
        .workspaces()
        .host_address(workspace_id, tilo_peer_state::CopyRole::Store)
        .unwrap();
    assert_eq!(
        tilo_peer_state::holder_role(&hint),
        tilo_peer_state::CopyRole::Store
    );
    workspaces
        .name_host(&created.workspace, |governance, key, now| {
            governance.author_keeping_device(&hint, key, now)
        })
        .await
        .unwrap();

    // Its request for the workspace's channel is kept for no one: it is no device
    // of a person the workspace has as a member.
    let channel = collab_types::WorkspaceId::parse(&created.workspace).unwrap();
    let request =
        collab_types::MlsAccessRequestV1::signed(channel, vec![1, 2, 3], 1, &site_key).unwrap();
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::MlsAccessRequest {
        workspace: created.workspace.clone(),
        request: Box::new(request),
        delivery: 1,
    })
    .unwrap();
    workspaces
        .absorb_carried(
            &created.workspace,
            site_cert.device_public_key,
            &body,
            super::Carried::Unkept,
        )
        .await
        .unwrap();
    let kept = server
        .workspace_documents()
        .retained(workspace_id)
        .await
        .is_ok_and(|retained| {
            retained.iter().any(|retained| {
                postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body).is_ok_and(
                    |message| {
                        matches!(
                            message,
                            neighbour_wire::NeighbourMsg::MlsAccessRequest { .. }
                        )
                    },
                )
            })
        });
    assert!(
        !kept,
        "the host kept another person's server's request for a channel"
    );

    // It keeps the workspace's ciphertext, which is sealed with a group's keys: it is
    // handed none of them, so the frames stored there open nowhere on it.
    let route = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "test.app".into(),
        cluster_id: created.workspace.clone(),
        document: "notes".into(),
    };
    assert!(
        matches!(workspaces.own_route_keys(&route, true).await, Some(Ok(_))),
        "the workspace seals its records with a group's keys"
    );
    assert!(
        !workspaces
            .key_holders(&created.workspace)
            .await
            .unwrap()
            .contains(&site_cert.device_public_key),
        "another person's server was handed the workspace's keys"
    );
    assert!(
        site_host
            .workspaces()
            .own_route_keys(&route, false)
            .await
            .is_none(),
        "the other person's server takes part in no key group of the workspace"
    );

    site_host.shutdown().await;
    server.shutdown().await;
}

/// A member's request that reached the host over the member's server session
/// is kept there already, as the member's own write: the host keeps no copy of
/// its own. Each request a crowd sent was kept twice, a second job on the
/// workspace's one-at-a-time queue for every one.
#[tokio::test]
async fn a_request_the_server_kept_is_not_kept_again_as_the_hosts() {
    let asked =
        a_member_asks_for_a_channel_through(DeviceCapabilities::member(), super::Carried::Kept)
            .await;
    assert!(
        !asked.retains_the_request().await,
        "the host kept a second copy of a request its server keeps as the member's"
    );
    asked.shut_down().await;
}

/// Every decision is kept when it is answered, and each change this host's device made for it
/// is carried, as its device signed it, before the answer: nothing it wrote waits to be sent,
/// a host that stops right after the last answer starts with all of them, and a pre-admission
/// among them takes the workspace as it stands.
#[tokio::test]
async fn every_decision_is_kept_and_carried_once_it_is_answered() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Public").await.unwrap();
    // Each name is one governance change.
    for index in 0..48 {
        tilo_test_env::within(
            format!("rename {index}"),
            workspaces.rename(&created.workspace, &format!("Public {index}")),
        )
        .await
        .unwrap();
    }
    tilo_test_env::within(
        "a pre-admission among the renames",
        workspaces.pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [9; 32],
                grant: signed_pre_admission(&created.workspace, &[9; 8]),
                recipient_user_public_key: None,
                inviter_name: None,
            },
        ),
    )
    .await
    .unwrap();
    {
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        let governed = governed.lock().await;
        assert_eq!(governed.summary().name, "Public 47");
        assert!(
            governed.unsent.is_empty(),
            "every change this device made is carried once answered: {} wait",
            governed.unsent.len()
        );
        let own = governed.peer().workspace_actor_for(&cert.device_public_key);
        let mut held = governed.peer().admitted_workspace().unwrap().clone();
        let written = held
            .get_changes(&[])
            .iter()
            .filter(|change| change.actor_id() == &own)
            .count();
        assert!(written >= 48, "{written} changes are this device's own");
    }
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    assert_eq!(
        server
            .workspaces()
            .summary(&created.workspace)
            .await
            .unwrap()
            .name,
        "Public 47",
        "the host starts again with every decision it answered"
    );
    server.shutdown().await;
}

/// A Complete whose challenge this host does not hold, and whose request it
/// never completed, is answered as lapsed: the device learns its retained
/// proof is dead and asks for a new challenge, instead of resending it to a
/// refusal forever.
#[tokio::test]
async fn a_proof_whose_challenge_the_host_no_longer_holds_is_answered_as_lapsed() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JOIN_PROOF_LAPSED, JoinChallengeRequest, JoinCompleteRequest, JoinTransportRequest,
        JoinTransportResponse, sign_join_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let (device, device_cert) = guest_device();
    let issued = workspaces
        .pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [9; 32],
                grant: signed_pre_admission(&created.workspace, b"entry"),
                recipient_user_public_key: Some(device_cert.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let service = workspaces.join_service();
    let endpoint = device_cert.device_public_key;
    let JoinTransportResponse::Challenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::Challenge(JoinChallengeRequest {
                signed_invitation: issued.signed_invitation,
                device_cert,
                request_id: [4; 32],
            }),
        )
        .await
    else {
        panic!("no challenge");
    };
    // A challenge the host never issued, as one it has since dropped looks.
    let mut dropped = answer.challenge;
    dropped.nonce = [0x5a; 32];
    let proof = sign_join_proof(dropped, &device).unwrap();
    let response = service
        .handle(
            endpoint,
            JoinTransportRequest::Complete(JoinCompleteRequest {
                proof,
                admission: Vec::new(),
            }),
        )
        .await;
    assert!(
        matches!(
            response,
            JoinTransportResponse::Error {
                code: JOIN_PROOF_LAPSED,
                ..
            }
        ),
        "{response:?}"
    );
    server.shutdown().await;
}

/// `windows`, the layout of `workspace` as `member`'s device minting in
/// `salt` writes it onto `doc`: what it made is its person's.
fn write_windows(
    doc: &mut automerge::AutoCommit,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
    windows: &[tilo_desktop_model::Strip],
) {
    let identity = tilo_desktop_model::WorkspaceId::parse(workspace).unwrap();
    let mut workspace = tilo_desktop_model::Workspace::new(identity, "Workspace");
    if let Some(held) = tilo_desktop_model::shared_resource::read(&*doc, None) {
        workspace.strips = held
            .tree
            .workspaces
            .into_iter()
            .flat_map(|held| held.strips)
            .collect();
    }
    workspace.strips.extend(windows.iter().cloned());
    let person = hex::encode(member.user_public_key);
    let mine = |id: u64| tilo_desktop_model::peer_salt_of(id) == salt;
    tilo_desktop_model::shared_resource::device::write(
        doc,
        identity,
        &workspace,
        &tilo_desktop_model::shared_resource::device::Writer {
            person: Some(&person),
            mine: &mine,
            records: tilo_desktop_model::shared_resource::device::Writer::NOTHING_NOTED,
            items: tilo_desktop_model::shared_resource::device::Writer::NOTHING_ITEMED,
        },
    )
    .unwrap();
}

/// The window titles `doc`'s layout holds.
fn titles_in(doc: &automerge::AutoCommit) -> Vec<String> {
    tilo_desktop_model::shared_resource::read(doc, None)
        .map(|desktop| {
            desktop
                .tree
                .workspaces
                .into_iter()
                .flat_map(|w| w.strips)
                .flat_map(|s| s.window_groups)
                .flat_map(|g| g.tabs)
                .map(|tab| tab.title)
                .collect()
        })
        .unwrap_or_default()
}

/// One window, a Chat tab, on one strip, with ids in the namespace `salt`.
fn one_chat_window(salt: u16) -> [tilo_desktop_model::Strip; 1] {
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    [Strip {
        id: StripId(own(1)),
        name: "Main".into(),
        window_groups: vec![WindowGroup {
            id: own(2),
            tabs: vec![Tab {
                id: own(3),
                title: "Chat".into(),
                tile: TileContent::Foreign {
                    app_id: "so.tilo.chat".into(),
                    surface_id: 7,
                },
                app_title: None,
                view: String::new(),
                pinned: false,
            }],
            active_tab: own(3),
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        }],
    }]
}

/// A window a member opens after joining is a change to the workspace's
/// layout, made by the member's person. The governor takes it in as the workspace's next
/// generation, so every other member sees the window; before, only record
/// writes were taken in and the window reached nobody.
#[tokio::test]
async fn a_window_a_member_opens_becomes_the_workspaces_next_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    workspaces
        .enter(&created.workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    let salt = 0x3333;
    let windows = one_chat_window(salt);
    let mut doc = {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let workspace = state.lock().await;
        workspace
            .peer()
            .admitted_workspace()
            .expect("a created workspace has a workspace document")
            .clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    write_windows(&mut doc, &created.workspace, &member, salt, &windows);
    doc.commit();
    let changes = doc.get_changes(&heads);
    assert!(
        !changes.is_empty(),
        "opening a window changes the workspace"
    );
    let hashes = changes
        .iter()
        .map(automerge::Change::hash)
        .collect::<Vec<_>>();
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        changes: changes
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
        workspace: created.workspace.clone(),
    })
    .unwrap();

    workspaces
        .absorb(&created.workspace, member.device_public_key, &body)
        .await
        .unwrap();

    {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let workspace = state.lock().await;
        let admitted = workspace.peer().admitted_workspace().unwrap().clone();
        drop(workspace);
        for hash in &hashes {
            assert!(
                admitted.has_change(hash),
                "the governed workspace holds the member's window"
            );
        }
    }
    drop(workspaces);
    server.shutdown().await;
}

/// A device of the owner's that is not this host removes a member itself,
/// in its own replica, and publishes the change: the host takes it in as
/// the workspace's next generation, and every invitation still waiting for the
/// person goes with them.
#[tokio::test]
async fn a_removal_another_device_writes_is_taken_in_with_the_persons_invitations() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let (writer, salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let writer_key = SigningKey::from_bytes(&[3; 32]);
    let (guest_key, guest) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&guest_key, &guest), 4).await;
    workspaces
        .pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [5; 32],
                grant: signed_pre_admission(&workspace, b"again"),
                recipient_user_public_key: Some(guest.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let waiting = |pending: Vec<PendingInvitation>| {
        pending
            .iter()
            .any(|pending| pending.recipient_user_public_key == Some(guest.user_public_key))
    };
    assert!(waiting(
        workspaces.pending(&workspace, &cert).await.unwrap()
    ));

    let body = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let governed = state.lock().await;
        let mut written = governed.peer().clone();
        let heads = written.workspace_heads().unwrap();
        written
            .governance_mut()
            .remove_member(guest.user_public_key, &writer_key, SystemTime::now())
            .unwrap();
        let changes = written.workspace_changes_since(&heads);
        drop(governed);
        postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
            changes,
            workspace: workspace.clone(),
        })
        .unwrap()
    };
    workspaces
        .absorb(&workspace, writer.device_public_key, &body)
        .await
        .unwrap();
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .all(|member| member.root() != Some(guest.user_public_key) || member.removed),
        "{members:?}"
    );
    assert!(
        !waiting(workspaces.pending(&workspace, &cert).await.unwrap()),
        "the removed person's invitation went with them"
    );

    let _windows = terminals_opened(&workspaces, &workspace, &writer, salt).await;
    drop(workspaces);
    server.shutdown().await;
}

/// A device of the governor's person, certified as a member and admitted to
/// `workspace`, and the id namespace it mints in.
pub(crate) async fn admitted_member(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    seed: u8,
) -> (DeviceCert, u16) {
    let (member_key, member) = further_device(cert, seed);
    workspaces
        .enter(workspace, (&member, &member_key))
        .await
        .expect("the person's further device lists itself");
    (member, u16::from(seed) << 8 | 0x11)
}

/// A further device of `cert`'s person, keyed by `seed`, which no workspace
/// lists yet.
pub(crate) fn further_device(cert: &DeviceCert, seed: u8) -> (SigningKey, DeviceCert) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    (member_key, member)
}

/// `member`'s change to `workspace`'s document opening two unconfined terminals
/// in its namespace `salt`, as its device would send it.
async fn terminals_opened(
    workspaces: &Workspaces,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
) -> Vec<u8> {
    use tilo_desktop_model::{Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth};
    let own = |counter: u64| (u64::from(salt) << 48) | counter;
    let terminal = |counter: u64, title: &str| WindowGroup {
        id: own(counter),
        tabs: vec![Tab {
            id: own(counter),
            title: title.into(),
            tile: TileContent::Foreign {
                app_id: "Alacritty".into(),
                surface_id: counter,
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: own(counter),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let windows = [Strip {
        id: StripId(own(1)),
        name: "Main".into(),
        window_groups: vec![terminal(2, "ssh prod-db"), terminal(3, "~/private/taxes")],
    }];
    let mut doc = {
        let state = workspaces.workspace(workspace).await.unwrap();
        let governed = state.lock().await;
        governed.peer().admitted_workspace().unwrap().clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    write_windows(&mut doc, workspace, member, salt, &windows);
    doc.commit();
    postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        workspace: workspace.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap()
}

/// The window titles the governed workspace holds: what every member is given.
async fn titles_shared(workspaces: &Workspaces, workspace: &str) -> Vec<String> {
    let state = workspaces.workspace(workspace).await.unwrap();
    let governed = state.lock().await;
    titles_in(governed.peer().admitted_workspace().unwrap())
}

/// A member's windows are the workspace's: the governor takes the change in, and
/// every member sees the windows.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the server stays held through the assertions; Server::shutdown consumes it"
)]
#[tokio::test]
async fn the_windows_a_member_opens_are_shared_by_the_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Team").await.unwrap().workspace;
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let body = terminals_opened(&workspaces, &workspace, &a, a_salt).await;
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    let mut titles = titles_shared(&workspaces, &workspace).await;
    titles.sort();
    assert_eq!(titles, ["ssh prod-db", "~/private/taxes"]);
    drop(workspaces);
    server.shutdown().await;
}

/// `member`'s change to `workspace`'s layout closing every window it made
/// in its namespace `salt`, as its device would send it.
async fn windows_closed(
    workspaces: &Workspaces,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
) -> Vec<u8> {
    let mut doc = {
        let state = workspaces.workspace(workspace).await.unwrap();
        let governed = state.lock().await;
        governed.peer().admitted_workspace().unwrap().clone()
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    tilo_desktop_model::shared_resource::device::withdraw(&mut doc, &|id| {
        tilo_desktop_model::peer_salt_of(id) == salt
    })
    .unwrap();
    doc.commit();
    postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        workspace: workspace.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap()
}

/// Whether `title` occurs in `frame`, a frame a device's replay delivers:
/// in its bytes as encoded for the wire, or in any operation of a workspace
/// document change it carries once decoded (a change's columns may be
/// compressed, which a search of the raw bytes alone would miss).
fn replayed_frame_carries(frame: &neighbour_wire::NeighbourMsg, title: &str) -> bool {
    let needle = title.as_bytes();
    let wire = postcard::to_allocvec(frame).unwrap();
    if wire.windows(needle.len()).any(|window| window == needle) {
        return true;
    }
    let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = frame else {
        return false;
    };
    changes.iter().any(|bytes| {
        automerge::Change::try_from(bytes.as_slice())
            .is_ok_and(|change| format!("{:?}", change.decode()).contains(title))
    })
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the server stays held through the assertions; Server::shutdown consumes it"
)]
#[tokio::test]
async fn a_member_admitted_later_is_replayed_no_window_withdrawn_before() {
    const TITLES: [&str; 2] = ["ssh prod-db", "~/private/taxes"];
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Team").await.unwrap();
    let workspace = created.workspace.clone();
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let body = terminals_opened(&workspaces, &workspace, &a, a_salt).await;
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(
        titles_shared(&workspaces, &workspace).await.len(),
        2,
        "the workspace shares A's terminals while they are open"
    );
    let body = windows_closed(&workspaces, &workspace, &a, a_salt).await;
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert!(
        titles_shared(&workspaces, &workspace).await.is_empty(),
        "the workspace's current state no longer holds A's terminals"
    );

    let (c, c_cert) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&c, &c_cert), 21).await;
    // What a device joining now is sent: a workspace's history reaches a device
    // over its link, whose holder sends the workspace and a signed checkpoint of
    // each of its records, as the link's catch-up builds them.
    let replayed = {
        let governed = workspaces.workspace(&workspace).await.unwrap();
        let governed = governed.lock().await;
        let mut holder = governed.peer().clone();
        holder
            .publication_mut()
            .bootstrap_messages(&key, SystemTime::now())
            .unwrap()
    };
    assert!(
        !replayed.is_empty(),
        "a device joining the workspace is replayed the workspace"
    );
    for title in TITLES {
        assert!(
            !replayed
                .iter()
                .any(|frame| replayed_frame_carries(frame, title)),
            "a member who joins later is replayed {title:?}, \
             a window withdrawn before they joined"
        );
    }
    drop(workspaces);
    server.shutdown().await;
}

/// A workspace becomes public in the directory: its layout stays what it
/// was. Open admission is not a decision about the layout: the workspace keeps
/// sharing its members' windows and strips across a restart, exactly as before
/// it was listed.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the server stays held through the assertions; Server::shutdown consumes it"
)]
#[tokio::test]
async fn a_listed_workspace_keeps_sharing_its_windows_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &cert, 3).await;
    let body = terminals_opened(&workspaces, &workspace, &a, a_salt).await;
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(
        titles_shared(&workspaces, &workspace).await.len(),
        2,
        "committed while the server still took members' windows in"
    );
    // Listed in the directory: anyone may come in as an editor.
    workspaces
        .publish(
            &workspace,
            &cert,
            tilo_server_protocol::directory::Listing {
                description: "Come in".into(),
                default_role: tilo_server_protocol::workspaces::Role::Editor,
            },
        )
        .await
        .unwrap();
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    assert_eq!(
        titles_shared(&workspaces, &workspace).await.len(),
        2,
        "the workspace still holds its members' windows"
    );
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(titles_shared(&workspaces, &workspace).await.len(), 2);
    drop(workspaces);
    server.shutdown().await;
}

/// The Lounge, 2026-10-20: a laptop's join completed at the host, but the
/// laptop restarted before it installed the answer. By the time it resent
/// its retained proof, others had joined: the host no longer replays the
/// answer, says so with its code, and gives the device, a member now, the
/// workspace's current state through the membership exchange, which matches
/// what the device's retained proof expects.
#[tokio::test]
async fn a_join_the_host_completed_before_the_workspace_moved_on_is_finished_through_membership() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JOIN_COMPLETED_REFRESH, JoinTransportRequest, JoinTransportResponse,
        MembershipChallengeRequest, MembershipCompleteRequest, sign_membership_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let (laptop, laptop_cert) = guest_device();
    let (complete, first) = entry_join(
        workspaces,
        &cert,
        &created.workspace,
        (&laptop, &laptop_cert),
        11,
    )
    .await;
    let JoinTransportResponse::Complete(delivered) = first else {
        panic!("the first Complete is admitted: {first:?}");
    };
    // The answer never reached the laptop. Someone else joins meanwhile.
    let (other, other_cert) = guest_device();
    let (_, joined) = entry_join(
        workspaces,
        &cert,
        &created.workspace,
        (&other, &other_cert),
        12,
    )
    .await;
    assert!(matches!(joined, JoinTransportResponse::Complete(_)));

    let service = workspaces.join_service();
    let endpoint = laptop_cert.device_public_key;
    let resent = service.handle(endpoint, complete).await;
    assert!(
        matches!(
            resent,
            JoinTransportResponse::Error {
                code: JOIN_COMPLETED_REFRESH,
                ..
            }
        ),
        "{resent:?}"
    );
    let JoinTransportResponse::MembershipChallenge(answer) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                workspace_id: created.workspace_id,
                device_cert: laptop_cert.clone(),
                request_id: [13; 32],
                delegation: None,
            }),
        )
        .await
    else {
        panic!("no membership challenge");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop)
        .await
        .unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest {
                proof,
                admission: Vec::new(),
            }),
        )
        .await
    else {
        panic!("no membership");
    };
    // What the device's retained proof checks the answer against.
    assert_eq!(current.version, 1);
    assert_eq!(current.invite_id, delivered.invite_id);
    assert_eq!(current.device_cert, laptop_cert);
    assert_eq!(current.role, delivered.role);
    assert_eq!(current.workspace_id, delivered.workspace_id);
    assert_eq!(current.host, delivered.host);
    assert!(current.generation > 0);
    assert!(current.certified_roster.contains(&laptop_cert));
    assert!(current.certified_roster.contains(&other_cert));
    assert!(current.certified_roster.contains(&current.host));
    server.shutdown().await;
}

/// A workspace that moves to its person's host keeps its workspace: the host
/// takes over governing the workspace under the identity it already has, and
/// refuses a workspace it holds already, rather than minting another.
#[tokio::test]
async fn a_workspace_moved_here_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let workspace = "6f1c2b9e-4d3a-4f5e-8a7b-1c2d3e4f5a6b";
    let moved = workspaces
        .create_workspace("Mine", Some(workspace))
        .await
        .unwrap();
    assert_eq!(
        moved.workspace, workspace,
        "the workspace keeps its identity"
    );
    assert_eq!(
        moved.workspace_id,
        WorkspaceId::parse(workspace).unwrap(),
        "and its workspace the id every device derives from it"
    );
    assert!(
        workspaces
            .create_workspace("Again", Some(workspace))
            .await
            .is_err(),
        "a workspace the host holds is never made a second time"
    );
    assert!(
        workspaces
            .create_workspace("Bad", Some("not a workspace"))
            .await
            .is_err()
    );
    let fresh = workspaces.create_workspace("Fresh", None).await.unwrap();
    assert_ne!(fresh.workspace, workspace);
    server.shutdown().await;
}

/// A workspace made on a device no registry has answered yet — fresh from its install, or offline
/// at first use — starts with the default apps its package cache already holds, each recorded as
/// the release the cache has: its version, package root and publisher.
#[tokio::test]
async fn a_workspace_made_with_no_catalogue_starts_with_the_defaults_the_cache_holds() {
    let dir = tempfile::tempdir().unwrap();
    let store = dir.path().join("apps");
    let config = dir.path().join("store-config");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::create_dir_all(&config).unwrap();
    let app = app_fleet::default_apps()[0];
    let (root, publisher) = ("9a".repeat(32), "2b".repeat(32));
    std::fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{app}\"\nversion = \"3.1.4\"\n\
             package_id_hex = \"{root}\"\npublisher_key_hex = \"{publisher}\"\n\
             payload_root = \"{}\"\nmanifest_path = \"{}\"\n",
            store.join("payloads").join(&root).display(),
            store
                .join("pkgs")
                .join(&root)
                .join("manifest.toml")
                .display(),
        ),
    )
    .unwrap();
    let mut env = tilo_test_env::ScopedEnv::new();
    // The cache is this test's whole store, and no registry is configured to answer.
    env.remove("TILO_APPS_SYSTEM_DIR")
        .set("TILO_APPS_DATA_DIR", &store)
        .set("TILO_STORE_CONFIG_DIR", &config)
        .set("TILO_WORKSPACE_REGISTRIES", r#"{"registries": []}"#);
    let (key, cert) = governor();
    let server = test_host(&dir.path().join("host"), &key, &cert).await;
    let made = server
        .workspaces()
        .create_workspace("First", None)
        .await
        .unwrap();
    let running = server
        .workspaces()
        .app_versions(&made.workspace)
        .await
        .unwrap();
    let release = running
        .iter()
        .map(|installed| &installed.release)
        .find(|release| release.app == app)
        .unwrap_or_else(|| panic!("the default {app} the cache holds is recorded: {running:?}"));
    assert_eq!(release.version, "3.1.4");
    assert_eq!(release.package, root);
    assert_eq!(release.publisher, publisher);
    assert_eq!(release.file, None, "the workspace keeps no file of it");
    assert_eq!(
        running.len(),
        1,
        "only what the cache holds is recorded with no catalogue: {running:?}"
    );
    server.shutdown().await;
}

/// A member whose person's root renewed its device certificate proves its
/// membership with the renewal: the server holds the renewal in place of the
/// certificate it replaces, answers the device as the same member with its
/// role, and refuses the older certificate from then on.
#[tokio::test]
async fn a_member_with_a_renewed_certificate_stays_the_same_member() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JoinTransportRequest, JoinTransportResponse, MembershipChallengeRequest,
        MembershipCompleteRequest, sign_membership_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let root = SigningKey::from_bytes(&[71; 32]);
    let laptop = SigningKey::from_bytes(&[72; 32]);
    let certify = |issued_at_ns: u64, expires_at_ns: Option<u64>| {
        let mut cert = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: laptop.verifying_key().to_bytes(),
            device_label: "laptop".into(),
            capabilities: DeviceCapabilities::member(),
            issued_at_ns,
            expires_at_ns,
            signature: [0; 64],
            issuer: None,
        };
        sign_device_cert(&mut cert, &root).unwrap();
        cert
    };
    let held = certify(1, None);
    let (_, first) = entry_join(workspaces, &cert, &created.workspace, (&laptop, &held), 21).await;
    let JoinTransportResponse::Complete(delivered) = first else {
        panic!("the join is admitted: {first:?}");
    };
    let now = tilo_clock::unix_now_ns();
    let renewed = certify(
        now.saturating_sub(1_000_000_000),
        Some(now.saturating_add(3_600_000_000_000)),
    );

    let service = workspaces.join_service();
    let endpoint = laptop.verifying_key().to_bytes();
    let challenge = |device_cert: DeviceCert, seed: u8| {
        service.handle(
            endpoint,
            JoinTransportRequest::MembershipChallenge(MembershipChallengeRequest {
                workspace_id: created.workspace_id,
                device_cert,
                request_id: [seed; 32],
                delegation: None,
            }),
        )
    };
    let JoinTransportResponse::MembershipChallenge(answer) = challenge(renewed.clone(), 22).await
    else {
        panic!("the renewal is challenged as the device it renews");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop)
        .await
        .unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest {
                proof,
                admission: Vec::new(),
            }),
        )
        .await
    else {
        panic!("the renewed member is answered");
    };
    assert_eq!(current.device_cert, renewed);
    assert_eq!(
        current.role, delivered.role,
        "the same member, with its role"
    );
    assert!(current.certified_roster.contains(&renewed));
    assert!(!current.certified_roster.contains(&held));
    assert!(
        matches!(
            challenge(held, 23).await,
            JoinTransportResponse::Error { .. }
        ),
        "the older certificate is refused after the renewal"
    );
    server.shutdown().await;
}

/// `workspace`'s history as it stands: the generation of the base it starts from, how many
/// changes follow that base, and the bytes of the base and of the changes after it.
struct Shape {
    generation: u64,
    entries: usize,
    base_bytes: usize,
    tail_bytes: usize,
    /// The bytes of the heaviest change after the base.
    heaviest: usize,
}

async fn history_shape(workspaces: &Workspaces, workspace: &str) -> Shape {
    use tilo_peer_state::workspace::base;
    let governed = workspaces.workspace(workspace).await.unwrap();
    let mut doc = governed
        .lock()
        .await
        .peer()
        .admitted_workspace()
        .unwrap()
        .clone();
    let root = base::root(&mut doc);
    let base_bytes = root
        .and_then(|root| doc.get_change_by_hash(&root))
        .map_or(0, |change| change.raw_bytes().len());
    let past = doc.get_changes(root.as_slice());
    Shape {
        generation: base::generation(&doc),
        entries: past.len(),
        base_bytes,
        tail_bytes: past.iter().map(|change| change.raw_bytes().len()).sum(),
        heaviest: past
            .iter()
            .map(|change| change.raw_bytes().len())
            .max()
            .unwrap_or(0),
    }
}

/// A crowd arriving through the entry costs the workspace one change each, added
/// to its history, not the whole workspace rebuilt, written and sent to every
/// member once per arrival. A change that takes something back starts the
/// workspace over at the next arrival, with a compaction its owner's device
/// signs, so a newcomer is never given what was withdrawn before they came.
/// Otherwise the workspace starts over from a fresh base only once the changes
/// since its last one weigh half as much as that base (and no less than a
/// floor), and the base holds its state in one compressed change: what a
/// newcomer fetches stays near the size of the workspace as it stands, not of
/// its history.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays held through the assertions; Server::shutdown consumes it"
)]
#[tokio::test]
async fn a_crowd_joining_adds_to_the_workspace_rather_than_starting_it_over() {
    const FLOOR: usize = 64 * 1024;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let workspace = created.workspace.clone();

    // The first arrival starts the workspace over only if its founding took
    // something back; arrivals after it add to the history.
    let mut arrivals = Vec::new();
    let (first, first_cert) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&first, &first_cert), 1).await;
    arrivals.push(first_cert);
    let shape = history_shape(&workspaces, &workspace).await;
    let (generation, mut entries) = (shape.generation, shape.entries);
    for seed in 2..=8 {
        let (guest, guest_cert) = guest_device();
        entry_join(&workspaces, &cert, &workspace, (&guest, &guest_cert), seed).await;
        arrivals.push(guest_cert);
        let shape = history_shape(&workspaces, &workspace).await;
        assert_eq!(
            shape.generation, generation,
            "arrival {seed} after only arrivals adds to the history"
        );
        assert!(shape.entries > entries, "arrival {seed} added its change");
        entries = shape.entries;
    }

    // A change that takes something back (the workspace's name) is added, and the
    // next arrival starts over.
    workspaces.rename(&workspace, "Lounge II").await.unwrap();
    let renamed = history_shape(&workspaces, &workspace).await;
    assert!(renamed.generation == generation && renamed.entries > entries);
    let (late, late_cert) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&late, &late_cert), 30).await;
    arrivals.push(late_cert);
    let mut before = history_shape(&workspaces, &workspace).await;
    assert!(
        before.generation > generation,
        "an arrival after a change that took something back starts the workspace over"
    );

    // Arrivals go on until the changes since the base weigh half of it.
    let mut started = None;
    for seed in 31..=250 {
        let (guest, guest_cert) = guest_device();
        entry_join(&workspaces, &cert, &workspace, (&guest, &guest_cert), seed).await;
        arrivals.push(guest_cert);
        let shape = history_shape(&workspaces, &workspace).await;
        if shape.generation > before.generation {
            started = Some(before);
            break;
        }
        before = shape;
    }
    let before = started.expect("the workspace starts over once its changes weigh half its base");
    assert!(
        before.tail_bytes + 2 * before.heaviest >= (before.base_bytes / 2).max(FLOOR),
        "the workspace started over before its changes weighed half its base: {} B of changes on a \
         {} B base",
        before.tail_bytes,
        before.base_bytes
    );
    // The base is the compaction its owner's device signed, and a newcomer is sent it.
    the_base_is_the_owners_signed_compaction(&workspaces, &workspace, cert.user_public_key).await;

    // Every arrival is a member.
    let members = workspaces.summary(&workspace).await.unwrap().members;
    for device in &arrivals {
        assert!(
            members
                .iter()
                .any(|member| member.root() == Some(device.user_public_key)),
            "an arrival is a member of the workspace"
        );
    }
    drop(workspaces);
    server.shutdown().await;
}

/// The workspace started over from a base that is the compaction its owner's device signed,
/// and a newcomer is sent that base as one compressed change.
async fn the_base_is_the_owners_signed_compaction(
    workspaces: &Workspaces,
    workspace: &str,
    owner: [u8; 32],
) {
    let Ok(governed) = workspaces.workspace(workspace).await else {
        unreachable!("the workspace is held here");
    };
    let mut doc = {
        let admitted = governed.lock().await;
        let peer = admitted.peer();
        let Some(doc) = peer.admitted_workspace() else {
            unreachable!("the workspace has a base");
        };
        doc.clone()
    };
    let Some(root) = tilo_peer_state::workspace::base::root(&mut doc) else {
        unreachable!("the workspace has a base");
    };
    assert_eq!(
        tilo_peer_state::workspace::compaction::compacted_by(&doc, workspace, &owner),
        Ok(owner),
        "the base is the compaction the owner's device signed"
    );
    let message = governed.lock().await.peer().workspace_message();
    let neighbour_wire::NeighbourMsg::SyncResp { changes, .. } = message else {
        unreachable!("the workspace as a newcomer is sent it");
    };
    let Some(state) = changes.iter().find(|bytes| {
        automerge::Change::try_from(bytes.as_slice()).is_ok_and(|change| change.hash() == root)
    }) else {
        unreachable!("a newcomer is sent the base");
    };
    assert_eq!(
        state.get(8),
        Some(&2),
        "the base's state is sent as a compressed change chunk"
    );
}

/// A host that renamed its own governor certificate — the same device key,
/// the same capabilities, a new label — serves the workspaces that
/// certificate already governs: a name is not what governs a workspace, and
/// refusing it made a host renamed by an upgrade unable to start.
#[tokio::test]
async fn a_host_serves_the_workspaces_its_renamed_certificate_governs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let created = server.workspaces().create("Demo").await.unwrap();
    server.shutdown().await;

    // The same device and capabilities, under another name, signed again by
    // the workspace's owner root as a server signs its own certificate.
    let mut renamed = tilo_user_identity::DeviceCert {
        device_label: "the same host, renamed".into(),
        ..cert.clone()
    };
    // Signed by the person's root, as the fixture's certificate was.
    let root = ed25519_dalek::SigningKey::from_bytes(&[1; 32]);
    tilo_user_identity::sign_device_cert(&mut renamed, &root).unwrap();
    let reopened = test_host(dir.path(), &key, &renamed).await;
    let names: Vec<String> = reopened
        .workspaces()
        .list()
        .await
        .into_iter()
        .map(|workspace| workspace.name)
        .collect();
    assert_eq!(names, vec![created.name]);
    reopened.shutdown().await;

    // Another device opening the same files keeps the workspace's owner:
    // the record names who owns it, and the device reading it takes no
    // part in that.
    let other_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let other_key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let mut other_cert = tilo_user_identity::DeviceCert {
        user_public_key: other_root.verifying_key().to_bytes(),
        device_public_key: other_key.verifying_key().to_bytes(),
        device_label: "another host".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut other_cert, &other_root).unwrap();
    let other = test_host(dir.path(), &other_key, &other_cert).await;
    let summary = other
        .workspaces()
        .summary(&created.workspace)
        .await
        .expect("another host serves the workspace from its files");
    let roots: Vec<Option<[u8; 32]>> = summary
        .members
        .iter()
        .map(collab_types::Member::root)
        .collect();
    assert_eq!(
        roots,
        vec![Some(cert.user_public_key)],
        "the owner stays the person who made it"
    );
    other.shutdown().await;
}

/// A workspace names its own host among its hosts, as its governor, so a member
/// reads where the workspace is served off the replica it already holds, and reads
/// it once.
#[tokio::test]
async fn a_workspace_names_its_own_host_to_every_member() {
    use std::time::Duration;
    use tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = tokio::time::timeout(Duration::from_secs(120), test_host(dir.path(), &key, &cert))
        .await
        .expect("the host opens");
    let addr = server.node_addr();
    let workspaces = server.workspaces();
    let created = tokio::time::timeout(Duration::from_secs(120), workspaces.create("Demo"))
        .await
        .expect("the workspace is created")
        .unwrap();
    let governed = tokio::time::timeout(
        Duration::from_secs(120),
        workspaces.workspace(&created.workspace),
    )
    .await
    .expect("the workspace opens")
    .unwrap();
    let hosts = {
        let governed = governed.lock().await;
        governed.peer().governance().keeping_devices()
    };
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    assert_eq!(hosts[0].host.device_public_key, cert.device_public_key);
    assert_eq!(
        WorkspaceDeviceHintV1::from_transport_hint(&hosts[0])
            .unwrap()
            .endpoint_addr,
        addr
    );
    let governors = tokio::time::timeout(
        Duration::from_secs(120),
        workspaces.hosts(&created.workspace),
    )
    .await
    .expect("the workspace names its hosts")
    .unwrap();
    assert_eq!(governors.len(), 1, "the governor is named once");
    server.shutdown().await;
}

/// Membership as the workspace's signed log makes it, on the server that hosts
/// it: a new workspace holds its owner alone, a person who joins through the
/// entry is a member, one removed is gone and comes back through a new
/// invitation, and a member's copy of the workspace reaches the membership
/// the server holds.
#[tokio::test]
async fn people_join_leave_and_rejoin_through_the_workspaces_log() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let workspace = workspaces.create("Plans").await.unwrap().workspace;
    let live = |members: &[tilo_peer_state::Member], person: [u8; 32]| {
        members
            .iter()
            .any(|member| member.root() == Some(person) && !member.removed)
    };
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        members
            .iter()
            .all(|member| member.root() == Some(cert.user_public_key) || member.removed),
        "a new workspace is its owner's alone: {members:?}"
    );

    let (guest_key, guest) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&guest_key, &guest), 31).await;
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(live(&members, guest.user_public_key), "{members:?}");

    // A member's copy, sent the workspace as it stands, holds the same people.
    let (server_policy, copy_policy) = {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let governed = state.lock().await;
        let held = governed.peer().admitted_workspace().unwrap().clone();
        let mut copy = automerge::AutoCommit::new();
        tilo_automerge::ApplyIncoming::apply_incoming(&mut copy, held.clone().get_changes(&[]))
            .unwrap();
        (
            tilo_peer_state::workspace::policy_of(&held, &workspace),
            tilo_peer_state::workspace::policy_of(&copy, &workspace),
        )
    };
    assert_eq!(server_policy, copy_policy);
    assert!(
        server_policy
            .members
            .contains_key(&hex::encode(guest.user_public_key))
    );

    workspaces
        .remove_member(
            &workspace,
            &cert,
            RemoveMemberRequest {
                request_id: [32; 32],
                user_public_key: guest.user_public_key,
            },
        )
        .await
        .unwrap();
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(!live(&members, guest.user_public_key), "{members:?}");

    entry_join(&workspaces, &cert, &workspace, (&guest_key, &guest), 33).await;
    let members = workspaces.summary(&workspace).await.unwrap().members;
    assert!(
        live(&members, guest.user_public_key),
        "a new invitation admits them again: {members:?}"
    );
    drop(workspaces);
    server.shutdown().await;
}

/// The member list `workspace_id`'s runtime on this host admits by.
async fn admitted(workspaces: &Workspaces, workspace: &str) -> tilo_peer_state::GovernedAccess {
    let id = workspaces
        .workspace(workspace)
        .await
        .unwrap()
        .lock()
        .await
        .workspace_id();
    let runtime = workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .expect("the host serves the workspace");
    let runtime = runtime.lock().await;
    runtime.members.as_ref().clone()
}

/// A removal is answered once the host admits by the list without the person,
/// and a change the signed member list committed is durable before anything
/// admits by it: a host stopped right after the commit starts again admitting
/// exactly what that list says.
#[tokio::test]
async fn a_restarted_host_admits_exactly_what_the_member_list_committed() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Plans").await.unwrap();
    let (guest_key, guest) = guest_device();
    entry_join(
        &workspaces,
        &cert,
        &created.workspace,
        (&guest_key, &guest),
        42,
    )
    .await;
    assert!(
        admitted(&workspaces, &created.workspace)
            .await
            .member_devices
            .contains(&guest.device_public_key),
        "a join is answered once the host admits the device"
    );
    workspaces
        .remove_member(
            &created.workspace,
            &cert,
            RemoveMemberRequest {
                request_id: [43; 32],
                user_public_key: guest.user_public_key,
            },
        )
        .await
        .unwrap();
    assert!(
        !admitted(&workspaces, &created.workspace)
            .await
            .member_devices
            .contains(&guest.device_public_key),
        "a removal is answered once the host no longer admits the device"
    );
    // Another device of the owner, as a browser page signed in as them is: the
    // member list commits it, and the host stops before admitting by it.
    let (page_key, page) = device_cert(
        &SigningKey::from_bytes(&[1; 32]),
        "browser",
        DeviceCapabilities::member(),
    );
    {
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        let mut governed = governed.lock().await;
        let now = SystemTime::now();
        let mut candidate = governed.peer().clone();
        assert!(
            candidate
                .governance_mut()
                .admit_member_device(&page, &page_key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .keep(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
            .unwrap();
    }
    drop(workspaces);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let governed = {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let state = state.lock().await;
        state
            .peer()
            .governance()
            .governed_access(SystemTime::now())
            .unwrap()
    };
    assert!(governed.member_devices.contains(&page.device_public_key));
    assert!(!governed.member_devices.contains(&guest.device_public_key));
    let admitted = admitted(&workspaces, &created.workspace).await;
    let sorted = |mut devices: Vec<[u8; 32]>| {
        devices.sort_unstable();
        devices
    };
    assert_eq!(
        admitted.member_devices,
        sorted(governed.member_devices),
        "the restarted host admits the devices the member list has"
    );
    assert_eq!(
        admitted.member_user_roots,
        sorted(governed.member_user_roots),
        "and the people it has"
    );
    drop(workspaces);
    server.shutdown().await;
}
