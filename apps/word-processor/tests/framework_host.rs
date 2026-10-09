//! The word processor on the app framework: the real component runtime,
//! record storage, permissions, per-author undo and History mode, driven the
//! way a person drives the editor — by typing into it.

use ui::framework::testing::TestApp;
use ui::permissions::Role;
use word_processor::framework::{Document, WordProcessorApp};

fn paragraphs(app: &TestApp<WordProcessorApp>) -> Vec<String> {
    app.read_document(|doc| {
        let c = word_processor::model::open(doc).unwrap();
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
    assert!(
        matches!(&context.what, Some(ui::tilo_actions::Did::Edit(edit)) if edit.field == "body"),
        "{context:?}"
    );
    assert_eq!(
        context
            .resource
            .as_ref()
            .map(|resource| resource.kind.as_str()),
        Some("word-processor.document")
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
    assert_eq!(version.state().text().unwrap().body().len(), 1);
}

#[test]
fn the_document_survives_a_relaunch() {
    let mut app = open();
    app.harness().type_text("kept");
    let app = app.reopen().unwrap();
    assert_eq!(paragraphs(&app), ["kept"]);
    assert_eq!(
        <Document as ui::tilo_resource::Resource>::KIND,
        "word-processor.document"
    );
}

#[test]
fn one_apply_creates_a_filled_table_and_one_undo_takes_it_back() {
    use tilo_document::action::insert_filled_table_action;
    let mut app = open();
    app.harness().type_text("These are the people");
    app.flush().unwrap();
    let after = app.read_document(|doc| {
        let c = word_processor::model::open(doc).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c).remove(0)
    });
    let cells: Vec<Vec<String>> = [["Name", "Role"], ["Priya Natarajan", "Product lead"]]
        .iter()
        .map(|row| row.iter().map(|&text| text.to_owned()).collect())
        .collect();
    let versions = app.versions().len();
    app.invoke_confirmed(
        ui::tilo_resource::Invoke::<Document>::edit(&word_processor::model::edit(
            &insert_filled_table_action(&after, cells),
        ))
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
        let c = word_processor::model::open(doc).unwrap();
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
        let c = word_processor::model::open(doc).unwrap();
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
    app.invoke_confirmed(
        ui::tilo_resource::Invoke::<Document>::edit(&word_processor::model::edit(&paste)).unwrap(),
    )
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
        let c = word_processor::model::open(doc).unwrap();
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
    app.invoke_confirmed(Invoke::<Document>::edit(&word_processor::model::edit(&load)).unwrap())
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
    app.invoke_confirmed(Invoke::group([Invoke::<Document>::edit(
        &word_processor::model::edit(&bold),
    )
    .unwrap()]))
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

fn history_labels(resource: &tilo_actions::Resource, out: &mut Vec<String>) {
    out.extend(
        resource
            .actions
            .iter()
            .filter(|action| action.id == "history")
            .map(|action| action.label.clone()),
    );
    for child in &resource.children {
        history_labels(child, out);
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
    let mut labels = Vec::new();
    for resource in app.harness().resources() {
        history_labels(&resource, &mut labels);
    }
    assert_eq!(
        labels,
        ["History"],
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

/// A launch naming a file opens the file's own record with the file
/// imported; launched on it again, the record — with the edits made since —
/// is the document, and another file is another record.
#[test]
fn a_file_launch_opens_the_files_own_record() {
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("report.md");
    std::fs::write(&report, "# Report\n\nOur committee met.\n").unwrap();
    let report = report.to_string_lossy().into_owned();

    let mut app = TestApp::<WordProcessorApp>::launched(&[&report]).unwrap();
    assert_eq!(app.record(), &ui::record::RecordId::for_file(&report));
    assert_eq!(paragraphs(&app), ["Report", "Our committee met."]);
    app.harness().type_text("Draft: ");
    let edited = paragraphs(&app);
    assert_ne!(edited, ["Report", "Our committee met."]);

    let app = app.relaunch(&[&report]).unwrap();
    assert_eq!(paragraphs(&app), edited, "a reopened file keeps its record");

    let notes = directory.path().join("notes.txt");
    std::fs::write(&notes, "first line\nsecond line\n").unwrap();
    let notes = notes.to_string_lossy().into_owned();
    let app = app.relaunch(&[&notes]).unwrap();
    assert_eq!(app.record(), &ui::record::RecordId::for_file(&notes));
    assert_eq!(paragraphs(&app), ["first line", "second line"]);
}

/// Every screenshot scenario stages the document through actions the
/// record keeps: the review a collaborator left, a framed image.
#[test]
fn scenarios_stage_the_document_through_actions() {
    ui::i18n::I18nManager::init("en-US");
    for scenario in <WordProcessorApp as ui::framework::ResourceApp>::scenarios() {
        let app = TestApp::<WordProcessorApp>::capture(Some(scenario)).unwrap();
        let document = snapshot(&app);
        match *scenario {
            "comments" | "threaded-comments" => {
                let threads = app.read_document(|doc| {
                    <Document as ui::tilo_resource::Resource>::read(doc)
                        .unwrap()
                        .comment_views(doc)
                });
                let comment = threads.first().expect("a staged comment");
                assert!(comment.range.is_some(), "it is about some text");
                assert_eq!(comment.author, "Maya Chen");
                let replies = if *scenario == "comments" { 0 } else { 2 };
                assert_eq!(comment.replies.len(), replies, "{scenario}");
                assert!(
                    document.blocks[0].text.ends_with(" — reviewed"),
                    "a tracked insertion: {:?}",
                    document.blocks[0].text
                );
            }
            "image-object" => {
                let image = document
                    .blocks
                    .iter()
                    .find(|block| block.kind == "image")
                    .expect("a staged image");
                assert_eq!(
                    image.attrs.get("width"),
                    Some(&tilo_doc::snapshot::AttrVal::Int(300))
                );
            }
            "find" => assert!(
                document
                    .blocks
                    .iter()
                    .any(|block| block.text.contains("report")),
                "the find scenario searches text it has"
            ),
            _ => assert_eq!(
                document
                    .blocks
                    .iter()
                    .map(|block| block.text.as_str())
                    .collect::<Vec<_>>(),
                [""]
            ),
        }
    }
}

/// Explicit local words follow the shared host dictionary in every window;
/// application settings no longer create a separate vocabulary store.
#[test]
fn the_persons_own_words_are_spelled_right_in_every_window() {
    use settings_store::personal_dictionary::{Service, host::Host};
    use std::sync::Arc;
    use ui::personal_dictionary::{Edit, Word};
    use word_processor::{PersonalDictionary, WordProcessorWidget};
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            ui::local_identity::install_source(None);
        }
    }
    let _guard = tilo_test_env::lock();
    let _restore = Restore;
    ui::local_identity::install_source(Some(Arc::new(ui::local_identity::FixedIdentity(
        ui::local_identity::LocalIdentity {
            group_public_key: [1; 32],
            display_name: String::new(),
            avatar_color: [0; 3],
        },
    ))));
    let profile = tempfile::tempdir().unwrap();
    let service = Arc::new(Service::new(profile.path().join("dictionary")).unwrap());
    let source = Arc::new(Host::new(
        Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        )),
        service,
        Arc::new(|| Some([1; 32])),
    ));
    let mut app = TestApp::<WordProcessorApp>::for_person(&"01".repeat(32)).unwrap();
    let mut host =
        ui::personal_dictionary::TestHost::new(source, "so.tilo.word-processor", [1; 32]).unwrap();
    let bias_reader = ui::personal_dictionary::reader();
    let flagged =
        app.with_surface(|editor: &mut WordProcessorWidget| !editor.is_spelled_right("qwordz"));
    assert_eq!(flagged, Some(true), "an unknown word is flagged");
    ui::personal_dictionary::edit(0, Edit::Add(Word::new("qwordz", None).unwrap())).unwrap();
    host.flush().unwrap();
    let mut app = app.reopen().unwrap();
    let known =
        app.with_surface(|editor: &mut WordProcessorWidget| editor.is_spelled_right("qwordz"));
    assert_eq!(known, Some(true), "the person's own word is known");
    assert_eq!(
        bias_reader
            .snapshot()
            .unwrap()
            .hints("en")
            .collect::<Vec<_>>(),
        ["qwordz"],
        "another consumer sees the same explicit addition"
    );
    assert!(
        app.settings::<PersonalDictionary>()
            .get()
            .dictionary_words
            .is_empty()
    );
}

/// The edits bound to the caret are published on the document itself, and
/// invoking one there builds it at the caret.
#[test]
fn an_edit_bound_to_the_caret_is_invoked_on_the_document() {
    let mut app = open();
    app.harness().type_text("Hello");
    let mut arguments = serde_json::Map::new();
    arguments.insert("text".into(), " world".into());
    app.harness()
        .invoke_action(
            &tilo_actions::ActionAddress::new("word-processor.document", "type-text"),
            &arguments,
            true,
            None,
        )
        .unwrap();
    app.harness().relayout();
    assert_eq!(paragraphs(&app), ["Hello world"]);
}

/// Two devices that each open the same file before they have synced create
/// its record each, and converge on one document: the file's content once,
/// and what each of them wrote.
#[test]
fn two_sessions_creating_one_files_record_converge_on_one_document() {
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let report = directory.path().join("report.md");
    std::fs::write(&report, "# Report\n\nOur committee met.\n").unwrap();
    let report = report.to_string_lossy().into_owned();

    let mut here = TestApp::<WordProcessorApp>::launched(&[&report]).unwrap();
    let mut there = TestApp::<WordProcessorApp>::launched(&[&report]).unwrap();
    assert_eq!(here.record(), there.record());
    here.harness().type_text("Here: ");
    there.harness().key("End");
    there.harness().type_text(" There.");
    let mut theirs = there.peer_document();
    here.merge_peer(&mut theirs).unwrap();
    let merged = paragraphs(&here);
    assert_eq!(merged.len(), 2, "the file's paragraphs once: {merged:?}");
    assert!(merged[0].starts_with("Here: "), "{merged:?}");
    assert!(merged[0].contains("Report"), "{merged:?}");
    assert!(merged[0].ends_with(" There."), "{merged:?}");
}

/// A file changed elsewhere since its record last read or wrote it is read
/// again when it is opened; one saved from here, or untouched, is not.
#[test]
fn reopening_a_file_changed_elsewhere_reads_it_again() {
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.md");
    std::fs::write(&path, "First draft\n").unwrap();
    let file = path.to_string_lossy().into_owned();

    let mut app = TestApp::<WordProcessorApp>::launched(&[&file]).unwrap();
    app.harness().type_text("Edited: ");
    let app = app.relaunch(&[&file]).unwrap();
    assert_eq!(
        paragraphs(&app),
        ["Edited: First draft"],
        "an unchanged file leaves the record as it is"
    );

    std::fs::write(&path, "Rewritten elsewhere\n").unwrap();
    let mut app = app.relaunch(&[&file]).unwrap();
    assert_eq!(paragraphs(&app), ["Rewritten elsewhere"]);
    assert!(
        app.undo(),
        "reading the file again is an edit Undo takes back"
    );
    assert_eq!(paragraphs(&app), ["Edited: First draft"]);
}

/// Saving the document to its own file records what the file now holds, so
/// opening it again reads nothing back in.
#[test]
fn a_file_saved_here_is_not_read_again() {
    ui::i18n::I18nManager::init("en-US");
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("notes.md");
    std::fs::write(&path, "First draft\n").unwrap();
    let file = path.to_string_lossy().into_owned();

    let mut app = TestApp::<WordProcessorApp>::launched(&[&file]).unwrap();
    app.harness().type_text("Edited: ");
    app.effect_confirmed(&word_processor::effects::WordProcessorEffect::Save { path: None })
        .unwrap();
    for _ in 0..5 {
        app.harness().relayout();
    }
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim(),
        "Edited: First draft"
    );
    let versions = app.versions().len();
    let app = app.relaunch(&[&file]).unwrap();
    assert_eq!(paragraphs(&app), ["Edited: First draft"]);
    assert_eq!(app.versions().len(), versions, "nothing was read back in");
}

/// The panels a person opens follow them to their next window, and the
/// language their shared spelling preferences name is the one words are
/// checked in.
#[test]
fn layout_and_spelling_language_follow_the_person() {
    use word_processor::effects::WordProcessorEffect;
    use word_processor::{WordProcessorWidget, WriterLayout};
    let mut app = open();
    let shown = app.with_surface(|editor: &mut WordProcessorWidget| editor.panels_shown());
    assert_eq!(shown, Some((true, false, false)), "the default layout");
    app.effect(&WordProcessorEffect::Styles).unwrap();
    app.effect(&WordProcessorEffect::Ruler).unwrap();
    app.harness().relayout();
    assert_eq!(
        app.settings::<WriterLayout>().get(),
        WriterLayout {
            show_ruler: false,
            show_styles_panel: true,
            show_outline: false,
        },
        "the layout is kept with the person's settings"
    );

    app.settings::<ui::text_assistance::TextAssistancePrefs>()
        .update(|prefs| prefs.spelling_language = "de_DE".into())
        .unwrap();
    let mut app = app.reopen().unwrap();
    let shown = app.with_surface(|editor: &mut WordProcessorWidget| editor.panels_shown());
    assert_eq!(
        shown,
        Some((false, true, false)),
        "a new window opens as left"
    );
    let language =
        app.with_surface(|editor: &mut WordProcessorWidget| editor.spell_language().to_owned());
    assert_eq!(language.as_deref(), Some("de_DE"));
}

/// What an agent does in a document for a person is history the person
/// reads in their language: the app's sentence for each edit and verb, and
/// who did it for whom.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    use tilo_document::action::{MarkVal, RegionKind, RichTextAction, Span};
    use ui::tilo_actions::{ActionInvoker, ActorKind};
    let mut app = open();
    let kind = <Document as ui::tilo_resource::Resource>::KIND;
    let block = app.read_document(|doc| {
        let c = word_processor::model::open(doc).unwrap();
        tilo_doc::model::blocks_in_order(doc, &c).remove(0)
    });
    let agent = ActionInvoker::actor("agent:claude")
        .with_kind(ActorKind::Agent)
        .acting_for("ada");
    let edit = |op: RichTextAction| serde_json::json!({ "edit": word_processor::model::edit(&op) });
    for op in [
        RichTextAction::SpliceText {
            region: RegionKind::Body,
            block: block.clone(),
            at: 0,
            del: 0,
            ins: "Quarterly report".into(),
        },
        RichTextAction::SetMark {
            region: RegionKind::Body,
            spans: vec![Span {
                block: block.clone(),
                start: 0,
                end: 9,
            }],
            name: "strong".into(),
            value: Some(MarkVal::Bool(true)),
            also_clear: Vec::new(),
        },
        RichTextAction::SetParagraphStyle {
            region: RegionKind::Body,
            blocks: vec![block.clone()],
            kind: "heading".into(),
            level: Some(1),
            ordered: None,
        },
    ] {
        app.call_by(agent.clone(), kind, "field-edit", edit(op))
            .unwrap();
    }
    app.call_by(
        agent.clone(),
        kind,
        "append-paragraph",
        serde_json::json!({"text": "Revenue grew."}),
    )
    .unwrap();
    app.call_by(
        agent.clone(),
        kind,
        "insert-table",
        serde_json::json!({"after": block, "rows": 2, "columns": 3}),
    )
    .unwrap();
    app.call_by(
        agent,
        kind,
        "add-comment",
        serde_json::json!({"block": block, "start": 0, "end": 9, "text": "Which quarter?"}),
    )
    .unwrap();

    let english = include_str!("../locales/en-US.ftl");
    let catalogue = tilo_history::Catalogue::new("en-US", &[english]);
    let reader = tilo_history::Reader::new("en-US").with_app(&catalogue);
    let entries: Vec<_> = app
        .versions()
        .into_iter()
        .filter_map(|version| version.context)
        .filter(|entry| {
            entry
                .invoker
                .as_ref()
                .is_some_and(|invoker| invoker.actor == "agent:claude")
        })
        .collect();
    let sentences: Vec<String> = entries.iter().map(|entry| reader.sentence(entry)).collect();
    assert_eq!(
        sentences,
        [
            "Edited the text",
            "Made 9 characters bold",
            "Made a heading of level 1",
            "Added a paragraph",
            "Inserted a table of 2 by 3",
            "Commented: Which quarter?",
        ]
    );
    let invoker = entries[0].invoker.as_ref().unwrap();
    assert_eq!(
        (invoker.kind, invoker.on_behalf_of.as_deref()),
        (ActorKind::Agent, Some("ada")),
        "the entry names the agent and the person it acted for"
    );
}
