#![expect(
    clippy::significant_drop_tightening,
    reason = "the relay and workspace handles a test binds are shared handles it keeps until the test ends; dropping one earlier closes the host it drives and releases no lock"
)]

use super::*;
use crate::host::{HostConfig, HostIdentity, HostMode, WorkspaceHost};
use tilo_user_identity::{DeviceCapabilities, sign_device_cert};

pub(crate) use super::fixtures::{
    admit_view_reader, door_join, governor, governor_with, restricted_until, signed_pre_admission,
    test_host, test_host_with_grants,
};

#[cfg(feature = "agents")]
mod bots;

#[tokio::test]
async fn a_pre_admission_for_one_person_issues_an_invitation_only_they_can_redeem() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
    relay.shutdown().await;
}

/// A person's device certificate, as a guest's browser makes one.
pub(crate) fn guest_device() -> (SigningKey, DeviceCert) {
    let root = SigningKey::from_bytes(&rand::random());
    let key = SigningKey::from_bytes(&rand::random());
    let mut cert = DeviceCert {
        user_public_key: root.verifying_key().to_bytes(),
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "guest".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut cert, &root).unwrap();
    (key, cert)
}

#[tokio::test]
async fn a_crowd_of_pre_admitted_guests_joins_in_one_change_and_a_bad_token_is_refused_alone() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Public").await.unwrap();
    let mut proofs = Vec::new();
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
        let (device, device_cert) = guest_device();
        people.push(device_cert.user_public_key);
        let JoinReply::Challenge { challenge } = workspaces
            .page_join(JoinStep::Ask {
                invitation: issued.signed_invitation,
                device_cert,
                request_id: rand::random(),
            })
            .await
        else {
            panic!("no challenge");
        };
        let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
        // The last guest presents someone else's token.
        let token = if index == 5 { [1; 8] } else { token };
        proofs.push(JoinStep::ProvePreAdmitted {
            challenge,
            signature,
            name: format!("Guest {index}"),
            token: token.to_vec(),
        });
    }
    let before = workspaces
        .summary(&created.workspace)
        .await
        .unwrap()
        .generation;
    let answers =
        n0_future::join_all(proofs.into_iter().map(|proof| workspaces.page_join(proof))).await;
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
        .map(|member| member.user_public_key)
        .collect();
    for person in people.iter().take(5) {
        assert!(members.contains(person));
    }
    assert!(!members.contains(&people[5]));
    relay.shutdown().await;
}

/// Concurrent pre-admissions and challenges share their workspace's writes, and
/// every one of them is written before it is answered: after a restart each
/// guest still proves its challenge and is admitted.
#[tokio::test]
async fn a_crowds_shared_writes_hold_each_invitation_grant_and_challenge_across_a_restart() {
    use ed25519_dalek::Signer as _;
    use tilo_relay_protocol::workspaces::{JoinReply, JoinStep, join_proof_digest};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
    let asked = n0_future::join_all(issued.iter().zip(&guests).map(
        |(issued, (_, device_cert))| {
            workspaces.page_join(JoinStep::Ask {
                invitation: issued.signed_invitation.clone(),
                device_cert: device_cert.clone(),
                request_id: rand::random(),
            })
        },
    ))
    .await;
    relay.shutdown().await;
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    for (index, (asked, (device, _))) in asked.into_iter().zip(&guests).enumerate() {
        let JoinReply::Challenge { challenge } = asked else {
            panic!("no challenge: {asked:?}");
        };
        let signature = device.sign(&join_proof_digest(&challenge)).to_bytes();
        let token = u8::try_from(index + 1).unwrap();
        let answer = workspaces
            .page_join(JoinStep::ProvePreAdmitted {
                challenge,
                signature,
                name: format!("Guest {index}"),
                token: vec![token; 8],
            })
            .await;
        assert!(matches!(answer, JoinReply::Admitted { .. }), "{answer:?}");
    }
    relay.shutdown().await;
}

/// An entry cut off between committing a device's admission and settling it
/// (the page that asked closed meanwhile) leaves governance holding the
/// device while the relay decides its changes against standings without it.
/// The device's next entry finds it admitted already, and settles what
/// governance holds before answering, so its changes are decided with it.
#[tokio::test]
async fn an_entry_cut_off_before_its_settle_is_settled_by_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
                .admit_certificate(page.clone(), &workspaces.inner.host.key, now)
                .unwrap()
        );
        let state = governed.collaboration().clone();
        governed
            .commit(candidate, &workspaces.inner.host.key, now, |_, _| Ok(state))
            .unwrap();
    }
    let access = &workspaces.inner.host.access;
    assert!(
        !access
            .projects_standing(created.workspace_id, page.device_public_key)
            .await,
        "the cut-off entry left the device out of the standings changes are decided against"
    );

    assert!(
        !workspaces.enter(&created.workspace, &page).await.unwrap(),
        "the device is admitted already"
    );
    assert!(
        access
            .projects_standing(created.workspace_id, page.device_public_key)
            .await,
        "the next entry settled the admission"
    );
    relay.shutdown().await;
}

