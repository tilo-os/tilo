//! What a turn on this server uses of its person's vault, by reference.
//!
//! An agent's tools act on the workspace's action service, which knows the
//! workspace's resources and nothing of a person's own vault. The uses of a
//! vault item by reference (adding a mail account whose password the vault
//! keeps, keeping a provider's key from it) are the server's own to answer,
//! through its entry for a turn it carries out
//! ([`crate::workspaces::hosted_use`]). [`TurnUses`] is that entry as one
//! turn's tools reach it, a provider of the turn's own ([`TurnProvider`]): it
//! lists the uses the turn is offered beside the workspace's resources, and
//! takes them.
//!
//! No device of the person's is on, so a use no grant covers waits for their
//! yes: the entry refuses it for want of it, and the call waits as its asked
//! record until their approve admits it again.
//!
//! A field of a web page is the one use whose secret leaves this server: the page is in the
//! workspace's Browser window, and only its engine writes into it. The turn names the field and
//! the item ([`person_vault::fill_row`]); the window is handed the fill and says which site its
//! page is at ([`super::windows::Windows::fill_for_turn`]); the entry decides for
//! that site ([`TurnUses::release`]), waiting for the person's yes as for every use.
//!
//! A secret the agent has no item for yet is asked for through the vault's own ask
//! ([`person_vault::ASK`]), the one way an agent comes by one: the call waits for the person,
//! whose asking view opens the vault's own add, in the system's protected password field and in
//! the system's words ([`person_vault::AskForItem::words`]). What they type goes into their
//! vault, and the call answers the item's reference.
//!
//! Nothing here keeps a secret, and nothing here shows one. The arguments name the item by
//! its reference, and the entry answers what the person would be answered: the account, the
//! provider. A text released for a page passes through here once, on its way to be sealed to
//! the window that fills it, in a type that prints nothing.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::person_vault::{FillField, FillPage};
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, Pending, RemoteActionTarget, person_vault,
};
use tilo_actions::{ActionOutput, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_workspace_agent::Agent;
use zeroize::Zeroizing;

use super::turn_provider::{Ending, Rows};
use super::windows::{Release, Releasing, TurnFill};
use super::{Reply, ServerAgents, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::hosted_use::HostedTurn;

/// What an agent is told of a page fill that waits for its person: nothing was filled yet.
const ASKED_PAGE: &str = "Your person is asked whether you may fill this vault item into this \
     site's page. Nothing was filled yet.";

/// What one turn uses of its person's vault. A turn's caller changes when
/// its person writes again while it works, so each use is taken as the caller
/// of the tool call that asked for it.
pub(super) struct TurnUses {
    agents: ServerAgents,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The uses this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

/// `invocation` succeeded with `value`.
fn succeeded(invocation: tilo_actions::InvocationId, value: Value) -> InvocationUpdate {
    InvocationUpdate::Succeeded {
        invocation,
        output: ActionOutput {
            value: Some(value),
            resources: Vec::new(),
        },
    }
}

impl TurnUses {
    /// What `turn` uses of its person's vault, where `reply` holds what its
    /// tools begin as and act through.
    pub(super) async fn for_turn(agents: &ServerAgents, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            bound: narrowed.as_deref(),
        };
        let mut offered = agents.workspaces.uses_offered(&hosted).await;
        offered.extend(agents.workspaces.calls_offered(&hosted).await);
        if agents.workspaces.secret_ask_for_turn(&hosted).await.is_ok() {
            offered.push(person_vault::ask_row());
        }
        // A page's field is offered where this server places Browser windows, to a turn the
        // entry would not refuse before the vault is opened.
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

    /// The turn as the entry knows it, for `caller`.
    fn hosted<'a>(
        &self,
        caller: &'a Caller,
        narrowed: Option<&'a [tilo_action_policy::Rule]>,
    ) -> HostedTurn<'a> {
        HostedTurn {
            workspace: self.workspace_id,
            caller,
            bound: narrowed,
        }
    }

    /// Take the use `request` asks for as `caller`: how it ended, or the
    /// server's refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = self.agent.bound_rules();
        let value = self
            .agents
            .workspaces
            .use_for_turn(&self.hosted(caller, narrowed.as_deref()), request)
            .await?;
        Ok(succeeded(request.invocation, value))
    }

    /// Have the field `request` names filled, in the page of this workspace's Browser window,
    /// from the vault item it names, as `caller`: how the fill ended, as the window said it,
    /// or the refusal. The window says which site its page is at, and [`Self::release`]
    /// decides for that site; a fill that waits for its person is refused for want of their
    /// yes, and their approve admits it again.
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
                tilo_failure::ignored!(
                    "arguments that name no field are refused below, saying what a fill needs",
                    serde_json::from_value(Value::Object(arguments.clone())).ok()
                )
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
        // Refused before any window is asked, as before the vault is opened.
        let narrowed = self.agent.bound_rules();
        self.agents
            .workspaces
            .may_fill_pages(&self.hosted(caller, narrowed.as_deref()))
            .await?;
        let fill = TurnFill {
            invocation: request.invocation,
            element: wanted.element.clone(),
            item: wanted.item.to_string(),
            reason: wanted.reason.trim().to_owned(),
        };
        let asks: Arc<Mutex<Option<AskedFacts>>> = Arc::default();
        let (this, by, asked, waits) = (
            Arc::clone(self),
            caller.clone(),
            request.clone(),
            Arc::clone(&asks),
        );
        let releasing: Releasing = Box::new(move |origin| {
            let decided: Pin<Box<dyn Future<Output = Release> + Send>> =
                Box::pin(async move { this.release(&by, &asked, wanted, origin, &waits).await });
            decided
        });
        let filled = browsers
            .fill_for_turn(self.workspace_id, fill, releasing)
            .await;
        match asks.lock_recover().take() {
            Some(facts) => Err(ActionCallError::asks(facts)),
            None => filled,
        }
    }

    /// Decide the fill `request` asked of what `wanted` names, for the site `origin` its
    /// window says its page is at, as `caller`: the entry releases the item's text for that
    /// site, or it waits for its person (what they are shown is left in `asks`), or it is
    /// refused. A released text is handed on once, to be sealed to the window, and nothing
    /// else here holds it.
    async fn release(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        wanted: FillField,
        origin: String,
        asks: &Mutex<Option<AskedFacts>>,
    ) -> Release {
        let narrowed = self.agent.bound_rules();
        let fill = FillPage {
            site: origin,
            item: wanted.item,
            reason: wanted.reason.trim().to_owned(),
        };
        let released = self
            .agents
            .workspaces
            .release_for_turn(&self.hosted(caller, narrowed.as_deref()), &fill, request)
            .await;
        match released {
            Ok(text) => Release::Text {
                text: Zeroizing::new(text.text().to_owned()),
                into: text.field().as_str(),
            },
            Err(ActionCallError::ConfirmationRequired(facts)) => {
                *asks.lock_recover() = Some(*facts);
                Release::Asked(ASKED_PAGE.to_owned())
            }
            Err(ActionCallError::Refused { reason }) => Release::Refused(reason),
            Err(error) => Release::Refused(error.to_string()),
        }
    }

    /// Ask the person `caller` acts for for the secret `request` names: it waits for them to
    /// type it into their vault through the vault's own add, and answers the reference it is
    /// kept under.
    async fn ask_for_secret(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = self.agent.bound_rules();
        let using = self
            .agents
            .workspaces
            .secret_ask_for_turn(&self.hosted(caller, narrowed.as_deref()))
            .await?;
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(ActionCallError::InvalidRequest {
                reason: "an ask for a secret names its kind and where it belongs".to_owned(),
            });
        };
        let asked: person_vault::AskForItem =
            serde_json::from_value(Value::Object(arguments.clone())).map_err(|_| {
                ActionCallError::InvalidRequest {
                    reason: "an ask for a secret names its kind (login, key, code-generator, \
                             ssh-key or value), belongsTo and, when known, username, and \
                             never the secret"
                        .to_owned(),
                }
            })?;
        let asked = asked
            .checked()
            .map_err(|reason| ActionCallError::InvalidRequest { reason })?;
        if request.approval().is_none() {
            return Err(ActionCallError::asks(
                crate::workspaces::person_vault::secret_asked(caller, &using.name, &asked),
            ));
        }
        let reference = self
            .agents
            .workspaces
            .asked_item(&using.workspace, &asked)
            .await
            .map_err(ActionCallError::refused)?;
        let answered = serde_json::to_value(person_vault::Asked { reference })
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        Ok(succeeded(request.invocation, answered))
    }
}

impl TurnProvider for TurnUses {
    /// The providers of the uses this turn is offered, as a listing of
    /// everything names them beside the workspace's resources.
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

    /// The rows `query` asks for, when it asks one of those providers.
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

    /// A use by reference and the ask for a secret are this server's entry's to answer, also
    /// for a turn it offers none, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        person_vault::described_use(target).is_some() || *target == person_vault::ask_target()
    }

    /// A turn's caller changes when its person writes again while it works,
    /// so the use is taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move {
            if request.target == person_vault::page_target() {
                return self.fill_page(&caller, &request).await;
            }
            if request.target == person_vault::ask_target() {
                return self.ask_for_secret(&caller, &request).await;
            }
            self.take(&caller, &request).await
        })
    }
}
