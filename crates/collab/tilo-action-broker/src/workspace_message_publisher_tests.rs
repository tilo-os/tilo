// Actual signed native owner and stable-stream RPC retirement, not a fake lease.
struct MessagePublisherOwner {
    owner: tilo_joined_room::NativeWorkspaceOwner,
    key: ed25519_dalek::SigningKey,
    certificate: tilo_user_identity::DeviceCert,
    registrations: Mutex<Vec<tilo_joined_room::AgentMessagePublisherRegistration>>,
    refuse_next: std::sync::atomic::AtomicBool,
    _directory: tempfile::TempDir,
}

impl MessagePublisherOwner {
    fn enrolled() -> Arc<Self> {
        let directory = tempfile::tempdir().unwrap();
        let owner = tilo_joined_room::open_file(&directory.path().join("owner.json")).unwrap();
        let root = ed25519_dalek::SigningKey::from_bytes(&[83; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[84; 32]);
        let certificate = tilo_joined_room::testing::certificate(&root, &key, "original worker");
        tilo_joined_room::testing::enroll_interactive_governor(&owner, &key, &certificate).unwrap();
        Arc::new(Self {
            owner,
            key,
            certificate,
            registrations: Mutex::new(Vec::new()),
            refuse_next: std::sync::atomic::AtomicBool::new(false),
            _directory: directory,
        })
    }

    fn assert_retired(&self, index: usize) {
        let registrations = self.registrations.lock().unwrap();
        assert!(
            self.owner
                .register_agent_message_publisher(
                    registrations.get(index).unwrap(),
                    std::time::SystemTime::now()
                )
                .is_err(),
            "actual RPC lifetime must retire owner-issued lease {index}"
        );
    }

    async fn wait_retired(&self, index: usize) {
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let registration = self
                .registrations
                .lock()
                .unwrap()
                .get(index)
                .unwrap()
                .clone();
            if self
                .owner
                .register_agent_message_publisher(&registration, std::time::SystemTime::now())
                .is_err()
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "stream release did not reach owner custody"
            );
            tokio::task::yield_now().await;
        }
    }
}

impl crate::services::WorkspaceMessages for MessagePublisherOwner {
    fn requires_room(&self, room: &str) -> bool {
        room == tilo_joined_room::testing::ROOM
    }

