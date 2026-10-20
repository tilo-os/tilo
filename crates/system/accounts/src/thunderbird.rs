//! Bounded Thunderbird account discovery. Only `prefs.js` metadata is read;
//! JavaScript is never executed and credential databases are never opened.
//!
//! Socket/auth values are Thunderbird's `MailNewsTypes2.idl` constants:
//! <https://searchfox.org/comm-central/source/mailnews/base/public/MailNewsTypes2.idl>

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::email::{AccountConfig, AuthMethod};

/// Metadata awaiting an explicit sign-in: legacy Mail readers only load `email`.
pub const PENDING_TYPE: &str = "email-pending";

const MAX_BYTES: u64 = 2 * 1024 * 1024;
const MAX_PREFERENCES: usize = 4096;
const MAX_ACCOUNTS: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Transport {
    Plain,
    StartTls,
    Tls,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub transport: Transport,
    /// An omitted Thunderbird preference remains unspecified.
    pub authentication: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub source_account: String,
    pub source_identity: String,
    pub email: String,
    pub display_name: String,
    pub incoming: Endpoint,
    pub outgoing: Endpoint,
}

/// How Thunderbird stores this account's mail locally.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerKind {
    Imap,
    Pop3,
    Local,
    Other,
}

/// One configured account and the profile directories that belong to it.
/// `directory` and `directory_rel` are source metadata: callers keep them out
/// of public receipts and resolve them only inside an already granted root.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountMetadata {
    pub source_account: String,
    pub source_identity: Option<String>,
    pub email: Option<String>,
    pub display_name: String,
    pub server_kind: ServerKind,
    pub server_key: String,
    pub hostname: Option<String>,
    pub directory: Option<String>,
    pub directory_rel: Option<String>,
    /// Present only for an IMAP account whose connection metadata is complete
    /// enough to review. Local and POP accounts keep their local mail instead.
    pub candidate: Option<Candidate>,
}

/// A Thunderbird address book section. The file is copied only when the
/// person selects this book.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddressBook {
    pub source_id: String,
    pub name: String,
    pub filename: Option<String>,
    pub uri: Option<String>,
    pub local: bool,
}

/// A Thunderbird calendar section. Local storage is copied; a remote calendar
/// remains metadata that needs reconnecting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calendar {
    pub source_id: String,
    pub name: String,
    pub uri: Option<String>,
    pub filename: Option<String>,
    pub kind: String,
    pub read_only: bool,
    pub enabled: bool,
}

/// Accounts, Local Folders, address books and calendars read from one
/// `prefs.js`. Folder contents are discovered separately from the profile
/// directory, because a preference alone never proves which messages exist.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileMetadata {
    pub accounts: Vec<AccountMetadata>,
    pub local_folders_server: Option<String>,
    pub address_books: Vec<AddressBook>,
    pub calendars: Vec<Calendar>,
}

/// Errors contain no source values, credentials, or preference contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscoveryError {
    Read,
    NotPreferencesFile,
    TooLarge,
    TooMany,
    Malformed,
    MissingMetadata,
    Unsupported,
}

