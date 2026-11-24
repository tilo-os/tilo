//! The person's mail accounts, kept where their workspace runs.
//!
//! A page has no mail connection and no vault of its own. Adding a mail
//! account from a browser therefore asks the host that runs the workspace:
//! it opens the server's session with the password once, keeps the account
//! and its secret in the workspace's own vault, and answers what it kept.
//! Nothing here carries a secret back: a reading names an account's address
//! and its servers, never its password.
//!
//! A page that reaches no mail server reads and sends its mail through the
//! same host: [`FETCH`] answers a folder as the server holds it, a message
//! is begun ([`BEGIN_SEND`]), its files' bytes follow in pieces
//! ([`SEND_PIECE`]) — a page holds no MIME builder, so the host writes the
//! message — and [`SEND`] hands it to its server; [`MARK`] tells a server
//! what the reader changed about a message. The host opens each session with
//! the secret its vault keeps, so nothing in this vocabulary carries one: a
//! member's device that may not read the vault reads and sends the
//! workspace's mail here, and is never handed a password.
//!
//! The same actions answer a device that keeps its own accounts, so the page
//! and the device read and change accounts through one vocabulary.
//!
//! A page open in a tab keeps the build it loaded while its server moves
//! on, so every message here reads past a field it does not know: a field
//! is added as an optional one, and an older page reads the newer answer.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider that answers the person's mail accounts under.
pub const PROVIDER: &str = "tilo.accounts";
/// The resource the accounts are actions of.
pub const RESOURCE: &str = "accounts";
/// The mail accounts the person's vault keeps, as a [`MailAccounts`].
pub const LIST: &str = "list";
/// Live: the same accounts as [`LIST`], sent again whenever they change,
/// until the caller cancels. What a window follows so an account added in
/// another one reaches it without asking again.
pub const WATCH: &str = "watch";
/// Add a mail account, once its server accepted its sign-in
/// ([`AddMailAccount`]), and answer it as a [`MailAccount`].
pub const ADD_MAIL: &str = "add-mail";
/// Add a mail account whose sign-in the person's vault already keeps
/// ([`AddMailWithItem`]), and answer it as a [`MailAccount`]. The password is
/// named by reference and the host resolves it, only for the two servers the
/// item belongs to. It carries no secret, so unlike [`ADD_MAIL`] an agent
/// acting for the person may ask it.
pub const ADD_MAIL_WITH_ITEM: &str = "add-mail-with-item";
/// Sign an account in again (`account`) with the entry the workspace's vault
/// keeps for it, through the same sign-in the account was added with: the
/// accounts reading then says how it went.
pub const RECONNECT: &str = "reconnect";
/// Check that an account's server still takes the sign-in the workspace's vault
/// keeps (`account`), answering the probe's own words `{ "result": "…" }`, or
/// the reason it refused them. Nothing is written.
pub const CHECK: &str = "check";
/// Replace an account's saved sign-in ([`ReplacePassword`]) once the server took
/// the new one: the vault's entry is the account's own, under the same reference.
pub const REPLACE_PASSWORD: &str = "replace-password";
/// Remove a mail account and its secret (`account`), answering the accounts
/// that remain as a [`MailAccounts`].
pub const REMOVE: &str = "remove";
/// Where the mail of an address lives ([`FindServers`]), worked out from the
/// address alone, answering [`FoundServers`]. Nothing signs in: the person
/// sees the servers before they type a password for them.
pub const FIND_SERVERS: &str = "find-servers";
/// Begin signing in on a provider's own page ([`BeginSignIn`]), answering
/// where the person's browser goes ([`SignInStep`]). The server finishes the
/// sign-in itself when the provider sends the browser back, keeps the
/// account, and every window following [`WATCH`] sees it arrive; until then
/// [`MailAccounts::signing_in`] names it.
pub const BEGIN_SIGN_IN: &str = "begin-sign-in";
/// Stop waiting for a sign-in begun with [`BEGIN_SIGN_IN`] (`signIn`), or
/// forget one that failed, answering the accounts as [`MailAccounts`].
pub const CANCEL_SIGN_IN: &str = "cancel-sign-in";
/// Say which of an account's services the person uses ([`SetServices`]),
/// answering the accounts as [`MailAccounts`].
pub const SET_SERVICES: &str = "set-services";
/// Fetch one folder of one account ([`FetchMail`]), answering what the
/// server holds as a [`FetchedMail`].
pub const FETCH: &str = "fetch";
/// Begin a message to send ([`BeginSend`]), answering the id [`SEND`] sends
/// it by ([`StartedSend`]).
pub const BEGIN_SEND: &str = "begin-send";
/// One piece of a begun message's file ([`SendPiece`]), answering the bytes
/// the host holds of that file ([`PieceKept`]).
pub const SEND_PIECE: &str = "send-piece";
/// Send the message begun under an id ([`SendId`]), answering what became of
/// it once it left ([`SentMail`]), or the reason it did not leave.
pub const SEND: &str = "send";
/// Tell one account's server what changed about a message ([`MarkMail`]):
/// read, starred, moved or removed.
pub const MARK: &str = "mark";
/// Read one message whole ([`ReadMail`]), answering it as a
/// [`FetchedMessage`].
pub const READ: &str = "read";
/// Search one account on its server ([`SearchMail`]), answering what it
/// found as a [`FoundMail`].
pub const SEARCH: &str = "search";
/// Keep the message begun under an id ([`SendId`]) in the account's Drafts
/// folder instead of sending it. Answers nothing.
pub const SAVE_DRAFT: &str = "save-draft";
/// What arrived in the person's inboxes after a place in their arrival log
/// ([`ArrivedAfter`]), answering a [`MailArrivals`]. Asking is listening:
/// from the first time it is asked, the host watches the person's inboxes
/// for as long as it runs, with no window open anywhere.
pub const ARRIVED: &str = "arrived";
/// The kind a message in one of the person's accounts is named by where something is made from
/// it ([`crate::params::FROM_ARGUMENT`]): `tl.<app>.<Type>` under [`PROVIDER`], so a read of it is
/// decided at that app's node, as every read of the person's accounts is.
pub const MESSAGE: &str = "tl.tilo.accounts.Message";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// Who provides an account: a provider whose own page signs a person in, one
/// whose sign-in is an app password, or any other mail provider.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Provider {
    /// Google: Gmail, Google Calendar and Google Drive, signed in on
    /// Google's page.
    Google,
    /// Microsoft: Outlook.com and Microsoft 365 mail and `OneDrive`, signed
    /// in on Microsoft's page.
    Microsoft,
    /// iCloud: mail, calendars and contacts with an app-specific password.
    Icloud,
    /// A Nextcloud server: files, calendars and contacts, signed in on the
    /// server's own page (Login Flow v2).
    Nextcloud,
    /// Any other mail provider: an address and its password.
    #[default]
    Other,
}

