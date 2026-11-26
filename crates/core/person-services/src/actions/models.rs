//! What a workspace's model providers publish at `workspace/models`: the
//! providers it keeps, their keys sealed in its vault, and the model each kind
//! of work runs on, as actions of the workspace's own tree, reached by a path as
//! every other action is. Each is decided by the workspace's policy on the
//! providers as any call of it is; a key travels in and is never read back.
//!
//! The actions are the ones the providers declare ([`ModelsAction`]): the tree lists each as
//! its declaration states it, and routes a call of it to the provider.

use tilo_actions::ActionParam;
use tilo_actions::remote::{
    CatalogueRow, WORKSPACE_MODELS, workspace_governance_child, workspace_governance_resource,
};
use tilo_resource::{Effect as _, EffectDecl};

use tilo_workspace_records::models::{self as model_providers, ModelsAction};
use tilo_workspace_service::workspaces::actions::DeclaredRow as _;
use tilo_workspace_service::workspaces::workspace_stores::Store;

/// Whether the workspace's tree offers the action `decl` declares: every one but those a
/// caller asks only of the provider itself. A turn of the model is asked by a window, a key
/// named by its vault item is answered as a use of the vault, and a computer links its own
/// model from its own pane.
fn in_tree(decl: &EffectDecl) -> bool {
    ![
        model_providers::ASK,
        model_providers::SET_KEY_FROM_ITEM,
        model_providers::LINK_THIS_COMPUTER,
    ]
    .contains(&decl.id)
}

/// The declaration of the action `action` the tree offers.
fn tree_declaration(action: &str) -> Option<EffectDecl> {
    model_providers::declaration(action).filter(in_tree)
}

/// The parameters the model providers' action `action` (a `tilo.models` action, or a verb of
/// them in a workspace's governance tree) is published with.
pub(super) fn params_of(action: &str) -> Option<Vec<ActionParam>> {
    tree_declaration(action).map(|decl| decl.params)
}

/// The `tilo.models` action `address` asks of a workspace's model providers in
/// its governance tree, and the workspace it names.
pub(super) fn models_action(address: &tilo_actions::ActionAddress) -> Option<(&str, &str)> {
    let workspace = address
        .resource
        .strip_prefix(tilo_actions::remote::workspace_governance_resource("").as_str())?
        .strip_suffix(WORKSPACE_MODELS)?
        .strip_suffix('/')?;
    let action = tree_declaration(&address.action)?.id;
    Some((workspace, action))
}

impl crate::PersonServices {
    /// The rows of `workspace`'s model providers (its UUID) `asked` is offered:
    /// those the workspace's policy on the providers lets it ask.
    pub(super) async fn models_rows(
        &self,
        asked: tilo_workspace_service::workspaces::actions::Asked<'_>,
        workspace: &str,
    ) -> Vec<CatalogueRow> {
        let root = workspace_governance_resource(workspace);
        let resource = workspace_governance_child(workspace, Some(WORKSPACE_MODELS));
        let mut rows = Vec::new();
        for verb in ModelsAction::declarations()
            .iter()
            .filter(|verb| in_tree(verb))
        {
            if self
                .reaching(asked, Store::MODELS, verb.id, verb.action_kind)
                .await
                .is_ok()
            {
                let mut row = verb.row(
                    tilo_workspace_service::workspaces::actions::GOVERNANCE,
                    &resource,
                    "Model providers",
                );
                row.parent_resource.clone_from(&root);
                // Reading `workspace/models` lists the providers.
                if verb.id == model_providers::LIST
                    && let Some(listing) = row.action.as_mut()
                {
                    listing
                        .contract
                        .tags
                        .push(tilo_actions::DOCUMENT_READ_TAG.to_owned());
                }
                rows.push(row);
            }
        }
        rows
    }

    /// Run `request`, one of the workspace's model provider actions in its
    /// governance tree, as the `tilo.models` action `action` it is, in the
    /// workspace `workspace` (its UUID) the request came through.
    pub(super) async fn models_invoke(
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
        asking.target = model_providers::target(action);
        self.model_providers_invoke(workspace_id, caller, certificate, &asking)
            .await
    }
}
