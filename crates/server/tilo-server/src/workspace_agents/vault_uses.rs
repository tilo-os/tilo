//! What a turn on this server uses of its workspace's vault, by reference.
//!
//! Uses are authorized by workspace `tilo-action-policy` grants and interactive
//! confirmations through [`Workspaces::ask_person_confirmation`] (never via
//! Chat approval cards, a Chat polling loop, or a custom `asked_person_first`
//! failure code).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_actions::remote::person_vault::{FillField, FillPage};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, Pending, RemoteActionTarget, person_vault,
};
use tilo_actions::{ActionOutput, InvocationId, InvocationUpdate};
use tilo_peer_state::document_frame::DocumentRoute;
use tilo_resource::Resource as _;
use tilo_server_protocol::certify::{ConfirmationAnswer, PendingConfirmation};
use tilo_workspace_agent::Agent;
use zeroize::Zeroizing;

use super::turn_provider::{Ending, Rows};
use super::{
    CHAT, HostAgents as Agents, Reply, Turn, TurnProvider, WorkspaceActionProvider, WorkspaceId,
    invoke_named,
};
use crate::hosted_browser::{Release, Releasing, TurnFill};
use crate::workspaces::hosted_use::{HostedTurn, OpenAsk, TurnRelease, TurnUse, UseAnswer};

/// How long the person has to answer a pending vault-use confirmation.
const QUESTION_WAIT_NS: u64 = 2 * 60 * 1_000_000_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AskedIn {
    conversation: String,
    cause: String,
    session: String,
}

/// What one turn uses of its workspace's vault.
pub(super) struct TurnUses {
    agents: Agents,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    offered: Vec<CatalogueRow>,
}

