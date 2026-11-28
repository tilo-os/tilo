use super::read::BufferedRead;
use super::*;

/// The availability registry of a device whose state is in `dir`.
fn registry_in(dir: &tempfile::TempDir) -> Arc<Mutex<AvailabilityRegistry>> {
    let path = dir.path().join("availability.json");
    Arc::new(Mutex::new(AvailabilityRegistry::open(path).unwrap()))
}

/// An empty namespace of the device `host`.
fn host() -> Arc<Mutex<Namespace>> {
    Arc::new(Mutex::new(Namespace::new("host").unwrap()))
}

mod availability_control_tests {
    use super::*;

    use crate::hydration::{ContentSource, StoreSource};
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;

    async fn wait_for_availability(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        record: fs_protocol::FileDocumentId,
        predicate: impl Fn(&DocumentAvailability) -> bool + Send + Sync,
    ) {
        wait_for_availability_with_timeout(
            registry,
            record,
            tilo_test_env::hang_bound(),
            predicate,
        )
        .await;
    }

    async fn wait_for_availability_with_timeout(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        record: fs_protocol::FileDocumentId,
        timeout: Duration,
        predicate: impl Fn(&DocumentAvailability) -> bool + Send + Sync,
    ) {
        tokio::time::timeout(timeout, async {
            loop {
                if registry.lock_recover().get(record).is_some_and(&predicate) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("availability state barrier");
    }

    async fn pinned_content_source() -> (Arc<ContentStore>, Hash, Hash) {
        let store = Arc::new(ContentStore::memory());
        let first = store
            .write_content(b"first remote version")
            .await
            .unwrap()
            .unwrap();
        let second = store
            .write_content(b"second remote version")
            .await
            .unwrap()
            .unwrap();
        (store, first, second)
    }

    /// The other device, reachable only from when it arrives.
    #[derive(Default)]
    struct Arriving(Mutex<Option<Arc<ContentStore>>>);

    impl ContentSource for Arriving {
        fn fetch(
            &self,
            hash: Hash,
        ) -> futures_util::future::BoxFuture<'static, Option<bytes::Bytes>> {
            let store = self.0.lock_recover().clone();
            Box::pin(async move { store?.local(hash).await })
        }
    }

    fn current_pinned_candidate(
        namespace: &Arc<Mutex<Namespace>>,
        registry: &Arc<Mutex<AvailabilityRegistry>>,
    ) -> PinnedCandidate {
        let hash = Hash::from([0x7a; 32]);
        let (document_id, parent) = {
            let mut namespace = namespace.lock().unwrap();
            let ino = namespace
                .create_file(ROOT_INO, "pinned.bin", Some(&hash), 1, 1)
                .unwrap();
            (
                namespace.document_id_for(ino).unwrap(),
                namespace.document_id_for(ROOT_INO),
            )
        };
        let root = parent.unwrap();
        registry
            .lock()
            .unwrap()
            .put(DocumentAvailability::new(root, EntryKind::Directory))
            .unwrap();
        let mut record = DocumentAvailability::new(document_id, EntryKind::File);
        record.parent = parent;
        record.policy = AvailabilityPolicy::Pinned;
        registry.lock().unwrap().put(record).unwrap();
        PinnedCandidate {
            document_id,
            parent,
            kind: EntryKind::File,
            hash: Some(hash),
            size: 1,
            path: "/pinned.bin".into(),
        }
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn delayed_pinned_hydration_is_cancelled_only_by_unpin() {
        {
            let dir = tempfile::tempdir().unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("wake-test").unwrap()));
            let registry = registry_in(&dir);
            let candidate = current_pinned_candidate(&namespace, &registry);
            assert!(pinned_candidate_is_current(
                &candidate, &namespace, &registry
            ));
            let (wake, mut wakes) = tokio::sync::watch::channel(0_u64);
            let writer_wake = wake.clone();
            let noisy_writer = tokio::spawn(async move {
                for _ in 0..5 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    writer_wake.send_modify(|generation| *generation = generation.wrapping_add(1));
                }
            });

            let completed = await_current_pinned_hydration(
                tokio::time::sleep(Duration::from_millis(75)),
                &candidate,
                &namespace,
                &registry,
                &mut wakes,
            )
            .await;
            noisy_writer.await.unwrap();
            assert!(completed.is_some(), "unrelated wakes must not starve a pin");
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("unpin-wake-test").unwrap()));
            let registry = registry_in(&dir);
            let candidate = current_pinned_candidate(&namespace, &registry);
            let (wake, mut wakes) = tokio::sync::watch::channel(0_u64);
            let cancellation_registry = Arc::clone(&registry);
            let document_id = candidate.document_id;
            let unpin = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut registry = cancellation_registry.lock().unwrap();
                let mut record = registry.get(document_id).unwrap().clone();
                record.policy = AvailabilityPolicy::Automatic;
                registry.put(record).unwrap();
                drop(registry);
                wake.send_modify(|generation| *generation = generation.wrapping_add(1));
            });

            let cancelled = await_current_pinned_hydration(
                tokio::time::sleep(tilo_test_env::hang_bound()),
                &candidate,
                &namespace,
                &registry,
                &mut wakes,
            )
            .await;
            unpin.await.unwrap();
            assert!(cancelled.is_none(), "unpin must cancel an active transfer");
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = Arc::new(Mutex::new(Namespace::new("bounded-pins").unwrap()));
            let folder = namespace
                .lock()
                .unwrap()
                .create_dir(ROOT_INO, "offline", 1)
                .unwrap();
            for index in 0..10_u8 {
                namespace
                    .lock()
                    .unwrap()
                    .create_file(
                        folder,
                        format!("{index}.bin"),
                        Some(&Hash::from([index.saturating_add(1); 32])),
                        1,
                        2,
                    )
                    .unwrap();
            }
            let registry = registry_in(&dir);
            let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
                .with_availability(Arc::clone(&registry));
            fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
                .unwrap();

            let (_wake, wakes) = tokio::sync::watch::channel(0_u64);
            reconcile_pinned_hydration(
                &namespace,
                Arc::clone(&hydrator),
                Arc::clone(&registry),
                None,
                wakes.clone(),
            )
            .await;
            let (attempted, queued) = {
                let registry = registry.lock().unwrap();
                let children = registry
                    .records()
                    .filter(|record| record.kind == EntryKind::File)
                    .collect::<Vec<_>>();
                let attempted = children
                    .iter()
                    .filter(|record| record.transfer.retry_count == 1)
                    .count();
                let queued = children
                    .iter()
                    .filter(|record| {
                        record.transfer.phase == TransferPhase::Queued
                            && record.transfer.retry_count == 0
                    })
                    .count();
                drop(registry);
                (attempted, queued)
            };
            assert_eq!(attempted, MAX_PINNED_FETCHES_PER_PASS);
            assert_eq!(queued, 10 - MAX_PINNED_FETCHES_PER_PASS);

            fs.set_pin_policy(folder, AvailabilityPolicy::Automatic)
                .unwrap();
            reconcile_pinned_hydration(
                &namespace,
                Arc::clone(&hydrator),
                Arc::clone(&registry),
                None,
                wakes,
            )
            .await;
            assert!(registry.lock().unwrap().records().all(|record| {
                record.kind == EntryKind::Directory
                    || (record.transfer.phase == TransferPhase::Idle
                        && record.transfer.retry_count == 0
                        && record.transfer.retry_at_ms.is_none())
            }));
        }
    }

    #[tokio::test]
    async fn pin_policy_persists_and_read_only_members_manage_offline_policy() {
        {
            let dir = tempfile::tempdir().unwrap();
            let registry_path = dir.path().join("availability.json");
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = host();
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(&registry_path).unwrap(),
            ));
            let fs = QuickFS::over(namespace.clone(), hydrator).with_availability(registry.clone());

            let folder = namespace
                .lock_recover()
                .create_dir(ROOT_INO, "offline", 1)
                .unwrap();
            fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
                .unwrap();

            let child = namespace
                .lock_recover()
                .create_file(folder, "later.txt", None, 0, 2)
                .unwrap();
            let projected = fs_protocol::AvailabilityProjectionV1::parse_xattr(
                &fs.availability_for(child).unwrap(),
            )
            .unwrap();
            assert_eq!(projected.policy, AvailabilityPolicy::Pinned);

            drop((fs, registry));
            let reopened = AvailabilityRegistry::open(registry_path).unwrap();
            let folder_id = namespace.lock_recover().document_id_for(folder).unwrap();
            assert_eq!(
                reopened.get(folder_id).map(|record| record.policy),
                Some(AvailabilityPolicy::Pinned)
            );
        }
        {
            const READER: [u8; 32] = [0x51; 32];
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = host();
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "shared.txt", None, 0, 1)
                .unwrap();
            let registry = registry_in(&dir);
            let mut acl = crate::acl::WorkspaceAcl::new();
            acl.grant_read(READER, "/shared.txt");
            let fs = QuickFS::over(namespace.clone(), hydrator)
                .with_availability(Arc::clone(&registry))
                .with_acl(acl, READER);

            assert_eq!(fs.check_access(ino, Access::Write), Err(EROFS));
            fs.set_pin_policy(ino, AvailabilityPolicy::Pinned).unwrap();
            let document_id = namespace.lock_recover().document_id_for(ino).unwrap();
            assert_eq!(
                registry.lock_recover().get(document_id).unwrap().policy,
                AvailabilityPolicy::Pinned
            );
        }
    }

    /// Records in `registry` for files under the folder `root`, each with its content's hash and
    /// the bytes of it kept here (none: not kept).
    fn keep_files_at(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        root: fs_protocol::FileDocumentId,
        files: &[(fs_protocol::FileDocumentId, Hash, u64)],
    ) {
        let mut registry = registry.lock_recover();
        registry
            .put(DocumentAvailability::new(root, EntryKind::Directory))
            .unwrap();
        for &(id, hash, bytes) in files {
            let mut record = DocumentAvailability::new(id, EntryKind::File);
            record.parent = Some(root);
            record.current_hash = Some(fs_protocol::ContentHash(*hash.as_bytes()));
            record.residency = if bytes == 0 {
                Residency::NotResident
            } else {
                Residency::Resident { bytes }
            };
            registry.put(record).unwrap();
        }
    }

    /// A file is freed here and fetched again by its content's hash: the availability record,
    /// the eviction and the fetch all name the hash the tree's node holds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(clippy::too_many_lines, reason = "one scenario, read top to bottom")]
    async fn a_file_freed_by_its_content_hash_is_fetched_again_by_it() {
        let dir = tempfile::tempdir().unwrap();
        let (source, first, second) = pinned_content_source().await;
        let client_store = Arc::new(ContentStore::memory());
        let bytes = b"first remote version";
        let written = client_store.write_content(bytes).await.unwrap().unwrap();
        assert_eq!(written, first, "the same bytes are the same content");
        let hydrator = Arc::new(
            Hydrator::new(Arc::clone(&client_store)).with_source(Arc::new(StoreSource(source))),
        );
        let namespace = Arc::new(Mutex::new(Namespace::new("evicting").unwrap()));
        let (ino, other_ino) = {
            let mut namespace = namespace.lock_recover();
            (
                namespace
                    .create_file(ROOT_INO, "kept.txt", Some(&first), bytes.len() as u64, 1)
                    .unwrap(),
                namespace
                    .create_file(ROOT_INO, "other.txt", Some(&second), 21, 1)
                    .unwrap(),
            )
        };
        let (root, document_id, other_id) = {
            let namespace = namespace.lock_recover();
            (
                namespace.document_id_for(ROOT_INO).unwrap(),
                namespace.document_id_for(ino).unwrap(),
                namespace.document_id_for(other_ino).unwrap(),
            )
        };
        let registry = registry_in(&dir);
        let content = |hash: Hash| Some(fs_protocol::ContentHash(*hash.as_bytes()));
        keep_files_at(
            &registry,
            root,
            &[
                (document_id, first, bytes.len() as u64),
                (other_id, second, 0),
            ],
        );
        let queue = Arc::new(tokio::sync::Mutex::new(
            EvictionQueue::open(dir.path().join("evictions.json")).unwrap(),
        ));
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry))
            .with_eviction_queue(queue);
        let projected = |id| {
            registry
                .lock_recover()
                .get(id)
                .map(|record| record.projection().current_hash)
        };
        assert_eq!(
            projected(document_id),
            Some(content(first)),
            "Files reads each file's availability by its own content's hash"
        );
        assert_eq!(projected(other_id), Some(content(second)));

        let coordinator = fs.eviction_coordinator().unwrap();
        let (request, plan) = coordinator
            .prepare(
                document_id,
                crate::eviction::EvictionMode::DestructiveOverride,
                None,
                Some(first),
            )
            .await
            .unwrap();
        assert_eq!(plan.content_hash, first);
        tokio::time::timeout(tilo_test_env::hang_bound(), async {
            while !coordinator
                .observe_once(document_id, request)
                .await
                .unwrap()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the freed content is gone from the store");
        assert_eq!(
            registry.lock_recover().get(document_id).unwrap().residency,
            Residency::NotResident
        );
        assert_eq!(
            hydrator.local_len(first).await,
            None,
            "the bytes are freed here"
        );

        let read = hydrator
            .read(first, 0, bytes.len() as u64)
            .await
            .expect("the device that keeps the content serves it by its hash");
        assert_eq!(read, bytes);
        assert_eq!(hydrator.local_len(first).await, Some(bytes.len() as u64));
        assert_eq!(
            registry
                .lock_recover()
                .get(document_id)
                .unwrap()
                .current_hash,
            content(first)
        );

        drop(fs);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pinned_folder_hydrates_future_child_retries_after_restart_and_tracks_new_version() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("availability.json");
        let (source, first, second) = pinned_content_source().await;
        let client_store = Arc::new(ContentStore::memory());
        let arriving = Arc::new(Arriving::default());
        let hydrator = Arc::new(
            Hydrator::new(client_store)
                .with_source(Arc::clone(&arriving) as Arc<dyn ContentSource>),
        );
        let namespace = Arc::new(Mutex::new(Namespace::new("pinned-client").unwrap()));
        let folder = namespace
            .lock()
            .unwrap()
            .create_dir(ROOT_INO, "offline", 1)
            .unwrap();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();

        let child = namespace
            .lock()
            .unwrap()
            .create_file(folder, "later.txt", Some(&first), 20, 2)
            .unwrap();
        let document_id = namespace.lock().unwrap().document_id_for(child).unwrap();
        fs.wake_pinned_hydration();
        wait_for_availability(&registry, document_id, |record| {
            record.transfer.last_error.is_some()
                && record.transfer.retry_count > 0
                && record.transfer.retry_at_ms.is_some()
        })
        .await;
        drop(fs);
        drop(registry);

        *arriving.0.lock_recover() = Some(source);
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        wait_for_availability(&registry, document_id, |record| {
            record.current_hash == Some(fs_protocol::ContentHash(*first.as_bytes()))
                && record.residency == Residency::Resident { bytes: 20 }
                && record.transfer.last_error.is_none()
        })
        .await;
        assert!(
            namespace
                .lock()
                .unwrap()
                .set_content(child, Some(&second), 21, 3)
        );
        fs.wake_pinned_hydration();
        wait_for_availability(&registry, document_id, |record| {
            record.current_hash == Some(fs_protocol::ContentHash(*second.as_bytes()))
                && record.residency == Residency::Resident { bytes: 21 }
                && record.transfer.last_error.is_none()
        })
        .await;

        drop(fs);
    }

    #[tokio::test]
    async fn keep_offline_holds_bytes_against_the_cache_budget_only_while_pinned() {
        use crate::store::area::Retention;
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let size = crate::content::FILE_BACKED_MIN + 1;
        let bytes = |byte: u8| vec![byte; usize::try_from(size).unwrap()];
        let read = store.write_content(&bytes(1)).await.unwrap().unwrap();
        let authored = store.write_content(&bytes(2)).await.unwrap().unwrap();
        store
            .files()
            .set_retention(read.as_bytes(), Retention::Cached)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("keep-offline").unwrap()));
        let (read_ino, authored_ino) = {
            let mut namespace = namespace.lock().unwrap();
            (
                namespace
                    .create_file(ROOT_INO, "read.bin", Some(&read), size, 1)
                    .unwrap(),
                namespace
                    .create_file(ROOT_INO, "authored.bin", Some(&authored), size, 1)
                    .unwrap(),
            )
        };
        let registry = registry_in(&dir);
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry));
        let (_wake, wakes) = tokio::sync::watch::channel(0_u64);
        let pass = || {
            reconcile_pinned_hydration(
                &namespace,
                Arc::clone(&hydrator),
                Arc::clone(&registry),
                None,
                wakes.clone(),
            )
        };
        let files = store.files();
        let retention = |hash: Hash| files.entry(hash.as_bytes()).map(|entry| entry.retention);
        files.set_budget(0);

        for ino in [read_ino, authored_ino] {
            fs.set_pin_policy(ino, AvailabilityPolicy::Pinned).unwrap();
        }
        let _ = pass().await;
        assert_eq!(
            retention(read),
            Some(Retention::Pinned),
            "a file already here is kept once it is pinned"
        );
        assert_eq!(retention(authored), Some(Retention::LocalOnly));
        files.evict_to_budget(&[]);
        assert!(
            files.entry(read.as_bytes()).is_some(),
            "the cache budget never takes a file kept offline"
        );

        for ino in [read_ino, authored_ino] {
            fs.set_pin_policy(ino, AvailabilityPolicy::Automatic)
                .unwrap();
        }
        let _ = pass().await;
        assert_eq!(
            retention(read),
            Some(Retention::Cached),
            "a file no pin covers is cache again"
        );
        assert_eq!(
            retention(authored),
            Some(Retention::LocalOnly),
            "a file only this device holds stays unevictable after its pin goes"
        );
        files.evict_to_budget(&[]);
        assert!(files.entry(read.as_bytes()).is_none());
        assert!(files.entry(authored.as_bytes()).is_some());

        drop(fs);
    }

    /// A file the person kept offline opens from this device's own bytes once the device that
    /// held them is gone: the pin fetched it whole, the cache budget cannot take it, and the
    /// mount serves it with no source to ask.
    #[tokio::test]
    async fn a_file_kept_offline_opens_with_the_device_that_held_it_gone() {
        use crate::store::area::Retention;
        let size = crate::content::FILE_BACKED_MIN + 1;
        let data: Vec<u8> = (0..size)
            .map(|at| u8::try_from(at % 251).unwrap())
            .collect();
        let peer = Arc::new(ContentStore::memory());
        let hash = peer.write_content(&data).await.unwrap().unwrap();
        // The other device, reachable while it is set and gone once it is not.
        let source = Arc::new(Arriving::default());
        *source.0.lock_recover() = Some(Arc::clone(&peer));

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)).with_source(source.clone()));
        let namespace = host();
        let ino = {
            let mut namespace = namespace.lock().unwrap();
            namespace
                .create_file(ROOT_INO, "kept.bin", Some(&hash), size, 1)
                .unwrap()
        };
        let registry = registry_in(&dir);
        let fs = Arc::new(
            QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
                .with_availability(Arc::clone(&registry)),
        );
        let (_wake, wakes) = tokio::sync::watch::channel(0_u64);

        fs.set_pin_policy(ino, AvailabilityPolicy::Pinned).unwrap();
        let _ = reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes.clone(),
        )
        .await;
        assert!(
            store.local(hash).await.is_some(),
            "keeping the file offline fetched it whole"
        );
        let retention = |hash: Hash| {
            store
                .files()
                .entry(hash.as_bytes())
                .map(|entry| entry.retention)
        };
        assert_eq!(
            retention(hash),
            Some(Retention::Pinned),
            "the pin holds it against the cache budget"
        );

        // The device that held the bytes disconnects, and the cache is made as small as it goes.
        *source.0.lock_recover() = None;
        store.files().set_budget(0);
        store.files().evict_to_budget(&[]);
        assert!(
            store.local(hash).await.is_some(),
            "the cache keeps what the pin holds"
        );

        // Read through the mount's own path on a thread that enters no runtime, as a FUSE
        // worker's thread does: the kept file opens with nothing to ask.
        let reader = Arc::clone(&fs);
        let (read, answered) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = read.send(
                reader
                    .read_path(std::path::Path::new("kept.bin"))
                    .expect("a kept file opens with the device that held it gone"),
            );
        });
        let read = answered
            .recv_timeout(Duration::from_secs(20))
            .expect("the mount answers the read");
        assert_eq!(read, data, "the bytes are the ones the device kept");
    }

    #[tokio::test]
    async fn a_pinned_download_never_evicts_a_file_the_same_pin_already_holds() {
        use crate::store::area::Retention;
        let size = crate::content::FILE_BACKED_MIN + 1;
        let bytes = |byte: u8| vec![byte; usize::try_from(size).unwrap()];
        let source_store = Arc::new(ContentStore::memory());
        let remote = source_store
            .write_content(&bytes(2))
            .await
            .unwrap()
            .unwrap();

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let here = store.write_content(&bytes(1)).await.unwrap().unwrap();
        store
            .files()
            .set_retention(here.as_bytes(), Retention::Cached)
            .unwrap();
        let hydrator = Arc::new(
            Hydrator::new(Arc::clone(&store)).with_source(Arc::new(StoreSource(source_store))),
        );
        let namespace = Arc::new(Mutex::new(Namespace::new("pinned-download").unwrap()));
        let folder = {
            let mut namespace = namespace.lock().unwrap();
            let folder = namespace.create_dir(ROOT_INO, "trip", 1).unwrap();
            namespace
                .create_file(folder, "here.bin", Some(&here), size, 1)
                .unwrap();
            namespace
                .create_file(folder, "remote.bin", Some(&remote), size, 1)
                .unwrap();
            folder
        };
        let registry = registry_in(&dir);
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry));
        let (_wake, wakes) = tokio::sync::watch::channel(0_u64);
        let files = store.files();
        files.set_budget(0);

        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();
        let _ = reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes,
        )
        .await;
        assert!(
            files
                .entry(remote.as_bytes())
                .is_some_and(|entry| entry.complete()),
            "the pass downloads the file that is not here yet"
        );
        assert_eq!(
            files.entry(here.as_bytes()).map(|entry| entry.retention),
            Some(Retention::Pinned),
            "the download never pushes out a file the same pin holds"
        );

        drop(fs);
    }

    fn worker_turns(fs: &QuickFS) -> u64 {
        fs.pinned_hydration
            .as_ref()
            .unwrap()
            .turns
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pins_are_repaired_on_announcements_without_a_tick() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let local = store.write_content(b"already here").await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(store));
        let namespace = Arc::new(Mutex::new(Namespace::new("announced-pin").unwrap()));
        let (folder, first) = {
            let mut namespace = namespace.lock().unwrap();
            let folder = namespace.create_dir(ROOT_INO, "offline", 1).unwrap();
            let first = namespace.create_file(folder, "first", None, 0, 1).unwrap();
            (folder, namespace.document_id_for(first).unwrap())
        };
        let registry = registry_in(&dir);
        let fs = QuickFS::over(Arc::clone(&namespace), Arc::clone(&hydrator))
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration_every(Duration::from_secs(3600));
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();
        wait_for_availability(&registry, first, |record| {
            record.residency == Residency::Resident { bytes: 0 }
        })
        .await;

        let remote = {
            let mut namespace = namespace.lock().unwrap();
            let ino = namespace
                .create_file(folder, "arrived-remotely", None, 0, 2)
                .unwrap();
            namespace.document_id_for(ino).unwrap()
        };
        wait_for_availability(&registry, remote, |record| {
            record.residency == Residency::Resident { bytes: 0 }
        })
        .await;

        let (root, other, inside) = {
            let mut namespace = namespace.lock().unwrap();
            let other = namespace
                .create_dir(ROOT_INO, "pinned-elsewhere", 3)
                .unwrap();
            let inside = namespace
                .create_file(other, "inside", Some(&local), 12, 3)
                .unwrap();
            (
                namespace.document_id_for(ROOT_INO).unwrap(),
                namespace.document_id_for(other).unwrap(),
                namespace.document_id_for(inside).unwrap(),
            )
        };
        let turns = worker_turns(&fs);
        tilo_test_env::within("the pass the namespace changes start", async {
            while worker_turns(&fs) == turns {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            registry
                .lock()
                .unwrap()
                .get(inside)
                .is_none_or(|record| record.residency != Residency::Resident { bytes: 12 }),
            "an unpinned file was proven resident"
        );
        {
            let mut registry = registry.lock().unwrap();
            if registry.get(root).is_none() {
                registry
                    .put(DocumentAvailability::new(root, EntryKind::Directory))
                    .unwrap();
            }
            let mut pinned = DocumentAvailability::new(other, EntryKind::Directory);
            pinned.parent = Some(root);
            pinned.policy = AvailabilityPolicy::Pinned;
            registry.put(pinned).unwrap();
        }
        wait_for_availability(&registry, inside, |record| {
            record.residency == Residency::Resident { bytes: 12 }
        })
        .await;

        drop(fs);
    }

    /// A retry runs off the thread that asked for it and reports either the completed transfer or
    /// one coarse failure a person can act on.
    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "each section keeps one former test's body verbatim"
    )]
    async fn transfer_retries_run_off_thread_and_report_actionable_outcomes() {
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let hash = store
                .write_content(b"available locally")
                .await
                .unwrap()
                .unwrap();
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = host();
            let registry = registry_in(&dir);
            let fs = QuickFS::over(namespace.clone(), hydrator).with_availability(registry.clone());
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "retry.txt", Some(&hash), 17, 1)
                .unwrap();
            let record = namespace.lock_recover().document_id_for(ino).unwrap();

            fs.retry_transfer(ino).unwrap();
            tilo_test_env::within("the retry completion event", async {
                loop {
                    let done = registry.lock_recover().get(record).is_some_and(|state| {
                        state.transfer.phase == TransferPhase::Idle
                            && state.transfer.last_error.is_none()
                            && state.residency == Residency::Resident { bytes: 17 }
                    });
                    if done {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = host();
            let registry = registry_in(&dir);
            let fs = QuickFS::over(namespace.clone(), hydrator).with_availability(registry.clone());
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "empty.txt", None, 0, 1)
                .unwrap();
            let record = namespace.lock_recover().document_id_for(ino).unwrap();

            fs.retry_transfer(ino).unwrap();
            assert_eq!(
                registry.lock_recover().get(record).unwrap().residency,
                Residency::Resident { bytes: 0 }
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(store));
            let namespace = host();
            let registry = registry_in(&dir);
            let fs = QuickFS::over(namespace.clone(), hydrator).with_availability(registry.clone());
            let missing = Hash::from([9; 32]);
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "missing.txt", Some(&missing), 4, 1)
                .unwrap();
            let record = namespace.lock_recover().document_id_for(ino).unwrap();

            fs.retry_transfer(ino).unwrap();
            tilo_test_env::within("the retry failure event", async {
                loop {
                    let failure = registry
                        .lock()
                        .unwrap()
                        .get(record)
                        .and_then(|state| state.transfer.last_error.clone());
                    if let Some(failure) = failure {
                        assert_eq!(
                            failure.message,
                            "No available replica could provide this file"
                        );
                        assert!(!failure.message.contains(&missing.to_string()));
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_does_not_project_completed_old_version_as_not_resident() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)));
        let namespace = host();
        let ino = namespace
            .lock_recover()
            .create_file(ROOT_INO, "empty.txt", None, 0, 1)
            .unwrap();
        let document_id = namespace.lock_recover().document_id_for(ino).unwrap();
        let content_hash = iroh_blobs::Hash::from([5; 32]);
        let registry = registry_in(&dir);
        let mut availability = DocumentAvailability::new(document_id, EntryKind::File);
        availability.current_hash = Some(fs_protocol::ContentHash(*content_hash.as_bytes()));
        availability.residency = Residency::Resident { bytes: 0 };
        registry.lock_recover().put(availability).unwrap();
        let mut queue = EvictionQueue::open(dir.path().join("evictions.json")).unwrap();
        let plan = crate::eviction::EvictionPlan {
            document_id,
            content_hash,
            mode: crate::eviction::EvictionMode::Safe,
            reclaimable: Vec::new(),
            shared_and_protected: Vec::new(),
            already_absent: Vec::new(),
            reclaimable_bytes: 0,
            unaccounted_reclaimable_blobs: 0,
            protected_shared_bytes: 0,
            unaccounted_protected_shared_blobs: 0,
        };
        let request_id = queue.enqueue(&plan, 1).unwrap().id;
        let observation = store
            .observe_eviction_request(&mut queue, request_id, 2)
            .await
            .unwrap();
        assert!(matches!(
            observation.state,
            EvictionRequestState::Completed { .. }
        ));
        let queue = Arc::new(tokio::sync::Mutex::new(queue));
        let mut current = registry.lock_recover().get(document_id).cloned().unwrap();
        current.current_hash = Some(fs_protocol::ContentHash([6; 32]));
        current.residency = Residency::Resident { bytes: 0 };
        registry.lock_recover().put(current).unwrap();
        let fs = QuickFS::over(namespace, hydrator)
            .with_availability(Arc::clone(&registry))
            .with_eviction_queue(queue);

        fs.resume_pending_evictions();
        assert_eq!(
            registry.lock_recover().get(document_id).unwrap().residency,
            Residency::Resident { bytes: 0 }
        );
    }
}

#[cfg(test)]
mod thumb_tests {
    //! The host-side thumbnail metadata path: `QuickFS::thumbnail_for` is
    //! the exact code `getxattr(user.tilo.thumb)` runs, so driving it
    //! exercises the production path end to end.
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use crate::thumbs::ThumbProvider;
    use std::io::Cursor;

    fn solid_png(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = image::RgbaImage::new(w, h);
        for px in buf.pixels_mut() {
            *px = image::Rgba([rgb[0], rgb[1], rgb[2], 255]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgba8(buf)
            .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn fixture(thumb_dir: &std::path::Path) -> (Arc<Mutex<Namespace>>, Arc<ContentStore>, QuickFS) {
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(store.clone()));
        let namespace = host();
        let provider = ThumbProvider::open(thumb_dir).unwrap();
        let fs = QuickFS::over(namespace.clone(), hydrator).with_thumbs(provider);
        (namespace, store, fs)
    }

    /// `thumbnail_for` is the exact code `getxattr(user.tilo.thumb)` runs: a photo's preview is
    /// made once, verified, published for peers and cached across remounts; a replicated
    /// preview is served before content; and neither is made for a file with no local content
    /// or for one that is not an image.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "each section keeps one former test's body verbatim"
    )]
    async fn thumbnails_are_generated_verified_published_cached_and_served_before_content() {
        {
            let dir = tempfile::tempdir().unwrap();
            let (namespace, store, fs) = fixture(&dir.path().join("thumbs"));

            let png = solid_png(600, 400, [40, 130, 220]);
            let mh = store.write_content(&png).await.unwrap().unwrap();
            let ino = {
                let mut ns = namespace.lock_recover();
                ns.create_file(ROOT_INO, "photo.png", Some(&mh), png.len() as u64, 1)
                    .unwrap()
            };

            assert!(namespace.lock_recover().thumb_hash(ino).is_none());

            let thumb = fs.thumbnail_for(ino).expect("host generates a preview");
            let decoded = thumbnail::decode_png(&thumb).expect("it is a valid PNG");
            assert!(decoded.is_valid());
            assert_eq!(
                decoded.max_edge(),
                crate::thumbs::HOST_EDGE,
                "downscaled to the host preview size"
            );
            assert!(
                decoded.max_edge() < u32::try_from(png.len()).unwrap_or(u32::MAX),
                "the preview is far smaller than the source"
            );

            let tbh = namespace
                .lock_recover()
                .thumb_hash(ino)
                .expect("thumbnail hash recorded on the node → projects to peers");
            assert!(
                store.has(tbh).await,
                "the thumbnail blob is stored for peers"
            );

            let again = fs.thumbnail_for(ino).expect("cached");
            assert_eq!(
                again, thumb,
                "second request is a cache hit, identical bytes"
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let png = solid_png(320, 240, [200, 60, 30]);

            let thumb = {
                let (namespace, store, fs) = fixture(&dir.path().join("thumbs"));
                let mh = store.write_content(&png).await.unwrap().unwrap();
                let ino = {
                    let mut ns = namespace.lock_recover();
                    ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                        .unwrap()
                };
                fs.thumbnail_for(ino).expect("generated")
            };

            let (namespace2, store2, fs2) = fixture(&dir.path().join("thumbs"));
            let mh = store2.write_content(&png).await.unwrap().unwrap();
            let ino2 = {
                let mut ns = namespace2.lock_recover();
                ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                    .unwrap()
            };
            let served = fs2
                .thumbnail_for(ino2)
                .expect("served from the on-disk cache");
            assert_eq!(
                served, thumb,
                "same content → same cached preview after remount"
            );
        }
        {
            let dir = tempfile::tempdir().unwrap();
            let (namespace, store, fs) = fixture(&dir.path().join("thumbs"));

            let preview = thumbnail::encode_png(
                &thumbnail::image_thumb(&solid_png(256, 256, [90, 200, 120]), 256).unwrap(),
            )
            .unwrap();
            let thumb_blob = store.ingest(preview.clone()).await.unwrap();

            let projected = iroh_blobs::Hash::from([5u8; 32]);
            let ino = {
                let mut ns = namespace.lock_recover();
                let ino = ns
                    .create_file(ROOT_INO, "big-photo.png", Some(&projected), 9_000_000, 1)
                    .unwrap();
                ns.set_thumb(ino, &thumb_blob);
                ino
            };

            let served = fs
                .thumbnail_for(ino)
                .expect("a projected preview is served before content");
            assert_eq!(
                served, preview,
                "the replicated thumbnail is served without local content"
            );

            let unthumbed = iroh_blobs::Hash::from([6u8; 32]);
            let img_ino = {
                let mut ns = namespace.lock_recover();
                ns.create_file(ROOT_INO, "remote.png", Some(&unthumbed), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(img_ino).is_none(),
                "no preview without local content — never hydrate the file to make one"
            );

            let zip_ino = {
                let mut ns = namespace.lock_recover();
                ns.create_file(ROOT_INO, "bundle.zip", Some(&unthumbed), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(zip_ino).is_none(),
                "a .zip is a badge, not a preview"
            );
        }
    }
}

#[cfg(test)]
mod predictive_tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;

    fn fixture() -> (Arc<Mutex<Namespace>>, QuickFS) {
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(store));
        let namespace = host();
        (namespace.clone(), QuickFS::over(namespace, hydrator))
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each fixture keeps one (ACL-filtered) namespace snapshot while asserting ordered predictive candidates"
    )]
    async fn prediction_selects_visible_siblings_of_the_same_type() {
        {
            let (namespace, fs) = fixture();
            let (first, second_hash, third_hash) = {
                let mut ns = namespace.lock_recover();
                let first = ns
                    .create_file(ROOT_INO, "01.png", Some(&Hash::from([1; 32])), 10, 1)
                    .unwrap();
                ns.create_file(ROOT_INO, "02.txt", Some(&Hash::from([2; 32])), 20, 1)
                    .unwrap();
                let second_hash = Hash::from([3; 32]);
                ns.create_file(ROOT_INO, "03.PNG", Some(&second_hash), 30, 1)
                    .unwrap();
                let third_hash = Hash::from([4; 32]);
                ns.create_file(ROOT_INO, "04.png", Some(&third_hash), 40, 1)
                    .unwrap();
                ns.create_file(ROOT_INO, "05.png", Some(&Hash::from([5; 32])), 50, 1)
                    .unwrap();
                (first, second_hash, third_hash)
            };

            assert_eq!(
                fs.predictive_candidates(first),
                vec![(second_hash, 30), (third_hash, 40)]
            );
        }
        {
            const ME: [u8; 32] = [0xA1; 32];
            let (namespace, fs) = fixture();
            let (first, visible_hash) = {
                let mut ns = namespace.lock_recover();
                let first = ns
                    .create_file(ROOT_INO, "01.png", Some(&Hash::from([1; 32])), 10, 1)
                    .unwrap();
                ns.create_file(ROOT_INO, "02.png", Some(&Hash::from([2; 32])), 20, 1)
                    .unwrap();
                let visible_hash = Hash::from([3; 32]);
                ns.create_file(ROOT_INO, "03.png", Some(&visible_hash), 30, 1)
                    .unwrap();
                (first, visible_hash)
            };
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ME, "/01.png").grant_read(ME, "/03.png");
            let fs = fs.with_acl(acl, ME);

            assert_eq!(
                fs.predictive_candidates(first),
                vec![(visible_hash, 30)],
                "the raw namespace's /02.png must not cross the filtered-view boundary"
            );
        }
    }

    #[tokio::test]
    #[allow(
        clippy::range_plus_one,
        clippy::needless_borrows_for_generic_args,
        clippy::significant_drop_tightening,
        reason = "the boundary fixture intentionally builds one page plus one and holds a stable namespace snapshot"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn record_catalogs_hide_acl_denied_records_and_refuse_stale_v2_pages() {
        {
            const ME: [u8; 32] = [0xA2; 32];
            let (namespace, fs) = fixture();
            let (visible_document, hidden_document) = {
                let mut ns = namespace.lock_recover();
                let folder = ns.create_dir(ROOT_INO, "folder", 1).unwrap();
                let visible = ns
                    .create_file(folder, "visible.txt", Some(&Hash::from([1; 32])), 10, 1)
                    .unwrap();
                let hidden = ns
                    .create_file(ROOT_INO, "hidden.txt", Some(&Hash::from([2; 32])), 20, 1)
                    .unwrap();
                (
                    ns.document_id_for(visible).unwrap(),
                    ns.document_id_for(hidden).unwrap(),
                )
            };
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ME, "/folder/visible.txt");
            let fs = fs.with_acl(acl, ME);

            let manifest: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(
                    &fs.document_index_manifest_v3()
                        .expect("root record catalog"),
                )
                .unwrap();
            let projection: fs_protocol::projection::DocumentIndexPageV3 = serde_json::from_slice(
                &fs.document_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .expect("the first page"),
            )
            .unwrap();
            assert!(projection.records.iter().any(|entry| {
                entry.document_id == visible_document && entry.path == "/folder/visible.txt"
            }));
            assert!(
                projection
                    .records
                    .iter()
                    .all(|entry| entry.document_id != hidden_document),
                "a record outside the mount's ACL-filtered view cannot enter search metadata"
            );
        }
        {
            let (namespace, fs) = fixture();
            {
                let mut ns = namespace.lock_recover();
                for index in 0..fs_protocol::projection::DOCUMENT_INDEX_PAGE_ENTRIES + 1 {
                    let byte = u8::try_from(index).expect("fixture index fits in one byte");
                    ns.create_file(
                        ROOT_INO,
                        &format!("late-{index}.txt"),
                        Some(&Hash::from([byte; 32])),
                        index as u64,
                        index as u64,
                    )
                    .unwrap();
                }
            }
            let manifest: fs_protocol::projection::DocumentIndexManifestV2 =
                serde_json::from_slice(&fs.document_index_manifest_v2().unwrap()).unwrap();
            manifest.validate().unwrap();
            assert_eq!(
                manifest.records,
                fs_protocol::projection::DOCUMENT_INDEX_PAGE_ENTRIES + 1
            );
            let first: fs_protocol::projection::DocumentIndexPageV2 =
                serde_json::from_slice(&fs.document_index_page_v2(&manifest.revision, 0).unwrap())
                    .unwrap();
            let second_offset = first.next_offset.expect("continuation page");
            let second: fs_protocol::projection::DocumentIndexPageV2 = serde_json::from_slice(
                &fs.document_index_page_v2(&manifest.revision, second_offset)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 1);

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "new.txt", None, 1, 1)
                .unwrap();
            assert!(
                fs.document_index_page_v2(&manifest.revision, 0).is_none(),
                "a namespace revision invalidates a page cursor rather than mixing rows"
            );
        }
        {
            const ME: [u8; 32] = [0xA3; 32];
            let (namespace, fs) = fixture();
            namespace
                .lock_recover()
                .create_file(ROOT_INO, "visible.txt", Some(&Hash::from([3; 32])), 3, 1)
                .unwrap();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible.txt");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: fs_protocol::projection::DocumentIndexManifestV2 =
                serde_json::from_slice(&fs.document_index_manifest_v2().unwrap()).unwrap();

            handle.replace(policy);
            let after: fs_protocol::projection::DocumentIndexManifestV2 =
                serde_json::from_slice(&fs.document_index_manifest_v2().unwrap()).unwrap();
            assert_ne!(before.revision, after.revision);
            assert!(
                fs.document_index_page_v2(&before.revision, 0).is_none(),
                "a page issued under the old ACL generation cannot be replayed"
            );
        }
    }

    #[tokio::test]
    async fn v3_record_catalog_hides_denied_rows_and_fails_closed_on_revoke() {
        {
            const ME: [u8; 32] = [0xA4; 32];
            let (namespace, fs) = fixture();
            let visible_document = {
                let mut ns = namespace.lock_recover();
                for index in 0..(fs_protocol::projection::DOCUMENT_INDEX_PAGE_ENTRIES * 2) {
                    ns.create_file(ROOT_INO, format!("hidden-{index:04}"), None, 0, 1)
                        .unwrap();
                }
                let ino = ns.create_file(ROOT_INO, "visible.txt", None, 0, 1).unwrap();
                ns.document_id_for(ino).unwrap()
            };
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible.txt");
            let fs = fs.with_acl(AclHandle::new(policy), ME);

            let before = fs.document_index_manifest_v3().unwrap();
            let manifest: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&before).unwrap();
            manifest.validate().unwrap();
            assert_eq!(manifest.initial_cursor.len(), 36);
            let first: fs_protocol::projection::DocumentIndexPageV3 = serde_json::from_slice(
                &fs.document_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(first.records.len(), 1);
            assert_eq!(first.records[0].document_id, visible_document);
            assert!(first.next_cursor.is_none());
            assert!(
                fs.document_index_page_v3(
                    &manifest.revision,
                    "00000000-0000-0000-0000-000000000000"
                )
                .is_none()
            );

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "another-hidden.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_document_index();
            assert_eq!(fs.document_index_manifest_v3().unwrap(), before);
            assert!(
                fs.document_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .is_some()
            );
        }
        {
            const ME: [u8; 32] = [0xA5; 32];
            let (namespace, fs) = fixture();
            let first_document = {
                let mut ns = namespace.lock_recover();
                let ino = ns.create_file(ROOT_INO, "first.txt", None, 0, 1).unwrap();
                ns.document_id_for(ino).unwrap()
            };
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&fs.document_index_manifest_v3().unwrap()).unwrap();
            assert!(fs.document_index_by_id_v2(first_document).is_some());

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "second.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_document_index();
            let after: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&fs.document_index_manifest_v3().unwrap()).unwrap();
            assert_ne!(before.revision, after.revision);
            assert!(
                fs.document_index_page_v3(&before.revision, &before.initial_cursor)
                    .is_none()
            );

            handle.replace(WorkspaceAcl::new());
            let mut revoked = None;
            for _ in 0..64 {
                revoked = fs.document_index_manifest_v3();
                if revoked.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let revoked: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&revoked.unwrap()).unwrap();
            assert_ne!(after.revision, revoked.revision);
            assert!(
                fs.document_index_page_v3(&after.revision, &after.initial_cursor)
                    .is_none()
            );
            assert!(fs.document_index_by_id_v2(first_document).is_none());
        }
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the catalogue fixture measures source scans across one stable namespace population"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn v3_record_catalog_pages_large_views_off_the_request_path() {
        {
            const ME: [u8; 32] = [0xA6; 32];
            let (namespace, fs) = fixture();
            {
                let mut ns = namespace.lock_recover();
                for index in 0..=fs_protocol::projection::DOCUMENT_INDEX_PAGE_ENTRIES {
                    ns.create_projected_file(
                        ROOT_INO,
                        format!("visible-{index:04}"),
                        fs_protocol::FileDocumentId(uuid::Uuid::from_u128((index + 1) as u128)),
                        None,
                        0,
                        1,
                    )
                    .unwrap();
                }
            }
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let fs = fs.with_acl(policy, ME);
            let manifest: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&fs.document_index_manifest_v3().unwrap()).unwrap();
            let first: fs_protocol::projection::DocumentIndexPageV3 = serde_json::from_slice(
                &fs.document_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                first.records.len(),
                fs_protocol::projection::DOCUMENT_INDEX_PAGE_ENTRIES
            );
            let second: fs_protocol::projection::DocumentIndexPageV3 = serde_json::from_slice(
                &fs.document_index_page_v3(
                    &manifest.revision,
                    first.next_cursor.as_deref().unwrap(),
                )
                .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 1);
            assert!(second.next_cursor.is_none());
            assert!(
                fs.document_index_by_id_v2(first.records[0].document_id)
                    .is_some()
            );
        }
        {
            const ME: [u8; 32] = [0xA7; 32];
            let (namespace, fs) = fixture();
            let visible_dir = namespace
                .lock_recover()
                .create_dir(ROOT_INO, "visible", 1)
                .unwrap();
            namespace
                .lock_recover()
                .create_file(visible_dir, "initial.txt", None, 0, 1)
                .unwrap();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible");
            let acl = AclHandle::new(policy.clone());
            let fs = fs.with_acl(acl.clone(), ME);
            let before: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&fs.document_index_manifest_v3().unwrap()).unwrap();

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "hidden-remote.txt", None, 0, 1)
                .unwrap();
            for _ in 0..16 {
                acl.replace(policy.clone());
            }
            let mut hidden = None;
            for _ in 0..64 {
                hidden = fs.document_index_manifest_v3();
                if hidden.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let hidden: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&hidden.unwrap()).unwrap();
            assert_eq!(
                hidden.page_entries, before.page_entries,
                "only the visible record is listed"
            );

            namespace
                .lock_recover()
                .create_file(visible_dir, "remote.txt", None, 0, 1)
                .unwrap();
            let mut updated = None;
            acl.replace(policy.clone());
            for _ in 0..64 {
                updated = fs.document_index_manifest_v3();
                if updated.as_ref().is_some_and(|bytes| {
                    serde_json::from_slice::<fs_protocol::projection::DocumentIndexManifestV3>(
                        bytes,
                    )
                    .is_ok_and(|manifest| manifest.revision != before.revision)
                }) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let updated: fs_protocol::projection::DocumentIndexManifestV3 =
                serde_json::from_slice(&updated.unwrap()).unwrap();
            assert_ne!(updated.revision, before.revision);
        }
        {
            const ME: [u8; 32] = [0xA8; 32];
            let (_namespace, fs) = fixture();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let acl = AclHandle::new(policy.clone());
            let fs = fs.with_acl(acl.clone(), ME);
            let weak = Arc::downgrade(&fs.acl_visible_document_index);
            drop(fs);
            acl.replace(policy);
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(
                weak.upgrade().is_none(),
                "the ACL subscriber must use a weak cache reference and exit after unmount"
            );
        }
    }

    #[tokio::test]
    async fn acl_index_incremental_matches_full_rebuild() {
        const ME: [u8; 32] = [0xA9; 32];
        let (namespace, fs) = fixture();
        let visible = {
            let mut ns = namespace.lock_recover();
            let visible = ns.create_dir(ROOT_INO, "visible", 1).unwrap();
            for index in 0..64 {
                ns.create_file(visible, format!("shown-{index:03}"), None, 0, 1)
                    .unwrap();
                ns.create_file(ROOT_INO, format!("hidden-{index:03}"), None, 0, 1)
                    .unwrap();
            }
            drop(ns);
            visible
        };
        let mut policy = WorkspaceAcl::new();
        policy.grant_read(ME, "/visible");
        let handle = AclHandle::new(policy);
        let fs = fs.with_acl(handle.clone(), ME);
        let rows = || namespace.lock_recover().indexed_document_count();
        let initial = fs.acl_visible_document_index().unwrap();
        assert_eq!(initial.acl_evaluations, rows());

        fs.refresh_acl_visible_document_index();
        assert!(Arc::ptr_eq(
            &initial,
            &fs.acl_visible_document_index().unwrap()
        ));

        let revealed = {
            let mut ns = namespace.lock_recover();
            ns.create_file(visible, "late.txt", None, 0, 2).unwrap();
            assert!(ns.rename(ROOT_INO, "hidden-000", visible, "revealed.txt", 2));
            ns.entry_at("visible/revealed.txt").unwrap().document_id
        };
        fs.refresh_acl_visible_document_index();
        let incremental = fs.acl_visible_document_index().unwrap();
        assert_eq!(incremental.acl_evaluations, 2);
        assert!(incremental.by_id.contains_key(&revealed));

        *fs.acl_visible_document_index.lock_recover() = None;
        fs.refresh_acl_visible_document_index();
        let full = fs.acl_visible_document_index().unwrap();
        assert_eq!(full.acl_evaluations, rows());
        assert_eq!(full.records, incremental.records);
        assert_eq!(full.hidden, incremental.hidden);
        assert_eq!(full.revision, incremental.revision);

        let mut widened = WorkspaceAcl::new();
        widened.grant_read(ME, "/");
        handle.replace(widened);
        fs.refresh_acl_visible_document_index();
        let regranted = fs.acl_visible_document_index().unwrap();
        assert_eq!(regranted.acl_evaluations, rows());
        assert!(regranted.hidden.is_empty());
    }
}

