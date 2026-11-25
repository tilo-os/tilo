//! The IMAP and SMTP connector as the mail origin speaks to it: what an account's server can
//! do, in the account's terms: the changes of a mailbox since what is held, a flag or a move of
//! a message, a search, a send. It names accounts by the id the workspace's settings give them
//! and messages by their key; servers, places on the server and credentials stay behind it,
//! in native code ([`crate::engine`]), reached in-process or over a socket ([`crate::remote`]).

/// A mail account of the workspace's settings, as the connector shows it: never its servers or
/// credentials.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountRef {
    /// The id the settings give it.
    pub id: String,
    pub address: String,
    /// The name it sends as.
    pub name: String,
    /// The other names and addresses it sends as, beside its own, each `Name <address>`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identities: Vec<String>,
}

/// A file a message carries, or one a message being sent takes along.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Part {
    /// What it is called: `Lease 2026.pdf`.
    pub name: String,
    /// Its media type: `application/pdf`.
    pub media: String,
    /// How many bytes it is.
    pub size: u64,
    /// The reference its bytes are kept under in this device's content store, the hash a
    /// `tilo-blob://` address names: empty while they are not on this device.
    pub content: String,
    /// The Content-ID its message's HTML shows it in place by (`cid:`): empty for a file attached
    /// to the message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub inline_id: String,
}

/// A message as its server holds it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImapMessage {
    /// Its Message-ID, or an id derived from its content.
    pub key: String,
    /// The conversation it belongs to: the key of the conversation's first message.
    pub thread: String,
    pub subject: String,
    pub from: String,
    pub from_name: String,
    pub to: String,
    pub cc: String,
    /// When it arrived, in seconds since the epoch.
    pub received: u64,
    pub body: String,
    /// Its `text/html` part as it came, when it has one: empty for a plain-text message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html: String,
    pub message_id: String,
    pub references: String,
    /// The files it carries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<Part>,
    pub unread: bool,
    pub flagged: bool,
    /// The key of its mailbox (`inbox`, `archive`, ...).
    pub mailbox: String,
}

/// What a mailbox's server said changed.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Changes {
    pub account: String,
    pub mailbox: String,
    /// Messages new to what is held.
    pub messages: Vec<ImapMessage>,
    /// `(key, unread, flagged)` of the messages held that the server still has.
    pub flags: Vec<(String, bool, bool)>,
    /// The keys of the messages held that the server no longer has: deleted, or moved away by
    /// another client.
    #[serde(default)]
    pub removed: Vec<String>,
    /// Whether the server holds messages older than the oldest one held: `None` when the answer
    /// says nothing of them.
    #[serde(default)]
    pub more: Option<bool>,
    /// The folder starting over: what it holds replaces what was held of it, so its messages are
    /// not news, and a person is not told of them.
    #[serde(default)]
    pub reset: bool,
}

/// A message to send.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outgoing {
    /// The name and address it goes from, `Name <address>`: one of the account's identities, or
    /// empty for the account's own.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub from: String,
    pub to: String,
    pub cc: String,
    /// Blind recipients: in the delivery envelope, never in a header of the message.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub bcc: String,
    pub subject: String,
    /// What it says, as it was written.
    pub body: String,
    /// What it says as HTML, the formatted alternative a mail program shows beside the words:
    /// empty when it says nothing more than `body`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub html_body: String,
    /// The Message-ID it answers.
    pub in_reply_to: String,
    /// The Message-IDs of the conversation it continues, oldest first, space separated.
    pub references: String,
    /// The calendar invitation it carries (iCalendar), sent as its calendar part so the guest's
    /// calendar answers it: empty for none.
    #[serde(default)]
    pub calendar_ics: String,
    /// The files it takes along, each kept in this device's content store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Part>,
}

/// A request handed to a server.
pub type Request = uuid::Uuid;

/// What a server answered.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    Changes(Changes),
    /// What a search found, beside what is held.
    Found {
        account: String,
        messages: Vec<ImapMessage>,
    },
    /// The server took the request (a send, a flag, a move).
    Accepted(Request),
    /// The server refused it: why.
    Refused {
        request: Request,
        reason: String,
    },
    /// The server could not be reached, so it never got the request: why.
    Unsent {
        request: Request,
        reason: String,
    },
    /// The account's server could not be reached: why, and of what kind the trouble is, so the
    /// window can say what helps: `signed-out`, `no-password`, `withheld`, `offline`, or
    /// `unreachable` for anything else.
    Unreachable {
        account: String,
        reason: String,
        kind: String,
    },
    /// New mail, or another client's change, arrived in a watched inbox.
    Arrived {
        account: String,
    },
    /// The server took a send and kept no copy in its account's Sent folder: why. It comes
    /// before the [`Self::Accepted`] that says the message went.
    Unfiled {
        request: Request,
        reason: String,
    },
}

/// Why a request was not made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unmade {
    /// The workspace has no such account on this device.
    NoAccount,
    /// The engine that talks to the servers stopped.
    EngineStopped,
    /// A file it takes along is not on this device: which, in words.
    Missing(String),
}

/// The connector to the mail servers of the workspace's accounts.
pub trait MailConnector {
    /// The workspace's mail accounts.
    fn accounts(&self) -> Vec<AccountRef>;
    /// Ask for what changed in `mailbox` of `account` since what is held.
    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request>;
    /// Ask for the messages older than the oldest one held in `mailbox` of `account`: the page
    /// before what is held.
    fn older(&mut self, account: &str, mailbox: &str) -> Option<Request>;
    /// Mark a message read or unread, flagged or not.
    fn flag(&mut self, account: &str, key: &str, unread: bool, flagged: bool) -> Option<Request>;
    /// Move a message to another mailbox.
    fn move_to(&mut self, account: &str, key: &str, mailbox: &str) -> Option<Request>;
    /// Delete a message from its server for good: its trash emptied of it.
    fn purge(&mut self, account: &str, key: &str) -> Option<Request>;
    /// Keep `draft` in the account's server Drafts folder, as another mail client would see it.
    fn save_draft(&mut self, account: &str, draft: Outgoing) -> Option<Request>;
    /// Search the account's server.
    fn search(&mut self, account: &str, text: &str) -> Option<Request>;
    /// Send a message from the account.
    fn send(&mut self, account: &str, message: Outgoing) -> Result<Request, Unmade>;
    /// Keep told of new mail in the account's inbox.
    fn watch(&mut self, account: &str);
    /// What the servers answered since last asked; never waits.
    fn events(&mut self) -> Vec<Event>;
}
