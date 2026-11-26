//! Accounts this computer is signed in to, as a migration source: the file servers GNOME Online
//! Accounts, the Nextcloud desktop app and rclone keep, the accounts of a Tilo home already here,
//! and the `OneDrive` accounts Windows keeps.
//!
//! An account is **connected**, never copied: it becomes one of the workspace's accounts through
//! the edit Settings makes of them ([`super::writer::change_accounts`]), and its files, calendars
//! and contacts then sync from its server as any account's do. Where this computer keeps its
//! password in a form Tilo can read (rclone's obscured `pass`, the Nextcloud app's keychain entry,
//! a Tilo home's vault), the password is sealed in this computer's vault with it; otherwise the
//! account arrives signed out and Settings asks for the password once. An account that signs in
//! with its provider's own sign-in (`OneDrive`) is never connected with another program's token:
//! the person is told to sign in to it.
//!
//! The configuration files are read within a byte bound, through no link, and only the fields
//! named here are kept: a foreign config can hold plaintext passwords, and its other fields never
//! enter a review.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use accounts::cloud_types;
use accounts::{Account, AccountId, Secret, SecretVault};
use anyhow::{Result, anyhow, bail};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::registry::Row;
use super::state::{Item, Review};
use super::{Cx, Here, Landed, Lander, Source};

/// The most a configuration file read here may hold.
const MAX_CONFIG: u64 = 2 * 1024 * 1024;

/// The accounts this computer is signed in to.
pub struct SignedIn;

/// What kind of account one found is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Nextcloud,
    WebDav,
    Spotify,
    OneDrive,
}

/// Where an account was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Origin {
    Gnome,
    Nextcloud,
    Rclone,
    Tilo,
    Windows,
}

impl Origin {
    const fn said(self) -> &'static str {
        match self {
            Self::Gnome => "GNOME Online Accounts",
            Self::Nextcloud => "the Nextcloud app",
            Self::Rclone => "rclone",
            Self::Tilo => "Tilo",
            Self::Windows => "Windows",
        }
    }
}

/// One account this computer is signed in to. Its password is never part of it: it is read
/// only when the account is connected.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Found {
    key: String,
    kind: Kind,
    origin: Origin,
    label: String,
    login: Option<String>,
    server: Option<String>,
    /// The id the account keeps where it was found: a Nextcloud app account's number, a Tilo
    /// account's id.
    native_id: Option<String>,
    /// Whether its password can be read here.
    password_here: bool,
}

/// What a scan of this computer found.
#[derive(Default)]
struct Scan {
    found: Vec<Found>,
    /// What could not be read, each named, never its path.
    unreadable: Vec<String>,
}

impl Source for SignedIn {
    fn here(&self, cx: &Cx<'_>, row: &'static Row) -> Here {
        let scan = scan(cx.home);
        if scan.found.is_empty() && scan.unreadable.is_empty() {
            return Here {
                unavailable: Some(
                    "this computer keeps no account of GNOME Online Accounts, the Nextcloud app, \
                     rclone, Tilo or OneDrive"
                        .to_owned(),
                ),
                ..Here::default()
            };
        }
        Here {
            here: Some(serde_json::json!(format!(
                "{} account(s)",
                scan.found.len()
            ))),
            next: Some(format!("tilo migrate review {}", row.key)),
            ..Here::default()
        }
    }

    fn review(&self, cx: &Cx<'_>, row: &'static Row, _: Option<&str>) -> Result<Review> {
        let scan = scan(cx.home);
        let mut review = Review {
            source: row.key.to_owned(),
            title: "Accounts this computer is signed in to".to_owned(),
            handle: cx.home.display().to_string(),
            items: scan.found.iter().map(item).collect(),
            unreadable: scan.unreadable,
        };
        super::keep_chosen(&mut review, cx.place.review(row.key)?.as_ref());
        Ok(review)
    }

    fn summary(&self, _: &'static Row, review: &Review) -> String {
        let chosen: Vec<&str> = review
            .chosen()
            .iter()
            .map(|item| item.title.as_str())
            .collect();
        format!(
            "Connect {}: {}",
            super::counted(chosen.len(), "accounts"),
            chosen.join(", ")
        )
    }

    fn land(
        &self,
        cx: &Cx<'_>,
        _: &'static Row,
        review: &Review,
        _: Option<&super::files::Sink>,
        lander: &mut Lander<'_>,
    ) -> Result<()> {
        let chosen: std::collections::BTreeSet<&str> = review
            .chosen()
            .into_iter()
            .map(|item| item.key.as_str())
            .collect();
        for found in scan(cx.home).found {
            if chosen.contains(found.key.as_str()) {
                connect(cx.home, &found, lander)?;
            }
        }
        Ok(())
    }
}

