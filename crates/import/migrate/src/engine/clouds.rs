//! Cloud drives as migration sources: Google Drive, `OneDrive`, Dropbox, iCloud Drive, Nextcloud
//! and any `WebDAV` server, read through the providers Files mounts them with
//! ([`tilo_fs::cloud::providers`]) and copied by the files engine ([`super::files`]).
//!
//! Connecting one makes the same account the installer and Settings make
//! ([`account_credentials::CredentialStore`]): Google, Microsoft and Dropbox sign in in the
//! person's browser (one link, which `connect` opens or prints, and which finishes by itself
//! when they approve), iCloud with the Apple ID, its password and the code Apple sends, and a
//! `WebDAV` or Nextcloud server with a password (an app password, for Nextcloud). The account's
//! secret is sealed in this computer's vault and never printed; the account is kept for import
//! only, so it does not become a synced drive of its own.

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
use super::state::{Item, Place, Review};

/// The field the account a drive was connected with is kept in.
const ACCOUNT: &str = "account";

/// The most folders a review lists to count what a drive holds; past it, counts are "at least".
const REVIEW_FOLDERS: usize = 400;

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

/// The account `source` was connected with last.
fn account(cx: &super::Cx<'_>, source: &Source) -> Result<Account> {
    let place = cx.place;
    let record = super::writer::accounts(&mut *cx.open()?)?;
    let type_id = type_of(source.key)?;
    let kept: Option<String> = place.field(source.key, ACCOUNT)?;
    record
        .of_type(type_id)
        .into_iter()
        .find(|account| kept.as_ref().is_none_or(|id| account.id.as_str() == id))
        .ok_or_else(|| {
            anyhow!(
                "{} is not connected: {}",
                source.title,
                source
                    .connect_command()
                    .unwrap_or_else(|| format!("tilo migrate connect {}", source.key))
            )
        })
}

