//! Workspace administration from a device: the names of the actions a server
//! serves for it, and the requests they carry.
//!
//! A server serves what it governs as actions addressed to its own device:
//!
//! - a workspace's own governance (provider `tilo.workspace`, named in
//!   `tilo-actions`), over `Envelope::RemoteAction` on the workspace;
//! - the host's own business, which no workspace decides (creating, listing and
//!   entering workspaces, letting an app author a workspace), as provider
//!   [`HOST_ACTIONS_PROVIDER`], to the devices of the server's own person
//!   only, over `Envelope::HostAction` on their session with it, naming no
//!   workspace ([`ask_host`]), or on a workspace they hold there.
//!
//! What a member does for themselves (their effect receipts, their shares,
//! the empty record of an app they open first) their own device writes into
//! the workspace; no server is asked for it.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field and variant order are the wire, so append only.

use collab_types::WorkspaceId;

use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage,
    RemoteActionTarget,
};
use tilo_workspace_join::{Member, MemberId, Role};

use crate::client::ServerClient;
use crate::transport::Envelope;

/// Ask `client`'s host its own operation `invoke`, directly over the
/// session (a device of the host's person asks; no workspace is named), and
/// answer its receipt.
///
/// # Errors
///
/// The session closed before the host answered, or the invocation could not
/// be encoded.
pub async fn ask_host(
    client: &ServerClient,
    invoke: InvokeRequest,
) -> Result<ReceiptStatus, String> {
    let invocation = invoke.invocation;
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(invoke))
        .encode()
        .map_err(|error| error.to_string())?;
    client
        .send(Envelope::HostAction { frame })
        .map_err(|_| "the host's session closed".to_string())?;
    loop {
        match client.recv().await {
            Some(Envelope::HostAction { frame }) => {
                let message = RemoteActionFrame::decode(&frame)
                    .map_err(|error| error.to_string())?
                    .message;
                if let RemoteActionMessage::Receipt(receipt) = message
                    && receipt.invocation == invocation
                {
                    return Ok(receipt.status);
                }
            }
            Some(_) => {}
            None => return Err("the host's session closed".into()),
        }
    }
}

/// Ask `client`'s server to enter this device into `workspace`, and answer the
/// invitation text the device redeems to hold it: [`ENTER_WORKSPACE`]'s ask, and
/// the invitation it answers with, in the shape every join redeems.
///
/// # Errors
///
/// The host did not answer, refused, or answered no invitation.
pub async fn enter_invitation(
    client: &ServerClient,
    workspace: &str,
    expires_in_seconds: u64,
) -> Result<String, String> {
    let arguments = serde_json::Map::from_iter([
        ("workspace".to_owned(), workspace.into()),
        ("expires_in_seconds".to_owned(), expires_in_seconds.into()),
    ]);
    let entered: EnteredWorkspace =
        match ask_host(client, host_invoke(ENTER_WORKSPACE, arguments)).await? {
            ReceiptStatus::Completed { output } => {
                serde_json::from_value(output.value.unwrap_or_default())
                    .map_err(|error| format!("the server answered no invitation: {error}"))?
            }
            ReceiptStatus::Rejected { error } => {
                return Err(format!("the server refused to enter it: {error}"));
            }
            ReceiptStatus::Failed { error } => return Err(error.message),
            ReceiptStatus::Accepted | ReceiptStatus::Cancelled { .. } => {
                return Err("the server did not answer the entry".into());
            }
        };
    let signed = entered.invitation.decode()?.signed_invitation;
    Ok(tilo_workspace_join::encode_invitation_text(&signed))
}

