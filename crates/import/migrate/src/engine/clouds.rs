//! Cloud drives as migration sources: Google Drive, `OneDrive`, Dropbox, iCloud Drive, Nextcloud
//! and any `WebDAV` server, read through the providers Files mounts them with
//! ([`tilo_fs::cloud::providers`]) and copied by the files engine ([`super::files`]).
//!
//! Connecting one makes the same account the installer and Settings make
//! ([`account_credentials::CredentialStore`]): Google, Microsoft and Dropbox sign in in the
//! person's browser (one link, which `connect` opens or prints, and which finishes by itself
//! when they approve), iCloud with the Apple ID, its password and the code Apple sends, and a
//! `WebDAV` or Nextcloud server with a password (an app password, for Nextcloud). The account's
//! secret is sealed in the workspace's vault and never printed; the account is kept for import
//! only, so it does not become a synced drive of its own. An account Settings connected, synced
//! or not, is read the same way: the workspace's accounts record names it, its grant is in the
//! vault.
//!
//! A drive is reviewed a folder at a time, a page at a time, each item by the drive's own id for
//! it, and chosen at any depth: a folder brings everything under it. A run's files land in the
//! workspace folder `start` names, and what it could not bring is its journal's `failed`, which a
//! second `start` copies alone.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use account_credentials::{CredentialStore, OAuthCloud};
use accounts::cloud_types::{self, FilesMode};
use accounts::{Account, AccountId};
use anyhow::{Context as _, Result, anyhow, bail};
use cloud_source::{CloudSource, ExportFormat, ItemId, ItemKind};
use serde_json::{Value, json};

use super::Lander;
use super::files::{self, Entry, Tree};
use super::registry::Row as Source;
use super::state::{Item, Place, Problem, Review};

/// The field the account a drive is read as is kept in.
const ACCOUNT: &str = "account";

/// The field the workspace folder a run's files go into is kept in.
const DESTINATION: &str = "destination";

/// How many items a review's page lists when the caller does not say, and at most.
const PAGE: usize = 200;
const MOST_PAGE: usize = 1_000;

/// How many folders up a review names the folder it lists from.
const DEPTH: usize = 64;

/// The account type a source's accounts have.
fn type_of(source: &str) -> Result<&'static str> {
    Ok(match source {
        "gdrive" => cloud_types::GOOGLE_DRIVE,
        "onedrive" => cloud_types::ONEDRIVE,
        "dropbox" => cloud_types::DROPBOX,
        "icloud" => cloud_types::ICLOUD_DRIVE,
        "nextcloud" => cloud_types::NEXTCLOUD,
        "webdav" => cloud_types::WEBDAV,
        other => bail!("{other} is not a cloud drive"),
    })
}

fn config_root() -> Result<std::path::PathBuf> {
    accounts::config_root().ok_or_else(|| anyhow!("this account has no Tilo config directory"))
}

/// The credential store accounts are signed in and refreshed with.
fn credentials(root: &Path) -> CredentialStore {
    CredentialStore::for_root(Some(root.to_path_buf())).with_apple_icloud()
}

/// `tilo migrate connect <drive>`.
///
/// # Errors
/// What the person typed or approved did not sign in; the error says what to do.
pub fn connect(cx: &super::Cx<'_>, source: &Source, how: super::Connecting<'_>) -> Result<Value> {
    let place = cx.place;
    let super::Connecting {
        url,
        login,
        secret_file,
    } = how;
    let root = config_root()?;
    let store = credentials(&root);
    let type_id = type_of(source.key)?;
    let (mut account, secret) = if let Some(cloud) = OAuthCloud::from_type_id(type_id) {
        sign_in(&store, cloud, source, login)?
    } else if type_id == cloud_types::ICLOUD_DRIVE {
        icloud(cx, &store, login, secret_file)?
    } else {
        password_account(cx, source, type_id, url, login, secret_file)?
    };
    cloud_types::set_files_mode(&mut account.config, FilesMode::Import);
    let mut workspace = cx.open()?;
    let id = crate::vault::persist::<anyhow::Error>(&root, &account, secret)?;
    let saving = accounts::person::AccountList::saving(&account);
    super::writer::change_accounts(&mut *workspace, (&saving.address(), &saving.arguments))?;
    place.keep_field(source.key, ACCOUNT, &id.as_str())?;
    Ok(json!({
        "source": source.key,
        "account": id.as_str(),
        "said": format!(
            "{} is connected (its sign-in is in this computer's vault); next: tilo migrate review {}",
            source.title, source.key
        ),
    }))
}

