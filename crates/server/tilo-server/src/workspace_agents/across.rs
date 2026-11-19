//! What a turn of a person's own assistant reaches of their other workspaces.
//!
//! A person's assistant lives in the workspace that is theirs alone, and a turn of it reaches
//! that workspace's resources and nothing of any other workspace. [`TurnElsewhere`] is the
//! one way past that, a provider of the turn's own ([`TurnProvider`]): it lists the other
//! workspaces its person is a member of on this server, each with whether the assistant may
//! read there, and performs authorized read-only lookups (`read` and `list-in-workspace`) in
//! another workspace governed on this server. It never grants write access across workspaces
//! and changes nothing anywhere.
//!
//! Whom it answers, decided again at every call:
//!
//! - a turn this server carries out for the agent's own keeper
//!   ([`Workspaces::keepers_turn`]): never a sub-agent's, and an agent its keeper narrowed
//!   only where a rule of its own names the action ([`HostedTurn::may_take`]);
//! - in a workspace that is its person's own: one this server keeps as theirs
//!   ([`Workspaces::owns`]), or, on a server that keeps none for people, one they are the
//!   owner of;
//! - where they are the only member not removed. Whoever is in that workspace holds the
//!   conversation, so from the moment somebody else is, a call is refused in words that say
//!   why. A turn of theirs that begins after that is still named the list where this server
//!   keeps the workspace as theirs, so that its call says so. On a server that keeps no
//!   workspaces for people nothing tells a person's own workspace that gained a member from a
//!   shared one they own, and such a turn is named none.
//!
//! Every other caller is answered what an unknown action is, and its listing of everything
//! names no such provider.
//!
//! What the list says of each workspace:
//!
//! - `always`: the person let reading there stay allowed for this agent
//!   ([`Workspaces::stands`]);
//! - `not_asked`: they are asked before anything is read there;
//! - `no_device`: the workspace admits them on no device of their own now. A device this
//!   server placed itself for a workspace's window is certified in a person's name and is
//!   often the newest they have there, and this server's own device is a member of every
//!   workspace it governs. Neither is ever the device a read for a person acts as, so a
//!   person with no other device there is on none. A server that was not told which devices
//!   it placed (`knowing_placed` of this server's agents) cannot tell them apart and says the
//!   same of every workspace;
//! - `another_server`: the person's own list of their workspaces names it and this server
//!   does not govern it ([`Workspaces::kept_elsewhere`]).
//!
//! A workspace the person is no member of is not in the list, whether it exists or not.
//!
//! A cross-workspace read or listing additionally verifies that:
//!
//! - the target workspace is another workspace governed on this server where the keeper is
//!   an active admitted member (not removed, not banned);
//! - the target workspace admits the keeper on an active device of their own (never the
//!   server's own device or a server-placed window device);
//! - the keeper granted lasting read permission for this assistant in that workspace
//!   ([`Workspaces::stands`]);
//! - the target workspace's data-destination policy is preserved by this conversation's
//!   workspace ([`Workspaces::workspace_destinations`]);
//! - the lookup executes strictly read-only (every undoable and outside action is denied)
//!   and returns explicit provenance (`workspace_origin`, `destination_workspace`,
//!   `acting_identity`, `destination_boundary`, and `read_only: true`).

use std::sync::Arc;

use serde::Deserialize;
use serde_json::{Map, Value, json};
use tilo_actions::remote::ActionCallError as NotTaken;
use tilo_actions::remote::person_asking::{ActionName, Asked, Thing};
use tilo_actions::remote::{
    Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument, InvokeRequest,
    Pending, RemoteActionTarget,
};
use tilo_actions::{
    ActionContract, ActionDescriptor, ActionFailure, ActionKind, ActionOutput, ActionParam,
    InvocationId, InvocationUpdate, ParamType,
};
use tilo_agent_session::workspace_actions::WorkspaceActions;
use tilo_agent_turn::Tools;
use tilo_mcp::McpServer;
use tilo_workspace_agent::Agent;

use super::turn_provider::{Ending, Rows};
use super::{Turn, TurnProvider, TurnScope, WorkspaceActionProvider, WorkspaceId};
use crate::environment::EnvironmentRegistry;
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::person_model::{AllowedDestination, DestinationPolicy};

/// The provider a turn's tools find its person's other workspaces under.
const PROVIDER: &str = "tilo.other-workspaces";
/// The one resource it holds: the person's other workspaces.
const MINE: &str = "workspaces";
/// The read of them.
const LIST: &str = "list";
/// Reading in one of them, as a permission that stays names it.
const READ: &str = "read";
/// Listing apps or resources in one of them, read-only.
const LIST_IN_WORKSPACE: &str = "list-in-workspace";
/// The code of a call refused in words.
const REFUSED: &str = "refused";
/// The code when the keeper is not an active admitted member of the target workspace here.
const NOT_MEMBER: &str = "not_member";
/// The code when the target workspace's data-destination policy blocks bringing data here.
const DESTINATION_BLOCKED: &str = "destination_blocked";
/// The role of a workspace's owner, as its membership names it.
const OWNER: &str = "owner";

/// The person let reading in the workspace stay allowed.
const ALWAYS: &str = "always";
/// The person is asked before anything is read in the workspace.
const NOT_ASKED: &str = "not_asked";
/// The workspace admits the person on no device of their own now.
const NO_DEVICE: &str = "no_device";
/// Another server keeps the workspace.
const ANOTHER_SERVER: &str = "another_server";