mod write_stage_tests {
    use super::*;

    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use crate::test_bytes::pseudo;

    struct Remote {
        dir: tempfile::TempDir,
        source_store: Arc<ContentStore>,
        namespace: Arc<Mutex<Namespace>>,
        fs: QuickFS,
    }

    impl Remote {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let source_store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())).with_source(
                Arc::new(crate::hydration::StoreSource(Arc::clone(&source_store))),
            ));
            let namespace = Arc::new(Mutex::new(Namespace::new("stage-client").unwrap()));
            let fs = QuickFS::over(Arc::clone(&namespace), hydrator)
                .with_write_stage_dir(dir.path().join("stage"));
            Self {
                dir,
                source_store,
                namespace,
                fs,
            }
        }

        async fn file(&self, name: &str, bytes: &[u8]) -> (u64, Hash) {
            let hash = self
                .source_store
                .write_content(bytes)
                .await
                .unwrap()
                .unwrap();
            let ino = self
                .namespace
                .lock()
                .unwrap()
                .create_file(ROOT_INO, name, Some(&hash), bytes.len() as u64, 1)
                .unwrap();
            (ino, hash)
        }

        fn fetched(&self) -> u64 {
            let (ranges, blobs) = self.fs.hydrator.received_bytes();
            ranges.saturating_add(blobs)
        }

        async fn committed(&self, ino: u64) -> Vec<u8> {
            let hash = self.fs.ns().hash_for(ino);
            match hash {
                Some(hash) => self.fs.hydrator.read(hash, 0, u64::MAX).await.unwrap(),
                None => Vec::new(),
            }
        }

        fn close(self) {
            drop(self.fs);
            drop(self.dir);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_to_remote_file_fetches_only_the_tail() {
        const LEN: usize = 64 * 1024 * 1024;
        let remote = Remote::new();
        let original = pseudo(LEN, 0x217);
        let (ino, base) = remote.file("video.mp4", &original).await;
        assert_eq!(remote.fetched(), 0);

        assert_eq!(remote.fs.write_buffer(ino, LEN as u64, b"!"), Ok(1));
        let fetched = remote.fetched();
        assert!(
            fetched <= u64::from(crate::content::MAX_CHUNK_LEN),
            "appending one byte fetched {fetched} bytes of a {LEN}-byte remote file"
        );
        let stage = remote.fs.stage(ino).unwrap();
        assert!(stage.lock().is_file_backed());
        assert_eq!(
            remote.fs.presented_attr(ino).unwrap().0.size,
            LEN as u64 + 1,
            "stat sees the staged length"
        );
        let StagedRead::Missing(missing) = remote
            .fs
            .read_staged(ino, LEN as u64 - 3, 16)
            .unwrap()
            .unwrap()
        else {
            panic!("the base's tail is not local");
        };
        let tail = missing.finish(&remote.fs.hydrator).await.unwrap();
        assert_eq!(&tail[..3], &original[LEN - 3..]);
        assert_eq!(&tail[3..], b"!");

        assert_eq!(remote.fs.flush_inode(ino), Ok(()));
        let mut expected = original;
        expected.push(b'!');
        let reference = ContentStore::memory();
        let previous = reference.write_content(&expected[..LEN]).await.unwrap();
        assert_eq!(previous, Some(base));
        let expected_hash = reference
            .write_content_after(&expected, previous)
            .await
            .unwrap();
        assert_eq!(remote.fs.ns().hash_for(ino), expected_hash);
        assert_eq!(
            blake3::hash(&remote.committed(ino).await),
            blake3::hash(&expected)
        );
        remote.close();
    }

    /// A truncation of a remote file reads only the kept prefix, and never reads what it drops.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncating_a_remote_file_reads_only_the_kept_prefix_and_nothing_it_drops() {
        let remote = Remote::new();
        let (unopened, _) = remote.file("big.iso", &pseudo(3 * 1024 * 1024, 1)).await;
        let (rewritten, _) = remote.file("notes.txt", &pseudo(300_000, 2)).await;
        let (open, _) = remote.file("open.bin", &pseudo(2 * 1024 * 1024, 3)).await;
        let original = pseudo(6 * 1024 * 1024, 4);
        let (kept, _) = remote.file("log.bin", &original).await;

        assert_eq!(remote.fs.truncate_inode(unopened, 0), Ok(()));
        assert_eq!(remote.fs.truncate_inode(rewritten, 0), Ok(()));
        assert_eq!(remote.fs.write_buffer(rewritten, 0, b"fresh"), Ok(5));
        assert_eq!(remote.fs.write_buffer(open, 7, b"x"), Ok(1));
        assert_eq!(remote.fs.truncate_inode(open, 0), Ok(()));
        assert_eq!(remote.fs.presented_attr(open).unwrap().0.size, 0);
        assert_eq!(remote.fs.flush_inode(rewritten), Ok(()));
        assert_eq!(remote.fs.flush_inode(open), Ok(()));
        assert_eq!(remote.fetched(), 0, "no truncation read what it dropped");
        assert_eq!(remote.fs.truncate_inode(kept, 1000), Ok(()));
        let fetched = remote.fetched();
        assert!(
            fetched <= u64::from(crate::content::MAX_CHUNK_LEN),
            "keeping 1000 bytes fetched {fetched}"
        );

        assert_eq!(remote.fs.ns().hash_for(unopened), None);
        assert_eq!(remote.fs.ns().getattr(unopened).unwrap().size, 0);
        assert_eq!(remote.fs.ns().hash_for(open), None);
        assert_eq!(remote.committed(rewritten).await, b"fresh");
        assert_eq!(remote.committed(kept).await, &original[..1000]);
        remote.close();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreachable_base_fails_the_commit_and_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("offline").unwrap()));
        let elsewhere = ContentStore::memory()
            .write_content(&pseudo(200_000, 5))
            .await
            .unwrap()
            .unwrap();
        let ino = namespace
            .lock()
            .unwrap()
            .create_file(ROOT_INO, "remote.txt", Some(&elsewhere), 200_000, 1)
            .unwrap();
        let fs = QuickFS::over(Arc::clone(&namespace), hydrator)
            .with_write_stage_dir(dir.path().join("stage"));

        assert_eq!(fs.write_buffer(ino, 200_000, b"tail"), Ok(4));
        assert_eq!(fs.flush_inode(ino), Err(EIO));
        assert_eq!(fs.ns().hash_for(ino), Some(elsewhere));
        assert_eq!(fs.ns().getattr(ino).unwrap().size, 200_000);
        assert!(fs.read_full_blocking(ino).is_err());
        assert_eq!(fs.staged_len(ino), Some(200_004), "the written bytes stay");
        drop(fs);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_flush_does_not_block_other_inode_reads() {
        let dir = tempfile::tempdir().unwrap();
        let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("contention").unwrap()));
        let fs = QuickFS::over(Arc::clone(&namespace), hydrator)
            .with_write_stage_dir(dir.path().join("stage"));
        let large = fs.create_empty_file(ROOT_INO, "large.bin").unwrap();
        fs.publish();
        let small = fs.create_empty_file(ROOT_INO, "small.txt").unwrap();
        fs.publish();
        let payload = pseudo(16 * 1024 * 1024, 6);
        assert_eq!(
            fs.write_buffer(large, 0, &payload),
            Ok(u32::try_from(payload.len()).unwrap())
        );
        assert_eq!(fs.write_buffer(small, 0, b"other inode"), Ok(11));

        let (entered_tx, entered) = std::sync::mpsc::channel();
        let (release, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        *fs.commit_pause.lock().unwrap() = Some(Arc::new(move |ino| {
            if ino == large {
                entered_tx.send(()).unwrap();
                let _ = release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(tilo_test_env::hang_bound());
            }
        }));
        let handle = &tokio::runtime::Handle::current();
        let fs = &fs;
        tokio::task::block_in_place(|| {
            std::thread::scope(|scope| {
                let flushing = scope.spawn(move || {
                    let _runtime = handle.enter();
                    fs.flush_inode(large)
                });
                entered.recv_timeout(tilo_test_env::hang_bound()).unwrap();
                let (done_tx, done) = std::sync::mpsc::channel();
                let other = scope.spawn(move || {
                    let _runtime = handle.enter();
                    let ReadPlan::Buffered(BufferedRead::Staged(Ok(StagedRead::Ready(read)))) =
                        fs.plan_read(small, 0, 0, 64)
                    else {
                        panic!("the small file is staged and complete");
                    };
                    let wrote = fs.write_buffer(small, 11, b"!");
                    let len = fs.staged_len(small);
                    done_tx.send(()).unwrap();
                    (read, wrote, len)
                });
                let finished = done.recv_timeout(tilo_test_env::hang_bound());
                release.send(()).unwrap();
                assert!(
                    finished.is_ok(),
                    "another inode's read waited for a large commit"
                );
                let (read, wrote, len) = other.join().unwrap();
                assert_eq!(read, b"other inode");
                assert_eq!(wrote, Ok(1));
                assert_eq!(len, Some(12));
                assert_eq!(flushing.join().unwrap(), Ok(()));
            });
        });
        let committed = fs.ns().hash_for(large).unwrap();
        let bytes = fs.hydrator.read(committed, 0, u64::MAX).await.unwrap();
        assert_eq!(
            blake3::hash(&bytes),
            blake3::hash(&payload),
            "the large file committed its bytes"
        );
    }

    /// What a reader of the mount gets for the whole of `ino`.
    fn served(fs: &QuickFS, ino: u64) -> Vec<u8> {
        match fs.plan_read(ino, 0, 0, 1 << 20) {
            ReadPlan::Local(data) => data,
            ReadPlan::Error(errno) => panic!("the read failed with errno {errno}"),
            ReadPlan::Buffered(_) => panic!("a closed file was read from a stage"),
            ReadPlan::Remote(..) => panic!("locally written content was asked of a peer"),
        }
    }

    /// A file written into the folder reads back byte for byte, whatever format its name
    /// names. `git reset --hard` once left every `.html` file empty.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_file_reads_back_the_bytes_written_whatever_its_format() {
        let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("formats").unwrap()));
        let fs = QuickFS::over(Arc::clone(&namespace), hydrator);
        tokio::task::spawn_blocking(move || {
            let contents: [(&str, &[u8]); 6] = [
                (
                    "html",
                    b"<!doctype html>\n<script>if (a < b && c > d) document.write('</p>');</script>\n<style>p > b { color: red }</style><p>unclosed <b>bold &nbsp; <!-- note -->\n",
                ),
                ("malformed", b"PK\x03\x04 not a zip <html><body></div>{\\rtf1 }"),
                ("non-utf8", b"caf\xe9 \xff\xfe\n# Title\n"),
                ("markdown", b"# Title\n\nsome *text*  \nwrapped\nline\n\n- a\n* b\n"),
                ("empty", b""),
                ("crlf", b"line one\r\nline two\r\n\r\n"),
            ];
            let extensions = [
                "md", "markdown", "MD", "odt", "docx", "rtf", "html", "htm", "HTML", "txt",
            ];
            for ext in extensions {
                for (what, bytes) in contents {
                    let name = format!("{what}.{ext}");
                    let ino = fs
                        .create_for_write(ROOT_INO, name.as_bytes(), 0o644)
                        .unwrap();
                    if !bytes.is_empty() {
                        assert_eq!(
                            fs.write_buffer(ino, 0, bytes),
                            Ok(u32::try_from(bytes.len()).unwrap()),
                            "{name}"
                        );
                    }
                    assert_eq!(fs.flush_inode(ino), Ok(()), "{name}");
                    fs.settle_last_close(ino, true);
                    assert_eq!(served(&fs, ino), bytes, "{name} reads back as written");
                    assert_eq!(
                        fs.presented_attr(ino).unwrap().0.size,
                        bytes.len() as u64,
                        "{name} is sized as written"
                    );
                }
            }
        })
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mode_given_to_a_file_or_directory_is_kept_without_setuid() {
        let hydrator = Arc::new(Hydrator::new(Arc::new(ContentStore::memory())));
        let namespace = Arc::new(Mutex::new(Namespace::new("modes").unwrap()));
        let fs = QuickFS::over(Arc::clone(&namespace), hydrator);
        tokio::task::spawn_blocking(move || {
            let tool = fs.create_empty_file(ROOT_INO, "tool").unwrap();
            assert_eq!(fs.ns().getattr(tool).unwrap().perm, 0o644);
            assert_eq!(fs.set_mode(tool, 0o100_755), Ok(()));
            assert_eq!(fs.ns().getattr(tool).unwrap().perm, 0o755);
            assert_eq!(fs.set_mode(tool, 0o104_755), Ok(()));
            assert_eq!(fs.ns().getattr(tool).unwrap().perm, 0o755, "never setuid");
            let shared = fs.create_empty_dir(ROOT_INO, "shared").unwrap();
            assert_eq!(fs.set_mode(shared, 0o42_775), Ok(()));
            assert_eq!(fs.ns().getattr(shared).unwrap().perm, 0o2775);
        })
        .await
        .unwrap();
    }
}