impl Provider {
    /// The provider by the name the wire gives it.
    #[must_use]
    pub fn from_key(key: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(key.to_owned())).ok()
    }

    /// The name the wire gives it.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Google => "google",
            Self::Microsoft => "microsoft",
            Self::Icloud => "icloud",
            Self::Nextcloud => "nextcloud",
            Self::Other => "other",
        }
    }

    /// Whether the person signs in on the provider's own page
    /// ([`BEGIN_SIGN_IN`]) rather than with a password.
    #[must_use]
    pub const fn signs_in_on_its_page(self) -> bool {
        matches!(self, Self::Google | Self::Microsoft | Self::Nextcloud)
    }

    /// The services an account of this provider can feed.
    #[must_use]
    pub const fn offers(self) -> Services {
        match self {
            Self::Google | Self::Icloud => Services {
                mail: true,
                calendar: true,
                files: false,
            },
            Self::Microsoft | Self::Other => Services {
                mail: true,
                calendar: false,
                files: false,
            },
            Self::Nextcloud => Services {
                mail: false,
                calendar: true,
                files: false,
            },
        }
    }
}

/// Which of an account's services the person uses. One account feeds every
/// app: Mail reads it where `mail` is on, Calendar where `calendar` is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Services {
    #[serde(default)]
    pub mail: bool,
    #[serde(default)]
    pub calendar: bool,
    #[serde(default)]
    pub files: bool,
}

impl Default for Services {
    /// A mail account's: its mail.
    fn default() -> Self {
        Self {
            mail: true,
            calendar: false,
            files: false,
        }
    }
}

impl Services {
    /// Only what `provider` offers of these.
    #[must_use]
    pub const fn within(self, provider: Provider) -> Self {
        let offered = provider.offers();
        Self {
            mail: self.mail && offered.mail,
            calendar: self.calendar && offered.calendar,
            files: self.files && offered.files,
        }
    }

