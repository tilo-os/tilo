//! A launched app's one connection, split between the device's daemon and this host.
//!
//! An app the shell launched speaks to this host on one connection. Its documents are the
//! person's workspaces', which the device's daemon keeps; what only this host serves (the
//! person's settings, the published resources Quick Settings changes, the drives and apps Files
//! lists, audio, calls, the person's devices, search) stays here. So each such connection is
//! joined at the daemon too, on a connection the shell vouches for on its own link to the daemon
//! ([`runtime_collab::ShellClient::vouched`]), and every call goes to the side that answers it:
//! the channel's presence, document, content, history, context, resources and committed reads to
//! the daemon, everything else here. A channel the app joins is joined on both sides, and is the
//! app's only once both took it: a shell without a daemon serves no app's documents, and its
//! apps' joins are refused saying why.
//!
//! The app sees one host: the handles it has are this router's, mapped to each side's own, a
//! subscription keeps the call id the app gave it, and an ask either side puts to the app carries an
//! id of the router's, mapped back when the app answers.

use std::collections::HashMap;

use runtime_collab::wire::shell::Vouch;
use runtime_collab::wire::{
    Ask, Call, Changes, ContextUpdate, Event, FRAMING, Handle, HostFrame, Operation, Publication,
    Reply, Service, Topic,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::mpsc;

/// What an app is told of a call the daemon was to answer, once the daemon hung up.
const DAEMON_ENDED: &str = "the device's daemon closed the app's connection";

/// The daemon's connection for `app`, an app this host admitted by its launch, where `services`
/// name the shell's link to the daemon, or why there is none: `None` for any other connection,
/// and on the daemon's own host, which routes nothing.
pub(super) async fn daemon_for(
    services: &crate::apps::services::ProcessServices,
    app: Option<&super::native_media::LocalAppConnection>,
) -> Option<Result<tokio::net::UnixStream, String>> {
    let link = services
        .device_daemon
        .as_ref()
        .filter(|_| services.shell.is_none())?;
    let app = app?;
    let vouch = Vouch {
        app: app.app_id.clone(),
        workspace: app.workspace_id.clone(),
        window: app.continuation_instance,
        windowless: app.windowless,
    };
    let link = match link {
        Ok(link) => link,
        Err(reason) => return Some(Err(reason.clone())),
    };
    Some(link.vouched(vouch).await.map_err(|error| {
        let error = format!("the device's daemon, which keeps this app's documents: {error}");
        tracing::error!(app = %app.app_id, %error, "an app was not joined at the daemon");
        error
    }))
}

/// Serve the app on `stream` through this host's `connection` and the `daemon`'s, `received`
/// keeping the objects Files hands over on its own connection. Without a `daemon`, each join
/// the app asks for is refused with why there is none.
#[allow(
    clippy::future_not_send,
    reason = "a connection is served on the LocalSet, as every other is"
)]
pub(super) async fn serve_routed(
    connection: std::rc::Rc<super::wire::WireConnection>,
    outgoing: mpsc::UnboundedReceiver<HostFrame>,
    stream: tokio::net::UnixStream,
    received: Option<runtime_portal::descriptors::ReceivedDescriptors>,
    daemon: Result<tokio::net::UnixStream, String>,
) -> Result<(), crate::apps::BrokerError> {
    let (local, routed) = tokio::net::UnixStream::pair().map_err(|error| {
        crate::apps::BrokerError::failed(format!("an app's connection cannot be routed: {error}"))
    })?;
    let (reader, writer) = local.into_split();
    let serving = super::wire::serve(connection, outgoing, reader, writer);
    let routing = async move {
        if let Some(received) = received {
            let (reader, writer) = tokio::io::split(
                runtime_portal::descriptors::DescriptorStream::receiving(stream, received),
            );
            route(reader, writer, routed, daemon).await;
        } else {
            let (reader, writer) = stream.into_split();
            route(reader, writer, routed, daemon).await;
        }
    };
    tokio::join!(serving, routing)
        .0
        .map_err(|error| crate::apps::BrokerError::failed(error.to_string()))
}

