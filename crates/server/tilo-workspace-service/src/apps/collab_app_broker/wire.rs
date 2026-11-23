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

use crate::apps::BrokerError;
use std::cell::Cell;
use std::rc::Weak;

use runtime_collab::wire::{
    Answer as Answered, Ask, Call, Changes, CheckProgress, ContextUpdate, Event, FRAMING, Handle,
    HostFrame, InvocationCall, Join, LiveActivityCall, Operation, Publication, Read, Reply, Report,
    Service, SettingsCall, Topic,
};
use tokio::io::{AsyncRead, AsyncWrite};

use collab_types::MemberId;

use super::turns::answers_turns;
use super::*;

mod join;
mod presence;
pub(super) use presence::PresenceSession;
use presence::presence_event;

/// What a subscription receives from the broker's fanout. A host worker's
/// feed ends with an error once that worker is no longer current, and a
/// native window's once its workspace or document ownership ended, checked on
/// both sides of every wait.
pub(super) struct Feed<T> {
    native_guard: Option<NativeContextReadGuard>,
    rx: mpsc::UnboundedReceiver<T>,
}

impl<T> Feed<T> {
    pub(super) fn new(rx: mpsc::UnboundedReceiver<T>) -> Self {
        Self {
            native_guard: None,
            rx,
        }
    }

    pub(super) fn with_native_guard(mut self, guard: Option<NativeContextReadGuard>) -> Self {
        self.native_guard = guard;
        self
    }

    fn check(&self) -> Result<(), BrokerError> {
        NativeContextReadGuard::check(self.native_guard.as_ref())
    }

    /// The next item, or `None` when the fanout closed.
    pub(super) async fn next(&mut self) -> Result<Option<T>, BrokerError> {
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
    channel: Rc<JoinedChannel>,
    presence: PresenceSession,
    /// The app's settings, opened on its first call.
    settings: RefCell<Option<Rc<runtime_collab::settings::host::AppSettingsServer>>>,
    /// The app's live activity, opened on its first call; it ends when the
    /// app closes the channel.
    live_activity: RefCell<Option<Rc<runtime_collab::live_activity::LiveActivityServer>>>,
}

impl ChannelSession {
    pub(super) fn new(channel: Rc<JoinedChannel>) -> Self {
        let presence = PresenceSession::new(
            channel.state.clone(),
            channel.channel_id.clone(),
            channel.connection_id,
            channel.verified_identity.clone(),
        );
        Self {
            channel,
            presence,
            settings: RefCell::default(),
            live_activity: RefCell::default(),
        }
    }

    fn settings(&self) -> Result<Rc<runtime_collab::settings::host::AppSettingsServer>, String> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map_err(Refusal::refusal)?;
        if let Some(settings) = self.settings.borrow().as_ref() {
            return Ok(Rc::clone(settings));
        }
        let settings = Rc::new(
            app_settings_capability(&self.state, route, self.verified_identity.clone())
                .map_err(Refusal::refusal)?,
        );
        *self.settings.borrow_mut() = Some(Rc::clone(&settings));
        Ok(settings)
    }

    fn live_activity(
        &self,
    ) -> Result<Rc<runtime_collab::live_activity::LiveActivityServer>, String> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)
            .map_err(Refusal::refusal)?;
        if let Some(activity) = self.live_activity.borrow().as_ref() {
            return Ok(Rc::clone(activity));
        }
        let activity = Rc::new(
            live_activity_capability(&self.state, route, self.verified_identity.clone())
                .map_err(Refusal::refusal)?,
        );
        *self.live_activity.borrow_mut() = Some(Rc::clone(&activity));
        Ok(activity)
    }

    /// Answer a call on the app's settings.
    fn settings_call(&self, call: SettingsCall) -> Answer<Reply> {
        let settings = match self.settings() {
            Ok(settings) => settings,
            Err(error) => return Box::pin(std::future::ready(Err(BrokerError::failed(error)))),
        };
        let view = |(revision, document)| Reply::Settings { revision, document };
        match call {
            SettingsCall::Read { after } => Box::pin(async move {
                settings
                    .read(after)
                    .await
                    .map(view)
                    .map_err(BrokerError::failed)
            }),
            SettingsCall::Update {
                section,
                values,
                action,
            } => Box::pin(std::future::ready(
                settings
                    .update(&section, &values, action.as_deref())
                    .map(view)
                    .map_err(BrokerError::failed),
            )),
            SettingsCall::Display { command, origin } => Box::pin(std::future::ready(
                settings
                    .display(&command, origin)
                    .map(Reply::Answer)
                    .map_err(BrokerError::failed),
            )),
        }
    }

    /// Answer a call on the app's live activity.
    fn live_activity_call(&self, call: LiveActivityCall) -> Answer<Reply> {
        let activity = match self.live_activity() {
            Ok(activity) => activity,
            Err(error) => return Box::pin(std::future::ready(Err(BrokerError::failed(error)))),
        };
        match call {
            LiveActivityCall::Show(shown) => Box::pin(std::future::ready(
                activity
                    .show(&shown)
                    .map(|()| Reply::Done)
                    .map_err(BrokerError::failed),
            )),
            LiveActivityCall::Controls { after } => Box::pin(async move {
                activity
                    .controls(after)
                    .await
                    .map(|(serial, controls)| Reply::Controls { serial, controls })
                    .map_err(BrokerError::failed)
            }),
        }
    }
}

/// A local-file command's answer on its way.
pub(super) type FileAnswer = std::pin::Pin<Box<dyn Future<Output = Result<Vec<u8>, BrokerError>>>>;

impl JoinedChannel {
    /// Answer a command about the person's local files, JSON both ways:
    /// external moderation or the file clipboard, told
    /// apart by their envelopes.
    pub(super) fn local_file_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
        on_behalf_of: &[u8],
    ) -> Result<FileAnswer, BrokerError> {
        if external_moderation::is_envelope(bytes) {
            let receive = external_moderation::start(&self, bytes, on_behalf_of)
                .map_err(BrokerError::failed)?;
            return Ok(Box::pin(async move {
                let reply = receive
                    .await
                    .map_err(|_| {
                        BrokerError::failed("external moderation outcome unobserved".into())
                    })?
                    .map_err(BrokerError::failed)?;
                serde_json::to_vec(&reply).map_err(|error| BrokerError::failed(error.to_string()))
            }));
        }
        let reply = self.file_clipboard_dispatch(bytes, on_behalf_of)?;
        Ok(Box::pin(std::future::ready(Ok(reply))))
    }
}

impl JoinedChannel {
    /// Answer a committed-document request. An `Await` is held until the
    /// read may have changed, and an Open whose record is still being
    /// decided (the workspace's host's replay, or the host it was asked of)
    /// or created on another device is awaited, all on this `LocalSet`, so
    /// every route's calls keep flowing meanwhile.
    pub(super) async fn committed_document_answer(
        self: Rc<Self>,
        request: runtime_collab::committed_document::Request,
    ) -> Result<runtime_collab::committed_document::Reply, String> {
        if let runtime_collab::committed_document::Request::Await { known_revision } = request {
            return self.await_committed_change(known_revision).await;
        }
        if let Some(waiting) = self.committed_arrival_wait(&request) {
            waiting.await;
        }
        self.committed_document_dispatch(&request)
    }
}

/// A local effect's record on its way into workspace activity.
pub(super) type Recording = std::pin::Pin<Box<dyn Future<Output = Result<(), BrokerError>>>>;

