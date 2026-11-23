//! The boundary through which a host may serve apps to remote callers.
//!
//! The server itself runs no apps and links none: it carries
//! [`Envelope::RemoteAction`](tilo_server_protocol::transport::Envelope::RemoteAction) frames between
//! devices of a workspace without executing them. A process that embeds the
//! relay and *does* host apps supplies an [`AppActionServer`] to the host
//! ([`ServiceConfig::apps`](crate::service::ServiceConfig::apps)); frames addressed
//! to the host's own device are then answered by it, and a server advertises
//! its `app_serving` service.
//! Without one, the capability stays off and such frames are refused as
//! undeliverable — all but the governance of the workspaces the server governs,
//! which it answers itself (`crate::workspaces::GovernedServer`).

use tilo_actions::remote::{ActionProvider, Caller, Pending, RemoteActionMessage};
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
    /// that arrives on a session. The host's own operations need it (they
    /// admit that certificate to a workspace); an app action server has no use
    /// for it, and the default ignores it.
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

    /// `caller`'s session became an agent's run
    /// ([`Envelope::AgentSession`](tilo_server_protocol::transport::Envelope::AgentSession))
    /// in `workspace_id`, the workspace the session entered: an app action server that
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
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
        invoker: Option<String>,
    ) -> Pending<Result<OpenedPage, PageRefusal>>;

    /// Act on `page` as its person did (the element's number, its semantic
    /// target and the action), or with no act, answer what changed on it
    /// since others wrote: the elements that changed, outermost first.
    fn act(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        act: Option<(u32, Option<String>, PageAction)>,
        signed: Option<Box<tilo_actions::remote::SignedCall>>,
        invoker: Option<String>,
    ) -> Pending<Result<Vec<PagePatch>, PageRefusal>>;

    /// Let go of `page`: its person closed it.
    fn close(&self, workspace_id: WorkspaceId, page: u64);

    /// `page` was opened for `person`: what the sheet the page draws over its
    /// app is put to, and whose answer it carries. Nothing by default.
    fn opened(&self, workspace_id: WorkspaceId, page: u64, person: &Caller) {
        let _ = (workspace_id, page, person);
    }

    /// The questions the page's person is asked, as the sheet the page draws
    /// over its app: HTML, or empty while nothing waits for their answer. A
    /// page holding their identity shows them the questions their agents' hosts
    /// put to them, in the same words a native session's sheet does.
    fn question_sheet(&self, workspace_id: WorkspaceId, page: u64) -> String {
        let _ = (workspace_id, page);
        String::new()
    }

    /// Answer the question `target` names on `page`, allowed or denied as the
    /// target says. `None` when the target names no question: the act is the
    /// app's own.
    fn answer_question(
        &self,
        workspace_id: WorkspaceId,
        page: u64,
        target: &str,
    ) -> Option<Pending<Result<Vec<PagePatch>, PageRefusal>>> {
        let _ = (workspace_id, page, target);
        None
    }
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

/// The host's own delivery of a frame an app action server hands it
/// ([`HostedBrowsers`](crate::hosted_browser) and its likes): every frame the
/// host routes reaches the device it names stamped with the caller who asked
/// for it, and this is how an app action server has one carried after it answered its
/// caller itself.
#[derive(Clone)]
pub struct Hand {
    workspaces: crate::peer::WorkspaceRegistry,
    runtime: tokio::runtime::Handle,
}

impl Hand {
    /// A hand over `workspaces`, delivering on `runtime`.
    #[must_use]
    pub fn new(
        workspaces: crate::peer::WorkspaceRegistry,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            workspaces,
            runtime,
        }
    }

    /// Hand `message` to the device `to` of `workspace_id`, as the caller
    /// `from` (device and user key) asked it: the envelope the host routes,
    /// stamped so the device runs the action as theirs. A device that is gone
    /// takes nothing, which is logged: what waits on it is answered by its
    /// own timeout.
    pub fn hand(
        &self,
        workspace_id: WorkspaceId,
        from: ([u8; 32], [u8; 32]),
        to: [u8; 32],
        message: tilo_actions::remote::RemoteActionMessage,
    ) {
        let workspaces = self.workspaces.clone();
        self.runtime.spawn(async move {
            let frame = match tilo_actions::remote::RemoteActionFrame::new(message).encode() {
                Ok(frame) => frame,
                Err(error) => {
                    tracing::error!(%error, "a frame this host hands over does not encode");
                    return;
                }
            };
            let Some(workspace) = workspaces.lock().await.get(&workspace_id).cloned() else {
                tracing::warn!(
                    workspace_id = %workspace_id,
                    "a frame was handed to a workspace this host has gone"
                );
                return;
            };
            let stamped = crate::transport::Envelope::RemoteAction {
                workspace_id,
                version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
                to_device_public_key: to,
                from_device_public_key: from.0,
                from_user_public_key: from.1,
                frame,
            };
            if !workspace.lock().await.route_directed(&to, stamped) {
                tracing::warn!(
                    workspace_id = %workspace_id,
                    device = %hex::encode(to),
                    "a frame this host handed over found no device to take it"
                );
            }
        });
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
