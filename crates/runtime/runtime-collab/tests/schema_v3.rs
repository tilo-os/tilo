//! Round-trips for the content, action and invocation verbs.
//!
//! These carry things the earlier verbs could not express — the bytes
//! behind a document's reference, an instruction addressed to another
//! app's document, and the operations one app exports to the session —
//! so each test drives the client wrapper against an in-memory broker and
//! asserts on what actually crossed the socket.
//!
//! Everything runs against an in-memory server over a bounded in-memory byte stream,
//! as in `schema_v2.rs`.

// capnp's generated `Server` traits declare each verb as `-> impl Future`,
// and these fakes return capnp's own `Promise`, which is the concrete type
// behind it. The lint's concern is a caller coming to depend on the narrower
// return; nothing outside this file constructs these servers, so the
// refinement stays where it is written.
#![allow(refining_impl_trait_internal)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::RefCell;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp::message::ReaderOptions;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio::task::LocalSet;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::{
    action_input, action_stream, blob_stream, channel_handle, collab_service, invocation_stream,
};
use runtime_collab::{CollabClient, CollabServiceClient, ExportedAction, ExportedInput};
use runtime_portal::CapnpClient;

// shared capture state

/// Everything the fake broker saw, in arrival order.
#[derive(Default)]
struct Captures {
    offered_blobs: Vec<(String, Vec<u8>)>,
    wanted_blobs: Vec<String>,
    sent_actions: Vec<(String, Vec<u8>)>,
    published: Vec<(String, Vec<PublishedAction>)>,
    completions: Vec<(u64, String, String)>,
    remarks: Vec<(String, (f32, f32, f32, f32))>,
}

/// One row decoded back out of a `publishActions` call, so a test can
/// assert on what the wire carried rather than on what it passed in.
#[derive(Debug, PartialEq, Eq)]
struct PublishedAction {
    resource: String,
    semantic_identity: Option<(String, String)>,
    action: String,
    resource_kind: String,
    resource_label: String,
    action_label: String,
    description: String,
    operation_kind: String,
    params_json: String,
    contract_json: String,
    input: PublishedInput,
}

#[derive(Debug, PartialEq, Eq)]
enum PublishedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        wants_content: bool,
    },
}

type SharedCaptures = Rc<RefCell<Captures>>;

/// Pre-seeded `(hash, bytes)` pairs shared between a `FakeChannel` and the
/// `FakeBlobStream` it hands out.
type BlobBacklog = Rc<RefCell<Vec<(String, Vec<u8>)>>>;

/// Pre-seeded `(type_id, payload, from_user_id)` triples shared between a
/// `FakeChannel` and the `FakeActionStream` it hands out.
type ActionBacklog = Rc<RefCell<Vec<(String, Vec<u8>, String)>>>;

// fake streams

struct FakeBlobStream {
    backlog: BlobBacklog,
}

impl blob_stream::Server for FakeBlobStream {
    fn read(
        self: Rc<Self>,
        _params: blob_stream::ReadParams,
        mut results: blob_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("blob backlog exhausted".into()));
        }
        let (hash, bytes) = backlog.remove(0);
        let mut r = results.get();
        r.set_hash(&hash);
        r.set_bytes(&bytes);
        Promise::ok(())
    }
}

struct FakeActionStream {
    backlog: ActionBacklog,
}

impl action_stream::Server for FakeActionStream {
    fn read(
        self: Rc<Self>,
        _params: action_stream::ReadParams,
        mut results: action_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("action backlog exhausted".into()));
        }
        let (type_id, payload, from) = backlog.remove(0);
        let mut r = results.get();
        r.set_type_id(&type_id);
        r.set_payload(&payload);
        r.set_from_user_id(&from);
        Promise::ok(())
    }
}

/// A seeded invocation. `argument` is `None` for an action that takes
/// nothing, which the wire states as `hasArgument = false`.
#[derive(Clone)]
struct SeedInvocation {
    invocation_uuid: [u8; 16],
    id: u64,
    resource: String,
    action: String,
    argument: Option<String>,
    actor: String,
}

