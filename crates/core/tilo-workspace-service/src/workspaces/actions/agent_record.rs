//! A workspace's record of its agents (`tilo.workspace.agent`), changed only through the three
//! actions its governance tree publishes under `workspace/agents`.
//!
//! The record names who works in the workspace, who keeps each agent, and what each may spend: it
//! is the agent's admission, so every writer reaches it one way, over the daemon's dispatch
//! (`tilo-actions/1`) like any other call: make an agent ([`WORKSPACE_AGENT_CREATE`]), change one
//! ([`WORKSPACE_AGENT_UPDATE`]), retire one ([`WORKSPACE_AGENT_DELETE`]). Each is the record's own
//! action, admitted by the workspace's ordinary grants for whoever acts (a person, or an agent
//! for its person) on the record's node, applied as them, and kept in the workspace's store as
//! this device's write, as any record of a kind its app declares is.

use serde_json::{Map, Value, json};
use tilo_action_policy::{Acting, Answer, MemberAccess, TreePath};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueRow, InvokeArgument, InvokeRequest, WORKSPACE_AGENT_CHANGES,
    WORKSPACE_AGENT_CREATE, WORKSPACE_AGENT_DELETE, WORKSPACE_AGENT_UPDATE,
    workspace_agent_effects, workspace_governance_operation,
};
use tilo_actions::{ActionKind, ActionParam, ParamType};
use tilo_resource::{ActionDecl, Invoke, Replica, Resource as _};
use tilo_server_protocol::WorkspaceId;
use tilo_workspace_agent::{AgentPermissions, KIND, OWNER, WorkspaceAgents};

use super::{Verb, Workspaces};

/// The actions of a workspace's agents, in catalogue order. Each is decided by the record's own
/// grants ([`Workspaces::admit_agents_action`]), never by a governance check.
const VERBS: [Verb; 3] = [
    Verb {
        id: WORKSPACE_AGENT_CREATE,
        label: "Add an agent",
        check: "",
        kind: ActionKind::Undoable,
        params: create_params,
    },
    Verb {
        id: WORKSPACE_AGENT_UPDATE,
        label: "Change an agent",
        check: "",
        kind: ActionKind::Undoable,
        params: update_params,
    },
    Verb {
        id: WORKSPACE_AGENT_DELETE,
        label: "Retire an agent",
        check: "",
        kind: ActionKind::Undoable,
        params: delete_params,
    },
];

fn create_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required("name", ParamType::Text, "Its name, as people read it."),
        ActionParam::optional(
            "id",
            ParamType::Text,
            "Its id: one already in use elsewhere; the next free one for its name when omitted.",
        ),
        ActionParam::optional(
            "model",
            ParamType::Text,
            "The model it plans with, as provider/model; the workspace's default when omitted.",
        ),
        ActionParam::optional(
            "mcp_client",
            ParamType::Text,
            "The MCP client it connects as; none for an agent a device of the workspace runs.",
        ),
        ActionParam::optional(
            "moderates",
            ParamType::Boolean,
            "Whether it watches the workspace's chat, seeing every message a member posts, so a \
             Moderator can have it act on what is not allowed.",
        ),
    ]
}

fn update_params() -> Vec<ActionParam> {
    vec![
        ActionParam::required(
            "action",
            ParamType::Text,
            "The record's action that changes it, by its id (rename-agent, set-agent-model, \
             set-agent-spend-limit, record-agent-spend and the rest).",
        ),
        ActionParam::required(
            "arguments",
            ParamType::Object,
            "That action's arguments, the agent among them.",
        ),
    ]
}

fn delete_params() -> Vec<ActionParam> {
    vec![ActionParam::required(
        "agent",
        ParamType::Text,
        "The agent, by its id.",
    )]
}

/// Whether `operation` is one of the agents' actions.
pub(super) fn is_agents_operation(operation: &str) -> bool {
    VERBS.iter().any(|verb| verb.id == operation)
}

/// The record's own action an operation is decided as, for the catalogue: making, retiring, and
/// renaming, which stands for every change of one.
fn record_action(operation: &str) -> &'static str {
    match operation {
        WORKSPACE_AGENT_CREATE => WorkspaceAgents::CREATE_AGENT.id,
        WORKSPACE_AGENT_DELETE => WorkspaceAgents::RETIRE_AGENT.id,
        _ => WorkspaceAgents::RENAME_AGENT.id,
    }
}

