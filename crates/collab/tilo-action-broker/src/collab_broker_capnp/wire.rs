//! The host side of the collaboration wire (`runtime_collab::wire`): one
//! dispatcher over its requests, with the channels a connection opened kept
//! here under their handles and its subscriptions pumped onto the one
//! outgoing frame queue.

#![allow(
    clippy::future_not_send,
    reason = "connections, channels and their calls live on the broker's LocalSet and never cross a thread"
)]
#![allow(
    clippy::wildcard_imports,
    reason = "the wire's dispatcher is the broker's channel core seen from another transport, and uses most of it"
)]

use std::cell::Cell;

use runtime_collab::wire::{
    Call, Changes, ContextUpdate, Event, FRAMING, Handle, HostFrame, InvocationCall, Join,
    Participant, Publication, Read, Reply, Report, Request, Service, TaskCall, TaskStep, Topic,
};
use tokio::io::{AsyncRead, AsyncWrite};

use super::*;

/// A channel's presence as one admitted endpoint has it: what it publishes,
/// and the feed of everyone else's.
pub(super) struct PresenceSession {
    state: Arc<CapnpBrokerState>,
    channel_id: String,
    peer_id: u64,
    verified_identity: AdmittedJoinIdentity,
    /// Broker-owned monotonic sequence per invocation. Publishers describe
    /// phases and targets but cannot reorder or resurrect their visual state.
    interaction_sequences: Mutex<HashMap<tilo_actions::InvocationId, u64>>,
}

impl PresenceSession {
    pub(super) fn new(
        state: Arc<CapnpBrokerState>,
        channel_id: String,
        peer_id: u64,
        verified_identity: AdmittedJoinIdentity,
    ) -> Self {
        Self {
            state,
            channel_id,
            peer_id,
            verified_identity,
            interaction_sequences: Mutex::new(HashMap::new()),
        }
    }

    /// Widened to room scope — presence is room-wide, only document
    /// payloads are confined to a record.
    fn route(&self) -> CrdtRoute {
        self.full_route().to_cluster_scope()
    }

    /// Unwidened, so peer replication carries the full app/cluster route.
    fn full_route(&self) -> CrdtRoute {
        CrdtRoute::from_channel_key(&self.channel_id)
    }

    fn revalidate(&self) -> Result<(), String> {
        let route = if self.verified_identity.host_scope.is_some() {
            self.full_route()
        } else {
            self.route()
        };
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Publish a keyless pointer position: the cursor stream's event and
    /// the default pointer key's presence.
    pub(super) fn publish_cursor(&self, x: f32, y: f32) -> Result<(), String> {
        self.revalidate()?;
        let (route, full_route) = (self.route(), self.full_route());
        let event = CollabEvent::RemoteCursorMoved {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            x,
            y,
        };
        self.state.publish_presence(
            &full_route,
            &PresenceKey::pointer_default(),
            &PresencePayload::Pointer { x, y },
        );
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &full_route);
        }
        Ok(())
    }

    /// Publish one keyed presence update after the broker's own checks: a
    /// status is trimmed and bounded, an interaction is attributed to this
    /// endpoint and sequenced, and the desktop's own presence is refused.
    pub(super) fn publish(&self, key: PresenceKey, payload: PresencePayload) -> Result<(), String> {
        self.revalidate()?;
        let payload = match payload {
            PresencePayload::Status { text, automatic } => {
                let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
                if text.chars().count() > ui::MAX_STATUS_CHARS {
                    return Err("status exceeds local protocol bound".into());
                }
                PresencePayload::Status { text, automatic }
            }
            PresencePayload::Interaction(update) => {
                PresencePayload::Interaction(Box::new(self.attributed(*update)?))
            }
            PresencePayload::Availability { .. } => {
                return Err("desktop presence is the shell's, never an app's".into());
            }
            other => other,
        };
        let route = self.route();
        self.state
            .publish_presence(&self.full_route(), &key, &payload);
        let event = CollabEvent::RemotePresenceUpdate {
            user_id: CapnpBrokerState::participant_id(&self.verified_identity),
            key,
            payload,
        };
        if let Some(fanout) = self.state.fanout.upgrade() {
            fanout.fanout_presence(&event, self.peer_id, &route, &self.full_route());
        }
        Ok(())
    }

    /// `update` as this endpoint's: its actor is the admitted one, and its
    /// sequence the broker's next for the invocation.
    fn attributed(
        &self,
        update: tilo_actions::SemanticInteraction,
    ) -> Result<tilo_actions::SemanticInteraction, String> {
        let actor = self.verified_identity.broker_actor();
        let normalized_actor = tilo_actions::InteractionActor::new(
            CapnpBrokerState::participant_id(&self.verified_identity),
            if actor.is_agent {
                tilo_actions::ActorKind::Agent
            } else {
                tilo_actions::ActorKind::Person
            },
        );
        let mut sequences = self
            .interaction_sequences
            .lock()
            .map_err(|_| "interaction sequence registry unavailable".to_owned())?;
        let current = sequences.entry(update.invocation).or_insert(0);
        *current = current.saturating_add(1);
        let sequence = *current;
        drop(sequences);
        let update = update.normalized_by_broker(normalized_actor, sequence);
        update
            .validate()
            .map_err(|error| format!("invalid interaction: {error}"))?;
        Ok(update)
    }

    /// Everyone else's presence in this room from now on.
    pub(super) fn subscribe(&self) -> Result<Feed<PresenceEnvelope>, String> {
        self.revalidate()?;
        Ok(Feed::new(
            HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity),
            self.state
                .subscribe_presence(&self.channel_id, self.peer_id),
        ))
    }
}

/// What a subscription receives from the broker's fanout. A host worker's
/// feed ends with an error once that worker is no longer current, and a
/// native window's once its room or document custody ended, checked on
/// both sides of every wait.
pub(super) struct Feed<T> {
    host_guard: Option<HostWorkerReadGuard>,
    native_guard: Option<NativeContextReadGuard>,
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> Feed<T> {
    pub(super) fn new(
        host_guard: Option<HostWorkerReadGuard>,
        rx: mpsc::UnboundedReceiver<T>,
    ) -> Self {
        Self {
            host_guard,
            native_guard: None,
            rx,
        }
    }

    pub(super) fn with_native_guard(mut self, guard: Option<NativeContextReadGuard>) -> Self {
        self.native_guard = guard;
        self
    }

    fn check(&self) -> Result<(), capnp::Error> {
        HostWorkerReadGuard::check(self.host_guard.as_ref())?;
        NativeContextReadGuard::check(self.native_guard.as_ref())
    }

    /// The next item, or `None` when the fanout closed.
    pub(super) async fn next(&mut self) -> Result<Option<T>, capnp::Error> {
        self.check()?;
        let Some(next) = self.rx.recv().await else {
            return Ok(None);
        };
        self.check()?;
        Ok(Some(next))
    }
}

/// What a connection opened under one handle: one admitted endpoint's hold
/// on a channel, and its presence there.
pub(super) struct ChannelSession {
    channel: Rc<CapnpChannelHandle>,
    presence: PresenceSession,
}

impl ChannelSession {
    pub(super) fn new(channel: Rc<CapnpChannelHandle>) -> Self {
        let presence = PresenceSession::new(
            channel.state.clone(),
            channel.channel_id.clone(),
            channel.connection_id,
            channel.verified_identity.clone(),
        );
        Self { channel, presence }
    }
}

/// A local-file command's answer on its way.
pub(super) type FileAnswer = std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, capnp::Error>>>>;

impl CapnpChannelHandle {
    /// Answer a command about the person's local files, JSON both ways:
    /// selected discovery and opening, write attribution, external
    /// moderation, or the file clipboard, told apart by their envelopes.
    pub(super) fn local_file_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        on_behalf_of: &[u8],
    ) -> Result<FileAnswer, capnp::Error> {
        #[cfg(target_os = "linux")]
        if runtime_collab::open_selected::discovery::is_envelope(bytes) {
            let future = self.selected_discovery_dispatch(bytes, on_behalf_of)?;
            return Ok(Box::pin(async move {
                let value = future.await.map_err(capnp::Error::failed)?;
                if !value.valid() {
                    return Err(capnp::Error::failed("invalid discovery response".into()));
                }
                let bytes =
                    serde_json::to_vec(&value).map_err(|e| capnp::Error::failed(e.to_string()))?;
                if bytes.len() > runtime_collab::open_selected::discovery::MAX_BYTES {
                    return Err(capnp::Error::failed(
                        "discovery response exceeds limit".into(),
                    ));
                }
                Ok(bytes)
            }));
        }
        #[cfg(target_os = "linux")]
        if runtime_collab::open_selected::v2::is_envelope(bytes) {
            let future = self.selected_open_v2_dispatch(bytes, on_behalf_of)?;
            return Ok(Box::pin(async move {
                let receipt = future.await.map_err(capnp::Error::failed)?;
                runtime_collab::open_selected::v2::encode_observed(&receipt)
                    .map_err(|e| capnp::Error::failed(e.to_string()))
            }));
        }
        #[cfg(target_os = "linux")]
        if runtime_collab::open_selected::is_envelope(bytes) {
            let future = self.selected_open_dispatch(bytes, on_behalf_of)?;
            return Ok(Box::pin(async move {
                let receipt = future.await.map_err(capnp::Error::failed)?;
                runtime_collab::open_selected::encode_observed(&receipt)
                    .map_err(|e| capnp::Error::failed(e.to_string()))
            }));
        }
        if runtime_collab::write_attribution::is_envelope(bytes) {
            let reply = self.write_attribution_dispatch(bytes, on_behalf_of)?;
            return Ok(Box::pin(std::future::ready(Ok(reply))));
        }
        if external_moderation::is_envelope(bytes) {
            let receive = external_moderation::start(&self, bytes, on_behalf_of)
                .map_err(capnp::Error::failed)?;
            return Ok(Box::pin(async move {
                let reply = receive
                    .await
                    .map_err(|_| {
                        capnp::Error::failed("external moderation outcome unobserved".into())
                    })?
                    .map_err(capnp::Error::failed)?;
                serde_json::to_vec(&reply).map_err(|error| capnp::Error::failed(error.to_string()))
            }));
        }
        let reply = self.file_clipboard_dispatch(bytes, on_behalf_of)?;
        Ok(Box::pin(std::future::ready(Ok(reply))))
    }
}

