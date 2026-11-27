//! A workspace's store answers from the index its start read, without opening its documents.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a test failing is its assertion failing"
)]

use std::collections::BTreeMap;

use collab_types::store::{Cell, Resource, Value};
use tilo_resource_store::Stamp;

/// A workspace keeps its chain as parts of its store: 26 documents, a thousand
/// resources each. The process is killed rather than stopped, so nothing is kept as it
/// goes: a start reads each document's snapshot of the index, not the document, and a
/// read is answered from that index.
#[test]
fn a_store_a_hard_stop_left_answers_from_its_index() {
    /// Parts of the chain.
    const PARTS: usize = 26;
    /// Resources each part holds.
    const EACH: usize = 1_000;
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let mut resources = super::Resources::open(&file, id);
    let stamp = Stamp::default();
    for part in 0..PARTS {
        let kind = format!("issue{part}");
        let before = resources.heads(&kind);
        {
            let mut store = resources.store();
            for index in 0..EACH {
                let resource = Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: BTreeMap::from([(
                        "title".to_owned(),
                        Cell::One(Value::Text(format!("Issue {part}-{index}"))),
                    )]),
                };
                store
                    .put(&kind, &format!("i{part}-{index}"), resource, &stamp)
                    .unwrap();
            }
            drop(store);
        }
        let doc = format!("*/{kind}#0");
        resources.kept(&doc, &before).unwrap();
    }
    // A hard stop: the process is killed, so nothing writes the store out.
    std::mem::forget(resources);

    let started = std::time::Instant::now();
    let reopened = super::Resources::open(&file, id);
    let opened = started.elapsed();
    let store = reopened.store();
    assert_eq!(
        store.in_memory(),
        0,
        "a start reads the index's snapshots, not the store's documents"
    );
    assert_eq!(
        store.count("issue13", &[], &|_| true),
        EACH,
        "the index answers for a part of the chain"
    );
    assert!(
        store
            .get("issue13", "i13-7", &|_| true)
            .is_some_and(|held| held.scalar("title") == Value::Text("Issue 13-7".into())),
        "a resource of a part reads from the index"
    );
    assert_eq!(store.in_memory(), 0, "reading it opened no document");
    drop(store);
    assert!(
        opened < std::time::Duration::from_secs(2),
        "the start took {opened:?}"
    );
}

/// A batch of writes to one record keeps each write as it is taken and the record's snapshot
/// once, as the batch ends: a process killed inside the batch starts again with every write it
/// took, read from the document, and one killed after it starts from the snapshot.
#[test]
fn a_batch_keeps_each_write_and_the_snapshot_once() {
    const WRITES: usize = 50;
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let kind = "task";
    let doc = format!("*/{kind}#0");
    let put = |resources: &mut super::Resources, index: usize| {
        let before = resources.heads(&doc);
        {
            let resource = Resource {
                readers: "*".into(),
                parent: None,
                order: None,
                fields: BTreeMap::from([(
                    "title".to_owned(),
                    Cell::One(Value::Text(format!("Task {index}"))),
                )]),
            };
            resources
                .store()
                .put(kind, &format!("t{index}"), resource, &Stamp::default())
                .unwrap();
        }
        resources.kept(&doc, &before).unwrap();
    };
    let count = |resources: &super::Resources| resources.store().count(kind, &[], &|_| true);

    let mut resources = super::Resources::open(&file, id);
    put(&mut resources, 0);
    resources.batch();
    for index in 1..WRITES {
        put(&mut resources, index);
    }
    // Killed inside the batch.
    std::mem::forget(resources);
    let mut reopened = super::Resources::open(&file, id);
    assert_eq!(
        count(&reopened),
        WRITES,
        "every write taken is there after the start"
    );

    reopened.batch();
    put(&mut reopened, WRITES);
    reopened.batch_kept();
    std::mem::forget(reopened);
    let reopened = super::Resources::open(&file, id);
    let (in_memory, kept) = {
        let store = reopened.store();
        (store.in_memory(), store.count(kind, &[], &|_| true))
    };
    assert_eq!(in_memory, 0, "the start read the snapshot the batch kept");
    assert_eq!(kept, WRITES + 1);
}

