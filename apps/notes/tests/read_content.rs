//! A notebook is read through its published `read` action, as a script or an
//! agent reads it: the pages it holds, then one page's title and blocks.

use automerge::transaction::Transactable;
use notes::NotesShell;
use notes::read;
use serde_json::json;
use tilo_document::RichTextAction;
use ui::framework::testing::TestApp;
use ui::permissions::Role;

fn read(app: &mut TestApp<NotesShell>, arguments: serde_json::Value) -> serde_json::Value {
    app.call("read", arguments)
        .unwrap_or_else(|error| panic!("read refused: {error:?}"))
        .expect("read answers a value")
}

#[test]
fn a_note_written_through_actions_reads_back_through_actions() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", serde_json::json!({ "title": "Groceries" }))
        .unwrap();

    let notebook = read(&mut app, json!({}));
    assert_eq!(notebook["kind"], "notes.notebook");
    let pages = notebook["pages"].as_array().expect("pages");
    let page = pages
        .iter()
        .find(|page| page["title"] == "Groceries")
        .unwrap_or_else(|| panic!("the new note is not listed: {notebook}"));
    let item = page["item"].as_str().expect("item id").to_owned();

    let id = app.read_document(|doc| {
        read::list_pages(doc)
            .into_iter()
            .find(|page| page.title == "Groceries")
            .expect("page")
            .id
    });
    crate::framework_support::edit_body(
        &mut app,
        id,
        &RichTextAction::AppendParagraph {
            text: "oat milk".into(),
        },
    );

    let note = read(&mut app, json!({ "item": item }));
    assert_eq!(note["kind"], "notes.page");
    assert_eq!(note["title"], "Groceries");
    let markdown = note["markdown"].as_str().expect("the body as Markdown");
    let line = markdown
        .lines()
        .find(|line| line.starts_with("oat milk ^"))
        .unwrap_or_else(|| panic!("{note}"));
    assert!(
        line.len() > "oat milk ^".len(),
        "the line ends in its block's id: {line}"
    );
    assert_eq!(note["blocks"], 1, "{note}");
    assert_eq!(
        note["body"],
        format!("{}/body", notes::actions::page_resource_id(id))
    );

    let missing = app.call("read", json!({ "item": "notes.page:not-a-page" }));
    assert!(missing.is_err(), "an unknown note is refused: {missing:?}");
}

/// Reading needs read permission only: a viewer reads, as a viewer sees.
#[test]
fn a_viewer_reads_the_notebook() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::with_role(Role::Viewer).unwrap();
    let notebook = read(&mut app, json!({}));
    assert!(notebook["pages"].is_array(), "{notebook}");
}

/// `read` is published as a read: a view tagged as the document read, so a
/// caller that just opened Notes finds it without knowing the app.
#[test]
fn read_is_published_as_the_notebooks_document_read() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    let resources = app.harness().resources();
    let notebook = resources
        .iter()
        .find(|resource| resource.id.as_str() == "notes.notebook")
        .expect("the notebook is published");
    let read = notebook
        .actions
        .iter()
        .find(|action| action.id == "read")
        .expect("the notebook publishes read");
    assert_eq!(read.kind, tilo_actions::OperationKind::View);
    assert!(read.contract.reads_document());
}

#[test]
fn note_and_body_reads_are_bound_document_projections() {
    fn find<'a>(
        resources: &'a [tilo_actions::Resource],
        id: &str,
    ) -> Option<&'a tilo_actions::Resource> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource)
            } else {
                find(&resource.children, id)
            }
        })
    }
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call("new-note", json!({"title": "Public note"}))
        .unwrap();
    app.settle();
    let id = crate::framework_support::shown(&app).pages()[0].id;
    let resources = app.harness().resources();
    for id in [
        notes::actions::page_resource_id(id),
        notes::actions::body_resource_id(id),
    ] {
        let resource = find(&resources, &id).expect("published document child");
        let read = resource
            .actions
            .iter()
            .find(|a| a.id == "read")
            .expect("child read");
        assert!(read.contract.reads_document());
        let encoded = read.build_json(&serde_json::Map::new()).unwrap();
        let payload: serde_json::Value = tilo_actions::decode_json(&encoded.payload).unwrap();
        assert_eq!(payload["item"], id);
        assert!(
            read.build_json(json!({"item": "private-account"}).as_object().unwrap())
                .is_err()
        );
        assert!(app.call("read", json!({"item": id})).is_ok());
    }
}

fn inspect_editor(app: &mut TestApp<NotesShell>) -> serde_json::Value {
    app.call("inspect-editor", json!({}))
        .expect("the read-only editor action is published")
        .expect("the action returns its observation")
}

