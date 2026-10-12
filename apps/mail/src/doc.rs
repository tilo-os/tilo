//! Automerge document model for the Mail app.
//!
//! State that must survive a crash, sync between peers, and participate in
//! undo lives here — not in widget fields. Concretely:
//!
//! ```text
//! root
//! ├── "flags"   (Map)  per-message mutable state, keyed by message-id string
//! │     └── <id> (Map) { read: bool, starred: bool, folder: Str, labels: List }
//! │           └── "server" (Map) { read, starred, folder } as the server has them
//! ├── "messages" (Map)  externally-ingested ids
//! │     └── <id> (Map) { present, purged, account, locator, server_removed }
//! ├── "local"   (Map)  messages held whole: delivered here or drafted
//! │     └── <id> (Str) the message as JSON (`crate::local::LocalMessage`)
//! └── "draft"   (Map)  the message currently being composed
//!       ├── to, cc, bcc, subject, body, html_body, … (Str) as last saved
//!       └── "rich_body" (Map) the body being written: qdoc rich text
//! ```
//!
//! A message fetched from a mail server also records where it lives there
//! (`account`, the `Message-ID` `locator`) and the flags and folder the server
//! last reported or confirmed. Where the document and the server differ, the
//! server has not been told yet: every device with the account brings the
//! server up to date ([`MailDoc::server_updates`]), whoever made the change —
//! a click, the palette, an agent, a collaborator or an undo.
//!
//! The immutable message corpus (headers/bodies/attachments) is *not* stored
//! here — it's hydrated from the server (or the sample corpus) into the
//! in-memory [`Mailbox`]. The exception is mail that has no server: a message
//! delivered into the mailbox without one (a seeded scenario, an import) and a
//! draft written through the mailbox's actions are held whole under `local`. Navigation, search, selection, scroll position and
//! editor formatting are deliberately device-local presentation state. Only
//! domain mutations (flags/folders/labels/drafts) ride in the CRDT.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_str};

use tilo_document::RichText;
use ui::widget::{BlockContent, BlockKind};

use crate::actions::{DraftBodySeed, IngestBaseline, Outgoing, ServerUpdate};
use crate::attachment_store;
use crate::model::{Draft, DraftAttachment, Folder, Mailbox};

/// The draft's key holding the body being written.
const RICH_BODY: &str = "rich_body";

/// A message's flag entry key holding what its server has.
const SERVER: &str = "server";
const OUTBOX: &str = "outbox";
/// Messages the document holds whole (`crate::local`).
const LOCAL: &str = "local";

pub struct MailDoc;

/// Per-message collaborative flags projected from the document for publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MessageFlags {
    pub id: String,
    pub folder: Folder,
    pub read: bool,
    pub starred: bool,
    pub labels: Vec<String>,
}

impl MailDoc {
    // ---- generic scalar helpers ----