/// Carry the app's calls on `app_reader` and the answers on `app_writer` between this host's own
/// serving of the app (`local`) and the daemon's (`daemon`), until the app or this host hangs up.
#[allow(
    clippy::future_not_send,
    reason = "a connection is served on the LocalSet, as every other is"
)]
pub(super) async fn route<R, W>(
    app_reader: R,
    app_writer: W,
    local: tokio::net::UnixStream,
    daemon: Result<tokio::net::UnixStream, String>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (inbound_tx, mut inbound) = mpsc::unbounded_channel();
    let (local_reader, local_writer) = local.into_split();
    let (to_app, app_frames) = mpsc::unbounded_channel();
    let (to_local, local_calls) = mpsc::unbounded_channel();
    let (to_daemon, daemon_calls) = mpsc::unbounded_channel();
    let (daemon, to_daemon) = match daemon {
        Ok(daemon) => (Some(daemon.into_split()), Ok(to_daemon)),
        Err(reason) => (None, Err(reason)),
    };
    let daemon_side = async {
        if let Some((daemon_reader, daemon_writer)) = daemon {
            tokio::join!(
                read_frames(daemon_reader, inbound_tx.clone(), |frame| {
                    Inbound::Host(Side::Daemon, frame)
                }),
                write_frames(daemon_writer, daemon_calls),
            );
        }
    };
    let reads = async {
        tokio::join!(
            read_frames(app_reader, inbound_tx.clone(), Inbound::App),
            read_frames(local_reader, inbound_tx.clone(), |frame| {
                Inbound::Host(Side::Local, frame)
            }),
            daemon_side,
            write_frames(app_writer, app_frames),
            write_frames(local_writer, local_calls),
        );
    };
    let mut router = Router {
        app: to_app,
        local: to_local,
        daemon: to_daemon,
        handles: HashMap::new(),
        next_handle: 1,
        waiting: HashMap::new(),
        subscriptions: HashMap::new(),
        asks: HashMap::new(),
        next_ask: 1,
        next_unasked: u64::MAX,
    };
    let routing = async {
        while let Some(next) = inbound.recv().await {
            match next {
                Inbound::App(Some(call)) => router.app_call(call),
                Inbound::Host(side, Some(frame)) => router.host_frame(side, frame),
                Inbound::Host(Side::Daemon, None) => router.daemon_ended(),
                Inbound::App(None) | Inbound::Host(Side::Local, None) => return,
            }
        }
    };
    tokio::select! {
        () = routing => {}
        () = reads => {}
    }
}

/// What reaches the router: the app's next call, or a side's next frame; `None` once it hung up.
enum Inbound {
    App(Option<Call>),
    Host(Side, Option<HostFrame>),
}

async fn read_frames<R, T>(
    mut reader: R,
    into: mpsc::UnboundedSender<Inbound>,
    wrap: impl Fn(Option<T>) -> Inbound,
) where
    R: AsyncRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    loop {
        let frame = FRAMING
            .read_value_async::<_, T>(&mut reader)
            .await
            .unwrap_or_else(|error| {
                tracing::debug!(%error, "an app's routed connection ended on a frame");
                None
            });
        let ended = frame.is_none();
        if into.send(wrap(frame)).is_err() || ended {
            return;
        }
    }
}

async fn write_frames<W, T>(mut writer: W, mut frames: mpsc::UnboundedReceiver<T>)
where
    W: AsyncWrite + Unpin,
    T: serde::Serialize + Sync,
{
    while let Some(frame) = frames.recv().await {
        if FRAMING
            .write_value_async(&mut writer, &frame)
            .await
            .is_err()
            || writer.flush().await.is_err()
        {
            return;
        }
    }
}

/// Who answers a call: this host, or the device's daemon.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Side {
    Local,
    Daemon,
}

/// Where a handle the app has is open.
#[derive(Clone, Copy, Debug, Default)]
struct Opened {
    local: Option<Handle>,
    daemon: Option<Handle>,
}

impl Opened {
    const fn on(&self, side: Side) -> Option<Handle> {
        match side {
            Side::Local => self.local,
            Side::Daemon => self.daemon,
        }
    }

    const fn set(&mut self, side: Side, handle: Handle) {
        match side {
            Side::Local => self.local = Some(handle),
            Side::Daemon => self.daemon = Some(handle),
        }
    }

    /// The side a call goes to, and its handle there: a call on the channel's documents to the
    /// daemon, any other here.
    const fn for_call(&self, documents: bool) -> Option<(Side, Handle)> {
        let side = if documents { Side::Daemon } else { Side::Local };
        match self.on(side) {
            Some(handle) => Some((side, handle)),
            None => None,
        }
    }
}

