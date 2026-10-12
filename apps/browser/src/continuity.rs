//! Bounded migration values applied to Browser's real library and private person record.
//! Parsing grants and authenticated installer delivery are separate from these actions.
use automerge::{ROOT, ReadDoc};
use serde::{Deserialize, Serialize};
use tilo_actions::{ActionError, Doc};

pub const MAX_IMPORT_ENTRIES: usize = 10_000;
pub const MAX_IMPORT_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_BUNDLE_ENTRIES: usize = 100_000;
pub const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BookmarkImport {
    pub url: String,
    pub title: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HistoryImport {
    pub url: String,
    pub title: String,
    pub last_visit: u64,
    pub visit_count: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBatch {
    /// Fingerprint of the reviewed source snapshot, never an ambient filesystem path.
    pub source: [u8; 32],
    pub bookmarks: Vec<BookmarkImport>,
    pub history: Vec<HistoryImport>,
}

/// Complete bounded reviewed snapshot; chunk identities bind the source, ordinal and content.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportBundle {
    pub source: [u8; 32],
    pub chunks: Vec<ImportBatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_tree: Option<crate::imported_folders::ImportedTree>,
}
impl ImportBundle {
    /// Decode a complete bounded action payload.
    /// # Errors
    /// Refuses oversized, malformed or incorrectly bound chunks.
    pub fn decode(value: &str) -> Result<Self, ActionError> {
        if value.len() > MAX_BUNDLE_BYTES {
            return Err(refused());
        }
        let bundle: Self = serde_json::from_str(value).map_err(|_| refused())?;
        bundle.validate()?;
        Ok(bundle)
    }
    /// Partition a complete normalized capture without discarding rows.
    /// # Errors
    /// Refuses sources exceeding bounded custody or invalid chunk metadata.
    pub fn from_batch(mut batch: ImportBatch) -> Result<Self, ActionError> {
        if batch.bookmarks.len().saturating_add(batch.history.len()) > MAX_BUNDLE_ENTRIES {
            return Err(refused());
        }
        let source = batch.source;
        let mut chunks = Vec::new();
        while !batch.bookmarks.is_empty() || !batch.history.is_empty() {
            let bookmarks: Vec<_> = batch
                .bookmarks
                .drain(..batch.bookmarks.len().min(500))
                .collect();
            let history: Vec<_> = batch
                .history
                .drain(..batch.history.len().min(500))
                .collect();
            let mut chunk = ImportBatch {
                source,
                bookmarks,
                history,
            };
            chunk.validate()?;
            chunk.source = chunk_binding(&source, chunks.len(), &chunk);
            chunks.push(chunk);
        }
        let bundle = Self {
            source,
            chunks,
            imported_tree: None,
        };
        bundle.validate()?;
        Ok(bundle)
    }
    /// # Errors
    /// Refuses malformed or oversized bundles before a record is changed.
    pub fn validate(&self) -> Result<(), ActionError> {
        if let Some(tree) = &self.imported_tree {
            tree.validate()?;
            if tree.source != self.source {
                return Err(refused());
            }
        }
        if serde_json::to_vec(self).map_err(|_| refused())?.len() > MAX_BUNDLE_BYTES {
            return Err(refused());
        }
        if self.chunks.len() > 200 {
            return Err(refused());
        }
        let mut bytes = 0usize;
        let mut rows = 0usize;
        let mut ids = std::collections::BTreeSet::new();
        let mut bookmarks = std::collections::BTreeSet::new();
        let mut history = std::collections::BTreeSet::new();
        for (ordinal, chunk) in self.chunks.iter().enumerate() {
            chunk.validate()?;
            if chunk.source != chunk_binding(&self.source, ordinal, chunk) {
                return Err(refused());
            }
            if !ids.insert(chunk.source) {
                return Err(refused());
            }
            for row in &chunk.bookmarks {
                if !bookmarks.insert(&row.url) {
                    return Err(refused());
                }
                bytes = bytes
                    .saturating_add(row.url.len())
                    .saturating_add(row.title.len());
            }
            for row in &chunk.history {
                if !history.insert(&row.url) {
                    return Err(refused());
                }
                bytes = bytes
                    .saturating_add(row.url.len())
                    .saturating_add(row.title.len());
            }
            rows = rows
                .saturating_add(chunk.bookmarks.len())
                .saturating_add(chunk.history.len());
        }
        if bytes > MAX_BUNDLE_BYTES || rows > MAX_BUNDLE_ENTRIES {
            return Err(refused());
        }
        Ok(())
    }
    /// The bookmark-only projection for the private personal library.
    pub fn bookmark_action(&self) -> crate::continuity_bookmarks::PersonalLibraryEdit {
        let mut bundle = self.clone();
        for (ordinal, chunk) in bundle.chunks.iter_mut().enumerate() {
            chunk.history.clear();
            chunk.source = chunk_binding(&bundle.source, ordinal, chunk);
        }
        crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarkBundle { bundle }
    }
    /// Visits enter only the current person's private resource.
    pub fn history_bundle(&self) -> ImportBundle {
        let mut bundle = self.clone();
        bundle.imported_tree = None;
        for (ordinal, chunk) in bundle.chunks.iter_mut().enumerate() {
            chunk.bookmarks.clear();
            chunk.source = chunk_binding(&bundle.source, ordinal, chunk);
        }
        bundle
    }
}

impl ImportBundle {
    pub fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError> {
        let selected = self.history_bundle();
        selected.validate()?;
        let encoded = serde_json::to_string(&selected).map_err(|_| refused())?;
        Ok(crate::history::BrowsingHistory::IMPORT_HISTORY_BUNDLE.with(encoded))
    }
}

fn chunk_binding(source: &[u8; 32], ordinal: usize, chunk: &ImportBatch) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"tilo-browser-complete-chunk-v1");
    hash.update(source);
    hash.update(&(ordinal as u64).to_le_bytes());
    hash.update(&(chunk.bookmarks.len() as u64).to_le_bytes());
    let field = |hash: &mut blake3::Hasher, text: &str| {
        hash.update(&(text.len() as u64).to_le_bytes());
        hash.update(text.as_bytes());
    };
    for row in &chunk.bookmarks {
        field(&mut hash, &row.url);
        field(&mut hash, &row.title);
    }
    hash.update(&(chunk.history.len() as u64).to_le_bytes());
    for row in &chunk.history {
        field(&mut hash, &row.url);
        field(&mut hash, &row.title);
        hash.update(&row.last_visit.to_le_bytes());
        hash.update(&row.visit_count.to_le_bytes());
    }
    *hash.finalize().as_bytes()
}

