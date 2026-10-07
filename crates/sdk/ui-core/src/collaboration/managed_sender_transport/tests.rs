//! Controlled real Cap'n Proto transport. This is not a receiver/effect owner.
#![allow(
    refining_impl_trait,
    reason = "the fake servers return the concrete capnp Promise, as the broker's own fake servers do"
)]
use super::*;
use crate::collaboration::RouteSpec;
use capnp::capability::Promise;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use runtime_collab::collab_capnp::{
    channel_handle, collab_service, published_invocation_observation,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::Duration,
};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

const HOST: [u8; 16] = [2; 16];
const ID: [u8; 16] = [1; 16];
const UPDATE: &str = r#"{"state":"accepted","invocation":"01010101-0101-0101-0101-010101010101"}"#;
#[derive(Clone, Copy)]
enum Mode {
    Good,
    #[cfg_attr(
        not(target_os = "macos"),
        expect(
            dead_code,
            reason = "only the macOS cancellation fixture completes a call"
        )
    )]
    Succeeded,
    Absent,
    WrongHost,
    WrongInvocation,
    WrongNested,
    Malformed,
    ZeroWire,
    Error,
    Hold,
}
#[derive(Clone, Debug, PartialEq, Eq)]
struct Invocation {
    binding: PublishedInvocationBinding,
    revision: String,
    resource: String,
    action: String,
    argument: Option<String>,
    confirmation: String,
}
#[derive(Default)]
struct Captured {
    calls: Vec<u8>,
    invokes: Vec<Invocation>,
}
struct Server {
    host: [u8; 16],
    invocation: [u8; 16],
    mode: Rc<Cell<Mode>>,
    captured: Rc<RefCell<Captured>>,
}
fn binding() -> PublishedInvocationBinding {
    PublishedInvocationBinding {
        host: HOST,
        peer_id: 9,
        app_id: "notes".into(),
        record_id: "record".into(),
        root: [3; 32],
        device: [4; 32],
        target_binding: [5; 32],
    }
}
fn reply(
    mode: Mode,
    host: &[u8; 16],
    invocation: &[u8; 16],
    mut out: published_invocation_observation::Builder<'_>,
) {
    out.set_host(host);
    out.set_invocation(invocation);
    if matches!(mode, Mode::Absent) {
        return;
    }
    out.set_found(true);
    out.set_id(99);
    let invocation = uuid::Uuid::from_bytes(*invocation);
    let update = if matches!(mode, Mode::Succeeded) {
        format!(r#"{{"state":"succeeded","invocation":"{invocation}","output":{{}}}}"#)
    } else {
        format!(r#"{{"state":"accepted","invocation":"{invocation}"}}"#)
    };
    out.set_update_json(&update);
    match mode {
        Mode::WrongHost => out.set_host(&[8; 16]),
        Mode::WrongInvocation => out.set_invocation(&[8; 16]),
        Mode::WrongNested => out.set_update_json(
            r#"{"state":"accepted","invocation":"08080808-0808-0808-0808-080808080808"}"#,
        ),
        Mode::Malformed => out.set_update_json("not-json"),
        Mode::ZeroWire => out.set_id(0),
        _ => {}
    }
}
fn status(mode: Mode) -> Promise<(), capnp::Error> {
    match mode {
        Mode::Hold => Promise::from_future(std::future::pending()),
        Mode::Error => Promise::err(capnp::Error::failed("PRIVATE_SERVER_DETAIL".into())),
        _ => Promise::ok(()),
    }
}
impl channel_handle::Server for Server {
    fn invoke_resolved_published_action_durable(
        self: Rc<Self>,
        params: channel_handle::InvokeResolvedPublishedActionDurableParams,
        mut results: channel_handle::InvokeResolvedPublishedActionDurableResults,
    ) -> Promise<(), capnp::Error> {
        let result = (|| -> Result<(), capnp::Error> {
            let p = params.get()?;
            assert_eq!(p.get_invocation()?, self.invocation);
            let b = p.get_binding()?;
            self.captured.borrow_mut().calls.push(48);
            self.captured.borrow_mut().invokes.push(Invocation {
                binding: PublishedInvocationBinding {
                    host: b.get_host()?.try_into().unwrap(),
                    peer_id: b.get_peer_id(),
                    app_id: b.get_app_id()?.to_str()?.into(),
                    record_id: b.get_record_id()?.to_str()?.into(),
                    root: b.get_root()?.try_into().unwrap(),
                    device: b.get_device()?.try_into().unwrap(),
                    target_binding: b.get_target_binding()?.try_into().unwrap(),
                },
                revision: p.get_revision()?.to_str()?.into(),
                resource: p.get_resource()?.to_str()?.into(),
                action: p.get_action()?.to_str()?.into(),
                argument: if p.get_has_argument() {
                    Some(p.get_argument()?.to_str()?.into())
                } else {
                    None
                },
                confirmation: p.get_confirmation_json()?.to_str()?.into(),
            });
            reply(
                self.mode.get(),
                &self.host,
                &self.invocation,
                results.get().init_observation(),
            );
            Ok(())
        })();
        if let Err(e) = result {
            return Promise::err(e);
        }
        status(self.mode.get())
    }
    fn published_invocation_observation(
        self: Rc<Self>,
        params: channel_handle::PublishedInvocationObservationParams,
        mut results: channel_handle::PublishedInvocationObservationResults,
    ) -> Promise<(), capnp::Error> {
        let result = (|| -> Result<(), capnp::Error> {
            let p = params.get()?;
            assert_eq!(p.get_host()?, self.host);
            assert_eq!(p.get_invocation()?, self.invocation);
            self.captured.borrow_mut().calls.push(49);
            reply(
                self.mode.get(),
                &self.host,
                &self.invocation,
                results.get().init_observation(),
            );
            Ok(())
        })();
        if let Err(e) = result {
            return Promise::err(e);
        }
        status(self.mode.get())
    }
    fn cancel_durable_published_invocation(
        self: Rc<Self>,
        params: channel_handle::CancelDurablePublishedInvocationParams,
        mut results: channel_handle::CancelDurablePublishedInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let result = (|| -> Result<(), capnp::Error> {
            let p = params.get()?;
            assert_eq!(p.get_host()?, self.host);
            assert_eq!(p.get_invocation()?, self.invocation);
            self.captured.borrow_mut().calls.push(50);
            reply(
                self.mode.get(),
                &self.host,
                &self.invocation,
                results.get().init_observation(),
            );
            Ok(())
        })();
        if let Err(e) = result {
            return Promise::err(e);
        }
        status(self.mode.get())
    }
}
struct Broker {
    channel: channel_handle::Client,
}
impl collab_service::Server for Broker {
    fn join_channel(
        self: Rc<Self>,
        _params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        results.get().set_handle(self.channel.clone());
        Promise::ok(())
    }
}
#[allow(
    clippy::future_not_send,
    reason = "the fixture drives a capnp-rpc server on a LocalSet with Rc state"
)]
async fn over_wire<F, Fut>(run: F)
where
    F: FnOnce(SdkRoute, Rc<Cell<Mode>>, Rc<RefCell<Captured>>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    over_wire_identity(HOST, ID, run).await;
}
#[allow(
    clippy::future_not_send,
    reason = "the fixture drives a capnp-rpc server on a LocalSet with Rc state"
)]
async fn over_wire_identity<F, Fut>(host: [u8; 16], invocation: [u8; 16], run: F)
where
    F: FnOnce(SdkRoute, Rc<Cell<Mode>>, Rc<RefCell<Captured>>) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::task::LocalSet::new()
        .run_until(async {
            let mode = Rc::new(Cell::new(Mode::Good));
            let captured = Rc::new(RefCell::new(Captured::default()));
            let channel = capnp_rpc::new_client(Server {
                host,
                invocation,
                mode: mode.clone(),
                captured: captured.clone(),
            });
            let broker: collab_service::Client = capnp_rpc::new_client(Broker { channel });
            let (server_io, client_io) = tokio::io::duplex(512);
            let (read, write) = tokio::io::split(server_io);
            let network = twoparty::VatNetwork::new(
                read.compat(),
                write.compat_write(),
                rpc_twoparty_capnp::Side::Server,
                capnp::message::ReaderOptions::default(),
            );
            let task =
                tokio::task::spawn_local(RpcSystem::new(Box::new(network), Some(broker.client)));
            let connection = runtime_portal::CapnpClient::from_connected_io(
                client_io,
                rpc_twoparty_capnp::Side::Client,
            );
            let channel = runtime_collab::CollabClient::from_raw(connection.bootstrap())
                .join_channel("transport-test", b"inert-test-identity")
                .await
                .unwrap();
            let route = SdkRoute {
                _checks: None,
                spec: RouteSpec {
                    app_id: "notes".into(),
                    cluster_id: uuid::Uuid::from_u128(7),
                    record: Some("record".into()),
                },
                channel,
                presence: None,
                workspace_controls: Default::default(),
                committed: false,
                encryption: std::sync::Arc::new(std::sync::Mutex::new(
                    crate::collaboration::route_keys::RouteKeys::plain(),
                )),
            };
            tilo_test_env::within("bounded transport test", run(route, mode, captured)).await;
            task.abort();
            let _ = task.await;
        })
        .await;
}
fn request<'a>(
    kind: u8,
    b: &'a PublishedInvocationBinding,
    argument: Option<&'a str>,
) -> Request<'a> {
    match kind {
        48 => Request::Invoke {
            invocation: &ID,
            binding: b,
            revision: "revision-original",
            resource: "resource-original",
            action: "action-original",
            argument,
            confirmation_json: "{\"current\":true}",
        },
        49 => Request::Observe {
            host: &HOST,
            invocation: &ID,
        },
        50 => Request::Cancel {
            host: &HOST,
            invocation: &ID,
        },
        _ => unreachable!(),
    }
}
#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_rpc_preserves_original_request_and_all_three_methods() {
    over_wire(|route, _, captured| async move {
        let b = binding();
        for arg in [None, Some(""), Some("exact original argument")] {
            let got = send(&route, request(48, &b, arg), tilo_test_env::hang_deadline())
                .await
                .unwrap();
            assert_eq!(got.host, HOST);
            assert_eq!(got.invocation, ID);
            assert_eq!(got.result.unwrap().update_json, UPDATE);
        }
        for kind in [49, 50] {
            assert_eq!(
                send(
                    &route,
                    request(kind, &b, None),
                    tilo_test_env::hang_deadline()
                )
                .await
                .unwrap()
                .result
                .unwrap()
                .id,
                99
            );
        }
        assert_eq!(captured.borrow().calls, [48, 48, 48, 49, 50]);
        assert_eq!(
            captured.borrow().invokes,
            [None, Some(""), Some("exact original argument")].map(|arg| Invocation {
                binding: b.clone(),
                revision: "revision-original".into(),
                resource: "resource-original".into(),
                action: "action-original".into(),
                argument: arg.map(str::to_owned),
                confirmation: "{\"current\":true}".into()
            })
        );
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_rpc_refuses_uncorrelated_and_failed_replies() {
    over_wire(|route, mode, captured| async move {
        let b = binding();
        for m in [
            Mode::WrongHost,
            Mode::WrongInvocation,
            Mode::WrongNested,
            Mode::Malformed,
            Mode::ZeroWire,
            Mode::Error,
        ] {
            mode.set(m);
            for kind in [48, 49, 50] {
                assert_eq!(
                    send(
                        &route,
                        request(kind, &b, None),
                        tilo_test_env::hang_deadline()
                    )
                    .await,
                    Err(Failure::Unavailable)
                );
            }
        }
        assert_eq!(captured.borrow().calls.len(), 18);
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_rpc_absence_stays_absence() {
    over_wire(|route, mode, captured| async move {
        mode.set(Mode::Absent);
        let b = binding();
        for kind in [48, 49, 50] {
            let o = send(
                &route,
                request(kind, &b, None),
                tilo_test_env::hang_deadline(),
            )
            .await
            .unwrap();
            assert_eq!(o.host, HOST);
            assert_eq!(o.invocation, ID);
            assert!(o.result.is_none());
        }
        assert_eq!(captured.borrow().calls, [48, 49, 50]);
    })
    .await;
}
#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_rpc_deadlines_bound_calls_without_resubmission() {
    over_wire(|route, mode, captured| async move {
        let b = binding();
        for kind in [48, 49, 50] {
            assert_eq!(
                send(
                    &route,
                    request(kind, &b, None),
                    Instant::now() - Duration::from_secs(1)
                )
                .await,
                Err(Failure::Deadline)
            );
        }
        assert!(captured.borrow().calls.is_empty());
        mode.set(Mode::Hold);
        for kind in [48, 49, 50] {
            let start = Instant::now();
            let deadline = start
                + if kind == 48 {
                    Duration::from_secs(30)
                } else {
                    Duration::from_millis(100)
                };
            assert_eq!(
                send(&route, request(kind, &b, None), deadline).await,
                Err(Failure::Deadline)
            );
            assert!(start.elapsed() < Duration::from_secs(3));
        }
        assert_eq!(captured.borrow().calls, [48, 49, 50]);
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_rpc_ready_reply_after_deadline_is_refused() {
    use std::{
        future::Future,
        sync::Arc,
        task::{Context, Poll, Wake, Waker},
    };
    struct ResponseWake(tokio::sync::Notify);
    impl Wake for ResponseWake {
        fn wake(self: Arc<Self>) {
            self.0.notify_one();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.notify_one();
        }
    }
    over_wire(|route, _, captured| async move {
        let binding = binding();
        let mut replies = Vec::new();
        for kind in [48, 49, 50] {
            let deadline = Instant::now() + Duration::from_secs(1);
            let mut pending = std::pin::pin!(send(&route, request(kind, &binding, None), deadline));
            let wake = Arc::new(ResponseWake(tokio::sync::Notify::new()));
            let waker = Waker::from(wake.clone());
            // Dispatch on the actual channel, then deliberately withhold all
            // further polls of send while the independent RPC systems run.
            assert!(matches!(
                pending.as_mut().poll(&mut Context::from_waker(&waker)),
                Poll::Pending
            ));
            tilo_test_env::within("the actual response to wake the caller", wake.0.notified())
                .await;
            assert!(
                Instant::now() < deadline,
                "wake must not be the elapsed timeout"
            );
            assert_eq!(captured.borrow().calls.last(), Some(&kind));
            // A short, bounded retained deadline; no app/effect owner is mocked.
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            assert!(Instant::now() >= deadline);
            replies.push(pending.await);
        }
        assert_eq!(captured.borrow().calls, [48, 49, 50]);
        assert_eq!(replies, vec![Err(Failure::Deadline); 3]);
    })
    .await;
}

#[cfg(target_os = "macos")]
#[tokio::test(flavor = "current_thread")]
async fn managed_sender_transport_real_physical_sender_dispatch_and_result_custody() {
    use super::super::{
        BridgeState, CollabEvent, CollabRequest, InboundBridge, LocalSdkDocumentSigner,
        QueuedSdkCommand, SdkCommand, SdkDocumentSigner, dispatch_sdk_request, enqueue_sdk_command,
    };
    use crate::host_worker::managed_sender::TransportFixture;
    use std::sync::atomic::Ordering;
    let mut fixture = TransportFixture::new();
    let original_invocation = fixture.invocation();
    let host = fixture.binding().host;
    Box::pin(over_wire_identity(
        host,
        original_invocation,
        |mut route, mode, captured| async move {
            route.spec = fixture.route();
            let expected = Invocation {
                binding: fixture.binding().clone(),
                revision: fixture.revision().into(),
                resource: fixture.resource().into(),
                action: fixture.action().into(),
                argument: fixture.argument().map(str::to_owned),
                confirmation: fixture.confirmation().into(),
            };
            let state = BridgeState::new();
            let (commands, mut command_rx) = tokio::sync::mpsc::channel(4);
            let (events, mut event_rx) = tokio::sync::mpsc::channel(4);
            let inbound = InboundBridge::new(events, state.clone());
            // The managed dispatch arm never uses document signing. This explicit
            // existing harness signer is not a receiver identity or authority.
            let mut signer = LocalSdkDocumentSigner::Inherited(SdkDocumentSigner::Harness(
                Box::new(ed25519_dalek::SigningKey::from_bytes(&[19; 32])),
            ));
            for kind in [48, 49, 50] {
                let attempt = match kind {
                    48 => fixture.invoke(),
                    49 => fixture.observe(),
                    50 => fixture.cancel(),
                    _ => unreachable!(),
                };
                mode.set(if kind == 50 {
                    Mode::Succeeded
                } else {
                    Mode::Good
                });
                assert!(enqueue_sdk_command(
                    &commands,
                    SdkCommand::ToRoute(
                        route.spec.clone(),
                        CollabRequest::ManagedInvocationAttempt { attempt }
                    ),
                    &state
                ));
                assert!(state.command_bytes.0.load(Ordering::Acquire) > 0);
                let QueuedSdkCommand {
                    command,
                    _reservation: command_credit,
                } = command_rx.recv().await.unwrap();
                let SdkCommand::ToRoute(spec, request) = command else {
                    panic!("original routed command")
                };
                assert_eq!(spec, route.spec);
                dispatch_sdk_request(&mut route, request, &mut signer, &inbound).await;
                assert!(state.command_bytes.0.load(Ordering::Acquire) > 0);
                drop(command_credit);
                assert_eq!(state.command_bytes.0.load(Ordering::Acquire), 0);
                let queued = event_rx.recv().await.unwrap();
                assert!(state.event_bytes.0.load(Ordering::Acquire) > 0);
                let CollabEvent::ManagedInvocationObservation { result } = &queued.event else {
                    panic!("original managed result")
                };
                let retained = result.clone();
                // Real result retains the original funded attempt independently
                // of the inbound queue's reservation.
                fixture.queue_result(result.clone());
                drop(queued);
                assert_eq!(state.event_bytes.0.load(Ordering::Acquire), 0);
                fixture.finish_result();
                let invocation = uuid::Uuid::from_bytes(original_invocation);
                let expected_json = if kind == 50 {
                    format!(r#"{{"state":"succeeded","invocation":"{invocation}","output":{{}}}}"#)
                } else {
                    format!(r#"{{"state":"accepted","invocation":"{invocation}"}}"#)
                };
                fixture.assert_readback(&expected_json);
                if kind == 50 {
                    fixture.assert_consume_busy();
                }
                drop(retained);
                assert_eq!(fixture.invocation(), original_invocation);
            }
            assert_eq!(captured.borrow().calls, [48, 49, 50]);
            assert_eq!(captured.borrow().invokes, [expected]);
            assert!(event_rx.try_recv().is_err());
            assert!(state.is_healthy());
            fixture.consume_and_finish();
        },
    ))
    .await;
}
