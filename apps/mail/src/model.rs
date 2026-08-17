//! Pure data model for the Mail app.
//!
//! Everything here is IO-free, render-free logic. The widgets read these
//! structs; the CRDT layer (`doc.rs`) persists the *mutable* per-message
//! state (read/unread, starred, current folder, drafts) and the account
//! configuration. The immutable message corpus (headers + bodies +
//! attachments) is hydrated from the server in the live app, or from the
//! bundled `sample.rs` corpus when running headless (`--shots`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use ui::icons::Icon;
use uuid::Uuid;

/// Stable identifier for a single message. In the live app this is derived
/// from the RFC 5322 `Message-ID` header; for synthetic/sample messages we
/// mint a v4 UUID.
pub type MessageId = Uuid;

/// The canonical system folders every account has. We keep them as an enum
/// (rather than free-form strings) so routing — archive/delete/move — is
/// total and exhaustively checked by the compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Folder {
    Inbox,
    Starred,
    Sent,
    Drafts,
    Archive,
    Trash,
    /// Spam / junk.
    Junk,
}

impl Folder {
    /// Display order in the sidebar.
    pub const ALL: [Folder; 7] = [
        Folder::Inbox,
        Folder::Starred,
        Folder::Sent,
        Folder::Drafts,
        Folder::Archive,
        Folder::Junk,
        Folder::Trash,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Folder::Inbox => "Inbox",
            Folder::Starred => "Starred",
            Folder::Sent => "Sent",
            Folder::Drafts => "Drafts",
            Folder::Archive => "Archive",
            Folder::Trash => "Trash",
            Folder::Junk => "Junk",
        }
    }

    /// The themed vector [`Icon`] shown to the left of the folder name. These
    /// come from the shared `ui` lucide set so they render as crisp strokes at
    /// any scale — never a font-dependent emoji that falls back to tofu.
    pub fn icon(self) -> Icon {
        match self {
            Folder::Inbox => Icon::Inbox,
            Folder::Starred => Icon::Star,
            Folder::Sent => Icon::Send,
            Folder::Drafts => Icon::Pencil,
            Folder::Archive => Icon::Archive,
            Folder::Trash => Icon::Trash,
            Folder::Junk => Icon::Ban,
        }
    }

    /// The stable string key this folder is stored under in the CRDT (and the
    /// key a [`crate::actions::MailAction::Move`] carries). The inverse of
    /// [`Folder::from_key`]. Kept here (rather than in `doc`) so the action layer
    /// and the persistence layer share one canonical spelling.
    pub fn as_key(self) -> &'static str {
        match self {
            Folder::Inbox => "inbox",
            Folder::Starred => "starred",
            Folder::Sent => "sent",
            Folder::Drafts => "drafts",
            Folder::Archive => "archive",
            Folder::Trash => "trash",
            Folder::Junk => "junk",
        }
    }

    /// Parse a folder key produced by [`Folder::as_key`]. Returns `None` for an
    /// unrecognised key (e.g. a user-defined local folder id), so a caller can
    /// treat it as a no-op rather than substituting a wrong folder.
    pub fn from_key(s: &str) -> Option<Folder> {
        Some(match s {
            "inbox" => Folder::Inbox,
            "starred" => Folder::Starred,
            "sent" => Folder::Sent,
            "drafts" => Folder::Drafts,
            "archive" => Folder::Archive,
            "trash" => Folder::Trash,
            "junk" => Folder::Junk,
            _ => return None,
        })
    }

    /// IMAP mailbox name this folder maps to on a typical server. Used by
    /// the transport layer when issuing `SELECT`.
    pub fn imap_name(self) -> &'static str {
        match self {
            Folder::Inbox => "INBOX",
            Folder::Starred => "INBOX", // virtual: filtered by \Flagged
            Folder::Sent => "Sent",
            Folder::Drafts => "Drafts",
            Folder::Archive => "Archive",
            Folder::Trash => "Trash",
            Folder::Junk => "Junk",
        }
    }
}

/// A single email address with an optional display name.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub name: Option<String>,
    pub email: String,
}

impl Addr {
    pub fn new(name: impl Into<String>, email: impl Into<String>) -> Self {
        let name = name.into();
        Self {
            name: if name.is_empty() { None } else { Some(name) },
            email: email.into(),
        }
    }

    pub fn bare(email: impl Into<String>) -> Self {
        Self { name: None, email: email.into() }
    }