/// The host operation `action` with `arguments`, under a new invocation.
fn host_invoke(
    action: &str,
    arguments: serde_json::Map<String, serde_json::Value>,
) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), action),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The provider a server answers its host operations under: creating,
/// listing and entering the workspaces it governs. Only the devices of the
/// server's own person are offered them, and invoking one is decided as
/// every action API decides a host operation.
pub const HOST_ACTIONS_PROVIDER: &str = tilo_actions::policy::agent_answers::ACCOUNT_APP;
/// The one resource the host operations are published on.
pub const HOST_WORKSPACES_RESOURCE: &str = "host.workspaces";
/// Create a governed workspace. Arguments: `name`, and the terms of its first
/// invitation when one is wanted (`invite_role`, `invite_recipient`,
/// `invitee_name`, `invite_expires_in_seconds`). Answers a [`CreatedWorkspace`].
pub const CREATE_WORKSPACE: &str = "workspace.create";
/// Make the one workspace of the caller's own person on a host that keeps
/// workspaces of people's own, or answer theirs again: any person's device
/// may ask. Arguments: `name` (the new workspace's; unused once they have
/// one), `language` (the language one they do not name is named in, a tag such
/// as `de-DE`; English when omitted), `workspace` (the identity of a workspace
/// that moves here as theirs; a different one than theirs is refused with a
/// `tilo_actions::remote::server_account::Refusal`), and `expires_in_seconds`
/// (how long the invitation stays redeemable; 600 when omitted). Answers a
/// [`CreatedWorkspace`] with the invitation the person's devices redeem to
/// hold it, as its administrator.
pub const CREATE_OWN_WORKSPACE: &str = "workspace.create-own";
/// Branch `workspace`, one of the caller's person's own on a host that keeps
/// workspaces of people's own: a new workspace of theirs, named `name` and
/// labelled `labels` when given, that starts with every record of `workspace`
/// with its history and the content those records name, and goes on apart
/// from it. What is tied to other people stays behind
/// (`tilo_desktop_model::branch::stays`), and the person is its only member.
/// `branch` is its identity (a UUID; a new one when omitted): asked again
/// under the same one, the host answers the branch it made and copies
/// nothing twice. All or nothing: a branch that could not take everything is
/// deleted again and the refusal names what did not come
/// ([`Refusal::BranchNotMade`]). Only a person's
/// device, or an agent's grant reaching all their workspaces, branches; a
/// workspace that is not theirs is refused with [`Refusal::NotYourOwn`]. Answers a
/// [`CreatedWorkspace`] with the invitation the person's devices redeem to
/// hold the branch (redeemable `expires_in_seconds`, 600 when omitted).
pub const BRANCH_OWN_WORKSPACE: &str = "workspace.branch-own";
/// Every workspace the host governs, each as a [`WorkspaceListing`].
pub const LIST_WORKSPACES: &str = "workspace.list";
/// Delete the workspace `workspace`, one of the caller's person's own (any
/// workspace this host governs, for the host's own person), with everything
/// the host keeps of it: its records, history, files and the storage they
/// took. Everyone in it is told it was deleted
/// ([`tilo_server_protocol::refusal::WORKSPACE_DELETED`]) and let in no more.
/// Final: there is no undo. Only a person's device, or an agent's grant
/// reaching all their workspaces, deletes; a workspace that is not theirs is
/// refused with [`Refusal::NotYourOwn`]. Answers `{"deleted": workspace}`.
pub const DELETE_WORKSPACE: &str = "workspace.delete";
/// The workspace's governance verb (on its [`tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER`]
/// resource) that turns running commands unattended on or off: `{"on": bool}`.
/// On, a key of the workspace's owner that reaches this workspace alone runs
/// commands in its machine without the person being asked each time; a key
/// reaching all their workspaces is still asked. Off by default. Turning it on
/// asks the person on their approving devices, whoever calls it, and waits for
/// their answer; turning it off asks nothing. It is the owner's alone: in a
/// workspace that is not their own it is refused with [`Refusal::NotYourOwn`].
/// Answers `{"workspace": uuid, "unattended": bool}`.
pub const RUN_UNATTENDED: &str = "commands.unattended";
/// Why the host refused one of its own workspace operations (asking for,
/// branching or deleting a workspace, or turning a workspace's commands
/// unattended on): what the refusal's words carry as JSON
/// ([`Refusal::to_reason`]), so a caller decides on the case without reading
/// them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum Refusal {
    /// The workspace is not one of the caller's own.
    NotYourOwn,
    /// A branch ([`BRANCH_OWN_WORKSPACE`]) could not take everything of its
    /// source: none was made, and `part` names what did not come (`the files
    /// its records name`, `the record plan of so.tilo.notes`).
    BranchNotMade { part: String },
}