impl TurnUses {
    /// What `turn` uses of the vault, where `reply` holds what its tools begin as.
    pub(super) async fn for_turn(agents: &Agents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = narrowing(&turn.agent, &reply.workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            keeper: &turn.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let mut offered = agents.workspaces.uses_offered(&hosted).await;
        offered.extend(agents.workspaces.calls_offered(&hosted).await);
        if agents.browsers.is_some() && agents.workspaces.may_fill_pages(&hosted).await.is_ok() {
            offered.push(person_vault::fill_row());
        }
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    fn confirmation_for(open: &OpenAsk) -> PendingConfirmation {
        let target = open.target();
        let action_label = if open.reason.trim().is_empty() {
            format!("use vault item for {}", target.address.action)
        } else {
            open.reason.trim().to_owned()
        };
        PendingConfirmation {
            id: rand::random(),
            agent_id: open.agent.clone(),
            agent_label: open.name.clone(),
            title: String::new(),
            action_label,
            resource_label: target.address.resource,
            arguments: Vec::new(),
            risk: "sensitive".to_owned(),
            kind: "outside".to_owned(),
            undoable: false,
            workspace: open.workspace.clone(),
            expires_at_ns: tilo_clock::unix_now_ns().saturating_add(QUESTION_WAIT_NS),
            page: None,
            ..PendingConfirmation::default()
        }
    }

    /// Take the use `request` asks for as `caller`.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let invocation = request.invocation;
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.agents.workspaces;
        let taken = workspaces
            .use_for_turn(&hosted, request, asked_in(caller))
            .await?;
        match taken {
            TurnUse::Done(value) => Ok(InvocationUpdate::Succeeded {
                invocation,
                output: ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            }),
            TurnUse::Waits => Err(ActionCallError::confirmation_required()),
            TurnUse::Asks(open) => {
                let mut person = [0u8; 32];
                if hex::decode_to_slice(&open.person, &mut person).is_err() {
                    workspaces.use_not_asked(&open);
                    return Err(ActionCallError::confirmation_required());
                }
                let question = Self::confirmation_for(&open);
                match workspaces.ask_person_confirmation(person, question).await {
                    Ok(ConfirmationAnswer::AllowOnce) => {
                        let answered = workspaces
                            .answer_use(&hosted, &open.ask, &open.detail, UseAnswer::Once)
                            .await?;
                        Ok(InvocationUpdate::Succeeded {
                            invocation,
                            output: ActionOutput {
                                value: answered,
                                resources: Vec::new(),
                            },
                        })
                    }
                    Ok(ConfirmationAnswer::Deny | ConfirmationAnswer::Reply(_)) => {
                        workspaces.use_not_asked(&open);
                        Err(ActionCallError::NotPermitted)
                    }
                    Err(_) => {
                        workspaces.use_not_asked(&open);
                        Err(ActionCallError::confirmation_required())
                    }
                }
            }
        }
    }

    /// Have the field `request` names filled in the Browser window.
    async fn fill_page(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let Some(browsers) = self.agents.browsers.clone() else {
            return Err(ActionCallError::Refused {
                reason: "This server keeps no Browser window, so it fills no page".to_owned(),
            });
        };
        let wanted: Option<FillField> = match &request.argument {
            InvokeArgument::Json(arguments) => {
                serde_json::from_value(Value::Object(arguments.clone())).ok()
            }
            _ => None,
        };
        let Some(wanted) = wanted else {
            return Err(ActionCallError::InvalidRequest {
                reason: "a field to fill names its element in the page's outline and the vault \
                         item to fill it from"
                    .to_owned(),
            });
        };
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        self.agents.workspaces.may_fill_pages(&hosted).await?;
        let invocation = request.invocation;
        let fill = TurnFill {
            invocation,
            element: wanted.element.clone(),
            item: wanted.item.to_string(),
            reason: wanted.reason.trim().to_owned(),
        };
        let (this, by) = (Arc::clone(self), caller.clone());
        let releasing: Releasing = Box::new(move |origin| {
            let decided: Pin<Box<dyn Future<Output = Release> + Send>> =
                Box::pin(async move { this.release(&by, invocation, wanted, origin).await });
            decided
        });
        browsers
            .fill_for_turn(self.workspace_id, fill, releasing)
            .await
    }

    async fn release(
        &self,
        caller: &Caller,
        invocation: InvocationId,
        wanted: FillField,
        origin: String,
    ) -> Release {
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let fill = FillPage {
            site: origin,
            item: wanted.item,
            reason: wanted.reason.trim().to_owned(),
        };
        let workspaces = &self.agents.workspaces;
        let released = workspaces
            .release_for_turn(&hosted, &fill, invocation, asked_in(caller))
            .await;
        match released {
            Ok(TurnRelease::Released(text)) => Release::Text {
                text: Zeroizing::new(text.text().to_owned()),
                into: text.field().as_str(),
            },
            Ok(TurnRelease::Waits) => {
                Release::Refused("confirmation is required to fill this vault item".to_owned())
            }
            Ok(TurnRelease::Asks(open)) => {
                let mut person = [0u8; 32];
                if hex::decode_to_slice(&open.person, &mut person).is_err() {
                    workspaces.use_not_asked(&open);
                    return Release::Refused(
                        "confirmation is required to fill this vault item".to_owned(),
                    );
                }
                let question = Self::confirmation_for(&open);
                match workspaces.ask_person_confirmation(person, question).await {
                    Ok(ConfirmationAnswer::AllowOnce) => {
                        if let Err(error) = workspaces
                            .answer_use(&hosted, &open.ask, &open.detail, UseAnswer::Once)
                            .await
                        {
                            return Release::Refused(error.to_string());
                        }
                        match workspaces
                            .release_for_turn(&hosted, &fill, invocation, asked_in(caller))
                            .await
                        {
                            Ok(TurnRelease::Released(text)) => Release::Text {
                                text: Zeroizing::new(text.text().to_owned()),
                                into: text.field().as_str(),
                            },
                            Ok(_) => Release::Refused(
                                "confirmation is required to fill this vault item".to_owned(),
                            ),
                            Err(ActionCallError::Refused { reason }) => Release::Refused(reason),
                            Err(error) => Release::Refused(error.to_string()),
                        }
                    }
                    Ok(ConfirmationAnswer::Deny | ConfirmationAnswer::Reply(_)) => {
                        workspaces.use_not_asked(&open);
                        Release::Refused("the person did not allow it".to_owned())
                    }
                    Err(why) => {
                        workspaces.use_not_asked(&open);
                        Release::Refused(why)
                    }
                }
            }
            Err(ActionCallError::Refused { reason }) => Release::Refused(reason),
            Err(error) => Release::Refused(error.to_string()),
        }
    }
}

impl TurnProvider for TurnUses {
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        let mut named: Vec<&str> = self
            .offered
            .iter()
            .map(|row| row.provider.as_str())
            .collect();
        named.dedup();
        named
            .into_iter()
            .map(|provider| CatalogueProvider {
                provider: provider.to_owned(),
                app_id: provider.to_owned(),
                workspace: self.workspace.clone(),
                document_id: String::new(),
                browsable: true,
            })
            .collect()
    }

    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        let provider = query.provider.as_deref()?;
        let rows: Vec<CatalogueRow> = self
            .offered
            .iter()
            .filter(|row| row.provider == provider)
            .cloned()
            .collect();
        (!rows.is_empty()).then(|| Pending::ready(Ok(rows)))
    }

    fn takes(&self, target: &RemoteActionTarget) -> bool {
        person_vault::described_use(target).is_some()
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move {
            if request.target == person_vault::page_target() {
                return self.fill_page(&caller, &request).await;
            }
            self.take(&caller, &request).await
        })
    }
}