#[cfg(test)]
mod read_lock_tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;

    const ME: [u8; 32] = [0xB7; 32];
    const BYTES: &[u8] = b"local bytes served without a peer";

    struct Fixture {
        namespace: Arc<Mutex<Namespace>>,
        fs: QuickFS,
        shared: u64,
        file: u64,
    }

    async fn fixture() -> Fixture {
        let store = Arc::new(ContentStore::memory());
        let hash = store.write_content(BYTES).await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(store));
        let namespace = host();
        let (shared, file) = {
            let mut ns = lock_namespace(&namespace);
            let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
            let file = ns
                .create_file(shared, "a.txt", Some(&hash), BYTES.len() as u64, 1)
                .unwrap();
            drop(ns);
            (shared, file)
        };
        let fs = QuickFS::over(namespace.clone(), hydrator);
        Fixture {
            namespace,
            fs,
            shared,
            file,
        }
    }

    fn shared_read_policy() -> WorkspaceAcl {
        let mut acl = WorkspaceAcl::new();
        acl.grant_read(ME, "/shared");
        acl
    }

    #[derive(Debug, PartialEq, Eq)]
    enum Reply {
        Data(Vec<u8>),
        Errno(i32),
    }

    fn data() -> Reply {
        Reply::Data(BYTES.to_vec())
    }

    fn read_all(fs: &QuickFS, ino: u64, fh: u64) -> Reply {
        match fs.plan_read(ino, fh, 0, 4096) {
            ReadPlan::Local(data) => Reply::Data(data),
            ReadPlan::Error(errno) => Reply::Errno(errno),
            ReadPlan::Buffered(_) => panic!("a file with no stage was answered from a buffer"),
            ReadPlan::Remote(..) => panic!("locally stored content was sent to a peer"),
        }
    }

    fn lock_namespace(namespace: &Arc<Mutex<Namespace>>) -> std::sync::MutexGuard<'_, Namespace> {
        namespace.lock_recover()
    }

    /// A read is answered only where the caller's view and its own open handles reach: a
    /// revoke, a rename out of view or an unlink makes it ENOENT, a directory read is EISDIR,
    /// and moving back into view makes it readable again.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "each section keeps one former test's body verbatim"
    )]
    async fn a_read_sees_only_what_the_view_and_its_handles_reach() {
        {
            let Fixture { fs, file, .. } = fixture().await;
            let handle = AclHandle::new(shared_read_policy());
            let fs = fs.with_acl(handle.clone(), ME);
            assert_eq!(read_all(&fs, file, 0), data());

            handle.replace(WorkspaceAcl::new());
            assert_eq!(read_all(&fs, file, 0), Reply::Errno(ENOENT));
            assert_eq!(fs.attr_for_request(file, None).err(), Some(ENOENT));
            drop(fs);
        }
        {
            let Fixture {
                namespace,
                fs,
                shared,
                file,
            } = fixture().await;
            let fs = fs.with_acl(shared_read_policy(), ME);
            assert_eq!(read_all(&fs, file, 0), data());

            assert!(lock_namespace(&namespace).rename(shared, "a.txt", ROOT_INO, "a.txt", 2));
            assert_eq!(read_all(&fs, file, 0), Reply::Errno(ENOENT));
            assert_eq!(fs.attr_for_request(file, None).err(), Some(ENOENT));

            assert!(lock_namespace(&namespace).rename(ROOT_INO, "a.txt", shared, "a.txt", 3));
            assert_eq!(
                read_all(&fs, file, 0),
                data(),
                "moving back into the view makes the file readable again"
            );
            drop(fs);
        }
        {
            let Fixture {
                namespace,
                fs,
                file,
                ..
            } = fixture().await;
            let fs = fs.with_acl(shared_read_policy(), ME);
            let fh = 7;
            fs.open_file_handles.lock_recover().insert(
                fh,
                OpenFileHandleState {
                    ino: file,
                    opened_path: "/shared/a.txt".to_string(),
                    writable: false,
                    append: false,
                },
            );
            assert!(lock_namespace(&namespace).remove(file));

            assert_eq!(
                read_all(&fs, file, fh),
                data(),
                "the handle opened before the unlink keeps reading"
            );
            assert_eq!(
                fs.attr_for_request(file, Some(fh)).map(|attr| attr.size),
                Ok(BYTES.len() as u64)
            );
            assert_eq!(
                read_all(&fs, file, 0),
                Reply::Errno(ENOENT),
                "no handle, no read"
            );
            assert_eq!(fs.attr_for_request(file, None).err(), Some(ENOENT));
            drop(fs);
        }
        {
            let Fixture { fs, shared, .. } = fixture().await;
            let fs = fs.with_acl(shared_read_policy(), ME);
            assert_eq!(read_all(&fs, shared, 0), Reply::Errno(EISDIR));
            drop(fs);
        }
    }

    /// A closed file's stage becomes its content entry by a link, and a write after that close
    /// gets a stage of its own rather than changing the bytes the first version is named by.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_closed_stage_becomes_the_entry_and_a_later_write_leaves_it_alone() {
        use std::os::unix::fs::MetadataExt as _;

        use crate::test_bytes::pseudo;

        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("adopt").unwrap()));
        let fs = QuickFS::over(Arc::clone(&namespace), hydrator)
            .with_write_stage_dir(dir.path().join("stage"));
        let ino = fs.create_empty_file(ROOT_INO, "photo.jpg").unwrap();
        fs.publish();

        let original = pseudo(256 * 1024, 9);
        assert_eq!(
            fs.write_buffer(ino, 0, &original),
            Ok(u32::try_from(original.len()).unwrap())
        );
        assert_eq!(fs.flush_inode(ino), Ok(()));
        let first = fs.ns().hash_for(ino).unwrap();
        assert_eq!(
            store.files().entry(first.as_bytes()).unwrap().size,
            original.len() as u64,
            "the closed file is stored as one entry, not loose chunks"
        );
        let entry = std::fs::metadata(store.files().path_of(first.as_bytes())).unwrap();

        let changed = pseudo(256 * 1024, 10);
        assert_eq!(
            fs.write_buffer(ino, 0, &changed),
            Ok(u32::try_from(changed.len()).unwrap())
        );
        assert_eq!(fs.flush_inode(ino), Ok(()));
        let second = fs.ns().hash_for(ino).unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::metadata(store.files().path_of(first.as_bytes()))
                .unwrap()
                .ino(),
            entry.ino(),
            "the second version did not write the first version's entry"
        );
        let m1 = store.manifest(first).await.unwrap();
        assert_eq!(
            store
                .read_local(first, &m1, 0..original.len() as u64)
                .await
                .unwrap(),
            original,
            "the first version still reads its own bytes"
        );
        let m2 = store.manifest(second).await.unwrap();
        assert_eq!(
            store
                .read_local(second, &m2, 0..changed.len() as u64)
                .await
                .unwrap(),
            changed
        );
        drop(fs);
    }

    /// A close needs no async runtime and no thread beside it: the staged photo is read, chunked
    /// and linked in on the thread that answered it. This runs on a plain test thread with no
    /// runtime entered, so a close that waited on the store's runtime — as it did through
    /// `block_in_place` — would panic here rather than quietly cost a hand-off per photo.
    #[test]
    fn a_close_stores_its_photo_without_a_runtime() {
        use crate::test_bytes::pseudo;

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (fs, store) = runtime.block_on(async {
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)));
            let namespace = Arc::new(Mutex::new(Namespace::new("adopt-plain").unwrap()));
            let fs =
                QuickFS::over(namespace, hydrator).with_write_stage_dir(dir.path().join("stage"));
            (fs, store)
        });
        let ino = fs.create_empty_file(ROOT_INO, "photo.jpg").unwrap();
        let photo = pseudo(256 * 1024, 11);
        assert_eq!(
            fs.write_buffer(ino, 0, &photo),
            Ok(u32::try_from(photo.len()).unwrap())
        );
        assert_eq!(fs.flush_inode(ino), Ok(()));
        let hash = fs.ns().hash_for(ino).unwrap();
        assert_eq!(
            store.files().entry(hash.as_bytes()).unwrap().size,
            photo.len() as u64,
            "the closed photo is one entry"
        );
        let stored = std::fs::read(store.files().path_of(hash.as_bytes())).unwrap();
        assert_eq!(stored, photo, "the entry holds the photo's bytes");
    }
}

