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
use tilo_server_protocol::workspaces::BRANCH_OWN_WORKSPACE;
use tilo_server_protocol::workspaces::{
    ActionInvitation, AdminRefusal, CATCH_UP_STANDING, CATCH_UP_WORKSPACE, CREATE_OWN_WORKSPACE,
    CREATE_WORKSPACE, CaughtUpWorkspace, CreatedWorkspace, DELETE_WORKSPACE, ENTER_WORKSPACE,
    EnteredWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACE, HOST_WORKSPACES_RESOURCE,
    HostedWorkspace, LIST_REVOCATIONS, LIST_WORKSPACES, StandingPage, UNHOST_WORKSPACE, WEB_SERVE,
    WorkspaceLabels, WorkspaceListing, administered_answer, administered_request, check_labels,
    labelled,
};
use tilo_user_identity::DeviceCert;

use super::{
    Args, DEFAULT_INVITATION_HOURS, HOUR_NS, Publisher, ROLES, SECOND_NS, Verb, agents, request_id,
};
use crate::service_authority::ServiceOperation;
use crate::workspaces::authority::WorkspaceOp;
use crate::workspaces::blocked::Blocked;
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
pub(super) const ACCOUNT_VERBS: [(Verb, ServiceOperation); 22] = [
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
            id: BLOCK,
            label: "Block a device or a person on this host",
            check: BLOCK,
            kind: roles::GOVERNANCE_KIND,
            params: block_params,
        },
        ServiceOperation::Block,
    ),
    (
        Verb {
            id: UNBLOCK,
            label: "Stop blocking a device or a person on this host",
            check: UNBLOCK,
            kind: roles::GOVERNANCE_KIND,
            params: block_params,
        },
        ServiceOperation::Unblock,
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
            id: CATCH_UP_WORKSPACE,
            label: "Take in a workspace's changes from a device keeping it",
            check: CATCH_UP_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: catch_up_params,
        },
        ServiceOperation::CatchUpWorkspace,
    ),
    (
        // The rest of what a catch-up answers.
        Verb {
            id: CATCH_UP_STANDING,
            label: "Read where a workspace's store stands after a catch-up",
            check: CATCH_UP_WORKSPACE,
            kind: roles::GOVERNANCE_KIND,
            params: catch_up_standing_params,
        },
        ServiceOperation::CatchUpWorkspace,
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
            id: LIST_SERVERS,
            label: "List your servers",
            check: LIST_SERVERS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        ServiceOperation::ListServers,
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
    (
        Verb {
            id: ADMIT_SERVER,
            label: "Take a server into your servers",
            check: ADMIT_SERVER,
            kind: roles::GOVERNANCE_KIND,
            params: admit_server_params,
        },
        ServiceOperation::AdmitServer,
    ),
    (
        Verb {
            id: KEEP_RECOVERY_KEY,
            label: "Keep this machine's disk recovery key",
            check: KEEP_RECOVERY_KEY,
            kind: ActionKind::Undoable,
            params: keep_recovery_key_params,
        },
        ServiceOperation::KeepRecoveryKey,
    ),
    (
        Verb {
            id: RECOVERY_KEYS,
            label: "Read your machines' disk recovery keys",
            check: RECOVERY_KEYS,
            kind: ActionKind::Read,
            params: Vec::new,
        },
        ServiceOperation::ListRecoveryKeys,
    ),
    (
        Verb {
            id: WEB_SERVE,
            label: "Serve this server's web UI",
            check: WEB_SERVE,
            kind: roles::GOVERNANCE_KIND,
            params: web_serve_params,
        },
        ServiceOperation::ServeWeb,
    ),
];

/// Plan a machine: the plan document the person's window or command wrote,
/// under the name the person sees it by, and where the token file it names is
/// written, for the command it answers.
pub(super) const PLAN_MACHINE: &str = "machine.plan";
/// The machines the person planned and has not installed.
pub(super) const LIST_MACHINES: &str = "machine.list";
/// The person's servers, as their workspace's document names them.
pub(super) const LIST_SERVERS: &str = "machine.servers";
/// One of them, by its plan.
pub(super) const SHOW_MACHINE: &str = "machine.show";
/// Take a machine that enrolled with a plan's token into its entry under its
/// device key.
pub(super) const ADMIT_MACHINE: &str = "machine.admit";
/// Take a server in among the person's servers, by the certificate its ticket's login answered
/// with and where its endpoint is dialled.
pub(super) const ADMIT_SERVER: &str = "server.admit";
/// Keep the disk recovery key of the machine that asks.
pub(super) const KEEP_RECOVERY_KEY: &str = tilo_actions::remote::machines::KEEP_RECOVERY_KEY;
/// The disk recovery keys of the person's machines.
pub(super) const RECOVERY_KEYS: &str = tilo_actions::remote::machines::RECOVERY_KEYS;
/// Block a device, or every device of a person, on this host.
pub(super) const BLOCK: &str = "host.block";
/// Lift a block the operator set.
pub(super) const UNBLOCK: &str = "host.unblock";

