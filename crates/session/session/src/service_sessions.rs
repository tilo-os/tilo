//! Account credentials through the certified native route of the app asking.
//!
//! The session is the credential store of the person's account secrets: it alone
//! opens their vault, under session ownership, which no app sandbox reaches.
//! On an app's own route it answers:
//!
//! - Browser: its private website sessions, listed and resolved, and
//!   forgotten by someone who administers the workspace;
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

use accounts::{AccountId, AccountStore};
use runtime_collab::{
    BrowserServiceSessionCommand as Command, BrowserServiceSessionDescriptor as Descriptor,
    BrowserServiceSessionReply as Reply,
};
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};
use std::sync::Arc;
use tilo_desktop_model::WorkspaceId;
use tilo_workspace_service::apps::services::{
    BrowserServiceSessionRequestContext as Context, BrowserServiceSessionServices, ItemAnswering,
};

use crate::engine::Engine;

const UNAVAILABLE: &str = "Selected website session unavailable";
use ui::apps::BROWSER;
const MAX_STORE: u64 = 2 * 1024 * 1024;

/// The person's account credentials, as the apps it serves reach it on the action API.
pub(crate) struct ServiceSessions {
    engine: Arc<Engine>,
    /// What Browser's asks for vault items are answered with.
    items: Arc<crate::item_lending::ItemLending>,
    /// This device's own workspace service: the copy of each workspace's vault and accounts
    /// an account's password is kept in and read from.
    service: tilo_workspace_service::service::ServiceSlot,
}

