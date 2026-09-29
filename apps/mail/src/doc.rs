//! Automerge document model for the Mail app.
//!
//! State that must survive a crash, sync between peers, and participate in
//! undo lives here — not in widget fields. Concretely:
//!
//! ```text
//! root
//! ├── "flags"   (Map)  per-message mutable state, keyed by message-id string
//! │     └── <id> (Map) { read: bool, starred: bool, folder: Str }
//! ├── "messages" (Map)  externally-ingested ids `{ present, purged }`
//! └── "draft"   (Map)  the message currently being composed
//!       ├── to, cc, bcc, subject, body, html_body, … (Str) as last saved
//!       └── "rich_body" (Map) the body being written: qdoc rich text
//! ```
//!
//! The immutable message corpus (headers/bodies/attachments) is *not* stored
//! here — it's hydrated from the server (or the sample corpus) into the
//! in-memory [`Mailbox`]. Navigation, search, selection, scroll position and
//! editor formatting are deliberately device-local presentation state. Only
//! domain mutations (flags/folders/labels/drafts) ride in the CRDT.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
use ui::doc::{read_bool, read_str};

use tilo_document::RichText;
use ui::widget::{BlockContent, BlockKind};

use crate::actions::DraftBodySeed;
use crate::attachment_store;
use crate::model::{Draft, DraftAttachment, Folder, Mailbox, Message};

/// The draft's key holding the body being written.
const RICH_BODY: &str = "rich_body";

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

    /// Seed previously-unseen flag fields from an externally-ingested message.
    ///
    /// This is intentionally *not* a user mutation path. Existing CRDT values
    /// win field-by-field, so a delayed IMAP fetch cannot overwrite a local or
    /// remote `MailAction`; server flags merely establish the first baseline
    /// for a newly hydrated message.
    pub fn ingest_message_flags(doc: &mut AutoCommit, msg: &Message) {
        Self::ingest_baseline(
            doc,
            &msg.id.to_string(),
            Some(msg.read),
            Some(msg.starred),
            Some(msg.folder),
        );
    }

    /// Register `id` and, when a flag is still unset, record the server baseline.
    pub fn ingest_baseline(
        doc: &mut AutoCommit,
        id: &str,
        read: Option<bool>,
        starred: Option<bool>,
        folder: Option<Folder>,
    ) {
        if id.is_empty() {
            return;
        }
        Self::register_message(doc, id);
        if let Some(read) = read
            && Self::get_read(doc, id).is_none()
        {
            Self::set_read(doc, id, read);
        }
        if let Some(starred) = starred
            && Self::get_starred(doc, id).is_none()
        {
            Self::set_starred(doc, id, starred);
        }
        if let Some(folder) = folder
            && Self::get_folder_for(doc, id).is_none()
        {
            Self::set_folder_for(doc, id, folder);
        }
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
            "attachments",
            RICH_BODY,
        ] {
            if doc.get(&d, key).ok().flatten().is_some() {
                let _ = doc.delete(&d, key);
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
            MailDoc::ingest_message_flags(&mut doc, mb.get(id).unwrap());

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
            MailDoc::ingest_message_flags(&mut doc, mb.get(id).unwrap());
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
            MailDoc::ingest_message_flags(&mut doc, &fetched);
            MailDoc::set_read(&mut doc, &id.to_string(), true);

            let mut stale_fetch = fetched;
            stale_fetch.read = false;
            MailDoc::ingest_message_flags(&mut doc, &stale_fetch);
            assert_eq!(MailDoc::get_read(&doc, &id.to_string()), Some(true));

            MailDoc::apply_flags(&doc, &mut mailbox);
            assert!(mailbox.get(id).is_some_and(|message| message.read));
        }

        // Phase `purge_tombstone_removes_the_hydrated_copy_on_peer_sync`.
        {
            let mut doc = AutoCommit::new();
            let mut mailbox = sample_mailbox();
            let id = *mailbox.messages.keys().next().expect("sample message");
            MailDoc::ingest_message_flags(&mut doc, mailbox.get(id).expect("message"));
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
}
