//! The email account type's configuration: the IMAP/SMTP settings stored as
//! an email [`crate::Account`]'s `config`, shared by Mail (which connects with
//! it) and Settings (which creates and edits it).

use serde::{Deserialize, Serialize};

pub const TYPE_ID: &str = "email";

/// How an account authenticates to its IMAP/SMTP servers. Password is the
/// classic user+secret; OAuth is the modern token flow the big providers now
/// **require** (Gmail and Outlook block plain-password IMAP), so those
/// providers preset to [`AuthMethod::OAuth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMethod {
    #[default]
    Password,
    /// `OAuth2` authorization-code + PKCE; the account holds a bearer token
    /// instead of a password (Mail's `oauth` module runs the flow).
    OAuth,
}

impl AuthMethod {
    pub fn as_key(self) -> &'static str {
        match self {
            AuthMethod::Password => "password",
            AuthMethod::OAuth => "oauth",
        }
    }
    pub fn from_key(s: &str) -> Self {
        match s {
            "oauth" => AuthMethod::OAuth,
            _ => AuthMethod::Password,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            AuthMethod::Password => "Password",
            AuthMethod::OAuth => "OAuth (sign in)",
        }
    }
}

/// Whether an account's settings sign in with a bearer token rather than a
/// password. Both the name the stored settings carry (`OAuth`, its serde
/// form) and the key [`AuthMethod::as_key`] answers (`oauth`) count.
#[must_use]
pub fn is_oauth(settings: &serde_json::Value) -> bool {
    settings
        .get("auth")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|auth| auth.eq_ignore_ascii_case("oauth"))
}

/// The key of an account's settings that names the entry of the workspace's vault its sign-in
/// is: a password, or the grant a provider's own sign-in page left. The value is the entry's
/// reference (`vault.item:<entry id>`), never the secret, so the settings stay free of one. Every
/// kind of account that signs in with a kept secret names it here, a calendar's and a cloud
/// server's as much as a mail account's.
pub const PASSWORD_ITEM_KEY: &str = "passwordItem";

/// The reference of the vault entry `settings` (an account's `config`) sign in with, when they
/// name one ([`PASSWORD_ITEM_KEY`]).
#[must_use]
pub fn password_item(settings: &serde_json::Value) -> Option<&str> {
    settings
        .get(PASSWORD_ITEM_KEY)
        .and_then(serde_json::Value::as_str)
        .filter(|reference| !reference.trim().is_empty())
}

/// Name `reference` as the vault entry `settings` sign in with, or, with `None`, name none:
/// an account whose sign-in went has none to use until its person signs in again.
pub fn set_password_item(settings: &mut serde_json::Value, reference: Option<&str>) {
    if !settings.is_object() {
        *settings = serde_json::json!({});
    }
    if let Some(settings) = settings.as_object_mut() {
        match reference {
            Some(reference) => {
                settings.insert(PASSWORD_ITEM_KEY.into(), reference.into());
            }
            None => {
                settings.remove(PASSWORD_ITEM_KEY);
            }
        }
    }
}

/// How a connection to a mail server becomes secure. There is no third way:
/// nothing signs in over a connection that stays in the clear.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// TLS from the first byte, as on IMAP's port 993.
    Tls,
    /// The connection starts in the clear and is upgraded with `STARTTLS`
    /// before anything signs in, as on IMAP's port 143.
    StartTls,
}

/// The port IMAP is registered on in the clear. A server there cannot be
/// spoken TLS to from the first byte, so a connection to it always upgrades.
pub const IMAP_CLEAR_PORT: u16 = 143;

impl Security {
    /// How the IMAP server on `port` is reached by an account whose
    /// `imap_tls` setting is `wrapped`: the setting decides, except on
    /// [`IMAP_CLEAR_PORT`], which only ever upgrades.
    #[must_use]
    pub const fn of_imap(wrapped: bool, port: u16) -> Self {
        if wrapped && port != IMAP_CLEAR_PORT {
            Self::Tls
        } else {
            Self::StartTls
        }
    }

    /// The `imap_tls` setting an account keeps for this.
    #[must_use]
    pub const fn wraps(self) -> bool {
        matches!(self, Self::Tls)
    }
}

/// How the IMAP server of the account with `settings` is reached: what
/// [`AccountConfig::imap_security`] answers, for a reader that holds the
/// settings as they are stored.
#[must_use]
pub fn imap_security_of(settings: &serde_json::Value) -> Security {
    let wrapped = settings
        .get("imap_tls")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true);
    let port = settings
        .get("imap_port")
        .and_then(serde_json::Value::as_u64)
        .and_then(|port| u16::try_from(port).ok())
        .unwrap_or(993);
    Security::of_imap(wrapped, port)
}

/// The IMAP capability Gmail names itself by.
pub const GMAIL_CAPABILITY: &str = "X-GM-EXT-1";