#[tokio::test]
async fn every_settled_decision_is_announced_with_its_room() {
    use tilo_action_policy::{Answer, Grant, KindSet, Role, Subject};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let mut changes = workspaces.governance_changes();
    let deny_outside = Grant::new(
        "public-guests-no-outside",
        Subject::Role(Role::Editor),
        tilo_action_policy::TreePath::workspace(&created.workspace),
        KindSet::OUTSIDE,
        Answer::Deny,
        Subject::Person(hex::encode(cert.user_public_key)),
    );
    workspaces
        .put_room_grants(&created.workspace, &cert, [4; 32], vec![deny_outside])
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
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_view_reader_refused(
    workspaces: &Workspaces,
    workspace: u64,
    caller: &tilo_actions::remote::Caller,
    source: &tilo_peer_state::document_frame::DocumentRoute,
) {
    assert!(
        workspaces
            .resource_view_policy(workspace, caller, &source.app_id, &source.record)
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
    workspace_id: u64,
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
        policy.standing.policy.role_of(&policy.standing.person),
        Some(collab_types::Role::Owner)
    );
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        record: "@tilo:package-primary".into(),
    };
    assert_eq!(
        workspaces
            .resource_view_document(created.workspace_id, &caller, &source)
            .await
            .err()
            .unwrap(),
        "this host takes part in no channel of the source's room",
        "the authorized reader reaches the existing-key boundary without creating channel state"
    );
    assert_eq!(
        workspaces
            .resource_view_document(0, &caller, &source)
            .await
            .err()
            .unwrap(),
        "source room mismatch"
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
            .resource_view_policy(0, &caller, "so.tilo.chat", "")
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
    relay.shutdown().await;
}

/// The workspace's default record a host seeds is the one every member's first
/// window opens: it names the default id, or members refuse it.
#[cfg(feature = "agents")]
#[tokio::test]
async fn a_seeded_workspace_default_record_names_the_default_id() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Seeded default record").await.unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    // An app whose windows each show a resource of their own: one that keeps
    // a workspace record (Scratchpad, Notes) seeds its package primary instead.
    let app = "so.tilo.sheets";
    assert!(!catalogue.get(app).unwrap().keeps_workspace_record());
    let record = ui::record::RecordId::workspace_default()
        .as_str()
        .to_owned();
    assert!(
        workspaces
            .prepare_application(&created.workspace, &cert, &catalogue, app, &record)
            .await
            .unwrap()
    );
    let bytes = workspaces
        .record_document(&created.workspace, app, &record)
        .await
        .unwrap()
        .expect("the room holds its default record");
    let doc = automerge::AutoCommit::load(&bytes).unwrap();
    let kind = catalogue.get(app).unwrap().record_kind.clone();
    assert_eq!(
        ui::record::identity(&doc),
        Some(ui::record::RecordCheckout::new(
            ui::record::RecordId::workspace_default(),
            kind
        ))
    );
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
#[tokio::test]
async fn governor_application_authoring_survives_publication_tail_rollover() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces
        .create("Native authoring rollover")
        .await
        .unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    workspaces
        .prepare_application(&created.workspace, &cert, &catalogue, "so.tilo.chat", "")
        .await
        .unwrap();
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        record: String::new(),
    };
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    for index in 1..=MAX_PUBLICATION_TAIL {
        let app = std::collections::BTreeSet::from([format!("so.tilo.rollover-fixture-{index}")]);
        assert!(
            workspaces
                .settle_app_authorship(
                    &created.workspace,
                    &cert,
                    &app,
                    &std::collections::BTreeSet::new()
                )
                .await
                .unwrap()
        );
    }
    let current = workspaces.workspace(&created.workspace).await.unwrap();
    let tail = current
        .lock()
        .await
        .record
        .outgoing
        .iter()
        .rev()
        .take_while(|publication| !publication.checkpoint)
        .count();
    assert!(
        tail >= MAX_PUBLICATION_TAIL,
        "real governance writes reach rollover"
    );
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
        "rollover must publish the new governor mutation into the retained source"
    );
    relay.shutdown().await;
}

#[test]
fn a_created_room_restores_with_its_governance_and_publication() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    assert!(!created.record.outgoing.is_empty());
    assert!(created.record.outgoing.iter().all(|entry| entry.checkpoint));
    let restored = GovernedWorkspace::open(created.path.clone(), &cert, None).unwrap();
    assert_eq!(restored.record.workspace, created.record.workspace);
    assert_eq!(restored.record.generation, 1);
    assert_eq!(restored.summary().name, "Team");
    assert!(restored.peer().has_governance());
}

/// A record's large state is a file of its own: a governance change that
/// does not touch it writes the workspace file alone, a change of it replaces
/// its file, and the workspace opens to what it committed.
#[test]
fn a_large_record_is_stored_apart_and_written_only_when_it_changes() {
    use automerge::transaction::Transactable as _;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    // Incompressible text, so the saved record stays large.
    let mut text = vec![0u8; 48 * 1024];
    blake3::Hasher::new()
        .update(b"large record")
        .finalize_xof()
        .fill(&mut text);
    let large = || {
        let mut doc = automerge::AutoCommit::new();
        doc.put(automerge::ROOT, "text", hex::encode(&text))
            .unwrap();
        doc.save()
    };
    let state = workspace.collaboration().clone();
    let mut candidate = workspace.peer().clone();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", large(), &key, SystemTime::now())
        .unwrap();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let stored = |workspace: &GovernedWorkspace| {
        workspace
            .record
            .peer
            .iter()
            .filter(|part| matches!(part, peer_parts::PeerPart::Stored { .. }))
            .cloned()
            .collect::<Vec<_>>()
    };
    let first = stored(&workspace);
    assert_eq!(first.len(), 1, "the large record is stored apart");
    assert_eq!(workspace.parts.files(), 1);
    let modified = |workspace: &GovernedWorkspace| {
        std::fs::read_dir(workspace.path.with_extension("parts"))
            .unwrap()
            .map(|entry| entry.unwrap().metadata().unwrap().modified().unwrap())
            .collect::<Vec<_>>()
    };
    let written = modified(&workspace);

    // A change that leaves the record alone does not write it again.
    let candidate = workspace.peer().clone();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    assert_eq!(stored(&workspace), first);
    assert_eq!(modified(&workspace), written);

    // Changing it stores it anew and removes what no workspace file names.
    let mut candidate = workspace.peer().clone();
    let mut edited =
        automerge::AutoCommit::load(&workspace.peer().document("notes", "plan").unwrap()).unwrap();
    edited.put(automerge::ROOT, "edited", true).unwrap();
    candidate
        .publication_mut()
        .publish_document("notes", "plan", edited.save(), &key, SystemTime::now())
        .unwrap();
    workspace
        .commit(candidate, &key, SystemTime::now(), |_, _| Ok(state.clone()))
        .unwrap();
    let second = stored(&workspace);
    assert_eq!(second.len(), 1);
    assert_ne!(second, first);
    assert_eq!(workspace.parts.files(), 1, "the replaced part is removed");

    let restored = GovernedWorkspace::open(workspace.path.clone(), &cert, None).unwrap();
    assert_eq!(
        restored.peer().document("notes", "plan"),
        workspace.peer().document("notes", "plan")
    );
    assert_eq!(restored.record.generation, workspace.record.generation);

    // A stored part that does not match its hash refuses to open.
    let peer_parts::PeerPart::Stored { digest, .. } = &second[0] else {
        unreachable!()
    };
    let path = workspace
        .path
        .with_extension("parts")
        .join(hex::encode(digest));
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 1;
    std::fs::write(&path, bytes).unwrap();
    assert!(GovernedWorkspace::open(workspace.path.clone(), &cert, None).is_err());
}

/// A workspace is restored by the device that governs it, which the record
/// holds by its key, what its certificate lets it do and how long it lasts: a
/// certificate's label is a name, so a host that renamed its own certificate
/// starts on the workspaces it served, while another device — another key, or
/// one the person granted more — is refused.
#[test]
fn a_workspace_is_restored_by_its_governing_device_and_no_other() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let created =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    let root = SigningKey::from_bytes(&[1; 32]);
    let mut renamed = cert.clone();
    renamed.device_label = "workspace host".into();
    sign_device_cert(&mut renamed, &root).unwrap();
    let restored = GovernedWorkspace::open(created.path.clone(), &renamed, None).unwrap();
    assert_eq!(restored.record.name, "Team");

    let mut granted_more = renamed.clone();
    granted_more.capabilities = DeviceCapabilities::primary();
    sign_device_cert(&mut granted_more, &root).unwrap();
    assert!(GovernedWorkspace::open(created.path.clone(), &granted_more, None).is_err());

    let other_key = SigningKey::from_bytes(&[3; 32]);
    let mut other = cert;
    other.device_public_key = other_key.verifying_key().to_bytes();
    sign_device_cert(&mut other, &root).unwrap();
    assert!(GovernedWorkspace::open(created.path, &other, None).is_err());
}

