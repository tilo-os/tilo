//! A field of a web page filled from its person's vault by a turn.
//!
//! Every other use of the vault by reference, and the ask for a secret the vault does not hold
//! yet, the turn asks of the device's dispatch ([`super::kept`]).
//!
//! A field of a web page is the one use whose secret leaves this server: the page is in the
//! workspace's Browser window, and only its engine writes into it. The turn names the field and
//! the item ([`workspace_vault::fill_row`]); the window is handed the fill and says which site its
//! page is at ([`super::windows::Windows::fill_for_turn`]); the entry decides for
//! that site ([`TurnUses::release`]), waiting for the person's yes as for every use.
//!
//! Nothing here keeps a secret, and nothing here shows one. The arguments name the item by
//! its reference, and the entry answers what the person would be answered: the account, the
//! provider. A text released for a page passes through here once, on its way to be sealed to
//! the window that fills it, in a type that prints nothing.

use std::sync::{Arc, Mutex};
use tilo_person_services::PersonServices;

use serde_json::Value;
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeArgument,
    InvokeRequest, Pending, RemoteActionTarget,
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_records::vault::{self as workspace_vault, FillField};

use super::turn_provider::{Ending, Rows};
use super::windows::{TurnFill, releasing};
use super::{Reply, ServerAgents, TurnProvider, WorkspaceId};

/// What one turn fills of a page from its person's vault. A turn's caller changes when its
/// person writes again while it works, so each fill is taken as the caller of the tool call that
/// asked for it.
pub(super) struct TurnUses {
    agents: ServerAgents,
    workspace: String,
    workspace_id: WorkspaceId,
    /// The fill this turn is offered: none where this server places no Browser window, or for a
    /// turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnUses {
    /// What the turn `reply` begins fills of a page, where `reply` holds what its tools begin
    /// as and act through.
    pub(super) async fn for_turn(agents: &ServerAgents, reply: &Reply) -> Arc<Self> {
        // A page's field is offered where this server places Browser windows, to a turn the
        // entry would not refuse before the vault is opened.
        let offered = if agents.browsers.is_some()
            && PersonServices::of(&agents.workspaces)
                .may_fill_pages_for(&reply.caller)
                .await
                .is_ok()
        {
            vec![workspace_vault::fill_row()]
        } else {
            Vec::new()
        };
        Arc::new(Self {
            agents: agents.clone(),
            workspace: reply.workspace.clone(),
            workspace_id: reply.provider.workspace,
            offered,
        })
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
        PersonServices::of(&self.agents.workspaces)
            .may_fill_pages_for(caller)
            .await?;
        let fill = TurnFill {
            invocation: request.invocation,
            element: wanted.element.clone(),
            item: wanted.item.to_string(),
        };
        let asks: Arc<Mutex<Option<AskedFacts>>> = Arc::default();
        let releasing = releasing(
            self.agents.workspaces.clone(),
            (caller.clone(), request.clone()),
            wanted,
            Arc::clone(&asks),
        );
        let filled = browsers
            .fill_for_turn(self.workspace_id, fill, releasing)
            .await;
        let asked = asks.lock_recover().take();
        match asked {
            Some(facts) => Err(ActionCallError::asks(facts)),
            None => filled,
        }
    }
}

impl TurnProvider for TurnUses {
    /// The vault's provider, as a listing of everything names it beside the workspace's
    /// resources, where this turn is offered a page's fill.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if self.offered.is_empty() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: workspace_vault::PROVIDER.to_owned(),
            app_id: workspace_vault::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
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

    /// A page's fill is carried out here, also for a turn it is not offered to, which the
    /// entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        *target == workspace_vault::target(workspace_vault::FILL_PAGE)
    }

    /// A turn's caller changes when its person writes again while it works,
    /// so the use is taken as the caller of the tool call that asked for it.
    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.fill_page(&caller, &request).await })
    }
}
