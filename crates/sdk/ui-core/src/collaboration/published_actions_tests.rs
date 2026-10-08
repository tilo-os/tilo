use super::*;
use capnp::capability::Promise;
use runtime_collab::collab_capnp::{channel_handle, collab_service};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::AtomicU64;
use tilo_actions::remote::{InvocationReceipt, ReceiptStatus};

struct Channel {
    calls: Rc<RefCell<Vec<RemoteActionFrame>>>,
    mode: Rc<Cell<u8>>,
    release: Rc<tokio::sync::Notify>,
}
#[expect(
    clippy::future_not_send,
    reason = "the capnp test endpoint runs on the owner LocalSet"
)]
impl channel_handle::Server for Channel {
    fn remote_action(
        self: Rc<Self>,
        params: channel_handle::RemoteActionParams,
        mut results: channel_handle::RemoteActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let request =
            RemoteActionFrame::decode_continuation(params.get().unwrap().get_frame().unwrap())
                .unwrap();
        self.calls.borrow_mut().push(request.clone());
        let mode = self.mode.get();
        Promise::from_future(async move {
            if mode == 3 {
                self.release.notified().await;
            }
            if mode == 2 {
                return Err(capnp::Error::failed(
                    "fixture disconnected after receiving request".into(),
                ));
            }
            let message = match request.message {
                RemoteActionMessage::CatalogueRequest { correlation, .. } => {
                    RemoteActionMessage::CatalogueResponse {
                        correlation,
                        result: Ok(Default::default()),
                    }
                }
                RemoteActionMessage::Invoke(request) => {
                    RemoteActionMessage::Receipt(InvocationReceipt {
                        invocation: if mode == 1 {
                            InvocationId::new()
                        } else {
                            request.invocation
                        },
                        status: ReceiptStatus::Accepted,
                    })
                }
                _ => panic!("unexpected fixture request"),
            };
            results
                .get()
                .set_frame(&RemoteActionFrame::new(message).encode().unwrap());
            Ok(())
        })
    }
}
struct Broker(channel_handle::Client);
#[expect(
    clippy::future_not_send,
    reason = "the capnp test endpoint runs on the owner LocalSet"
)]
impl collab_service::Server for Broker {
    fn join_channel(
        self: Rc<Self>,
        _: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_handle(self.0.clone());
        Promise::ok(())
    }
}
fn invoke() -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: "other".into(),
            address: tilo_actions::ActionAddress::new("document", "read"),
        },
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
    }
}
#[expect(
    clippy::future_not_send,
    reason = "the observer runs on the endpoint LocalSet"
)]
async fn observed(calls: &Rc<RefCell<Vec<RemoteActionFrame>>>, count: usize) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while calls.borrow().len() < count {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("production channel must receive the request");
}

#[tokio::test(flavor = "current_thread")]
async fn published_action_client_matches_original_parent_and_retains_unknown_child_without_retry() {
    tokio::task::LocalSet::new().run_until(async {
        let fixture = crate::invocation_scope::tests::fixture(1841);
        let time = Arc::new(AtomicU64::new(0));
        let scope = crate::invocation_scope::tests::scope(&fixture, 1841, "caller", 100, &time);
        let client = scope.published_actions().unwrap();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mode = Rc::new(Cell::new(0));
        let raw = capnp_rpc::new_client(Channel {
            calls: calls.clone(), mode: mode.clone(), release: Rc::new(tokio::sync::Notify::new()),
        });
        let broker = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(Broker(raw)));
        let channel = broker.join_channel("fixture", &[]).await.unwrap();
        let registration = register(scope.route().channel_id(), channel).unwrap();
        assert!(matches!(client.query(Default::default()).unwrap().await,
            PublishedActionAnswer::Answer(RemoteActionMessage::CatalogueResponse { result: Ok(_), .. })));
        assert_eq!(calls.borrow()[0].on_behalf_of, Some(scope.invocation()));
        let request = invoke();
        assert!(matches!(client.invoke(request.clone()).unwrap().await,
            PublishedActionAnswer::Answer(RemoteActionMessage::Receipt(receipt)) if receipt.invocation == request.invocation && receipt.status == ReceiptStatus::Accepted));
        let recorded = calls.borrow()[1].message.clone();
        let RemoteActionMessage::Invoke(recorded) = recorded else { panic!() };
        assert_eq!(recorded.on_behalf_of, Some(scope.invocation()));
        assert_eq!(calls.borrow()[1].on_behalf_of, None);
        mode.set(1);
        assert_eq!(client.invoke(request.clone()).unwrap().await,
            PublishedActionAnswer::Unknown { invocation: Some(request.invocation) });
        mode.set(2);
        assert_eq!(client.invoke(request.clone()).unwrap().await,
            PublishedActionAnswer::Unknown { invocation: Some(request.invocation) });
        assert_eq!(calls.borrow().len(), 4, "neither wrong response nor lost admission retries");
        scope.cancel();
        assert!(client.invoke(request).is_err());
        assert_eq!(calls.borrow().len(), 4);
        drop(registration);
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn published_action_client_queued_cancel_and_dropped_waiter_release_existing_worker() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = crate::invocation_scope::tests::fixture(1842);
            let time = Arc::new(AtomicU64::new(0));
            let scope = crate::invocation_scope::tests::scope(&fixture, 1842, "caller", 100, &time);
            let client = scope.published_actions().unwrap();
            let calls = Rc::new(RefCell::new(Vec::new()));
            let mode = Rc::new(Cell::new(3));
            let release = Rc::new(tokio::sync::Notify::new());
            let raw = capnp_rpc::new_client(Channel {
                calls: calls.clone(),
                mode: mode.clone(),
                release: release.clone(),
            });
            let broker = runtime_collab::CollabClient::from_raw(capnp_rpc::new_client(Broker(raw)));
            let channel = broker.join_channel("fixture", &[]).await.unwrap();
            let registration = register(scope.route().channel_id(), channel).unwrap();
            let held = client.query(Default::default()).unwrap();
            observed(&calls, 1).await;
            let queued = client.invoke(invoke()).unwrap();
            let second_queued = client.query(Default::default()).unwrap();
            assert!(
                client.invoke(invoke()).is_err(),
                "full bounded queue refuses before dispatch"
            );
            scope.cancel();
            drop(held);
            assert_eq!(queued.await, PublishedActionAnswer::Unavailable);
            assert_eq!(second_queued.await, PublishedActionAnswer::Unavailable);
            assert_eq!(
                calls.borrow().len(),
                1,
                "cancelled queued child never crosses channel"
            );
            let fresh = crate::invocation_scope::tests::scope(&fixture, 1843, "caller", 100, &time);
            mode.set(0);
            assert!(
                matches!(
                    fresh
                        .published_actions()
                        .unwrap()
                        .query(Default::default())
                        .unwrap()
                        .await,
                    PublishedActionAnswer::Answer(_)
                ),
                "dropped blocked request releases worker for fresh custody"
            );
            assert_eq!(calls.borrow().len(), 2);
            mode.set(3);
            let request = invoke();
            let held = fresh
                .published_actions()
                .unwrap()
                .invoke(request.clone())
                .unwrap();
            observed(&calls, 3).await;
            drop(registration);
            assert_eq!(
                held.await,
                PublishedActionAnswer::Unknown {
                    invocation: Some(request.invocation)
                },
                "route retirement cannot relabel an admitted unknown as failure or retry"
            );
        })
        .await;
}