/// Longest resource path accepted for a cross-workspace lookup, in bytes.
const MAX_PATH_BYTES: usize = 512;
/// Longest pagination cursor accepted for a cross-workspace lookup, in bytes.
const MAX_CURSOR_BYTES: usize = 256;
/// Largest page size accepted for a cross-workspace resource read, in characters.
const MAX_READ_CHARS: usize = 32_768;
/// Largest row limit accepted for a cross-workspace listing.
const MAX_LIST_LIMIT: usize = 200;

/// What a call is told once its person's own workspace has somebody else in it.
const NOT_ALONE: &str = "The workspace this conversation is in has another member now, and \
     whoever is in it can read this conversation. So nothing of your person's other \
     workspaces is listed or read from here. Tell them that, and that it holds for as long \
     as somebody else is a member of this workspace.";

/// What one turn is told of its person's other workspaces.
pub(super) struct TurnElsewhere {
    workspaces: Workspaces,
    /// The action server reaching every workspace governed here.
    actions: WorkspaceActions,
    /// The devices this server placed itself, where it was told them.
    placed: Option<EnvironmentRegistry>,
    /// The turn's own workspace.
    workspace: String,
    workspace_id: WorkspaceId,
    /// The turn's agent, as the workspace's list held it when the turn began.
    agent: Agent,
    /// The conversation and cause of this turn, carried on cross-workspace reads.
    conversation: String,
    cause: String,
    /// What the person asked of this turn, for session labelling.
    asked: String,
    /// Whether the turn's listing names the provider: this answered the turn when it began,
    /// or would have but for somebody else in its person's own workspace, where this server
    /// keeps that workspace as theirs. No other turn's listing names it.
    offered: bool,
}

/// Arguments for [`READ`]: reading a single resource path in another workspace.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    workspace: String,
    path: String,
    #[serde(default)]
    cursor: Option<String>,
    #[serde(default)]
    max_chars: Option<usize>,
}

/// Arguments for [`LIST_IN_WORKSPACE`]: listing apps or resources in another workspace.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListInArgs {
    workspace: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    cursor: Option<String>,
}

/// A verified cross-workspace read context: every boundary check has already passed.
struct AuthorizedTarget {
    target_id: WorkspaceId,
    target_workspace: String,
    target_name: String,
    destination_workspace: String,
    person: [u8; 32],
    device: [u8; 32],
    agent_id: String,
    agent_name: String,
    member_role: String,
    permission_id: String,
    source_policy: DestinationPolicy,
    destination_policy: DestinationPolicy,
}

impl AuthorizedTarget {
    /// Explicit provenance metadata naming the source workspace, destination workspace,
    /// acting identity, destination boundary, and read-only enforcement.
    fn provenance(&self, path: Option<&str>) -> Value {
        json!({
            "workspace_origin": self.target_workspace,
            "workspace_origin_name": self.target_name,
            "destination_workspace": self.destination_workspace,
            "path": path,
            "acting_identity": {
                "person": hex::encode(self.person),
                "device": hex::encode(self.device),
                "agent": self.agent_id,
                "agent_name": self.agent_name,
                "role": self.member_role,
            },
            "destination_boundary": {
                "source_policy": describe_policy(&self.source_policy),
                "destination_policy": describe_policy(&self.destination_policy),
                "preserved": true,
            },
            "read_only": true,
        })
    }

    /// The structured read-only response carrying `content` beside its explicit provenance.
    fn payload(&self, path: Option<&str>, content: String) -> Value {
        let mut value = self.provenance(path);
        if let Some(fields) = value.as_object_mut() {
            fields.insert("content".to_owned(), Value::String(content));
        }
        value
    }
}

/// Why a caller is told nothing of its person's other workspaces.
enum Refusal {
    /// It is no turn this answers: it is answered what an unknown action is.
    Unknown,
    /// Its person's own workspace has somebody else in it now.
    NotAlone,
}

/// Why a cross-workspace lookup target was refused before reaching the target workspace.
enum TargetRefusal {
    /// Refused in words with structured provenance (ended invocation failure).
    Failed(ActionFailure),
    /// Malformed request rejected before invocation.
    Rejected(NotTaken),
}

/// Make `scope`'s service answer as well for the other workspaces of `turn`'s person, where
/// `placed` holds the devices this server placed itself.
pub(super) async fn join(
    workspaces: &Workspaces,
    placed: Option<&EnvironmentRegistry>,
    turn: &Turn,
    scope: &mut TurnScope,
) {
    let mut own = TurnElsewhere {
        workspaces: workspaces.clone(),
        actions: scope.kept.actions.clone(),
        placed: placed.cloned(),
        workspace: scope.workspace.clone(),
        workspace_id: scope.workspace_id,
        agent: turn.agent.clone(),
        conversation: turn.conversation.clone(),
        cause: turn.cause.clone(),
        asked: turn.asked.clone(),
        offered: false,
    };
    let caller = scope.reply(turn).caller;
    let mut offered = false;
    for action in [LIST, READ, LIST_IN_WORKSPACE] {
        match own.whose(&caller, action).await {
            Ok(_) => {
                offered = true;
                break;
            }
            // Where this server keeps the workspace as that person's own, a turn of theirs that
            // begins once somebody else is in it is still named the list, so that its call says
            // why nothing is listed. Nobody else's turn is named it.
            Err(Refusal::NotAlone) if workspaces.keeps_own_workspaces() => {
                offered = true;
                break;
            }
            Err(Refusal::NotAlone | Refusal::Unknown) => {}
        }
    }
    own.offered = offered;
    let service = &mut scope.provider;
    *service = service.clone().answering(Arc::new(own));
}

