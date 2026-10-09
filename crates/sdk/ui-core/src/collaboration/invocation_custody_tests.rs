// Exercises the actual SDK command queue and Capnp channel with a controlled
// host committer. Concrete native bounded-body authorization is qualified elsewhere.
struct CustodyCommitter {
    mode: u8,
    unqualified: Arc<std::sync::atomic::AtomicUsize>,
    received: std::sync::Mutex<Option<crate::invocation_scope::DocumentAuthorization>>,
    entered: mpsc::Sender<()>,
    release: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
impl HostDocumentCommitter for CustodyCommitter {
    fn commit(&self, _: &RouteSpec, _: DocumentMutation) -> HostDocumentCommit {
        self.unqualified
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err("unqualified fallback must never run".into()) })
    }
    fn commit_scoped(
        &self,
        route: &RouteSpec,
        mutation: DocumentMutation,
        authorization: crate::invocation_scope::DocumentAuthorization,
    ) -> HostDocumentCommit {
        *self.received.lock().unwrap() = Some(authorization.clone());
        self.entered.send(()).unwrap();
        let release = self.release.lock().unwrap().take().unwrap();
        let route = route.clone();
        let mode = self.mode;
        Box::pin(async move {
            release.await.map_err(|error| error.to_string())?;
            authorization.check(&route, &mutation)?;
            let mut contributors = 0;
            authorization.validate_owner_contributors(|request, exact_route, _token| {
                if request.actor != "original-queue-caller" || *exact_route != route {
                    return Err("original association changed".into());
                }
                contributors += 1;
                Ok(())
            })?;
            if contributors != 1 {
                return Err("original contributor omitted".into());
            }
            if mode == 3 {
                return Err("uncertain owner persistence result".into());
            }
            let mut operation = SignedOp::new_unsigned(
                mutation.encode().map_err(|error| error.to_string())?,
                collab_types::document_artifact_id(
                    &route.app_id,
                    &route.cluster_id.to_string(),
                    route.record.as_deref().unwrap_or(""),
                ),
                collab_types::Ed25519PublicKey::from_bytes([9; 32]),
                collab_types::document_attestation_id(
                    &route.app_id,
                    &route.cluster_id.to_string(),
                    route.record.as_deref().unwrap_or(""),
                ),
                collab_types::OpKind::Change,
                crate::time::wall(),
                1,
            );
            if mode == 2 {
                operation.payload = vec![0, 1, 2];
            }
            Ok(operation)
        })
    }
}

#[test]
fn causal_scope_sdk_queue_retains_original_custody_through_host_await_and_terminal_barrier() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut failures = vec![];
    for mode in 0..4 {
        let (route, mutation, authorization, time, fixture) =
            crate::invocation_scope::tests::queued_document_fixture();
        let captures = Arc::new(std::sync::Mutex::new(Captures::default()));
        let broker = FakeBroker {
            captures: captures.clone(),
            op_backlog: vec![],
            presence_backlog: vec![],
            context_backlog: vec![],
            scrollback_response: None,
        };
        let (fd, _, thread) = stand_up_fake_broker(broker);
        let (entered, observed) = mpsc::channel();
        let (release, waiting) = tokio::sync::oneshot::channel();
        let unqualified = Arc::new(AtomicUsize::new(0));
        let host = Arc::new(CustodyCommitter {
            mode,
            unqualified: unqualified.clone(),
            received: Default::default(),
            entered,
            release: std::sync::Mutex::new(Some(waiting)),
        });
        let mut manager = CollaborationManager::for_route_from(
            SdkConnectSource::Fd(fd),
            route.app_id.clone(),
            route.cluster_id,
            route.record.clone(),
            None,
            Some(SdkJoinIdentity {
                presentation: vec![],
                participant_kind: "person",
                signer: SdkDocumentSigner::Host(host.clone()),
            }),
            "person",
        )
        .unwrap();
        manager.require_document_authorization();
        assert!(
            !manager.send(CollabRequest::document_incremental(vec![1])),
            "required manager rejects missing custody before queue admission"
        );
        if mode == 0 {
            let state = broker_state(&manager);
            let ready = state.document_reads.close_for_test(&route.channel_id());
            assert_eq!(
                manager
                    .try_send_to_route(route.clone(), CollabRequest::document_incremental(vec![1])),
                Err(crate::collaboration::CollabSendError::Unavailable),
                "pending readiness cannot bypass required original custody"
            );
            assert!(
                manager
                    .try_send_to_route(
                        route.clone(),
                        CollabRequest::Document {
                            mutation,
                            authorization: Some(authorization)
                        }
                    )
                    .is_ok(),
                "a known pending committed route retains original authorized custody"
            );
            let pending = manager.begin_document_publication_barrier().unwrap();
            std::thread::sleep(Duration::from_millis(60));
            assert!(
                host.received.lock().unwrap().is_none(),
                "owner commit cannot precede route readiness"
            );
            assert_eq!(observed.try_recv(), Err(mpsc::TryRecvError::Empty));
            assert_eq!(pending.try_complete(), Ok(None));
            assert!(captures.lock().unwrap().signed_ops.is_empty());
            ready.store(true, Ordering::Release);
        } else {
            assert!(manager.send(CollabRequest::Document {
                mutation,
                authorization: Some(authorization)
            }));
        }
        tilo_test_env::recv("the gated invocation RPC", &observed);
        assert!(host.received.lock().unwrap().is_some());
        assert!(manager.send(CollabRequest::UpdateInvocation {
            id: 91,
            update: tilo_actions::InvocationUpdate::Succeeded {
                invocation: tilo_actions::InvocationId::new(),
                output: Default::default()
            }
        }));
        let barrier = manager.begin_document_publication_barrier().unwrap();
        assert_eq!(
            captures.lock().unwrap().terminal_updates,
            0,
            "terminal must wait behind actual commit"
        );
        if mode == 1 {
            time.store(100, Ordering::Release);
        }
        release.send(()).unwrap();
        let result = barrier.wait(Some(tilo_test_env::hang_deadline()));
        if result.is_err() != (mode != 0) {
            failures.push(format!("mode {mode}: wrong barrier outcome"));
        }
        assert_eq!(unqualified.load(Ordering::SeqCst), 0);
        if captures.lock().unwrap().terminal_updates != usize::from(mode == 0) {
            failures.push(format!("mode {mode}: wrong terminal outcome"));
        }
        time.store(100, Ordering::Release);
        let next_allowed = crate::invocation_scope::tests::queued_next_edit(&fixture, &time);
        if next_allowed != (mode == 0) {
            failures.push(format!(
                "mode {mode}: exact committed guards were not pruned, or uncertain guards were lost"
            ));
        }
        drop(manager);
        thread.join().unwrap();
    }
    assert!(failures.is_empty(), "{failures:?}");
}
