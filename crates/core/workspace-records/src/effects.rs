//! What an action declares it writes besides its own resource, read from the step tables of the
//! records that declare it, so a device checks a call's change by the one declaration of each
//! action.

use tilo_actions::remote::{
    ACCOUNTS_PROVIDER, CONNECTIONS_PROVIDER, MODELS_PROVIDER, MODERATED_APP, MODERATION_HISTORY,
    MODERATION_PROVIDER, MODERATION_UNDO, RemoteActionTarget, VAULT_PROVIDER,
    WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MAIL_ACCOUNTS, WORKSPACE_VAULT,
    workspace_agent_effects, workspace_governance_operation, workspace_governance_resource,
};

use crate::{accounts, connections, models, vault};

/// What the action `target` names declares it writes besides its own resource
/// ([`tilo_actions::ActionContract::effects`]), from its declaration: a device reading a record checks
/// here whether a step a call's change takes there is one the called action takes. Every action
/// that writes another record declares it, and nothing a call carries adds to it.
#[must_use]
pub fn declared_effects(target: &RemoteActionTarget) -> Vec<tilo_actions::ActionEffect> {
    // An operation is asked either at its own resource (`workspace/agents#create`) or by its
    // name at the workspace's root (`workspace#agent.create`).
    let operation = workspace_governance_operation(&target.address)
        .map(|(_, operation)| operation)
        .or_else(|| {
            let root = target
                .address
                .resource
                .strip_prefix(workspace_governance_resource("").as_str())?;
            (!root.is_empty() && !root.contains('/')).then_some(target.address.action.as_str())
        });
    if target.provider == WORKSPACE_GOVERNANCE_PROVIDER
        && let Some(operation) = operation
    {
        let agents = workspace_agent_effects(operation);
        if !agents.is_empty() {
            return agents;
        }
    }
    let address = &target.address;
    // The workspace's mail accounts in its tree (`workspace/mail-accounts`) take the steps their
    // `tilo.accounts` action takes; adding one there is `add`.
    let in_mail_accounts = target.provider == WORKSPACE_GOVERNANCE_PROVIDER
        && address
            .resource
            .strip_prefix(workspace_governance_resource("").as_str())
            .and_then(|rest| rest.strip_suffix(WORKSPACE_MAIL_ACCOUNTS))
            .and_then(|workspace| workspace.strip_suffix('/'))
            .is_some_and(|workspace| !workspace.is_empty() && !workspace.contains('/'));
    let mut effects = match target.provider.as_str() {
        _ if in_mail_accounts => account_steps(match address.action.as_str() {
            "add" => "add-mail",
            action => action,
        }),
        ACCOUNTS_PROVIDER if address.resource == "accounts" => account_steps(&address.action),
        MODELS_PROVIDER => vault_steps(&models::MODEL_VAULT_STEPS, &address.action),
        CONNECTIONS_PROVIDER => vault_steps(&connections::CONNECTION_VAULT_STEPS, &address.action),
        // A moderation undo takes a person's changes back in the chat's record.
        MODERATION_PROVIDER
            if address.resource == MODERATION_HISTORY && address.action == MODERATION_UNDO =>
        {
            vec![tilo_actions::ActionEffect::undo(MODERATED_APP)]
        }
        _ => Vec::new(),
    };
    // The vault's own actions take their steps in its record, whether asked of the vault's
    // provider or of the vault in the workspace's tree (`workspace/vault`).
    let in_vault = target.provider == VAULT_PROVIDER
        || (target.provider == WORKSPACE_GOVERNANCE_PROVIDER
            && address
                .resource
                .strip_prefix(workspace_governance_resource("").as_str())
                .and_then(|rest| rest.strip_suffix(WORKSPACE_VAULT))
                .and_then(|workspace| workspace.strip_suffix('/'))
                .is_some_and(|workspace| !workspace.contains('/')));
    if in_vault {
        for effect in vault_steps(&vault::VAULT_STEPS, &address.action) {
            if !effects.contains(&effect) {
                effects.push(effect);
            }
        }
    }
    // An app's own history takes back what it wrote: `undo` on the app's own resource
    // (`tl:<app>#undo`) undoes that app's records, whichever kinds the invocation wrote where.
    if address.action == tilo_actions::ActionEffect::UNDO
        && let Some(app) = address.resource.strip_prefix("tl:")
        && !app.is_empty()
        && !app.contains(':')
    {
        let undo = tilo_actions::ActionEffect::undo(app);
        if !effects.contains(&undo) {
            effects.push(undo);
        }
    }
    effects
}

/// [`accounts::ACCOUNT_STEPS`] of the accounts action `action`.
fn account_steps(action: &str) -> Vec<tilo_actions::ActionEffect> {
    accounts::ACCOUNT_STEPS
        .iter()
        .find(|(id, _, _)| *id == action)
        .map_or_else(Vec::new, |(_, vault, record)| {
            vault
                .iter()
                .map(|verb| vault::vault_step(verb))
                .chain(record.iter().map(|verb| {
                    tilo_actions::ActionEffect::new(
                        tilo_app_id::apps::SETTINGS,
                        ACCOUNTS_PROVIDER,
                        *verb,
                    )
                }))
                .collect()
        })
}

/// The steps of `table` the action `action` takes, as effects.
fn vault_steps(table: &[(&str, &[&str])], action: &str) -> Vec<tilo_actions::ActionEffect> {
    table
        .iter()
        .find(|(id, _)| *id == action)
        .map_or_else(Vec::new, |(_, steps)| {
            steps.iter().map(|verb| vault::vault_step(verb)).collect()
        })
}

#[cfg(test)]
mod tests {
    use tilo_actions::remote::answers_a_secret;

    use super::*;

    /// Each record writing in the vault declares that it does, a reading declares nothing, and
    /// each action answering with a secret is one the host keeps out of every record of a call.
    #[test]
    fn the_records_declare_what_they_write_and_answer() {
        for (target, what) in [
            (vault::target(vault::ADD), "adding a login"),
            (accounts::target(accounts::ADD_MAIL), "adding an account"),
            (models::target(models::SET_KEY), "keeping a key"),
        ] {
            assert!(
                !declared_effects(&target).is_empty(),
                "{what} writes in the vault"
            );
        }
        for target in [
            vault::target(vault::LIST),
            accounts::target(accounts::LIST),
            models::target(models::LIST),
        ] {
            assert!(
                declared_effects(&target).is_empty(),
                "{target:?} only reads"
            );
        }
        for action in accounts::ACCOUNT_SECRET_ANSWERS {
            assert!(answers_a_secret(action), "{action}");
        }
        for action in vault::VAULT_SECRET_ANSWERS {
            assert!(answers_a_secret(action), "{action}");
        }
    }
}