/// Validate every chunk conflict and aggregate capacity before any record write.
pub(crate) fn preflight_bundle(
    doc: &Doc,
    namespace: &str,
    bundle: &ImportBundle,
    current: usize,
) -> Result<(), ActionError> {
    bundle.validate()?;
    if namespace == "history" && bundle.imported_tree.is_some() {
        return Err(refused());
    }
    let mut additions = 0usize;
    let mut receipts = 0usize;
    for chunk in &bundle.chunks {
        if (namespace == "history" && !chunk.bookmarks.is_empty())
            || (namespace == "bookmarks" && !chunk.history.is_empty())
        {
            return Err(refused());
        }
        if receipt(doc, namespace, chunk)?.is_some() {
            receipts = receipts.saturating_add(1);
            additions = additions.saturating_add(if namespace == "history" {
                chunk.history.len()
            } else {
                chunk.bookmarks.len()
            });
        }
    }
    let prefix = format!("browser.import.{namespace}:");
    let existing = doc.keys(ROOT).filter(|k| k.starts_with(&prefix)).count();
    if current.saturating_add(additions) > MAX_BUNDLE_ENTRIES
        || existing.saturating_add(receipts) > 1024
    {
        return Err(refused());
    }
    Ok(())
}
impl ImportBatch {
    /// Decode an action payload before allocating unbounded imported metadata.
    /// # Errors
    /// Refuses malformed, oversized or invalid data.
    pub fn decode(value: &str) -> Result<Self, ActionError> {
        if value.len() > MAX_IMPORT_BYTES {
            return Err(refused());
        }
        let batch: Self = serde_json::from_str(value).map_err(|_| refused())?;
        batch.validate()?;
        Ok(batch)
    }
    /// Produce the shared-library action without including private visit data.
    pub fn bookmark_action(&self) -> crate::continuity_bookmarks::PersonalLibraryEdit {
        crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarks {
            batch: Self {
                source: self.source,
                bookmarks: self.bookmarks.clone(),
                history: Vec::new(),
            },
        }
    }
    /// Produce the action that belongs only to the current person’s history record.
    pub fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError> {
        let mut selected = self.clone();
        selected.bookmarks.clear();
        ImportBundle::from_batch(selected)?.history_action()
    }

