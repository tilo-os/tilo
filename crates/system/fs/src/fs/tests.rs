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
                if registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(record)
                    .is_some_and(&predicate)
                {
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
        // Phase `unrelated_wakes_do_not_cancel_a_delayed_pinned_hydration`.
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
        // Phase `unpin_wake_cancels_a_delayed_pinned_hydration`.
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

            // The transfer outlasts any unpin: persisting the unpinned
            // registry is a durable write, which a loaded disk can hold for
            // longer than any fixed transfer time.
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
        // Phase `pinned_hydration_is_bounded_and_unpin_cancels_queued_retries`.
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
        // Phase `pin_policy_persists_and_a_directory_covers_future_descendants`.
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
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_dir(ROOT_INO, "offline", 1)
                .unwrap();
            fs.set_pin_policy(folder, AvailabilityPolicy::Pinned)
                .unwrap();

            // Created after the pin: no direct registry row is needed for the
            // effective projection to inherit the stable directory policy.
            let child = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(folder, "later.txt", None, 0, 2)
                .unwrap();
            let projected = crate::syncstate::AvailabilityProjectionV1::parse_xattr(
                &fs.availability_for(child).unwrap(),
            )
            .unwrap();
            assert_eq!(projected.policy, AvailabilityPolicy::Pinned);

            drop((fs, registry));
            let reopened = AvailabilityRegistry::open(registry_path).unwrap();
            let folder_id = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(folder)
                .unwrap();
            assert_eq!(
                reopened.get(folder_id).map(|record| record.policy),
                Some(AvailabilityPolicy::Pinned)
            );
        }
        // Phase `a_read_only_collaborator_can_manage_device_local_offline_policy`.
        {
            const READER: [u8; 32] = [0x51; 32];
            let dir = tempfile::tempdir().unwrap();
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let hydrator = Arc::new(Hydrator::new(endpoint, store));
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let ino = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            let record_id = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino)
                .unwrap();
            assert_eq!(
                registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(record_id)
                    .unwrap()
                    .policy,
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

        // This file did not exist when its stable parent was pinned.
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
        // Issue the same wake used after a local namespace publish. It must not
        // wait for the deliberately slow repair scan.
        fs.wake_pinned_hydration();
        // No peer is configured yet. The worker records an actionable error
        // and a durable backoff rather than spinning or claiming success.
        wait_for_availability(&registry, record_id, |record| {
            record.transfer.last_error.is_some()
                && record.transfer.retry_count > 0
                && record.transfer.retry_at_ms.is_some()
        })
        .await;
        drop(fs);
        drop(registry);

        // A process restart reopens the queued policy/error state. Once the
        // peer is reachable the inherited promise completes without another
        // user action.
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
        // A later immutable version invalidates the old residency proof and
        // is hydrated under the same durable ancestor pin.
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
        // Let the pass that observes the worker's own registry writes run.
        tokio::time::sleep(repair.saturating_mul(10)).await;

        let walks = || namespace.lock().unwrap().node_entry_read_count();
        let settled = walks();
        tokio::time::sleep(repair.saturating_mul(25)).await;
        assert_eq!(
            walks(),
            settled,
            "ticks with nothing changed must not walk the namespace"
        );

        // A change that arrives without a wake (as a remote apply does) is
        // still repaired by the next tick.
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
        // Phase `retry_transfer_runs_off_thread_and_persists_completion`.
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
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "retry.txt", Some(&hash), 17, 1)
                .unwrap();
            let record = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino)
                .unwrap();

            fs.retry_transfer(ino).unwrap();
            tilo_test_env::within("the retry completion event", async {
                loop {
                    let done = registry
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .get(record)
                        .is_some_and(|state| {
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
        // Phase `retrying_an_empty_file_completes_without_a_blob_fetch`.
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
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "empty.txt", None, 0, 1)
                .unwrap();
            let record = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino)
                .unwrap();

            fs.retry_transfer(ino).unwrap();
            assert_eq!(
                registry
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(record)
                    .unwrap()
                    .residency,
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
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "missing.txt", Some(&missing), 4, 1)
            .unwrap();
        let record = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();

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
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "empty.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let version_id = fs_protocol::FileVersionId::new();
        let registry = Arc::new(Mutex::new(
            AvailabilityRegistry::open(dir.path().join("availability.json")).unwrap(),
        ));
        let mut availability = RecordAvailability::new(record_id, RecordKind::File);
        availability.current_version = Some(version_id);
        availability.residency = Residency::Resident { bytes: 0 };
        registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(availability)
            .unwrap();
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
        // Simulate a new immutable revision arriving after the old request
        // completed but before daemon restart recovery runs.
        let newer_version = fs_protocol::FileVersionId::new();
        let mut current = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(record_id)
            .cloned()
            .unwrap();
        current.current_version = Some(newer_version);
        current.residency = Residency::Resident { bytes: 0 };
        registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .put(current)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_availability(Arc::clone(&registry))
            .with_eviction_queue(queue);

        fs.resume_pending_evictions();
        assert_eq!(
            registry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(record_id)
                .unwrap()
                .residency,
            Residency::Resident { bytes: 0 }
        );
    }
}

#[cfg(test)]
mod share_projection_tests {
    use super::*;
    use crate::namespace::ROOT_INO;
    use crate::share_authority::{ShareAuthority, ShareGrantStatus, ShareProjectionV1};
    use crate::store::ContentStore;
    use collab_types::Ed25519PublicKey;
    use fs_protocol::{FileInviteSpec, FileInviteSubject, FilePermission, mint_file_invite};
    use iroh::endpoint::{Endpoint, presets};

    #[tokio::test]
    async fn quickfs_projects_authority_state_by_stable_record_id() {
        let dir = tempfile::tempdir().unwrap();
        let authority =
            Arc::new(ShareAuthority::open(dir.path().join("share-authority.json")).unwrap());
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(ROOT_INO, "shared.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let (issuer_key, issuer) = collab_types::generate_keypair();
        let (_, recipient) = collab_types::generate_keypair();
        let token = mint_file_invite(
            FileInviteSpec {
                workspace_id: uuid::Uuid::new_v4(),
                record_id,
                subject: FileInviteSubject::Person(recipient),
                permission: FilePermission::View,
                expires_at_ms: None,
                max_redemptions: None,
            },
            &issuer_key,
        )
        .unwrap();
        authority.register(&token, &[issuer], 1).unwrap();

        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let fs =
            QuickFS::new(namespace, hydrator, heuristics).with_share_authority(authority.clone());

        let projected =
            ShareProjectionV1::parse_xattr(&fs.share_projection_for(ino, 2).expect("share xattr"))
                .unwrap();
        assert_eq!(projected.record_id, record_id);
        assert_eq!(projected.grants.len(), 1);
        assert_eq!(projected.grants[0].status, ShareGrantStatus::Active);
        assert_eq!(
            projected.grants[0].subject,
            crate::share_authority::ShareSubjectProjection::Person {
                person: Ed25519PublicKey::from_bytes(*recipient.as_bytes())
            }
        );

        authority.revoke(token.id).unwrap();
        let revoked =
            ShareProjectionV1::parse_xattr(&fs.share_projection_for(ino, 3).expect("share xattr"))
                .unwrap();
        assert_eq!(revoked.grants[0].status, ShareGrantStatus::Revoked);
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
    use uuid::Uuid;

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
        // Phase `host_generates_a_thumbnail_and_records_its_hash`.
        // The headline: the host generates a preview from a file's local
        // content, serves it as metadata, records its blob hash on the node so
        // it projects to peers, and a second request hits the cache.
        {
            let dir = tempfile::tempdir().unwrap();
            let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

            // An image file whose content is local to the host.
            let png = solid_png(600, 400, [40, 130, 220]);
            let mh = store.write_content(&png).await.unwrap().unwrap();
            let ino = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ns.create_file(ROOT_INO, "photo.png", Some(&mh), png.len() as u64, 1)
                    .unwrap()
            };

            // No thumbnail recorded yet.
            assert!(
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .thumb_hash(ino)
                    .is_none()
            );

            // The metadata path returns a sized PNG preview.
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

            // The thumbnail's blob hash is now on the node (so it gossips), and the
            // blob is a content blob in the store that peers can hydrate.
            let tbh = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .thumb_hash(ino)
                .expect("thumbnail hash recorded on the node → projects to peers");
            assert!(
                store.has(tbh).await,
                "the thumbnail blob is stored for peers"
            );

            // A second request returns the identical bytes from the cache.
            let again = fs.thumbnail_for(ino).expect("cached");
            assert_eq!(
                again, thumb,
                "second request is a cache hit, identical bytes"
            );
        }
        // Phase `signed_mount_publishes_and_installs_verified_thumbnail_metadata`.
        {
            let dir = tempfile::tempdir().unwrap();
            let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
            let store = Arc::new(ContentStore::memory());
            let image = solid_png(320, 240, [35, 90, 180]);
            let manifest = store.write_content(&image).await.unwrap().unwrap();
            let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
            let ino = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(
                    ROOT_INO,
                    "signed.png",
                    Some(&manifest),
                    image.len() as u64,
                    1,
                )
                .unwrap();
            let record_id = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino)
                .unwrap();
            let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
            let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
            let device = ed25519_dalek::SigningKey::from_bytes(&[0x72; 32]);
            let person = ed25519_dalek::SigningKey::from_bytes(&[0x61; 32]);
            let actor = fs_protocol::FileActor {
                person: collab_types::Ed25519PublicKey(person.verifying_key().to_bytes()),
                device: collab_types::Ed25519PublicKey(device.verifying_key().to_bytes()),
            };
            let repository = Arc::new(
                crate::record_repository::RecordRepository::open(dir.path().join("records"))
                    .unwrap(),
            );
            let publisher = Arc::new(
                crate::record_publisher::RecordPublisher::new(
                    repository,
                    Uuid::from_u128(0x7065),
                    actor,
                    device,
                )
                .unwrap(),
            );
            let history = Arc::new(
                crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
            );
            let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
                .with_thumbs(ThumbProvider::open(dir.path().join("thumbs")).unwrap())
                .with_history(history, actor)
                .unwrap()
                .with_record_publisher(publisher.clone())
                .unwrap();

            assert!(fs.thumbnail_for(ino).is_some());
            let verified = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .thumb_hash(ino)
                .expect("signed preview is installed in the verified projection");
            let snapshot = publisher.snapshot(record_id).unwrap().unwrap();
            assert!(snapshot.metadata.unwrap().previews.iter().any(|preview| {
                preview.status == fs_protocol::PreviewStatus::Ready
                    && preview.hash == Some(fs_protocol::ContentHash(*verified.as_bytes()))
            }));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thumbnails_survive_remounts_and_are_absent_without_content() {
        // Phase `thumbnail_survives_a_remount`.
        // A preview survives a remount: a fresh mount over the same cache
        // directory serves the same thumbnail without re-reading the file's content.
        {
            let dir = tempfile::tempdir().unwrap();
            let png = solid_png(320, 240, [200, 60, 30]);

            let thumb = {
                let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;
                let mh = store.write_content(&png).await.unwrap().unwrap();
                let ino = {
                    let mut ns = namespace
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    ns.create_file(ROOT_INO, "pic.png", Some(&mh), png.len() as u64, 1)
                        .unwrap()
                };
                fs.thumbnail_for(ino).expect("generated")
            };

            // A brand-new mount (fresh store, content NOT local) over the same
            // thumb cache still serves the preview, confirming it came from the
            // persistent cache, not a re-decode.
            let (namespace2, store2, fs2) = fixture(dir.path().join("thumbs")).await;
            // Same content hash → same cache key. Register the node without the bytes.
            let mh = store2.write_content(&png).await.unwrap().unwrap();
            // Remove the content so it is not local on this mount.
            let ino2 = {
                let mut ns = namespace2
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // Phase `no_local_content_and_badge_types_yield_no_thumbnail`.
        // A file whose content is not local yields no thumbnail (the host never
        // hydrates content just to preview it); a badge-only type never does either.
        {
            let dir = tempfile::tempdir().unwrap();
            let (namespace, _store, fs) = fixture(dir.path().join("thumbs")).await;

            // A projected image: a hash the store never ingested (not local).
            let projected = iroh_blobs::Hash::from([3u8; 32]);
            let img_ino = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ns.create_file(ROOT_INO, "remote.png", Some(&projected), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(img_ino).is_none(),
                "no preview without local content — never hydrate the file to make one"
            );

            // A non-previewable type is never a thumbnail regardless of content.
            let zip_ino = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                ns.create_file(ROOT_INO, "bundle.zip", Some(&projected), 4096, 1)
                    .unwrap()
            };
            assert!(
                fs.thumbnail_for(zip_ino).is_none(),
                "a .zip is a badge, not a preview"
            );
        }
    }

    /// The projected-preview-before-content path: a peer with only gossiped
    /// metadata (thumbnail blob hash + blob) shows a preview WITHOUT the
    /// file's content ever being local.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_gossiped_thumbnail_is_served_before_content() {
        let dir = tempfile::tempdir().unwrap();
        let (namespace, store, fs) = fixture(dir.path().join("thumbs")).await;

        // The small thumbnail a host generated, present as a content blob
        // (as it would be after hydrating the tiny blob from the host).
        let preview = thumbnail::encode_png(
            &thumbnail::image_thumb(&solid_png(256, 256, [90, 200, 120]), 256).unwrap(),
        )
        .unwrap();
        let thumb_blob = store.ingest(preview.clone()).await.unwrap();

        // The file's content is projected (not local), but the node carries the
        // gossiped thumbnail hash.
        let projected = iroh_blobs::Hash::from([5u8; 32]);
        let ino = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
            "the gossiped thumbnail is served without local content"
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
        // Phase `opening_an_image_selects_the_next_two_images_not_other_types`.
        {
            let (namespace, fs) = fixture().await;
            let (first, second_hash, third_hash) = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // Phase `prediction_never_includes_a_sibling_hidden_by_the_acl`.
        {
            const ME: [u8; 32] = [0xA1; 32];
            let (namespace, fs) = fixture().await;
            let (first, visible_hash) = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        // Phase `record_catalog_contains_nested_records_but_not_acl_hidden_siblings`.
        {
            const ME: [u8; 32] = [0xA2; 32];
            let (namespace, fs) = fixture().await;
            let (visible_record, hidden_record) = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
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

            let bytes = fs.record_index_projection().expect("root record catalog");
            let projection: crate::syncstate::RecordIndexProjectionV1 =
                serde_json::from_slice(&bytes).unwrap();
            projection.validate().unwrap();
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
        // Phase `record_catalog_v2_pages_without_a_giant_xattr_and_refuses_stale_revisions`.
        {
            let (namespace, fs) = fixture().await;
            {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for index in 0..crate::syncstate::RECORD_INDEX_PAGE_ENTRIES + 1 {
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
            let manifest: crate::syncstate::RecordIndexManifestV2 =
                serde_json::from_slice(&fs.record_index_manifest_v2().unwrap()).unwrap();
            manifest.validate().unwrap();
            assert_eq!(
                manifest.records,
                crate::syncstate::RECORD_INDEX_PAGE_ENTRIES + 1
            );
            let first: crate::syncstate::RecordIndexPageV2 =
                serde_json::from_slice(&fs.record_index_page_v2(&manifest.revision, 0).unwrap())
                    .unwrap();
            let second_offset = first.next_offset.expect("continuation page");
            let second: crate::syncstate::RecordIndexPageV2 = serde_json::from_slice(
                &fs.record_index_page_v2(&manifest.revision, second_offset)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(second.records.len(), 1);
            assert!(fs.record_index_projection().is_some_and(
                |legacy| legacy.len() <= crate::syncstate::MAX_RECORD_INDEX_XATTR_BYTES
            ));

            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "new.txt", None, 1, 1)
                .unwrap();
            assert!(
                fs.record_index_page_v2(&manifest.revision, 0).is_none(),
                "a namespace revision invalidates a page cursor rather than mixing rows"
            );
        }
        // Phase `record_catalog_v2_refuses_pages_after_a_live_acl_generation_change`.
        {
            const ME: [u8; 32] = [0xA3; 32];
            let (namespace, fs) = fixture().await;
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "visible.txt", Some(&Hash::from([3; 32])), 3, 1)
                .unwrap();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible.txt");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: crate::syncstate::RecordIndexManifestV2 =
                serde_json::from_slice(&fs.record_index_manifest_v2().unwrap()).unwrap();

            handle.replace(policy);
            let after: crate::syncstate::RecordIndexManifestV2 =
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
        // Phase `record_catalog_v3_hides_denied_corpus_and_stably_reaches_visible_rows`.
        {
            const ME: [u8; 32] = [0xA4; 32];
            let (namespace, fs) = fixture().await;
            let visible_record = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                // A deliberately large denied prefix must not change either the
                // public manifest shape or the work of the visible page.
                for index in 0..(crate::syncstate::RECORD_INDEX_PAGE_ENTRIES * 2) {
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
            let manifest: crate::syncstate::RecordIndexManifestV3 =
                serde_json::from_slice(&before).unwrap();
            manifest.validate().unwrap();
            assert_eq!(manifest.initial_cursor.len(), 36);
            let first: crate::syncstate::RecordIndexPageV3 = serde_json::from_slice(
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

            // This would have leaked through a raw count, offset, source
            // revision, or freshly-issued initial token.  It is indistinguishable
            // to the restricted authority.
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "another-hidden.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_record_index();
            assert_eq!(fs.record_index_manifest_v3().unwrap(), before);
            assert!(
                fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .is_some()
            );
        }
        // Phase `record_catalog_v3_visible_change_resets_and_acl_revoke_fails_closed`.
        {
            const ME: [u8; 32] = [0xA5; 32];
            let (namespace, fs) = fixture().await;
            let first_record = {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let ino = ns.create_file(ROOT_INO, "first.txt", None, 0, 1).unwrap();
                ns.record_id_for(ino).unwrap()
            };
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let handle = AclHandle::new(policy.clone());
            let fs = fs.with_acl(handle.clone(), ME);
            let before: crate::syncstate::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            assert!(fs.record_index_by_id_v2(first_record).is_some());

            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "second.txt", None, 0, 1)
                .unwrap();
            fs.refresh_acl_visible_record_index();
            let after: crate::syncstate::RecordIndexManifestV3 =
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
            let revoked: crate::syncstate::RecordIndexManifestV3 =
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
        // Phase `record_catalog_v3_pages_a_large_visible_view_without_source_rescans`.
        {
            const ME: [u8; 32] = [0xA6; 32];
            let (namespace, fs) = fixture().await;
            {
                let mut ns = namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                for index in 0..=crate::syncstate::RECORD_INDEX_PAGE_ENTRIES {
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
            let manifest: crate::syncstate::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            let rebuilds = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_index_rebuild_count();
            let page_reads = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_index_page_read_count();
            let first: crate::syncstate::RecordIndexPageV3 = serde_json::from_slice(
                &fs.record_index_page_v3(&manifest.revision, &manifest.initial_cursor)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(
                first.records.len(),
                crate::syncstate::RECORD_INDEX_PAGE_ENTRIES
            );
            let second: crate::syncstate::RecordIndexPageV3 = serde_json::from_slice(
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
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_index_rebuild_count(),
                rebuilds,
                "serving v3 continuations must not rebuild or walk the Automerge corpus"
            );
            assert_eq!(
                namespace
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .record_index_page_read_count(),
                page_reads,
                "manifest, page, and exact record requests use the retained snapshot, never Namespace pages"
            );
        }
        // Phase `record_catalog_v3_refreshes_coalesced_remote_generations_off_request_path`.
        {
            const ME: [u8; 32] = [0xA7; 32];
            let (namespace, fs) = fixture().await;
            let visible_dir = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_dir(ROOT_INO, "visible", 1)
                .unwrap();
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(visible_dir, "initial.txt", None, 0, 1)
                .unwrap();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/visible");
            let sync = crate::sync::SyncHandle::test_handle();
            let fs = fs.with_sync(sync.clone()).with_acl(policy, ME);
            let before: crate::syncstate::RecordIndexManifestV3 =
                serde_json::from_slice(&fs.record_index_manifest_v3().unwrap()).unwrap();
            let page_reads = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_index_page_read_count();

            // A denied remote mutation rebuilds privately but has no public
            // generation/token effect. Several notifications coalesce to the
            // latest watch value, not one traversal per received delta.
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(ROOT_INO, "hidden-remote.txt", None, 0, 1)
                .unwrap();
            for _ in 0..16 {
                sync.notify_record_index_changed();
            }
            let mut hidden = None;
            for _ in 0..64 {
                hidden = fs.record_index_manifest_v3();
                if hidden.is_some() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            assert_eq!(hidden.unwrap(), serde_json::to_vec(&before).unwrap());
            let after_hidden_reads = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_index_page_read_count();
            assert!(
                after_hidden_reads.saturating_sub(page_reads) <= 2,
                "a burst must coalesce before rebuilding the retained view"
            );

            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .create_file(visible_dir, "remote.txt", None, 0, 1)
                .unwrap();
            let mut updated = None;
            sync.notify_record_index_changed();
            for _ in 0..64 {
                updated = fs.record_index_manifest_v3();
                if updated
                    .as_ref()
                    .is_some_and(|bytes| bytes != &serde_json::to_vec(&before).unwrap())
                {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
            let updated: crate::syncstate::RecordIndexManifestV3 =
                serde_json::from_slice(&updated.unwrap()).unwrap();
            assert_ne!(updated.revision, before.revision);
        }
        // Phase `record_catalog_v3_sync_worker_does_not_keep_dropped_mount_alive`.
        {
            const ME: [u8; 32] = [0xA8; 32];
            let (_namespace, fs) = fixture().await;
            let sync = crate::sync::SyncHandle::test_handle();
            let mut policy = WorkspaceAcl::new();
            policy.grant_read(ME, "/");
            let fs = fs.with_sync(sync.clone()).with_acl(policy, ME);
            let weak = Arc::downgrade(&fs.acl_visible_record_index);
            drop(fs);
            sync.notify_record_index_changed();
            for _ in 0..8 {
                tokio::task::yield_now().await;
            }
            assert!(
                weak.upgrade().is_none(),
                "the sync subscriber must use a weak cache reference and exit after unmount"
            );
        }
    }

    #[tokio::test]
    async fn acl_index_incremental_matches_full_rebuild() {
        const ME: [u8; 32] = [0xA9; 32];
        let (namespace, fs) = fixture().await;
        let visible = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
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
        let rows = || {
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .indexed_record_count()
        };
        let initial = fs.acl_visible_record_index().unwrap();
        assert_eq!(initial.acl_evaluations, rows());

        // A notification that moved no row keeps the retained view as is.
        fs.refresh_acl_visible_record_index();
        assert!(Arc::ptr_eq(
            &initial,
            &fs.acl_visible_record_index().unwrap()
        ));

        // A new row and a moved row are the only ones judged again.
        let revealed = {
            let mut ns = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            ns.create_file(visible, "late.txt", None, 0, 2).unwrap();
            assert!(ns.rename(ROOT_INO, "hidden-000", visible, "revealed.txt", 2));
            ns.entry_at("visible/revealed.txt").unwrap().record_id
        };
        fs.refresh_acl_visible_record_index();
        let incremental = fs.acl_visible_record_index().unwrap();
        assert_eq!(incremental.acl_evaluations, 2);
        assert!(incremental.by_id.contains_key(&revealed));

        // Judging every row from scratch agrees row for row.
        *fs.acl_visible_record_index
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        fs.refresh_acl_visible_record_index();
        let full = fs.acl_visible_record_index().unwrap();
        assert_eq!(full.acl_evaluations, rows());
        assert_eq!(full.records, incremental.records);
        assert_eq!(full.hidden, incremental.hidden);
        assert_eq!(full.revision, incremental.revision);

        // A new ACL revision judges every row again.
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
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(crate::namespace::ROOT_INO, "before.txt", None, 0, 1)
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_history(history.clone(), actor)
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
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(ino)
                .map(|hash| fs_protocol::ContentHash(*hash.as_bytes()))
        );
        fs.commit_content(ino, b"durable bytes").unwrap();
        assert_eq!(
            history.snapshot(record_id).unwrap().versions.len(),
            2,
            "flush/fsync/release of unchanged bytes must not invent versions"
        );

        assert!(
            !fs.rename_named(
                crate::namespace::ROOT_INO,
                "before.txt",
                crate::namespace::ROOT_INO,
                "after.txt",
            )
            .unwrap()
            .is_empty()
        );
        let after_rename = history.snapshot(record_id).unwrap();
        assert_eq!(after_rename.versions.len(), 3);
        assert_eq!(after_rename.versions[2].provenance, FileProvenance::Rename);
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino),
            Some(record_id)
        );

        drop(fs);
        drop(history);
        let reopened =
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap();
        assert_eq!(reopened.snapshot(record_id).unwrap().versions.len(), 3);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restoring_history_rehydrates_bytes_and_appends_without_rewriting() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"first durable revision";
        let original_hash = store.write_content(original).await.unwrap().unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "history.txt",
                Some(&original_hash),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();
        let original_version = history.snapshot(record_id).unwrap().versions[0].id;

        fs.commit_content(ino, b"second revision").unwrap();
        assert_eq!(
            fs.history_version_preview(ino, original_version).unwrap(),
            original,
            "preview reads immutable bytes without restoring them"
        );
        assert_eq!(history.snapshot(record_id).unwrap().versions.len(), 2);
        fs.restore_version(ino, original_version).unwrap();

        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        let snapshot = history.snapshot(record_id).unwrap();
        assert_eq!(snapshot.versions.len(), 3);
        assert_eq!(snapshot.heads.len(), 1);
        let restored = snapshot
            .versions
            .iter()
            .find(|version| snapshot.heads.contains(&version.id))
            .unwrap();
        assert_eq!(
            restored.provenance,
            FileProvenance::Restore {
                from: original_version
            }
        );
        // Equal millisecond timestamps are ordered by version id, so array
        // positions do not identify the original revision or restored head.
        let baseline = snapshot
            .versions
            .iter()
            .find(|version| version.id == original_version)
            .unwrap();
        assert_eq!(baseline.content_hash, restored.content_hash);
        assert_eq!(
            baseline.content_hash,
            Some(fs_protocol::ContentHash(*original_hash.as_bytes()))
        );
    }

    fn history_fs(
        dir: &tempfile::TempDir,
        endpoint: &Endpoint,
        namespace: Namespace,
    ) -> (QuickFS, Arc<crate::history::FileHistoryStore>) {
        let hydrator = Arc::new(Hydrator::new(
            endpoint.clone(),
            Arc::new(crate::store::ContentStore::memory()),
        ));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let actor = FileActor {
            person: Ed25519PublicKey([1; 32]),
            device: Ed25519PublicKey([2; 32]),
        };
        let fs = QuickFS::new(Arc::new(Mutex::new(namespace)), hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();
        (fs, history)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn document_versions_are_fetchable() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let mut namespace = Namespace::new("documents").unwrap();
        let doc = namespace
            .create_document(crate::namespace::ROOT_INO, "notes.md", "md", 1)
            .unwrap();
        let record_id = namespace.record_id_for(doc).unwrap();
        let (fs, history) = history_fs(&dir, &endpoint, namespace);
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn restoring_a_qdoc_appends_a_crdt_revision_of_the_current_document() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let mut namespace = Namespace::new("documents").unwrap();
        let ino = namespace
            .create_file(crate::namespace::ROOT_INO, "notes.qdoc", None, 0, 1)
            .unwrap();
        let record_id = namespace.record_id_for(ino).unwrap();
        let (fs, history) = history_fs(&dir, &endpoint, namespace);

        let mut document = tilo_doc::Document::new();
        let block = document.insert_block(None, "paragraph");
        document.splice_text(&block, 0, 0, "first");
        let first = document.save();
        assert_eq!(fs.commit_bytes(ino, &first), Ok(()));
        let first_version = history.snapshot(record_id).unwrap().heads[0];
        document.splice_text(&block, 5, 0, " and second");
        let second = document.save();
        assert_eq!(fs.commit_bytes(ino, &second), Ok(()));

        fs.restore_version(ino, first_version).unwrap();

        let restored = fs.read_full_blocking(ino).unwrap();
        assert_ne!(
            restored, first,
            "a restore is a new revision, not old bytes"
        );
        assert_eq!(
            tilo_doc::Document::load(&restored).unwrap().materialize(),
            tilo_doc::Document::load(&first).unwrap().materialize()
        );
        let mut restored_doc = automerge::AutoCommit::load(&restored).unwrap();
        for head in automerge::AutoCommit::load(&second).unwrap().get_heads() {
            assert!(
                restored_doc.get_change_by_hash(&head).is_some(),
                "the restore continues the current document's history"
            );
        }
        let snapshot = history.snapshot(record_id).unwrap();
        let head = snapshot
            .versions
            .iter()
            .find(|version| snapshot.heads.contains(&version.id))
            .unwrap();
        assert_eq!(
            head.provenance,
            FileProvenance::Restore {
                from: first_version
            }
        );
        assert_eq!(fs.history_version_preview(ino, head.id).unwrap(), restored);
        drop(fs);
        endpoint.close().await;
    }
}

#[cfg(test)]
mod record_publisher_integration_tests {
    use super::*;
    use collab_types::Ed25519PublicKey;
    use ed25519_dalek::SigningKey;
    use fs_protocol::{FileActor, FileProvenance};
    use iroh::endpoint::{Endpoint, presets};
    use uuid::Uuid;

    fn actor_and_signer() -> (FileActor, SigningKey) {
        let person = SigningKey::from_bytes(&[0x41; 32]);
        let device = SigningKey::from_bytes(&[0x52; 32]);
        (
            FileActor {
                person: Ed25519PublicKey(person.verifying_key().to_bytes()),
                device: Ed25519PublicKey(device.verifying_key().to_bytes()),
            },
            device,
        )
    }

    fn published_snapshot(
        repository: &crate::record_repository::RecordRepository,
        publisher: &crate::record_publisher::RecordPublisher,
        record_id: fs_protocol::FileRecordId,
    ) -> crate::record_protocol::RecordProtocolSnapshotV1 {
        let key = crate::record_repository::RecordRepositoryKey {
            workspace_id: publisher.workspace_id(),
            record_id,
        };
        repository
            .snapshot(
                key,
                &publisher.actor(),
                &publisher.local_authorizer(record_id),
            )
            .unwrap()
            .into()
    }

    // End-to-end signed publication/restart scenario; keeping its ordered
    // durability assertions together makes partial publication visible.
    #[allow(
        clippy::too_many_lines,
        reason = "the scenario keeps signed publication, mutation, restart, and exact-state assertions together"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publisher_attach_mutations_and_restart_preserve_exact_record_state() {
        let dir = tempfile::tempdir().unwrap();
        let history_path = dir.path().join("history.json");
        let repository_path = dir.path().join("records");
        let workspace_id = Uuid::from_u128(0x51);
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"opaque bytes that predate record publishing";
        let original_manifest = store.write_content(original).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "before.txt",
                Some(&original_manifest),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&repository_path).unwrap());
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository.clone(),
                workspace_id,
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let key = crate::record_repository::RecordRepositoryKey {
            workspace_id,
            record_id,
        };

        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap();
        assert!(
            !repository.contains(key),
            "history attachment alone must not masquerade as signed publication"
        );
        let fs = fs.with_record_publisher(publisher.clone()).unwrap();

        let baseline = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(baseline.record_id, record_id);
        assert_eq!(baseline.binding.as_ref().unwrap().name, "before.txt");
        assert_eq!(baseline.versions.len(), 1);
        assert_eq!(baseline.versions[0].provenance, FileProvenance::Create);
        let baseline_metadata = baseline.metadata.as_ref().unwrap();
        assert_eq!(
            baseline_metadata.content_hash,
            Some(fs_protocol::ContentHash(*original_manifest.as_bytes()))
        );
        assert_eq!(baseline_metadata.byte_len, original.len() as u64);

        // The protocol-visible content hash is the manifest root itself, not
        // a hash of reconstructed file bytes or an incidental chunk hash.
        let manifest_bytes = store.local(original_manifest).await.unwrap();
        let manifest = crate::content::Manifest::decode(&manifest_bytes).unwrap();
        let mut reconstructed = Vec::new();
        for chunk in manifest.chunks().iter().copied() {
            let chunk_hash = Hash::from(chunk.hash);
            let bytes = store.local(chunk_hash).await.unwrap();
            assert_eq!(bytes.len(), chunk.len as usize);
            reconstructed.extend_from_slice(&bytes);
        }
        assert_eq!(reconstructed, original);

        let baseline_sequence = repository.highest_sequence(key, &actor).unwrap();
        let edited = b"new durable bytes";
        repository.fail_writes(true);
        assert!(
            fs.commit_content(ino, edited).is_err(),
            "namespace mutation must wait for durable signed publication"
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(ino),
            Some(original_manifest)
        );
        assert_eq!(history.snapshot(record_id).unwrap().versions.len(), 1);
        repository.fail_writes(false);

        fs.commit_content(ino, edited).unwrap();
        let after_edit = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_edit.versions.len(), 2);
        assert!(
            after_edit
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Edit)
        );
        let edited_manifest = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .hash_for(ino)
            .unwrap();
        assert_eq!(
            after_edit.metadata.as_ref().unwrap().content_hash,
            Some(fs_protocol::ContentHash(*edited_manifest.as_bytes()))
        );
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(baseline_sequence + 3),
            "an edit publishes a signed version, atomic revision, and metadata"
        );

        let before_noop = after_edit;
        let before_noop_sequence = repository.highest_sequence(key, &actor);
        fs.commit_content(ino, edited).unwrap();
        assert_eq!(
            published_snapshot(&repository, &publisher, record_id),
            before_noop,
            "an unchanged commit must add neither a version nor metadata"
        );
        assert_eq!(
            repository.highest_sequence(key, &actor),
            before_noop_sequence
        );

        assert!(
            !fs.rename_named(
                crate::namespace::ROOT_INO,
                "before.txt",
                crate::namespace::ROOT_INO,
                "after.txt",
            )
            .unwrap()
            .is_empty()
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(ino),
            Some(record_id)
        );
        let after_rename = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_rename.binding.as_ref().unwrap().name, "after.txt");
        assert_eq!(
            after_rename.binding.as_ref().unwrap().parent,
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(crate::namespace::ROOT_INO)
        );
        assert_eq!(after_rename.versions.len(), 3);
        assert!(
            after_rename
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Rename)
        );
        assert_eq!(
            after_rename.metadata.as_ref().unwrap().content_hash,
            Some(fs_protocol::ContentHash(*edited_manifest.as_bytes()))
        );
        let installed_revision = {
            let namespace = namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let node_id = namespace.id_for_ino(ino).unwrap();
            namespace.content_revision_for_id(node_id)
        };
        assert_eq!(
            installed_revision, after_rename.content_revision,
            "rename installs the exact signed revision while preserving content"
        );
        let sequence_before_restart = repository.highest_sequence(key, &actor).unwrap();
        let namespace_bytes = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .save();

        drop(fs);
        drop(publisher);
        drop(repository);
        drop(history);

        let namespace = Arc::new(Mutex::new(
            Namespace::from_bytes("restarted-host", &namespace_bytes).unwrap(),
        ));
        let restarted_ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lookup(crate::namespace::ROOT_INO, "after.txt")
            .unwrap()
            .ino
            .0;
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .record_id_for(restarted_ino),
            Some(record_id)
        );
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&repository_path).unwrap());
        let (_, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository.clone(),
                workspace_id,
                actor,
                signer,
            )
            .unwrap(),
        );
        let hydrator = Arc::new(Hydrator::new(endpoint, store));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let restarted = QuickFS::new(namespace, hydrator, heuristics)
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap();
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(sequence_before_restart),
            "restart backfill must observe the durable watermark and replay nothing"
        );
        assert_eq!(
            published_snapshot(&repository, &publisher, record_id),
            after_rename
        );

        restarted
            .commit_content(restarted_ino, b"edit after restart")
            .unwrap();
        let after_restart_edit = published_snapshot(&repository, &publisher, record_id);
        assert_eq!(after_restart_edit.versions.len(), 4);
        assert_eq!(
            repository.highest_sequence(key, &actor),
            Some(sequence_before_restart + 3),
            "new operations must continue after the recovered actor watermark"
        );

        let (created_ino, delta) = restarted
            .create_empty_file(crate::namespace::ROOT_INO, "signed-new.txt")
            .unwrap();
        assert!(!delta.is_empty());
        let created_record = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(created_ino)
            .unwrap();
        let created = publisher.snapshot(created_record).unwrap().unwrap();
        assert_eq!(created.binding.as_ref().unwrap().name, "signed-new.txt");
        assert_eq!(created.versions.len(), 1);
        assert_eq!(created.versions[0].provenance, FileProvenance::Create);
        assert_eq!(
            created.content_revision.unwrap().version_id,
            created.versions[0].id
        );
        assert_eq!(
            restarted
                .namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .hash_for(created_ino),
            None,
            "the empty signed baseline is visible only through its verified revision"
        );

        let (created_dir_ino, dir_delta) = restarted
            .create_empty_dir(crate::namespace::ROOT_INO, "signed-folder")
            .unwrap();
        assert!(!dir_delta.is_empty());
        let created_dir_record = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(created_dir_ino)
            .unwrap();
        let created_dir = publisher.snapshot(created_dir_record).unwrap().unwrap();
        assert_eq!(created_dir.binding.as_ref().unwrap().name, "signed-folder");
        assert_eq!(
            created_dir.binding.as_ref().unwrap().kind,
            fs_protocol::FileNodeKind::Directory
        );
        assert_eq!(created_dir.versions.len(), 1);
        assert!(created_dir.metadata.is_none());
        restarted
            .rename_named(
                crate::namespace::ROOT_INO,
                "signed-folder",
                crate::namespace::ROOT_INO,
                "renamed-folder",
            )
            .unwrap();
        let renamed_dir = publisher.snapshot(created_dir_record).unwrap().unwrap();
        assert_eq!(renamed_dir.binding.as_ref().unwrap().name, "renamed-folder");
        assert_eq!(renamed_dir.versions.len(), 2);
        assert!(
            renamed_dir
                .versions
                .iter()
                .any(|version| version.provenance == FileProvenance::Rename)
        );
        assert!(renamed_dir.metadata.is_none());

        // Until the durable transaction coordinator is wired through this
        // FUSE entry point, occupied destination must fail before either
        // participant acquires a version or repository operation.
        let source_before = publisher.snapshot(record_id).unwrap().unwrap();
        let destination_before = publisher.snapshot(created_record).unwrap().unwrap();
        let source_sequence = repository.highest_sequence(key, &actor);
        let destination_key = crate::record_repository::RecordRepositoryKey {
            workspace_id,
            record_id: created_record,
        };
        let destination_sequence = repository.highest_sequence(destination_key, &actor);
        let source_history = history.snapshot(record_id).unwrap();
        let destination_history = history.snapshot(created_record).unwrap();
        assert_eq!(
            restarted.rename_named(
                crate::namespace::ROOT_INO,
                "after.txt",
                crate::namespace::ROOT_INO,
                "signed-new.txt",
            ),
            Err(EEXIST)
        );
        let mut namespace = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "after.txt")
                .unwrap()
                .ino
                .0,
            restarted_ino
        );
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "signed-new.txt")
                .unwrap()
                .ino
                .0,
            created_ino
        );
        drop(namespace);
        assert_eq!(
            publisher.snapshot(record_id).unwrap().unwrap(),
            source_before
        );
        assert_eq!(
            publisher.snapshot(created_record).unwrap().unwrap(),
            destination_before
        );
        assert_eq!(repository.highest_sequence(key, &actor), source_sequence);
        assert_eq!(
            repository.highest_sequence(destination_key, &actor),
            destination_sequence
        );
        assert_eq!(history.snapshot(record_id).unwrap(), source_history);
        assert_eq!(
            history.snapshot(created_record).unwrap(),
            destination_history
        );

        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("record-transactions"),
            )
            .unwrap(),
        ));
        let namespace_path = dir.path().join("namespace-transaction.bin");
        let restarted = restarted
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        assert!(
            restarted
                .rename_named(
                    crate::namespace::ROOT_INO,
                    "after.txt",
                    crate::namespace::ROOT_INO,
                    "signed-new.txt",
                )
                .unwrap()
                .is_empty(),
            "the coordinator broadcasts the staged delta itself"
        );
        let mut namespace = restarted
            .namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "after.txt")
                .is_none()
        );
        assert_eq!(
            namespace
                .lookup(crate::namespace::ROOT_INO, "signed-new.txt")
                .unwrap()
                .ino
                .0,
            restarted_ino
        );
        assert!(namespace.ino_for_record_id(created_record).is_none());
        drop(namespace);
        assert!(namespace_path.exists());
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .count(),
            0
        );
        let source_after_replacement = publisher.snapshot(record_id).unwrap().unwrap();
        let victim_after_replacement = publisher.snapshot(created_record).unwrap().unwrap();
        assert_eq!(
            source_after_replacement.binding.as_ref().unwrap().name,
            "signed-new.txt"
        );
        assert!(
            victim_after_replacement
                .binding
                .as_ref()
                .unwrap()
                .tombstoned
        );
        assert_eq!(
            source_after_replacement.versions.len(),
            source_before.versions.len() + 1
        );
        assert_eq!(
            victim_after_replacement.versions.len(),
            destination_before.versions.len() + 1
        );
    }

    // Full conflict-resolution matrix across durable history and restart.
    #[allow(
        clippy::too_many_lines,
        reason = "the scenario covers the full conflict-resolution matrix across durable history and restart"
    )]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keep_current_and_use_other_append_signed_convergent_resolutions() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let original = b"original branch";
        let original_manifest = store.write_content(original).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                "conflicted.txt",
                Some(&original_manifest),
                original.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let repository = Arc::new(
            crate::record_repository::RecordRepository::open(dir.path().join("records")).unwrap(),
        );
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let namespace_path = dir.path().join("namespace.automerge");
        let fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        let original_version = history.snapshot(record_id).unwrap().versions[0].id;

        fs.commit_content(ino, b"current branch").unwrap();
        let current_version = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .content_revision_for(ino)
            .unwrap()
            .version_id;
        let first_conflict = fs_protocol::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![original_version, current_version],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(first_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&first_conflict))
            .unwrap();

        fs.resolve_conflict(
            ino,
            first_conflict.id,
            fs_protocol::ConflictStrategy::KeepCurrent,
            current_version,
        )
        .unwrap();
        assert_eq!(fs.read_full_blocking(ino).unwrap(), b"current branch");
        let after_keep = history.snapshot(record_id).unwrap();
        let keep_resolution = after_keep
            .conflicts
            .iter()
            .find(|conflict| conflict.id == first_conflict.id)
            .unwrap()
            .resolution
            .clone()
            .unwrap();
        assert_eq!(
            keep_resolution.strategy,
            fs_protocol::ConflictStrategy::KeepCurrent
        );
        let keep_result = after_keep
            .versions
            .iter()
            .find(|version| version.id == keep_resolution.result)
            .unwrap();
        assert_eq!(
            keep_result.provenance,
            fs_protocol::FileProvenance::ConflictResolution
        );
        assert!(
            first_conflict
                .versions
                .iter()
                .all(|member| keep_result.parents.contains(member))
        );

        let second_conflict = fs_protocol::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![keep_result.id, original_version],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(second_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&second_conflict))
            .unwrap();
        fs.resolve_conflict(
            ino,
            second_conflict.id,
            fs_protocol::ConflictStrategy::UseOther,
            original_version,
        )
        .unwrap();
        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        let signed_state = publisher.snapshot(record_id).unwrap().unwrap();
        let resolution = signed_state
            .conflicts
            .iter()
            .find(|conflict| conflict.id == second_conflict.id)
            .unwrap()
            .resolution
            .as_ref()
            .unwrap();
        assert_eq!(resolution.strategy, fs_protocol::ConflictStrategy::UseOther);
        assert_eq!(
            signed_state.content_revision.unwrap().version_id,
            resolution.result,
            "the signed visible revision and deterministic resolution winner must agree"
        );
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .content_revision_for(ino)
                .unwrap()
                .version_id,
            resolution.result
        );

        let source_current = resolution.result;
        let keep_both_conflict = fs_protocol::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![source_current, keep_result.id],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(keep_both_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&keep_both_conflict))
            .unwrap();
        let keep_both_transaction_id = keep_both_uuid(
            b"transaction",
            publisher.workspace_id(),
            record_id,
            keep_both_conflict.id,
            keep_result.id,
            actor,
        );
        let target_record = fs_protocol::FileRecordId(keep_both_uuid(
            b"target-record",
            publisher.workspace_id(),
            record_id,
            keep_both_conflict.id,
            keep_result.id,
            actor,
        ));
        crate::transaction_coordinator::arm_coordinator_fault(
            keep_both_transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterHistoryAppend,
        );
        assert_eq!(
            fs.resolve_conflict(
                ino,
                keep_both_conflict.id,
                fs_protocol::ConflictStrategy::KeepBoth,
                keep_result.id,
            ),
            Err(EIO)
        );
        assert!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(
                    crate::namespace::ROOT_INO,
                    keep_both_name("conflicted.txt", target_record)
                )
                .is_none(),
            "history-only crash must expose no sibling"
        );
        assert!(
            history
                .snapshot(record_id)
                .unwrap()
                .conflicts
                .iter()
                .find(|value| value.id == keep_both_conflict.id)
                .unwrap()
                .resolution
                .is_none()
        );
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        crate::transaction_coordinator::arm_coordinator_fault(
            keep_both_transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterNamespaceDeltaStaged,
        );
        let staged_recovery = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone());
        assert!(staged_recovery.is_err());
        assert!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .lookup(
                    crate::namespace::ROOT_INO,
                    keep_both_name("conflicted.txt", target_record),
                )
                .is_none(),
            "durably staged namespace bytes remain invisible before publication"
        );
        assert_eq!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(keep_both_transaction_id)
                .unwrap()
                .phase,
            crate::record_transaction::RecordTransactionPhase::HistoryCommitted
        );
        drop(publisher);
        drop(history);
        drop(journal);
        let history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        fs.resolve_conflict(
            ino,
            keep_both_conflict.id,
            fs_protocol::ConflictStrategy::KeepBoth,
            keep_result.id,
        )
        .unwrap();
        let target_name = keep_both_name("conflicted.txt", target_record);
        let target_ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .lookup(crate::namespace::ROOT_INO, &target_name)
            .expect("Keep Both sibling is atomically visible")
            .ino
            .0;
        assert_eq!(fs.read_full_blocking(ino).unwrap(), original);
        assert_eq!(
            fs.read_full_blocking(target_ino).unwrap(),
            b"current branch"
        );
        let source_after = publisher.snapshot(record_id).unwrap().unwrap();
        let target_after = publisher.snapshot(target_record).unwrap().unwrap();
        assert_eq!(target_after.binding.as_ref().unwrap().name, target_name);
        assert_eq!(target_after.versions.len(), 1);
        assert_eq!(
            source_after
                .conflicts
                .iter()
                .find(|conflict| conflict.id == keep_both_conflict.id)
                .unwrap()
                .resolution
                .as_ref()
                .unwrap()
                .strategy,
            fs_protocol::ConflictStrategy::KeepBoth
        );
        let follow_on_conflict = fs_protocol::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![
                source_after.content_revision.unwrap().version_id,
                keep_result.id,
            ],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(follow_on_conflict.clone()).unwrap();
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(&metadata, &[], std::slice::from_ref(&follow_on_conflict))
            .unwrap();
        assert!(
            publisher
                .snapshot(record_id)
                .unwrap()
                .unwrap()
                .conflicts
                .iter()
                .any(|conflict| conflict.id == follow_on_conflict.id),
            "a later signed conflict may reference the committed transaction revision"
        );
        let source_versions = history.snapshot(record_id).unwrap().versions.len();
        fs.resolve_conflict(
            ino,
            keep_both_conflict.id,
            fs_protocol::ConflictStrategy::KeepBoth,
            keep_result.id,
        )
        .unwrap();
        assert_eq!(
            history.snapshot(record_id).unwrap().versions.len(),
            source_versions
        );
        assert_eq!(history.snapshot(target_record).unwrap().versions.len(), 1);
        assert!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
        assert!(namespace_path.exists());

        // Reopening reconstructs both signed record projections from the
        // atomic namespace snapshot and treats the same request as an exact
        // replay, without another version or sibling.
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);
        drop(namespace);
        let restarted_namespace = Arc::new(Mutex::new(
            Namespace::from_bytes("host-restart", &std::fs::read(&namespace_path).unwrap())
                .unwrap(),
        ));
        let restarted_history = Arc::new(
            crate::history::FileHistoryStore::open(dir.path().join("history.json")).unwrap(),
        );
        let restarted_publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(
                    crate::record_repository::RecordRepository::open(dir.path().join("records"))
                        .unwrap(),
                ),
                Uuid::from_u128(0xc011),
                actor,
                signer,
            )
            .unwrap(),
        );
        let restarted_journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(
                dir.path().join("transactions"),
            )
            .unwrap(),
        ));
        let restarted_hydrator = Arc::new(Hydrator::new(endpoint, store));
        let restarted_heuristics = Arc::new(Heuristics::new(restarted_hydrator.clone()));
        let restarted = QuickFS::new(
            restarted_namespace.clone(),
            restarted_hydrator,
            restarted_heuristics,
        )
        .without_legacy_record_migration()
        .with_history(restarted_history.clone(), actor)
        .unwrap()
        .with_record_publisher(restarted_publisher)
        .unwrap()
        .with_replacement_transactions(restarted_journal.clone(), namespace_path)
        .unwrap();
        let restarted_source = restarted_namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .ensure_ino_for_record_id(record_id)
            .unwrap();
        restarted
            .resolve_conflict(
                restarted_source,
                keep_both_conflict.id,
                fs_protocol::ConflictStrategy::KeepBoth,
                keep_result.id,
            )
            .unwrap();
        assert_eq!(
            restarted_history
                .snapshot(record_id)
                .unwrap()
                .versions
                .len(),
            source_versions
        );
        assert_eq!(
            restarted_history
                .snapshot(target_record)
                .unwrap()
                .versions
                .len(),
            1
        );
        assert!(
            restarted_journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
    }

    // Shared deterministic merge/recovery scenario used by both supported
    // codecs; each crash boundary is intentionally asserted in sequence.
    #[allow(
        clippy::too_many_lines,
        reason = "the shared scenario asserts every crash boundary for both supported merge codecs in order"
    )]
    async fn merge_recovers_after_history_crash_and_replays_idempotently(
        name: &str,
        base_bytes: Vec<u8>,
        current_bytes: Vec<u8>,
        other_bytes: Vec<u8>,
        qdoc: bool,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::bind(presets::Minimal).await.unwrap();
        let store = Arc::new(crate::store::ContentStore::memory());
        let base_hash = store.write_content(&base_bytes).await.unwrap().unwrap();
        let namespace = Arc::new(Mutex::new(Namespace::new("host").unwrap()));
        let ino = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .create_file(
                crate::namespace::ROOT_INO,
                name,
                Some(&base_hash),
                base_bytes.len() as u64,
                1,
            )
            .unwrap();
        let record_id = namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .record_id_for(ino)
            .unwrap();
        let hydrator = Arc::new(Hydrator::new(endpoint.clone(), store.clone()));
        let heuristics = Arc::new(Heuristics::new(hydrator.clone()));
        let history_path = dir.path().join("history.json");
        let records_path = dir.path().join("records");
        let journal_path = dir.path().join("transactions");
        let namespace_path = dir.path().join("namespace.automerge");
        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let repository =
            Arc::new(crate::record_repository::RecordRepository::open(&records_path).unwrap());
        let (actor, signer) = actor_and_signer();
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                repository,
                Uuid::from_u128(0xc011),
                actor,
                signer.clone(),
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let fs = QuickFS::new(namespace.clone(), hydrator.clone(), heuristics.clone())
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher.clone())
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path.clone())
            .unwrap();
        let base_version = history.snapshot(record_id).unwrap().versions[0].id;
        fs.commit_content(ino, &current_bytes).unwrap();
        let current = history
            .snapshot(record_id)
            .unwrap()
            .versions
            .last()
            .unwrap()
            .clone();
        let other_hash = store.write_content(&other_bytes).await.unwrap().unwrap();
        let other = fs_protocol::FileVersionV1 {
            id: fs_protocol::FileVersionId::new(),
            record_id,
            parents: vec![base_version],
            actor,
            timestamp_ms: current.timestamp_ms.saturating_add(1),
            content_hash: Some(fs_protocol::ContentHash(*other_hash.as_bytes())),
            metadata_hash: fs_protocol::ContentHash([77; 32]),
            provenance: fs_protocol::FileProvenance::Edit,
        };
        history.append(other.clone()).unwrap();
        let conflict = fs_protocol::FileConflictV1 {
            id: Uuid::new_v4(),
            record_id,
            versions: vec![current.id, other.id],
            status: fs_protocol::ConflictStatus::Open,
            resolution: None,
        };
        history.record_conflict(conflict.clone()).unwrap();
        let expected = crate::conflict_merge::merge_exact(
            crate::conflict_merge::ExactMergeInput {
                version_id: current.id,
                bytes: current_bytes.clone(),
            },
            crate::conflict_merge::ExactMergeInput {
                version_id: other.id,
                bytes: other_bytes.clone(),
            },
            (!qdoc).then_some(crate::conflict_merge::ExactMergeBase {
                version_id: base_version,
                bytes: base_bytes.clone(),
            }),
        )
        .unwrap()
        .bytes;
        let metadata = publisher
            .snapshot(record_id)
            .unwrap()
            .unwrap()
            .metadata
            .unwrap();
        publisher
            .publish_state(
                &metadata,
                std::slice::from_ref(&other),
                std::slice::from_ref(&conflict),
            )
            .unwrap();
        let transaction_id = merge_uuid(
            b"transaction",
            publisher.workspace_id(),
            record_id,
            conflict.id,
            &conflict.versions,
            actor,
        );
        crate::transaction_coordinator::arm_coordinator_fault(
            transaction_id,
            crate::transaction_coordinator::CoordinatorFaultPoint::AfterHistoryAppend,
        );
        assert_eq!(
            fs.resolve_conflict(
                ino,
                conflict.id,
                fs_protocol::ConflictStrategy::Merge,
                other.id
            ),
            Err(EIO)
        );
        assert_eq!(fs.read_full_blocking(ino).unwrap(), current_bytes);
        assert!(
            history
                .snapshot(record_id)
                .unwrap()
                .conflicts
                .iter()
                .find(|value| value.id == conflict.id)
                .unwrap()
                .resolution
                .is_none()
        );
        drop(fs);
        drop(publisher);
        drop(history);
        drop(journal);

        let history = Arc::new(crate::history::FileHistoryStore::open(&history_path).unwrap());
        let publisher = Arc::new(
            crate::record_publisher::RecordPublisher::new(
                Arc::new(crate::record_repository::RecordRepository::open(&records_path).unwrap()),
                Uuid::from_u128(0xc011),
                actor,
                signer,
            )
            .unwrap(),
        );
        let journal = Arc::new(Mutex::new(
            crate::record_transaction::RecordTransactionJournal::open(&journal_path).unwrap(),
        ));
        let restarted = QuickFS::new(namespace.clone(), hydrator, heuristics)
            .without_legacy_record_migration()
            .with_history(history.clone(), actor)
            .unwrap()
            .with_record_publisher(publisher)
            .unwrap()
            .with_replacement_transactions(journal.clone(), namespace_path)
            .unwrap();
        assert_eq!(
            restarted.read_full_blocking(ino).unwrap(),
            expected.as_slice()
        );
        let resolution = history
            .snapshot(record_id)
            .unwrap()
            .conflicts
            .into_iter()
            .find(|value| value.id == conflict.id)
            .unwrap()
            .resolution
            .unwrap();
        assert_eq!(resolution.strategy, fs_protocol::ConflictStrategy::Merge);
        assert_eq!(
            namespace
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .content_revision_for(ino)
                .unwrap()
                .version_id,
            resolution.result
        );
        assert!(
            journal
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .entries()
                .next()
                .is_none()
        );
        restarted
            .resolve_conflict(
                ino,
                conflict.id,
                fs_protocol::ConflictStrategy::Merge,
                other.id,
            )
            .unwrap();
        assert_eq!(
            restarted.read_full_blocking(ino).unwrap(),
            expected.as_slice()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn text_merge_recovers_after_history_crash_and_replays_idempotently() {
        merge_recovers_after_history_crash_and_replays_idempotently(
            "merge.txt",
            b"base".to_vec(),
            b"base-tail".to_vec(),
            b"head-base".to_vec(),
            false,
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn qdoc_merge_recovers_after_history_crash_and_replays_idempotently() {
        let mut base = tilo_doc::Document::new();
        let block = base.insert_block(None, "paragraph");
        base.splice_text(&block, 0, 0, "base");
        let base_bytes = base.save();
        let mut current = base.fork();
        current.splice_text(&block, 0, 0, "L");
        let current_bytes = current.save();
        let mut other = base.fork();
        other.splice_text(&block, 4, 0, "R");
        let other_bytes = other.save();
        merge_recovers_after_history_crash_and_replays_idempotently(
            "merge.qdoc",
            base_bytes,
            current_bytes,
            other_bytes,
            true,
        )
        .await;
    }
}

/// Counts the bytes each thread allocates (cumulative, never decremented),
/// so a test can bound what a read path copies.
mod allocation_count {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    pub struct Counting;

    std::thread_local! {
        static ALLOCATED: Cell<u64> = const { Cell::new(0) };
    }

    fn add(bytes: usize) {
        // A thread being torn down has no counter left; nothing is measured there.
        let _ = ALLOCATED.try_with(|count| count.set(count.get().saturating_add(bytes as u64)));
    }

    // SAFETY: every call forwards to the system allocator with the caller's
    // arguments; the counter only records sizes.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            add(layout.size());
            // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract, which `System` shares.
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            add(layout.size());
            // SAFETY: as for `alloc`.
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: `ptr` came from this allocator, i.e. from `System`, with this layout.
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            add(new_size);
            // SAFETY: as for `dealloc`, and the caller upholds `realloc`'s size contract.
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// Bytes this thread allocated while running `run`.
    pub fn allocated_by(run: impl FnOnce()) -> u64 {
        let before = ALLOCATED.with(Cell::get);
        run();
        ALLOCATED.with(Cell::get).saturating_sub(before)
    }
}

/// FUSE writes stage per inode and read the committed content only where
/// a read or the commit needs it.
mod write_stage_tests {
    use super::*;

    use crate::namespace::ROOT_INO;
    use crate::store::ContentStore;
    use iroh::endpoint::{Endpoint, presets};
    use iroh::protocol::Router;

    use crate::test_bytes::pseudo;

    /// A peer serving committed content, and a mount on another endpoint
    /// that knows the files by namespace entry only.
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

        /// A file whose committed content only the peer holds.
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

        /// Content bytes this mount has received from peers.
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
        // Read-your-writes across the boundary: the base's tail comes from
        // the peer, the appended byte from the stage.
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

        // `: > big.iso` on a file nobody has open commits it empty.
        assert_eq!(remote.fs.truncate_inode(unopened, 0), Ok(()));
        // `open(O_TRUNC)` then a write: the truncation commits, the write
        // stages over nothing.
        assert_eq!(remote.fs.truncate_inode(rewritten, 0), Ok(()));
        assert_eq!(remote.fs.write_buffer(rewritten, 0, b"fresh"), Ok(5));
        // A truncation of an open file only shortens its stage.
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

        // The write is accepted: it does not need the base.
        assert_eq!(fs.write_buffer(ino, 200_000, b"tail"), Ok(4));
        // The commit does, and no peer has it: close reports the failure and
        // the committed content is untouched, never replaced by zeros.
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
        let (large, delta) = fs.create_empty_file(ROOT_INO, "large.bin").unwrap();
        fs.publish(delta);
        let (small, delta) = fs.create_empty_file(ROOT_INO, "small.txt").unwrap();
        fs.publish(delta);
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
                // `large` is mid-commit with its stage locked. Another
                // inode's read, write and stat must not wait for it.
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

        // About 4 MiB of markdown, written and committed the way an editor
        // saves it, then closed.
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

        // `stat` exports once; a change to another file keeps that export.
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

        // An edit of the document itself is a new export.
        assert_eq!(fs.write_buffer(doc, 0, b"# Title\n\n"), Ok(9));
        assert_eq!(fs.flush_inode(doc), Ok(()));
        fs.settle_last_close(doc, true);
        assert!(!Arc::ptr_eq(&exported, &fs.document_export(doc).unwrap()));
        drop(fs);
        endpoint.close().await;
    }
}

/// A FUSE `read` and `getattr` of a local file read the inode under one
/// namespace lock, and that single read still enforces every visibility
/// rule the separate lookups did: ACL revocation, a move out of the view,
/// unlink, and the open-handle exception for an unlinked inode.
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

    /// A mount over a namespace holding `/shared/a.txt`, whose content is
    /// already in the local store.
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

    /// What the FUSE `read` callback would reply with.
    #[derive(Debug, PartialEq, Eq)]
    enum Reply {
        Data(Vec<u8>),
        Errno(i32),
    }

    fn data() -> Reply {
        Reply::Data(BYTES.to_vec())
    }

    /// The whole file through the production read planner.
    fn read_all(fs: &QuickFS, ino: u64, fh: u64) -> Reply {
        match fs.plan_read(ino, fh, 0, 4096) {
            ReadPlan::Local(data) => Reply::Data(data),
            ReadPlan::Error(errno) => Reply::Errno(errno),
            ReadPlan::Buffered(_) => panic!("a file with no stage was answered from a buffer"),
            ReadPlan::Remote(_) => panic!("locally stored content was sent to a peer"),
        }
    }

    fn lock_namespace(namespace: &Arc<Mutex<Namespace>>) -> std::sync::MutexGuard<'_, Namespace> {
        namespace
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_takes_namespace_lock_once() {
        // Without an ACL, and with one (the path is then built and checked).
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
        fs.open_file_handles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(
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