/// The review's item for `found`.
fn item(found: &Found) -> Item {
    let what = match found.kind {
        Kind::Nextcloud => "Nextcloud",
        Kind::WebDav => "WebDAV",
        Kind::Spotify => "Spotify",
        Kind::OneDrive => "OneDrive",
    };
    let at = found
        .server
        .as_deref()
        .map_or_else(String::new, |server| format!(" at {server}"));
    let note = if found.kind == Kind::OneDrive {
        Some("Signs in with Microsoft's own sign-in: you sign in to it once it is here".to_owned())
    } else if !found.password_here {
        Some(format!(
            "{} keeps no password Tilo can read: Settings asks for it once",
            found.origin.said()
        ))
    } else {
        None
    };
    Item {
        key: found.key.clone(),
        kind: "account".to_owned(),
        title: format!("{} ({what}{at}, from {})", found.label, found.origin.said()),
        count: 1,
        counts: "accounts".to_owned(),
        within: None,
        chosen: false,
        note,
    }
}

/// The folder a home keeps its programs' settings in.
fn config_dir(home: &Path) -> PathBuf {
    home.join(".config")
}

/// Every account `home` is signed in to.
fn scan(home: &Path) -> Scan {
    let config = config_dir(home);
    let mut scan = Scan::default();
    for (relative, origin, label) in [
        (
            "goa-1.0/accounts.conf",
            Origin::Gnome,
            "GNOME Online Accounts",
        ),
        (
            "Nextcloud/nextcloud.cfg",
            Origin::Nextcloud,
            "the Nextcloud app's accounts",
        ),
        ("rclone/rclone.conf", Origin::Rclone, "rclone's accounts"),
    ] {
        match metadata(&config, &config.join(relative)) {
            Ok(Some(text)) => match origin {
                Origin::Gnome => gnome(&text, &mut scan),
                Origin::Nextcloud => nextcloud(&text, &mut scan),
                _ => rclone(&text, &mut scan),
            },
            Ok(None) => {}
            Err(()) => scan.unreadable.push(format!("{label} could not be read")),
        }
    }
    tilo(&config, &mut scan);
    windows(&mut scan);
    scan
}

/// The bounded bytes of the file at `path` below `root`, refusing a link anywhere in between.
/// `None` when there is no file there.
///
/// # Errors
/// A link, something other than a file, a file over the bound, or a failed read.
#[expect(
    clippy::result_unit_err,
    reason = "its callers tell only that a saved account is unreadable, never why"
)]
pub fn metadata_bytes(root: &Path, path: &Path) -> Result<Option<Zeroizing<Vec<u8>>>, ()> {
    use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    let exists = std::fs::symlink_metadata(path);
    match exists {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(()),
        Ok(info) if !info.is_file() || info.len() > MAX_CONFIG => return Err(()),
        Ok(_) => {}
    }
    check_components(root, path)?;
    // A directory opens as a directory on every host; `File::open` refuses one on Windows, which
    // would make every saved account unreadable there.
    let root_handle =
        cap_primitives::fs::open_ambient_dir(root, cap_primitives::ambient_authority())
            .map_err(|_| ())?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(
        &root_handle,
        path.strip_prefix(root).map_err(|_| ())?,
        &options,
    )
    .map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Zeroizing::new(Vec::new());
    file.take(MAX_CONFIG.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if !u64::try_from(bytes.len()).is_ok_and(|len| len <= MAX_CONFIG) {
        return Err(());
    }
    Ok(Some(bytes))
}

/// [`metadata_bytes`] as UTF-8 text.
fn metadata(root: &Path, path: &Path) -> Result<Option<Zeroizing<String>>, ()> {
    let Some(bytes) = metadata_bytes(root, path)? else {
        return Ok(None);
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| ())?;
    Ok(Some(Zeroizing::new(text.to_owned())))
}

/// Refuse `path` when any folder between `root` and it is a link or not a plain name.
fn check_components(root: &Path, path: &Path) -> Result<(), ()> {
    let relative = path.strip_prefix(root).map_err(|_| ())?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(());
        }
        current.push(component);
        if std::fs::symlink_metadata(&current)
            .map_err(|_| ())?
            .is_symlink()
        {
            return Err(());
        }
    }
    Ok(())
}

