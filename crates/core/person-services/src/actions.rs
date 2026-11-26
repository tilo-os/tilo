//! The person's providers as the device's actions registry answers them: each registered to
//! answer in the workspaces the device holds, as the connection that carried a call admitted
//! it ([`Providers::answer_in_workspaces`]).

#[cfg(feature = "agents")]
mod calendars;
mod connections;
mod mail;
mod model_asks;
mod models;
mod telling;
mod vault;

use std::sync::Arc;

use tilo_actions::ActionParam;
use tilo_actions::remote::{
    ActionCallError, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    RemoteActionTarget, person_telling,
};
use tilo_workspace_service::models as model_providers;
use tilo_workspace_service::workspaces::actions::LEARNED;
use tilo_workspace_service::workspaces::providers::{
    Answered, Answering, Live, WorkspaceAnswers, WorkspaceCall,
};
use tilo_workspace_service::workspaces::workspace_stores::Store;
use tilo_workspace_service::{person_connections, workspace_accounts, workspace_vault};

use crate::{PersonServices, WeakPersonServices};

/// The calendar rows a turn this server carries out is offered
/// ([`crate::person_settings::turn_calendars`]).
#[cfg(feature = "agents")]
pub(crate) fn calendar_rows() -> Vec<CatalogueRow> {
    calendars::rows()
}

/// The providers the person services answer, each under its own id and granted as that
/// application.
const PROVIDED: [&str; 6] = [
    workspace_accounts::PROVIDER,
    model_providers::PROVIDER,
    workspace_vault::PROVIDER,
    person_telling::PROVIDER,
    person_connections::PROVIDER,
    // What the workspace's agents learned about the person.
    LEARNED,
];

/// Register the person services on `workspaces` as they open: an embedder names it among its
/// service's registrations ([`tilo_workspace_service::service::ServiceConfig::registers`]),
/// and the workspaces answer the person's providers from then on.
pub fn register(workspaces: &tilo_workspace_service::workspaces::Workspaces) {
    PersonServices::of(workspaces).register();
}

impl PersonServices {
    /// Answer the person's providers on the device's one actions registry from now on, tell
    /// each call that waits for a person's yes to that person, and carry what the host kept of
    /// its workspaces' vaults before into them, as the workspaces open ([`register`]).
    fn register(&self) {
        let providers = self.providers();
        for provider in PROVIDED {
            providers.answer_in_workspaces(
                provider,
                provider,
                Arc::new(Provided {
                    services: self.downgrade(),
                    provider,
                }),
            );
        }
        let services = self.downgrade();
        self.wrap_asked_records(Arc::new(move |records| {
            let Some(services) = services.upgrade() else {
                return records;
            };
            // An app's asked use of a vault item waits in the same store, under its own dispatch.
            if let Some(store) = services.asked_store() {
                services.keep_vault_asks_in(&store);
            }
            services.told(records)
        }));
        // Where a device was reached while its Tilo was closed goes with its key.
        let services = self.downgrade();
        self.on_device_revoked(Arc::new(move |user, device| {
            if let Some(services) = services.upgrade() {
                services.forget_push_address_of(user, device);
            }
        }));
        let carrying = self.clone();
        tokio::spawn(async move { carrying.carry_vaults_over().await });
    }
}

/// One of the person's providers ([`PROVIDED`]), answering for the services while they live.
struct Provided {
    services: WeakPersonServices,
    provider: &'static str,
}

impl Provided {
    fn services(&self) -> Result<PersonServices, ActionCallError> {
        self.services
            .upgrade()
            .ok_or_else(|| ActionCallError::Unavailable {
                reason: "the workspace closed".into(),
            })
    }
}

/// How the parameters of a provider's action are read, by the action's name.
type ParamsOf = fn(&str) -> Option<Vec<ActionParam>>;

