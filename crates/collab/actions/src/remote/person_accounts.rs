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
//! the secret its vault keeps, so nothing in this vocabulary carries one.
//!
//! The same actions answer a device that keeps its own accounts, so the page
//! and the device read and change accounts through one vocabulary.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the host answers the person's mail accounts under.
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
/// Remove a mail account and its secret (`account`), answering the accounts
/// that remain as a [`MailAccounts`].
pub const REMOVE: &str = "remove";
/// Fetch one folder of one account ([`FetchMail`]), answering what the
/// server holds as a [`FetchedMail`].
pub const FETCH: &str = "fetch";
/// Begin a message to send ([`BeginSend`]), answering the id [`SEND`] sends
/// it by ([`StartedSend`]).
pub const BEGIN_SEND: &str = "begin-send";
/// One piece of a begun message's file ([`SendPiece`]), answering the bytes
/// the host holds of that file ([`PieceKept`]).
pub const SEND_PIECE: &str = "send-piece";
/// Send the message begun under an id ([`SendId`]), answering nothing: the
/// message left, or the reason it did not.
pub const SEND: &str = "send";
/// Tell one account's server what changed about a message ([`MarkMail`]):
/// read, starred, moved or removed.
pub const MARK: &str = "mark";

/// The target of action `action` of [`RESOURCE`].
#[must_use]
pub fn target(action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, action),
    }
}

/// One mail account, as [`LIST`] and [`ADD_MAIL`] answer it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}

/// Every mail account the person's vault keeps.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailAccounts {
    pub accounts: Vec<MailAccount>,
    /// The account the person chose last, when the store names one; the
    /// first account otherwise.
    #[serde(default)]
    pub active: String,
}

/// A mail account to add. The password travels here and nowhere back: the
/// host keeps it in the person's vault and answers only [`MailAccount`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddMailAccount {
    pub address: String,
    #[serde(default)]
    pub display_name: String,
    /// The sign-in name, when it is not the address itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub incoming_host: String,
    #[serde(default = "default_imaps_port")]
    pub incoming_port: u16,
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
            .finish_non_exhaustive()
    }
}

const fn default_imaps_port() -> u16 {
    993
}

const fn default_submissions_port() -> u16 {
    465
}

/// One address as mail carries it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailAddress {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub email: String,
}

/// A file a message carries beside its words.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}

/// What the reader did to a message the server no longer reports as such.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MailFlag {
    pub uid: u32,
    pub read: bool,
    pub starred: bool,
}

/// Ask one folder of one account, by its key (`inbox`, `sent`, …).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
}

/// A folder as its server answered it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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

/// A message to send through one of the person's accounts. The host writes
/// it as mail and hands it to the server; nothing of it comes back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendFile {
    pub filename: String,
    pub mime: String,
    pub size: u64,
}

/// What [`BEGIN_SEND`] answers: the id [`SEND`] and [`SEND_PIECE`] name the
/// message by.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartedSend {
    pub send: String,
}

/// One piece of one of a begun message's files: its bytes at `offset`,
/// hex-encoded so the frame stays self-describing text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
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
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PieceKept {
    pub written: u64,
}

/// The message [`SEND`] sends.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SendId {
    pub send: String,
}

/// What one message's server is told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkMail {
    /// The account's id, as [`LIST`] names it.
    pub account: String,
    /// The folder key the message was read in.
    pub folder: String,
    pub uid: u32,
    pub uid_validity: u32,
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
        };
        let json = serde_json::to_value(&read).unwrap();
        assert!(json.get("password").is_none());
        assert_eq!(json["hasSecret"], serde_json::json!(true));
    }

    /// A host names its actions without having read a catalogue, as it names
    /// its billing account, and the name is the provider's resource.
    #[test]
    fn the_actions_are_addressed_without_a_catalogue() {
        assert_eq!(target(ADD_MAIL).provider, PROVIDER);
        assert_eq!(target(ADD_MAIL).address.resource, RESOURCE);
        assert_eq!(target(ADD_MAIL).address.action, ADD_MAIL);
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
