//! What a session with a mail server is asked and answers: a folder's page of messages, one
//! message, what a search found, what changed about a message, and the message that leaves.
//!
//! A message whose words would not fit one answer arrives by its start and says so
//! ([`FetchedMessage::partial`]).

use serde::{Deserialize, Serialize};

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

/// One message as a server holds it.
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
    /// the whole would not fit the answer it came in. [`ReadMail`] answers
    /// the message whole.
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
    /// The account's id.
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
    /// The account's id.
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
    /// The account's id.
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

/// A message to send through one of the person's accounts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMail {
    /// The account's id.
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

/// What a send answers once the message left.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SentMail {
    /// Why the account's Sent folder holds no copy of the message, when it
    /// was tried and could not be filed. The message left all the same.
    /// Absent when the copy is filed, the folder already held it, or the
    /// account's server files what it sends by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unfiled: Option<String>,
}

/// What one message's server is told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarkMail {
    /// The account's id.
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
