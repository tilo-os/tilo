//! Account credentials through the certified native route of the app asking.
//!
//! The session is the credential store of the person's account secrets: it alone
//! opens their vault, under session ownership, which no app sandbox reaches.
//! On an app's own route it answers:
//!
//! - Browser: the workspace's website sessions, listed and resolved, and forgotten by someone
//!   who administers the workspace: each is one of the workspace's accounts, its cookies sealed
//!   in this device's vault;
//! - Settings, the keeper of the workspace's accounts: keeping an account's password in the
//!   workspace's vault, which it is never released back, and naming the entry it is kept in;
//! - any app's engine: the sign-in of the workspace's account that names the vault entry it
//!   asks for, read from this device's own copy of the workspace's vault
//!   ([`crate::account_passwords`]), for a person whose role lets them use Passwords;
//! - Browser, where this session holds a workspace's Browser: the text of a vault item for
//!   one page action that fills a field from it by reference, which the workspace's server
//!   keeps and decides about ([`crate::item_lending`]). The session keeps no vault for it.
//!
//! Every request is decided on the certificate the broker attested on the
//! app's connection, against the workspace that connection is in: a current
//! device of the person this workspace's identity presents here, who may
//! still read that app's record in the workspace — and, to forget a
//! selection, who administers the workspace. What it answers never enters a
//! workspace's records: the vault stays on the device, and a website session
//! crosses the app's own channel only.

use accounts::{Account, AccountId};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionDescriptor as Descriptor,
    BrowserServiceSessionReply as Reply,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::apps::services::{
    BrowserServiceSessionRequestContext as Context, BrowserServiceSessionServices, ItemAnswering,
};

use crate::engine::Engine;

const UNAVAILABLE: &str = "Selected website session unavailable";
use ui::apps::BROWSER;

/// The person's account credentials, as the apps it serves reach it on the action API.
pub(crate) struct ServiceSessions {
    engine: Arc<Engine>,
    /// What Browser's asks for vault items are answered with.
    items: Arc<crate::item_lending::ItemLending>,
    /// The device's daemon, which keeps each workspace's vault and accounts: where an account's
    /// password is kept and released, and its accounts are read.
    daemon: tilo_session_client::daemon_documents::DaemonShell,
}

impl ServiceSessions {
    pub(crate) fn new(
        engine: Arc<Engine>,
        items: Arc<crate::item_lending::ItemLending>,
        daemon: tilo_session_client::daemon_documents::DaemonShell,
    ) -> Self {
        Self {
            engine,
            items,
            daemon,
        }
    }

    /// The certified person's own Browser route, readable on Browser's record in the workspace
    /// (and, for `admin`, administered by this workspace), now and rechecked after.
    fn authorize_selection(&self, context: &Context, admin: bool) -> Result<(), String> {
        context.revalidate()?;
        let person = hex::encode(context.identity_public_key());
        if context.app_id() != BROWSER || context.person() != person {
            return Err("Account credentials requires the certified person's own app".into());
        }
        let workspace = WorkspaceId::parse(context.workspace_id())
            .ok_or("Invalid account-credentials workspace")?;
        self.engine.owner().with_current(workspace, |view| {
            let certificate = context.read_context().certificate();
            if !view.peer().certificates().contains(certificate) {
                return Err("This device left the workspace".into());
            }
            let now = std::time::SystemTime::now();
            if admin {
                view.peer()
                    .workspace()
                    .check_workspace_administration(certificate.device_public_key, now)?;
            }
            let member = view
                .peer()
                .permissions(certificate.device_public_key, now)
                .map_err(|error| error.to_string())?;
            if member.person_id() != Some(context.person()) {
                return Err("This person left the workspace".into());
            }
            // Reading the app's own record here is what the broker itself asks of a join: the
            // person's grants on the workspace decide it.
            let route = context.read_context().route();
            let node = collab_types::validator::document_node(
                &route.cluster_id,
                &route.app_id,
                &route.document_id,
            );
            if !member.allows(&node, tilo_action_policy::ActionKind::Read, now) {
                return Err("This person can no longer read the app".into());
            }
            Ok(())
        })?;
        let selected = tilo_identity::store::selected_actor_id_prompt(context.workspace_id())
            .map_err(|_| "This workspace's selected identity is unavailable")?;
        if selected.as_deref() != Some(context.person()) {
            return Err("This workspace's selected identity changed".into());
        }
        context.revalidate()
    }
}

impl BrowserServiceSessionServices for ServiceSessions {
    fn authorize(&self, context: &Context) -> Result<(), String> {
        self.authorize_selection(context, false)
    }

    fn authorize_command(&self, context: &Context, command: &Command) -> Result<(), String> {
        if !command.browser_only() {
            // An account's password is decided once, by the workspace's vault, for the device
            // that asks ([`tilo_workspace_service::workspaces::Workspaces::account_secret`]).
            return context.revalidate();
        }
        self.authorize_selection(context, matches!(command, Command::Forget { .. }))
    }