#[test]
fn a_workspace_name_must_be_printable_and_short() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    for name in ["", "   ", "a\u{7}b", &"x".repeat(81)] {
        assert!(
            GovernedWorkspace::create(dir.path(), name, &cert, &key, None, SystemTime::now())
                .is_err()
        );
    }
}

#[test]
fn a_long_tail_compacts_into_a_fresh_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let mut workspace =
        GovernedWorkspace::create(dir.path(), "Team", &cert, &key, None, SystemTime::now())
            .unwrap();
    workspace.record.published = workspace.record.outgoing.len();
    for index in 0..=MAX_PUBLICATION_TAIL {
        workspace.record.outgoing.push(Publication {
            bytes: vec![u8::try_from(index % 200).unwrap()],
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

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one window write decided as the workspace's grants change keeps its fixture visible"
)]
async fn legacy_window_write_respects_current_member_grants() {
    use collab_types::Role;
    use tilo_action_policy::{Answer, Grant, KindSet, Lifetime, Subject};
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let (relay, workspaces) = {
        let relay = test_host(dir.path(), &key, &cert).await;
        let workspaces = relay.workspaces().clone();
        (relay, workspaces)
    };
    let created = workspaces.create("Legacy write restriction").await.unwrap();
    assert_eq!(
        workspaces
            .legacy_writer_role(&created.workspace, cert.device_public_key)
            .await,
        Some(Role::Owner)
    );
    // The owner's own device on an operating system Tilo does not control:
    // held to the foreign-device bound, which the workspace may narrow.
    let owner_root = SigningKey::from_bytes(&[1; 32]);
    let foreign_key = SigningKey::from_bytes(&[6; 32]);
    let mut capabilities = DeviceCapabilities::member();
    capabilities.foreign = true;
    let mut foreign = DeviceCert {
        user_public_key: owner_root.verifying_key().to_bytes(),
        device_public_key: foreign_key.verifying_key().to_bytes(),
        device_label: "borrowed laptop".into(),
        capabilities,
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut foreign, &owner_root).unwrap();
    let state = workspaces.workspace(&created.workspace).await.unwrap();
    {
        let mut workspace = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        candidate
            .admit_certificate(foreign.clone(), &key, now)
            .unwrap();
        let collaboration = workspace.collaboration().clone();
        workspace
            .commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    assert_eq!(
        workspaces
            .legacy_writer_role(&created.workspace, foreign.device_public_key)
            .await,
        Some(Role::Owner),
        "a foreign device changes the room's content"
    );
    {
        let mut workspace = state.lock().await;
        let now = SystemTime::now();
        let mut candidate = workspace.peer().clone();
        // The grant lasts from its own commit, however long the workspace took
        // to set up.
        let deadline_ms = tilo_clock::try_unix_now_ms()
            .unwrap()
            .checked_add(5000)
            .unwrap();
        let read_only = Grant::new(
            "legacy-window-read-only-foreign",
            Subject::ForeignDevice,
            tilo_action_policy::TreePath::workspace(&created.workspace),
            KindSet::UNDOABLE,
            Answer::Deny,
            Subject::Person(hex::encode(cert.user_public_key)),
        )
        .with_lifetime(Lifetime::Until(deadline_ms));
        candidate
            .governance_mut()
            .put_governed_grants(&[read_only], &key, now)
            .unwrap();
        let collaboration = workspace.collaboration().clone();
        workspace
            .commit(candidate, &key, now, |_, _| Ok(collaboration))
            .unwrap();
    }
    assert_eq!(
        workspaces
            .legacy_writer_role(&created.workspace, foreign.device_public_key)
            .await,
        None,
        "ownership must not bypass a current grant denying changes from foreign devices"
    );
    // Expiration is an authorized transition and must be observed without restart.
    tilo_test_env::within("the current read-only grant expires", async {
        loop {
            if workspaces
                .legacy_writer_role(&created.workspace, foreign.device_public_key)
                .await
                == Some(Role::Owner)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    drop(workspaces);
    crate::workspaces::fixtures::TestHost::shutdown(relay).await;
}

#[cfg(feature = "agents")]
#[tokio::test]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the relay remains live through the policy assertions and its consuming shutdown"
)]
async fn applications_are_prepared_for_the_current_owner_only() {
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Private app authoring").await.unwrap();
    let chat = std::collections::BTreeSet::from(["so.tilo.chat".to_owned()]);
    assert!(
        workspaces
            .settle_app_authorship(
                &created.workspace,
                &cert,
                &chat,
                &std::collections::BTreeSet::new()
            )
            .await
            .unwrap()
    );
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    Box::pin(assert_canonical_chat_preparation(
        workspaces,
        &created.workspace,
        &cert,
        &catalogue,
    ))
    .await;
    let state = workspaces.workspace(&created.workspace).await.unwrap();
    {
        let workspace = state.lock().await;
        let now = std::time::SystemTime::now();
        let policy = workspace
            .peer()
            .permissions(cert.device_public_key, now)
            .unwrap()
            .policy;
        let grant = policy
            .grants
            .iter()
            .find(|grant| grant.id.as_str() == "app-author:so.tilo.chat")
            .unwrap();
        assert_eq!(
            grant.subject,
            tilo_action_policy::Subject::App("so.tilo.chat".into())
        );
        assert!(policy.app_authors("so.tilo.chat", &tilo_action_policy::Context::at_time(now),));
    }
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
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_canonical_chat_preparation(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_participant::AppCatalogue,
) {
    assert!(
        workspaces
            .prepare_application(workspace, cert, catalogue, "so.tilo.chat", "")
            .await
            .unwrap()
    );
    let prepared = {
        let current = workspaces.workspace(workspace).await.unwrap();
        current
            .lock()
            .await
            .peer()
            .document("so.tilo.chat", "")
            .expect("canonical Chat record exists")
    };
    assert!(!prepared.is_empty());
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: workspace.into(),
        record: String::new(),
    };
    let workspace_id = source.workspace_id().unwrap();
    let retained = workspaces
        .resource_view_document(workspace_id, &caller, &source)
        .await
        .expect("the already-owned governor primary has a signed retained source");
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
    {
        let current = workspaces.workspace(workspace).await.unwrap();
        assert_eq!(
            current
                .lock()
                .await
                .peer()
                .document("so.tilo.chat", "")
                .unwrap(),
            prepared,
            "preparing again must preserve the actual original document"
        );
    }
}

/// The workspace's copy of `record` of `app`, when it has one.
#[cfg(feature = "agents")]
async fn workspace_record(
    workspaces: &Workspaces,
    workspace: &str,
    (app, record): (&str, &str),
) -> Option<automerge::AutoCommit> {
    let current = workspaces.workspace(workspace).await.unwrap();
    let bytes = current.lock().await.peer().document(app, record);
    bytes.map(|bytes| automerge::AutoCommit::load(&bytes).unwrap())
}

/// The governor starts a workspace record no app factory makes, such as the
/// shared desktop, then changes it in place: the record keeps its graph, an
/// unchanged pass publishes nothing, and another certificate is refused.
#[cfg(feature = "agents")]
#[tokio::test]
async fn governor_authors_a_room_record_no_factory_makes() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    const DESKTOP: (&str, &str) = ("so.tilo.browser-desktop", "desktop");
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Shared desktop").await.unwrap();
    let workspace = created.workspace.as_str();
    let read = || workspace_record(workspaces, workspace, DESKTOP);
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
        .author_room_record(workspace, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "first").map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let mut first = read().await.expect("the record was started");
    assert_eq!(text(&first), "first");
    workspaces
        .author_room_record(workspace, &cert, DESKTOP, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(read().await.unwrap().get_heads(), first.get_heads());
    workspaces
        .author_room_record(workspace, &cert, DESKTOP, |doc| {
            doc.put(ROOT, "desktop", "second")
                .map_err(|e| e.to_string())
        })
        .await
        .unwrap();
    let second = read().await.unwrap();
    assert_eq!(text(&second), "second");
    let first_head = first.get_heads()[0];
    assert!(
        second.get_change_by_hash(&first_head).is_some(),
        "the record keeps its graph"
    );
    let mut different = cert.clone();
    different.device_label = "different current certificate".into();
    sign_device_cert(&mut different, &SigningKey::from_bytes(&[1; 32])).unwrap();
    assert!(
        workspaces
            .author_room_record::<()>(workspace, &different, DESKTOP, |_| {
                panic!("another certificate must never reach the change")
            })
            .await
            .is_err()
    );
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one authoring pass: granted, written, read back through the native view and the voice binding"
)]
async fn governor_application_authoring_preserves_native_view_and_voice_binding() {
    use automerge::{ROOT, ReadDoc, transaction::Transactable};
    let scratch = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(scratch.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces
        .create("Native governor authoring")
        .await
        .unwrap();
    let catalogue = tilo_agent_participant::AppCatalogue::native();
    assert_unprepared_authoring_refused(workspaces, &created.workspace, &cert, &catalogue).await;
    workspaces
        .prepare_application(&created.workspace, &cert, &catalogue, "so.tilo.chat", "")
        .await
        .unwrap();
    let source = tilo_peer_state::document_frame::DocumentRoute {
        app_id: "so.tilo.chat".into(),
        cluster_id: created.workspace.clone(),
        record: String::new(),
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
    let voice = workspaces
        .author_application(
            &created.workspace,
            &cert,
            &catalogue,
            "so.tilo.chat",
            "",
            |doc| {
                chat::demo::start(doc, "author", "Author", "AU")
                    .map_err(|error| error.to_string())?;
                doc.put(ROOT, "preserved_native_marker", true)
                    .map_err(|error| error.to_string())?;
                Ok(chat::projection::read(doc)
                    .channel_rows()
                    .into_iter()
                    .find(|channel| channel.kind == chat::model::ChannelKind::Voice)
                    .unwrap()
                    .id)
            },
        )
        .await
        .unwrap();
    let retained = workspaces
        .resource_view_document(created.workspace_id, &caller, &source)
        .await
        .expect("native replay accepts the actual governor-authored publication");
    let mut doc = automerge::AutoCommit::load(&retained.bytes).unwrap();
    assert!(doc.get(ROOT, "preserved_native_marker").unwrap().is_some());
    let current = workspaces.workspace(&created.workspace).await.unwrap();
    let guard = current.lock().await;
    let binding = guard
        .peer()
        .media()
        .resolve_media_binding("so.tilo.chat", "", &voice)
        .expect("the same native source resolves through production voice admission");
    assert_eq!(binding.channel_id, voice);
    assert_eq!(
        doc.get_heads(),
        guard
            .peer()
            .plaintext_complete_heads("so.tilo.chat", "")
            .unwrap(),
        "native view bytes and the governor's voice binding share the committed graph"
    );
    drop(guard);
    relay.shutdown().await;
}

#[cfg(feature = "agents")]
async fn assert_unprepared_authoring_refused(
    workspaces: &Workspaces,
    workspace: &str,
    cert: &DeviceCert,
    catalogue: &tilo_agent_participant::AppCatalogue,
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

async fn is_banned(
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
    reason = "the relay stays live until its consuming shutdown"
)]
async fn a_renamed_room_shows_its_new_name_to_members_and_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
    let again = workspaces
        .rename(&created.workspace, "Plaza")
        .await
        .unwrap();
    assert_eq!(
        again.generation, renamed.generation,
        "the same name changes nothing"
    );
    assert!(workspaces.rename(&created.workspace, " ").await.is_err());
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
    relay.shutdown().await;
    let restored = GovernedWorkspace::open(path, &cert, None).unwrap();
    assert_eq!(restored.summary().name, "Plaza");
    assert_eq!(
        restored.peer().governance().workspace_name().as_deref(),
        Some("Plaza")
    );
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn the_owner_bans_and_unbans_and_a_stranger_cannot() {
    use tilo_relay_session::workspaces::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Demo").await.unwrap();
    let guest = [7; 32];
    let request = |request_id| RemoveMemberRequest {
        request_id,
        user_public_key: guest,
    };
    let stranger_root = SigningKey::from_bytes(&[9; 32]);
    let stranger_key = SigningKey::from_bytes(&[10; 32]);
    let mut stranger = DeviceCert {
        user_public_key: stranger_root.verifying_key().to_bytes(),
        device_public_key: stranger_key.verifying_key().to_bytes(),
        device_label: "stranger".into(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    sign_device_cert(&mut stranger, &stranger_root).unwrap();
    assert!(
        workspaces
            .ban_member(&created.workspace, &stranger, request([1; 32]))
            .await
            .is_err(),
        "someone not in the room bans nobody"
    );
    workspaces
        .ban_member(&created.workspace, &cert, request([2; 32]))
        .await
        .unwrap();
    assert!(is_banned(workspaces, &created.workspace, &cert, guest).await);
    assert!(
        workspaces
            .unban_member(&created.workspace, &cert, request([2; 32]))
            .await
            .is_err(),
        "one request id names one change"
    );
    workspaces
        .unban_member(&created.workspace, &cert, request([3; 32]))
        .await
        .unwrap();
    assert!(!is_banned(workspaces, &created.workspace, &cert, guest).await);
    relay.shutdown().await;
}

#[tokio::test]
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays live until its consuming shutdown"
)]
async fn the_owner_restricts_until_a_time_and_lifts_it() {
    use tilo_relay_session::workspaces::RemoveMemberRequest;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
    relay.shutdown().await;
}

/// A relay certified with `capabilities`, a workspace it governs, and one of
/// the workspace's member devices, with the channel route that member asks for.
struct AskedForAChannel {
    /// The relay's state.
    state: tempfile::TempDir,
    relay: crate::workspaces::fixtures::TestHost,
    workspaces: Workspaces,
    workspace: String,
    channel: collab_types::ChannelId,
}

/// Start a relay certified with `capabilities`, create a workspace on it, admit
/// a member device of the person, and hand the workspace that device's MLS
/// access request for a fresh record channel, as its runtime sends one.
async fn a_member_asks_for_a_channel(capabilities: DeviceCapabilities) -> AskedForAChannel {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor_with(capabilities);
    let relay = test_host(&dir.path().join("relay"), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Sealed").await.unwrap();
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let mut member = cert.clone();
    member.device_public_key = member_key.verifying_key().to_bytes();
    member.capabilities = DeviceCapabilities::member();
    sign_device_cert(&mut member, &SigningKey::from_bytes(&[1; 32])).unwrap();
    admit_view_reader(&workspaces, &created.workspace, &key, &member).await;
    // The device names the record it asks for: a channel nobody named may
    // be anyone's own record and admits no one.
    let route = collab_types::ChannelRoute::new("so.tilo.notes", "tab:0000000000000001");
    let channel = route.channel(&created.workspace);
    let vault = channel_crypto::VaultKey::new([3; 32]);
    let member_dir = dir.path().join("member");
    std::fs::create_dir_all(&member_dir).unwrap();
    let request = channel_crypto::ChannelPeer::waiting(
        Some(&member_dir),
        channel,
        Box::new(member_key.clone()),
        &vault,
    )
    .unwrap()
    .access_request_now(1, false, &vault)
    .unwrap()
    .expect("a member waiting for the channel asks for it");
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest {
        room: created.workspace.clone(),
        request: Box::new(request),
        route: Box::new(route),
        delivery: 1,
    })
    .unwrap();
    workspaces
        .absorb(&created.workspace, member.device_public_key, &body)
        .await
        .unwrap();
    AskedForAChannel {
        state: dir,
        relay,
        workspaces,
        workspace: created.workspace,
        channel,
    }
}

impl AskedForAChannel {
    /// Whether the workspace kept the member's request among the records it
    /// retains for the devices that take part in its channels.
    async fn retains_the_request(&self) -> bool {
        let workspace_id = crate::room_workspace_id(&self.workspace).unwrap();
        self.relay
            .workspace_records()
            .retained(workspace_id)
            .await
            .is_ok_and(|retained| {
                retained.iter().any(|retained| {
                    postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&retained.body).is_ok_and(
                        |message| {
                            matches!(
                                message,
                                neighbour_wire::NeighbourMsg::MlsRoutedAccessRequest {
                                    room,
                                    request,
                                    ..
                                } if room == self.workspace && request.channel == self.channel
                            )
                        },
                    )
                })
            })
    }

    /// Shut the relay down, then remove its state.
    async fn shut_down(self) {
        let Self {
            state,
            relay,
            workspaces,
            ..
        } = self;
        drop(workspaces);
        relay.shutdown().await;
        drop(state);
    }
}

/// security.md, "What a workspace host reads and originates": a host that runs
/// no channel service takes part in no channel and holds no channel key, and
/// keeps what the workspace's channels are made of like any other governance,
/// for whoever takes part.
#[tokio::test]
async fn a_store_host_retains_what_its_members_send_for_a_channels_group() {
    let asked = a_member_asks_for_a_channel(DeviceCapabilities::host()).await;
    assert!(
        asked.workspaces.device_keys().is_none(),
        "a host runs no channel service and holds no channel keys"
    );
    assert!(
        asked.retains_the_request().await,
        "the room retained the member's request for its channels"
    );
    asked.shut_down().await;
}

/// A decision does not wait for workspace in its workspace owner's queue. With
/// the owner stalled on the workspace's first change and more changes behind it
/// than its queue holds, every decision still settles and answers, and a
/// pre-admission still takes the workspace; once the owner moves, every change
/// is retained, in the order the workspace published them.
#[tokio::test]
async fn decisions_settle_while_their_workspace_owner_is_stalled() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Public").await.unwrap();
    let records = workspaces.inner.host.records.clone();
    // The creation's own changes are retained before the owner stalls.
    records.retained(created.workspace_id).await.unwrap();
    let root = dir
        .path()
        .join("workspaces")
        .join(created.workspace_id.to_string());
    let (release, reached) = crate::store::test_barrier::install(&root, "state-persist");
    // Each name is one governance change: more than the owner queues.
    for index in 0..48 {
        tilo_test_env::within(
            format!("rename {index} while the owner is stalled"),
            workspaces.rename(&created.workspace, &format!("Public {index}")),
        )
        .await
        .unwrap();
    }
    tilo_test_env::within("the owner stalls on the first change", reached)
        .await
        .unwrap();
    tilo_test_env::within(
        "a pre-admission while the owner is stalled",
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
    drop(release);
    let retained = tilo_test_env::within(
        "every change retained once the owner moves",
        records.retained(created.workspace_id),
    )
    .await
    .unwrap();
    let published: Vec<Vec<u8>> = {
        let governed = workspaces.workspace(&created.workspace).await.unwrap();
        let governed = governed.lock().await;
        governed
            .record
            .outgoing
            .iter()
            .filter(|publication| !publication.checkpoint)
            .map(|publication| publication.bytes.clone())
            .collect()
    };
    assert!(
        published.len() >= 48,
        "{} changes published",
        published.len()
    );
    let host = cert.device_public_key;
    let mut bodies = retained
        .iter()
        .filter(|body| body.author == host)
        .map(|body| &body.body);
    for change in &published {
        assert!(
            bodies.any(|body| body == change),
            "a published change is retained, after the ones published before it"
        );
    }
    relay.shutdown().await;
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let (device, device_cert) = guest_device();
    let issued = workspaces
        .pre_admit(
            &cert,
            PreAdmitRequest {
                request_id: [9; 32],
                grant: signed_pre_admission(&created.workspace, b"door"),
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
    relay.shutdown().await;
}

/// `windows`, the arrangement of `workspace` as `member`'s device minting in
/// `salt` writes it onto `doc`: what it made is its person's.
fn write_windows(
    doc: &mut automerge::AutoCommit,
    workspace: &str,
    member: &DeviceCert,
    salt: u16,
    windows: &[tilo_desktop_model::Strip],
) {
    let identity = tilo_desktop_model::WorkspaceId::parse(workspace).unwrap();
    let mut workspace = tilo_desktop_model::Workspace::in_workspace(
        tilo_desktop_model::WorkspaceSlot::of_workspace(identity),
        identity,
        "Room",
    );
    if let Some(held) = tilo_desktop_model::shared_record::read(&*doc, None) {
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
    tilo_desktop_model::shared_record::device::write(
        doc,
        identity,
        &workspace,
        &tilo_desktop_model::shared_record::device::Writer {
            person: Some(&person),
            mine: &mine,
            retained: &|_| false,
            carried: tilo_desktop_model::shared_record::device::Writer::NOTHING_CARRIED,
            records: tilo_desktop_model::shared_record::device::Writer::NOTHING_NOTED,
        },
    )
    .unwrap();
}

/// The window titles `doc`'s arrangement holds.
fn titles_in(doc: &automerge::AutoCommit) -> Vec<String> {
    tilo_desktop_model::shared_record::read(doc, None)
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
/// arrangement, made by the member's person. The governor takes it in as the workspace's next
/// generation, so every other member sees the window; before, only record
/// writes were taken in and the window reached nobody.
#[tokio::test]
async fn a_window_a_member_opens_becomes_the_rooms_next_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
            .expect("a created room has a room document")
            .clone();
        (workspace.record.generation, doc)
    };
    let heads = doc.get_heads();
    doc.set_actor(automerge::ActorId::from(member.device_public_key.to_vec()));
    write_windows(&mut doc, &created.workspace, &member, salt, &windows);
    doc.commit();
    let changes = doc.get_changes(&heads);
    assert!(!changes.is_empty(), "opening a window changes the room");
    let hashes = changes
        .iter()
        .map(automerge::Change::hash)
        .collect::<Vec<_>>();
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        room: created.workspace.clone(),
        changes: changes
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
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
            "the governor committed the window as the room's next generation"
        );
        let mut admitted = workspace.peer().admitted_workspace().unwrap().clone();
        drop(workspace);
        for hash in &hashes {
            assert!(
                admitted.get_change_by_hash(hash).is_some(),
                "the governed room holds the member's window"
            );
        }
    }
    drop(workspaces);
    relay.shutdown().await;
}

/// A device of the owner's that is not this host removes a member itself,
/// in its own replica, and publishes the change: the host takes it in as
/// the workspace's next generation, and every invitation still waiting for the
/// person goes with them.
#[tokio::test]
async fn a_removal_another_device_writes_is_taken_in_with_the_persons_invitations() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Lounge").await.unwrap().workspace;
    let (writer, salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
    let writer_key = SigningKey::from_bytes(&[3; 32]);
    let (guest_key, guest) = guest_device();
    door_join(&workspaces, &cert, &workspace, (&guest_key, &guest), 4).await;
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
            room: workspace.clone(),
            changes,
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
            .all(|member| member.user_public_key != guest.user_public_key || member.removed),
        "{members:?}"
    );
    assert!(
        !waiting(workspaces.pending(&workspace, &cert).await.unwrap()),
        "the removed person's invitation went with them"
    );

    let _windows = terminals_opened(&workspaces, &workspace, &writer, salt).await;
    drop(workspaces);
    relay.shutdown().await;
}

/// A device of the governor's person, certified as a member and admitted to
/// `workspace`, and the id namespace it mints in.
async fn admitted_member(
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
        room: workspace.to_owned(),
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

/// A document write the workspace refused is filed as this device's verdict in
/// the workspace's own document, signed by its device and naming the write's
/// route, author and sequence: one fact, carried by the sync that already
/// carries the workspace. A change to the workspace's topology names no record
/// and files nothing, and the same write again keeps the verdict held.
#[tokio::test]
async fn a_refused_document_write_is_filed_in_the_workspaces_document() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let workspace = workspaces.create("Studio").await.unwrap().workspace;
    let (member, member_salt) = admitted_member(&workspaces, &workspace, &key, &cert, 3).await;
    let member_key = SigningKey::from_bytes(&[3; 32]);
    let author = collab_types::Ed25519PublicKey::from_bytes(member_key.verifying_key().to_bytes());
    let (app, record) = ("so.tilo.notes", "notes");
    let mut envelope = collab_types::SignedOp::new_unsigned(
        vec![7; 12],
        collab_types::document_artifact_id(app, &workspace, record),
        author,
        collab_types::document_attestation_id(app, &workspace, record),
        collab_types::OpKind::Change,
        SystemTime::now(),
        7,
    );
    collab_types::sign_op(&mut envelope, &member_key).unwrap();
    let body = postcard::to_stdvec(&neighbour_wire::NeighbourMsg::AppDocument {
        peer: tilo_peer_state::peer_handle(member_key.verifying_key().as_bytes()),
        app_id: app.into(),
        cluster_id: workspace.clone(),
        record_id: record.into(),
        envelope: Box::new(envelope),
    })
    .unwrap();
    let reason = "record read forbidden: notes/main";
    workspaces
        .publish_refusal(&workspace, &body, reason)
        .await
        .unwrap();
    {
        let state = workspaces.workspace(&workspace).await.unwrap();
        let governed = state.lock().await;
        let held = governed.peer().workspace().write_refusals();
        assert_eq!(held.len(), 1, "the workspace files its verdict: {held:?}");
        let refusal = &held[0];
        assert_eq!(
            refusal.author, member.device_public_key,
            "the write's author"
        );
        assert_eq!(refusal.by, cert.device_public_key, "the refusing device");
        assert!(
            refusal.signed_by(&cert.device_public_key),
            "signed as its own"
        );
        assert_eq!(
            (refusal.app.as_str(), refusal.record.as_str()),
            (app, record)
        );
        assert_eq!(refusal.seq, 7);
        assert_eq!(refusal.reason, neighbour_wire::RefusalReason::Forbidden);
        assert_eq!(refusal.detail, reason);
    }
    // A topology change is no record's write: it names no route for a verdict.
    let topology = terminals_opened(&workspaces, &workspace, &member, member_salt).await;
    workspaces
        .publish_refusal(
            &workspace,
            &topology,
            "a change of arrangement is no record write",
        )
        .await
        .unwrap();
    // The same write again keeps the verdict the workspace already holds.
    workspaces
        .publish_refusal(&workspace, &body, reason)
        .await
        .unwrap();
    let state = workspaces.workspace(&workspace).await.unwrap();
    let governed = state.lock().await;
    assert_eq!(
        governed
            .peer()
            .workspace()
            .write_refusals()
            .iter()
            .filter(|refusal| refusal.author == member.device_public_key)
            .count(),
        1,
        "one verdict per write's author and route"
    );
    drop(governed);
    drop(state);
    drop(workspaces);
    relay.shutdown().await;
}

/// A member's windows are the room's: the governor takes the change in, and
/// every member sees the windows.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn the_windows_a_member_opens_are_shared_by_the_room() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
    relay.shutdown().await;
}

/// `member`'s change to `workspace`'s arrangement closing every window it made
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
    tilo_desktop_model::shared_record::device::withdraw(&mut doc, &|id| {
        tilo_desktop_model::peer_salt_of(id) == salt
    })
    .unwrap();
    doc.commit();
    postcard::to_stdvec(&neighbour_wire::NeighbourMsg::SyncResp {
        room: workspace.to_owned(),
        changes: doc
            .get_changes(&heads)
            .iter()
            .map(|change| change.raw_bytes().to_vec())
            .collect(),
    })
    .unwrap()
}