impl Refusal {
    /// The refusal as an action's refusal reason
    /// ([`tilo_actions::remote::ActionCallError::Refused`]): its JSON, which
    /// [`Self::from_reason`] reads back typed and which a caller that does
    /// not read it still shows as plain data.
    #[must_use]
    pub fn to_reason(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.to_string())
    }

    /// The refusal an action's refusal reason carries, when it carries one
    /// ([`Self::to_reason`]).
    #[must_use]
    pub fn from_reason(reason: &str) -> Option<Self> {
        serde_json::from_str(reason).ok()
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotYourOwn => f.write_str("that workspace is not one of your own"),
            Self::BranchNotMade { part } => {
                write!(f, "the branch was not made: {part} did not come")
            }
        }
    }
}
/// Admit the calling device to `workspace` as one of its owner's. Answers
/// an [`EnteredWorkspace`]: the invitation the device redeems to hold the workspace,
/// restricted to its person and redeemable `expires_in_seconds` (600 when
/// omitted).
pub const ENTER_WORKSPACE: &str = "workspace.enter";
/// The revocations of the host's person the host enforces, as the person's
/// devices announced them: a JSON array of root-signed
/// `tilo_user_identity::RevocationEntry`. What a service that signs the
/// person in checks a device against, from the same list the host's own
/// handshakes read.
pub const LIST_REVOCATIONS: &str = "identity.revocations";

/// Hold a workspace another host holds, as that holder asks: arguments
/// `workspace`, `workspace_id`, `holder_hint` (the handing holder's
/// signed address) and `bootstrap` (the workspace as a joining member gets it,
/// the host's device admitted), each signed value postcard in hex. Only a
/// certified host serves workspaces this way. Answers a [`HostedWorkspace`].
pub const HOST_WORKSPACE: &str = "workspace.host";
/// Let `workspace` go, as its owners ask. Answers `{"stopped": bool}`.
pub const UNHOST_WORKSPACE: &str = "workspace.unhost";
/// Take in what the store of the asking device, one the workspace names among
/// those keeping it, keeps of `workspace` and this host's store lacks, over the
/// workspace's device link: what a device that hands a workspace over, or lets
/// it go, waits for before it does. Arguments `workspace`, `names_content`,
/// and the content the asking device keeps none of itself: listed in
/// `absent`, or, with `absent_parts`, as it handed the list before with
/// [`CATCH_UP_ABSENT`]. Answers a [`CaughtUpWorkspace`]: with `cursor_pages`,
/// its cursors in pages, the first here and the rest from
/// [`CATCH_UP_STANDING`].
pub const CATCH_UP_WORKSPACE: &str = "workspace.catch-up";
/// Hand part of the list of content the asking device, one keeping
/// `workspace`, keeps none of itself (`absent`), which its next
/// [`CATCH_UP_WORKSPACE`] with `absent_parts` counts as the whole list. With
/// `first` the part starts the list afresh, and an empty first part lets the
/// list go. A list too long for one ask's frame goes in parts, each well
/// within it. Answers `{"kept": count}`, how many addresses the list holds.
pub const CATCH_UP_ABSENT: &str = "workspace.catch-up.absent";
/// Where the answering host's store of `workspace` stands on the lineages
/// after `after` (a lineage, postcard in hex), as the device keeping it that
/// asked [`CATCH_UP_WORKSPACE`] reads the rest of its answer's cursors: a
/// store keeps a lineage per record and author, more than one frame carries.
/// Answers a [`StandingPage`].
pub const CATCH_UP_STANDING: &str = "workspace.catch-up.standing";
/// The person's whole workspace list, as one of their own devices keeps it
/// here: argument `list`, a [`PersonalList`] naming each workspace of theirs,
/// wherever it is hosted (one of their desktops, a paired device, a server),
/// with that host's signed address. Only a device of the person's own keeps
/// it, never an agent's grant or a stand-in; each keep replaces the last.
/// The devices the list carries are added to the person's devices kept
/// here, never taken away: a device leaves only by its root's revocation.
/// Answers `{"kept": count}`.
pub const KEEP_PERSONAL: &str = "workspace.personal.keep";
/// The action that names one of the person's devices ([`PersonalDevice::name`]):
/// what their lists show for it from then on, instead of the name it enrolled
/// with (`{device, name}`, the device by its key in hex).
pub const PERSONAL_DEVICE_NAME: &str = "workspace.personal.name";
/// The caller's person's workspace list ([`PersonalList`]): the one their
/// devices kept here, with the workspaces this host keeps for them, each
/// naming its host and how to reach it. Any certified key of the person's
/// reads it; an agent's grant reads only what its scope reaches. A page
/// asking it may name its browser's tab devices (argument `browser`, a
/// [`PersonalBrowser`]): they join the person's devices, and the list marks
/// them as that browser's. Asked for one workspace (argument `workspace`),
/// the host names its own signed address for it
/// ([`PersonalWorkspace::enter_at`]): where a device of the person's that
/// does not hold it yet asks to be taken in. A device that follows the list
/// names the version it holds (argument [`PERSONAL_AFTER`], as
/// [`PersonalList::version`] says it): over the device action service the
/// answer then waits until the list moves past it, or a while went by.
pub const LIST_PERSONAL: &str = "workspace.personal";
/// The argument of [`LIST_PERSONAL`] naming the version of the list the asking
/// device holds.
pub const PERSONAL_AFTER: &str = "after";
/// The most workspaces a kept list names.
pub const MAX_PERSONAL_WORKSPACES: usize = 4096;
/// The most devices a person's record keeps.
pub const MAX_PERSONAL_DEVICES: usize = 1024;