impl Candidate {
    /// A known provider is selected by the actual IMAP host, including
    /// Microsoft mail on domains not present in the domain preset list.
    pub fn provider(&self) -> Option<&'static str> {
        match self.incoming.host.to_ascii_lowercase().as_str() {
            "outlook.office365.com" | "imap-mail.outlook.com" => Some("microsoft"),
            "imap.gmail.com" | "imap.googlemail.com" => Some("google"),
            _ => None,
        }
    }

    pub fn requires_oauth(&self) -> bool {
        self.provider().is_some()
            || self.incoming.authentication == Some(10)
            || self.outgoing.authentication == Some(10)
    }

    /// Build shared configuration only after an explicit authentication
    /// choice. Missing source preferences never imply password auth.
    /// Credentials and enrollment are separate custody operations.
    ///
    /// # Errors
    /// Refuses a transport/authentication the native Mail connector cannot
    /// faithfully represent, including different endpoint usernames.
    pub fn reviewed_config(
        &self,
        authentication: AuthMethod,
    ) -> Result<AccountConfig, DiscoveryError> {
        if (self.requires_oauth() && authentication != AuthMethod::OAuth)
            || (authentication == AuthMethod::OAuth && self.provider().is_none())
        {
            return Err(DiscoveryError::Unsupported);
        }
        let supported_auth = |value| matches!(value, None | Some(3 | 10));
        if !supported_auth(self.incoming.authentication)
            || !supported_auth(self.outgoing.authentication)
            || matches!((self.incoming.authentication, self.outgoing.authentication), (Some(incoming), Some(outgoing)) if incoming != outgoing)
        {
            return Err(DiscoveryError::Unsupported);
        }
        if self.incoming.username != self.outgoing.username
            || self.incoming.transport != Transport::Tls
            || !matches!(
                (self.outgoing.transport, self.outgoing.port),
                (Transport::Tls, 465) | (Transport::StartTls, 587 | 25)
            )
        {
            return Err(DiscoveryError::Unsupported);
        }
        let config = AccountConfig {
            display_name: self.display_name.clone(),
            email: self.email.clone(),
            imap_host: self.incoming.host.clone(),
            imap_port: self.incoming.port,
            imap_tls: true,
            smtp_host: self.outgoing.host.clone(),
            smtp_port: self.outgoing.port,
            smtp_tls: true,
            username: self.incoming.username.clone(),
            auth: authentication,
            import_pending: true,
            password: String::new(),
        };
        if !config.is_valid() {
            return Err(DiscoveryError::Malformed);
        }
        Ok(config)
    }
}

/// Read only the chosen ordinary preferences file. Symlinks and file swaps
/// are refused before reading; the resulting handle pins this file.
///
/// # Errors
/// Returns a value-free error for unreadable, replaced, malformed or
/// over-budget metadata. Never reads another profile file.
pub fn discover(path: &Path) -> Result<Vec<Candidate>, DiscoveryError> {
    if path.file_name().is_none_or(|name| name != "prefs.js") {
        return Err(DiscoveryError::NotPreferencesFile);
    }
    let selected = std::fs::symlink_metadata(path).map_err(|_| DiscoveryError::Read)?;
    if !selected.file_type().is_file() {
        return Err(DiscoveryError::NotPreferencesFile);
    }
    let file = std::fs::File::open(path).map_err(|_| DiscoveryError::Read)?;
    let opened = file.metadata().map_err(|_| DiscoveryError::Read)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if selected.dev() != opened.dev() || selected.ino() != opened.ino() {
            return Err(DiscoveryError::NotPreferencesFile);
        }
    }
    if opened.len() > MAX_BYTES {
        return Err(DiscoveryError::TooLarge);
    }
    read_selected("prefs.js", file)
}

/// Parse the descriptor the existing file chooser already opened. The path
/// is never recovered and reopened.
///
/// # Errors
/// Refuses a different file name or metadata exceeding the discovery budget.
pub fn read_selected(name: &str, reader: impl Read) -> Result<Vec<Candidate>, DiscoveryError> {
    if Path::new(name)
        .file_name()
        .is_none_or(|name| name != "prefs.js")
    {
        return Err(DiscoveryError::NotPreferencesFile);
    }
    let mut text = String::new();
    reader
        .take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| DiscoveryError::Read)?;
    parse(&text)
}

/// Parse generated `user_pref` data without evaluating any JavaScript.
/// Unknown preferences are discarded before their values are parsed.
///
/// # Errors
/// Reports only error categories, never preference values.
pub fn parse(text: &str) -> Result<Vec<Candidate>, DiscoveryError> {
    let prefs = read_preferences(text)?;
    candidates(&prefs)
}

/// Parse one profile's accounts, Local Folders, address books and calendars
/// without evaluating JavaScript or opening any credential database. Folder
/// message counts and contents require the profile directory and are not
/// inferred from preferences.
///
/// # Errors
/// Reports only error categories, never preference values.
pub fn parse_profile(text: &str) -> Result<ProfileMetadata, DiscoveryError> {
    let prefs = read_preferences(text)?;
    profile_metadata(&prefs)
}

fn read_preferences(text: &str) -> Result<Preferences, DiscoveryError> {
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_BYTES {
        return Err(DiscoveryError::TooLarge);
    }
    let mut prefs = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = preference(line)? {
            prefs.insert(key, value);
            if prefs.len() > MAX_PREFERENCES {
                return Err(DiscoveryError::TooMany);
            }
        }
    }
    Ok(prefs)
}