/// The sections of an INI-style `text`, keeping only the `allowed` keys: a foreign config may
/// hold plaintext passwords, and its other fields never leave here.
fn sections(text: &str, allowed: &[&str]) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut result: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            name.clone_into(&mut section);
        } else if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let leaf = key.rsplit('\\').next().unwrap_or(key);
            if allowed.contains(&leaf) && value.len() <= 2048 {
                result
                    .entry(section.clone())
                    .or_default()
                    .insert(key.into(), value.trim().into());
            }
        }
    }
    result
}

/// `raw` as a server address an account may be connected at: HTTPS (or HTTP on this computer),
/// with no login, query or fragment in it.
fn safe_server(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    if account_credentials::nextcloud::server_url(raw).is_err()
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

/// A key no two accounts share: `prefix` and the hash of what names the account.
fn key_of(prefix: &str, parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(u64::try_from(part.len()).unwrap_or(u64::MAX).to_le_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hex::encode(hash.finalize());
    format!("{prefix}-{}", digest.get(..16).unwrap_or(&digest))
}

/// GNOME Online Accounts' file servers.
fn gnome(text: &str, scan: &mut Scan) {
    for (section, config) in sections(
        text,
        &["Provider", "Identity", "PresentationIdentity", "Uri"],
    ) {
        if !section.starts_with("Account ") {
            continue;
        }
        let kind = match config.get("Provider").map(String::as_str) {
            Some("owncloud" | "nextcloud") => Kind::Nextcloud,
            Some("webdav") => Kind::WebDav,
            _ => continue,
        };
        let login = config
            .get("PresentationIdentity")
            .or_else(|| config.get("Identity"))
            .cloned();
        let Some(server) = config.get("Uri").and_then(|value| safe_server(value)) else {
            continue;
        };
        scan.found.push(Found {
            key: key_of("gnome", &[&section, &server]),
            kind,
            origin: Origin::Gnome,
            label: login.clone().unwrap_or_else(|| "Online account".to_owned()),
            login,
            server: Some(server),
            native_id: None,
            password_here: false,
        });
    }
}

/// The Nextcloud desktop app's accounts: each `N\url`, `N\http_user`… group of `[Accounts]`.
fn nextcloud(text: &str, scan: &mut Scan) {
    let parsed = sections(
        text,
        &[
            "url",
            "user",
            "http_user",
            "webflow_user",
            "displayName",
            "authType",
            "http_clientCertPkcs12",
        ],
    );
    let Some(config) = parsed.get("Accounts") else {
        return;
    };
    let mut groups: BTreeMap<&str, BTreeMap<&str, &str>> = BTreeMap::new();
    for (key, value) in config {
        if let Some((id, leaf)) = key.split_once('\\')
            && !leaf.contains('\\')
        {
            groups.entry(id).or_default().insert(leaf, value);
        }
    }
    for (id, values) in groups {
        let Some(raw_server) = values.get("url") else {
            continue;
        };
        if safe_server(raw_server).is_none() {
            continue;
        }
        // The keychain names the account by the server as the app keeps it.
        let server = (*raw_server).to_owned();
        let username = values
            .get("http_user")
            .or_else(|| values.get("user"))
            .or_else(|| values.get("webflow_user"))
            .copied()
            .unwrap_or_default()
            .to_owned();
        let password_here = values.get("http_user").is_some_and(|user| !user.is_empty())
            && values.get("authType").is_none_or(|kind| *kind == "http")
            && values
                .get("http_clientCertPkcs12")
                .is_none_or(|cert| cert.is_empty())
            && keychain_entry(&server, &username, id);
        let label = values
            .get("displayName")
            .copied()
            .unwrap_or(&username)
            .chars()
            .take(256)
            .collect();
        scan.found.push(Found {
            key: key_of("nextcloud", &[id, &server, &username]),
            kind: Kind::Nextcloud,
            origin: Origin::Nextcloud,
            label,
            login: Some(username),
            server: Some(server),
            native_id: Some(id.to_owned()),
            password_here,
        });
    }
}

/// Whether the system keychain names an entry for the Nextcloud app's account `id`.
#[cfg(target_os = "linux")]
fn keychain_entry(server: &str, username: &str, id: &str) -> bool {
    tilo_secrets::owner_migration::NextcloudCredential::new(server, username, id).is_ok()
}

#[cfg(not(target_os = "linux"))]
fn keychain_entry(_: &str, _: &str, _: &str) -> bool {
    false
}

/// rclone's `WebDAV` remotes (Nextcloud's among them).
fn rclone(text: &str, scan: &mut Scan) {
    if text.trim_start().starts_with("RCLONE_ENCRYPT_V0:") {
        scan.unreadable.push(
            "rclone's accounts are encrypted with a password; decrypt the config to bring them"
                .to_owned(),
        );
        return;
    }
    for (label, config) in sections(text, &["type", "url", "user", "vendor"]) {
        let kind = match config.get("type").map(String::as_str) {
            Some("webdav") if config.get("vendor").is_some_and(|s| s == "nextcloud") => {
                Kind::Nextcloud
            }
            Some("webdav") => Kind::WebDav,
            _ => continue,
        };
        let Some(server) = config.get("url").and_then(|s| safe_server(s)) else {
            continue;
        };
        let login = config.get("user").cloned();
        scan.found.push(Found {
            key: key_of(
                "rclone",
                &[&label, &server, login.as_deref().unwrap_or_default()],
            ),
            kind,
            origin: Origin::Rclone,
            label,
            login,
            server: Some(server),
            native_id: None,
            password_here: true,
        });
    }
}

/// The accounts a Tilo home here keeps: its file servers and its Spotify.
fn tilo(config: &Path, scan: &mut Scan) {
    let root = config.join("tilo");
    let listed = (|| -> Result<Vec<Account>, ()> {
        // The home's `tilo` folder is read as part of the wider config folder: a redirected
        // folder is not read.
        let Some(bytes) = metadata_bytes(config, &accounts::person::staged_path_in(&root))? else {
            return Ok(Vec::new());
        };
        let record = tilo_resource::Replica::<accounts::person::AccountList>::load(&bytes)
            .map_err(|_| ())?
            .state()
            .map_err(|_| ())?;
        Ok(record.all())
    })();
    let Ok(listed) = listed else {
        scan.unreadable
            .push("this computer's Tilo accounts could not be read".to_owned());
        return;
    };
    for account in listed {
        let kind = match account.type_id.as_str() {
            "nextcloud" => Kind::Nextcloud,
            "webdav" => Kind::WebDav,
            "spotify"
                if matches!(
                    account.id.as_str(),
                    "music.spotify" | "music.spotify.streaming"
                ) =>
            {
                Kind::Spotify
            }
            _ => continue,
        };
        if !account.enabled || account.id.as_str().is_empty() || account.id.as_str().len() > 256 {
            continue;
        }
        scan.found.push(Found {
            key: key_of("tilo", &[account.id.as_str()]),
            kind,
            origin: Origin::Tilo,
            label: account
                .display_name
                .chars()
                .filter(|c| !c.is_control())
                .take(256)
                .collect(),
            login: account
                .config
                .get("username")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            server: account
                .config
                .get("server_url")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            native_id: Some(account.id.as_str().to_owned()),
            password_here: true,
        });
    }
}

/// The `OneDrive` accounts Windows keeps: only their address, from the registry.
fn windows(scan: &mut Scan) {
    match windows_accounts() {
        Ok(emails) => {
            for email in emails {
                scan.found.push(Found {
                    key: key_of("onedrive", &[&email.to_ascii_lowercase()]),
                    kind: Kind::OneDrive,
                    origin: Origin::Windows,
                    label: email.clone(),
                    login: Some(email),
                    server: None,
                    native_id: None,
                    password_here: false,
                });
            }
        }
        Err(why) => scan.unreadable.push(why),
    }
}

/// The addresses of the `OneDrive` accounts Windows keeps for this person.
#[cfg(windows)]
fn windows_accounts() -> Result<Vec<String>, String> {
    let mut command = std::process::Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        ONEDRIVE_ACCOUNTS,
    ]);
    let output = bounded_command::run_prepared(
        command,
        None,
        256 * 1024,
        std::time::Duration::from_secs(10),
    )
    .map_err(|_| "Windows' OneDrive accounts could not be read".to_owned())?;
    if !output.status.success() {
        return Err("Windows' OneDrive accounts could not be read".to_owned());
    }
    parse_onedrive(&output.stdout)
}

