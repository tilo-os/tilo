//! Automerge document model for the Mail app.
//!
//! State that must survive a crash, sync between peers, and participate in
//! undo lives here — not in widget fields. Concretely:
//!
//! ```text
//! root
//! ├── "ui"      (Map)  selected folder / view mode / search query / selection
//! ├── "flags"   (Map)  per-message mutable state, keyed by message-id string
//! │     └── <id> (Map) { read: bool, starred: bool, folder: Str }
//! ├── "account" (Map)  IMAP/SMTP connection settings (the setup form)
//! └── "draft"   (Map)  the message currently being composed
//! ```
//!
//! The immutable message corpus (headers/bodies/attachments) is *not* stored
//! here — it's hydrated from the server (or the sample corpus) into the
//! in-memory [`Mailbox`]. Only the flags ride in the CRDT, which is what lets
//! "mark read on peer A" show up on peer B and undo cleanly.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{AccountConfig, Draft, Folder, Mailbox, Message};

pub struct MailDoc;

impl MailDoc {
    // ---- generic scalar helpers ----

    fn map(doc: &mut AutoCommit, parent: &ObjId, key: &str) -> ObjId {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Map)
                .expect("put_object map never fails"),
        }
    }

    fn read_str(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<String> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_bool(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<bool> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_i64(doc: &AutoCommit, obj: &ObjId, key: &str) -> Option<i64> {
        match doc.get(obj, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64(),
            _ => None,
        }
    }

    // ---- UI state ----

    fn ui(doc: &mut AutoCommit) -> ObjId {
        Self::map(doc, &ROOT, "ui")
    }

    pub fn set_folder(doc: &mut AutoCommit, folder: Folder) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "folder", folder_key(folder));
    }

    pub fn get_folder(doc: &AutoCommit) -> Folder {
        let ui = match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Folder::Inbox,
        };
        Self::read_str(doc, &ui, "folder")
            .and_then(|s| folder_from_key(&s))
            .unwrap_or(Folder::Inbox)
    }

    pub fn set_mode(doc: &mut AutoCommit, mode: &str) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "mode", mode);
    }

    pub fn get_mode(doc: &AutoCommit) -> String {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_str(doc, &ui, "mode").unwrap_or_else(|| "mailbox".into())
            }
            _ => "mailbox".into(),
        }
    }

    pub fn set_search(doc: &mut AutoCommit, query: &str) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "search", query);
    }

    pub fn get_search(doc: &AutoCommit) -> String {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_str(doc, &ui, "search").unwrap_or_default()
            }
            _ => String::new(),
        }
    }

    pub fn set_selected(doc: &mut AutoCommit, message_id: Option<&str>) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "selected", message_id.unwrap_or(""));
    }

    /// Persist the compose body format choice ("html" | "text") so the user's
    /// preference is remembered across compose windows and sessions.
    pub fn set_compose_format(doc: &mut AutoCommit, key: &str) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "compose_format", key);
    }

    pub fn get_compose_format(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => Self::read_str(doc, &ui, "compose_format"),
            _ => None,
        }
    }

    /// Persist which account is active in the sidebar.
    pub fn set_active_account(doc: &mut AutoCommit, idx: usize) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "active_account", idx as i64);
    }

    pub fn get_active_account(doc: &AutoCommit) -> Option<usize> {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_i64(doc, &ui, "active_account").map(|v| v.max(0) as usize)
            }
            _ => None,
        }
    }

    /// Persist the user's explicit sidebar open/collapsed choice as view/pref
    /// state, so it survives a window resize and a relaunch (and syncs). `None`
    /// clears it back to the width-driven default.
    pub fn set_sidebar_collapsed(doc: &mut AutoCommit, collapsed: Option<bool>) {
        let ui = Self::ui(doc);
        match collapsed {
            Some(v) => {
                let _ = doc.put(&ui, "sidebar_collapsed", v);
            }
            None => {
                let _ = doc.delete(&ui, "sidebar_collapsed");
            }
        }
    }

    /// The persisted sidebar collapse choice, or `None` if the user has never
    /// set one (follow the responsive default).
    pub fn get_sidebar_collapsed(doc: &AutoCommit) -> Option<bool> {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => Self::read_bool(doc, &ui, "sidebar_collapsed"),
            _ => None,
        }
    }

    pub fn get_selected(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_str(doc, &ui, "selected").filter(|s| !s.is_empty())
            }
            _ => None,
        }
    }

    /// Mark the document as already seeded so a later open restores the
    /// persisted session state instead of overwriting it from defaults.
    pub fn set_seeded(doc: &mut AutoCommit) {
        let ui = Self::ui(doc);
        let _ = doc.put(&ui, "seeded", true);
    }

    /// Whether this document has been seeded before (i.e. carries a real
    /// persisted session: folder / selection / active account / format).
    pub fn is_seeded(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_bool(doc, &ui, "seeded").unwrap_or(false)
            }
            _ => false,
        }
    }

    // ---- per-message flags ----

    fn flags_map(doc: &mut AutoCommit) -> ObjId {
        Self::map(doc, &ROOT, "flags")
    }

    /// Mirror a message's mutable flags into the CRDT.
    pub fn store_flags(doc: &mut AutoCommit, msg: &Message) {
        let flags = Self::flags_map(doc);
        let key = msg.id.to_string();
        let entry = Self::map(doc, &flags, &key);
        ui::doc::put_if_changed(doc, &entry, "read", msg.read);
        ui::doc::put_if_changed(doc, &entry, "starred", msg.starred);
        ui::doc::put_if_changed(doc, &entry, "folder", folder_key(msg.folder));
    }

    /// Overlay any persisted flags from the CRDT onto an in-memory mailbox.
    /// The corpus is the source of truth for content; the CRDT wins on flags.
    pub fn apply_flags(doc: &AutoCommit, mb: &mut Mailbox) {
        let flags = match doc.get(&ROOT, "flags") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return,
        };
        let ids: Vec<_> = mb.messages.keys().copied().collect();
        for id in ids {
            let key = id.to_string();
            let entry = match doc.get(&flags, key.as_str()) {
                Ok(Some((Value::Object(ObjType::Map), e))) => e,
                _ => continue,
            };
            if let Some(m) = mb.get_mut(id) {
                if let Some(r) = Self::read_bool(doc, &entry, "read") {
                    m.read = r;
                }
                if let Some(s) = Self::read_bool(doc, &entry, "starred") {
                    m.starred = s;
                }
                if let Some(f) = Self::read_str(doc, &entry, "folder").and_then(|s| folder_from_key(&s))
                {
                    m.folder = f;
                }
                if let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(&entry, "labels") {
                    m.labels = read_str_list(doc, &list);
                }
            }
        }
    }

    // ---- account config ----

    pub fn store_account(doc: &mut AutoCommit, cfg: &AccountConfig) {
        let acc = Self::map(doc, &ROOT, "account");
        ui::doc::put_if_changed(doc, &acc, "display_name", cfg.display_name.as_str());
        ui::doc::put_if_changed(doc, &acc, "email", cfg.email.as_str());
        ui::doc::put_if_changed(doc, &acc, "imap_host", cfg.imap_host.as_str());
        ui::doc::put_if_changed(doc, &acc, "imap_port", cfg.imap_port as i64);
        ui::doc::put_if_changed(doc, &acc, "imap_tls", cfg.imap_tls);
        ui::doc::put_if_changed(doc, &acc, "smtp_host", cfg.smtp_host.as_str());
        ui::doc::put_if_changed(doc, &acc, "smtp_port", cfg.smtp_port as i64);
        ui::doc::put_if_changed(doc, &acc, "smtp_tls", cfg.smtp_tls);
        ui::doc::put_if_changed(doc, &acc, "username", cfg.username.as_str());
        ui::doc::put_if_changed(doc, &acc, "auth", cfg.auth.as_key());
    }

    pub fn load_account(doc: &AutoCommit) -> Option<AccountConfig> {
        let acc = match doc.get(&ROOT, "account") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return None,
        };
        Some(AccountConfig {
            display_name: Self::read_str(doc, &acc, "display_name").unwrap_or_default(),
            email: Self::read_str(doc, &acc, "email").unwrap_or_default(),
            imap_host: Self::read_str(doc, &acc, "imap_host").unwrap_or_default(),
            imap_port: Self::read_i64(doc, &acc, "imap_port").unwrap_or(993) as u16,
            imap_tls: Self::read_bool(doc, &acc, "imap_tls").unwrap_or(true),
            smtp_host: Self::read_str(doc, &acc, "smtp_host").unwrap_or_default(),
            smtp_port: Self::read_i64(doc, &acc, "smtp_port").unwrap_or(465) as u16,
            smtp_tls: Self::read_bool(doc, &acc, "smtp_tls").unwrap_or(true),
            username: Self::read_str(doc, &acc, "username").unwrap_or_default(),
            auth: Self::read_str(doc, &acc, "auth")
                .map(|s| crate::model::AuthMethod::from_key(&s))
                .unwrap_or_default(),
            password: String::new(),
        })
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
    }

    pub fn load_draft(doc: &AutoCommit) -> Draft {
        let d = match doc.get(&ROOT, "draft") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Draft::default(),
        };
        Draft {
            to: Self::read_str(doc, &d, "to").unwrap_or_default(),
            cc: Self::read_str(doc, &d, "cc").unwrap_or_default(),
            bcc: Self::read_str(doc, &d, "bcc").unwrap_or_default(),
            subject: Self::read_str(doc, &d, "subject").unwrap_or_default(),
            body: Self::read_str(doc, &d, "body").unwrap_or_default(),
            html_body: Self::read_str(doc, &d, "html_body"),
            attachments: Vec::new(),
            in_reply_to: None,
        }
    }

    /// Clear the persisted draft so a later open no longer re-materialises it.
    /// Backs [`crate::actions::MailAction::DiscardDraft`].
    pub fn clear_draft(doc: &mut AutoCommit) {
        let d = Self::map(doc, &ROOT, "draft");
        for key in ["to", "cc", "bcc", "subject", "body", "html_body"] {
            if doc.get(&d, key).ok().flatten().is_some() {
                let _ = doc.delete(&d, key);
            }
        }
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
        let flags = match doc.get(&ROOT, "flags") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return None,
        };
        match doc.get(&flags, id) {
            Ok(Some((Value::Object(ObjType::Map), e))) => Some(e),
            _ => None,
        }
    }

    /// The stored read flag for a message, or `None` if unset.
    pub fn get_read(doc: &AutoCommit, id: &str) -> Option<bool> {
        let entry = Self::flag_entry_ref(doc, id)?;
        Self::read_bool(doc, &entry, "read")
    }

    /// The stored starred flag for a message, or `None` if unset.
    pub fn get_starred(doc: &AutoCommit, id: &str) -> Option<bool> {
        let entry = Self::flag_entry_ref(doc, id)?;
        Self::read_bool(doc, &entry, "starred")
    }

    /// The stored folder for a message, or `None` if unset / an unknown key.
    pub fn get_folder_for(doc: &AutoCommit, id: &str) -> Option<Folder> {
        let entry = Self::flag_entry_ref(doc, id)?;
        Self::read_str(doc, &entry, "folder").and_then(|s| folder_from_key(&s))
    }

    // ---- per-message labels/tags (`flags/<id>/labels`) ----

    /// The `flags/<id>/labels` list for a message id, created if absent.
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
        let flags = match doc.get(&ROOT, "flags") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let entry = match doc.get(&flags, id) {
            Ok(Some((Value::Object(ObjType::Map), e))) => e,
            _ => return Vec::new(),
        };
        let list = match doc.get(&entry, "labels") {
            Ok(Some((Value::Object(ObjType::List), l))) => l,
            _ => return Vec::new(),
        };
        read_str_list(doc, &list)
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
        let folders = match doc.get(&ROOT, "folders") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let mut out: Vec<(String, String)> = doc
            .keys(&folders)
            .filter_map(|k| Self::read_str(doc, &folders, &k).map(|name| (k, name)))
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
        if let Ok(Some((Value::Scalar(v), _))) = doc.get(list, i) {
            if let ScalarValue::Str(s) = v.as_ref() {
                out.push(s.to_string());
            }
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
    fn folder_round_trips() {
        let mut doc = AutoCommit::new();
        MailDoc::set_folder(&mut doc, Folder::Archive);
        assert_eq!(MailDoc::get_folder(&doc), Folder::Archive);
    }

    #[test]
    fn mode_and_search_round_trip() {
        let mut doc = AutoCommit::new();
        assert_eq!(MailDoc::get_mode(&doc), "mailbox");
        MailDoc::set_mode(&mut doc, "compose");
        assert_eq!(MailDoc::get_mode(&doc), "compose");
        MailDoc::set_search(&mut doc, "launch");
        assert_eq!(MailDoc::get_search(&doc), "launch");
    }

    #[test]
    fn flags_persist_and_overlay() {
        let mut doc = AutoCommit::new();
        let mut mb = sample_mailbox();
        let id = *mb.messages.keys().next().unwrap();
        mb.mark_read(id, true);
        mb.toggle_star(id);
        let starred = mb.get(id).unwrap().starred;
        MailDoc::store_flags(&mut doc, mb.get(id).unwrap());

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
        MailDoc::store_flags(&mut doc, mb.get(id).unwrap());
        MailDoc::apply_flags(&doc, &mut mb3);
        assert_eq!(mb3.get(id).unwrap().starred, starred);
        assert!(mb3.get(id).unwrap().read);
    }

    #[test]
    fn seeded_marker_round_trips() {
        let mut doc = AutoCommit::new();
        assert!(!MailDoc::is_seeded(&doc), "fresh doc is not seeded");
        MailDoc::set_seeded(&mut doc);
        assert!(MailDoc::is_seeded(&doc), "set_seeded persists");
    }

    #[test]
    fn active_account_and_compose_format_round_trip() {
        let mut doc = AutoCommit::new();
        assert_eq!(MailDoc::get_active_account(&doc), None);
        MailDoc::set_active_account(&mut doc, 2);
        assert_eq!(MailDoc::get_active_account(&doc), Some(2));
        MailDoc::set_compose_format(&mut doc, "text");
        assert_eq!(MailDoc::get_compose_format(&doc).as_deref(), Some("text"));
    }

    #[test]
    fn account_round_trips() {
        let mut doc = AutoCommit::new();
        let cfg = AccountConfig {
            email: "a@b.example".into(),
            imap_host: "imap.b.example".into(),
            imap_port: 993,
            smtp_host: "smtp.b.example".into(),
            smtp_port: 587,
            username: "a@b.example".into(),
            ..Default::default()
        };
        MailDoc::store_account(&mut doc, &cfg);
        let loaded = MailDoc::load_account(&doc).unwrap();
        assert_eq!(loaded.email, "a@b.example");
        assert_eq!(loaded.smtp_port, 587);
        assert!(loaded.imap_tls);
    }

    #[test]
    fn draft_round_trips() {
        let mut doc = AutoCommit::new();
        let draft = Draft {
            to: "x@y.example".into(),
            subject: "Hi".into(),
            body: "Body".into(),
            ..Default::default()
        };
        MailDoc::store_draft(&mut doc, &draft);
        let loaded = MailDoc::load_draft(&doc);
        assert_eq!(loaded.to, "x@y.example");
        assert_eq!(loaded.subject, "Hi");
    }

    #[test]
    fn repeated_store_account_keeps_doc_bounded() {
        // The account form is mirrored into the CRDT every frame; with the
        // change-guarded writes, re-storing an unchanged account is a no-op.
        let mut doc = AutoCommit::new();
        let cfg = AccountConfig {
            email: "a@b.example".into(),
            imap_host: "imap.b.example".into(),
            imap_port: 993,
            smtp_host: "smtp.b.example".into(),
            smtp_port: 587,
            username: "a@b.example".into(),
            ..Default::default()
        };
        MailDoc::store_account(&mut doc, &cfg);
        let a = doc.save().len();
        for _ in 0..50 {
            MailDoc::store_account(&mut doc, &cfg);
        }
        let b = doc.save().len();
        assert_eq!(a, b, "re-storing an unchanged account grew the document {a} -> {b}");
    }

    #[test]
    fn labels_add_remove_and_round_trip() {
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

    #[test]
    fn local_folder_registry_crud() {
        let mut doc = AutoCommit::new();
        assert!(MailDoc::list_folders(&doc).is_empty());
        MailDoc::create_folder(&mut doc, "f-a", "Receipts");
        MailDoc::create_folder(&mut doc, "f-b", "Travel");
        assert_eq!(
            MailDoc::list_folders(&doc),
            vec![("f-a".to_string(), "Receipts".to_string()), ("f-b".to_string(), "Travel".to_string())]
        );
        MailDoc::rename_folder(&mut doc, "f-a", "Invoices");
        // Renaming an unknown id does nothing.
        MailDoc::rename_folder(&mut doc, "ghost", "X");
        MailDoc::delete_folder(&mut doc, "f-b");
        assert_eq!(MailDoc::list_folders(&doc), vec![("f-a".to_string(), "Invoices".to_string())]);
    }

    #[test]
    fn granular_flag_setters_and_clear_draft() {
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

        let draft = Draft { to: "x@y.example".into(), subject: "Hi".into(), ..Default::default() };
        MailDoc::store_draft(&mut doc, &draft);
        assert!(!MailDoc::load_draft(&doc).to.is_empty());
        MailDoc::clear_draft(&mut doc);
        assert!(MailDoc::load_draft(&doc).to.is_empty(), "discard clears the persisted draft");
    }

    #[test]
    fn repeated_store_draft_keeps_doc_bounded() {
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
        assert_eq!(a, b, "re-storing an unchanged draft grew the document {a} -> {b}");
    }
}
