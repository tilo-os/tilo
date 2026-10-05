use super::{NativeAppOpenAccess, native_app_open_access};
use crate::shell::native_workspace_owner::{NativeWorkspaceOwner, test_enrolled_owner};
use crate::shell::state::RoomId;
use collab_types::caps::{CapOverride, Effect, Scope, Subject, Verb};
use ed25519_dalek::SigningKey;
use std::time::SystemTime;
use tilo_peer_state::TransportOrigin;

fn signed_governance(
    owner: &NativeWorkspaceOwner,
    room: RoomId,
    now: SystemTime,
    cap: CapOverride,
) {
    let key = SigningKey::from_bytes(&[61; 32]);
    let messages = owner
        .with_current(room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            candidate.put_governed_capabilities(&[cap], &key, now)?;
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
    let (certificate, person) = owner
        .with_current(room, |view| {
            let (_, _, person) = view
                .peer()
                .permissions(view.device().device_public_key, now)?;
            Ok((view.device().clone(), person))
        })
        .unwrap();
    signed_governance(
        &owner,
        room,
        now,
        CapOverride::grant(Subject::app("so.tilo.chat"), Scope::Workspace, Verb::Write)
            .with_id("native-open-chat-author"),
    );
    assert!(matches!(
        native_app_open_access(&owner, room, &certificate, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Author
    ));
    signed_governance(
        &owner,
        room,
        now,
        CapOverride::new(
            Subject::person(person),
            Scope::Workspace,
            Effect::Restrict,
            Verb::Read,
        )
        .with_id("native-open-person-viewer"),
    );
    assert!(matches!(
        native_app_open_access(&owner, room, &certificate, "so.tilo.chat").unwrap(),
        NativeAppOpenAccess::Viewer
    ));
}
