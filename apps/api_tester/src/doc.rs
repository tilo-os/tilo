//! Automerge document model for the API workspace.
//!
//! The workspace — collections, environments, the selected environment,
//! the request currently being edited, and the response history — lives in
//! the shared CRDT so two peers can edit the same set of requests and
//! converge, and so that `ui::persist` can carry all of it across a
//! restart. The document shape is:
//!
//! ```text
//! root
//! ├── "url"          (Text)   # the active request's URL (live-collaborative)
//! ├── "body"         (Text)   # the active request's body editor buffer
//! ├── "collections"  (Text)   # JSON snapshot of Vec<Collection>
//! ├── "environments" (Text)   # JSON snapshot of Vec<Environment>
//! ├── "active_env"   (Str)    # selected environment id, or ""
//! ├── "request"      (Text)   # JSON snapshot of the active Request's
//! │                           #   structured fields (method/params/headers/auth/…)
//! └── "history"      (Text)   # JSON snapshot of Vec<HistoryEntry>
//! ```
//!
//! The URL and body are first-class `Text` objects so character-level
//! edits merge between collaborators; the richer structures round-trip as
//! JSON snapshots (the same pragmatic split the notes app uses for its
//! page tree). "Which response I'm viewing" is intentionally not stored —
//! it's local to each peer.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ReadDoc, ScalarValue, Value};

use crate::model::{Collection, Environment, HistoryEntry, Request};

/// Stateless namespace for the workspace document helpers.
pub struct ApiDoc;

impl ApiDoc {
    /// Get-or-create a top-level `Text` object at `key`.
    fn text_obj(doc: &mut AutoCommit, root: &ObjId, key: &str) -> ObjId {
        match doc.get(root, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            // `put_object` on a fresh key of a live document does not fail in
            // practice; falling back to `root` itself (rather than panicking)
            // keeps a write against a genuinely corrupt document non-fatal.
            _ => doc
                .put_object(root, key, ObjType::Text)
                .unwrap_or_else(|_| root.clone()),
        }
    }

    /// Read a top-level `Text` object's contents (empty if absent).
    pub fn read_text(doc: &AutoCommit, root: &ObjId, key: &str) -> String {
        match doc.get(root, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Splice `value` into the `Text` object at `key`, but only when it
    /// actually differs — avoids churn that would defeat incremental sync.
    pub fn set_text(doc: &mut AutoCommit, root: &ObjId, key: &str, value: &str) {
        let obj = Self::text_obj(doc, root, key);
        ui::doc::splice_text_diffed(doc, &obj, value);
    }

    /// Get-or-create the live `Text` object backing the active request's
    /// URL — returned so the URL bar's `TextInput` can bind to it.
    pub fn url_text(doc: &mut AutoCommit, root: &ObjId) -> ObjId {
        Self::text_obj(doc, root, "url")
    }

    /// Get-or-create the live `Text` object backing the body editor.
    pub fn body_text(doc: &mut AutoCommit, root: &ObjId) -> ObjId {
        Self::text_obj(doc, root, "body")
    }

    /// Persist the collection list as a JSON snapshot.
    pub fn set_collections(doc: &mut AutoCommit, root: &ObjId, collections: &[Collection]) {
        if let Ok(json) = serde_json::to_string(collections) {
            Self::set_text(doc, root, "collections", &json);
        }
    }

    /// Read the collection list back (empty on missing / malformed JSON).
    pub fn collections(doc: &AutoCommit, root: &ObjId) -> Vec<Collection> {
        let json = Self::read_text(doc, root, "collections");
        serde_json::from_str(&json).unwrap_or_default()
    }

    pub fn set_environments(doc: &mut AutoCommit, root: &ObjId, envs: &[Environment]) {
        if let Ok(json) = serde_json::to_string(envs) {
            Self::set_text(doc, root, "environments", &json);
        }
    }

    pub fn environments(doc: &AutoCommit, root: &ObjId) -> Vec<Environment> {
        let json = Self::read_text(doc, root, "environments");
        serde_json::from_str(&json).unwrap_or_default()
    }

    pub fn set_active_env(doc: &mut AutoCommit, root: &ObjId, id: Option<&str>) {
        let _ = doc.put(root, "active_env", id.unwrap_or("").to_string());
    }

    pub fn active_env(doc: &AutoCommit, root: &ObjId) -> Option<String> {
        match doc.get(root, "active_env") {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) if !s.is_empty() => Some(s.to_string()),
                _ => None,
            },
            _ => None,
        }
    }

    /// Persist the active request's structured fields (everything except
    /// the URL and body buffer, which are live `Text` objects).
    pub fn set_request(doc: &mut AutoCommit, root: &ObjId, req: &Request) {
        if let Ok(json) = serde_json::to_string(req) {
            Self::set_text(doc, root, "request", &json);
        }
    }

    /// Read the active request snapshot, if present.
    pub fn request(doc: &AutoCommit, root: &ObjId) -> Option<Request> {
        let json = Self::read_text(doc, root, "request");
        if json.is_empty() {
            None
        } else {
            serde_json::from_str(&json).ok()
        }
    }

