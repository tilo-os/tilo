//! What an agent acting for a person may do of a workspace's governance and of
//! the server's own operations: the grants naming it in the workspace decide, at
//! the node an app's action of the same provider, kind and verb has
//! ([`AnswerScope::action`]), as for any action of an app. Its person sets them
//! for it as for any other action. Without one, the agent's delegation
//! answers ([`tilo_action_policy::Acting::handed_preset`]), as it does where no
//! workspace is asked about: reading is allowed,
//! and whatever changes something asks the person first, on their approving
//! devices ([`crate::workspaces::AsksPeople`]). The person's own checks follow,
//! the same their own page gets ([`Workspaces::authorize`]), so an agent never
//! does more than its person may.
//!
//! The person acting themselves is the person here, and so is a key they made
//! for their own programs ([`tilo_user_identity::DeviceCapabilities::server_key`]).

use std::sync::Arc;

use serde_json::Value;
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::roles::{GOVERNANCE_KIND, Rule, governance};
use tilo_action_policy::{ActionKind, Answer, Context, Grant, Grantee, Party, WorkspacePolicy};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_server_protocol::workspaces::WorkspacesRequest;
use tilo_server_session::workspaces::{CREATE_WORKSPACE, ENTER_WORKSPACE, LIST_WORKSPACES};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::WorkspaceAgents;

use super::super::authority::WorkspaceOp;
use super::Workspaces;

/// How long the person has to answer.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

/// One action of a workspace's governance or of the server's own operations, as the
/// grants deciding an agent's call of it name it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Operation<'a> {
    /// The provider publishing it: the workspaces' or the server's own.
    pub provider: &'a str,
    /// The kind of resource it is on.
    pub kind: &'a str,
    pub action: &'a str,
    /// Reading, an undoable change or one that reaches beyond the document.
    pub effect: ActionKind,
    /// What the person reads it as when they are asked.
    pub label: &'a str,
}

/// Whether `caller`, on the device `certificate` certifies, is that device's
/// person in `workspace` (the account where `None`): acting themselves, or
/// through a key they made for their own programs that reaches it. A key of
/// theirs for one workspace is them there and nowhere else.
pub(crate) fn is_the_person(
    caller: &Caller,
    certificate: &DeviceCert,
    workspace: Option<WorkspaceId>,
) -> bool {
    let reaches = match workspace {
        Some(workspace) => certificate.capabilities.own_key_in(&workspace.to_string()),
        None => certificate.capabilities.own_key_in("all"),
    };
    caller.acting().is_person()
        || (reaches && caller.device_public_key() == Some(&certificate.device_public_key))
}

/// Whether `caller` acts for the person of its device: that person, or an
/// agent of theirs. A bot acts for nobody here.
pub(crate) fn acts_for_its_person(caller: &Caller) -> bool {
    caller.acting().is_person() || caller.acting().is_agent()
}

/// What decides an agent's calls in one workspace: the grants there naming it,
/// the bound the workspace's list of its agents keeps for it, as its person set
/// it under Agents, and its delegation. Asked in no workspace, or in one not
/// governed here, its delegation alone.
pub(crate) struct AgentGrants {
    /// The agent; `None` may do nothing here.
    agent: Option<String>,
    workspace: String,
    policy: Option<Arc<WorkspacePolicy>>,
    granted: Vec<Grant>,
}

impl AgentGrants {
    const NONE: Self = Self {
        agent: None,
        workspace: String::new(),
        policy: None,
        granted: Vec::new(),
    };

    /// The answer for `operation` at `now`, Unix nanoseconds.
    pub(crate) fn answer(&self, operation: &Operation<'_>, now: u64) -> Answer {
        let Some(agent) = &self.agent else {
            return Answer::Deny;
        };
        let node = AnswerScope::action(operation.provider, operation.kind, operation.action)
            .node(&self.workspace);
        let cx = Context::at_time(super::super::system_time(now));
        self.policy
            .as_deref()
            .map_or_else(
                || Party::new(vec![Grantee::agent(agent)], &[]),
                |policy| policy.party_of(Grantee::agent(agent)),
            )
            .with_grants(&self.granted)
            .decide(&node, operation.effect, &cx)
            .answer
    }
}

