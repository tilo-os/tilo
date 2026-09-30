//! The word processor on the app framework: the real component runtime,
//! record storage, permissions, per-author undo and History mode, driven the
//! way a person drives the editor — by typing into it.

use tilo_document::RichText;
use ui::framework::testing::TestApp;
use ui::permissions::Role;
use word_processor::framework::WordProcessorApp;

fn paragraphs(app: &TestApp<WordProcessorApp>) -> Vec<String> {
    app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.text)
            .collect()
    })
}

fn open() -> TestApp<WordProcessorApp> {
    ui::i18n::I18nManager::init("en-US");
    TestApp::<WordProcessorApp>::new().unwrap()
}

#[test]
fn a_keystroke_is_an_action_and_typing_undoes_a_word_at_a_time() {
    let mut app = open();
    app.harness().type_text("hello world");
    assert_eq!(paragraphs(&app), ["hello world"]);
    let typed = app.versions().len();
    assert!(typed > 1, "every keystroke is an attributed change");
    let last = app.versions().pop().unwrap();
    let context = last.context.as_ref().unwrap();
    assert_eq!(context.action_label, "Splice Text");
    assert_eq!(
        context
            .resource
            .as_ref()
            .map(|resource| resource.kind.as_str()),
        Some("tilo.rich-text")
    );

    // A word is one step: undo takes back "world", redo restores it, and a
    // second undo takes back "hello ".
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["hello "]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app), ["hello world"]);
    assert!(app.undo());
    assert!(app.undo());
    assert_eq!(paragraphs(&app), [""]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app), ["hello "]);
}

#[test]
fn enter_splits_the_paragraph_as_its_own_step() {
    let mut app = open();
    app.harness().type_text("first");
    app.harness().key("Enter");
    app.harness().type_text("second");
    assert_eq!(paragraphs(&app), ["first", "second"]);
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["first", ""]);
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["first"]);
}

#[test]
fn a_viewer_is_refused_and_the_document_is_untouched() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<WordProcessorApp>::with_role(Role::Viewer).unwrap();
    let before = app.versions().len();
    app.harness().type_text("sneaky");
    app.harness().key("Enter");
    assert_eq!(paragraphs(&app), [""]);
    assert_eq!(app.versions().len(), before, "nothing was written");
}

#[test]
fn history_draws_a_past_version_with_the_editors_own_layout() {
    let mut app = open();
    app.harness().type_text("First draft");
    app.flush().unwrap();
    let past = app.read_document(|doc| doc.clone().get_heads());
    app.harness().key("Enter");
    app.harness().type_text("Added later");
    assert_eq!(paragraphs(&app), ["First draft", "Added later"]);

    let version = app.past_version(&past).unwrap();
    let shown = version.texts().join("\n");
    assert!(shown.contains("First draft"), "{shown}");
    assert!(!shown.contains("Added later"), "{shown}");
    assert!(version.size().width() > 0.0, "the page was laid out");
    assert_eq!(version.state().body().len(), 1);
}

#[test]
fn the_document_survives_a_relaunch() {
    let mut app = open();
    app.harness().type_text("kept");
    let app = app.reopen().unwrap();
    assert_eq!(paragraphs(&app), ["kept"]);
    assert_eq!(
        <RichText as ui::tilo_resource::Resource>::KIND,
        "tilo.rich-text"
    );
}

#[test]
fn one_apply_creates_a_filled_table_and_one_undo_takes_it_back() {
    use tilo_document::action::insert_filled_table_action;
    let mut app = open();
    app.harness().type_text("These are the people");
    app.flush().unwrap();
    let after = app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c).remove(0)
    });
    let cells: Vec<Vec<String>> = [["Name", "Role"], ["Priya Natarajan", "Product lead"]]
        .iter()
        .map(|row| row.iter().map(|&text| text.to_owned()).collect())
        .collect();
    let versions = app.versions().len();
    app.invoke_confirmed(
        ui::tilo_resource::Invoke::<RichText>::apply(&insert_filled_table_action(&after, cells))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(app.versions().len(), versions + 1, "one change");
    assert_eq!(
        paragraphs(&app),
        [
            "These are the people",
            "Name",
            "Role",
            "Priya Natarajan",
            "Product lead"
        ]
    );
    let kinds = app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c)
            .blocks
            .into_iter()
            .map(|block| block.kind)
            .collect::<Vec<_>>()
    });
    assert_eq!(kinds, ["paragraph", "cell", "cell", "cell", "cell"]);

    // One step: the whole table, cells and text, goes with one undo.
    assert!(app.undo());
    assert_eq!(paragraphs(&app), ["These are the people"]);
    assert!(app.redo());
    assert_eq!(paragraphs(&app).len(), 5);
}