    pub fn validate(&self) -> Result<(), ActionError> {
        if self.bookmarks.len() > MAX_IMPORT_ENTRIES || self.history.len() > MAX_IMPORT_ENTRIES {
            return Err(refused());
        }
        let mut bytes = 0usize;
        for rows in [
            self.bookmarks
                .iter()
                .map(|r| (&r.url, &r.title))
                .collect::<Vec<_>>(),
            self.history
                .iter()
                .map(|r| (&r.url, &r.title))
                .collect::<Vec<_>>(),
        ] {
            let mut seen = std::collections::BTreeSet::new();
            for (url, title) in rows {
                let normalized =
                    crate::actions::normalized_bookmark_url(url).ok_or_else(refused)?;
                if normalized != *url || title.len() > 4096 || !seen.insert(normalized) {
                    return Err(refused());
                }
                bytes = bytes
                    .checked_add(url.len())
                    .and_then(|n| n.checked_add(title.len()))
                    .ok_or_else(refused)?;
            }
        }
        if bytes > MAX_IMPORT_BYTES || self.history.iter().any(|r| r.visit_count == 0) {
            return Err(refused());
        }
        Ok(())
    }
}
fn refused() -> ActionError {
    ActionError::Refused("Browser import is invalid or exceeds its limit".into())
}

pub(crate) fn tag(source: &[u8; 32], url: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(source);
    digest.update(url.as_bytes());
    format!("import:{}", digest.finalize().to_hex())
}