impl CapnpChannelHandle {
    /// Answer a committed-document request. An `Await` is held until the
    /// read may have changed, and an Open whose record is still being
    /// decided (the room's host's replay) or created on another device is
    /// awaited, all on this `LocalSet`, so every channel's calls keep
    /// flowing meanwhile.
    pub(super) async fn committed_document_answer(
        self: Rc<Self>,
        request: runtime_collab::committed_document::Request,
    ) -> Result<runtime_collab::committed_document::Reply, String> {
        use runtime_collab::committed_document::Request;
        if let Request::Await { known_revision } = request {
            return self.await_committed_change(known_revision).await;
        }
        if let Some(waiting) = self.committed_history_wait(&request) {
            waiting.await;
        }
        let prepared = match self.committed_creation_elsewhere(&request) {
            Some(preparing) => Some(preparing.await),
            None => None,
        };
        self.committed_document_dispatch_prepared(&request, prepared)
    }
}

/// A local effect's record on its way into workspace activity.
pub(super) type Recording = std::pin::Pin<Box<dyn Future<Output = Result<(), capnp::Error>>>>;

/// A service's answer on its way.
/// A change to an app's published action surface.
pub(super) enum SurfaceChange {
    /// The whole surface, replacing what was published.
    Whole(Vec<runtime_collab::ExportedAction>),
    /// A change to what was published.
    Delta(runtime_collab::SurfaceDelta),
}

/// What the host answers once its work is done.
pub(super) type Answer<T> = std::pin::Pin<Box<dyn Future<Output = Result<T, capnp::Error>>>>;

/// A task call's answer, which a task step may wait on.
pub(super) type TaskAnswer = std::pin::Pin<Box<dyn Future<Output = Result<Reply, capnp::Error>>>>;
type Answering = std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, String>>>>;

impl ChannelSession {
    /// Answer a command to one of the host's services on this channel.
    fn command(
        &self,
        service: Service,
        body: &[u8],
        on_behalf_of: &[u8],
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Reply, String>>>> {
        let channel = Rc::clone(&self.channel);
        let failed = |error: capnp::Error| error.to_string();
        let ready = |reply: Result<Vec<u8>, capnp::Error>| -> Result<Answering, String> {
            let reply = reply.map_err(failed)?;
            Ok(Box::pin(std::future::ready(Ok(reply))))
        };
        let answer =
            match service {
                Service::AudioOutput => channel.audio_output_dispatch(body).map_err(failed).map(
                    |snapshot| -> Answering {
                        Box::pin(async move {
                            serde_json::to_vec(&snapshot.await?)
                                .map_err(|_| "audio output reply encoding failed".to_owned())
                        })
                    },
                ),
                Service::NativeMedia => channel.native_media_dispatch(body).map_err(failed).map(
                    |snapshot| -> Answering {
                        Box::pin(async move { snapshot.await?.encode().map_err(Into::into) })
                    },
                ),
                Service::Realtime => {
                    ready(channel.realtime_dispatch(&channel.route().app_id, body))
                }
                Service::NearbyTransfer => {
                    ready(channel.nearby_transfer_dispatch(&channel.route().app_id, body))
                }
                // The secret answer moves onto the wire without a copy left
                // behind.
                Service::BrowserServiceSession => ready(
                    channel
                        .browser_service_session_dispatch(body)
                        .map(|mut reply| std::mem::take(&mut *reply)),
                ),
                Service::WifiContinuity => channel
                    .wifi_continuity_dispatch(&channel.route().app_id, body)
                    .map_err(failed)
                    .map(|reply| -> Answering {
                        Box::pin(async move { reply.await.map_err(|error| error.to_string()) })
                    }),
                Service::Speaker => ready(channel.speaker_dispatch(body)),
                Service::CommittedDocument => {
                    runtime_collab::committed_document::Request::decode(body)
                        .map_err(Into::into)
                        .map(|request| -> Answering {
                            Box::pin(async move {
                                channel
                                    .committed_document_answer(request)
                                    .await?
                                    .encode()
                                    .map_err(Into::into)
                            })
                        })
                }
                Service::LocalFile => channel
                    .local_file_dispatch(body, on_behalf_of)
                    .map_err(failed)
                    .map(|reply| -> Answering {
                        Box::pin(async move { reply.await.map_err(|error| error.to_string()) })
                    }),
                Service::ApprovedCheck => runtime_collab::approved_checks::Request::decode(body)
                    .and_then(|request| approved_checks::context(&channel, request))
                    .map(|context| -> Answering { Box::pin(approved_checks::execute(context)) }),
                Service::RemoteAction => {
                    let reply = channel.remote_action_dispatch(body);
                    Ok(
                        Box::pin(async move { reply.await.map_err(|error| error.to_string()) })
                            as Answering,
                    )
                }
                Service::PersonChat => channel
                    .person_chat_dispatch(body, on_behalf_of)
                    .map(|reply| -> Answering { Box::pin(std::future::ready(Ok(reply))) }),
            };
        Box::pin(async move { Ok(Reply::Answer(answer?.await?)) })
    }
}

impl std::ops::Deref for ChannelSession {
    type Target = CapnpChannelHandle;

    fn deref(&self) -> &CapnpChannelHandle {
        &self.channel
    }
}

impl CapnpChannelHandle {
    /// Admit, commit where the room's owner must hold it first, and fan out
    /// one signed document change.
    pub(super) async fn publish_document(&self, envelope: SignedOp) -> Result<(), capnp::Error> {
        self.writable()?;
        let (state, route) = (&self.state, self.route());
        let current_identity = state.revalidate_admission(&route, &self.verified_identity)?;
        verify_session_author(&current_identity, envelope.author)
            .map_err(|error| capnp::Error::failed(format!("send: {error}")))?;
        state.authorize_certified_document(&route, &current_identity, &envelope)?;
        let owner_required =
            state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id);
        let _owner_order = if owner_required {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        if owner_required {
            state.revalidate_admission(&route, &self.verified_identity)?;
            state
                .commit_owner_request(
                    tilo_owner_commit::OwnerCommitRequest {
                        room: route.cluster_id.clone(),
                        operation: tilo_owner_commit::CommitOperation::Document {
                            app: route.app_id.clone(),
                            record: route.record_id.clone(),
                            envelope: Box::new(envelope.clone()),
                        },
                    },
                    self.record_kind.clone(),
                )
                .await?;
        }
        let current_identity = state.revalidate_admission(&route, &self.verified_identity)?;
        let authorized =
            state.authorize_certified_document(&route, &current_identity, &envelope)?;
        handle_authorized_signed_op_send(
            state,
            self.connection_id,
            &route,
            &current_identity,
            &authorized,
            envelope,
        );
        Ok(())
    }

    /// Every document change on this channel from now on, other endpoints'
    /// and peers'.
    pub(super) fn subscribe_ops(&self) -> Result<Feed<OpEnvelope>, capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        Ok(Feed::new(
            self.host_guard(),
            self.state
                .subscribe_ops(&self.channel_id, self.connection_id),
        ))
    }

