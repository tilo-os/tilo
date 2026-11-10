//! What the service needs of the host it runs under, and what the host
//! hands it: the host's own membership of its workspaces
//! ([`WorkspaceMember`]), the standing and read policy it decides a view by
//! ([`ViewReadPolicy`]), the verified snapshot a view is discovered from
//! ([`ViewDocument`]), and the two doors back into the host
//! ([`WorkspaceRecordsLink`] for everything the service asks, and
//! [`FeedbackReviewer`] for the model a review is looked over with).

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::{
    ActionHostError, Caller, ConfirmationQuestion, ConfirmationReply, Pending,
};
use tilo_peer_state::document_frame::DocumentRoute;

use super::WorkspaceShell;
use crate::host::HostEndpoint;

/// How the service takes part in the host's workspaces: as the host's own
/// device, a member of each workspace's channels like any other. It opens only
/// what a workspace's participants admitted it to and seals everything it writes.
#[derive(Clone)]
pub struct WorkspaceMember {
    /// The host's endpoint and certificate; the member is the host device.
    pub endpoint: HostEndpoint,
    /// The host device's key, which signs the member's changes.
    pub key: SigningKey,
    /// The runtime the member's sessions run on.
    pub runtime: tokio::runtime::Handle,
}

/// Current governed read policy supplied by the authenticated host boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewReadPolicy {
    /// Governed workspace whose current policy authorized the exact source record.
    pub workspace: String,
    /// The certified caller's current standing in the workspace, including the
    /// bound of a device that is not theirs.
    pub standing: ui::permissions::Standing,
    /// The workspace's current sandbox capabilities.
    pub sandbox: Vec<collab_types::caps::CapOverride>,
    /// Host time used to evaluate capability expiry.
    pub unix_time: u64,
}

impl ViewReadPolicy {
    pub(crate) fn permissions(&self) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::new(
            ui::permissions::Actor::person(self.standing.person.clone())
                .in_workspace(self.workspace.clone()),
            self.sandbox.clone(),
            self.standing.clone(),
        )
        .at_unix_time(self.unix_time)
    }
}

/// An existing verified source snapshot for internal view discovery only.
pub struct ViewDocument {
    /// Replayed snapshot; never returned in the public metadata response.
    pub bytes: Vec<u8>,
    /// Content revision for catalogue freshness.
    pub revision: String,
}

/// What the service needs of its host. Methods block; they are called from
/// the service's own thread, never from the host's runtime.
pub trait WorkspaceRecordsLink: Send + Sync {
    /// The user root the host chains to: the person whose device this service is.
    fn user_public_key(&self) -> [u8; 32];
    /// How the service joins the host's workspaces.
    fn member(&self) -> WorkspaceMember;
    /// Whether `workspace` admits `caller`, on the device it speaks for: a
    /// person the workspace lets in, as its governance records them, and the
    /// person the host serves itself. Anyone else is a stranger to it.
    fn admits(&self, _workspace: u64, _caller: &Caller) -> bool {
        false
    }
    /// What the workspace whose channels are `workspace` is called, which a
    /// desktop the host starts there is named after, as a page names its own.
    fn workspace_name(&self, _workspace: u64) -> Option<String> {
        None
    }
    /// Current certified-device admission and exact record Read capability.
    /// Absence denies; this never inherits document invocation authority.
    fn view_read_policy(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        None
    }
    /// The certified caller's standing across `workspace` now, with no
    /// record's read checked: what its content and its new records are
    /// decided by. Absence denies.
    fn workspace_policy(&self, _workspace: u64, _caller: &Caller) -> Option<ViewReadPolicy> {
        None
    }
    /// Read an exact existing source using already-owned host keys. The host
    /// must check current certified Read under its workspace lock before key access
    /// and again before returning. Missing keys never cause enrollment.
    fn view_document(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        Err("existing view documents unavailable".into())
    }
    /// The content keys of `route`'s channel, when the host device itself
    /// takes part in the channels of `route`'s workspace: it is then already the
    /// channel's member, and the service, which joins as that device, can
    /// only be handed the keys the host holds. `create` opens the channel of
    /// a record the service is about to write for the first time.
    ///
    /// `None` when the host does not take part in the workspace's channels: the
    /// service waits for a participant to admit it over the channel, like
    /// any member.
    /// `Some(Err)` when the host refused the hand-off or holds no such
    /// channel. Called only on behalf of a caller the service serves.
    fn host_keys(
        &self,
        _route: &DocumentRoute,
        _create: bool,
    ) -> Option<Result<channel_crypto::ChannelContentKeys, String>> {
        None
    }
    /// The listener-owned public locator; absence cannot mint a browser link.
    fn view_locator(
        &self,
        _workspace: &str,
        _provider: &str,
    ) -> Option<tilo_actions::remote::ResourceViewLocator> {
        None
    }
    /// Whether the bot installed as `installation` in `workspace` may show
    /// other names on what it writes. Absence refuses.
    fn bot_personas(&self, _workspace: u64, _installation: &str) -> bool {
        false
    }
    /// Where the workspaces' Terminal sessions run their shells on this
    /// host ([`WorkspaceShell`]). Absence leaves a session's `run` to the
    /// device running the session.
    fn shell(&self) -> Option<Arc<dyn WorkspaceShell>> {
        None
    }
    /// The bytes `workspace`'s content store holds under `hash`, for an
    /// avatar an appearance names. Absence refuses the appearance.
    fn content_blob(&self, _workspace: u64, _hash: &str) -> Option<Vec<u8>> {
        None
    }
    /// `workspace`'s content store on this host: where a caller's uploads
    /// go, what the record workers it runs actions in are lent, and what
    /// the host answers the workspace's members from. Absence leaves the
    /// workers without one and refuses uploads.
    fn workspace_content(&self, _workspace: u64) -> Option<tilo_content::ContentStore> {
        None
    }
    /// The model the host looks over its reporters' reviews with, when it
    /// lends one ([`FeedbackReviewer`]). Absence leaves reviews to the
    /// reporters' own devices.
    fn feedback_reviewer(&self) -> Option<Arc<dyn FeedbackReviewer>> {
        None
    }
    /// Put `question` about one invocation the host runs in `workspace` to
    /// the person `caller` is or acts for, on their own devices, and answer
    /// what they said. `None` when this host has nobody to ask, which leaves
    /// the invocation refused: nobody else confirms for them.
    fn ask_person(
        &self,
        _workspace: u64,
        _caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionHostError>>> {
        None
    }
}

/// A model the host lends its admitted reporters to look over a report's
/// review: it is asked only the feedback review contract
/// (`ui::feedback::model_review`) over the report's model view, which shows
/// personal values only as placeholders, and only quotes from the texts
/// asked about go back.
pub trait FeedbackReviewer: Send + Sync {
    /// What a review calls the model: the provider and model.
    fn name(&self) -> String;
    /// Ask the model `prompt` for `caller`, its answer held to the JSON
    /// `schema` where the provider can, then call `done` with its
    /// answer, from any thread. A host may refuse a caller who asks too
    /// often.
    fn review(
        &self,
        caller: &Caller,
        prompt: String,
        schema: serde_json::Value,
        done: Box<dyn FnOnce(Result<String, String>) + Send>,
    );
}