    /// Best display string: the name if present, otherwise the local-part
    /// of the address, otherwise the full address.
    pub fn display(&self) -> String {
        if let Some(n) = &self.name {
            if !n.is_empty() {
                return n.clone();
            }
        }
        self.email.clone()
    }

    /// Initials for the avatar disc (1-2 chars).
    pub fn initials(&self) -> String {
        let base = self.name.clone().unwrap_or_else(|| self.email.clone());
        let mut chars = base
            .split(|c: char| c.is_whitespace() || c == '.' || c == '@' || c == '_')
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.chars().next());
        let a = chars.next().unwrap_or('?');
        match chars.next() {
            Some(b) => format!("{}{}", a.to_uppercase(), b.to_uppercase()),
            None => a.to_uppercase().to_string(),
        }
    }

    /// RFC 5322 mailbox rendering: `Name <email>` or bare `email`.
    pub fn rfc5322(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => format!("{} <{}>", n, self.email),
            _ => self.email.clone(),
        }
    }
}

/// A decoded attachment. When the bytes are present locally (`hydrated`) they
/// are held in `content`, so the reader can save the real file to disk; in the
/// live app the bytes are hydrated on demand (same projection-vs-hydration
/// story as the qOS FUSE layer) and `content` is empty until then.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub mime: String,
    pub size: u64,
    /// Whether the bytes are present locally (vs. server-side only).
    pub hydrated: bool,
    /// The decoded bytes, when hydrated. Never serialised into the shared
    /// settings store (it holds message content, not settings).
    #[serde(skip, default)]
    pub content: Vec<u8>,
}

impl Attachment {
    /// A metadata-only attachment (no bytes yet — projection, not hydrated).
    pub fn projected(filename: impl Into<String>, mime: impl Into<String>, size: u64) -> Self {
        Self { filename: filename.into(), mime: mime.into(), size, hydrated: false, content: Vec::new() }
    }

    /// A fully-hydrated attachment carrying its decoded bytes.
    pub fn hydrated(filename: impl Into<String>, mime: impl Into<String>, content: Vec<u8>) -> Self {
        Self { filename: filename.into(), mime: mime.into(), size: content.len() as u64, hydrated: true, content }
    }

    /// Whether the bytes are present locally and this attachment can be saved.
    pub fn is_downloadable(&self) -> bool {
        self.hydrated && !self.content.is_empty()
    }

    /// Human-readable size, e.g. `14 KiB`, `1.2 MiB`.
    pub fn size_human(&self) -> String {
        ui::format::bytes(self.size)
    }

    /// Coarse category used to pick a themed vector [`Icon`] for the chip.
    pub fn icon(&self) -> Icon {
        let m = self.mime.to_ascii_lowercase();
        if m.starts_with("image/") {
            Icon::Image
        } else if m.contains("pdf") {
            Icon::FileText
        } else if m.starts_with("text/") {
            Icon::FileText
        } else if m.contains("zip") || m.contains("compressed") || m.contains("tar") || m.contains("gzip") {
            Icon::Archive
        } else if m.contains("calendar") {
            Icon::Calendar
        } else {
            Icon::Paperclip
        }
    }
}

/// A single message. The immutable corpus fields (from/to/subject/body/…)
/// come from the server or sample corpus; the mutable flags
/// (read/starred/folder) are mirrored into the CRDT so they sync and undo.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    /// RFC 5322 `Message-ID` (without angle brackets), if known.
    pub rfc_message_id: Option<String>,
    /// The `In-Reply-To` / `References` anchor used for threading.
    pub in_reply_to: Option<String>,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    /// Plain-text body (the parsed text/plain part, or an HTML-stripped
    /// fallback).
    pub body: String,
    /// The original `text/html` part, when the message carried one. Rendered
    /// natively as sanitized rich text + images (remote content blocked by
    /// default); `None` for plain-text-only mail, which renders as text. Never
    /// serialised into the shared settings store — it is message content.
    #[serde(default)]
    pub html_body: Option<String>,
    pub attachments: Vec<Attachment>,
    /// Minutes since the Unix epoch — coarse, but enough for relative
    /// "2h ago" style rendering without pulling `chrono`.
    pub timestamp_min: i64,
    // ---- mutable flags (mirrored to the CRDT) ----
    pub read: bool,
    pub starred: bool,
    pub folder: Folder,
    /// User-applied labels/tags, mirrored from the CRDT (`flags/<id>/labels`).
    /// These are set exclusively through [`crate::actions::MailAction::AddLabel`]
    /// / [`crate::actions::MailAction::RemoveLabel`] and overlaid onto the
    /// in-memory message by [`crate::doc::MailDoc::apply_flags`]; empty for a
    /// message that has never been tagged.
    #[serde(default)]
    pub labels: Vec<String>,
}