/// Every entry the store under `root` archived.
fn archived_bytes(root: &std::path::Path) -> Vec<Vec<u8>> {
    let mut found = Vec::new();
    let mut dirs = vec![
        root.join(crate::store::RETAINED_DIR)
            .join(crate::store::compaction::ARCHIVE_DIR),
    ];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if let Ok(bytes) = std::fs::read(&path) {
                found.push(bytes);
            }
        }
    }
    found
}

/// Whether `title` occurs in `frame`, a frame a device's replay delivers:
/// in its bytes as encoded for the wire, or in any operation of a workspace
/// document change it carries once decoded (a change's columns may be
/// compressed, which a search of the raw bytes alone would miss).
fn replayed_frame_carries(frame: &crate::transport::Envelope, title: &str) -> bool {
    let needle = title.as_bytes();
    let wire = crate::frame::encode(frame).unwrap();
    if wire.windows(needle.len()).any(|window| window == needle) {
        return true;
    }
    let crate::transport::Envelope::Delta { delta, .. } = frame else {
        return false;
    };
    let Some(
        crate::coedit::CoeditPayload::Checkpoint { body, .. }
        | crate::coedit::CoeditPayload::RetainedDelta { body, .. },
    ) = crate::coedit::CoeditPayload::decode(delta)
    else {
        return false;
    };
    let Ok(neighbour_wire::NeighbourMsg::SyncResp { changes, .. }) =
        postcard::from_bytes::<neighbour_wire::NeighbourMsg>(&body)
    else {
        return false;
    };
    changes.iter().any(|bytes| {
        automerge::Change::try_from(bytes.as_slice())
            .is_ok_and(|change| format!("{:?}", change.decode()).contains(title))
    })
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn a_member_admitted_later_is_replayed_no_window_withdrawn_before() {
    const TITLES: [&str; 2] = ["ssh prod-db", "~/private/taxes"];
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
        "the room shares A's terminals while they are open"
    );
    let body = windows_closed(&workspaces, &workspace, &a, a_salt).await;
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert!(
        titles_shared(&workspaces, &workspace).await.is_empty(),
        "the room's current state no longer holds A's terminals"
    );

    let (c, c_cert) = guest_device();
    door_join(&workspaces, &cert, &workspace, (&c, &c_cert), 21).await;
    // Whatever the workspace published is retained before C's replay is read.
    workspaces
        .inner
        .host
        .records
        .retained(created.workspace_id)
        .await
        .unwrap();
    let workspace = workspaces
        .inner
        .host
        .workspaces
        .lock()
        .await
        .get(&created.workspace_id)
        .cloned()
        .expect("the relay hosts the room's workspace");
    let (replayed, archived) = {
        let runtime = workspace.lock().await;
        (
            crate::store::test_support::replay_frames(&runtime.store),
            archived_bytes(runtime.store.root()),
        )
    };
    assert!(
        !replayed.is_empty(),
        "a device joining the room is replayed the room"
    );
    for title in TITLES {
        assert!(
            !replayed
                .iter()
                .any(|frame| replayed_frame_carries(frame, title)),
            "a member who joins later is replayed {title:?}, \
             a window withdrawn before they joined"
        );
        assert!(
            archived.iter().any(|bytes| {
                crate::frame::decode(bytes.get(4..).unwrap_or_default())
                    .is_ok_and(|frame| replayed_frame_carries(&frame, title))
            }),
            "the relay keeps {title:?} archived for those who held it"
        );
    }
    drop(workspaces);
    relay.shutdown().await;
}