mod listing_stream_tests {
    use super::*;

    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;

    /// A listing is one pass over its folder: its later pages continue what its first page
    /// listed even while the folder is being filled, and the next listing asks again.
    #[tokio::test]
    async fn a_listing_s_pages_are_one_pass_and_the_next_listing_is_fresh() {
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(store));
        let namespace = Arc::new(Mutex::new(Namespace::new("listing-replica").unwrap()));
        let flat = {
            let mut ns = namespace.lock_recover();
            let flat = ns.create_dir(ROOT_INO, "flat", 1).unwrap();
            for i in 0..3_u64 {
                ns.create_file(flat, format!("f{i}"), None, 0, i).unwrap();
            }
            drop(ns);
            flat
        };
        let fs = QuickFS::over(namespace.clone(), hydrator);
        let first = fs.listing_page(flat, 0).unwrap();
        assert_eq!(first.len(), 5, "`.` and `..` and the three files");
        {
            let mut ns = namespace.lock_recover();
            ns.create_file(flat, "late", None, 0, 9).unwrap();
        }
        let next = fs
            .listing_page(flat, u64::try_from(first.len()).unwrap())
            .unwrap();
        assert_eq!(
            next, first,
            "a listing's pages are one pass over its folder"
        );
        let fresh = fs.listing_page(flat, 0).unwrap();
        assert_eq!(fresh.len(), 6, "the next listing sees the new name");
    }
}