/// Sign in to `cloud` in the person's browser: the link is opened, or printed when no browser
/// opens, and the sign-in finishes by itself when they approve.
fn sign_in(
    store: &CredentialStore,
    cloud: OAuthCloud,
    source: &Source,
    login: Option<&str>,
) -> Result<(Account, accounts::Secret)> {
    if !store.is_configured(cloud.oauth_use()) {
        bail!(
            "this Tilo has no {} sign-in registered, so it can't connect. A build with Tilo's \
             registrations has one; a private build names its own with TILO_OAUTH_*_CLIENT_ID",
            source.title
        );
    }
    let authorization = store
        .begin_sign_in(cloud, login)
        .map_err(|error| anyhow!("{} sign-in didn't start: {error}", source.title))?;
    let link = authorization.url().to_owned();
    eprintln!(
        "Sign in to {} in your browser; this finishes by itself once you approve:\n  {link}",
        source.title
    );
    open_browser(&link);
    let tokens = CredentialStore::finish_sign_in(authorization).map_err(|error| {
        anyhow!(
            "{} sign-in didn't finish ({error}). Start `tilo migrate connect {}` again and approve in the browser",
            source.title,
            source.key
        )
    })?;
    store
        .cloud_account(cloud, source.title, &tokens)
        .map_err(|error| anyhow!("{} didn't give access to its files: {error}", source.title))
}

/// Open `link` in the person's browser, when this computer has one to open.
fn open_browser(link: &str) {
    if std::env::var_os("TILO_NO_BROWSER").is_some() {
        return;
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(link)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Sign in to iCloud with the Apple ID, its password and, when Apple asks, the code it sent.
fn icloud(
    cx: &super::Cx<'_>,
    store: &CredentialStore,
    login: Option<&str>,
    secret_file: Option<&Path>,
) -> Result<(Account, accounts::Secret)> {
    use account_credentials::icloud::ICloudSignIn;
    let apple_id = login
        .map(str::trim)
        .filter(|login| !login.is_empty())
        .ok_or_else(|| anyhow!("iCloud is named by its Apple ID: --login you@icloud.com"))?;
    if !store.icloud().is_available() {
        bail!("this Tilo can't sign in to iCloud");
    }
    let password = cx.secret(
        &format!("Password of {apple_id} (it is not shown): "),
        secret_file,
    )?;
    let session = match store
        .icloud()
        .sign_in(apple_id, &password)
        .map_err(|error| anyhow!("iCloud didn't sign in: {error}"))?
    {
        ICloudSignIn::Complete(session) => session,
        ICloudSignIn::NeedsCode(challenge) => {
            let code = cx.secret("The code Apple sent to your devices: ", None)?;
            store
                .icloud()
                .verify_code(&challenge, &code)
                .map_err(|error| anyhow!("iCloud didn't take the code: {error}"))?
        }
    };
    store
        .icloud_account(apple_id, &session)
        .map_err(|error| anyhow!("{error}"))
}

/// A `WebDAV` or Nextcloud account, checked by listing its files before it is kept.
fn password_account(
    cx: &super::Cx<'_>,
    source: &Source,
    type_id: &str,
    url: Option<&str>,
    login: Option<&str>,
    secret_file: Option<&Path>,
) -> Result<(Account, accounts::Secret)> {
    let server = url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "{} is reached at its address: --url https://…",
                source.title
            )
        })?;
    if !cloud_source::is_secure_endpoint(server) {
        bail!("{server} is not an https:// address (plain http is only for this computer)");
    }
    let username = login.unwrap_or_default().trim().to_owned();
    if type_id == cloud_types::NEXTCLOUD && username.is_empty() {
        bail!("Nextcloud is signed in to with your user name: --login you");
    }
    let what = if type_id == cloud_types::NEXTCLOUD {
        "App password (Nextcloud: Settings → Security → Devices & sessions; it is not shown): "
    } else {
        "Password (it is not shown): "
    };
    let password = cx.secret(what, secret_file)?;
    if password.is_empty() {
        bail!("nothing was typed");
    }
    let mut account = Account::new(AccountId::new(uuid::Uuid::new_v4().to_string()), type_id);
    source.title.clone_into(&mut account.display_name);
    account.config = json!({"server_url": server, "username": username, "use_files": true});
    cloud_types::set_files_mode(&mut account.config, FilesMode::Import);
    let checked = open_with(
        &account,
        Arc::new(cloud_source::StaticCredential(
            cloud_source::Credential::Basic {
                username,
                password: password.clone(),
            },
        )),
    )?;
    checked.list(&checked.root()).map_err(|error| {
        anyhow!(
            "{} didn't list its files ({error}): check the address, the user name and the password",
            source.title
        )
    })?;
    Ok((account, accounts::Secret::password(password.to_string())))
}