/// What a call both sides were sent becomes, once both answered.
#[derive(Clone, Copy, Debug)]
enum Combined {
    /// A channel the app has once both sides opened it.
    Join,
    /// The app's handle, closed on every side it is open on.
    Close(Handle),
    /// The channel's context: its roster and permissions from the daemon, this host's directed
    /// asks (following the person, a handoff) from here.
    Context,
    /// A subscription carried on both sides, stopped on both.
    Unsubscribe,
}

/// What an app's call waits on.
enum Waiting {
    /// The answer of the one side it went to.
    One(Side),
    /// The answers of the sides still `awaiting`, and those `answered` already.
    Both {
        combined: Combined,
        awaiting: Vec<Side>,
        answered: Vec<(Side, Result<Reply, String>)>,
    },
}

struct Router {
    app: mpsc::UnboundedSender<HostFrame>,
    local: mpsc::UnboundedSender<Call>,
    /// Why there is no daemon to send to, once it hung up or where the shell has none.
    daemon: Result<mpsc::UnboundedSender<Call>, String>,
    /// Each handle the app has, by its number.
    handles: HashMap<u64, Opened>,
    next_handle: u64,
    /// The calls not answered yet, by the app's id for them.
    waiting: HashMap<u64, Waiting>,
    /// The sides carrying each subscription the app has, by its call id.
    subscriptions: HashMap<u64, Vec<Side>>,
    /// The asks put to the app, by the router's id: the side that asked, and its id.
    asks: HashMap<u64, (Side, u64)>,
    next_ask: u64,
    /// The id of the next call the router makes itself, counted down from the top, where no
    /// app's call id reaches: its answer goes to nobody.
    next_unasked: u64,
}

