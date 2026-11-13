use super::*;

mod availability_control_tests {
    use super::*;

    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    async fn wait_for_availability(
        registry: &Arc<Mutex<AvailabilityRegistry>>,
        record: fs_protocol::FileRecordId,
        predicate: impl Fn(&RecordAvailability) -> bool + Send + Sync,
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
        record: fs_protocol::FileRecordId,
        timeout: Duration,
        predicate: impl Fn(&RecordAvailability) -> bool + Send + Sync,
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

    async fn pinned_content_source() -> (Endpoint, Router, Hash, Hash) {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = ContentStore::memory();
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
        let router = Router::builder(endpoint.clone())
            .accept(iroh_blobs::ALPN, store.blobs_protocol(None))
            .spawn();
        (endpoint, router, first, second)
    }

    async fn close_pinned_test_source(router: Router, source: Endpoint, client: Endpoint) {
        router.shutdown().await.unwrap();
        source.close().await;
        client.close().await;
    }

    fn current_pinned_candidate(
        namespace: &Arc<Mutex<Namespace>>,
        registry: &Arc<Mutex<AvailabilityRegistry>>,
    ) -> PinnedCandidate {
        let hash = Hash::from([0x7a; 32]);
        let (record_id, parent) = {
            let mut namespace = namespace.lock().unwrap();
            let ino = namespace
                .create_file(ROOT_INO, "pinned.bin", Some(&hash), 1, 1)
                .unwrap();
            (
                namespace.record_id_for(ino).unwrap(),
                namespace.record_id_for(ROOT_INO),
            )
        };
        let root = parent.unwrap();
        registry
            .lock()
            .unwrap()
            .put(RecordAvailability::new(root, RecordKind::Directory))
            .unwrap();
        let mut record = RecordAvailability::new(record_id, RecordKind::File);
        record.parent = parent;
        record.policy = AvailabilityPolicy::Pinned;
        registry.lock().unwrap().put(record).unwrap();
        PinnedCandidate {
            record_id,
            parent,
            kind: RecordKind::File,
            version: None,
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
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
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
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
            let candidate = current_pinned_candidate(&namespace, &registry);
            let (wake, mut wakes) = tokio::sync::watch::channel(0_u64);
            let cancellation_registry = Arc::clone(&registry);
            let record_id = candidate.record_id;
            let unpin = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                let mut registry = cancellation_registry.lock().unwrap();
                let mut record = registry.get(record_id).unwrap().clone();
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
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
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
            let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
            let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
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
                    .filter(|record| record.kind == RecordKind::File)
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
                record.kind == RecordKind::Directory
                    || (record.transfer.phase == TransferPhase::Idle
                        && record.transfer.retry_count == 0
                        && record.transfer.retry_at_ms.is_none())
            }));
            endpoint.close().await;
        }
    }

    #[tokio::test]
    async fn pin_policy_persists_and_read_only_collaborators_manage_offline_policy() {
        {
            let dir = tempfile::tempdir().unwrap();
            let registry_path = dir.path().join("availability.json");
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(&registry_path).unwrap(),
            ));
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_availability(registry.clone());

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
            let folder_id = namespace.lock_recover().record_id_for(folder).unwrap();
            assert_eq!(
                reopened.get(folder_id).map(|record| record.policy),
                Some(AvailabilityPolicy::Pinned)
            );
        }
        {
            const READER: [u8; 32] = [0x51; 32];
            let dir = tempfile::tempdir().unwrap();
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "shared.txt", None, 0, 1)
                .unwrap();
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
            let mut acl = crate::acl::WorkspaceAcl::new();
            acl.grant_read(READER, "/shared.txt");
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_availability(Arc::clone(&registry))
                .with_acl(acl, READER);

            assert_eq!(fs.check_access(ino, Access::Write), Err(EROFS));
            fs.set_pin_policy(ino, AvailabilityPolicy::Pinned).unwrap();
            let record_id = namespace.lock_recover().record_id_for(ino).unwrap();
            assert_eq!(
                registry.lock_recover().get(record_id).unwrap().policy,
                AvailabilityPolicy::Pinned
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pinned_folder_hydrates_future_child_retries_after_restart_and_tracks_new_version() {
        let dir = tempfile::tempdir().unwrap();
        let registry_path = dir.path().join("availability.json");
        let (source_endpoint, source_router, first, second) = pinned_content_source().await;
        let client_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let client_store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(client_endpoint.clone(), client_store));
        let namespace = Arc::new(Mutex::new(Namespace::new("pinned-client").unwrap()));
        let folder = namespace
            .lock()
            .unwrap()
            .create_dir(ROOT_INO, "offline", 1)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();

        let child = namespace
            .lock()
            .unwrap()
            .create_file(folder, "later.txt", Some(&first), 20, 2)
            .unwrap();
        let record_id = namespace.lock().unwrap().record_id_for(child).unwrap();
        let first_version = fs_protocol::FileVersionId::new();
        let first_revision = fs_protocol::ContentRevisionV1 {
            schema_version: fs_protocol::CONTENT_REVISION_VERSION,
            record_id,
            version_id: first_version,
            content_hash: Some(fs_protocol::ContentHash(*first.as_bytes())),
            byte_len: 20,
            metadata_hash: fs_protocol::ContentHash([1; 32]),
        };
        assert!(
            namespace
                .lock()
                .unwrap()
                .set_content_revision(child, &first_revision, 2)
        );
        fs.wake_pinned_hydration();
        wait_for_availability(&registry, record_id, |record| {
            record.transfer.last_error.is_some()
                && record.transfer.retry_count > 0
                && record.transfer.retry_at_ms.is_some()
        })
        .await;
        drop(fs);
        drop(registry);

        hydrator.add_peer(source_endpoint.addr());
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(&registry_path).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration();
        wait_for_availability(&registry, record_id, |record| {
            record.current_version == Some(first_version)
                && record.residency == Residency::Resident { bytes: 20 }
                && record.transfer.last_error.is_none()
        })
        .await;
        let second_version = fs_protocol::FileVersionId::new();
        let second_revision = fs_protocol::ContentRevisionV1 {
            version_id: second_version,
            content_hash: Some(fs_protocol::ContentHash(*second.as_bytes())),
            byte_len: 21,
            metadata_hash: fs_protocol::ContentHash([2; 32]),
            ..first_revision
        };
        assert!(
            namespace
                .lock()
                .unwrap()
                .set_content_revision(child, &second_revision, 3)
        );
        fs.wake_pinned_hydration();
        wait_for_availability(&registry, record_id, |record| {
            record.current_version == Some(second_version)
                && record.residency == Residency::Resident { bytes: 21 }
                && record.transfer.last_error.is_none()
        })
        .await;

        drop(fs);
        close_pinned_test_source(source_router, source_endpoint, client_endpoint).await;
    }

    #[tokio::test]
    async fn keep_offline_holds_bytes_against_the_cache_budget_only_while_pinned() {
        use crate::store::area::Retention;
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let size = crate::content::FILE_BACKED_MIN + 1;
        let bytes = |byte: u8| vec![byte; usize::try_from(size).unwrap()];
        let read = store.write_content(&bytes(1)).await.unwrap().unwrap();
        let authored = store.write_content(&bytes(2)).await.unwrap().unwrap();
        store
            .files()
            .set_retention(read.as_bytes(), Retention::Cached)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), Arc::clone(&store)));
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
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
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
        endpoint.close().await;
    }

    #[tokio::test]
    async fn a_pinned_download_never_evicts_a_file_the_same_pin_already_holds() {
        use crate::store::area::Retention;
        let size = crate::content::FILE_BACKED_MIN + 1;
        let bytes = |byte: u8| vec![byte; usize::try_from(size).unwrap()];
        let source_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let source_store = ContentStore::memory();
        let remote = source_store
            .write_content(&bytes(2))
            .await
            .unwrap()
            .unwrap();
        let source_router = Router::builder(source_endpoint.clone())
            .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
            .spawn();

        let dir = tempfile::tempdir().unwrap();
        let client_endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let here = store.write_content(&bytes(1)).await.unwrap().unwrap();
        store
            .files()
            .set_retention(here.as_bytes(), Retention::Cached)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(client_endpoint.clone(), Arc::clone(&store)));
        hydrator.add_peer(source_endpoint.addr());
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
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
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
        close_pinned_test_source(source_router, source_endpoint, client_endpoint).await;
    }

    fn worker_turns(fs: &QuickFS) -> u64 {
        fs.pinned_hydration
            .as_ref()
            .unwrap()
            .turns
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    async fn after_turns(fs: &QuickFS, turns: u64, every: Duration) {
        let from = worker_turns(fs);
        tilo_test_env::within(format!("{turns} turns of the pin worker"), async {
            while worker_turns(fs) < from.saturating_add(turns) {
                tokio::time::sleep(every).await;
            }
        })
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pin_worker_idle_tick_does_not_walk_namespace() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("idle-pin").unwrap()));
        let (folder, last) = {
            let mut namespace = namespace.lock().unwrap();
            let folder = namespace.create_dir(ROOT_INO, "offline", 1).unwrap();
            let mut last = ROOT_INO;
            for index in 0..8 {
                last = namespace
                    .create_file(folder, format!("empty-{index}"), None, 0, 1)
                    .unwrap();
            }
            (folder, namespace.record_id_for(last).unwrap())
        };
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let repair = Duration::from_millis(20);
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry))
            .with_pinned_hydration_every(repair);
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();
        wait_for_availability(&registry, last, |record| {
            record.residency == Residency::Resident { bytes: 0 }
        })
        .await;
        let walks = || namespace.lock().unwrap().node_entry_read_count();
        let settled = loop {
            let before = walks();
            after_turns(&fs, 2, repair).await;
            if walks() == before {
                break before;
            }
        };
        after_turns(&fs, 25, repair).await;
        assert_eq!(
            walks(),
            settled,
            "ticks with nothing changed must not walk the namespace"
        );

        let late = {
            let mut namespace = namespace.lock().unwrap();
            let ino = namespace
                .create_file(folder, "arrived-remotely", None, 0, 2)
                .unwrap();
            namespace.record_id_for(ino).unwrap()
        };
        wait_for_availability(&registry, late, |record| {
            record.residency == Residency::Resident { bytes: 0 }
        })
        .await;
        assert!(walks() > settled);

        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pins_are_repaired_on_announcements_without_a_tick() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let local = store.write_content(b"already here").await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let namespace = Arc::new(Mutex::new(Namespace::new("announced-pin").unwrap()));
        let (folder, first) = {
            let mut namespace = namespace.lock().unwrap();
            let folder = namespace.create_dir(ROOT_INO, "offline", 1).unwrap();
            let first = namespace.create_file(folder, "first", None, 0, 1).unwrap();
            (folder, namespace.record_id_for(first).unwrap())
        };
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
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
            namespace.record_id_for(ino).unwrap()
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
                namespace.record_id_for(ROOT_INO).unwrap(),
                namespace.record_id_for(other).unwrap(),
                namespace.record_id_for(inside).unwrap(),
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
                    .put(RecordAvailability::new(root, RecordKind::Directory))
                    .unwrap();
            }
            let mut pinned = RecordAvailability::new(other, RecordKind::Directory);
            pinned.parent = Some(root);
            pinned.policy = AvailabilityPolicy::Pinned;
            registry.put(pinned).unwrap();
        }
        wait_for_availability(&registry, inside, |record| {
            record.residency == Residency::Resident { bytes: 12 }
        })
        .await;

        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test]
    async fn first_pin_pass_persists_registry_once() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let local = store.write_content(b"already here").await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let namespace = Arc::new(Mutex::new(Namespace::new("pin-once").unwrap()));
        let (folder, empty, resident) = {
            let mut namespace = namespace.lock().unwrap();
            let folder = namespace.create_dir(ROOT_INO, "offline", 1).unwrap();
            let mut empty = ROOT_INO;
            for index in 0..1_000 {
                empty = namespace
                    .create_file(folder, format!("empty-{index:04}"), None, 0, 1)
                    .unwrap();
            }
            let resident = namespace
                .create_file(folder, "resident.txt", Some(&local), 12, 1)
                .unwrap();
            (
                folder,
                namespace.record_id_for(empty).unwrap(),
                namespace.record_id_for(resident).unwrap(),
            )
        };
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), Arc::clone(&hydrator), heuristics)
            .with_availability(Arc::clone(&registry));
        fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
            .unwrap();
        let (_wake, wakes) = tokio::sync::watch::channel(0_u64);
        let persists = || registry.lock().unwrap().persist_count();

        let before = persists();
        let outcome = reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes.clone(),
        )
        .await;
        assert_eq!(
            persists().saturating_sub(before),
            1,
            "a pass over a 1 000-record pinned folder commits the registry once"
        );
        assert!(!outcome.more_due);
        {
            let registry = registry.lock().unwrap();
            assert_eq!(
                registry.get(empty).unwrap().residency,
                Residency::Resident { bytes: 0 }
            );
            assert_eq!(
                registry.get(resident).unwrap().residency,
                Residency::Resident { bytes: 12 }
            );
            assert_eq!(
                registry.effective_policy(resident).unwrap(),
                Some(AvailabilityPolicy::Pinned)
            );
            drop(registry);
        }

        let before = persists();
        let _ = reconcile_pinned_hydration(
            &namespace,
            Arc::clone(&hydrator),
            Arc::clone(&registry),
            None,
            wakes,
        )
        .await;
        assert_eq!(
            persists(),
            before,
            "a pass that finds everything current writes nothing"
        );

        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test]
    async fn transfer_retries_run_off_thread_and_report_actionable_outcomes() {
        {
            let dir = tempfile::tempdir().unwrap();
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hash = store
                .write_content(b"available locally")
                .await
                .unwrap()
                .unwrap();
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_availability(registry.clone());
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "retry.txt", Some(&hash), 17, 1)
                .unwrap();
            let record = namespace.lock_recover().record_id_for(ino).unwrap();

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
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let registry = Arc::new(Mutex::new(
                AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
            ));
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_availability(registry.clone());
            let ino = namespace
                .lock_recover()
                .create_file(ROOT_INO, "empty.txt", None, 0, 1)
                .unwrap();
            let record = namespace.lock_recover().record_id_for(ino).unwrap();

            fs.retry_transfer(ino).unwrap();
            assert_eq!(
                registry.lock_recover().get(record).unwrap().residency,
                Residency::Resident { bytes: 0 }
            );
        }
    }

    #[tokio::test]
    async fn retry_failure_is_coarse_and_actionable() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_availability(registry.clone());
        let missing = Hash::from([9; 32]);
        let ino = namespace
            .lock_recover()
            .create_file(ROOT_INO, "missing.txt", Some(&missing), 4, 1)
            .unwrap();
        let record = namespace.lock_recover().record_id_for(ino).unwrap();

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restart_does_not_project_completed_old_version_as_not_resident() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, Arc::clone(&store)));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock_recover()
            .create_file(ROOT_INO, "empty.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace.lock_recover().record_id_for(ino).unwrap();
        let version_id = fs_protocol::FileVersionId::new();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let mut availability = RecordAvailability::new(record_id, RecordKind::File);
        availability.current_version = Some(version_id);
        availability.residency = Residency::Resident { bytes: 0 };
        registry.lock_recover().put(availability).unwrap();
        let mut queue = EvictionQueue::open(dir.path().join("evictions.json")).unwrap();
        let plan = crate::eviction::EvictionPlan {
            record_id,
            version_id,
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
        let newer_version = fs_protocol::FileVersionId::new();
        let mut current = registry.lock_recover().get(record_id).cloned().unwrap();
        current.current_version = Some(newer_version);
        current.residency = Residency::Resident { bytes: 0 };
        registry.lock_recover().put(current).unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_availability(Arc::clone(&registry))
            .with_eviction_queue(queue);

        fs.resume_pending_evictions();
        assert_eq!(
            registry.lock_recover().get(record_id).unwrap().residency,
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
    use iroh::endpoint::Endpoint;
    use iroh::endpoint::presets;
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

    async fn fixture(
        thumb_dir: std::path::PathBuf,
    ) -> (Arc<Mutex<Namespace>>, Arc<ContentStore>, QuickFS) {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store.clone()));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let provider = ThumbProvider::open(&thumb_dir).unwrap();
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics).with_thumbs(provider);
        (namespace, store, fs)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    async fn thumbnails_are_generated_verified_and_published() {
        {
            let dir = tempfile::tempdir().unwrap();
            let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

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
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thumbnails_survive_remounts_and_are_absent_without_content() {
        {
            let dir = tempfile::tempdir().unwrap();
            let png = solid_png(320, 240, [200, 60, 30]);

            let thumb = {
                let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;
                let mh = store.write_content(&png).await.unwrap().unwrap();
                let ino = {
                    let mut ns = namespace.lock_recover();
                    ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                        .unwrap()
                };
                fs.thumbnail_for(ino).expect("generated")
            };

            let (namespace2, store2, fs2) = fixture(dir.path().join("thumbs")).await;
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
            let (namespace, _store, fs) = fixture(dir.path().join("thumbs")).await;

            let projected = iroh_blobs::Hash::from([3u8; 32]);
            let img_ino = {
                let mut ns = namespace.lock_recover();
                ns.create_file(ROOT_INO, "remote.png", Some(&projected), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(img_ino).is_none(),
                "no preview without local content — never hydrate the file to make one"
            );

            let zip_ino = {
                let mut ns = namespace.lock_recover();
                ns.create_file(ROOT_INO, "bundle.zip", Some(&projected), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(zip_ino).is_none(),
                "a .zip is a badge, not a preview"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_replicated_thumbnail_is_served_before_content() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

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
    }
}

#[cfg(test)]
mod predictive_tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};

    async fn fixture() -> (Arc<Mutex<Namespace>>, QuickFS) {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        (
            namespace.clone(),
            QuickFS::new(namespace, hydrator, heuristics),
        )
    }

    #[tokio::test]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "each fixture keeps one (ACL-filtered) namespace snapshot while asserting ordered predictive candidates"
    )]
    async fn prediction_selects_visible_siblings_of_the_same_type() {
        {
            let (namespace, fs) = fixture().await;
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
            let (namespace, fs) = fixture().await;
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
            let (namespace, fs) = fixture().await;
            let (visible_record, hidden_record) = {
                let mut ns = namespace.lock_recover();
                let folder = ns.create_dir(ROOT_INO, "folder", 1).unwrap();
                let visible = ns
                    .create_file(folder, "visible.txt", Some(&Hash::from([1; 32])), 10, 1)
                    .unwrap();
                let hidden = ns
                    .create_file(ROOT_INO, "hidden.txt", Some(&Hash::from([2; 32])), 20, 1)
                    .unwrap();
                (
                    ns.record_id_for(visible).unwrap(),
                    ns.record_id_for(hidden).unwrap(),
                )
            };
            let mut acl = WorkspaceAcl::new();
            acl.grant_read(ME, "/folder/visible.txt");
            let fs = fs.with_acl(acl, ME);

            let manifest: fs_protocol::projection::RecordIndexManifestV3 = serde_json::from_slice(
                &fs.record_index_manifest_v3().expect("root record catalog"),
            )
            .unwrap();
            let projection: fs_protocol::projection::RecordIndexPageV3 = serde_json::from_slice(
                &fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .expect("the first page"),
            )
            .unwrap();
            assert!(projection.records.iter().any(|entry| {
                entry.record_id == visible_record && entry.path == "/folder/visible.txt"
            }));
            assert!(
                projection
                    .records
                    .iter()
                    .all(|entry| entry.record_id != hidden_record),
                "a record outside the mount's ACL-filtered view cannot enter search metadata"
            );
        }
        {
            let (namespace, fs) = fixture().await;
            {
                let mut ns = namespace.lock_recover();
                for index in 0..fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES + 1 {
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
            let manifest: fs_protocol::projection::RecordIndexManifestV2 =
                serde_json::from_slice(&fs.record_index_manifest_v2().unwrap()).unwrap();
            manifest.validate().unwrap();
            assert_eq!(
                manifest.records,
                fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES + 1
            );
            let first: fs_protocol::projection::RecordIndexPageV2 =
                serde_json::from_slice(&fs.record_index_page_v2(&manifest.revision, 0).unwrap())
                    .unwrap();
            let second_offset = first.next_offset.expect("continuation page");
            let second: fs_protocol::projection::RecordIndexPageV2 = serde_json::from_slice(
                &fs.record_index_page_v2(&manifest.revision, second_offset)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 1);

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "new.txt", None, 1, 1)
                .unwrap();
            assert!(
                fs.record_index_page_v2(&manifest.revision, 0).is_none(),
                "a namespace revision invalidates a page cursor rather than mixing rows"
            );
        }
        {
            const ME: [u8; 32] = [0xA3; 32];
            let (namespace, fs) = fixture().await;
            namespace
                .lock_recover()
                .create_file(ROOT_INO, "visible.txt", Some(&Hash::from([3; 32])), 3, 1)
                .unwrap();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible.txt");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: fs_protocol::projection::RecordIndexManifestV2 =
                serde_json::from_slice(&fs.record_index_manifest_v2().unwrap()).unwrap();

            handle.replace(policy);
            let after: fs_protocol::projection::RecordIndexManifestV2 =
                serde_json::from_slice(&fs.record_index_manifest_v2().unwrap()).unwrap();
            assert_ne!(before.revision, after.revision);
            assert!(
                fs.record_index_page_v2(&before.revision, 0).is_none(),
                "a page issued under the old ACL generation cannot be replayed"
            );
        }
    }

    #[tokio::test]
    async fn v3_record_catalog_hides_denied_rows_and_fails_closed_on_revoke() {
        {
            const ME: [u8; 32] = [0xA4; 32];
            let (namespace, fs) = fixture().await;
            let visible_record = {
                let mut ns = namespace.lock_recover();
                for index in 0..(fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES * 2) {
                    ns.create_file(ROOT_INO, format!("hidden-{index:04}"), None, 0, 1)
                        .unwrap();
                }
                let ino = ns.create_file(ROOT_INO, "visible.txt", None, 0, 1).unwrap();
                ns.record_id_for(ino).unwrap()
            };
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible.txt");
            let fs = fs.with_acl(AclHandle::new(policy), ME);

            let before = fs.record_index_manifest_v3().unwrap();
            let manifest: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&before).unwrap();
            manifest.validate().unwrap();
            assert_eq!(manifest.initial_cursor.len(), 36);
            let first: fs_protocol::projection::RecordIndexPageV3 = serde_json::from_slice(
                &fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(first.records.len(), 1);
            assert_eq!(first.records[0].record_id, visible_record);
            assert!(first.next_cursor.is_none());
            assert!(
                fs.record_index_page_v3(&manifest.revision, "00000000-0000-0000-0000-000000000000")
                    .is_none()
            );

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "another-hidden.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_record_index();
            assert_eq!(fs.record_index_manifest_v3().unwrap(), before);
            assert!(
                fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .is_some()
            );
        }
        {
            const ME: [u8; 32] = [0xA5; 32];
            let (namespace, fs) = fixture().await;
            let first_record = {
                let mut ns = namespace.lock_recover();
                let ino = ns.create_file(ROOT_INO, "first.txt", None, 0, 1).unwrap();
                ns.record_id_for(ino).unwrap()
            };
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            assert!(fs.record_index_by_id_v2(first_record).is_some());

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "second.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_record_index();
            let after: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            assert_ne!(before.revision, after.revision);
            assert!(
                fs.record_index_page_v3(&before.revision, &before.initial_cursor)
                    .is_none()
            );

            handle.replace(WorkspaceAcl::new());
            let mut revoked = None;
            for _ in 0..64 {
                revoked = fs.record_index_manifest_v3();
                if revoked.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let revoked: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&revoked.unwrap()).unwrap();
            assert_ne!(after.revision, revoked.revision);
            assert!(
                fs.record_index_page_v3(&after.revision, &after.initial_cursor)
                    .is_none()
            );
            assert!(fs.record_index_by_id_v2(first_record).is_none());
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
            let (namespace, fs) = fixture().await;
            {
                let mut ns = namespace.lock_recover();
                for index in 0..=fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES {
                    ns.create_projected_file(
                        ROOT_INO,
                        format!("visible-{index:04}"),
                        fs_protocol::FileRecordId(uuid::Uuid::from_u128((index + 1) as u128)),
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
            let manifest: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            let rebuilds = namespace.lock_recover().record_index_rebuild_count();
            let page_reads = namespace.lock_recover().record_index_page_read_count();
            let first: fs_protocol::projection::RecordIndexPageV3 = serde_json::from_slice(
                &fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                first.records.len(),
                fs_protocol::projection::RECORD_INDEX_PAGE_ENTRIES
            );
            let second: fs_protocol::projection::RecordIndexPageV3 = serde_json::from_slice(
                &fs.record_index_page_v3(&manifest.revision, first.next_cursor.as_deref().unwrap())
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 1);
            assert!(second.next_cursor.is_none());
            assert!(
                fs.record_index_by_id_v2(first.records[0].record_id)
                    .is_some()
            );
            assert_eq!(
                namespace.lock_recover().record_index_rebuild_count(),
                rebuilds,
                "serving v3 continuations must not rebuild or walk the Automerge corpus"
            );
            assert_eq!(
                namespace.lock_recover().record_index_page_read_count(),
                page_reads,
                "manifest, page, and exact record requests use the retained snapshot, never Namespace pages"
            );
        }
        {
            const ME: [u8; 32] = [0xA7; 32];
            let (namespace, fs) = fixture().await;
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
            let before: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            let page_reads = namespace.lock_recover().record_index_page_read_count();

            namespace
                .lock_recover()
                .create_file(ROOT_INO, "hidden-remote.txt", None, 0, 1)
                .unwrap();
            for _ in 0..16 {
                acl.replace(policy.clone());
            }
            let mut hidden = None;
            for _ in 0..64 {
                hidden = fs.record_index_manifest_v3();
                if hidden.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let hidden: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&hidden.unwrap()).unwrap();
            assert_eq!(
                hidden.page_entries, before.page_entries,
                "only the visible record is listed"
            );
            let after_hidden_reads = namespace.lock_recover().record_index_page_read_count();
            assert!(
                after_hidden_reads.saturating_sub(page_reads) <= 2,
                "a burst must coalesce before rebuilding the retained view"
            );

            namespace
                .lock_recover()
                .create_file(visible_dir, "remote.txt", None, 0, 1)
                .unwrap();
            let mut updated = None;
            acl.replace(policy.clone());
            for _ in 0..64 {
                updated = fs.record_index_manifest_v3();
                if updated.as_ref().is_some_and(|bytes| {
                    serde_json::from_slice::<fs_protocol::projection::RecordIndexManifestV3>(bytes)
                        .is_ok_and(|manifest| manifest.revision != before.revision)
                }) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let updated: fs_protocol::projection::RecordIndexManifestV3 =
                serde_json::from_slice(&updated.unwrap()).unwrap();
            assert_ne!(updated.revision, before.revision);
        }
        {
            const ME: [u8; 32] = [0xA8; 32];
            let (_namespace, fs) = fixture().await;
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let acl = AclHandle::new(policy.clone());
            let fs = fs.with_acl(acl.clone(), ME);
            let weak = Arc::downgrade(&fs.acl_visible_record_index);
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
        let (namespace, fs) = fixture().await;
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
        let rows = || namespace.lock_recover().indexed_record_count();
        let initial = fs.acl_visible_record_index().unwrap();
        assert_eq!(initial.acl_evaluations, rows());

        fs.refresh_acl_visible_record_index();
        assert!(Arc::ptr_eq(
            &initial,
            &fs.acl_visible_record_index().unwrap()
        ));

        let revealed = {
            let mut ns = namespace.lock_recover();
            ns.create_file(visible, "late.txt", None, 0, 2).unwrap();
            assert!(ns.rename(ROOT_INO, "hidden-000", visible, "revealed.txt", 2));
            ns.entry_at("visible/revealed.txt").unwrap().record_id
        };
        fs.refresh_acl_visible_record_index();
        let incremental = fs.acl_visible_record_index().unwrap();
        assert_eq!(incremental.acl_evaluations, 2);
        assert!(incremental.by_id.contains_key(&revealed));

        *fs.acl_visible_record_index.lock_recover() = None;
        fs.refresh_acl_visible_record_index();
        let full = fs.acl_visible_record_index().unwrap();
        assert_eq!(full.acl_evaluations, rows());
        assert_eq!(full.records, incremental.records);
        assert_eq!(full.hidden, incremental.hidden);
        assert_eq!(full.revision, incremental.revision);

        let mut widened = WorkspaceAcl::new();
        widened.grant_read(ME, "/");
        handle.replace(widened);
        fs.refresh_acl_visible_record_index();
        let regranted = fs.acl_visible_record_index().unwrap();
        assert_eq!(regranted.acl_evaluations, rows());
        assert!(regranted.hidden.is_empty());
    }
}

#[cfg(test)]
mod history_integration_tests {
    use super::*;
    use collab_types::Ed25519PublicKey;
    use fs_protocol::{FileActor, FileProvenance};
    use iroh::endpoint::{Endpoint, presets};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn existing_record_edit_and_rename_append_durable_versions() {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock_recover()
            .create_file(crate::namespace::ROOT_INO, "before.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace.lock_recover().record_id_for(ino).unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(crate::history_fixture::History::new(Arc::clone(&namespace)));
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_history(actor)
            .unwrap();

        let baseline = history.snapshot(record_id).unwrap();
        assert_eq!(baseline.versions.len(), 1);
        assert_eq!(baseline.versions[0].provenance, FileProvenance::Create);

        fs.commit_content(ino, b"durable bytes").unwrap();
        let after_edit = history.snapshot(record_id).unwrap();
        assert_eq!(after_edit.versions.len(), 2);
        assert_eq!(after_edit.versions[1].provenance, FileProvenance::Edit);
        assert_eq!(after_edit.versions[1].parents, baseline.heads);
        assert_eq!(
            after_edit.versions[1].content_hash,
            namespace
                .lock_recover()
                .hash_for(ino)
                .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()))
        );
        fs.commit_content(ino, b"durable bytes").unwrap();
        assert_eq!(
            history.snapshot(record_id).unwrap().versions.len(),
            2,
            "flush/fsync/release of unchanged bytes must not invent versions"
        );

        fs.rename_named(
            crate::namespace::ROOT_INO,
            "before.txt",
            crate::namespace::ROOT_INO,
            "after.txt",
        )
        .unwrap();
        let after_rename = history.snapshot(record_id).unwrap();
        assert_eq!(after_rename.versions.len(), 3);
        assert_eq!(after_rename.versions[2].provenance, FileProvenance::Rename);
        assert_eq!(namespace.lock_recover().record_id_for(ino), Some(record_id));

        drop(fs);
        drop(history);
        let reopened =
            Namespace::from_bytes("reopened-history", &namespace.lock_recover().save()).unwrap();
        assert_eq!(
            reopened.record_history(record_id).unwrap().versions.len(),
            3
        );
    }

    fn history_fs(
        endpoint: &Endpoint,
        namespace: Namespace,
    ) -> (QuickFS, Arc<crate::history_fixture::History>) {
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(crate::store::ContentStore::memory()),
        ));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let namespace = Arc::new(Mutex::new(namespace));
        let history = Arc::new(crate::history_fixture::History::new(Arc::clone(&namespace)));
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(actor)
            .unwrap();
        (fs, history)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn document_versions_are_fetchable() {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let mut namespace = Namespace::new("documents").unwrap();
        let doc = namespace
            .create_document(crate::namespace::ROOT_INO, "notes.md", "md", 1)
            .unwrap();
        let record_id = namespace.record_id_for(doc).unwrap();
        let (fs, history) = history_fs(&endpoint, namespace);
        assert!(fs.is_document(doc));

        let edits: [&[u8]; 2] = [b"# One\n\nfirst\n", b"# Two\n\nsecond\n"];
        for edit in edits {
            assert_eq!(fs.commit_bytes(doc, edit), Ok(()));
        }
        let snapshot = history.snapshot(record_id).unwrap();
        let edited = snapshot
            .versions
            .iter()
            .filter(|version| version.provenance == FileProvenance::Edit)
            .count();
        assert_eq!(edited, edits.len());
        let mut read = Vec::new();
        for version in &snapshot.versions {
            let Some(hash) = version.content_hash else {
                assert_ne!(version.provenance, FileProvenance::Edit);
                continue;
            };
            let bytes = fs
                .hydrator
                .read(Hash::from(hash.0), 0, u64::MAX)
                .await
                .expect("every recorded document version is fetchable");
            assert_eq!(
                fs.history_version_preview(doc, version.id).unwrap(),
                bytes,
                "history previews the version's own payload"
            );
            read.push(bytes);
        }
        for edit in edits {
            assert!(
                read.iter().any(|bytes| bytes == edit),
                "{:?} is readable from history",
                String::from_utf8_lossy(edit)
            );
        }
        drop(fs);
        endpoint.close().await;
    }
}

mod allocation_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    std::thread_local! {
        static ALLOCATED: Cell<u64> = const { Cell::new(0) };
    }

    fn add(bytes: usize) {
        let _ = ALLOCATED.try_with(|count| count.set(count.get().saturating_add(bytes as u64)));
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            add(layout.size());
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            add(layout.size());
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            add(new_size);
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    pub fn allocated_by(run: impl FnOnce()) -> u64 {
        let before = ALLOCATED.with(Cell::get);
        run();
        ALLOCATED.with(Cell::get).saturating_sub(before)
    }
}

mod write_stage_tests {
    use super::*;

    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    use crate::test_bytes::pseudo;

    struct Remote {
        dir: tempfile::TempDir,
        router: Router,
        source: Endpoint,
        client: Endpoint,
        source_store: ContentStore,
        namespace: Arc<Mutex<Namespace>>,
        fs: QuickFS,
    }

    impl Remote {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let source = Endpoint::bind(presets::Minimal).await.unwrap();
            let source_store = ContentStore::memory();
            let router = Router::builder(source.clone())
                .accept(iroh_blobs::ALPN, source_store.blobs_protocol(None))
                .spawn();
            let client = Endpoint::bind(presets::Minimal).await.unwrap();
            let hydrator = Arc::new(Hydrator::new(
                client.clone(),
                Arc::new(ContentStore::memory()),
            ));
            hydrator.add_peer(source.addr());
            let namespace = Arc::new(Mutex::new(Namespace::new("stage-client").unwrap()));
            let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
            let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics)
                .with_write_stage_dir(dir.path().join("stage"));
            Self {
                dir,
                router,
                source,
                client,
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

        async fn close(self) {
            drop(self.fs);
            self.router.shutdown().await.unwrap();
            self.source.close().await;
            self.client.close().await;
            drop(self.dir);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn append_to_remote_file_fetches_only_the_tail() {
        const LEN: usize = 64 * 1024 * 1024;
        let remote = Remote::new().await;
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
        remote.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncate_to_zero_does_not_hydrate() {
        let remote = Remote::new().await;
        let (unopened, _) = remote.file("big.iso", &pseudo(3 * 1024 * 1024, 1)).await;
        let (rewritten, _) = remote.file("notes.txt", &pseudo(300_000, 2)).await;
        let (open, _) = remote.file("open.bin", &pseudo(2 * 1024 * 1024, 3)).await;

        assert_eq!(remote.fs.truncate_inode(unopened, 0), Ok(()));
        assert_eq!(remote.fs.truncate_inode(rewritten, 0), Ok(()));
        assert_eq!(remote.fs.write_buffer(rewritten, 0, b"fresh"), Ok(5));
        assert_eq!(remote.fs.write_buffer(open, 7, b"x"), Ok(1));
        assert_eq!(remote.fs.truncate_inode(open, 0), Ok(()));
        assert_eq!(remote.fs.presented_attr(open).unwrap().0.size, 0);
        assert_eq!(remote.fs.flush_inode(rewritten), Ok(()));
        assert_eq!(remote.fs.flush_inode(open), Ok(()));

        assert_eq!(remote.fetched(), 0, "no truncation read what it dropped");
        assert_eq!(remote.fs.ns().hash_for(unopened), None);
        assert_eq!(remote.fs.ns().getattr(unopened).unwrap().size, 0);
        assert_eq!(remote.fs.ns().hash_for(open), None);
        assert_eq!(remote.committed(rewritten).await, b"fresh");
        remote.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn truncating_an_unopened_remote_file_reads_only_the_kept_prefix() {
        let remote = Remote::new().await;
        let original = pseudo(6 * 1024 * 1024, 4);
        let (ino, _) = remote.file("log.bin", &original).await;
        assert_eq!(remote.fs.truncate_inode(ino, 1000), Ok(()));
        let fetched = remote.fetched();
        assert!(
            fetched <= u64::from(crate::content::MAX_CHUNK_LEN),
            "keeping 1000 bytes fetched {fetched}"
        );
        assert_eq!(remote.committed(ino).await, &original[..1000]);
        remote.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unreachable_base_fails_the_commit_and_never_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
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
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics)
            .with_write_stage_dir(dir.path().join("stage"));

        assert_eq!(fs.write_buffer(ino, 200_000, b"tail"), Ok(4));
        assert_eq!(fs.flush_inode(ino), Err(EIO));
        assert_eq!(fs.ns().hash_for(ino), Some(elsewhere));
        assert_eq!(fs.ns().getattr(ino).unwrap().size, 200_000);
        assert!(fs.read_full_blocking(ino).is_err());
        assert_eq!(fs.staged_len(ino), Some(200_004), "the written bytes stay");
        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn large_flush_does_not_block_other_inode_reads() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("contention").unwrap()));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics)
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
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_document_reads_back_the_bytes_written_until_the_document_is_edited() {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("documents").unwrap()));
        let doc = namespace
            .lock()
            .unwrap()
            .create_document(ROOT_INO, "question.md", "md", 1)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics);
        let fs = tokio::task::spawn_blocking(move || {
            let write = |bytes: &[u8]| {
                assert_eq!(
                    fs.write_buffer(doc, 0, bytes),
                    Ok(u32::try_from(bytes.len()).unwrap())
                );
                assert_eq!(fs.flush_inode(doc), Ok(()));
                fs.settle_last_close(doc, true);
            };
            let question = b"Question X\nfirst line of thought\n";
            write(question);
            assert_eq!(
                &*fs.document_export(doc).unwrap(),
                question,
                "the file is the bytes written"
            );
            assert_eq!(
                fs.presented_attr(doc).unwrap().0.size,
                question.len() as u64
            );
            fs
        })
        .await
        .unwrap();
        {
            let mut ns = namespace.lock().unwrap();
            let block = ns.document_insert_block(doc, None, "paragraph").unwrap();
            ns.document_splice(doc, &block, 0, 0, "added in a rich view");
        }
        let edited = tokio::task::spawn_blocking(move || fs.document_export(doc).unwrap())
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&edited).contains("added in a rich view"));
        assert_ne!(&*edited, b"Question X\nfirst line of thought\n");
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_mode_given_to_a_file_or_directory_is_kept_without_setuid() {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("modes").unwrap()));
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics);
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
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn document_read_does_not_clone_per_chunk() {
        const CHUNK: u32 = 128 * 1024;
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(ContentStore::memory()),
        ));
        let namespace = Arc::new(Mutex::new(Namespace::new("documents").unwrap()));
        let doc = namespace
            .lock()
            .unwrap()
            .create_document(ROOT_INO, "big.md", "md", 1)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(Arc::clone(&hydrator)));
        let fs = QuickFS::new(Arc::clone(&namespace), hydrator, heuristics);
        assert!(fs.is_document(doc));

        let paragraph = "Collaboration lives below applications. ".repeat(50);
        let mut text = String::new();
        while text.len() < 4 * 1024 * 1024 + 64 * 1024 {
            text.push_str(&paragraph);
            text.push_str("\n\n");
        }
        assert_eq!(
            fs.write_buffer(doc, 0, text.as_bytes()),
            Ok(u32::try_from(text.len()).unwrap())
        );
        assert_eq!(fs.flush_inode(doc), Ok(()));
        fs.settle_last_close(doc, true);
        assert!(fs.stage(doc).is_none());

        let size = fs.presented_attr(doc).unwrap().0.size;
        assert!(size >= 4 * 1024 * 1024, "export is {size} bytes");
        let exported = fs.document_export(doc).unwrap();
        namespace
            .lock()
            .unwrap()
            .create_file(ROOT_INO, "unrelated.txt", None, 0, 2)
            .unwrap();
        assert!(
            Arc::ptr_eq(&exported, &fs.document_export(doc).unwrap()),
            "a change elsewhere in the namespace re-exported the document"
        );

        let mut read = Vec::with_capacity(exported.len());
        let allocated = allocation_count::allocated_by(|| {
            let mut offset = 0_u64;
            while offset < size {
                match fs.plan_read(doc, 0, offset, CHUNK) {
                    ReadPlan::Buffered(BufferedRead::Shared(bytes, range)) => {
                        read.extend_from_slice(&bytes[range]);
                    }
                    _ => panic!("a document read is served from its export"),
                }
                offset += u64::from(CHUNK);
            }
        });
        assert_eq!(
            blake3::hash(&read),
            blake3::hash(&exported),
            "the chunks reassemble the export"
        );
        assert!(
            allocated <= 2 * size,
            "reading a {size}-byte document in {CHUNK}-byte chunks allocated {allocated} bytes"
        );

        assert_eq!(fs.write_buffer(doc, 0, b"# Title\n\n"), Ok(9));
        assert_eq!(fs.flush_inode(doc), Ok(()));
        fs.settle_last_close(doc, true);
        assert!(!Arc::ptr_eq(&exported, &fs.document_export(doc).unwrap()));
        drop(fs);
        endpoint.close().await;
    }
}