/// The account a migration of `source` reads: `named` when the caller names one, else the one a
/// review of it read last, else the first of its type the workspace's accounts record keeps with
/// its grant in the workspace's vault, as Settings and `connect` keep it. Nothing signs in here:
/// an account that is not connected says how to connect it.
fn connected_account(cx: &super::Cx<'_>, source: &Source, named: Option<&str>) -> Result<Account> {
    let record = super::writer::accounts(&mut *cx.open()?)?;
    let type_id = type_of(source.key)?;
    let kept: Option<String> = match named {
        Some(id) => Some(id.to_owned()),
        None => cx.place.field(source.key, ACCOUNT)?,
    };
    let vault = accounts::SecretVault::open_in(&config_root()?);
    let mut of_type = record.of_type(type_id).into_iter();
    let found = match &kept {
        Some(id) => of_type.find(|account| account.id.as_str() == id),
        None => of_type.find(|account| account.enabled && vault.get(&account.id).is_some()),
    };
    found.ok_or_else(|| {
        anyhow!(
            "{} is not connected: connect it in Settings, or {}",
            source.title,
            source
                .connect_command()
                .unwrap_or_else(|| format!("tilo migrate connect {}", source.key))
        )
    })
}

/// The drive `account` names, reached with `credentials`.
#[cfg(target_os = "linux")]
pub(crate) fn open_with(
    account: &Account,
    credentials: Arc<dyn cloud_source::CredentialSource>,
) -> Result<Arc<dyn CloudSource>> {
    let spec = tilo_fs::cloud::providers::source_spec(account)
        .ok_or_else(|| anyhow!("the account {} has no files to read", account.display_name))?;
    tilo_fs::cloud::providers::open(&spec, credentials)
        .map_err(|error| anyhow!("{} couldn't be opened: {error}", account.display_name))
}

/// The drive `account` names: Files' providers are Linux's for now.
#[cfg(not(target_os = "linux"))]
pub(crate) fn open_with(
    account: &Account,
    _: Arc<dyn cloud_source::CredentialSource>,
) -> Result<Arc<dyn CloudSource>> {
    bail!(
        "{} comes over from a Linux computer for now",
        account.display_name
    )
}

/// The drive the connected `source` is, reached with the grant the vault at this computer's
/// root keeps for `account`: a password as it is, a provider's grant renewed as the accounts
/// provider renews it, the renewed sign-in kept back in that vault.
fn open(account: &Account, source: &Source) -> Result<Arc<dyn CloudSource>> {
    let root = config_root()?;
    let secret = accounts::SecretVault::open_in(&root)
        .get(&account.id)
        .ok_or_else(|| {
            anyhow!(
                "{} has no sign-in in this computer's vault: sign it in again in Settings",
                source.title
            )
        })?;
    if matches!(
        account.type_id.as_str(),
        cloud_types::WEBDAV | cloud_types::NEXTCLOUD
    ) {
        let username = account
            .config
            .get("username")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        return open_with(
            account,
            Arc::new(cloud_source::StaticCredential(
                cloud_source::Credential::Basic {
                    username,
                    password: zeroize::Zeroizing::new(secret.password),
                },
            )),
        );
    }
    open_with(
        account,
        Arc::new(GrantDrive {
            store: credentials(&root),
            root,
            account: account.clone(),
            secret: Mutex::new(secret),
            rejected: std::sync::atomic::AtomicBool::new(false),
        }),
    )
}