impl Message {
    /// Normalise a subject for threading: strip leading `Re:` / `Fwd:`
    /// chains and collapse whitespace.
    pub fn thread_subject(&self) -> String {
        normalize_subject(&self.subject)
    }

    /// First non-empty line of the body, used as the list snippet.
    pub fn snippet(&self) -> String {
        let s = self
            .body
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        let cleaned: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        cleaned
    }

    pub fn has_attachments(&self) -> bool {
        !self.attachments.is_empty()
    }
}

/// Strip `Re:` / `RE:` / `Fwd:` / `FW:` prefixes (possibly repeated) and
/// trim, so replies thread under the original subject.
pub fn normalize_subject(subject: &str) -> String {
    let mut s = subject.trim();
    loop {
        let lower = s.to_ascii_lowercase();
        let trimmed = if let Some(rest) = lower.strip_prefix("re:") {
            s.len() - rest.trim_start().len()
        } else if let Some(rest) = lower.strip_prefix("fwd:") {
            s.len() - rest.trim_start().len()
        } else if let Some(rest) = lower.strip_prefix("fw:") {
            s.len() - rest.trim_start().len()
        } else {
            0
        };
        if trimmed == 0 {
            break;
        }
        s = s[trimmed..].trim_start();
    }
    s.to_string()
}

/// A conversation: 1+ messages sharing a normalized subject, ordered oldest
/// → newest. The list pane renders one row per thread.
#[derive(Clone, Debug)]
pub struct Thread {
    pub subject: String,
    pub message_ids: Vec<MessageId>,
}

/// How an account authenticates to its IMAP/SMTP servers. Password is the
/// classic user+secret; OAuth is the modern token flow the big providers now
/// **require** (Gmail and Outlook block plain-password IMAP), so those
/// providers preset to [`AuthMethod::OAuth`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AuthMethod {
    /// Username + password (the field the setup form collects).
    #[default]
    Password,
    /// OAuth2 authorization-code + PKCE; the account holds a bearer token
    /// instead of a password (see [`crate::oauth`]).
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

/// Account-level connection settings. This is exactly the form the
/// account-setup screen collects and the transport layer consumes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountConfig {
    pub display_name: String,
    pub email: String,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_tls: bool,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    /// How this account signs in. Providers that block password IMAP
    /// (Gmail/Outlook) preset to [`AuthMethod::OAuth`].
    #[serde(default)]
    pub auth: AuthMethod,
    /// Stored only in memory in this slice; a real deployment pushes this to
    /// the OS secret store. Never written to the shared settings JSON (the
    /// Settings app injects it at connect time) — hence `serde(skip)`.
    #[serde(skip)]
    pub password: String,
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
            password: String::new(),
        }
    }
}

impl AccountConfig {
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
                // Generic guess: imap./smtp. + domain.
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
        // Office365 / Gmail prefer 587 STARTTLS; both also accept 465.
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

/// Which representation the compose body is authored in. Persisted so the
/// user's choice is remembered across compose windows and sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComposeFormat {
    /// WYSIWYG rich text, serialised to an HTML MIME part on send.
    Html,
    /// Plain text, serialised to a `text/plain` MIME part on send.
    Text,
}

impl Default for ComposeFormat {
    fn default() -> Self {
        ComposeFormat::Html
    }
}

impl ComposeFormat {
    pub fn as_key(self) -> &'static str {
        match self {
            ComposeFormat::Html => "html",
            ComposeFormat::Text => "text",
        }
    }

    pub fn from_key(s: &str) -> Self {
        match s {
            "text" => ComposeFormat::Text,
            _ => ComposeFormat::Html,
        }
    }
}

/// A draft message being composed. Persisted to the CRDT so it survives a
/// crash and syncs across peers.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    /// Plain-text body (always populated; the canonical text alternative).
    pub body: String,
    /// HTML body, set when the draft was authored in WYSIWYG mode. Sent as the
    /// `text/html` part alongside `body` as the `text/plain` alternative.
    #[serde(default)]
    pub html_body: Option<String>,
    /// Filesystem paths of files staged as attachments for this draft. The
    /// bytes are read from these paths when the message is built (see
    /// `transport::build_mime`), so what is sent is the real file — no
    /// fabricated placeholder payload.
    pub attachments: Vec<String>,
    /// If this draft is a reply, the message it answers.
    pub in_reply_to: Option<MessageId>,
}