/// A person's workspaces, each with where it is hosted: what
/// [`LIST_PERSONAL`] answers and [`KEEP_PERSONAL`] keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalList {
    pub workspaces: Vec<PersonalWorkspace>,
    /// The person's devices and the other keys their root certified (an
    /// agent's grant, a script's key): each as its certificate says, one per
    /// key. A device that signs a certificate adds it here (a desktop holding
    /// the root with its list, the host that carried a pairing the page's
    /// root answered), so every device of theirs reads one list. What a
    /// host answers marks the ones a revocation it keeps refuses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<PersonalDevice>,
}

/// One key of the person's, as their record keeps it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalDevice {
    /// Its certificate, signed by the person's root.
    pub cert: tilo_user_identity::DeviceCert,
    /// The name the person gave it ([`PERSONAL_DEVICE_NAME`]), where they
    /// named it: what their lists show instead of the name the certificate
    /// carries, so two computers enrolled under one name read apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// A revocation the answering host keeps refuses it. What a device keeps
    /// says nothing here: the host answers it from its own revocations.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub revoked: bool,
    /// The browser profile the key is a tab of (its id, 32 lowercase hex
    /// characters), when a page of that browser said so
    /// ([`PersonalBrowser`]). Each tab of a browser is its own device, since
    /// one key has one writer, so a browser shows as all of its tabs' keys
    /// together: one entry, taken back as one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
    /// The agent this key runs, as the sessions it opened on this host named
    /// it ("Claude Code"): a computer set up with `tilo login` is a device of
    /// the person's that an agent works on, and says so when it starts one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// When a session of this key with this host last opened or ended, ms
    /// since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_ms: Option<i64>,
    /// A session of this key is open with this host now.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub online: bool,
}

/// The most tab keys one browser profile names ([`PersonalBrowser`]).
pub const MAX_BROWSER_DEVICES: usize = 64;

/// One browser profile's tab devices, as a page of it names them when it
/// asks for the person's record ([`LIST_PERSONAL`]'s `browser` argument):
/// the profile's random id and the certificate of each tab's key, newest
/// last. Every tab of the profile shares what it names, so whichever tab
/// asks names them all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalBrowser {
    /// 32 lowercase hex characters.
    pub id: String,
    /// At most [`MAX_BROWSER_DEVICES`].
    pub certs: Vec<tilo_user_identity::DeviceCert>,
}

impl PersonalBrowser {
    /// Whether `id` is a browser profile's id: 32 lowercase hex characters.
    #[must_use]
    pub fn is_id(id: &str) -> bool {
        id.len() == 32
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }
}