/// Off Windows there is no registry: it names no account.
#[cfg(not(windows))]
fn windows_accounts() -> Result<Vec<String>, String> {
    parse_onedrive(b"[]")
}

/// The addresses in the registry reader's answer, each once.
fn parse_onedrive(bytes: &[u8]) -> Result<Vec<String>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Profile {
        /// The account's folder, which the `windows` source lists.
        #[serde(rename = "root")]
        _root: PathBuf,
        email: String,
    }
    let refused = || "Windows' OneDrive accounts could not be read".to_owned();
    if bytes.len() > 256 * 1024 {
        return Err(refused());
    }
    let rows: Vec<Profile> = serde_json::from_slice(bytes).map_err(|_| refused())?;
    if rows.len() > 128
        || rows
            .iter()
            .any(|row| row.email.len() > 2048 || row.email.chars().any(char::is_control))
    {
        return Err(refused());
    }
    let mut emails: Vec<String> = Vec::new();
    for row in rows {
        if !emails
            .iter()
            .any(|kept| kept.eq_ignore_ascii_case(&row.email))
        {
            emails.push(row.email);
        }
    }
    Ok(emails)
}

/// The registry reader: only the `OneDrive` accounts' folders and addresses.
#[cfg(windows)]
const ONEDRIVE_ACCOUNTS: &str = r#"
$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
$rows = [System.Collections.Generic.List[object]]::new()
$accounts = 'HKCU:\Software\Microsoft\OneDrive\Accounts'
if (Test-Path -LiteralPath $accounts) {
 $keys = @(Get-ChildItem -LiteralPath $accounts)
 if ($keys.Count -gt 128) { throw 'Too many accounts' }
 foreach ($key in $keys) {
  $v = Get-ItemProperty -LiteralPath $key.PSPath
  if ($v.UserFolder -and $v.UserEmail) {
   $rows.Add(@{ root=$v.UserFolder; email=$v.UserEmail })
  }
 }
}
[Console]::Out.Write((ConvertTo-Json -InputObject @($rows.ToArray()) -Depth 3 -Compress))
"#;