    fn command(&self, context: &Context, command: Command) -> Result<Reply, String> {
        self.authorize_command(context, &command)?;
        if !command.valid() {
            return Err("Invalid account credentials request".into());
        }
        if command == (Command::LendsItems {}) {
            // Answered by what this session is: it says so before its server can be asked.
            return Ok(Reply::ItemsLent {
                lent: self.items.lends(context.workspace_id()),
            });
        }
        // An item's use ([`Self::item_use`]), an account's password and a website session
        // ([`Self::answered_later`]) are waited for, and never here.
        Err("This is not answered on the connection's dispatch".into())
    }

    fn answered_later(&self, context: &Context, command: Command) -> ItemAnswering {
        let ready = |answer: Result<Reply, String>| -> ItemAnswering {
            Box::pin(std::future::ready(answer))
        };
        if !command.valid() {
            return ready(Err("Invalid account credentials request".into()));
        }
        let not_yet = |why: &str| -> ItemAnswering {
            tracing::warn!(target: crate::account_passwords::LOG, app = context.app_id(), why, "an account's password was asked for before this device holds its workspaces");
            ready(Err(
                tilo_workspace_service::workspaces::account_logins::VAULT_NOT_HERE.into(),
            ))
        };
        if self.daemon.link().is_err() {
            return not_yet("it reaches no device daemon");
        }
        let Ok(tasks) = tilo_rt::try_app() else {
            return not_yet("the session's tasks are not running");
        };
        let daemon = self.daemon.clone();
        let workspace = context.workspace_id().to_owned();
        if command.website_session() {
            let Some(root) = accounts::config_root() else {
                return ready(Err(UNAVAILABLE.into()));
            };
            let identity = context.identity_public_key();
            let answering = tasks.spawn(async move {
                website_session(&daemon, &workspace, root, identity, command).await
            });
            return Box::pin(async move { answering.await.map_err(|_| UNAVAILABLE.to_owned())? });
        }
        let app = context.app_id().to_owned();
        let by = crate::account_passwords::Asker {
            person: context.identity_public_key(),
            device: context.read_context().certificate().device_public_key,
        };
        let answering = tasks.spawn(async move {
            crate::account_passwords::answer(&daemon, &workspace, &app, by, command).await
        });
        Box::pin(async move {
            answering.await.map_err(|_| {
                tilo_workspace_service::workspaces::account_logins::VAULT_NOT_HERE.to_owned()
            })?
        })
    }