/// One workspace of a person's list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonalWorkspace {
    /// The workspace's UUID.
    pub workspace: String,
    /// Its name.
    pub name: String,
    /// Where it is hosted, when the list says: `None` for a workspace no
    /// device of the person said the host of yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<KeepingDeviceAddress>,
    /// The part the person takes in it, when the host that answers knows:
    /// `owner` for one of their own, else the role they hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What its owner labelled it with ([`WorkspaceLabels`]), when the host
    /// that answers keeps it for them.
    #[serde(default, skip_serializing_if = "WorkspaceLabels::is_empty")]
    pub labels: WorkspaceLabels,
    /// When the server that answers made it, ms since the epoch: two
    /// workspaces of one name are told apart by it. `None` when the list's
    /// keeper does not say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_at_ms: Option<i64>,
    /// Whether its owner let keys made for it alone run commands in its
    /// machine without asking them each time
    /// ([`RUN_UNATTENDED`]), when the host that answers keeps it for them.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// The answering host's own signed address for it
    /// (`tilo_peer_state::join::TransportHint`, postcard in hex), when the
    /// host keeps it and the reader asked for this workspace alone: where a
    /// device of the person's that holds none of it asks to be taken in.
    /// [`Self::host`] stays as the list has it, so a hint there still names
    /// another device that keeps it, never the host that answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter_at: Option<String>,
}

impl PersonalWorkspace {
    /// Where a device of the person's that keeps none of it enters it: the answering host's own
    /// signed address ([`Self::enter_at`]), else the one the list keeps for the device hosting it.
    ///
    /// # Errors
    /// The list names neither, or what it names is no signed address.
    pub fn entered_at(&self) -> Result<tilo_workspace_join::TransportHint, String> {
        let hint = self
            .enter_at
            .as_deref()
            .or_else(|| self.host.as_ref()?.hint.as_deref())
            .ok_or("no device the list knows names where it is kept")?;
        let bytes = hex::decode(hint).map_err(|error| error.to_string())?;
        postcard::from_bytes(&bytes).map_err(|error| error.to_string())
    }
}

/// Where a workspace is hosted, as a device dials it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepingDeviceAddress {
    /// The host as people know it: a server's name (`eu1.tilo.sh`), or the
    /// label of the person's device that hosts it.
    pub name: String,
    /// The host's own signed address for the workspace
    /// (`tilo_peer_state::join::TransportHint`), postcard in hex: what a
    /// device dials, checked against the host's certificate it carries.
    /// `None` for a workspace the host that answered the list keeps itself:
    /// the reader reaches it where it asked, and a person with hundreds of
    /// them there reads their list in one answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl KeepingDeviceAddress {
    /// The address `hint` (a `TransportHint`, postcard) gives, named as its
    /// host's certificate names it; `None` for a hint that is no signed
    /// workspace address.
    #[must_use]
    pub fn of_hint(hint: &[u8]) -> Option<Self> {
        let decoded: tilo_workspace_join::TransportHint = postcard::from_bytes(hint).ok()?;
        let (host, _) =
            tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
                &decoded,
            )
            .ok()?;
        Some(Self {
            name: host.device_certificate.device_label,
            hint: Some(hex::encode(hint)),
        })
    }

    /// The signed address this names, checked against its host's certificate.
    ///
    /// # Errors
    /// It is no signed workspace address.
    pub fn address(
        &self,
    ) -> Result<tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1, String> {
        let hint = self
            .hint
            .as_deref()
            .ok_or("the host that answered keeps it: it is reached there")?;
        let bytes = hex::decode(hint).map_err(|error| error.to_string())?;
        let decoded: tilo_workspace_join::TransportHint =
            postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        tilo_delivery_protocol::workspace_join::WorkspaceDeviceHintV1::authenticate_transport_hint(
            &decoded,
        )
        .map(|(host, _)| host)
        .map_err(|error| error.to_string())
    }
}

impl PersonalList {
    /// The version of `list`, a list as [`LIST_PERSONAL`] answers it: what a device following
    /// the list names as [`PERSONAL_AFTER`].
    #[must_use]
    pub fn version(list: &serde_json::Value) -> String {
        blake3::hash(list.to_string().as_bytes())
            .to_hex()
            .to_string()
    }

