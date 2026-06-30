//! Automerge projection of the snippet library.
//!
//! The library lives as a live CRDT structure so two peers can edit the
//! same set of snippets concurrently and converge:
//!
//! ```text
//! root
//! └── "snippets" (Map, keyed by SnippetId string)
//!     └── <snippet-id> (Map)
//!         ├── "title"       (Text)  # collaborative title
//!         ├── "language"    (Str)   # language slug
//!         ├── "folder"      (Str)
//!         ├── "description" (Text)
//!         ├── "body"        (Text)  # the snippet body (placeholders live here)
//!         ├── "favorite"    (Bool)
//!         ├── "uses"        (Int)
//!         └── "tags"        (List of Str)
//! ```
//!
//! Each function takes a borrowed `&mut AutoCommit` — the app's shared
//! `State::doc`. "Which snippet is selected" and "which folder is open"
//! are deliberately NOT in the document: they're per-peer view state, so
//! two collaborators can browse different snippets while sharing one
//! library.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};

use crate::model::{Language, Snippet, SnippetId};

/// Stateless namespace for the library document helpers.
pub struct SnippetsDoc;

impl SnippetsDoc {
    /// Get-or-create the top-level `snippets` Map.
    pub fn snippets_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "snippets") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "snippets", ObjType::Map)
                .expect("put_object snippets on root never fails"),
        }
    }

    /// Get-or-create the per-snippet Map for `id`.
    fn ensure_snippet(doc: &mut AutoCommit, id: SnippetId) -> ObjId {
        let map = Self::snippets_map(doc);
        let key = id.to_string();
        match doc.get(&map, key.as_str()) {
            Ok(Some((Value::Object(ObjType::Map), sid))) => sid,
            _ => doc
                .put_object(&map, key.as_str(), ObjType::Map)
                .expect("put_object snippet map never fails"),
        }
    }

    /// Re-splice a `Text` field only when its content actually changed —
    /// avoids op churn on idempotent upserts.
    fn put_text(doc: &mut AutoCommit, parent: &ObjId, key: &str, value: &str) {
        let text_id = match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => doc
                .put_object(parent, key, ObjType::Text)
                .expect("put_object text never fails"),
        };
        let cur = doc.text(&text_id).unwrap_or_default();
        if cur != value {
            let cur_chars = cur.chars().count() as isize;
            let _ = doc.splice_text(&text_id, 0, cur_chars, value);
        }
    }

    fn read_text(doc: &AutoCommit, parent: &ObjId, key: &str) -> String {
        match doc.get(parent, key) {
            Ok(Some((Value::Object(ObjType::Text), id))) => doc.text(&id).unwrap_or_default(),
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// Insert or update a whole snippet. Title / description / body are
    /// stored as `Text` so concurrent edits merge; scalars are plain
    /// puts; tags are rewritten as a fresh list when they differ.
    pub fn upsert(doc: &mut AutoCommit, s: &Snippet) {
        let snip = Self::ensure_snippet(doc, s.id);
        Self::put_text(doc, &snip, "title", &s.title);
        Self::put_text(doc, &snip, "description", &s.description);
        Self::put_text(doc, &snip, "body", &s.body);
        let _ = doc.put(&snip, "language", s.language.slug());
        let _ = doc.put(&snip, "folder", s.folder.as_str());
        let _ = doc.put(&snip, "favorite", s.favorite);
        let _ = doc.put(&snip, "uses", s.uses as i64);

        // Tags list: only rewrite when the set differs to avoid churn.
        let cur_tags = Self::read_tags(doc, &snip);
        if cur_tags != s.tags {
            let list = match doc.get(&snip, "tags") {
                Ok(Some((Value::Object(ObjType::List), id))) => id,
                _ => doc
                    .put_object(&snip, "tags", ObjType::List)
                    .expect("put_object tags never fails"),
            };
            let len = doc.length(&list);
            for _ in 0..len {
                let _ = doc.delete(&list, 0);
            }
            for (i, t) in s.tags.iter().enumerate() {
                let _ = doc.insert(&list, i, t.as_str());
            }
        }
    }

    fn read_tags(doc: &AutoCommit, snip: &ObjId) -> Vec<String> {
        let list = match doc.get(snip, "tags") {
            Ok(Some((Value::Object(ObjType::List), id))) => id,
            _ => return Vec::new(),
        };
        let len = doc.length(&list);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i) {
                if let ScalarValue::Str(s) = v.as_ref() {
                    out.push(s.to_string());
                }
            }
        }
        out
    }

    fn read_bool(doc: &AutoCommit, parent: &ObjId, key: &str) -> bool {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(v), _))) => matches!(v.as_ref(), ScalarValue::Boolean(true)),
            _ => false,
        }
    }

    fn read_int(doc: &AutoCommit, parent: &ObjId, key: &str) -> u32 {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(v), _))) => v.to_i64().unwrap_or(0).max(0) as u32,
            _ => 0,
        }
    }

    /// Remove a snippet from the document.
    pub fn remove(doc: &mut AutoCommit, id: SnippetId) {
        let map = Self::snippets_map(doc);
        let key = id.to_string();
        let _ = doc.delete(&map, key.as_str());
    }

    /// Read every snippet out of the document. Entries whose id key
    /// doesn't parse are skipped.
    pub fn list(doc: &AutoCommit) -> Vec<Snippet> {
        let map = match doc.get(&ROOT, "snippets") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for key in doc.keys(&map) {
            let Ok(id) = key.parse::<SnippetId>() else { continue };
            let Ok(Some((Value::Object(ObjType::Map), snip))) = doc.get(&map, key.as_str()) else {
                continue;
            };
            let language = Language::from_slug(&Self::read_str(doc, &snip, "language"));
            out.push(Snippet {
                id,
                title: Self::read_text(doc, &snip, "title"),
                language,
                folder: Self::read_str(doc, &snip, "folder"),
                tags: Self::read_tags(doc, &snip),
                body: Self::read_text(doc, &snip, "body"),
                description: Self::read_text(doc, &snip, "description"),
                favorite: Self::read_bool(doc, &snip, "favorite"),
                uses: Self::read_int(doc, &snip, "uses"),
            });
        }
        out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
        out
    }

    fn read_str(doc: &AutoCommit, parent: &ObjId, key: &str) -> String {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(v), _))) => match v.as_ref() {
                ScalarValue::Str(s) => s.to_string(),
                _ => String::new(),
            },
            _ => String::new(),
        }
    }

    /// True if the document has no snippets yet — used by the app to
    /// decide whether to seed the starter library or adopt a peer's.
    pub fn is_empty(doc: &AutoCommit) -> bool {
        match doc.get(&ROOT, "snippets") {
            Ok(Some((Value::Object(ObjType::Map), id))) => doc.keys(&id).next().is_none(),
            _ => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Language;

    fn sample() -> Snippet {
        Snippet {
            title: "Deploy".into(),
            language: Language::Shell,
            folder: "Ops".into(),
            tags: vec!["k8s".into(), "deploy".into()],
            body: "kubectl rollout restart deployment/${name}".into(),
            description: "Restart a deployment".into(),
            favorite: true,
            uses: 3,
            ..Snippet::blank("Ops")
        }
    }

    #[test]
    fn round_trips_all_fields() {
        let mut doc = AutoCommit::new();
        let s = sample();
        SnippetsDoc::upsert(&mut doc, &s);
        let got = SnippetsDoc::list(&doc);
        assert_eq!(got.len(), 1);
        let g = &got[0];
        assert_eq!(g.id, s.id);
        assert_eq!(g.title, "Deploy");
        assert_eq!(g.language, Language::Shell);
        assert_eq!(g.folder, "Ops");
        assert_eq!(g.tags, vec!["k8s", "deploy"]);
        assert_eq!(g.body, s.body);
        assert_eq!(g.description, "Restart a deployment");
        assert!(g.favorite);
        assert_eq!(g.uses, 3);
    }

    #[test]
    fn upsert_is_idempotent() {
        let mut doc = AutoCommit::new();
        let s = sample();
        SnippetsDoc::upsert(&mut doc, &s);
        SnippetsDoc::upsert(&mut doc, &s);
        assert_eq!(SnippetsDoc::list(&doc).len(), 1, "second upsert must not duplicate");
    }

    #[test]
    fn tags_update_replaces_set() {
        let mut doc = AutoCommit::new();
        let mut s = sample();
        SnippetsDoc::upsert(&mut doc, &s);
        s.tags = vec!["only".into()];
        SnippetsDoc::upsert(&mut doc, &s);
        assert_eq!(SnippetsDoc::list(&doc)[0].tags, vec!["only"]);
    }

    #[test]
    fn remove_deletes_snippet() {
        let mut doc = AutoCommit::new();
        let s = sample();
        SnippetsDoc::upsert(&mut doc, &s);
        SnippetsDoc::remove(&mut doc, s.id);
        assert!(SnippetsDoc::list(&doc).is_empty());
        assert!(SnippetsDoc::is_empty(&doc));
    }

    #[test]
    fn two_docs_merge_snippets() {
        let mut a = AutoCommit::new();
        SnippetsDoc::upsert(&mut a, &sample());

        // B joins from A's state, adds its own snippet.
        let mut b = AutoCommit::load(&a.save()).unwrap();
        let mut s2 = Snippet::blank("Git");
        s2.title = "Amend".into();
        SnippetsDoc::upsert(&mut b, &s2);

        let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
        a.merge(&mut b_loaded).unwrap();
        let titles: Vec<_> = SnippetsDoc::list(&a).into_iter().map(|s| s.title).collect();
        assert!(titles.contains(&"Deploy".to_string()));
        assert!(titles.contains(&"Amend".to_string()));
    }
}
