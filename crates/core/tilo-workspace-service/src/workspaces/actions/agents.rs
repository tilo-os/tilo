//! What an agent acting for a person may do of a workspace's governance and of
//! the server's own operations: the grants naming it in the workspace decide, at
//! the node an app's action of the same provider, kind and verb has
//! ([`AnswerScope::action`]), as for any action of an app. Its person sets them
//! for it as for any other action. Without one, the agent's delegation
//! answers ([`tilo_action_policy::Acting::handed_preset`]), as it does where no
//! workspace is asked about: reading is allowed,
//! and whatever changes something waits for the person's yes, as the call's
//! asked record. The person's own checks follow,
//! the same their own page gets ([`Workspaces::authorize`]), so an agent never
//! does more than its person may.
//!
//! The person acting themselves is the person here, and so is a key they made
//! for their own programs ([`tilo_user_identity::DeviceCapabilities::server_key`]).

use std::sync::Arc;

use serde_json::Value;
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::roles::{GOVERNANCE_KIND, governance};
use tilo_action_policy::{ActionKind, Answer, Context, Grant, Grantee, Party, WorkspacePolicy};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::workspaces::WorkspacesRequest;
use tilo_server_protocol::workspaces::{CREATE_WORKSPACE, ENTER_WORKSPACE, LIST_WORKSPACES};
use tilo_user_identity::DeviceCert;
use tilo_workspace_agent::WorkspaceAgents;

use super::super::authority::WorkspaceOp;
use super::Workspaces;

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

/// Whether the certificate `certificate`, which `caller`'s person signed for the agent acting
/// through it, grants it `operation` of the server itself (`workspace` `None`): their signature
/// is their yes, so the agent is not asked again for it. An agent's grant that reaches all of
/// its person's workspaces makes workspaces ([`tilo_user_identity::AgentScope::makes_workspaces`]);
/// no certificate grants anything else of the server's own operations, which still ask.
fn certificate_grants(
    caller: &Caller,
    certificate: &DeviceCert,
    workspace: Option<WorkspaceId>,
    operation: &Operation<'_>,
) -> bool {
    let its_own = caller.device_public_key() == Some(&certificate.device_public_key)
        && caller.user_public_key() == Some(&certificate.user_public_key)
        && certificate.capabilities.agent_grant_holds();
    let makes = matches!(
        operation.action,
        tilo_server_protocol::workspaces::CREATE_OWN_WORKSPACE | CREATE_WORKSPACE
    ) && certificate
        .capabilities
        .agent
        .is_some_and(|scope| scope.makes_workspaces());
    workspace.is_none() && its_own && makes
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
    /// The person the agent acts for, as its certificate chains to: an agent is the pair, and
    /// only its person's grants reach it.
    person: String,
    workspace: String,
    policy: Option<Arc<WorkspacePolicy>>,
    granted: Vec<Grant>,
}

impl AgentGrants {
    const NONE: Self = Self {
        agent: None,
        person: String::new(),
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
                |policy| policy.party_of_agent(agent, &self.person),
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
        let person = acting.acted_for_person().unwrap_or_default().to_owned();
        let governed = match workspace_id {
            Some(id) => self.held(id).await,
            None => None,
        };
        let (Some(governed), Some(device)) = (governed, caller.device_public_key()) else {
            return AgentGrants {
                agent: Some(agent.to_owned()),
                person,
                workspace: String::new(),
                policy: None,
                granted: [acting.handed_preset(""), caller.granted()].concat(),
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
        granted.extend(caller.granted());
        AgentGrants {
            agent: Some(agent.to_owned()),
            person,
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
        (app, document): (&str, &str),
    ) -> Result<Option<R::State>, String> {
        let Some(bytes) = self
            .resource_document(workspace, app, document)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(None);
        };
        read_resource_bytes::<R>(&bytes).map(Some)
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

    /// The agents that work in `workspace`, as its list of its agents names them, each with the
    /// person it acts for: whom a broker admits as an agent there. None while the list does not
    /// exist or cannot be read.
    #[cfg(feature = "apps")]
    pub async fn listed_agents(&self, workspace: &str) -> Vec<crate::apps::services::ListedAgent> {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        let Ok(Some(agents)) = self
            .read_resource::<WorkspaceAgents>(workspace, (tilo_workspace_agent::OWNER, &list))
            .await
        else {
            return Vec::new();
        };
        agents
            .active()
            .map(|agent| crate::apps::services::ListedAgent {
                id: agent.id.0.as_str().to_owned(),
                name: agent.name.clone(),
                avatar: agent.avatar.clone(),
                acts_for: agent.acts_for.clone(),
            })
            .collect()
    }

    /// Whether the agent `agent` still works in `workspace`: the workspace's list of its
    /// agents names it and it is not retired.
    pub async fn agent_is_active(&self, workspace: &str, agent: &str) -> bool {
        let list = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
        matches!(
            self.read_resource::<WorkspaceAgents>(
                workspace,
                (tilo_workspace_agent::OWNER, &list)
            )
            .await,
            Ok(Some(agents))
                if agents
                    .agents
                    .find(&tilo_resource::Id::from_raw(agent.to_owned()))
                    .is_some_and(|kept| kept.is_active())
        )
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
        (arguments, approved): (&serde_json::Map<String, Value>, bool),
    ) -> Result<(), ActionCallError> {
        if is_the_person(caller, certificate, workspace_id) {
            return Ok(());
        }
        if caller.acting().agent_id().is_none() {
            return Err(ActionCallError::UnknownAction);
        }
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
            Answer::Ask
                if approved || certificate_grants(caller, certificate, workspace_id, operation) =>
            {
                Ok(())
            }
            Answer::Ask => {
                let place = match workspace_id {
                    Some(id) => self.workspace_name(id).await,
                    None => None,
                };
                let shown = |value: &Value| {
                    value
                        .as_str()
                        .map_or_else(|| value.to_string(), str::to_owned)
                };
                Err(ActionCallError::asks(AskedFacts {
                    on: Some(place.unwrap_or_else(|| "your workspaces".into())),
                    facts: arguments
                        .iter()
                        .map(|(name, value)| Fact::new(name.clone(), shown(value)))
                        .collect(),
                    verb: operation.label.to_lowercase(),
                    ..AskedFacts::by(crate::workspaces::asking::asked_by(caller))
                }))
            }
        }
    }
}

/// Each request a page asks ([`WorkspacesRequest`]), by its name, as the
/// action asked of the server itself (`true`) or of the workspaces it is: an agent's grant
/// for the one is its grant for the other.
const PAGE_ACTIONS: [(&str, bool, &str); 20] = [
    ("List", true, LIST_WORKSPACES),
    ("Create", true, CREATE_WORKSPACE),
    ("Place", true, CREATE_WORKSPACE),
    ("Enter", true, ENTER_WORKSPACE),
    ("Invite", false, tilo_actions::remote::WORKSPACE_INVITE),
    ("PreAdmit", false, tilo_actions::remote::WORKSPACE_INVITE),
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
        self.admit_operation(
            workspace_id,
            caller,
            device,
            &operation,
            (&arguments, false),
        )
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
            } => (Some(workspace), WorkspaceOp::admit(*role, *administrator)),
            Asked::Create { .. } | Asked::Place { .. } => (None, CREATING),
            Asked::Cancel { workspace, .. }
            | Asked::Unlist { workspace, .. }
            | Asked::MailInvite { workspace, .. } => {
                (Some(workspace), WorkspaceOp::Govern(governance::ADD_MEMBER))
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
