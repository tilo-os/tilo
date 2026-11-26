//! The passwords of the workspace's accounts, kept in the workspace's vault, which the device's
//! daemon keeps and answers for.
//!
//! An account of the workspace's accounts record names the vault entry its sign-in is in
//! ([`accounts::email::password_item`]). Settings, the keeper of the accounts, hands this
//! session a password typed for an account; it is kept in that entry (a new one for an account
//! that names none), bound to the account's servers, and Settings is told the entry to name.
//! Any app's engine asks for an account's sign-in by that entry
//! (`collaboration::browser_service_sessions::AccountSecrets`): it is released for an enabled
//! account that names the entry, only at the servers that account names now, and the use is
//! written into the entry first, saying which app signed in. Both are decided for the person
//! on the app's connection, at the daemon: their role must let them use Passwords in that
//! workspace and, for a sign-in, read the app asking
//! (`tilo_workspace_service::workspaces::Workspaces::account_secret`), as the account sign-ins of
//! the cloud service and the origin connectors are. Any member's device that may read the vault
//! signs in with the same entry. Where the workspace's rules ask its person first, nothing is
//! released: no password leaves the vault on a question, on this device or another.
//!
//! An app that syncs one of the workspace's accounts reports how that went on the same
//! connection; the daemon writes it into the workspace's accounts as this device's report,
//! signed with this device's key, for every member's devices to read.

use accounts::email::password_item;
use accounts::person::AccountList;
use runtime_collab::wire::shell::{ShellCall, ShellReply, VaultCall};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionReply as Reply,
};
use tilo_actions::remote::ItemRef;
use tilo_peer_state::workspace_documents::DocumentRead;
use tilo_resource::{Id, Replica};
use tilo_session_client::daemon_documents::{DaemonShell, document_read, whole};

/// Where what happens to the workspace's account passwords is logged: with the session's own
/// account of what it did, which a session's log keeps.
pub(crate) const LOG: &str = tilo_startup_metrics::ACCOUNT_PASSWORDS_TARGET;

/// What an app is told when its ask is not one it may make.
pub(crate) const REFUSED: &str = "This account's secret is not available to this app";

/// Who asks: the person, on their device.
#[derive(Clone, Copy)]
pub(crate) struct Asker {
    pub(crate) person: [u8; 32],
    pub(crate) device: [u8; 32],
}

/// Answer `command`, an account's password kept or released, that `app` sent on `by`'s own
/// connection in `workspace`, as the daemon on `daemon` decides it.
pub(crate) async fn answer(
    daemon: &DaemonShell,
    workspace: &str,
    app: &str,
    by: Asker,
    command: Command,
) -> Result<Reply, String> {
    let asked = match command {
        Command::StoreAccountSecret { .. } => "keep",
        Command::ReportAccountSync { .. } => "report sync",
        _ => "release",
    };
    tracing::info!(target: LOG, app, asked, "an app asked for an account's password to be kept or released");
    let answered = answering(daemon, workspace, app, by, command).await;
    if let Err(error) = &answered {
        tracing::warn!(target: LOG, app, asked, %error, "an account's password was not kept or released");
    }
    answered
}

async fn answering(
    daemon: &DaemonShell,
    workspace: &str,
    app: &str,
    by: Asker,
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
            // A password typed anew for an account replaces the one its entry keeps: the entry
            // is the one the workspace's record names, never one the app asking names.
            let held = workspace_accounts(daemon, workspace).await?;
            let item = held
                .accounts
                .get(&Id::from_raw(account.clone()))
                .and_then(|kept| password_item(&kept.config))
                .and_then(ItemRef::parse)
                .map(|item| item.to_string());
            let kept = vault(
                daemon,
                VaultCall::Keep {
                    workspace: workspace.to_owned(),
                    person: by.person,
                    device: by.device,
                    item,
                    title: account.clone(),
                    username,
                    places: servers,
                    password: String::clone(&password),
                },
            )
            .await?;
            tracing::info!(target: LOG, %account, app, "kept an account's password in the workspace's vault");
            Ok(Reply::AccountSecretStored {
                account,
                item: kept,
            })
        }
        Command::ReleaseAccountSecret { item } => {
            let item = ItemRef::parse(&item).ok_or(REFUSED)?.to_string();
            let secret = vault(
                daemon,
                VaultCall::Release {
                    workspace: workspace.to_owned(),
                    person: by.person,
                    device: by.device,
                    app: app.to_owned(),
                    item: item.clone(),
                },
            )
            .await?;
            tracing::info!(target: LOG, %item, app, "released an account's sign-in from the workspace's vault");
            Ok(Reply::AccountSecret {
                item,
                secret: zeroize::Zeroizing::new(secret),
            })
        }
        Command::ReportAccountSync {
            account,
            health,
            signed_in,
            message,
        } => {
            let account = vault(
                daemon,
                VaultCall::ReportSync {
                    workspace: workspace.to_owned(),
                    person: by.person,
                    device: by.device,
                    account,
                    health,
                    signed_in,
                    message,
                },
            )
            .await?;
            Ok(Reply::AccountSyncReported { account })
        }
        _ => Err(REFUSED.into()),
    }
}

/// The workspace's accounts, as the daemon keeps its accounts record: none while it keeps none.
///
/// # Errors
/// The daemon did not answer, or the record does not read.
pub(crate) async fn workspace_accounts(
    daemon: &DaemonShell,
    workspace: &str,
) -> Result<AccountList, String> {
    let record = tilo_workspace_service::workspaces::account_logins::accounts_document();
    let read = daemon
        .read(whole(workspace, accounts::person::ACCOUNTS_OWNER, &record))
        .await?;
    match document_read(&read)? {
        DocumentRead::Whole { bytes, .. } => Replica::<AccountList>::load(&bytes)
            .and_then(|replica| replica.state())
            .map_err(|error| error.to_string()),
        _ => Ok(AccountList::default()),
    }
}

/// Ask the workspace's vault `call` at the daemon: what it answered, never shown in a log.
///
/// # Errors
/// The daemon refused the call, saying why, or did not answer.
pub(crate) async fn vault(daemon: &DaemonShell, call: VaultCall) -> Result<String, String> {
    match daemon.ask(ShellCall::Vault(call)).await? {
        ShellReply::Vault(answer) => answer.map(|answer| answer.0),
        _ => Err("the daemon answered a vault ask with something else".to_owned()),
    }
}