impl TurnElsewhere {
    /// The person `caller`'s turn acts for, where it is one this answers `action` to: a turn
    /// of the agent's own keeper, in a workspace that is theirs and holds nobody else now.
    async fn whose(&self, caller: &Caller, action: &str) -> Result<[u8; 32], Refusal> {
        if self.agent.is_sub() {
            return Err(Refusal::Unknown);
        }
        let bound = self
            .agent
            .bound_rules()
            .map(|_| self.agent.turn_rules(&self.workspace));
        let turn = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: bound.as_deref(),
        };
        let Ok(kept) = self.workspaces.keepers_turn(&turn).await else {
            return Err(Refusal::Unknown);
        };
        let target = RemoteActionTarget {
            provider: PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(MINE, action),
        };
        if kept.agent != self.agent.id.0 || !turn.may_take(&self.workspace, &target) {
            return Err(Refusal::Unknown);
        }
        let person = hex::encode(kept.person);
        let Ok((_, people)) = self.workspaces.membership(&self.workspace).await else {
            return Err(Refusal::Unknown);
        };
        let theirs = if self.workspaces.keeps_own_workspaces() {
            self.workspaces.owns(&kept.person, &self.workspace)
        } else {
            people
                .get(&person)
                .is_some_and(|mark| !mark.removed && !mark.banned && mark.role == OWNER)
        };
        if !theirs {
            return Err(Refusal::Unknown);
        }
        if people
            .iter()
            .any(|(who, mark)| *who != person && !mark.removed)
        {
            return Err(Refusal::NotAlone);
        }
        Ok(kept.person)
    }

    /// The device of `person`'s own that a read for them in the workspace `there` acts as:
    /// the newest that workspace admits them on now that is neither this server's own device
    /// nor one this server placed itself ([`Workspaces::persons_own_device`]). None where they
    /// have no such device there, and none where these agents were not told what this server
    /// placed. What the agents were told is asked once more here.
    async fn device_there(&self, there: WorkspaceId, person: [u8; 32]) -> Option<[u8; 32]> {
        let placed = self.placed.as_ref()?;
        let device = self.workspaces.persons_own_device(there, person).await?;
        let theirs = device != self.workspaces.governor().device_public_key
            && placed.of(&device, tilo_clock::unix_now_ns()).is_none();
        theirs.then_some(device)
    }

    /// What an answer of always to reading in the workspace `there` covers: this agent, asked
    /// in the turn's own workspace, reading in that one workspace.
    fn reading_in(&self, there: &str) -> Asked {
        Asked {
            workspace: self.workspace.clone(),
            agent: self.agent.id.0.clone(),
            action: ActionName {
                provider: PROVIDER.to_owned(),
                kind: MINE.to_owned(),
                action: READ.to_owned(),
            },
            on: Some(Thing {
                document: String::new(),
                resource: there.to_owned(),
            }),
        }
    }

    /// The other workspaces `person` is a member of on this server, and those their own list
    /// says another server keeps, each with whether this agent may read there.
    async fn list(&self, person: [u8; 32]) -> Value {
        let me = hex::encode(person);
        let mut here = self.workspaces.list().await;
        here.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.workspace.cmp(&b.workspace))
        });
        let mut listed = Vec::new();
        for summary in here {
            if summary.workspace == self.workspace {
                continue;
            }
            let member = self
                .workspaces
                .membership(&summary.workspace)
                .await
                .is_ok_and(|(_, people)| {
                    people
                        .get(&me)
                        .is_some_and(|mark| !mark.removed && !mark.banned && !mark.role.is_empty())
                });
            if !member {
                continue;
            }
            let reading = if self
                .device_there(summary.workspace_id, person)
                .await
                .is_none()
            {
                NO_DEVICE
            } else if self
                .workspaces
                .stands(&person, &self.reading_in(&summary.workspace))
                .is_some()
            {
                ALWAYS
            } else {
                NOT_ASKED
            };
            listed.push(json!({
                "workspace": summary.workspace,
                "name": summary.name,
                "reading": reading,
            }));
        }
        for (workspace, name) in self.workspaces.kept_elsewhere(&person).await {
            listed.push(json!({
                "workspace": workspace,
                "name": name,
                "reading": ANOTHER_SERVER,
            }));
        }
        json!({
            "workspaces": listed,
            "reading": {
                "always": "Your person allowed reading there for good.",
                "not_asked": "Your person is asked before anything is read there.",
                "no_device": "That workspace admits your person on no device of their own \
                              now, so nothing is read there for them.",
                "another_server": "Another server keeps that workspace. Nothing of it is \
                                   reached from here.",
            },
        })
    }

    /// Verify active membership of `person` in `target_workspace` on this server, returning
    /// their role or a clear refusal distinguishing another server from non-membership.
    async fn member_role_there(
        &self,
        person: [u8; 32],
        target_workspace: &str,
    ) -> Result<String, ActionFailure> {
        let me = hex::encode(person);
        match self.workspaces.membership(target_workspace).await {
            Ok((_, people)) => people
                .get(&me)
                .filter(|mark| !mark.removed && !mark.banned && !mark.role.is_empty())
                .map(|mark| mark.role.clone())
                .ok_or_else(|| {
                    self.refused(
                        NOT_MEMBER,
                        "Your person is not an active member of that workspace on this server, \
                         so nothing may be read there.",
                        target_workspace,
                    )
                }),
            Err(_) => {
                if let Some((_, name)) = self
                    .workspaces
                    .kept_elsewhere(&person)
                    .await
                    .into_iter()
                    .find(|(id, _)| id == target_workspace)
                {
                    return Err(failure(
                        ANOTHER_SERVER,
                        format!(
                            "Another server keeps that workspace ({name}). Cross-workspace reads \
                             only reach workspaces governed on this server."
                        ),
                        Some(json!({
                            "reason": ANOTHER_SERVER,
                            "workspace_origin": target_workspace,
                            "workspace_origin_name": name,
                            "destination_workspace": self.workspace,
                        })),
                    ));
                }
                Err(self.refused(
                    NOT_MEMBER,
                    "That workspace is not governed on this server for your person, so nothing \
                     may be read there.",
                    target_workspace,
                ))
            }
        }
    }

    /// Verify that bringing data from `target_id` into this conversation's workspace preserves
    /// `target_id`'s data-destination policy. Fails closed if either policy cannot be read.
    fn check_destination_boundary(
        &self,
        target_id: WorkspaceId,
        target_workspace: &str,
    ) -> Result<(DestinationPolicy, DestinationPolicy), ActionFailure> {
        let source_policy = self
            .workspaces
            .workspace_destinations(&target_id)
            .map_err(|_| {
                self.refused(
                    DESTINATION_BLOCKED,
                    "The target workspace's data-destination policy could not be read, so \
                     nothing is read from it.",
                    target_workspace,
                )
            })?;
        let destination_policy = self
            .workspaces
            .workspace_destinations(&self.workspace_id)
            .map_err(|_| {
                self.refused(
                    DESTINATION_BLOCKED,
                    "This workspace's data-destination policy could not be read, so nothing is \
                     brought in from another workspace.",
                    target_workspace,
                )
            })?;
        if !destination_allows(&source_policy, &destination_policy) {
            return Err(failure(
                DESTINATION_BLOCKED,
                "The target workspace restricts where its data may go more tightly than this \
                 conversation's workspace, so reading its content here is refused to preserve \
                 its destination boundary.",
                Some(json!({
                    "reason": DESTINATION_BLOCKED,
                    "workspace_origin": target_workspace,
                    "destination_workspace": self.workspace,
                    "source_policy": describe_policy(&source_policy),
                    "destination_policy": describe_policy(&destination_policy),
                })),
            ));
        }
        Ok((source_policy, destination_policy))
    }

    fn refused(&self, code: &str, message: &str, target_workspace: &str) -> ActionFailure {
        failure(
            code,
            message,
            Some(json!({
                "reason": code,
                "workspace_origin": target_workspace,
                "destination_workspace": self.workspace,
            })),
        )
    }

    /// Verify that `person` may perform a read-only lookup in `raw_workspace` on this server
    /// and that bringing content back into this turn's workspace preserves all boundaries.
    async fn authorize_target(
        &self,
        person: [u8; 32],
        raw_workspace: &str,
    ) -> Result<AuthorizedTarget, TargetRefusal> {
        let target_id = WorkspaceId::parse(raw_workspace.trim()).ok_or_else(|| {
            TargetRefusal::Rejected(NotTaken::InvalidRequest {
                reason: "workspace must be a valid workspace id".to_owned(),
            })
        })?;
        let target_workspace = target_id.to_string();
        if target_id == self.workspace_id {
            return Err(TargetRefusal::Failed(self.refused(
                REFUSED,
                "That workspace is the one this conversation is already in; use the ordinary \
                 workspace tools here.",
                &target_workspace,
            )));
        }
        let member_role = self
            .member_role_there(person, &target_workspace)
            .await
            .map_err(TargetRefusal::Failed)?;
        let Some(device) = self.device_there(target_id, person).await else {
            return Err(TargetRefusal::Failed(self.refused(
                NO_DEVICE,
                "That workspace admits your person on no active device of their own now (a \
                 server-owned or server-placed device is never used), so nothing is read there \
                 for them.",
                &target_workspace,
            )));
        };
        let Some(permission_id) = self
            .workspaces
            .stands(&person, &self.reading_in(&target_workspace))
        else {
            return Err(TargetRefusal::Failed(self.refused(
                NOT_ASKED,
                "Your person has not granted read access to that workspace for this assistant \
                 yet. Ask your person to allow reading in that workspace before looking there.",
                &target_workspace,
            )));
        };
        let (source_policy, destination_policy) = self
            .check_destination_boundary(target_id, &target_workspace)
            .map_err(TargetRefusal::Failed)?;
        let target_name = self
            .workspaces
            .workspace_name(target_id)
            .await
            .unwrap_or_default();
        Ok(AuthorizedTarget {
            target_id,
            target_workspace,
            target_name,
            destination_workspace: self.workspace.clone(),
            person,
            device,
            agent_id: self.agent.id.0.clone(),
            agent_name: self.agent.name.clone(),
            member_role,
            permission_id,
            source_policy,
            destination_policy,
        })
    }

    /// Build the strictly read-only [`Caller`] for looking into `target_workspace` as `person`
    /// on their own active `device`, delegated to this assistant with every non-read action
    /// denied.
    fn target_caller(&self, person: [u8; 32], device: [u8; 32], target_workspace: &str) -> Caller {
        let session = format!(
            "agent-session:across-{}",
            blake3::hash(
                format!(
                    "{}\0{}\0{}\0{}\0{}",
                    self.workspace,
                    target_workspace,
                    self.conversation,
                    self.agent.id.0,
                    self.cause
                )
                .as_bytes(),
            )
            .to_hex()
            .get(..24)
            .unwrap_or_default()
        );
        Caller::authenticated(person, device)
            .in_workspace(target_workspace)
            .delegated_to(self.agent.id.0.clone())
            .named(&self.agent.name)
            .in_agent_session(session)
            .asked(&self.asked)
            .on_turn(
                tilo_action_policy::turn::Turn::new(self.conversation.clone(), self.cause.clone()),
                reading_only(target_workspace),
            )
    }

    /// Execute one read-only MCP tool (`tilo_read` or `tilo_ls`) against `authorized`'s
    /// workspace action provider, with no personal or cross-workspace turn providers attached.
    async fn execute_read_only(
        &self,
        authorized: &AuthorizedTarget,
        path: Option<&str>,
        tool: &str,
        args: Value,
    ) -> Result<Value, ActionFailure> {
        let provider = WorkspaceActionProvider::new(self.actions.clone(), authorized.target_id);
        let caller = self.target_caller(
            authorized.person,
            authorized.device,
            &authorized.target_workspace,
        );
        let server = McpServer::for_turn(provider, caller).map_err(|error| {
            failure(
                REFUSED,
                format!("Could not open a read-only lookup in that workspace: {error}"),
                Some(authorized.provenance(path)),
            )
        })?;
        let answer = Tools::call(&server, tool, args).await;
        if answer.is_error {
            return Err(failure(
                REFUSED,
                answer.text,
                Some(authorized.provenance(path)),
            ));
        }
        self.workspaces
            .permission_used(&authorized.person, &authorized.permission_id);
        Ok(authorized.payload(path, answer.text))
    }

    /// Carry out [`READ`]: read one validated app or resource path in another workspace.
    async fn read_in(
        &self,
        invocation: InvocationId,
        person: [u8; 32],
        given: &Map<String, Value>,
    ) -> Result<InvocationUpdate, NotTaken> {
        let args = read_as::<ReadArgs>(given)?;
        let path = validate_path(&args.path)?;
        let cursor = args.cursor.as_deref().map(validate_cursor).transpose()?;
        let max_chars = args
            .max_chars
            .map(|limit| validate_bound("max_chars", limit, MAX_READ_CHARS))
            .transpose()?;
        let authorized = match self.authorize_target(person, &args.workspace).await {
            Ok(authorized) => authorized,
            Err(TargetRefusal::Failed(error)) => {
                return Ok(InvocationUpdate::Failed { invocation, error });
            }
            Err(TargetRefusal::Rejected(error)) => return Err(error),
        };
        let mut tool_args = Map::new();
        tool_args.insert("path".to_owned(), Value::String(path.clone()));
        if let Some(cursor) = cursor {
            tool_args.insert("cursor".to_owned(), Value::String(cursor));
        }
        if let Some(max_chars) = max_chars {
            tool_args.insert("max_chars".to_owned(), json!(max_chars));
        }
        match self
            .execute_read_only(
                &authorized,
                Some(&path),
                "tilo_read",
                Value::Object(tool_args),
            )
            .await
        {
            Ok(value) => Ok(succeeded(invocation, value)),
            Err(error) => Ok(InvocationUpdate::Failed { invocation, error }),
        }
    }

    /// Carry out [`LIST_IN_WORKSPACE`]: list apps or resources in another workspace, read-only.
    async fn list_in(
        &self,
        invocation: InvocationId,
        person: [u8; 32],
        given: &Map<String, Value>,
    ) -> Result<InvocationUpdate, NotTaken> {
        let args = read_as::<ListInArgs>(given)?;
        let path = args
            .path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(validate_path)
            .transpose()?;
        let kind = args.kind.as_deref().map(validate_list_kind).transpose()?;
        let limit = args
            .limit
            .map(|count| validate_bound("limit", count, MAX_LIST_LIMIT))
            .transpose()?;
        let cursor = args.cursor.as_deref().map(validate_cursor).transpose()?;
        let authorized = match self.authorize_target(person, &args.workspace).await {
            Ok(authorized) => authorized,
            Err(TargetRefusal::Failed(error)) => {
                return Ok(InvocationUpdate::Failed { invocation, error });
            }
            Err(TargetRefusal::Rejected(error)) => return Err(error),
        };
        let mut tool_args = Map::new();
        if let Some(path) = &path {
            tool_args.insert("path".to_owned(), Value::String(path.clone()));
        }
        if let Some(kind) = kind {
            tool_args.insert("kind".to_owned(), Value::String(kind.to_owned()));
        }
        if let Some(limit) = limit {
            tool_args.insert("limit".to_owned(), json!(limit));
        }
        if let Some(cursor) = cursor {
            tool_args.insert("cursor".to_owned(), Value::String(cursor));
        }
        match self
            .execute_read_only(
                &authorized,
                path.as_deref(),
                "tilo_ls",
                Value::Object(tool_args),
            )
            .await
        {
            Ok(value) => Ok(succeeded(invocation, value)),
            Err(error) => Ok(InvocationUpdate::Failed { invocation, error }),
        }
    }

    /// Take the call `request` asks for as `caller`: how it ended, or why this server did
    /// not take it.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, NotTaken> {
        let invocation = request.invocation;
        let address = &request.target.address;
        if address.resource != MINE {
            return Err(NotTaken::UnknownAction);
        }
        let verb = address.action.as_str();
        if !matches!(verb, LIST | READ | LIST_IN_WORKSPACE) {
            return Err(NotTaken::UnknownAction);
        }
        let person = match self.whose(caller, verb).await {
            Ok(person) => person,
            Err(Refusal::Unknown) => return Err(NotTaken::UnknownAction),
            Err(Refusal::NotAlone) => {
                return Ok(InvocationUpdate::Failed {
                    invocation,
                    error: failure(REFUSED, NOT_ALONE, None),
                });
            }
        };
        let none = Map::new();
        let given = match &request.argument {
            InvokeArgument::Json(given) => given,
            _ => &none,
        };
        match verb {
            LIST => Ok(succeeded(invocation, self.list(person).await)),
            READ => self.read_in(invocation, person, given).await,
            LIST_IN_WORKSPACE => self.list_in(invocation, person, given).await,
            _ => Err(NotTaken::UnknownAction),
        }
    }
}

