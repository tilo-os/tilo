use super::*;
use crate::resource::{DocumentId, ResourceOwners, ResourceType, author_writer};
use automerge::ReadDoc;
use automerge::transaction::Transactable;

const APP: &str = "so.tilo.test";
const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000ab";

tilo_test_env::scenario! {
    fn record_writes_keep_their_author_and_restore_the_scope;

    phase publication_retains_original_typed_identity_not_a_supplied_alias;
    {
                let records = ResourceOwners::in_memory()
            .open(APP, "workspace")
            .unwrap();
        let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        records.create_resource(&checkout).unwrap();
        let (binding, _scope, _owner) = mount(&records, &checkout, "You", 1, true);
        let publication = BoundPublication::default();
        let mut resource = tilo_actions::ResourceSpec::new("app:so.tilo.chat", "store.app", "Chat");
        resource.semantic_identity = tilo_actions::ResourceSemanticIdentity::new("person", "forged");
        resource.children.push(tilo_actions::ResourceSpec::new(
            "chat.person:scribe",
            "chat.agent",
            "An unrelated display name",
        ));
        let projected = publication.project(&binding, resource).unwrap();
        assert!(projected.id.as_str().starts_with(PUBLICATION_PREFIX));
        assert_eq!(
            projected.semantic_identity,
            tilo_actions::ResourceSemanticIdentity::new("store.app", "app:so.tilo.chat")
        );
        assert_eq!(
            projected.children.first().unwrap().semantic_identity,
            tilo_actions::ResourceSemanticIdentity::new("chat.agent", "chat.person:scribe")
        );
        assert!(
            publication
                .target(&tilo_actions::ActionAddress::new(
                    "app:so.tilo.chat",
                    "open"
                ))
                .unwrap()
                .is_none(),
            "descriptive identity must not become a dispatch-address fallback"
        );
        assert!(
            publication
                .target(&tilo_actions::ActionAddress::new(
                    projected.id.as_str(),
                    "open"
                ))
                .unwrap()
                .is_some()
        );
        binding.closing.set(true);
        assert!(
            publication
                .project(
                    &binding,
                    tilo_actions::ResourceSpec::new("x", "store.app", "X")
                )
                .is_err()
        );
    }

    phase trusted_person_scope_rejects_reattribution_and_restores_after_nested_callbacks;
    {
        use crate::invocation_scope::InvokerScope;
        use tilo_actions::ActionInvoker;

                let records = ResourceOwners::in_memory()
            .open(APP, "workspace")
            .unwrap();
        let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        records.create_resource(&checkout).unwrap();
        let (mut binding, _scope, _owner) = mount(&records, &checkout, "person", 1, true);
        binding.author = MemberId::person("agent");
        std::rc::Rc::make_mut(&mut binding.operation_context).invoker =
            Some(ActionInvoker::person("person"));
        let mut outer = AutoCommit::new();
        assert_eq!(executing_person(), None);
        assert!(!local_person_invocation_permitted());
        binding
            .with_resource_frame(&mut outer, false, |doc, _| {
                assert_eq!(executing_person(), Some(MemberId::person("person")));
                assert!(local_person_invocation_permitted());
                {
                    let _invoker = InvokerScope::enter(ActionInvoker::person("agent"));
                    assert!(!local_person_invocation_permitted());
                }
                {
                    let _invoker = InvokerScope::enter(ActionInvoker::person("person"));
                    assert!(local_person_invocation_permitted());
                }
                let mut nested = binding.clone();
                nested.local_person = MemberId::person("other-person");
                nested
                    .with_resource_frame(doc, false, |_, _| {
                        assert_eq!(executing_person(), Some(MemberId::person("other-person")));
                    })
                    .unwrap();
                assert_eq!(executing_person(), Some(MemberId::person("person")));
            })
            .unwrap();
        assert_eq!(executing_person(), None);
        assert!(!local_person_invocation_permitted());

        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = binding.with_resource_frame(&mut outer, false, |_, _| {
                assert_eq!(executing_person(), Some(MemberId::person("person")));
                panic!("callback unwound");
            });
        }));
        assert!(panic.is_err());
        assert_eq!(executing_person(), None);
        assert!(!local_person_invocation_permitted());
    }

    phase checked_record_preview_attribution_uses_admitted_actor_and_restores_on_unwind;
    {
        use crate::invocation_scope::InvokerScope;
        use tilo_actions::ActionInvoker;
        use tilo_resource::ActionCx;

                let records = ResourceOwners::in_memory()
            .open(APP, "workspace")
            .unwrap();
        let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        records.create_resource(&checkout).unwrap();
        let (mut binding, _scope, _owner) = mount(&records, &checkout, "person", 1, true);
        std::rc::Rc::make_mut(&mut binding.operation_context).invoker =
            Some(ActionInvoker::person("untrusted-label"));
        let mut outer = AutoCommit::new();
        assert_eq!(ActionCx::for_apply(&outer, b"preview", 0).author(), None);
        binding
            .with_resource_frame(&mut outer, false, |doc, _| {
                assert_eq!(
                    ActionCx::for_apply(doc, b"preview", 0).author(),
                    Some(&MemberId::person("person"))
                );
                let _invoker = InvokerScope::enter(ActionInvoker::person("guest"));
                binding
                    .with_resource_frame(doc, false, |doc, _| {
                        assert_eq!(
                            ActionCx::for_apply(doc, b"preview", 0).author(),
                            Some(&MemberId::person("guest"))
                        );
                        assert!(!local_person_invocation_permitted());
                    })
                    .unwrap();
                assert_eq!(
                    ActionCx::for_apply(doc, b"preview", 0).author(),
                    Some(&MemberId::person("person"))
                );
            })
            .unwrap();
        assert_eq!(ActionCx::for_apply(&outer, b"preview", 0).author(), None);
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = binding.with_resource_frame(&mut outer, false, |doc, _| {
                assert_eq!(
                    ActionCx::for_apply(doc, b"preview", 0).author(),
                    Some(&MemberId::person("person"))
                );
                panic!("preview callback unwound");
            });
        }));
        assert!(panic.is_err());
        assert_eq!(ActionCx::for_apply(&outer, b"preview", 0).author(), None);
    }

    phase per_author_mounts_write_as_their_authors_and_the_record_keeps_its_pages_writer;
    {
                let checkout = DocumentCheckout::new(DocumentId::minted(), ResourceType::new("note"));
        let documents: std::rc::Rc<dyn crate::resource::DocumentPort> =
            std::rc::Rc::new(crate::resource::MemoryWorkspace::default());
        let writer = {
            let records = ResourceOwners::writing(std::rc::Rc::clone(&documents))
                .open(APP, WORKSPACE)
                .unwrap();
            records.create_resource(&checkout).unwrap();
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
                you.history_request(&mut outer, tilo_ui_base::widget::undo_access::UndoRequest::Undo)
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
            records.try_flush_written().unwrap();
            writer
        };

        let records = ResourceOwners::writing(std::rc::Rc::clone(&documents))
            .open(APP, WORKSPACE)
            .unwrap();
        records.open_existing_resource_checked(&checkout).unwrap();
        let reopened_writer = records
            .read(&checkout.id, |doc| doc.get_actor().clone())
            .unwrap();
        assert_ne!(
            reopened_writer, writer,
            "a page writes the record under a writer it mints, never the one a saved row carries"
        );
        let (you, _scope, _owner) = mount(&records, &checkout, "You", 4, true);
        let mut outer = AutoCommit::new();
        let again = writers_of(&records, &checkout.id, || {
            you.with_document(&mut outer, |doc, _| doc.put(ROOT, "you", 2).unwrap())
                .unwrap();
        });
        assert_eq!(
            again,
            [author_writer(&reopened_writer, "You")],
            "a page's author writes as the writer the page minted"
        );
    }
}

/// A mount of `checkout` for `author`, as `ProcessContext::callback_binding` makes one.
pub(super) fn mount(
    records: &ResourceStore,
    checkout: &DocumentCheckout,
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
        control_domain: tilo_ui_base::control_domain::WeakControlDomain::default(),
        queued_controls: Rc::new(RefCell::new(std::collections::VecDeque::new())),
        search_requests: Rc::default(),
        instance_id: instance,
        app_id: APP.into(),
        device_id: "test-device".into(),
        local_person: MemberId::person(author),
        records: records.clone(),
        checkout: checkout.clone(),
        permissions: tilo_ui_base::permissions::SharedPermissions::new(
            PermissionContext::permissive(),
        ),
        author: MemberId::person(author),
        operation_context: std::rc::Rc::default(),
        writes_as_author,
    };
    (binding, scope, owner)
}

fn writers_of(
    records: &ResourceStore,
    id: &DocumentId,
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