/// The drive `account` names, reached with `credentials`.
#[cfg(target_os = "linux")]
pub(super) fn open_with(
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
pub(super) fn open_with(
    account: &Account,
    _: Arc<dyn cloud_source::CredentialSource>,
) -> Result<Arc<dyn CloudSource>> {
    bail!(
        "{} comes over from a Linux computer for now",
        account.display_name
    )
}

/// The drive the connected `source` is, reached with the credential the vault keeps, refreshed
/// as Files refreshes it.
fn open(cx: &super::Cx<'_>, source: &Source) -> Result<Arc<dyn CloudSource>> {
    let account = account(cx, source)?;
    let root = config_root()?;
    let credential = account_credentials::DriveSignIn::new(credentials(&root), account.clone());
    open_with(&account, Arc::new(credential))
}

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

/// A connected drive as a tree of files: its top-level entries are what a review lists.
pub struct Drive {
    source: Arc<dyn CloudSource>,
    named: &'static Source,
    top: Vec<cloud_source::Item>,
    /// The documents that live only in the provider (Google Docs), by id, and the format each
    /// comes over as.
    exports: Mutex<HashMap<String, ExportFormat>>,
}

impl Drive {
    /// The connected `source`, with its top level read.
    ///
    /// # Errors
    /// It is not connected, or did not answer.
    pub fn open(cx: &super::Cx<'_>, named: &'static Source) -> Result<Self> {
        Self::of(open(cx, named)?, named)
    }

    /// The drive `source` is, with its top level read.
    ///
    /// # Errors
    /// It did not answer.
    pub fn of(source: Arc<dyn CloudSource>, named: &'static Source) -> Result<Self> {
        let top = source
            .list(&source.root())
            .map_err(|error| said(named, &error))?;
        Ok(Self {
            source,
            named,
            top,
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

    /// Walk `top` to its files, listing at most `folders` folders; whether it stopped early.
    fn walk(
        &self,
        top: &cloud_source::Item,
        folders: usize,
        found: &mut Vec<Entry>,
        unread: &mut Vec<String>,
    ) -> bool {
        let Some(name) = self.name_of(top) else {
            unread.push(format!("{} can't be exported", top.name));
            return false;
        };
        if !top.is_folder() {
            found.push(entry(top, name));
            return false;
        }
        let mut listed = 0_usize;
        let mut walking = vec![(top.id.clone(), name)];
        while let Some((id, path)) = walking.pop() {
            if listed >= folders {
                return true;
            }
            listed = listed.saturating_add(1);
            let children = match self.source.list(&id) {
                Ok(children) => children,
                Err(error) => {
                    unread.push(format!("{path}: {error}"));
                    continue;
                }
            };
            for child in children {
                let Some(name) = self.name_of(&child) else {
                    unread.push(format!("{path}/{}: can't be exported", child.name));
                    continue;
                };
                let at = format!("{path}/{name}");
                if child.is_folder() {
                    walking.push((child.id.clone(), at));
                } else {
                    found.push(entry(&child, at));
                }
            }
        }
        false
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
    fn files(&self, key: &str) -> Result<(Vec<Entry>, Vec<String>)> {
        let top = self
            .top
            .iter()
            .find(|item| self.name_of(item).as_deref() == Some(key))
            .ok_or_else(|| anyhow!("{} no longer holds {key}", self.named.title))?;
        let mut found = Vec::new();
        let mut unread = Vec::new();
        self.walk(top, usize::MAX, &mut found, &mut unread);
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

/// `tilo migrate review <drive>`: its top-level folders and files, each counted.
///
/// # Errors
/// It is not connected, or did not answer.
pub fn review(cx: &super::Cx<'_>, named: &'static Source) -> Result<Review> {
    let drive = Drive::open(cx, named)?;
    let kept = cx.place.review(named.key)?;
    let mut items = Vec::new();
    let mut unreadable = Vec::new();
    // Each top-level entry gets an equal share of the folders a review lists; a drive with no
    // top-level entry at all gets them all.
    let budget = REVIEW_FOLDERS
        .checked_div(drive.top.len())
        .unwrap_or(REVIEW_FOLDERS);
    for top in &drive.top {
        let Some(name) = drive.name_of(top) else {
            unreadable.push(format!("{} can't be exported", top.name));
            continue;
        };
        let mut found = Vec::new();
        let partial = drive.walk(top, budget.max(1), &mut found, &mut unreadable);
        let bytes: u64 = found.iter().map(|entry| entry.size).sum();
        items.push(Item {
            key: name.clone(),
            kind: if top.is_folder() { "folder" } else { "file" }.to_owned(),
            title: name.clone(),
            count: found.len(),
            counts: format!(
                "{}files, {}",
                if partial { "or more " } else { "" },
                super::size(bytes)
            ),
            within: None,
            chosen: kept
                .as_ref()
                .is_some_and(|kept| kept.items.iter().any(|old| old.key == name && old.chosen)),
            note: None,
        });
    }
    Ok(Review {
        source: named.key.to_owned(),
        title: named.title.to_owned(),
        handle: named.key.to_owned(),
        items,
        unreadable,
    })
}

/// Bring the chosen top-level entries of the connected drive into the workspace's files, under
/// a folder named after the drive.
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
    let drive = Drive::open(cx, named)?;
    files::land(&drive, review, sink, cx.place, lander)
}

/// A cloud drive.
pub struct Clouds;

impl super::Source for Clouds {
    fn here(&self, cx: &super::Cx<'_>, row: &'static Source) -> super::Here {
        super::Here {
            next: if cx.place.field::<String>(row.key, ACCOUNT).is_ok_and(|kept| kept.is_some()) {
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

    fn review(&self, cx: &super::Cx<'_>, row: &'static Source, _: Option<&str>) -> Result<Review> {
        review(cx, row)
    }

    fn summary(&self, row: &'static Source, review: &Review) -> String {
        files::summary(review, row.title)
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