/// A workspace becomes public in the directory: its arrangement stays what it
/// was. Open admission is not a decision about the arrangement: the room keeps
/// sharing its members' windows and strips across a restart, exactly as before
/// it was listed.
#[allow(
    clippy::significant_drop_tightening,
    reason = "fires only in some feature sets: the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn a_listed_room_keeps_sharing_its_windows_after_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
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
        "committed while the relay still took members' windows in"
    );
    // Listed in the directory: anyone may come in as an editor.
    workspaces
        .inner
        .listings
        .set(
            &workspace,
            Some(tilo_relay_protocol::directory::Listing {
                description: "Come in".into(),
                default_role: tilo_relay_protocol::workspaces::WorkspaceRole::Editor,
            }),
        )
        .unwrap();
    drop(workspaces);
    relay.shutdown().await;

    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    assert_eq!(
        titles_shared(&workspaces, &workspace).await.len(),
        2,
        "the room still holds its members' windows"
    );
    workspaces
        .absorb(&workspace, a.device_public_key, &body)
        .await
        .unwrap();
    assert_eq!(titles_shared(&workspaces, &workspace).await.len(), 2);
    drop(workspaces);
    relay.shutdown().await;
}

/// The Lounge, 2026-10-20: a laptop's join completed at the host, but the
/// laptop restarted before it installed the answer. By the time it resent
/// its retained proof, others had joined: the host no longer replays the
/// answer, says so with its code, and gives the device, a member now, the
/// workspace's current state through the membership exchange, which matches
/// what the device's retained proof expects.
#[tokio::test]
async fn a_join_the_host_completed_before_the_room_moved_on_is_finished_through_membership() {
    use tilo_delivery_protocol::workspace_join::WorkspaceJoinService as _;
    use tilo_peer_state::join::{
        JOIN_COMPLETED_REFRESH, JoinTransportRequest, JoinTransportResponse,
        MembershipChallengeRequest, MembershipCompleteRequest, sign_membership_proof,
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let created = workspaces.create("Lounge").await.unwrap();
    let (laptop, laptop_cert) = guest_device();
    let (complete, first) = door_join(
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
    let (_, joined) = door_join(
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
                room: created.workspace.clone(),
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
    assert_eq!(current.room, delivered.room);
    assert_eq!(current.workspace_id, delivered.workspace_id);
    assert_eq!(current.host, delivered.host);
    assert!(current.generation > 0);
    assert!(current.certified_roster.contains(&laptop_cert));
    assert!(current.certified_roster.contains(&other_cert));
    assert!(current.certified_roster.contains(&current.host));
    relay.shutdown().await;
}

/// A workspace that moves to its person's host keeps its workspace: the host
/// takes over governing the workspace under the identity it already has, and
/// refuses a workspace it holds already, rather than minting another.
#[tokio::test]
async fn a_room_moved_here_keeps_its_identity() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
    let workspace = "6f1c2b9e-4d3a-4f5e-8a7b-1c2d3e4f5a6b";
    let moved = workspaces
        .create_workspace("Mine", Some(workspace))
        .await
        .unwrap();
    assert_eq!(moved.workspace, workspace, "the room keeps its identity");
    assert_eq!(
        moved.workspace_id,
        crate::room_workspace_id(workspace).unwrap(),
        "and its workspace the id every device derives from it"
    );
    assert!(
        workspaces
            .create_workspace("Again", Some(workspace))
            .await
            .is_err(),
        "a room the host holds is never made a second time"
    );
    assert!(
        workspaces
            .create_workspace("Bad", Some("not a room"))
            .await
            .is_err()
    );
    let fresh = workspaces.create_workspace("Fresh", None).await.unwrap();
    assert_ne!(fresh.workspace, workspace);
    relay.shutdown().await;
}

/// A member whose person's root renewed its device certificate proves its
/// membership with the renewal: the relay holds the renewal in place of the
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
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces();
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
    let (_, first) = door_join(workspaces, &cert, &created.workspace, (&laptop, &held), 21).await;
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
                room: created.workspace.clone(),
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
    relay.shutdown().await;
}

/// Whether `workspace`'s publication was just started over (every entry a
/// checkpoint) rather than added to (its last entry a change), and how
/// many entries it holds.
async fn publication_shape(workspaces: &Workspaces, workspace: &str) -> (bool, usize) {
    let governed = workspaces.workspace(workspace).await.unwrap();
    let governed = governed.lock().await;
    let outgoing = &governed.record.outgoing;
    let shape = (
        outgoing.iter().all(|publication| publication.checkpoint),
        outgoing.len(),
    );
    drop(governed);
    shape
}

/// A crowd arriving through the door costs the workspace one change each, added
/// to what it publishes, not the whole workspace rebuilt, written and sent to
/// every member once per arrival. Anything other than an arrival in between
/// starts the workspace over at the next one, as before: a newcomer is never
/// given what was taken back before they came.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the relay stays held through the assertions; Relay::shutdown consumes it"
)]
#[tokio::test]
async fn a_crowd_joining_adds_to_the_room_rather_than_starting_it_over() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let workspaces = relay.workspaces().clone();
    let created = workspaces.create("Lounge").await.unwrap();
    let workspace = created.workspace.clone();

    // The first arrival starts the workspace over: nothing says yet that what it
    // published since its last start holds only arrivals.
    let (first, first_cert) = guest_device();
    door_join(&workspaces, &cert, &workspace, (&first, &first_cert), 1).await;
    let (started_over, mut entries) = publication_shape(&workspaces, &workspace).await;
    assert!(started_over, "the first arrival starts the room over");

    for seed in 2..=8 {
        let (guest, guest_cert) = guest_device();
        door_join(&workspaces, &cert, &workspace, (&guest, &guest_cert), seed).await;
        let (started_over, now) = publication_shape(&workspaces, &workspace).await;
        assert!(
            !started_over,
            "arrival {seed} after only arrivals adds to the publication"
        );
        assert!(now > entries, "arrival {seed} added its change");
        entries = now;
    }

    // A change that is not an arrival: the next arrival starts over.
    workspaces.rename(&workspace, "Lounge II").await.unwrap();
    let (late, late_cert) = guest_device();
    door_join(&workspaces, &cert, &workspace, (&late, &late_cert), 30).await;
    assert!(
        publication_shape(&workspaces, &workspace).await.0,
        "an arrival after another kind of change starts the room over"
    );

    // And arrivals after that start add to it again.
    let (next, next_cert) = guest_device();
    door_join(&workspaces, &cert, &workspace, (&next, &next_cert), 31).await;
    assert!(!publication_shape(&workspaces, &workspace).await.0);

    // Every arrival is a member.
    let members = workspaces.summary(&workspace).await.unwrap().members;
    for device in [&first_cert, &late_cert, &next_cert] {
        assert!(
            members
                .iter()
                .any(|member| member.user_public_key == device.user_public_key),
            "an arrival is a member of the room"
        );
    }
    drop(workspaces);
    relay.shutdown().await;
}