    /// Whether none is on.
    #[must_use]
    pub const fn none(self) -> bool {
        !(self.mail || self.calendar || self.files)
    }
}

/// One account, as [`LIST`] and [`ADD_MAIL`] answer it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccount {
    /// The account's id, as [`REMOVE`] names it.
    pub id: String,
    pub address: String,
    pub display_name: String,
    pub incoming_host: String,
    pub incoming_port: u16,
    pub outgoing_host: String,
    pub outgoing_port: u16,
    /// Whether the person's vault keeps the account's sign-in. A host
    /// never answers the secret itself.
    pub has_secret: bool,
    /// Other names and addresses it sends as, each `Name <address>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
    /// Who provides it.
    #[serde(default)]
    pub provider: Provider,
    /// Which of its services the person uses.
    #[serde(default)]
    pub services: Services,
    /// The server a Nextcloud account signs in to, or the calendar server of
    /// another; empty where there is none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
}

/// A sign-in begun with [`BEGIN_SIGN_IN`] that has not made an account yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SigningIn {
    /// The sign-in's id, as [`CANCEL_SIGN_IN`] names it.
    pub id: String,
    pub provider: Provider,
    /// The Nextcloud server it signs in to; empty for the others.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// Why it made no account, once it failed: in words the person reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<String>,
}

/// Every mail account the person's vault keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAccounts {
    pub accounts: Vec<MailAccount>,
    /// The account the person chose last, when the store names one; the
    /// first account otherwise.
    #[serde(default)]
    pub active: String,
    /// The sign-ins begun on a provider's page that made no account yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signing_in: Vec<SigningIn>,
    /// The providers this server can sign in with now, in the order its
    /// page offers them: Google or Microsoft once a web client is
    /// configured for it, Nextcloud always. Only these are worth a button.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub signs_in_with: Vec<Provider>,
}

/// An address whose servers to find ([`FIND_SERVERS`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindServers {
    pub address: String,
}

/// One server and its port.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAt {
    /// The server's name.
    pub name: String,
    pub port: u16,
}

/// Where an address's mail lives ([`FIND_SERVERS`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundServers {
    /// Who provides it. A provider that signs in on its own page
    /// ([`Provider::signs_in_on_its_page`]) is signed in to there, never with
    /// a password.
    pub provider: Provider,
    pub incoming: ServerAt,
    pub outgoing: ServerAt,
    /// The sign-in name, when the provider says it is not the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

/// A sign-in to begin on a provider's own page ([`BEGIN_SIGN_IN`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginSignIn {
    /// Google, Microsoft or Nextcloud.
    pub provider: Provider,
    /// The Nextcloud server's address; empty for the others.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub server: String,
    /// What the account is used for: the provider is asked for only that.
    #[serde(default)]
    pub services: Services,
    /// The account this signs in to again, when its sign-in stopped working;
    /// empty for a new account.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub account: String,
}

/// Where the person's browser goes to sign in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignInStep {
    /// The sign-in's id, as [`MailAccounts::signing_in`] and
    /// [`CANCEL_SIGN_IN`] name it.
    pub id: String,
    /// The provider's page to open.
    pub open: String,
}

/// A sign-in to stop waiting for ([`CANCEL_SIGN_IN`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelSignIn {
    pub sign_in: String,
}

/// The services one account feeds ([`SET_SERVICES`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetServices {
    pub account: String,
    pub services: Services,
}

/// A mail account to add. The password travels here and nowhere back: the
/// host keeps it in the person's vault and answers only [`MailAccount`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMailAccount {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is not the address itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    /// The incoming (IMAP) server. Empty: the server finds both from
    /// the address ([`FIND_SERVERS`]).
    #[serde(default)]
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
    /// The outgoing (SMTP) server. Empty: found with the incoming one.
    #[serde(default)]
    pub outgoing_host: String,
    #[serde(default = "default_submissions_port")]
    pub outgoing_port: u16,
    /// The account's sign-in. Empty for an account another mail program
    /// moved in whose sign-in the person has not given yet.
    #[serde(default)]
    pub password: String,
    /// Other names and addresses it sends as, each `Name <address>`.
    #[serde(default)]
    pub identities: Vec<String>,
    /// What the account is used for. An iCloud account's calendars are
    /// kept with the same app password where `calendar` is on.
    #[serde(default)]
    pub services: Services,
}

