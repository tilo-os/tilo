//! The boundary through which a host may serve apps to its devices.
//!
//! A host itself runs no app process of its own: devices ask its action service
//! for an app's actions, and it answers what it can without running anything. A
//! process that embeds the host and *does* serve apps supplies an
//! [`AppActionServer`] to it ([`ServiceConfig::apps`](crate::service::ServiceConfig::apps));
//! requests for an app's actions are then answered through [`Served`], and a
//! server advertises its `app_serving` service.
//! Without one, the capability stays off and such requests are refused at once
//! — all but the governance of the workspaces the server governs, which it
//! answers itself (`crate::workspaces::GovernedServer`).

use std::sync::Arc;

use tilo_actions::remote::{ActionProvider, Caller, Pending, RemoteActionMessage, ResourceReads};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::page::{OpenedPage, PageAction, PageAddress, PagePatch, PageRefusal};

/// Answers remote-action requests addressed to this host.
///
/// `caller` is the actor the server authenticated for the connection the
/// request arrived on; implementations decide authority from it alone.
pub trait AppActionServer: Send + Sync {
    /// Answer one message for `workspace_id` — whenever the answer is ready,
    /// never blocking the connection — or `None` when it needs no reply.
    fn serve(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>>;

    /// As [`Self::serve`], with the certificate the server's handshake
    /// verified for `caller`'s device: what the server hands every message
    /// that arrives on its actions link. The host's own operations need it
    /// (they admit that certificate to a workspace); an app action server has
    /// no use for it, and the default ignores it.
    fn serve_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let _ = certificate;
        self.serve(workspace_id, caller, message)
    }

    /// Answer one of the host's own operations (`tilo.host`), asked of the
    /// host itself by `caller` on the device `certificate` names, as the
    /// server's handshake verified it: no workspace is involved. An app action server that
    /// governs no workspaces answers none.
    fn serve_host(
        &self,
        caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let _ = (caller, certificate, message);
        None
    }

    /// Whether this app action server answers `message` from `caller` at all. One that
    /// does not is refused as undeliverable, as by a host that serves no
    /// apps.
    fn answers(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        let _ = (caller, message);
        true
    }

    /// The content stores this app action server keeps its workspaces' content in, when
    /// it brings them: the host then keeps everything else it keeps for a
    /// workspace there too, so a host has one store per workspace. `None`
    /// leaves the host to open its own.
    #[cfg(not(target_arch = "wasm32"))]
    fn content(&self) -> Option<std::sync::Arc<tilo_content::WorkspaceStores>> {
        None
    }

    /// `caller`'s requests on its link are one run of an agent in
    /// `workspace_id`, the workspace the link entered: an app action server that
    /// speaks for the runs it executes makes the run present there now, so an
    /// agent that connected and has not acted yet is already seen, waiting.
    /// Nothing by default.
    fn session_arrived(&self, workspace_id: WorkspaceId, caller: &Caller) {
        let _ = (workspace_id, caller);
    }

    /// The workspace `workspace_id` was deleted: let go of whatever this
    /// app action server holds for it (its app workers, what it found of the
    /// workspace). Nothing by default.
    fn forget_workspace(&self, workspace_id: WorkspaceId) {
        let _ = workspace_id;
    }

    /// Stop answering and let go of what this app action server holds, its
    /// own worker threads included, serving what it holds before it returns.
    /// The host calls it as the service whose links it answers stops,
    /// while the runtime those threads work on is still running. Later
    /// requests are refused as unavailable. Nothing by default.
    fn shutdown(&self) {}

    /// Where `message`, sent to this host by `caller`, goes instead of this
    /// app action server: to the device of `workspace_id` that holds what it addresses, or
    /// to the caller as this app action server's own answer. Some actions exist only on a
    /// window, and the window need not be on this host (a Browser whose page
    /// runs on a VM host). The host hands the frame to that device stamped
    /// with `caller`, as it hands over every frame it routes, so the action
    /// runs there as the caller's. `None` serves it here, as before.
    fn route(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        message: &RemoteActionMessage,
    ) -> Option<Routing> {
        let _ = (workspace_id, caller, message);
        None
    }