struct FakeInvocationStream {
    backlog: Rc<RefCell<Vec<SeedInvocation>>>,
}

impl invocation_stream::Server for FakeInvocationStream {
    fn read(
        self: Rc<Self>,
        _params: invocation_stream::ReadParams,
        mut results: invocation_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("invocation backlog exhausted".into()));
        }
        let seed = backlog.remove(0);
        let mut r = results.get();
        r.set_id(seed.id);
        r.set_invocation_uuid(&seed.invocation_uuid);
        r.set_resource(&seed.resource);
        r.set_action(&seed.action);
        r.set_actor(&seed.actor);
        if let Some(a) = &seed.argument {
            r.set_has_argument(true);
            r.set_argument(a);
        } else {
            r.set_has_argument(false);
            r.set_argument("");
        }
        Promise::ok(())
    }
}

// fake channel

struct FakeChannel {
    captures: SharedCaptures,
    blob_backlog: BlobBacklog,
    action_backlog: ActionBacklog,
    invocation_backlog: Rc<RefCell<Vec<SeedInvocation>>>,
}

impl channel_handle::Server for FakeChannel {
    fn offer_blob(
        self: Rc<Self>,
        params: channel_handle::OfferBlobParams,
        _results: channel_handle::OfferBlobResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let hash = r.get_hash()?.to_str()?.to_owned();
            let bytes = r.get_bytes()?.to_vec();
            captures.borrow_mut().offered_blobs.push((hash, bytes));
            Ok(())
        })
    }

    fn want_blob(
        self: Rc<Self>,
        params: channel_handle::WantBlobParams,
        _results: channel_handle::WantBlobResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let hash = params.get()?.get_hash()?.to_str()?.to_owned();
            captures.borrow_mut().wanted_blobs.push(hash);
            Ok(())
        })
    }

    fn subscribe_blobs(
        self: Rc<Self>,
        _params: channel_handle::SubscribeBlobsParams,
        mut results: channel_handle::SubscribeBlobsResults,
    ) -> Promise<(), capnp::Error> {
        let client: blob_stream::Client = capnp_rpc::new_client(FakeBlobStream {
            backlog: self.blob_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn send_action(
        self: Rc<Self>,
        params: channel_handle::SendActionParams,
        _results: channel_handle::SendActionResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let type_id = r.get_type_id()?.to_str()?.to_owned();
            let payload = r.get_payload()?.to_vec();
            captures.borrow_mut().sent_actions.push((type_id, payload));
            Ok(())
        })
    }

    fn subscribe_actions(
        self: Rc<Self>,
        _params: channel_handle::SubscribeActionsParams,
        mut results: channel_handle::SubscribeActionsResults,
    ) -> Promise<(), capnp::Error> {
        let client: action_stream::Client = capnp_rpc::new_client(FakeActionStream {
            backlog: self.action_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn publish_actions(
        self: Rc<Self>,
        params: channel_handle::PublishActionsParams,
        _results: channel_handle::PublishActionsResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let app_id = r.get_app_id()?.to_str()?.to_owned();
            let list = r.get_actions()?;
            let mut rows = Vec::with_capacity(list.len() as usize);
            for i in 0..list.len() {
                let a = list.get(i);
                let input = match a.get_input()?.which()? {
                    action_input::Which::None(()) => PublishedInput::None,
                    action_input::Which::Text(t) => PublishedInput::Text {
                        prompt: t.get_prompt()?.to_str()?.to_owned(),
                        placeholder: t.get_placeholder()?.to_str()?.to_owned(),
                        wants_content: t.get_wants_content(),
                    },
                };
                rows.push(PublishedAction {
                    resource: a.get_resource()?.to_str()?.to_owned(),
                    semantic_identity: if a.has_semantic_identity() {
                        let identity = a.get_semantic_identity()?;
                        Some((
                            identity.get_kind()?.to_str()?.to_owned(),
                            identity.get_key()?.to_str()?.to_owned(),
                        ))
                    } else {
                        None
                    },
                    action: a.get_action()?.to_str()?.to_owned(),
                    resource_kind: a.get_resource_kind()?.to_str()?.to_owned(),
                    resource_label: a.get_resource_label()?.to_str()?.to_owned(),
                    action_label: a.get_action_label()?.to_str()?.to_owned(),
                    description: a.get_description()?.to_str()?.to_owned(),
                    operation_kind: a.get_operation_kind()?.to_str()?.to_owned(),
                    params_json: a.get_params_json()?.to_str()?.to_owned(),
                    contract_json: a.get_contract_json()?.to_str()?.to_owned(),
                    input,
                });
            }
            captures.borrow_mut().published.push((app_id, rows));
            Ok(())
        })
    }

    fn subscribe_invocations(
        self: Rc<Self>,
        _params: channel_handle::SubscribeInvocationsParams,
        mut results: channel_handle::SubscribeInvocationsResults,
    ) -> Promise<(), capnp::Error> {
        let client: invocation_stream::Client = capnp_rpc::new_client(FakeInvocationStream {
            backlog: self.invocation_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn complete_invocation(
        self: Rc<Self>,
        params: channel_handle::CompleteInvocationParams,
        _results: channel_handle::CompleteInvocationResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let code = r.get_code()?.to_str()?.to_owned();
            let message = r.get_message()?.to_str()?.to_owned();
            captures
                .borrow_mut()
                .completions
                .push((r.get_id(), code, message));
            Ok(())
        })
    }

    fn compose_remark(
        self: Rc<Self>,
        params: channel_handle::ComposeRemarkParams,
        _results: channel_handle::ComposeRemarkResults,
    ) -> Promise<(), capnp::Error> {
        let captures = self.captures.clone();
        Promise::from_future(async move {
            let r = params.get()?;
            let reference = r.get_reference()?.to_str()?.to_owned();
            let rect = (r.get_x(), r.get_y(), r.get_w(), r.get_h());
            captures.borrow_mut().remarks.push((reference, rect));
            Ok(())
        })
    }
}

// fake broker

#[derive(Default)]
struct FakeBroker {
    captures: SharedCaptures,
    blob_backlog: Vec<(String, Vec<u8>)>,
    action_backlog: Vec<(String, Vec<u8>, String)>,
    invocation_backlog: Vec<SeedInvocation>,
}

impl collab_service::Server for FakeBroker {
    fn join_channel(
        self: Rc<Self>,
        _params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let chan = FakeChannel {
            captures: self.captures.clone(),
            blob_backlog: Rc::new(RefCell::new(self.blob_backlog.clone())),
            action_backlog: Rc::new(RefCell::new(self.action_backlog.clone())),
            invocation_backlog: Rc::new(RefCell::new(self.invocation_backlog.clone())),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(chan);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

fn stand_up_client(broker: FakeBroker) -> CollabClient {
    let (server_side, client_side) = tokio::io::duplex(512);

    let broker_client: collab_service::Client = capnp_rpc::new_client(broker);
    let (server_read, server_write) = tokio::io::split(server_side);
    let server_network = Box::new(twoparty::VatNetwork::new(
        server_read.compat(),
        server_write.compat_write(),
        rpc_twoparty_capnp::Side::Server,
        ReaderOptions::default(),
    ));
    let server_rpc = RpcSystem::new(server_network, Some(broker_client.client));
    tokio::task::spawn_local(async move {
        let _ = server_rpc.await;
    });

    let connection = CapnpClient::from_connected_io(client_side, rpc_twoparty_capnp::Side::Client);
    let client: CollabServiceClient = connection.bootstrap();
    CollabClient::from_raw(client)
}

// tests

#[tokio::test(flavor = "current_thread")]
async fn blob_offers_wants_and_streams_reach_the_broker_verbatim() {
    // phase: offer_and_want_blob_reach_the_broker_verbatim
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-blob", b"identity").await.unwrap();

                channel
                    .offer_blob("b3-abc", b"\x00\x01\x02binary")
                    .await
                    .unwrap();
                channel.want_blob("b3-def").await.unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.offered_blobs,
                    vec![("b3-abc".to_string(), b"\x00\x01\x02binary".to_vec())]
                );
                assert_eq!(c.wanted_blobs, vec!["b3-def".to_string()]);
            })
            .await;
    }
    // phase: subscribe_blobs_yields_hash_and_bytes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    blob_backlog: vec![
                        ("b3-one".into(), b"first".to_vec()),
                        ("b3-two".into(), vec![0xff, 0x00, 0xff]),
                    ],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-blob-rx", b"identity")
                    .await
                    .unwrap();
                let stream = channel.subscribe_blobs().await.unwrap();

                let first = stream.read().await.unwrap();
                assert_eq!(first.hash, "b3-one");
                assert_eq!(first.bytes, b"first");

                // Non-UTF8 content survives: `bytes` is Data, and the only
                // text on this stream is the hash.
                let second = stream.read().await.unwrap();
                assert_eq!(second.hash, "b3-two");
                assert_eq!(second.bytes, vec![0xff, 0x00, 0xff]);
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
async fn actions_and_remarks_carry_type_attribution_and_opaque_payloads() {
    // phase: send_action_carries_type_id_and_opaque_payload
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-act", b"identity").await.unwrap();

                channel
                    .send_action("word-processor.splice", b"\x01\x02opaque")
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.sent_actions,
                    vec![(
                        "word-processor.splice".to_string(),
                        b"\x01\x02opaque".to_vec()
                    )]
                );
            })
            .await;
    }
    // phase: subscribe_actions_keeps_sender_attribution_separate_from_payload
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    action_backlog: vec![(
                        "chat.remark".into(),
                        b"payload".to_vec(),
                        "peer-3".into(),
                    )],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-act-rx", b"identity")
                    .await
                    .unwrap();
                let stream = channel.subscribe_actions().await.unwrap();

                let ev = stream.read().await.unwrap();
                assert_eq!(ev.type_id, "chat.remark");
                assert_eq!(ev.payload, b"payload");
                // The broker's claim about who sent the frame, which the
                // payload never gets to overwrite.
                assert_eq!(ev.from_user_id, "peer-3");
            })
            .await;
    }
    // phase: publish_actions_round_trips_both_input_shapes
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-pub", b"identity").await.unwrap();

                channel
                    .publish_actions(
                        "word-processor",
                        &[
                            ExportedAction {
                                semantic_identity: Some(
                                    runtime_collab::ExportedResourceSemanticIdentity {
                                        kind: "document".into(),
                                        key: "source-document:plan".into(),
                                    },
                                ),
                                resource: "word-processor.document".into(),
                                action: "save".into(),
                                resource_kind: "document".into(),
                                resource_label: "Plan".into(),
                                action_label: "Save".into(),
                                description: None,
                                operation_kind: tilo_actions::OperationKind::Effect,
                                params: Vec::new(),
                                contract: tilo_actions::ActionContract::default(),
                                input: ExportedInput::None,
                                view: None,
                            },
                            ExportedAction {
                                semantic_identity: None,
                                resource: "word-processor.document".into(),
                                action: "insert-image".into(),
                                resource_kind: "document".into(),
                                resource_label: "Plan".into(),
                                action_label: "Insert image".into(),
                                description: Some("Place an image at the caret".into()),
                                operation_kind: tilo_actions::OperationKind::Document,
                                params: Vec::new(),
                                contract: tilo_actions::ActionContract::default(),
                                input: ExportedInput::Text {
                                    prompt: "Which image?".into(),
                                    placeholder: "content hash".into(),
                                    wants_content: true,
                                },
                                view: None,
                            },
                        ],
                    )
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(c.published.len(), 1);
                let (app_id, rows) = &c.published[0];
                assert_eq!(app_id, "word-processor");
                assert_eq!(rows.len(), 2);

                assert_eq!(rows[0].action, "save");
                assert_eq!(rows[0].resource, "word-processor.document");
                assert_eq!(
                    rows[0].semantic_identity,
                    Some(("document".into(), "source-document:plan".into()))
                );
                assert!(rows[1].semantic_identity.is_none());
                // An absent description and an empty one are the same fact,
                // so `None` has to arrive as the empty string.
                assert_eq!(rows[0].description, "");
                assert_eq!(rows[0].operation_kind, "effect");
                assert_eq!(rows[0].params_json, "[]");
                assert_eq!(
                    serde_json::from_str::<tilo_actions::ActionContract>(&rows[0].contract_json)
                        .unwrap(),
                    tilo_actions::ActionContract::default()
                );
                assert_eq!(rows[0].input, PublishedInput::None);

                assert_eq!(rows[1].action, "insert-image");
                assert_eq!(rows[1].resource_kind, "document");
                assert_eq!(rows[1].action_label, "Insert image");
                assert_eq!(rows[1].description, "Place an image at the caret");
                assert_eq!(rows[1].operation_kind, "document");
                assert_eq!(rows[1].params_json, "[]");
                assert_eq!(
                    rows[1].input,
                    PublishedInput::Text {
                        prompt: "Which image?".into(),
                        placeholder: "content hash".into(),
                        wants_content: true,
                    }
                );
            })
            .await;
    }
    // phase: compose_remark_carries_the_reference_and_the_window_local_rect
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab
                    .join_channel("chan-remark", b"identity")
                    .await
                    .unwrap();

                let reference = r#"{"referent":{"Doc":{"id":"plan"}}}"#;
                channel
                    .compose_remark(reference, (12.0, 34.0, 200.0, 24.0))
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(c.remarks.len(), 1);
                assert_eq!(c.remarks[0].0, reference);
                assert_eq!(c.remarks[0].1, (12.0, 34.0, 200.0, 24.0));
            })
            .await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn invocations_distinguish_empty_arguments_and_report_every_outcome() {
    // phase: invocation_distinguishes_an_empty_argument_from_no_argument
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let broker = FakeBroker {
                    invocation_backlog: vec![
                        SeedInvocation {
                            invocation_uuid: [0xa5; 16],
                            id: 1,
                            resource: "word-processor.document".into(),
                            action: "save".into(),
                            argument: None,
                            actor: "alice".into(),
                        },
                        SeedInvocation {
                            invocation_uuid: [0x5a; 16],
                            id: 2,
                            resource: "word-processor.document".into(),
                            action: "set-title".into(),
                            argument: Some(String::new()),
                            actor: "agent-7".into(),
                        },
                    ],
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-inv", b"identity").await.unwrap();
                let stream = channel.subscribe_invocations().await.unwrap();

                let first = stream.read().await.unwrap();
                assert_eq!(first.id, 1);
                assert_eq!(
                    first.invocation_uuid, [0xa5; 16],
                    "high UUID bits survive native SDK delivery"
                );
                assert_eq!(first.action, "save");
                assert_eq!(first.actor, "alice");
                assert_eq!(first.argument, None, "an action that takes nothing");

                // The pair of fields exists for exactly this case: an action legitimately
                // given `""` must not read as one given no argument at all.
                let second = stream.read().await.unwrap();
                assert_eq!(second.id, 2);
                assert_eq!(second.invocation_uuid, [0x5a; 16]);
                assert_eq!(second.argument, Some(String::new()));
                assert_eq!(second.actor, "agent-7");
            })
            .await;
    }
    // phase: complete_invocation_reports_both_success_and_refusal
    {
        let local = LocalSet::new();
        local
            .run_until(async {
                let captures: SharedCaptures = Rc::new(RefCell::new(Captures::default()));
                let broker = FakeBroker {
                    captures: captures.clone(),
                    ..Default::default()
                };
                let collab = stand_up_client(broker);
                let channel = collab.join_channel("chan-done", b"identity").await.unwrap();

                channel.complete_invocation(1, "", "").await.unwrap();
                channel
                    .complete_invocation(2, "unknown_action", "no such action on this resource")
                    .await
                    .unwrap();

                let c = captures.borrow();
                assert_eq!(
                    c.completions,
                    vec![
                        // Empty code is the "it ran" answer.
                        (1, String::new(), String::new()),
                        (
                            2,
                            "unknown_action".to_string(),
                            "no such action on this resource".to_string()
                        ),
                    ]
                );
            })
            .await;
    }
}