    /// # Panics
    /// Never in practice: `parent` is either `ROOT` (permanent for the life of
    /// the document) or a Map this module itself just resolved, so
    /// `put_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT or a just-resolved Map, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write this key into the wrong object"
    )]
    fn map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object map never fails"),
        }
    }

    // ---- per-message flags ----

    fn flags_map(doc: &mut AutoCommit) -> ObjId {
        Self::map(doc, &ROOT, "flags")
    }

    /// Register a message and record what its source reports. Fields the
    /// document has no value for yet take the reported one; existing values
    /// win, so a delayed fetch cannot overwrite a `MailAction`. A message
    /// fetched from a server (`account` and `locator`) also records the
    /// server's flags, and a flag the server changed since it last reported
    /// it — another mail client read the message — is adopted.
    pub fn ingest_baseline(doc: &mut AutoCommit, baseline: &IngestBaseline) {
        let id = baseline.id.as_str();
        if id.is_empty() {
            return;
        }
        Self::register_message(doc, id);
        let folder = baseline.folder.as_deref().and_then(Folder::from_key);
        let (Some(account), Some(locator)) = (&baseline.account, &baseline.locator) else {
            if let Some(read) = baseline.read
                && Self::get_read(doc, id).is_none()
            {
                Self::set_read(doc, id, read);
            }
            if let Some(starred) = baseline.starred
                && Self::get_starred(doc, id).is_none()
            {
                Self::set_starred(doc, id, starred);
            }
            if let Some(folder) = folder
                && Self::get_folder_for(doc, id).is_none()
            {
                Self::set_folder_for(doc, id, folder);
            }
            return;
        };
        let messages = Self::messages_map(doc);
        let entry = Self::map(doc, &messages, id);
        ui::doc::put_if_changed(doc, &entry, "account", account.as_str());
        ui::doc::put_if_changed(doc, &entry, "locator", locator.as_str());
        let flags = Self::flag_entry(doc, id);
        let server = Self::map(doc, &flags, SERVER);
        for (key, reported) in [("read", baseline.read), ("starred", baseline.starred)] {
            let Some(reported) = reported else { continue };
            let known = read_bool(doc, &server, key);
            if known == Some(reported) {
                continue;
            }
            ui::doc::put_if_changed(doc, &server, key, reported);
            if known.is_some() || read_bool(doc, &flags, key).is_none() {
                ui::doc::put_if_changed(doc, &flags, key, reported);
            }
        }
        if let Some(reported) = folder {
            let known = read_str(doc, &server, "folder").and_then(|key| folder_from_key(&key));
            if known != Some(reported) {
                ui::doc::put_if_changed(doc, &server, "folder", folder_key(reported));
                if known.is_some() || Self::get_folder_for(doc, id).is_none() {
                    Self::set_folder_for(doc, id, reported);
                }
            }
        }
    }

    /// Record that the server now holds what `update` asked of it.
    pub fn server_applied(doc: &mut AutoCommit, update: &ServerUpdate) {
        let id = update.id();
        if Self::server_entry(doc, id).is_none() {
            return;
        }
        match update {
            ServerUpdate::Flags { read, starred, .. } => {
                let flags = Self::flag_entry(doc, id);
                let server = Self::map(doc, &flags, SERVER);
                ui::doc::put_if_changed(doc, &server, "read", *read);
                ui::doc::put_if_changed(doc, &server, "starred", *starred);
            }
            ServerUpdate::Move { to, .. } => {
                let flags = Self::flag_entry(doc, id);
                let server = Self::map(doc, &flags, SERVER);
                ui::doc::put_if_changed(doc, &server, "folder", folder_key(*to));
            }
            ServerUpdate::Remove { .. } => {
                let messages = Self::messages_map(doc);
                let entry = Self::map(doc, &messages, id);
                ui::doc::put_if_changed(doc, &entry, "server_removed", true);
            }
        }
    }

    /// Where a fetched message lives on its server: `(account, locator)`.
    fn server_entry(doc: &AutoCommit, id: &str) -> Option<(String, String)> {
        let Ok(Some((Value::Object(ObjType::Map), messages))) = doc.get(&ROOT, "messages") else {
            return None;
        };
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&messages, id) else {
            return None;
        };
        Some((
            read_str(doc, &entry, "account")?,
            read_str(doc, &entry, "locator")?,
        ))
    }

    /// What every fetched message's server still has to be told: the
    /// document's flags, folder and permanent deletions where the server
    /// differs, in the order a server applies them (flags in the folder the
    /// message is in, then the move).
    pub fn server_updates(doc: &AutoCommit) -> Vec<ServerUpdate> {
        let Ok(Some((Value::Object(ObjType::Map), messages))) = doc.get(&ROOT, "messages") else {
            return Vec::new();
        };
        let mut updates = Vec::new();
        for id in doc.keys(&messages) {
            let Some((account, locator)) = Self::server_entry(doc, &id) else {
                continue;
            };
            let Some(flags) = Self::flag_entry_ref(doc, &id) else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), server))) = doc.get(&flags, SERVER) else {
                continue;
            };
            let Some(at) = read_str(doc, &server, "folder").and_then(|key| folder_from_key(&key))
            else {
                continue;
            };
            if Self::is_message_purged(doc, &id) {
                let removed = doc
                    .get(&messages, id.as_str())
                    .ok()
                    .flatten()
                    .and_then(|(_, entry)| read_bool(doc, &entry, "server_removed"));
                if removed != Some(true) {
                    updates.push(ServerUpdate::Remove {
                        id,
                        account,
                        locator,
                        folder: at,
                    });
                }
                continue;
            }
            let read = read_bool(doc, &flags, "read");
            let starred = read_bool(doc, &flags, "starred");
            let server_read = read_bool(doc, &server, "read");
            let server_starred = read_bool(doc, &server, "starred");
            if read != server_read || starred != server_starred {
                updates.push(ServerUpdate::Flags {
                    id: id.clone(),
                    account: account.clone(),
                    locator: locator.clone(),
                    folder: at,
                    read: read.or(server_read).unwrap_or(false),
                    starred: starred.or(server_starred).unwrap_or(false),
                });
            }
            if let Some(to) = Self::get_folder_for(doc, &id)
                && to != at
            {
                updates.push(ServerUpdate::Move {
                    id,
                    account,
                    locator,
                    from: at,
                    to,
                });
            }
        }
        updates
    }

    /// Overlay any persisted flags from the CRDT onto an in-memory mailbox.
    /// The corpus is the source of truth for content; the CRDT wins on flags.
    pub fn apply_flags(doc: &AutoCommit, mb: &mut Mailbox) {
        // A permanent delete is a replicated tombstone. Remove only ids with
        // that explicit marker; a document from before the registry schema may
        // legitimately have flags but no `messages` map at all.
        let purged: Vec<_> = mb
            .messages
            .keys()
            .copied()
            .filter(|id| Self::is_message_purged(doc, &id.to_string()))
            .collect();
        for id in purged {
            mb.purge(id);
        }
        let Ok(Some((Value::Object(ObjType::Map), flags))) = doc.get(&ROOT, "flags") else {
            return;
        };
        let ids: Vec<_> = mb.messages.keys().copied().collect();
        for id in ids {
            let key = id.to_string();
            let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&flags, key.as_str())
            else {
                continue;
            };
            let mut changed = false;
            if let Some(m) = mb.get_mut(id) {
                if let Some(r) = read_bool(doc, &entry, "read") {
                    changed |= m.read != r;
                    m.read = r;
                }
                if let Some(s) = read_bool(doc, &entry, "starred") {
                    changed |= m.starred != s;
                    m.starred = s;
                }
                if let Some(f) = read_str(doc, &entry, "folder").and_then(|s| folder_from_key(&s)) {
                    changed |= m.folder != f;
                    m.folder = f;
                }
                if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&entry, "labels") {
                    let labels = read_str_list(doc, &list);
                    changed |= m.labels != labels;
                    m.labels = labels;
                }
            }
            if changed {
                mb.touch_search_revision();
            }
        }
    }

    // ---- draft ----

    pub fn store_draft(doc: &mut AutoCommit, draft: &Draft) {
        let d = Self::map(doc, &ROOT, "draft");
        if let Some(sender) = &draft.sender {
            if let Ok(value) = serde_json::to_string(sender) {
                ui::doc::put_if_changed(doc, &d, "sender", value.as_str());
            }
        } else if doc.get(&d, "sender").ok().flatten().is_some() {
            let _ = doc.delete(&d, "sender");
        }
        ui::doc::put_if_changed(doc, &d, "to", draft.to.as_str());
        ui::doc::put_if_changed(doc, &d, "cc", draft.cc.as_str());
        ui::doc::put_if_changed(doc, &d, "bcc", draft.bcc.as_str());
        ui::doc::put_if_changed(doc, &d, "subject", draft.subject.as_str());
        ui::doc::put_if_changed(doc, &d, "body", draft.body.as_str());
        if let Some(html) = &draft.html_body {
            ui::doc::put_if_changed(doc, &d, "html_body", html.as_str());
        }
        if let Some(calendar_ics) = &draft.calendar_ics {
            ui::doc::put_if_changed(doc, &d, "calendar_ics", calendar_ics.as_str());
        } else if doc.get(&d, "calendar_ics").ok().flatten().is_some() {
            let _ = doc.delete(&d, "calendar_ics");
        }
        let attachments = attachment_store::bounded(&draft.attachments);
        if let Ok(serialized) = serde_json::to_string(&attachments) {
            ui::doc::put_if_changed(doc, &d, "attachments", serialized.as_str());
        }
    }

    pub fn load_draft(doc: &AutoCommit) -> Draft {
        let Ok(Some((Value::Object(ObjType::Map), d))) = doc.get(&ROOT, "draft") else {
            return Draft::default();
        };
        Draft {
            sender: read_str(doc, &d, "sender").and_then(|raw| serde_json::from_str(&raw).ok()),
            to: read_str(doc, &d, "to").unwrap_or_default(),
            cc: read_str(doc, &d, "cc").unwrap_or_default(),
            bcc: read_str(doc, &d, "bcc").unwrap_or_default(),
            subject: read_str(doc, &d, "subject").unwrap_or_default(),
            body: read_str(doc, &d, "body").unwrap_or_default(),
            html_body: read_str(doc, &d, "html_body"),
            calendar_ics: read_str(doc, &d, "calendar_ics")
                .filter(|value| crate::actions::valid_calendar_invitation(value)),
            attachments: read_str(doc, &d, "attachments")
                .and_then(|raw| serde_json::from_str::<Vec<DraftAttachment>>(&raw).ok())
                .map(|items| attachment_store::bounded(&items))
                .unwrap_or_default(),
            in_reply_to: None,
        }
    }

    /// The body being written: rich text under the draft's `rich_body`,
    /// once started. Never writes.
    pub fn draft_body(doc: &AutoCommit) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), d))) = doc.get(&ROOT, "draft") else {
            return None;
        };
        match doc.get(&d, RICH_BODY) {
            Ok(Some((Value::Object(ObjType::Map), body))) => Some(body),
            _ => None,
        }
    }

    /// Start the body being written afresh from `seed`. Backs
    /// [`crate::actions::MailAction::StartDraftBody`].
    pub fn start_draft_body(doc: &mut AutoCommit, seed: &DraftBodySeed) {
        let d = Self::map(doc, &ROOT, "draft");
        let Ok(body) = doc.put_object(&d, RICH_BODY, ObjType::Map) else {
            return;
        };
        if let Some(html) = seed.html.as_deref().filter(|html| !html.trim().is_empty()) {
            // Mail bodies carry no inline images; any the HTML embeds are left out.
            let mut images = tilo_content::ContentInputs::new();
            tilo_doc_codecs::html::import_html_under(doc, &body, html, &mut images);
            return;
        }
        if RichText::initialize_under(doc, &body).is_err() {
            return;
        }
        let blocks: Vec<BlockContent> = seed
            .text
            .split('\n')
            .map(|line| match line.strip_prefix("> ") {
                Some(quoted) => BlockContent {
                    kind: BlockKind::Quote,
                    ..BlockContent::paragraph("", quoted)
                },
                None => BlockContent::paragraph("", line),
            })
            .collect();
        if let Err(error) = tilo_rich_text::fill_under(doc, &body, &blocks) {
            tracing::warn!(%error, "draft body could not be started");
        }
    }

    /// Clear the persisted draft so a later open no longer re-materialises it.
    /// Backs [`crate::actions::MailAction::DiscardDraft`].
    pub fn clear_draft(doc: &mut AutoCommit) {
        let d = Self::map(doc, &ROOT, "draft");
        for key in [
            "to",
            "cc",
            "bcc",
            "subject",
            "body",
            "html_body",
            "calendar_ics",
            "sender",
            "attachments",
            RICH_BODY,
        ] {
            if doc.get(&d, key).ok().flatten().is_some() {
                let _ = doc.delete(&d, key);
            }
        }
    }

    // ---- outbox ----------------------------------------------------------
    //
    // Messages sent but not yet delivered, each under its id: the message as
    // JSON, and the reason its server refused it once it did.

    /// Queue `outgoing` in the outbox.
    pub fn queue_outgoing(doc: &mut AutoCommit, outgoing: &Outgoing) {
        let Ok(message) = serde_json::to_string(outgoing) else {
            return;
        };
        let outbox = Self::map(doc, &ROOT, OUTBOX);
        let entry = Self::map(doc, &outbox, &outgoing.id);
        ui::doc::put_if_changed(doc, &entry, "message", message.as_str());
        if doc.get(&entry, "failed").ok().flatten().is_some() {
            let _ = doc.delete(&entry, "failed");
        }
    }

    /// Take message `id` out of the outbox.
    pub fn remove_outgoing(doc: &mut AutoCommit, id: &str) {
        if let Ok(Some((Value::Object(ObjType::Map), outbox))) = doc.get(&ROOT, OUTBOX)
            && doc.get(&outbox, id).ok().flatten().is_some()
        {
            let _ = doc.delete(&outbox, id);
        }
    }

    /// Record why message `id`'s server refused it.
    pub fn fail_outgoing(doc: &mut AutoCommit, id: &str, reason: &str) {
        if let Ok(Some((Value::Object(ObjType::Map), outbox))) = doc.get(&ROOT, OUTBOX)
            && let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&outbox, id)
        {
            ui::doc::put_if_changed(doc, &entry, "failed", reason);
        }
    }

    /// Every message in the outbox, with the reason its server refused it
    /// if it did.
    pub fn outbox(doc: &AutoCommit) -> Vec<(Outgoing, Option<String>)> {
        let Ok(Some((Value::Object(ObjType::Map), outbox))) = doc.get(&ROOT, OUTBOX) else {
            return Vec::new();
        };
        doc.keys(&outbox)
            .filter_map(|id| {
                let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&outbox, id.as_str())
                else {
                    return None;
                };
                let outgoing: Outgoing =
                    serde_json::from_str(&read_str(doc, &entry, "message")?).ok()?;
                (outgoing.id == id).then(|| (outgoing, read_str(doc, &entry, "failed")))
            })
            .collect()
    }

    // ---- messages the document holds whole ------------------------------
    //
    // Mail delivered without a server and drafts written through the
    // mailbox's actions (`crate::local`), each as JSON under `local/<id>`.
    // Their folder and flags are the ordinary `flags/<id>` entry.

    /// Store `message` under its id, replacing what was stored there.
    pub fn store_local(doc: &mut AutoCommit, message: &crate::local::LocalMessage) {
        let Ok(json) = serde_json::to_string(message) else {
            return;
        };
        let local = Self::map(doc, &ROOT, LOCAL);
        ui::doc::put_if_changed(doc, &local, message.id.as_str(), json.as_str());
    }

    /// The message stored whole under `id`, unless it was purged.
    pub fn local_message(doc: &AutoCommit, id: &str) -> Option<crate::local::LocalMessage> {
        let Ok(Some((Value::Object(ObjType::Map), local))) = doc.get(&ROOT, LOCAL) else {
            return None;
        };
        if Self::is_message_purged(doc, id) {
            return None;
        }
        let message: crate::local::LocalMessage =
            serde_json::from_str(&read_str(doc, &local, id)?).ok()?;
        (message.id == id).then(|| message.bounded())
    }

    /// Whether anything was ever stored under `id`, purged or not.
    pub fn has_local_entry(doc: &AutoCommit, id: &str) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), local))) = doc.get(&ROOT, LOCAL) else {
            return false;
        };
        doc.get(&local, id).ok().flatten().is_some()
    }

    /// Every message stored whole that was not purged, with its flags, in
    /// id order.
    pub fn local_messages(doc: &AutoCommit) -> Vec<crate::local::Stored> {
        let Ok(Some((Value::Object(ObjType::Map), local))) = doc.get(&ROOT, LOCAL) else {
            return Vec::new();
        };
        let mut ids: Vec<String> = doc.keys(&local).collect();
        ids.sort();
        ids.into_iter()
            .filter_map(|id| {
                let message = Self::local_message(doc, &id)?;
                Some(crate::local::Stored {
                    flags: crate::local::Flags {
                        folder: Self::get_folder_for(doc, &id).unwrap_or(Folder::Inbox),
                        read: Self::get_read(doc, &id).unwrap_or(false),
                        starred: Self::get_starred(doc, &id).unwrap_or(false),
                        labels: Self::get_labels(doc, &id),
                    },
                    message,
                })
            })
            .collect()
    }

    /// The stored message whose `Message-ID` is `message_id`.
    pub fn local_by_message_id(
        doc: &AutoCommit,
        message_id: &str,
    ) -> Option<crate::local::LocalMessage> {
        let Ok(Some((Value::Object(ObjType::Map), local))) = doc.get(&ROOT, LOCAL) else {
            return None;
        };
        let wanted = crate::local::bare_message_id(message_id);
        doc.keys(&local)
            .filter_map(|id| Self::local_message(doc, &id))
            .find(|message| message.message_id == wanted)
    }

    /// Hydrate every stored message `mb` does not hold yet into it, so the
    /// window lists and opens them like fetched mail.
    pub fn hydrate_local(doc: &AutoCommit, mb: &mut Mailbox) {
        for stored in Self::local_messages(doc) {
            let Some(message) = stored.message.to_message(&stored.flags) else {
                continue;
            };
            if mb.get(message.id).is_none_or(|held| *held != message) {
                mb.insert(message);
            }
        }
    }

    // ---- known message registry -----------------------------------------
    //
    // The corpus itself remains outside Automerge, but its stable identifiers
    // are recorded when Mail ingests it. A later resource action can therefore
    // refuse an id that was purged or was never materialised rather than
    // silently creating an orphan `flags/<id>` map. Ingestion is external state
    // reconciliation; every user mutation below still goes through MailAction.

    fn messages_map(doc: &mut AutoCommit) -> ObjId {
        Self::map(doc, &ROOT, "messages")
    }

    /// Register a message id obtained from the external corpus as an eligible
    /// action target. This is idempotent and deliberately not an action:
    /// downloading mail is external ingestion, not a collaborative edit.
    pub fn register_message(doc: &mut AutoCommit, id: &str) {
        if id.is_empty() {
            return;
        }
        let messages = Self::messages_map(doc);
        let entry = Self::map(doc, &messages, id);
        // `purged` is a write-once tombstone.  A stale IMAP hydration can race
        // a permanent delete on another replica; it may still write `present`,
        // but it can never clear this marker and so can never revive the
        // message as an action target or projection.
        if read_bool(doc, &entry, "purged") == Some(true) {
            return;
        }
        ui::doc::put_if_changed(doc, &entry, "present", true);
        // Materialise the stable flag map while ingesting the external id.
        // Otherwise two peers can concurrently create separate map objects for
        // a just-hydrated message (one while marking read, one while starring)
        // and Automerge must choose a container instead of merging the scalar
        // fields inside it.
        let _ = Self::flag_entry(doc, id);
    }

    /// Whether an id is currently a valid, materialised message target.
    /// Explicit tombstones and unregistered ids fail closed.
    pub fn is_present_message(doc: &AutoCommit, id: &str) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), messages))) = doc.get(&ROOT, "messages") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&messages, id) else {
            return false;
        };
        read_bool(doc, &entry, "purged") != Some(true)
            && read_bool(doc, &entry, "present").unwrap_or(false)
    }

    /// Record a permanent local purge. Unlike a missing registry entry this is
    /// an explicit tombstone, so a delayed action can never resurrect flags.
    pub fn mark_message_purged(doc: &mut AutoCommit, id: &str) {
        let messages = Self::messages_map(doc);
        let entry = Self::map(doc, &messages, id);
        ui::doc::put_if_changed(doc, &entry, "purged", true);
        ui::doc::put_if_changed(doc, &entry, "present", false);
    }

    /// Whether the document carries an explicit purge tombstone for `id`.
    fn is_message_purged(doc: &AutoCommit, id: &str) -> bool {
        let Ok(Some((Value::Object(ObjType::Map), messages))) = doc.get(&ROOT, "messages") else {
            return false;
        };
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&messages, id) else {
            return false;
        };
        read_bool(doc, &entry, "purged") == Some(true)
            // Documents written before the explicit `purged` field retain
            // their original `present: false` tombstones on upgrade.
            || read_bool(doc, &entry, "present") == Some(false)
    }

    // ---- granular per-message flag writers (the action apply path) ----
    //
    // These key off the message-id *string* (not a whole `Message`), because an
    // action carries only the id and the target value. Each is idempotent via
    // `put_if_changed`, so a redundant write is a no-op and never grows the doc.

    /// The `flags/<id>` entry map for a message id string, created if absent.
    fn flag_entry(doc: &mut AutoCommit, id: &str) -> ObjId {
        let flags = Self::flags_map(doc);
        Self::map(doc, &flags, id)
    }

    /// Set a message's read flag.
    pub fn set_read(doc: &mut AutoCommit, id: &str, read: bool) {
        let entry = Self::flag_entry(doc, id);
        ui::doc::put_if_changed(doc, &entry, "read", read);
    }

    /// Set a message's starred flag.
    pub fn set_starred(doc: &mut AutoCommit, id: &str, starred: bool) {
        let entry = Self::flag_entry(doc, id);
        ui::doc::put_if_changed(doc, &entry, "starred", starred);
    }

    /// Move a message to `folder` (archive / delete-to-trash / junk / inbox are
    /// all this with a specific target).
    pub fn set_folder_for(doc: &mut AutoCommit, id: &str, folder: Folder) {
        let entry = Self::flag_entry(doc, id);
        ui::doc::put_if_changed(doc, &entry, "folder", folder_key(folder));
    }

    /// Read a message's stored flag entry, if any.
    fn flag_entry_ref(doc: &AutoCommit, id: &str) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), flags))) = doc.get(&ROOT, "flags") else {
            return None;
        };
        match doc.get(&flags, id) {
            Ok(Some((Value::Object(ObjType::Map), e))) => Some(e),
            _ => None,
        }
    }

    /// The stored read flag for a message, or `None` if unset.
    pub fn get_read(doc: &AutoCommit, id: &str) -> Option<bool> {
        let entry = Self::flag_entry_ref(doc, id)?;
        read_bool(doc, &entry, "read")
    }

    /// The stored starred flag for a message, or `None` if unset.
    pub fn get_starred(doc: &AutoCommit, id: &str) -> Option<bool> {
        let entry = Self::flag_entry_ref(doc, id)?;
        read_bool(doc, &entry, "starred")
    }

    /// The stored folder for a message, or `None` if unset / an unknown key.
    pub fn get_folder_for(doc: &AutoCommit, id: &str) -> Option<Folder> {
        let entry = Self::flag_entry_ref(doc, id)?;
        read_str(doc, &entry, "folder").and_then(|s| folder_from_key(&s))
    }

    // ---- per-message labels/tags (`flags/<id>/labels`) ----

    /// The `flags/<id>/labels` list for a message id, created if absent.
    ///
    /// # Panics
    /// Never in practice: `entry` is a Map `flag_entry` just resolved-or-
    /// created, so `put_object` on it only fails for an already-corrupted
    /// document.
    #[allow(
        clippy::expect_used,
        reason = "entry is a Map flag_entry just resolved-or-created, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write this message's labels into the wrong object"
    )]
    fn labels_list(doc: &mut AutoCommit, id: &str) -> ObjId {
        let entry = Self::flag_entry(doc, id);
        match doc.get(&entry, "labels") {
            Ok(Some((Value::Object(ObjType::List), list))) => list,
            _ => doc
                .put_object(&entry, "labels", ObjType::List)
                .expect("put_object list never fails"),
        }
    }

    /// Read a message's labels, in stored order.
    pub fn get_labels(doc: &AutoCommit, id: &str) -> Vec<String> {
        let Ok(Some((Value::Object(ObjType::Map), flags))) = doc.get(&ROOT, "flags") else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::Map), entry))) = doc.get(&flags, id) else {
            return Vec::new();
        };
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&entry, "labels") else {
            return Vec::new();
        };
        read_str_list(doc, &list)
    }

    /// Collaborative flag projection for every non-purged message id. The
    /// immutable corpus (subject, body, attachments) stays outside Automerge;
    /// Document ops only need ids and flag/label/folder state.
    pub fn list_message_flags(doc: &AutoCommit) -> Vec<MessageFlags> {
        let Ok(Some((Value::Object(ObjType::Map), flags))) = doc.get(&ROOT, "flags") else {
            return Vec::new();
        };
        let mut out: Vec<MessageFlags> = doc
            .keys(&flags)
            .filter(|id| !Self::is_message_purged(doc, id))
            .map(|id| MessageFlags {
                id: id.clone(),
                folder: Self::get_folder_for(doc, &id).unwrap_or(Folder::Inbox),
                read: Self::get_read(doc, &id).unwrap_or(false),
                starred: Self::get_starred(doc, &id).unwrap_or(false),
                labels: Self::get_labels(doc, &id),
            })
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Add a label to a message. A no-op when the label is already present, so
    /// the operation is deterministic and the list stays a set.
    pub fn add_label(doc: &mut AutoCommit, id: &str, label: &str) {
        if label.is_empty() {
            return;
        }
        let list = Self::labels_list(doc, id);
        if read_str_list(doc, &list).iter().any(|l| l == label) {
            return;
        }
        let idx = doc.length(&list);
        let _ = doc.insert(&list, idx, label);
    }

    /// Remove a label from a message. A no-op when it is not present.
    pub fn remove_label(doc: &mut AutoCommit, id: &str, label: &str) {
        let list = Self::labels_list(doc, id);
        let current = read_str_list(doc, &list);
        if let Some(idx) = current.iter().position(|l| l == label) {
            let _ = doc.delete(&list, idx);
        }
    }

    // ---- user-defined local folders (`folders/<id> = name`) ----
    //
    // A registry of local folders the user creates. Kept separate from the
    // system `Folder` enum: system folders are fixed, these are data. Stored as a
    // flat `id -> name` map so create/rename/delete are single, replayable puts.

    fn folders_map(doc: &mut AutoCommit) -> ObjId {
        Self::map(doc, &ROOT, "folders")
    }

    /// Create (or, given an existing id, overwrite) a local folder.
    pub fn create_folder(doc: &mut AutoCommit, id: &str, name: &str) {
        if id.is_empty() {
            return;
        }
        let folders = Self::folders_map(doc);
        ui::doc::put_if_changed(doc, &folders, id, name);
    }

    /// Rename a local folder. A no-op when the id is unknown, so a stray replay
    /// can't resurrect a deleted folder.
    pub fn rename_folder(doc: &mut AutoCommit, id: &str, name: &str) {
        let folders = Self::folders_map(doc);
        if doc.get(&folders, id).ok().flatten().is_some() {
            ui::doc::put_if_changed(doc, &folders, id, name);
        }
    }

    /// Delete a local folder from the registry.
    pub fn delete_folder(doc: &mut AutoCommit, id: &str) {
        let folders = Self::folders_map(doc);
        if doc.get(&folders, id).ok().flatten().is_some() {
            let _ = doc.delete(&folders, id);
        }
    }

    /// List the user-defined local folders as `(id, name)`, sorted by id so the
    /// resource tree (and any UI over it) has a stable order across replicas.
    pub fn list_folders(doc: &AutoCommit) -> Vec<(String, String)> {
        let Ok(Some((Value::Object(ObjType::Map), folders))) = doc.get(&ROOT, "folders") else {
            return Vec::new();
        };
        let mut out: Vec<(String, String)> = doc
            .keys(&folders)
            .filter_map(|k| read_str(doc, &folders, &k).map(|name| (k, name)))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// Read an Automerge list of strings into a `Vec<String>`, in stored order.
fn read_str_list(doc: &AutoCommit, list: &ObjId) -> Vec<String> {
    let n = doc.length(list);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        if let Ok(Some((Value::Scalar(v), _))) = doc.get(list, i)
            && let ScalarValue::Str(s) = v.as_ref()
        {
            out.push(s.to_string());
        }
    }
    out
}

/// The CRDT storage key for a folder. Canonical spelling lives on the model
/// ([`Folder::as_key`]) so the action layer and this persistence layer agree.
fn folder_key(f: Folder) -> &'static str {
    f.as_key()
}