impl std::fmt::Debug for AddMailAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddMailAccount")
            .field("address", &self.address)
            .field("display_name", &self.display_name)
            .field("username", &self.username)
            .field("incoming_host", &self.incoming_host)
            .field("incoming_port", &self.incoming_port)
            .field("outgoing_host", &self.outgoing_host)
            .field("outgoing_port", &self.outgoing_port)
            .field("services", &self.services)
            .finish_non_exhaustive()
    }
}

const fn default_imaps_port() -> u16 {
    993
}

const fn default_submissions_port() -> u16 {
    465
}

/// A mail account to add with a sign-in the person's vault keeps. Nothing
/// here is a secret: `password` names the vault item, which must be a login
/// that belongs to both servers (`imap://<incoming host>:<port>` and
/// `smtp://<outgoing host>:<port>`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddMailWithItem {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is neither the item's own nor the address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
    pub outgoing_host: String,
    #[serde(default = "default_submissions_port")]
    pub outgoing_port: u16,
    /// The vault item that is the account's sign-in.
    pub password: super::person_vault::ItemRef,
    /// Why whoever asks needs it, in a sentence its person reads when they
    /// are asked first; empty when nothing was said.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// One address as mail carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAddress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

/// A file a message carries beside its words.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAttachment {
    pub filename: String,
    /// The part's media type.
    pub mime: String,
    /// Its size in bytes.
    pub size: u64,
    /// The `Content-ID` of a part the message's HTML shows in place.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_id: Option<String>,
}

/// One message as a server holds it, read by the host that keeps the
/// account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedMessage {
    /// The server's `UID`, which names the message within its folder.
    pub uid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub from: MailAddress,
    #[serde(default)]
    pub to: Vec<MailAddress>,
    #[serde(default)]
    pub cc: Vec<MailAddress>,
    pub subject: String,
    /// The message as plain text.
    pub body: String,
    /// The `text/html` part, when the message really carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_body: Option<String>,
    #[serde(default)]
    pub attachments: Vec<MailAttachment>,
    /// When it was sent, in minutes since the Unix epoch.
    pub minutes: i64,
    pub read: bool,
    pub starred: bool,
    /// Whether this carries only the start of the message's words, because
    /// the whole would not fit the answer it came in. [`READ`] answers the
    /// message whole.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// What the reader did to a message the server no longer reports as such.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailFlag {
    pub uid: u32,
    pub read: bool,
    pub starred: bool,
}

/// Ask one folder of one account, by its key (`inbox`, `sent`, …).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folder's key: `inbox`, `sent`, `drafts`, `archive`, `trash` or
    /// `junk`.
    pub folder: String,
    /// The newest `UID` the reader already holds of this folder, and the
    /// validity it was read under: what is newer is what arrived. Absent
    /// for a folder read for the first time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_uid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_uid_validity: Option<u32>,
    /// The oldest `UID` the reader holds of this folder, when it asks for
    /// the page before that one instead of for what arrived: a reader
    /// scrolling past the oldest message it holds. Read under
    /// `known_uid_validity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_uid: Option<u32>,
}

/// A folder as its server answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedMail {
    /// The account's id, as the request named it.
    pub account: String,
    /// The folder's key, as the request named it.
    pub folder: String,
    pub uid_validity: u32,
    /// Whether the folder started over: its validity changed, so nothing
    /// the reader held of it is there any more.
    pub reset: bool,
    /// Whether the server holds messages older than the ones answered.
    pub has_older: bool,
    /// The newest `UID` in the folder, which the next read continues after.
    pub newest_uid: u32,
    pub messages: Vec<FetchedMessage>,
    /// The messages the reader already holds whose read or starred state
    /// the server reports differently.
    #[serde(default)]
    pub flags: Vec<MailFlag>,
}

/// Ask for one message, by where its server holds it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folder's key, as [`FetchMail`] names one.
    pub folder: String,
    pub uid: u32,
    /// The validity the `UID` was read under.
    pub uid_validity: u32,
}

/// The most messages a search answers of one folder.
pub const MAX_FOUND: u32 = 25;

