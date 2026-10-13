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
    candidates(&prefs)
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
        "mail.accountmanager.accounts" | "mail.smtp.defaultserver"
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
            Some("useremail" | "fullName" | "smtpServer"),
            None,
        ) | (
            Some("mail"),
            Some("server"),
            Some(_),
            Some("type" | "hostname" | "port" | "userName" | "socketType" | "authMethod"),
            None,
        ) | (
            Some("mail"),
            Some("smtpserver"),
            Some(_),
            Some("hostname" | "port" | "username" | "try_ssl" | "authMethod"),
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

#[cfg(test)]
mod tests;
