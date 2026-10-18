//! Browser adapter for reviewed imported bookmark hierarchy.
pub use browser_continuity::imported_folders::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::continuity::ImportActions;
    use crate::personal_bookmarks::PersonalBookmarks;
    use ui::tilo_resource::{Replica, Resource as _};

    fn find<'a>(
        rows: &'a [tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        rows.iter().find_map(|row| {
            if row.id.as_str() == id {
                Some(row)
            } else {
                find(&row.children, id)
            }
        })
    }

    /// The production Browser resource/surface, mounted live with an explicitly
    /// lent deterministic engine instead of starting a native browser process.
    struct TestBrowser;
    impl ui::framework::ResourceApp for TestBrowser {
        type Resource = crate::resource::Browser;
        const ID: &'static str = "so.tilo.browser";
        const TITLE: &'static str = "Browser";
        fn view(cx: &mut ui::framework::ViewCx<Self::Resource>) -> ui::widget::Element {
            let engine = cx
                .take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>()
                .unwrap();
            cx.surface(crate::BrowserWindow::new(
                crate::BrowserWidget::with_engine(engine, true),
                false,
            ))
        }
    }

    #[test]
    fn canonical_hierarchy_retry_and_removal_survive_reload_without_url_dedup() {
        let source = [17; 32];
        let folder = |id: &str, parent: Option<&str>, position| ImportedNode {
            id: id.into(),
            parent: parent.map(str::to_owned),
            position,
            title: id.into(),
            kind: NodeKind::Folder,
        };
        let bookmark = |id: &str, parent: &str| ImportedNode {
            id: id.into(),
            parent: Some(parent.into()),
            position: 0,
            title: id.into(),
            kind: NodeKind::Bookmark {
                url: "https://example.org/".into(),
            },
        };
        let tree = ImportedTree {
            source,
            nodes: vec![
                folder("root", None, 0),
                folder("one", Some("root"), 0),
                folder("two", Some("root"), 1),
                bookmark("first", "one"),
                bookmark("second", "two"),
            ],
        };
        let mut bundle =
            crate::continuity::ImportBundle::from_batch(crate::continuity::ImportBatch {
                source,
                bookmarks: vec![crate::continuity::BookmarkImport {
                    url: "https://example.org/".into(),
                    title: "Example".into(),
                }],
                history: vec![],
            })
            .unwrap();
        bundle.imported_tree = Some(tree.clone());
        let action = bundle.bookmark_action().unwrap();
        let mut library = Replica::<PersonalBookmarks>::new();
        library.apply(action.clone()).unwrap();
        let state = library.state().unwrap();
        assert_eq!(state.imported_trees(), vec![tree]);
        assert_eq!(
            state.bookmarks().entries().len(),
            1,
            "legacy flat URL identity remains compatible"
        );
        let trees = state.imported_trees();
        let captured = &trees[0];
        let children: Vec<_> = captured
            .nodes
            .iter()
            .filter(|node| node.parent.as_deref() == Some("root"))
            .collect();
        assert_eq!(children.len(), 2);
        let first = captured
            .nodes
            .iter()
            .find(|node| node.parent.as_deref() == Some(&children[0].id))
            .unwrap();
        let second = captured
            .nodes
            .iter()
            .find(|node| node.parent.as_deref() == Some(&children[1].id))
            .unwrap();
        let first = crate::personal_bookmarks::node_resource(captured, first);
        let second = crate::personal_bookmarks::node_resource(captured, second);
        assert_eq!(first.label, "first");
        assert_eq!(second.label, "second");
        assert!(
            first
                .actions
                .iter()
                .any(|action| action.id == "remove-personal-imported-node")
        );
        library
            .apply(
                PersonalBookmarks::REMOVE_IMPORTED_NODE
                    .with((source_id(&source), "one".to_owned())),
            )
            .unwrap();
        let mut reopened = Replica::<PersonalBookmarks>::from_doc(
            ui::tilo_resource::Doc::load(&library.doc_mut().save()).unwrap(),
        );
        reopened.apply(action).unwrap();
        let restored = reopened.state().unwrap().imported_trees();
        assert_eq!(
            restored[0]
                .nodes
                .iter()
                .map(|n| n.id.as_str())
                .collect::<Vec<_>>(),
            vec!["root", "two", "second"]
        );
        let mut conflicting = bundle;
        conflicting.imported_tree.as_mut().unwrap().nodes[0].title = "changed source".into();
        assert!(
            reopened
                .apply(conflicting.bookmark_action().unwrap())
                .is_err()
        );
        assert_eq!(reopened.state().unwrap().imported_trees(), restored);
    }

    #[test]
    fn published_placement_opens_actual_test_engine_and_removes_only_that_placement() {
        let source = [51; 32];
        let mut bundle =
            crate::continuity::ImportBundle::from_batch(crate::continuity::ImportBatch {
                source,
                bookmarks: vec![crate::continuity::BookmarkImport {
                    url: "https://example.org/migrated".into(),
                    title: "Migrated".into(),
                }],
                history: vec![],
            })
            .unwrap();
        bundle.imported_tree = Some(ImportedTree {
            source,
            nodes: vec![
                ImportedNode {
                    id: "root".into(),
                    parent: None,
                    position: 0,
                    title: "Imported toolbar".into(),
                    kind: NodeKind::Folder,
                },
                ImportedNode {
                    id: "placement".into(),
                    parent: Some("root".into()),
                    position: 0,
                    title: "Migrated page".into(),
                    kind: NodeKind::Bookmark {
                        url: "https://example.org/migrated".into(),
                    },
                },
            ],
        });
        let engine = ui::webview::test_engine::TestEngine::new();
        let mut app = ui::framework::testing::TestApp::<TestBrowser>::for_person_with_lent(
            &"19".repeat(32),
            engine.clone(),
        )
        .unwrap();
        app.call_on(
            crate::personal_bookmarks::SCOPE,
            "import-personal-bookmarks",
            serde_json::json!({"bundle":serde_json::to_string(&bundle).unwrap()}),
        )
        .unwrap();
        app.settle();
        let record = app
            .person::<crate::personal_bookmarks::PersonalBookmarks>()
            .unwrap();
        let trees = record.get().imported_trees();
        let tree = &trees[0];
        let leaf = crate::personal_bookmarks::node_resource(
            tree,
            tree.nodes
                .iter()
                .find(|node| node.id == "placement")
                .unwrap(),
        );
        app.call_on(
            &crate::personal_bookmarks::node_id(&source, "root"),
            "show-personal-bookmark-folder",
            serde_json::json!({}),
        )
        .unwrap();
        app.settle();
        assert!(leaf.actions.iter().any(|a| a.id == "open"));
        let id = leaf.id.as_str().to_owned();
        let publication = app.harness().resources();
        let published = find(&publication, &id).unwrap_or_else(|| {
            panic!(
                "Imported placement missing from actual framework publication: {}",
                publication
                    .iter()
                    .map(|row| row.id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        assert!(
            published.actions.iter().any(|action| action.id == "open"),
            "Placement lacks open in actual framework publication: {}",
            published
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        app.call_on(&id, "open", serde_json::json!({})).unwrap();
        app.settle();
        assert_eq!(
            engine.current_uri().as_deref(),
            Some("https://example.org/migrated")
        );
        app.call_on(&id, "remove-personal-imported-node", serde_json::json!({}))
            .unwrap();
        app.settle();
        assert_eq!(record.get().imported_trees()[0].nodes.len(), 1);
        assert_eq!(
            record.get().bookmarks().entries().len(),
            1,
            "removing one placement must not erase independent flat URL bookmark"
        );
        app.call_on(
            crate::personal_bookmarks::SCOPE,
            "import-personal-bookmarks",
            serde_json::json!({"bundle":serde_json::to_string(&bundle).unwrap()}),
        )
        .unwrap();
        app.settle();
        assert_eq!(record.get().imported_trees()[0].nodes.len(), 1);
    }

    #[test]
    fn orphan_cycle_duplicate_order_and_corrupt_state_refuse_before_projection() {
        let node = ImportedNode {
            id: "root".into(),
            parent: None,
            position: 0,
            title: "Root".into(),
            kind: NodeKind::Folder,
        };
        let valid = ImportedTree {
            source: [23; 32],
            nodes: vec![node.clone()],
        };
        let mut invalid = valid.clone();
        invalid.nodes[0].parent = Some("missing".into());
        assert!(invalid.validate().is_err());
        invalid.nodes[0].parent = Some("root".into());
        assert!(invalid.validate().is_err());
        invalid = valid.clone();
        let mut duplicate = node;
        duplicate.id = "other".into();
        invalid.nodes.push(duplicate);
        assert!(invalid.validate().is_err());
        // A kept tree that is not one reads as no library at all, never as
        // an empty one.
        let mut doc = ui::tilo_resource::Doc::new();
        let trees = ui::tilo_resource::Slot::Key(&automerge::ROOT, "trees")
            .object(&mut doc, automerge::ObjType::Map)
            .unwrap();
        ui::tilo_resource::write_key(
            &mut doc,
            &trees,
            &source_id(&valid.source),
            &ui::tilo_resource::Atom("{}".into()),
        )
        .unwrap();
        assert!(crate::personal_bookmarks::PersonalBookmarks::read(&doc).is_err());
    }
}