/// A host that renamed its own governor certificate — the same device key,
/// the same capabilities, a new label — serves the workspaces that
/// certificate already governs: a name is not what governs a workspace, and
/// refusing it made a host renamed by an upgrade unable to start.
#[tokio::test]
async fn a_host_serves_the_workspaces_its_renamed_certificate_governs() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = test_host(dir.path(), &key, &cert).await;
    let created = relay.workspaces().create("Demo").await.unwrap();
    relay.shutdown().await;

    // The same device and capabilities, under another name, signed again by
    // the workspace's owner root as a relay signs its own certificate.
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

    // Another device's certificate is still refused: only the label moved,
    // and a host whose certificate names a different key does not serve the
    // workspace.
    let other_root = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
    let other_key = ed25519_dalek::SigningKey::from_bytes(&[4; 32]);
    let mut other_cert = tilo_user_identity::DeviceCert {
        user_public_key: other_root.verifying_key().to_bytes(),
        device_public_key: other_key.verifying_key().to_bytes(),
        device_label: "another host".into(),
        capabilities: DeviceCapabilities::host(),
        issued_at_ns: 1,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    tilo_user_identity::sign_device_cert(&mut other_cert, &other_root).unwrap();
    let refused = WorkspaceHost::open(
        HostConfig {
            max_workspace_grants: None,
            ..HostConfig::new(dir.path().to_owned(), HostMode::CertifiedHost)
        },
        HostIdentity {
            user_public_key: other_cert.user_public_key,
            device_signing_key: other_key.clone(),
            device_cert: other_cert,
            revocations: RevocationList::new(),
        },
        iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
            .secret_key(iroh::SecretKey::from_bytes(&other_key.to_bytes()))
            .bind()
            .await
            .expect("a loopback endpoint binds"),
        None,
    )
    .await
    .expect_err("a host of another device does not open the workspace");
    assert!(
        refused
            .to_string()
            .contains("governed by a different device"),
        "{refused}"
    );
}