/// A record holding `paragraphs` paragraphs, pasted through the editor's own
/// paste action.
fn long_document(paragraphs: usize) -> TestApp<WordProcessorApp> {
    use tilo_document::ClipBlock;
    use tilo_document::action::{RegionKind, RichTextAction};
    let mut app = open();
    let blocks: Vec<ClipBlock> = (0..paragraphs)
        .map(|n| ClipBlock {
            kind: if n % 40 == 0 { "heading" } else { "paragraph" }.into(),
            text: format!("Paragraph {n}: the rehearsal ran long and the doors opened late."),
            marks: Vec::new(),
            level: (n % 40 == 0).then_some(2),
            ordered: None,
            align: None,
            attrs: std::collections::BTreeMap::default(),
        })
        .collect();
    let first = app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c).remove(0)
    });
    let paste = RichTextAction::PasteFragment {
        region: RegionKind::Body,
        start_block: first.clone(),
        start: 0,
        end_block: first,
        end: 0,
        blocks,
        adopt_first_style: false,
    };
    app.invoke_confirmed(ui::tilo_resource::Invoke::<RichText>::apply(&paste).unwrap())
        .unwrap();
    app
}

/// The median time of a run of keystrokes.
fn keystroke_cost(app: &mut TestApp<WordProcessorApp>) -> std::time::Duration {
    app.harness().type_text("warm ");
    let mut costs: Vec<_> = "typing along"
        .chars()
        .map(|ch| {
            let started = ui::time::now();
            app.harness().key(&ch.to_string());
            started.elapsed()
        })
        .collect();
    costs.sort();
    costs[costs.len() / 2]
}

#[test]
fn a_keystroke_in_a_long_document_costs_what_it_costs_in_a_short_one() {
    let mut short = long_document(200);
    let mut long = long_document(4_000);
    let short_cost = keystroke_cost(&mut short);

    // Typing and Enter in the long document never list or sort its
    // paragraphs, and read only the paragraphs around the page on screen.
    let scans = tilo_doc::model::ordered_keys_scan_count();
    let long_cost = keystroke_cost(&mut long);
    long.harness().key("Enter");
    long.harness().type_text("new line");
    assert_eq!(
        tilo_doc::model::ordered_keys_scan_count(),
        scans,
        "a keystroke listed the document's paragraphs"
    );
    let text = paragraphs(&long);
    assert_eq!(text.len(), 4_001, "{:?}", &text[..3]);
    assert_eq!(text[0], "warm typing along");
    assert!(
        text[1].starts_with("new lineParagraph 0"),
        "{:?}",
        &text[..3]
    );

    // Cost independent of size: twenty times the paragraphs, well under
    // twice the time (slack for a loaded machine).
    assert!(
        long_cost < short_cost * 3,
        "a keystroke cost {long_cost:?} in 4000 paragraphs against {short_cost:?} in 200"
    );
}

/// The document as stored, formatting included.
fn snapshot(app: &TestApp<WordProcessorApp>) -> tilo_doc::DocSnapshot {
    app.read_document(|doc| {
        let c = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &c)
    })
}

#[test]
fn undoing_a_mark_over_several_paragraphs_clears_it_from_every_one() {
    use tilo_document::action::{MarkVal, RegionKind, RichTextAction, Span};
    use ui::tilo_resource::Invoke;
    let mut app = open();
    let load = RichTextAction::ReplaceBody {
        markdown: "Background\n\nMethod\n\nResults\n".into(),
    };
    app.invoke_confirmed(Invoke::<RichText>::apply(&load).unwrap())
        .unwrap();
    let before = snapshot(&app);
    let spans = before
        .blocks
        .iter()
        .map(|block| Span {
            block: block.id.clone(),
            start: 0,
            end: block.text.chars().count(),
        })
        .collect();
    let bold = RichTextAction::SetMark {
        region: RegionKind::Body,
        spans,
        name: "bold".into(),
        value: Some(MarkVal::Bool(true)),
        also_clear: Vec::new(),
    };
    // Through `group`, the door an agent uses for a multi-part edit.
    app.invoke_confirmed(Invoke::group([Invoke::<RichText>::apply(&bold).unwrap()]))
        .unwrap();
    let marked = snapshot(&app);
    assert_eq!(marked.blocks.len(), 3);
    for block in &marked.blocks {
        assert!(
            block.marks.iter().any(|mark| mark.name == "bold"),
            "{block:?}"
        );
    }

    assert!(app.undo());
    assert_eq!(snapshot(&app).blocks, before.blocks);
}