impl ServiceSessions {
    pub(crate) fn new(
        engine: Arc<Engine>,
        items: Arc<crate::item_lending::ItemLending>,
        service: tilo_workspace_service::service::ServiceSlot,
    ) -> Self {
        Self {
            engine,
            items,
            service,
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
        if command.item_use().is_some() {
            // Its answer is waited for, and never here ([`Self::item_use`]).
            return Err("An item's use is not answered on the connection's dispatch".into());
        }
        if !command.browser_only() {
            // Its answer is waited for, and never here ([`Self::account_secret`]).
            return Err(
                "An account's password is not answered on the connection's dispatch".into(),
            );
        }
        let root = accounts::config_root().ok_or(UNAVAILABLE)?;
        let reply = resolve(&root, context.identity_public_key(), command.clone())?;
        self.authorize_command(context, &command)?;
        Ok(reply)
    }

    fn account_secret(&self, context: &Context, command: Command) -> ItemAnswering {
        let ready = |answer: Result<Reply, String>| -> ItemAnswering {
            Box::pin(std::future::ready(answer))
        };
        let not_yet = |why: &str| -> ItemAnswering {
            tracing::warn!(target: crate::account_passwords::LOG, app = context.app_id(), why, "an account's password was asked for before this device holds its workspaces");
            ready(Err(crate::account_passwords::NO_VAULT_YET.into()))
        };
        let Some(service) = self.service.service() else {
            return not_yet("its own workspace service is not open yet");
        };
        let Ok(tasks) = tilo_rt::try_app() else {
            return not_yet("the session's tasks are not running");
        };
        let workspaces = service.workspaces().clone();
        let workspace = context.workspace_id().to_owned();
        let app = context.app_id().to_owned();
        let by = tilo_actions::remote::Caller::authenticated(
            context.identity_public_key(),
            context.read_context().certificate().device_public_key,
        )
        .in_workspace(workspace.clone());
        let answering = tasks.spawn(async move {
            crate::account_passwords::answer(&workspaces, &workspace, &app, &by, command).await
        });
        Box::pin(async move {
            answering
                .await
                .map_err(|_| crate::account_passwords::NO_VAULT_YET.to_owned())?
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

/// One website session's selection, as the vault and the person's accounts answer for it.
fn resolve(root: &Path, identity: [u8; 32], command: Command) -> Result<Reply, String> {
    let store = read_store(root).map_err(|_| UNAVAILABLE)?;
    let credential_store = account_credentials::CredentialStore::for_root(Some(root.to_owned()));
    if store
        .all()
        .iter()
        .filter(|account| {
            account.type_id.as_str() == account_credentials::service_sessions::SERVICE_SESSION_TYPE
        })
        .count()
        > runtime_collab::MAX_BROWSER_SERVICE_SESSIONS
    {
        return Err("Too many selected website sessions".into());
    }
    if let Command::Forget { id, setup_binding } = command {
        credential_store
            .forget_service_session(&AccountId::from(id.clone()), identity, &setup_binding)
            .map_err(|_| UNAVAILABLE)?;
        return Ok(Reply::Forgotten { id, setup_binding });
    }
    let reply = match command {
        Command::List {} => {
            let mut sessions = Vec::new();
            for account in store.all().iter().filter(|account| {
                account.type_id.as_str()
                    == account_credentials::service_sessions::SERVICE_SESSION_TYPE
                    && account.enabled
                    && account.owner.is_none()
            }) {
                if sessions.len() >= runtime_collab::MAX_BROWSER_SERVICE_SESSIONS {
                    return Err("Too many selected website sessions".into());
                }
                let Some(binding) = account
                    .config
                    .get("setup_binding")
                    .and_then(serde_json::Value::as_str)
                else {
                    continue;
                };
                // Metadata presence is not proof of usable, current-person ownership.
                let Ok(session) = credential_store.service_session(&account.id, identity, binding)
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
            let id = AccountId::from(id);
            if store.get(&id).is_none() {
                return Err(UNAVAILABLE.into());
            }
            let session = credential_store
                .service_session(&id, identity, &setup_binding)
                .map_err(|_| UNAVAILABLE)?;
            Reply::Resolved { session }
        }
        // Forget is handled before the read snapshot is validated; account secrets are not website
        // sessions.
        Command::Forget { .. }
        | Command::StoreAccountSecret { .. }
        | Command::ReleaseAccountSecret { .. }
        | Command::UseItem { .. }
        | Command::LendsItems {} => return Err(UNAVAILABLE.into()),
    };
    if read_store(root).map_err(|_| UNAVAILABLE)? != store {
        return Err("Selected website sessions changed".into());
    }
    if !reply.valid() {
        return Err(UNAVAILABLE.into());
    }
    Ok(reply)
}

/// Traverse only held directory descriptors; neither missing ownership nor metadata discovery
/// creates, chmods, or follows a redirected namespace.
fn directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::ErrorKind::InvalidInput.into());
    }
    let mut held = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => held = child(&held, name, true)?,
            _ => return Err(io::ErrorKind::InvalidInput.into()),
        }
    }
    owned(&held)?;
    Ok(held)
}

fn child(parent: &File, name: &std::ffi::OsStr, is_directory: bool) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let flags = libc::O_RDONLY
        | libc::O_NOFOLLOW
        | libc::O_CLOEXEC
        | libc::O_NONBLOCK
        | if is_directory { libc::O_DIRECTORY } else { 0 };
    // SAFETY: live held parent and NUL-terminated single-component name; the
    // successful descriptor has unique ownership transferred into File.
    let fd = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn owned(file: &File) -> io::Result<()> {
    let metadata = file.metadata()?;
    if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o022 != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    Ok(())
}

fn read_store(root: &Path) -> io::Result<AccountStore> {
    let root = directory(root)?;
    let accounts = child(&root, std::ffi::OsStr::new("accounts"), true)?;
    owned(&accounts)?;
    let file = child(&accounts, std::ffi::OsStr::new("store.json"), false)?;
    owned(&file)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_STORE {
        return Err(io::ErrorKind::InvalidData.into());
    }
    let mut bytes = Vec::new();
    file.take(MAX_STORE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STORE {
        return Err(io::ErrorKind::InvalidData.into());
    }
    serde_json::from_slice(&bytes).map_err(|_| io::ErrorKind::InvalidData.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn ownership_is_kept_only_for_the_accounts_keeper_and_under_its_own_metadata;

        phase missing_redirected_or_public_metadata_never_creates_or_adopts_ownership;
        {
            use std::os::unix::fs::PermissionsExt as _;
            let root = tempfile::tempdir().unwrap();
            std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
            assert!(resolve(root.path(), [7; 32], Command::List {}).is_err());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
            let external = tempfile::tempdir().unwrap();
            AccountStore::new().save_in(external.path()).unwrap();
            symlink(
                external.path().join("accounts"),
                root.path().join("accounts"),
            )
            .unwrap();
            assert!(read_store(root.path()).is_err());
            std::fs::remove_file(root.path().join("accounts")).unwrap();
            AccountStore::new().save_in(root.path()).unwrap();
            std::fs::set_permissions(
                root.path().join("accounts/store.json"),
                std::fs::Permissions::from_mode(0o666),
            )
            .unwrap();
            assert!(read_store(root.path()).is_err());
            assert!(!root.path().join("accounts/vault").exists());
        }
    }

    use browser_continuity::sessions::{Cookie, SameSite, Service, SessionCookies};
    use std::os::unix::fs::{PermissionsExt, symlink};

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
        let mut store = AccountStore::new();
        store.upsert(account.clone());
        store.save_in(root.path()).unwrap();
        let mut vault = accounts::SecretVault::open_in(root.path());
        vault.put(
            &id,
            &account_credentials::service_sessions::service_session_secret(&session).unwrap(),
        );
        vault.try_save().unwrap();
        let before = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
        let Reply::List { sessions } = resolve(root.path(), [7; 32], Command::List {}).unwrap()
        else {
            panic!("Expected list");
        };
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].id, session.session_id);
        let Reply::Resolved { session: restored } = resolve(
            root.path(),
            [7; 32],
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
                Command::Resolve {
                    id: id.as_str().into(),
                    setup_binding: "prepared-operation".into()
                }
            )
            .is_err()
        );
        account.enabled = false;
        store.upsert(account);
        store.save_in(root.path()).unwrap();
        assert!(
            resolve(
                root.path(),
                [7; 32],
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
        assert!(resolve(root.path(), [8; 32], forget.clone()).is_err());
        assert_eq!(
            std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap(),
            before
        );
        let forgotten = resolve(root.path(), [7; 32], forget.clone()).unwrap();
        assert!(forgotten.matches(&forget));
        assert!(AccountStore::load_in(root.path()).get(&id).is_none());
        assert!(
            resolve(
                root.path(),
                [7; 32],
                Command::Resolve {
                    id: id.as_str().into(),
                    setup_binding: "prepared-operation".into(),
                }
            )
            .is_err()
        );
        assert!(
            resolve(root.path(), [7; 32], forget.clone())
                .unwrap()
                .matches(&forget)
        );
        let Reply::List { sessions } = resolve(root.path(), [7; 32], Command::List {}).unwrap()
        else {
            panic!("Expected list after revocation");
        };
        assert!(sessions.is_empty());
    }
}