fn folder_from_key(s: &str) -> Option<Folder> {
    Folder::from_key(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample::sample_mailbox;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn flags_and_purges_converge_without_resurrection() {
        // Phase `flags_persist_and_overlay`.
        {
            let mut doc = AutoCommit::new();
            let mut mb = sample_mailbox();
            let id = *mb.messages.keys().next().unwrap();
            mb.mark_read(id, true);
            mb.toggle_star(id);
            let starred = mb.get(id).unwrap().starred;
            MailDoc::ingest_baseline(&mut doc, &IngestBaseline::from_message(mb.get(id).unwrap()));

            // Fresh corpus with default flags; overlay should restore them.
            let mut mb2 = sample_mailbox();
            // Ensure the same id exists (it won't, since uuids regenerate) — so
            // store/overlay round-trips on the SAME mailbox instance instead.
            MailDoc::apply_flags(&doc, &mut mb2);
            // The flagged id is from mb (different uuid space). Re-test on mb:
            let mut mb3 = mb.clone();
            for m in mb3.messages.values_mut() {
                m.read = false;
                m.starred = false;
            }
            MailDoc::ingest_baseline(&mut doc, &IngestBaseline::from_message(mb.get(id).unwrap()));
            MailDoc::apply_flags(&doc, &mut mb3);
            assert_eq!(mb3.get(id).unwrap().starred, starred);
            assert!(mb3.get(id).unwrap().read);
        }

        // Phase `delayed_ingestion_never_overwrites_a_canonical_flag_action`.
        {
            let mut doc = AutoCommit::new();
            let mut mailbox = sample_mailbox();
            let id = *mailbox.messages.keys().next().expect("sample message");
            let fetched = mailbox.get(id).expect("sample message").clone();
            MailDoc::ingest_baseline(&mut doc, &IngestBaseline::from_message(&fetched));
            MailDoc::set_read(&mut doc, &id.to_string(), true);

            let mut stale_fetch = fetched;
            stale_fetch.read = false;
            MailDoc::ingest_baseline(&mut doc, &IngestBaseline::from_message(&stale_fetch));
            assert_eq!(MailDoc::get_read(&doc, &id.to_string()), Some(true));

            MailDoc::apply_flags(&doc, &mut mailbox);
            assert!(mailbox.get(id).is_some_and(|message| message.read));
        }

        // Phase `purge_tombstone_removes_the_hydrated_copy_on_peer_sync`.
        {
            let mut doc = AutoCommit::new();
            let mut mailbox = sample_mailbox();
            let id = *mailbox.messages.keys().next().expect("sample message");
            MailDoc::ingest_baseline(
                &mut doc,
                &IngestBaseline::from_message(mailbox.get(id).expect("message")),
            );
            MailDoc::mark_message_purged(&mut doc, &id.to_string());

            MailDoc::apply_flags(&doc, &mut mailbox);
            assert!(
                mailbox.get(id).is_none(),
                "a peer converges by dropping the tombstoned hydrated message"
            );
        }

        // Phase `stale_ingestion_cannot_resurrect_a_purged_message`.
        {
            let mut doc = AutoCommit::new();
            MailDoc::register_message(&mut doc, "m1");
            MailDoc::mark_message_purged(&mut doc, "m1");
            // A delayed external hydration is explicitly allowed to try to
            // register its stable id, but the write-once purge marker wins.
            MailDoc::register_message(&mut doc, "m1");
            assert!(!MailDoc::is_present_message(&doc, "m1"));
            assert!(MailDoc::is_message_purged(&doc, "m1"));
        }

        // Phase `granular_flag_setters_and_clear_draft`.
        {
            let mut doc = AutoCommit::new();
            MailDoc::set_read(&mut doc, "m1", true);
            MailDoc::set_starred(&mut doc, "m1", true);
            MailDoc::set_folder_for(&mut doc, "m1", Folder::Archive);
            let mut mb = sample_mailbox();
            // Point a real message at the same id so the overlay is observable.
            let some = *mb.messages.keys().next().unwrap();
            {
                let m = mb.get_mut(some).unwrap();
                m.read = false;
                m.starred = false;
                m.folder = Folder::Inbox;
            }
            MailDoc::set_read(&mut doc, &some.to_string(), true);
            MailDoc::set_starred(&mut doc, &some.to_string(), true);
            MailDoc::set_folder_for(&mut doc, &some.to_string(), Folder::Archive);
            MailDoc::add_label(&mut doc, &some.to_string(), "work");
            MailDoc::apply_flags(&doc, &mut mb);
            let m = mb.get(some).unwrap();
            assert!(m.read && m.starred);
            assert_eq!(m.folder, Folder::Archive);
            assert_eq!(m.labels, vec!["work"]);

            let draft = Draft {
                to: "x@y.example".into(),
                subject: "Hi".into(),
                ..Default::default()
            };
            MailDoc::store_draft(&mut doc, &draft);
            assert!(!MailDoc::load_draft(&doc).to.is_empty());
            MailDoc::clear_draft(&mut doc);
            assert!(
                MailDoc::load_draft(&doc).to.is_empty(),
                "discard clears the persisted draft"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn drafts_labels_and_folders_round_trip_and_stay_bounded() {
        // Phase `draft_round_trips`.
        {
            let mut doc = AutoCommit::new();
            let content = tilo_content::ContentStore::memory();
            let attachment =
                crate::attachment_store::stage_bytes(&content, b"draft blob", "agenda.txt")
                    .expect("portable attachment");
            let draft = Draft {
                to: "x@y.example".into(),
                subject: "Hi".into(),
                body: "Body".into(),
                attachments: vec![attachment.clone()],
                ..Default::default()
            };
            MailDoc::store_draft(&mut doc, &draft);
            let loaded = MailDoc::load_draft(&doc);
            assert_eq!(loaded.to, "x@y.example");
            assert_eq!(loaded.subject, "Hi");
            assert_eq!(loaded.attachments, vec![attachment]);
        }

        // Phase `labels_add_remove_and_round_trip`.
        {
            let mut doc = AutoCommit::new();
            MailDoc::add_label(&mut doc, "m1", "work");
            MailDoc::add_label(&mut doc, "m1", "urgent");
            // Duplicate add is a no-op — labels stay a set.
            MailDoc::add_label(&mut doc, "m1", "work");
            assert_eq!(MailDoc::get_labels(&doc, "m1"), vec!["work", "urgent"]);
            MailDoc::remove_label(&mut doc, "m1", "work");
            assert_eq!(MailDoc::get_labels(&doc, "m1"), vec!["urgent"]);
            // Removing an absent label is a no-op.
            MailDoc::remove_label(&mut doc, "m1", "nope");
            assert_eq!(MailDoc::get_labels(&doc, "m1"), vec!["urgent"]);
        }

        // Phase `local_folder_registry_crud`.
        {
            let mut doc = AutoCommit::new();
            assert!(MailDoc::list_folders(&doc).is_empty());
            MailDoc::create_folder(&mut doc, "f-a", "Receipts");
            MailDoc::create_folder(&mut doc, "f-b", "Travel");
            assert_eq!(
                MailDoc::list_folders(&doc),
                vec![
                    ("f-a".to_string(), "Receipts".to_string()),
                    ("f-b".to_string(), "Travel".to_string())
                ]
            );
            MailDoc::rename_folder(&mut doc, "f-a", "Invoices");
            // Renaming an unknown id does nothing.
            MailDoc::rename_folder(&mut doc, "ghost", "X");
            MailDoc::delete_folder(&mut doc, "f-b");
            assert_eq!(
                MailDoc::list_folders(&doc),
                vec![("f-a".to_string(), "Invoices".to_string())]
            );
        }

        // Phase `repeated_store_draft_keeps_doc_bounded`.
        {
            let mut doc = AutoCommit::new();
            let draft = Draft {
                to: "x@y.example".into(),
                subject: "Hi".into(),
                body: "Body".repeat(50),
                ..Default::default()
            };
            MailDoc::store_draft(&mut doc, &draft);
            let a = doc.save().len();
            for _ in 0..50 {
                MailDoc::store_draft(&mut doc, &draft);
            }
            let b = doc.save().len();
            assert_eq!(
                a, b,
                "re-storing an unchanged draft grew the document {a} -> {b}"
            );
        }
    }

    /// A message fetched from `account` as its server has it.
    fn fetched(id: &str, read: bool, folder: Folder) -> IngestBaseline {
        IngestBaseline {
            id: id.into(),
            read: Some(read),
            starred: Some(false),
            folder: Some(folder.as_key().into()),
            account: Some("you@example.test".into()),
            locator: Some(format!("{id}@example.test")),
        }
    }

    /// Where the document and the server differ, the server has to be told;
    /// once it confirms, nothing is left. A change taken back before then
    /// leaves nothing to tell, and a message that exists only here never
    /// goes to a server.
    #[test]
    fn server_updates_follow_the_document_until_the_server_confirms() {
        let mut doc = AutoCommit::new();
        MailDoc::ingest_baseline(&mut doc, &fetched("m1", false, Folder::Inbox));
        assert!(MailDoc::server_updates(&doc).is_empty(), "nothing changed");

        MailDoc::set_read(&mut doc, "m1", true);
        let flags = ServerUpdate::Flags {
            id: "m1".into(),
            account: "you@example.test".into(),
            locator: "m1@example.test".into(),
            folder: Folder::Inbox,
            read: true,
            starred: false,
        };
        assert_eq!(MailDoc::server_updates(&doc), std::slice::from_ref(&flags));
        MailDoc::server_applied(&mut doc, &flags);
        assert!(MailDoc::server_updates(&doc).is_empty());

        MailDoc::set_read(&mut doc, "m1", false);
        MailDoc::set_read(&mut doc, "m1", true);
        assert!(
            MailDoc::server_updates(&doc).is_empty(),
            "a change taken back leaves nothing to tell"
        );

        MailDoc::set_folder_for(&mut doc, "m1", Folder::Trash);
        let moved = ServerUpdate::Move {
            id: "m1".into(),
            account: "you@example.test".into(),
            locator: "m1@example.test".into(),
            from: Folder::Inbox,
            to: Folder::Trash,
        };
        assert_eq!(MailDoc::server_updates(&doc), std::slice::from_ref(&moved));
        MailDoc::server_applied(&mut doc, &moved);
        assert!(MailDoc::server_updates(&doc).is_empty());

        MailDoc::mark_message_purged(&mut doc, "m1");
        let removed = ServerUpdate::Remove {
            id: "m1".into(),
            account: "you@example.test".into(),
            locator: "m1@example.test".into(),
            folder: Folder::Trash,
        };
        assert_eq!(
            MailDoc::server_updates(&doc),
            std::slice::from_ref(&removed)
        );
        MailDoc::server_applied(&mut doc, &removed);
        assert!(MailDoc::server_updates(&doc).is_empty());

        let mut local = IngestBaseline::from_message(
            &sample_mailbox().messages.values().next().cloned().unwrap(),
        );
        local.id = "draft".into();
        MailDoc::ingest_baseline(&mut doc, &local);
        MailDoc::set_starred(&mut doc, "draft", true);
        assert!(
            MailDoc::server_updates(&doc).is_empty(),
            "a local message stays local"
        );
    }

    /// A flag the server changed since it last reported it (another mail
    /// client read the message) is adopted; one it reports unchanged never
    /// overwrites a change still on its way to it.
    #[test]
    fn a_fetch_adopts_server_changes_and_keeps_changes_on_their_way() {
        let mut doc = AutoCommit::new();
        MailDoc::ingest_baseline(&mut doc, &fetched("m1", false, Folder::Inbox));
        MailDoc::ingest_baseline(&mut doc, &fetched("m1", true, Folder::Inbox));
        assert_eq!(MailDoc::get_read(&doc, "m1"), Some(true));
        assert!(MailDoc::server_updates(&doc).is_empty());

        MailDoc::set_starred(&mut doc, "m1", true);
        MailDoc::ingest_baseline(&mut doc, &fetched("m1", true, Folder::Inbox));
        assert_eq!(MailDoc::get_starred(&doc, "m1"), Some(true));
        assert_eq!(
            MailDoc::server_updates(&doc).len(),
            1,
            "the star still has to go"
        );

        MailDoc::ingest_baseline(&mut doc, &fetched("m1", true, Folder::Archive));
        assert_eq!(
            MailDoc::get_folder_for(&doc, "m1"),
            Some(Folder::Archive),
            "a move made elsewhere is adopted"
        );
    }
}