impl TurnProvider for TurnElsewhere {
    /// The provider, as a listing of everything names it beside the workspace's resources:
    /// to a turn this did not answer when it began, not at all.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if !self.offered {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: PROVIDER.to_owned(),
            app_id: PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The rows `query` asks for, when it asks this provider.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (self.offered && query.provider.as_deref() == Some(PROVIDER))
            .then(|| Pending::ready(Ok(rows())))
    }

    /// Everything asked of this provider is this one's to answer, also what it refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
    }

    /// What a read or listing in another workspace answers may have been written by another
    /// member of that workspace, so the turn's tools label it as data to reason about and
    /// never an instruction to follow.
    fn carries_untrusted(&self, target: &RemoteActionTarget) -> bool {
        target.provider == PROVIDER
            && target.address.resource == MINE
            && matches!(target.address.action.as_str(), READ | LIST_IN_WORKSPACE)
    }

    /// A turn's caller changes when its person writes again while it works, so a call is
    /// taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// Every action in `workspace` that is not a read, denied.
fn reading_only(workspace: &str) -> Vec<tilo_action_policy::Rule> {
    vec![tilo_action_policy::Rule::new(
        tilo_action_policy::TreePath::workspace(workspace).rest(),
        tilo_action_policy::KindSet::UNDOABLE.union(tilo_action_policy::KindSet::OUTSIDE),
        tilo_action_policy::Answer::Deny,
    )]
}