/// Ask an account's server which messages it holds that match. Every part
/// that is given has to match; at least one is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folders to look in, by their keys. The inbox and the archive
    /// when none is named.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// Words the message holds anywhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// A name or address it came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// A name or address it went to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Words of its subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// The most messages answered of each folder, newest first:
    /// [`MAX_FOUND`] when absent or larger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// What a search found, folder by folder.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundMail {
    pub folders: Vec<FoundFolder>,
}

/// The messages a search found in one folder, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundFolder {
    /// The folder's key, as the search named it.
    pub folder: String,
    pub uid_validity: u32,
    pub messages: Vec<FetchedMessage>,
}

/// One message that reached an inbox while the person's host watched it:
/// what it is about, and where its server holds it. The message itself
/// stays on its server, and [`READ`] answers it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailArrival {
    /// Its place in the person's arrival log, which only grows.
    pub seq: u64,
    /// What names this arrival for good: the same for the same message of
    /// the same account however often it is told.
    pub id: String,
    /// When the host saw it, in milliseconds since the Unix epoch.
    pub at_ms: i64,
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folder's key: `inbox`.
    pub folder: String,
    pub uid: u32,
    pub uid_validity: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    pub from: MailAddress,
    #[serde(default)]
    pub to: Vec<MailAddress>,
    #[serde(default)]
    pub cc: Vec<MailAddress>,
    pub subject: String,
    /// The start of what it says, as plain text.
    pub excerpt: String,
    #[serde(default)]
    pub attachments: Vec<MailAttachment>,
    /// When it was sent, in minutes since the Unix epoch.
    pub minutes: i64,
}

impl MailArrival {
    /// The message this arrival tells of, as what an invocation is made from: named by the
    /// arrival's id, which stays the same for the same message, and never copied.
    #[must_use]
    pub fn made_from(&self) -> crate::params::MadeFrom {
        crate::params::MadeFrom {
            resource: self.id.clone(),
            kind: MESSAGE.to_owned(),
        }
    }

    /// The arrival as plain facts, each under one name, for whoever
    /// compares them: a trigger's filter, a step's input. `id`, `account`,
    /// `folder`, `uid`, `uidValidity`, `from` (the address), `fromName`,
    /// `to` (the addresses, comma-separated), `subject`, `excerpt`,
    /// `messageId`, `inReplyTo`, `hasAttachments`, `attachments` (their
    /// names), `atMs` and `minutes`.
    #[must_use]
    pub fn facts(&self) -> serde_json::Map<String, serde_json::Value> {
        let addresses = |list: &[MailAddress]| {
            list.iter()
                .map(|address| address.email.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let facts = serde_json::json!({
            "id": self.id,
            "account": self.account,
            "folder": self.folder,
            "uid": self.uid,
            "uidValidity": self.uid_validity,
            "from": self.from.email,
            "fromName": self.from.name.clone().unwrap_or_default(),
            "to": addresses(&self.to),
            "cc": addresses(&self.cc),
            "subject": self.subject,
            "excerpt": self.excerpt,
            "messageId": self.message_id.clone().unwrap_or_default(),
            "inReplyTo": self.in_reply_to.clone().unwrap_or_default(),
            "hasAttachments": !self.attachments.is_empty(),
            "attachments": self
                .attachments
                .iter()
                .map(|attachment| attachment.filename.as_str())
                .collect::<Vec<_>>(),
            "atMs": self.at_ms,
            "minutes": self.minutes,
        });
        match facts {
            serde_json::Value::Object(facts) => facts,
            _ => serde_json::Map::new(),
        }
    }
}

/// Ask what arrived after a place in the person's arrival log: everything
/// the log still keeps when no place is named.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArrivedAfter {
    #[serde(default)]
    pub after: u64,
}

/// What arrived, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailArrivals {
    pub arrivals: Vec<MailArrival>,
    /// The newest place the log has given out: what the next ask names as
    /// `after` to be answered only what arrives from now.
    pub newest: u64,
}

/// A message to send through one of the person's accounts. The host writes
/// it as mail and hands it to the server; nothing of it comes back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The name and address it is sent as.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    pub from_address: String,
    /// Recipients, comma-separated.
    pub to: String,
    #[serde(default)]
    pub cc: String,
    /// Blind recipients: they receive it, and no copy carries their names.
    #[serde(default)]
    pub bcc: String,
    pub subject: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub html_body: Option<String>,
    /// The `Message-ID` to write, so a reply joins its conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Vec<String>,
}

