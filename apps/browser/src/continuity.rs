//! Bounded migration values applied to Browser's real library and private person record.
//! Parsing grants and authenticated installer delivery are separate from these actions.
use automerge::{ROOT, ReadDoc};
use tilo_actions::{ActionError, Doc};

pub use browser_continuity::imports::{
    BookmarkImport, HistoryImport, ImportBatch, ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES,
    MAX_IMPORT_BYTES, MAX_IMPORT_ENTRIES,
};

/// App-specific action projections for reviewed import values.
pub trait ImportActions {
    /// Produce a bookmark action without private visit data.
    fn bookmark_action(&self) -> crate::continuity_bookmarks::PersonalLibraryEdit;
    /// Produce an action for the current person's private history record.
    /// # Errors
    /// Refuses invalid or oversized import payloads.
    fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError>;
}

impl ImportActions for ImportBundle {
    /// The bookmark-only projection for the private personal library.
    fn bookmark_action(&self) -> crate::continuity_bookmarks::PersonalLibraryEdit {
        let bundle = self.bookmark_bundle();
        crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarkBundle { bundle }
    }
    fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError> {
        let selected = self.history_bundle();
        selected.validate()?;
        let encoded = serde_json::to_string(&selected).map_err(|_| refused())?;
        Ok(crate::history::BrowsingHistory::IMPORT_HISTORY_BUNDLE.with(encoded))
    }
}
impl ImportActions for ImportBatch {
    /// Produce the shared-library action without including private visit data.
    fn bookmark_action(&self) -> crate::continuity_bookmarks::PersonalLibraryEdit {
        crate::continuity_bookmarks::PersonalLibraryEdit::ImportBookmarks {
            batch: Self {
                source: self.source,
                bookmarks: self.bookmarks.clone(),
                history: Vec::new(),
            },
        }
    }
    /// Produce the action that belongs only to the current person’s history record.
    fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError> {
        let mut selected = self.clone();
        selected.bookmarks.clear();
        ImportBundle::from_batch(selected)?.history_action()
    }
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
