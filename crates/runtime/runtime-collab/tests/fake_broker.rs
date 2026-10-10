//! One in-memory `CollabService` the integration tests talk to over a real
//! Cap'n Proto connection.
//!
//! Every `join_channel` gets a fresh `ChannelHandle` seeded from the
//! broker's backlogs; every stream pops its backlog in order and fails once
//! it is exhausted. Whatever the client sends lands in [`Captures`], in
//! arrival order, for the test to assert on.

// capnp's generated `Server` traits declare each verb as `-> impl Future`,
// and these fakes return capnp's own `Promise`, which is the concrete type
// behind it. The lint's concern is a caller coming to depend on the narrower
// return; nothing outside this test binary constructs these servers, so the
// refinement stays where it is written.
#![allow(refining_impl_trait)]

use std::cell::RefCell;
use std::rc::Rc;

use capnp::capability::Promise;
use capnp::message::ReaderOptions;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use runtime_collab::raw::{
    action_input, action_stream, blob_stream, channel_context, channel_handle, collab_service,
    context_stream, invocation_stream, presence_event, presence_handle, presence_payload,
    presence_stream, scrollback_batch, stream,
};
use runtime_collab::{CollabClient, CollabServiceClient};
use runtime_portal::CapnpClient;

/// Everything the fake broker saw, in arrival order.
#[derive(Default)]
pub struct Captures {
    /// `(channel_id, identity bytes)` from every `join_channel`.
    pub joins: Vec<(String, Vec<u8>)>,
    pub sent_ops: Vec<Vec<u8>>,
    pub presence_pointer: Vec<(Vec<u8>, f32, f32)>,
    pub presence_caret: Vec<(Vec<u8>, u64)>,
    pub presence_selection: Vec<(Vec<u8>, u64, u64)>,
    pub presence_typing: Vec<(Vec<u8>, u64, u64)>,
    pub presence_arrived: Vec<Vec<u8>>,
    pub presence_left: Vec<Vec<u8>>,
    pub presence_cursor: Vec<(f32, f32)>,
    pub presence_status: Vec<(Vec<u8>, String, bool)>,
    pub scrollback_requests: Vec<(Vec<u8>, u64, u32)>,
    pub offered_blobs: Vec<(String, Vec<u8>)>,
    pub wanted_blobs: Vec<String>,
    pub sent_actions: Vec<(String, Vec<u8>)>,
    pub published: Vec<(String, Vec<PublishedAction>)>,
    pub completions: Vec<(u64, String, String)>,
    pub remarks: Vec<(String, (f32, f32, f32, f32))>,
}

pub type SharedCaptures = Rc<RefCell<Captures>>;

/// What each joined channel hands out.
#[derive(Default)]
pub struct FakeBroker {
    pub captures: SharedCaptures,
    /// `(op bytes, from-user attribution)` pairs the op stream yields.
    pub op_backlog: Vec<(Vec<u8>, String)>,
    pub presence_backlog: Vec<SeedPresence>,
    pub context_backlog: Vec<SeedContext>,
    pub scrollback_response: Option<SeedScrollback>,
    /// `(hash, bytes)` pairs the blob stream yields.
    pub blob_backlog: Vec<(String, Vec<u8>)>,
    /// `(type_id, payload, from_user_id)` triples the action stream yields.
    pub action_backlog: Vec<(String, Vec<u8>, String)>,
    pub invocation_backlog: Vec<SeedInvocation>,
}

type Backlog<T> = Rc<RefCell<Vec<T>>>;

fn backlog<T: Clone>(seed: &[T]) -> Backlog<T> {
    Rc::new(RefCell::new(seed.to_vec()))
}

impl collab_service::Server for FakeBroker {
    fn join_channel(
        self: Rc<Self>,
        params: collab_service::JoinChannelParams,
        mut results: collab_service::JoinChannelResults,
    ) -> Promise<(), capnp::Error> {
        let join = params.get().and_then(|reader| {
            let channel = reader.get_channel_id()?.to_str()?.to_owned();
            Ok((channel, reader.get_identity()?.to_vec()))
        });
        match join {
            Ok(join) => self.captures.borrow_mut().joins.push(join),
            Err(e) => return Promise::err(e),
        }
        let chan = FakeChannel {
            captures: self.captures.clone(),
            op_backlog: backlog(&self.op_backlog),
            presence_backlog: backlog(&self.presence_backlog),
            context_backlog: backlog(&self.context_backlog),
            scrollback_response: self.scrollback_response.clone(),
            blob_backlog: backlog(&self.blob_backlog),
            action_backlog: backlog(&self.action_backlog),
            invocation_backlog: backlog(&self.invocation_backlog),
        };
        let client: channel_handle::Client = capnp_rpc::new_client(chan);
        results.get().set_handle(client);
        Promise::ok(())
    }
}