fn preference(line: &str) -> Result<Option<(String, Value)>, DiscoveryError> {
    let Some(body) = line.trim().strip_prefix("user_pref(") else {
        return Ok(None);
    };
    let mut key = serde_json::Deserializer::from_str(body).into_iter::<String>();
    let name = key
        .next()
        .ok_or(DiscoveryError::Malformed)?
        .map_err(|_| DiscoveryError::Malformed)?;
    if !allowed(&name) {
        return Ok(None);
    }
    let value = body
        .get(key.byte_offset()..)
        .ok_or(DiscoveryError::Malformed)?
        .trim()
        .strip_prefix(',')
        .ok_or(DiscoveryError::Malformed)?
        .trim()
        .strip_suffix(");")
        .ok_or(DiscoveryError::Malformed)?
        .trim();
    let value: Value = serde_json::from_str(value).map_err(|_| DiscoveryError::Malformed)?;
    if value.as_str().is_some_and(|value| value.len() > 4096) {
        return Err(DiscoveryError::TooLarge);
    }
    Ok(Some((name, value)))
}

fn allowed(key: &str) -> bool {
    if matches!(
        key,
        "mail.accountmanager.accounts"
            | "mail.accountmanager.defaultaccount"
            | "mail.accountmanager.localfoldersserver"
            | "mail.smtp.defaultserver"
            | "calendar.list"
    ) {
        return true;
    }
    let mut parts = key.split('.');
    matches!(
        (
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
            parts.next(),
        ),
        (
            Some("mail"),
            Some("account"),
            Some(_),
            Some("server" | "identities"),
            None
        ) | (
            Some("mail"),
            Some("identity"),
            Some(_),
            Some("useremail" | "fullName" | "smtpServer" | "valid"),
            None,
        ) | (
            Some("mail"),
            Some("server"),
            Some(_),
            Some(
                "type"
                    | "hostname"
                    | "port"
                    | "userName"
                    | "socketType"
                    | "authMethod"
                    | "directory"
                    | "directory-rel"
                    | "name"
                    | "realhostname"
                    | "realuserName"
                    | "login_at_startup"
                    | "rememberPassword"
                    | "offline_download"
                    | "check_new_mail"
                    | "deferred_to_account"
            ),
            None,
        ) | (
            Some("mail"),
            Some("smtpserver"),
            Some(_),
            Some("hostname" | "port" | "username" | "try_ssl" | "authMethod"),
            None,
        ) | (
            Some("ldap_2"),
            Some("servers"),
            Some(_),
            Some("description" | "filename" | "dirType" | "uri" | "position"),
            None,
        ) | (
            Some("calendar"),
            Some("registry"),
            Some(_),
            Some(
                "name"
                    | "calendar-uri"
                    | "type"
                    | "readOnly"
                    | "disabled"
                    | "color"
                    | "refreshInterval"
                    | "filename"
                    | "uri"
            ),
            None,
        )
    )
}

type Preferences = BTreeMap<String, Value>;

fn string(prefs: &Preferences, key: &str) -> Option<String> {
    prefs.get(key)?.as_str().map(str::to_owned)
}

fn required(prefs: &Preferences, key: &str) -> Result<String, DiscoveryError> {
    string(prefs, key)
        .filter(|value| !value.trim().is_empty())
        .ok_or(DiscoveryError::MissingMetadata)
}

fn reference(value: &str) -> Result<&str, DiscoveryError> {
    if value.len() > 64
        || value.is_empty()
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err(DiscoveryError::Malformed);
    }
    Ok(value)
}

fn candidates(prefs: &Preferences) -> Result<Vec<Candidate>, DiscoveryError> {
    let Some(accounts) = string(prefs, "mail.accountmanager.accounts") else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for account in accounts
        .split(',')
        .map(str::trim)
        .filter(|key| !key.is_empty())
    {
        reference(account)?;
        let server = required(prefs, &format!("mail.account.{account}.server"))?;
        reference(&server)?;
        if string(prefs, &format!("mail.server.{server}.type")).as_deref() != Some("imap") {
            continue;
        }
        let incoming = endpoint(prefs, &format!("mail.server.{server}"), false)?;
        let identities = required(prefs, &format!("mail.account.{account}.identities"))?;
        for identity in identities
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            reference(identity)?;
            found.push(candidate(prefs, account, identity, &incoming)?);
            if found.len() > MAX_ACCOUNTS {
                return Err(DiscoveryError::TooMany);
            }
        }
    }
    Ok(found)
}

