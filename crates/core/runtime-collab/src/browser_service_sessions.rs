//! Request-scoped account credentials from the session's ownership; never a
//! collaboration event or document.
//!
//! One request shape on an app's own admitted route reaches the one
//! owner, the session, which holds the person's account vault:
//!
//! - Browser lists, resolves and forgets its website sessions;
//! - Settings, the keeper of the workspace's accounts, keeps an account's
//!   password in the workspace's vault
//!   ([`BrowserServiceSessionCommand::StoreAccountSecret`]), which is never
//!   readable back by Settings, and is told the entry the account names;
//! - any app's engine is released the secret of one of the workspace's accounts, asked for by
//!   the vault entry the account names as its sign-in (its `passwordItem`, `vault.item:<id>`)
//!   ([`BrowserServiceSessionCommand::ReleaseAccountSecret`]): Mail's to read and send mail,
//!   Files' to reach a `WebDAV` or Nextcloud server. The session decides on the person's grants
//!   on Passwords, releases it only for the servers that account names, and writes the use
//!   into the entry first. An app asks through
//!   `collaboration::browser_service_sessions::AccountSecrets`;
//! - Browser, in a session that holds a workspace's Browser, is handed the text of a vault
//!   item for one page action that fills a field from it by reference
//!   ([`BrowserServiceSessionCommand::UseItem`]). Its session asks the workspace's server,
//!   which decides, and answers the text, that its person is asked first, or a refusal
//!   with its reason. The text is held by types that wipe themselves and print nothing.
//!
//! A sandboxed app never opens the vault itself: its sandbox reaches neither
//! the vault's files nor the key storage that seals them.
use browser_continuity_core::sessions::{Service, SessionCookies};
use serde::{Deserialize, Serialize};
use tilo_actions::InvocationId;
use zeroize::Zeroizing;

pub const MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES: usize = 4096;
pub const MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BROWSER_SERVICE_SESSIONS: usize = 128;

/// The longest account secret storage seals or releases.
pub const MAX_ACCOUNT_SECRET_BYTES: usize = 2048;

