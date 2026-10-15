//! A member completing an app's bootstrap on the empty record the room's
//! governor (a host) created for it: an ordinary signed member change.
use super::*;
use crate::{ApplicationInitialization, ApplicationRecord};

const FRESH: &str = "fresh-record";

fn route() -> ApplicationRecord {
    ApplicationRecord::declared(APP, FRESH, "fixture.kind", &["fixture.kind".to_owned()]).unwrap()
}

/// The governor signs the record's empty genesis and the member's owner
/// admits it from the relay, as a host's answer to the member arrives.
fn governor_creates_empty_record(joined: &Enrolled) {
    let key = SigningKey::from_bytes(&[61; 32]);
    let (messages, governor) = joined
        .owner
        .with_current(joined.room, |view| {
            let before = view.peer().clone();
            let mut candidate = before.clone();
            candidate.publish_document(APP, FRESH, AutoCommit::new().save(), &key, joined.now)?;
            let messages = candidate.durable_mutation_messages(
                &before,
                &key,
                joined.now,
                view.generation().checked_add(1).unwrap(),
            )?;
            Ok((messages, view.governor().device_public_key))
        })
        .unwrap();
    for message in messages {
        joined
            .owner
            .receive(
                joined.room,
                TransportOrigin::RetainedRelayDelta(governor),
                message,
                joined.now,
            )
            .unwrap();
    }
}

fn captured(joined: &Enrolled) -> crate::CommittedDocument {
    joined
        .owner
        .with_current(joined.room, |view| {
            match view.read_document(
                member().verifying_key().to_bytes(),
                APP,
                FRESH,
                None,
                64 * 1024,
                joined.now,
            )? {
                DocumentRead::Captured(document) => Ok(document),
                _ => Err("fixture record capture unavailable".into()),
            }
        })
        .unwrap()
}

fn bootstrap(from: &crate::CommittedDocument) -> Vec<u8> {
    let mut document = AutoCommit::load(&from.bytes).unwrap();
    document.put(ROOT, "title", "First note").unwrap();
    document.commit_edit("fixture", "title");
    document.save()
}

/// A member enrolled in a room whose governor created the empty record:
/// the room, the member's certificate, the empty capture and the bootstrap
/// the app made from it.
fn member_with_empty_record() -> (
    Enrolled,
    tilo_user_identity::DeviceCert,
    crate::CommittedDocument,
    Vec<u8>,
) {
    let joined = enrolled();
    let creator = joined
        .owner
        .with_current(joined.room, |view| Ok(view.device().clone()))
        .unwrap();
    governor_creates_empty_record(&joined);
    let empty = captured(&joined);
    assert!(
        AutoCommit::load(&empty.bytes)
            .unwrap()
            .get_heads()
            .is_empty(),
        "the governor's genesis is empty"
    );
    let document = bootstrap(&empty);
    (joined, creator, empty, document)
}

fn outgoing(joined: (RoomId, SystemTime), owner: &NativeWorkspaceOwner) -> usize {
    owner
        .with_outgoing(joined.0, joined.1, |_, outgoing| {
            Ok(outgoing.entries().count())
        })
        .unwrap()
}

#[test]
fn a_member_bootstrap_needs_its_own_key_and_never_signs_a_snapshot() {
    let (joined, creator, empty, document) = member_with_empty_record();
    let route = route();
    let initialization = ApplicationInitialization {
        route: &route,
        revision: empty.revision,
        document: &document,
    };
    let queued = outgoing((joined.room, joined.now), &joined.owner);
    assert!(
        joined
            .owner
            .initialize_application_record(
                joined.room,
                &creator,
                &member(),
                initialization,
                joined.now
            )
            .is_err(),
        "a member never signs a governor's snapshot of the record"
    );
    assert!(
        joined
            .owner
            .initialize_application_record_as_member(
                joined.room,
                &creator,
                &SigningKey::from_bytes(&[61; 32]),
                initialization,
                joined.now,
            )
            .is_err(),
        "only the member's own key signs its bootstrap"
    );
    assert_eq!(captured(&joined).revision, empty.revision);
    assert_eq!(
        outgoing((joined.room, joined.now), &joined.owner),
        queued,
        "a refused bootstrap publishes nothing"
    );
}

#[test]
fn a_member_bootstrap_is_its_own_signed_change_onto_the_hosts_empty_record() {
    let (joined, creator, empty, document) = member_with_empty_record();
    let route = route();
    let initialization = ApplicationInitialization {
        route: &route,
        revision: empty.revision,
        document: &document,
    };
    let queued = outgoing((joined.room, joined.now), &joined.owner);

    let revision = joined
        .owner
        .initialize_application_record_as_member(
            joined.room,
            &creator,
            &member(),
            initialization,
            joined.now,
        )
        .unwrap();
    let written = captured(&joined);
    assert_eq!(revision, written.revision);
    assert_ne!(revision, empty.revision);
    assert_eq!(
        AutoCommit::load(&written.bytes)
            .unwrap()
            .get(ROOT, "title")
            .unwrap()
            .unwrap()
            .0
            .into_string()
            .as_deref()
            .ok(),
        Some("First note")
    );
    assert_eq!(
        outgoing((joined.room, joined.now), &joined.owner),
        queued + 1,
        "the bootstrap is one member write the relay receives"
    );
    let author = joined
        .owner
        .with_current(joined.room, |view| {
            Ok(view
                .peer()
                .replay_floors()
                .into_iter()
                .any(|(app, record, author, _)| {
                    app == APP && record == FRESH && author == creator.device_public_key
                }))
        })
        .unwrap();
    assert!(author, "the record holds the member's own signed change");

    assert_eq!(
        joined
            .owner
            .initialize_application_record_as_member(
                joined.room,
                &creator,
                &member(),
                initialization,
                joined.now,
            )
            .unwrap_err(),
        "application initialization revision changed",
        "a bootstrap against a superseded revision is refused"
    );
    drop(joined.owner);
    let reopened = open_file(&joined.path).unwrap();
    assert_eq!(
        outgoing((joined.room, joined.now), &reopened),
        queued + 1,
        "the member write is durable"
    );
}