    fn install_publisher(
        &self,
        request: &crate::services::AgentMessagePublisherInstallation,
    ) -> Result<tilo_joined_room::AgentMessagePublisherLease, String> {
        use ed25519_dalek::Signer as _;
        if self
            .refuse_next
            .swap(false, std::sync::atomic::Ordering::AcqRel)
        {
            return Err("injected native owner installation refusal".into());
        }
        let mut registration = tilo_joined_room::AgentMessagePublisherRegistration {
            data: request.data().clone(),
            attestor: self.certificate.clone(),
            signature: Vec::new(),
        };
        registration.signature = self
            .key
            .sign(&registration.signing_bytes()?)
            .to_bytes()
            .to_vec();
        let lease = self
            .owner
            .register_agent_message_publisher(&registration, std::time::SystemTime::now())?;
        self.registrations.lock().unwrap().push(registration);
        Ok(lease)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn native_message_publisher_actual_stream_and_directory_order_and_retirement() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for directory_first in [false, true] {
                let owner = MessagePublisherOwner::enrolled();
                let room = signed_worker::OriginalRoom::with_message_service(owner.clone());
                let (worker, _) = room.join_worker(98).await;
                let directory = signed_worker::directory_state("directory-r1");
                if directory_first {
                    worker
                        .handle
                        .publish_action_directory(
                            &room.route.app_id,
                            signed_worker::directory_server(&directory),
                        )
                        .await
                        .unwrap();
                    assert!(owner.registrations.lock().unwrap().is_empty());
                }
                let original = worker.handle.subscribe_stable_invocations().await.unwrap();
                if !directory_first {
                    assert!(owner.registrations.lock().unwrap().is_empty());
                    worker
                        .handle
                        .publish_action_directory(
                            &room.route.app_id,
                            signed_worker::directory_server(&directory),
                        )
                        .await
                        .unwrap();
                }
                assert_eq!(owner.registrations.lock().unwrap().len(), 1);
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        signed_worker::directory_server(&directory),
                    )
                    .await
                    .unwrap();
                owner.assert_retired(0);
                assert_eq!(owner.registrations.lock().unwrap().len(), 2);
                let replacement = worker.handle.subscribe_stable_invocations().await.unwrap();
                owner.assert_retired(1);
                assert_eq!(owner.registrations.lock().unwrap().len(), 3);
                drop(original);
                assert!(
                    owner
                        .owner
                        .register_agent_message_publisher(
                            owner.registrations.lock().unwrap().get(2).unwrap(),
                            std::time::SystemTime::now()
                        )
                        .is_ok(),
                    "old stream disposal must not retire replacement custody"
                );
                drop(replacement);
                owner.wait_retired(2).await;
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_message_publisher_closed_before_directory_cannot_install_late_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let owner = MessagePublisherOwner::enrolled();
            let room = signed_worker::OriginalRoom::with_message_service(owner.clone());
            let (worker, _) = room.join_worker(99).await;
            let stream = worker.handle.subscribe_stable_invocations().await.unwrap();
            drop(stream);
            tokio::task::yield_now().await;
            let directory = signed_worker::directory_state("directory-r1");
            assert!(
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        signed_worker::directory_server(&directory)
                    )
                    .await
                    .is_err()
            );
            owner.assert_retired(0);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_message_publisher_failed_installation_retires_provisional_stream() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let owner = MessagePublisherOwner::enrolled();
            let room = signed_worker::OriginalRoom::with_message_service(owner.clone());
            let (worker, _) = room.join_worker(101).await;
            let directory = signed_worker::directory_state("directory-r1");
            worker
                .handle
                .publish_action_directory(
                    &room.route.app_id,
                    signed_worker::directory_server(&directory),
                )
                .await
                .unwrap();
            owner
                .refuse_next
                .store(true, std::sync::atomic::Ordering::Release);
            assert!(worker.handle.subscribe_stable_invocations().await.is_err());
            assert!(owner.registrations.lock().unwrap().is_empty());
            assert!(
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        signed_worker::directory_server(&directory)
                    )
                    .await
                    .is_err(),
                "directory retry cannot resurrect a stream never returned to the app"
            );
            owner.assert_retired(0);
            let genuine = worker.handle.subscribe_stable_invocations().await.unwrap();
            assert_eq!(owner.registrations.lock().unwrap().len(), 2);
            assert!(
                owner
                    .owner
                    .register_agent_message_publisher(
                        owner.registrations.lock().unwrap().get(1).unwrap(),
                        std::time::SystemTime::now()
                    )
                    .is_ok(),
                "a genuinely returned replacement stream installs fresh custody"
            );
            drop(genuine);
            owner.wait_retired(1).await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn native_message_publisher_nonstable_disconnect_and_shutdown_retire_actual_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for retirement in ["nonstable", "disconnect", "shutdown"] {
                let owner = MessagePublisherOwner::enrolled();
                let room = signed_worker::OriginalRoom::with_message_service(owner.clone());
                let (worker, _) = room.join_worker(100).await;
                let legacy = worker.handle.subscribe_invocations().await.unwrap();
                let directory = signed_worker::directory_state("directory-r1");
                worker
                    .handle
                    .publish_action_directory(
                        &room.route.app_id,
                        signed_worker::directory_server(&directory),
                    )
                    .await
                    .unwrap();
                assert!(
                    owner.registrations.lock().unwrap().is_empty(),
                    "legacy streams never receive addressed Message custody"
                );
                let stable = worker.handle.subscribe_stable_invocations().await.unwrap();
                assert_eq!(owner.registrations.lock().unwrap().len(), 1);
                let replacement = match retirement {
                    "nonstable" => Some(worker.handle.subscribe_invocations().await.unwrap()),
                    "disconnect" => {
                        room.state.remove_connection(100);
                        None
                    }
                    "shutdown" => {
                        room.state.stop();
                        None
                    }
                    _ => unreachable!(),
                };
                owner.assert_retired(0);
                assert_eq!(owner.registrations.lock().unwrap().len(), 1);
                drop(replacement);
                drop(stable);
                drop(legacy);
            }
        })
        .await;
}