    /// Whether every entry is a workspace UUID with a printable name and an
    /// address of bounded size, and the list is no longer than
    /// [`MAX_PERSONAL_WORKSPACES`]: what a host keeps.
    ///
    /// # Errors
    /// What is wrong with it, for the device that sent it.
    pub fn check(&self) -> Result<(), String> {
        if self.workspaces.len() > MAX_PERSONAL_WORKSPACES {
            return Err(format!(
                "a list names at most {MAX_PERSONAL_WORKSPACES} workspaces"
            ));
        }
        if self.devices.len() > MAX_PERSONAL_DEVICES {
            return Err(format!(
                "a list names at most {MAX_PERSONAL_DEVICES} devices"
            ));
        }
        for entry in &self.workspaces {
            uuid::Uuid::parse_str(&entry.workspace)
                .map_err(|_| format!("{:?} names no workspace", entry.workspace))?;
            if entry.name.chars().count() > 200 || entry.name.chars().any(char::is_control) {
                return Err(format!(
                    "the name of {} is not a workspace's name",
                    entry.workspace
                ));
            }
            if let Some(host) = &entry.host
                && (host.name.chars().count() > 200
                    || host
                        .hint
                        .as_deref()
                        .is_some_and(|hint| hint.len() > 16 * 1024 || hex::decode(hint).is_err()))
            {
                return Err(format!("the host of {} is not an address", entry.workspace));
            }
        }
        Ok(())
    }
}

/// What [`HOST_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedWorkspace {
    /// The serving host's own signed address for the workspace, postcard in hex.
    pub host_hint: String,
}

/// What [`CATCH_UP_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaughtUpWorkspace {
    /// Where the answering host's store of the workspace stands once it kept
    /// what it took in: a cursor per author's line, postcard in hex.
    pub cursors: String,
    /// How many blobs of the content the workspace's files name the answering
    /// host still lacks, kept apart from the writes that name them.
    #[serde(default)]
    pub content_missing: u32,
    /// Whether the answering host took in content while it answered. What it
    /// lacks is counted over the resources that open there, which grow while
    /// it takes in the workspace, so a count above the last answer's can still
    /// follow content it fetched.
    #[serde(default)]
    pub fetched: bool,
    /// Whether the store stands on more lineages than `cursors` names: the
    /// rest are read with [`CATCH_UP_STANDING`], after the last one named.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more_cursors: bool,
}

/// What [`CATCH_UP_STANDING`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingPage {
    /// Where the answering host's store stands on the lineages after the one
    /// asked for, in lineage order: a cursor per lineage, postcard in hex.
    pub cursors: String,
    /// Whether it stands on more lineages after the last one named.
    pub more: bool,
}

/// One workspace a server governs, as its host actions answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListing {
    /// The workspace's UUID.
    pub workspace: String,
    /// The server workspace carrying the workspace.
    pub workspace_id: WorkspaceId,
    /// The workspace's name.
    pub name: String,
    /// The workspace's committed governance generation.
    pub generation: u64,
    /// The part the asking device's person takes in it, when the answer is a
    /// person's listing: `owner` for a workspace of their own, else the role
    /// they hold (`viewer`, `commenter`, `editor`, `moderator`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What its owner labelled it with, answered to the owner alone: a
    /// member invited in reads none.
    #[serde(default, skip_serializing_if = "WorkspaceLabels::is_empty")]
    pub labels: WorkspaceLabels,
    /// Whether its owner let keys made for it alone run commands in its
    /// machine without asking them each time ([`RUN_UNATTENDED`]), answered
    /// to the owner alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// When the workspace was made, ms since the epoch: the time of the first
    /// change its governance holds, which a later change does not move. Two
    /// workspaces of one name are told apart by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_at_ms: Option<i64>,
}

/// What a person labels a workspace of their own with, for their own
/// programs to find it by: `customer_id` → `cus_8121`. A backend that keeps
/// a workspace per customer names each with its own id for that customer,
/// and lists, finds and deletes them by it. Only the workspace's owner sets
/// or reads them; they are kept by the host beside whose workspace it is,
/// not in the workspace, so nobody invited into it sees them.
pub type WorkspaceLabels = std::collections::BTreeMap<String, String>;

/// Whether a workspace labelled `labels` carries every one of `wanted`: any
/// workspace does for none.
#[must_use]
pub fn labelled(labels: &WorkspaceLabels, wanted: &WorkspaceLabels) -> bool {
    wanted
        .iter()
        .all(|(name, value)| labels.get(name) == Some(value))
}