    fn host_guard(&self) -> Option<HostWorkerReadGuard> {
        HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity)
    }

    /// The host's record-content service, for a channel still admitted.
    fn record_content(
        &self,
    ) -> Result<(CrdtRoute, Arc<crate::record_content::RouteContent>), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(capnp::Error::failed(
                "record content belongs to a certified app route".into(),
            ));
        }
        let content = self.state.services.record_content.clone().ok_or_else(|| {
            capnp::Error::unimplemented("this host keeps no record content".into())
        })?;
        Ok((route, content))
    }

    /// Keep content the record references, which the app stored.
    pub(super) fn offer_content(&self, hash: &str, bytes: &[u8]) -> Result<(), capnp::Error> {
        self.writable()?;
        let (route, content) = self.record_content()?;
        content
            .offer(&route, hash, bytes)
            .map_err(capnp::Error::failed)
    }

    /// Fetch content the record references and the app lacks; it arrives on
    /// the content feed.
    pub(super) fn want_content(&self, hash: &str) -> Result<(), capnp::Error> {
        let (route, content) = self.record_content()?;
        content.want(&route, hash);
        Ok(())
    }

    /// The channel's context from now on, starting with its current
    /// roster. A native private window's context also follows the
    /// continuation it shows.
    pub(super) fn context_feed(&self) -> Result<Feed<ContextUpdate>, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let native = self
            .state
            .native_roster_for_subscriber(&route, &self.verified_identity)
            .map_err(capnp::Error::failed)?;
        let instance = self
            .native_private_window_instance()
            .map_err(capnp::Error::failed)?;
        let document = match instance {
            Some(_) => Some(self.document_read_context().map_err(capnp::Error::failed)?),
            None => None,
        };
        let rx = self
            .state
            .subscribe_context(&self.channel_id, self.connection_id, native.clone());
        if let Some(instance) = instance {
            let alias = CrdtRoute::record(
                &route.app_id,
                &route.cluster_id,
                format!("continuation:{instance}"),
            )
            .channel_key();
            self.state
                .alias_private_context(&self.channel_id, &alias, self.connection_id);
        }
        let native_guard = native.map(|native| NativeContextReadGuard {
            state: self.state.clone(),
            route,
            identity: native.admitted,
            document,
        });
        Ok(Feed::new(self.host_guard(), rx).with_native_guard(native_guard))
    }

    /// Ask another endpoint holding this channel to publish the document's
    /// current state.
    pub(super) fn ask_for_document(&self) -> Result<(), capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let _ = self
            .state
            .request_document_snapshot(&self.channel_id, self.connection_id);
        Ok(())
    }

    /// Take a report about this channel's own instance or record from the
    /// app `claimed_app`, which must be the route's: no app reports for
    /// another, and a channel held to read reports nothing.
    pub(super) fn report(&self, claimed_app: &str, report: Report) -> Result<(), capnp::Error> {
        let verb = match &report {
            Report::Continuation { .. } => "reportContinuation",
            Report::ClearContinuation => "clearContinuation",
            Report::Handoff(_) => "handoff",
            Report::SearchIndex(_) => "publishSearchIndex",
        };
        self.writable()?;
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id.is_empty() || claimed_app != route.app_id {
            return Err(capnp::Error::failed(format!(
                "{verb} appId must match the certified connection route"
            )));
        }
        // A continuation report or a handoff step speaks only for this
        // instance; a search index for the record the route names.
        let instance = match report {
            Report::SearchIndex(_) => None,
            _ => Some(self.continuation_instance().map_err(capnp::Error::failed)?),
        };
        let room = tilo_desktop_model::RoomId::parse(&route.cluster_id).ok_or_else(|| {
            capnp::Error::failed(format!("{verb} route has no durable workspace room"))
        })?;
        let services = &self.state.services;
        let continuations = |unsupported: &str| {
            services
                .continuations
                .as_ref()
                .ok_or_else(|| unsupported.to_owned())
        };
        let app_id = route.app_id.clone();
        let instance = instance.unwrap_or_default();
        match report {
            Report::Continuation {
                schema_version,
                metadata,
            } => continuations("continuation reporting unsupported").and_then(|service| {
                service.report(room, app_id, instance, schema_version, metadata)
            }),
            Report::ClearContinuation => continuations("continuation clearing unsupported")
                .and_then(|service| service.clear(room, &app_id, instance)),
            Report::Handoff(message) => continuations("handoff unsupported")
                .and_then(|service| service.handoff(room, app_id, instance, message)),
            Report::SearchIndex(delta) => {
                if delta.len() > crate::services::MAX_SEARCH_INDEX_DELTA_BYTES {
                    return Err(capnp::Error::failed(
                        "publishSearchIndex delta exceeds its bound".into(),
                    ));
                }
                services
                    .search_index
                    .as_ref()
                    .ok_or_else(|| "search index publication unsupported".to_owned())
                    .and_then(|service| service.publish(room, &app_id, &route.record_id, &delta))
            }
        }
        .map_err(capnp::Error::failed)
    }

    /// Record a terminal effect this app ran itself on one of the actions
    /// it publishes, as workspace activity: committed by the room's owner
    /// where one keeps effect receipts, else projected here.
    pub(super) fn record_local_effect_dispatch(
        &self,
        resource: &str,
        action: &str,
        update: tilo_actions::InvocationUpdate,
    ) -> Result<Recording, capnp::Error> {
        // No app changes another app's route: a channel held to read refuses.
        self.writable()?;
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(&update) else {
            return Err(capnp::Error::failed(
                "only a terminal effect may enter workspace activity".into(),
            ));
        };
        let Some((surface, offered)) = self
            .state
            .surfaces
            .lock()
            .ok()
            .and_then(|surfaces| surfaces.get(&self.peer_id).cloned())
            .and_then(|surface| {
                let offered = surface
                    .actions
                    .iter()
                    .find(|candidate| candidate.resource == resource && candidate.action == action)
                    .cloned()?;
                Some((surface, offered))
            })
        else {
            return Err(capnp::Error::failed(
                "the application no longer publishes that action".into(),
            ));
        };
        if offered.operation_kind != tilo_actions::OperationKind::Effect {
            return Err(capnp::Error::failed(
                "only Effects produce durable invocation receipts".into(),
            ));
        }
        let what = tilo_actions::Did::verb(
            tilo_actions::ActionKey::new(&surface.app_id, &offered.resource_kind, &offered.action),
            [],
        );
        let mut context = tilo_actions::ActionContext::for_resource(
            offered.resource,
            offered.resource_kind,
            offered.resource_label,
        )
        .with_app(&surface.app_id, &surface.app_id)
        .with_described(tilo_actions::Described::new(what));
        let actor = current.broker_actor().id;
        if self.state.durable_owner_required
            || self
                .state
                .publisher
                .records_effects_for(&surface.route.cluster_id)
        {
            context.purpose = None;
            let receipt = tilo_desktop_model::WorkspaceEffectReceipt::redacted(
                update.invocation(),
                &actor,
                context,
                status,
                tilo_clock::unix_now_i64(),
            );
            let state = self.state.clone();
            let route = self.route();
            let identity = self.verified_identity.clone();
            return Ok(Box::pin(async move {
                let _order = state.owner_commit_order.lock().await;
                state.revalidate_admission(&route, &identity)?;
                state
                    .commit_owner_request(
                        tilo_owner_commit::OwnerCommitRequest {
                            room: surface.route.cluster_id,
                            operation: tilo_owner_commit::CommitOperation::EffectReceipt {
                                invocation: update.invocation().0,
                                redacted: serde_json::to_vec(&receipt)
                                    .map_err(|e| capnp::Error::failed(e.to_string()))?,
                            },
                        },
                        None,
                    )
                    .await?;
                state.revalidate_admission(&route, &identity)?;
                Ok(())
            }));
        }
        context.purpose = None;
        self.state
            .services
            .legacy_workspace
            .as_ref()
            .ok_or_else(|| "native effect projection unsupported".to_string())
            .and_then(|projection| {
                projection.record_effect(
                    &surface.route.cluster_id,
                    update.invocation(),
                    &actor,
                    context,
                    status,
                )
            })
            .map_err(capnp::Error::failed)?;
        Ok(Box::pin(std::future::ready(Ok(()))))
    }

    /// The wait for `of`'s next change past `after`, which resolves to the
    /// revision to wait from next. It says only that something changed;
    /// the endpoint reads again, checked as always.
    pub(super) fn change(
        &self,
        of: Changes,
        after: u64,
    ) -> Result<crate::services::NearbyTransferChange, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let services = &self.state.services;
        match of {
            Changes::Speakers => speaker_change(services, &route.app_id, after),
            Changes::Drives => drive_changes::drive_change(services, &route.app_id, after),
            Changes::Applications => app_changes::app_change(services, &route.app_id, after),
            Changes::NearbyTransfers => {
                let context = crate::services::NearbyTransferRequestContext::authenticated(
                    self.connection_id,
                    route.app_id.clone(),
                    route.cluster_id.clone(),
                    route.channel_key(),
                    self.verified_identity
                        .agent_id
                        .is_none()
                        .then(|| self.verified_identity.principal_id.clone()),
                );
                nearby_transfer_change(services, &context, after)
            }
            Changes::ChannelKeys => {
                if route.app_id.is_empty() || route.cluster_id.is_empty() {
                    return Err(capnp::Error::failed(
                        "channel keys belong to a certified app route".into(),
                    ));
                }
                let keys = services.channel_keys.clone().ok_or_else(|| {
                    capnp::Error::unimplemented("this host has no channel service to follow".into())
                })?;
                let channel = collab_types::document_channel_id(
                    &route.app_id,
                    &route.cluster_id,
                    &route.record_id,
                );
                Ok(Box::pin(async move {
                    keys.channel_changed(channel, after).await
                }))
            }
        }
    }

    /// Answer a realtime command `claimed_app` sent, JSON both ways.
    pub(super) fn realtime_dispatch(
        &self,
        claimed_app: &str,
        bytes: &[u8],
    ) -> Result<Vec<u8>, capnp::Error> {
        let context = self.realtime_context()?;
        if bytes.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "realtime command exceeds local protocol bound".into(),
            ));
        }
        let reply = delegate_realtime_command(&self.state.services, &context, claimed_app, bytes)?;
        if reply.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "realtime reply exceeds local protocol bound".into(),
            ));
        }
        Ok(reply)
    }

    /// Answer a nearby-transfer command `claimed_app` sent, JSON both ways.
    pub(super) fn nearby_transfer_dispatch(
        &self,
        claimed_app: &str,
        bytes: &[u8],
    ) -> Result<Vec<u8>, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let context = crate::services::NearbyTransferRequestContext::authenticated(
            self.connection_id,
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.channel_key(),
            self.verified_identity
                .agent_id
                .is_none()
                .then(|| self.verified_identity.principal_id.clone()),
        );
        delegate_nearby_transfer_command(&self.state.services, &context, claimed_app, bytes)
    }

    /// Answer a Wi-Fi continuity command `claimed_app` sent, JSON both
    /// ways; the certified Settings route's only.
    pub(super) fn wifi_continuity_dispatch(
        &self,
        claimed_app: &str,
        bytes: &[u8],
    ) -> Result<impl Future<Output = Result<Vec<u8>, capnp::Error>> + use<>, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id != WIFI_CONTINUITY_APP_ID || claimed_app != route.app_id {
            return Err(capnp::Error::failed(
                "Wi-Fi continuity requires the certified Settings connection route".into(),
            ));
        }
        if bytes.len() > runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "Wi-Fi continuity command exceeds the local protocol bound".into(),
            ));
        }
        let services = self.state.services.clone();
        let claimed_app = claimed_app.to_owned();
        let command = bytes.to_vec();
        Ok(async move {
            // NetworkManager and nl80211 queries are bounded blocking work.
            // Keep them off the broker's event loop and preserve the same
            // certified Settings validation used by every local command.
            tokio::task::spawn_blocking(move || {
                delegate_wifi_continuity_command(&services, &route.app_id, &claimed_app, &command)
            })
            .await
            .map_err(|error| capnp::Error::failed(error.to_string()))?
        })
    }

    /// Answer a private service-session command from the Browser for the
    /// page its person reads.
    pub(super) fn browser_service_session_dispatch(
        &self,
        bytes: &[u8],
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, capnp::Error> {
        let refused = || capnp::Error::failed("private service-session request refused".into());
        if self.route().app_id != "so.tilo.browser"
            || self.verified_identity.agent_id.is_some()
            || self.verified_identity.host_scope.is_some()
        {
            return Err(refused());
        }
        let read = self.document_read_context().map_err(|_| refused())?;
        let context = crate::services::BrowserServiceSessionRequestContext::authenticated(
            self.connection_id,
            self.verified_identity.principal_id.clone(),
            read,
        );
        delegate_browser_service_session_command(&self.state.services, &context, bytes)
    }

    /// Answer a speaker-group command, JSON both ways. Only the certified
    /// Settings route manages the person's speaker groups.
    pub(super) fn speaker_dispatch(&self, bytes: &[u8]) -> Result<Vec<u8>, capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id != SPEAKER_APP_ID {
            return Err(capnp::Error::failed(
                "speaker groups are managed only by the certified Settings application".into(),
            ));
        }
        if bytes.len() > runtime_collab::MAX_SPEAKER_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "speaker command exceeds the local protocol bound".into(),
            ));
        }
        let command = serde_json::from_slice::<runtime_collab::SpeakerCommand>(bytes)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        if !command.valid() {
            return Err(capnp::Error::failed(
                "speaker command is structurally invalid".into(),
            ));
        }
        let speakers = self.state.services.speakers.as_ref().ok_or_else(|| {
            capnp::Error::failed("speaker groups are unsupported by this host".into())
        })?;
        let reply = serde_json::to_vec(&speakers.command(command))
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        if reply.len() > runtime_collab::MAX_SPEAKER_WIRE_BYTES {
            return Err(capnp::Error::failed(
                "speaker reply exceeds the local protocol bound".into(),
            ));
        }
        Ok(reply)
    }

    /// Up to `max` admitted changes to `artifact` before `before_seq`, and
    /// whether that is all there is. Where local history ends, the
    /// channel's other devices are asked for what lies before it; their
    /// answer lands in durable history, so the page is reported as not
    /// exhausted and the endpoint's next read picks it up.
    pub(super) fn history(
        &self,
        artifact: ArtifactId,
        before_seq: u64,
        max: u32,
    ) -> Result<(Vec<Vec<u8>>, bool), capnp::Error> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let broker_channels = &self.state.broker_channels;
        let (ops, mut exhausted) = read_scrollback(broker_channels, artifact, before_seq, max);
        if exhausted {
            let peer_bound = ops
                .iter()
                .filter_map(|bytes| postcard::from_bytes::<SignedOp>(bytes).ok())
                .map(|op| op.seq)
                .min()
                .unwrap_or(before_seq);
            let ask = broker_channels
                .lock()
                .is_ok_and(|mut ch| ch.request_peer_history(artifact, peer_bound));
            if ask
                && self.state.publisher().scrollback_request(
                    &route,
                    artifact,
                    peer_bound,
                    u16::try_from(max).unwrap_or(u16::MAX),
                )
            {
                exhausted = false;
            }
        }
        Ok((ops, exhausted))
    }

    /// The task runs the workspace hands this delegated agent from now on.
    /// A replacement subscription is handed the approved run again.
    pub(super) fn task_run_feed(&self) -> Result<Feed<TaskRunEnvelope>, capnp::Error> {
        if self.state.services.legacy_workspace.is_none()
            && !self.state.task_admitted_room(&self.route().cluster_id)
        {
            return Err(capnp::Error::failed(
                "task execution service unsupported".into(),
            ));
        }
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let agent_id = current.agent_id.ok_or_else(|| {
            capnp::Error::failed(
                "only an admitted delegated agent may subscribe to task runs".into(),
            )
        })?;
        let (tx, rx) = mpsc::unbounded_channel();
        self.state
            .task_subscribers
            .lock()
            .map_err(|_| capnp::Error::failed("task subscription registry unavailable".into()))?
            .insert(
                self.peer_id,
                TaskSubscription {
                    agent_id,
                    cluster_id: self.route().cluster_id,
                    tx,
                },
            );
        // A replacement stream needs the canonical approved run again, even
        // when the authenticated connection itself did not change.
        if let Ok(mut runs) = self.state.pending_task_runs.lock() {
            runs.retain(|_, run| run.peer_id != self.peer_id);
        }
        self.state.local_turn.notify_one();
        let feed = Feed::new(self.host_guard(), rx);
        self.state.refresh_native_task_delivery();
        Ok(feed)
    }

    /// One of this delegated agent's calls about the task run `run_id` it
    /// holds under `lease`.
    pub(super) fn task_call(&self, run_id: u64, lease: String, call: TaskCall) -> TaskAnswer {
        let agent_id = match self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)
            .and_then(|current| {
                current.agent_id.ok_or_else(|| {
                    capnp::Error::failed("only an agent may act on a task run".into())
                })
            }) {
            Ok(agent_id) => agent_id,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        let answer = match call {
            TaskCall::Invoke(step) => return self.invoke_task_step(agent_id, run_id, lease, step),
            TaskCall::Report(update) => {
                serde_json::from_str::<runtime_collab::TaskRunUpdate>(&update)
                    .map_err(|error| capnp::Error::failed(format!("invalid task update: {error}")))
                    .and_then(|update| {
                        self.state
                            .record_task_run_update(
                                self.peer_id,
                                &agent_id,
                                run_id,
                                &lease,
                                &update,
                            )
                            .map_err(capnp::Error::failed)
                    })
                    .map(|()| Reply::Done)
            }
            TaskCall::Cancelled => self.task_cancellation(&agent_id, run_id, &lease),
        };
        Box::pin(std::future::ready(answer))
    }

    fn task_cancellation(
        &self,
        agent_id: &str,
        run_id: u64,
        lease: &str,
    ) -> Result<Reply, capnp::Error> {
        let cancelled = self
            .state
            .task_run_cancelled(self.peer_id, agent_id, run_id, lease)
            .map_err(capnp::Error::failed)?;
        let run = self
            .state
            .task_run_for(self.peer_id, agent_id, run_id, lease)
            .map_err(capnp::Error::failed)?;
        let control = self
            .state
            .native_task_status(&run, lease)
            .map_err(capnp::Error::failed)?
            .control
            .map(|control| serde_json::to_string(&control))
            .transpose()
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        Ok(Reply::TaskCancelled { cancelled, control })
    }

    /// Start `step` for the task run, as a native task's re-ask when it
    /// names the invocation it runs under.
    fn invoke_task_step(
        &self,
        agent_id: String,
        run_id: u64,
        lease: String,
        step: TaskStep,
    ) -> TaskAnswer {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let caller = super::durable_local_invocation::LocalCaller::capture(self).ok();
        let route = self.route();
        let identity = self.verified_identity.clone();
        Box::pin(async move {
            if !step.provider.is_empty() && step.peer_id != 0 {
                return Err(capnp::Error::failed(
                    "task target mixes live and stable provider identities".into(),
                ));
            }
            let target = runtime_collab::TaskAction {
                peer_id: step.peer_id,
                provider: &step.provider,
                resource: &step.resource,
                action: &step.action,
                argument: step.argument.as_deref(),
            };
            let provenance =
                super::native_task_execution::child_provenance(&lease, &step.invocation, caller)?;
            let (id, update) = if let Some((caller, invocation)) = &provenance {
                let reask = super::native_task_execution::ChildReask {
                    peer_id,
                    agent_id: &agent_id,
                    run_id,
                    lease: &lease,
                    invocation: *invocation,
                    target,
                    caller,
                };
                let wait = super::native_task_execution::ChildWait::new(
                    step.wait_ms,
                    step.seen.as_deref(),
                )?;
                state
                    .invoke_native_task_child(&reask, wait, (&route, &identity))
                    .await?
            } else {
                if !step.provider.is_empty() {
                    return Err(capnp::Error::failed(
                        "closed-app activation requires a native task's authenticated custody"
                            .into(),
                    ));
                }
                let job = state
                    .start_task_action(
                        peer_id,
                        &agent_id,
                        run_id,
                        &lease,
                        target,
                        (None, (false, None)),
                    )
                    .map_err(capnp::Error::failed)?
                    .start(&state)
                    .await
                    .map_err(capnp::Error::failed)?;
                let update = state.invocation_status(job.id).ok_or_else(|| {
                    capnp::Error::failed("accepted task action has no broker state".into())
                })?;
                (job.id, update)
            };
            let update = serde_json::to_string(&update)
                .map_err(|error| capnp::Error::failed(error.to_string()))?;
            Ok(Reply::Invoked { id, update })
        })
    }

    /// Answer the app's remote-action `frame` with the frame its caller,
    /// door, grants and the person decide.
    pub(super) fn remote_action_dispatch(&self, frame: &[u8]) -> Answer<Vec<u8>> {
        use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
        let encode = |message: RemoteActionMessage| {
            RemoteActionFrame::new(message)
                .encode()
                .map_err(|error| capnp::Error::failed(error.to_string()))
        };
        let answer = match self.remote_action_answer(frame) {
            Ok(answer) => answer,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        Box::pin(async move { encode(answer.await?) })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one admission: caller, door, grants and the person's questions, in order"
    )]
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the action host moves into whichever answer serves the request"
    )]
    fn remote_action_answer(
        &self,
        frame: &[u8],
    ) -> Result<Answer<tilo_actions::remote::RemoteActionMessage>, capnp::Error> {
        use tilo_actions::remote::RemoteActionFrame;
        let original_time = self
            .state
            .registered_policy_time()
            .map_err(capnp::Error::failed)?;
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let request = RemoteActionFrame::decode(frame)
            .map_err(|error| capnp::Error::failed(error.to_string()))?;
        let route = self.route();
        let person = current.caller(&route.cluster_id);
        // An app's request continuing an invocation someone asked of it (an
        // edit its effect submits, another app's action its effect calls) is
        // decided as that invocation's caller, under its custody; its open
        // of an application, else as the person whose press it follows.
        // Anything else, including what continues an invocation the app made
        // of itself, is the app's own door's to decide.
        let (decided, continued_origin) = if current.agent_id.is_none() {
            match self.state.app_request_caller(
                self.peer_id,
                &route.app_id,
                &person,
                &request.message,
            ) {
                Some(Ok((caller, origin))) => (Some(Ok(caller)), origin),
                Some(Err(refused)) => (Some(Err(refused)), None),
                None => (None, None),
            }
        } else {
            (
                self.state.continued_caller(self.peer_id, &request.message),
                None,
            )
        };
        let caller = match decided {
            Some(Ok(caller)) => caller,
            Some(Err(refused)) => return Ok(ready(*refused)),
            None if current.agent_id.is_some() => person.clone(),
            None => match self
                .state
                .app_door(&person, &route.app_id, &request.message)
            {
                Ok(caller) => caller,
                Err(refusal) => return Ok(ready(*refusal)),
            },
        };
        if let tilo_actions::remote::RemoteActionMessage::CatalogueRequest { correlation, query } =
            &request.message
            && query.view_resource.is_some()
        {
            return Ok(self.resource_view_query(*correlation, query.clone()));
        }

        if tilo_actions::remote::refuse(
            &request.message,
            tilo_actions::remote::ActionHostError::UnknownAction,
        )
        .is_none()
        {
            return Err(capnp::Error::failed(
                "only remote-action requests are answered".into(),
            ));
        }
        // An application reaching for another app's action it holds no
        // grant for: the person using it is asked first, or it is refused.
        let consent = match (caller.principal(), &request.message) {
            (
                tilo_actions::remote::CallerPrincipal::App(app_id),
                tilo_actions::remote::RemoteActionMessage::Invoke(invoke),
            ) => match self.state.app_action_consent(&person, app_id, invoke) {
                app_consent::ConsentStep::Serve => None,
                app_consent::ConsentStep::Refuse => {
                    return refusal(&request.message);
                }
                app_consent::ConsentStep::Ask(consent) => Some(consent),
            },
            _ => None,
        };
        // A person's answer to what an issue's run asks them is theirs to
        // give on the host's sheet, never the app's.
        let question = match &request.message {
            tilo_actions::remote::RemoteActionMessage::Invoke(invoke) => {
                match self.state.run_question(&route.app_id, &caller, invoke) {
                    run_question::AnswerStep::Serve => None,
                    run_question::AnswerStep::Refuse => {
                        return refusal(&request.message);
                    }
                    run_question::AnswerStep::Ask(question) => Some(question),
                }
            }
            _ => None,
        };
        let admitting_person = current.principal_id.clone();
        let host = action_host::ConnectionActionHost {
            state: Arc::clone(&self.state),
            own_peer: self.peer_id,
            origin: continued_origin.unwrap_or_else(|| InvocationCaller::Local {
                route,
                time: original_time,
                admitted: Box::new(current),
            }),
        };
        if let Some(question) = question {
            return Ok(super::run_question::serve_answered(
                Arc::clone(&self.state),
                question,
                (host, caller),
                request.message,
            ));
        }
        if let Some(consent) = consent {
            return Ok(super::app_consent::serve_after_consent(
                Arc::clone(&self.state),
                consent,
                host,
                caller,
                request.message,
            ));
        }
        // An invocation the caller may not confirm itself is put to the
        // person who admitted the connection, on their own device.
        let answer = super::witness::serve_answering(
            Arc::clone(&self.state),
            host,
            caller,
            admitting_person,
            request.message,
        );
        // Awaited on this local set, which is also where an application's
        // directory answers; the connection's other calls keep flowing.
        Ok(Box::pin(async move {
            answer
                .await
                .ok_or_else(|| capnp::Error::failed("the action host dropped the request".into()))
        }))
    }

    /// Start handing this app the invocations of its published actions,
    /// replacing any stream it held; `stable` asks for stable invocation ids.
    pub(super) fn invocation_feed(
        &self,
        stable: bool,
    ) -> Result<InvocationStreamServer, capnp::Error> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        // Stream replacement and resolved dispatch share the admission boundary:
        // an admitted old token must never dispatch to a replacement stream.
        let Ok(admission) = self.state.admission_lock.lock() else {
            return Err(capnp::Error::failed(
                "invocation admission registry unavailable".into(),
            ));
        };
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        if let Ok(mut directories) = self.state.canonical_directories.lock() {
            directories.stream_replaced(self.peer_id);
        } else {
            return Err(capnp::Error::failed(
                "canonical stream registry unavailable".into(),
            ));
        }
        if let Ok(mut capabilities) = self.state.stable_invocation_subscribers.lock() {
            if stable {
                capabilities.insert(self.peer_id);
            } else {
                capabilities.remove(&self.peer_id);
            }
        } else {
            return Err(capnp::Error::failed(
                "application capability registry unavailable".into(),
            ));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        if let Ok(mut subscribers) = self.state.invocation_subscribers.lock() {
            subscribers.insert(self.peer_id, tx);
        } else {
            return Err(capnp::Error::failed(
                "invocation subscriber registry unavailable".into(),
            ));
        }
        drop(admission);
        self.state.invocation_streams_changed();
        // The app the launcher attested, else the one this channel names.
        let (app, room) = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .map_or_else(
                || {
                    let route = self.route();
                    (route.app_id, route.cluster_id)
                },
                |app| (app.app_id.clone(), app.workspace_id.clone()),
            );
        let delivered = invocation_delivery_notice(&self.state.services, app, room);
        Ok(InvocationStreamServer {
            host_guard: HostWorkerReadGuard::for_channel(
                &self.state,
                self.peer_id,
                &self.verified_identity,
            ),
            rx: invocation_queue(self.peer_id, rx),
            delivered,
        })
    }

    /// What the app says, or asks, of the invocation `id` it was handed.
    pub(super) fn invocation_call(&self, id: u64, call: InvocationCall) -> Answer<Reply> {
        match call {
            InvocationCall::Complete { code, message } => {
                self.complete_invocation(id, code, message)
            }
            InvocationCall::Update(update) => {
                let state = self.state.clone();
                let peer_id = self.peer_id;
                let admission = Some((self.route(), self.verified_identity.clone()));
                Box::pin(async move {
                    let update = serde_json::from_str(&update).map_err(|error| {
                        capnp::Error::failed(format!("invalid invocation update: {error}"))
                    })?;
                    state
                        .apply_invocation_update(peer_id, id, update, admission)
                        .await?;
                    Ok(Reply::Done)
                })
            }
            InvocationCall::Cancelled => self.invocation_cancellation(id),
        }
    }

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the pending entry borrows the registry until it is staged; the lint misreads the borrow"
    )]
    fn complete_invocation(&self, id: u64, code: String, message: String) -> Answer<Reply> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let route = self.route();
        let identity = self.verified_identity.clone();
        let host_guard = HostWorkerReadGuard::for_channel(&state, peer_id, &identity);
        Box::pin(async move {
            let _owner_order = if state.durable_owner_required
                || state.publisher.requires_owner_for(&route.cluster_id)
            {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            if state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id)
            {
                state.revalidate_admission(&route, &identity)?;
            }
            let completion = InvocationCompletion { code, message };
            if !completion.code.is_empty() && completion.message.trim().is_empty() {
                return Err(capnp::Error::failed(
                    "failed invocation completion requires a message".into(),
                ));
            }
            let (staged, update) = {
                let registry = state
                    .pending_invocations
                    .lock()
                    .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?;
                let pending = registry
                    .get(&id)
                    .filter(|pending| pending.peer_id == peer_id)
                    .ok_or_else(|| {
                        capnp::Error::failed(
                            "an application may complete only its own invocation".into(),
                        )
                    })?;
                if completion.code.is_empty() {
                    pending
                        .result
                        .validate(&tilo_actions::ActionOutput::default())
                        .map_err(|error| {
                            capnp::Error::failed(format!(
                                "legacy completion violated its published result contract: {error}"
                            ))
                        })?;
                }
                let invocation = pending.update.invocation();
                let update = if completion.code.is_empty() {
                    tilo_actions::InvocationUpdate::Succeeded {
                        invocation,
                        output: tilo_actions::ActionOutput::default(),
                    }
                } else {
                    tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: completion.code.clone(),
                            message: completion.message.clone(),
                            details: None,
                            retryable: false,
                        },
                    }
                };
                (pending.clone(), update)
            };
            state
                .commit_effect_before_terminal(&staged, &update)
                .await?;
            HostWorkerReadGuard::check(host_guard.as_ref())?;
            if state.durable_owner_required || state.publisher.requires_owner_for(&route.cluster_id)
            {
                state.revalidate_admission(&route, &identity)?;
            }
            state
                .settle_definite_outcome(peer_id, id, &staged.update, &update)
                .await?;
            let pending = state
                .pending_invocations
                .lock()
                .map_err(|_| capnp::Error::failed("invocation registry unavailable".into()))?
                .remove(&id)
                .ok_or_else(|| capnp::Error::failed("unknown invocation id".into()))?;
            state.publish_activity(pending.activity.as_ref(), &update);
            state.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            state.record_effect_receipt(&pending, &update);
            Ok(Reply::Done)
        })
    }

    fn invocation_cancellation(&self, id: u64) -> Answer<Reply> {
        let checked = HostWorkerReadGuard::check(
            HostWorkerReadGuard::for_channel(&self.state, self.peer_id, &self.verified_identity)
                .as_ref(),
        );
        if let Err(error) = checked {
            return Box::pin(std::future::ready(Err(error)));
        }
        let owns = self
            .state
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.peer_id == self.peer_id))
            .unwrap_or(false);
        if !owns {
            return Box::pin(std::future::ready(Err(capnp::Error::failed(
                "unknown invocation id".into(),
            ))));
        }
        let cancelled = self
            .state
            .cancellation_requests
            .lock()
            .is_ok_and(|requests| requests.contains(&id));
        if !cancelled {
            return ready(Reply::Cancelled(false));
        }
        // The application hears of the request once it is durable.
        let durable = self.state.invocation_in_flight(id);
        Box::pin(async move {
            durable.wait().await.map_err(capnp::Error::failed)?;
            Ok(Reply::Cancelled(true))
        })
    }

    /// Publish the app's action surface on this channel: a whole one, or a
    /// change to the one it published, applied all or not at all.
    pub(super) fn publish_surface(
        &self,
        claimed_app: &str,
        change: SurfaceChange,
    ) -> Result<(), capnp::Error> {
        // No app changes another app's route: a channel held to read refuses.
        self.writable()?;
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id.is_empty() || claimed_app != route.app_id {
            return Err(capnp::Error::failed(
                "a published surface's appId must match the certified connection route".into(),
            ));
        }
        let peer_id = self.peer_id;
        let unavailable = || capnp::Error::failed("action surface registry unavailable".into());
        let (actions, table) = match change {
            SurfaceChange::Whole(rows) => (validate_published_surface(rows)?, None),
            SurfaceChange::Delta(delta) => {
                // Apply to a copy: a delta that does not apply, or whose
                // result is not a valid surface, changes nothing.
                let mut table = self
                    .state
                    .surface_tables
                    .lock()
                    .map_err(|_| unavailable())?
                    .get(&peer_id)
                    .cloned()
                    .unwrap_or_default();
                table.apply(&delta).map_err(|reason| {
                    capnp::Error::failed(format!("action surface delta refused: {reason}"))
                })?;
                (validate_published_surface(table.actions())?, Some(table))
            }
        };
        let offers = !actions.is_empty();
        let surface = PublishedSurface {
            peer_id,
            app_id: route.app_id.clone(),
            route,
            actions,
            live: true,
        };
        {
            let mut tables = self
                .state
                .surface_tables
                .lock()
                .map_err(|_| unavailable())?;
            match table {
                Some(table) => {
                    tables.insert(peer_id, table);
                }
                // A whole surface supersedes any delta state: a later delta
                // without `reset` finds no shapes and is refused, so the
                // publisher resets.
                None => {
                    tables.remove(&peer_id);
                }
            }
        }
        self.state.launchable.retain(&surface);
        let previous = self
            .state
            .surfaces
            .lock()
            .map_err(|_| unavailable())?
            .insert(peer_id, surface.clone());
        self.state
            .advance_witness_publication(peer_id, WitnessPublicationKind::Surface);
        self.state
            .surface_republished(peer_id, &surface, previous.as_ref());
        if offers {
            self.state.launch_published(peer_id);
        }
        Ok(())
    }

    /// News of the record's content from now on.
    pub(super) fn subscribe_content(
        &self,
    ) -> Result<Feed<crate::record_content::ContentNews>, capnp::Error> {
        let (route, content) = self.record_content()?;
        Ok(Feed::new(self.host_guard(), content.subscribe(&route)))
    }
}