#[test]
fn editor_inspection_reuses_the_native_observer_and_writes_nothing() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call(
        "new-note",
        serde_json::json!({ "title": "Private fixture title" }),
    )
    .unwrap();
    let page = crate::framework_support::shown(&app).pages()[0].id;
    crate::framework_support::edit_body(
        &mut app,
        page,
        &RichTextAction::AppendParagraph {
            text: "Private body fixture".into(),
        },
    );
    app.settle();
    let canonical_body = app.read_document(|doc| {
        read::page_block_snapshots(doc, page)
            .into_iter()
            .map(|(_, text)| text)
            .collect::<Vec<_>>()
            .join("\n")
    });
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let native = app
        .harness()
        .native_input_snapshot()
        .expect("native Notes observation");
    let observed = inspect_editor(&mut app);
    assert_eq!(observed, native, "one observer serves both existing doors");
    assert_eq!(observed["selected_page"], observed["main_page"]);
    assert_eq!(observed["main_page"], observed["editor_binding_page"]);
    assert_eq!(observed["body_hash"], observed["rendered_body_hash"]);
    assert_eq!(
        observed["body_hash"],
        blake3::hash(canonical_body.as_bytes()).to_hex().to_string()
    );
    assert_eq!(observed["body_chars"], canonical_body.chars().count());
    assert!(observed["body_chars"].as_u64().unwrap() > 0);
    assert!(!observed.to_string().contains("Private body fixture"));
    assert!(observed.get("caret").is_some());
    assert!(observed.get("editor_focused").is_some());
    for flag in [
        "active_line_present",
        "active_line_focused",
        "active_line_disabled",
        "editor_read_only",
        "editor_composing",
        "editor_route_available",
        "sidebar_renaming",
        "sidebar_menu_open",
        "editor_menu_open",
    ] {
        assert!(observed[flag].is_boolean(), "typed routing flag {flag}");
    }
    assert!(observed.get("text").is_none());
    assert!(observed.get("body").is_none());
    assert!(!observed.to_string().contains("Private fixture title"));
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before
    );
    assert_eq!(inspect_editor(&mut app), observed);
}

#[test]
fn editor_inspection_preserves_actual_wrong_page_and_stale_cache_evidence() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.call(
        "new-note",
        serde_json::json!({ "title": "First private fixture" }),
    )
    .unwrap();
    let first = crate::framework_support::shown(&app).pages()[0].id;
    app.call(
        "new-note",
        serde_json::json!({ "title": "Second private fixture" }),
    )
    .unwrap();
    let second = crate::framework_support::shown(&app)
        .pages()
        .iter()
        .find(|page| page.id != first)
        .unwrap()
        .id;
    app.call("open-note", json!({ "page": second }))
        .expect("the second page opens through its actual published row");
    app.settle();
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let canonical = app.read_document(Clone::clone);
    // A real panel binding can lag or disagree with the local selected route.
    // Inspection must report that state rather than rebinding it to a target.
    app.surface_mut::<notes::NotesApp>()
        .main_panel_mut()
        .bind_canonical_page(&canonical, first, "First private fixture")
        .unwrap();
    let observed = inspect_editor(&mut app);
    assert_eq!(observed["selected_page"], second.to_string());
    assert_eq!(observed["editor_binding_page"], second.to_string());
    assert_eq!(observed["main_page"], first.to_string());
    assert_ne!(observed["main_page"], observed["selected_page"]);
    // The production panel loader establishes a rendered cache independently
    // of the canonical body. Do not substitute this cache for the document.
    app.surface_mut::<notes::NotesApp>()
        .main_panel_mut()
        .load_page(second, "Second private fixture", "Stale private cache");
    let cached = inspect_editor(&mut app);
    assert_eq!(cached["selected_page"], second.to_string());
    assert_eq!(cached["main_page"], second.to_string());
    assert_ne!(cached["body_hash"], cached["rendered_body_hash"]);
    assert_eq!(cached["body_chars"], 0);
    assert_eq!(
        cached["rendered_body_hash"],
        blake3::hash(b"Stale private cache").to_hex().to_string()
    );
    assert!(!cached.to_string().contains("Stale private cache"));
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before
    );
    assert!(
        app.call("inspect-editor", json!({"page":first})).is_err(),
        "a caller cannot inject the page identity to be reported"
    );
}

#[test]
fn a_viewer_inspects_editor_without_gaining_note_mutation_rights() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::with_role(Role::Viewer).unwrap();
    let before = app.read_document(|doc| (doc.base_heads(), doc.pending_ops()));
    let observed = inspect_editor(&mut app);
    assert!(observed["selected_page"].is_null());
    assert!(observed["main_page"].is_null());
    assert!(observed["body_hash"].is_null());
    assert!(
        app.call("new-note", serde_json::json!({ "title": "Not permitted" }))
            .is_err()
    );
    assert_eq!(inspect_editor(&mut app), observed);
    assert_eq!(
        app.read_document(|doc| (doc.base_heads(), doc.pending_ops())),
        before
    );
}