/// The longest text of a vault item a page's field is filled with, in bytes: the most its
/// keeper seals.
pub const MAX_ITEM_TEXT_BYTES: usize = 4096;
/// The longest words an item's use is answered with in place of its text, in bytes: a
/// question's words, or a refusal's reason.
pub const MAX_ITEM_WORDS_BYTES: usize = 2048;
/// The longest origin an item is asked for, in bytes.
pub const MAX_ITEM_ORIGIN_BYTES: usize = 512;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrowserServiceSessionCommand {
    List {},
    Resolve {
        id: String,
        setup_binding: String,
    },
    Forget {
        id: String,
        setup_binding: String,
    },
    /// Keep `password` as the login of the workspace's account `account` in the workspace's
    /// vault: signing in as `username` at `servers` (`imap://host:port`, `smtp://host:port`, or
    /// a file server's origin, `https://host:port`) and nowhere else. Only the accounts' keeper (Settings) may; nothing reads it back to
    /// it.
    StoreAccountSecret {
        account: String,
        password: Zeroizing<String>,
        username: String,
        servers: Vec<String>,
    },
    /// The secret of the workspace's account that names the vault entry `item`
    /// (`vault.item:<entry id>`) as its sign-in, for the asking app's engine to sign in with.
    ReleaseAccountSecret {
        item: String,
    },
    /// How the sync of the workspace's account `account` went on this device: what every
    /// member reads of it in the workspace's accounts, written as this device's report.
    ReportAccountSync {
        account: String,
        /// Where its sync stands, in the word `accounts::SyncHealth` is spelled with
        /// (`healthy`, `needs_sign_in`, ...).
        health: String,
        /// Whether its person just signed in to it.
        signed_in: bool,
        /// A short summary a person reads, never a server's own words.
        message: String,
    },
    /// The text an item of the person's vault keeps for the site Browser's page is at, for
    /// the one page action that fills a field from it by reference. It names nobody and no
    /// reason: whoever keeps the vault decides by the fill it handed out itself.
    UseItem {
        /// The page action that fills: an answer is for this one and no other.
        invocation: InvocationId,
        /// The item, by its reference: never a text an item keeps.
        item: String,
        /// The site the page is at: its origin, as the engine committed it.
        origin: String,
    },
    /// Whether this session has vault items used for Browser's pages at all: a session
    /// that holds a workspace's Browser does, a person's own session does not.
    LendsItems {},
}
impl std::fmt::Debug for BrowserServiceSessionCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::List {} => f.write_str("List"),
            Self::Resolve { id, .. } => f.debug_struct("Resolve").field("id", id).finish(),
            Self::Forget { id, .. } => f.debug_struct("Forget").field("id", id).finish(),
            Self::StoreAccountSecret { account, .. } => f
                .debug_struct("StoreAccountSecret")
                .field("account", account)
                .finish_non_exhaustive(),
            Self::ReleaseAccountSecret { item } => f
                .debug_struct("ReleaseAccountSecret")
                .field("item", item)
                .finish(),
            Self::ReportAccountSync {
                account, health, ..
            } => f
                .debug_struct("ReportAccountSync")
                .field("account", account)
                .field("health", health)
                .finish_non_exhaustive(),
            Self::UseItem {
                invocation,
                item,
                origin,
            } => f
                .debug_struct("UseItem")
                .field("invocation", invocation)
                .field("item", item)
                .field("origin", origin)
                .finish(),
            Self::LendsItems {} => f.write_str("LendsItems"),
        }
    }
}
/// The kind of field an item's text is for, in the words the engine knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemField {
    /// A password: only a password field takes it.
    Password,
    /// A code a sign-in asks for as its second step.
    Code,
    /// Any other value kept for the site.
    Value,
}
impl ItemField {
    /// The kind `word` names, as the item's keeper and the engine spell it.
    #[must_use]
    pub fn from_word(word: &str) -> Option<Self> {
        match word {
            "password" => Some(Self::Password),
            "code" => Some(Self::Code),
            "value" => Some(Self::Value),
            _ => None,
        }
    }
    /// The word the engine knows it by.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Code => "code",
            Self::Value => "value",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrowserServiceSessionDescriptor {
    pub id: String,
    pub service: Service,
    pub setup_binding: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum BrowserServiceSessionReply {
    List {
        sessions: Vec<BrowserServiceSessionDescriptor>,
    },
    Resolved {
        session: SessionCookies,
    },
    Forgotten {
        id: String,
        setup_binding: String,
    },
    /// The account's password is kept in the workspace's vault, in the entry `item` names
    /// (`vault.item:<entry id>`): what the account names.
    AccountSecretStored {
        account: String,
        item: String,
    },
    /// The secret of the account that names the entry `item`, released to the asking app.
    AccountSecret {
        item: String,
        secret: Zeroizing<String>,
    },
    /// The account's sync is what the workspace's accounts say of it.
    AccountSyncReported {
        account: String,
    },
    /// The item's text for that page action and that site, and the kind of field it is for.
    ItemText {
        invocation: InvocationId,
        text: Zeroizing<String>,
        into: ItemField,
    },
    /// The person is asked first and nothing was released: what whoever asked is told.
    ItemAsked {
        invocation: InvocationId,
        words: String,
    },
    /// The item is not released there, and why, in words that name no secret.
    ItemRefused {
        invocation: InvocationId,
        reason: String,
    },
    /// Whether this session has vault items used for Browser's pages.
    ItemsLent {
        lent: bool,
    },
}
impl std::fmt::Debug for BrowserServiceSessionReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BrowserServiceSessionReply { private request-scoped reply }")
    }
}
fn binding(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn id(value: &str) -> bool {
    browser_continuity_core::sessions::valid_id(value)
}
/// An account id as the person's accounts record keys it (an address, a
/// provider id): bounded, printable.
fn account(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}
fn secret(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ACCOUNT_SECRET_BYTES
}
/// A vault item's reference, as an action names it: a text an item keeps is none.
fn item_reference(value: &str) -> bool {
    tilo_actions::remote::person_vault::ItemRef::parse(value).is_some()
}
/// A server an account's password belongs to: a mail server (`imap://` or `smtp://`, a name and
/// a port, and nothing after them), or a file server's origin.
fn item_server(value: &str) -> bool {
    item_origin(value) || mail_server(value)
}
fn mail_server(value: &str) -> bool {
    value.len() <= MAX_ITEM_ORIGIN_BYTES
        && value
            .strip_prefix("imap://")
            .or_else(|| value.strip_prefix("smtp://"))
            .is_some_and(|name| {
                !name.is_empty()
                    && !name.chars().any(|character| {
                        character.is_control()
                            || character.is_whitespace()
                            || matches!(character, '/' | '?' | '#' | '@' | '\\')
                    })
            })
}
/// The origin a page is at: a scheme of the web, a name and at most a port, and nothing
/// after them.
fn item_origin(value: &str) -> bool {
    value.len() <= MAX_ITEM_ORIGIN_BYTES
        && value
            .strip_prefix("https://")
            .or_else(|| value.strip_prefix("http://"))
            .is_some_and(|name| {
                !name.is_empty()
                    && !name.chars().any(|character| {
                        character.is_control()
                            || character.is_whitespace()
                            || matches!(character, '/' | '?' | '#' | '@' | '\\')
                    })
            })
}
fn item_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_ITEM_TEXT_BYTES
}
fn item_words(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= MAX_ITEM_WORDS_BYTES
}
/// `words` as an answer carries them: trimmed, `otherwise` where there are none, and cut at
/// [`MAX_ITEM_WORDS_BYTES`] on a character's boundary, so a long reason is shortened and
/// never lost.
fn carried_words(words: &str, otherwise: &str) -> String {
    let words = words.trim();
    let words = if words.is_empty() { otherwise } else { words };
    let mut end = words.len().min(MAX_ITEM_WORDS_BYTES);
    while !words.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    words.get(..end).unwrap_or(otherwise).to_owned()
}