impl WorkspaceAnswers for Provided {
    fn children(&self) -> &'static [&'static str] {
        match self.provider {
            workspace_vault::PROVIDER => &[tilo_actions::remote::WORKSPACE_VAULT],
            workspace_accounts::PROVIDER => &[tilo_actions::remote::WORKSPACE_MAIL_ACCOUNTS],
            model_providers::PROVIDER => &[tilo_actions::remote::WORKSPACE_MODELS],
            _ => &[],
        }
    }

    fn turn_takes(&self, target: &RemoteActionTarget) -> bool {
        crate::hosted_use::takes(target)
    }

    /// Another device of the person's asks their models; their accounts, vault, rule, the
    /// services they connected and what was learned about them are asked where they are kept,
    /// by a turn or an app's own connection.
    fn roster_asks(&self, _target: &RemoteActionTarget) -> bool {
        self.provider == model_providers::PROVIDER
    }

    fn tree_rows<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        workspace: &'a str,
    ) -> Answering<'a, Vec<CatalogueRow>> {
        Box::pin(async move {
            let Ok(services) = self.services() else {
                return Vec::new();
            };
            let asked = (call.workspace, call.caller, call.certificate);
            match self.provider {
                workspace_accounts::PROVIDER => services.mail_account_rows(asked, workspace).await,
                workspace_vault::PROVIDER => services.vault_rows(asked, workspace).await,
                model_providers::PROVIDER => services.models_rows(asked, workspace).await,
                _ => Vec::new(),
            }
        })
    }

    fn params(&self, target: &RemoteActionTarget) -> Option<Vec<ActionParam>> {
        let address = &target.address;
        let (child, own): (ParamsOf, _) = match self.provider {
            workspace_vault::PROVIDER => (workspace_vault::params_of, vault::vault_action(address)),
            workspace_accounts::PROVIDER => {
                (mail::account_params_of, mail::account_action(address))
            }
            model_providers::PROVIDER => (models::params_of, models::models_action(address)),
            _ => return None,
        };
        if let Some((_, action)) = own {
            return child(action);
        }
        match self.provider {
            workspace_accounts::PROVIDER => workspace_accounts::params_of(&address.action),
            _ => child(&address.action),
        }
    }

    fn answers_a_secret(&self, target: &RemoteActionTarget) -> bool {
        let address = &target.address;
        let action = match self.provider {
            workspace_vault::PROVIDER => vault::vault_action(address).map(|(_, action)| action),
            workspace_accounts::PROVIDER => mail::account_action(address).map(|(_, action)| action),
            model_providers::PROVIDER => models::models_action(address).map(|(_, action)| action),
            _ => return false,
        };
        tilo_actions::remote::answers_a_secret(action.unwrap_or(&address.action))
    }

    fn offer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        query: &'a CatalogueQuery,
    ) -> Answering<'a, Vec<(CatalogueProvider, Vec<CatalogueRow>)>> {
        Box::pin(async move {
            let Ok(services) = self.services() else {
                return Vec::new();
            };
            if query
                .provider
                .as_deref()
                .is_some_and(|wanted| wanted != self.provider)
            {
                return Vec::new();
            }
            let (caller, certificate) = (call.caller, call.certificate);
            let asked = (call.workspace, caller, certificate);
            let (publisher, rows) = match self.provider {
                // An agent or a key the workspace's policy lets read its mail or its calendars
                // finds what it may do with them here. A page asks directly, and is offered no
                // row.
                workspace_accounts::PROVIDER
                    if caller.acting().is_agent() || caller.acting().app().is_some() =>
                {
                    (mail::MAIL, services.mail_rows(asked).await)
                }
                // Passwords finds the vault's own calls its person may make, which it calls by
                // address.
                workspace_vault::PROVIDER => (vault::VAULT, services.passwords_rows(asked).await),
                // An agent of a person's finds that person's log of what
                // they were told here: it tells them, and marks what it
                // told as not to be missed.
                person_telling::PROVIDER => {
                    if !crate::PersonServices::tells_as_agent(caller, certificate, call.workspace) {
                        return Vec::new();
                    }
                    (telling::TELLING, telling::agent_rows())
                }
                // An agent finds what it may read of the services the workspace connected.
                // Adding one is no row.
                person_connections::PROVIDER if services.connects_as_agent(asked).await => {
                    (connections::CONNECTIONS, connections::agent_rows())
                }
                _ => return Vec::new(),
            };
            vec![(publisher.entry(""), rows)]
        })
    }

    /// What an agent's turn asks of what its workspace keeps for the person who asked it is
    /// decided for that turn.
    fn answer_turn<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>> {
        Box::pin(async move {
            self.services()?
                .answer_turn(call.caller, request)
                .await
                .and_then(|receipt| match receipt.status {
                    tilo_actions::remote::ReceiptStatus::Completed { output } => {
                        Ok(Answered::Value(output.value.unwrap_or_default()))
                    }
                    _ => Err(ActionCallError::Unanswered),
                })
        })
    }

    fn answer<'a>(
        &'a self,
        call: WorkspaceCall<'a>,
        request: &'a InvokeRequest,
    ) -> Answering<'a, Result<Answered, ActionCallError>> {
        Box::pin(async move {
            let services = self.services()?;
            if model_asks::is_model_ask(request) {
                return services.ask_model(call, request).await;
            }
            if request.target == workspace_accounts::target(workspace_accounts::WATCH) {
                return services.watch_accounts(call, request).await;
            }
            services.answer(call, request).await.map(Answered::Value)
        })
    }
}

