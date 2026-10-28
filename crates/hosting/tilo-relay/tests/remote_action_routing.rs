//! Remote-action envelopes: the relay routes a frame to the one addressed
//! device, stamps the authenticated sender, never stores it, and answers
//! frames addressed to itself only through a supplied app-serving plane.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationLedger,
    InvocationReceipt, InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
    RemoteActionFrame, RemoteActionMessage, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_delivery_protocol::peer_actions::UnavailableReason;
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::{ActionHostPlane, Envelope, RelayClient};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay, spawn_relay_serving_apps};

const WS: u64 = 11;

async fn subscribed(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> RelayClient {
    subscribed_in(addr, user, device, WS).await
}

async fn subscribed_in(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
    workspace_id: u64,
) -> RelayClient {
    let (client, ready) = subscribe(addr, user, device, workspace_id).await;
    assert!(
        matches!(ready, Some(Envelope::Ready { .. })),
        "got {ready:?}"
    );
    client
}

/// Connect as `device`, subscribe to `workspace_id`, and return the relay's
/// first answer, whatever it is.
async fn subscribe(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
    workspace_id: u64,
) -> (RelayClient, Option<Envelope>) {
    let client = RelayClient::connect(
        client_endpoint().await,
        addr,
        user.user_pk,
        device.cert.clone(),
        device.sk.clone(),
        RevocationList::new(),
    )
    .await
    .expect("connect");
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    let ready = tilo_test_env::within("ready timeout", client.recv()).await;
    (client, ready)
}

fn frame(message: RemoteActionMessage) -> Vec<u8> {
    RemoteActionFrame::new(message).encode().unwrap()
}

fn directed(to: [u8; 32], frame: Vec<u8>) -> Envelope {
    Envelope::RemoteAction {
        workspace_id: WS,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: to,
        // A sender cannot choose who it claims to be.
        from_device_public_key: [0xEE; 32],
        from_user_public_key: [0xEE; 32],
        frame,
    }
}

async fn next(client: &RelayClient, who: &str) -> Envelope {
    tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
        .await
        .unwrap_or_else(|_| panic!("{who} timed out"))
        .unwrap_or_else(|| panic!("{who} connection closed"))
}

fn invoke(invocation: InvocationId) -> RemoteActionMessage {
    RemoteActionMessage::Invoke(InvokeRequest {
        on_behalf_of: None,
        invocation,
        target: RemoteActionTarget {
            provider: "app".into(),
            address: ActionAddress::new("note:1", "archive"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    })
}

#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
async fn stamped_action_frames_route_to_their_addressee_or_are_answered() {
    // Phase `the_relay_routes_a_stamped_frame_to_the_addressed_device_only`.
    {
        let user = TestUser::fresh("Alice");
        let host = user.enrol("home", DeviceCapabilities::host());
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("desktop", DeviceCapabilities::member());
        let c = user.enrol("phone", DeviceCapabilities::member());
        let members = [a.device_pubkey(), b.device_pubkey(), c.device_pubkey()];
        let relay = Box::pin(spawn_relay(&user, host, &[WS], &members)).await;
        let addr = relay.relay.node_addr();
        let a_client = subscribed(addr.clone(), &user, &a).await;
        let b_client = subscribed(addr.clone(), &user, &b).await;
        let c_client = subscribed(addr, &user, &c).await;

        let request = frame(invoke(InvocationId::new()));
        a_client
            .send(directed(b.device_pubkey(), request.clone()))
            .unwrap();
        match next(&b_client, "B").await {
            Envelope::RemoteAction {
                to_device_public_key,
                from_device_public_key,
                from_user_public_key,
                frame,
                ..
            } => {
                assert_eq!(to_device_public_key, b.device_pubkey());
                assert_eq!(from_device_public_key, a.device_pubkey());
                assert_eq!(from_user_public_key, user.user_pk);
                assert_eq!(frame, request, "the relay forwards the frame untouched");
            }
            other => panic!("B expected a remote action, got {other:?}"),
        }
        let stray = tokio::time::timeout(Duration::from_millis(300), c_client.recv()).await;
        assert!(stray.is_err(), "a directed frame reaches no other device");
    }
    // Phase `undeliverable_frames_are_answered_not_stored`.
    {
        let user = TestUser::fresh("Alice");
        let host = user.enrol("home", DeviceCapabilities::host());
        let host_key = host.device_pubkey();
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let b = user.enrol("desktop", DeviceCapabilities::member());
        let relay = Box::pin(spawn_relay(
            &user,
            host,
            &[WS],
            &[a.device_pubkey(), b.device_pubkey()],
        ))
        .await;
        let addr = relay.relay.node_addr();
        let a_client = subscribed(addr.clone(), &user, &a).await;

        // B is a member but offline: nothing is queued for it.
        a_client
            .send(directed(
                b.device_pubkey(),
                frame(invoke(InvocationId::new())),
            ))
            .unwrap();
        assert!(matches!(
            next(&a_client, "A").await,
            Envelope::RemoteActionUndeliverable { to_device_public_key, .. }
                if to_device_public_key == b.device_pubkey()
        ));
        let b_client = subscribed(addr, &user, &b).await;
        let replayed = tokio::time::timeout(Duration::from_millis(300), b_client.recv()).await;
        assert!(replayed.is_err(), "remote actions are never replayed late");

        a_client
            .send(directed(host_key, frame(invoke(InvocationId::new()))))
            .unwrap();
        assert!(matches!(
            next(&a_client, "A").await,
            Envelope::RemoteActionUndeliverable { reason, .. } if reason.contains("does not serve apps")
        ));
        assert!(!relay.relay.handle().advertised_services().app_serving);

        // An envelope version this relay does not know is refused, not guessed.
        let Envelope::RemoteAction {
            workspace_id,
            to_device_public_key,
            from_device_public_key,
            from_user_public_key,
            frame,
            ..
        } = directed(b.device_pubkey(), frame(invoke(InvocationId::new())))
        else {
            unreachable!();
        };
        a_client
            .send(Envelope::RemoteAction {
                workspace_id,
                version: REMOTE_ACTION_ENVELOPE_VERSION + 1,
                to_device_public_key,
                from_device_public_key,
                from_user_public_key,
                frame,
            })
            .unwrap();
        assert!(matches!(
            next(&a_client, "A").await,
            Envelope::RemoteActionUndeliverable { reason, .. } if reason.contains("version")
        ));
    }
    // Phase `an_app_serving_host_answers_frames_addressed_to_itself`.
    {
        let user = TestUser::fresh("Alice");
        let host = user.enrol("home", DeviceCapabilities::host());
        let host_key = host.device_pubkey();
        let a = user.enrol("laptop", DeviceCapabilities::primary());
        let actions = Arc::new(RecordingHost::default());
        let relay = Box::pin(spawn_relay_serving_apps(
            &user,
            host,
            &[WS],
            &[a.device_pubkey()],
            Arc::new(ActionHostPlane::new(SharedHost(Arc::clone(&actions)))),
        ))
        .await;
        assert!(relay.relay.handle().advertised_services().app_serving);
        let a_client = subscribed(relay.relay.node_addr(), &user, &a).await;

        let id = InvocationId::new();
        a_client
            .send(directed(host_key, frame(invoke(id))))
            .unwrap();
        let Envelope::RemoteAction {
            to_device_public_key,
            from_device_public_key,
            frame: reply,
            ..
        } = next(&a_client, "A").await
        else {
            panic!("the host answers with a remote action frame");
        };
        assert_eq!(to_device_public_key, a.device_pubkey());
        assert_eq!(from_device_public_key, host_key);
        let RemoteActionMessage::Receipt(receipt) =
            RemoteActionFrame::decode(&reply).unwrap().message
        else {
            panic!("an invoke is answered with a receipt");
        };
        assert_eq!(receipt.invocation, id);
        assert!(matches!(receipt.status, ReceiptStatus::Completed { .. }));
        assert_eq!(
            *actions.callers.lock().unwrap(),
            [Caller::authenticated(user.user_pk, a.device_pubkey())],
            "the plane sees the principal the relay authenticated, not the claimed sender"
        );
    }
}

/// How the relay answered a frame it must refuse: the typed reason it led
/// with, or `None` when the access audit closed the session first —
/// either way nothing was served. A served frame fails the test.
async fn refusal(client: &RelayClient, who: &str) -> Option<UnavailableReason> {
    loop {
        match tokio::time::timeout(tilo_test_env::hang_bound(), client.recv())
            .await
            .unwrap_or_else(|_| panic!("{who} timed out"))
        {
            None => return None,
            Some(Envelope::RemoteActionUndeliverable { reason, .. }) => {
                let typed = UnavailableReason::from_wire_reason(&reason);
                assert!(typed.is_some(), "{reason} leads with a wire token");
                return typed;
            }
            Some(Envelope::NotAuthorised { .. }) => {}
            Some(other) => panic!("{who} expected a refusal, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn removed_member_session_ends_without_serving_an_action() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::host());
    let host_key = host.device_pubkey();
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let b = user.enrol("desktop", DeviceCapabilities::member());
    let actions = Arc::new(RecordingHost::default());
    let relay = Box::pin(spawn_relay_serving_apps(
        &user,
        host,
        &[WS],
        &[a.device_pubkey(), b.device_pubkey()],
        Arc::new(ActionHostPlane::new(SharedHost(Arc::clone(&actions)))),
    ))
    .await;
    let addr = relay.relay.node_addr();
    let _a_client = subscribed(addr.clone(), &user, &a).await;
    let b_client = subscribed(addr, &user, &b).await;

    // Removing B wakes the access audit, which ends B's session. Wait for
    // that end rather than racing a send against it; the refusal of a frame
    // that arrives before the audit is covered at the handler by
    // `peer::private_transport_tests::removed_member_remote_action_is_refused`.
    crate::common::admit_members(&relay.relay, WS, &[a.device_pubkey()], &relay.state_root).await;
    while tilo_test_env::within("removed member's session to end", b_client.recv())
        .await
        .is_some()
    {}
    tilo_test_env::within(
        "removed member's outbound to close",
        b_client.outbound.closed(),
    )
    .await;
    assert!(
        b_client
            .send(directed(host_key, frame(invoke(InvocationId::new()))))
            .is_err(),
        "an ended session queues no action"
    );
    assert!(
        actions.callers.lock().unwrap().is_empty(),
        "a removed member never reaches the app-serving plane"
    );
}

#[tokio::test]
async fn remote_action_to_removed_target_is_refused() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::host());
    let a = user.enrol("laptop", DeviceCapabilities::primary());
    let c = user.enrol("phone", DeviceCapabilities::member());
    let relay = Box::pin(spawn_relay(
        &user,
        host,
        &[WS],
        &[a.device_pubkey(), c.device_pubkey()],
    ))
    .await;
    let addr = relay.relay.node_addr();
    let a_client = subscribed(addr.clone(), &user, &a).await;
    let c_client = subscribed(addr, &user, &c).await;

    crate::common::admit_members(&relay.relay, WS, &[a.device_pubkey()], &relay.state_root).await;
    a_client
        .send(directed(
            c.device_pubkey(),
            frame(invoke(InvocationId::new())),
        ))
        .unwrap();
    match next(&a_client, "A").await {
        Envelope::RemoteActionUndeliverable {
            to_device_public_key,
            reason,
            ..
        } => {
            assert_eq!(to_device_public_key, c.device_pubkey());
            assert_eq!(
                UnavailableReason::from_wire_reason(&reason),
                Some(UnavailableReason::NotAuthorised),
                "{reason}"
            );
        }
        other => panic!("A expected a refusal, got {other:?}"),
    }
    let deadline = tokio::time::Instant::now()
        .checked_add(Duration::from_millis(300))
        .unwrap();
    while let Ok(Some(envelope)) = tokio::time::timeout_at(deadline, c_client.recv()).await {
        assert!(
            !matches!(envelope, Envelope::RemoteAction { .. }),
            "a removed device receives no remote action"
        );
    }
}

#[tokio::test]
async fn expired_agent_delegation_cannot_invoke_host() {
    let user = TestUser::fresh("Alice");
    let host = user.enrol("home", DeviceCapabilities::host());
    let host_key = host.device_pubkey();
    let agent = user.enrol("agent", DeviceCapabilities::member());
    let room = "6f1c2a3e-2b7d-4c55-9a0e-5d1f00c0ffee";
    let ws = tilo_relay::room_workspace_id(room).unwrap();
    let actions = Arc::new(RecordingHost::default());
    let relay = Box::pin(spawn_relay_serving_apps(
        &user,
        host,
        &[ws],
        &[agent.device_pubkey()],
        Arc::new(ActionHostPlane::new(SharedHost(Arc::clone(&actions)))),
    ))
    .await;
    let to_host = |frame| Envelope::RemoteAction {
        workspace_id: ws,
        version: REMOTE_ACTION_ENVELOPE_VERSION,
        to_device_public_key: host_key,
        from_device_public_key: [0xEE; 32],
        from_user_public_key: [0xEE; 32],
        frame,
    };
    let delegated = [Caller::delegated_agent(
        "helper",
        user.user_pk,
        agent.device_pubkey(),
    )];

    // Delegate the subscribed device briefly; until expiry it acts as the
    // agent. The live half only proves something if its answer came back
    // before the delegation ran out: a runner that stalls past a short
    // lifetime may rightly refuse it. Then the delegation is issued again,
    // living twice as long, on a fresh session (the expired one may already
    // be cut off by the access audit, and subscribing under it is refused).
    let mut lifetime = Duration::from_millis(500);
    let (client, expires) = loop {
        let issued = tilo_clock::try_unix_now_ns().unwrap();
        let expires = issued
            .checked_add(u64::try_from(lifetime.as_nanos()).unwrap())
            .unwrap();
        let delegation = Delegation::sign(
            &user.user_sk,
            agent.device_pubkey(),
            tilo_relay_protocol::delegation::Delegate::Agent {
                agent_id: "helper".into(),
            },
            "Helper",
            [0x7c, 0x3a, 0xed],
            DelegateScope {
                rooms: vec![room.into()],
                apps: Vec::new(),
            },
            issued,
            expires,
        )
        .unwrap();
        relay.relay.register_agent(delegation).unwrap();
        actions.callers.lock().unwrap().clear();
        let (client, ready) = subscribe(relay.relay.node_addr(), &user, &agent, ws).await;
        let answer = if matches!(ready, Some(Envelope::Ready { .. })) {
            client
                .send(to_host(frame(invoke(InvocationId::new()))))
                .unwrap();
            Some(next(&client, "agent").await)
        } else {
            None
        };
        if tilo_clock::try_unix_now_ns().unwrap() < expires {
            assert!(
                matches!(answer, Some(Envelope::RemoteAction { .. })),
                "a live delegation subscribes ({ready:?}) and reaches the host, got {answer:?}"
            );
            assert_eq!(*actions.callers.lock().unwrap(), delegated);
            break (client, expires);
        }
        assert!(
            lifetime < tilo_test_env::hang_bound(),
            "no delegation lifetime up to {lifetime:?} outlived one round trip"
        );
        lifetime = lifetime.saturating_mul(2);
    };

    // Just past expiry. An access audit may already have ended the session
    // (one is due at the expiry, and a connection's first comes a second
    // after it connected), and then the frame is never sent.
    let remaining = expires.saturating_sub(tilo_clock::try_unix_now_ns().unwrap());
    tokio::time::sleep(Duration::from_nanos(remaining).saturating_add(Duration::from_millis(20)))
        .await;
    let _ = client.send(to_host(frame(invoke(InvocationId::new()))));
    let refused = refusal(&client, "agent").await;
    assert!(
        refused.is_none_or(|reason| reason == UnavailableReason::NotAuthorised),
        "got {refused:?}"
    );
    assert_eq!(
        *actions.callers.lock().unwrap(),
        delegated,
        "an expired delegation never reaches the app-serving plane"
    );
}

/// Records which callers reached it; runs every admitted invocation to
/// completion immediately.
#[derive(Default)]
struct RecordingHost {
    callers: Mutex<Vec<Caller>>,
    ledger: Mutex<InvocationLedger<()>>,
}

/// Lets the test keep a handle on the host the relay owns.
#[derive(Clone)]
struct SharedHost(Arc<RecordingHost>);

impl ActionHost for SharedHost {
    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.0.callers.lock().unwrap().push(caller.clone());
        let admitted = self
            .0
            .ledger
            .lock()
            .unwrap()
            .admit(caller, &request, || Ok(()))
            .map(|_| ());
        Pending::ready(admitted.map(|()| InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput::default(),
            },
        }))
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.0.ledger.lock().unwrap().get(caller, id).map(|()| {
            InvocationState::new(
                *id,
                InvocationUpdate::Succeeded {
                    invocation: *id,
                    output: tilo_actions::ActionOutput::default(),
                },
            )
        })
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionHostError>> {
        tilo_actions::remote::Pending::ready(Err(ActionHostError::NotCancellable {
            reason: "finished".into(),
        }))
    }
}