/// A drive's sign-in over the grant the vault at `root` keeps for `account`: made by the one
/// credential store, renewed when due or after the drive refused it, and the renewed grant
/// kept back in that vault.
struct GrantDrive {
    store: CredentialStore,
    root: std::path::PathBuf,
    account: Account,
    secret: Mutex<accounts::Secret>,
    rejected: std::sync::atomic::AtomicBool,
}

impl cloud_source::CredentialSource for GrantDrive {
    fn credential(&self) -> cloud_source::Result<cloud_source::Credential> {
        let secret = self
            .secret
            .lock()
            .map_err(|_| cloud_source::Error::Unauthorized)?
            .clone();
        let force = self
            .rejected
            .swap(false, std::sync::atomic::Ordering::SeqCst);
        let made = self
            .store
            .drive_credential(&self.account, &secret, force)
            .map_err(|error| match error {
                account_credentials::CredentialError::Offline => cloud_source::Error::Offline,
                _ => cloud_source::Error::Unauthorized,
            })?;
        if let Some(renewed) = made.renewed {
            let mut vault = accounts::SecretVault::open_in(&self.root);
            vault.put(&self.account.id, &renewed);
            if let Err(error) = vault.try_save() {
                return Err(cloud_source::Error::Io(std::io::Error::other(error)));
            }
            if let Ok(mut kept) = self.secret.lock() {
                *kept = renewed;
            }
        }
        made.credential.ok_or(cloud_source::Error::Unauthorized)
    }