/// Whether `target` names a secret by reference, which carries none, so an agent acting for the
/// person asks it too: the vault decides whether the item belongs where it would be used.
fn uses_by_reference(target: &RemoteActionTarget) -> bool {
    matches!(
        (target.provider.as_str(), target.address.action.as_str()),
        (
            workspace_accounts::PROVIDER,
            workspace_accounts::ADD_MAIL_WITH_ITEM
        ) | (
            model_providers::PROVIDER,
            model_providers::SET_KEY_FROM_ITEM
        ) | (workspace_vault::PROVIDER, workspace_vault::ASK)
    )
}

impl PersonServices {
    /// Answer `request` for `call` by what it asks of the person's providers.
    async fn answer(
        &self,
        call: WorkspaceCall<'_>,
        request: &InvokeRequest,
    ) -> Result<serde_json::Value, ActionCallError> {
        let (workspace, caller, certificate) = (call.workspace, call.caller, call.certificate);
        let asked = (workspace, caller, certificate);
        let address = &request.target.address;
        // A child of a workspace's own tree is asked of its governance.
        if request.target.provider == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER {
            if let Some(child) = mail::account_action(address) {
                return Box::pin(self.mail_account_invoke(asked, child, request)).await;
            }
            if let Some(child) = vault::vault_action(address) {
                return Box::pin(self.vault_invoke(asked, child, request)).await;
            }
            if let Some(child) = models::models_action(address) {
                return Box::pin(self.models_invoke(asked, child, request)).await;
            }
            return Err(ActionCallError::UnknownAction);
        }
        match (request.target.provider.as_str(), address.action.as_str()) {
            _ if uses_by_reference(&request.target) => {
                self.vault_use_invoke(workspace, caller, certificate, request)
                    .await
            }
            // The person's own mail accounts and model providers, kept here when their
            // workspace runs here: a page has no connection to a mail server and no vault of
            // its own.
            (workspace_accounts::PROVIDER, _) => {
                Box::pin(self.person_accounts_invoke(workspace, caller, certificate, request)).await
            }
            (model_providers::PROVIDER, _) => {
                self.model_providers_invoke(workspace, caller, certificate, request)
                    .await
            }
            // What the person keeps to sign in with, in that same vault: nothing of it is read
            // back, and no agent asks.
            (workspace_vault::PROVIDER, _) => {
                self.person_vault_invoke(workspace, caller, certificate, request)
                    .await
            }
            // What the person was told: their agents tell them, and only they read it.
            (person_telling::PROVIDER, _) => {
                self.person_telling_invoke(workspace, caller, certificate, request)
                    .await
            }
            // What the workspace's agents learned about the person: theirs to read and to
            // change. A turn reaches the same actions only as its engine offers them.
            (LEARNED, _) => {
                self.person_learned_invoke(workspace, caller, certificate, request)
                    .await
            }
            (person_connections::PROVIDER, _) => {
                Self::connections_invoke(self, asked, request).await
            }
            _ => Err(ActionCallError::UnknownAction),
        }
    }

    /// Follow `call`'s workspace's mail accounts, whom its policy lets read them, as `request`
    /// asks ([`workspace_accounts::WATCH`]): the accounts now, and again whenever the person's
    /// settings move and they read differently.
    async fn watch_accounts(
        &self,
        call: WorkspaceCall<'_>,
        request: &InvokeRequest,
    ) -> Result<Answered, ActionCallError> {
        let reach = self
            .reaching_allowed(
                (call.workspace, call.caller, call.certificate),
                Store::MAIL,
                workspace_accounts::WATCH,
                tilo_action_policy::ActionKind::Read,
                request,
            )
            .await?;
        let workspace = reach.workspace.to_string();
        let mut changes = self.person_settings_changes();
        let first = self.accounts_listing(&workspace).await?;
        let (live, follows) = tokio::sync::watch::channel(Live::Item(first));
        let services = self.downgrade();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    moved = changes.changed() => if moved.is_err() { return },
                    () = live.closed() => return,
                }
                let Some(services) = services.upgrade() else {
                    return;
                };
                if let Ok(now) = services.accounts_listing(&workspace).await {
                    live.send_if_modified(|read| {
                        let moved = *read != Live::Item(now.clone());
                        if moved {
                            *read = Live::Item(now);
                        }
                        moved
                    });
                }
            }
        });
        Ok(Answered::Live(follows))
    }
}

#[cfg(test)]
#[path = "actions_tests.rs"]
pub(crate) mod actions_tests;