mod settled_read_tests {
    use super::*;

    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::ContentStore;
    use crate::namespace::ROOT_INO;
    use crate::test_bytes::pseudo;

    /// The service the daemon runs for one workspace's tree: a persistent content store, the
    /// mount over a namespace with its change journal, and the window its writes sync on.
    fn service(
        root: &std::path::Path,
        handle: &tokio::runtime::Handle,
        journal: Option<ChangeJournal>,
    ) -> (Arc<QuickFS>, Arc<ContentStore>) {
        let _entered = handle.enter();
        let store = Arc::new(
            handle
                .block_on(ContentStore::open(root.join("content")))
                .unwrap(),
        );
        let hydrator = Arc::new(Hydrator::new(Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("settled-reads").unwrap()));
        let mut fs = QuickFS::over(namespace, hydrator).with_write_stage_dir(root.join("stage"));
        if let Some(journal) = journal {
            fs = fs.with_change_journal(journal);
        }
        fs.sync_on_window(Arc::clone(&store), None);
        (Arc::new(fs), store)
    }

    /// A file written through the mount's write path and closed: its bytes are committed content,
    /// with no stage a read could answer from.
    fn settled(fs: &QuickFS, name: &str, bytes: &[u8]) -> u64 {
        let ino = fs.create_empty_file(ROOT_INO, name).unwrap();
        fs.write_buffer(ino, 0, bytes).unwrap();
        fs.flush_inode(ino).unwrap();
        fs.drop_buffer(ino);
        ino
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .max_blocking_threads(32)
            .enable_all()
            .build()
            .unwrap()
    }