impl Workspaces {
    /// What decides `caller`'s agent's calls in the workspace `workspace_id`
    /// carries. An agent of a device the workspace does not hold may do
    /// nothing there.
    pub(crate) async fn agent_grants(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
    ) -> AgentGrants {
        let acting = caller.acting();
        let Some(agent) = acting.agent_id() else {
            return AgentGrants::NONE;
        };
        let governed = match workspace_id {
            Some(id) => self.held(id).await,
            None => None,
        };
        let (Some(governed), Some(device)) = (governed, caller.device_public_key()) else {
            return AgentGrants {
                agent: Some(agent.to_owned()),
                workspace: String::new(),
                policy: None,
                granted: acting.handed_preset(""),
            };
        };
        let now = tilo_clock::unix_now_ns();
        let member = governed
            .lock()
            .await
            .peer()
            .permissions(*device, super::super::system_time(now));
        let Ok(member) = member else {
            return AgentGrants::NONE;
        };
        let workspace = member.policy.workspace.clone();
        let mut granted = acting.handed_preset(&workspace);
        let kept = self.kept_bound(&workspace, agent).await;
        granted.extend(caller.granted(&kept.unwrap_or_default()));
        AgentGrants {
            agent: Some(agent.to_owned()),
            workspace,
            policy: Some(member.policy),
            granted,
        }
    }

    /// The resource `document` of `app` in `workspace` as this server holds it,
    /// read as `R`; `None` where the workspace has none.
    ///
    /// # Errors
    /// The workspace is not governed here, or the resource does not read as `R`.
    pub async fn read_resource<R: tilo_resource::Resource>(
        &self,
        workspace: &str,
        route: (&str, &str),
    ) -> Result<Option<R::State>, String> {
        self.read_for_registration::<R>(workspace, route, None)
            .await
    }

    async fn read_for_registration<R: tilo_resource::Resource>(
        &self,
        workspace: &str,
        (app, document): (&str, &str),
        computer: Option<&str>,
    ) -> Result<Option<R::State>, String> {
        let Some(bytes) = self
            .resource_document(workspace, app, document)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        #[cfg(all(feature = "agents", not(target_arch = "wasm32")))]
        if R::KIND == tilo_workspace_agent::KIND
            && collab_types::workspace_document(app, document)
                .is_some_and(|known| known.kind == tilo_workspace_agent::KIND)
        {
            let mut roster =
                crate::agents::agent_record::open(self, workspace, &bytes, computer).await?;
            return crate::agents::opened::<R>(&mut roster).map(Some);
        }
        #[cfg(not(all(feature = "agents", not(target_arch = "wasm32"))))]
        let _ = computer;
        read_resource_bytes::<R>(&bytes).map(Some)
    }

    /// What the agent `agent` is held to in `workspace`, as the workspace's
    /// list of its agents keeps it: the bound kept for it there, and nothing
    /// at all once it was retired. A list this server cannot read holds it to
    /// nothing: what it may do is never more than the list says. `None` while
    /// the list does not name it.
    pub async fn kept_bound(&self, workspace: &str, agent: &str) -> Option<Vec<Rule>> {
        self.bound_for_registration(workspace, agent, None).await
    }

