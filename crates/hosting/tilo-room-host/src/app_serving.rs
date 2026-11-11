//! The seam through which a host may serve apps to remote callers.
//!
//! The relay itself runs no apps and links none: it carries
//! [`Envelope::RemoteAction`](tilo_relay_protocol::transport::Envelope::RemoteAction) frames between
//! devices of a workspace without executing them. A process that embeds the
//! relay and *does* host apps supplies an [`AppServingPlane`] to the host
//! ([`HostConfig::apps`](crate::host::HostConfig::apps)); frames addressed
//! to the host's own device are then answered by it, and a relay advertises
//! its `app_serving` service.
//! Without one, the capability stays off and such frames are refused as
//! undeliverable — all but the governance of the workspaces the relay governs,
//! which it answers itself (`crate::workspaces::GovernedPlane`).

use tilo_actions::remote::{ActionHost, Caller, Pending, RemoteActionMessage};

/// Answers remote-action requests addressed to this host.
///
/// `caller` is the principal the relay authenticated for the connection the
/// request arrived on; implementations decide authority from it alone.
pub trait AppServingPlane: Send + Sync {
    /// Answer one message for `workspace_id` — whenever the answer is ready,
    /// never blocking the connection — or `None` when it needs no reply.
    fn serve(
        &self,
        workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>>;

    /// As [`Self::serve`], with the certificate the relay's handshake
    /// verified for `caller`'s device: what the relay hands every message
    /// that arrives on a session. The host's own operations need it (they
    /// admit that certificate to a workspace); an app-serving plane has no use
    /// for it, and the default ignores it.
    fn serve_certified(
        &self,
        workspace_id: u64,
        caller: &Caller,
        certificate: &tilo_user_identity::DeviceCert,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        let _ = certificate;
        self.serve(workspace_id, caller, message)
    }

    /// Answer one of the host's own operations (`tilo.host`), asked of the
    /// host itself by `caller` on the device `certificate` names, as the
    /// relay's handshake verified it: no workspace is involved. A plane that
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

    /// Whether this plane answers `message` from `caller` at all. One that
    /// does not is refused as undeliverable, as by a host that serves no
    /// apps.
    fn answers(&self, caller: &Caller, message: &RemoteActionMessage) -> bool {
        let _ = (caller, message);
        true
    }

    /// The content stores this plane keeps its workspaces' content in, when
    /// it brings them: the host then keeps everything else it keeps for a
    /// workspace there too, so a host has one store per workspace. `None`
    /// leaves the host to open its own.
    #[cfg(not(target_arch = "wasm32"))]
    fn content(&self) -> Option<std::sync::Arc<tilo_content::WorkspaceStores>> {
        None
    }

    /// `caller`'s session became an agent's run
    /// ([`Envelope::AgentRun`](tilo_relay_protocol::transport::Envelope::AgentRun))
    /// in `workspace_id`, the workspace the session entered: a plane that
    /// speaks for the runs it executes makes the run present there now, so an
    /// agent that connected and has not acted yet is already seen, waiting.
    /// Nothing by default.
    fn run_arrived(&self, workspace_id: u64, caller: &Caller) {
        let _ = (workspace_id, caller);
    }

    /// The workspace `workspace_id` was deleted: let go of whatever this
    /// plane holds for it (its app workers, what it found of the
    /// workspace). Nothing by default.
    fn forget_workspace(&self, workspace_id: u64) {
        let _ = workspace_id;
    }

    /// Where `message`, sent to this host by device `sender`, goes instead of
    /// this plane: to the device of `workspace_id` that holds what it
    /// addresses. Some actions exist only on a window, and the window need not
    /// be on this host (a Browser whose page runs on a VM host). The host hands
    /// the frame to that device stamped with `sender`, as it hands over every
    /// frame it routes, so the action runs there as the caller's. `None` serves
    /// it here, as before.
    fn route(
        &self,
        workspace_id: u64,
        sender: [u8; 32],
        message: &RemoteActionMessage,
    ) -> Option<Routing> {
        let _ = (workspace_id, sender, message);
        None
    }

    /// What device `from` answered `message` in `workspace_id`, on its way
    /// to the caller: the host only carries it, and a plane that handed the
    /// frame over may want to know how it went.
    fn relayed(&self, workspace_id: u64, from: [u8; 32], message: &RemoteActionMessage) {
        let _ = (workspace_id, from, message);
    }

    /// Whether this plane waits on answers to frames of `workspace_id`: the
    /// host decodes what it forwards there for [`Self::relayed`], and reads
    /// the rest untouched.
    fn tracks(&self, workspace_id: u64) -> bool {
        let _ = workspace_id;
        false
    }
}

/// Where a frame is served instead of on the host it was addressed to
/// ([`AppServingPlane::route`]).
#[derive(Clone, Debug)]
pub enum Routing {
    /// The device of the workspace holding the window the frame addresses.
    Device([u8; 32]),
    /// The frame is refused, and this is why: the caller's typed answer.
    Refused(tilo_actions::remote::ActionHostError),
}

/// An [`AppServingPlane`] backed by one [`ActionHost`] for every workspace.
pub struct ActionHostPlane<H> {
    host: H,
}

impl<H> ActionHostPlane<H> {
    /// Serve every workspace from `host`.
    pub const fn new(host: H) -> Self {
        Self { host }
    }
}

/// `H` is cloned into an answer that waits for an invocation to move.
impl<H: ActionHost + Clone + Send + Sync + 'static> AppServingPlane for ActionHostPlane<H> {
    fn serve(
        &self,
        _workspace_id: u64,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        tilo_actions::remote::serve(&self.host, caller, message)
    }
}