impl BrowserServiceSessionCommand {
    pub fn valid(&self) -> bool {
        match self {
            Self::List {} | Self::LendsItems {} => true,
            Self::Resolve {
                id: value,
                setup_binding,
            }
            | Self::Forget {
                id: value,
                setup_binding,
            } => id(value) && binding(setup_binding),
            Self::StoreAccountSecret {
                account: value,
                password,
                username,
                servers,
            } => {
                account(value)
                    && secret(password)
                    && username.len() <= 320
                    && !username.chars().any(char::is_control)
                    && !servers.is_empty()
                    && servers.len() <= 8
                    && servers.iter().all(|server| item_server(server))
            }
            Self::ReleaseAccountSecret { item } => item_reference(item),
            Self::ReportAccountSync {
                account: value,
                health,
                message,
                ..
            } => {
                account(value)
                    && !health.is_empty()
                    && health.len() <= 32
                    && health.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                    && message.len() <= MAX_ITEM_WORDS_BYTES
            }
            Self::UseItem { item, origin, .. } => item_reference(item) && item_origin(origin),
        }
    }
    /// Whether only Browser may send this: its website sessions, and the
    /// vault items its pages are filled from. The account-secret requests
    /// are each authorized by the owner for the app sending them.
    pub fn browser_only(&self) -> bool {
        matches!(
            self,
            Self::List {}
                | Self::Resolve { .. }
                | Self::Forget { .. }
                | Self::UseItem { .. }
                | Self::LendsItems {}
        )
    }
    /// Whether this keeps or releases an account's password, or reports how its sync went:
    /// answered once the workspace's vault or accounts on the device were read or written,
    /// never on the connection's dispatch.
    #[must_use]
    pub const fn account_secret(&self) -> bool {
        matches!(
            self,
            Self::StoreAccountSecret { .. }
                | Self::ReleaseAccountSecret { .. }
                | Self::ReportAccountSync { .. }
        )
    }
    /// Whether this lists, resolves or forgets one of the workspace's website sessions:
    /// answered once the workspace's accounts on the device were read, never on the
    /// connection's dispatch.
    #[must_use]
    pub const fn website_session(&self) -> bool {
        matches!(
            self,
            Self::List {} | Self::Resolve { .. } | Self::Forget { .. }
        )
    }
    /// The page action an item's use is asked for: `None` for every other command.
    #[must_use]
    pub const fn item_use(&self) -> Option<InvocationId> {
        match self {
            Self::UseItem { invocation, .. } => Some(*invocation),
            _ => None,
        }
    }
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if !self.valid() {
            return Err("invalid service-session command");
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(self).map_err(|_| "service-session encoding failed")?,
        );
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES {
            return Err("service-session command too large");
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_COMMAND_BYTES {
            return Err("service-session command too large");
        }
        let command: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid service-session command")?;
        if !command.valid() {
            return Err("invalid service-session command");
        }
        Ok(command)
    }
}
impl BrowserServiceSessionReply {
    pub fn valid(&self) -> bool {
        match self {
            Self::List { sessions } => {
                let mut ids = std::collections::BTreeSet::new();
                sessions.len() <= MAX_BROWSER_SERVICE_SESSIONS
                    && sessions
                        .iter()
                        .all(|s| id(&s.id) && binding(&s.setup_binding) && ids.insert(&s.id))
            }
            Self::Resolved { session } => session.validate().is_ok(),
            Self::Forgotten {
                id: value,
                setup_binding,
            } => id(value) && binding(setup_binding),
            Self::AccountSecretStored {
                account: value,
                item,
            } => account(value) && item_reference(item),
            Self::AccountSecret { item, secret: kept } => item_reference(item) && secret(kept),
            Self::AccountSyncReported { account: value } => account(value),
            Self::ItemText { text, .. } => item_text(text),
            Self::ItemAsked { words, .. } => item_words(words),
            Self::ItemRefused { reason, .. } => item_words(reason),
            Self::ItemsLent { .. } => true,
        }
    }
    /// The answer that the item `invocation` asked for is not released, and why. `reason`
    /// names no secret; it is carried within [`MAX_ITEM_WORDS_BYTES`].
    #[must_use]
    pub fn item_refused(invocation: InvocationId, reason: &str) -> Self {
        Self::ItemRefused {
            invocation,
            reason: carried_words(reason, "The item was not released, and nobody said why."),
        }
    }
    /// The answer that the person is asked first, in the words whoever asked is told.
    #[must_use]
    pub fn item_asked(invocation: InvocationId, words: &str) -> Self {
        Self::ItemAsked {
            invocation,
            words: carried_words(words, "Its person is asked first, so nothing was filled."),
        }
    }
    pub fn matches(&self, command: &BrowserServiceSessionCommand) -> bool {
        match (self, command) {
            (Self::List { .. }, BrowserServiceSessionCommand::List {}) => self.valid(),
            (
                Self::Resolved { session },
                BrowserServiceSessionCommand::Resolve { id, setup_binding },
            ) => {
                self.valid() && &session.session_id == id && &session.setup_binding == setup_binding
            }
            (
                Self::Forgotten {
                    id: forgotten,
                    setup_binding: binding,
                },
                BrowserServiceSessionCommand::Forget { id, setup_binding },
            ) => self.valid() && forgotten == id && binding == setup_binding,
            (
                Self::AccountSecretStored {
                    account: stored, ..
                },
                BrowserServiceSessionCommand::StoreAccountSecret { account, .. },
            ) => self.valid() && stored == account,
            (
                Self::AccountSecret { item: released, .. },
                BrowserServiceSessionCommand::ReleaseAccountSecret { item },
            ) => self.valid() && released == item,
            (
                Self::AccountSyncReported { account: reported },
                BrowserServiceSessionCommand::ReportAccountSync { account, .. },
            ) => self.valid() && reported == account,
            // An item's answer is for the page action that asked, whichever of the three
            // it is.
            (
                Self::ItemText {
                    invocation: answered,
                    ..
                }
                | Self::ItemAsked {
                    invocation: answered,
                    ..
                }
                | Self::ItemRefused {
                    invocation: answered,
                    ..
                },
                BrowserServiceSessionCommand::UseItem { invocation, .. },
            ) => self.valid() && answered == invocation,
            (Self::ItemsLent { .. }, BrowserServiceSessionCommand::LendsItems {}) => true,
            _ => false,
        }
    }
    pub fn encode(&self) -> Result<Zeroizing<Vec<u8>>, &'static str> {
        if !self.valid() {
            return Err("invalid service-session reply");
        }
        let bytes = Zeroizing::new(
            serde_json::to_vec(self).map_err(|_| "service-session encoding failed")?,
        );
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err("service-session reply too large");
        }
        Ok(bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err("service-session reply too large");
        }
        let reply: Self =
            serde_json::from_slice(bytes).map_err(|_| "invalid service-session reply")?;
        if !reply.valid() {
            return Err("invalid service-session reply");
        }
        Ok(reply)
    }
}
#[cfg(feature = "client")]
impl crate::RecordHandle {
    /// Resolve only this admitted Browser channel's own selected credentials.
    pub async fn browser_service_session_command(
        &self,
        command: &BrowserServiceSessionCommand,
    ) -> Result<BrowserServiceSessionReply, crate::NotAvailable> {
        let failed = |message: &str| crate::NotAvailable::Refused(message.into());
        let bytes = command.encode().map_err(failed)?;
        let reader = Zeroizing::new(self.browser_service_session(bytes.to_vec()).await?);
        if reader.len() > MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES {
            return Err(failed("service-session reply too large"));
        }
        let reply = BrowserServiceSessionReply::decode(&reader).map_err(failed)?;
        if !reply.matches(command) {
            return Err(failed("service-session reply does not match request"));
        }
        Ok(reply)
    }
}
#[cfg(test)]
mod tests {
    tilo_test_env::scenario! {
        fn browser_service_session_wire;

        phase forget_wire_requires_exact_selection_and_reply_binding;
        {
            let command = super::BrowserServiceSessionCommand::Forget {
                id: "selected-slack".into(),
                setup_binding: "operation".into(),
            };
            assert_eq!(
                super::BrowserServiceSessionCommand::decode(&command.encode().unwrap()).unwrap(),
                command
            );
            let reply = super::BrowserServiceSessionReply::Forgotten {
                id: "selected-slack".into(),
                setup_binding: "operation".into(),
            };
            assert!(
                super::BrowserServiceSessionReply::decode(&reply.encode().unwrap())
                    .unwrap()
                    .matches(&command)
            );
            assert!(
                !reply.matches(&super::BrowserServiceSessionCommand::Resolve {
                    id: "selected-slack".into(),
                    setup_binding: "operation".into(),
                })
            );
            assert!(
                !reply.matches(&super::BrowserServiceSessionCommand::Forget {
                    id: "selected-slack".into(),
                    setup_binding: "foreign".into(),
                })
            );
            for bytes in [
                br#"{"kind":"forget","id":"selected-slack","setup_binding":"operation","person":"foreign"}"#.as_slice(),
                br#"{"kind":"forget","id":"selected-slack","setup_binding":""}"#.as_slice(),
                br#"{"kind":"forget","id":"../foreign","setup_binding":"operation"}"#.as_slice(),
            ] {
                assert!(super::BrowserServiceSessionCommand::decode(bytes).is_err());
            }
        }

        phase an_account_password_is_kept_for_mail_and_file_servers_alone;
        {
            let keeping = |servers: &[&str]| super::BrowserServiceSessionCommand::StoreAccountSecret {
                account: "nextcloud-1".into(),
                password: zeroize::Zeroizing::new("app-password".into()),
                username: "ada".into(),
                servers: servers.iter().map(|&server| server.to_owned()).collect(),
            };
            for servers in [
                &["imap://mail.example:993", "smtp://mail.example:465"][..],
                &["https://cloud.example"],
                &["http://127.0.0.1:42559"],
            ] {
                assert!(keeping(servers).valid(), "{servers:?}");
            }
            for servers in [
                &[][..],
                &["https://cloud.example/remote.php"],
                &["ftp://cloud.example"],
                &["https://ada@cloud.example"],
            ] {
                assert!(!keeping(servers).valid(), "{servers:?}");
            }
        }

        phase rejects_identity_injection_and_redacts_malformed_credentials;
        {
            assert!(
                BrowserServiceSessionCommand::decode(br#"{"kind":"list","person":"someone"}"#).is_err()
            );
            let error = BrowserServiceSessionReply::decode(
                br#"{"kind":"resolved","session":"private-synthetic-value"}"#,
            )
            .unwrap_err();
            assert!(!error.contains("private-synthetic-value"));
            assert!(
                BrowserServiceSessionReply::decode(&vec![
                    0;
                    MAX_BROWSER_SERVICE_SESSION_REPLY_BYTES + 1
                ])
                .is_err()
            );
        }

        phase selected_private_reply_roundtrips_and_binds_exact_request;
        {
            use browser_continuity_core::sessions::{Cookie, SameSite};
            let session = SessionCookies {
                schema: 1,
                session_id: "selected-session".into(),
                service: Service::Slack,
                identity_public_key: [21; 32],
                setup_binding: "prepared-binding".into(),
                source_snapshot_digest: [22; 32],
                origin_storage: Vec::new(),
                cookies: vec![Cookie {
                    host: "app.slack.com".into(),
                    domain: "app.slack.com".into(),
                    host_only: true,
                    name: "synthetic-session".into(),
                    value: "synthetic-private-value".into(),
                    path: "/".into(),
                    expires_unix_seconds: None,
                    secure: true,
                    http_only: true,
                    same_site: SameSite::Lax,
                }],
            };
            let reply = BrowserServiceSessionReply::Resolved { session };
            let encoded = reply.encode().unwrap();
            let decoded = BrowserServiceSessionReply::decode(&encoded).unwrap();
            assert!(decoded.matches(&BrowserServiceSessionCommand::Resolve {
                id: "selected-session".into(),
                setup_binding: "prepared-binding".into()
            }));
            assert!(!decoded.matches(&BrowserServiceSessionCommand::Resolve {
                id: "other-session".into(),
                setup_binding: "prepared-binding".into()
            }));
            assert!(!decoded.matches(&BrowserServiceSessionCommand::Resolve {
                id: "selected-session".into(),
                setup_binding: "other-binding".into()
            }));
            assert!(!format!("{decoded:?}").contains("synthetic-private-value"));
            assert!(!decoded.matches(&BrowserServiceSessionCommand::List {}));
        }

        phase bounds_and_deduplicates_public_descriptors;
        {
            let descriptor = BrowserServiceSessionDescriptor {
                id: "opaque-session".into(),
                service: Service::Slack,
                setup_binding: "prepared-binding".into(),
            };
            assert!(
                BrowserServiceSessionReply::List {
                    sessions: vec![descriptor.clone()]
                }
                .valid()
            );
            assert!(
                !BrowserServiceSessionReply::List {
                    sessions: vec![descriptor.clone(), descriptor]
                }
                .valid()
            );
            assert!(
                !BrowserServiceSessionCommand::Resolve {
                    id: "../escape".into(),
                    setup_binding: "binding".into()
                }
                .valid()
            );
        }

        phase an_item_use_names_a_page_action_an_item_by_reference_and_an_origin;
        {
            let invocation = tilo_actions::InvocationId::new();
            let command = BrowserServiceSessionCommand::UseItem {
                invocation,
                item: ITEM.into(),
                origin: ORIGIN.into(),
            };
            assert!(command.browser_only());
            assert_eq!(command.item_use(), Some(invocation));
            assert_eq!(BrowserServiceSessionCommand::List {}.item_use(), None);
            assert_eq!(
                BrowserServiceSessionCommand::decode(&command.encode().unwrap()).unwrap(),
                command
            );
            // As it is spelled on the wire, and with one word more than it names.
            let spelled = |more: &str| {
                format!(
                    r#"{{"kind":"use-item","invocation":"{}","item":"{ITEM}","origin":"{ORIGIN}"{more}}}"#,
                    invocation.0
                )
            };
            assert_eq!(
                BrowserServiceSessionCommand::decode(spelled("").as_bytes()).unwrap(),
                command
            );
            for more in [r#","person":"foreign""#, r#","reason":"because""#] {
                assert!(BrowserServiceSessionCommand::decode(spelled(more).as_bytes()).is_err());
            }
            for (item, origin) in [
                // A text an item keeps is never an item's name.
                (MADE_UP_TEXT, ORIGIN),
                ("", ORIGIN),
                ("vault.item:not-an-id", ORIGIN),
                (ITEM, ""),
                (ITEM, "https://"),
                (ITEM, "login.example.test"),
                (ITEM, "file:///home/ada/page.html"),
                (ITEM, "https://login.example.test/sign-in"),
                (ITEM, "https://login.example.test?next=1"),
                (ITEM, "https://someone@login.example.test"),
                (ITEM, "https://login.example.test\n"),
                (ITEM, "https://login.example.test other.example.test"),
            ] {
                let command = BrowserServiceSessionCommand::UseItem {
                    invocation,
                    item: item.into(),
                    origin: origin.into(),
                };
                assert!(
                    !command.valid() && command.encode().is_err(),
                    "an item of {} bytes at {origin:?} is no item's use",
                    item.len()
                );
            }
            assert!(
                !BrowserServiceSessionCommand::UseItem {
                    invocation,
                    item: ITEM.into(),
                    origin: format!("https://{}.example.test", "a".repeat(MAX_ITEM_ORIGIN_BYTES)),
                }
                .valid()
            );
            // The question whether items are lent at all, and its answer.
            let question = BrowserServiceSessionCommand::LendsItems {};
            assert!(question.browser_only());
            assert_eq!(
                BrowserServiceSessionCommand::decode(br#"{"kind":"lends-items"}"#).unwrap(),
                question
            );
            for lent in [true, false] {
                let answer = BrowserServiceSessionReply::decode(
                    &BrowserServiceSessionReply::ItemsLent { lent }.encode().unwrap(),
                )
                .unwrap();
                assert!(
                    matches!(answer, BrowserServiceSessionReply::ItemsLent { lent: said } if said == lent)
                );
                assert!(answer.matches(&question));
                assert!(!answer.matches(&command));
                assert!(!answer.matches(&BrowserServiceSessionCommand::List {}));
            }
        }

        phase an_item_use_is_answered_for_its_own_page_action_in_one_of_three_ways;
        {
            let invocation = tilo_actions::InvocationId::new();
            let asked = BrowserServiceSessionCommand::UseItem {
                invocation,
                item: ITEM.into(),
                origin: ORIGIN.into(),
            };
            let another = BrowserServiceSessionCommand::UseItem {
                invocation: tilo_actions::InvocationId::new(),
                item: ITEM.into(),
                origin: ORIGIN.into(),
            };
            let carried = |answer: &BrowserServiceSessionReply| {
                BrowserServiceSessionReply::decode(&answer.encode().unwrap()).unwrap()
            };
            let released = carried(&BrowserServiceSessionReply::ItemText {
                invocation,
                text: Zeroizing::new(MADE_UP_TEXT.to_owned()),
                into: ItemField::Password,
            });
            // The text arrives whole with the kind of field it is for. Nothing here prints it.
            assert!(matches!(
                &released,
                BrowserServiceSessionReply::ItemText { text, into: ItemField::Password, .. }
                    if text.as_str() == MADE_UP_TEXT
            ));
            let first = carried(&BrowserServiceSessionReply::item_asked(
                invocation,
                "  Ada is asked whether the assistant may sign in.  ",
            ));
            assert!(matches!(
                &first,
                BrowserServiceSessionReply::ItemAsked { words, .. }
                    if words == "Ada is asked whether the assistant may sign in."
            ));
            let refused = carried(&BrowserServiceSessionReply::item_refused(
                invocation,
                "The item is not kept for that site.",
            ));
            assert!(matches!(
                &refused,
                BrowserServiceSessionReply::ItemRefused { reason, .. }
                    if reason == "The item is not kept for that site."
            ));
            for answer in [&released, &first, &refused] {
                assert!(answer.matches(&asked));
                assert!(!answer.matches(&another));
                assert!(!answer.matches(&BrowserServiceSessionCommand::List {}));
                assert!(!answer.matches(&BrowserServiceSessionCommand::LendsItems {}));
            }
            // A refusal with no reason still says something, and a long one is shortened on a
            // character's boundary and not lost.
            assert!(matches!(
                BrowserServiceSessionReply::item_refused(invocation, " "),
                BrowserServiceSessionReply::ItemRefused { reason, .. } if !reason.trim().is_empty()
            ));
            let long = BrowserServiceSessionReply::item_refused(
                invocation,
                &"ä".repeat(MAX_ITEM_WORDS_BYTES),
            );
            assert!(long.valid());
            assert!(matches!(
                &long,
                BrowserServiceSessionReply::ItemRefused { reason, .. }
                    if reason.len() == MAX_ITEM_WORDS_BYTES && reason.chars().all(|character| character == 'ä')
            ));
            // An empty text and one longer than anything sealed are no answer.
            for text in [String::new(), "x".repeat(MAX_ITEM_TEXT_BYTES + 1)] {
                let answer = BrowserServiceSessionReply::ItemText {
                    invocation,
                    text: Zeroizing::new(text),
                    into: ItemField::Value,
                };
                assert!(!answer.valid() && answer.encode().is_err());
                assert!(!answer.matches(&asked));
            }
            for (kind, word) in [
                (ItemField::Password, "password"),
                (ItemField::Code, "code"),
                (ItemField::Value, "value"),
            ] {
                assert_eq!(ItemField::from_word(word), Some(kind));
                assert_eq!(kind.as_str(), word);
                assert_eq!(serde_json::to_value(kind).unwrap(), word);
            }
            assert_eq!(ItemField::from_word("Password"), None);
            assert_eq!(ItemField::from_word(""), None);
        }

        phase nothing_printed_of_an_item_use_holds_the_items_text;
        {
            let invocation = tilo_actions::InvocationId::new();
            let text = || Zeroizing::new(MADE_UP_TEXT.to_owned());
            let printed = [
                format!(
                    "{:?}",
                    BrowserServiceSessionCommand::UseItem {
                        invocation,
                        item: ITEM.into(),
                        origin: ORIGIN.into(),
                    }
                ),
                format!("{:?}", BrowserServiceSessionCommand::LendsItems {}),
                format!(
                    "{:?}",
                    BrowserServiceSessionReply::ItemText {
                        invocation,
                        text: text(),
                        into: ItemField::Password,
                    }
                ),
                format!(
                    "{:#?}",
                    BrowserServiceSessionReply::ItemText {
                        invocation,
                        text: text(),
                        into: ItemField::Code,
                    }
                ),
                format!("{:?}", BrowserServiceSessionReply::item_asked(invocation, "asked")),
                format!("{:?}", BrowserServiceSessionReply::item_refused(invocation, "refused")),
                format!("{:?}", BrowserServiceSessionReply::ItemsLent { lent: true }),
                format!("{:?}", ItemField::Password),
                // What an answer that is malformed around its text is refused with.
                BrowserServiceSessionReply::decode(
                    format!(r#"{{"kind":"item-text","text":"{MADE_UP_TEXT}"}}"#).as_bytes(),
                )
                .unwrap_err()
                .to_owned(),
                BrowserServiceSessionReply::decode(
                    format!(
                        r#"{{"kind":"item-text","invocation":"{}","text":"{MADE_UP_TEXT}","into":"elsewhere"}}"#,
                        invocation.0
                    )
                    .as_bytes(),
                )
                .unwrap_err()
                .to_owned(),
                // What a text longer than anything sealed is refused with.
                BrowserServiceSessionReply::ItemText {
                    invocation,
                    text: Zeroizing::new(MADE_UP_TEXT.repeat(MAX_ITEM_TEXT_BYTES)),
                    into: ItemField::Value,
                }
                .encode()
                .err()
                .map_or_else(|| ENCODED.to_owned(), str::to_owned),
                // What a command that names a text where an item's reference belongs is refused
                // with.
                BrowserServiceSessionCommand::UseItem {
                    invocation,
                    item: MADE_UP_TEXT.into(),
                    origin: ORIGIN.into(),
                }
                .encode()
                .err()
                .map_or_else(|| ENCODED.to_owned(), str::to_owned),
            ];
            for printed in &printed {
                assert!(
                    !printed.contains(MADE_UP_TEXT) && !printed.contains("made-up"),
                    "one of the prints holds the item's text"
                );
                assert_ne!(printed, ENCODED, "what must be refused was encoded");
            }
            // The command's print still says what it is for, which is no secret.
            assert!(printed.iter().any(|printed| printed.contains(ORIGIN)));
        }
    }
    use super::*;

    /// A vault item's reference, made up.
    const ITEM: &str = "vault.item:0123456789abcdef0123456789abcdef";
    /// Where the made-up item is kept for.
    const ORIGIN: &str = "https://login.example.test";
    /// What the made-up item keeps: nobody's password, and it says so.
    const MADE_UP_TEXT: &str = "made-up item text, nobody's password/7+7%";
    /// What stands in a list of prints where something that must be refused was encoded.
    const ENCODED: &str = "encoded although it must be refused";
}
