use automerge::{AutoCommit, ROOT, transaction::Transactable};
use tilo_ui::portable_storage::{PortableStorage, StorageKey};
use tilo_ui::resource::{DocumentCheckout, DocumentId, ResourceOwners, ResourceType};

tilo_test_env::scenario! {
    fn a_package_primary_record_is_shared_admitted_and_partitioned;

    phase primary_owner_is_shared_and_keeps_recordless_wire_identity;
    {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        let a = ResourceOwners::portable(cache.clone())
            .open("so.tilo.chat", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            .unwrap();
        let b = ResourceOwners::portable(cache.clone())
            .open("so.tilo.chat", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
            .unwrap();
        a.reserve_resource(&checkout)
            .unwrap()
            .initialize(|doc| {
                doc.put(ROOT, "message", "one").unwrap();
                Ok(())
            })
            .unwrap();
        b.try_open_resource(&checkout).unwrap();
        assert_eq!(a.route(&checkout).unwrap().document, None);
        b.with(&checkout.id, |doc| {
            doc.put(ROOT, "message", "two").unwrap();
        })
        .unwrap();
        let heads = a
            .read(
                &checkout.id,
                automerge::transaction::Transactable::base_heads,
            )
            .unwrap();
        assert_eq!(
            b.read(
                &checkout.id,
                automerge::transaction::Transactable::base_heads
            )
            .unwrap(),
            heads
        );
        assert!(
            a.read(&checkout.id, tilo_ui::resource::identity)
                .unwrap()
                .is_none()
        );
        assert!(
            cache
                .pending_writes()
                .iter()
                .any(|w| matches!(w.key, StorageKey::Primary { .. }))
        );
        assert_ne!(checkout.id, DocumentId::for_tab(1));
    }

    phase native_primary_directory_matches_existing_workspace_document;
    {
        let root = tempfile::tempdir().unwrap();
        let package = "so.tilo.chat";
        let workspace_id = "workspace-a";
        let path =
            app_state::DocStore::workspace_primary_dir_at(root.path(), package, workspace_id).unwrap();
        assert_eq!(
            path,
            root.path()
                .join(package)
                .join("workspaces")
                .join(workspace_id)
        );
        let mut owner = app_state::DocStore::open_canonical_at(package, &path).unwrap();
        let mut doc = owner.new_document();
        doc.put(ROOT, "message", "native").unwrap();
        doc.commit();
        owner.save(&mut doc).unwrap();
        let actor = doc.get_actor().clone();
        let bytes = doc.save();
        drop(owner);
        let store = ResourceOwners::new(root.path().into())
            .open(package, workspace_id)
            .unwrap();
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        store.try_open_resource(&checkout).unwrap();
        let original = AutoCommit::load(&bytes).unwrap();
        assert_eq!(
            store.read(&checkout.id, |d| d.get_actor().clone()).unwrap(),
            actor
        );
        assert_eq!(
            store
                .read(
                    &checkout.id,
                    automerge::transaction::Transactable::base_heads
                )
                .unwrap(),
            original.base_heads()
        );
    }

    phase primary_admission_rejects_stamped_records_and_wire_parse_rejects_internal_key;
    {
        let cache = PortableStorage::new("profile", "device").unwrap();
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
        cache
            .hydrate(
                StorageKey::Primary {
                    package: "so.tilo.chat".into(),
                    workspace: "workspace".into(),
                },
                doc.save(),
                Some(doc.get_actor().to_bytes().to_vec()),
            )
            .unwrap();
        let store = ResourceOwners::portable(cache)
            .open("so.tilo.chat", "workspace")
            .unwrap();
        assert!(matches!(
            store.try_open_resource(&checkout),
            Err(tilo_ui::resource::ResourceOpenError::IdentityMismatch)
        ));
    }

    phase primary_ownership_is_partitioned_by_package_workspace_and_kind;
    {
        let cache = PortableStorage::new("profile", "device").unwrap();
        let checkout = DocumentCheckout::package_primary(ResourceType::new("chat.workspace"));
        let mut owners = ResourceOwners::portable(cache);
        let a = owners.open("so.tilo.chat", "workspace-a").unwrap();
        a.reserve_resource(&checkout)
            .unwrap()
            .initialize(|_| Ok(()))
            .unwrap();
        assert!(
            owners
                .open("so.tilo.chat", "workspace-b")
                .unwrap()
                .try_open_resource(&checkout)
                .is_err()
        );
        assert!(
            owners
                .open("so.tilo.other", "workspace-a")
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
