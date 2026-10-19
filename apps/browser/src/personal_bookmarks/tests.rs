use super::*;
use crate::continuity::{BookmarkImport, ImportBatch, ImportBundle};
use crate::imported_folders::{ImportedNode, ImportedTree, NodeKind};
use ui::framework::testing::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::search::SearchProvider;
use ui::theme::Theme;
use ui::widget::Element;

struct TestBrowser;
impl ResourceApp for TestBrowser {
    type Resource = crate::Browser;
    const ID: &'static str = "so.tilo.browser";
    const TITLE: &'static str = "Browser";
    fn view(cx: &mut ViewCx<Self::Resource>) -> Element {
        let engine = cx
            .take_lent::<std::sync::Arc<ui::webview::test_engine::TestEngine>>()
            .unwrap();
        cx.surface(crate::BrowserWindow::new(
            crate::BrowserWidget::with_engine(engine, true),
            false,
        ))
    }
}
fn bundle() -> ImportBundle {
    let source = [64; 32];
    let mut bundle = ImportBundle::from_batch(ImportBatch {
        source,
        bookmarks: vec![BookmarkImport {
            url: "https://continuity.test/repeated".into(),
            title: "Repeated".into(),
        }],
        history: Vec::new(),
    })
    .unwrap();
    let mut nodes = vec![
        ImportedNode {
            id: "bar".into(),
            parent: None,
            position: 0,
            title: "Bookmarks bar".into(),
            kind: NodeKind::Folder,
        },
        ImportedNode {
            id: "first".into(),
            parent: Some("bar".into()),
            position: 0,
            title: "First placement".into(),
            kind: NodeKind::Bookmark {
                url: "https://continuity.test/repeated".into(),
            },
        },
        ImportedNode {
            id: "separator".into(),
            parent: Some("bar".into()),
            position: 1,
            title: String::new(),
            kind: NodeKind::Separator,
        },
        ImportedNode {
            id: "again".into(),
            parent: Some("bar".into()),
            position: 2,
            title: "Repeated placement".into(),
            kind: NodeKind::Bookmark {
                url: "https://continuity.test/repeated".into(),
            },
        },
    ];
    nodes.extend((3..63).map(|n| ImportedNode {
        id: format!("page{n}"),
        parent: Some("bar".into()),
        position: n,
        title: format!("Personal page {n}"),
        kind: NodeKind::Bookmark {
            url: format!("https://continuity.test/page{n}"),
        },
    }));
    bundle.imported_tree = Some(ImportedTree { source, nodes });
    bundle.validate().unwrap();
    bundle
}
fn live() -> (
    TestApp<TestBrowser>,
    std::sync::Arc<ui::webview::test_engine::TestEngine>,
) {
    let engine = ui::webview::test_engine::TestEngine::new();
    let app =
        TestApp::<TestBrowser>::for_person_with_lent(&"68".repeat(32), engine.clone()).unwrap();
    (app, engine)
}
fn import(app: &mut TestApp<TestBrowser>, bundle: &ImportBundle) {
    app.call_on(
        SCOPE,
        "import-personal-bookmarks",
        serde_json::json!({"bundle":serde_json::to_string(bundle).unwrap()}),
    )
    .unwrap();
    app.settle();
}
#[test]
fn live_personal_import_search_folder_and_removal_never_edit_workspace_bookmarks() {
    let (mut app, engine) = live();
    let bundle = bundle();
    import(&mut app, &bundle);
    let record = app.person::<PersonalBookmarks>().unwrap();
    let original = record.get();
    assert_eq!(original.imported_trees()[0].nodes.len(), 64);
    assert!(app.state().bookmarks.is_empty());
    let entries = app
        .with_surface::<crate::BrowserWindow, _>(|surface| surface.page().search_entries())
        .unwrap();
    let search = ui::framework::testing::search_source::<crate::Browser>(entries);
    let hits = search
        .search_page(
            &ui::search::SearchQuery::new("Personal page 62")
                .within(crate::actions::BOOKMARKS_SCOPE),
            None,
            256,
            20,
        )
        .results;
    let address = hits
        .iter()
        .find(|hit| hit.title == "Personal page 62")
        .unwrap()
        .action_address()
        .unwrap();
    app.call_on(
        address.resource.as_str(),
        &address.action,
        serde_json::json!({}),
    )
    .unwrap();
    assert_eq!(
        engine.current_uri().as_deref(),
        Some("https://continuity.test/page62")
    );
    app.call_on(
        &node_id(&bundle.source, "bar"),
        "show-personal-bookmark-folder",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    assert_folder_controls(&mut app, &bundle);
    app.call_on(
        &node_id(&bundle.source, "first"),
        "remove-personal-imported-node",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    import(&mut app, &bundle);
    let state = record.get();
    assert!(
        !state.imported_trees()[0]
            .nodes
            .iter()
            .any(|node| node.id == "first")
    );
    assert!(
        state.imported_trees()[0]
            .nodes
            .iter()
            .any(|node| node.id == "again")
    );
    assert!(app.state().bookmarks.is_empty());
}
fn find(rows: &[tilo_actions::Resource], id: &str) -> bool {
    rows.iter()
        .any(|row| row.id.as_str() == id || find(&row.children, id))
}
fn hits<P: SearchProvider>(
    search: &P,
    text: &str,
    scope: Option<&str>,
) -> Vec<ui::search::SearchResult> {
    let query = scope.map_or_else(
        || ui::search::SearchQuery::new(text),
        |scope| ui::search::SearchQuery::new(text).within(scope),
    );
    search.search_page(&query, None, 256, 20).results
}
#[test]
fn personal_search_entries_join_the_bookmarks_scope_and_no_other_library_section() {
    let (mut app, _engine) = live();
    let bundle = bundle();
    import(&mut app, &bundle);
    let flat_url = "https://continuity.test/flat";
    app.person::<PersonalBookmarks>()
        .unwrap()
        .apply(&PersonalBookmarkAction::Add {
            url: flat_url.into(),
            title: "Personal flat page".into(),
            tag: "bookmark:personal-search-boundary".into(),
        })
        .unwrap();
    app.settle();
    assert!(app.state().bookmarks.is_empty());

    let entries = app
        .with_surface::<crate::BrowserWindow, _>(|surface| surface.page().search_entries())
        .unwrap();
    let search = ui::framework::testing::search_source::<crate::Browser>(entries);

    let bookmarks = hits(
        &search,
        "Personal flat page",
        Some(crate::actions::BOOKMARKS_SCOPE),
    );
    assert_eq!(bookmarks.len(), 1);
    let flat = bookmarks[0].action_address().unwrap();
    assert_eq!(flat.resource, flat_id(flat_url));
    assert_eq!(flat.action, "open");

    let imported = hits(
        &search,
        "Personal page 62",
        Some(crate::actions::BOOKMARKS_SCOPE),
    );
    assert_eq!(imported.len(), 1);
    let placement = imported[0].action_address().unwrap();
    assert_eq!(placement.resource, node_id(&bundle.source, "page62"));
    assert_eq!(placement.action, "open");

    for scope in [
        crate::actions::HISTORY_SCOPE,
        crate::actions::DOWNLOADS_SCOPE,
    ] {
        for text in ["Personal flat page", "Personal page 62"] {
            assert!(
                hits(&search, text, Some(scope)).is_empty(),
                "{text} must not appear under {scope}"
            );
        }
    }
    for text in ["Personal flat page", "Personal page 62"] {
        assert_eq!(hits(&search, text, Some(SCOPE)).len(), 1);
        assert_eq!(hits(&search, text, None).len(), 1);
    }
    assert!(app.state().bookmarks.is_empty());
}
#[test]
fn removing_a_personal_placement_removes_only_its_own_search_hit() {
    let (mut app, _engine) = live();
    let bundle = bundle();
    import(&mut app, &bundle);
    let entries = app
        .with_surface::<crate::BrowserWindow, _>(|surface| surface.page().search_entries())
        .unwrap();
    let search = ui::framework::testing::search_source::<crate::Browser>(entries);
    assert_eq!(
        hits(
            &search,
            "First placement",
            Some(crate::actions::BOOKMARKS_SCOPE)
        )
        .len(),
        1
    );

    app.call_on(
        &node_id(&bundle.source, "first"),
        "remove-personal-imported-node",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    import(&mut app, &bundle);
    assert!(app.state().bookmarks.is_empty());

    let entries = app
        .with_surface::<crate::BrowserWindow, _>(|surface| surface.page().search_entries())
        .unwrap();
    let search = ui::framework::testing::search_source::<crate::Browser>(entries);
    assert!(
        hits(
            &search,
            "First placement",
            Some(crate::actions::BOOKMARKS_SCOPE)
        )
        .is_empty()
    );
    assert_eq!(
        hits(
            &search,
            "Repeated placement",
            Some(crate::actions::BOOKMARKS_SCOPE)
        )
        .len(),
        1
    );
    assert_eq!(
        hits(
            &search,
            "Personal page 62",
            Some(crate::actions::BOOKMARKS_SCOPE)
        )
        .len(),
        1
    );
    assert!(app.state().bookmarks.is_empty());
}
fn assert_folder_controls(app: &mut TestApp<TestBrowser>, bundle: &ImportBundle) {
    use crate::personal_bookmark_view::ViewRequest;
    let record = app.person::<PersonalBookmarks>().unwrap();
    let state = record.get();
    let next = crate::browser_widget::personal::request_resource(
        &state.projected(),
        ViewRequest::Folder {
            source: bundle.source,
            id: Some("bar".into()),
            offset: 50,
        },
    )
    .unwrap();
    app.call_on(
        next.id.as_str(),
        "show-personal-bookmark-folder",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    let publication = app.harness().resources();

    assert!(find(&publication, &node_id(&bundle.source, "page62")));
    assert!(!find(&publication, &node_id(&bundle.source, "first")));
    app.call_on(
        &node_id(&bundle.source, "bar"),
        "show-personal-bookmark-folder",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
}
#[test]
fn live_personal_bookmark_folder_rasterizes_in_both_themes_and_scaled_viewports() {
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        for (viewport, scale) in [((480, 800), 1.0), ((1180, 820), 1.0), ((2560, 1440), 2.0)] {
            let (mut app, _engine) = live();
            let bundle = bundle();
            import(&mut app, &bundle);
            app.call_on(
                &node_id(&bundle.source, "bar"),
                "show-personal-bookmark-folder",
                serde_json::json!({}),
            )
            .unwrap();
            app.harness().resize(viewport).unwrap();
            app.harness()
                .set_theme(theme.clone().with_ui_scale(scale).with_reduce_motion(true));
            app.settle();
            let pixels = app.harness().render_rgba();
            let expected = (viewport.0 as usize)
                .checked_mul(viewport.1 as usize)
                .unwrap()
                .checked_mul(4)
                .unwrap();
            assert_eq!(pixels.len(), expected);
            let ground = pixels.get(..4).unwrap();
            assert!(
                pixels
                    .chunks_exact(4)
                    .filter(|pixel| *pixel != ground)
                    .count()
                    > 200
            );
            assert!(app.state().bookmarks.is_empty());
            if let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") {
                let root = std::path::PathBuf::from(directory).join("browser-personal-bookmarks");
                std::fs::create_dir_all(&root).unwrap();
                app.harness().render_png(&root.join(format!(
                    "personal-folder-{name}-{scale}x-{}x{}.png",
                    viewport.0, viewport.1
                )));
            }
        }
    }
}

/// A library as Browser kept it before it was a record: its adds, a
/// tombstone, an import receipt, an imported tree and a removed placement as
/// prefixed keys at the document's root. Opened now, it holds the same
/// bookmarks and trees, and nothing stays under the old keys.
#[test]
fn a_library_kept_before_the_record_opens_with_its_bookmarks_and_trees() {
    use automerge::ReadDoc as _;
    use ui::tilo_resource::{Atom, Slot, evolution, write_key};
    let mut doc = ui::tilo_resource::Doc::new();
    let root = automerge::ROOT;
    for (tag, url, title) in [
        ("bookmark:a", "https://kept.test/", "Kept"),
        ("bookmark:b", "https://gone.test/", "Gone"),
    ] {
        let add = Slot::Key(&root, &format!("browser.bookmarks.v2.add:{tag}"))
            .object(&mut doc, automerge::ObjType::Map)
            .unwrap();
        write_key(&mut doc, &add, "url", &Atom(url.into())).unwrap();
        write_key(&mut doc, &add, "title", &Atom(title.into())).unwrap();
    }
    write_key(
        &mut doc,
        &root,
        "browser.bookmarks.v2.remove:bookmark:b",
        &true,
    )
    .unwrap();
    let tree = ImportedTree {
        source: [9; 32],
        nodes: vec![
            ImportedNode {
                id: "root".into(),
                parent: None,
                position: 0,
                title: "Imported".into(),
                kind: NodeKind::Folder,
            },
            ImportedNode {
                id: "leaf".into(),
                parent: Some("root".into()),
                position: 0,
                title: "Leaf".into(),
                kind: NodeKind::Bookmark {
                    url: "https://kept.test/".into(),
                },
            },
        ],
    };
    let source = crate::imported_folders::source_id(&tree.source);
    write_key(
        &mut doc,
        &root,
        &format!("browser.import.bookmarks:{source}"),
        &Atom("digest".into()),
    )
    .unwrap();
    write_key(
        &mut doc,
        &root,
        &format!("browser.imported-tree:{source}"),
        &Atom(serde_json::to_string(&tree).unwrap()),
    )
    .unwrap();
    write_key(
        &mut doc,
        &root,
        &format!(
            "browser.imported-tree-removed:{}",
            crate::imported_folders::node_key(&tree.source, "leaf")
        ),
        &true,
    )
    .unwrap();
    doc.commit();

    assert!(
        evolution::read_checked::<PersonalBookmarks>(&doc).is_err(),
        "an unmigrated library never reads as the record"
    );
    assert!(evolution::migrate::<PersonalBookmarks>(&mut doc).unwrap());
    let library = evolution::read_checked::<PersonalBookmarks>(&doc).unwrap();
    let urls: Vec<String> = library
        .bookmarks()
        .entries()
        .iter()
        .map(|bookmark| bookmark.url.clone())
        .collect();
    assert_eq!(
        urls,
        ["https://kept.test/"],
        "the removed add stays removed"
    );
    assert_eq!(
        library.trees.get(&source),
        Some(&tree),
        "the tree is kept whole"
    );
    let shown: Vec<String> = library.imported_trees()[0]
        .nodes
        .iter()
        .map(|node| node.id.clone())
        .collect();
    assert_eq!(shown, ["root"], "the removed placement stays removed");
    assert_eq!(
        library
            .receipts
            .get(&source)
            .map(|digest| digest.0.as_str()),
        Some("digest"),
        "a retried import is still recognised"
    );
    assert!(
        doc.keys(root).all(|key| !key.starts_with("browser.")),
        "nothing stays under the old keys"
    );
}