    /// As [`Self::route`], with the certificate the server's handshake
    /// verified for `caller`'s device: what the app action server may hold the caller to
    /// beyond who it is — a key's own scope, say. An app action server that needs only the
    /// caller routes as before.
    fn route_certified(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: &RemoteActionMessage,
    ) -> Option<Routing> {
        let _ = certificate;
        self.route(workspace_id, caller, message)
    }

    /// What device `from` answered `message` in `workspace_id`, on its way
    /// to the caller: the host only carries it, and an app action server that handed the
    /// frame over may want to know how it went. The answer the app action server gives
    /// here is what the caller receives in its place — a window that refuses
    /// an action for want of the person's word leaves its caller waiting
    /// instead of refused — and `None` carries the answer on as it is.
    fn relayed(
        &self,
        workspace_id: WorkspaceId,
        from: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        let _ = (workspace_id, from, message);
        None
    }

    /// Whether what device `from` sends `to` in `workspace_id` answers frames
    /// this app action server routed from `to` to `from` ([`Self::route`]): the host then
    /// stamps it as its own, since `to` asked this host and hears its answers
    /// from the device it asked. `false` by default: a frame is stamped with
    /// the device that sent it.
    fn answers_for_host(&self, workspace_id: WorkspaceId, from: [u8; 32], to: [u8; 32]) -> bool {
        let _ = (workspace_id, from, to);
        false
    }

    /// Whether this app action server waits on answers to frames of `workspace_id`: the
    /// host decodes what it forwards there for [`Self::relayed`], and reads
    /// the rest untouched.
    fn tracks(&self, workspace_id: WorkspaceId) -> bool {
        let _ = workspace_id;
        false
    }

    /// The workspaces' apps as web pages its members open
    /// ([`tilo_server_protocol::page`]), where this app action server shows
    /// them. None by default.
    fn pages(&self) -> Option<&dyn AppPages> {
        None
    }
}

/// A workspace's apps as web pages: each page an app's view of the
/// workspace's document, as the member who opened it may see it.
pub trait AppPages: Send + Sync {
    /// Show `caller` the page `address` names in `workspace_id`.
    fn open(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        address: PageAddress,
    ) -> Pending<Result<OpenedPage, PageRefusal>>;

    /// Act on `page` as its person did (the element's number, its semantic
    /// target and the action), under the call they signed for it when they
    /// signed one, or with no act, answer what changed on it since others
    /// wrote: the elements that changed, outermost first.
    fn act(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<PageAct>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>>;

    /// Let go of `page`: its person closed it.
    fn close(&self, workspace_id: WorkspaceId, page: u64);
}

/// What a page's person did on it ([`AppPages::act`]).
pub struct PageAct {
    /// The element's number (`data-p`).
    pub id: u32,
    /// The semantic target it carries (`data-t`).
    pub target: Option<String>,
    pub action: PageAction,
    /// The call they signed for it, naming the page's app, when they signed one.
    pub signed: Option<Box<tilo_actions::remote::SignedCall>>,
}

/// Where a frame is served instead of on the host it was addressed to
/// ([`AppActionServer::route`]).
pub enum Routing {
    /// The device of the workspace holding the window the frame addresses.
    Device([u8; 32]),
    /// That device, handed `message` in place of the frame the caller sent:
    /// what an app action server that answers for the person hands over — the caller's own
    /// action, carrying an answer the caller could not put there itself.
    Replaced {
        to: [u8; 32],
        message: Box<RemoteActionMessage>,
    },
    /// This app action server answers the frame itself — the caller's typed refusal, or
    /// the answer to an invocation it holds — now or when it is ready.
    Answer(Pending<RemoteActionMessage>),
}

/// The host's way to hand a request to a device it served a workspace's apps
/// to — a window whose page runs on a machine of its own: the device action
/// service carries it, over the link that device holds open to this host,
/// exactly as the host asks any other device.
///
/// A message handed for another caller than this host is one the caller signed
/// (`InvokeRequest::signed`); the device it reaches runs it as that caller,
/// since the link a page holds proves no device of its own. A message this
/// host composed is its own, and runs as this host.
#[derive(Clone)]
pub struct Hand {
    /// The action service that reaches the devices, where this host serves
    /// workspaces at all.
    actions: Option<tilo_peer_state::actions::ActionService>,
    /// The apps this host serves, told what the device it handed a request to
    /// answered: an app action server counts what ran where it was handed.
    /// Held weakly, as the app action server may hold this hand.
    served: Option<std::sync::Weak<dyn AppActionServer>>,
}

impl Hand {
    /// A hand that reaches devices over `actions`, reporting what they answer to
    /// `served`.
    #[must_use]
    pub fn new(
        actions: tilo_peer_state::actions::ActionService,
        served: Option<Arc<dyn AppActionServer>>,
    ) -> Self {
        Self {
            actions: Some(actions),
            served: served.map(|served| Arc::downgrade(&served)),
        }
    }

