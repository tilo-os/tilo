//! The host's own operations as actions: creating, listing and entering the
//! workspaces this server governs, and letting an app author one of them
//! (provider [`HOST_ACTIONS_PROVIDER`]).
//!
//! No workspace decides them: they are the server's person's alone
//! ([`ServiceOperation`]), and [`Workspaces::authorize`] says so for every entry,
//! but for making a person's own workspace, which a host that keeps them
//! offers anyone ([`CREATE_OWN_WORKSPACE`]).
//! The catalogue asks the same decision, so a device of anyone else is
//! offered nothing, and invoking one is the same `UnknownAction` as invoking
//! one that does not exist. The device is the one whose certificate the
//! server's handshake verified, which entering a workspace admits.
//!
//! Creating a workspace may issue its first invitation, and entering one answers
//! the invitation the device redeems to hold it: a device shares a workspace
//! or follows its host's workspaces without asking any workspace's governance before
//! it holds the workspace.

use serde_json::Value;
use tilo_action_policy::{Answer, roles};
use tilo_actions::remote::{ActionCallError, Caller, CatalogueRow, InvokeRequest};
use tilo_actions::{ActionKind, ActionParam, ParamType};
use tilo_peer_state::join::Role;
use tilo_server_session::workspaces::BRANCH_OWN_WORKSPACE;
use tilo_server_session::workspaces::{
    ActionInvitation, CREATE_OWN_WORKSPACE, CREATE_WORKSPACE, CreatedWorkspace, DELETE_WORKSPACE,
    ENTER_WORKSPACE, EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACE,
    HOST_WORKSPACES_RESOURCE, HostedWorkspace, KEEP_PERSONAL, LIST_PERSONAL, LIST_REVOCATIONS,
    LIST_WORKSPACES, PERSONAL_DEVICE_NAME, PersonalList, Refusal, UNHOST_WORKSPACE,
    WorkspaceLabels, WorkspaceListing, check_labels, labelled,
};
use tilo_user_identity::DeviceCert;

use super::{
    Args, DEFAULT_INVITATION_HOURS, HOUR_NS, Publisher, ROLES, SECOND_NS, Verb, agents, invalid,
    refused, request_id,
};
use crate::service_authority::ServiceOperation;
use crate::workspaces::authority::WorkspaceOp;
use crate::workspaces::{IssueInvitationRequest, WorkspaceSummary, Workspaces};
use tilo_server_protocol::WorkspaceId;

mod machine;

/// The host's own operations.
pub(super) const ACCOUNT: Publisher = Publisher {
    provider: HOST_ACTIONS_PROVIDER,
    kind: HOST_WORKSPACES_RESOURCE,
};

/// What the host's workspaces resource is called.
const LABEL: &str = "Workspaces";

/// How long the invitation a device entering a workspace, or its person's
/// own, redeems stays redeemable when it does not say.
const OWN_INVITATION_SECONDS: u64 = 600;

/// The part a person takes in a workspace of their own, as a listing names
/// it: they made it and administer it.
const OWNER_ROLE: &str = "owner";

