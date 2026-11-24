use automerge::{AutoCommit, ROOT, ReadDoc as _, transaction::Transactable};
use std::rc::Rc;
use tilo_ui::resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceType};

const APP: &str = "so.tilo.chat";
const WORKSPACE: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

tilo_test_env::scenario! {
    fn a_package_primary_record_is_shared_admitted_and_partitioned;

    phase primary_owner_is_shared_and_keeps_recordless_wire_identity;
    {
        let workspace = Rc::new(tilo_ui::resource::MemoryWorkspace::default());
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        let a = ResourceOwners::writing(Rc::clone(&workspace) as Rc<dyn tilo_ui::resource::DocumentPort>)
            .open(APP, WORKSPACE)
            .unwrap();
        a.reserve_resource(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "message", "one").unwrap();
                Ok(())
            })
            .unwrap();
        a.flush().unwrap();
        // Another window of the app reads the record the workspace now holds.
        let mut owners = ResourceOwners::writing(Rc::clone(&workspace) as Rc<dyn tilo_ui::resource::DocumentPort>);
        let b = owners.open(APP, WORKSPACE).unwrap();
        b.try_open_resource(&checkout).unwrap();
        assert_eq!(a.route(&checkout).unwrap().document, None);
        b.with(&checkout.id, |doc| {
            doc.put(ROOT, "message", "two").unwrap();
            doc.commit();
        })
        .unwrap();
        b.flush().unwrap();
        // The other window's change reached the workspace, for the next start to read.
        let again = ResourceOwners::writing(
            Rc::clone(&workspace) as Rc<dyn tilo_ui::resource::DocumentPort>,
        )
            .open(APP, WORKSPACE)
            .unwrap();
        again.open_existing_resource_checked(&checkout).unwrap();
        assert_eq!(
            again
                .read(&checkout.id, |doc| {
                    doc.get(ROOT, "message")
                        .unwrap()
                        .unwrap()
                        .0
                        .as_str()
                        .map(str::to_owned)
                })
                .unwrap(),
            Some("two".to_owned())
        );
        assert!(
            a.read(&checkout.id, tilo_ui::resource::identity)
                .unwrap()
                .is_none()
        );
        assert_ne!(checkout.id, DocumentId::for_tab(1));
    }

    phase primary_admission_rejects_stamped_records_and_wire_parse_rejects_internal_key;
    {
        let workspace = Rc::new(tilo_ui::resource::MemoryWorkspace::default());
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        assert!(DocumentId::parse(checkout.id.as_str()).is_none());
        let mut doc = AutoCommit::new();
        tilo_ui::resource::init_scoped(
            &mut doc,
            &DocumentId::for_tab(7),
            &checkout.kind,
            "workspace",
            Some("workspace"),
        );
        doc.commit();
        let mut written = tilo_ui::resource::DocumentPort::write(
            &*workspace,
            APP,
            WORKSPACE,
            checkout.id.route(),
            doc.save(),
            true,
            None,
        );
        let _ = written(std::time::Duration::ZERO);
        let mut owners = ResourceOwners::writing(workspace);
        let store = owners.open(APP, WORKSPACE).unwrap();
        assert!(matches!(
            store.try_open_resource(&checkout),
            Err(tilo_ui::resource::ResourceOpenError::IdentityMismatch)
        ));
    }

    phase primary_ownership_is_partitioned_by_package_workspace_and_kind;
    {
        let workspace = Rc::new(tilo_ui::resource::MemoryWorkspace::default());
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        let mut owners = ResourceOwners::writing(Rc::clone(&workspace) as Rc<dyn tilo_ui::resource::DocumentPort>);
        let a = owners.open(APP, WORKSPACE).unwrap();
        a.reserve_resource(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        a.flush().unwrap();
        assert!(
            owners
                .open(APP, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaab")
                .unwrap()
                .try_open_resource(&checkout)
                .is_err()
        );
        assert!(
            owners
                .open("so.tilo.other", WORKSPACE)
                .unwrap()
                .try_open_resource(&checkout)
                .is_err()
        );
        assert!(
            a.try_open_resource(&DocumentCheckout::package_primary(ResourceType::new(
                "wrong.kind"
            )))
            .is_err()
        );
    }
}
