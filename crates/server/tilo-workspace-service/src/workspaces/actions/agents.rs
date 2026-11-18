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

use serde_json::Value;
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::roles::{GOVERNANCE_KIND, governance};
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, WorkspacePolicy};
use tilo_actions::remote::{ActionCallError, Caller};
use tilo_server_protocol::WorkspaceId;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_server_protocol::workspaces::WorkspacesRequest;
use tilo_server_session::workspaces::{CREATE_WORKSPACE, ENTER_WORKSPACE, LIST_WORKSPACES};
use tilo_user_identity::DeviceCert;

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
/// person: acting themselves, or through a key they made for their own
/// programs.
pub(crate) fn is_the_person(caller: &Caller, certificate: &DeviceCert) -> bool {
    caller.acting().is_person()
        || (certificate.capabilities.server_key
            && caller.device_public_key() == Some(&certificate.device_public_key))
}

/// Whether `caller` acts for the person of its device: that person, or an
/// agent of theirs. A bot acts for nobody here.
pub(crate) fn acts_for_its_person(caller: &Caller) -> bool {
    caller.acting().is_person() || caller.acting().is_agent()
}

/// The answer the grants of the workspace `policy` governs give `caller`'s
/// agent for `operation` at `now`, Unix nanoseconds; without a workspace,
/// what its delegation alone answers.
pub(crate) fn agent_answer(
    policy: Option<&WorkspacePolicy>,
    caller: &Caller,
    operation: &Operation<'_>,
    now: u64,
) -> Answer {
    let acting = caller.acting();
    let Some(agent) = acting.agent_id() else {
        return Answer::Deny;
    };
    let workspace = policy.map_or("", |policy| policy.workspace.as_str());
    let granted = acting.handed_preset(workspace);
    let node =
        AnswerScope::action(operation.provider, operation.kind, operation.action).node(workspace);
    let cx = Context::at_time(super::super::system_time(now));
    policy
        .map_or_else(
            || Party::new(vec![Grantee::agent(agent)], &[]),
            |policy| policy.party_of(Grantee::agent(agent)),
        )
        .with_grants(&granted)
        .decide(&node, operation.effect, &cx)
        .answer
}

impl Workspaces {
    /// The answer for `caller`'s agent's `operation`, by the grants of the
    /// workspace `workspace_id` carries. Asked in no workspace, or in one not
    /// governed here, no grant names the agent and its delegation answers.
    /// An agent of a device the workspace does not hold may do nothing there.
    pub(crate) async fn agent_answer_in(
        &self,
        workspace_id: Option<WorkspaceId>,
        caller: &Caller,
        operation: &Operation<'_>,
    ) -> Answer {
        let now = tilo_clock::unix_now_ns();
        let governed = match workspace_id {
            Some(id) => self.held(id).await,
            None => None,
        };
        let (Some(governed), Some(device)) = (governed, caller.device_public_key()) else {
            return agent_answer(None, caller, operation, now);
        };
        let member = governed
            .lock()
            .await
            .peer()
            .permissions(*device, super::super::system_time(now));
        match member {
            Ok(member) => agent_answer(Some(&*member.policy), caller, operation, now),
            Err(_) => Answer::Deny,
        }
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
        if is_the_person(caller, certificate) {
            return Ok(());
        }
        let Some(agent) = caller.acting().agent_id() else {
            return Err(ActionCallError::UnknownAction);
        };
        match self.agent_answer_in(workspace_id, caller, operation).await {
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
                    expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
                    page: None,
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
        let asker = self
            .inner
            .asks_people
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
            .ok_or_else(ActionCallError::confirmation_required)?;
        match asker.ask(person, question).await {
            Ok(ConfirmationAnswer::AllowOnce) => Ok(()),
            Ok(ConfirmationAnswer::Deny) => Err(ActionCallError::Refused {
                reason: "the person did not allow it".into(),
            }),
            Err(_) => Err(ActionCallError::confirmation_required()),
        }
    }
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
const PAGE_ACTIONS: [(&str, bool, &str); 24] = [
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
    ("SetRole", false, super::ADD_MEMBER),
    ("Remove", false, governance::REMOVE_MEMBER),
    ("Ban", false, governance::BAN),
    ("Unban", false, governance::UNBAN),
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
        let verb = super::host::HOST_VERBS
            .iter()
            .map(|(verb, _)| verb)
            .find(|verb| verb.id == action);
        (super::host::HOST, verb)
    } else {
        let verb = super::VERBS
            .iter()
            .chain(&super::OWNER_CONFIRMED)
            .find(|verb| verb.id == action);
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
        if is_the_person(caller, device) {
            return Ok(());
        }
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
        let Some(&(_, own, action)) = PAGE_ACTIONS.iter().find(|(page, ..)| *page == name) else {
            return Err(format!("an agent does not ask for {name} here"));
        };
        let operation = operation_of(own, action);
        let workspace_id = fields
            .get("workspace")
            .or_else(|| fields.pointer("/grant/workspace"))
            .and_then(Value::as_str)
            .and_then(WorkspaceId::parse);
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

#[cfg(test)]
pub(super) mod tests;