/// The most labels one workspace keeps.
pub const MAX_WORKSPACE_LABELS: usize = 16;
/// The longest a label's name is, in bytes.
pub const MAX_LABEL_NAME_BYTES: usize = 63;
/// The longest a label's value is, in characters.
pub const MAX_LABEL_VALUE_CHARS: usize = 256;

/// `labels`, if every one is a label a workspace keeps: at most
/// [`MAX_WORKSPACE_LABELS`], each name 1 to [`MAX_LABEL_NAME_BYTES`] of
/// `a-z`, `0-9`, `_`, `-` and `.`, starting with a letter or digit, each value
/// 1 to [`MAX_LABEL_VALUE_CHARS`] characters with no control characters.
///
/// # Errors
///
/// Why one is not, said so a caller can fix it.
pub fn check_labels(labels: &WorkspaceLabels) -> Result<(), String> {
    if labels.len() > MAX_WORKSPACE_LABELS {
        return Err(format!(
            "a workspace keeps at most {MAX_WORKSPACE_LABELS} labels, not {}",
            labels.len()
        ));
    }
    for (name, value) in labels {
        let named = !name.is_empty()
            && name.len() <= MAX_LABEL_NAME_BYTES
            && name
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.".contains(&byte)
            });
        if !named {
            return Err(format!(
                "the label name `{name}` is not 1 to {MAX_LABEL_NAME_BYTES} of a-z, 0-9, `_`, `-` and `.`, starting with a letter or digit"
            ));
        }
        let count = value.chars().count();
        if count == 0 || count > MAX_LABEL_VALUE_CHARS || value.chars().any(char::is_control) {
            return Err(format!(
                "the label `{name}` has no value of 1 to {MAX_LABEL_VALUE_CHARS} printable characters"
            ));
        }
    }
    Ok(())
}

/// An invitation a server's workspace action issued, as it answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvitation {
    /// Digest of the signed invitation, 64 hex characters.
    pub invite: String,
    /// The signed invitation a device redeems, in hex.
    pub invitation: String,
}

impl ActionInvitation {
    #[must_use]
    pub fn of(issued: &IssuedInvitation) -> Self {
        Self {
            invite: hex::encode(issued.invite_id),
            invitation: hex::encode(&issued.signed_invitation),
        }
    }

    /// The invitation's id and its signed bytes.
    pub fn decode(&self) -> Result<IssuedInvitation, String> {
        let mut invite_id = [0; 32];
        hex::decode_to_slice(&self.invite, &mut invite_id)
            .map_err(|_| "the server named an invalid invitation id".to_owned())?;
        let signed_invitation = hex::decode(&self.invitation)
            .map_err(|_| "the server sent an invalid invitation".to_owned())?;
        Ok(IssuedInvitation {
            invite_id,
            signed_invitation,
        })
    }
}

/// What [`CREATE_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedWorkspace {
    #[serde(flatten)]
    pub workspace: WorkspaceListing,
    /// Its first invitation, when one was asked for.
    #[serde(default)]
    pub invitation: Option<ActionInvitation>,
}

/// What [`ENTER_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredWorkspace {
    /// Whether governance changed to admit the caller.
    pub changed: bool,
    /// The invitation the caller redeems to hold the workspace.
    pub invitation: ActionInvitation,
}

pub use tilo_workspace_join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};

/// Change a member's role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMemberRoleRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
    /// Their new role.
    pub role: Role,
}

/// Remove a member and every device of theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMemberRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
}

/// A governed workspace as its administrators see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    /// The workspace's UUID.
    pub workspace: String,
    /// The server workspace carrying the workspace.
    pub workspace_id: WorkspaceId,
    /// The workspace's name.
    pub name: String,
    pub generation: u64,
    /// Every person the workspace has — its owner and everyone its governance
    /// admitted — removed ones included.
    pub members: Vec<Member>,
    /// The devices each member holds now.
    pub devices: std::collections::BTreeMap<MemberId, Vec<[u8; 32]>>,
}

impl WorkspaceSummary {
    /// How many people the workspace has now: everyone it has not removed.
    #[must_use]
    pub fn people(&self) -> usize {
        self.members.iter().filter(|member| !member.removed).count()
    }
}

#[cfg(test)]
mod wire_tests;