/// Check a source receipt before any writes. Existing identical receipts are retries.
pub(crate) fn receipt(
    doc: &Doc,
    namespace: &str,
    batch: &ImportBatch,
) -> Result<Option<(String, String)>, ActionError> {
    batch.validate()?;
    let key = format!(
        "browser.import.{namespace}:{}",
        blake3::Hash::from(batch.source).to_hex()
    );
    let namespace_prefix = format!("browser.import.{namespace}:");
    if doc.get(ROOT, &key)?.is_none()
        && doc
            .keys(ROOT)
            .filter(|k| k.starts_with(&namespace_prefix))
            .count()
            >= 1024
    {
        return Err(refused());
    }
    let encoded = serde_json::to_vec(batch).map_err(|_| refused())?;
    if encoded.len() > MAX_IMPORT_BYTES {
        return Err(refused());
    }
    let digest = blake3::hash(&encoded).to_hex().to_string();
    if let Some((value, _)) = doc.get(ROOT, &key)? {
        if value.as_str() == Some(digest.as_str()) {
            return Ok(None);
        }
        return Err(ActionError::Refused(
            "Browser source receipt conflicts with this import".into(),
        ));
    }
    Ok(Some((key, digest)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{BrowsingHistory, History, browsing_history};
    use tilo_actions::Action;
    use ui::tilo_resource::Replica;

    fn batch() -> ImportBatch {
        ImportBatch {
            source: [7; 32],
            bookmarks: vec![BookmarkImport {
                url: "https://old.test/".into(),
                title: "Imported title".into(),
            }],
            history: vec![HistoryImport {
                url: "https://old.test/".into(),
                title: "Older title".into(),
                last_visit: 123,
                visit_count: 17,
            }],
        }
    }

    #[test]
    fn continuity_actions_preserve_existing_library_and_retry_without_resurrection() {
        let mut library = Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        library
            .apply_action(&crate::personal_bookmarks::PersonalBookmarkAction::Add {
                url: "https://existing.test/".into(),
                title: "My bookmark".into(),
                tag: "native".into(),
            })
            .unwrap();
        let crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarks { batch } =
            batch().bookmark_action()
        else {
            panic!("bookmarks");
        };
        let imported = crate::personal_bookmarks::PersonalBookmarkAction::Import {
            bundle: ImportBundle::from_batch(batch).unwrap(),
        };
        library.apply_action(&imported).unwrap();
        library.apply_action(&imported).unwrap();
        let state = library.state().unwrap();
        assert_eq!(state.bookmarks.entries().len(), 2);
        assert!(
            state
                .bookmarks
                .entries()
                .iter()
                .any(|r| r.title == "My bookmark")
        );
        let entry = state
            .bookmarks
            .entries()
            .iter()
            .find(|r| r.url == "https://old.test/")
            .unwrap();
        assert_eq!(entry.tags.len(), 1);
        library
            .apply_action(&crate::personal_bookmarks::PersonalBookmarkAction::Remove {
                url: entry.url.clone(),
                observed_tags: entry.tags.clone(),
            })
            .unwrap();
        library.apply_action(&imported).unwrap();
        assert_eq!(library.state().unwrap().bookmarks.entries().len(), 1);
    }

    #[test]
    fn continuity_history_is_private_additive_and_idempotent_with_original_counts() {
        let mut history = Replica::<BrowsingHistory>::new();
        history
            .apply(BrowsingHistory::VISIT.with(browsing_history::Visit {
                url: "https://old.test/".into(),
                title: "Current title".into(),
                at: 456,
            }))
            .unwrap();
        history.apply(batch().history_action().unwrap()).unwrap();
        history.apply(batch().history_action().unwrap()).unwrap();
        let entries = history.state().unwrap().entries();
        let row = &entries[0];
        assert_eq!((row.visit_count, row.last_visit), (18, 456));
        assert_eq!(row.title, "Current title");
        history
            .apply(BrowsingHistory::VISIT.with(browsing_history::Visit {
                url: row.url.clone(),
                title: "New title".into(),
                at: 789,
            }))
            .unwrap();
        assert_eq!(history.state().unwrap().entries()[0].visit_count, 19);
        history
            .apply(BrowsingHistory::CLEAR_HISTORY.with(()))
            .unwrap();
        history.apply(batch().history_action().unwrap()).unwrap();
        assert!(history.state().unwrap().is_empty());
        let tree = ui::tilo_resource::publish::<BrowsingHistory>(
            "private",
            "History",
            &history.state().unwrap(),
        );
        let declaration = tree
            .actions
            .iter()
            .find(|action| action.id == "import-history-bundle")
            .unwrap();
        assert_eq!(declaration.params()[0].ty, tilo_actions::ParamType::Secret);
        assert_eq!(
            declaration.contract.need().kind,
            tilo_actions::ActionKind::Undoable
        );
        assert!(!declaration.contract.need().privileged);
    }

    #[test]
    fn continuity_preflight_refuses_cross_record_or_changed_source_before_any_write() {
        let mut doc = Doc::new();
        let batch = batch();
        assert!(
            crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarks {
                batch: batch.clone()
            }
            .apply(&mut doc)
            .is_err()
        );
        assert!(doc.keys(ROOT).next().is_none());
        let mut history = Replica::<BrowsingHistory>::new();
        let mixed = ImportBundle::from_batch(batch.clone()).unwrap();
        assert!(
            history
                .apply(
                    BrowsingHistory::IMPORT_HISTORY_BUNDLE
                        .with(serde_json::to_string(&mixed).unwrap())
                )
                .is_err()
        );
        assert!(history.state().unwrap().is_empty());
        history.apply(batch.history_action().unwrap()).unwrap();
        let saved = history.state().unwrap();
        let mut changed = batch.clone();
        changed.history[0].visit_count = 33;
        assert!(history.apply(changed.history_action().unwrap()).is_err());
        assert_eq!(history.state().unwrap(), saved);
        let mut invalid = batch;
        invalid.bookmarks.push(BookmarkImport {
            url: "file:///secret".into(),
            title: "Private".into(),
        });
        let before = doc.save();
        assert!(invalid.bookmark_action().apply(&mut doc).is_err());
        assert_eq!(doc.save(), before);
        assert_eq!(history.state().unwrap(), saved);
    }

    #[test]
    fn complete_imported_history_search_opens_oldest_and_last_rows_through_real_resources() {
        use ui::search::SearchProvider;
        use ui::tilo_resource::Effect;
        let raw = ImportBatch {
            source: [52; 32],
            bookmarks: Vec::new(),
            history: (0..10_001)
                .map(|n| HistoryImport {
                    url: format!("https://migration.test/{n:05}"),
                    title: match n {
                        0 => "OldestLegacyNeedle".into(),
                        10_000 => "LastLegacyNeedle".into(),
                        _ => format!("Page {n}"),
                    },
                    last_visit: n,
                    visit_count: 4,
                })
                .collect(),
        };
        let bundle = ImportBundle::from_batch(raw).unwrap();
        let mut personal = Replica::<BrowsingHistory>::new();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        let history = History::of(&personal.state().unwrap());
        assert_eq!(
            history.len(),
            10_001,
            "all imported rows must be reachable through the actual person projection"
        );
        let index = crate::search_provider::BrowserSearchIndex::default();
        index.sync(&history);
        assert_eq!(index.entries().len(), 10_001);
        for (needle, url) in [
            ("OldestLegacyNeedle", "https://migration.test/00000"),
            ("LastLegacyNeedle", "https://migration.test/10000"),
        ] {
            let provider = index.provider();
            let hits = provider.search(needle);
            let hit = hits.iter().find(|hit| hit.title == needle).unwrap();
            let address = hit.action_address().unwrap();
            assert_eq!(
                address.resource.as_str(),
                crate::actions::history_resource_id(url)
            );
            let resource = provider.resolve_action_resource(&address).unwrap();
            let action = resource
                .actions
                .iter()
                .find(|a| a.id == address.action)
                .unwrap();
            let encoded = action.build_json(&serde_json::Map::new()).unwrap();
            let payload: serde_json::Value = serde_json::from_slice(&encoded.payload).unwrap();
            let effect = crate::effects::BrowserEffect::decode(
                payload["effect"].as_str().unwrap(),
                payload["args"].as_object().unwrap(),
            )
            .unwrap();
            let engine = ui::webview::test_engine::TestEngine::new();
            let mut widget = crate::BrowserWidget::with_engine(engine.clone(), true);
            widget
                .admit(ui::tilo_actions::InvocationId::new(), effect.operation())
                .unwrap();
            assert_eq!(engine.current_uri().as_deref(), Some(url));
        }
        personal
            .apply(BrowsingHistory::FORGET.with("https://migration.test/00000".to_owned()))
            .unwrap();
        personal.apply(bundle.history_action().unwrap()).unwrap();
        let history = History::of(&personal.state().unwrap());
        assert_eq!(history.len(), 10_000);
        index.invalidate();
        index.sync(&history);
        assert!(index.provider().search("OldestLegacyNeedle").is_empty());
        assert!(
            index
                .resource(&crate::actions::history_resource_id(
                    "https://migration.test/00000"
                ))
                .is_none()
        );
        assert!(
            index
                .resource(&crate::actions::history_resource_id(
                    "https://migration.test/10000"
                ))
                .is_some()
        );
    }

    #[test]
    fn complete_bundle_preflights_all_chunks_and_preserves_exact_retry_counts() {
        let raw = ImportBatch {
            source: [31; 32],
            bookmarks: Vec::new(),
            history: (0..501)
                .map(|n| HistoryImport {
                    url: format!("https://history.test/{n}"),
                    title: format!("Page {n}"),
                    last_visit: n,
                    visit_count: 3,
                })
                .collect(),
        };
        let bundle = ImportBundle::from_batch(raw).unwrap();
        assert_eq!(bundle.chunks.len(), 2);
        let mut conflicted = ImportBatch {
            source: bundle.source,
            bookmarks: vec![],
            history: bundle.chunks[1].history.clone(),
        };
        conflicted.history[0].visit_count = 99;
        let mut doc = Replica::<BrowsingHistory>::new();
        doc.apply(conflicted.history_action().unwrap()).unwrap();
        let before = doc.state().unwrap();
        assert!(doc.apply(bundle.history_action().unwrap()).is_err());
        assert_eq!(
            doc.state().unwrap(),
            before,
            "source conflict must not write early chunks"
        );
        assert_eq!(doc.state().unwrap().imported.len(), 1);
        let mut changed = bundle.clone();
        changed.chunks[1].history[0].title = "Changed after review".into();
        assert!(changed.validate().is_err());
        let mut valid = Replica::<BrowsingHistory>::new();
        valid.apply(bundle.history_action().unwrap()).unwrap();
        let first = valid.state().unwrap();
        valid.apply(bundle.history_action().unwrap()).unwrap();
        assert_eq!(valid.state().unwrap(), first);
        assert_eq!(valid.state().unwrap().imported.len(), 501);
        let history = History::of(&valid.state().unwrap());
        assert_eq!(history.len(), 501);
        assert!(history.entries().iter().all(|row| row.visit_count == 3));
        let tree = ui::tilo_resource::publish::<BrowsingHistory>("private", "History", &first);
        let declaration = tree
            .actions
            .iter()
            .find(|action| action.id == "import-history-bundle")
            .unwrap();
        assert_eq!(declaration.params()[0].ty, tilo_actions::ParamType::Secret);
    }

    #[test]
    fn selected_bookmark_projection_excludes_history_before_private_import() {
        let crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarks { batch: selected } =
            batch().bookmark_action()
        else {
            panic!("bookmark projection");
        };
        assert!(selected.history.is_empty());
        let mut library = Replica::<crate::personal_bookmarks::PersonalBookmarks>::new();
        library
            .apply_action(&crate::personal_bookmarks::PersonalBookmarkAction::Import {
                bundle: ImportBundle::from_batch(selected).unwrap(),
            })
            .unwrap();
        assert_eq!(
            library.state().unwrap().bookmarks.entries()[0].url,
            "https://old.test/"
        );
    }
}
