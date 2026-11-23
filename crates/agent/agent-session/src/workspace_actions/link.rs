//! What the service needs of the holder it runs under, and what the holder
//! hands it: the holder's own device in its workspaces
//! ([`HostDevice`]), the access and read policy it decides a view by
//! ([`ViewReadPolicy`]), the verified snapshot a view is discovered from
//! ([`ViewDocument`]), and the two APIs back into the host
//! ([`WorkspaceDocumentsLink`] for everything the service asks, and
//! [`FeedbackReviewer`] for the model a review is looked over with).

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use tilo_actions::remote::person_asking::{Asked, Words};
use tilo_actions::remote::{
    ActionCallError, Caller, ConfirmationQuestion, ConfirmationReply, Pending,
};
use tilo_peer_state::document_frame::DocumentRoute;

use super::WorkspaceShell;
use crate::service::ServerEndpoint;
use tilo_server_protocol::WorkspaceId;

/// The host's own device, as the service takes part in its workspaces: a
/// member of each workspace's documents like any other device. It opens only
/// what a workspace's members admitted it to and seals everything it writes.
#[derive(Clone)]
pub struct HostDevice {
    /// The host's endpoint and certificate; the member is the host device.
    pub endpoint: ServerEndpoint,
    /// The host device's key, which signs the member's changes.
    pub key: SigningKey,
    /// The runtime the member's sessions run on.
    pub runtime: tokio::runtime::Handle,
    /// The host device's own key service, when it runs one: the member's session reads and asks
    /// with it, since a device holds one place in each workspace's key group and a second
    /// participant under the same key would take that place from the first each time it asked.
    /// `None` when the member's session is the device's only participant and holds its own keys.
    pub keys: Option<tilo_session_client::Lent>,
}

/// Current governed read policy supplied by the authenticated host boundary.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ViewReadPolicy {
    /// Governed workspace whose current policy authorized the exact source record.
    pub workspace: String,
    /// The certified caller's current member access in the workspace, including the
    /// bound of a device that is not theirs.
    pub access: ui::permissions::MemberAccess,
    /// The workspace's current sandbox capabilities.
    pub sandbox: Vec<collab_types::caps::CapOverride>,
    /// Host time used to evaluate capability expiry.
    pub unix_time: u64,
    /// What the workspace's list of its agents bounds the agent asking to, when an agent asks
    /// for the person ([`WorkspaceDocumentsLink::workspace_agent`]): empty follows the default.
    #[serde(default)]
    pub agent_bound: Vec<ui::permissions::policy::Rule>,
}

