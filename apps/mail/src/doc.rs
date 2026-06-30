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

    pub fn get_selected(doc: &AutoCommit) -> Option<String> {
        match doc.get(&ROOT, "ui") {
            Ok(Some((Value::Object(ObjType::Map), ui))) => {
                Self::read_str(doc, &ui, "selected").filter(|s| !s.is_empty())
            }
            _ => None,
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
        let _ = doc.put(&entry, "read", msg.read);
        let _ = doc.put(&entry, "starred", msg.starred);
        let _ = doc.put(&entry, "folder", folder_key(msg.folder));
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
            }
        }
    }

    // ---- account config ----

    pub fn store_account(doc: &mut AutoCommit, cfg: &AccountConfig) {
        let acc = Self::map(doc, &ROOT, "account");
        let _ = doc.put(&acc, "display_name", cfg.display_name.as_str());
        let _ = doc.put(&acc, "email", cfg.email.as_str());
        let _ = doc.put(&acc, "imap_host", cfg.imap_host.as_str());
        let _ = doc.put(&acc, "imap_port", cfg.imap_port as i64);
        let _ = doc.put(&acc, "imap_tls", cfg.imap_tls);
        let _ = doc.put(&acc, "smtp_host", cfg.smtp_host.as_str());
        let _ = doc.put(&acc, "smtp_port", cfg.smtp_port as i64);
        let _ = doc.put(&acc, "smtp_tls", cfg.smtp_tls);
        let _ = doc.put(&acc, "username", cfg.username.as_str());
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
            password: String::new(),
        })
    }

    // ---- draft ----

    pub fn store_draft(doc: &mut AutoCommit, draft: &Draft) {
        let d = Self::map(doc, &ROOT, "draft");
        let _ = doc.put(&d, "to", draft.to.as_str());
        let _ = doc.put(&d, "cc", draft.cc.as_str());
        let _ = doc.put(&d, "bcc", draft.bcc.as_str());
        let _ = doc.put(&d, "subject", draft.subject.as_str());
        let _ = doc.put(&d, "body", draft.body.as_str());
        if let Some(html) = &draft.html_body {
            let _ = doc.put(&d, "html_body", html.as_str());
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
}

fn folder_key(f: Folder) -> &'static str {
    match f {
        Folder::Inbox => "inbox",
        Folder::Starred => "starred",
        Folder::Sent => "sent",
        Folder::Drafts => "drafts",
        Folder::Archive => "archive",
        Folder::Trash => "trash",
        Folder::Junk => "junk",
    }
}

fn folder_from_key(s: &str) -> Option<Folder> {
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
}