/// What the History preview pane shows, as assistive technology reads it.
fn preview_text(app: &mut TestApp<WordProcessorApp>) -> String {
    fn find(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
        if node.test_id.as_deref() == Some("history-preview") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let inspection = app.harness().inspection();
    find(&inspection.root)
        .and_then(|node| node.semantics.as_ref())
        .and_then(|semantics| semantics.name.clone())
        .expect("History shows a preview of the selected version")
}

fn heads(app: &TestApp<WordProcessorApp>) -> Vec<automerge::ChangeHash> {
    app.read_document(|doc| doc.clone().get_heads())
}

fn targets(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
    out.extend(node.action_target.clone());
    for child in &node.children {
        targets(child, out);
    }
}

fn timeline(node: &ui::headless::InspectionNode) -> Option<&ui::headless::InspectionNode> {
    if node.test_id.as_deref() == Some("activity_timeline") {
        return Some(node);
    }
    node.children.iter().find_map(timeline)
}

fn action_ids(resource: &tilo_actions::Resource, out: &mut Vec<String>) {
    out.extend(resource.actions.iter().map(|action| action.id.clone()));
    for child in &resource.children {
        action_ids(child, out);
    }
}

/// Open History and select the change whose result is `version`.
fn select_version(app: &mut TestApp<WordProcessorApp>, version: &[automerge::ChangeHash]) {
    let [hash] = version else {
        panic!("a single-author edit leaves one head");
    };
    if !app.harness().targets().contains_key("history-back-live") {
        app.harness().click_named("history-open").unwrap();
    }
    let entry = format!("timeline-entry:{}", &hash.to_string()[..32]);
    // A typed word is a run of keystrokes; expand runs until the change shows.
    for _ in 0..4 {
        let inspection = app.harness().inspection();
        let mut published = Vec::new();
        if let Some(semantics) = timeline(&inspection.root).and_then(|node| node.semantics.as_ref())
        {
            targets(semantics, &mut published);
        }
        if published.contains(&entry) {
            break;
        }
        for group in published
            .iter()
            .filter(|target| target.starts_with("timeline-group:"))
        {
            let _ = app.harness().perform_semantic_named(
                "activity_timeline",
                Some(group),
                &ui::a11y::Action::Expand,
            );
        }
        app.harness().relayout();
    }
    app.harness()
        .perform_semantic_named(
            "activity_timeline",
            Some(&entry),
            &ui::a11y::Action::Activate,
        )
        .unwrap();
    app.harness().relayout();
}

fn confirm_restore(app: &mut TestApp<WordProcessorApp>) {
    let confirm = app
        .harness()
        .a11y_node_named("Restore version")
        .expect("the review offers Restore version");
    app.harness()
        .perform_a11y_action(confirm, &ui::a11y::Action::Activate)
        .unwrap();
    app.harness().relayout();
}

#[test]
fn history_previews_a_version_and_restores_it_like_a_revert() {
    let mut app = open();
    app.harness().type_text("alpha");
    app.flush().unwrap();
    let v1 = heads(&app);
    app.harness().type_text(" beta");
    app.flush().unwrap();
    let v2 = heads(&app);
    app.harness().type_text(" gamma");
    app.flush().unwrap();
    let v3 = heads(&app);
    assert_eq!(paragraphs(&app), ["alpha beta gamma"]);
    let mut actions = Vec::new();
    for resource in app.harness().resources() {
        action_ids(&resource, &mut actions);
    }
    assert!(actions.iter().any(|id| id == "outline"), "{actions:?}");
    assert!(
        !actions.iter().any(|id| id == "history"),
        "the editor's own version strip is not offered beside the shared History"
    );
    assert!(app.harness().targets().contains_key("history-open"));

    select_version(&mut app, &v1);
    let shown = preview_text(&mut app);
    assert!(shown.contains("alpha"), "{shown}");
    assert!(
        !shown.contains("beta"),
        "the preview is v1, not live: {shown}"
    );
    assert_eq!(
        paragraphs(&app),
        ["alpha beta gamma"],
        "previewing writes nothing"
    );
    assert_eq!(heads(&app), v3);

    let before = app.versions().len();
    app.harness().click_named("history-restore").unwrap();
    confirm_restore(&mut app);
    assert_eq!(paragraphs(&app), ["alpha"]);
    let versions = app.versions();
    assert_eq!(versions.len(), before + 1, "one forward change");
    for kept in [&v2[0], &v3[0]] {
        assert!(
            versions.iter().any(|version| version.hash == *kept),
            "later versions stay in history"
        );
    }

    // Back in the live document, Ctrl+Z takes the restore back.
    app.harness().click_named("history-back-live").unwrap();
    assert!(app.undo(), "Ctrl+Z reverses the restore");
    assert_eq!(paragraphs(&app), ["alpha beta gamma"]);
}

#[test]
fn a_stale_review_and_a_viewer_cannot_restore() {
    let mut app = open();
    app.harness().type_text("alpha");
    app.flush().unwrap();
    let v1 = heads(&app);
    app.harness().type_text(" beta");
    app.flush().unwrap();

    select_version(&mut app, &v1);
    app.harness().click_named("history-restore").unwrap();
    app.merge_peer_edit(|doc| {
        use automerge::transaction::Transactable;
        doc.put(automerge::ROOT, "peer_note", "arrived during review")
            .unwrap();
    });
    let moved = heads(&app);
    confirm_restore(&mut app);
    assert_eq!(heads(&app), moved, "a stale review writes nothing");
    assert_eq!(paragraphs(&app), ["alpha beta"]);
    assert!(
        app.harness().a11y_node_named("Refresh review").is_some(),
        "the review asks to be refreshed"
    );

    ui::i18n::I18nManager::init("en-US");
    let mut viewer = TestApp::<WordProcessorApp>::with_role(Role::Viewer).unwrap();
    viewer.merge_peer_edit(|doc| {
        use automerge::transaction::Transactable;
        doc.put(automerge::ROOT, "peer_note", "first").unwrap();
    });
    let first = heads(&viewer);
    viewer.merge_peer_edit(|doc| {
        use automerge::transaction::Transactable;
        doc.put(automerge::ROOT, "peer_note", "second").unwrap();
    });
    let latest = heads(&viewer);
    select_version(&mut viewer, &first);
    viewer.harness().click_named("history-restore").unwrap();
    confirm_restore(&mut viewer);
    assert_eq!(heads(&viewer), latest, "a viewer's restore writes nothing");
    assert!(
        viewer
            .harness()
            .inspection_json()
            .unwrap()
            .contains("no longer have permission"),
        "the refusal names the missing permission"
    );
}

/// A computer's session configures the collaboration service for every app
/// it launches, and a hosted editor has no file record of its own: `open`
/// with a path imports the file into the hosted document, and the import is
/// kept like any other edit.
#[test]
fn open_with_a_path_imports_the_file_under_a_configured_collaboration_service() {
    const CHILD: &str = "TILO_WRITER_HOSTED_OPEN_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "framework_host::open_with_a_path_imports_the_file_under_a_configured_collaboration_service",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("TILO_SVC_COLLAB_FD", "-1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "isolated hosted open failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let directory = std::env::temp_dir().join(format!("tilo-hosted-open-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("report.md");
    std::fs::write(&path, "# Report\n\nOur comittee met.\n").unwrap();

    let mut app = open();
    app.harness().type_text("draft");
    let mut arguments = serde_json::Map::new();
    arguments.insert("path".into(), path.to_string_lossy().into_owned().into());
    app.harness()
        .invoke_action(
            &tilo_actions::ActionAddress::new("word-processor.document", "open"),
            &arguments,
            true,
            None,
        )
        .unwrap();
    for _ in 0..20 {
        if paragraphs(&app) == ["Report", "Our comittee met."] {
            break;
        }
        app.harness().relayout();
    }
    assert_eq!(paragraphs(&app), ["Report", "Our comittee met."]);
    let app = app.reopen().unwrap();
    assert_eq!(paragraphs(&app), ["Report", "Our comittee met."]);
    std::fs::remove_dir_all(directory).unwrap();
}
