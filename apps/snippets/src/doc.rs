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
use ui::doc::{put_if_changed, put_text_diffed, read_bool, read_str, read_str_or_text};

use crate::model::{Language, Snippet, SnippetId};

/// Stateless namespace for the library document helpers.
pub struct SnippetsDoc;

impl SnippetsDoc {
    /// Get-or-create the top-level `snippets` Map.
    ///
    /// # Panics
    /// Never in practice: `ROOT` is permanent for the life of the document,
    /// so `put_object` on it only fails for an already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "parent is ROOT, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write the library into the wrong object"
    )]
    pub fn snippets_map(doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "snippets") {
            Ok(Some((Value::Object(ObjType::Map), id))) => id,
            _ => doc
                .put_object(&ROOT, "snippets", ObjType::Map)
                .expect("put_object snippets on root never fails"),
        }
    }

    /// Get-or-create the per-snippet Map for `id`.
    #[allow(
        clippy::expect_used,
        reason = "parent is the snippets Map just resolved above, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write this snippet into the wrong object"
    )]
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

    /// Insert or update a whole snippet. Title / description / body are
    /// stored as `Text` so concurrent edits merge; scalars are plain
    /// puts; tags are rewritten as a fresh list when they differ.
    ///
    /// # Panics
    /// Never in practice: every `put_object` call here targets `ROOT` or a
    /// Map this function just resolved, so it only fails for an
    /// already-corrupted document.
    #[allow(
        clippy::expect_used,
        reason = "parent is the snippet Map just resolved above, so put_object cannot fail short of a corrupted document — falling back to another ObjId would silently write the tags into the wrong object"
    )]
    pub fn upsert(doc: &mut AutoCommit, s: &Snippet) {
        let snip = Self::ensure_snippet(doc, s.id);
        // Title / description / body are diff-spliced Text — a one-char edit
        // splices one char, never rewrites (and tombstones) the whole field.
        put_text_diffed(doc, &snip, "title", &s.title);
        put_text_diffed(doc, &snip, "description", &s.description);
        put_text_diffed(doc, &snip, "body", &s.body);
        // Scalars are change-guarded so an idempotent upsert emits no ops.
        put_if_changed(doc, &snip, "language", s.language.slug());
        put_if_changed(doc, &snip, "folder", s.folder.as_str());
        put_if_changed(doc, &snip, "favorite", s.favorite);
        put_if_changed(doc, &snip, "uses", i64::from(s.uses));

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
        let Ok(Some((Value::Object(ObjType::List), list))) = doc.get(snip, "tags") else {
            return Vec::new();
        };
        let len = doc.length(&list);
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            if let Ok(Some((Value::Scalar(v), _))) = doc.get(&list, i)
                && let ScalarValue::Str(s) = v.as_ref()
            {
                out.push(s.to_string());
            }
        }
        out
    }

    fn read_int(doc: &AutoCommit, parent: &ObjId, key: &str) -> u32 {
        match doc.get(parent, key) {
            Ok(Some((Value::Scalar(v), _))) => v.as_i64().unwrap_or(0).max(0) as u32,
            _ => 0,
        }
    }

    /// The text object of snippet `id`'s body, created when the snippet has
    /// none yet; `None` when there is no such snippet.
    pub fn body_text(doc: &mut AutoCommit, id: SnippetId) -> Option<ObjId> {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(&ROOT, "snippets") else {
            return None;
        };
        let key = id.to_string();
        let Ok(Some((Value::Object(ObjType::Map), snip))) = doc.get(&map, key.as_str()) else {
            return None;
        };
        match doc.get(&snip, "body") {
            Ok(Some((Value::Object(ObjType::Text), text))) => Some(text),
            _ => doc.put_object(&snip, "body", ObjType::Text).ok(),
        }
    }

    /// Remove a snippet from the document.
    pub fn remove(doc: &mut AutoCommit, id: SnippetId) {
        let map = Self::snippets_map(doc);
        let key = id.to_string();
        let _ = doc.delete(&map, key.as_str());
    }

    /// Read a single snippet by id, or `None` if the document has no such row.
    /// The action layer uses this to read-modify-write one field on the
    /// authoritative document without touching the rest of the library.
    pub fn get(doc: &AutoCommit, id: SnippetId) -> Option<Snippet> {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(&ROOT, "snippets") else {
            return None;
        };
        let key = id.to_string();
        let Ok(Some((Value::Object(ObjType::Map), snip))) = doc.get(&map, key.as_str()) else {
            return None;
        };
        let language = Language::from_slug(&read_str(doc, &snip, "language").unwrap_or_default());
        Some(Snippet {
            id,
            title: read_str_or_text(doc, &snip, "title").unwrap_or_default(),
            language,
            folder: read_str(doc, &snip, "folder").unwrap_or_default(),
            tags: Self::read_tags(doc, &snip),
            body: read_str_or_text(doc, &snip, "body").unwrap_or_default(),
            description: read_str_or_text(doc, &snip, "description").unwrap_or_default(),
            favorite: read_bool(doc, &snip, "favorite").unwrap_or(false),
            uses: Self::read_int(doc, &snip, "uses"),
        })
    }

    /// Read every snippet out of the document. Entries whose id key
    /// doesn't parse are skipped.
    pub fn list(doc: &AutoCommit) -> Vec<Snippet> {
        let Ok(Some((Value::Object(ObjType::Map), map))) = doc.get(&ROOT, "snippets") else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for key in doc.keys(&map) {
            let Ok(id) = key.parse::<SnippetId>() else {
                continue;
            };
            let Ok(Some((Value::Object(ObjType::Map), snip))) = doc.get(&map, key.as_str()) else {
                continue;
            };
            let language =
                Language::from_slug(&read_str(doc, &snip, "language").unwrap_or_default());
            out.push(Snippet {
                id,
                title: read_str_or_text(doc, &snip, "title").unwrap_or_default(),
                language,
                folder: read_str(doc, &snip, "folder").unwrap_or_default(),
                tags: Self::read_tags(doc, &snip),
                body: read_str_or_text(doc, &snip, "body").unwrap_or_default(),
                description: read_str_or_text(doc, &snip, "description").unwrap_or_default(),
                favorite: read_bool(doc, &snip, "favorite").unwrap_or(false),
                uses: Self::read_int(doc, &snip, "uses"),
            });
        }
        out.sort_by_key(|a| a.title.to_lowercase());
        out
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
    use tilo_automerge::ApplyIncoming;

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

    /// Snippet documents round-trip every field and upserts neither duplicate nor grow the doc.
    #[test]
    fn snippet_doc_round_trips_and_upserts_idempotently() {
        // Case `round_trips_all_fields`.
        {
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

        // Case `upsert_is_idempotent`.
        {
            let mut doc = AutoCommit::new();
            let s = sample();
            SnippetsDoc::upsert(&mut doc, &s);
            SnippetsDoc::upsert(&mut doc, &s);
            assert_eq!(
                SnippetsDoc::list(&doc).len(),
                1,
                "second upsert must not duplicate"
            );
        }

        // Case `tags_update_replaces_set`.
        {
            let mut doc = AutoCommit::new();
            let mut s = sample();
            SnippetsDoc::upsert(&mut doc, &s);
            s.tags = vec!["only".into()];
            SnippetsDoc::upsert(&mut doc, &s);
            assert_eq!(SnippetsDoc::list(&doc)[0].tags, vec!["only"]);
        }

        // Case `idempotent_upsert_does_not_grow_the_doc`.
        {
            // Change-guarded scalars + diff-spliced Text make a re-store of an
            // unchanged snippet a true no-op (the old blind puts grew it forever).
            let mut doc = AutoCommit::new();
            let s = sample();
            SnippetsDoc::upsert(&mut doc, &s);
            let before = doc.save().len();
            for _ in 0..25 {
                SnippetsDoc::upsert(&mut doc, &s);
            }
            assert_eq!(
                doc.save().len(),
                before,
                "idempotent upsert must not grow the document"
            );
        }
    }

    /// Body edits are bounded splices; remove deletes; two docs merge.
    #[test]
    fn snippet_doc_edits_splice_remove_and_merge() {
        // Case `editing_one_body_char_is_a_bounded_splice`.
        {
            // Appending one char to a long snippet body must diff-splice (grow the
            // doc a little), not delete+reinsert the whole body every upsert — the
            // O(len^2) tombstone churn that HIG §6 forbids.
            let mut doc = AutoCommit::new();
            let mut s = sample();
            s.body = "x".repeat(200);
            SnippetsDoc::upsert(&mut doc, &s);
            let before = doc.save().len();
            s.body.push('!');
            SnippetsDoc::upsert(&mut doc, &s);
            let grew = doc.save().len() - before;
            assert_eq!(SnippetsDoc::list(&doc)[0].body, s.body);
            assert!(
                grew < 40,
                "appending one char to a snippet body grew the doc by {grew} bytes (expected a single-char splice)"
            );
        }

        // Case `remove_deletes_snippet`.
        {
            let mut doc = AutoCommit::new();
            let s = sample();
            SnippetsDoc::upsert(&mut doc, &s);
            SnippetsDoc::remove(&mut doc, s.id);
            assert!(SnippetsDoc::list(&doc).is_empty());
            assert!(SnippetsDoc::is_empty(&doc));
        }

        // Case `two_docs_merge_snippets`.
        {
            let mut a = AutoCommit::new();
            SnippetsDoc::upsert(&mut a, &sample());

            // B joins from A's state, adds its own snippet.
            let mut b = AutoCommit::load(&a.save()).unwrap();
            let mut s2 = Snippet::blank("Git");
            s2.title = "Amend".into();
            SnippetsDoc::upsert(&mut b, &s2);

            let mut b_loaded = AutoCommit::load(&b.save()).unwrap();
            a.merge_incoming(&mut b_loaded).unwrap();
            let titles: Vec<_> = SnippetsDoc::list(&a).into_iter().map(|s| s.title).collect();
            assert!(titles.contains(&"Deploy".to_string()));
            assert!(titles.contains(&"Amend".to_string()));
        }
    }
}