    fn rejected(&self) {
        self.rejected
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Drives tests read in place of a provider, by the id of the account they stand for.
#[cfg(test)]
pub(crate) static FAKES: std::sync::LazyLock<Mutex<HashMap<String, Arc<dyn CloudSource>>>> =
    std::sync::LazyLock::new(Mutex::default);

/// What a drive's error means for the person: sign in again, or try later.
fn said(source: &Source, error: &cloud_source::Error) -> anyhow::Error {
    match error {
        cloud_source::Error::Unauthorized | cloud_source::Error::Forbidden => anyhow!(
            "{} wants you to sign in again: {}",
            source.title,
            source
                .connect_command()
                .unwrap_or_else(|| format!("tilo migrate connect {}", source.key))
        ),
        other if other.is_transient() => anyhow!(
            "{} isn't answering right now ({other}); start again in a while to resume",
            source.title
        ),
        other => anyhow!("{}: {other}", source.title),
    }
}

/// A connected drive as a tree of files, each item by the drive's own id for it.
pub struct Drive {
    source: Arc<dyn CloudSource>,
    named: &'static Source,
    /// The documents that live only in the provider (Google Docs), by id, and the format each
    /// comes over as.
    exports: Mutex<HashMap<String, ExportFormat>>,
}

impl Drive {
    /// The connected `named`, as `account` (by its id), or the account its migration reads.
    ///
    /// # Errors
    /// It is not connected.
    pub fn open(cx: &super::Cx<'_>, named: &'static Source, account: Option<&str>) -> Result<Self> {
        #[cfg(test)]
        {
            let kept: Option<String> = match account {
                Some(id) => Some(id.to_owned()),
                None => cx.place.field(named.key, ACCOUNT)?,
            };
            let fake = kept.and_then(|id| {
                FAKES
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(&id)
                    .cloned()
            });
            if let Some(source) = fake {
                return Self::of(source, named);
            }
        }
        Self::of(open(&connected_account(cx, named, account)?, named)?, named)
    }

    /// The drive `source` is.
    ///
    /// # Errors
    /// None yet: a drive is read when it is listed.
    pub fn of(source: Arc<dyn CloudSource>, named: &'static Source) -> Result<Self> {
        Ok(Self {
            source,
            named,
            exports: Mutex::new(HashMap::new()),
        })
    }

    /// The name a file of the drive keeps here: its own, or its export's for a document that
    /// lives only in the provider; `None` for one that cannot come over.
    fn name_of(&self, item: &cloud_source::Item) -> Option<String> {
        match &item.kind {
            ItemKind::File | ItemKind::Folder => Some(safe_name(&item.name)),
            ItemKind::Native(document) => {
                let format = cloud_source::materialize::preferred_export(document).ok()?;
                self.exports
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(item.id.as_str().to_owned(), format.clone());
                Some(format!("{}.{}", safe_name(&item.name), format.extension))
            }
        }
    }

    /// Every file under the folder `folder`, named from `at` down (nothing for the drive's own
    /// top); what could not be listed or exported is answered in `unread`.
    fn walk(&self, folder: ItemId, at: String, found: &mut Vec<Entry>, unread: &mut Vec<Problem>) {
        let mut walking = vec![(folder, at)];
        while let Some((id, path)) = walking.pop() {
            let children = match self.source.list(&id) {
                Ok(children) => children,
                Err(error) => {
                    unread.push(Problem {
                        key: id.as_str().to_owned(),
                        name: if path.is_empty() {
                            self.named.title.to_owned()
                        } else {
                            path.clone()
                        },
                        why: said(self.named, &error).to_string(),
                    });
                    continue;
                }
            };
            for child in children {
                let Some(name) = self.name_of(&child) else {
                    unread.push(Problem {
                        key: child.id.as_str().to_owned(),
                        name: joined(&path, &child.name),
                        why: format!("{} can't export this kind of document", self.named.title),
                    });
                    continue;
                };
                let at = joined(&path, &name);
                if child.is_folder() {
                    walking.push((child.id.clone(), at));
                } else {
                    found.push(entry(&child, at));
                }
            }
        }
    }

    /// One item as a review lists it.
    fn listed(item: &cloud_source::Item) -> Value {
        let (kind, export_as) = match &item.kind {
            ItemKind::Folder => ("folder", None),
            ItemKind::File => ("file", None),
            ItemKind::Native(document) => (
                "document",
                cloud_source::materialize::preferred_export(document)
                    .ok()
                    .map(|format| format.extension),
            ),
        };
        let mut listed = serde_json::Map::new();
        listed.insert("key".into(), json!(item.id.as_str()));
        listed.insert("name".into(), json!(item.name));
        listed.insert("kind".into(), json!(kind));
        if let Some(size) = item.size.filter(|_| item.is_file()) {
            listed.insert("size".into(), size.into());
        }
        if let Some(modified) = item
            .modified
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        {
            listed.insert("modified".into(), modified.as_secs().into());
        }
        if let Some(extension) = export_as {
            listed.insert("export_as".into(), extension.into());
        }
        Value::Object(listed)
    }

    /// The folders from the drive's top down to `folder`, each by its key and name.
    fn path_to(&self, folder: &ItemId) -> Vec<Value> {
        let root = self.source.root();
        let mut path = Vec::new();
        let mut at = folder.clone();
        for _ in 0..DEPTH {
            if at == root {
                break;
            }
            let Ok(item) = self.source.stat(&at) else {
                break;
            };
            path.push(json!({"key": item.id.as_str(), "name": item.name}));
            match item.parents.first() {
                Some(parent) => at = parent.clone(),
                None => break,
            }
        }
        path.push(json!({"key": root.as_str(), "name": self.named.title}));
        path.reverse();
        path
    }

    /// The key of the item at `path` from the drive's top, each part its name on the drive or
    /// the name its export has here.
    fn resolve(&self, path: &str) -> Result<String> {
        let mut at = self.source.root();
        for part in path.split('/').filter(|part| !part.is_empty()) {
            let children = self
                .source
                .list(&at)
                .map_err(|error| said(self.named, &error))?;
            at = children
                .into_iter()
                .find(|child| child.name == part || self.name_of(child).as_deref() == Some(part))
                .map(|child| child.id)
                .ok_or_else(|| anyhow!("{} holds no {path}", self.named.title))?;
        }
        Ok(at.0)
    }

    /// The item `key` names, as a review keeps it once chosen.
    fn chosen(&self, key: &str) -> Result<Item> {
        let root = self.source.root();
        if key == root.as_str() {
            return Ok(Item {
                key: key.to_owned(),
                kind: "folder".to_owned(),
                title: self.named.title.to_owned(),
                count: 0,
                counts: "everything".to_owned(),
                within: None,
                chosen: true,
                note: None,
            });
        }
        let item = self
            .source
            .stat(&ItemId::new(key))
            .map_err(|error| said(self.named, &error))?;
        let title = self.name_of(&item).unwrap_or_else(|| item.name.clone());
        Ok(Item {
            key: key.to_owned(),
            kind: match item.kind {
                ItemKind::Folder => "folder",
                ItemKind::File => "file",
                ItemKind::Native(_) => "document",
            }
            .to_owned(),
            title,
            count: usize::from(!item.is_folder()),
            counts: item
                .size
                .filter(|_| item.is_file())
                .map_or_else(String::new, super::size),
            within: None,
            chosen: true,
            note: None,
        })
    }
}

/// `name` under the path `path` (the drive's top when empty).
fn joined(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_owned()
    } else {
        format!("{path}/{name}")
    }
}

fn entry(item: &cloud_source::Item, path: String) -> Entry {
    Entry {
        path,
        size: item.size.unwrap_or(0),
        modified: item.modified,
        id: item.id.as_str().to_owned(),
    }
}

/// A provider's name as a file's name here: no separators, never `.` or `..`.
fn safe_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c == '/' || c == '\0' { '_' } else { c })
        .collect();
    match cleaned.as_str() {
        "" | "." | ".." => "_".to_owned(),
        _ => cleaned,
    }
}