/// The display name (final path component) of a staged attachment path.
pub fn attachment_display_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// The whole mailbox aggregate: an account, its messages, and the derived
/// folder/thread structure. Pure logic — the widgets ask it questions.
#[derive(Clone, Debug, Default)]
pub struct Mailbox {
    pub account: AccountConfig,
    pub messages: HashMap<MessageId, Message>,
    /// Insertion order is irrelevant; folder views sort by timestamp.
    pub order: Vec<MessageId>,
}

impl Mailbox {
    pub fn new(account: AccountConfig) -> Self {
        Self { account, messages: HashMap::new(), order: Vec::new() }
    }

    pub fn insert(&mut self, msg: Message) {
        if !self.messages.contains_key(&msg.id) {
            self.order.push(msg.id);
        }
        self.messages.insert(msg.id, msg);
    }

    pub fn get(&self, id: MessageId) -> Option<&Message> {
        self.messages.get(&id)
    }

    pub fn get_mut(&mut self, id: MessageId) -> Option<&mut Message> {
        self.messages.get_mut(&id)
    }

    /// Number of unread messages routed to `folder` (Starred is virtual).
    pub fn unread_count(&self, folder: Folder) -> usize {
        self.messages_in(folder).iter().filter(|m| !m.read).count()
    }

    /// Total messages visible in `folder`.
    pub fn count(&self, folder: Folder) -> usize {
        self.messages_in(folder).len()
    }

    /// All messages routed into `folder`, newest first. `Starred` is a
    /// virtual folder: any starred message not in Trash.
    pub fn messages_in(&self, folder: Folder) -> Vec<&Message> {
        let mut v: Vec<&Message> = self
            .order
            .iter()
            .filter_map(|id| self.messages.get(id))
            .filter(|m| match folder {
                Folder::Starred => m.starred && m.folder != Folder::Trash,
                f => m.folder == f,
            })
            .collect();
        v.sort_by(|a, b| b.timestamp_min.cmp(&a.timestamp_min));
        v
    }

    /// Group the messages of `folder` into threads, each thread ordered
    /// oldest→newest, and the thread list ordered by most-recent activity.
    pub fn threads_in(&self, folder: Folder) -> Vec<Thread> {
        let mut by_subject: HashMap<String, Vec<MessageId>> = HashMap::new();
        for m in self.messages_in(folder) {
            by_subject
                .entry(m.thread_subject())
                .or_default()
                .push(m.id);
        }
        let mut threads: Vec<Thread> = by_subject
            .into_iter()
            .map(|(subject, mut ids)| {
                ids.sort_by_key(|id| self.messages[id].timestamp_min);
                Thread { subject, message_ids: ids }
            })
            .collect();
        // Order threads by their newest message, descending.
        threads.sort_by(|a, b| {
            let an = a
                .message_ids
                .iter()
                .map(|id| self.messages[id].timestamp_min)
                .max()
                .unwrap_or(0);
            let bn = b
                .message_ids
                .iter()
                .map(|id| self.messages[id].timestamp_min)
                .max()
                .unwrap_or(0);
            bn.cmp(&an)
        });
        threads
    }