    /// A hand for a host that holds no workspace of its own: it hands nothing
    /// on, which is logged.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            actions: None,
            served: None,
        }
    }

    /// Hand `message` to device `to` of `workspace_id`, for the caller `from`
    /// (device and user key): over the link that device holds open to this
    /// host, through the action service. A device this host holds no link to
    /// takes nothing, which is logged: what waits on it is answered by its own
    /// timeout.
    pub fn hand(
        &self,
        workspace_id: WorkspaceId,
        from: ([u8; 32], [u8; 32]),
        to: [u8; 32],
        message: tilo_actions::remote::RemoteActionMessage,
    ) {
        let Some(remote) = self
            .actions
            .as_ref()
            .and_then(|actions| actions.remote_held(to, &workspace_id.to_string()))
        else {
            tracing::warn!(
                workspace_id = %workspace_id,
                device = %hex::encode(to),
                "a request was handed to a device this host holds no link to"
            );
            return;
        };
        // A message handed for a caller other than this host runs as the device that signed it,
        // which the message must carry; the receiver reads that signature, never this host's word.
        if !matches!(
            &message,
            tilo_actions::remote::RemoteActionMessage::Invoke(request) if request.signed.is_some()
        ) {
            tracing::debug!(
                workspace_id = %workspace_id,
                device = %hex::encode(to),
                caller = %hex::encode(from.0),
                "a request handed on carries no signature of its caller and runs as this host"
            );
        }
        let reporter = self.served.clone();
        remote.ask_message(message).then(move |answer| {
            if let (Some(reporter), Some(Some(answer))) = (reporter, answer)
                && let Some(server) = reporter.upgrade()
            {
                // What the device holding the window answered, as the app action server reads
                // it: it counts the run, and answers the caller that follows it.
                let _ = server.relayed(workspace_id, to, &answer);
            }
        });
    }
}

/// The apps this host serves, as a provider on its action service: how a
/// device of a workspace reaches the apps here, and how a page action that
/// only a window on some device can run is handed to that device.
#[derive(Clone)]
pub(crate) struct Served(Arc<ServedInner>);

struct ServedInner {
    /// The apps this host serves, held weakly: what it serves may hold this
    /// provider ([`Self::actions`]).
    server: std::sync::Weak<dyn AppActionServer>,
    /// The action service this provider answers on, which also reaches the
    /// devices holding windows: held weakly, as it holds this provider.
    actions: tilo_peer_state::actions::WeakActions,
    /// The workspaces this host holds, read for the certificate a caller's
    /// scope is decided by.
    workspaces: Arc<crate::workspaces::device_actions::WorkspacesMap>,
    runtime: tokio::runtime::Handle,
}

impl Served {
    /// The apps `server` serves, answered on `actions` beside the host's own
    /// actions, with `workspaces` read for who asks.
    #[must_use]
    pub(crate) fn new(
        server: &Arc<dyn AppActionServer>,
        workspaces: Arc<crate::workspaces::device_actions::WorkspacesMap>,
        actions: &tilo_peer_state::actions::ActionService,
    ) -> Self {
        Self(Arc::new(ServedInner {
            server: Arc::downgrade(server),
            actions: actions.weak(),
            workspaces,
            runtime: tokio::runtime::Handle::current(),
        }))
    }

    /// The certificate this host holds for `caller`'s device in `workspace`,
    /// when it holds one.
    async fn certificate(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
    ) -> Option<tilo_user_identity::DeviceCert> {
        crate::workspaces::device_actions::certificate_of(
            &self.0.workspaces,
            &workspace.to_string(),
            *caller.device_public_key()?,
        )
        .await
    }