/// The host's operations in catalogue order, each with the
/// [`ServiceOperation`] it is.
pub(super) const ACCOUNT_VERBS: [(Verb, ServiceOperation); 16] = [
    (
        Verb {
            id: CREATE_WORKSPACE,
            label: "Create workspace",
            check: CREATE_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: create_params,
        },
        ServiceOperation::CreateWorkspace,
    ),
    (
        Verb {
            id: CREATE_OWN_WORKSPACE,
            label: "Create your own workspace",
            check: CREATE_OWN_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: create_own_params,
        },
        ServiceOperation::CreateOwnWorkspace,
    ),
    (
        Verb {
            id: DELETE_WORKSPACE,
            label: "Delete your workspace",
            check: DELETE_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: workspace_params,
        },
        ServiceOperation::DeleteOwnWorkspace,
    ),
    (
        Verb {
            id: BRANCH_OWN_WORKSPACE,
            label: "Branch your workspace",
            check: BRANCH_OWN_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: branch::params,
        },
        ServiceOperation::BranchOwnWorkspace,
    ),
    (
        Verb {
            id: LIST_WORKSPACES,
            label: "List workspaces",
            check: LIST_WORKSPACES,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        ServiceOperation::ListWorkspaces,
    ),
    (
        Verb {
            id: LIST_PERSONAL,
            label: "List your workspaces, wherever they are hosted",
            check: LIST_PERSONAL,
            kind: ActionKind::Read,
            params: list_personal_params,
        },
        ServiceOperation::ReadPersonalList,
    ),
    (
        Verb {
            id: KEEP_PERSONAL,
            label: "Keep your workspace list here",
            check: KEEP_PERSONAL,
            kind: roles::GOVERNANCE_KIND,
            params: keep_personal_params,
        },
        ServiceOperation::KeepPersonalList,
    ),
    (
        Verb {
            id: PERSONAL_DEVICE_NAME,
            label: "Name one of your devices",
            check: PERSONAL_DEVICE_NAME,
            kind: roles::GOVERNANCE_KIND,
            params: name_personal_params,
        },
        ServiceOperation::NamePersonalDevice,
    ),
    (
        Verb {
            id: LIST_REVOCATIONS,
            label: "List revoked devices",
            check: LIST_REVOCATIONS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        ServiceOperation::ListRevocations,
    ),
    (
        Verb {
            id: ENTER_WORKSPACE,
            label: "Enter workspace",
            check: ENTER_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: enter_params,
        },
        ServiceOperation::EnterWorkspace,
    ),
    (
        Verb {
            id: HOST_WORKSPACE,
            label: "Serve a workspace for its governor",
            check: HOST_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: host_workspace_params,
        },
        ServiceOperation::HostWorkspace,
    ),
    (
        Verb {
            id: UNHOST_WORKSPACE,
            label: "Stop serving a workspace for its governor",
            check: UNHOST_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: workspace_params,
        },
        ServiceOperation::UnhostWorkspace,
    ),
    (
        Verb {
            id: PLAN_MACHINE,
            label: "Plan a machine",
            check: PLAN_MACHINE,
            kind: roles::GOVERNANCE_KIND,
            params: plan_machine_params,
        },
        ServiceOperation::PlanMachine,
    ),
    (
        Verb {
            id: LIST_MACHINES,
            label: "List the machines you planned",
            check: LIST_MACHINES,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        ServiceOperation::ListMachines,
    ),
    (
        Verb {
            id: SHOW_MACHINE,
            label: "Show a machine you planned",
            check: SHOW_MACHINE,
            kind: ActionKind::Read,
            params: machine_params,
        },
        ServiceOperation::ShowMachine,
    ),
    (
        Verb {
            id: ADMIT_MACHINE,
            label: "Take a planned machine in",
            check: ADMIT_MACHINE,
            kind: roles::GOVERNANCE_KIND,
            params: admit_machine_params,
        },
        ServiceOperation::AdmitMachine,
    ),
];

/// Plan a machine: the plan document the person's window or command wrote,
/// under the name the person sees it by, and where the token file it names is
/// written, for the command it answers.
pub(super) const PLAN_MACHINE: &str = "machine.plan";
/// The machines the person planned and has not installed.
pub(super) const LIST_MACHINES: &str = "machine.list";
/// One of them, by its plan.
pub(super) const SHOW_MACHINE: &str = "machine.show";
/// Take a machine that enrolled with a plan's token into its entry under its
/// device key.
pub(super) const ADMIT_MACHINE: &str = "machine.admit";

fn plan_machine_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "plan",
            ParamType::Text,
            "The plan document, as the command or the window wrote it (JSON).",
        ),
        ActionParam::required(
            "label",
            ParamType::Text,
            "The name you see the machine by, 1 to 80 bytes.",
        ),
        ActionParam::required(
            "token_file",
            ParamType::Text,
            "Where the token file the medium carries is, which the answer names.",
        ),
        ActionParam::optional(
            "installer_url",
            ParamType::Text,
            "Where the installer is served, for the command the answer prints.",
        ),
    ]
}

fn machine_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "plan",
        ParamType::Text,
        "The plan's id, as listing the machines prints it.",
    )]
}

fn admit_machine_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "device",
            ParamType::Text,
            "The machine's device key, 64 lowercase hex characters.",
        ),
        ActionParam::required(
            "nonce",
            ParamType::Text,
            "The token's nonce, 32 lowercase hex characters.",
        ),
        ActionParam::optional(
            "drives",
            ParamType::Text,
            "The drives the machine reported, as JSON; none when it has not reported.",
        ),
        ActionParam::optional(
            "ask",
            ParamType::Text,
            "The choices the machine asks about, choice to reason, as JSON.",
        ),
    ]
}

fn workspace_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "workspace",
        ParamType::Text,
        "The workspace, as listing workspaces names it.",
    )]
}

/// Which of a person's workspaces a listing reads: those labelled with every
/// one of `labels`, and only `workspace` when it names one. Every one for
/// the default.
#[derive(Default)]
pub(in crate::workspaces) struct Listed {
    pub(in crate::workspaces) labels: WorkspaceLabels,
    pub(in crate::workspaces) workspace: Option<String>,
}

impl Listed {
    /// Whether the listing reads `workspace` at all, by its id.
    fn names(&self, workspace: &str) -> bool {
        self.workspace
            .as_deref()
            .is_none_or(|only| only == workspace)
    }

    /// Whether a workspace of the person's list, labelled `labels`, is one
    /// the listing names.
    pub(in crate::workspaces) fn holds(&self, workspace: &str, labels: &WorkspaceLabels) -> bool {
        self.names(workspace) && labelled(labels, &self.labels)
    }
}

fn list_personal_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "labels",
            ParamType::Object,
            "Only the workspaces of yours labelled with every one of these (names to values).",
        ),
        ActionParam::optional(
            "workspace",
            ParamType::Text,
            "Only this workspace (a UUID), when it is yours.",
        ),
    ]
}