    pub fn set_history(doc: &mut AutoCommit, root: &ObjId, history: &[HistoryEntry]) {
        if let Ok(json) = serde_json::to_string(history) {
            Self::set_text(doc, root, "history", &json);
        }
    }

    pub fn history(doc: &AutoCommit, root: &ObjId) -> Vec<HistoryEntry> {
        let json = Self::read_text(doc, root, "history");
        serde_json::from_str(&json).unwrap_or_default()
    }

    /// Whether the document has been seeded with any workspace content yet.
    pub fn is_empty(doc: &AutoCommit, root: &ObjId) -> bool {
        Self::read_text(doc, root, "collections").is_empty()
            && Self::read_text(doc, root, "request").is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HttpMethod, KeyValue};

    fn doc() -> (AutoCommit, ObjId) {
        (AutoCommit::new(), automerge::ROOT)
    }

    #[test]
    fn collections_round_trip() {
        let (mut d, root) = doc();
        let mut c = Collection::new("Sample");
        c.requests.push(Request::new("List users"));
        ApiDoc::set_collections(&mut d, &root, &[c.clone()]);
        let back = ApiDoc::collections(&d, &root);
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].name, "Sample");
        assert_eq!(back[0].requests[0].name, "List users");
    }

    #[test]
    fn environments_and_active_round_trip() {
        let (mut d, root) = doc();
        let mut e = Environment::new("Production");
        e.variables
            .push(KeyValue::new("base_url", "https://api.example.com"));
        ApiDoc::set_environments(&mut d, &root, &[e.clone()]);
        ApiDoc::set_active_env(&mut d, &root, Some(&e.id.to_string()));
        assert_eq!(ApiDoc::environments(&d, &root)[0].name, "Production");
        assert_eq!(ApiDoc::active_env(&d, &root), Some(e.id.to_string()));
    }

    #[test]
    fn request_snapshot_round_trips() {
        let (mut d, root) = doc();
        let mut r = Request::new("Create user");
        r.method = HttpMethod::Post;
        r.headers.push(KeyValue::new("Accept", "application/json"));
        ApiDoc::set_request(&mut d, &root, &r);
        let back = ApiDoc::request(&d, &root).unwrap();
        assert_eq!(back.method, HttpMethod::Post);
        assert_eq!(back.headers[0].key, "Accept");
    }

    #[test]
    fn url_text_is_live_and_editable() {
        let (mut d, root) = doc();
        let obj = ApiDoc::url_text(&mut d, &root);
        let _ = d.splice_text(&obj, 0, 0, "https://api.example.com/users");
        assert_eq!(
            ApiDoc::read_text(&d, &root, "url"),
            "https://api.example.com/users"
        );
    }

    #[test]
    fn set_text_is_idempotent() {
        let (mut d, root) = doc();
        ApiDoc::set_text(&mut d, &root, "request", "{}");
        let heads_before = d.get_heads();
        ApiDoc::set_text(&mut d, &root, "request", "{}");
        // No-op write must not advance the document.
        assert_eq!(heads_before, d.get_heads());
    }

    #[test]
    fn appending_one_char_is_a_bounded_splice() {
        // Anti-churn: growing a long Text field by one char must splice only
        // the new char, not rewrite the whole value.
        let (mut d, root) = doc();
        let long = "x".repeat(300);
        ApiDoc::set_text(&mut d, &root, "body", &long);
        let before = d.save().len();
        let mut longer = long.clone();
        longer.push('!');
        ApiDoc::set_text(&mut d, &root, "body", &longer);
        let grew = d.save().len() - before;
        assert_eq!(
            ApiDoc::read_text(&d, &root, "body"),
            longer,
            "value round-trips"
        );
        assert!(
            grew < 40,
            "appending one char grew the doc by {grew} bytes (expected a single-char splice)"
        );
    }

    #[test]
    fn repeated_set_text_keeps_doc_bounded() {
        // Idempotent store: re-writing the same text must emit no ops, so the
        // serialized document does not grow at all.
        let (mut d, root) = doc();
        let long = "y".repeat(300);
        ApiDoc::set_text(&mut d, &root, "body", &long);
        let a = d.save().len();
        for _ in 0..50 {
            ApiDoc::set_text(&mut d, &root, "body", &long);
        }
        let b = d.save().len();
        assert_eq!(
            a, b,
            "re-storing the same text grew the document {a} -> {b}"
        );
    }

    #[test]
    fn two_docs_merge_collections() {
        let (mut a, root) = doc();
        ApiDoc::set_collections(&mut a, &root, &[Collection::new("A")]);
        let mut b = AutoCommit::load(&a.save()).unwrap();
        ApiDoc::set_environments(&mut b, &root, &[Environment::new("Env-B")]);
        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        assert_eq!(ApiDoc::collections(&a, &root)[0].name, "A");
        assert_eq!(ApiDoc::environments(&a, &root)[0].name, "Env-B");
    }
}