    /// Case-insensitive full-text search across subject/sender/body, scoped
    /// to a folder or (when `None`) the whole mailbox excluding Trash.
    pub fn search(&self, query: &str, scope: Option<Folder>) -> Vec<&Message> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut v: Vec<&Message> = self
            .order
            .iter()
            .filter_map(|id| self.messages.get(id))
            .filter(|m| match scope {
                Some(f) => m.folder == f,
                None => m.folder != Folder::Trash,
            })
            .filter(|m| {
                m.subject.to_ascii_lowercase().contains(&q)
                    || m.from.display().to_ascii_lowercase().contains(&q)
                    || m.from.email.to_ascii_lowercase().contains(&q)
                    || m.body.to_ascii_lowercase().contains(&q)
            })
            .collect();
        v.sort_by(|a, b| b.timestamp_min.cmp(&a.timestamp_min));
        v
    }

    // ---- mutating actions (the reader/list issue these) ----

    pub fn mark_read(&mut self, id: MessageId, read: bool) {
        if let Some(m) = self.messages.get_mut(&id) {
            m.read = read;
        }
    }

    pub fn toggle_star(&mut self, id: MessageId) -> bool {
        if let Some(m) = self.messages.get_mut(&id) {
            m.starred = !m.starred;
            m.starred
        } else {
            false
        }
    }

    pub fn move_to(&mut self, id: MessageId, folder: Folder) {
        if let Some(m) = self.messages.get_mut(&id) {
            m.folder = folder;
        }
    }

    pub fn archive(&mut self, id: MessageId) {
        self.move_to(id, Folder::Archive);
    }

    /// Move to Trash, or permanently drop if already in Trash.
    pub fn delete(&mut self, id: MessageId) {
        let in_trash = self.messages.get(&id).map(|m| m.folder == Folder::Trash);
        match in_trash {
            Some(true) => {
                self.messages.remove(&id);
                self.order.retain(|x| *x != id);
            }
            Some(false) => self.move_to(id, Folder::Trash),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(subject: &str, ts: i64, folder: Folder) -> Message {
        Message {
            id: Uuid::new_v4(),
            rfc_message_id: None,
            in_reply_to: None,
            from: Addr::new("Ada Lovelace", "ada@analytical.example"),
            to: vec![Addr::bare("you@qos.example")],
            cc: vec![],
            subject: subject.into(),
            body: "First line.\nSecond line.".into(),
            html_body: None,
            attachments: vec![],
            timestamp_min: ts,
            read: false,
            starred: false,
            folder,
            labels: Vec::new(),
        }
    }

    #[test]
    fn normalize_strips_reply_prefixes() {
        assert_eq!(normalize_subject("Re: Re: Hello"), "Hello");
        assert_eq!(normalize_subject("FWD: Fw: Plan"), "Plan");
        assert_eq!(normalize_subject("Plain"), "Plain");
    }

    #[test]
    fn threads_group_by_normalized_subject() {
        let mut mb = Mailbox::default();
        mb.insert(msg("Launch plan", 100, Folder::Inbox));
        mb.insert(msg("Re: Launch plan", 200, Folder::Inbox));
        mb.insert(msg("Lunch?", 150, Folder::Inbox));
        let threads = mb.threads_in(Folder::Inbox);
        assert_eq!(threads.len(), 2);
        // Most-recent-first: "Launch plan" (200) before "Lunch?" (150).
        assert_eq!(threads[0].subject, "Launch plan");
        assert_eq!(threads[0].message_ids.len(), 2);
        assert_eq!(threads[1].subject, "Lunch?");
    }

    #[test]
    fn unread_count_and_mark_read() {
        let mut mb = Mailbox::default();
        let m = msg("Hi", 10, Folder::Inbox);
        let id = m.id;
        mb.insert(m);
        assert_eq!(mb.unread_count(Folder::Inbox), 1);
        mb.mark_read(id, true);
        assert_eq!(mb.unread_count(Folder::Inbox), 0);
    }

    #[test]
    fn star_makes_message_appear_in_virtual_folder() {
        let mut mb = Mailbox::default();
        let m = msg("Hi", 10, Folder::Inbox);
        let id = m.id;
        mb.insert(m);
        assert_eq!(mb.count(Folder::Starred), 0);
        assert!(mb.toggle_star(id));
        assert_eq!(mb.count(Folder::Starred), 1);
    }

    #[test]
    fn delete_moves_to_trash_then_purges() {
        let mut mb = Mailbox::default();
        let m = msg("Hi", 10, Folder::Inbox);
        let id = m.id;
        mb.insert(m);
        mb.delete(id);
        assert_eq!(mb.get(id).unwrap().folder, Folder::Trash);
        mb.delete(id);
        assert!(mb.get(id).is_none());
    }

    #[test]
    fn search_matches_subject_and_body_case_insensitively() {
        let mut mb = Mailbox::default();
        mb.insert(msg("Launch Plan", 10, Folder::Inbox));
        let hits = mb.search("launch", None);
        assert_eq!(hits.len(), 1);
        let body_hits = mb.search("second line", None);
        assert_eq!(body_hits.len(), 1);
        assert!(mb.search("nothing", None).is_empty());
    }

    #[test]
    fn autofill_recognises_gmail() {
        let mut cfg = AccountConfig { email: "person@gmail.com".into(), ..Default::default() };
        assert!(cfg.autofill_from_email());
        assert_eq!(cfg.imap_host, "imap.gmail.com");
        assert_eq!(cfg.smtp_host, "smtp.gmail.com");
        assert!(cfg.is_valid());
    }

    #[test]
    fn validate_flags_missing_fields() {
        let cfg = AccountConfig::default();
        assert!(!cfg.is_valid());
        let keys: Vec<_> = cfg.validate().into_iter().map(|(k, _)| k).collect();
        assert!(keys.contains(&"email"));
        assert!(keys.contains(&"imap_host"));
    }
}
