use super::*;
use crate::portable_storage::PortableStorage;
use crate::record::{RecordId, RecordKind, RecordOwners, author_writer};
use automerge::ReadDoc;
use automerge::transaction::Transactable;

const APP: &str = "so.tilo.test";

/// A mount of `checkout` for `author`, as `HostContext::callback_binding`
/// makes one.
fn mount(
    records: &RecordStore,
    checkout: &RecordCheckout,
    author: &str,
    instance: u64,
    writes_as_author: bool,
) -> (CallbackBinding, crate::WakeScope, Rc<()>) {
    let scope = crate::WakeScope::new();
    let owner = Rc::new(());
    let binding = CallbackBinding {
        lifetime: scope.handle(),
        closing: Rc::new(Cell::new(false)),
        registry_owner: Rc::downgrade(&owner),
        lifetime_id: uuid::Uuid::new_v4(),
        control_domain: crate::control_custody::WeakControlDomain::default(),
        queued_controls: Rc::new(RefCell::new(std::collections::VecDeque::new())),
        search_requests: Rc::default(),
        instance_id: instance,
        app_id: APP.into(),
        records: records.clone(),
        checkout: checkout.clone(),
        permissions: Rc::new(RefCell::new(PermissionContext::permissive())),
        author: UserId(author.into()),
        operation_context: tilo_actions::ActionContext::default(),
        writes_as_author,
    };
    (binding, scope, owner)
}

/// The actors of the changes `write` adds to the record.
fn writers_of(
    records: &RecordStore,
    id: &RecordId,
    write: impl FnOnce(),
) -> Vec<automerge::ActorId> {
    let before = records.with(id, |doc| doc.get_heads()).unwrap();
    write();
    records
        .with(id, |doc| {
            doc.get_changes(&before)
                .iter()
                .map(|change| change.actor_id().clone())
                .collect()
        })
        .unwrap()
}

/// Mounts of one profile and device share a record's document; each writes
/// as its author's own actor, the record keeps its writer for everything
/// else and for storage, and a reopened record's author writes as before.
#[test]
fn per_author_mounts_write_as_their_authors_and_the_record_keeps_its_writer() {
    let cache = PortableStorage::new("profile", "device").unwrap();
    let checkout = RecordCheckout::new(RecordId::minted(), RecordKind::new("note"));
    let (writer, you_writer) = {
        let records = RecordOwners::portable(cache.clone())
            .open(APP, "room")
            .unwrap();
        records.create_record(&checkout).unwrap();
        let writer = records
            .read(&checkout.id, |doc| doc.get_actor().clone())
            .unwrap();
        let you_writer = author_writer(&writer, "You");
        let agent_writer = author_writer(&writer, "Reading agent");
        assert_ne!(you_writer, agent_writer);
        assert_ne!(you_writer, writer);

        let (you, _you_scope, _you_owner) = mount(&records, &checkout, "You", 1, true);
        let (agent, _agent_scope, _agent_owner) =
            mount(&records, &checkout, "Reading agent", 2, true);
        let (host, _host_scope, _host_owner) = mount(&records, &checkout, "You", 3, false);
        let mut outer = AutoCommit::new();
        let id = &checkout.id;
        let typed = writers_of(&records, id, || {
            you.with_document(&mut outer, |doc, _| doc.put(ROOT, "you", 1).unwrap())
                .unwrap();
        });
        assert_eq!(typed, std::slice::from_ref(&you_writer));
        let answered = writers_of(&records, id, || {
            agent
                .with_document(&mut outer, |doc, _| doc.put(ROOT, "agent", 1).unwrap())
                .unwrap();
        });
        assert_eq!(answered, std::slice::from_ref(&agent_writer));
        let undone = writers_of(&records, id, || {
            you.history_request(&mut outer, crate::undo::UndoRequest::Undo)
                .unwrap()
                .unwrap();
        });
        assert!(
            !undone.is_empty() && undone.iter().all(|actor| actor == &you_writer),
            "an undo is its author's too: {undone:?}"
        );
        let hosted = writers_of(&records, id, || {
            host.with_document(&mut outer, |doc, _| doc.put(ROOT, "host", 1).unwrap())
                .unwrap();
        });
        assert_eq!(
            hosted,
            std::slice::from_ref(&writer),
            "a mount without per-author writers writes as the record"
        );
        records
            .read(id, |doc| {
                assert_eq!(
                    doc.get_actor(),
                    &writer,
                    "outside a loan, the record's writer"
                );
                assert!(doc.get(ROOT, "you").unwrap().is_none());
                assert!(doc.get(ROOT, "agent").unwrap().is_some());
            })
            .unwrap();
        records.flush();
        (writer, you_writer)
    };

    let records = RecordOwners::portable(cache).open(APP, "room").unwrap();
    records.open_existing_record_checked(&checkout).unwrap();
    assert_eq!(
        records
            .read(&checkout.id, |doc| doc.get_actor().clone())
            .unwrap(),
        writer,
        "the store keeps the record's writer"
    );
    let (you, _scope, _owner) = mount(&records, &checkout, "You", 4, true);
    let mut outer = AutoCommit::new();
    let again = writers_of(&records, &checkout.id, || {
        you.with_document(&mut outer, |doc, _| doc.put(ROOT, "you", 2).unwrap())
            .unwrap();
    });
    assert_eq!(again, [you_writer], "the same author writes as before");
}