/// A document whose id is longer than a file system names one file is kept, and read again by
/// the next start, as any other is.
#[test]
fn a_document_with_a_long_id_is_kept_across_a_start() {
    let dir = tempfile::tempdir().unwrap();
    let id = tilo_server_protocol::WorkspaceId::new();
    let file = dir.path().join("workspace.workspace");
    let kind = "so.tilo.example.a-kind-with-a-long-name.".repeat(8);
    let mut resources = super::Resources::open(&file, id);
    let doc = {
        let mut store = resources.store();
        let resource = Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: BTreeMap::from([(
                "title".to_owned(),
                Cell::One(Value::Text("Long named".into())),
            )]),
        };
        store.put(&kind, "p1", resource, &Stamp::default()).unwrap();
        store.index().get(&kind, "p1").unwrap().doc.clone()
    };
    assert!(
        hex::encode(&doc).len() > 255,
        "the id is longer than a name"
    );
    resources.kept(&doc, &[]).unwrap();
    std::mem::forget(resources);

    let reopened = super::Resources::open(&file, id);
    let store = reopened.store();
    let read = store
        .get(&kind, "p1", &|_| true)
        .is_some_and(|held| held.scalar("title") == Value::Text("Long named".into()));
    drop(store);
    assert!(read, "the resource reads again after the start");
}

/// The person `person` as a stamp names its invoker.
fn as_person(person: [u8; 32]) -> tilo_member_types::Actor {
    tilo_member_types::Actor::Member(tilo_member_types::MemberId::person(hex::encode(person)))
}

/// A platform write's stamp: `verb`, naming `actor` as its invoker, or who it acted for.
fn platform_stamp(
    verb: &str,
    actor: Option<tilo_member_types::Actor>,
    acted_for: Option<[u8; 32]>,
) -> Stamp {
    Stamp {
        invoked: String::new(),
        verb: verb.to_owned(),
        invoker: actor.map(|actor| Stamp::by(&actor)).unwrap_or_default(),
        acted_for: acted_for.map(hex::encode),
        ..Stamp::default()
    }
}

/// A call the person `person`'s device `key` signed, as a host keeps it beside a change it
/// carried out for them.
fn signed_call(person: [u8; 32]) -> Box<tilo_actions::remote::SignedCall> {
    use ed25519_dalek::Signer as _;
    use tilo_actions::remote::signed_invocation::InvocationStatement;
    use tilo_actions::remote::{SignedCall, SignedInvocation};
    use tilo_user_identity::{DeviceCapabilities, DeviceCert};
    let key = ed25519_dalek::SigningKey::from_bytes(&[7; 32]);
    let statement = InvocationStatement {
        workspace: "w".to_owned(),
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "test.provider".to_owned(),
            address: tilo_actions::ActionAddress::new("record", "action"),
        },
        arguments: [0; 32],
        on_behalf_of: None,
        issued_at_ns: 0,
        signer: key.verifying_key().to_bytes(),
    };
    let signature = key.sign(&statement.signing_message());
    let cert = DeviceCert {
        user_public_key: person,
        device_public_key: key.verifying_key().to_bytes(),
        device_label: "test".to_owned(),
        capabilities: DeviceCapabilities::member(),
        issued_at_ns: 0,
        expires_at_ns: None,
        signature: [0; 64],
        issuer: None,
    };
    Box::new(SignedCall {
        signed: SignedInvocation::new(
            statement,
            signature.to_bytes(),
            postcard::to_allocvec(&cert).expect("a certificate writes"),
        ),
        argument: None,
        allowed: None,
    })
}

/// A platform verb's wider write belongs to the device or person the invocation names: the
/// author's own person, or a signed call by the named person. Another member's device naming
/// them is refused, and a verb that is no platform verb belongs to whoever wrote it.
#[test]
fn a_platform_verb_belongs_to_the_person_the_invocation_names() {
    let person = [0xab; 32];
    let other = [0xcd; 32];
    for verb in tilo_resource_store::PLATFORM_VERBS {
        let own = platform_stamp(verb, Some(as_person(person)), None);
        assert!(
            super::platform_write_belongs(Some(&own), Some(person)),
            "{verb} from the person it names"
        );
        let foreign = platform_stamp(verb, Some(as_person(other)), None);
        assert!(
            !super::platform_write_belongs(Some(&foreign), Some(person)),
            "{verb} naming another member is no device of the author's"
        );
        let acted = platform_stamp(verb, None, Some(person));
        assert!(
            super::platform_write_belongs(Some(&acted), Some(person)),
            "{verb} made for the person it acted for"
        );
        // A host that carries the person's call writes as itself, and keeps their signature.
        let mut signed = platform_stamp(verb, Some(as_person(person)), None);
        signed.signed = Some(signed_call(person));
        assert!(
            super::platform_write_belongs(Some(&signed), Some(other)),
            "{verb} carried for the person by their signed call"
        );
        // A signature by someone other than the named person is no call of theirs.
        let mut forged = platform_stamp(verb, Some(as_person(person)), None);
        forged.signed = Some(signed_call(other));
        assert!(
            !super::platform_write_belongs(Some(&forged), Some(other)),
            "{verb} whose signature is not the named person's"
        );
    }
    // A verb the platform does not write under is the author's own, whatever it names.
    let own_verb = platform_stamp("type", Some(as_person(other)), None);
    assert!(super::platform_write_belongs(Some(&own_verb), Some(person)));
}
