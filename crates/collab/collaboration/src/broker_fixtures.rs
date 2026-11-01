//! The broker's test fixtures: managers wired to a fake broker in each state
//! a route can be in, for the tests here and in crates that test against the
//! `test-support` feature.

use super::*;

impl CollaborationManager {
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_history_invocation_fixture_for_test(
        history: Vec<u8>,
    ) -> NativeHistoryInvocationFixture {
        tests::native_history_invocation_fixture(history)
    }

    /// Wire/SDK ordering fixture only; not a native authority proof.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture(app, room, history)
    }

    /// Socket-backed ordering fixture on an actual acknowledged named route.
    /// This fixture does not establish native caller authority.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_named_fixture_for_test(route: RouteSpec, history: Vec<u8>) -> Self {
        let (mut manager, _) =
            tests::native_initial_fixture(&route.app_id, route.cluster_id, history);
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager
            .await_primary_route()
            .expect("actual named route acknowledgement");
        manager
    }

    /// [`Self::native_initial_named_fixture_for_test`] whose broker admits
    /// the connection's own invocations against the surface it published;
    /// the probe lists each invocation as it arrived and whether it was
    /// offered then. It refuses every committed read of the `refused`
    /// routes, so they never open.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_named_invocation_fixture_for_test(
        route: RouteSpec,
        history: Vec<u8>,
        refused: &[RouteSpec],
    ) -> (
        Self,
        Box<dyn Fn() -> Vec<(tilo_actions::ActionAddress, bool)>>,
    ) {
        let (mut manager, invocations) = tests::native_initial_invocation_fixture(
            &route.app_id,
            route.cluster_id,
            history,
            refused.iter().map(RouteSpec::channel_id).collect(),
        );
        assert!(manager.send(CollabRequest::Hello {
            app_id: route.app_id,
            cluster_id: route.cluster_id,
            record: route.record,
            record_kind: None,
            handle: None,
        }));
        manager
            .await_primary_route()
            .expect("actual named route acknowledgement");
        (manager, invocations)
    }

    /// A connected route whose document reads are not Required, as a
    /// standalone runtime admits it.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn connected_fixture_for_test(app: &str, room: Uuid) -> Self {
        tests::connected_manager_for(app, room)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_fixture_gated_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_fixture_gated(app, room, history, Some(gate))
    }

    /// Socket-backed committed-read fixture; the probe observes real recheck requests.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_initial_parked_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
        gate: tokio::sync::oneshot::Receiver<()>,
    ) -> (Self, Box<dyn Fn() -> usize>) {
        tests::native_initial_parked_fixture(app, room, history, gate)
    }

    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_refresh_fixture_for_test(
        app: &str,
        room: Uuid,
        history: Vec<u8>,
    ) -> (Self, NativeRefreshProbe) {
        tests::native_refresh_fixture(app, room, history)
    }

    /// A native app's manager joining `route` in the background as
    /// [`Self::join_native`] does, over a fake broker that holds the join
    /// until the probe answers or refuses it, as a loaded broker does. The
    /// route's committed history is `history`, of the record kind `kind`;
    /// the room stands as `room` until the probe says otherwise. Make it in
    /// the app's wake scope, as a launch does.
    #[cfg(any(test, feature = "test-support"))]
    #[cfg(unix)]
    #[allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "a fixture for the toolkit's tests"
    )]
    pub fn native_joining_fixture_for_test(
        route: RouteSpec,
        kind: &str,
        history: Vec<u8>,
        room: runtime_collab::RoomStatus,
    ) -> (Self, JoiningProbe) {
        let (answer, gate) = tokio::sync::oneshot::channel();
        let (room_status, status) = tokio::sync::watch::channel(Ok(room));
        let broker = tests::FakeBroker {
            captures: Arc::new(std::sync::Mutex::new(tests::Captures {
                committed_history_fixture: Some(history),
                join_gates: std::collections::VecDeque::from([gate]),
                room_status: Some(status),
                // The session's answer to the app's private requests, as the
                // custodian gives it: sealing stores, release answers with
                // the secret, anything else is not the session's.
                service_sessions: Some(Arc::new(|command| match command {
                    runtime_collab::BrowserServiceSessionCommand::StoreAccountSecret {
                        account,
                        ..
                    } => Ok(
                        runtime_collab::BrowserServiceSessionReply::AccountSecretStored {
                            account: account.clone(),
                        },
                    ),
                    runtime_collab::BrowserServiceSessionCommand::ReleaseAccountSecret {
                        account,
                    } => Ok(runtime_collab::BrowserServiceSessionReply::AccountSecret {
                        account: account.clone(),
                        password: zeroize::Zeroizing::new("released-secret".into()),
                    }),
                    _ => Err("the session serves no such request".into()),
                })),
                ..tests::Captures::default()
            })),
            ..tests::FakeBroker::default()
        };
        let (fd, captures, _thread) = tests::stand_up_fake_broker(broker);
        let manager = Self::connect_in_background(
            SdkConnectSource::Fd(fd),
            SdkIdentitySource::Ready(Some(SdkJoinIdentity {
                presentation: harness_presentation(),
                participant_kind: "person",
                signer: harness_document_signer(),
            })),
            route,
            Some(kind.to_owned()),
            None,
        );
        let probe = JoiningProbe {
            answer: std::sync::Mutex::new(Some(answer)),
            room: room_status,
            sent: Box::new(move || captures.lock().unwrap().signed_ops.len()),
        };
        (manager, probe)
    }
}