impl Tree for Drive {
    /// Everything `key` (a drive's item id) holds: a file, or every file under a folder, which
    /// lands in a folder of its name. The drive's top lands its contents directly.
    fn files(&self, key: &str) -> Result<(Vec<Entry>, Vec<Problem>)> {
        let mut found = Vec::new();
        let mut unread = Vec::new();
        let root = self.source.root();
        if key == root.as_str() {
            self.walk(root, String::new(), &mut found, &mut unread);
            return Ok((found, unread));
        }
        let item = self
            .source
            .stat(&ItemId::new(key))
            .map_err(|error| said(self.named, &error))?;
        match self.name_of(&item) {
            None => unread.push(Problem {
                key: key.to_owned(),
                name: item.name.clone(),
                why: format!("{} can't export this kind of document", self.named.title),
            }),
            Some(name) if item.is_folder() => {
                self.walk(item.id.clone(), name, &mut found, &mut unread);
            }
            Some(name) => found.push(entry(&item, name)),
        }
        Ok((found, unread))
    }

    fn copy_to(&self, entry: &Entry, to: &Path) -> Result<()> {
        let id = ItemId::new(entry.id.clone());
        let export = self
            .exports
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&entry.id)
            .cloned();
        let download = match export {
            Some(format) => self.source.export(&id, &format),
            None => self.source.read(&id, None, None),
        }
        .map_err(|error| said(self.named, &error))?;
        let mut body = download.body;
        let mut file =
            std::fs::File::create(to).with_context(|| format!("writing {}", to.display()))?;
        std::io::copy(&mut body, &mut file).with_context(|| format!("fetching {}", entry.path))?;
        Ok(())
    }
}

/// What a review of a drive is asked: one folder (its top when `within` is not given), a page at
/// a time.
#[derive(Clone, Copy, Debug, Default)]
pub struct Page<'a> {
    /// The connected account to read, by its id; the one read last when not given.
    pub account: Option<&'a str>,
    /// The folder, by the key a review listed it under.
    pub within: Option<&'a str>,
    /// Where the page starts: the `next` the page before answered.
    pub cursor: Option<&'a str>,
    pub limit: Option<usize>,
}