impl CapnpCollabService {
    /// Admit `join` and hold its channel for this connection: the identity
    /// is verified, a room still being joined or placed is waited for, the
    /// route's access and the participant's delegation are decided, and the
    /// route is held and published.
    #[expect(
        clippy::too_many_lines,
        reason = "one ordered join admission: identity, route, custody and channel handle"
    )]
    pub(super) async fn join(&self, join: Join) -> Result<CapnpChannelHandle, capnp::Error> {
        let state = self.state.clone();
        let peer_id = self.peer_id;
        let mut phases = JoinPhases::start();
        let Join {
            channel: channel_id,
            identity,
            participant,
            record_kind,
        } = join;
        let route = CrdtRoute::from_channel_key(&channel_id);
        let mut verified_identity = admit_join_presentation(
            &route,
            &identity,
            state.membership.as_ref(),
            state
                .trusted_devices
                .as_ref()
                .map(|devices| devices.as_slice()),
            state.certificate_authority.get().map(Arc::as_ref),
            self.worker
                .as_ref()
                .and_then(|worker| worker.host_scope.as_ref()),
        )
        .map_err(|error| {
            capnp::Error::failed(format!("collaboration join identity rejected: {error}"))
        })?;
        phases.mark("verify");
        // A room still being joined or placed on its host holds the join
        // until apps can open documents in it, whichever client asks.
        if let Some(memberships) = state.services.workspace_memberships.clone() {
            let mut status = memberships.room_status(&route.cluster_id);
            while matches!(status, runtime_collab::RoomStatus::Waiting { .. }) {
                status = memberships
                    .room_status_change(&route.cluster_id, status)
                    .await;
            }
            if let runtime_collab::RoomStatus::Refused(reason) = status {
                return Err(capnp::Error::failed(reason));
            }
        }
        // An attested launch joins its own route; another app's only to
        // read a record whose kinds it declared it reads.
        let route_access = match self.lease.local_media.as_ref() {
            Some(app) => {
                let person_room = verified_identity
                    .verified
                    .owns_person_room(&route.cluster_id);
                let now = state.policy_now_ns().map_err(capnp::Error::failed)? / 1_000_000_000;
                attested_route_access(&app.app_id, &route, person_room, || {
                    identity_reads_record(
                        state.membership.as_ref(),
                        &verified_identity,
                        &route,
                        now,
                    )
                })
                .map_err(capnp::Error::failed)?
            }
            None => RouteAccess::Own,
        };
        if let Some(worker) = &self.worker {
            if route != worker.route
                || verified_identity.verified.user_public_key != worker.root
                || verified_identity.verified.device_public_key != worker.device
            {
                return Err(capnp::Error::failed(
                    "worker descriptor route or certificate mismatch".into(),
                ));
            }
            if let Some(registration) = &worker.registration {
                if registration.route != route
                    || verified_identity.certificate.as_ref() != Some(&registration.certificate)
                {
                    return Err(capnp::Error::failed(
                        "registered worker exact certificate changed".into(),
                    ));
                }
                verified_identity.worker_registration = Some(registration.clone());
            }
        } else if state.durable_owner_required && !route.app_id.is_empty() {
            return Err(capnp::Error::failed(
                "application requires host-owned worker descriptor admission".into(),
            ));
        }
        let record_kind = route_record_kind(
            &route,
            joined_record_kind(record_kind.as_deref().unwrap_or_default())?,
        )?;
        let person = matches!(participant, Participant::Person { .. });
        if (verified_identity.host_scope.is_some()
            || verified_identity.worker_registration.is_some())
            && !person
        {
            return Err(capnp::Error::failed(
                "host worker cannot delegate an agent identity".into(),
            ));
        }
        match participant {
            Participant::Person {
                handle: participant_handle,
            } => {
                // A Person handle is presentation, not its certified identity.
                let participant_handle = participant_handle
                    .trim()
                    .chars()
                    .take(64)
                    .collect::<String>();
                if !participant_handle.is_empty() {
                    verified_identity.display_handle = Some(participant_handle);
                }
            }
            Participant::Agent {
                id: participant_handle,
            } => {
                // Unlike a display handle, this names the exact workspace
                // principal. Normalize as enrollment does, then validate
                // its byte bound; truncation can silently select another
                // admitted Agent that shares the same display prefix.
                let participant_handle = participant_handle.trim();
                // Bound allocation before Unicode case normalization;
                // every normalized character consumes at least one byte.
                if participant_handle.chars().take(129).count() > 128 {
                    return Err(capnp::Error::failed(
                        "invalid canonical workspace agent identifier".into(),
                    ));
                }
                let participant_handle = participant_handle.to_lowercase();
                tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RemoveAgent {
                    agent: participant_handle.clone(),
                }
                .validate()
                .map_err(capnp::Error::failed)?;
                let membership = state.membership.as_ref().ok_or_else(|| {
                    capnp::Error::failed("agent session has no workspace authority".into())
                })?;
                let now = state.policy_now_ns().map_err(capnp::Error::failed)? / 1_000_000_000;
                membership
                    .decide_in_room(&route.cluster_id, |workspace| {
                        admit_agent_delegation_at(
                            workspace,
                            &mut verified_identity,
                            &participant_handle,
                            now,
                        )
                    })
                    .and_then(std::convert::identity)
                    .map_err(|error| {
                        capnp::Error::failed(format!("agent collaboration join rejected: {error}"))
                    })?;
            }
        }
        // An app the person opens, launched or restored into a tab, is
        // let author the room as its person may let it: asked where
        // every opening of an app joins. The join goes on meanwhile:
        // the app asks for the room's keys again until the grant is in.
        if person
            && verified_identity.agent_id.is_none()
            && let Some(certificate) = verified_identity.certificate.clone()
        {
            let state = Arc::clone(&state);
            let route = route.clone();
            tokio::task::spawn_local(async move {
                if let Err(error) = state.admit_opening_app(&route, &certificate).await {
                    tracing::warn!(
                        room = %route.cluster_id,
                        app = %route.app_id,
                        %error,
                        "an opening app was not let author its room"
                    );
                }
            });
        }
        tracing::info!(
            target: "tilo_collab_probe",
            channel = %channel_id,
            peer_id,
            role = ?verified_identity.role,
            principal = %verified_identity.principal_id,
            acting_as = ?verified_identity.agent_id,
            device = ?{
                collab_types::Ed25519PublicKey::from_bytes(
                    verified_identity.verified.device_public_key,
                )
            },
            "PROBE join_channel",
        );
        if self.worker.is_none()
            && person
            && let Some(registration) = host_workers::packaged_app_registration(
                self.app_worker.as_ref(),
                &route,
                &verified_identity,
            )
        {
            verified_identity.worker_registration = Some(registration);
        }
        phases.mark("admit");
        let ensured = channel_id.clone();
        let retained = verified_identity.clone();
        let hold = self.holds.hold(&state, &self.directories, peer_id, &route);
        phases.mark("hold");
        let hold_id = hold.id();
        let handle = CapnpChannelHandle {
            person_chat_views: RefCell::default(),
            committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
            state: state.clone(),
            directories: Rc::clone(&self.directories),
            channel_id,
            peer_id: hold_id,
            connection_id: peer_id,
            verified_identity,
            record_kind,
            route_access,
            lease: Some(Rc::clone(&self.lease)),
            _hold: Some(hold),
        };
        // Refused, the handle and its hold drop here and withdraw again.
        state.retain_host_worker(hold_id, &route, &retained)?;
        phases.mark("retain-worker");
        state.ensure_channel(&ensured);
        phases.mark("ensure-channel");
        state
            .publisher()
            .local_route(&crate::route::CrdtRoute::from_channel_key(&ensured));
        phases.mark("publish-route");
        phases.report(&ensured);
        Ok(handle)
    }
}