    pub(crate) async fn bound_for_registration(
        &self,
        workspace: &str,
        agent: &str,
        computer: Option<&str>,
    ) -> Option<Vec<Rule>> {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let kept = match self
            .read_for_registration::<WorkspaceAgents>(
                workspace,
                (tilo_workspace_agent::OWNER, &list),
                computer,
            )
            .await
        {
            Ok(agents) => agents?
                .agents
                .find(&tilo_resource::Id::from_raw(agent.to_owned()))
                .cloned()?,
            Err(error) => {
                tracing::warn!(%workspace, %agent, %error, "the workspace's agents could not be read: the agent may do nothing");
                return Some(nothing(workspace));
            }
        };
        Some(if kept.is_active() {
            kept.turn_rules(workspace)
        } else {
            nothing(workspace)
        })
    }

    /// What `workspace`'s list of its agents calls the agent `agent`, when it names it: the name
    /// its person reads, which a question it asks is put under. `None` while the list does not
    /// name it, or cannot be read.
    pub async fn agent_name(&self, workspace: &str, agent: &str) -> Option<String> {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let agents = self
            .read_resource::<WorkspaceAgents>(workspace, (tilo_workspace_agent::OWNER, &list))
            .await
            .ok()??;
        let named = agents
            .agents
            .find(&tilo_resource::Id::from_raw(agent.to_owned()))?;
        let name = named.name.trim();
        (!name.is_empty()).then(|| name.to_owned())
    }

    /// Let `caller`, on the device `certificate` certifies, take `operation`
    /// in the workspace `workspace_id` carries (`None`: asked of the server
    /// itself), with `arguments`: at once for its person, and for their agent
    /// as its grants answer, asking the person first where they say so.
    ///
    /// # Errors
    /// A bot, an agent its grants refuse, or one its person did not allow.
    pub(crate) async fn admit_operation(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        certificate: &DeviceCert,
        operation: &Operation<'_>,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<(), ActionCallError> {
        if is_the_person(caller, certificate, workspace_id) {
            return Ok(());
        }
        let Some(agent) = caller.acting().agent_id() else {
            return Err(ActionCallError::UnknownAction);
        };
        let now = tilo_clock::unix_now_ns();
        match self
            .agent_grants(workspace_id, caller)
            .await
            .answer(operation, now)
        {
            Answer::Allow => Ok(()),
            Answer::Deny => Err(ActionCallError::Refused {
                reason: format!(
                    "this agent may not {} here; its person allows it in the workspace's \
                     settings, under Agents",
                    operation.label.to_lowercase()
                ),
            }),
            Answer::Ask => {
                let place = match workspace_id {
                    Some(id) => self.workspace_name(id).await,
                    None => None,
                };
                let question = PendingConfirmation {
                    id: question_id(),
                    agent_id: agent.to_owned(),
                    agent_label: caller
                        .agent_name()
                        .unwrap_or(&certificate.device_label)
                        .to_owned(),
                    title: String::new(),
                    action_label: operation.label.to_lowercase(),
                    resource_label: place.clone().unwrap_or_else(|| "your workspaces".into()),
                    arguments: arguments
                        .iter()
                        .map(|(name, value)| {
                            let shown = value
                                .as_str()
                                .map_or_else(|| value.to_string(), str::to_owned);
                            (name.clone(), shown)
                        })
                        .collect(),
                    risk: "privileged".into(),
                    kind: "outside".into(),
                    undoable: false,
                    workspace: workspace_id.map(|id| id.to_string()).unwrap_or_default(),
                    expires_at_ns: now.saturating_add(QUESTION_WAIT_NS),
                    page: None,
                    ask: None,
                    allow_always: false,
                };
                self.ask_person(certificate.user_public_key, question).await
            }
        }
    }