/// Whether content from a workspace under `source` may be brought into a conversation in a
/// workspace under `destination`: every destination allowed by `destination` must also be
/// permitted by `source`.
fn destination_allows(source: &DestinationPolicy, destination: &DestinationPolicy) -> bool {
    match source {
        DestinationPolicy::Any => true,
        DestinationPolicy::NoneOutsideServer => match destination {
            DestinationPolicy::NoneOutsideServer => true,
            DestinationPolicy::Only(rules) => {
                !rules.is_empty()
                    && rules
                        .iter()
                        .all(|rule| matches!(rule, AllowedDestination::ThisServer))
            }
            DestinationPolicy::Any => false,
        },
        DestinationPolicy::Only(allowed) => {
            if allowed.is_empty() {
                return false;
            }
            match destination {
                DestinationPolicy::Any => false,
                DestinationPolicy::NoneOutsideServer => {
                    allowed.contains(&AllowedDestination::ThisServer)
                }
                DestinationPolicy::Only(dest_rules) => {
                    !dest_rules.is_empty()
                        && dest_rules
                            .iter()
                            .all(|dest_rule| allowed.iter().any(|src| rule_covers(src, dest_rule)))
                }
            }
        }
    }
}

/// Whether one source-workspace destination rule `source` covers `destination`.
fn rule_covers(source: &AllowedDestination, destination: &AllowedDestination) -> bool {
    match (source, destination) {
        (AllowedDestination::ThisServer, AllowedDestination::ThisServer)
        | (AllowedDestination::PrivateNetwork, AllowedDestination::PrivateNetwork) => true,
        (AllowedDestination::Provider(src), AllowedDestination::Provider(dst))
        | (AllowedDestination::Endpoint(src), AllowedDestination::Endpoint(dst)) => {
            let (src, dst) = (src.trim(), dst.trim());
            !src.is_empty() && src.eq_ignore_ascii_case(dst)
        }
        _ => false,
    }
}