    fn item_use(&self, context: &Context, command: Command) -> ItemAnswering {
        // Browser's ask is decided as every request of Browser's is: the certified person's
        // own connection, in a workspace this device is still in. What that refuses is told
        // with its reason. Any other app is told that only Browser asks.
        if context.app_id() == BROWSER
            && let Some(invocation) = command.item_use()
            && let Err(reason) = self.authorize_selection(context, false)
        {
            return Box::pin(std::future::ready(Ok(Reply::item_refused(
                invocation, &reason,
            ))));
        }
        self.items
            .answer(context.app_id(), context.workspace_id(), &command)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

/// One of the workspace's website sessions listed, resolved or forgotten, as the workspace's
/// accounts and this device's vault under `root` answer for it. A forgotten session's account
/// leaves the workspace's accounts once its cookies are gone from the vault.
async fn website_session(
    daemon: &tilo_session_client::daemon_documents::DaemonShell,
    workspace: &str,
    root: PathBuf,
    identity: [u8; 32],
    command: Command,
) -> Result<Reply, String> {
    let accounts = crate::account_passwords::workspace_accounts(daemon, workspace)
        .await?
        .all();
    // The vault opens on this device's keyring or its files: off the runtime's threads.
    let reply = tokio::task::spawn_blocking(move || resolve(&root, identity, &accounts, command))
        .await
        .map_err(|_| UNAVAILABLE.to_owned())??;
    if let Reply::Forgotten { id, .. } = &reply {
        crate::account_passwords::vault(
            daemon,
            runtime_collab::wire::shell::VaultCall::Forget {
                workspace: workspace.to_owned(),
                account: id.clone(),
            },
        )
        .await?;
    }
    Ok(reply)
}

/// One website session's selection, as the vault under `root` and the workspace's `accounts`
/// answer for it.
fn resolve(
    root: &Path,
    identity: [u8; 32],
    accounts: &[Account],
    command: Command,
) -> Result<Reply, String> {
    let credential_store = account_credentials::CredentialStore::for_root(Some(root.to_owned()));
    let is_session = |account: &&Account| {
        account.type_id.as_str() == account_credentials::service_sessions::SERVICE_SESSION_TYPE
    };
    if accounts.iter().filter(is_session).count() > runtime_collab::MAX_BROWSER_SERVICE_SESSIONS {
        return Err("Too many selected website sessions".into());
    }
    let kept = |id: &AccountId| accounts.iter().find(|account| &account.id == id);
    let reply = match command {
        Command::Forget { id, setup_binding } => {
            let account = AccountId::from(id.clone());
            credential_store
                .forget_service_session(&account, kept(&account), identity, &setup_binding)
                .map_err(|_| UNAVAILABLE)?;
            Reply::Forgotten { id, setup_binding }
        }
        Command::List {} => {
            let mut sessions = Vec::new();
            for account in accounts
                .iter()
                .filter(is_session)
                .filter(|account| account.enabled)
            {
                let Some(binding) = account
                    .config
                    .get("setup_binding")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                // Metadata presence is not proof of usable, current-person ownership.
                let Ok(session) = credential_store.service_session(account, identity, binding)
                else {
                    continue;
                };
                sessions.push(Descriptor {
                    id: account.id.as_str().into(),
                    service: session.service,
                    setup_binding: binding.into(),
                });
            }
            Reply::List { sessions }
        }
        Command::Resolve { id, setup_binding } => {
            let account = kept(&AccountId::from(id)).ok_or(UNAVAILABLE)?;
            let session = credential_store
                .service_session(account, identity, &setup_binding)
                .map_err(|_| UNAVAILABLE)?;
            Reply::Resolved { session }
        }
        // Account secrets, items and what this session lends are not website sessions.
        Command::StoreAccountSecret { .. }
        | Command::ReleaseAccountSecret { .. }
        | Command::UseItem { .. }
        | Command::LendsItems {} => return Err(UNAVAILABLE.into()),
    };
    if !reply.valid() {
        return Err(UNAVAILABLE.into());
    }
    Ok(reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    use browser_continuity::sessions::{Cookie, SameSite, Service, SessionCookies};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one named scenario walked through in order"
    )]
    fn native_private_ownership_lists_and_resolves_only_current_enabled_person() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let session = SessionCookies {
            schema: 1,
            session_id: "selected-slack".into(),
            service: Service::Slack,
            identity_public_key: [7; 32],
            setup_binding: "prepared-operation".into(),
            source_snapshot_digest: [9; 32],
            origin_storage: Vec::new(),
            cookies: vec![Cookie {
                host: "app.slack.com".into(),
                domain: "slack.com".into(),
                host_only: false,
                name: "d".into(),
                value: "private-session-marker".into(),
                path: "/".into(),
                expires_unix_seconds: None,
                secure: true,
                http_only: true,
                same_site: SameSite::None,
            }],
        };
        let id = AccountId::from(session.session_id.clone());
        let mut account = accounts::Account::new(id.clone(), "browser-service-session");
        account.config =
            serde_json::json!({"service":"slack","setup_binding":"prepared-operation"});
        let mut kept = vec![account.clone()];
        let mut vault = accounts::SecretVault::open_in(root.path());
        vault.put(
            &id,
            &account_credentials::service_sessions::service_session_secret(&session).unwrap(),
        );
        vault.try_save().unwrap();
        let before = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
        let Reply::List { sessions } =
            resolve(root.path(), [7; 32], &kept, Command::List {}).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.session_id);
        let Reply::Resolved { session: restored } = resolve(
            root.path(),
            [7; 32],
            &kept,
            Command::Resolve {
                id: id.as_str().into(),
                setup_binding: "prepared-operation".into(),
            },
        )
        .unwrap() else {
            panic!("Expected session");
        };
        assert_eq!(restored, session);
        assert!(!format!("{restored:?}").contains("private-session-marker"));
        assert!(
            resolve(
                root.path(),
                [8; 32],
                &kept,
                Command::Resolve {
                    id: id.as_str().into(),
                    setup_binding: "prepared-operation".into()
                }
            )
            .is_err()
        );
        account.enabled = false;
        kept = vec![account];
        assert!(
            resolve(
                root.path(),
                [7; 32],
                &kept,
                Command::Resolve {
                    id: id.as_str().into(),
                    setup_binding: "prepared-operation".into()
                }
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
            before
        );
        let forget = Command::Forget {
            id: id.as_str().into(),
            setup_binding: "prepared-operation".into(),
        };
        assert!(resolve(root.path(), [8; 32], &kept, forget.clone()).is_err());
        assert_eq!(
            std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
            before
        );
        let forgotten = resolve(root.path(), [7; 32], &kept, forget.clone()).unwrap();
        assert!(forgotten.matches(&forget));
        // The workspace's accounts no longer have it once it was forgotten.
        kept.clear();
        assert!(
            resolve(
                root.path(),
                [7; 32],
                &kept,
                Command::Resolve {
                    id: id.as_str().into(),
                    setup_binding: "prepared-operation".into(),
                }
            )
            .is_err()
        );
        assert!(
            resolve(root.path(), [7; 32], &kept, forget.clone())
                .unwrap()
                .matches(&forget)
        );
        let Reply::List { sessions } =
            resolve(root.path(), [7; 32], &kept, Command::List {}).unwrap()
        else {
            panic!("Expected list after revocation");
        };
        assert!(sessions.is_empty());
    }
}