    /// Hand `message` to device `to` of `workspace` over the link it holds
    /// open to this host, and answer with what it made of it.
    fn hand_to(
        &self,
        workspace: WorkspaceId,
        to: [u8; 32],
        message: RemoteActionMessage,
    ) -> Pending<RemoteActionMessage> {
        let Some(remote) = self
            .0
            .actions
            .upgrade()
            .and_then(|actions| actions.remote_held(to, &workspace.to_string()))
        else {
            let refused = tilo_actions::remote::refuse(
                &message,
                tilo_actions::remote::ActionCallError::Unavailable {
                    reason: "no link reaches the device this request names".into(),
                },
            );
            return Pending::ready(refused.unwrap_or(message));
        };
        let server = self.0.server.upgrade();
        let (completer, answer) = tilo_actions::remote::pending();
        remote.ask_message(message).then(move |answer| {
            let Some(Some(mut answer)) = answer else {
                return;
            };
            // What the device holding the window answered, as the app action server reads it: a
            // window that refuses a page action for want of the person's word answers instead.
            if let Some(server) = &server {
                answer = server.relayed(workspace, to, &answer).unwrap_or(answer);
            }
            completer.complete(answer);
        });
        answer
    }

    /// Answer `message` from `caller` in `workspace`: where a device holding
    /// what it addresses runs it, there; else through the apps this host
    /// serves.
    async fn answer(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Pending<RemoteActionMessage> {
        let Some(server) = self.0.server.upgrade() else {
            return Self::refused(message, "this host serves no apps any more");
        };
        let certificate = self.certificate(workspace, caller).await;
        let route = if let Some(certificate) = &certificate {
            server.route_certified(workspace, caller, certificate, &message)
        } else {
            server.route(workspace, caller, &message)
        };
        match route {
            Some(Routing::Device(to)) => self.hand_to(workspace, to, message),
            Some(Routing::Replaced { to, message }) => self.hand_to(workspace, to, *message),
            Some(Routing::Answer(answer)) => answer,
            None => {
                let reply = if let Some(certificate) = &certificate {
                    server.serve_certified(workspace, caller, certificate, message.clone())
                } else {
                    server.serve(workspace, caller, message.clone())
                };
                reply.unwrap_or_else(|| {
                    Self::refused(message, "the host does not answer that request")
                })
            }
        }
    }

    /// `message` refused for `reason`, so its asker learns at once instead of
    /// waiting for an answer that never comes.
    fn refused(message: RemoteActionMessage, reason: &str) -> Pending<RemoteActionMessage> {
        Pending::ready(
            tilo_actions::remote::refuse(
                &message,
                tilo_actions::remote::ActionCallError::Unavailable {
                    reason: reason.to_owned(),
                },
            )
            .unwrap_or(message),
        )
    }

    /// What `workspace` `caller`'s request names, or why it names none.
    fn workspace_of(caller: &Caller) -> Option<WorkspaceId> {
        WorkspaceId::parse(caller.workspace()?)
    }

    /// Ask `workspace`'s apps for `message` from `caller` off this thread, and
    /// hand back what they answer.
    fn later(
        &self,
        workspace: WorkspaceId,
        caller: Caller,
        message: RemoteActionMessage,
    ) -> Pending<RemoteActionMessage> {
        let (completer, answer) = tilo_actions::remote::pending();
        let this = self.clone();
        self.0.runtime.spawn(async move {
            let reply = this.answer(workspace, &caller, message).await.await;
            if let Some(reply) = reply {
                completer.complete(reply);
            }
        });
        answer
    }
}

/// The record reads this host's action service answers: each goes whole to the app action
/// server, whose reader is the device's one ([`ResourceReads`]), deciding it for who asks as it
/// decides a local app's, an agent's or another device's.
struct ServedReads(Served);

impl ServedReads {
    fn whole() -> tilo_actions::remote::ActionCallError {
        tilo_actions::remote::ActionCallError::Unavailable {
            reason: "a read goes whole to the device's reader".into(),
        }
    }
}

impl ResourceReads for ServedReads {
    fn read(
        &self,
        _caller: &Caller,
        _address: &tilo_actions::remote::ResourceAddress,
    ) -> Result<tilo_actions::remote::ResourceCopy, tilo_actions::remote::ActionCallError> {
        Err(Self::whole())
    }