fn candidate(
    prefs: &Preferences,
    account: &str,
    identity: &str,
    incoming: &Endpoint,
) -> Result<Candidate, DiscoveryError> {
    let prefix = format!("mail.identity.{identity}");
    let smtp = string(prefs, &format!("{prefix}.smtpServer"))
        .filter(|value| !value.is_empty())
        .or_else(|| string(prefs, "mail.smtp.defaultserver"))
        .ok_or(DiscoveryError::MissingMetadata)?;
    reference(&smtp)?;
    Ok(Candidate {
        source_account: account.into(),
        source_identity: identity.into(),
        email: required(prefs, &format!("{prefix}.useremail"))?,
        display_name: string(prefs, &format!("{prefix}.fullName")).unwrap_or_default(),
        incoming: incoming.clone(),
        outgoing: endpoint(prefs, &format!("mail.smtpserver.{smtp}"), true)?,
    })
}

fn endpoint(prefs: &Preferences, prefix: &str, outgoing: bool) -> Result<Endpoint, DiscoveryError> {
    let number = |name: &str| {
        prefs
            .get(&format!("{prefix}.{name}"))
            .and_then(Value::as_i64)
    };
    let port = number("port")
        .and_then(|value| u16::try_from(value).ok())
        .filter(|value| *value != 0)
        .ok_or(DiscoveryError::MissingMetadata)?;
    let transport = match number(if outgoing { "try_ssl" } else { "socketType" }) {
        Some(0) => Transport::Plain,
        Some(2) => Transport::StartTls,
        Some(3) => Transport::Tls,
        _ => return Err(DiscoveryError::Unsupported),
    };
    Ok(Endpoint {
        host: required(prefs, &format!("{prefix}.hostname"))?,
        port,
        username: required(
            prefs,
            &format!(
                "{prefix}.{}",
                if outgoing { "username" } else { "userName" }
            ),
        )?,
        transport,
        authentication: prefs
            .get(&format!("{prefix}.authMethod"))
            .map(|value| value.as_i64().ok_or(DiscoveryError::Malformed))
            .transpose()?,
    })
}

fn profile_metadata(prefs: &Preferences) -> Result<ProfileMetadata, DiscoveryError> {
    let mut metadata = ProfileMetadata::default();
    if let Some(accounts) = string(prefs, "mail.accountmanager.accounts") {
        for account in accounts
            .split(',')
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            reference(account)?;
            let Some(server) = string(prefs, &format!("mail.account.{account}.server")) else {
                continue;
            };
            reference(&server)?;
            let kind = server_kind(prefs, &server);
            let identities =
                string(prefs, &format!("mail.account.{account}.identities")).unwrap_or_default();
            let identity = identities
                .split(',')
                .map(str::trim)
                .find(|value| !value.is_empty() && reference(value).is_ok())
                .map(str::to_owned);
            let email = identity
                .as_ref()
                .and_then(|identity| string(prefs, &format!("mail.identity.{identity}.useremail")));
            let display_name = identity
                .as_ref()
                .and_then(|identity| string(prefs, &format!("mail.identity.{identity}.fullName")))
                .filter(|value| !value.trim().is_empty())
                .or_else(|| string(prefs, &format!("mail.server.{server}.name")))
                .unwrap_or_else(|| email.clone().unwrap_or_else(|| "Mail account".into()));
            let candidate = if kind == ServerKind::Imap {
                endpoint(prefs, &format!("mail.server.{server}"), false)
                    .ok()
                    .and_then(|incoming| {
                        candidate(
                            prefs,
                            account,
                            identity.as_deref().unwrap_or_default(),
                            &incoming,
                        )
                        .ok()
                    })
            } else {
                None
            };
            metadata.accounts.push(AccountMetadata {
                source_account: account.into(),
                source_identity: identity,
                email,
                display_name,
                server_kind: kind,
                server_key: server.clone(),
                hostname: string(prefs, &format!("mail.server.{server}.hostname")),
                directory: string(prefs, &format!("mail.server.{server}.directory")),
                directory_rel: string(prefs, &format!("mail.server.{server}.directory-rel")),
                candidate,
            });
            if metadata.accounts.len() > MAX_ACCOUNTS {
                return Err(DiscoveryError::TooMany);
            }
        }
    }
    if let Some(server) = string(prefs, "mail.accountmanager.localfoldersserver") {
        reference(&server)?;
        if !metadata
            .accounts
            .iter()
            .any(|account| account.server_key == server)
        {
            metadata.accounts.push(AccountMetadata {
                source_account: "localfolders".into(),
                source_identity: None,
                email: None,
                display_name: "Local Folders".into(),
                server_kind: ServerKind::Local,
                server_key: server.clone(),
                hostname: None,
                directory: string(prefs, &format!("mail.server.{server}.directory")),
                directory_rel: string(prefs, &format!("mail.server.{server}.directory-rel")),
                candidate: None,
            });
        }
        metadata.local_folders_server = Some(server);
    }
    address_books(prefs, &mut metadata);
    calendars(prefs, &mut metadata);
    Ok(metadata)
}