    /// A settled read started on a thread of its own, as a mount's worker answers one. Answers
    /// whether the reader got the whole file, and the thread to join once it is done.
    fn read_on_another_thread(
        fs: Arc<QuickFS>,
        handle: tokio::runtime::Handle,
        ino: u64,
        len: u32,
    ) -> (std::sync::mpsc::Receiver<bool>, std::thread::JoinHandle<()>) {
        let (done, finished) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            let _entered = handle.enter();
            let plan = fs.plan_read(ino, 0, 0, len);
            let _ =
                done.send(matches!(plan, ReadPlan::Local(bytes) if bytes.len() == len as usize));
        });
        (finished, reader)
    }

    /// A fresh file's journal reaches the disk inside the request that made it. That wait is the
    /// writer's, never a reader's: a read of settled content answers while the write it is beside
    /// is still in its journal.
    #[test]
    fn a_settled_read_does_not_wait_for_a_fresh_writes_journal() {
        let rt = runtime();
        let handle = rt.handle().clone();
        let dir = tempfile::tempdir().unwrap();
        let hold = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Mutex::new(release_rx);
        let journal: ChangeJournal = {
            let hold = Arc::clone(&hold);
            Arc::new(move |_namespace: &Mutex<Namespace>| {
                // The first journal after this is armed is the one held open: later calls
                // (a file's name and its content each journal once) pass at once.
                if hold.swap(false, Ordering::AcqRel) {
                    let _ = entered_tx.send(());
                    let _ = release_rx
                        .lock()
                        .unwrap()
                        .recv_timeout(tilo_test_env::hang_bound());
                }
                Ok(())
            })
        };
        let (fs, _store) = service(dir.path(), &handle, Some(journal));
        let bytes = pseudo(4096, 1);
        let ino = settled(&fs, "old.txt", &bytes);
        // From here a fresh write's journal waits until this test says it may.
        hold.store(true, Ordering::Release);
        let writer = {
            let fs = Arc::clone(&fs);
            let handle = handle.clone();
            std::thread::spawn(move || {
                let _entered = handle.enter();
                fs.create_empty_file(ROOT_INO, "fresh.txt")
            })
        };
        entered_rx
            .recv_timeout(tilo_test_env::hang_bound())
            .expect("the fresh write reached its journal");
        let (finished, reader) = read_on_another_thread(
            Arc::clone(&fs),
            handle,
            ino,
            u32::try_from(bytes.len()).unwrap(),
        );
        let answered = finished.recv_timeout(Duration::from_secs(5));
        let _ = release_tx.send(());
        drop(release_tx);
        let _ = writer.join().unwrap();
        let _ = reader.join();
        assert!(
            answered.unwrap_or(false),
            "a read of settled content waited for a fresh write's journal"
        );
    }

    /// Reads of settled content answer on the thread that asked for them: a reader never queues
    /// for a slot in the daemon's blocking pool, where fresh writes and syncs do their work.
    #[test]
    fn a_settled_read_does_not_wait_for_the_daemons_blocking_pool() {
        let rt = runtime();
        let handle = rt.handle().clone();
        let dir = tempfile::tempdir().unwrap();
        let (fs, _store) = service(dir.path(), &handle, None);
        let bytes = pseudo(4096, 2);
        let ino = settled(&fs, "old.txt", &bytes);
        // Every blocking thread the daemon has is taken, as a burst of fresh writes and their
        // syncs can take them.
        let (took_tx, took_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        for _ in 0..32 {
            let took_tx = took_tx.clone();
            let release_rx = Arc::clone(&release_rx);
            handle.spawn_blocking(move || {
                let _ = took_tx.send(());
                let _ = release_rx.lock().unwrap().recv();
            });
        }
        for _ in 0..32 {
            took_rx
                .recv_timeout(tilo_test_env::hang_bound())
                .expect("a blocking thread");
        }
        let (finished, reader) = read_on_another_thread(
            Arc::clone(&fs),
            handle,
            ino,
            u32::try_from(bytes.len()).unwrap(),
        );
        let answered = finished.recv_timeout(Duration::from_secs(5));
        drop(release_tx);
        let _ = reader.join();
        assert!(
            answered.unwrap_or(false),
            "a read of settled content waited for a blocking-pool slot"
        );
    }
}