fn block_params() -> Vec<ActionParam> {
    vec![
        ActionParam::optional(
            "device",
            ParamType::Text,
            "The device's public key, 64 lowercase hex characters. Give this or a person's key.",
        ),
        ActionParam::optional(
            "user",
            ParamType::Text,
            "A person's root public key, 64 lowercase hex characters: every device of theirs. Give \
             this or a device's key.",
        ),
    ]
}

fn keep_recovery_key_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "key",
        ParamType::Text,
        "The disk recovery key this machine installed with.",
    )]
}

fn admit_server_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "cert",
            ParamType::Text,
            "The certificate the ticket's login answered with, in hex.",
        ),
        ActionParam::required(
            "endpoint",
            ParamType::Text,
            "Where the server is dialled, as its endpoint advertises it (JSON).",
        ),
    ]
}

fn web_serve_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "serving",
        ParamType::Boolean,
        "Whether this server serves its own web UI.",
    )]
}

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

fn catch_up_params() -> Vec<ActionParam> {
    let mut params = workspace_params();
    params.push(ActionParam::optional(
        "cursor_pages",
        ParamType::Boolean,
        "Whether the answer's cursors come in pages, the rest read after it.",
    ));
    params
}

fn catch_up_standing_params() -> Vec<ActionParam> {
    let mut params = workspace_params();
    params.push(ActionParam::required(
        "after",
        ParamType::Text,
        "The lineage the page starts after, postcard in hex.",
    ));
    params
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
}

fn host_workspace_params() -> Vec<ActionParam> {
    vec![
        workspace_params().remove(0),
        ActionParam::required(
            "workspace_id",
            ParamType::Integer,
            "The workspace's transport shard.",
        ),
        ActionParam::optional(
            "holder_hint",
            ParamType::Text,
            "The handing holder's signed address for the workspace, postcard in hex: with \
             `bootstrap`, the hand-over.",
        ),
        ActionParam::optional(
            "bootstrap",
            ParamType::Text,
            "The workspace as a joining member gets it, this host's device admitted, postcard in \
             hex. Without it, the host answers the address it signs for the workspace and takes \
             nothing in.",
        ),
    ]
}

/// `value`, postcard in hex, as the host operations carry signed values.
fn hex_postcard(value: &impl serde::Serialize) -> Result<String, ActionCallError> {
    postcard::to_stdvec(value)
        .map(hex::encode)
        .map_err(|error| ActionCallError::refused(error.to_string()))
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
        labels: tilo_server_protocol::workspaces::WorkspaceLabels::new(),
        unattended: false,
        made_at_ms: None,
    }
}