/// One endpoint's connection: the handles it holds, its live subscriptions,
/// and the queue every outgoing frame takes.
pub(super) struct WireConnection {
    /// What joins channels for this connection. A connection without one
    /// only holds the channels opened for it.
    service: Option<Rc<CapnpCollabService>>,
    channels: RefCell<HashMap<Handle, Rc<ChannelSession>>>,
    next_handle: Cell<u64>,
    subscriptions: RefCell<HashMap<u64, (Handle, tokio::task::AbortHandle)>>,
    frames: mpsc::UnboundedSender<HostFrame>,
}

impl Drop for WireConnection {
    fn drop(&mut self) {
        for (_, (_, task)) in self.subscriptions.get_mut().drain() {
            task.abort();
        }
    }
}

impl WireConnection {
    /// A connection and the queue of the frames it sends.
    pub(super) fn new(
        service: Option<Rc<CapnpCollabService>>,
    ) -> (Rc<Self>, mpsc::UnboundedReceiver<HostFrame>) {
        let (frames, outgoing) = mpsc::unbounded_channel();
        let connection = Rc::new(Self {
            service,
            channels: RefCell::default(),
            next_handle: Cell::new(1),
            subscriptions: RefCell::default(),
            frames,
        });
        (connection, outgoing)
    }

