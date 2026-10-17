use super::*;

use capnp::capability::Promise;
use std::cell::{Cell, RefCell};

#[derive(Debug, PartialEq, Eq)]
struct Received {
    peer: u64,
    revision: String,
    resource: String,
    action: String,
    argument: Option<String>,
    confirmation: String,
}

struct ResolvedServer(Rc<RefCell<Vec<Received>>>);
impl collab_capnp::channel_handle::Server for ResolvedServer {
    fn invoke_resolved_published_action(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::InvokeResolvedPublishedActionParams,
        mut results: collab_capnp::channel_handle::InvokeResolvedPublishedActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            let p = params.get()?;
            self.0.borrow_mut().push(Received {
                peer: p.get_peer_id(),
                revision: p.get_revision()?.to_str()?.to_owned(),
                resource: p.get_resource()?.to_str()?.to_owned(),
                action: p.get_action()?.to_str()?.to_owned(),
                argument: p
                    .get_has_argument()
                    .then(|| Ok::<String, capnp::Error>(p.get_argument()?.to_str()?.to_owned()))
                    .transpose()?,
                confirmation: p.get_confirmation_json()?.to_str()?.to_owned(),
            });
            let mut out = results.get();
            out.set_id(987);
            out.set_update_json(r#"{"state":"accepted"}"#);
            Ok(())
        })())
    }
}

// The server owns only the old eager method. The generated unsupported
// method returns the same Unimplemented category an older dispatch does.
struct LegacyServer(Rc<Cell<usize>>);
impl collab_capnp::channel_handle::Server for LegacyServer {
    fn invoke_published_action(
        self: Rc<Self>,
        params: collab_capnp::channel_handle::InvokePublishedActionParams,
        mut results: collab_capnp::channel_handle::InvokePublishedActionResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        Promise::from((|| {
            let p = params.get()?;
            assert_eq!(p.get_peer_id(), 9);
            assert_eq!(p.get_resource()?.to_str()?, "r");
            assert_eq!(p.get_action()?.to_str()?, "a");
            assert!(!p.get_has_argument());
            self.0.set(self.0.get().saturating_add(1));
            results.get().set_id(21);
            Ok(())
        })())
    }
}