/// A structured, secret-free summary of a workspace's data-destination policy.
fn describe_policy(policy: &DestinationPolicy) -> Value {
    match policy {
        DestinationPolicy::Any => json!({"kind": "any"}),
        DestinationPolicy::NoneOutsideServer => json!({"kind": "none_outside_server"}),
        DestinationPolicy::Only(rules) => json!({
            "kind": "only",
            "allowed": rules.iter().map(AllowedDestination::describe).collect::<Vec<_>>(),
        }),
    }
}

/// Parse `given` as the typed argument struct for one action.
fn read_as<T: serde::de::DeserializeOwned>(given: &Map<String, Value>) -> Result<T, NotTaken> {
    serde_json::from_value(Value::Object(given.clone())).map_err(|error| NotTaken::InvalidRequest {
        reason: error.to_string(),
    })
}

/// Strictly validate an app or resource path for a cross-workspace read or listing.
fn validate_path(raw: &str) -> Result<String, NotTaken> {
    let path = raw.trim();
    if path.is_empty() {
        return Err(NotTaken::InvalidRequest {
            reason: "path must not be empty".to_owned(),
        });
    }
    if path.len() > MAX_PATH_BYTES {
        return Err(NotTaken::InvalidRequest {
            reason: format!("path exceeds {MAX_PATH_BYTES} bytes"),
        });
    }
    if path.chars().any(char::is_control) {
        return Err(NotTaken::InvalidRequest {
            reason: "path must not contain control characters".to_owned(),
        });
    }
    if path.starts_with('/') || path.ends_with('/') || path.contains('\\') || path.contains('#') {
        return Err(NotTaken::InvalidRequest {
            reason: "path must be a relative app or resource path without action suffixes"
                .to_owned(),
        });
    }
    let mut segments = path.split('/');
    let Some(first) = segments.next() else {
        return Err(NotTaken::InvalidRequest {
            reason: "path must not be empty".to_owned(),
        });
    };
    if first.is_empty() || first.starts_with('.') || first.contains(':') {
        return Err(NotTaken::InvalidRequest {
            reason: "path must begin with a valid app or provider name".to_owned(),
        });
    }
    for segment in segments {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err(NotTaken::InvalidRequest {
                reason: "path must not contain empty or traversal segments".to_owned(),
            });
        }
    }
    Ok(path.to_owned())
}