/// Connect `found` as one of the workspace's accounts, unless the workspace has it.
fn connect(home: &Path, found: &Found, lander: &mut Lander<'_>) -> Result<()> {
    if found.kind == Kind::OneDrive {
        let email = found.login.as_deref().unwrap_or_default();
        lander.say(&format!(
            "{email} is a OneDrive account: it signs in with Microsoft's own sign-in, so it is \
             not connected with Windows' sign-in. Connect it with `tilo migrate connect onedrive \
             --login {email}`, or in Settings > Accounts"
        ));
        return Ok(());
    }
    let (account, secret) = account_of(home, found)?;
    let root = accounts::config_root()
        .ok_or_else(|| anyhow!("this account has no Tilo config directory"))?;
    let label = found.label.clone();
    lander.once(&found.key, "accounts", |workspace, _| {
        let kept = super::writer::accounts(workspace)?;
        if kept.get(&account.id).is_some() {
            return Ok(Landed::Already);
        }
        if let Some(secret) = secret {
            crate::vault::persist::<anyhow::Error>(&root, &account, secret)?;
        }
        let saving = accounts::person::AccountList::saving(&account);
        super::writer::change_accounts(workspace, (&saving.address(), &saving.arguments))?;
        Ok(Landed::One)
    })?;
    lander.say(&format!("{label} is connected"));
    Ok(())
}

/// The workspace account `found` becomes, and its password when this computer keeps one Tilo
/// can read.
fn account_of(home: &Path, found: &Found) -> Result<(Account, Option<Secret>)> {
    let config = config_dir(home);
    if found.origin == Origin::Tilo {
        return tilo_account(&config, found);
    }
    let type_id = match found.kind {
        Kind::Nextcloud if found.origin != Origin::Rclone => cloud_types::NEXTCLOUD,
        // rclone's address is the exact files root (Nextcloud's /remote.php/dav/files/<id>, or a
        // chosen folder), so it is kept as a plain WebDAV account.
        Kind::Nextcloud | Kind::WebDav => cloud_types::WEBDAV,
        Kind::Spotify | Kind::OneDrive => bail!("{} is not a file server", found.label),
    };
    let server = found
        .server
        .as_deref()
        .ok_or_else(|| anyhow!("{} names no server", found.label))?;
    let server = account_credentials::nextcloud::server_url(server)
        .map_err(|_| anyhow!("{} names no server Tilo can reach", found.label))?
        .to_string();
    let username = found.login.clone().unwrap_or_default();
    let mut account = Account::new(AccountId::new(found.key.clone()), type_id);
    account.display_name.clone_from(&found.label);
    account.config =
        serde_json::json!({"server_url": server, "username": username, "use_files": true});
    let password = match found.origin {
        Origin::Rclone => Some(rclone_password(&config, found)?),
        Origin::Nextcloud if found.password_here => Some(nextcloud_password(&config, found)?),
        _ => None,
    };
    let secret = password.map(|password| {
        let secret = Secret::password(password.to_string());
        account_credentials::copied_secret(&account, &secret)
    });
    Ok((account, secret))
}