/// Serve `broker` on `server_side` from the current `LocalSet`.
pub fn serve<S>(broker: FakeBroker, server_side: S)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + 'static,
{
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
}

/// A typed client connected to `broker` over an in-memory byte stream.
pub fn stand_up_client(broker: FakeBroker) -> CollabClient {
    let (server_side, client_side) = tokio::io::duplex(512);
    serve(broker, server_side);
    let connection = CapnpClient::from_connected_io(client_side, rpc_twoparty_capnp::Side::Client);
    let client: CollabServiceClient = connection.bootstrap();
    CollabClient::from_raw(client)
}

struct FakeChannel {
    captures: SharedCaptures,
    op_backlog: Backlog<(Vec<u8>, String)>,
    presence_backlog: Backlog<SeedPresence>,
    context_backlog: Backlog<SeedContext>,
    scrollback_response: Option<SeedScrollback>,
    blob_backlog: Backlog<(String, Vec<u8>)>,
    action_backlog: Backlog<(String, Vec<u8>, String)>,
    invocation_backlog: Backlog<SeedInvocation>,
}

impl channel_handle::Server for FakeChannel {
    fn send(
        self: Rc<Self>,
        params: channel_handle::SendParams,
        _results: channel_handle::SendResults,
    ) -> Promise<(), capnp::Error> {
        let op = match params
            .get()
            .and_then(channel_handle::send_params::Reader::get_op)
        {
            Ok(op) => op.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().sent_ops.push(op);
        Promise::ok(())
    }

    fn subscribe(
        self: Rc<Self>,
        _params: channel_handle::SubscribeParams,
        mut results: channel_handle::SubscribeResults,
    ) -> Promise<(), capnp::Error> {
        let client: stream::Client = capnp_rpc::new_client(FakeOpStream {
            backlog: self.op_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn presence(
        self: Rc<Self>,
        _params: channel_handle::PresenceParams,
        mut results: channel_handle::PresenceResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_handle::Client = capnp_rpc::new_client(FakePresence {
            captures: self.captures.clone(),
            presence_backlog: self.presence_backlog.clone(),
        });
        results.get().set_handle(client);
        Promise::ok(())
    }

    fn retired_unsigned_snapshot(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedSnapshotParams,
        _results: channel_handle::RetiredUnsignedSnapshotResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn retired_unsigned_incremental(
        self: Rc<Self>,
        _params: channel_handle::RetiredUnsignedIncrementalParams,
        _results: channel_handle::RetiredUnsignedIncrementalResults,
    ) -> Promise<(), capnp::Error> {
        Promise::ok(())
    }

    fn subscribe_context(
        self: Rc<Self>,
        _params: channel_handle::SubscribeContextParams,
        mut results: channel_handle::SubscribeContextResults,
    ) -> Promise<(), capnp::Error> {
        let client: context_stream::Client = capnp_rpc::new_client(FakeContextStream {
            backlog: self.context_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }

    fn fetch_scrollback_for(
        self: Rc<Self>,
        params: channel_handle::FetchScrollbackForParams,
        mut results: channel_handle::FetchScrollbackForResults,
    ) -> Promise<(), capnp::Error> {
        let p = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().scrollback_requests.push((
            p.get_artifact_id()
                .map_or_else(|_| Vec::new(), <[u8]>::to_vec),
            p.get_before_seq(),
            p.get_max(),
        ));
        let seed = self.scrollback_response.clone().unwrap_or(SeedScrollback {
            ops: Vec::new(),
            earliest_seq: 0,
            latest_seq: 0,
            done: true,
        });
        let mut batch: scrollback_batch::Builder = results.get().init_batch();
        {
            let mut ops = batch
                .reborrow()
                .init_ops(u32::try_from(seed.ops.len()).unwrap());
            for (i, op) in seed.ops.iter().enumerate() {
                ops.set(u32::try_from(i).unwrap(), op);
            }
        }
        batch.set_earliest_seq(seed.earliest_seq);
        batch.set_latest_seq(seed.latest_seq);
        batch.set_done(seed.done);
        Promise::ok(())
    }

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

/// Pre-seeded events; each `read()` pops one, as a (user, `key_bytes`,
/// payload) triple encoded via the `PresencePayloadKind` helper below.
#[derive(Clone)]
pub struct SeedPresence {
    pub user_id: String,
    pub key_bytes: Vec<u8>,
    pub payload: PresencePayloadKind,
}

#[derive(Clone)]
pub enum PresencePayloadKind {
    Cursor { x: f32, y: f32 },
    Pointer { x: f32, y: f32 },
    Caret { pos: u64 },
    Selection { start: u64, end: u64 },
    Typing { start: u64, end: u64 },
    Status { text: String, automatic: bool },
    Arrived,
    Left,
}

struct FakePresenceStream {
    backlog: Rc<RefCell<Vec<SeedPresence>>>,
}

impl presence_stream::Server for FakePresenceStream {
    fn read(
        self: Rc<Self>,
        _params: presence_stream::ReadParams,
        mut results: presence_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("presence backlog exhausted".into()));
        }
        let seed = backlog.remove(0);
        let mut ev: presence_event::Builder = results.get().init_event();
        ev.set_user_id(&seed.user_id);
        ev.set_key_bytes(&seed.key_bytes);
        let mut payload: presence_payload::Builder = ev.init_payload();
        match seed.payload {
            PresencePayloadKind::Cursor { x, y } => {
                let mut b = payload.init_cursor();
                b.set_x(x);
                b.set_y(y);
            }
            PresencePayloadKind::Pointer { x, y } => {
                let mut b = payload.init_pointer();
                b.set_x(x);
                b.set_y(y);
            }
            PresencePayloadKind::Caret { pos } => {
                payload.init_caret().set_pos(pos);
            }
            PresencePayloadKind::Selection { start, end } => {
                let mut b = payload.init_selection();
                b.set_start(start);
                b.set_end(end);
            }
            PresencePayloadKind::Typing { start, end } => {
                let mut b = payload.init_typing();
                b.set_start(start);
                b.set_end(end);
            }
            PresencePayloadKind::Status { text, automatic } => {
                let mut b = payload.init_status();
                b.set_text(&text);
                b.set_automatic(automatic);
            }
            PresencePayloadKind::Arrived => payload.set_arrived(()),
            PresencePayloadKind::Left => payload.set_left(()),
        }
        Promise::ok(())
    }
}

// fake presence handle

struct FakePresence {
    captures: SharedCaptures,
    presence_backlog: Rc<RefCell<Vec<SeedPresence>>>,
}

impl presence_handle::Server for FakePresence {
    fn publish_cursor(
        self: Rc<Self>,
        params: presence_handle::PublishCursorParams,
        _results: presence_handle::PublishCursorResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_cursor
            .push((r.get_x(), r.get_y()));
        Promise::ok(())
    }

    fn publish_pointer(
        self: Rc<Self>,
        params: presence_handle::PublishPointerParams,
        _results: presence_handle::PublishPointerResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_pointer
            .push((key, r.get_x(), r.get_y()));
        Promise::ok(())
    }

    fn publish_caret(
        self: Rc<Self>,
        params: presence_handle::PublishCaretParams,
        _results: presence_handle::PublishCaretResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_caret
            .push((key, r.get_pos()));
        Promise::ok(())
    }

    fn publish_selection(
        self: Rc<Self>,
        params: presence_handle::PublishSelectionParams,
        _results: presence_handle::PublishSelectionResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_selection
            .push((key, r.get_start(), r.get_end()));
        Promise::ok(())
    }

    fn publish_typing(
        self: Rc<Self>,
        params: presence_handle::PublishTypingParams,
        _results: presence_handle::PublishTypingResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures
            .borrow_mut()
            .presence_typing
            .push((key, r.get_start(), r.get_end()));
        Promise::ok(())
    }

    fn publish_arrived(
        self: Rc<Self>,
        params: presence_handle::PublishArrivedParams,
        _results: presence_handle::PublishArrivedResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().presence_arrived.push(key);
        Promise::ok(())
    }

    fn publish_left(
        self: Rc<Self>,
        params: presence_handle::PublishLeftParams,
        _results: presence_handle::PublishLeftResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        self.captures.borrow_mut().presence_left.push(key);
        Promise::ok(())
    }

    fn publish_status(
        self: Rc<Self>,
        params: presence_handle::PublishStatusParams,
        _results: presence_handle::PublishStatusResults,
    ) -> Promise<(), capnp::Error> {
        let r = match params.get() {
            Ok(r) => r,
            Err(e) => return Promise::err(e),
        };
        let key = match r.get_key_bytes() {
            Ok(b) => b.to_vec(),
            Err(e) => return Promise::err(e),
        };
        let text_reader = match r.get_text() {
            Ok(text) => text,
            Err(e) => return Promise::err(e),
        };
        let text = match text_reader.to_str() {
            Ok(text) => text.to_owned(),
            Err(e) => return Promise::err(capnp::Error::failed(e.to_string())),
        };
        self.captures
            .borrow_mut()
            .presence_status
            .push((key, text, r.get_automatic()));
        Promise::ok(())
    }

    fn subscribe_all(
        self: Rc<Self>,
        _params: presence_handle::SubscribeAllParams,
        mut results: presence_handle::SubscribeAllResults,
    ) -> Promise<(), capnp::Error> {
        let client: presence_stream::Client = capnp_rpc::new_client(FakePresenceStream {
            backlog: self.presence_backlog.clone(),
        });
        results.get().set_stream(client);
        Promise::ok(())
    }
}

struct FakeOpStream {
    backlog: Backlog<(Vec<u8>, String)>,
}

impl stream::Server for FakeOpStream {
    fn read(
        self: Rc<Self>,
        _params: stream::ReadParams,
        mut results: stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("op backlog exhausted".into()));
        }
        let (op, from) = backlog.remove(0);
        let mut r = results.get();
        r.set_op(&op);
        r.set_from_user_id(&from);
        Promise::ok(())
    }
}

/// A seeded `ChannelContext` snapshot to emit on the next `read()`, kept as
/// owned data so the test can pass typed values in.
#[derive(Clone)]
pub struct SeedContext {
    pub collaborators: Vec<(String, String, u32)>,
    pub permissions: Vec<String>,
}

struct FakeContextStream {
    backlog: Rc<RefCell<Vec<SeedContext>>>,
}

impl context_stream::Server for FakeContextStream {
    fn read(
        self: Rc<Self>,
        _params: context_stream::ReadParams,
        mut results: context_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        let mut backlog = self.backlog.borrow_mut();
        if backlog.is_empty() {
            return Promise::err(capnp::Error::failed("context backlog exhausted".into()));
        }
        let seed = backlog.remove(0);
        let mut ctx: channel_context::Builder = results.get().init_ctx();
        {
            let mut collabs = ctx
                .reborrow()
                .init_collaborators(u32::try_from(seed.collaborators.len()).unwrap());
            for (i, (uid, name, color)) in seed.collaborators.iter().enumerate() {
                let mut c = collabs.reborrow().get(u32::try_from(i).unwrap());
                c.set_user_id(uid);
                c.set_display_name(name);
                c.set_color(*color);
            }
        }
        {
            let mut perms = ctx.init_permissions(u32::try_from(seed.permissions.len()).unwrap());
            for (i, p) in seed.permissions.iter().enumerate() {
                perms.set(u32::try_from(i).unwrap(), p.as_str());
            }
        }
        Promise::ok(())
    }
}

#[derive(Clone)]
pub struct SeedScrollback {
    pub ops: Vec<Vec<u8>>,
    pub earliest_seq: u64,
    pub latest_seq: u64,
    pub done: bool,
}

/// One row decoded back out of a `publishActions` call, so a test can
/// assert on what the wire carried rather than on what it passed in.
#[derive(Debug, PartialEq, Eq)]
pub struct PublishedAction {
    pub resource: String,
    pub semantic_identity: Option<(String, String)>,
    pub action: String,
    pub resource_kind: String,
    pub resource_label: String,
    pub action_label: String,
    pub description: String,
    pub operation_kind: String,
    pub params_json: String,
    pub contract_json: String,
    pub input: PublishedInput,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PublishedInput {
    None,
    Text {
        prompt: String,
        placeholder: String,
        wants_content: bool,
    },
}

struct FakeBlobStream {
    backlog: Backlog<(String, Vec<u8>)>,
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
    backlog: Backlog<(String, Vec<u8>, String)>,
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
pub struct SeedInvocation {
    pub invocation_uuid: [u8; 16],
    pub id: u64,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
    pub actor: String,
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