fn keep_personal_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "list",
        ParamType::Object,
        "Your workspaces, each with where it is hosted, as your device holds them.",
    )]
}

fn name_personal_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "device",
            ParamType::Text,
            "The device's key, 64 lowercase hex characters.",
        ),
        ActionParam::required(
            "name",
            ParamType::Text,
            "The name you want it to read by, such as “Claude Code on laptop 2”.",
        ),
    ]
}

fn host_workspace_params() -> Vec<ActionParam> {
    vec![
        workspace_params().remove(0),
        ActionParam::required(
            "workspace_id",
            ParamType::Integer,
            "The workspace's transport shard.",
        ),
        ActionParam::required(
            "holder_hint",
            ParamType::Text,
            "The handing holder's signed address for the workspace, postcard in hex.",
        ),
        ActionParam::required(
            "bootstrap",
            ParamType::Text,
            "The workspace as a joining member gets it, this host's device admitted, postcard in hex.",
        ),
    ]
}

/// `value`, postcard in hex, as the host operations carry signed values.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, ActionCallError> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| refused(&error))
}

fn create_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "name",
            ParamType::Text,
            "The workspace's name, 1 to 80 printable characters.",
        ),
        ActionParam::optional(
            "invite_role",
            ParamType::Text,
            "Issue the workspace's first invitation, with this role; `administrator` gives \
             them an editor's part and the administrator grants.",
        )
        .with_choices(ROLES),
        ActionParam::optional(
            "invite_recipient",
            ParamType::Text,
            "Only this person (user root key, 64 hex characters) may redeem the first invitation; anyone holding it when omitted.",
        ),
        ActionParam::optional(
            "invitee_name",
            ParamType::Text,
            "How the first invitation names its recipient to them.",
        ),
        ActionParam::optional(
            "inviter_name",
            ParamType::Text,
            "The name the inviting person gives, recorded in the first invitation.",
        ),
        ActionParam::optional(
            "invite_expires_in_seconds",
            ParamType::Integer,
            "How long the first invitation stays redeemable: 7 days when omitted, at most 30.",
        ),
        ActionParam::optional(
            "workspace",
            ParamType::Text,
            "The workspace it is, when one moves here from the device that made it: its identity (a UUID) stays its own. A new workspace when omitted.",
        ),
    ]
}

fn create_own_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "name",
            ParamType::Text,
            "The new workspace's name, 1 to 80 printable characters. Naming one makes another; omitting it answers the first you made.",
        ),
        ActionParam::optional(
            "language",
            ParamType::Text,
            "The language a workspace you do not name is named in, as a tag such as `de-DE`; English when omitted.",
        ),
        ActionParam::optional(
            "workspace",
            ParamType::Text,
            "One of your workspaces, or one you were invited into, to enter (a UUID); with a `name`, the id a new workspace is made under, so a retried create answers the one it made. An id that names none of these is refused.",
        ),
        ActionParam::optional(
            "labels",
            ParamType::Object,
            "What you label it with, for your programs to find it by: names (a-z, 0-9, `_`, `-`, `.`) to values, at most 16, which only you read. Given for one of yours, in place of its labels.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation your devices redeem to hold the workspace stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

fn enter_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "workspace",
            ParamType::Text,
            "The workspace, as listing workspaces names it.",
        ),
        ActionParam::optional(
            "expires_in_seconds",
            ParamType::Integer,
            "How long the invitation this device redeems to hold the workspace stays redeemable: 600 seconds when omitted.",
        ),
    ]
}

/// `summary`, as the host's actions answer a workspace.
fn listing(summary: &WorkspaceSummary) -> WorkspaceListing {
    WorkspaceListing {
        workspace: summary.workspace.clone(),
        workspace_id: summary.workspace_id,
        name: summary.name.clone(),
        generation: summary.generation,
        role: None,
        labels: tilo_server_session::workspaces::WorkspaceLabels::new(),
        unattended: false,
    }
}

fn answer(value: &impl serde::Serialize) -> Result<Value, ActionCallError> {
    serde_json::to_value(value).map_err(|error| refused(&error))
}

/// When an invitation issued at `now` for `seconds` stops being redeemable.
const fn expiry(now: u64, seconds: u64) -> u64 {
    now.saturating_add(seconds.saturating_mul(SECOND_NS))
}

/// The terms of a workspace's first invitation, when its creation asks for
/// one, and whether it makes its holder an administrator.
fn first_invitation(
    args: &Args,
    request_id: [u8; 32],
    now: u64,
) -> Result<Option<(IssueInvitationRequest, bool)>, ActionCallError> {
    if args.text("invite_role")?.is_none() {
        return Ok(None);
    }
    let expires_at_ns = match args.integer("invite_expires_in_seconds")? {
        Some(seconds) => expiry(now, seconds),
        None => now.saturating_add(DEFAULT_INVITATION_HOURS.saturating_mul(HOUR_NS)),
    };
    let (role, makes_administrator) = args.invitation_role_at("invite_role")?;
    Ok(Some((
        IssueInvitationRequest {
            request_id,
            role,
            recipient_user_public_key: args.key("invite_recipient")?,
            invitee_name: args.text("invitee_name")?.map(str::to_owned),
            expires_at_ns,
            inviter_name: args.text("inviter_name")?.map(str::to_owned),
        },
        makes_administrator,
    )))
}