/// Validate a pagination cursor passed to a cross-workspace lookup.
fn validate_cursor(raw: &str) -> Result<String, NotTaken> {
    let cursor = raw.trim();
    if cursor.is_empty() || cursor.len() > MAX_CURSOR_BYTES || cursor.chars().any(char::is_control)
    {
        return Err(NotTaken::InvalidRequest {
            reason: "cursor is not valid".to_owned(),
        });
    }
    Ok(cursor.to_owned())
}

/// Validate the optional `kind` filter of [`LIST_IN_WORKSPACE`].
fn validate_list_kind(raw: &str) -> Result<&'static str, NotTaken> {
    match raw.trim() {
        "resources" => Ok("resources"),
        "actions" => Ok("actions"),
        _ => Err(NotTaken::InvalidRequest {
            reason: "kind must be \"resources\" or \"actions\"".to_owned(),
        }),
    }
}

/// Validate a positive bounded integer parameter.
fn validate_bound(name: &str, value: usize, max: usize) -> Result<usize, NotTaken> {
    if value == 0 || value > max {
        return Err(NotTaken::InvalidRequest {
            reason: format!("{name} must be between 1 and {max}"),
        });
    }
    Ok(value)
}

fn succeeded(invocation: InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

fn failure(code: &str, message: impl Into<String>, details: Option<Value>) -> ActionFailure {
    ActionFailure {
        code: code.to_owned(),
        message: message.into(),
        details,
        retryable: false,
    }
}

/// One row of this provider's, strictly read-only and offered to agents alone.
fn row(action: &str, label: &str, description: &str, params: Vec<ActionParam>) -> CatalogueRow {
    let mut contract = ActionContract {
        kind: ActionKind::Read,
        ..ActionContract::default()
    };
    // A turn's own: no person picks it from a list.
    contract.set_audience(tilo_actions::contract::Audience::Agents);
    CatalogueRow::for_action(
        PROVIDER,
        PROVIDER,
        ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            address: tilo_actions::ActionAddress::new(MINE, action),
            resource_kind: MINE.to_owned(),
            resource_label: "Other workspaces".to_owned(),
            action_label: label.to_owned(),
            keywords: Vec::new(),
            icon: None,
            description: Some(description.to_owned()),
            locus: tilo_actions::ExecutionLocus::ServerPreferred,
            kind: tilo_actions::OperationKind::Effect,
            contract,
            input: tilo_actions::InputSpec::None,
            params,
            view: None,
        },
    )
}