/// The most files one message carries.
pub const MAX_SEND_FILES: usize = 16;
/// The most one file may be.
pub const MAX_SEND_FILE_BYTES: u64 = 32 * 1024 * 1024;
/// The most a message's files may be together.
pub const MAX_SEND_TOTAL_BYTES: u64 = 128 * 1024 * 1024;
/// The most raw bytes one [`SEND_PIECE`] carries: what an action frame holds
/// beside the rest of the request.
pub const MAX_SEND_PIECE_BYTES: usize = 256 * 1024;

/// A message to begin sending: what it says and whom it goes to, its files,
/// and the calendar invitation it carries. The host writes the message as
/// mail — a page has no clock to make a MIME boundary with — and hands it to
/// the account's server once [`SEND`] says so.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeginSend {
    pub message: SendMail,
    /// Bounded RFC 5545 `METHOD:REQUEST` payload for a calendar invitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calendar_ics: Option<String>,
    #[serde(default)]
    pub files: Vec<SendFile>,
}

/// A file a begun message carries. Its bytes follow in [`SEND_PIECE`]s, in
/// order, until the host holds `size` of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendFile {
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

/// What [`BEGIN_SEND`] answers: the id [`SEND`] and [`SEND_PIECE`] name the
/// message by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartedSend {
    pub send: String,
}

/// One piece of one of a begun message's files: its bytes at `offset`,
/// hex-encoded so the frame stays self-describing text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendPiece {
    pub send: String,
    /// The file's place in the message's `files`.
    pub file: u32,
    pub offset: u64,
    pub bytes: String,
}

/// What [`SEND_PIECE`] answers: the bytes the host holds of that file, so a
/// piece that went astray resumes where it got to.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PieceKept {
    pub written: u64,
}

/// The message [`SEND`] sends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendId {
    pub send: String,
}

/// What [`SEND`] answers once the message left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentMail {
    /// Why the account's Sent folder holds no copy of the message, when the
    /// host tried to file one and could not. The message left all the same.
    /// Absent when the copy is filed, the folder already held it, or the
    /// account's server files what it sends by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfiled: Option<String>,
}