/// Whether an account's own servers keep a copy of every message they send
/// in its Sent folder, so a mail program must not file a second one. Gmail
/// does, and is known by its name or by naming itself among its IMAP
/// capabilities ([`GMAIL_CAPABILITY`], which the caller looks for).
/// Microsoft's servers do, and are known by their names. A plain server
/// files nothing.
#[must_use]
pub fn servers_file_sent(imap_host: &str, smtp_host: &str, names_itself_gmail: bool) -> bool {
    let filing = |host: &str| {
        let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
        [
            "gmail.com",
            "googlemail.com",
            "outlook.com",
            "office365.com",
            "office.com",
        ]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
    };
    names_itself_gmail || filing(imap_host) || filing(smtp_host)
}

/// Account-level connection settings. This is exactly the form the
/// account-setup screen collects and the transport layer consumes.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    /// Whether the IMAP connection is TLS from the first byte. Otherwise it
    /// starts in the clear and upgrades with `STARTTLS` before anything
    /// signs in ([`AccountConfig::imap_security`]).
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    /// How this account signs in. Providers that block password IMAP
    /// (Gmail/Outlook) preset to [`AuthMethod::OAuth`].
    #[serde(default)]
    pub auth: AuthMethod,
    /// Imported metadata must not reuse any existing secret or open a connection
    /// until the person explicitly finishes this account's secure sign-in.
    #[serde(default)]
    pub import_pending: bool,
    /// The other names and addresses it sends as, beside its own, each
    /// `Name <address>` ([`sender_identities`]), as the mail program it came
    /// from had them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    /// Held only in memory: the secret lives in the [`crate::SecretVault`] and
    /// is injected at connect time, never written to the account store —
    /// hence `serde(skip)`.
    #[serde(skip)]
    pub password: String,
}

impl std::fmt::Debug for AccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountConfig").finish_non_exhaustive()
    }
}

impl Default for AccountConfig {
    fn default() -> Self {
        Self {
            display_name: String::new(),
            email: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            imap_tls: true,
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_tls: true,
            username: String::new(),
            auth: AuthMethod::Password,
            import_pending: false,
            identities: Vec::new(),
            password: String::new(),
        }
    }
}

/// The key of an email account's settings that keeps the other names and
/// addresses it sends as ([`AccountConfig::identities`]).
pub const IDENTITIES_KEY: &str = "identities";

/// The most sender identities one account keeps.
pub const MAX_IDENTITIES: usize = 64;

/// `identities` (each `Name <address>` or an address) as an account keeps
/// them: trimmed, without blanks or repeats.
///
/// # Errors
/// More than [`MAX_IDENTITIES`], or one that names no address.
pub fn sender_identities(identities: Vec<String>) -> Result<Vec<String>, String> {
    let mut kept: Vec<String> = Vec::new();
    for identity in identities {
        let identity = identity.trim().to_owned();
        if identity.is_empty() || kept.contains(&identity) {
            continue;
        }
        let address = identity
            .rsplit_once('<')
            .map_or(identity.as_str(), |(_, address)| {
                address.trim_end_matches('>')
            });
        if !address.contains('@') || identity.chars().count() > 320 {
            return Err(format!("{identity:?} names no address it sends as"));
        }
        kept.push(identity);
    }
    if kept.len() > MAX_IDENTITIES {
        return Err(format!(
            "an account sends as at most {MAX_IDENTITIES} names and addresses"
        ));
    }
    Ok(kept)
}