impl Router {
    #[expect(
        clippy::too_many_lines,
        reason = "one routing table: each call of the wire, and the side that answers it"
    )]
    fn app_call(&mut self, Call { id, request }: Call) {
        match request {
            Operation::Join(join) => {
                let daemon = Some(Operation::Join(join.clone()));
                self.both_sides(id, Combined::Join, Some(Operation::Join(join)), daemon);
            }
            // Where a workspace stands is where it is kept.
            Operation::Workspace { channel, seen } => {
                self.forward(id, Side::Daemon, Operation::Workspace { channel, seen });
            }
            Operation::Publish {
                channel,
                publication,
            } => {
                let documents = matches!(
                    publication,
                    Publication::Presence(_)
                        | Publication::Document(_)
                        | Publication::Content { .. }
                );
                self.on_channel(id, channel, documents, false, |channel| {
                    Operation::Publish {
                        channel,
                        publication,
                    }
                });
            }
            Operation::Subscribe {
                channel,
                topic: Topic::Context,
            } => match self.handles.get(&channel.0).copied() {
                Some(opened) => {
                    let subscribe = |handle| Operation::Subscribe {
                        channel: handle,
                        topic: Topic::Context,
                    };
                    self.both_sides(
                        id,
                        Combined::Context,
                        opened.local.map(subscribe),
                        opened.daemon.map(subscribe),
                    );
                }
                None => self.unknown(id, channel),
            },
            Operation::Subscribe { channel, topic } => {
                let documents = matches!(
                    topic,
                    Topic::Presence | Topic::Ops | Topic::Content | Topic::Resources(_)
                );
                self.on_channel(id, channel, documents, true, |channel| {
                    Operation::Subscribe { channel, topic }
                });
            }
            Operation::Read { channel, read } => {
                self.on_channel(id, channel, true, false, |channel| Operation::Read {
                    channel,
                    read,
                });
            }
            Operation::Command {
                channel,
                service,
                body,
                on_behalf_of,
            } => {
                let documents = matches!(service, Service::CommittedDocument | Service::Resources);
                self.on_channel(id, channel, documents, false, |channel| {
                    Operation::Command {
                        channel,
                        service,
                        body,
                        on_behalf_of,
                    }
                });
            }
            Operation::Changed { channel, of, after } => {
                let documents = matches!(of, Changes::GroupKeys);
                self.on_channel(id, channel, documents, false, |channel| {
                    Operation::Changed { channel, of, after }
                });
            }
            Operation::Admit { channel, report } => {
                self.on_channel(id, channel, true, false, |channel| Operation::Admit {
                    channel,
                    report,
                });
            }
            Operation::Launch {
                channel,
                app_id,
                package_root,
                on_behalf_of,
            } => self.on_channel(id, channel, false, false, |channel| Operation::Launch {
                channel,
                app_id,
                package_root,
                on_behalf_of,
            }),
            Operation::Invocation {
                channel,
                id: invocation,
                call,
            } => self.on_channel(id, channel, false, false, |channel| Operation::Invocation {
                channel,
                id: invocation,
                call,
            }),
            Operation::Settings { channel, call } => {
                self.on_channel(id, channel, false, false, |channel| Operation::Settings {
                    channel,
                    call,
                });
            }
            Operation::LiveActivity { channel, call } => {
                self.on_channel(id, channel, false, false, |channel| {
                    Operation::LiveActivity { channel, call }
                });
            }
            Operation::ControlInput {
                subscription,
                input,
            } => {
                let side = self
                    .subscriptions
                    .get(&subscription)
                    .and_then(|sides| sides.first().copied())
                    .unwrap_or(Side::Local);
                self.forward(
                    id,
                    side,
                    Operation::ControlInput {
                        subscription,
                        input,
                    },
                );
            }
            Operation::Answer { ask, answer } => match self.asks.remove(&ask) {
                Some((side, asked)) => {
                    self.forward(id, side, Operation::Answer { ask: asked, answer });
                }
                None => self.reply(id, Err(format!("no ask {ask} is waiting on an answer"))),
            },
            Operation::Unsubscribe { subscription } => {
                let sides = self
                    .subscriptions
                    .remove(&subscription)
                    .unwrap_or_else(|| vec![Side::Local]);
                let stop = |side| {
                    sides
                        .contains(&side)
                        .then_some(Operation::Unsubscribe { subscription })
                };
                self.both_sides(
                    id,
                    Combined::Unsubscribe,
                    stop(Side::Local),
                    stop(Side::Daemon),
                );
            }
            Operation::Close { handle } => match self.handles.get(&handle.0).copied() {
                Some(opened) => {
                    let close = |handle| Operation::Close { handle };
                    self.both_sides(
                        id,
                        Combined::Close(handle),
                        opened.local.map(close),
                        opened.daemon.map(close),
                    );
                }
                None => self.unknown(id, handle),
            },
            // The session's published resources are this host's.
            Operation::Services => self.forward(id, Side::Local, Operation::Services),
            Operation::Snapshot { services, resource } => {
                self.on_channel(id, services, false, false, |services| Operation::Snapshot {
                    services,
                    resource,
                });
            }
            Operation::Watch { services, resource } => {
                self.on_channel(id, services, false, true, |services| Operation::Watch {
                    services,
                    resource,
                });
            }
            Operation::Invoke {
                services,
                resource,
                action,
                arguments,
            } => self.on_channel(id, services, false, false, |services| Operation::Invoke {
                services,
                resource,
                action,
                arguments,
            }),
            Operation::Shell(_) => self.reply(
                id,
                Err("an app's connection is not the person's shell".to_owned()),
            ),
        }
    }

    /// Send `request`, made for the side the app's `channel` takes it on, there; a `subscribes`
    /// call's id names a subscription that side then carries.
    fn on_channel(
        &mut self,
        id: u64,
        channel: Handle,
        documents: bool,
        subscribes: bool,
        request: impl FnOnce(Handle) -> Operation,
    ) {
        let Some((side, handle)) = self
            .handles
            .get(&channel.0)
            .and_then(|opened| opened.for_call(documents))
        else {
            return self.unknown(id, channel);
        };
        if subscribes {
            self.subscriptions.insert(id, vec![side]);
        }
        self.forward(id, side, request(handle));
    }

    fn forward(&mut self, id: u64, side: Side, request: Operation) {
        match self.send(side, Call { id, request }) {
            Ok(()) => {
                self.waiting.insert(id, Waiting::One(side));
            }
            Err(unsent) => {
                self.subscriptions.remove(&id);
                self.reply(id, Err(unsent));
            }
        }
    }

    /// Send each side its request of the call `id`, and answer the app once every side that took
    /// one answered.
    fn both_sides(
        &mut self,
        id: u64,
        combined: Combined,
        local: Option<Operation>,
        daemon: Option<Operation>,
    ) {
        let mut awaiting = Vec::new();
        let mut answered = Vec::new();
        for (side, request) in [(Side::Local, local), (Side::Daemon, daemon)] {
            let Some(request) = request else { continue };
            match self.send(side, Call { id, request }) {
                Ok(()) => awaiting.push(side),
                Err(unsent) => answered.push((side, Err(unsent))),
            }
        }
        if awaiting.is_empty() {
            self.finish(id, combined, &answered);
        } else {
            self.waiting.insert(
                id,
                Waiting::Both {
                    combined,
                    awaiting,
                    answered,
                },
            );
        }
    }

    fn finish(&mut self, id: u64, combined: Combined, answered: &[(Side, Result<Reply, String>)]) {
        let refusal = || {
            answered
                .iter()
                .filter_map(|(side, result)| result.as_ref().err().map(|error| (*side, error)))
                .min_by_key(|(side, _)| *side == Side::Local)
                .map_or_else(
                    || "nothing on this connection answers that".to_owned(),
                    |(_, error)| error.clone(),
                )
        };
        let took = |side: &Side| {
            answered
                .iter()
                .any(|(answered, result)| answered == side && result.is_ok())
        };
        let result = match combined {
            Combined::Join => {
                let mut opened = Opened::default();
                for (side, result) in answered {
                    if let Ok(Reply::Opened(handle)) = result {
                        opened.set(*side, *handle);
                    }
                }
                if opened.local.is_some() && opened.daemon.is_some() {
                    Ok(Reply::Opened(self.open(opened)))
                } else {
                    // The side that took the channel lets it go again: the app has it nowhere.
                    for side in [Side::Local, Side::Daemon] {
                        if let Some(handle) = opened.on(side) {
                            self.close_unasked(side, handle);
                        }
                    }
                    Err(refusal())
                }
            }
            Combined::Close(handle) => {
                self.handles.remove(&handle.0);
                if answered.iter().any(|(_, result)| result.is_ok()) {
                    Ok(Reply::Done)
                } else {
                    Err(refusal())
                }
            }
            Combined::Context => {
                let sides: Vec<Side> = [Side::Local, Side::Daemon]
                    .into_iter()
                    .filter(took)
                    .collect();
                if sides.len() == answered.len() {
                    self.subscriptions.insert(id, sides);
                    Ok(Reply::Subscribed)
                } else {
                    Err(refusal())
                }
            }
            Combined::Unsubscribe => {
                if answered.iter().any(|(_, result)| result.is_ok()) {
                    Ok(Reply::Done)
                } else {
                    Err(refusal())
                }
            }
        };
        self.reply(id, result);
    }

    fn host_frame(&mut self, side: Side, frame: HostFrame) {
        match frame {
            HostFrame::Reply { id, result } => match self.waiting.remove(&id) {
                Some(Waiting::One(to)) if to == side => {
                    if result.is_err() {
                        self.subscriptions.remove(&id);
                    }
                    let result = match result {
                        Ok(Reply::Opened(handle)) => {
                            let mut opened = Opened::default();
                            opened.set(side, handle);
                            Ok(Reply::Opened(self.open(opened)))
                        }
                        other => other,
                    };
                    self.reply(id, result);
                }
                Some(Waiting::Both {
                    combined,
                    mut awaiting,
                    mut answered,
                }) if awaiting.contains(&side) => {
                    awaiting.retain(|awaited| *awaited != side);
                    answered.push((side, result));
                    if awaiting.is_empty() {
                        self.finish(id, combined, &answered);
                    } else {
                        self.waiting.insert(
                            id,
                            Waiting::Both {
                                combined,
                                awaiting,
                                answered,
                            },
                        );
                    }
                }
                // A reply from a side the call never went to answers nothing.
                Some(waiting) => {
                    self.waiting.insert(id, waiting);
                }
                None => {}
            },
            HostFrame::Event {
                subscription,
                event,
            } => {
                if let Event::Context(update) = &event
                    && self
                        .subscriptions
                        .get(&subscription)
                        .is_some_and(|sides| sides.len() > 1)
                    && !context_from(side, update)
                {
                    return;
                }
                self.tell_app(HostFrame::Event {
                    subscription,
                    event,
                });
            }
            HostFrame::Ended {
                subscription,
                reason,
            } => {
                let last = self
                    .subscriptions
                    .get_mut(&subscription)
                    .is_none_or(|sides| {
                        sides.retain(|carrying| *carrying != side);
                        sides.is_empty()
                    });
                if last {
                    self.subscriptions.remove(&subscription);
                    self.tell_app(HostFrame::Ended {
                        subscription,
                        reason,
                    });
                }
            }
            HostFrame::Ask { id, ask } => self.ask_app(side, id, ask),
        }
    }

    /// Put `side`'s ask `id` to the app, under an id of the router's and on the app's handle.
    fn ask_app(&mut self, side: Side, id: u64, ask: Ask) {
        let asked = self.next_ask;
        self.next_ask = self.next_ask.wrapping_add(1);
        self.asks.insert(asked, (side, id));
        let ask = match ask {
            Ask::Query { channel, query } => Ask::Query {
                channel: self.app_handle(side, channel),
                query,
            },
            Ask::Resolve {
                channel,
                revision,
                resource,
                action,
                local_document,
            } => Ask::Resolve {
                channel: self.app_handle(side, channel),
                revision,
                resource,
                action,
                local_document,
            },
        };
        self.tell_app(HostFrame::Ask { id: asked, ask });
    }

    /// The daemon hung up: every call it had yet to answer is refused, and every subscription it
    /// carried ends where it carried it alone.
    fn daemon_ended(&mut self) {
        self.daemon = Err(DAEMON_ENDED.to_owned());
        let unanswered: Vec<u64> = self
            .waiting
            .iter()
            .filter(|(_, waiting)| match waiting {
                Waiting::One(side) => *side == Side::Daemon,
                Waiting::Both { awaiting, .. } => awaiting.contains(&Side::Daemon),
            })
            .map(|(id, _)| *id)
            .collect();
        for id in unanswered {
            self.host_frame(
                Side::Daemon,
                HostFrame::Reply {
                    id,
                    result: Err(DAEMON_ENDED.to_owned()),
                },
            );
        }
        let carried: Vec<u64> = self
            .subscriptions
            .iter()
            .filter(|(_, sides)| sides.contains(&Side::Daemon))
            .map(|(subscription, _)| *subscription)
            .collect();
        for subscription in carried {
            self.host_frame(
                Side::Daemon,
                HostFrame::Ended {
                    subscription,
                    reason: DAEMON_ENDED.to_owned(),
                },
            );
        }
    }

    fn open(&mut self, opened: Opened) -> Handle {
        let handle = Handle(self.next_handle);
        self.next_handle = self.next_handle.wrapping_add(1);
        self.handles.insert(handle.0, opened);
        handle
    }

    /// The app's handle for `side`'s `handle`.
    fn app_handle(&self, side: Side, handle: Handle) -> Handle {
        self.handles
            .iter()
            .find(|(_, opened)| opened.on(side) == Some(handle))
            .map_or(handle, |(app, _)| Handle(*app))
    }

    /// Send `call` to `side`, or say why it cannot go there.
    fn send(&self, side: Side, call: Call) -> Result<(), String> {
        match side {
            Side::Local => self
                .local
                .send(call)
                .map_err(|_| "this host closed the app's connection".to_owned()),
            Side::Daemon => match &self.daemon {
                Ok(daemon) => daemon.send(call).map_err(|_| DAEMON_ENDED.to_owned()),
                Err(reason) => Err(reason.clone()),
            },
        }
    }

    /// Close `side`'s `handle`, which the app never got: its answer is nobody's.
    fn close_unasked(&mut self, side: Side, handle: Handle) {
        let id = self.next_unasked;
        self.next_unasked = self.next_unasked.wrapping_sub(1);
        let _ = self.send(
            side,
            Call {
                id,
                request: Operation::Close { handle },
            },
        );
    }

    fn unknown(&self, id: u64, handle: Handle) {
        self.reply(
            id,
            Err(format!("this connection has no handle {}", handle.0)),
        );
    }

    fn reply(&self, id: u64, result: Result<Reply, String>) {
        self.tell_app(HostFrame::Reply { id, result });
    }

    fn tell_app(&self, frame: HostFrame) {
        let _ = tilo_failure::ignored!(
            "the app hung up, which ends its routing too",
            self.app.send(frame)
        );
    }
}

/// Whether `side` says `update` of a context both sides carry: the daemon the roster, the
/// permissions and the document's own asks; this host whom the person follows and a handoff.
const fn context_from(side: Side, update: &ContextUpdate) -> bool {
    let here = matches!(
        update,
        ContextUpdate::FollowChanged { .. } | ContextUpdate::Handoff(_)
    );
    match side {
        Side::Local => here,
        Side::Daemon => !here,
    }
}
