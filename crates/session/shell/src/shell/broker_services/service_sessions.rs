//! Account custody through the certified native route of the app asking.
//!
//! The session is the custodian of the person's account secrets: it alone
//! opens their vault, under session custody, which no app sandbox reaches.
//! On an app's own route it answers:
//!
//! - Browser: its private website sessions, Read or Admin;
//! - Settings, the keeper of the person's accounts: sealing an account's
//!   secret, which it is never released back;
//! - the app that keeps an account's type ([`accounts::person::secret_keeper`],
//!   Mail for email): that one account's secret.
use super::NativeOwnerCommitService;
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
use tilo_action_broker::services::{
    BrowserServiceSessionRequestContext as Context, BrowserServiceSessionServices,
    NativeDocumentReads,
};

const UNAVAILABLE: &str = "Selected website session unavailable";
const BROWSER: &str = "so.tilo.browser";
const MAX_STORE: u64 = 2 * 1024 * 1024;
pub(super) struct NativeBrowserServiceSessions {
    owner: Arc<NativeOwnerCommitService>,
}
impl NativeBrowserServiceSessions {
    pub(super) fn new(owner: Arc<NativeOwnerCommitService>) -> Self {
        Self { owner }
    }
}
impl NativeBrowserServiceSessions {
    /// The certified person's own route of `app`, with Read on that app (or
    /// Admin of the workspace and Outside on it, for `admin`), now and
    /// rechecked after.
    fn authorize_selection(&self, context: &Context, admin: bool, app: &str) -> Result<(), String> {
        context.revalidate()?;
        let person = collab_types::caps::Principal::person_from_key(
            collab_types::crypto::Ed25519PublicKey::from_bytes(context.identity_public_key()),
        );
        if context.app_id() != app || context.person() != person.id() {
            return Err("Account custody requires the certified person's own app".into());
        }
        self.owner.authorize(context.read_context())?;
        let room = super::super::state::RoomId::parse(context.workspace_id())
            .ok_or("Invalid selected Browser room")?;
        let owner = self
            .owner
            .owner
            .opened("Native Browser owner unavailable")?;
        owner.with_current(room, |view| {
            let certificate = context.read_context().certificate();
            if !view.peer().certificates().contains(certificate) {
                return Err("Selected Browser certificate changed".into());
            }
            let now = std::time::SystemTime::now();
            if admin {
                view.peer()
                    .workspace()
                    .check_workspace_administration(certificate.device_public_key, now)?;
            }
            let member = view
                .peer()
                .permissions(certificate.device_public_key, now)?;
            if member.person != context.person() {
                return Err("Selected Browser person changed".into());
            }
            let node = member.policy.node().app(context.app_id());
            let application = tilo_action_policy::Authority::person(member.policy.party_of(
                tilo_action_policy::Subject::App(context.app_id().to_owned()),
            ));
            let kind = if admin {
                tilo_action_policy::ActionKind::Outside
            } else {
                tilo_action_policy::ActionKind::Read
            };
            if !member.allows(&node, kind, now)
                || application
                    .decide(&node, kind, &tilo_action_policy::Context::at_time(now))
                    .answer
                    != tilo_action_policy::Answer::Allow
            {
                return Err("Selected Browser permission changed".into());
            }
            Ok(())
        })?;
        let selected = crate::identity_registry::selected_principal_id(context.workspace_id())
            .map_err(|_| "Selected identity unavailable")?;
        if selected.as_deref() != Some(context.person()) {
            return Err("Selected identity changed".into());
        }
        context.revalidate()
    }
}
impl BrowserServiceSessionServices for NativeBrowserServiceSessions {
    fn authorize(&self, context: &Context) -> Result<(), String> {
        self.authorize_selection(context, false, BROWSER)
    }
    fn authorize_command(&self, context: &Context, command: &Command) -> Result<(), String> {
        match command {
            Command::StoreAccountSecret { .. } => {
                self.authorize_selection(context, false, accounts::person::ACCOUNTS_OWNER)
            }
            // Which app keeps the account is read from the person's
            // accounts when it is resolved; here, only that the route is
            // the person's own, with Read on its app.
            Command::ReleaseAccountSecret { .. } => {
                self.authorize_selection(context, false, context.app_id())
            }
            _ => self.authorize_selection(
                context,
                matches!(command, Command::Forget { .. }),
                BROWSER,
            ),
        }
    }
    fn command(&self, context: &Context, command: Command) -> Result<Reply, String> {
        self.authorize_command(context, &command)?;
        if !command.valid() {
            return Err("Invalid account custody request".into());
        }
        let root = accounts::config_root().ok_or(UNAVAILABLE)?;
        let reply = if command.browser_only() {
            resolve(&root, context.identity_public_key(), command.clone())?
        } else {
            account_secret(&root, context.app_id(), command.clone())?
        };
        self.authorize_command(context, &command)?;
        Ok(reply)
    }
    fn disconnect(&self, _peer_id: u64) {}
}

