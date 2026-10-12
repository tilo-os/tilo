use super::{NativeAppOpenAccess, native_app_open_access, native_launch_capabilities};
use crate::shell::native_workspace_owner::{NativeWorkspaceOwner, test_enrolled_owner};
use crate::shell::state::RoomId;
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::TransportOrigin;

/// What one signed governance change writes.
enum Governed {
    Grant(tilo_action_policy::Grant),
}

fn signed_governance(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    change: &Governed,
) {
    let key = SigningKey::from_bytes(&[61; 32]);
    let messages = owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            match change {
                Governed::Grant(grant) => {
                    candidate.put_governed_grants(std::slice::from_ref(grant), &key, now)?;
                }
            }
            candidate.durable_mutation_messages(
                &before,
                &key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
}

#[test]
fn native_viewer_only_reopens_real_record_without_mutation() {
    let (_directory, owner, room, _now, _key) = test_enrolled_owner();
    let (certificate, generation, bytes) = owner
        .with_current(room, |view| {
            Ok((
                view.device().clone(),
                view.generation(),
                view.peer().document("so.tilo.chat", "").unwrap(),
            ))
        })
        .unwrap();
    assert!(matches!(
        native_app_open_access(&owner, room, &certificate, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Viewer
    ));
    assert!(native_app_open_access(&owner, room, &certificate, "so.tilo.missing").is_err());
    let mut forged = certificate;
    forged.device_label.push_str(" forged");
    assert!(native_app_open_access(&owner, room, &forged, "so.tilo.chat").is_err());
    owner
        .with_current(room, |view| {
            assert_eq!(view.generation(), generation);
            assert_eq!(view.peer().document("so.tilo.chat", "").unwrap(), bytes);
            Ok(())
        })
        .unwrap();
}

#[test]
fn native_existing_writer_preserves_delegation_but_person_restriction_wins() {
    let (_directory, owner, room, now, _key) = test_enrolled_owner();
    let (certificate, person, room_node) = owner
        .with_current(room, |view| {
            let member = view
                .peer()
                .permissions(view.device().device_public_key, now)?;
            Ok((view.device().clone(), member.person, member.policy.node()))
        })
        .unwrap();
    signed_governance(
        &owner,
        room,
        now,
        &Governed::Grant(tilo_action_policy::roles::app_authorship(
            &room.to_string(),
            "so.tilo.chat",
            tilo_action_policy::Subject::person(person.clone()),
        )),
    );
    assert!(matches!(
        native_app_open_access(&owner, room, &certificate, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Author
    ));
    signed_governance(
        &owner,
        room,
        now,
        &Governed::Grant(tilo_action_policy::Grant::new(
            "native-open-person-viewer",
            tilo_action_policy::Subject::person(person.clone()),
            room_node,
            tilo_action_policy::KindSet::UNDOABLE,
            tilo_action_policy::Answer::Deny,
            tilo_action_policy::Subject::person(person),
        )),
    );
    assert!(matches!(
        native_app_open_access(&owner, room, &certificate, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Viewer
    ));
}

#[test]
fn a_distinct_admitted_read_only_device_never_borrows_the_governors_app_write() {
    let (_directory, owner, room, now, _key) = test_enrolled_owner();
    signed_governance(
        &owner,
        room,
        now,
        &Governed::Grant(tilo_action_policy::roles::app_authorship(
            &room.to_string(),
            "so.tilo.chat",
            tilo_action_policy::Subject::person("governor"),
        )),
    );
    let signing_identity = SigningKey::from_bytes(&[72; 32]);
    let viewer_key = SigningKey::from_bytes(&[71; 32]);
    let mut viewer =
        tilo_joined_room::testing::certificate(&signing_identity, &viewer_key, "Read-only device");
    viewer.capabilities = tilo_user_identity::DeviceCapabilities::member();
    viewer.capabilities.foreign = true;
    tilo_user_identity::sign_device_cert(&mut viewer, &signing_identity).unwrap();
    admit_viewer(&owner, room, now, &viewer, &viewer_key);
    let governor = owner
        .with_current(room, |view| {
            assert_ne!(view.device(), &viewer);
            assert_ne!(view.governor(), &viewer);
            assert_ne!(view.governor().user_public_key, viewer.user_public_key);
            assert!(view.peer().certificates().contains(&viewer));
            assert!(
                view.peer()
                    .check_workspace_administration(viewer.device_public_key, now)
                    .is_err()
            );
            assert!(
                view.peer()
                    .check_workspace_administration(view.governor().device_public_key, now)
                    .is_ok()
            );
            Ok(view.governor().clone())
        })
        .unwrap();
    assert!(matches!(
        native_app_open_access(&owner, room, &governor, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Author
    ));
    assert!(matches!(
        native_app_open_access(&owner, room, &viewer, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Viewer
    ));
    assert!(native_app_open_access(&owner, room, &viewer, "so.tilo.missing").is_err());
    let grants = owner
        .with_current(room, |view| {
            Ok(view
                .peer()
                .permissions(governor.device_public_key, now)?
                .policy
                .grants)
        })
        .unwrap();
    let mut workspace = tilo_desktop_model::Workspace::new(
        tilo_desktop_model::WorkspaceId(71),
        "Admitted viewer launch",
    );
    workspace.set_room(room);
    workspace.policy.grants = grants;
    let seconds = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!(workspace.launch_grants(seconds).authors("so.tilo.chat"));
    let constrained =
        native_launch_capabilities(&workspace, "so.tilo.chat", NativeAppOpenAccess::Viewer);
    assert!(!constrained.authors("so.tilo.chat"));
}

fn admit_viewer(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    viewer: &tilo_user_identity::DeviceCert,
    viewer_key: &SigningKey,
) {
    use tilo_peer_state::join::{self, JoinTransportRequest, JoinTransportResponse};
    let governor_key = SigningKey::from_bytes(&[61; 32]);
    let certificate = owner
        .with_current(room, |view| Ok(view.governor().clone()))
        .unwrap();
    let mut governor = tilo_joined_room::testing::governor::TestGovernor::empty_interactive(
        certificate,
        governor_key.clone(),
        &room.to_string(),
    )
    .unwrap();
    let hint = governor.hint(iroh_base::EndpointAddr::new(
        iroh_base::EndpointId::from_bytes(&governor_key.verifying_key().to_bytes()).unwrap(),
    ));
    let now_ns = u64::try_from(
        now.duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    )
    .unwrap();
    let invitation = governor.issue(
        vec![hint],
        join::MemberRole::Viewer,
        Some(viewer.user_public_key),
        None,
        now_ns.checked_add(60_000_000_000).unwrap(),
    );
    let JoinTransportResponse::Challenge(response) = governor.handle(
        viewer.device_public_key,
        JoinTransportRequest::Challenge(join::JoinChallengeRequest {
            signed_invitation: invitation,
            device_cert: viewer.clone(),
            request_id: [74; 32],
        }),
    ) else {
        panic!("actual signed viewer challenge refused")
    };
    let proof = join::sign_join_proof(response.challenge.clone(), viewer_key).unwrap();
    let redemption = join::verify_join_proof(
        &proof,
        &response.challenge,
        viewer,
        &tilo_user_identity::RevocationList::new(),
        now_ns,
    )
    .unwrap();
    let messages = owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            assert!(candidate.admit_invited_member(&redemption, &governor_key, now)?);
            candidate.durable_mutation_messages(
                &before,
                &governor_key,
                now,
                view.generation().checked_add(1).unwrap(),
            )
        })
        .unwrap();
    for message in messages {
        owner
            .receive(
                room,
                TransportOrigin::RetainedRelayDelta(governor_key.verifying_key().to_bytes()),
                message,
                now,
            )
            .unwrap();
    }
}
