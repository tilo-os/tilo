//! The person's own records on the runtime's routes: a widget's records
//! kept beside its document are published on their own route, in the
//! person's room, and merged from the person's other devices.

use super::*;
use crate::record::person::{PersonRecord, PersonRecords};
use crate::record::{RecordKind, RecordOwners};
use automerge::ReadDoc as _;
use automerge::transaction::Transactable as _;
use std::rc::Rc;

const APP: &str = "so.tilo.test-person";
const ROOT_KEY: [u8; 32] = [42; 32];

fn person() -> crate::permissions::UserId {
    crate::permissions::UserId(hex::encode(ROOT_KEY))
}

/// One device's copy of the person's watched list.
fn device() -> PersonRecord {
    let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    PersonRecords::open(&mut owners, APP, person())
        .unwrap()
        .open_record(RecordKind::new("test.watched"), |doc| {
            doc.put_object(automerge::ROOT, "watched", automerge::ObjType::Map)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .unwrap()
}

fn watch(record: &PersonRecord, video: &str) {
    record
        .edit(true, |doc| {
            let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
            doc.put(&watched, video, true)
                .map_err(|error| error.to_string())
        })
        .unwrap();
}

fn watched(record: &PersonRecord) -> Vec<String> {
    record
        .read(|doc| {
            let watched = doc.get(automerge::ROOT, "watched").unwrap().unwrap().1;
            doc.keys(&watched).collect()
        })
        .unwrap()
}

/// A view that keeps the person's record open beside what it shows.
struct Holds {
    text: crate::widget::Text,
    owner: crate::history_mode::HistoryOwner,
}

impl crate::widget::Decorator for Holds {
    type Inner = crate::widget::Text;
    fn inner(&self) -> &crate::widget::Text {
        &self.text
    }
    fn inner_mut(&mut self) -> &mut crate::widget::Text {
        &mut self.text
    }
    fn record_owners(&self) -> Vec<crate::history_mode::HistoryOwner> {
        vec![self.owner.clone()]
    }
}

fn app_holding(record: &PersonRecord) -> App {
    let mut app = App::new_with_controlled_inputs(
        State::new(),
        Box::new(Holds {
            text: crate::widget::Text::new("shown"),
            owner: record.history_owner(),
        }),
    );
    app.collaboration.manager = CollaborationManager::connected_for_lifecycle_test();
    // The app shows a record of a shared room; the person's record is not in it.
    app.actions.bound_broker_route_for_test = Some(crate::collaboration::PrimaryRouteWitness {
        route: crate::collaboration::RouteSpec::record(APP, uuid::Uuid::new_v4(), "shown"),
        epoch: uuid::Uuid::new_v4(),
    });
    app.refresh_document_routes();
    // The broker admits each route join asynchronously, and a route refuses
    // documents until its document mode is resolved.
    let deadline = tilo_test_env::hang_deadline();
    while !app.document_routes.owners.iter().all(|entry| {
        app.collaboration
            .manager
            .document_route_publishable_for_test(&entry.route)
    }) {
        assert!(
            crate::time::now() < deadline,
            "the record's route never opened"
        );
        std::thread::sleep(crate::time::Duration::from_millis(5));
    }
    app
}

fn snapshots(app: &mut App) -> Vec<(crate::collaboration::RouteSpec, Vec<u8>)> {
    std::mem::take(&mut app.document_routes.sent)
        .into_iter()
        .filter_map(|(route, request)| match request {
            CollabRequest::Document {
                mutation: collab_types::DocumentMutation::Snapshot(bytes),
                ..
            } => Some((route, bytes)),
            _ => None,
        })
        .collect()
}

fn from_device(
    route: &crate::collaboration::RouteSpec,
    document: Vec<u8>,
) -> collab_types::SignedOp {
    collab_types::SignedOp::new_unsigned(
        collab_types::DocumentMutation::Snapshot(document)
            .encode()
            .unwrap(),
        collab_types::document_artifact_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::Ed25519PublicKey::from_bytes([0; 32]),
        collab_types::document_attestation_id(
            &route.app_id,
            &route.cluster_id.to_string(),
            route.record.as_deref().unwrap_or(""),
        ),
        collab_types::OpClass::Splice,
        crate::time::SystemTime::UNIX_EPOCH,
        0,
    )
}

#[test]
fn a_persons_record_rides_its_own_route_in_their_room_and_converges_across_devices() {
    let laptop = device();
    let phone = device();
    watch(&laptop, "on-the-laptop");
    watch(&phone, "on-the-phone");
    let mut laptop_app = app_holding(&laptop);
    let mut phone_app = app_holding(&phone);

    // Each device publishes its record on the record's own route, in the
    // person's room, never on the route of the record it shows.
    laptop_app.publish_owned_documents();
    let sent = snapshots(&mut laptop_app);
    assert_eq!(sent.len(), 1, "{sent:?}");
    let (route, laptop_document) = sent.into_iter().next().unwrap();
    assert_eq!(route.app_id, APP);
    assert_eq!(route.cluster_id, collab_types::person_room(&ROOT_KEY));
    assert_eq!(route.record.as_deref(), Some(laptop.checkout().id.as_str()));
    assert_eq!(laptop.route(), Some(route.clone()));
    phone_app.publish_owned_documents();
    let (phone_route, _) = snapshots(&mut phone_app).into_iter().next().unwrap();
    assert_eq!(phone_route, route, "both devices meet on one record");

    // The phone receives the laptop's record: it merges, and having
    // something the laptop lacks, answers with its own.
    phone_app.apply_signed_document_envelope(&from_device(&route, laptop_document), None);
    assert_eq!(watched(&phone), ["on-the-laptop", "on-the-phone"]);
    phone_app.publish_owned_documents();
    let answer = snapshots(&mut phone_app);
    assert_eq!(
        answer.len(),
        1,
        "the phone answers a snapshot it is ahead of"
    );

    // The laptop takes the answer and is then level: nothing more to send.
    let (_, answer) = answer.into_iter().next().unwrap();
    laptop_app.apply_signed_document_envelope(&from_device(&route, answer), None);
    assert_eq!(watched(&laptop), watched(&phone));
    laptop_app.publish_owned_documents();
    assert!(
        snapshots(&mut laptop_app).is_empty(),
        "a device level with the snapshot it received does not answer it"
    );

    // The person's own undo reaches their edit, merged state and all.
    watch(&laptop, "later");
    assert!(laptop.undo());
    assert_eq!(watched(&laptop), ["on-the-laptop", "on-the-phone"]);
    assert!(laptop.redo());
    assert!(watched(&laptop).contains(&"later".to_owned()));
}

#[test]
fn without_a_root_identity_the_persons_records_stay_on_the_device() {
    let mut owners = RecordOwners::with_store(Rc::new(durable_store::MemoryStore::new()));
    let records =
        PersonRecords::open(&mut owners, APP, crate::permissions::UserId("local".into())).unwrap();
    assert_eq!(records.room(), None);
    let record = records
        .open_record(RecordKind::new("test.watched"), |doc| {
            doc.put_object(automerge::ROOT, "watched", automerge::ObjType::Map)
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .unwrap();
    watch(&record, "kept-here");
    assert_eq!(record.route(), None, "no route names a device-only record");
    let mut app = app_holding(&record);
    app.publish_owned_documents();
    assert!(app.document_routes.sent.is_empty());
}