/// `review` of a drive: one folder's folders, files and documents, a page at a time, each by the
/// drive's own id for it, with the folders above it.
///
/// # Errors
/// It is not connected, did not answer, or the cursor is not one a page answered.
pub fn review(cx: &super::Cx<'_>, named: &'static Source, page: Page<'_>) -> Result<Value> {
    let drive = Drive::open(cx, named, page.account)?;
    if let Some(account) = page.account {
        cx.place.keep_field(named.key, ACCOUNT, &account)?;
    }
    let review = if let Some(review) = cx.place.review(named.key)? {
        review
    } else {
        let review = empty(named);
        cx.place.keep_review(&review)?;
        review
    };
    let folder = page.within.map_or_else(|| drive.source.root(), ItemId::new);
    let mut children = drive
        .source
        .list(&folder)
        .map_err(|error| said(named, &error))?;
    // Folders first, then by name: the same order every page, so a cursor stays where it was.
    children.sort_by(|a, b| {
        b.is_folder()
            .cmp(&a.is_folder())
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
    let offset = match page.cursor {
        Some(cursor) => cursor
            .parse::<usize>()
            .map_err(|_| anyhow!("{cursor:?} is not a cursor a review answered"))?,
        None => 0,
    };
    let limit = page.limit.unwrap_or(PAGE).clamp(1, MOST_PAGE);
    let end = offset.saturating_add(limit).min(children.len());
    let items: Vec<Value> = children
        .get(offset.min(end)..end)
        .unwrap_or_default()
        .iter()
        .map(Drive::listed)
        .collect();
    let journal = cx.place.journal(named.key)?;
    Ok(json!({
        "source": named.key,
        "title": named.title,
        "path": drive.path_to(&folder),
        "items": items,
        "next": (end < children.len()).then(|| end.to_string()),
        "chosen": review.chosen().iter().map(|item| item.key.as_str()).collect::<Vec<_>>(),
        "state": journal.state,
    }))
}

/// A drive's review before anything is chosen.
fn empty(named: &'static Source) -> Review {
    Review {
        source: named.key.to_owned(),
        title: named.title.to_owned(),
        handle: named.key.to_owned(),
        items: Vec::new(),
        unreadable: Vec::new(),
    }
}

/// A key that names an item by its path from the drive's top (`path:Work/Essay.docx`), as a
/// synced drive's folder shows it, rather than by its id.
const BY_PATH: &str = "path:";

/// `choose` of a drive: items of any depth by their keys, files and folders mixed; a folder brings
/// everything under it, and the drive's top (the first key of a review's `path`) all of it. An
/// item may be named by its path (`path:Work/Essay.docx`), and is kept by its id.
///
/// # Errors
/// A key names nothing on the drive, or the choice could not be kept.
pub fn choose(
    cx: &super::Cx<'_>,
    named: &'static Source,
    keys: &[String],
    leave_out: bool,
    only: bool,
) -> Result<Value> {
    let mut review = cx.place.review(named.key)?.unwrap_or_else(|| empty(named));
    if only {
        for item in &mut review.items {
            item.chosen = false;
        }
    }
    let mut drive: Option<Drive> = None;
    for key in keys {
        let by_path = key.strip_prefix(BY_PATH);
        if by_path.is_none()
            && let Some(item) = review.items.iter_mut().find(|item| item.key == *key)
        {
            item.chosen = !leave_out;
            continue;
        }
        let opened = match drive.take() {
            Some(opened) => opened,
            None => Drive::open(cx, named, None)?,
        };
        let key = match by_path {
            Some(path) => opened.resolve(path)?,
            None => key.clone(),
        };
        if let Some(item) = review.items.iter_mut().find(|item| item.key == key) {
            item.chosen = !leave_out;
        } else if !leave_out {
            review.items.push(opened.chosen(&key)?);
        }
        drive = Some(opened);
    }
    review.items.retain(|item| item.chosen);
    cx.place.keep_review(&review)?;
    Ok(super::review_answer(&review, &cx.place.journal(named.key)?))
}

/// Read `named` as the connected account `account` from now on.
///
/// # Errors
/// It could not be kept.
pub fn keep_account(place: &Place, named: &Source, account: &str) -> Result<()> {
    place.keep_field(named.key, ACCOUNT, &account)
}

/// The workspace folder `given` names for a run's files, as a path inside the workspace's files;
/// a folder named after the drive when not given.
///
/// # Errors
/// It is not a path inside the workspace.
pub fn destination(named: &Source, given: Option<&str>) -> Result<String> {
    let Some(given) = given.map(|given| given.trim_matches('/')) else {
        return Ok(named.title.to_owned());
    };
    let parts: Vec<&str> = given.split('/').collect();
    if given.is_empty() || parts.iter().any(|part| matches!(*part, "" | "." | "..")) {
        bail!(
            "{given:?} is not a folder in the workspace: name it by its path, like Documents/Trip"
        );
    }
    Ok(parts.join("/"))
}

/// Keep `destination` as where the next run of `named` puts its files.
///
/// # Errors
/// It could not be kept.
pub fn keep_destination(place: &Place, named: &Source, destination: &str) -> Result<()> {
    place.keep_field(named.key, DESTINATION, &destination)
}

/// `sink`, which `start` made for the folder named after the drive, moved to `destination`.
fn moved(sink: &files::Sink, title: &str, destination: &str) -> files::Sink {
    match sink {
        files::Sink::Folder(path) => {
            let base = if path.ends_with(title) {
                path.parent()
                    .map_or_else(|| path.clone(), Path::to_path_buf)
            } else {
                path.clone()
            };
            files::Sink::Folder(
                destination
                    .split('/')
                    .fold(base, |path, part| path.join(part)),
            )
        }
        files::Sink::Upload(_) => files::Sink::Upload(destination.to_owned()),
    }
}

/// Bring the chosen items of the connected drive into the workspace's files, under the folder
/// `start` was given.
///
/// # Errors
/// The migration was stopped, the drive or the workspace refused.
pub fn land(
    cx: &super::Cx<'_>,
    named: &'static Source,
    review: &Review,
    sink: &files::Sink,
    lander: &mut Lander<'_>,
) -> Result<()> {
    let drive = Drive::open(cx, named, None)?;
    let destination: String = cx
        .place
        .field(named.key, DESTINATION)?
        .unwrap_or_else(|| named.title.to_owned());
    lander.journal.destination = Some(destination.clone());
    files::land(
        &drive,
        review,
        &moved(sink, named.title, &destination),
        cx.place,
        lander,
    )
}

/// A cloud drive.
pub struct Clouds;

impl super::Source for Clouds {
    fn here(&self, cx: &super::Cx<'_>, row: &'static Source) -> super::Here {
        super::Here {
            next: if cx
                .place
                .field::<String>(row.key, ACCOUNT)
                .is_ok_and(|kept| kept.is_some())
            {
                Some(format!("tilo migrate review {}", row.key))
            } else {
                row.connect_command()
            },
            ..super::Here::default()
        }
    }

    fn connect(
        &self,
        cx: &super::Cx<'_>,
        row: &'static Source,
        how: super::Connecting<'_>,
    ) -> Result<Value> {
        connect(cx, row, how)
    }

    /// The drive's top level, each by its key, keeping what was chosen.
    fn review(&self, cx: &super::Cx<'_>, row: &'static Source, _: Option<&str>) -> Result<Review> {
        let drive = Drive::open(cx, row, None)?;
        let kept = cx.place.review(row.key)?;
        let mut review = kept.unwrap_or_else(|| empty(row));
        let top = drive
            .source
            .list(&drive.source.root())
            .map_err(|error| said(row, &error))?;
        for item in top {
            let key = item.id.as_str();
            if review.items.iter().any(|kept| kept.key == key) {
                continue;
            }
            let mut listed = drive.chosen(key)?;
            listed.chosen = false;
            review.items.push(listed);
        }
        Ok(review)
    }

    fn summary(&self, row: &'static Source, review: &Review) -> String {
        let names: Vec<&str> = review
            .chosen()
            .iter()
            .map(|item| item.title.as_str())
            .collect();
        format!("Bring {} from {}", names.join(", "), row.title)
    }

    fn files_under(&self, row: &'static Source) -> Option<&'static str> {
        Some(row.title)
    }

    fn land(
        &self,
        cx: &super::Cx<'_>,
        row: &'static Source,
        review: &Review,
        sink: Option<&files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let sink = sink.ok_or_else(|| anyhow!("files have nowhere to go"))?;
        land(cx, row, review, sink, lander)
    }

    fn landed(
        &self,
        _: &super::Cx<'_>,
        _: &'static Source,
        journal: &super::state::Journal,
    ) -> Result<usize> {
        // Files are counted as the last run found them: brought, or there already.
        Ok(journal
            .brought
            .values()
            .sum::<usize>()
            .saturating_add(journal.already_there))
    }
}