/// What one message's server is told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folder key the message was read in.
    pub folder: String,
    pub uid: u32,
    pub uid_validity: u32,
    /// Its `Message-ID`, which finds it when where it is held is not known
    /// (`uid` 0) or its folder was numbered anew: a draft's earlier copy is
    /// known by nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Its read state, when that changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    /// Its starred state, when that changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
    /// The folder key it moves to, when it moved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_folder: Option<String>,
    /// Whether it is removed for good.
    #[serde(default)]
    pub delete: bool,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    /// A tab whose page is older than its server reads the server's answer
    /// past a field added since, so updating the server breaks no open tab.
    #[test]
    fn an_older_page_reads_past_a_field_added_since() {
        let mut answered = serde_json::to_value(MailAccounts {
            accounts: vec![MailAccount {
                address: "ada@example.test".into(),
                ..MailAccount::default()
            }],
            ..MailAccounts::default()
        })
        .unwrap();
        answered["accounts"][0]["addedLater"] = serde_json::json!("anything");
        answered["addedLater"] = serde_json::json!(1);
        let read: MailAccounts = serde_json::from_value(answered).unwrap();
        assert_eq!(read.accounts[0].address, "ada@example.test");

        // No message of a person's accounts refuses what it does not know.
        let source = include_str!("person_accounts.rs");
        assert!(!source.contains(concat!("deny_unknown", "_fields")));
    }

    /// A password never reaches a log: the account read for a person hides
    /// it, and the reading the host answers has no field to carry one.
    #[test]
    fn nothing_read_back_carries_the_sign_in() {
        let adding = AddMailAccount {
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            username: None,
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            password: "hunter2".into(),
            identities: Vec::new(),
            services: Services::default(),
        };
        let printed = format!("{adding:?}");
        assert!(!printed.contains("hunter2"), "{printed}");
        assert!(printed.contains("imap.example.com"), "{printed}");

        let read = MailAccount {
            id: "ada@example.com".into(),
            address: "ada@example.com".into(),
            display_name: "Ada".into(),
            incoming_host: "imap.example.com".into(),
            incoming_port: 993,
            outgoing_host: "smtp.example.com".into(),
            outgoing_port: 465,
            has_secret: true,
            identities: Vec::new(),
            ..Default::default()
        };
        let json = serde_json::to_value(&read).unwrap();
        assert!(json.get("password").is_none());
        assert_eq!(json["hasSecret"], serde_json::json!(true));
    }

    /// An account added without a port signs in over the standard secure
    /// ports; a caller that names none is not refused.
    #[test]
    fn a_mail_account_defaults_to_the_secure_ports() {
        let adding: AddMailAccount = serde_json::from_value(serde_json::json!({
            "address": "ada@example.com",
            "incomingHost": "imap.example.com",
            "outgoingHost": "smtp.example.com",
            "password": "hunter2",
        }))
        .unwrap();
        assert_eq!(adding.incoming_port, 993);
        assert_eq!(adding.outgoing_port, 465);
    }

    /// A read folder says what arrived and which of the reader's messages
    /// its server holds differently; a first read names no window.
    #[test]
    fn a_folder_read_names_its_window_and_answers_what_arrived() {
        let asking: FetchMail = serde_json::from_value(serde_json::json!({
            "account": "ada@example.com",
            "folder": "inbox",
        }))
        .unwrap();
        assert_eq!(asking.known_uid, None);
        assert_eq!(asking.known_uid_validity, None);

        let answered = FetchedMail {
            account: "ada@example.com".into(),
            folder: "inbox".into(),
            uid_validity: 7,
            reset: false,
            has_older: true,
            newest_uid: 42,
            messages: vec![FetchedMessage {
                uid: 42,
                message_id: Some("42@example.com".into()),
                from: MailAddress {
                    name: Some("Ada".into()),
                    email: "ada@example.com".into(),
                },
                subject: "Hello".into(),
                body: "Hi".into(),
                minutes: 100,
                ..FetchedMessage::default()
            }],
            flags: vec![MailFlag {
                uid: 41,
                read: true,
                starred: false,
            }],
        };
        let json = serde_json::to_value(&answered).unwrap();
        assert_eq!(json["newestUid"], serde_json::json!(42));
        assert_eq!(json["messages"][0]["uid"], serde_json::json!(42));
        assert_eq!(json["flags"][0]["read"], serde_json::json!(true));
    }

    /// A message to send names its account and whom it goes to, and its
    /// blind recipients are a field of their own, never folded into a copy
    /// header the server would show the others.
    #[test]
    fn a_message_to_send_carries_its_blind_recipients_apart() {
        let sending: SendMail = serde_json::from_value(serde_json::json!({
            "account": "ada@example.com",
            "fromAddress": "ada@example.com",
            "to": "bob@example.com",
            "bcc": "carl@example.com",
            "subject": "Hello",
            "body": "Hi",
        }))
        .unwrap();
        assert_eq!(sending.cc, "");
        assert_eq!(sending.bcc, "carl@example.com");
        assert!(sending.references.is_empty());
        let printed = format!("{sending:?}");
        assert!(printed.contains("ada@example.com"), "{printed}");
    }

    /// A message begins with the files it carries and the invitation it
    /// brings, and each piece names the file it adds to and where.
    #[test]
    fn a_begun_message_names_its_files_and_their_pieces() {
        let beginning: BeginSend = serde_json::from_value(serde_json::json!({
            "message": {
                "account": "ada@example.com",
                "fromAddress": "ada@example.com",
                "to": "bob@example.com",
                "subject": "Contract",
                "body": "Attached.",
            },
            "calendarIcs": "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n",
            "files": [{
                "filename": "contract.pdf",
                "mime": "application/pdf",
                "size": 1024,
            }],
        }))
        .unwrap();
        assert_eq!(beginning.files[0].filename, "contract.pdf");

        let piece = SendPiece {
            send: "abc".into(),
            file: 0,
            offset: 512,
            bytes: "00ff".into(),
        };
        let printed = serde_json::to_value(&piece).unwrap();
        assert_eq!(printed["offset"], serde_json::json!(512));
        assert_eq!(printed["file"], serde_json::json!(0));
        let kept = PieceKept { written: 514 };
        assert_eq!(
            serde_json::to_value(&kept).unwrap()["written"],
            serde_json::json!(514)
        );
    }
}

/// A new sign-in for an account whose saved one no longer works: the account's id and the
/// password or app password its server took.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplacePassword {
    pub account: String,
    pub password: String,
}

impl std::fmt::Debug for ReplacePassword {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplacePassword")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}
