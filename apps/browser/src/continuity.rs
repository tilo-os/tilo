//! Bounded migration values applied to Browser's real library and private person record.
//! Parsing grants and authenticated installer delivery are separate from these actions.
use tilo_actions::ActionError;

pub use browser_continuity::imports::{
    BookmarkImport, HistoryImport, ImportBatch, ImportBundle, MAX_BUNDLE_BYTES, MAX_BUNDLE_ENTRIES,
    MAX_IMPORT_BYTES, MAX_IMPORT_ENTRIES,
};

/// App-specific action projections for reviewed import values.
pub trait ImportActions {
    /// Produce the personal library's import, without private visit data.
    /// # Errors
    /// Refuses invalid or oversized import payloads.
    fn bookmark_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::personal_bookmarks::PersonalBookmarks>, ActionError>;
    /// Produce an action for the current person's private history record.
    /// # Errors
    /// Refuses invalid or oversized import payloads.
    fn history_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::history::BrowsingHistory>, ActionError>;
}

impl ImportActions for ImportBundle {
    /// The bookmark-only projection for the private personal library.
    fn bookmark_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::personal_bookmarks::PersonalBookmarks>, ActionError>
    {
        let selected = self.bookmark_bundle();
        selected.validate()?;
        let encoded = serde_json::to_string(&selected).map_err(|_| refused())?;
        Ok(crate::personal_bookmarks::PersonalBookmarks::IMPORT_BOOKMARKS.with(encoded))
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
    /// The personal library's import of the batch's bookmarks, without
    /// private visit data.
    fn bookmark_action(
        &self,
    ) -> Result<ui::tilo_resource::Invoke<crate::personal_bookmarks::PersonalBookmarks>, ActionError>
    {
        ImportBundle::from_batch(Self {
            source: self.source,
            bookmarks: self.bookmarks.clone(),
            history: Vec::new(),
        })?
        .bookmark_action()
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
fn refused() -> ActionError {
    ActionError::Refused("Browser import is invalid or exceeds its limit".into())
}

pub(crate) fn tag(source: &[u8; 32], url: &str) -> String {
    let mut digest = blake3::Hasher::new();
    digest.update(source);
    digest.update(url.as_bytes());
    format!("import:{}", digest.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{BrowsingHistory, History, browsing_history};
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
        use crate::personal_bookmarks::PersonalBookmarks;
        let mut library = Replica::<PersonalBookmarks>::new();
        library
            .apply(PersonalBookmarks::ADD_BOOKMARK.with((
                "https://existing.test/".to_owned(),
                "My bookmark".to_owned(),
            )))
            .unwrap();
        let imported = batch().bookmark_action().unwrap();
        library.apply(imported.clone()).unwrap();
        library.apply(imported.clone()).unwrap();
        let state = library.state().unwrap().projected();
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
            .apply(PersonalBookmarks::REMOVE_BOOKMARK.with((entry.url.clone(), entry.tags.clone())))
            .unwrap();
        library.apply(imported).unwrap();
        assert_eq!(library.state().unwrap().bookmarks().entries().len(), 1);
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
        use crate::personal_bookmarks::PersonalBookmarks;
        let mut library = Replica::<PersonalBookmarks>::new();
        let batch = batch();
        // Visits never enter the personal library.
        let with_history = ImportBundle::from_batch(batch.clone()).unwrap();
        assert!(
            library
                .apply(
                    PersonalBookmarks::IMPORT_BOOKMARKS
                        .with(serde_json::to_string(&with_history).unwrap())
                )
                .is_err()
        );
        assert_eq!(library.state().unwrap(), PersonalBookmarks::default());
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
        let before = library.doc_mut().save();
        assert!(
            invalid
                .bookmark_action()
                .map_or(true, |action| library.apply(action).is_err())
        );
        assert_eq!(library.doc_mut().save(), before);
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
            // A hit is anchored at its history row; the row the window
            // publishes for it carries the action that opens the page.
            let provider = index.provider();
            let hits = provider.search(needle);
            let hit = hits.iter().find(|hit| hit.title == needle).unwrap();
            assert_eq!(hit.action_id, crate::actions::history_resource_id(url));
            let resource = index.resource(&hit.action_id).unwrap();
            assert_eq!(resource.kind, "browser.history-entry");
            let [action] = resource.actions.as_slice() else {
                panic!("a history row offers one action: open its page");
            };
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
        use crate::personal_bookmarks::PersonalBookmarks;
        let import = batch().bookmark_action().unwrap();
        let bundle: ImportBundle =
            serde_json::from_str(import.arguments["bundle"].as_str().unwrap()).unwrap();
        assert!(bundle.chunks.iter().all(|chunk| chunk.history.is_empty()));
        let mut library = Replica::<PersonalBookmarks>::new();
        library.apply(import).unwrap();
        assert_eq!(
            library.state().unwrap().bookmarks().entries()[0].url,
            "https://old.test/"
        );
    }
}
