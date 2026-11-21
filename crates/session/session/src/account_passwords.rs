//! The passwords of the workspace's accounts, kept in the workspace's vault and read from this
//! device's own copy of it.
//!
//! An account of the workspace's accounts record names the vault entry its password is in
//! ([`accounts::email::password_item`]). Settings, the keeper of the accounts, hands this
//! session a password typed for an account; it is kept in that entry (a new one for an account
//! that names none), bound to the account's servers, and Settings is told the entry to name.
//! The app that keeps the account's type (Mail for email) is released the password of an
//! enabled account, only at the servers the account names now, and the use is written down in
//! the entry first. Both are decided for the person on the app's connection: their role must
//! let them use Passwords in that workspace ([`Workspaces::may_use_vault`]). Any member's
//! device signs in with the same entry; none keeps a copy of its own.
//!
//! A password this device kept before an account's was the workspace's is carried into the
//! vault ([`Workspaces::carry_device_passwords`]): as the session starts, and when an account
//! that names no entry yet is asked for.

use accounts::email::{AccountConfig, password_item, password_places};
use accounts::person::secret_keeper;
use runtime_collab::{BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_resource::Id;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::account_logins::{AccountLogin, NO_ENTRY};

/// What an app is told when its ask is not one it may make.
pub(crate) const REFUSED: &str = accounts::secret_release::REFUSED;

/// What is said while this device's workspace service is not up yet.
pub(crate) const NO_VAULT_YET: &str =
    "This device is still opening its workspaces, so no account's password is available yet. \
     Try again in a moment.";

/// Answer `command`, an account's password kept or released, that `app` sent on `by`'s own
/// connection in `workspace`.
pub(crate) async fn answer(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    by: &Caller,
    command: Command,
) -> Result<Reply, String> {
    match command {
        Command::StoreAccountSecret {
            account,
            password,
            username,
            servers,
        } => {
            if app != accounts::person::ACCOUNTS_OWNER {
                return Err(REFUSED.into());
            }
            let places = servers
                .iter()
                .map(|server| accounts::vault_items::Place::parse(server))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            // A password typed anew for an account replaces the one its entry keeps: the entry
            // is the one the workspace's record names, never one the app asking names.
            let held = workspaces.workspace_accounts(workspace).await?;
            let named = held
                .accounts
                .get(&Id::from_raw(account.clone()))
                .and_then(|kept| password_item(&kept.config))
                .and_then(ItemRef::parse);
            let item = workspaces
                .keep_account_password(
                    workspace,
                    by,
                    named.as_ref(),
                    AccountLogin {
                        title: account.clone(),
                        username,
                        places,
                    },
                    password,
                )
                .await?;
            tracing::info!(%account, app, "kept an account's password in the workspace's vault");
            Ok(Reply::AccountSecretStored {
                account,
                item: item.to_string(),
            })
        }
        Command::ReleaseAccountSecret { account } => {
            let mut held = workspaces.workspace_accounts(workspace).await?;
            let mut stored = held
                .store()
                .get(&accounts::AccountId::from(account.clone()))
                .cloned()
                .ok_or(REFUSED)?;
            if !stored.enabled || secret_keeper(stored.type_id.as_str()) != Some(app) {
                return Err(REFUSED.into());
            }
            if password_item(&stored.config).is_none() {
                // A password this device kept before is carried into the vault now.
                carry(workspaces, by).await;
                held = workspaces.workspace_accounts(workspace).await?;
                stored = held
                    .store()
                    .get(&accounts::AccountId::from(account.clone()))
                    .cloned()
                    .ok_or(REFUSED)?;
            }
            let item = password_item(&stored.config)
                .and_then(ItemRef::parse)
                .ok_or(NO_ENTRY)?;
            let config = AccountConfig::from_value(stored.config.clone()).map_err(|_| REFUSED)?;
            let places = password_places(&config).map_err(|error| error.to_string())?;
            let released = workspaces
                .account_password(workspace, by, &item, &places, &account)
                .await?;
            tracing::info!(%account, app, "released an account's password from the workspace's vault to the app that keeps its type");
            Ok(Reply::AccountSecret {
                account,
                password: released.secret,
            })
        }
        _ => Err(REFUSED.into()),
    }
}

/// Carry the mail passwords this device kept in its own credentials into the workspaces'
/// vault, as `by` (the device's person).
pub(crate) async fn carry(workspaces: &Workspaces, by: &Caller) {
    let Some(root) = accounts::config_root() else {
        return;
    };
    // A device that never sealed a credential has none to carry, and its keyring is not asked.
    if !accounts::vault_dir_in(&root).join("vault.enc").exists() {
        return;
    }
    // Opening them may ask the keyring, which blocks: never on the service's own tasks.
    let opened =
        tokio::task::spawn_blocking(move || accounts::AccountCredentials::open_in(&root)).await;
    let mut credentials = match opened {
        Ok(Ok(credentials)) => credentials,
        Ok(Err(error)) => {
            tracing::warn!(%error, "this device's account credentials do not open, so its mail passwords stay in them");
            return;
        }
        Err(error) => {
            tracing::warn!(%error, "this device's account credentials were not opened");
            return;
        }
    };
    if let Err(error) = workspaces.carry_device_passwords(&mut credentials, by).await {
        tracing::warn!(%error, "this device's mail passwords were carried, but its credentials did not save");
    }
}