/// Run `action` against `server` through a real Cap'n Proto connection.
pub(crate) async fn over_wire<S, F, Fut>(server: S, action: F)
where
    S: collab_capnp::channel_handle::Server + 'static,
    F: FnOnce(ChannelHandle) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    tokio::task::LocalSet::new()
        .run_until(async {
            let (server_io, client_io) = tokio::io::duplex(512);
            let raw: collab_capnp::channel_handle::Client = capnp_rpc::new_client(server);
            let task =
                tokio::task::spawn_local(runtime_portal::serve_connected_io(server_io, raw.client));
            let connection = runtime_portal::CapnpClient::from_connected_io(
                client_io,
                capnp_rpc::rpc_twoparty_capnp::Side::Client,
            );
            let channel = ChannelHandle {
                inner: Rc::new(connection.bootstrap()),
                descriptors: None,
            };
            tilo_test_env::within("the RPC under test", action(channel)).await;
            task.abort();
            let _ = task.await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_preserves_exact_revision_and_optional_argument_over_wire() {
    let received = Rc::new(RefCell::new(Vec::new()));
    over_wire(ResolvedServer(received.clone()), |channel| async move {
        for argument in [None, Some(""), Some("exact argument")] {
            assert_eq!(
                channel
                    .invoke_resolved_published_action(
                        42,
                        "revision-7",
                        "notes.block:p:b",
                        "open:hash",
                        argument,
                        "{\"approved\":true}"
                    )
                    .await
                    .unwrap(),
                (987, r#"{"state":"accepted"}"#.into())
            );
        }
    })
    .await;
    let received = received.borrow();
    assert_eq!(received.len(), 3);
    for (row, argument) in received
        .iter()
        .zip([None, Some(""), Some("exact argument")])
    {
        assert_eq!(
            *row,
            Received {
                peer: 42,
                revision: "revision-7".into(),
                resource: "notes.block:p:b".into(),
                action: "open:hash".into(),
                argument: argument.map(str::to_owned),
                confirmation: "{\"approved\":true}".into()
            }
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_accepts_exact_byte_limits() {
    let received = Rc::new(RefCell::new(Vec::new()));
    over_wire(ResolvedServer(received.clone()), |channel| async move {
        channel
            .invoke_resolved_published_action(
                9,
                &"é".repeat(256),
                &"r".repeat(512),
                &"a".repeat(512),
                Some(&"é".repeat(16 * 1024)),
                &" ".repeat(4 * 1024),
            )
            .await
            .unwrap();
    })
    .await;
    let rows = received.borrow();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].revision.len(), 512);
    assert_eq!(rows[0].resource.len(), 512);
    assert_eq!(rows[0].action.len(), 512);
    assert_eq!(rows[0].argument.as_ref().unwrap().len(), 32 * 1024);
    assert_eq!(rows[0].confirmation.len(), 4 * 1024);
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_refuses_invalid_input_before_rpc() {
    let received = Rc::new(RefCell::new(Vec::new()));
    over_wire(ResolvedServer(received.clone()), |channel| async move {
        for (field, invalid) in [
            (0, String::new()),
            (0, "x".repeat(513)),
            (0, "revision\0tail".into()),
            (1, String::new()),
            (1, "r".repeat(513)),
            (1, "r\n".into()),
            (1, "r\0".into()),
            (2, String::new()),
            (2, "a".repeat(513)),
            (2, "a\u{7f}".into()),
            (2, "a\0".into()),
            (3, "x".repeat(32769)),
            (3, "é".repeat(16385)),
            (3, "arg\0tail".into()),
            (4, "x".repeat(4097)),
            (4, "{}\0".into()),
        ] {
            let mut args = ["revision", "r", "a", "argument", "{}"];
            args[field] = &invalid;
            assert!(
                channel
                    .invoke_resolved_published_action(
                        9,
                        args[0],
                        args[1],
                        args[2],
                        Some(args[3]),
                        args[4]
                    )
                    .await
                    .is_err(),
                "invalid field {field} reached RPC"
            );
        }
    })
    .await;
    assert!(
        received.borrow().is_empty(),
        "rejected inputs were transmitted"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn resolved_invocation_old_server_is_unimplemented_without_eager_fallback() {
    let eager_calls = Rc::new(Cell::new(0));
    let seen = eager_calls.clone();
    over_wire(LegacyServer(eager_calls), |channel| async move {
        assert_eq!(channel.invoke_published_action(9, "r", "a", None, "{}").await.unwrap(), (21, String::new()));
        assert_eq!(seen.get(), 1);
        let error = channel.invoke_resolved_published_action(9, "revision", "r", "a", None, "{}").await.unwrap_err();
        assert!(matches!(error, NotAvailable::Capnp(error) if error.kind == capnp::ErrorKind::Unimplemented));
        assert_eq!(seen.get(), 1, "unsupported revisioned call fell back to eager invocation");
    }).await;
}

#[test]
fn resolved_invocation_keeps_legacy_eager_parameter_bytes() {
    // Literal pre-@47 @21 parameters: peer9, r, a, present empty argument,
    // confirmation {}. Two data words and four pointer words are unchanged.
    let golden: &[u8] = &[
        0, 0, 0, 0, 11, 0, 0, 0, 0, 0, 0, 0, 2, 0, 4, 0, 9, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0,
        0, 0, 13, 0, 0, 0, 18, 0, 0, 0, 13, 0, 0, 0, 18, 0, 0, 0, 13, 0, 0, 0, 10, 0, 0, 0, 13, 0,
        0, 0, 26, 0, 0, 0, b'r', 0, 0, 0, 0, 0, 0, 0, b'a', 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, b'{', b'}', 0, 0, 0, 0, 0, 0,
    ];
    let decoded =
        capnp::serialize::read_message(&mut &golden[..], capnp::message::ReaderOptions::default())
            .unwrap();
    let old = decoded
        .get_root::<collab_capnp::channel_handle::invoke_published_action_params::Reader<'_>>()
        .unwrap();
    assert_eq!(old.get_peer_id(), 9);
    assert_eq!(old.get_resource().unwrap().to_str().unwrap(), "r");
    assert_eq!(old.get_action().unwrap().to_str().unwrap(), "a");
    assert!(old.get_has_argument());
    assert_eq!(old.get_argument().unwrap().to_str().unwrap(), "");
    assert_eq!(old.get_confirmation_json().unwrap().to_str().unwrap(), "{}");
    let mut encoded = capnp::message::Builder::new_default();
    let mut p = encoded
        .init_root::<collab_capnp::channel_handle::invoke_published_action_params::Builder<'_>>();
    p.set_peer_id(9);
    p.set_resource("r");
    p.set_action("a");
    p.set_argument("");
    p.set_has_argument(true);
    p.set_confirmation_json("{}");
    assert_eq!(capnp::serialize::write_message_to_words(&encoded), golden);
}