mod shipped_access_tests {
    use super::*;

    use crate::acl::IdentityKey;
    use crate::caps_file::CapabilityFile;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;

    /// The person the mount acts as.
    const ME: IdentityKey = [0x5A; 32];

    /// The list a session sends a mount whose person has write access to workspace `id` by
    /// their role: the workspace's folder bounded, and the person granted inside it.
    fn sessions_list(id: &str) -> String {
        use tilo_action_policy::{Grantee, Role};
        use tilo_permissions::caps::Access;
        use tilo_permissions::caps::CapOverride;
        use tilo_permissions::caps::Effect;
        use tilo_permissions::caps::FsAuthority;
        use tilo_permissions::caps::Scope;

        let scope = Scope::FsPath(format!("/var/workspace/.workspaces/{id}"));
        let capabilities = vec![
            CapOverride::new(
                Grantee::role(Role::Viewer),
                scope.clone(),
                Effect::WorkspaceFileBoundary,
                Access::Read,
            ),
            CapOverride::new(
                Grantee::person(hex::encode(ME)),
                scope,
                Effect::WorkspaceFileAccess,
                Access::Write,
            ),
        ];
        serde_json::to_string(&FsAuthority { capabilities }).unwrap()
    }

    /// The mount holds no one's files at its root and opens a workspace's folder only by the
    /// role the session reports: until the session's list arrives, the list the image seeds
    /// refuses every write, at the root as a read-only file system and under a workspace's
    /// folder as a folder not there yet.
    #[tokio::test]
    async fn the_shipped_list_refuses_writes_until_the_sessions_list_arrives() {
        let machine = tempfile::tempdir().unwrap();
        let path = machine.path().join("fs-capabilities.json");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../os/usr/share/tilo/fs-capabilities.json"),
            &path,
        )
        .unwrap();
        let caps = CapabilityFile::load(
            &path,
            ME,
            vec![tilo_member_types::MemberId::Person(
                collab_types::PersonId::parse("local").unwrap(),
            )],
        )
        .expect("the shipped list loads");