/// A room names its own host among its hosts, as its governor, so a member
/// reads where the room is served off the replica it already holds, and reads
/// it once.
#[tokio::test]
async fn a_room_names_its_own_host_to_every_member() {
    use std::time::Duration;
    use tilo_delivery_protocol::workspace_join::IrohWorkspaceHostHintV1;
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let relay = tokio::time::timeout(Duration::from_secs(120), test_host(dir.path(), &key, &cert))
        .await
        .expect("the host opens");
    let addr = relay.node_addr();
    let workspaces = relay.workspaces();
    let created = tokio::time::timeout(Duration::from_secs(120), workspaces.create("Demo"))
        .await
        .expect("the room is created")
        .unwrap();
    let governed = tokio::time::timeout(
        Duration::from_secs(120),
        workspaces.workspace(&created.workspace),
    )
    .await
    .expect("the room opens")
    .unwrap();
    let hosts = {
        let governed = governed.lock().await;
        governed.peer().governance().workspace_hosts()
    };
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    assert_eq!(hosts[0].host.device_public_key, cert.device_public_key);
    assert_eq!(
        IrohWorkspaceHostHintV1::from_transport_hint(&hosts[0])
            .unwrap()
            .endpoint_addr,
        addr
    );
    let served = tokio::time::timeout(
        Duration::from_secs(120),
        workspaces.hosts(&created.workspace),
    )
    .await
    .expect("the room names its hosts")
    .unwrap();
    assert_eq!(served.len(), 1, "the governor is named once");
    relay.shutdown().await;
}