impl ViewReadPolicy {
    pub(crate) fn permissions(&self) -> ui::permissions::PermissionContext {
        ui::permissions::PermissionContext::new(
            ui::permissions::Acting::person(self.access.person_id().unwrap_or_default()),
            self.sandbox.clone(),
            self.access.clone(),
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
pub trait WorkspaceDocumentsLink: Send + Sync {
    /// The user root the host chains to: the person whose device this service is.
    fn user_public_key(&self) -> [u8; 32];
    /// The host's own device, as the service joins its workspaces.
    fn own_device(&self) -> HostDevice;
    /// The documents of the workspaces this device takes part in, as its own roles read
    /// them: its workspace service on a computer, its replicated copy on the web. `None` when
    /// it holds none.
    fn documents(&self) -> Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>> {
        None
    }
    /// The same documents, written as this device itself writes them. `None` when it holds
    /// none.
    fn writes(&self) -> Option<Arc<dyn tilo_peer_state::workspace_documents::DeviceWrites>> {
        None
    }
    /// Whether `workspace` admits `caller`, on the device it speaks for: a
    /// person the workspace lets in, as its governance records them, and the
    /// person the host serves itself. Anyone else is a stranger to it.
    fn admits(&self, _workspace: WorkspaceId, _caller: &Caller) -> bool {
        false
    }
    /// Current certified-device admission and exact record Read capability.
    /// Absence denies; this never inherits document invocation authority.
    fn view_read_policy(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        None
    }
    /// The certified caller's access across `workspace` now, with no
    /// record's read checked: what its content and its new records are
    /// decided by. Absence denies.
    fn workspace_policy(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
    ) -> Option<ViewReadPolicy> {
        None
    }
    /// Current governed policy for a hosted turn bound to a certified roster peer.
    /// An exact route also requires the signed delegation to reach that app.
    fn roster_policy(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _inside: Option<(&str, &str)>,
    ) -> Option<ViewReadPolicy> {
        None
    }
    /// The revocations this device holds from its people's roots: what the key that signed an
    /// agent's invocation is checked against before the invocation runs.
    fn revocations(&self) -> tilo_user_identity::RevocationList {
        tilo_user_identity::RevocationList::new()
    }
    /// Whether `workspace` keeps `caller`'s key as one its person revoked: nothing it asks is
    /// answered until they approve another, and every refusal says so.
    fn key_revoked(&self, _workspace: WorkspaceId, _caller: &Caller) -> bool {
        false
    }
    /// What `workspace`'s member list calls each of its people and agents: the names a view this
    /// device renders for an agent shows for the authors it draws, from the same list the person's
    /// own page is told. Empty where it names none.
    fn members(&self, _workspace: &str) -> Vec<(ui::permissions::MemberId, String)> {
        Vec::new()
    }
    /// Who `caller` runs commands as in `workspace`: an agent by its own name, else the person
    /// it acts for, as the workspace's member list names them, and their address in the
    /// workspace, which the VM's programs record as the author of what they make. `None` where
    /// this device knows no address for them, and the programs record none.
    fn person(
        &self,
        _workspace: &str,
        _caller: &Caller,
    ) -> Option<tilo_actions::remote::environment_commands::Person> {
        None
    }
    /// The formats and the zone the person `caller` is, or acts for, reads dates in, where
    /// this device keeps them for its people: a view rendered for the caller shows their dates
    /// and clocks as their own windows do. `None` renders in this device's own, which on a
    /// person's own device are theirs.
    fn region_of(&self, _caller: &Caller) -> Option<crate::worker::PersonRegion> {
        None
    }
    /// The agent `caller` is, which a person's own device handed its session to, works in
    /// `workspace`: the server adds it to the workspace's list of its agents where that list
    /// does not name it yet, and answers the bound the list keeps for it, which narrows
    /// everything it does there. Empty follows the default.
    fn workspace_agent(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
    ) -> Vec<ui::permissions::policy::Rule> {
        Vec::new()
    }
    /// The `.tapp` `workspace` keeps for `app`, when its governance installs
    /// a version and this device has the package file: the model every holder
    /// of the workspace applies the app's actions from, whichever device
    /// built the app. `None` for an app the workspace installs no such
    /// version of.
    fn installed_package(&self, _workspace: WorkspaceId, _app: &str) -> Option<Vec<u8>> {
        None
    }
    /// Apply `call`, an action of an installed app, in `workspace` as
    /// `caller`: the device executes the module the app version's package
    /// carries, admits and publishes what it wrote under the invocation's
    /// actor — the person, agent or bot the caller is — and answers the
    /// invocation a peer replays. `Ok(None)` when this device applies no
    /// packaged actions; `Err` with why it refused.
    fn apply_packaged_action(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _call: PackagedCall<'_>,
    ) -> Result<Option<serde_json::Value>, String> {
        Ok(None)
    }
    /// The signed `surface.toml` of the version of `app` the workspace
    /// installs, when this device keeps its package: every resource kind the
    /// app publishes with its verbs, by which the catalogue lists its actions
    /// exactly as an app compiled in is listed. `None` for an app the
    /// workspace installs no such version of, or one this device holds no
    /// package of.
    fn packaged_surface(&self, _workspace: WorkspaceId, _app: &str) -> Option<Vec<u8>> {
        None
    }
    /// Read an exact existing source using already-owned host keys. The host
    /// must check current certified Read under its workspace lock before key access
    /// and again before returning. Missing keys never cause enrollment.
    fn view_document(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _source: &DocumentRoute,
    ) -> Result<ViewDocument, String> {
        Err("existing view documents unavailable".into())
    }
    /// The content keys of `route`'s document, when the host device itself
    /// takes part in the documents of `route`'s workspace: it is then already the
    /// document's member, and the service, which joins as that device, can
    /// only be handed the keys the host holds. `create` opens the document of
    /// a record the service is about to write for the first time.
    ///
    /// `None` when the host does not take part in the workspace's channels: the
    /// service waits for a member to admit it over the document, like
    /// any member.
    /// `Some(Err)` when the host refused the hand-off or holds no such
    /// document. Called only on behalf of a caller the service serves.
    fn serving_keys(
        &self,
        _route: &DocumentRoute,
        _create: bool,
    ) -> Option<Result<key_group::ContentKeys, String>> {
        None
    }
    /// Every record this host holds of the workspace whose documents are
    /// `channel`, by the app whose document it is and its id, with the parts
    /// of a chain left to the record that keeps them.
    ///
    /// A public workspace sends a device its records only when it opens one,
    /// so this member's replica holds a handful of them: what is listed here
    /// and absent from it is a record the host is asked for when a request
    /// names it. `None` when the host cannot say, which leaves the replica's
    /// own records the only ones the service knows.
    fn records(&self, _channel: &str) -> Option<Vec<(String, String)>> {
        None
    }
    /// The actions of `workspace` this host runs itself, not a record's (its
    /// governance: giving yourself a name, inviting someone), whose words hold
    /// `needle`, as `caller` may run them: what a page's search lists beside
    /// the records' actions. Empty where it runs none.
    fn own_actions(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _needle: &str,
    ) -> Vec<tilo_actions::remote::CatalogueRow> {
        Vec::new()
    }
    /// Run `request`, one of [`Self::own_actions`], for `caller`: what it
    /// answered, or why it refused.
    ///
    /// # Errors
    ///
    /// The host refused it, or runs no such action.
    fn run_own_action(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        Err(ActionCallError::UnknownAction)
    }
    /// A short invite link to `workspace`, made now and kept for the page's
    /// next press of the row that hands one over: a clipboard write happens
    /// inside a press, and the host's answer comes after it. The action makes
    /// the next one. `None` where the host makes none ahead.
    fn ready_invite_link(&self, _workspace: WorkspaceId, _caller: &Caller) -> Option<String> {
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
    fn bot_personas(&self, _workspace: WorkspaceId, _installation: &str) -> bool {
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
    fn content_blob(&self, _workspace: WorkspaceId, _hash: &str) -> Option<Vec<u8>> {
        None
    }
    /// `workspace`'s content store on this host: where a caller's uploads
    /// go, what the record workers it runs actions in are lent, and what
    /// the host answers the workspace's members from. Absence leaves the
    /// workers without one and refuses uploads.
    fn workspace_content(&self, _workspace: WorkspaceId) -> Option<tilo_content::ContentStore> {
        None
    }
    /// Change `workspace`'s tree where this host keeps it: `change` makes the change in the tree
    /// the tree's sync holds, as `acting`, and this answers what it answered once the
    /// workspace's records carry it ([`tilo_fs::files_sync::StoreSync::change`]). Absence refuses
    /// the change.
    fn change_tree(
        &self,
        _workspace: WorkspaceId,
        _acting: Option<tilo_actions::ActionContext>,
        _change: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        Err("this host changes no workspace's tree".to_owned())
    }
    /// What `read` answers of `workspace`'s tree where this host keeps it, as the workspace has
    /// it now ([`tilo_fs::files_sync::StoreSync::read`]). Absence refuses the read.
    fn read_tree(
        &self,
        _workspace: WorkspaceId,
        _read: &mut dyn FnMut(
            &mut tilo_fs::namespace::Namespace,
        ) -> Result<serde_json::Value, String>,
    ) -> Result<serde_json::Value, String> {
        Err("this host reads no workspace's tree".to_owned())
    }
    /// A link the host's web origin serves `link`'s files at, out of
    /// `store` (`workspace`'s), for a little while
    /// ([`tilo_actions::remote::ContentLink`]): the caller was found to read
    /// the workspace already. Absence: this host serves no such links.
    fn content_link(
        &self,
        _workspace: WorkspaceId,
        _store: tilo_content::ContentStore,
        _link: &tilo_actions::remote::ContentLinkAsk,
    ) -> Option<tilo_actions::remote::ContentLink> {
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
        _workspace: WorkspaceId,
        _caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionCallError>>> {
        None
    }
    /// Whether `caller` runs a command in the machine of `workspace` without
    /// its person being asked each time: a key reaching that workspace alone,
    /// whose person let that workspace run commands unattended. Nobody does
    /// by default: every command the person's grants leave to them is asked.
    fn runs_unattended(&self, _workspace: WorkspaceId, _caller: &Caller) -> bool {
        false
    }
    /// The lasting permission of the person `caller` acts for that covers
    /// `asked`, by its id: that person answered a question about exactly
    /// this with always and has not taken it back
    /// ([`tilo_actions::remote::person_asking`]). `None` asks first, which is
    /// what a holder that keeps none answers.
    fn stands(&self, _workspace: WorkspaceId, _caller: &Caller, _asked: &Asked) -> Option<String> {
        None
    }
    /// What the lasting permission `permission` covers ran unasked for
    /// `caller`: counted where its person reads what stands.
    fn permission_used(&self, _workspace: WorkspaceId, _caller: &Caller, _permission: &str) {}
}

/// What puts an agent's question to its person in the conversation its turn
/// is in: the runtime that works agents on this server, which the service is
/// given ([`super::WorkspaceActions::ask_in_conversations`]).
pub trait AsksInConversation: Send + Sync {
    /// Put what `caller`, an agent working inside a turn of `workspace`,
    /// wants to do to its person as a question in the conversation, where
    /// it waits until they answer it, however long that takes
    /// ([`AskedInTurn`]). Answered once the server kept what was asked: `Ok`
    /// and the question is theirs to answer, or why it could not be asked.
    /// The invocation then ends as asked, and the server does it from the
    /// person's own answer ([`super::WorkspaceActions::serve_allowed`]).
    /// `None` leaves it to the person's devices
    /// ([`WorkspaceDocumentsLink::ask_person`]).
    fn ask(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
        asked: AskedInTurn,
    ) -> Option<Pending<Result<(), String>>>;
}

/// One action of an installed app, as the action API asks a holder to apply
/// it from the app version's package.
#[derive(Debug)]
pub struct PackagedCall<'a> {
    pub(super) invocation: tilo_actions::InvocationId,
    /// The app's id, as its package and its resources name it.
    pub app: &'a str,
    /// The resource kind the action is for: the app's model serves every
    /// resource the app declares, and routes the action by this.
    pub kind: &'a str,
    /// The resource's document id: `""` for what the app keeps per workspace.
    pub document: &'a str,
    /// The `.tapp` the workspace keeps for that app version.
    pub package: &'a [u8],
    /// The action's id in the resource's vocabulary.
    pub action: &'a str,
    /// Its arguments, as the action API binds them.
    pub arguments: serde_json::Map<String, serde_json::Value>,
    /// The origin clock, milliseconds since the Unix epoch.
    pub now_ms: i64,
}

impl PackagedCall<'_> {
    /// The action service's invocation, after the worker claimed it for execution.
    #[must_use]
    pub const fn invocation(&self) -> tilo_actions::InvocationId {
        self.invocation
    }
}

/// What an agent working inside a turn wants to do, as its person is asked
/// about it in the conversation the turn is in.
#[derive(Clone, Debug)]
pub struct AskedInTurn {
    /// The invocation: its target and the arguments it would go with. It
    /// holds nothing a draft withholds.
    pub request: tilo_actions::remote::InvokeRequest,
    /// The invocation as the person reviews it.
    pub draft: tilo_actions::Draft,
    /// What is asked, by the names the system gave it: what an answer of
    /// always would let stand. `None` offers no such answer: the invocation
    /// carries its resource's other actions, and an always is for the action
    /// that asked, never for what carried it.
    pub asked: Option<Asked>,
    /// The action and the thing it is on, in the system's own words. The
    /// workspace's name is the server's to add.
    pub words: Words,
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