        let namespace = host();
        let (workspaces, team, other) = {
            let mut ns = namespace.lock_recover();
            let workspaces = ns.create_dir(ROOT_INO, ".workspaces", 1).unwrap();
            let team = ns.create_dir(workspaces, "team", 1).unwrap();
            let other = ns.create_dir(workspaces, "other", 1).unwrap();
            drop(ns);
            (workspaces, team, other)
        };
        let fs = QuickFS::over(
            namespace,
            Arc::new(Hydrator::new(Arc::new(ContentStore::memory()))),
        )
        .with_acl(caps.handle(), ME);

        assert_eq!(fs.check_new_name(ROOT_INO, b"notes.md"), Err(EROFS));
        assert_eq!(fs.check_new_name(ROOT_INO, b".workspaces2"), Err(EROFS));
        assert_eq!(fs.check_new_name(team, b"notes.md"), Err(ENOENT));
        assert_eq!(fs.check_access(team, Access::Write), Err(ENOENT));
        assert_eq!(fs.check_access(workspaces, Access::Write), Err(ENOENT));

        caps.keep(&sessions_list("team"))
            .expect("the session's list is in force");

        assert_eq!(fs.check_new_name(team, b"notes.md"), Ok(()));
        assert_eq!(fs.check_access(team, Access::Write), Ok(()));
        assert_eq!(
            fs.check_new_name(ROOT_INO, b"notes.md"),
            Err(EROFS),
            "a person's role never opens the mount's root"
        );
        assert!(fs.check_new_name(workspaces, b"elsewhere").is_err());
        assert!(fs.check_access(workspaces, Access::Write).is_err());
        assert!(
            fs.check_new_name(other, b"notes.md").is_err(),
            "a workspace the person has no role in stays closed"
        );

        // The next start, before the session speaks again, mounts with what it said last.
        let restarted = CapabilityFile::load(&path, ME, Vec::new())
            .unwrap()
            .handle();
        assert!(restarted.permits(&ME, "/.workspaces/team/notes.md", Access::Write));
        assert!(!restarted.permits(&ME, "/notes.md", Access::Write));
    }
}