impl Workspaces {
    /// The host's operations `caller`, on `device`, may run, asked in the
    /// workspace `workspace_id` carries: what its person may, and of that
    /// what the grants there do not refuse their agent.
    pub(super) async fn account_rows(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        device: &DeviceCert,
    ) -> Vec<CatalogueRow> {
        let now = tilo_clock::unix_now_ns();
        let person = device.user_public_key == self.inner.host.cert.user_public_key;
        let grants = if agents::is_the_person(caller, device, workspace_id) {
            None
        } else {
            Some(self.agent_grants(workspace_id, caller).await)
        };
        let mut rows = Vec::new();
        for (verb, operation) in &ACCOUNT_VERBS {
            let offered = (person || !operation.decided_by_its_workspace())
                && self.permits(device, None, WorkspaceOp::Host(*operation), now)
                && grants.as_ref().is_none_or(|grants| {
                    grants.answer(&verb.operation(ACCOUNT), now) != Answer::Deny
                });
            if offered {
                rows.push(verb.row(ACCOUNT, HOST_WORKSPACES_RESOURCE, LABEL));
            }
        }
        rows
    }

    /// Run the host operation `request` names for `caller` on `device`, the
    /// certificate the server's handshake verified for it, asked in the
    /// workspace `workspace_id` carries: an agent's call as its grants there
    /// answer ([`agents`]).
    pub(super) async fn account_invoke(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let (verb, operation) = self.account_operation(device, request)?;
        let now = tilo_clock::unix_now_ns();
        let args = Args::of(&request.argument)?;
        let request_id = request_id(&device.user_public_key, request.invocation);
        // What needs no answer from an agent's person is checked first; they
        // are asked last, so their yes is never refused after.
        self.check_account(device, operation, &args, request_id, now)
            .await?;
        self.admit_account(workspace_id, caller, device, verb, request)
            .await?;
        match operation {
            ServiceOperation::CreateWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.create_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::CreateOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.create_own_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::ListWorkspaces => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                answer(&self.listed(device, &Listed::default()).await)
            }
            ServiceOperation::DeleteOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let workspace = args.required_text("workspace")?;
                self.delete(workspace)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "deleted": workspace }))
            }
            ServiceOperation::BranchOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.branch_own_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::ReadPersonalList
            | ServiceOperation::KeepPersonalList
            | ServiceOperation::NamePersonalDevice => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                self.personal_invoke(device, operation, &args).await
            }
            ServiceOperation::ListRevocations => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                answer(&self.revocations().iter().cloned().collect::<Vec<_>>())
            }
            ServiceOperation::EnterWorkspace => {
                let workspace = args.required_text("workspace")?;
                let seconds = args
                    .integer("expires_in_seconds")?
                    .unwrap_or(OWN_INVITATION_SECONDS);
                let changed = self
                    .enter(workspace, device)
                    .await
                    .map_err(|error| refused(&error))?;
                let own = IssueInvitationRequest {
                    request_id,
                    role: Role::Editor,
                    recipient_user_public_key: Some(device.user_public_key),
                    invitee_name: None,
                    expires_at_ns: expiry(now, seconds),
                    inviter_name: None,
                };
                let issued = self
                    .issue(workspace, device, own)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&EnteredWorkspace {
                    changed,
                    invitation: ActionInvitation::of(&issued),
                })
            }
            ServiceOperation::HostWorkspace | ServiceOperation::UnhostWorkspace => {
                self.serving_invoke(device, operation, &args, now).await
            }
            ServiceOperation::PlanMachine
            | ServiceOperation::ListMachines
            | ServiceOperation::ShowMachine
            | ServiceOperation::AdmitMachine => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                Box::pin(machine::invoke(self, operation, &args)).await
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// The operation `request` names, which `device` may carry out.
    /// Unknown and forbidden are the same answer.
    fn account_operation(
        &self,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<(&'static Verb, ServiceOperation), ActionCallError> {
        let address = &request.target.address;
        let (verb, operation) = ACCOUNT_VERBS
            .iter()
            .find(|(verb, _)| verb.id == address.action)
            .ok_or(ActionCallError::UnknownAction)?;
        if address.resource != HOST_WORKSPACES_RESOURCE
            || !self.permits(
                device,
                None,
                WorkspaceOp::Host(*operation),
                tilo_clock::unix_now_ns(),
            )
        {
            return Err(ActionCallError::UnknownAction);
        }
        Ok((verb, *operation))
    }

    /// What the operation `operation` with `args` by `device` is held to
    /// beyond who may ask for it: a workspace one deletes is theirs, the site let
    /// in whoever makes a workspace of their own or keeps a list here, and an
    /// agent's own grant invites nobody but its person into a workspace it
    /// makes.
    async fn check_account(
        &self,
        device: &DeviceCert,
        operation: ServiceOperation,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<(), ActionCallError> {
        let person = device.user_public_key;
        let entering = match operation {
            ServiceOperation::CreateWorkspace => {
                // An agent's grant governs nothing, so it invites nobody: the
                // first invitation of a workspace it makes is for its own
                // person (their runtime joins with it).
                if device.capabilities.agent.is_some()
                    && first_invitation(args, request_id, now)?
                        .is_some_and(|(terms, _)| terms.recipient_user_public_key != Some(person))
                {
                    return Err(refused(
                        &"an agent's grant invites only its own person to a workspace it makes",
                    ));
                }
                false
            }
            ServiceOperation::DeleteOwnWorkspace => {
                // The server's person deletes any workspace it governs; anyone
                // else one of their own, which their grant reaches.
                let workspace = args.required_text("workspace")?;
                let theirs =
                    person == self.inner.host.cert.user_public_key || self.owns(&person, workspace);
                if !theirs || !self.lists_to(device, workspace) {
                    return Err(refused(&Refusal::NotYourOwn.to_reason()));
                }
                false
            }
            // A new workspace of a person's own is made for a person the site
            // let in: what is theirs already they enter as they are.
            ServiceOperation::CreateOwnWorkspace => {
                let makes = device
                    .capabilities
                    .agent
                    .is_none_or(|scope| scope.makes_workspaces());
                let moving = args.text("workspace")?;
                let named = args.text("name")?.is_some();
                makes
                    && self.own().is_some_and(|own| {
                        let mine = own.of(&person);
                        let theirs = |moving: &str| mine.iter().any(|own| own == moving);
                        if named {
                            !moving.is_some_and(theirs)
                        } else {
                            moving.is_none() && mine.is_empty()
                        }
                    })
            }
            // A list is kept for a person who takes part here: one the site
            // let in, or a member of a workspace here, whom an invitation
            // brought. Anyone else holds nothing here to keep a list of.
            ServiceOperation::KeepPersonalList => !self.inner.memberships.holds_any(&person),
            _ => false,
        };
        // Anyone can make a root, so a certificate alone is nobody's way past
        // the site's entry: they hear the entry's own refusal, with its wait
        // list.
        if entering && !self.let_in(&person, now).await {
            return Err(refused(&self.not_let_in().await.to_reason()));
        }
        Ok(())
    }

    /// Let `caller` on `device` have the operation `verb` asks for carried out
    /// ([`Self::account_invoke`]): asking for one's own workspace that is there
    /// already enters it, and makes nothing, as every key of the person's does
    /// to work in it; anything else as [`Self::admit_operation`] lets it.
    async fn admit_account(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        device: &DeviceCert,
        verb: &Verb,
        request: &InvokeRequest,
    ) -> Result<(), ActionCallError> {
        let arguments = super::arguments_of(request);
        let named = arguments
            .get("workspace")
            .and_then(Value::as_str)
            .and_then(WorkspaceId::parse);
        let entering = match named {
            Some(id) if verb.id == CREATE_OWN_WORKSPACE => self.held(id).await.is_some(),
            _ => false,
        };
        if entering {
            return Ok(());
        }
        self.admit_operation(
            workspace_id,
            caller,
            device,
            &verb.operation(ACCOUNT),
            &arguments,
        )
        .await
    }

    /// Read or keep `device`'s person's workspace list, already permitted to.
    async fn personal_invoke(
        &self,
        device: &DeviceCert,
        operation: ServiceOperation,
        args: &Args,
    ) -> Result<Value, ActionCallError> {
        if operation == ServiceOperation::ReadPersonalList {
            self.note_own_device(device);
            // A page names its browser's tab keys; one it can't name as the
            // contract says still reads the list.
            if let Ok(Some(browser)) = args.object("browser") {
                self.note_browser(device, browser);
            }
            // A backend finds its customer's workspace by its labels: only
            // the workspaces labelled so are read, however many it has.
            let only = Listed {
                labels: args.object("labels")?.unwrap_or_default(),
                workspace: args.text("workspace")?.map(str::to_owned),
            };
            check_labels(&only.labels).map_err(invalid)?;
            let here = self.listed(device, &only).await;
            return answer(&self.personal(device, &here, &only));
        }
        if operation == ServiceOperation::NamePersonalDevice {
            let key = args.required_text("device")?;
            let name = args.required_text("name")?;
            self.name_device(device, key, name)
                .map_err(|error| refused(&error))?;
            return Ok(serde_json::json!({ "named": key }));
        }
        let list: PersonalList = args
            .object("list")?
            .ok_or_else(|| invalid("`list` is required"))?;
        let kept = self
            .keep_personal(device, list)
            .map_err(|error| refused(&error))?;
        self.note_own_device(device);
        Ok(serde_json::json!({ "kept": kept }))
    }

    /// Make the workspace `args` names, as `device`, already permitted to,
    /// with its first invitation when asked.
    async fn create_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionCallError> {
        let name = args.required_text("name")?;
        let workspace = args.text("workspace")?;
        let first = first_invitation(args, request_id, now)?;
        // An agent's grant governs nothing: the server issues the first
        // invitation of a workspace it makes, to its own person
        // ([`Self::check_account`]), as the workspace's governor.
        let inviter = if device.capabilities.agent.is_some() {
            &self.inner.host.cert
        } else {
            device
        };
        let summary = self
            .create_workspace(name, workspace)
            .await
            .map_err(|error| refused(&error))?;
        let invitation = match first {
            Some((terms, makes_administrator)) => Some(ActionInvitation::of(
                &self
                    .issue_as(&summary.workspace, inviter, terms, makes_administrator)
                    .await
                    .map_err(|error| refused(&error))?,
            )),
            None => None,
        };
        answer(&CreatedWorkspace {
            workspace: listing(&summary),
            invitation,
        })
    }

    /// The workspaces `device` lists on this host, each with the part their
    /// person takes in it: the host's person every one, anyone else the ones
    /// of their own and the ones they were invited into, found by their person
    /// so a host with many workspaces answers a person with a few of them in
    /// the time those few take. With `only`, only the workspaces it names,
    /// the others never read.
    /// `device`'s person's whole workspace list as their devices read it now:
    /// what the host sends every session of theirs when the list changes.
    pub(in crate::workspaces) async fn personal_of(&self, device: &DeviceCert) -> PersonalList {
        let only = Listed::default();
        let here = self.listed(device, &only).await;
        self.personal(device, &here, &only)
    }

    async fn listed(&self, device: &DeviceCert, only: &Listed) -> Vec<WorkspaceListing> {
        let labels = &only.labels;
        let person = device.user_public_key;
        if person != self.inner.host.cert.user_public_key {
            let own = self.own().map(|own| own.of(&person)).unwrap_or_default();
            let mut listed: Vec<WorkspaceListing> = Vec::new();
            for workspace in &own {
                if !only.names(workspace)
                    || !self.lists_to(device, workspace)
                    || !self
                        .own()
                        .is_some_and(|own| labelled(&own.labels_of(workspace), labels))
                {
                    continue;
                }
                if let Ok(mut one) = self.listing_of(workspace).await {
                    one.role = Some(OWNER_ROLE.to_owned());
                    if let Some(own) = self.own() {
                        one.labels = own.labels_of(workspace);
                    }
                    one.unattended = self.listed_unattended(workspace);
                    listed.push(one);
                }
            }
            let now = tilo_clock::unix_now_ns();
            // A workspace they were invited into carries none of their labels.
            let invited = if labels.is_empty() {
                self.inner.memberships.of(&person)
            } else {
                Vec::new()
            };
            for workspace in invited {
                if own.contains(&workspace)
                    || !only.names(&workspace)
                    || !self.lists_to(device, &workspace)
                {
                    continue;
                }
                let Some(role) = self.role_of(&workspace, person, now).await else {
                    continue;
                };
                if let Ok(mut one) = self.listing_of(&workspace).await {
                    one.role = Some(role.key().to_owned());
                    listed.push(one);
                }
            }
            listed.sort_by(|a, b| {
                a.name
                    .cmp(&b.name)
                    .then_with(|| a.workspace.cmp(&b.workspace))
            });
            return listed;
        }
        self.list()
            .await
            .iter()
            .filter(|summary| {
                only.names(&summary.workspace) && self.lists_to(device, &summary.workspace)
            })
            .map(|summary| WorkspaceListing {
                role: Some(OWNER_ROLE.to_owned()),
                labels: self
                    .own()
                    .map(|own| own.labels_of(&summary.workspace))
                    .unwrap_or_default(),
                unattended: self.listed_unattended(&summary.workspace),
                ..listing(summary)
            })
            .filter(|listed| labelled(&listed.labels, labels))
            .collect()
    }

    /// The workspace of `device`'s person's own it asks for, already permitted
    /// to, with an invitation into it: the first they made when the ask names
    /// none (made now if they have none), one of theirs or one they were
    /// invited into by its id, or another made for a `name`. Every other id is
    /// refused: the host never answers a workspace the ask did not name.
    async fn create_own_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
    ) -> Result<Value, ActionCallError> {
        let person = device.user_public_key;
        let name = args.text("name")?;
        let language = args.text("language")?;
        let moving = args.text("workspace")?;
        let labels: Option<WorkspaceLabels> = args.object("labels")?;
        if let Some(labels) = &labels {
            check_labels(labels).map_err(invalid)?;
        }
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        let Some(own) = self.own() else {
            return self
                .enter_host_persons(device, name, moving, request_id, expiry(now, seconds))
                .await;
        };
        // A grant for one workspace enters that one, already made: it makes no other.
        let makes = device
            .capabilities
            .agent
            .is_none_or(|scope| scope.makes_workspaces());
        // Nor does it label one: labels are its person's, for their programs.
        if !makes
            && (name.is_some()
                || labels.is_some()
                || !moving.is_some_and(|workspace| {
                    device
                        .capabilities
                        .agent
                        .is_some_and(|scope| scope.reaches(workspace))
                }))
        {
            return Err(refused(&Refusal::NotYourOwn.to_reason()));
        }
        // What could be made twice at once waits for the first: a workspace
        // the ask names (a retried create answers the one the first made),
        // and a person's own workspace asked for by no name (two of their
        // devices asking at once get one). A named new workspace has a fresh
        // identity of its own, so a backend's hundreds are made side by side.
        let making = match (moving, name) {
            (Some(workspace), _) => Some(own.making(workspace.to_owned()).await),
            (None, None) => Some(own.making(hex::encode(person)).await),
            (None, Some(_)) => None,
        };
        let mine = own.of(&person);
        let theirs = moving.filter(|moving| mine.iter().any(|own| own == moving));
        // A key the workspace keeps as revoked is told so, and invited no more.
        if device.capabilities.agent.is_some()
            && let Some(moving) = moving
        {
            self.refuse_revoked_key(moving, device)
                .await
                .map_err(|error| refused(&error))?;
        }
        let summary = if let Some(moving) = theirs {
            // One of theirs: a device of theirs entering it, or a grant's
            // parent asking for the invitation into it.
            self.summary(moving)
                .await
                .map_err(|error| refused(&error))?
        } else if let Some(joined) = moving.filter(|_| name.is_none())
            && let Some(role) = self.role_of(joined, person, now).await
        {
            // Another's, which the person was invited into: another device
            // of theirs (their key) enters it as them, at the role they hold.
            drop(making);
            let summary = self
                .summary(joined)
                .await
                .map_err(|error| refused(&error))?;
            return self
                .invite_own_person(summary, person, role, request_id, expiry(now, seconds))
                .await;
        } else if name.is_none() && moving.is_some() {
            // A workspace that is not theirs: another's, or one this host has
            // never made. The host makes a person's own workspace under an id
            // of its own choosing, so an id it does not know is never theirs,
            // and it is never read as some other workspace they have.
            return Err(refused(&Refusal::NotYourOwn.to_reason()));
        } else if name.is_none()
            && let Some(first) = mine.first()
        {
            // A plain ask for "my workspace" answers the first they made.
            self.summary(first).await.map_err(|error| refused(&error))?
        } else if !makes {
            return Err(refused(&Refusal::NotYourOwn.to_reason()));
        } else {
            // Another one, for a person the site let in ([`Self::check_account`]):
            // there is no bound on how many a person keeps, and one they did
            // not name is named in their language.
            let name = name.map_or_else(|| crate::i18n::workspace_name(language), str::to_owned);
            let summary = self
                .create_workspace(&name, moving)
                .await
                .map_err(|error| refused(&error))?;
            own.record(&person, &summary.workspace)
                .map_err(|error| refused(&error))?;
            summary
        };
        if let Some(labels) = labels {
            own.label(&summary.workspace, labels)
                .map_err(|error| refused(&error))?;
        }
        drop(making);
        // A person admitted before (another of their devices joined) is
        // made its administrator now; one joining with the invitation is
        // as they are admitted.
        self.administer_own(&summary.workspace, person, now)
            .await
            .map_err(|error| refused(&error))?;
        self.invite_own_person(
            summary,
            person,
            Role::Editor,
            request_id,
            expiry(now, seconds),
        )
        .await
    }

    /// Invite the host's own person (`device` is theirs) into the workspace
    /// `workspace` governed here, as a host that keeps no workspaces of anyone
    /// else's answers `workspace.create-own`: every workspace it governs is
    /// theirs, so another device of theirs, or a key of theirs whose scope
    /// reaches it, enters it as them. It makes none: that is `workspace.create`.
    async fn enter_host_persons(
        &self,
        device: &DeviceCert,
        name: Option<&str>,
        workspace: Option<&str>,
        request_id: [u8; 32],
        expires_at_ns: u64,
    ) -> Result<Value, ActionCallError> {
        let workspace = match (name, workspace) {
            (None, Some(workspace)) if self.lists_to(device, workspace) => workspace,
            _ => return Err(refused(&Refusal::NotYourOwn.to_reason())),
        };
        let summary = self
            .summary(workspace)
            .await
            .map_err(|error| refused(&error))?;
        self.invite_own_person(
            summary,
            device.user_public_key,
            Role::Editor,
            request_id,
            expires_at_ns,
        )
        .await
    }

    /// The role `person` holds in `workspace` now, a member it has not
    /// removed; `None` for anyone else, and for a workspace not governed
    /// here.
    async fn role_of(&self, workspace: &str, person: [u8; 32], now: u64) -> Option<Role> {
        let governed = self.workspace(workspace).await.ok()?;
        let governed = governed.lock().await;
        governed
            .peer()
            .governance()
            .governed_members(crate::workspaces::system_time(now))
            .ok()?
            .members
            .into_iter()
            .find(|member| member.root() == Some(person) && !member.removed)
            .and_then(|member| member.role)
    }

    /// Answer `summary` with an invitation for its `person` as `role`, until
    /// `expires_at_ns`: the workspace's governor, the host, invites them.
    async fn invite_own_person(
        &self,
        summary: WorkspaceSummary,
        person: [u8; 32],
        role: Role,
        request_id: [u8; 32],
        expires_at_ns: u64,
    ) -> Result<Value, ActionCallError> {
        let terms = IssueInvitationRequest {
            request_id,
            role,
            recipient_user_public_key: Some(person),
            invitee_name: None,
            expires_at_ns,
            inviter_name: None,
        };
        let issued = self
            .issue(&summary.workspace, &self.inner.host.cert, terms)
            .await
            .map_err(|error| refused(&error))?;
        let mut workspace = listing(&summary);
        if let Some(own) = self.own()
            && own.is_of(&person, &summary.workspace)
        {
            workspace.labels = own.labels_of(&summary.workspace);
            workspace.unattended = own.runs_unattended(&summary.workspace);
        }
        answer(&CreatedWorkspace {
            workspace,
            invitation: Some(ActionInvitation::of(&issued)),
        })
    }

    /// Whom this host takes the workspace `request` hands over for, as
    /// `caller`, its governor, asks: nobody when its work setting takes it
    /// anyway, else the first of its owners, other than this host's own
    /// person, this host keeps workspaces for ([`Workspaces::keeps_for_owner`]).
    ///
    /// # Errors
    ///
    /// The host keeps it for none of its owners: why, for the first of them.
    async fn kept_for(
        &self,
        caller: &DeviceCert,
        request: &super::super::HostWorkspaceRequest,
        now: u64,
    ) -> Result<Option<[u8; 32]>, String> {
        let host = &self.inner.host;
        if super::super::work_takes(&host.work, caller, &host.cert, request) {
            return Ok(None);
        }
        let mut refusal = None;
        // Its own person's workspace is the work setting's to take: for them,
        // this host is one of their devices, not a host that let them in.
        let others = super::super::owners(request)?
            .into_iter()
            .filter(|owner| *owner != host.cert.user_public_key);
        for owner in others {
            match self.keeps_for_owner(&owner, now).await {
                Ok(()) => return Ok(Some(owner)),
                Err(reason) => {
                    refusal.get_or_insert(reason);
                }
            }
        }
        Err(refusal.unwrap_or_else(|| {
            "this host keeps a workspace another host governs only for an owner of it".into()
        }))
    }

    /// The host operations that have one host of the person serve a workspace
    /// another governs ([`super::super::holding`]), already permitted to `device`.
    async fn serving_invoke(
        &self,
        device: &DeviceCert,
        operation: ServiceOperation,
        args: &Args,
        now: u64,
    ) -> Result<Value, ActionCallError> {
        match operation {
            ServiceOperation::HostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let request = super::super::HostWorkspaceRequest {
                    workspace: args.required_text("workspace")?.to_owned(),
                    workspace_id: WorkspaceId::parse(args.required_text("workspace_id")?)
                        .ok_or_else(|| invalid("`workspace_id` is a workspace id"))?,
                    holder_hint: super::super::decode_hex(args.required_text("holder_hint")?)
                        .map_err(|error| invalid(format!("`holder_hint`: {error}")))?,
                    bootstrap: super::super::decode_hex(args.required_text("bootstrap")?)
                        .map_err(|error| invalid(format!("`bootstrap`: {error}")))?,
                    role: serde_json::from_value(Value::String(
                        args.required_text("role")?.to_owned(),
                    ))
                    .map_err(|_| invalid("`role` is `key_holder` or `store`"))?,
                };
                self.kept_for(device, &request, now)
                    .await
                    .map_err(|error| refused(&error))?;
                let hint = self
                    .hold(device, &request)
                    .await
                    .map_err(|error| refused(&error))?;
                answer(&HostedWorkspace {
                    host_hint: hex_postcard(&hint)?,
                })
            }
            ServiceOperation::UnhostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| refused(&error))?;
                let stopped = self
                    .unhold(device, args.required_text("workspace")?)
                    .await
                    .map_err(|error| refused(&error))?;
                Ok(serde_json::json!({ "stopped": stopped }))
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }
}

#[cfg(test)]
mod accounts_tests;
#[cfg(test)]
mod addresses_tests;
#[cfg(test)]
mod asking_tests;
mod branch;
#[cfg(all(test, feature = "agents"))]
mod calendar_tests;
#[cfg(test)]
mod connections_tests;
#[cfg(test)]
mod kept_asks_tests;
#[cfg(test)]
mod mail_assistant_tests;
#[cfg(test)]
mod mail_turn_tests;
#[cfg(test)]
mod own_tests;
#[cfg(test)]
mod page_use_tests;
#[cfg(test)]
mod personal_tests;
#[cfg(test)]
mod telling_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod told_tests;
#[cfg(test)]
mod turn_delegation_tests;
#[cfg(test)]
mod turn_use_tests;
#[cfg(test)]
mod vault_tests;
#[cfg(test)]
mod web_call_tests;
