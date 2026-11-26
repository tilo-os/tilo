//! What a workspace's vault publishes at `workspace/vault`: the person's items,
//! as actions of the workspace's own tree, reached by a path as every other
//! action is, and by Passwords as its person's window on them. Each is decided by the
//! workspace's policy on its vault as any call of it is; what an agent asks by
//! reference is offered elsewhere ([`workspace_vault::use_rows`]).

use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_VAULT, workspace_governance_child, workspace_governance_resource,
};
use tilo_resource::EffectDecl;

use tilo_workspace_service::workspace_vault::{self, PERSON_ACTIONS};
use tilo_workspace_service::workspaces::actions::{DeclaredRow as _, Publisher};
use tilo_workspace_service::workspaces::workspace_stores::Store;

/// The vault as Passwords reaches it: `tilo.vault`'s own calls, which the app calls by their
/// address (`tilo.vault/vault#list`) as its person, on their device.
pub(super) const VAULT: Publisher = Publisher {
    provider: workspace_vault::PROVIDER,
    kind: workspace_vault::RESOURCE,
};

/// The declarations of the actions a person asks of the workspace's vault, in catalogue order.
fn person_actions() -> impl Iterator<Item = EffectDecl> {
    PERSON_ACTIONS
        .into_iter()
        .filter_map(workspace_vault::declaration)
}

/// The `tilo.vault` action `address` asks of a workspace's vault in its
/// governance tree, and the workspace it names.
pub(super) fn vault_action(address: &tilo_actions::ActionAddress) -> Option<(&str, &str)> {
    let workspace = address
        .resource
        .strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?
        .strip_suffix(WORKSPACE_VAULT)?
        .strip_suffix('/')?;
    let action = PERSON_ACTIONS
        .into_iter()
        .find(|action| *action == address.action)?;
    Some((workspace, action))
}

impl crate::PersonServices {
    /// The rows of `workspace`'s vault (its UUID) `asked` is offered: those the
    /// workspace's policy on its vault lets it ask.
    pub(super) async fn vault_rows(
        &self,
        asked: tilo_workspace_service::workspaces::actions::Asked<'_>,
        workspace: &str,
    ) -> Vec<CatalogueRow> {
        let root = workspace_governance_resource(workspace);
        let resource = workspace_governance_child(workspace, Some(WORKSPACE_VAULT));
        let mut rows = Vec::new();
        for verb in person_actions() {
            if self
                .reaching(asked, Store::VAULT, verb.id, verb.action_kind)
                .await
                .is_ok()
            {
                let mut row = verb.row(
                    tilo_workspace_service::workspaces::actions::GOVERNANCE,
                    &resource,
                    "Vault",
                );
                row.parent_resource.clone_from(&root);
                if let Some(descriptor) = row.action.as_mut() {
                    descriptor.contract.effects = workspace_vault::effects(verb.id);
                }
                rows.push(row);
            }
        }
        rows
    }

    /// The rows of the vault an app is offered as `asked`, by the vault's own provider: each
    /// call of it the app's grants and its person's let it make or ask. A caller that is no app
    /// is offered the vault's rows under the workspace's ([`Self::vault_rows`]).
    pub(super) async fn passwords_rows(
        &self,
        asked: tilo_workspace_service::workspaces::actions::Asked<'_>,
    ) -> Vec<CatalogueRow> {
        let (_, caller, _) = asked;
        if caller.acting().app().is_none() {
            return Vec::new();
        }
        // An app is offered what its grants already let it do; what it would have to ask for
        // first, it asks for when it calls, never by finding it listed.
        let mut rows = Vec::new();
        for decl in person_actions() {
            if self
                .reaching(asked, Store::VAULT, decl.id, decl.action_kind)
                .await
                .is_ok_and(|reach| reach.answer == tilo_action_policy::Answer::Allow)
            {
                rows.push(decl.row(VAULT, workspace_vault::RESOURCE, "Vault"));
            }
        }
        rows
    }

    /// Run `request`, one of the workspace's vault actions in its governance
    /// tree, as the `tilo.vault` action `action` it is, in the workspace
    /// `workspace` (its UUID) the request came through.
    pub(super) async fn vault_invoke(
        &self,
        asked: tilo_workspace_service::workspaces::actions::Asked<'_>,
        (workspace, action): (&str, &str),
        request: &tilo_actions::remote::InvokeRequest,
    ) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
        let (workspace_id, caller, certificate) = asked;
        let here = match workspace_id {
            Some(id) => self.workspace_identity(id).await,
            None => None,
        };
        if here.as_deref() != Some(workspace) {
            return Err(tilo_actions::remote::ActionCallError::UnknownAction);
        }
        let mut asking = request.clone();
        asking.target = workspace_vault::target(action);
        self.person_vault_invoke(workspace_id, caller, certificate, &asking)
            .await
    }
}