fn server_kind(prefs: &Preferences, server: &str) -> ServerKind {
    match string(prefs, &format!("mail.server.{server}.type")).as_deref() {
        Some("imap") => ServerKind::Imap,
        Some("pop3") => ServerKind::Pop3,
        Some("none") => ServerKind::Local,
        _ => ServerKind::Other,
    }
}

fn address_books(prefs: &Preferences, metadata: &mut ProfileMetadata) {
    let mut books: BTreeMap<String, BTreeMap<&str, String>> = BTreeMap::new();
    for (key, value) in prefs {
        let Some(rest) = key.strip_prefix("ldap_2.servers.") else {
            continue;
        };
        let Some((id, leaf)) = rest.split_once('.') else {
            continue;
        };
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            continue;
        }
        let Some(value) = value.as_str() else {
            continue;
        };
        if value.len() > 4096 {
            continue;
        }
        if matches!(leaf, "description" | "filename" | "dirType" | "uri") {
            books
                .entry(id.to_owned())
                .or_default()
                .insert(leaf, value.to_owned());
        }
    }
    for (id, fields) in books {
        let filename = fields.get("filename").cloned();
        let uri = fields.get("uri").cloned();
        let local = filename.is_some()
            || uri
                .as_deref()
                .is_none_or(|uri| !uri.starts_with("ldap://") && !uri.starts_with("ldaps://"));
        let name = fields
            .get("description")
            .filter(|value| !value.trim().is_empty())
            .cloned()
            .or_else(|| filename.clone())
            .unwrap_or_else(|| "Address book".into());
        metadata.address_books.push(AddressBook {
            source_id: id,
            name,
            filename,
            uri,
            local,
        });
    }
}

fn calendars(prefs: &Preferences, metadata: &mut ProfileMetadata) {
    let mut calendars: BTreeMap<String, BTreeMap<&str, String>> = BTreeMap::new();
    for (key, value) in prefs {
        let Some(rest) = key.strip_prefix("calendar.registry.") else {
            continue;
        };
        let Some((id, leaf)) = rest.split_once('.') else {
            continue;
        };
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            continue;
        }
        let value = match value {
            Value::String(value) => value.clone(),
            Value::Bool(value) => value.to_string(),
            _ => continue,
        };
        if value.len() > 4096 {
            continue;
        }
        if matches!(
            leaf,
            "name" | "calendar-uri" | "type" | "readOnly" | "disabled" | "filename" | "uri"
        ) {
            calendars
                .entry(id.to_owned())
                .or_default()
                .insert(leaf, value);
        }
    }
    for (id, fields) in calendars {
        if !fields.contains_key("name")
            && !fields.contains_key("calendar-uri")
            && !fields.contains_key("type")
        {
            continue;
        }
        let kind = fields.get("type").cloned().unwrap_or_default();
        metadata.calendars.push(Calendar {
            source_id: id,
            name: fields
                .get("name")
                .filter(|value| !value.trim().is_empty())
                .cloned()
                .unwrap_or_else(|| "Calendar".into()),
            uri: fields
                .get("calendar-uri")
                .or_else(|| fields.get("uri"))
                .cloned(),
            filename: fields.get("filename").cloned(),
            kind,
            read_only: fields.get("readOnly").is_some_and(|value| value == "true"),
            enabled: fields.get("disabled").is_none_or(|value| value != "true"),
        });
    }
}

#[cfg(test)]
mod tests;