/// What the broker answers once its work is done.
pub(super) type Answer<T> = std::pin::Pin<Box<dyn Future<Output = Result<T, BrokerError>>>>;

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
        let failed = Refusal::refusal;
        let ready = |reply: Result<Vec<u8>, BrokerError>| -> Result<Answering, String> {
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
                // behind. An item's use is waited for here, by the answer, and
                // never by the dispatch that started it.
                Service::BrowserServiceSession => channel
                    .browser_service_session_dispatch(body)
                    .map_err(failed)
                    .map(|reply| -> Answering {
                        Box::pin(async move {
                            reply
                                .await
                                .map(|mut reply| std::mem::take(&mut *reply))
                                .map_err(Refusal::refusal)
                        })
                    }),
                Service::WifiContinuity => channel
                    .wifi_continuity_dispatch(&channel.route().app_id, body)
                    .map_err(failed)
                    .map(|reply| -> Answering {
                        Box::pin(async move { reply.await.map_err(Refusal::refusal) })
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
                        Box::pin(async move { reply.await.map_err(Refusal::refusal) })
                    }),
                Service::ApprovedCheck => runtime_collab::approved_checks::Request::decode(body)
                    .and_then(|request| approved_checks::context(&channel, request))
                    .map(|context| -> Answering { Box::pin(approved_checks::execute(context)) }),
                Service::RemoteAction => {
                    let reply = channel.remote_action_dispatch(body);
                    Ok(Box::pin(async move { reply.await.map_err(Refusal::refusal) }) as Answering)
                }
                Service::Resources => {
                    // Keep the continuation reference so the broker can refuse unsupported writes.
                    channel
                        .resources_dispatch(body, on_behalf_of)
                        .map_err(failed)
                        .map(|reply| -> Answering {
                            Box::pin(async move {
                                let reply = reply.await?;
                                serde_json::to_vec(&reply)
                                    .map_err(|_| "the store's answer cannot be encoded".to_owned())
                            })
                        })
                }
            };
        Box::pin(async move { Ok(Reply::Answer(answer?.await?)) })
    }
}

impl std::ops::Deref for ChannelSession {
    type Target = JoinedChannel;

    fn deref(&self) -> &JoinedChannel {
        &self.channel
    }
}

impl JoinedChannel {
    /// Admit, commit where the workspace's owner must hold it first, and fan out
    /// one signed document change.
    pub(super) async fn publish_document(&self, envelope: SignedOp) -> Result<(), BrokerError> {
        self.writable()?;
        // A change of the record the channel carries, under the id it is kept under.
        let (state, route) = (&self.state, self.resource_route());
        let current_identity = state.revalidate_admission(&route, &self.verified_identity)?;
        verify_session_author(&current_identity, envelope.author)
            .map_err(|error| BrokerError::failed(format!("send: {error}")))?;
        state.authorize_certified_document(&route, &current_identity, &envelope)?;
        let owner_required = state.durable_owner_required || state.publisher.has_owner();
        let _owner_order = if owner_required {
            Some(state.owner_commit_order.lock().await)
        } else {
            None
        };
        // The write as the owner took it: signed again where another local
        // writer's later write passed its sequence.
        let envelope = if owner_required {
            state.revalidate_admission(&route, &self.verified_identity)?;
            state
                .commit_owner_document(
                    tilo_peer_state::owner_commit::OwnerCommitRequest {
                        workspace: route.cluster_id.clone(),
                        operation: tilo_peer_state::owner_commit::CommitOperation::Document {
                            app: route.app_id.clone(),
                            document: route.document_id.clone(),
                            envelope: Box::new(envelope),
                            readers: None,
                        },
                    },
                    self.resource_type.clone(),
                )
                .await?
        } else {
            envelope
        };
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

    /// Every document change on this route from now on, other endpoints'
    /// and peers'.
    pub(super) fn subscribe_ops(&self) -> Result<Feed<OpEnvelope>, BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        Ok(Feed::new(
            self.state
                .subscribe_ops(&self.channel_id, self.connection_id),
        ))
    }