/// The record's declaration of its action `id`.
fn declared(id: &str) -> Result<ActionDecl<WorkspaceAgents>, ActionCallError> {
    WorkspaceAgents::declarations()
        .into_iter()
        .find(|declared| declared.id == id)
        .ok_or_else(|| ActionCallError::InvalidRequest {
            reason: format!("the agents record has no action {id}"),
        })
}

#[expect(
    clippy::needless_pass_by_value,
    reason = "`map_err` hands each error over by value"
)]
fn unavailable(error: impl ToString) -> ActionCallError {
    ActionCallError::Unavailable {
        reason: error.to_string(),
    }
}

/// The record's id among the workspace's documents.
fn record_id() -> String {
    collab_types::workspace_document_id(KIND)
}

/// The JSON object `argument` names.
fn object(argument: &InvokeArgument) -> Result<Map<String, Value>, ActionCallError> {
    match argument {
        InvokeArgument::None => Ok(Map::new()),
        InvokeArgument::Json(object) => Ok(object.clone()),
        InvokeArgument::Text(_) => Err(ActionCallError::InvalidRequest {
            reason: "an agent's change takes named arguments".to_owned(),
        }),
    }
}

impl Workspaces {
    /// The rows of the agents' actions in `workspace` (named `name`) that the record's grants let
    /// `acting` take.
    pub(super) async fn agents_rows(
        &self,
        workspace: &str,
        name: &str,
        acting: &Acting,
    ) -> Vec<CatalogueRow> {
        let mut rows = Vec::new();
        for verb in &VERBS {
            if self
                .admit_agents_action(workspace, acting, record_action(verb.id))
                .await
                .unwrap_or(false)
            {
                let mut row = verb.governance_row(workspace, name);
                if let Some(descriptor) = row.action.as_mut() {
                    descriptor.contract.effects = workspace_agent_effects(verb.id);
                }
                rows.push(row);
            }
        }
        rows
    }