fn narrowing(agent: &Agent, workspace: &str) -> Option<Vec<tilo_action_policy::Rule>> {
    (agent.bound_rules().is_some() || agent.is_sub()).then(|| agent.turn_rules(workspace))
}

fn asked_in(caller: &Caller) -> Value {
    let turn = caller.turn();
    let kept = AskedIn {
        conversation: turn
            .map(|turn| turn.conversation.clone())
            .unwrap_or_default(),
        cause: turn.map(|turn| turn.message.clone()).unwrap_or_default(),
        session: caller
            .agent_session()
            .map(|session| session.id)
            .unwrap_or_default(),
    };
    serde_json::to_value(kept).unwrap_or(Value::Null)
}

/// Say on the question `message` in the part `document` what came of its
/// answer.
pub(super) async fn settle(
    (service, asker): (&WorkspaceActionProvider, &Caller),
    workspace: &str,
    (document, message): (&str, &str),
    outcome: &str,
    told: Option<String>,
) -> Result<(), String> {
    let route = DocumentRoute {
        app_id: CHAT.to_owned(),
        cluster_id: workspace.to_owned(),
        document: document.to_owned(),
    };
    let mut arguments = Map::new();
    arguments.insert("message".into(), json!(message));
    arguments.insert("outcome".into(), json!(outcome));
    if let Some(told) = told {
        arguments.insert("detail".into(), json!(told));
    }
    let resource = format!("{}/messages/{message}", chat::parts::ChatMessages::KIND);
    invoke_named(
        service,
        asker,
        &route,
        &resource,
        "settle-question",
        arguments,
    )
    .await
    .map(drop)
}

impl Agents {
    /// Every part of the chat's messages `workspace` holds, by its document.
    pub(super) async fn chat_parts(
        &self,
        workspace: &str,
    ) -> Vec<(String, chat::parts::ChatMessages)> {
        let Ok(routes) = self.workspaces.document_routes(workspace).await else {
            return Vec::new();
        };
        let mut parts = Vec::new();
        for (app, document) in routes {
            if app != CHAT
                || collab_types::document::DocumentId::parse(&document)
                    .is_none_or(|part| chat::parts::part_index(&part).is_none())
            {
                continue;
            }
            if let Ok(Some(part)) = self
                .read::<chat::parts::ChatMessages>(workspace, CHAT, &document)
                .await
            {
                parts.push((document, part));
            }
        }
        parts
    }
}