    /// Put `question` to `person` on their approving devices and wait for
    /// their answer: only their allowing it lets what it asks about happen.
    /// With nobody to ask through, and for a question nobody answered, their
    /// confirmation is still wanted.
    ///
    /// # Errors
    /// The person did not allow it, or was not asked.
    pub(crate) async fn ask_person(
        &self,
        person: [u8; 32],
        question: PendingConfirmation,
    ) -> Result<(), ActionCallError> {
        // Whoever waits for this invocation hears now that it waits for its person, not later
        // that it took long.
        let _ = ASKING.try_with(|told| told.notify_one());
        let asker = self
            .inner
            .asks_people
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(ActionCallError::confirmation_required)?;
        match asker.ask(person, question).await {
            Ok(ConfirmationAnswer::AllowOnce) => Ok(()),
            Ok(
                ConfirmationAnswer::Deny
                | ConfirmationAnswer::Reply(_)
                | ConfirmationAnswer::Secret(_)
                | ConfirmationAnswer::AllowAlways,
            ) => Err(ActionCallError::Refused {
                reason: "the person did not allow it".into(),
            }),
            Err(_) => Err(ActionCallError::confirmation_required()),
        }
    }
}

/// Every action anywhere in `workspace` refused: what a retired agent may do.
fn nothing(workspace: &str) -> Vec<Rule> {
    vec![Rule::new(
        tilo_action_policy::TreePath::workspace(workspace).rest(),
        tilo_action_policy::KindSet::ALL,
        Answer::Deny,
    )]
}

tokio::task_local! {
    /// Told when the invocation of this task puts a question to its person
    /// ([`Workspaces::ask_person`]): its caller then hears it waits for their approval.
    pub(crate) static ASKING: std::sync::Arc<tokio::sync::Notify>;
}

/// A fresh question's id.
pub(crate) fn question_id() -> [u8; 32] {
    let mut id = [0_u8; 32];
    for half in id.chunks_mut(16) {
        half.copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    }
    id
}

/// Each request a page asks ([`WorkspacesRequest`]), by its name, as the
/// action asked of the server itself (`true`) or of the workspaces it is: an agent's grant
/// for the one is its grant for the other.
const PAGE_ACTIONS: [(&str, bool, &str); 20] = [
    ("List", true, LIST_WORKSPACES),
    ("Create", true, CREATE_WORKSPACE),
    ("Place", true, CREATE_WORKSPACE),
    ("Enter", true, ENTER_WORKSPACE),
    ("Invite", false, governance::INVITE),
    ("PreAdmit", false, governance::INVITE),
    ("Cancel", false, super::CANCEL_INVITATION),
    ("Pending", false, super::INVITATIONS),
    ("WaitList", false, super::WAIT_LIST),
    ("Unlist", false, super::UNLIST),
    ("MailInvite", false, super::MAIL_INVITATION),
    ("Notices", false, super::MEMBERS),
    ("Permits", false, super::MEMBERS),
    ("Name", false, super::NAME),
    ("Rename", false, governance::RENAME),
    ("Share", false, governance::SETTINGS),
    ("Unshare", false, governance::SETTINGS),
    ("Servers", false, super::SERVERS),
    ("AddServer", false, super::ADD_SERVER),
    ("RemoveServer", false, super::REMOVE_SERVER),
];