fn answer(value: &impl serde::Serialize) -> Result<Value, ActionCallError> {
    serde_json::to_value(value).map_err(|error| ActionCallError::refused(error.to_string()))
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

    /// Hand `device` an invitation of its own to the workspace `args` names, to
    /// redeem there: its device signs its own admission with it.
    async fn enter_invoke(
        &self,
        device: &DeviceCert,
        args: &Args,
        request_id: [u8; 32],
        now: u64,
        local_address: Option<iroh_base::EndpointAddr>,
    ) -> Result<Value, ActionCallError> {
        let workspace = args.required_text("workspace")?;
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        // The invitation is the entering device's own: its person's role, an Editor's at most,
        // and no grant to invite. The local device redeems in process; one sent to another
        // device needs to wait for the host's reachable address.
        let address = match local_address {
            Some(address) => address,
            None => self.invitation_addr().await,
        };
        let issued = self
            .own_invitation(
                workspace,
                device,
                (request_id, expiry(now, seconds)),
                &address,
            )
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        answer(&EnteredWorkspace {
            invitation: ActionInvitation::of(&issued),
        })
    }

    /// Serve this device's own web UI as `serving` asks, and keep the choice in `device`'s
    /// entry among its person's servers, which every device of theirs reads and which its
    /// next start follows. A device with no entry there serves or stops all the same: the
    /// choice is only kept where the document names it.
    async fn serve_web_invoke(
        &self,
        device: &DeviceCert,
        serving: bool,
    ) -> Result<Value, ActionCallError> {
        self.serve_web(serving).map_err(ActionCallError::refused)?;
        // Stopping aborts the listener's tasks; letting them run drops what answers on the
        // address before this answers, so a page that asks now is not served.
        tokio::task::yield_now().await;
        let device_key = device.device_public_key;
        let kept = crate::workspaces::servers::read(self)
            .await
            .is_ok_and(|servers| {
                servers
                    .servers
                    .get(&crate::workspaces::servers::device_id(&device_key))
                    .is_some()
            });
        if kept {
            let served = if serving {
                String::new()
            } else {
                crate::workspaces::servers::WEB_SERVED[1].to_owned()
            };
            crate::workspaces::servers::apply(
                self,
                crate::workspaces::servers::Servers::SET_WEB
                    .with((crate::workspaces::servers::device_id(&device_key), served)),
            )
            .await
            .map_err(ActionCallError::refused)?;
        }
        Ok(serde_json::json!({ "serving": serving }))
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
        local_address: Option<iroh_base::EndpointAddr>,
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
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                self.create_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::CreateOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                self.create_own_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::ListWorkspaces => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                answer(&self.listed(device, &Listed::default()).await)
            }
            ServiceOperation::DeleteOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                let workspace = args.required_text("workspace")?;
                self.delete(workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                Ok(serde_json::json!({ "deleted": workspace }))
            }
            ServiceOperation::BranchOwnWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                self.branch_own_invoke(device, &args, request_id, now).await
            }
            ServiceOperation::ListRevocations => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                answer(&self.revocations().iter().cloned().collect::<Vec<_>>())
            }
            ServiceOperation::EnterWorkspace => {
                self.enter_invoke(device, &args, request_id, now, local_address)
                    .await
            }
            ServiceOperation::Block | ServiceOperation::Unblock => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                self.blocking_invoke(operation, &args).await
            }
            operation if operation.decided_by_its_workspace() => {
                Box::pin(self.serving_invoke(device, (verb.id, operation), &args, now)).await
            }
            operation if operation.of_machines() => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Box::pin(machine::invoke(self, device, operation, &args)).await
            }
            ServiceOperation::AdmitServer => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                Box::pin(machine::admit_server(self, &args)).await
            }
            ServiceOperation::ServeWeb => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                let serving = args
                    .boolean("serving")?
                    .ok_or_else(|| ActionCallError::invalid_request("`serving` is required"))?;
                self.serve_web_invoke(device, serving).await
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Enter through the host action for redemption on this same endpoint.
    pub(crate) async fn enter_here(
        &self,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let address = self.endpoint_addr();
        if &device.device_public_key != address.id.as_bytes()
            || request.target.provider != HOST_ACTIONS_PROVIDER
            || request.target.address.action != ENTER_WORKSPACE
        {
            return Err(ActionCallError::UnknownAction);
        }
        let caller = crate::service::local_caller(device);
        Box::pin(self.account_invoke(None, &caller, device, request, Some(address))).await
    }

    /// Answer the page's administration request the host action carries
    /// ([`tilo_server_protocol::workspaces::ADMINISTER_WORKSPACES`]) as
    /// `device`, the caller's own device: each request inside it is decided as
    /// the workspace it names (or the host, where it names none) decides it,
    /// exactly as the same request on a page's own session is.
    ///
    /// # Errors
    /// The arguments name no request, or the reply does not encode.
    pub(super) async fn administer_invoke(
        &self,
        caller: &Caller,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let args = Args::of(&request.argument)?;
        let asked = administered_request(&args.0)
            .map_err(|error| ActionCallError::InvalidRequest { reason: error })?;
        let reply = self.answer_administration(device, caller, asked).await;
        administered_answer(&reply).map_err(|reason| ActionCallError::Refused { reason })
    }

    /// The operation `request` names, which `device` may carry out. An
    /// operation the host does not publish is unknown; one it publishes, which
    /// `device` may not carry out, is refused with the reason why: the
    /// operations are the same on every host, so saying so tells nobody more.
    fn account_operation(
        &self,
        device: &DeviceCert,
        request: &InvokeRequest,
    ) -> Result<(&'static Verb, ServiceOperation), ActionCallError> {
        let address = &request.target.address;
        let (verb, operation) = ACCOUNT_VERBS
            .iter()
            .find(|(verb, _)| verb.id == address.action)
            .filter(|_| address.resource == HOST_WORKSPACES_RESOURCE)
            .ok_or(ActionCallError::UnknownAction)?;
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(*operation),
            tilo_clock::unix_now_ns(),
        )
        .map_err(|error| ActionCallError::refused(error.to_string()))?;
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
                    return Err(ActionCallError::refused(
                        "an agent's grant invites only its own person to a workspace it makes",
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
                    return Err(ActionCallError::refused(
                        AdminRefusal::NotYourOwn.to_reason(),
                    ));
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
                        if named {
                            !moving.is_some_and(|moving| own.is_of(&person, moving))
                        } else {
                            moving.is_none() && !own.any_of(&person)
                        }
                    })
            }
            _ => false,
        };
        // Anyone can make a root, so a certificate alone is nobody's way past
        // the site's entry: they hear the entry's own refusal, with its wait
        // list.
        if entering && !self.let_in(&person, now).await {
            return Err(ActionCallError::refused(
                self.not_let_in().await.to_reason(),
            ));
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
        // A disk's recovery key opens it with nothing else: only the person, on a device of
        // their own, keeps or reads one. No agent is asked on their behalf, and a stand-in is
        // no device of theirs.
        if matches!(verb.id, KEEP_RECOVERY_KEY | RECOVERY_KEYS) {
            return if caller.acting().is_person()
                && !device.capabilities.stand_in
                && caller.device_public_key() == Some(&device.device_public_key)
            {
                Ok(())
            } else {
                Err(ActionCallError::UnknownAction)
            };
        }
        // A block is the operator's own: only their person, on a device of
        // their own keeps or lifts one. No agent is asked on their behalf, and
        // a stand-in is no device of theirs.
        if matches!(verb.id, BLOCK | UNBLOCK) {
            return if caller.acting().is_person()
                && !device.capabilities.stand_in
                && caller.device_public_key() == Some(&device.device_public_key)
            {
                Ok(())
            } else {
                Err(ActionCallError::UnknownAction)
            };
        }
        // A machine's own record, written when the person certified the machine with their root:
        // their device asks it as itself (the session's agent is that device's run), so no agent
        // is asked on their behalf.
        if matches!(verb.id, SHOW_MACHINE | ADMIT_MACHINE | ADMIT_SERVER)
            && !device.capabilities.stand_in
            && caller.user_public_key() == Some(&device.user_public_key)
            && caller.device_public_key() == Some(&device.device_public_key)
        {
            return Ok(());
        }
        self.admit_operation(
            workspace_id,
            caller,
            device,
            &verb.operation(ACCOUNT),
            (&arguments, request.approval().is_some()),
        )
        .await
    }

    /// Block or unblock the key `args` names, as this host's operator: the
    /// operator's own record of it, which every host they run refuses
    /// ([`crate::workspaces::blocked`]). One key is named: a device's, or a
    /// person's root key, whose every device is refused.
    async fn blocking_invoke(
        &self,
        operation: ServiceOperation,
        args: &Args,
    ) -> Result<Value, ActionCallError> {
        let (device, key) = match (args.text("device")?, args.text("user")?) {
            (Some(device), None) => (true, device),
            (None, Some(user)) => (false, user),
            _ => {
                return Err(ActionCallError::invalid_request(
                    "name one key to block or unblock: a device or a person's user key",
                ));
            }
        };
        let key = crate::workspaces::blocked::key_bytes(key).ok_or_else(|| {
            ActionCallError::invalid_request("a key is 32 bytes in lowercase hex")
        })?;
        let key = hex::encode(key);
        let action = match (operation, device) {
            (ServiceOperation::Block, true) => Blocked::BLOCK_DEVICE.with(key.clone()),
            (ServiceOperation::Block, false) => Blocked::BLOCK_USER.with(key.clone()),
            (ServiceOperation::Unblock, true) => Blocked::UNBLOCK_DEVICE.with(key.clone()),
            (ServiceOperation::Unblock, false) => Blocked::UNBLOCK_USER.with(key.clone()),
            _ => return Err(ActionCallError::UnknownAction),
        };
        self.apply_blocked(action)
            .await
            .map_err(ActionCallError::refused)?;
        // What the operator asked of the key is its state now; the record of
        // it, and its history, is the operator's.
        Ok(serde_json::json!({
            "blocked": operation == ServiceOperation::Block,
            "key": key,
        }))
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
        // The workspace is its maker's: their person owns it from the step
        // that makes it, and this host administers it for them.
        let summary = self
            .create_for(name, workspace, device.user_public_key)
            .await
            .map_err(ActionCallError::refused)?;
        let invitation = match first {
            Some((terms, makes_administrator)) => Some(ActionInvitation::of(
                &self
                    .issue_as(&summary.workspace, inviter, terms, makes_administrator)
                    .await
                    .map_err(|error| ActionCallError::refused(error.to_string()))?,
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
    async fn listed(&self, device: &DeviceCert, only: &Listed) -> Vec<WorkspaceListing> {
        let labels = &only.labels;
        let person = device.user_public_key;
        // The person's own workspace holds what is theirs alone, and is never one of the
        // workspaces their shell shows them: it is left out of the list as the shell leaves it
        // out of what it takes in.
        let their_own = tilo_server_protocol::servers::person_workspace_id(&person);
        if person != self.inner.host.cert.user_public_key {
            let own = self.own().map(|own| own.of(&person)).unwrap_or_default();
            let now = tilo_clock::unix_now_ns();
            // A workspace they were invited into carries none of their labels. Every one the
            // list may name is read in one pass over the registry, however many they keep: the
            // list is built as often as it changes, and a lock per workspace would cost more
            // the more workspaces they have.
            let mut invited = if labels.is_empty() {
                self.inner.memberships.of(&person)
            } else {
                Vec::new()
            };
            invited.retain(|workspace| !own.contains(workspace));
            let wanted: std::collections::HashSet<String> = own
                .iter()
                .chain(&invited)
                .filter(|workspace| {
                    **workspace != their_own
                        && only.names(workspace)
                        && self.lists_to(device, workspace)
                })
                .cloned()
                .collect();
            let mut kept = self.listings_of(&wanted).await;
            let mut listed: Vec<WorkspaceListing> = Vec::new();
            for workspace in &own {
                if !wanted.contains(workspace) || !labelled(&self.labels_of(workspace), labels) {
                    continue;
                }
                if let Some(mut one) = kept.remove(workspace) {
                    one.role = Some(OWNER_ROLE.to_owned());
                    one.labels = self.labels_of(workspace);
                    one.unattended = self.runs_unattended(workspace);
                    listed.push(one);
                }
            }
            for workspace in invited {
                if !wanted.contains(&workspace) {
                    continue;
                }
                let Some(role) = self.role_of(&workspace, person, now).await else {
                    continue;
                };
                if let Some(mut one) = kept.remove(&workspace) {
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
        // The person who owns this server lists what it governs: each workspace's own
        // listing answers when it was made, which no summary carries.
        let mut listed: Vec<WorkspaceListing> = Vec::new();
        for summary in self.list().await {
            if summary.workspace == their_own
                || !only.names(&summary.workspace)
                || !self.lists_to(device, &summary.workspace)
            {
                continue;
            }
            let mut one = WorkspaceListing {
                role: Some(OWNER_ROLE.to_owned()),
                labels: self.labels_of(&summary.workspace),
                unattended: self.runs_unattended(&summary.workspace),
                ..listing(&summary)
            };
            one.made_at_ms = self
                .listing_of(&summary.workspace)
                .await
                .ok()
                .and_then(|listing| listing.made_at_ms);
            if labelled(&one.labels, labels) {
                listed.push(one);
            }
        }
        listed
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
            check_labels(labels).map_err(ActionCallError::invalid_request)?;
        }
        let seconds = args
            .integer("expires_in_seconds")?
            .unwrap_or(OWN_INVITATION_SECONDS);
        let Some(own) = self.own() else {
            return self
                .enter_host_persons(device, name, moving, request_id, expiry(now, seconds))
                .await;
        };
        // A grant for one workspace enters that one, already made: it makes no other, and labels
        // none, labels being its person's for their programs.
        let makes = device
            .capabilities
            .agent
            .is_none_or(|scope| scope.makes_workspaces());
        if !makes && Self::a_grant_asks_for_more(device, (name, moving), labels.as_ref()) {
            return Err(ActionCallError::refused(
                AdminRefusal::NotYourOwn.to_reason(),
            ));
        }
        // A backend that keeps a workspace per customer labels each with its own id for that
        // customer: asking again with the same labels when a call is retried answers the
        // workspace labelled so, not another one beside it.
        if let Some(existing) = self.own_labelled_for(person, (name, moving), labels.as_ref(), own)
        {
            let summary = self
                .summary(&existing)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
            return self
                .invite_own_person(
                    summary,
                    person,
                    Role::Editor,
                    request_id,
                    expiry(now, seconds),
                )
                .await;
        }
        // What could be made twice at once waits for the first: a workspace
        // the ask names (a retried create answers the one the first made),
        // and a person's own workspace asked for by no name (two of their
        // devices asking at once get one). A named new workspace has a fresh
        // identity of its own, so a backend's hundreds are made side by side.
        let making = Self::own_making(own, (moving, name), person).await;
        let theirs = moving.filter(|moving| own.is_of(&person, moving));
        // A key the workspace keeps as revoked is told so, and invited no more.
        self.refuse_revoked_agent_key(device, moving).await?;
        let summary = if let Some(moving) = theirs {
            // One of theirs: a device of theirs entering it, or a grant's
            // parent asking for the invitation into it.
            self.summary(moving)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?
        } else if let Some(joined) = moving.filter(|_| name.is_none())
            && let Some(role) = self.role_of(joined, person, now).await
        {
            // Another's, which the person was invited into or was handed:
            // another device of theirs (their key) enters it as them, at the
            // role they hold. No invitation gives the owner's place: theirs
            // is the Editor's invitation a workspace of their own invites
            // them with, which admits their device and gives them no role.
            drop(making);
            let summary = self
                .summary(joined)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
            let role = if role == Role::Owner {
                Role::Editor
            } else {
                role
            };
            return self
                .invite_own_person(summary, person, role, request_id, expiry(now, seconds))
                .await;
        } else if name.is_none() && moving.is_some() {
            // A workspace that is not theirs: another's, or one this host has
            // never made. The host makes a person's own workspace under an id
            // of its own choosing, so an id it does not know is never theirs,
            // and it is never read as some other workspace they have.
            return Err(ActionCallError::refused(
                AdminRefusal::NotYourOwn.to_reason(),
            ));
        } else if name.is_none()
            && let Some(first) = own.first_of(&person)
        {
            // A plain ask for "my workspace" answers the first they made.
            self.summary(&first)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?
        } else if !makes {
            return Err(ActionCallError::refused(
                AdminRefusal::NotYourOwn.to_reason(),
            ));
        } else {
            // Another one, for a person the site let in ([`Self::check_account`]): one they did
            // not name is named in their language. The person's own workspace and the entry in
            // their list are their own device's to write: this host founds and writes neither.
            self.make_own_workspace(person, (name, moving, language), own)
                .await?
        };
        if let Some(labels) = labels {
            self.label(&summary.workspace, &labels)
                .map_err(ActionCallError::refused)?;
        }
        drop(making);
        self.invite_own_person(
            summary,
            person,
            Role::Editor,
            request_id,
            expiry(now, seconds),
        )
        .await
    }

    /// The wait that makes a workspace two asks at once would make one by the name the ask gives,
    /// or by its person where it names none; a named new workspace is fresh, so it waits for none.
    async fn own_making(
        own: &crate::workspaces::own::OwnWorkspaces,
        (moving, name): (Option<&str>, Option<&str>),
        person: [u8; 32],
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        match (moving, name) {
            (Some(workspace), _) => Some(own.making(workspace.to_owned()).await),
            (None, None) => Some(own.making(hex::encode(person)).await),
            (None, Some(_)) => None,
        }
    }

    /// A key the workspace keeps as revoked is told so, and invited no more.
    async fn refuse_revoked_agent_key(
        &self,
        device: &DeviceCert,
        moving: Option<&str>,
    ) -> Result<(), ActionCallError> {
        if device.capabilities.agent.is_some()
            && let Some(moving) = moving
        {
            self.refuse_revoked_key(moving, device)
                .await
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
        }
        Ok(())
    }

    /// `person`'s new workspace, made under `name` or the one its language names, and recorded
    /// as theirs: their own device writes the entry in their list.
    async fn make_own_workspace(
        &self,
        person: [u8; 32],
        (name, moving, language): (Option<&str>, Option<&str>, Option<&str>),
        own: &crate::workspaces::own::OwnWorkspaces,
    ) -> Result<WorkspaceSummary, ActionCallError> {
        let name = name.map_or_else(|| crate::i18n::workspace_name(language), str::to_owned);
        let summary = self
            .create_for(&name, moving, person)
            .await
            .map_err(ActionCallError::refused)?;
        own.record(&person, &summary.workspace)
            .map_err(ActionCallError::refused)?;
        Ok(summary)
    }

    /// The workspace `person` already has under exactly `labels`, which an ask naming no
    /// workspace and retried with the same labels answers rather than making another.
    fn own_labelled_for(
        &self,
        person: [u8; 32],
        (name, moving): (Option<&str>, Option<&str>),
        labels: Option<&WorkspaceLabels>,
        own: &crate::workspaces::own::OwnWorkspaces,
    ) -> Option<String> {
        if moving.is_some() || name.is_none() {
            return None;
        }
        let labels = labels.filter(|labels| !labels.is_empty())?;
        own.of(&person)
            .into_iter()
            .find(|workspace| self.labels_of(workspace) == *labels)
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
            _ => {
                return Err(ActionCallError::refused(
                    AdminRefusal::NotYourOwn.to_reason(),
                ));
            }
        };
        let summary = self
            .summary(workspace)
            .await
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        self.invite_own_person(
            summary,
            device.user_public_key,
            Role::Editor,
            request_id,
            expires_at_ns,
        )
        .await
    }

    /// Whether `device`'s grant, which reaches one workspace already made, is asked for more
    /// than that: a name, a label, or a workspace it does not reach.
    fn a_grant_asks_for_more(
        device: &DeviceCert,
        (name, moving): (Option<&str>, Option<&str>),
        labels: Option<&WorkspaceLabels>,
    ) -> bool {
        name.is_some()
            || labels.is_some()
            || !moving.is_some_and(|workspace| {
                device
                    .capabilities
                    .agent
                    .is_some_and(|scope| scope.reaches(workspace))
            })
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
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        let mut workspace = listing(&summary);
        if let Some(own) = self.own()
            && own.is_of(&person, &summary.workspace)
        {
            workspace.labels = self.labels_of(&summary.workspace);
            workspace.unattended = self.runs_unattended(&summary.workspace);
        }
        answer(&CreatedWorkspace {
            workspace,
            invitation: Some(ActionInvitation::of(&issued)),
        })
    }

    /// Whom this host takes the workspace `request` hands over for, as
    /// `caller`, its governor, asks: nobody when its work setting takes it
    /// anyway, else its owner, when that is not this host's own person and
    /// this host keeps workspaces for them ([`Workspaces::keeps_for_owner`]).
    ///
    /// # Errors
    ///
    /// The host keeps it not for its owner: why.
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
        // A workspace its own person governs was taken above, whatever the work
        // setting; this host keeps another's only for an owner it keeps
        // workspaces for.
        let owner = super::super::owner(request)?;
        if owner == host.cert.user_public_key {
            return Err(
                "this host keeps a workspace another host governs only for its owner".into(),
            );
        }
        self.keeps_for_owner(&owner, now).await?;
        Ok(Some(owner))
    }

    /// A catch-up a device keeping a workspace asks for, already permitted
    /// to `device`: the catch-up itself ([`CATCH_UP_WORKSPACE`]), or the rest
    /// of where this host's store stands after it ([`CATCH_UP_STANDING`]).
    async fn catch_up_invoke(
        &self,
        device: &DeviceCert,
        action: &str,
        args: &Args,
    ) -> Result<Value, ActionCallError> {
        let workspace = args.required_text("workspace")?;
        if action == CATCH_UP_STANDING {
            let after =
                crate::workspaces::decode_hex(args.required_text("after")?).map_err(|error| {
                    ActionCallError::invalid_request(format!("`after` is no lineage: {error}"))
                })?;
            let (cursors, more) = self
                .standing_after(device, workspace, after)
                .await
                .map_err(ActionCallError::refused)?;
            return answer(&StandingPage {
                cursors: hex_postcard(&cursors)?,
                more,
            });
        }
        let cursors = Box::pin(self.catch_up_from(device, workspace))
            .await
            .map_err(ActionCallError::refused)?;
        // A store stands on a lineage per record and author: a device
        // that reads the rest after is answered the first page.
        let (cursors, more_cursors) = if args.flag("cursor_pages")? {
            self.standing_page(cursors, None)
        } else {
            (cursors, false)
        };
        answer(&CaughtUpWorkspace {
            cursors: hex_postcard(&cursors)?,
            more_cursors,
        })
    }

    /// The host operations that have one host of the person serve a workspace
    /// another governs ([`super::super::holding`]), already permitted to `device`:
    /// the action asked, and the operation it is.
    async fn serving_invoke(
        &self,
        device: &DeviceCert,
        (action, operation): (&str, ServiceOperation),
        args: &Args,
        now: u64,
    ) -> Result<Value, ActionCallError> {
        match operation {
            ServiceOperation::HostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                let workspace_id = WorkspaceId::parse(args.required_text("workspace_id")?)
                    .ok_or_else(|| {
                        ActionCallError::invalid_request("`workspace_id` is a workspace id")
                    })?;
                let role = serde_json::from_value::<tilo_peer_state::CopyRole>(Value::String(
                    args.required_text("role")?.to_owned(),
                ))
                .map_err(|_| {
                    ActionCallError::invalid_request("`role` is `key_holder` or `store`")
                })?;
                // Asked with no hand-over, the call names no workspace to keep: it
                // asks the address this host signs for it, which the owner's device
                // names it by in the change it hands over with.
                let Some(bootstrap) = args.text("bootstrap")? else {
                    if args.text("holder_hint")?.is_some() {
                        return Err(ActionCallError::invalid_request(
                            "a hand-over names both `holder_hint` and `bootstrap`",
                        ));
                    }
                    let hint = self
                        .host_address(workspace_id, role)
                        .map_err(|error| ActionCallError::refused(error.to_string()))?;
                    return answer(&HostedWorkspace {
                        host_hint: hex_postcard(&hint)?,
                        kept: Vec::new(),
                    });
                };
                let request = super::super::HostWorkspaceRequest {
                    workspace: args.required_text("workspace")?.to_owned(),
                    workspace_id,
                    holder_hint: super::super::decode_hex(args.required_text("holder_hint")?)
                        .map_err(|error| {
                            ActionCallError::invalid_request(format!("`holder_hint`: {error}"))
                        })?,
                    bootstrap: super::super::decode_hex(bootstrap).map_err(|error| {
                        ActionCallError::invalid_request(format!("`bootstrap`: {error}"))
                    })?,
                    role,
                };
                Box::pin(self.kept_for(device, &request, now))
                    .await
                    .map_err(ActionCallError::refused)?;
                let (hint, kept) = Box::pin(self.hold(device, &request))
                    .await
                    .map_err(ActionCallError::refused)?;
                answer(&HostedWorkspace {
                    host_hint: hex_postcard(&hint)?,
                    kept,
                })
            }
            ServiceOperation::UnhostWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                let workspace = args.required_text("workspace")?;
                if args.boolean("hand_on")?.unwrap_or(false) {
                    let handed = self
                        .hand_on_before_leaving(device, workspace)
                        .await
                        .map_err(ActionCallError::refused)?;
                    return Ok(serde_json::json!({ "handed": handed }));
                }
                let stopped = self
                    .unhold(device, workspace)
                    .await
                    .map_err(ActionCallError::refused)?;
                Ok(serde_json::json!({ "stopped": stopped }))
            }
            ServiceOperation::CatchUpWorkspace => {
                self.authorize(device, None, WorkspaceOp::Host(operation), now)
                    .map_err(|error| ActionCallError::refused(error.to_string()))?;
                #[cfg(any(test, feature = "test-support"))]
                if self.speaks_no_catch_up_now() {
                    return Err(ActionCallError::UnknownAction);
                }
                self.catch_up_invoke(device, action, args).await
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
#[cfg(test)]
mod calendar_accounts_tests;
#[cfg(all(test, feature = "agents"))]
mod calendars_turn_tests;
#[cfg(test)]
mod connections_tests;
#[cfg(all(test, feature = "agents"))]
mod device_key_tests;
#[cfg(test)]
mod formats_tests;
#[cfg(test)]
mod mail_reach_tests;
#[cfg(all(test, feature = "agents"))]
mod mail_sign_in_tests;
#[cfg(all(test, feature = "agents"))]
mod mail_turn_tests;
#[cfg(test)]
mod own_tests;
#[cfg(all(test, feature = "agents"))]
mod page_login_tests;
#[cfg(all(test, feature = "agents"))]
mod page_use_tests;
#[cfg(all(test, feature = "agents"))]
mod release_tests;
#[cfg(test)]
#[cfg(test)]
mod sign_in_tests;
#[cfg(all(test, feature = "agents"))]
mod sign_tests;
#[cfg(test)]
mod telling_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod told_tests;
#[cfg(test)]
mod turn_use_tests;
#[cfg(all(test, feature = "agents"))]
mod vault_asked_tests;
#[cfg(test)]
mod vault_tests;
#[cfg(test)]
mod waiting;
#[cfg(all(test, feature = "agents"))]
mod web_call_tests;