    /// The device's record-content service, for a route still admitted.
    fn document_content(
        &self,
    ) -> Result<(CrdtRoute, Arc<crate::content::RouteContent>), BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let route = self.resource_route();
        if route.app_id.is_empty() || route.cluster_id.is_empty() {
            return Err(BrokerError::failed(
                "record content belongs to a certified app route".into(),
            ));
        }
        let content = self
            .state
            .services
            .document_content
            .clone()
            .ok_or_else(|| {
                BrokerError::unimplemented("this provider keeps no record content".into())
            })?;
        Ok((route, content))
    }

    /// Keep content the record references, which the app stored.
    pub(super) fn offer_content(&self, hash: &str, bytes: &[u8]) -> Result<(), BrokerError> {
        self.writable()?;
        let (route, content) = self.document_content()?;
        content
            .offer(&route, hash, bytes)
            .map_err(BrokerError::failed)
    }

    /// Fetch content the record references and the app lacks; it arrives on
    /// the content feed.
    pub(super) fn want_content(&self, hash: &str) -> Result<(), BrokerError> {
        let (route, content) = self.document_content()?;
        content.want(&route, hash);
        Ok(())
    }

    /// The route's context from now on, starting with its current
    /// roster. A native private window's context also follows the
    /// continuation it shows.
    pub(super) fn context_feed(&self) -> Result<Feed<ContextUpdate>, BrokerError> {
        let route = self.resource_route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let native = self
            .state
            .native_roster_for_subscriber(&route, &self.verified_identity)
            .map_err(BrokerError::failed)?;
        let instance = self
            .native_private_window_instance()
            .map_err(BrokerError::failed)?;
        let document = match instance {
            Some(_) => Some(self.document_read_context().map_err(BrokerError::failed)?),
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
        Ok(Feed::new(rx).with_native_guard(native_guard))
    }

    /// Ask another endpoint holding this route to publish the document's
    /// current state.
    pub(super) fn ask_for_document(&self) -> Result<(), BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let _ = self
            .state
            .request_document_snapshot(&self.channel_id, self.connection_id);
        Ok(())
    }

    /// Take a report about this route's own instance or record from the
    /// app `claimed_app`, which must be the route's: no app reports for
    /// another, and a channel held to read reports nothing.
    pub(super) fn report(&self, claimed_app: &str, report: Report) -> Result<(), BrokerError> {
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
            return Err(BrokerError::failed(format!(
                "{verb} appId must match the certified connection route"
            )));
        }
        // A continuation report or a handoff step speaks only for this
        // instance; a search index for the record the route names.
        let instance = match report {
            Report::SearchIndex(_) => None,
            _ => Some(self.continuation_instance().map_err(BrokerError::failed)?),
        };
        let workspace = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
            .ok_or_else(|| BrokerError::failed(format!("{verb} route has no durable workspace")))?;
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
                service.report(workspace, app_id, instance, schema_version, metadata)
            }),
            Report::ClearContinuation => continuations("continuation clearing unsupported")
                .and_then(|service| service.clear(workspace, &app_id, instance)),
            Report::Handoff(message) => continuations("handoff unsupported")
                .and_then(|service| service.handoff(workspace, app_id, instance, message)),
            Report::SearchIndex(delta) => {
                if delta.len() > crate::apps::services::MAX_SEARCH_INDEX_DELTA_BYTES {
                    return Err(BrokerError::failed(
                        "publishSearchIndex delta exceeds its bound".into(),
                    ));
                }
                services
                    .search_index
                    .as_ref()
                    .ok_or_else(|| "search index publication unsupported".to_owned())
                    .and_then(|service| {
                        service.publish(workspace, &app_id, &route.document_id, &delta)
                    })
            }
        }
        .map_err(BrokerError::failed)
    }

    /// Record a terminal effect this app ran itself on one of the actions
    /// it publishes, as workspace activity: committed by the workspace's owner
    /// where one keeps effect receipts, else left to the app's own record.
    pub(super) fn record_local_effect_dispatch(
        &self,
        resource: &str,
        action: &str,
        update: tilo_actions::InvocationUpdate,
    ) -> Result<Recording, BrokerError> {
        // No app changes another app's route: a channel held to read refuses.
        self.writable()?;
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let Some(status) = tilo_actions::EffectReceiptStatus::from_terminal(&update) else {
            return Err(BrokerError::failed(
                "only a terminal effect may enter workspace activity".into(),
            ));
        };
        let Some((surface, offered)) = self
            .state
            .publishers
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
            return Err(BrokerError::failed(
                "the application no longer publishes that action".into(),
            ));
        };
        if offered.operation_kind != tilo_actions::OperationKind::Effect {
            return Err(BrokerError::failed(
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
        let actor = current.broker_actor().member().clone();
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
                    .commit_owner_request(tilo_peer_state::owner_commit::OwnerCommitRequest {
                        workspace: surface.route.cluster_id,
                        operation: tilo_peer_state::owner_commit::CommitOperation::EffectReceipt {
                            invocation: update.invocation().0,
                            redacted: serde_json::to_vec(&receipt)
                                .map_err(|e| BrokerError::failed(e.to_string()))?,
                        },
                    })
                    .await?;
                state.revalidate_admission(&route, &identity)?;
                Ok(())
            }));
        }
        // No owner keeps effect receipts for this workspace: the app's effect
        // leaves no record beyond its own.
        Ok(Box::pin(std::future::ready(Ok(()))))
    }

    /// The wait for `of`'s next change past `after`, which resolves to the
    /// revision to wait from next. It says only that something changed;
    /// the endpoint reads again, checked as always.
    pub(super) fn change(
        &self,
        of: Changes,
        after: u64,
    ) -> Result<crate::apps::services::NearbyTransferChange, BrokerError> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let services = &self.state.services;
        match of {
            Changes::Speakers => speaker_change(services, &route.app_id, after),
            Changes::Drives => drive_changes::drive_change(services, &route.app_id, after),
            Changes::Applications => app_changes::app_change(services, &route.app_id, after),
            Changes::NearbyTransfers => {
                let context = crate::apps::services::NearbyTransferRequestContext::authenticated(
                    self.connection_id,
                    route.app_id.clone(),
                    route.cluster_id.clone(),
                    route.channel_key(),
                    self.verified_identity
                        .agent_id
                        .is_none()
                        .then(|| self.verified_identity.actor_key.clone()),
                );
                nearby_transfer_change(services, &context, after)
            }
            Changes::GroupKeys => {
                let route = self.resource_route();
                if route.app_id.is_empty() || route.cluster_id.is_empty() {
                    return Err(BrokerError::failed(
                        "channel keys belong to a certified app route".into(),
                    ));
                }
                let keys = services.group_keys.clone().ok_or_else(|| {
                    BrokerError::unimplemented(
                        "this provider has no channel service to follow".into(),
                    )
                })?;
                let workspace = tilo_desktop_model::WorkspaceId::parse(&route.cluster_id)
                    .ok_or_else(|| BrokerError::failed("the route names no workspace".into()))?;
                Ok(Box::pin(async move {
                    keys.workspace_changed(workspace, after).await
                }))
            }
        }
    }

    /// Answer a realtime command `claimed_app` sent, JSON both ways.
    pub(super) fn realtime_dispatch(
        &self,
        claimed_app: &str,
        bytes: &[u8],
    ) -> Result<Vec<u8>, BrokerError> {
        let context = self.realtime_context()?;
        if bytes.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
            return Err(BrokerError::failed(
                "realtime command exceeds local protocol bound".into(),
            ));
        }
        let reply = delegate_realtime_command(&self.state.services, &context, claimed_app, bytes)?;
        if reply.len() > collab_types::realtime::MAX_REALTIME_WIRE_BYTES {
            return Err(BrokerError::failed(
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
    ) -> Result<Vec<u8>, BrokerError> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let context = crate::apps::services::NearbyTransferRequestContext::authenticated(
            self.connection_id,
            route.app_id.clone(),
            route.cluster_id.clone(),
            route.channel_key(),
            self.verified_identity
                .agent_id
                .is_none()
                .then(|| self.verified_identity.actor_key.clone()),
        );
        delegate_nearby_transfer_command(&self.state.services, &context, claimed_app, bytes)
    }

    /// Answer a Wi-Fi continuity command `claimed_app` sent, JSON both
    /// ways; the certified Settings route's only.
    pub(super) fn wifi_continuity_dispatch(
        &self,
        claimed_app: &str,
        bytes: &[u8],
    ) -> Result<impl Future<Output = Result<Vec<u8>, BrokerError>> + use<>, BrokerError> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id != WIFI_CONTINUITY_APP_ID || claimed_app != route.app_id {
            return Err(BrokerError::failed(
                "Wi-Fi continuity requires the certified Settings connection route".into(),
            ));
        }
        if bytes.len() > runtime_collab::MAX_WIFI_CONTINUITY_WIRE_BYTES {
            return Err(BrokerError::failed(
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
            .map_err(|error| BrokerError::failed(error.to_string()))?
        })
    }

    /// Answer a private service-session command from an app's own route:
    /// the Browser's website sessions, an account secret for the app the
    /// owner releases it to, or a vault item's text for one of Browser's
    /// page actions. Only the last is waited for: it comes from outside this
    /// process, so this hands back its answer and returns.
    pub(super) fn browser_service_session_dispatch(
        &self,
        bytes: &[u8],
    ) -> Result<Answer<zeroize::Zeroizing<Vec<u8>>>, BrokerError> {
        let refused = || BrokerError::failed("private service-session request refused".into());
        // Only the person's own app connection: never an agent's. Which app
        // may ask what is decided per request.
        if self.verified_identity.agent_id.is_some() {
            return Err(refused());
        }
        let read = self.document_read_context().map_err(|_| refused())?;
        let context = crate::apps::services::BrowserServiceSessionRequestContext::authenticated(
            self.connection_id,
            self.verified_identity.actor_key.clone(),
            read,
        );
        if let Some(awaited) = item_use::delegate_item_use(
            self.state.services.browser_service_sessions.as_ref(),
            &context,
            bytes,
        ) {
            return awaited;
        }
        delegate_browser_service_session_command(&self.state.services, &context, bytes).map(ready)
    }

    /// Answer a speaker-group command, JSON both ways. Only the certified
    /// Settings route manages the person's speaker groups.
    pub(super) fn speaker_dispatch(&self, bytes: &[u8]) -> Result<Vec<u8>, BrokerError> {
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id != SPEAKER_APP_ID {
            return Err(BrokerError::failed(
                "speaker groups are managed only by the certified Settings application".into(),
            ));
        }
        if bytes.len() > runtime_collab::MAX_SPEAKER_WIRE_BYTES {
            return Err(BrokerError::failed(
                "speaker command exceeds the local protocol bound".into(),
            ));
        }
        let command = serde_json::from_slice::<runtime_collab::SpeakerCommand>(bytes)
            .map_err(|error| BrokerError::failed(error.to_string()))?;
        if !command.valid() {
            return Err(BrokerError::failed(
                "speaker command is structurally invalid".into(),
            ));
        }
        let speakers = self.state.services.speakers.as_ref().ok_or_else(|| {
            BrokerError::failed("speaker groups are unsupported by this provider".into())
        })?;
        let reply = serde_json::to_vec(&speakers.command(command))
            .map_err(|error| BrokerError::failed(error.to_string()))?;
        if reply.len() > runtime_collab::MAX_SPEAKER_WIRE_BYTES {
            return Err(BrokerError::failed(
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
    ) -> Result<(Vec<Vec<u8>>, bool), BrokerError> {
        let route = self.resource_route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        let broker_channels = &self.state.broker_channels;
        let (ops, oldest, mut exhausted) =
            read_scrollback(broker_channels, artifact, before_seq, max);
        if exhausted {
            let peer_bound = oldest.unwrap_or(before_seq);
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

    /// Answer the app's remote-action `frame` with the frame its caller,
    /// api, grants and the person decide.
    pub(super) fn remote_action_dispatch(&self, frame: &[u8]) -> Answer<Vec<u8>> {
        use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
        let encode = |message: RemoteActionMessage| {
            RemoteActionFrame::new(message)
                .encode()
                .map_err(|error| BrokerError::failed(error.to_string()))
        };
        let answer = match self.remote_action_answer(frame) {
            Ok(answer) => answer,
            Err(error) => return Box::pin(std::future::ready(Err(error))),
        };
        Box::pin(async move { encode(answer.await?) })
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one remote message's routing: a turn's requests, an agent session's and the device's own"
    )]
    fn remote_action_answer(
        &self,
        frame: &[u8],
    ) -> Result<Answer<tilo_actions::remote::RemoteActionMessage>, BrokerError> {
        use tilo_actions::remote::RemoteActionFrame;
        let original_time = self
            .state
            .registered_policy_time()
            .map_err(BrokerError::failed)?;
        let current = self
            .state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let request = RemoteActionFrame::decode(frame)
            .map_err(|error| BrokerError::failed(error.to_string()))?;
        let route = self.route();
        let person = current.caller(&route.cluster_id);
        // An app's request continuing an invocation someone asked of it (an
        // edit its effect submits, another app's action its effect calls) is
        // decided as that invocation's caller, under its ownership; its open
        // of an application, else as the person whose press it follows.
        // Anything else, including what continues an invocation the app made
        // of itself, is the app's own API's to decide.
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
        } else if let Some(caller) = self.state.turn_caller(self.peer_id) {
            // An agent answering a turn makes every request as the turn, through its session.
            let api = self.state.agent_session_api(
                &caller,
                InvocationCaller::Local {
                    route,
                    time: original_time,
                    admitted: Box::new(current),
                },
            );
            let Some(api) = api else {
                return refusal(
                    &request.message,
                    tilo_actions::remote::ActionCallError::NotPermitted,
                );
            };
            let answer =
                tilo_actions::remote::serve(&api, &caller, request.message).ok_or_else(|| {
                    BrokerError::failed("only remote-action requests are answered".into())
                })?;
            return Ok(Box::pin(async move {
                answer.await.ok_or_else(|| {
                    BrokerError::failed("the action provider dropped the request".into())
                })
            }));
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
            None => match self.state.app_api(&person, &route.app_id, &request.message) {
                Ok(caller) => caller,
                Err(refusal) => return Ok(ready(*refusal)),
            },
        };
        if tilo_actions::remote::refuse(
            &request.message,
            tilo_actions::remote::ActionCallError::UnknownAction,
        )
        .is_none()
        {
            return Err(BrokerError::failed(
                "only remote-action requests are answered".into(),
            ));
        }
        // An application reaching for another app's action it holds no
        // grant for: the person using it is asked first, or it is refused.
        let consent = match (caller.acting().app(), &request.message) {
            (Some(app_id), tilo_actions::remote::RemoteActionMessage::Invoke(invoke)) => {
                match self.state.app_action_consent(&person, app_id, invoke) {
                    app_consent::ConsentStep::Serve => None,
                    app_consent::ConsentStep::Refuse(refused) => {
                        return refusal(&request.message, refused);
                    }
                    app_consent::ConsentStep::Ask(consent) => Some(consent),
                }
            }
            _ => None,
        };
        let admitting_person = current.actor_key.clone();
        let provider = action_provider::ConnectionActionProvider {
            state: Arc::clone(&self.state),
            own_peer: self.peer_id,
            origin: continued_origin.unwrap_or_else(|| InvocationCaller::Local {
                route,
                time: original_time,
                admitted: Box::new(current),
            }),
        };
        if let Some(consent) = consent {
            return Ok(super::app_consent::serve_after_consent(
                Arc::clone(&self.state),
                consent,
                provider,
                caller,
                request.message,
            ));
        }
        // An invocation the caller may not confirm itself is put to the
        // person who admitted the connection, on their own device.
        let answer = super::approval::serve_answering(
            Arc::clone(&self.state),
            provider,
            caller,
            admitting_person,
            request.message,
        );
        // Awaited on this local set, which is also where an application's
        // directory answers; the connection's other calls keep flowing.
        Ok(Box::pin(async move {
            answer.await.ok_or_else(|| {
                BrokerError::failed("the action provider dropped the request".into())
            })
        }))
    }

    /// Start handing this app the invocations of its published actions,
    /// replacing any stream it held; `stable` asks for stable invocation ids.
    /// The stream comes with what ends it once the app asks for no more
    /// ([`InvocationFeedEnd`]).
    pub(super) fn invocation_feed(
        &self,
        stable: bool,
    ) -> Result<(InvocationStreamServer, InvocationFeedEnd), BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        // Stream replacement and resolved dispatch share the admission boundary:
        // an admitted old token must never dispatch to a replacement stream.
        let Ok(admission) = self.state.admission_lock.lock() else {
            return Err(BrokerError::failed(
                "invocation admission registry unavailable".into(),
            ));
        };
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        if let Ok(mut directories) = self.state.canonical_directories.lock() {
            directories.stream_replaced(self.peer_id);
        } else {
            return Err(BrokerError::failed(
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
            return Err(BrokerError::failed(
                "application capability registry unavailable".into(),
            ));
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let end = InvocationFeedEnd {
            state: Arc::clone(&self.state),
            peer_id: self.peer_id,
            sender: tx.downgrade(),
        };
        if let Ok(mut subscribers) = self.state.publishers.streams.lock() {
            subscribers.insert(self.peer_id, tx);
        } else {
            return Err(BrokerError::failed(
                "invocation subscriber registry unavailable".into(),
            ));
        }
        drop(admission);
        self.state.publishers.streams_changed();
        // The app the launcher attested, else the one this channel names.
        let (app, workspace) = self
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
        let delivered = invocation_delivery_notice(&self.state.services, app, workspace);
        Ok((
            InvocationStreamServer {
                rx: invocation_queue(self.peer_id, rx),
                delivered,
            },
            end,
        ))
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
                        BrokerError::failed(format!("invalid invocation update: {error}"))
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
        Box::pin(async move {
            let _owner_order = if state.durable_owner_required || state.publisher.has_owner() {
                Some(state.owner_commit_order.lock().await)
            } else {
                None
            };
            if state.durable_owner_required || state.publisher.has_owner() {
                state.revalidate_admission(&route, &identity)?;
            }
            let completion = InvocationCompletion { code, message };
            if !completion.code.is_empty() && completion.message.trim().is_empty() {
                return Err(BrokerError::failed(
                    "failed invocation completion requires a message".into(),
                ));
            }
            let (staged, update) = {
                let registry = state
                    .pending_invocations
                    .lock()
                    .map_err(|_| BrokerError::failed("invocation registry unavailable".into()))?;
                let pending = registry
                    .get(&id)
                    .filter(|pending| pending.peer_id == peer_id)
                    .ok_or_else(|| {
                        BrokerError::failed(
                            "an application may complete only its own invocation".into(),
                        )
                    })?;
                if completion.code.is_empty() {
                    pending
                        .result
                        .validate(&tilo_actions::ActionOutput::default())
                        .map_err(|error| {
                            BrokerError::failed(format!(
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
            if state.durable_owner_required || state.publisher.has_owner() {
                state.revalidate_admission(&route, &identity)?;
            }
            state
                .settle_definite_outcome(peer_id, id, &staged.update, &update)
                .await?;
            let pending = state
                .pending_invocations
                .lock()
                .map_err(|_| BrokerError::failed("invocation registry unavailable".into()))?
                .remove(&id)
                .ok_or_else(|| BrokerError::failed("unknown invocation id".into()))?;
            state.publish_activity(pending.activity.as_ref(), &update);
            state.record_completion(id, pending.completion_owner(), update.clone());
            if let Ok(mut requests) = state.cancellation_requests.lock() {
                requests.remove(&id);
            }
            let _ = pending.tx.send(completion);
            Ok(Reply::Done)
        })
    }

    fn invocation_cancellation(&self, id: u64) -> Answer<Reply> {
        let owns = self
            .state
            .pending_invocations
            .lock()
            .ok()
            .and_then(|pending| pending.get(&id).map(|job| job.peer_id == self.peer_id))
            .unwrap_or(false);
        if !owns {
            return Box::pin(std::future::ready(Err(BrokerError::failed(
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
            durable.wait().await.map_err(BrokerError::failed)?;
            Ok(Reply::Cancelled(true))
        })
    }

    /// Change the app's action surface on this channel (a reset delta
    /// replaces it whole), applied all or not at all.
    pub(super) fn publish_surface(
        &self,
        claimed_app: &str,
        delta: &runtime_collab::SurfaceDelta,
    ) -> Result<(), BrokerError> {
        // No app changes another app's route: a channel held to read refuses.
        self.writable()?;
        let route = self.route();
        let current = self
            .state
            .revalidate_admission(&route, &self.verified_identity)?;
        if route.app_id.is_empty() || claimed_app != route.app_id {
            return Err(BrokerError::failed(
                "a published surface's appId must match the certified connection route".into(),
            ));
        }
        let peer_id = self.peer_id;
        let unavailable = || BrokerError::failed("action surface registry unavailable".into());
        // Apply to a copy: a delta that does not apply, or whose result is
        // not a valid surface, changes nothing.
        let mut table = self
            .state
            .surface_tables
            .lock()
            .map_err(|_| unavailable())?
            .get(&peer_id)
            .cloned()
            .unwrap_or_default();
        table.apply(delta).map_err(|reason| {
            BrokerError::failed(format!("action surface delta refused: {reason}"))
        })?;
        let valid = validate_published_surface(table.actions()).map_err(BrokerError::failed)?;
        // A row left out is the app's own to fix; the rest of what it offers is offered.
        for refused in &valid.refused {
            tracing::warn!(app = %route.app_id, refused, "a published action is not offered");
        }
        let actions = valid.actions;
        let surface = PublishedSurface {
            peer_id,
            app_id: route.app_id.clone(),
            route,
            actions,
            live: true,
        };
        self.state
            .surface_tables
            .lock()
            .map_err(|_| unavailable())?
            .insert(peer_id, table);
        self.state.launchable.retain(&surface);
        let previous = self
            .state
            .publishers
            .surfaces
            .lock()
            .map_err(|_| unavailable())?
            .insert(peer_id, surface.clone());
        self.state
            .advance_approval_publication(peer_id, ApprovalPublicationKind::Surface);
        self.state
            .surface_republished(peer_id, &surface, previous.as_ref());
        // What the window now publishes is decided for it.
        self.state.refresh_shown_actions_of(peer_id);
        // A catalogue offering its record is what an `open` of it waits for:
        // a directory alone is offered before the app is ready.
        self.state
            .launch_published(&surface, self.resource_type.as_deref());
        // An agent that publishes how it answers a turn is handed its turns.
        if let Some(agent) = &current.agent_id
            && answers_turns(&surface)
        {
            self.state
                .turns
                .answer(peer_id, &surface.route.cluster_id, agent);
            self.state.local_turn.notify_one();
        }
        Ok(())
    }

    /// The action catalogue this endpoint may see: as it stands, then each
    /// time it changes.
    pub(super) fn catalogue_feed(&self) -> Result<ActionCatalogueStreamServer, BrokerError> {
        self.state
            .revalidate_admission(&self.route(), &self.verified_identity)?;
        let cluster = self.route().cluster_id;
        let actor = self.verified_identity.broker_actor();
        let rows = self
            .state
            .action_catalogue_for(&cluster, &actor)
            .map_err(BrokerError::failed)?;
        let follower = CatalogueFollower::new(&catalogue_rows(rows));
        if let Ok(mut subscribers) = self.state.action_catalogue_subscribers.lock() {
            // A connection follows one catalogue: a stream it held ends.
            let held = (cluster, actor, Arc::downgrade(&follower));
            if let Some((_, _, replaced)) = subscribers.insert(self.connection_id, held)
                && let Some(replaced) = replaced.upgrade()
            {
                replaced.end();
            }
        } else {
            return Err(BrokerError::failed(
                "action catalogue subscriber registry unavailable".into(),
            ));
        }
        Ok(ActionCatalogueStreamServer { follower })
    }

    /// Open the installed app `app_id` (from `package_root`, if named) for
    /// the person's press or the invocation `on_behalf_of` (UUID bytes),
    /// answered once it started or failed to.
    pub(super) fn launch(
        self: &Rc<Self>,
        app_id: &str,
        package_root: &str,
        on_behalf_of: &[u8],
    ) -> Answer<Reply> {
        let refuse =
            |error: BrokerError| -> Answer<Reply> { Box::pin(std::future::ready(Err(error))) };
        let parent = match observed_launch::launch_parent(on_behalf_of) {
            Ok(parent) => parent,
            Err(error) => return refuse(error),
        };
        let app_id = app_id.to_owned();
        let package_root = match observed_launch::package_root(package_root) {
            Ok(root) => root,
            Err(error) => return refuse(error),
        };
        // An app opens another only for the person's press in it or an
        // invocation delivered to it ([`app_open_api`]); an agent's own
        // connection, by its own grants.
        if self.verified_identity.agent_id.is_none()
            && self
                .state
                .app_open_authority(self.peer_id, &self.route().app_id, parent)
                .is_none()
        {
            return refuse(BrokerError::failed(
                "nothing the person or an invocation asked for is behind this launch".into(),
            ));
        }
        if let Err(error) = self.current_launch_authority(&app_id, parent) {
            return refuse(BrokerError::failed(error));
        }

        let admitted = match self.admit_observed_launch(&app_id, package_root.as_deref(), parent) {
            Ok(admitted) => admitted,
            Err(error) => return refuse(BrokerError::failed(error)),
        };
        #[cfg(test)]
        self.observed_launch_effect_barrier();
        // Admission may block on durable storage. Discard the earlier policy
        // snapshot and judge both launch and authorship again at the effect.
        let started =
            self.current_launch_authority(&app_id, parent)
                .and_then(|(workspace, actor, now)| {
                    let author = observed_launch::launch_author(&actor, &workspace, now);
                    self.begin_original_launch(
                        &app_id,
                        package_root.as_deref(),
                        &workspace,
                        author.as_deref(),
                        parent,
                    )
                });
        let channel = Rc::clone(self);
        Box::pin(async move {
            let outcome = observed_launch::wait_for_startup(started).await;
            let update = channel.finish_observed_launch(&app_id, parent, &admitted, outcome)?;
            let update = serde_json::to_string(&update)
                .map_err(|error| BrokerError::failed(error.to_string()))?;
            Ok(Reply::Invoked {
                id: admitted.id,
                update,
            })
        })
    }

    /// News of the record's content from now on.
    pub(super) fn subscribe_content(
        &self,
    ) -> Result<Feed<crate::content::ContentNews>, BrokerError> {
        let (route, content) = self.document_content()?;
        Ok(Feed::new(content.subscribe(&route)))
    }
}

impl JoinedChannel {
    /// Register `directory` as this app's action directory, which the host
    /// asks instead of keeping its rows.
    pub(super) fn publish_directory(
        &self,
        directory: Rc<dyn DirectorySource>,
    ) -> Result<(), BrokerError> {
        // No app changes another app's route: a channel held to read refuses.
        self.writable()?;
        let route = self.route();
        self.state
            .revalidate_admission(&route, &self.verified_identity)?;
        validate_directory_identifier(
            &route.app_id,
            "app id",
            DIRECTORY_MAX_IDENTIFIER_BYTES,
            true,
        )?;
        let app_id = route.app_id.clone();
        let registration = Rc::new(uuid::Uuid::new_v4());
        let Ok(_admission) = self.state.admission_lock.lock() else {
            return Err(BrokerError::failed(
                "directory admission unavailable".into(),
            ));
        };
        self.state
            .register_canonical_directory(self.peer_id, &route, *registration)
            .map_err(BrokerError::failed)?;
        self.directories.directories.borrow_mut().insert(
            self.peer_id,
            RegisteredActionDirectory {
                registration,
                pagination: directory_pagination::DirectoryPagination::default(),
                publishing_person: self
                    .verified_identity
                    .certificate
                    .as_ref()
                    .map(|cert| person_id_for_root(cert.user_public_key)),
                app_id: app_id.clone(),
                route: route.clone(),
                directory,
            },
        );
        if let Ok(mut roots) = self.state.action_directory_roots.lock() {
            roots.insert(
                self.peer_id,
                ActionDirectoryRoot {
                    peer_id: self.peer_id,
                    app_id,
                    cluster_id: route.cluster_id.clone(),
                    document_id: route.document_id.clone(),
                },
            );
        }
        self.state
            .advance_approval_publication(self.peer_id, ApprovalPublicationKind::Directory);
        self.retain_native_checkout(&route);
        self.state.refresh_action_directory_root_snapshot();
        // The directory can alter remote discoverability without an eager
        // action snapshot changing, so tell catalogue subscribers.
        self.state.catalogue_changed();
        Ok(())
    }

    /// The realtime continuity events this app sees.
    pub(super) fn realtime_feed(&self) -> Result<Feed<Vec<u8>>, BrokerError> {
        let context = self.realtime_context()?;
        let service = self.state.services.realtime.as_ref().ok_or_else(|| {
            BrokerError::failed("realtime continuity is unsupported by this provider".into())
        })?;
        let rx = service.subscribe(&context).map_err(BrokerError::failed)?;
        Ok(Feed::new(rx))
    }
}

/// What an action directory a connection published answers.
pub(super) type Asked<T> = std::pin::Pin<Box<dyn Future<Output = Result<T, String>>>>;

/// An app's action directory, which the app answers for: the host keeps no
/// copy of its rows and asks it for each page and resolution.
pub(super) trait DirectorySource {
    /// A page for `query`, whose rows' size stays within `max_bytes`.
    fn query(
        &self,
        query: runtime_collab::ActionDirectoryQuery,
        max_bytes: u32,
    ) -> Asked<runtime_collab::ActionDirectoryPage>;

    /// The action `resource`/`action` at `revision` (empty: now); with
    /// `local_document`, only from the snapshot a local document action was
    /// captured from.
    fn resolve(
        &self,
        target: (&str, &str, &str),
        local_document: bool,
        max_bytes: u32,
    ) -> Asked<runtime_collab::ActionDirectoryResolution>;
}

/// The directory an endpoint published on one of its channels, asked over
/// its connection.
pub(super) struct WireDirectory {
    connection: Weak<WireConnection>,
    channel: Handle,
}

impl WireDirectory {
    fn ask(&self, ask: Ask) -> Asked<Answered> {
        let asked = self
            .connection
            .upgrade()
            .ok_or_else(|| "the application's connection closed".to_owned())
            .map(|connection| connection.ask(ask));
        Box::pin(async move {
            asked?
                .await
                .map_err(|_| "the application's connection closed".to_owned())?
        })
    }
}

impl DirectorySource for WireDirectory {
    fn query(
        &self,
        query: runtime_collab::ActionDirectoryQuery,
        max_bytes: u32,
    ) -> Asked<runtime_collab::ActionDirectoryPage> {
        let asked = self.ask(Ask::Query {
            channel: self.channel,
            query,
        });
        Box::pin(async move {
            let Answered::Page(page) = asked.await? else {
                return Err("the application answered a page with something else".into());
            };
            runtime_collab::read_directory_answer(
                &page,
                u64::from(max_bytes),
                |page: &runtime_collab::ActionDirectoryPage| page.rows.iter().collect(),
            )
        })
    }

    fn resolve(
        &self,
        (revision, resource, action): (&str, &str, &str),
        local_document: bool,
        max_bytes: u32,
    ) -> Asked<runtime_collab::ActionDirectoryResolution> {
        let asked = self.ask(Ask::Resolve {
            channel: self.channel,
            revision: revision.to_owned(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            local_document,
        });
        Box::pin(async move {
            let Answered::Resolution(resolution) = asked.await? else {
                return Err("the application answered a resolution with something else".into());
            };
            runtime_collab::read_directory_answer(
                &resolution,
                u64::from(max_bytes),
                |resolution: &runtime_collab::ActionDirectoryResolution| match resolution {
                    runtime_collab::ActionDirectoryResolution::Found { row, .. } => {
                        vec![row.as_ref()]
                    }
                    _ => Vec::new(),
                },
            )
        })
    }
}

/// One endpoint's connection: the handles it holds, its live subscriptions,
/// and the queue every outgoing frame takes.
pub(super) struct WireConnection {
    /// What joins channels for this connection. A connection without one
    /// only holds the channels opened for it.
    service: Option<Rc<AppConnection>>,
    channels: RefCell<HashMap<Handle, Rc<ChannelSession>>>,
    /// The handle this connection opened on the session's published
    /// resources, once it asked for them.
    published: Cell<Option<Handle>>,
    next_handle: Cell<u64>,
    subscriptions: RefCell<HashMap<u64, (Handle, tokio::task::AbortHandle)>>,
    /// The workspace control sessions subscriptions hold, for their input.
    controls: RefCell<HashMap<u64, Rc<dyn crate::apps::services::WorkspaceControlSession>>>,
    /// What ends each invocation subscription once its app asks for no more.
    invocation_ends: RefCell<HashMap<u64, InvocationFeedEnd>>,
    /// The broker's asks of the endpoint waiting for its answer.
    asks: RefCell<HashMap<u64, tokio::sync::oneshot::Sender<Result<Answered, String>>>>,
    next_ask: Cell<u64>,
    frames: mpsc::UnboundedSender<HostFrame>,
}

impl Drop for WireConnection {
    fn drop(&mut self) {
        for (_, (_, task)) in self.subscriptions.get_mut().drain() {
            task.abort();
        }
        for (_, control) in self.controls.get_mut().drain() {
            close_control(control);
        }
    }
}

/// End a workspace control session the endpoint let go of.
fn close_control(control: Rc<dyn crate::apps::services::WorkspaceControlSession>) {
    tokio::task::spawn_local(async move {
        let _ = control.close().await;
    });
}

/// What a connection reaches of the session's published resources: the
/// provider and the caller it reads them for.
struct Publishing {
    published: Arc<dyn crate::apps::services::PublishedResources>,
    caller: crate::apps::services::Caller,
}

impl WireConnection {
    /// A connection and the queue of the frames it sends.
    pub(super) fn new(
        service: Option<Rc<AppConnection>>,
    ) -> (Rc<Self>, mpsc::UnboundedReceiver<HostFrame>) {
        let (frames, outgoing) = mpsc::unbounded_channel();
        let connection = Rc::new(Self {
            service,
            channels: RefCell::default(),
            published: Cell::default(),
            next_handle: Cell::new(1),
            subscriptions: RefCell::default(),
            controls: RefCell::default(),
            invocation_ends: RefCell::default(),
            asks: RefCell::default(),
            next_ask: Cell::new(1),
            frames,
        });
        (connection, outgoing)
    }

    /// Ask the endpoint `ask`; its answer arrives as a
    /// [`Operation::Answer`], or never if the connection closes first.
    fn ask(&self, ask: Ask) -> tokio::sync::oneshot::Receiver<Result<Answered, String>> {
        let id = self.next_ask.get();
        self.next_ask.set(id.wrapping_add(1));
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asks.borrow_mut().insert(id, answer);
        if self.frames.send(HostFrame::Ask { id, ask }).is_err() {
            self.asks.borrow_mut().remove(&id);
        }
        answered
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

    /// What this connection reaches of the session's published resources,
    /// and for whom: the app and workspace its launcher named, when this
    /// host publishes anything at all, and the broker state a granted
    /// reading is decided from.
    fn publishing(&self) -> Result<Publishing, String> {
        let unlaunched = || "this connection speaks for no launched app".to_owned();
        let service = self.service.as_ref().ok_or_else(unlaunched)?;
        let published = service
            .state
            .services
            .published
            .clone()
            .ok_or_else(|| "this provider publishes no resources".to_owned())?;
        let launch = service.lease.local_media.as_ref().ok_or_else(unlaunched)?;
        Ok(Publishing {
            published,
            caller: crate::apps::services::Caller {
                app_id: launch.app_id.clone(),
                workspace: launch.workspace_id.clone(),
            },
        })
    }

    /// This connection's hold on the session's published resources: one
    /// handle per connection, the same one every time it asks.
    fn open_published(&self) -> Handle {
        if let Some(handle) = self.published.get() {
            return handle;
        }
        let handle = Handle(self.next_handle.get());
        self.next_handle.set(handle.0.wrapping_add(1));
        self.published.set(Some(handle));
        handle
    }

    /// Refuse a call that names a handle this connection does not hold on the
    /// session's published resources.
    fn holds_published(&self, handle: Handle) -> Result<(), String> {
        (self.published.get() == Some(handle))
            .then_some(())
            .ok_or_else(|| "this connection does not hold the session's published resources".into())
    }

    /// Let go of what this connection held on the published resources: the
    /// handle it opened, and the reads of them it watched through it.
    fn close_published(&self) {
        let Some(handle) = self.published.take() else {
            return;
        };
        self.subscriptions.borrow_mut().retain(|_, (of, task)| {
            let held = *of == handle;
            if held {
                task.abort();
            }
            !held
        });
    }

    /// Answer one call. A reply, and the events a subscription it starts
    /// delivers, take the outgoing queue in that order; a call that waits (a
    /// join, a workspace's change, a document change the workspace's owner commits
    /// first) answers when done.
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per request answered once its work is done"
    )]
    pub(super) fn call(self: &Rc<Self>, Call { id, request }: Call) {
        let answer: std::pin::Pin<Box<dyn Future<Output = Result<Reply, String>>>> = match request {
            Operation::Publish {
                channel,
                publication: Publication::Document(envelope),
            } => {
                let channel = self.channel(channel);
                Box::pin(async move {
                    channel?
                        .publish_document(*envelope)
                        .await
                        .map(|()| Reply::Done)
                        .map_err(Refusal::refusal)
                })
            }
            Operation::Join(join) => {
                let (service, connection) = (self.service.clone(), Rc::downgrade(self));
                Box::pin(async move {
                    let service = service.ok_or("this connection joins no channels")?;
                    let channel = service.join(join).await.map_err(Refusal::refusal)?;
                    let connection = connection.upgrade().ok_or("the connection closed")?;
                    Ok(Reply::Opened(
                        connection.open(ChannelSession::new(Rc::new(channel))),
                    ))
                })
            }
            Operation::Workspace { channel, seen } => {
                let memberships = self
                    .service
                    .as_ref()
                    .and_then(|service| service.state.services.workspace_memberships.clone());
                let workspace = CrdtRoute::from_channel_key(&channel).cluster_id;
                Box::pin(async move {
                    Ok(Reply::Workspace(match (memberships, seen) {
                        (Some(memberships), Some(seen)) => {
                            memberships.workspace_status_change(&workspace, seen).await
                        }
                        (Some(memberships), None) => memberships.workspace_status(&workspace),
                        (None, _) => runtime_collab::WorkspaceStatus::default(),
                    }))
                })
            }
            Operation::Publish {
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
                    recorded?.await.map_err(Refusal::refusal)?;
                    Ok(Reply::Done)
                })
            }
            Operation::Changed { channel, of, after } => {
                let change = self
                    .channel(channel)
                    .and_then(|session| session.change(of, after).map_err(|e| e.to_string()));
                Box::pin(async move { Ok(Reply::Changed(change?.await)) })
            }
            Operation::Command {
                channel,
                service,
                body,
                on_behalf_of,
            } => match self.channel(channel) {
                Ok(session) => session.command(service, &body, &on_behalf_of),
                Err(error) => Box::pin(std::future::ready(Err(error))),
            },
            Operation::Launch {
                channel,
                app_id,
                package_root,
                on_behalf_of,
            } => {
                let answer = self.channel(channel).map(|session| {
                    session
                        .channel
                        .launch(&app_id, &package_root, &on_behalf_of)
                });
                Box::pin(async move { answer?.await.map_err(Refusal::refusal) })
            }
            Operation::Invocation {
                channel,
                id: invocation,
                call,
            } => {
                let answer = self
                    .channel(channel)
                    .map(|session| session.invocation_call(invocation, call));
                Box::pin(async move { answer?.await.map_err(Refusal::refusal) })
            }
            Operation::ControlInput {
                subscription,
                input,
            } => {
                let control = self
                    .controls
                    .borrow()
                    .get(&subscription)
                    .cloned()
                    .ok_or_else(|| format!("no workspace control session {subscription}"));
                Box::pin(async move {
                    control?.send_input(input).await?;
                    Ok(Reply::Done)
                })
            }
            Operation::Settings { channel, call } => {
                let answer = self
                    .channel(channel)
                    .map(|session| session.settings_call(call));
                Box::pin(async move { answer?.await.map_err(Refusal::refusal) })
            }
            Operation::LiveActivity { channel, call } => {
                let answer = self
                    .channel(channel)
                    .map(|session| session.live_activity_call(call));
                Box::pin(async move { answer?.await.map_err(Refusal::refusal) })
            }
            Operation::Admit { channel, report } => {
                let answer = self.channel(channel).map(|session| {
                    super::local_document_action::admit(Rc::clone(&session.channel), report)
                });
                Box::pin(
                    async move { answer?.await.map(Reply::Admitted).map_err(Refusal::refusal) },
                )
            }
            Operation::Snapshot { services, resource } => {
                let asking = self
                    .holds_published(services)
                    .and_then(|()| self.publishing());
                Box::pin(async move {
                    let publishing = asking?;
                    super::published::read(Ok(publishing
                        .published
                        .snapshot(&publishing.caller, &resource)))
                    .await
                })
            }
            Operation::Invoke {
                services,
                resource,
                action,
                arguments,
            } => Box::pin(super::published::invoke(
                self.holds_published(services)
                    .and_then(|()| self.publishing())
                    .map(|publishing| {
                        publishing.published.invoke(
                            &publishing.caller,
                            &resource,
                            &action,
                            arguments,
                        )
                    }),
            )),
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
        reason = "one arm per request the provider answers at once"
    )]
    #[allow(
        clippy::significant_drop_tightening,
        reason = "a subscription's channel is held while its feed is opened"
    )]
    fn dispatch(self: &Rc<Self>, id: u64, request: Operation) -> Result<Reply, String> {
        match request {
            Operation::Publish {
                channel,
                publication: Publication::Presence(presence),
            } => {
                let (key, payload) = crate::apps::collab_presence::from_peer(presence)
                    .ok_or_else(|| "presence does not decode".to_owned())?;
                self.channel(channel)?.presence.publish(key, payload)?;
                Ok(Reply::Done)
            }
            Operation::Publish {
                publication: Publication::Document(_) | Publication::Effect { .. },
                ..
            }
            | Operation::Join(_)
            | Operation::Workspace { .. }
            | Operation::Command { .. }
            | Operation::Changed { .. }
            | Operation::Invocation { .. }
            | Operation::Settings { .. }
            | Operation::LiveActivity { .. }
            | Operation::Admit { .. }
            | Operation::ControlInput { .. }
            | Operation::Launch { .. } => Err("answered when done".into()),
            // Both are answered by `call`, which waits for the session.
            Operation::Snapshot { .. } | Operation::Invoke { .. } => {
                Err("answered when done".into())
            }
            Operation::Publish {
                channel,
                publication: Publication::Report(report),
            } => {
                let session = self.channel(channel)?;
                let reported = session.report(&session.route().app_id, report);
                drop(session);
                reported.map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Publish {
                channel,
                publication: Publication::Surface(delta),
            } => {
                let delta = serde_json::from_str(&delta)
                    .map_err(|error| format!("invalid surface change: {error}"))?;
                let session = self.channel(channel)?;
                let published = session.publish_surface(&session.route().app_id, &delta);
                drop(session);
                published.map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Publish {
                channel,
                publication: Publication::Content { hash, bytes },
            } => {
                self.channel(channel)?
                    .offer_content(&hash, &bytes)
                    .map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Read {
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
                    .map_err(Refusal::refusal)?;
                Ok(Reply::History { ops, exhausted })
            }
            Operation::Read {
                channel,
                read: Read::Document,
            } => {
                self.channel(channel)?
                    .ask_for_document()
                    .map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Read {
                channel,
                read: Read::Content { hash },
            } => {
                self.channel(channel)?
                    .want_content(&hash)
                    .map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Subscribe { channel, topic } => {
                if self.subscriptions.borrow().contains_key(&id) {
                    return Err(format!("subscription {id} is already live"));
                }
                let session = self.channel(channel)?;
                let frames = self.frames.clone();
                let failed = Refusal::refusal;
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
                        let (stream, end) = session.invocation_feed(stable_ids).map_err(failed)?;
                        self.invocation_ends.borrow_mut().insert(id, end);
                        tokio::task::spawn_local(pump_pulled(stream, id, frames, Event::Invocation))
                    }
                    Topic::Catalogue => {
                        let stream = session.catalogue_feed().map_err(failed)?;
                        tokio::task::spawn_local(pump_pulled(stream, id, frames, Event::Catalogue))
                    }
                    Topic::WorkspaceControl { target_device } => {
                        let control = session.workspace_control(target_device).map_err(failed)?;
                        self.controls.borrow_mut().insert(id, Rc::clone(&control));
                        tokio::task::spawn_local(async move {
                            let reason = loop {
                                match control.read().await {
                                    Ok(event) => {
                                        let closed = matches!(
                                            event,
                                            runtime_collab::WorkspaceControlEvent::Closed(_)
                                        );
                                        if frames
                                            .send(HostFrame::Event {
                                                subscription: id,
                                                event: Event::WorkspaceControl(event),
                                            })
                                            .is_err()
                                        {
                                            return;
                                        }
                                        if closed {
                                            break "the workspace control session closed"
                                                .to_owned();
                                        }
                                    }
                                    Err(error) => break error,
                                }
                            };
                            let _ = frames.send(HostFrame::Ended {
                                subscription: id,
                                reason,
                            });
                        })
                    }
                    Topic::Realtime => {
                        let feed = session.realtime_feed().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |event| {
                            Some(Event::Realtime(event))
                        }))
                    }
                    Topic::ApprovedCheck(request) => {
                        let progress = runtime_collab::approved_checks::Request::decode(&request)
                            .and_then(|request| {
                                super::approved_checks::context(&session.channel, request)
                            })
                            .and_then(super::approved_checks::reported_progress)?;
                        tokio::task::spawn_local(pump_pulled(
                            progress,
                            id,
                            frames,
                            Event::ApprovedCheck,
                        ))
                    }
                    Topic::Content => {
                        let feed = session.subscribe_content().map_err(failed)?;
                        tokio::task::spawn_local(pump(
                            feed,
                            id,
                            frames,
                            |news: crate::content::ContentNews| {
                                Some(Event::Content {
                                    hash: news.hash,
                                    state: news.state,
                                    bytes: news.bytes,
                                })
                            },
                        ))
                    }
                    Topic::ShownActions => {
                        let feed = session.shown_actions().map_err(failed)?;
                        tokio::task::spawn_local(pump(feed, id, frames, |decisions| {
                            Some(Event::ShownActions(decisions))
                        }))
                    }
                    Topic::Resources(query) => {
                        let opening = session.resources_open(&query).map_err(failed)?;
                        tokio::task::spawn_local(async move {
                            let reason = match opening.await {
                                Ok(mut pages) => loop {
                                    let Some(page) = pages.recv().await else {
                                        break "the store let go of the query".to_owned();
                                    };
                                    let Ok(page) = serde_json::to_vec(&page) else {
                                        break "the store's page cannot be encoded".to_owned();
                                    };
                                    let event = Event::Resources(page);
                                    if frames
                                        .send(HostFrame::Event {
                                            subscription: id,
                                            event,
                                        })
                                        .is_err()
                                    {
                                        return;
                                    }
                                },
                                Err(error) => error,
                            };
                            let _ = frames.send(HostFrame::Ended {
                                subscription: id,
                                reason,
                            });
                        })
                    }
                };
                self.subscriptions
                    .borrow_mut()
                    .insert(id, (channel, task.abort_handle()));
                Ok(Reply::Subscribed)
            }
            Operation::Answer { ask, answer } => {
                let waiting = self
                    .asks
                    .borrow_mut()
                    .remove(&ask)
                    .ok_or_else(|| format!("no ask {ask} is waiting"))?;
                let _ = waiting.send(answer);
                Ok(Reply::Done)
            }
            Operation::Publish {
                channel,
                publication: Publication::Directory,
            } => {
                self.channel(channel)?
                    .publish_directory(Rc::new(WireDirectory {
                        connection: Rc::downgrade(self),
                        channel,
                    }))
                    .map_err(Refusal::refusal)?;
                Ok(Reply::Done)
            }
            Operation::Unsubscribe { subscription } => {
                // An app that asks for no more invocations is still handed every one already
                // addressed to it, then told the stream ended: it answers them before it goes,
                // so none is left with an outcome nobody knows.
                if let Some(end) = self.invocation_ends.borrow_mut().remove(&subscription) {
                    end.finish();
                    self.subscriptions.borrow_mut().remove(&subscription);
                    return Ok(Reply::Done);
                }
                if let Some(control) = self.controls.borrow_mut().remove(&subscription) {
                    close_control(control);
                }
                let (_, task) = self
                    .subscriptions
                    .borrow_mut()
                    .remove(&subscription)
                    .ok_or_else(|| format!("no subscription {subscription}"))?;
                task.abort();
                Ok(Reply::Done)
            }
            Operation::Services => {
                self.publishing()?;
                Ok(Reply::Opened(self.open_published()))
            }
            Operation::Watch { services, resource } => {
                self.holds_published(services)?;
                let publishing = self.publishing()?;
                if self.subscriptions.borrow().contains_key(&id) {
                    return Err(format!("subscription {id} is already live"));
                }
                let (readings, deliveries) = mpsc::unbounded_channel();
                let watch = publishing
                    .published
                    .watch(&publishing.caller, &resource, readings)?;
                let task = super::published::serving(
                    publishing.published,
                    watch,
                    deliveries,
                    self.frames.clone(),
                    id,
                );
                self.subscriptions.borrow_mut().insert(id, (services, task));
                Ok(Reply::Subscribed)
            }
            Operation::Close { handle } => {
                if self.published.get() == Some(handle) {
                    self.close_published();
                    return Ok(Reply::Done);
                }
                self.channels
                    .borrow_mut()
                    .remove(&handle)
                    .ok_or_else(|| format!("no channel {} on this connection", handle.0))?;
                let mut controls = self.controls.borrow_mut();
                self.subscriptions
                    .borrow_mut()
                    .retain(|subscription, (channel, task)| {
                        let keep = *channel != handle;
                        if !keep {
                            task.abort();
                            if let Some(control) = controls.remove(subscription) {
                                close_control(control);
                            }
                        }
                        keep
                    });
                Ok(Reply::Done)
            }
        }
    }
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
            Err(error) => break error.refusal(),
        }
    };
    let _ = frames.send(HostFrame::Ended {
        subscription,
        reason,
    });
}

/// A stream the broker pulls one item at a time.
trait Pulled {
    type Item;
    async fn pull(&self) -> Result<Self::Item, BrokerError>;
}

impl Pulled for InvocationStreamServer {
    type Item = runtime_collab::wire::Invocation;
    async fn pull(&self) -> Result<Self::Item, BrokerError> {
        self.next().await?.on_wire()
    }
}

impl Pulled for super::approved_checks::ReportedProgress {
    type Item = CheckProgress;
    async fn pull(&self) -> Result<Self::Item, BrokerError> {
        self.next().await.map_err(BrokerError::failed)
    }
}

impl Pulled for ActionCatalogueStreamServer {
    type Item = String;
    async fn pull(&self) -> Result<Self::Item, BrokerError> {
        self.next().await
    }
}

async fn pump_pulled<S: Pulled>(
    stream: S,
    subscription: u64,
    frames: mpsc::UnboundedSender<HostFrame>,
    event: impl Fn(S::Item) -> Event,
) {
    let reason = loop {
        match stream.pull().await {
            Ok(item) => {
                let event = event(item);
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
            Err(error) => break error.refusal(),
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

/// What an endpoint is told of a refusal: its reason, without the kind of
/// error the broker carried it as.
pub(super) trait Refusal {
    fn refusal(self) -> String;
}

impl Refusal for BrokerError {
    fn refusal(self) -> String {
        self.reason
    }
}

impl Refusal for String {
    fn refusal(self) -> String {
        self
    }
}

/// An answer already given.
fn ready<T: 'static>(answer: T) -> Answer<T> {
    Box::pin(std::future::ready(Ok(answer)))
}

/// The app's remote-action `message` refused with `error`.
fn refusal(
    message: &tilo_actions::remote::RemoteActionMessage,
    error: tilo_actions::remote::ActionCallError,
) -> Result<Answer<tilo_actions::remote::RemoteActionMessage>, BrokerError> {
    tilo_actions::remote::refuse(message, error)
        .map(ready)
        .ok_or_else(|| BrokerError::failed("only remote-action requests are answered".into()))
}