#[cfg(test)]
mod read_lock_tests {
    use super::*;
    use crate::acl::WorkspaceAcl;
    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};

    const ME: [u8; 32] = [0xB7; 32];
    const BYTES: &[u8] = b"local bytes served without a peer";

    struct Fixture {
        endpoint: Endpoint,
        namespace: Arc<Mutex<Namespace>>,
        fs: QuickFS,
        shared: u64,
        file: u64,
    }

    async fn fixture() -> Fixture {
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hash = store.write_content(BYTES).await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let (shared, file) = {
            let mut ns = lock_namespace(&namespace);
            let shared = ns.create_dir(ROOT_INO, "shared", 1).unwrap();
            let file = ns
                .create_file(shared, "a.txt", Some(&hash), BYTES.len() as u64, 1)
                .unwrap();
            drop(ns);
            (shared, file)
        };
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics);
        Fixture {
            endpoint,
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

    fn locks(fs: &QuickFS) -> usize {
        fs.namespace_locks.load(Ordering::Relaxed)
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
            ReadPlan::Remote(_) => panic!("locally stored content was sent to a peer"),
        }
    }

    fn lock_namespace(namespace: &Arc<Mutex<Namespace>>) -> std::sync::MutexGuard<'_, Namespace> {
        namespace.lock_recover()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_takes_namespace_lock_once() {
        for acl in [None, Some(shared_read_policy())] {
            let Fixture {
                endpoint, fs, file, ..
            } = fixture().await;
            let fs = match acl {
                Some(acl) => fs.with_acl(acl, ME),
                None => fs,
            };
            let before = locks(&fs);
            assert_eq!(read_all(&fs, file, 0), data());
            assert_eq!(
                locks(&fs) - before,
                1,
                "a read of local content takes the namespace lock exactly once"
            );
            drop(fs);
            endpoint.close().await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn getattr_of_a_file_takes_namespace_lock_once() {
        let Fixture {
            endpoint, fs, file, ..
        } = fixture().await;
        let fs = fs.with_acl(shared_read_policy(), ME);
        let before = locks(&fs);
        let (attr, is_doc) = fs.attr_for_request(file, None).unwrap();
        assert_eq!(locks(&fs) - before, 1);
        assert_eq!(attr.size, BYTES.len() as u64);
        assert_eq!(attr.kind, FileType::RegularFile);
        assert!(!is_doc);
        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_after_acl_revoke_is_enoent() {
        let Fixture {
            endpoint, fs, file, ..
        } = fixture().await;
        let handle = AclHandle::new(shared_read_policy());
        let fs = fs.with_acl(handle.clone(), ME);
        assert_eq!(read_all(&fs, file, 0), data());

        handle.replace(WorkspaceAcl::new());
        assert_eq!(read_all(&fs, file, 0), Reply::Errno(ENOENT));
        assert_eq!(fs.attr_for_request(file, None).err(), Some(ENOENT));
        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_after_rename_out_of_view_is_enoent() {
        let Fixture {
            endpoint,
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
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unlinked_file_reads_only_through_its_open_handle() {
        let Fixture {
            endpoint,
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
            },
        );
        assert!(lock_namespace(&namespace).remove(file));

        assert_eq!(
            read_all(&fs, file, fh),
            data(),
            "the handle opened before the unlink keeps reading"
        );
        assert_eq!(
            fs.attr_for_request(file, Some(fh))
                .map(|(attr, _)| attr.size),
            Ok(BYTES.len() as u64)
        );
        assert_eq!(
            read_all(&fs, file, 0),
            Reply::Errno(ENOENT),
            "no handle, no read"
        );
        assert_eq!(fs.attr_for_request(file, None).err(), Some(ENOENT));
        drop(fs);
        endpoint.close().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_of_a_directory_is_eisdir() {
        let Fixture {
            endpoint,
            fs,
            shared,
            ..
        } = fixture().await;
        let fs = fs.with_acl(shared_read_policy(), ME);
        assert_eq!(read_all(&fs, shared, 0), Reply::Errno(EISDIR));
        drop(fs);
        endpoint.close().await;
    }
}
