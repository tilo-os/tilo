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
    assert_eq!(original.imported_trees[0].nodes.len(), 64);
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
        !state.imported_trees[0]
            .nodes
            .iter()
            .any(|node| node.id == "first")
    );
    assert!(
        state.imported_trees[0]
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
fn assert_folder_controls(app: &mut TestApp<TestBrowser>, bundle: &ImportBundle) {
    use crate::personal_bookmark_view::ViewRequest;
    let record = app.person::<PersonalBookmarks>().unwrap();
    let state = record.get();
    let next = crate::browser_widget::personal::request_resource(
        &state,
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