/// The rows a catalogue offers an agent: listing other workspaces, and authorized read-only
/// listing and reading inside another workspace on this server.
fn rows() -> Vec<CatalogueRow> {
    let target_workspace = || {
        ActionParam::required(
            "workspace",
            ParamType::Text,
            "The target workspace id (UUID) from `other-workspaces/workspaces#list`.",
        )
    };
    vec![
        row(
            LIST,
            "List workspaces",
            "The other workspaces your person is a member of on this server, each by its \
             name and id, with whether you may read there: always, not asked yet, on no \
             device of their own there, or kept by another server. It reads nothing in \
             any of them.",
            Vec::new(),
        ),
        row(
            READ,
            "Read in workspace",
            "Read a resource at `path` in another workspace your person belongs to on this \
             server. Strictly read-only: requires active membership, an active device of \
             your person's own there, lasting read permission (`always`), and compatible \
             data-destination boundaries. Returns `workspace_origin`, `destination_workspace`, \
             `acting_identity`, `destination_boundary`, and `read_only: true`.",
            vec![
                target_workspace(),
                ActionParam::required(
                    "path",
                    ParamType::Text,
                    "App or resource path to read in the target workspace (e.g. `notes/Todo` \
                     or `word-processor/Document`).",
                ),
                ActionParam::optional(
                    "cursor",
                    ParamType::Text,
                    "Pagination cursor returned by an earlier read.",
                ),
                ActionParam::optional(
                    "max_chars",
                    ParamType::Integer,
                    "Maximum characters to return in one page (1 to 32768).",
                ),
            ],
        ),
        row(
            LIST_IN_WORKSPACE,
            "List in workspace",
            "List apps or an app's resources in another workspace your person belongs to on \
             this server. Strictly read-only, under the same membership, own-device, lasting \
             read permission (`always`), and data-destination boundary checks as `read`. \
             Returns `workspace_origin`, `destination_workspace`, `acting_identity`, \
             `destination_boundary`, and `read_only: true`.",
            vec![
                target_workspace(),
                ActionParam::optional(
                    "path",
                    ParamType::Text,
                    "Optional app or resource path to list (omit to list apps in the workspace).",
                ),
                ActionParam::optional(
                    "kind",
                    ParamType::Text,
                    "Optional filter: `resources` or `actions`.",
                ),
                ActionParam::optional(
                    "limit",
                    ParamType::Integer,
                    "Maximum rows to return (1 to 200).",
                ),
                ActionParam::optional(
                    "cursor",
                    ParamType::Text,
                    "Pagination cursor returned by an earlier listing.",
                ),
            ],
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_strictly_read_only_and_agent_scoped() {
        let offered = rows();
        let verbs: Vec<&str> = offered
            .iter()
            .filter_map(|entry| entry.action.as_ref())
            .map(|action| action.address.action.as_str())
            .collect();
        assert_eq!(verbs, [LIST, READ, LIST_IN_WORKSPACE]);
        for entry in &offered {
            let action = entry.action.as_ref().expect("action row");
            assert_eq!(
                (entry.provider.as_str(), action.address.resource.as_str()),
                (PROVIDER, MINE)
            );
            assert_eq!(action.contract.kind, ActionKind::Read);
            assert!(action.contract.reaches_agents() && !action.contract.reaches_people());
        }
        let read_action = offered[1].action.as_ref().expect("read action");
        assert!(
            read_action
                .params
                .iter()
                .any(|param| param.name == "workspace" && param.required)
        );
        assert!(
            read_action
                .params
                .iter()
                .any(|param| param.name == "path" && param.required)
        );
        let list_in_action = offered[2]
            .action
            .as_ref()
            .expect("list-in-workspace action");
        assert!(
            list_in_action
                .params
                .iter()
                .any(|param| param.name == "workspace" && param.required)
        );
        assert!(
            list_in_action
                .params
                .iter()
                .any(|param| param.name == "path" && !param.required)
        );
    }

    #[test]
    fn validate_path_accepts_valid_paths_and_rejects_unsafe_ones() {
        for valid in [
            "notes",
            "notes/Todo",
            "word-processor/Document",
            "chat/Messages",
            "so.tilo.notes/Q3: Plan",
            "notes@w1/Todo",
        ] {
            assert_eq!(validate_path(valid).as_deref(), Ok(valid), "{valid}");
        }
        for invalid in [
            "",
            "   ",
            "/notes/Todo",
            "notes/Todo/",
            "notes//Todo",
            "../secret",
            "notes/../secret",
            "notes/./Todo",
            ".hidden/Todo",
            "notes\\Todo",
            "notes/Todo#delete",
            "word-processor#append-paragraph",
            "raw:so.tilo.chat:123",
            "notes/\0Todo",
            "notes/\nTodo",
        ] {
            assert!(validate_path(invalid).is_err(), "must reject: {invalid:?}");
        }
        let too_long = format!("notes/{}", "a".repeat(MAX_PATH_BYTES));
        assert!(validate_path(&too_long).is_err());
    }

    #[test]
    fn destination_policy_boundaries_fail_closed_for_stricter_sources() {
        let local = DestinationPolicy::NoneOutsideServer;
        let any = DestinationPolicy::Any;
        let only_server = DestinationPolicy::Only(vec![AllowedDestination::ThisServer]);
        let only_anthropic =
            DestinationPolicy::Only(vec![AllowedDestination::Provider("Anthropic".to_owned())]);
        let only_anthropic_lower =
            DestinationPolicy::Only(vec![AllowedDestination::Provider("anthropic".to_owned())]);
        let only_openai =
            DestinationPolicy::Only(vec![AllowedDestination::Provider("openai".to_owned())]);
        let server_or_anthropic = DestinationPolicy::Only(vec![
            AllowedDestination::ThisServer,
            AllowedDestination::Provider("anthropic".to_owned()),
        ]);
        let none = DestinationPolicy::Only(Vec::new());

        assert!(destination_allows(&any, &any));
        assert!(destination_allows(&any, &local));
        assert!(destination_allows(&any, &only_openai));

        assert!(destination_allows(&local, &local));
        assert!(destination_allows(&local, &only_server));
        assert!(!destination_allows(&local, &any));
        assert!(!destination_allows(&local, &server_or_anthropic));

        assert!(destination_allows(&only_anthropic, &only_anthropic_lower));
        assert!(!destination_allows(&only_anthropic, &any));
        assert!(!destination_allows(&only_anthropic, &only_openai));
        assert!(!destination_allows(&only_anthropic, &local));

        assert!(destination_allows(&server_or_anthropic, &local));
        assert!(destination_allows(&server_or_anthropic, &only_server));
        assert!(destination_allows(
            &server_or_anthropic,
            &only_anthropic_lower
        ));
        assert!(!destination_allows(&server_or_anthropic, &any));
        assert!(!destination_allows(&server_or_anthropic, &only_openai));

        assert!(!destination_allows(&none, &any));
        assert!(!destination_allows(&none, &local));
        assert!(!destination_allows(&none, &none));
    }

    #[test]
    fn reading_only_denies_all_writes_and_outside_effects() {
        let rules = reading_only("00000000-0000-0000-0000-000000000001");
        assert_eq!(rules.len(), 1);
        let rule = &rules[0];
        assert_eq!(rule.answer, tilo_action_policy::Answer::Deny);
        assert!(
            rule.kinds
                .contains(tilo_action_policy::ActionKind::Undoable)
        );
        assert!(rule.kinds.contains(tilo_action_policy::ActionKind::Outside));
        assert!(!rule.kinds.contains(tilo_action_policy::ActionKind::Read));
    }

    #[test]
    fn auxiliary_validators_enforce_bounds_and_kinds() {
        assert_eq!(validate_list_kind("resources"), Ok("resources"));
        assert_eq!(validate_list_kind(" actions "), Ok("actions"));
        assert!(validate_list_kind("writes").is_err());

        assert_eq!(validate_bound("limit", 1, MAX_LIST_LIMIT), Ok(1));
        assert_eq!(
            validate_bound("limit", MAX_LIST_LIMIT, MAX_LIST_LIMIT),
            Ok(MAX_LIST_LIMIT)
        );
        assert!(validate_bound("limit", 0, MAX_LIST_LIMIT).is_err());
        assert!(validate_bound("limit", MAX_LIST_LIMIT + 1, MAX_LIST_LIMIT).is_err());

        assert_eq!(validate_cursor("page:2").as_deref(), Ok("page:2"));
        assert!(validate_cursor("").is_err());
        assert!(validate_cursor("bad\ncursor").is_err());
        assert!(validate_cursor(&"x".repeat(MAX_CURSOR_BYTES + 1)).is_err());
    }
}