/// The account a Tilo home keeps, and the secret its vault keeps for it, as this computer's
/// own copy of that grant.
fn tilo_account(config: &Path, found: &Found) -> Result<(Account, Option<Secret>)> {
    let root = config.join("tilo");
    let id = found.native_id.as_deref().unwrap_or_default();
    let bytes = metadata_bytes(config, &accounts::person::staged_path_in(&root))
        .map_err(|()| anyhow!("this computer's Tilo accounts could not be read"))?
        .ok_or_else(|| anyhow!("{} is no longer here", found.label))?;
    let unreadable = || anyhow!("this computer's Tilo accounts could not be read");
    let record = tilo_resource::Replica::<accounts::person::AccountList>::load(&bytes)
        .map_err(|_| unreadable())?
        .state()
        .map_err(|_| unreadable())?;
    let account = record
        .get(&AccountId::new(id))
        .filter(|account| account.enabled)
        .ok_or_else(|| anyhow!("{} is no longer here", found.label))?;
    let vault = SecretVault::open_read_only_in(&root);
    if vault.recovery_error().is_some() {
        bail!("the vault of this computer's Tilo home could not be read");
    }
    let secret = vault
        .get(&account.id)
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let secret = crate::vault::PrivateSecret(secret);
            account_credentials::copied_secret(&account, &secret.0)
        });
    Ok((account, secret))
}

/// The password rclone keeps for `found`, in its obscured form undone.
fn rclone_password(config: &Path, found: &Found) -> Result<Zeroizing<String>> {
    let unreadable = || anyhow!("rclone's password for {} could not be read", found.label);
    let text = metadata(config, &config.join("rclone/rclone.conf"))
        .map_err(|()| unreadable())?
        .ok_or_else(unreadable)?;
    let mut section = "";
    let mut encoded = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = name;
        } else if section == found.label
            && let Some((key, value)) = line.split_once('=')
            && key.trim() == "pass"
        {
            if encoded.is_some() {
                return Err(unreadable());
            }
            encoded = Some(value.trim());
        }
    }
    let encoded = encoded.ok_or_else(unreadable)?;
    // rclone's obscuring is a fixed-key cipher, not a protection: the password is sealed in the
    // vault at once.
    if encoded.len() > 16 * 1024 {
        return Err(unreadable());
    }
    tilo_seal::compat::rclone_obscure(encoded).map_err(|_| unreadable())
}

/// The password the system keychain keeps for the Nextcloud app's account `found`, read only
/// while the app's config still names it as reviewed.
#[cfg(target_os = "linux")]
fn nextcloud_password(config: &Path, found: &Found) -> Result<Zeroizing<String>> {
    let unreadable = || {
        anyhow!(
            "the keychain's password for {} could not be read; Settings asks for it once",
            found.label
        )
    };
    let path = config.join("Nextcloud/nextcloud.cfg");
    let before = metadata(config, &path).map_err(|()| unreadable())?;
    let mut scan = Scan::default();
    if let Some(text) = &before {
        nextcloud(text, &mut scan);
    }
    if !scan.found.iter().any(|now| now == found) {
        bail!(
            "{} changed in the Nextcloud app since it was reviewed",
            found.label
        );
    }
    let request = tilo_secrets::owner_migration::NextcloudCredential::new(
        found.server.as_deref().unwrap_or_default(),
        found.login.as_deref().unwrap_or_default(),
        found.native_id.as_deref().unwrap_or_default(),
    )
    .map_err(|_| unreadable())?;
    let secret = request
        .read_blocking()
        .map_err(|_| unreadable())?
        .ok_or_else(unreadable)?;
    let value = std::str::from_utf8(secret.expose()).map_err(|_| unreadable())?;
    if value.is_empty() || value.len() > 4096 {
        return Err(unreadable());
    }
    Ok(Zeroizing::new(value.to_owned()))
}

#[cfg(not(target_os = "linux"))]
fn nextcloud_password(_: &Path, found: &Found) -> Result<Zeroizing<String>> {
    bail!(
        "the Nextcloud app's password for {} is read on Linux only",
        found.label
    )
}
