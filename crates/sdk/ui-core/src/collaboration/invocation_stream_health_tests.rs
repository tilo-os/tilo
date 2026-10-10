// Real SDK subscription/read RPCs and the production forwarding task, without
// a second transport or a fabricated invocation settlement.
mod invocation_stream_health_tests {
    use super::*;
    use runtime_collab::collab_capnp::invocation_stream;

    #[derive(Default)]
    struct Probe {
        subscriptions: std::cell::Cell<usize>,
        reads: RefCell<Vec<bool>>,
        serves_native_source: std::cell::Cell<bool>,
    }

    struct Service(Rc<Probe>);
    impl collab_service::Server for Service {
        fn join_channel(
            self: Rc<Self>,
            _params: collab_service::JoinChannelParams,
            mut results: collab_service::JoinChannelResults,
        ) -> Promise<(), capnp::Error> {
            results
                .get()
                .set_handle(capnp_rpc::new_client(Channel(self.0.clone())));
            Promise::ok(())
        }
    }

    struct Channel(Rc<Probe>);
    impl channel_handle::Server for Channel {
        fn subscribe_invocations(
            self: Rc<Self>,
            params: channel_handle::SubscribeInvocationsParams,
            mut results: channel_handle::SubscribeInvocationsResults,
        ) -> Promise<(), capnp::Error> {
            assert!(params.get().unwrap().get_stable_invocation_ids());
            self.0.subscriptions.set(self.0.subscriptions.get() + 1);
            results
                .get()
                .set_stream(capnp_rpc::new_client(Stream(self.0.clone())));
            Promise::ok(())
        }
    }

    struct Stream(Rc<Probe>);
    impl invocation_stream::Server for Stream {
        fn read(
            self: Rc<Self>,
            params: invocation_stream::ReadParams,
            mut results: invocation_stream::ReadResults,
        ) -> Promise<(), capnp::Error> {
            let native = params.get().unwrap().get_native_document_source();
            let first = self.0.reads.borrow().is_empty();
            self.0.reads.borrow_mut().push(native);
            if native && !self.0.serves_native_source.get() {
                return Promise::err(capnp::Error::failed("source admission refused".into()));
            }
            if !first {
                return Promise::err(capnp::Error::disconnected("stream closed".into()));
            }
            let mut result = results.get();
            result.set_id(41);
            result.set_invocation_uuid(Uuid::from_u128(42).as_bytes());
            result.set_resource("opaque-record/control");
            result.set_action("open");
            result.set_actor("person");
            if native {
                result.set_source_token(&[8; 32]);
                result.set_source_spec_json(native_source_spec_json());
            }
            Promise::ok(())
        }
    }

    fn native_source_spec_json() -> String {
        serde_json::to_string(&crate::invocation_scope::PublishedInvocationSpec {
            kind: tilo_actions::OperationKind::Document,
            contract: tilo_actions::ActionContract::default(),
            params: Vec::new(),
            input: tilo_actions::InputSpec::None,
            directory_revision: Some("current-native-source".into()),
            fresh_binding: None,
        })
        .unwrap()
    }

    fn run_reader(native: bool) -> (BridgeState, Rc<Probe>, Vec<CollabEvent>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let probe = Rc::new(Probe::default());
            let client = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(Service(
                probe.clone(),
            )));
            let channel = client.join_channel("fixture", &[]).await.unwrap();
            let route = RouteSpec {
                app_id: "so.tilo.notes".into(),
                cluster_id: Uuid::nil(),
                record: Some("record".into()),
            };
            let state = BridgeState::new();
            let continuity = Arc::new(PeerStreamContinuity::default());
            let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
            spawn_invocation_stream(
                &channel,
                route,
                native,
                &InboundBridge::new(tx, state.clone()),
                &continuity,
            )
            .await;
            for _ in 0..64 {
                tokio::task::yield_now().await;
            }
            let mut events = Vec::new();
            while let Ok(event) = rx.try_recv() {
                events.push(event.event);
            }
            (state, probe, events)
        })
    }

    fn assert_closed(state: &BridgeState) {
        assert!(
            !state.is_healthy(),
            "an established reader cannot disappear silently"
        );
        assert!(
            matches!(state.publication_failure.lock().unwrap().as_ref(), Some(DocumentPublicationError::Failed(reason)) if reason == "application invocation stream read")
        );
    }

    #[test]
    fn native_read_refusal_withdraws_bridge_without_terminal_or_retry() {
        let (state, probe, events) = run_reader(true);
        assert_closed(&state);
        assert!(
            events.is_empty(),
            "a source refusal cannot invent an effect or settlement"
        );
        assert_eq!(probe.subscriptions.get(), 1);
        assert_eq!(*probe.reads.borrow(), vec![true]);
    }

    #[test]
    fn legacy_stream_eof_preserves_original_uuid_without_terminal_or_retry() {
        let (state, probe, events) = run_reader(false);
        assert_closed(&state);
        assert!(
            matches!(events.as_slice(), [CollabEvent::Invocation { id: 41, invocation, native_source: None, .. }] if invocation.0 == Uuid::from_u128(42))
        );
        assert_eq!(probe.subscriptions.get(), 1);
        assert_eq!(*probe.reads.borrow(), vec![false, false]);
    }

    #[test]
    fn native_reader_uses_the_replacement_route_and_committed_read_mode() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let probe = Rc::new(Probe::default());
            probe.serves_native_source.set(true);
            let client = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(Service(
                probe.clone(),
            )));
            let channel = client.join_channel("fixture", &[]).await.unwrap();
            let route = RouteSpec::record("so.tilo.chat", Uuid::new_v4(), "new-primary");
            let state = BridgeState::new();
            let continuity = Arc::new(PeerStreamContinuity::default());
            let (tx, mut rx) = tokio_mpsc::channel(SDK_EVENT_QUEUE);
            spawn_invocation_stream(
                &channel,
                route.clone(),
                true,
                &InboundBridge::new(tx, state.clone()),
                &continuity,
            )
            .await;
            let event = rx.recv().await.unwrap().event;
            let CollabEvent::Invocation {
                native_source: Some(source),
                ..
            } = event
            else {
                panic!("native read must retain its exact immutable source");
            };
            assert_eq!(source.route(), &route);
            tilo_test_env::within("the completed native stream to fail closed", async {
                while state.is_healthy() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert_closed(&state);
            assert!(
                rx.try_recv().is_err(),
                "the stream EOF cannot invent another invocation or settlement"
            );
            assert_eq!(probe.subscriptions.get(), 1);
            assert_eq!(*probe.reads.borrow(), vec![true, true]);
        });
    }
}
