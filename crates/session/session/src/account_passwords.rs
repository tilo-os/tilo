//! The passwords of the workspace's accounts, kept in the workspace's vault and read from this
//! device's own copy of it.
//!
//! An account of the workspace's accounts record names the vault entry its sign-in is in
//! ([`accounts::email::password_item`]). Settings, the keeper of the accounts, hands this
//! session a password typed for an account; it is kept in that entry (a new one for an account
//! that names none), bound to the account's servers, and Settings is told the entry to name.
//! Any app's engine asks for an account's sign-in by that entry
//! (`collaboration::browser_service_sessions::AccountSecrets`): it is released for an enabled
//! account that names the entry, only at the servers that account names now, and the use is
//! written into the entry first, saying which app signed in. Both are decided for the person
//! on the app's connection: their role must let them use Passwords in that workspace and, for a
//! sign-in, read the app asking ([`Workspaces::account_secret`]), as the account sign-ins of the
//! cloud service and the origin connectors are. Any member's device signs in with the same entry.
//! Where the workspace's rules ask its person first, this device asks nobody: the workspace's host
//! asks them on their approving devices, and releases the password from its vault once they said
//! yes ([`tilo_actions::remote::person_accounts::SIGN_IN`]).

use accounts::email::password_item;
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_resource::Id;
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::account_logins::{ASKS_FIRST, AccountLogin};

/// Where what happens to the workspace's account passwords is logged: with the session's own
/// account of what it did, which a session's log keeps.
pub(crate) const LOG: &str = tilo_startup_metrics::ACCOUNT_PASSWORDS_TARGET;

/// What an app is told when its ask is not one it may make.
pub(crate) const REFUSED: &str = "This account's secret is not available to this app";

/// What a release the workspace's rules ask about first is refused with where no host of the
/// workspace is reached to ask its person.
pub(crate) const NO_HOST_TO_ASK: &str = "The workspace asks you first before this account's \
                                         password leaves its vault, and none of its servers is \
                                         reached to ask you. Try again once one is.";

/// What a host's answer to a sign-in that did not come, or came as something else, is said as.
const HOST_DID_NOT_ANSWER: &str = "The workspace's server did not answer this account's sign-in.";

/// How long a host is waited on for a sign-in it asks the person about: as long as it asks.
const HOST_WAIT: std::time::Duration = std::time::Duration::from_secs(48);

/// The hosts of the workspaces this device takes part in: where a release the workspace's rules
/// ask about first goes, a device of a member holding no vault to answer it from, and asking
/// nobody itself.
static HOSTS: std::sync::OnceLock<
    std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>,
> = std::sync::OnceLock::new();

/// Ask `hosts` from now on for the releases the workspace's rules ask about first.
pub(crate) fn asking(
    hosts: std::sync::Arc<dyn tilo_workspace_service::apps::services::HostedWorkspaces>,
) {
    if HOSTS.set(hosts).is_err() {
        tracing::debug!(target: LOG, "the hosts asked for sign-ins were set before");
    }
}

/// Answer `command`, an account's password kept or released, that `app` sent on `by`'s own
/// connection in `workspace`.
pub(crate) async fn answer(
    workspaces: &Workspaces,
    workspace: &str,
    app: &str,
    by: &Caller,
    command: Command,
) -> Result<Reply, String> {
    let asked = if matches!(command, Command::StoreAccountSecret { .. }) {
        "keep"
    } else {
        "release"
    };
    tracing::info!(target: LOG, app, asked, "an app asked for an account's password to be kept or released");
    let answered = answering(workspaces, workspace, app, by, command).await;
    if let Err(error) = &answered {
        tracing::warn!(target: LOG, app, asked, %error, "an account's password was not kept or released");
    }
    answered
}

async fn answering(
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
            tracing::info!(target: LOG, %account, app, "kept an account's password in the workspace's vault");
            Ok(Reply::AccountSecretStored {
                account,
                item: item.to_string(),
            })
        }
        Command::ReleaseAccountSecret { item } => {
            let item = ItemRef::parse(&item).ok_or(REFUSED)?;
            // This device asks nobody: what the rules ask about first, the workspace's host asks
            // its person, and releases from its vault.
            let secret = match workspaces.account_secret(workspace, by, app, &item).await {
                Err(error) if error == ASKS_FIRST => from_host(workspace, app, &item).await?,
                released => released?.secret,
            };
            tracing::info!(target: LOG, %item, app, "released an account's sign-in from the workspace's vault");
            Ok(Reply::AccountSecret {
                item: item.to_string(),
                secret,
            })
        }
        _ => Err(REFUSED.into()),
    }
}

/// The password of `item` for `app`, released by `workspace`'s host once it asked this device's
/// person ([`person_accounts::SIGN_IN`]).
async fn from_host(
    workspace: &str,
    app: &str,
    item: &ItemRef,
) -> Result<zeroize::Zeroizing<String>, String> {
    use tilo_actions::remote::{
        ActionCallError, InvokeArgument, InvokeRequest, ReceiptStatus, person_accounts,
    };
    use tilo_actions::{InvocationId, InvocationUpdate};
    let host = HOSTS
        .get()
        .and_then(|hosts| hosts.host_actions(workspace))
        .ok_or(NO_HOST_TO_ASK)?;
    let asked = serde_json::to_value(person_accounts::SignIn {
        item: item.to_string(),
        app: app.to_owned(),
    })
    .ok()
    .and_then(|asked| asked.as_object().cloned())
    .ok_or(HOST_DID_NOT_ANSWER)?;
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target: person_accounts::target(person_accounts::SIGN_IN),
        argument: InvokeArgument::Json(asked),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: u32::try_from(HOST_WAIT.as_millis()).ok(),
        appearance: None,
        signed: None,
    };
    // The host decides by this device's certified session, never by a caller named here.
    let caller = Caller::local_person("tilo");
    let receipt = host
        .invoke(&caller, request)
        .await
        .ok_or(HOST_DID_NOT_ANSWER)?
        .map_err(|error| error.to_string())?;
    let output = match receipt.status {
        ReceiptStatus::Completed { output } => output,
        ReceiptStatus::Accepted => {
            let now = tilo_clock::now();
            let until = now.checked_add(HOST_WAIT).unwrap_or(now);
            let settled =
                tilo_actions::remote::settle_with(&host, &caller, receipt.invocation, None, until)
                    .await
                    .flatten();
            match settled.map(|state| state.update) {
                Some(InvocationUpdate::Succeeded { output, .. }) => output,
                Some(InvocationUpdate::Failed { error, .. }) => return Err(error.message),
                _ => return Err(HOST_DID_NOT_ANSWER.into()),
            }
        }
        ReceiptStatus::Rejected {
            error: ActionCallError::Refused { reason },
        } => return Err(reason),
        ReceiptStatus::Rejected { error } => return Err(error.to_string()),
        ReceiptStatus::Failed { error } => return Err(error.message),
        ReceiptStatus::Cancelled { .. } => return Err(HOST_DID_NOT_ANSWER.into()),
    };
    let signed: person_accounts::SignedIn = output
        .value
        .and_then(|value| serde_json::from_value(value).ok())
        .ok_or(HOST_DID_NOT_ANSWER)?;
    Ok(zeroize::Zeroizing::new(signed.secret))
}