/// The action `action` of the server itself (`own`) or of the workspaces, as the grants
/// deciding an agent's call of it name it: as its verb is published, and
/// what no verb publishes as the workspace's governance all the same.
fn operation_of(own: bool, action: &'static str) -> Operation<'static> {
    let (publisher, verb) = if own {
        let verb = super::host::ACCOUNT_VERBS
            .iter()
            .map(|(verb, _)| verb)
            .find(|verb| verb.id == action);
        (super::host::ACCOUNT, verb)
    } else {
        let verb = super::VERBS.iter().find(|verb| verb.id == action);
        (super::GOVERNANCE, verb)
    };
    verb.map_or(
        Operation {
            provider: publisher.provider,
            kind: publisher.kind,
            action,
            effect: GOVERNANCE_KIND,
            label: action,
        },
        |verb| verb.operation(publisher),
    )
}

impl Workspaces {
    /// Let `caller`, on `device`, have the page request `request` answered:
    /// what the same action asked of the workspaces or the server would let it
    /// ([`Self::admit_operation`]).
    ///
    /// # Errors
    /// Why not, as the page reads a refusal.
    pub async fn admit_page(
        &self,
        device: &DeviceCert,
        caller: &Caller,
        request: &WorkspacesRequest,
    ) -> Result<(), String> {
        // The request is its name, with its fields under it: what the person
        // reads of it is its text, and the people it names by their key.
        let asked = serde_json::to_value(request).unwrap_or_default();
        let (name, fields) = match &asked {
            Value::Object(asked) => asked
                .iter()
                .next()
                .map_or(("", Value::Null), |(name, fields)| {
                    (name.as_str(), fields.clone())
                }),
            Value::String(name) => (name.as_str(), Value::Null),
            _ => ("", Value::Null),
        };
        let workspace_id = fields
            .get("workspace")
            .or_else(|| fields.pointer("/grant/workspace"))
            .and_then(Value::as_str)
            .and_then(WorkspaceId::parse);
        if is_the_person(caller, device, workspace_id) {
            return Ok(());
        }
        let Some(&(_, own, action)) = PAGE_ACTIONS.iter().find(|(page, ..)| *page == name) else {
            return Err(format!("an agent does not ask for {name} here"));
        };
        // What the person's own checks refuse is refused before they are
        // asked about it.
        self.check_page(device, request)
            .await
            .map_err(|refused| refused.to_string())?;
        let operation = operation_of(own, action);
        let arguments = fields
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(name, value)| match value {
                Value::String(_) => Some((name.clone(), value.clone())),
                Value::Array(bytes) if bytes.len() == 32 && name.ends_with("public_key") => {
                    let key: Vec<u8> = bytes
                        .iter()
                        .filter_map(|byte| u8::try_from(byte.as_u64()?).ok())
                        .collect();
                    Some((name.clone(), Value::String(hex::encode(key))))
                }
                _ => None,
            })
            .collect();
        self.admit_operation(workspace_id, caller, device, &operation, &arguments)
            .await
            .map_err(|refused| refused.to_string())
    }
}

/// Making a workspace, as a page asks for it.
const CREATING: WorkspaceOp<'static> =
    WorkspaceOp::Host(crate::service_authority::ServiceOperation::CreateWorkspace);

impl Workspaces {
    /// The person's own checks of the page request `request` by `device`, as
    /// the request makes them when it is carried out:
    /// the role an invitation gives, making a workspace, and the governance
    /// action an invitation's or the workspace's name's change is decided by.
    async fn check_page(
        &self,
        device: &DeviceCert,
        request: &WorkspacesRequest,
    ) -> Result<(), super::super::CollaborationError> {
        use WorkspacesRequest as Asked;
        let (workspace, op) = match request {
            Asked::Invite {
                workspace,
                role,
                administrator,
                ..
            } => (Some(workspace), WorkspaceOp::invite(*role, *administrator)),
            Asked::Create { .. } | Asked::Place { .. } => (None, CREATING),
            Asked::Cancel { workspace, .. }
            | Asked::Unlist { workspace, .. }
            | Asked::MailInvite { workspace, .. } => {
                (Some(workspace), WorkspaceOp::Govern(governance::INVITE))
            }
            Asked::Rename { workspace, .. } => {
                (Some(workspace), WorkspaceOp::Govern(governance::RENAME))
            }
            _ => return Ok(()),
        };
        self.check_now(device, workspace.map(String::as_str), op)
            .await
    }
}

#[cfg(test)]
pub(super) mod tests;

pub(in crate::workspaces) fn read_resource_bytes<R: tilo_resource::Resource>(
    bytes: &[u8],
) -> Result<R::State, String> {
    let mut doc = automerge::AutoCommit::load(bytes).map_err(|error| error.to_string())?;
    tilo_resource::evolution::migrate::<R>(&mut doc)
        .map_err(|error| format!("bring the resource up to version {}: {error}", R::VERSION))?;
    R::read(&doc).map_err(|error| error.to_string())
}