    fn watch(
        &self,
        _caller: &Caller,
        _address: &tilo_actions::remote::ResourceAddress,
        _wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        Err(Self::whole())
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: tilo_actions::remote::ResourceReadRequest,
    ) -> Pending<RemoteActionMessage> {
        let Some(workspace) =
            WorkspaceId::parse(&read.workspace).or_else(|| Served::workspace_of(caller))
        else {
            return Pending::ready(RemoteActionMessage::ResourceReading {
                correlation,
                result: Err(tilo_actions::remote::ActionCallError::NotPermitted),
            });
        };
        self.0.later(
            workspace,
            caller.clone(),
            RemoteActionMessage::ResourceRead {
                correlation,
                on_behalf_of: None,
                read,
            },
        )
    }
}

impl ActionProvider for Served {
    fn serves(&self, _provider: &str) -> bool {
        true
    }

    fn resource_reads(&self) -> Option<Arc<dyn ResourceReads>> {
        Some(Arc::new(ServedReads(self.clone())))
    }

    fn query(
        &self,
        caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>>
    {
        let Some(workspace) = Served::workspace_of(caller) else {
            return Pending::ready(Err(tilo_actions::remote::ActionCallError::NotPermitted));
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let this = self.clone();
        let caller = caller.clone();
        self.0.runtime.spawn(async move {
            let reply = this
                .answer(
                    workspace,
                    &caller,
                    RemoteActionMessage::CatalogueRequest {
                        correlation: 0,
                        query,
                    },
                )
                .await
                .await;
            completer.complete(match reply {
                Some(RemoteActionMessage::CatalogueResponse { result, .. }) => result,
                _ => Err(tilo_actions::remote::ActionCallError::Unavailable {
                    reason: "the host does not answer that request".into(),
                }),
            });
        });
        answer
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        let Some(workspace) = Served::workspace_of(caller) else {
            return Pending::ready(Err(tilo_actions::remote::ActionCallError::NotPermitted));
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let this = self.clone();
        let caller = caller.clone();
        self.0.runtime.spawn(async move {
            let reply = this
                .answer(workspace, &caller, RemoteActionMessage::Invoke(request))
                .await
                .await;
            completer.complete(match reply {
                Some(RemoteActionMessage::Receipt(receipt)) => match receipt.status {
                    tilo_actions::remote::ReceiptStatus::Rejected { error } => Err(error),
                    _ => Ok(receipt),
                },
                _ => Err(tilo_actions::remote::ActionCallError::Unavailable {
                    reason: "the host does not answer that request".into(),
                }),
            });
        });
        answer
    }

    /// What a window reported of an invocation it runs is answered by the apps
    /// this host serves at once; one still to move is followed with
    /// [`Self::next_change`].
    fn observe(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        let workspace = Served::workspace_of(caller)?;
        let mut answer = self.later(
            workspace,
            caller.clone(),
            RemoteActionMessage::Observe {
                correlation: 0,
                invocation: *id,
                wait_ms: None,
                seen: None,
            },
        );
        match answer.poll_ready() {
            tilo_actions::remote::PendingPoll::Ready(RemoteActionMessage::Update {
                state, ..
            }) => state,
            _ => None,
        }
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        let workspace = Served::workspace_of(caller)?;
        let (changed, change) = tilo_actions::remote::pending();
        let this = self.clone();
        let caller = caller.clone();
        let id = *id;
        self.0.runtime.spawn(async move {
            let _ = this
                .later(
                    workspace,
                    caller,
                    RemoteActionMessage::Observe {
                        correlation: 0,
                        invocation: id,
                        wait_ms: Some(tilo_peer_state::actions::CHANGE_WAIT_MS),
                        seen: None,
                    },
                )
                .await;
            changed.complete(());
        });
        Some(change)
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        let Some(workspace) = Served::workspace_of(caller) else {
            return Pending::ready(Err(tilo_actions::remote::ActionCallError::NotPermitted));
        };
        let (completer, answer) = tilo_actions::remote::pending();
        let this = self.clone();
        let caller = caller.clone();
        let id = *id;
        self.0.runtime.spawn(async move {
            let reply = this
                .later(
                    workspace,
                    caller,
                    RemoteActionMessage::Cancel { invocation: id },
                )
                .await;
            completer.complete(match reply {
                Some(RemoteActionMessage::CancelResponse { result, .. }) => result,
                _ => Err(tilo_actions::remote::ActionCallError::Unavailable {
                    reason: "the host does not answer that request".into(),
                }),
            });
        });
        answer
    }
}

/// An [`AppActionServer`] backed by one [`ActionProvider`] for every workspace.
pub struct ActionProviderServer<H> {
    host: H,
}

impl<H> ActionProviderServer<H> {
    /// Serve every workspace from `host`.
    pub const fn new(host: H) -> Self {
        Self { host }
    }
}

/// `H` is cloned into an answer that waits for an invocation to move.
impl<H: ActionProvider + Clone + Send + Sync + 'static> AppActionServer
    for ActionProviderServer<H>
{
    fn serve(
        &self,
        _workspace_id: WorkspaceId,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        tilo_actions::remote::serve(&self.host, caller, message)
    }
}

/// The host's own operations ([`HOST_ACTIONS_PROVIDER`]) answered on its action service for
/// requests that name no workspace: a page's whole workspace administration, and the operations
/// the catalogue publishes, asked of the host itself.
///
/// No workspace's roster names who asks here, so each request proves itself: the signed
/// invocation it carries is checked against the host's revocations, and the certificate it names
/// is who the host answers, exactly as it answers that device's own session.
pub(crate) struct HostOperations {
    server: Arc<dyn AppActionServer>,
    revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    runtime: tokio::runtime::Handle,
}

impl HostOperations {
    /// Answer `server`'s own operations for the devices the host's revocations do not refuse.
    ///
    /// # Panics
    /// Outside a runtime context, where there is nothing to answer on.
    #[must_use]
    pub(crate) fn new(
        server: Arc<dyn AppActionServer>,
        revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    ) -> Self {
        Self {
            server,
            revocations,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// The certificate the signed invocation `request` carries, checked as a host checks one:
    /// signed by its device, fresh, and not revoked.
    fn certificate(
        &self,
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<tilo_user_identity::DeviceCert, tilo_actions::remote::ActionCallError> {
        let revocations = self.revocations.read().map_err(|_| {
            tilo_actions::remote::ActionCallError::Unavailable {
                reason: "this host cannot read its revocations".into(),
            }
        })?;
        tilo_server_protocol::invocation::verify_invocation(
            "",
            request,
            &revocations,
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| tilo_actions::remote::ActionCallError::Refused {
            reason: format!("the request does not prove its device: {error}"),
        })
    }
}

impl ActionProvider for HostOperations {
    fn serves(&self, provider: &str) -> bool {
        provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>>
    {
        // The host's operations are listed where a workspace's catalogue is read, from the
        // certificate a workspace names; a request that names none reads none.
        Pending::ready(Ok(tilo_actions::remote::CataloguePage::from_rows(
            "0".into(),
            Vec::new(),
            &query,
        )))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::ActionCallError;
        let certificate = match self.certificate(&request) {
            Ok(certificate) => certificate,
            Err(error) => return Pending::ready(Err(error)),
        };
        let caller =
            Caller::authenticated(certificate.user_public_key, certificate.device_public_key);
        let server = Arc::clone(&self.server);
        let (completer, answer) = tilo_actions::remote::pending();
        self.runtime.spawn(async move {
            let Some(reply) =
                server.serve_host(&caller, &certificate, RemoteActionMessage::Invoke(request))
            else {
                completer.complete(Err(ActionCallError::Unavailable {
                    reason: "this host answers no such operation".into(),
                }));
                return;
            };
            completer.complete(match reply.await {
                Some(RemoteActionMessage::Receipt(receipt)) => Ok(receipt),
                _ => Err(ActionCallError::Unavailable {
                    reason: "the host answered no receipt".into(),
                }),
            });
        });
        answer
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}

/// The entry a device asks before it is a member, answered on the host's action service for the
/// requests that name no workspace ([`tilo_server_protocol::workspaces::ENTRY_SCOPE`]).
///
/// A device without a certificate from its host proves itself twice: its certificate is checked
/// as a certified device's is, and its signature over the request proves it holds the key that
/// certificate names. Only the entry action is answered here; nothing else is reachable in this
/// scope without a certificate.
pub(crate) struct EntryOperations {
    workspaces: crate::workspaces::Workspaces,
    revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    runtime: tokio::runtime::Handle,
}

impl EntryOperations {
    /// Answer the entry for the devices the host's revocations do not refuse.
    ///
    /// # Panics
    /// Outside a runtime context, where there is nothing to answer on.
    #[must_use]
    pub(crate) fn new(
        workspaces: crate::workspaces::Workspaces,
        revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    ) -> Self {
        Self {
            workspaces,
            revocations,
            runtime: tokio::runtime::Handle::current(),
        }
    }
}

impl ActionProvider for EntryOperations {
    fn serves(&self, provider: &str) -> bool {
        provider == tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
    }

    fn query(
        &self,
        _caller: &Caller,
        query: tilo_actions::remote::CatalogueQuery,
    ) -> Pending<Result<tilo_actions::remote::CataloguePage, tilo_actions::remote::ActionCallError>>
    {
        Pending::ready(Ok(tilo_actions::remote::CataloguePage::from_rows(
            "0".into(),
            Vec::new(),
            &query,
        )))
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: tilo_actions::remote::InvokeRequest,
    ) -> Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError>,
    > {
        use tilo_actions::remote::ActionCallError;
        // Nothing but the entry is answered here: a caller with no certificate reaches no other
        // action, however it names one.
        if request.target.provider != tilo_server_protocol::workspaces::HOST_ACTIONS_PROVIDER
            || request.target.address.action != tilo_server_protocol::workspaces::ENTER_ENTRY
        {
            return Pending::ready(Err(ActionCallError::UnknownAction));
        }
        let Some(source) = caller.device_public_key().copied() else {
            return Pending::ready(Err(ActionCallError::NotPermitted));
        };
        let tilo_actions::remote::InvokeArgument::Json(arguments) = &request.argument else {
            return Pending::ready(Err(ActionCallError::InvalidRequest {
                reason: "an entry names its entry and certificate".into(),
            }));
        };
        let (entering, cert) = match tilo_server_protocol::workspaces::entered_request(arguments) {
            Ok(asked) => asked,
            Err(reason) => return Pending::ready(Err(ActionCallError::InvalidRequest { reason })),
        };
        // The certificate is the device asking, and a certified one the host does not refuse.
        let refused = match self.revocations.read() {
            Ok(revocations) => {
                let now = tilo_clock::unix_now_ns();
                tilo_user_identity::verify_device_cert_at(&cert, now).is_err()
                    || cert.device_public_key != source
                    || revocations.refuses_at(&cert, now)
            }
            Err(_) => true,
        };
        if refused {
            return Pending::ready(Err(ActionCallError::Refused {
                reason: "the entry's certificate is not one this host takes".into(),
            }));
        }
        let invocation = request.invocation;
        let (completer, answer) = tilo_actions::remote::pending();
        let workspaces = self.workspaces.clone();
        self.runtime.spawn(async move {
            let entry = tilo_server_protocol::entering::Entry {
                entering,
                access: None,
                proof: [0; 64],
            };
            let entered = crate::handshake::Admit::enter(&workspaces, &cert, entry).await;
            completer.complete(
                tilo_server_protocol::workspaces::entered_answer(&entered)
                    .map(|output| tilo_actions::remote::InvocationReceipt {
                        invocation,
                        status: tilo_actions::remote::ReceiptStatus::Completed {
                            output: tilo_actions::ActionOutput {
                                value: Some(output),
                                resources: Vec::new(),
                            },
                        },
                    })
                    .map_err(|reason| ActionCallError::Refused { reason }),
            );
        });
        answer
    }

    fn observe(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Option<tilo_actions::remote::InvocationState> {
        None
    }

    fn cancel(
        &self,
        _caller: &Caller,
        _id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), tilo_actions::remote::ActionCallError>> {
        Pending::ready(Err(
            tilo_actions::remote::ActionCallError::UnknownInvocation,
        ))
    }
}