    /// Hold `session` for this connection under a fresh handle.
    pub(super) fn open(&self, session: ChannelSession) -> Handle {
        let handle = Handle(self.next_handle.get());
        self.next_handle.set(handle.0.wrapping_add(1));
        self.channels.borrow_mut().insert(handle, Rc::new(session));
        handle
    }

    fn channel(&self, handle: Handle) -> Result<Rc<ChannelSession>, String> {
        self.channels
            .borrow()
            .get(&handle)
            .cloned()
            .ok_or_else(|| format!("no channel {} on this connection", handle.0))
    }

    /// Answer one call. A reply, and the events a subscription it starts
    /// delivers, take the outgoing queue in that order; a call that waits (a
    /// join, a room's change, a document change the room's owner commits
    /// first) answers when done.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per request answered once its work is done"
    )]
    pub(super) fn call(self: &Rc<Self>, Call { id, request }: Call) {
        let answer: std::pin::Pin<Box<dyn Future<Output = Result<Reply, String>>>> = match request {
            Request::Publish {
                channel,
                publication: Publication::Document(envelope),
            } => {
                let channel = self.channel(channel);
                Box::pin(async move {
                    channel?
                        .publish_document(*envelope)
                        .await
                        .map(|()| Reply::Done)
                        .map_err(|error| error.to_string())
                })
            }
            Request::Join(join) => {
                let (service, connection) = (self.service.clone(), Rc::downgrade(self));
                Box::pin(async move {
                    let service = service.ok_or("this connection joins no channels")?;
                    let channel = service
                        .join(join)
                        .await
                        .map_err(|error| error.to_string())?;
                    let connection = connection.upgrade().ok_or("the connection closed")?;
                    Ok(Reply::Opened(
                        connection.open(ChannelSession::new(Rc::new(channel))),
                    ))
                })
            }
            Request::Room { channel, seen } => {
                let memberships = self
                    .service
                    .as_ref()
                    .and_then(|service| service.state.services.workspace_memberships.clone());
                let room = CrdtRoute::from_channel_key(&channel).cluster_id;
                Box::pin(async move {
                    Ok(Reply::Room(match (memberships, seen) {
                        (Some(memberships), Some(seen)) => {
                            memberships.room_status_change(&room, seen).await
                        }
                        (Some(memberships), None) => memberships.room_status(&room),
                        (None, _) => runtime_collab::RoomStatus::default(),
                    }))
                })
            }
            Request::Publish {
                channel,
                publication:
                    Publication::Effect {
                        resource,
                        action,
                        update,
                    },
            } => {
                let recorded = self.channel(channel).and_then(|session| {
                    let update = serde_json::from_str(&update).map_err(|e| e.to_string())?;
                    session
                        .record_local_effect_dispatch(&resource, &action, update)
                        .map_err(|e| e.to_string())
                });
                Box::pin(async move {
                    recorded?.await.map_err(|error| error.to_string())?;
                    Ok(Reply::Done)
                })
            }
            Request::Changed { channel, of, after } => {
                let change = self
                    .channel(channel)
                    .and_then(|session| session.change(of, after).map_err(|e| e.to_string()));
                Box::pin(async move { Ok(Reply::Changed(change?.await)) })
            }
            Request::Command {
                channel,
                service,
                body,
                on_behalf_of,
            } => match self.channel(channel) {
                Ok(session) => session.command(service, &body, &on_behalf_of),
                Err(error) => Box::pin(std::future::ready(Err(error))),
            },
            Request::Invocation {
                channel,
                id: invocation,
                call,
            } => {
                let answer = self
                    .channel(channel)
                    .map(|session| session.invocation_call(invocation, call));
                Box::pin(async move { answer?.await.map_err(|error| error.to_string()) })
            }
            Request::Task {
                channel,
                run_id,
                lease,
                call,
            } => {
                let answer = self
                    .channel(channel)
                    .map(|session| session.task_call(run_id, lease, call));
                Box::pin(async move { answer?.await.map_err(|error| error.to_string()) })
            }
            request => {
                let result = self.dispatch(id, request);
                let _ = self.frames.send(HostFrame::Reply { id, result });
                return;
            }
        };
        let frames = self.frames.clone();
        tokio::task::spawn_local(async move {
            let result = answer.await;
            let _ = frames.send(HostFrame::Reply { id, result });
        });
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per request the host answers at once"
    )]
    fn dispatch(self: &Rc<Self>, id: u64, request: Request) -> Result<Reply, String> {
        match request {
            Request::Publish {
                channel,
                publication: Publication::Presence(presence),
            } => {
                let (key, payload) = crate::collab_presence::from_peer(presence)
                    .ok_or_else(|| "presence does not decode".to_owned())?;
                self.channel(channel)?.presence.publish(key, payload)?;
                Ok(Reply::Done)
            }
            Request::Publish {
                publication: Publication::Document(_) | Publication::Effect { .. },
                ..
            }
            | Request::Join(_)
            | Request::Room { .. }
            | Request::Command { .. }
            | Request::Changed { .. }
            | Request::Task { .. }
            | Request::Invocation { .. } => Err("answered when done".into()),
            Request::Publish {
                channel,
                publication: Publication::Report(report),
            } => {
                let session = self.channel(channel)?;
                let reported = session.report(&session.route().app_id, report);
                drop(session);
                reported.map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Publish {
                channel,
                publication: Publication::Actions(actions),
            } => {
                let actions = serde_json::from_str(&actions)
                    .map_err(|error| format!("invalid published actions: {error}"))?;
                let session = self.channel(channel)?;
                let published =
                    session.publish_surface(&session.route().app_id, SurfaceChange::Whole(actions));
                drop(session);
                published.map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Publish {
                channel,
                publication: Publication::Surface(delta),
            } => {
                let delta = serde_json::from_str(&delta)
                    .map_err(|error| format!("invalid surface change: {error}"))?;
                let session = self.channel(channel)?;
                let published =
                    session.publish_surface(&session.route().app_id, SurfaceChange::Delta(delta));
                drop(session);
                published.map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Publish {
                channel,
                publication: Publication::Content { hash, bytes },
            } => {
                self.channel(channel)?
                    .offer_content(&hash, &bytes)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Read {
                channel,
                read:
                    Read::History {
                        artifact,
                        before_seq,
                        max,
                    },
            } => {
                let (ops, exhausted) = self
                    .channel(channel)?
                    .history(artifact, before_seq, max)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::History { ops, exhausted })
            }
            Request::Read {
                channel,
                read: Read::Document,
            } => {
                self.channel(channel)?
                    .ask_for_document()
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Read {
                channel,
                read: Read::Content { hash },
            } => {
                self.channel(channel)?
                    .want_content(&hash)
                    .map_err(|error| error.to_string())?;
                Ok(Reply::Done)
            }
            Request::Subscribe { channel, topic } => {
                if self.subscriptions.borrow().contains_key(&id) {
                    return Err(format!("subscription {id} is already live"));
                }
                let session = self.channel(channel)?;
                let frames = self.frames.clone();
                let failed = |error: capnp::Error| error.to_string();
                let task = match topic {
                    Topic::Presence => {
                        let feed = session.presence.subscribe()?;
                        tokio::task::spawn_local(pump(feed, id, frames, presence_event))
                    }
                    Topic::Ops => {
                        let feed = session.subscribe_ops().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |envelope: OpEnvelope| {
                            Some(Event::Op {
                                op: envelope.op_bytes,
                                from_user_id: envelope.from_user_id,
                                admission: envelope.admission,
                            })
                        }))
                    }
                    Topic::Context => {
                        let feed = session.context_feed().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |update| {
                            Some(Event::Context(update))
                        }))
                    }
                    Topic::Invocations { stable_ids } => {
                        let stream = session.invocation_feed(stable_ids).map_err(failed)?;
                        tokio::task::spawn_local(pump_invocations(stream, id, frames))
                    }
                    Topic::TaskRuns => {
                        let feed = session.task_run_feed().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |run| {
                            task_run_json(run).ok().map(Event::TaskRun)
                        }))
                    }
                    Topic::Content => {
                        let feed = session.subscribe_content().map_err(failed)?;
                        tokio::task::spawn_local(pump(
                            feed,
                            id,
                            frames,
                            |news: crate::record_content::ContentNews| {
                                Some(Event::Content {
                                    hash: news.hash,
                                    state: news.state,
                                    bytes: news.bytes,
                                })
                            },
                        ))
                    }
                };
                self.subscriptions
                    .borrow_mut()
                    .insert(id, (channel, task.abort_handle()));
                Ok(Reply::Subscribed)
            }
            Request::Unsubscribe { subscription } => {
                let (_, task) = self
                    .subscriptions
                    .borrow_mut()
                    .remove(&subscription)
                    .ok_or_else(|| format!("no subscription {subscription}"))?;
                task.abort();
                Ok(Reply::Done)
            }
            Request::Close { handle } => {
                self.channels
                    .borrow_mut()
                    .remove(&handle)
                    .ok_or_else(|| format!("no channel {} on this connection", handle.0))?;
                self.subscriptions
                    .borrow_mut()
                    .retain(|_, (channel, task)| {
                        let keep = *channel != handle;
                        if !keep {
                            task.abort();
                        }
                        keep
                    });
                Ok(Reply::Done)
            }
        }
    }
}