fn resolve(root: &Path, identity: [u8; 32], command: Command) -> Result<Reply, String> {
    let store = read_store(root).map_err(|_| UNAVAILABLE)?;
    let custodian = account_custody::Custodian::for_root(Some(root.to_owned()));
    if store
        .all()
        .iter()
        .filter(|account| {
            account.type_id.as_str() == account_custody::service_sessions::SERVICE_SESSION_TYPE
        })
        .count()
        > runtime_collab::MAX_BROWSER_SERVICE_SESSIONS
    {
        return Err("Too many selected website sessions".into());
    }
    if let Command::Forget { id, setup_binding } = command {
        custodian
            .forget_service_session(&AccountId::from(id.clone()), identity, &setup_binding)
            .map_err(|_| UNAVAILABLE)?;
        return Ok(Reply::Forgotten { id, setup_binding });
    }
    let reply = match command {
        Command::List => {
            let mut sessions = Vec::new();
            for account in store.all().iter().filter(|account| {
                account.type_id.as_str() == account_custody::service_sessions::SERVICE_SESSION_TYPE
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
                // Metadata presence is not proof of usable, current-person custody.
                let Ok(session) = custodian.service_session(&account.id, identity, binding) else {
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
            let session = custodian
                .service_session(&id, identity, &setup_binding)
                .map_err(|_| UNAVAILABLE)?;
            Reply::Resolved { session }
        }
        // Forget is handled before the read snapshot is validated; account
        // secrets are not website sessions.
        Command::Forget { .. }
        | Command::StoreAccountSecret { .. }
        | Command::ReleaseAccountSecret { .. } => return Err(UNAVAILABLE.into()),
    };
    if read_store(root).map_err(|_| UNAVAILABLE)? != store {
        return Err("Selected website sessions changed".into());
    }
    if !reply.valid() {
        return Err(UNAVAILABLE.into());
    }
    Ok(reply)
}

/// Seal or release one account's secret under `root` for the app `app`
/// ([`accounts::custody`] decides). The secret never enters the person's
/// accounts record or a log.
fn account_secret(root: &Path, app: &str, command: Command) -> Result<Reply, String> {
    match command {
        Command::StoreAccountSecret { account, password } => {
            let mut credentials = accounts::AccountCredentials::open_in(root)?;
            accounts::custody::seal(&mut credentials, app, &account, &password)?;
            tracing::info!(%account, app, "account custody: sealed an account's secret");
            Ok(Reply::AccountSecretStored { account })
        }
        Command::ReleaseAccountSecret { account } => {
            let store = read_store(root).map_err(|_| accounts::custody::REFUSED)?;
            let credentials = accounts::AccountCredentials::open_in(root)?;
            let password = accounts::custody::release(&store, &credentials, app, &account)?;
            tracing::info!(%account, app, "account custody: released an account's secret to the app that keeps its type");
            Ok(Reply::AccountSecret { account, password })
        }
        _ => Err(accounts::custody::REFUSED.into()),
    }
}

/// Traverse only held directory descriptors; neither missing custody nor metadata
/// discovery creates, chmods, or follows a redirected namespace.
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

    /// Settings seals an email account's password, which Settings is never
    /// released; Mail, which keeps email accounts, is released it while the
    /// account is enabled; no other app is, and no other type of account
    /// is released to Mail.
    #[test]
    fn custody_seals_for_settings_and_releases_only_to_the_accounts_keeper() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let store_secret = |app: &str, account: &str| {
            account_secret(
                root.path(),
                app,
                Command::StoreAccountSecret {
                    account: account.into(),
                    password: zeroize::Zeroizing::new("app-password".into()),
                },
            )
        };
        let release = |app: &str, account: &str| {
            account_secret(
                root.path(),
                app,
                Command::ReleaseAccountSecret {
                    account: account.into(),
                },
            )
        };
        assert!(store_secret("so.tilo.mail", "me@example.test").is_err());
        assert!(matches!(
            store_secret("so.tilo.settings", "me@example.test").unwrap(),
            Reply::AccountSecretStored { account } if account == "me@example.test"
        ));
        assert!(matches!(
            store_secret("so.tilo.settings", "dav@example.test").unwrap(),
            Reply::AccountSecretStored { .. }
        ));
        // Sealed, but no account of the person's yet: nothing is released.
        assert!(release("so.tilo.mail", "me@example.test").is_err());

        let mut store = AccountStore::new();
        let mut mail = accounts::Account::new(AccountId::from("me@example.test"), "email");
        store.upsert(mail.clone());
        store.upsert(accounts::Account::new(
            AccountId::from("dav@example.test"),
            "caldav",
        ));
        store.save_in(root.path()).unwrap();
        let Reply::AccountSecret { account, password } =
            release("so.tilo.mail", "me@example.test").unwrap()
        else {
            panic!("Mail is released its email account's secret");
        };
        assert_eq!(account, "me@example.test");
        assert_eq!(password.as_str(), "app-password");
        for (app, account) in [
            ("so.tilo.settings", "me@example.test"),
            ("so.tilo.calendar", "me@example.test"),
            ("so.tilo.notes", "me@example.test"),
            ("so.tilo.mail", "dav@example.test"),
            ("so.tilo.mail", "nobody@example.test"),
        ] {
            assert!(
                release(app, account).is_err(),
                "{app} must not be released {account}"
            );
        }
        mail.enabled = false;
        store.upsert(mail);
        store.save_in(root.path()).unwrap();
        assert!(release("so.tilo.mail", "me@example.test").is_err());
    }
    use browser_continuity::sessions::{Cookie, SameSite, Service, SessionCookies};
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    #[allow(clippy::too_many_lines, reason = "one scenario read top to bottom")]
    fn native_private_custody_lists_and_resolves_only_current_enabled_person() {
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
            &account_custody::service_sessions::service_session_secret(&session).unwrap(),
        );
        vault.try_save().unwrap();
        let before = std::fs::read(root.path().join("accounts/vault/vault.enc")).unwrap();
        let Reply::List { sessions } = resolve(root.path(), [7; 32], Command::List).unwrap() else {
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
        let Reply::List { sessions } = resolve(root.path(), [7; 32], Command::List).unwrap() else {
            panic!("Expected list after revocation");
        };
        assert!(sessions.is_empty());
    }

    #[test]
    fn missing_redirected_or_public_metadata_never_creates_or_adopts_custody() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(resolve(root.path(), [7; 32], Command::List).is_err());
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