/// The other names and addresses the account with `settings` sends as.
#[must_use]
pub fn sender_identities_of(settings: &serde_json::Value) -> Vec<String> {
    settings
        .get(IDENTITIES_KEY)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// The places of the workspace's vault an email account's password belongs to: the server
/// its mail is read from and the one it is handed to, as `imap://host:port` and
/// `smtp://host:port`. The one entry is used at both and nowhere else.
///
/// # Errors
/// A server's name or port is no place.
pub fn password_places(
    config: &AccountConfig,
) -> Result<Vec<crate::vault_items::Place>, crate::vault_items::ItemError> {
    let imap = crate::vault_items::Place::server("imap", &config.imap_host, config.imap_port)?;
    let smtp = crate::vault_items::Place::server("smtp", &config.smtp_host, config.smtp_port)?;
    Ok(vec![imap, smtp])
}

impl AccountConfig {
    /// Decode non-secret connection metadata using the shared account schema.
    ///
    /// Keeping this concrete entry point beside the schema lets thin apps use
    /// the shared runtime's decoder rather than each instantiating one.
    pub fn from_value(value: serde_json::Value) -> serde_json::Result<Self> {
        serde_json::from_value(value)
    }

    /// Encode connection metadata. The in-memory password is never included.
    pub fn to_value(&self) -> serde_json::Result<serde_json::Value> {
        serde_json::to_value(self)
    }

    /// How this account's IMAP server is reached.
    #[must_use]
    pub const fn imap_security(&self) -> Security {
        Security::of_imap(self.imap_tls, self.imap_port)
    }

    /// What the account is called: its display name, falling back to the
    /// email, falling back to a generic placeholder.
    pub fn label(&self) -> &str {
        if !self.display_name.is_empty() {
            &self.display_name
        } else if !self.email.is_empty() {
            &self.email
        } else {
            "New account"
        }
    }

    /// Heuristic autofill: given an email, guess the provider's
    /// IMAP/SMTP hosts. Mirrors what Thunderbird's autoconfig does for the
    /// big providers. Returns true if it recognised the domain.
    pub fn autofill_from_email(&mut self) -> bool {
        let Some((local, domain)) = self.email.split_once('@') else {
            return false;
        };
        if self.username.is_empty() {
            self.username = self.email.clone();
        }
        if self.display_name.is_empty() {
            self.display_name = local.to_string();
        }
        let (imap, smtp) = match domain.to_ascii_lowercase().as_str() {
            "gmail.com" | "googlemail.com" => ("imap.gmail.com", "smtp.gmail.com"),
            "outlook.com" | "hotmail.com" | "live.com" => {
                ("outlook.office365.com", "smtp.office365.com")
            }
            "yahoo.com" => ("imap.mail.yahoo.com", "smtp.mail.yahoo.com"),
            "icloud.com" | "me.com" => ("imap.mail.me.com", "smtp.mail.me.com"),
            "proton.me" | "protonmail.com" => ("127.0.0.1", "127.0.0.1"), // bridge
            other => {
                self.imap_host = format!("imap.{other}");
                self.smtp_host = format!("smtp.{other}");
                self.imap_port = 993;
                self.imap_tls = true;
                self.smtp_port = 465;
                self.smtp_tls = true;
                return false;
            }
        };
        self.imap_host = imap.to_string();
        self.smtp_host = smtp.to_string();
        self.imap_port = 993;
        self.imap_tls = true;
        self.smtp_port = 587;
        self.smtp_tls = true;
        true
    }

    /// Field-level validation for the setup form. Returns a list of
    /// (field-key, message) problems; empty means valid.
    pub fn validate(&self) -> Vec<(&'static str, String)> {
        let mut errs = Vec::new();
        if !self.email.contains('@') {
            errs.push(("email", "Enter a valid email address".into()));
        }
        if self.imap_host.trim().is_empty() {
            errs.push(("imap_host", "IMAP host is required".into()));
        }
        if self.smtp_host.trim().is_empty() {
            errs.push(("smtp_host", "SMTP host is required".into()));
        }
        if self.username.trim().is_empty() {
            errs.push(("username", "Username is required".into()));
        }
        if self.imap_port == 0 {
            errs.push(("imap_port", "Port must be 1-65535".into()));
        }
        if self.smtp_port == 0 {
            errs.push(("smtp_port", "Port must be 1-65535".into()));
        }
        errs
    }

    pub fn is_valid(&self) -> bool {
        self.validate().is_empty()
    }
}

/// The default port of a mail server of `kind` (`"imap"` or `"smtp"`), where its person writes no
/// port: IMAPS for reading and submissions for sending, both TLS from the first byte.
#[must_use]
pub const fn usual_port(kind: &str) -> u16 {
    if kind.eq_ignore_ascii_case("imap") {
        993
    } else {
        465
    }
}

/// A server as a person writes it: its name, or its name and a port after a colon, an IPv6
/// address in brackets (`[2001:db8::1]:993`). With no port, `usual` is where it answers.
///
/// # Errors
/// The text is no server's name and port, or the port is not one.
pub fn parse_server(text: &str, usual: u16) -> Result<Option<(String, u16)>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    let (name, port) = if let Some(rest) = text.strip_prefix('[') {
        let (name, rest) = rest.split_once(']').ok_or_else(|| {
            "Write an IPv6 server as its address in brackets, and its port after a colon".to_owned()
        })?;
        match rest.strip_prefix(':') {
            Some(port) => (name, Some(port)),
            None if rest.is_empty() => (name, None),
            None => {
                return Err(
                    "Write a server's port after a colon, as in [2001:db8::1]:993".to_owned(),
                );
            }
        }
    } else if text.matches(':').count() == 1 {
        let (name, port) = text
            .split_once(':')
            .ok_or_else(|| "Write a server's port after a colon".to_owned())?;
        (name, Some(port))
    } else if text.contains(':') {
        return Err(
            "Write an IPv6 server as its address in brackets, and its port after a colon"
                .to_owned(),
        );
    } else {
        (text, None)
    };
    let name = name.trim();
    if name.is_empty() || name.chars().any(char::is_whitespace) {
        return Err("Write a mail server as its name, such as imap.example.org".to_owned());
    }
    let port = match port {
        Some(port) => port
            .trim()
            .parse::<u16>()
            .ok()
            .filter(|port| *port != 0)
            .ok_or_else(|| "A mail server's port is a number from 1 to 65535".to_owned())?,
        None => usual,
    };
    Ok(Some((name.to_owned(), port)))
}
