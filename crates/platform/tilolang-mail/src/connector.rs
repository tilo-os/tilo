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
    pub message_id: String,
    pub references: String,
    pub attachments: Vec<String>,
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
}

/// A message to send.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Outgoing {
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    /// The Message-ID it answers.
    pub in_reply_to: String,
    /// The Message-IDs of the conversation it continues, oldest first, space separated.
    pub references: String,
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
    /// The account's server could not be reached: why.
    Unreachable {
        account: String,
        reason: String,
    },
    /// New mail, or another client's change, arrived in a watched inbox.
    Arrived {
        account: String,
    },
}

/// Why a request was not made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Unmade {
    /// The workspace has no such account on this device.
    NoAccount,
    /// The engine that talks to the servers stopped.
    EngineStopped,
}

/// The connector to the mail servers of the workspace's accounts.
pub trait MailConnector {
    /// The workspace's mail accounts.
    fn accounts(&self) -> Vec<AccountRef>;
    /// Ask for what changed in `mailbox` of `account` since what is held.
    fn changes(&mut self, account: &str, mailbox: &str) -> Option<Request>;
    /// Mark a message read or unread, flagged or not.
    fn flag(&mut self, account: &str, key: &str, unread: bool, flagged: bool) -> Option<Request>;
    /// Move a message to another mailbox.
    fn move_to(&mut self, account: &str, key: &str, mailbox: &str) -> Option<Request>;
    /// Search the account's server.
    fn search(&mut self, account: &str, text: &str) -> Option<Request>;
    /// Send a message from the account.
    ///
    /// # Errors
    /// Why the send was not made: no such account here, or the engine is gone.
    fn send(&mut self, account: &str, message: Outgoing) -> Result<Request, Unmade>;
    /// Keep told of new mail in the account's inbox.
    fn watch(&mut self, account: &str);
    /// What the servers answered since last asked; never waits.
    fn events(&mut self) -> Vec<Event>;
}
