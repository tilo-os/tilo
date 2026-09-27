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
use tilo_relay::transport::REMOTE_ACTION_ENVELOPE_VERSION;
use tilo_relay::{ActionHostPlane, Envelope, RelayClient};
use tilo_user_identity::{DeviceCapabilities, RevocationList};

use crate::common::{TestDevice, TestUser, client_endpoint, spawn_relay, spawn_relay_serving_apps};

const WS: u64 = 11;

async fn subscribed(
    addr: iroh_base::EndpointAddr,
    user: &TestUser,
    device: &TestDevice,
) -> RelayClient {
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
    client
        .send(Envelope::Subscribe { workspace_id: WS })
        .unwrap();
    let ready = tilo_test_env::within("ready timeout", client.recv()).await;
    assert!(
        matches!(ready, Some(Envelope::Ready { .. })),
        "got {ready:?}"
    );
    client
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
        invocation,
        target: RemoteActionTarget {
            provider: "app".into(),
            address: ActionAddress::new("note:1", "archive"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
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

        // Without an app-serving plane the relay executes nothing.
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

/// Records which callers reached it; runs every admitted invocation to
/// completion immediately.
#[derive(Default)]
struct RecordingHost {
    callers: Mutex<Vec<Caller>>,
    ledger: Mutex<InvocationLedger<()>>,
}

/// Lets the test keep a handle on the host the relay owns.
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

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Result<(), ActionHostError> {
        Err(ActionHostError::NotCancellable {
            reason: "finished".into(),
        })
    }
}