    /// Run the agents' action `request` names in `workspace_id` as `caller` acts: the workspace's
    /// record of its agents, changed on this device's copy of the workspace and decided by the
    /// record's grants, not by a governance check.
    pub async fn agents_invoke(
        &self,
        workspace_id: WorkspaceId,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let (workspace, operation) = workspace_governance_operation(&request.target.address)
            .ok_or(ActionCallError::UnknownAction)?;
        if workspace != workspace_id.to_string() {
            return Err(ActionCallError::UnknownAction);
        }
        let acting = caller.acting();
        let mut arguments = object(&request.argument)?;
        match operation {
            WORKSPACE_AGENT_CREATE => {
                Box::pin(self.create_agent((workspace, acting, request), arguments))
                    .await
                    .map(|agent| json!({ "agent": agent }))
            }
            WORKSPACE_AGENT_UPDATE => {
                let Some(Value::String(action)) = arguments.remove("action") else {
                    return Err(ActionCallError::InvalidRequest {
                        reason: "a change of an agent names the record's action".to_owned(),
                    });
                };
                // A change of an agent is one of the steps the action declares. Where an agent
                // waits or runs is written by the device that runs it, from the ticket it serves
                // and the certificate a redeemed ticket gave it: no change of an agent says it.
                if !WORKSPACE_AGENT_CHANGES.contains(&action.as_str()) {
                    return Err(ActionCallError::InvalidRequest {
                        reason: format!("{action} is not a change of an agent"),
                    });
                }
                let changed = match arguments.remove("arguments") {
                    Some(Value::Object(changed)) => changed,
                    None | Some(Value::Null) => Map::new(),
                    Some(_) => {
                        return Err(ActionCallError::InvalidRequest {
                            reason: "a change's arguments are named".to_owned(),
                        });
                    }
                };
                let invoke = declared(&action)?.with_arguments(changed);
                Box::pin(self.act_on_agents((workspace, acting, Some(request)), invoke))
                    .await
                    .map(|()| Value::Null)
            }
            WORKSPACE_AGENT_DELETE => {
                let Some(agent) = arguments
                    .get("agent")
                    .and_then(Value::as_str)
                    .filter(|agent| !agent.is_empty())
                    .map(str::to_owned)
                else {
                    return Err(ActionCallError::InvalidRequest {
                        reason: "retiring an agent names it".to_owned(),
                    });
                };
                let invoke = declared(WorkspaceAgents::RETIRE_AGENT.id)?.with_arguments(
                    Map::from_iter([("agent".to_owned(), Value::String(agent.clone()))]),
                );
                Box::pin(self.act_on_agents((workspace, acting, Some(request)), invoke)).await?;
                // Retiring an agent takes the triggers its keeper saved for it with it, so a
                // shortcut that still names a retired agent starts nothing.
                #[cfg(feature = "agents")]
                self.remove_agent_triggers(workspace, acting, &agent).await;
                Ok(Value::Null)
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Make an agent in `workspace` as `acting` asks: the one place an agent comes into a
    /// workspace, whoever asks for it. Answers the agent's id, which is `arguments`' `id` where the
    /// asker named one and the record's next free id for its name otherwise.
    async fn create_agent(
        &self,
        (workspace, acting, request): (&str, &Acting, &InvokeRequest),
        mut arguments: Map<String, Value>,
    ) -> Result<String, ActionCallError> {
        let named = match arguments.get("id") {
            Some(Value::String(id)) if !id.trim().is_empty() => Some(id.trim().to_owned()),
            _ => None,
        };
        let name = arguments
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_owned();
        // An agent a person's device runs answers triggers; one a workspace's host runs as an
        // MCP client is reached by its client's conversation, not by a saved shortcut.
        #[cfg(feature = "agents")]
        let answers_triggers = arguments
            .get("mcp_client")
            .and_then(Value::as_str)
            .is_none_or(|client| client.trim().is_empty());
        // A Moderator's ask for the chat watch, kept beside the agent's own triggers.
        let moderates = arguments.remove("moderates");
        #[cfg(feature = "agents")]
        let moderates = matches!(moderates, Some(Value::Bool(true)));
        #[cfg(not(feature = "agents"))]
        let _ = moderates;
        let id = if let Some(id) = named {
            id
        } else {
            let stored = self.agents_record(workspace).await?;
            let id = match stored.as_deref() {
                Some(bytes) => Replica::<WorkspaceAgents>::load(bytes)
                    .and_then(|record| record.state())
                    .map_err(unavailable)?
                    .free_id(&name),
                None => name.clone(),
            };
            arguments.insert("id".to_owned(), Value::String(id.clone()));
            id
        };
        let invoke = declared(WorkspaceAgents::CREATE_AGENT.id)?.with_arguments(arguments);
        self.act_on_agents((workspace, acting, Some(request)), invoke)
            .await?;
        // The agent's triggers are saved where it is made, once: whoever asks, and whichever
        // device of theirs it is, the person who keeps it has them from its first turn.
        // A service built without agents runs no triggers and saves none.
        #[cfg(feature = "agents")]
        if answers_triggers {
            self.save_default_triggers(workspace, acting, &id, &name)
                .await;
            if moderates {
                self.save_moderates(workspace, acting, &id, &name).await;
            }
        }
        Ok(id)
    }

    /// Save the triggers a new agent answers first (`triggered::triggers`) as `acting`'s person's,
    /// when it is this device's own person who keeps the agent: a shortcut is signed as the device
    /// that saves it, so another person's agent is theirs to save.
    #[cfg(feature = "agents")]
    async fn save_default_triggers(
        &self,
        workspace: &str,
        acting: &Acting,
        agent: &str,
        name: &str,
    ) {
        let Some(person) = acting.person_id().filter(|_| self.is_own_person(acting)) else {
            return;
        };
        let save = tilo_automation::ShortcutChange::Save(Box::new(
            crate::agents::triggered::triggers(agent, person, name),
        ));
        if let Err(error) = self.change_own_shortcuts(workspace, save).await {
            tracing::warn!(%workspace, %agent, %error, "a new agent's triggers were not saved");
        }
    }

    /// Save `agent`'s chat watch (`triggered::moderates`) as `acting`'s person's, when it is
    /// this device's own person who keeps the agent: it is one more shortcut of theirs, saved
    /// when they make the agent that watches the workspace's chat.
    #[cfg(feature = "agents")]
    async fn save_moderates(&self, workspace: &str, acting: &Acting, agent: &str, name: &str) {
        let Some(person) = acting.person_id().filter(|_| self.is_own_person(acting)) else {
            return;
        };
        let save = tilo_automation::ShortcutChange::Save(Box::new(
            crate::agents::triggered::moderates(agent, person, name),
        ));
        if let Err(error) = self.change_own_shortcuts(workspace, save).await {
            tracing::warn!(%workspace, %agent, %error, "an agent's chat watch was not saved");
        }
    }

    /// Take away the triggers saved for `agent`, once it is retired: the shortcut this device's
    /// person saved for it. Another person's agent is theirs to take away.
    #[cfg(feature = "agents")]
    async fn remove_agent_triggers(&self, workspace: &str, acting: &Acting, agent: &str) {
        if !self.is_own_person(acting) {
            return;
        }
        // Nothing was saved where the agent was reached by its client's conversation rather than
        // its triggers, or where another device of the person saved it.
        let saved: Vec<String> = self
            .described()
            .await
            .into_iter()
            .find(|held| held.id().to_string() == workspace)
            .map(|held| {
                held.shortcuts
                    .iter()
                    .map(|saved| saved.id().to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let its = [
            crate::agents::triggered::shortcut_id(agent),
            crate::agents::triggered::moderation_shortcut_id(agent),
        ];
        for shortcut in its.into_iter().filter(|its| saved.contains(its)) {
            let delete = tilo_automation::ShortcutChange::Delete(shortcut);
            if let Err(error) = self.change_own_shortcuts(workspace, delete).await {
                tracing::warn!(%workspace, %agent, %error, "a retired agent's triggers were not removed");
            }
        }
    }

    /// Whether `acting` is this device's own person: the root the device acts for. A shortcut is
    /// signed as the device that saves it, so only their own person's are this device's to save or
    /// take away.
    #[cfg(feature = "agents")]
    fn is_own_person(&self, acting: &Acting) -> bool {
        let Some(person) = acting.person_id() else {
            return false;
        };
        let own = self.signing_device().cert.user_public_key;
        match tilo_action_policy::PersonId::parse(person) {
            Ok(person) => person.root() == Some(own),
            Err(error) => {
                tracing::warn!(%error, "an agent's person is not a bare root key");
                false
            }
        }
    }

    /// Show `ticket` on `agent` of `workspace` as the one this device waits to run it with, as
    /// this device's own write, as its person: the agent's person redeems it to approve the key
    /// it names.
    ///
    /// # Errors
    /// The workspace's grants do not let this device's person change its agents, or the record
    /// did not take it.
    #[cfg(feature = "agents")]
    pub(crate) async fn wait_to_run_agent(
        &self,
        workspace: &str,
        agent: &str,
        ticket: &str,
    ) -> Result<(), ActionCallError> {
        self.runs_on_here(
            workspace,
            WorkspaceAgents::WAIT_TO_RUN_AGENT.id,
            agent,
            ("ticket", ticket.to_owned()),
        )
        .await
    }

    /// Keep `cert`, which the agent's person's root issued redeeming this device's ticket, on
    /// `agent` of `workspace`, as this device's own write, as its person: what it runs the agent
    /// under from now on, a restart included.
    ///
    /// # Errors
    /// As [`Self::wait_to_run_agent`], or the certificate does not encode.
    #[cfg(feature = "agents")]
    pub(crate) async fn run_agent_on(
        &self,
        workspace: &str,
        agent: &str,
        cert: &tilo_user_identity::DeviceCert,
    ) -> Result<(), ActionCallError> {
        let certificate = hex::encode(postcard::to_stdvec(cert).map_err(unavailable)?);
        self.runs_on_here(
            workspace,
            WorkspaceAgents::RUN_AGENT_ON.id,
            agent,
            ("certificate", certificate),
        )
        .await
    }

    #[cfg(feature = "agents")]
    async fn runs_on_here(
        &self,
        workspace: &str,
        action: &str,
        agent: &str,
        (name, value): (&str, String),
    ) -> Result<(), ActionCallError> {
        let acting = Acting::person(hex::encode(self.signing_device().cert.user_public_key));
        let device = self.device_cert().device_public_key;
        let invoke = declared(action)?.with_arguments(Map::from_iter([
            ("agent".to_owned(), Value::String(agent.to_owned())),
            ("device".to_owned(), Value::String(hex::encode(device))),
            (name.to_owned(), Value::String(value)),
        ]));
        Box::pin(self.act_on_agents((workspace, &acting, None), invoke)).await
    }

    /// The workspace's record of its agents as `caller` reads it: the agents, each with its name,
    /// model and whether it works, as a view of another language reads them. `None` where this
    /// device holds none of the workspace's record yet.
    ///
    /// # Errors
    /// The record is on its way from another device, or it does not read.
    pub async fn agents_reading(
        &self,
        workspace: &str,
        _caller: &Caller,
    ) -> Result<Option<Value>, ActionCallError> {
        let Some(bytes) = self.agents_record(workspace).await? else {
            return Ok(None);
        };
        let record = Replica::<WorkspaceAgents>::load(&bytes).map_err(unavailable)?;
        let state = record.state().map_err(unavailable)?;
        Ok(Some(tilo_resource::read::reading::<WorkspaceAgents>(
            &state,
        )))
    }

    /// What `caller` may change of the workspace's agents: whether the record's grants let it
    /// pause or resume one, which is what a settings view offers the switch by.
    ///
    /// # Errors
    /// The workspace is not held here, or its grants do not read.
    pub async fn agents_permissions(
        &self,
        workspace: &str,
        caller: &Caller,
    ) -> Result<AgentPermissions, ActionCallError> {
        let acting = caller.acting();
        Ok(AgentPermissions {
            pause: self
                .admit_agents_action(workspace, acting, WorkspaceAgents::PAUSE_AGENT.id)
                .await?,
            resume: self
                .admit_agents_action(workspace, acting, WorkspaceAgents::RESUME_AGENT.id)
                .await?,
        })
    }

    /// The workspace's record of its agents as this device holds it; `None` where it holds none
    /// yet.
    async fn agents_record(&self, workspace: &str) -> Result<Option<Vec<u8>>, ActionCallError> {
        self.resource_document(workspace, OWNER, &record_id())
            .await
            .map_err(unavailable)
    }

    /// Apply `invoke` to `workspace`'s record of its agents as `acting`, once the workspace's
    /// grants let them on the record's node, and write it as this device's own. Where this
    /// device keeps the workspace's own copy, the change keeps `call`, the request it answers, as
    /// its caller signed it: every device reading it sees that they asked, and decides the step the action
    /// declares as theirs.
    async fn act_on_agents(
        &self,
        (workspace, acting, call): (&str, &Acting, Option<&InvokeRequest>),
        invoke: Invoke<WorkspaceAgents>,
    ) -> Result<(), ActionCallError> {
        if !self
            .admit_agents_action(workspace, acting, invoke.id)
            .await?
        {
            return Err(ActionCallError::NotPermitted);
        }
        let served = match call {
            Some(call) if self.holds_own_copy(workspace).await => {
                tilo_actions::remote::SignedCall::of(call).map(|signed| (call.invocation, signed))
            }
            _ => None,
        };
        // The change is the workspace's own app's action: the record is the one it keeps for the
        // whole workspace, and every device decides the change against that app's surface.
        #[cfg(feature = "apps")]
        return self
            .author_record(workspace, (OWNER, KIND, &record_id()), |doc| {
                let mut record = Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                    .as_acting(acting.clone())
                    .in_app(OWNER, "Workspace");
                if let Some((invocation, signed)) = served {
                    record = record.serving(invocation, signed);
                }
                let applied =
                    record
                        .apply(invoke)
                        .map(drop)
                        .map_err(|error| ActionCallError::Refused {
                            reason: error.to_string(),
                        });
                std::mem::swap(doc, record.doc_mut());
                Ok(applied)
            })
            .await
            .map_err(unavailable)?;
        // A device that serves no apps keeps no store records.
        #[cfg(not(feature = "apps"))]
        {
            drop((served, invoke));
            Err(unavailable("this device keeps no records of the workspace"))
        }
    }

    /// Whether the workspace's grants let `acting` take the record's action `verb`: decided on
    /// the record's node for that action, as an app's window decides it.
    async fn admit_agents_action(
        &self,
        workspace: &str,
        acting: &Acting,
        verb: &str,
    ) -> Result<bool, ActionCallError> {
        let declared = declared(verb)?;
        let state = self.workspace(workspace).await.map_err(unavailable)?;
        let device = self.signing_device().cert.device_public_key;
        let now = tilo_clock::wall();
        let policy = state
            .lock()
            .await
            .peer()
            .permissions(device, now)
            .map_err(unavailable)?
            .policy;
        let access = MemberAccess::new(policy, acting.clone());
        let node = access.node(&tilo_actions::action_node(
            &TreePath::root(),
            OWNER,
            KIND,
            &record_id(),
            declared.field,
            verb,
        ));
        let cx = tilo_action_policy::Context::at_time(now);
        Ok(match access.answer(&node, declared.kind, &cx) {
            Answer::Deny => false,
            Answer::Allow | Answer::Ask => true,
        })
    }
}

#[cfg(all(test, feature = "apps", feature = "device-keys"))]
#[path = "agent_record_tests.rs"]
mod tests;
