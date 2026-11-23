#![expect(
    clippy::significant_drop_tightening,
    reason = "the server and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use super::*;
use crate::service::{ServiceConfig, ServiceIdentity, WorkspaceService};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

pub(crate) use super::fixtures::{
    admit_view_reader, device_cert, entry_join, governor, governor_with, restricted_until,
    signed_pre_admission, test_host, test_host_with_grants,
};

pub(crate) mod admission_cost;
#[cfg(feature = "agents")]
mod host_address;
mod invitation_read;
pub(crate) mod large_document;
mod link_windows;
#[cfg(feature = "agents")]
mod lookup;
mod one_copy_two_writers;
mod restart_parts;

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

/// A person's device certificate, as a guest's browser makes one.
pub(crate) fn guest_device() -> (SigningKey, DeviceCert) {
    device_cert(
        &SigningKey::from_bytes(&rand::random()),
        "guest",
        DeviceCapabilities::member(),
    )
}

#[tokio::test]
async fn a_crowd_of_pre_admitted_guests_joins_in_one_change_and_a_bad_token_is_refused_alone() {
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
        let (_, device_cert) = guest_device();
        people.push(device_cert.user_public_key);
        // The last guest presents someone else's token.
        let token = if index == 5 { [1; 8] } else { token };
        redemptions.push((
            issued.signed_invitation,
            device_cert,
            format!("Guest {index}"),
            token,
        ));
    }
    let before = workspaces
        .summary(&created.workspace)
        .await
        .unwrap()
        .generation;
    let answers = n0_future::join_all(redemptions.into_iter().map(
        |(invitation, device_cert, name, token)| {
            let workspaces = workspaces.clone();
            async move {
                workspaces
                    .admit_proven(
                        invitation,
                        device_cert,
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
    let summary = workspaces.summary(&created.workspace).await.unwrap();
    assert_eq!(summary.generation, before + 1, "one change admits them all");
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
    for (index, (issued, (_, device_cert))) in issued.iter().zip(&guests).enumerate() {
        let token = u8::try_from(index + 1).unwrap();
        let answer = workspaces
            .admit_proven(
                issued.signed_invitation.clone(),
                device_cert.clone(),
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
    let page = {
        let root = SigningKey::from_bytes(&[1; 32]);
        let mut page = DeviceCert {
            user_public_key: root.verifying_key().to_bytes(),
            device_public_key: SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
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
                .admit_member_device(&page, &workspaces.inner.host.key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .commit(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
            .unwrap();
    }
    assert!(
        !workspaces
            .decides_for(created.workspace_id, page.device_public_key)
            .await,
        "the cut-off entry left the device out of the device_access changes are decided against"
    );

    assert!(
        !workspaces.enter(&created.workspace, &page).await.unwrap(),
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
        .commit(candidate, key, now, |_, _| Ok(collaboration))
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
    admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
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
        Caller::local_agent("agent-with-no-delegation"),
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
    admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
    // The member's agent asks as the member, on the member's own device.
    let agent = Caller::authenticated(member.user_public_key, member.device_public_key);
    assert!(
        workspaces.admits(created.workspace_id, &agent).await,
        "a member's agent reads the workspace its person is in"
    );
    for stranger in [
        Caller::authenticated([9; 32], member.device_public_key),
        Caller::authenticated(member.user_public_key, [9; 32]),
        Caller::local_agent("agent-with-no-delegation"),
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
            .commit(candidate, &key, now, |_, _| Ok(collaboration))
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

/// The workspace's default record a host seeds is the one every member's first
/// window opens: it names the default id, or members refuse it.
#[cfg(feature = "agents")]
#[tokio::test]
async fn a_seeded_workspace_default_record_names_the_default_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Seeded default record").await.unwrap();
    let catalogue = tilo_agent_session::AppCatalogue::native();
    // An app whose windows each show a resource of their own: one that keeps
    // a workspace record (Scratchpad, Notes) seeds its package primary instead.
    let app = "so.tilo.word-processor";
    assert!(!catalogue.get(app).unwrap().keeps_workspace_resource());
    let record = collab_types::document::DocumentId::workspace_default()
        .as_str()
        .to_owned();
    assert!(
        workspaces
            .prepare_application(&created.workspace, &cert, &catalogue, app, &record)
            .await
            .unwrap()
    );
    let bytes = workspaces
        .resource_document(&created.workspace, app, &record)
        .await
        .unwrap()
        .expect("the workspace holds its default record");
    let doc = automerge::AutoCommit::load(&bytes).unwrap();
    let kind = catalogue.get(app).unwrap().resource_type.clone();
    assert_eq!(
        ::collaboration::resource::identity(&doc),
        Some(collab_types::document::DocumentCheckout::new(
            collab_types::document::DocumentId::workspace_default(),
            kind
        ))
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
#[tokio::test]
async fn an_app_record_authored_here_survives_its_changes_rolling_over() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces
        .create("Native authoring rollover")
        .await
        .unwrap();
    let catalogue = tilo_agent_session::AppCatalogue::native();
    workspaces
        .prepare_application(&created.workspace, &cert, &catalogue, "so.tilo.chat", "")
        .await
        .unwrap();
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        document: String::new(),
    };
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let due = || async {
        let current = workspaces.workspace(&created.workspace).await.unwrap();
        starts_over(&current.lock().await.record.outgoing)
    };
    for index in 1..=MAX_PUBLICATION_TAIL {
        if due().await {
            break;
        }
        let grant = tilo_action_policy::Grant::new(
            format!("rollover-fixture-{index}"),
            tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
            tilo_action_policy::TreePath::workspace(&created.workspace).app("so.tilo.chat"),
            tilo_action_policy::KindSet::READ,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
        );
        workspaces
            .put_workspace_grants(
                &created.workspace,
                &cert,
                {
                    let mut request = [0; 32];
                    request[..8].copy_from_slice(&index.to_le_bytes());
                    request
                },
                vec![grant],
            )
            .await
            .unwrap();
    }
    assert!(due().await, "real governance writes reach rollover");
    workspaces
        .author_application(
            &created.workspace,
            &cert,
            &catalogue,
            "so.tilo.chat",
            "",
            |doc| {
                doc.put(ROOT, "rollover_counter", 1_u64)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    let retained = workspaces
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .expect("the mutation crossing rollover must be immediately replayable");
    let doc = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert_eq!(
        doc.get(ROOT, "rollover_counter")
            .unwrap()
            .map(|(value, _)| value.as_u64()),
        Some(Some(1)),
        "rollover must publish the new change into the kept source"
    );
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
        None,
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    assert!(!created.record.outgoing.is_empty());
    assert!(created.record.outgoing.iter().all(|entry| entry.checkpoint));
    let restored = GovernedWorkspace::open(created.path.clone(), None).unwrap();
    assert_eq!(restored.record.workspace, created.record.workspace);
    assert_eq!(restored.record.generation, 1);
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
        None,
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    let path = created.path.clone();
    drop(created);
    // The same state as the version before kept it: what an earlier build wrote.
    let (mut stored, peer) = kept_peer_state(&path);
    let peer = tilo_peer_state::kept_before_own_removal(&peer).unwrap();
    assert_eq!(postcard::take_from_bytes::<u32>(&peer).unwrap().0, 11);
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

/// A document write as a build before this one published it: the member's own record it was,
/// `own`, before its reader set.
fn published_before_own_removal(
    write: &neighbour_wire::NeighbourMsg,
    own: Option<(&str, [u8; 32])>,
) -> Vec<u8> {
    let neighbour_wire::NeighbourMsg::AppDocument { readers, .. } = write else {
        panic!("a document write");
    };
    let mut bytes = postcard::to_allocvec(write).unwrap();
    let today = postcard::to_allocvec(readers).unwrap();
    bytes.truncate(bytes.len().saturating_sub(today.len()));
    bytes.extend(postcard::to_allocvec(&own).unwrap());
    bytes.extend(today);
    bytes
}

/// A workspace whose publications an earlier build kept, document writes among them that named
/// the member's own record they were, opens with every one of them read as this build writes
/// it, so each is carried and served; its files hold them so from then on.
#[test]
fn changes_an_earlier_build_kept_to_send_open_in_todays_layout() {
    use neighbour_wire::NeighbourMsg;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created = GovernedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        None,
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    let path = created.path.clone();
    drop(created);
    let write = |seq: u64, readers: Option<&str>| NeighbourMsg::AppDocument {
        peer: 3,
        app_id: "so.tilo.workspace".into(),
        cluster_id: "team".into(),
        document_id: format!("uuid:{seq}"),
        envelope: Box::new(collab_types::SignedOp::new_unsigned(
            vec![0xDE, 0xAD, 0xBE, 0xEF],
            collab_types::ArtifactId(uuid::Uuid::from_bytes([0x11; 16])),
            collab_types::Ed25519PublicKey::from_bytes([7; 32]),
            collab_types::AttestationId(uuid::Uuid::from_bytes([0x22; 16])),
            collab_types::OpKind::Change,
            SystemTime::UNIX_EPOCH,
            seq,
        )),
        readers: readers.map(str::to_owned),
    };
    // Two in the publications file, one in the record's own tail after them, each as that
    // build wrote it; the record has carried none of them yet.
    let writes = [
        (write(1, None), None),
        (write(2, Some("members")), Some(("notes.page", [7; 32]))),
        (write(3, None), Some(("notes.page", [9; 32]))),
    ];
    let kept: Vec<Publication> = writes
        .iter()
        .map(|(write, own)| {
            let bytes = published_before_own_removal(write, *own);
            assert!(
                NeighbourMsg::decode(&bytes).is_err(),
                "that layout does not read as today's"
            );
            Publication {
                bytes: bytes.into(),
                checkpoint: false,
            }
        })
        .collect();
    let mut stored: document_format::StoredResource =
        postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    let (mut file, mut filed) =
        appended::Appended::<Publication>::open(&path, PUBLICATIONS, stored.filed_publications)
            .unwrap();
    filed.append(&mut stored.outgoing);
    let carried = filed.len();
    filed.extend(kept[..2].iter().cloned());
    stored.filed_publications = file.file(&filed).unwrap();
    stored.outgoing = kept[2..].to_vec();
    stored.published = carried;
    std::fs::write(&path, postcard::to_stdvec(&stored).unwrap()).unwrap();

    let restored = GovernedWorkspace::open(path.clone(), None).unwrap();
    let to_carry: Vec<NeighbourMsg> = restored.record.outgoing[restored.record.published..]
        .iter()
        .map(|publication| NeighbourMsg::decode(&publication.bytes).unwrap())
        .collect();
    let expected: Vec<NeighbourMsg> = writes.iter().map(|(write, _)| write.clone()).collect();
    assert_eq!(
        to_carry, expected,
        "each is carried as today's build reads it"
    );
    drop(restored);

    let stored: document_format::StoredResource =
        postcard::from_bytes(&std::fs::read(&path).unwrap()).unwrap();
    let (_, mut filed) =
        appended::Appended::<Publication>::open(&path, PUBLICATIONS, stored.filed_publications)
            .unwrap();
    filed.extend(stored.outgoing);
    let written: Vec<NeighbourMsg> = filed[carried..]
        .iter()
        .map(|publication| NeighbourMsg::decode(&publication.bytes).unwrap())
        .collect();
    assert_eq!(written, expected, "the files hold them in today's layout");
    let reopened = GovernedWorkspace::open(path, None).unwrap();
    assert_eq!(reopened.summary().name, "Team");
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
            None,
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

#[test]
fn a_long_tail_compacts_into_a_fresh_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace = GovernedWorkspace::create(
        dir.path(),
        "Team",
        &cert,
        &key,
        None,
        (&[], &[]),
        SystemTime::now(),
    )
    .unwrap();
    workspace.record.published = workspace.record.outgoing.len();
    for index in 0..=MAX_PUBLICATION_TAIL {
        workspace.record.outgoing.push(Publication {
            bytes: vec![u8::try_from(index % 200).unwrap()].into(),
            checkpoint: false,
        });
    }
    let candidate = workspace.peer().clone();
    let state = workspace.collaboration().clone();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state))
        .unwrap();
    assert!(
        workspace
            .record
            .outgoing
            .iter()
            .all(|entry| entry.checkpoint)
    );
    assert_eq!(
        workspace.record.published, 0,
        "the fresh checkpoint is republished"
    );
    assert_eq!(workspace.record.generation, 2);
}

/// A few changes weighing half the workspace's base start it over, however
/// few they are; many light ones do not until they weigh as much.
#[test]
fn a_heavy_tail_compacts_and_a_light_one_does_not() {
    let base = Publication {
        bytes: vec![0; 4 * MIN_PUBLICATION_TAIL_BYTES].into(),
        checkpoint: true,
    };
    let change = |len: usize| Publication {
        bytes: vec![1; len].into(),
        checkpoint: false,
    };
    let light: Vec<Publication> = std::iter::once(base.clone())
        .chain((0..100).map(|_| change(1024)))
        .collect();
    assert!(!starts_over(&light), "100 KiB of changes on a 256 KiB base");
    let heavy = vec![
        base,
        change(MIN_PUBLICATION_TAIL_BYTES),
        change(MIN_PUBLICATION_TAIL_BYTES),
    ];
    assert!(starts_over(&heavy), "two changes as heavy as half the base");
    let small = vec![
        Publication {
            bytes: vec![0; 1024].into(),
            checkpoint: true,
        },
        change(2048),
    ];
    assert!(
        !starts_over(&small),
        "a small workspace starts over no more often than its floor"
    );
}

#[cfg(feature = "agents")]
#[tokio::test]
async fn applications_are_prepared_for_the_current_owner_only() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Private app authoring").await.unwrap();
    let catalogue = tilo_agent_session::AppCatalogue::native();
    Box::pin(assert_canonical_chat_preparation(
        workspaces,
        &created.workspace,
        &cert,
        &catalogue,
    ))
    .await;
    let other_root = SigningKey::from_bytes(&[73; 32]);
    let other_key = SigningKey::from_bytes(&[74; 32]);
    let mut stranger = cert.clone();
    stranger.user_public_key = other_root.verifying_key().to_bytes();
    stranger.device_public_key = other_key.verifying_key().to_bytes();
    sign_device_cert(&mut stranger, &other_root).unwrap();
    let mut changed_certificate = cert.clone();
    changed_certificate.device_label = "different signed certificate".into();
    sign_device_cert(&mut changed_certificate, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        workspaces
            .prepare_application(
                &created.workspace,
                &changed_certificate,
                &catalogue,
                "so.tilo.chat",
                ""
            )
            .await
            .is_err()
    );
    assert!(
        workspaces
            .prepare_application(
                &created.workspace,
                &stranger,
                &catalogue,
                "so.tilo.chat",
                ""
            )
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_canonical_chat_preparation(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_session::AppCatalogue,
) {
    assert!(
        workspaces
            .prepare_application(workspace, cert, catalogue, "so.tilo.chat", "")
            .await
            .unwrap()
    );
    let prepared = workspaces
        .resource_document(workspace, "so.tilo.chat", "")
        .await
        .unwrap()
        .expect("canonical Chat record exists");
    assert!(!prepared.is_empty());
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.into(),
        document: String::new(),
    };
    let workspace_id = source.workspace_id().unwrap();
    let retained = workspaces
        .resource_view_document(workspace_id, &caller, &source)
        .await
        .expect("the already-owned primary has a signed kept source");
    let mut original = automerge::AutoCommit::load(&prepared).unwrap();
    let mut arrived = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert_eq!(arrived.get_heads(), original.get_heads());
    assert_view_reader_refused(
        workspaces,
        workspace_id,
        &tilo_actions::remote::Caller::authenticated([9; 32], cert.device_public_key),
        &source,
    )
    .await;
    assert!(
        !workspaces
            .prepare_application(workspace, cert, catalogue, "so.tilo.chat", "")
            .await
            .unwrap()
    );
    assert_eq!(
        workspaces
            .resource_document(workspace, "so.tilo.chat", "")
            .await
            .unwrap()
            .unwrap(),
        prepared,
        "preparing again must preserve the actual original document"
    );
}

/// The workspace's copy of `record` of `app`, when it has one.
#[cfg(feature = "agents")]
async fn workspace_resource(
    workspaces: &Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
) -> Option<automerge::AutoCommit> {
    let bytes = workspaces
        .resource_document(workspace, app, record)
        .await
        .unwrap();
    bytes.map(|bytes| automerge::AutoCommit::load(&bytes).unwrap())
}

/// The host starts a workspace record no app factory makes, such as the
/// shared desktop, then changes it in place: the record keeps its graph, an
/// unchanged pass publishes nothing, and another certificate is refused.
#[cfg(feature = "agents")]
#[tokio::test]
async fn the_host_authors_a_workspace_record_no_factory_makes() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Shared desktop").await.unwrap();
    let workspace = created.workspace.as_str();
    let read = || workspace_resource(workspaces, workspace, DESKTOP);
    let text = |doc: &automerge::AutoCommit| {
        doc.get(ROOT, "desktop")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .unwrap()
    };
    // The workspace holds its shared desktop from the start, empty.
    assert!(
        read()
            .await
            .is_none_or(|doc| doc.get(ROOT, "desktop").unwrap().is_none())
    );
    workspaces
        .author_workspace_document(workspace, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "first").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let mut first = read().await.expect("the record was started");
    assert_eq!(text(&first), "first");
    workspaces
        .author_workspace_document(workspace, &cert, DESKTOP, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(read().await.unwrap().get_heads(), first.get_heads());
    workspaces
        .author_workspace_document(workspace, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "second")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let second = read().await.unwrap();
    assert_eq!(text(&second), "second");
    let first_head = first.get_heads()[0];
    assert!(second.has_change(&first_head), "the record keeps its graph");
    let mut different = cert.clone();
    different.device_label = "different current certificate".into();
    sign_device_cert(&mut different, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        workspaces
            .author_workspace_document::<()>(workspace, &different, DESKTOP, |_| {
                panic!("another certificate must never reach the change")
            })
            .await
            .is_err()
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one authoring pass: granted, written and read back through the native view"
)]
async fn an_authored_chat_keeps_its_native_view() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(scratch.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    let created = workspaces.create("Native host authoring").await.unwrap();
    let catalogue = tilo_agent_session::AppCatalogue::native();
    assert_unprepared_authoring_refused(workspaces, &created.workspace, &cert, &catalogue).await;
    workspaces
        .prepare_application(&created.workspace, &cert, &catalogue, "so.tilo.chat", "")
        .await
        .unwrap();
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        document: String::new(),
    };
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let original = workspaces
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .unwrap();
    assert_eq!(
        workspaces
            .author_application(
                &created.workspace,
                &cert,
                &catalogue,
                "so.tilo.chat",
                "",
                |_| Ok(42)
            )
            .await
            .unwrap(),
        42
    );
    let mut different = cert.clone();
    different.device_label = "different current certificate".into();
    sign_device_cert(&mut different, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        workspaces
            .author_application::<()>(
                &created.workspace,
                &different,
                &catalogue,
                "so.tilo.chat",
                "",
                |_| { panic!("another certificate must never reach the trusted change") }
            )
            .await
            .is_err()
    );
    assert!(
        workspaces
            .author_application(
                &created.workspace,
                &cert,
                &catalogue,
                "so.tilo.chat",
                "",
                |doc| {
                    doc.put(ROOT, "rollback_marker", true).unwrap();
                    Err::<(), _>("the authoring callback failed".into())
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        workspaces
            .resource_view_document(created.workspace_id, &caller, &source)
            .await
            .unwrap()
            .bytes,
        original.bytes,
        "a refused or failed authoring callback cannot publish a partial graph"
    );
    workspaces
        .author_application(
            &created.workspace,
            &cert,
            &catalogue,
            "so.tilo.chat",
            "",
            |doc| {
                doc.put(ROOT, "preserved_native_marker", true)
                    .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    let retained = workspaces
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .expect("native replay accepts the actual publication the host authored");
    let mut doc = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert!(doc.get(ROOT, "preserved_native_marker").unwrap().is_some());
    let committed = workspaces
        .resource_document(&created.workspace, "so.tilo.chat", "")
        .await
        .unwrap()
        .expect("the workspace reads its Chat");
    assert_eq!(
        doc.get_heads(),
        automerge::AutoCommit::load(&committed).unwrap().get_heads(),
        "native view bytes are the committed graph"
    );
    server.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_unprepared_authoring_refused(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_session::AppCatalogue,
) {
    for app in ["so.tilo.chat", "so.tilo.unregistered"] {
        assert!(
            workspaces
                .author_application::<()>(workspace, cert, catalogue, app, "", |_| {
                    panic!("missing or unregistered records must refuse before the callback")
                })
                .await
                .is_err()
        );
    }
}

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
    admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
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

/// Every decision is kept when it is answered, and what it published is carried
/// on the workspace's link in the order the workspace published it: a host that
/// stops right after the last answer starts with all of them, and a
/// pre-admission among them takes the workspace as it stands.
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
        assert!(
            governed.record.outgoing.len() >= 48,
            "{} changes published",
            governed.record.outgoing.len()
        );
        assert_eq!(
            governed.record.published,
            governed.record.outgoing.len(),
            "every change the workspace published is carried on its link once answered"
        );
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
            JoinTransportRequest::Complete(JoinCompleteRequest { proof }),
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
    admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
    let salt = 0x3333;
    let windows = one_chat_window(salt);
    let (generation, mut doc) = {
        let state = workspaces.workspace(&created.workspace).await.unwrap();
        let workspace = state.lock().await;
        let doc = workspace
            .peer()
            .admitted_workspace()
            .expect("a created workspace has a workspace document")
            .clone();
        (workspace.record.generation, doc)
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
        assert!(
            workspace.record.generation > generation,
            "the governor committed the window as the workspace's next generation"
        );
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
    let (writer, salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
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
    key: &SigningKey,
    cert: &DeviceCert,
    seed: u8,
) -> (DeviceCert, u16) {
    let member_key = SigningKey::from_bytes(&[seed; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(workspaces, workspace, key, &member).await;
    (member, u16::from(seed) << 8 | 0x11)
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
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
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
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
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
    let (a, a_salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
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
            }),
        )
        .await
    else {
        panic!("no membership challenge");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop).unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
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
            }),
        )
    };
    let JoinTransportResponse::MembershipChallenge(answer) = challenge(renewed.clone(), 22).await
    else {
        panic!("the renewal is challenged as the device it renews");
    };
    let proof = sign_membership_proof(answer.challenge, &laptop).unwrap();
    let JoinTransportResponse::MembershipComplete(current) = service
        .handle(
            endpoint,
            JoinTransportRequest::MembershipComplete(MembershipCompleteRequest { proof }),
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

/// `workspace`'s publication as it stands: whether it was just started over
/// (every entry a checkpoint) rather than added to (its last entry a change),
/// how many entries it holds, and the bytes of its base and of the changes
/// after it.
struct Shape {
    started_over: bool,
    entries: usize,
    base_bytes: usize,
    tail_bytes: usize,
}

async fn publication_shape(workspaces: &Workspaces, workspace: &str) -> Shape {
    let governed = workspaces.workspace(workspace).await.unwrap();
    let governed = governed.lock().await;
    let outgoing = &governed.record.outgoing;
    let (base, changes) = based(outgoing);
    let bytes = |publications: &[Publication]| -> usize {
        publications
            .iter()
            .map(|publication| publication.bytes.len())
            .sum()
    };
    let shape = Shape {
        started_over: changes.is_empty(),
        entries: outgoing.len(),
        base_bytes: bytes(base),
        tail_bytes: bytes(changes),
    };
    drop(governed);
    shape
}

/// A crowd arriving through the entry costs the workspace one change each, added
/// to what it publishes, not the whole workspace rebuilt, written and sent to
/// every member once per arrival. A change that takes something back starts
/// the workspace over at the next arrival, so a newcomer is never given what
/// was withdrawn before they came. Otherwise the workspace starts over from a
/// fresh base only once the changes since its last one weigh half as much as
/// that base, and the base holds its state in one compressed change: what a
/// newcomer fetches stays near the size of the workspace as it stands, not of
/// its history.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the server stays held through the assertions; Server::shutdown consumes it"
)]
#[tokio::test]
async fn a_crowd_joining_adds_to_the_workspace_rather_than_starting_it_over() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let workspace = created.workspace.clone();

    // The first arrival starts the workspace over: nothing says yet that what
    // it published since its last start takes nothing back.
    let mut arrivals = Vec::new();
    let (first, first_cert) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&first, &first_cert), 1).await;
    arrivals.push(first_cert);
    let shape = publication_shape(&workspaces, &workspace).await;
    assert!(
        shape.started_over,
        "the first arrival starts the workspace over"
    );
    let mut entries = shape.entries;
    for seed in 2..=8 {
        let (guest, guest_cert) = guest_device();
        entry_join(&workspaces, &cert, &workspace, (&guest, &guest_cert), seed).await;
        arrivals.push(guest_cert);
        let shape = publication_shape(&workspaces, &workspace).await;
        assert!(
            !shape.started_over,
            "arrival {seed} after only arrivals adds to the publication"
        );
        assert!(shape.entries > entries, "arrival {seed} added its change");
        entries = shape.entries;
    }

    // A change that takes something back (the workspace's name) is added, and the
    // next arrival starts over.
    workspaces.rename(&workspace, "Lounge II").await.unwrap();
    let renamed = publication_shape(&workspaces, &workspace).await;
    assert!(!renamed.started_over && renamed.entries > entries);
    let (late, late_cert) = guest_device();
    entry_join(&workspaces, &cert, &workspace, (&late, &late_cert), 30).await;
    arrivals.push(late_cert);
    assert!(
        publication_shape(&workspaces, &workspace)
            .await
            .started_over,
        "an arrival after a change that took something back starts the workspace over"
    );
    let mut before = publication_shape(&workspaces, &workspace).await;

    // Arrivals go on until the changes since the base weigh half of it.
    let mut started = None;
    for seed in 31..=250 {
        let (guest, guest_cert) = guest_device();
        entry_join(&workspaces, &cert, &workspace, (&guest, &guest_cert), seed).await;
        arrivals.push(guest_cert);
        let shape = publication_shape(&workspaces, &workspace).await;
        if shape.started_over {
            started = Some(before);
            break;
        }
        before = shape;
    }
    let before = started.expect("the workspace starts over once its changes weigh half its base");
    assert!(
        before.tail_bytes >= (before.base_bytes / 2).max(MIN_PUBLICATION_TAIL_BYTES),
        "the workspace started over before its changes weighed half its base: {} B of changes on a \
         {} B base",
        before.tail_bytes,
        before.base_bytes
    );
    let governed = workspaces.workspace(&workspace).await.unwrap();
    let base = governed
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .find_map(|publication| {
            match postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&publication.bytes) {
                Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, .. }) => Some(changes),
                _ => None,
            }
        })
        .expect("the base carries the workspace's document");
    // Its state in one change, and the receipts a start over records in one more.
    assert!(base.len() <= 2, "the base is {} changes", base.len());
    let state = base.iter().max_by_key(|change| change.len()).unwrap();
    assert_eq!(
        state.get(8),
        Some(&2),
        "the base's state is sent as a compressed change chunk"
    );

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
    let (_, page) = device_cert(
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
                .admit_member_device(&page, &workspaces.inner.host.key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .commit(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
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