/// Another participant's presence as the wire carries it; `None` for what
/// no app receives or what does not fit the wire.
fn presence_event(envelope: PresenceEnvelope) -> Option<Event> {
    if !envelope.payload.reaches_apps() {
        return None;
    }
    Some(Event::Presence {
        user_id: envelope.user_id,
        presence: collab_types::PeerPresence {
            key: envelope.key_bytes,
            payload: crate::collab_presence::payload_to_peer(&envelope.payload)?,
        },
    })
}

/// Deliver `feed` as the subscription `subscription`'s events until it
/// ends, then say why.
async fn pump<T>(
    mut feed: Feed<T>,
    subscription: u64,
    frames: mpsc::UnboundedSender<HostFrame>,
    event: impl Fn(T) -> Option<Event>,
) {
    let reason = loop {
        match feed.next().await {
            Ok(Some(item)) => {
                let Some(event) = event(item) else {
                    continue;
                };
                if frames
                    .send(HostFrame::Event {
                        subscription,
                        event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Ok(None) => break "the channel's fanout closed".to_owned(),
            Err(error) => break error.to_string(),
        }
    };
    let _ = frames.send(HostFrame::Ended {
        subscription,
        reason,
    });
}

async fn pump_invocations(
    stream: InvocationStreamServer,
    subscription: u64,
    frames: mpsc::UnboundedSender<HostFrame>,
) {
    let reason = loop {
        match stream.next().await.and_then(InvocationEnvelope::on_wire) {
            Ok(invocation) => {
                let event = Event::Invocation(invocation);
                if frames
                    .send(HostFrame::Event {
                        subscription,
                        event,
                    })
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => break error.to_string(),
        }
    };
    let _ = frames.send(HostFrame::Ended {
        subscription,
        reason,
    });
}

type NextCall<R> = (R, Result<Option<Call>, wire_frame::FrameError>);

async fn next_call<R: AsyncRead + Unpin>(mut reader: R) -> NextCall<R> {
    let call = FRAMING.read_value_async(&mut reader).await;
    (reader, call)
}

/// Serve `connection` until the endpoint hangs up. Must run on a
/// `LocalSet`; dropping the future ends the connection's subscriptions.
///
/// # Errors
///
/// A malformed frame or a failed write.
pub(super) async fn serve<R, W>(
    connection: Rc<WireConnection>,
    mut outgoing: mpsc::UnboundedReceiver<HostFrame>,
    reader: R,
    mut writer: W,
) -> Result<(), wire_frame::FrameError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;
    let mut next = Box::pin(next_call(reader));
    loop {
        tokio::select! {
            (reader, call) = &mut next => {
                let Some(call) = call? else {
                    return Ok(());
                };
                connection.call(call);
                next = Box::pin(next_call(reader));
            }
            Some(frame) = outgoing.recv() => {
                FRAMING.write_value_async(&mut writer, &frame).await?;
                writer.flush().await?;
            }
        }
    }
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod wire_tests;

/// An answer already given.
fn ready<T: 'static>(answer: T) -> Answer<T> {
    Box::pin(std::future::ready(Ok(answer)))
}

/// The app's remote-action `message` refused as not permitted.
fn refusal(
    message: &tilo_actions::remote::RemoteActionMessage,
) -> Result<Answer<tilo_actions::remote::RemoteActionMessage>, capnp::Error> {
    tilo_actions::remote::refuse(message, tilo_actions::remote::ActionHostError::NotPermitted)
        .map(ready)
        .ok_or_else(|| capnp::Error::failed("only remote-action requests are answered".into()))
}
