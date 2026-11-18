mod agent_content;
mod search_refresh;
use super::*;
use crate::note_list::NoteList;
use tempfile::TempDir;

/// The notes app as the framework holds it.
pub(super) type App = ui::framework::testing::TestApp<crate::NotesShell>;

pub(super) fn open() -> App {
    ui::i18n::I18nManager::init("en-US");
    App::new().unwrap()
}

/// The window's notes surface.
pub(super) fn notes(app: &mut App) -> &mut NotesApp {
    app.surface_mut::<NotesApp>()
}

/// Every note the notes hold, in the order they are shown.
pub(super) fn page_ids(app: &App) -> Vec<PageId> {
    let parts = app.chain_parts::<NoteList>();
    crate::Pages::of(&parts)
        .iter()
        .filter_map(|(id, _)| crate::notebook::page_of(id))
        .collect()
}

/// The part keeping note `page`.
fn part_of(app: &App, page: PageId) -> u64 {
    let parts = app.chain_parts::<NoteList>();
    crate::Pages::of(&parts)
        .part_of(&crate::notebook::page_id(page))
        .expect("a part keeps the note")
}

/// Read the document of the part keeping note `page`, as the window
/// holds it.
pub(super) fn part_doc<T>(app: &App, page: PageId, read: impl FnOnce(&AutoCommit) -> T) -> T {
    app.read_document_of(&crate::note_list::chain().part(part_of(app, page)), read)
        .expect("the part is held")
}

/// The heads of the notebook and every part of the notes: what changes
/// with any write.
fn stored(app: &App) -> Vec<Vec<automerge::ChangeHash>> {
    let mut heads = vec![app.read_document(AutoCommit::base_heads)];
    for (index, _) in app.chain_parts::<NoteList>() {
        heads.push(
            app.read_document_of(
                &crate::note_list::chain().part(index),
                AutoCommit::base_heads,
            )
            .unwrap(),
        );
    }
    heads
}

/// Apply a note's `action` as this window's person does, through the
/// notes' chain, and show what it changed.
pub(super) fn apply(app: &mut App, action: &actions::Action) {
    app.invoke_child::<NoteList>(
        &crate::note_list::chain_resource(),
        ui::tilo_resource::Invoke::apply(action).unwrap(),
    )
    .unwrap();
    app.settle();
}

/// A note's `action` arriving from another member's replica, as sync
/// delivers it: written into the part keeping its note; no frame is drawn.
pub(super) fn arrive(app: &mut App, action: &actions::Action) {
    let parts = app.chain_parts::<NoteList>();
    let mut action = action.clone();
    crate::note_list::prepare(
        std::slice::from_mut(&mut action),
        &app.state(),
        &parts,
        None,
    )
    .unwrap();
    let index = ui::framework::placed::<NoteList>(&action, &parts);
    app.replica_arrives(&crate::note_list::chain().part(index), |doc| {
        tilo_actions::apply_local_by(doc, &action, actions::encode(&action), Some(b"peer"))
            .unwrap();
    })
    .unwrap();
}

/// The notes as they stand now, which the window shows from its next
/// sync on.
pub(super) fn current(app: &mut App) -> Workspace {
    let notes = notes(app);
    let notebook = notes.live.as_ref().expect("hosted").get();
    let parts = notes
        .notes
        .as_ref()
        .expect("the notes are held")
        .chain
        .parts();
    Workspace::of(&notebook, &parts, &Workspace::default())
}

/// Show what the window holds now, as a sync does.
pub(super) fn present(app: &mut App) {
    let notes = notes(app);
    let notebook = notes.live.as_ref().expect("hosted").get();
    let parts = notes
        .notes
        .as_ref()
        .expect("the notes are held")
        .chain
        .parts();
    notes.adopt_resource_state(&notebook, &parts);
}

/// A notebook holding one "Welcome" note of several paragraphs, open.
pub(super) fn seeded() -> (App, PageId) {
    let mut app = open();
    let welcome = PageId::new_v4();
    let blocks = MainPanel::seed_blocks(
        "Welcome to notes.\n\nA second paragraph to read.\n\nA third one, last.",
        |_| None,
    );
    apply(
        &mut app,
        &actions::create_page_action(welcome, None, "Welcome"),
    );
    apply(&mut app, &actions::write_body_action(welcome, &blocks));
    notes(&mut app).navigate_to(welcome);
    app.settle();
    assert_eq!(notes(&mut app).selected_for_test(), Some(welcome));
    (app, welcome)
}

#[test]
fn transclusion_previews_follow_selected_marks_remote_edits_and_deleted_targets() {
    let (mut app, source) = seeded();
    assert!(
        notes(&mut app).transclusion_previews().is_empty(),
        "ordinary notes must not project every other note as a preview"
    );
    let target = PageId::new_v4();
    let blocks = MainPanel::seed_blocks("Original preview", |_| None);
    let source_block = part_doc(&app, source, |doc| {
        crate::read::page_block_snapshots(doc, source)
    })[0]
        .clone();
    for action in [
        actions::create_page_action(target, None, "Target"),
        actions::write_body_action(target, &blocks),
        actions::set_mark_action(
            source,
            &source_block.0,
            0..source_block.1.len(),
            &ui::widget::Mark::Transclusion(target),
        ),
    ] {
        arrive(&mut app, &action);
    }
    app.settle();
    let previews = notes(&mut app).transclusion_previews();
    assert_eq!(previews.len(), 1);
    assert_eq!(previews[&target].title, "Target");
    assert_eq!(previews[&target].excerpt, "Original preview");

    let block = part_doc(&app, target, |doc| {
        crate::read::page_block_snapshots(doc, target)
    })[0]
        .clone();
    arrive(
        &mut app,
        &actions::edit_body_action(
            target,
            tilo_document::RichTextAction::SpliceText {
                region: tilo_document::action::RegionKind::Body,
                block: block.0,
                at: 0,
                del: block.1.chars().count(),
                ins: "é".repeat(181),
            },
        ),
    );
    app.settle();
    assert_eq!(
        notes(&mut app).transclusion_previews()[&target].excerpt,
        format!("{}…", "é".repeat(180))
    );

    notes(&mut app).navigate_to(target);
    app.settle();
    assert!(notes(&mut app).transclusion_previews().is_empty());
    notes(&mut app).navigate_to(source);
    app.settle();
    assert_eq!(notes(&mut app).transclusion_previews().len(), 1);
    arrive(&mut app, &actions::delete_page_action(target));
    app.settle();
    assert!(notes(&mut app).transclusion_previews().is_empty());
}

/// An import's result is the window bar's notice, and a capture keeps
/// it up however long the frame takes.
#[test]
fn a_capture_keeps_the_import_notice_in_the_window_bar() {
    // A capture pins the clock for the rest of the process, which would
    // stop it under every test beside this one: the frames that must
    // keep the notice up are called on this thread, held at the capture time.
    let at = ui::clock::capture_wall().expect("the capture time exists in the local zone");
    ui::time::testing::with_wall(at, || {
        let (mut app, _) = seeded();
        notes(&mut app).notify(
            "Imported Project brief",
            ui::widget::toast::ToastKind::Success,
        );
        app.settle();
        app.settle();
        assert_eq!(
            ui::window_toolbar::notice().map(|notice| notice.text),
            Some("Imported Project brief".to_owned())
        );
    });
}

#[test]
fn published_block_range_selects_unicode_in_the_real_editor_without_editing() {
    use tilo_document::RichTextAction;
    let mut app = open();
    app.call_on(
        &crate::note_list::chain_resource(),
        "new-note",
        serde_json::json!({"title": "Range fixture"}),
    )
    .unwrap();
    let page = page_ids(&app)[0];
    apply(
        &mut app,
        &actions::edit_body_action(
            page,
            RichTextAction::AppendParagraph {
                text: "café notes".into(),
            },
        ),
    );
    let block = part_doc(&app, page, |doc| {
        crate::read::page_block_snapshots(doc, page)
            .into_iter()
            .find(|(_, text)| text == "café notes")
            .unwrap()
            .0
    });
    let before = stored(&app);
    for (start, end, unit) in [(3, 4, "Chars"), (3, 5, "Bytes")] {
        app.call_on(
            &crate::actions::page_resource_id(page),
            "reveal-block-range",
            serde_json::json!({ "block": block, "start": start, "end": end, "unit": unit }),
        )
        .unwrap();
        app.settle();
        assert_eq!(
            notes(&mut app).main.editor().selected_text().as_deref(),
            Some("é")
        );
        assert_eq!(stored(&app), before);
    }
    for (start, end, unit) in [
        (4, 3, "Chars"),
        (4, 5, "Bytes"),
        (0, 100, "Chars"),
        (0, 1, "Pixels"),
    ] {
        assert!(
            app.call_on(
                &crate::actions::page_resource_id(page),
                "reveal-block-range",
                serde_json::json!({ "block": block, "start": start, "end": end, "unit": unit })
            )
            .is_err()
        );
        assert_eq!(
            notes(&mut app).main.editor().selected_text().as_deref(),
            Some("é")
        );
    }
    assert_eq!(stored(&app), before);
}

#[test]
fn a_block_edit_before_binding_discards_the_pending_range() {
    let (mut app, page) = seeded();
    let shown = current(&mut app);
    let notes_app = notes(&mut app);
    let block = notes_app.workspace.bodies[&page][0].id.clone();
    notes_app
        .open_block_range(&shown, page, block.clone(), (0, 1, "Chars"))
        .unwrap();
    let action = actions::edit_body_action(
        page,
        tilo_document::RichTextAction::SpliceText {
            region: tilo_document::action::RegionKind::Body,
            block,
            at: 0,
            del: 0,
            ins: "changed before bind ".into(),
        },
    );
    notes_app.apply_page(&action).unwrap();
    app.settle();
    let notes_app = notes(&mut app);
    assert!(notes_app.pending_block_range.is_none());
    assert!(
        notes_app.main.editor().selected_text().is_none(),
        "never select offsets against different text"
    );
}

#[test]
fn images_beside_a_markdown_file_become_notebook_assets() {
    eprintln!("phase images_beside_a_launched_file_join_the_notebook");
    {
        let source = TempDir::new().unwrap();
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png"));
        std::fs::write(source.path().join("photo.png"), bytes).unwrap();
        std::fs::write(source.path().join("outside.png"), bytes).unwrap();
        let markdown = "![Portable](photo.png)\n![Kept](https://example.test/a.png)\n";
        let (imported, assets) =
            adopt_canonical_markdown_assets(&source.path().join("Imported.md"), markdown);
        let asset = format!("asset:{}", blake3::hash(bytes).to_hex());
        assert_eq!(
            imported,
            format!("![Portable]({asset})\n![Kept](https://example.test/a.png)\n")
        );
        assert_eq!(assets, vec![(asset, bytes.to_vec())]);
    }

    eprintln!("phase repeated_markdown_images_only_consume_the_unique_asset_budget_once");
    {
        let tmp = TempDir::new().unwrap();
        let image = tmp.path().join("image.png");
        std::fs::write(
            &image,
            include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")),
        )
        .unwrap();
        let markdown = "![Repeated](image.png)\n".repeat(10);

        let (imported, assets) =
            adopt_canonical_markdown_assets(&tmp.path().join("note.md"), &markdown);

        assert_eq!(assets.len(), 1);
        let source = &assets[0].0;
        assert_eq!(imported, format!("![Repeated]({source})\n").repeat(10));
    }
}
#[test]
fn a_hosted_notebook_draws_its_images() {
    let mut app = open();
    let page = PageId::new_v4();
    let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")).to_vec();
    let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
    for action in [
        actions::create_page_action(page, None, "Shots"),
        actions::place_image_action(page, bytes, Some("Settings")),
    ] {
        apply(&mut app, &action);
    }
    let store = app
        .surface(|notes: &mut NotesApp| notes.main.image_store().cloned())
        .unwrap()
        .expect("the hosted editor draws images from the notebook");
    let _ = store.get(&source);
    store.block_until_idle(tilo_test_env::hang_bound());
    assert!(store.get(&source).is_some(), "the image decodes");
}

#[test]
fn the_empty_notebook_creates_its_first_note_from_the_empty_state() {
    let mut app = open();
    app.settle();
    assert!(
        notes(&mut app).workspace.pages().is_empty(),
        "a new notebook starts empty"
    );
    // The empty state's activation reads nothing of the document it is
    // handed: it presses New note.
    let mut untouched = AutoCommit::new();
    assert!(notes(&mut app).main.perform_action(
        &ui::a11y::Action::Activate,
        &[],
        &mut Edits::lend(&mut untouched),
        &automerge::ROOT,
    ));
    app.settle();
    assert_eq!(notes(&mut app).workspace.pages().len(), 1);
    assert!(notes(&mut app).workspace.selected().is_some());
    assert_eq!(page_ids(&app).len(), 1, "the note is kept");
}

/// The directories of the documents `profile` keeps.
fn document_directories(
    profile: &std::path::Path,
) -> std::collections::HashSet<std::path::PathBuf> {
    let mut found = std::collections::HashSet::new();
    let mut open = vec![profile.to_path_buf()];
    while let Some(directory) = open.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                open.push(path);
            } else if path
                .file_name()
                .is_some_and(|name| name == "state.automerge")
            {
                found.insert(directory.clone());
            }
        }
    }
    found
}

/// A save that fails holds its notice in the window's bar, "Try again"
/// beside it, for as long as the failure stands; "Try again" recovers
/// the actual document writer (not a latch the test consumes after the
/// production owner took it), and the save that succeeds takes the
/// failure down.
#[test]
fn a_failed_save_holds_its_notice_with_try_again_until_a_save_succeeds() {
    use ui::tilo_resource::Resource as _;
    use ui::widget::toast::ToastKind;

    ui::i18n::I18nManager::init("en-US");
    let profile = TempDir::new().unwrap();
    let mut app = App::open_data_dir(profile.path()).unwrap();
    app.flush().unwrap();
    let before = document_directories(profile.path());
    let page = uuid::Uuid::new_v4();
    apply(
        &mut app,
        &crate::actions::create_page_action(page, None, "Retry receipt"),
    );
    app.flush().unwrap();
    // The note is kept in the first part of the notes, a document of its
    // own beside the notebook's.
    let directory = document_directories(profile.path())
        .difference(&before)
        .next()
        .cloned()
        .expect("the part's document directory");
    let backup = profile.path().join("held-document");
    std::fs::rename(&directory, &backup).unwrap();
    // A regular file in place of this test's directory makes writes fail
    // for root as well as ordinary users; no permission assumptions.
    std::fs::write(&directory, b"deliberate test storage fault").unwrap();
    app.invoke_child::<NoteList>(
        &crate::note_list::chain_resource(),
        NoteList::RENAME.with(crate::note_list::note_list::Rename {
            page: crate::notebook::page_id(page),
            title: "Retried receipt".into(),
        }),
    )
    .unwrap();
    assert!(app.flush().is_err(), "the actual writer must fail");
    app.settle();
    app.surface::<NotesApp, _>(|notes| {
        assert!(matches!(
            notes.resource_save_status,
            ui::resource::ResourceSaveStatus::Failed(_)
        ));
    })
    .unwrap();
    let held = ui::window_toolbar::notice().expect("the failure is said in the window's bar");
    assert_eq!(held.kind, ToastKind::Error);
    assert!(held.text.starts_with("Save failed: "), "{held:?}");
    let retry = held.actions.first().cloned().expect("Try again beside it");
    assert_eq!(retry.name, "Try again");
    // In the window's bar, wide and compact, Try again stands right
    // after the failure, naming itself where the row fits it, and never
    // in the overflow menu; the failure is cut short only where nothing
    // that yields to it stands beside it.
    app.harness().present_in_window_bar();
    for width in [1280.0, 420.0] {
        let shown = app
            .harness()
            .window_bar_notice(width)
            .expect("the failure is up in the bar");
        let button = shown.buttons.first().expect("Try again stands in the row");
        assert_eq!(button.name, "Try again");
        assert!(
            button.bounds.x0 > shown.text_bounds.x1,
            "{width} px: {shown:?}"
        );
        // Wide, inline; compact, in a strip of its own under the row,
        // its text whole.
        assert_eq!(
            shown.strip.is_some(),
            width < 1280.0,
            "{width} px: {shown:?}"
        );
        assert!(shown.whole && button.labelled, "{width} px: {shown:?}");
    }
    app.settle();
    app.settle();
    assert_eq!(
        ui::window_toolbar::notice(),
        Some(held),
        "the failure stands until a save succeeds"
    );

    std::fs::remove_file(&directory).unwrap();
    std::fs::rename(&backup, &directory).unwrap();
    app.harness()
        .press_window_part(ui::window_toolbar::ToolbarEvent::Action { id: retry.id });
    app.settle();
    app.surface::<NotesApp, _>(|notes| {
        assert_eq!(
            notes.resource_save_status,
            ui::resource::ResourceSaveStatus::Saved,
            "the real owner must acknowledge the repaired writer"
        );
    })
    .unwrap();
    assert!(!app.take_save_retry().unwrap(), "no second retry remains");
    let said = ui::window_toolbar::notice().expect("the save that ends it says so");
    assert_eq!(
        (said.text.as_str(), said.kind, said.actions.first()),
        ("Saved", ToastKind::Success, None),
        "the failure is down"
    );
    let bytes = std::fs::read(directory.join("state.automerge")).unwrap();
    let persisted = automerge::AutoCommit::load(&bytes).unwrap();
    let part = NoteList::read(&persisted).unwrap();
    assert_eq!(
        part.pages
            .get(&crate::notebook::page_id(page))
            .map(|page| page.title.as_str()),
        Some("Retried receipt"),
        "the retry must make the failed edit durable on disk"
    );
}

#[test]
fn status_reports_live_word_counts() {
    eprintln!("phase word_count_updates_without_reopening_status");
    {
        let (mut app, _) = seeded();
        let app = notes(&mut app);
        app.refresh_status();
        app.main
            .editor_mut()
            .set_blocks(vec![ui::widget::Block::paragraph("One two three")]);
        app.refresh_status();
        assert_eq!(app.status.state().word_count, Some(3));
    }

    eprintln!("phase words_are_counted_once_per_change_of_the_note");
    {
        let (mut app, _welcome) = seeded();
        let app = notes(&mut app);
        app.refresh_status();
        let boards = app.main.word_count();
        assert!(boards > 0, "the seeded note has words");
        let counted = app.word_count.expect("the count was taken");
        assert_eq!(counted.1, boards);
        // A status refresh that saw no change reuses what it counted.
        for _ in 0..3 {
            app.refresh_status();
            assert_eq!(app.word_count, Some(counted), "no recount without a change");
            assert_eq!(app.status.state().word_count, Some(boards));
        }
        // An edit of the note is recounted.
        app.main
            .editor_mut()
            .set_blocks(vec![ui::widget::Block::paragraph("One two three")]);
        app.refresh_status();
        assert_eq!(app.status.state().word_count, Some(3));
        let edited = app.word_count.expect("the edited count was taken");
        assert_eq!(edited.1, 3, "the live count, not one left behind");
        assert_ne!(edited.0, counted.0, "the revision moved with the edit");
    }

    eprintln!("phase an_unchanged_notebook_skips_backlink_index_work");
    {
        let (mut app, welcome) = seeded();
        let app = notes(&mut app);
        app.backlink_index.insert(welcome, Vec::new());
        assert!(app.unindexed.is_empty());
        app.refresh_backlinks();
        assert_eq!(app.backlink_index.get(&welcome), Some(&Vec::new()));
    }
}

/// The window's search rows are the text each note's part holds.
pub(super) fn assert_search_snapshot_matches_canonical(app: &mut App) {
    let workspace = notes(app).workspace.clone();
    assert_eq!(workspace.bodies.len(), workspace.pages().len());
    for page in workspace.pages() {
        let actual = workspace.bodies[&page.id]
            .iter()
            .map(|block| (block.id.clone(), block.text.clone()))
            .collect::<Vec<_>>();
        assert_eq!(
            actual,
            part_doc(app, page.id, |doc| crate::read::page_block_snapshots(
                doc, page.id
            ))
        );
    }
}

#[test]
fn canonical_open_refuses_missing_pages_and_blocks_before_projection_adoption() {
    let (mut app, page) = seeded();
    let shown = current(&mut app);
    let pending = notes(&mut app).pending_open.len();
    assert_eq!(
        notes(&mut app).open_block(&shown, page, Some("missing-block".into())),
        Err("the block no longer exists".into())
    );
    assert_eq!(notes(&mut app).pending_open.len(), pending);
    arrive(&mut app, &actions::delete_page_action(page));
    let shown = current(&mut app);
    assert!(shown.get(page).is_none());
    assert!(
        notes(&mut app).workspace.get(page).is_some(),
        "the prior presentation still contains the page"
    );
    assert_eq!(
        notes(&mut app).open_block(&shown, page, None),
        Err("the note no longer exists".into())
    );
    assert_eq!(
        notes(&mut app).pending_open.len(),
        pending,
        "refused Open cannot queue a selection"
    );
}

#[test]
fn search_snapshot_tracks_remote_rich_edits_and_removed_link_targets() {
    let (mut app, source) = seeded();
    let target = PageId::new_v4();
    let absent = PageId::new_v4();
    arrive(
        &mut app,
        &actions::create_page_action(target, None, "Destination"),
    );
    let source_blocks = part_doc(&app, source, |doc| {
        crate::read::page_block_snapshots(doc, source)
    });
    for (block, range, link) in [
        (&source_blocks[0].0, 0..4, target),
        (&source_blocks[0].0, 5..7, target),
        (&source_blocks[0].0, 8..10, source),
        (&source_blocks[1].0, 0..4, absent),
    ] {
        arrive(
            &mut app,
            &actions::set_mark_action(source, block, range, &ui::widget::Mark::Link(link)),
        );
    }
    let rich_before = part_doc(&app, source, |doc| {
        crate::read::page_stored_blocks(doc, source)
    });
    app.settle();
    assert_search_snapshot_matches_canonical(&mut app);
    let notes_app = notes(&mut app);
    assert_eq!(
        notes_app.backlink_index.get(&target),
        Some(&vec![(source, 2)])
    );
    assert!(!notes_app.backlink_index.contains_key(&source));
    assert!(!notes_app.backlink_index.contains_key(&absent));
    assert_eq!(
        part_doc(&app, source, |doc| crate::read::page_stored_blocks(
            doc, source
        )),
        rich_before
    );

    arrive(&mut app, &actions::delete_page_action(target));
    app.settle();
    assert_search_snapshot_matches_canonical(&mut app);
    assert!(!notes(&mut app).workspace.bodies.contains_key(&target));
    assert!(!notes(&mut app).backlink_index.contains_key(&target));
    assert_eq!(
        part_doc(&app, source, |doc| crate::read::page_stored_blocks(
            doc, source
        )),
        rich_before
    );
}

#[test]
fn navigation_and_backlinks_follow_stable_page_identity() {
    eprintln!("phase native_link_marks_build_a_cached_backlink_and_open_its_canonical_source");
    {
        let (mut app, source) = seeded();

        // Create the destination through the normal page action path, then
        // attach a real native link mark to an existing source block. This is
        // intentionally not a Markdown-parser test: incoming links must be
        // derived from the same canonical mark data members receive.
        let target = PageId::new_v4();
        notes(&mut app)
            .apply_page(&actions::create_page_action(
                target,
                None,
                crate::notebook::NO_TITLE,
            ))
            .unwrap();
        app.settle();
        assert_ne!(source, target);
        let source_block = notes(&mut app).workspace.bodies[&source][0].id.clone();
        let action = crate::actions::set_mark_action(
            source,
            &source_block,
            0..4,
            &ui::widget::Mark::Link(target),
        );
        notes(&mut app).apply_page(&action).unwrap();

        notes(&mut app).navigate_to(target);
        app.settle();
        assert_eq!(
            notes(&mut app).selected_backlink_chips(),
            vec![BacklinkChip {
                page: source,
                title: "Welcome".into(),
                references: 1,
            }],
            "the selected page projects its incoming native link once"
        );

        notes(&mut app)
            .open_note(source)
            .expect("a backlink opens its source note");
        app.settle();
        assert_eq!(notes(&mut app).selected_for_test(), Some(source));
    }
}

#[test]
fn exported_notes_carry_their_images_and_import_them_again() {
    eprintln!("phase an_exported_note_carries_its_images_and_imports_them_again");
    {
        let bytes = include_bytes!(concat!(env!("OUT_DIR"), "/settings.png")).to_vec();
        let source = format!("asset:{}", blake3::hash(&bytes).to_hex());
        let assets = HashMap::from([(source.clone(), bytes.clone())]);
        let markdown = format!("Intro\n\n![Diagram]({source})\n![Elsewhere](asset:missing)\n");

        let exported = inline_images(&markdown, &assets);
        assert!(!exported.contains(&source), "{exported}");
        assert!(exported.contains("![Diagram](data:image/png;base64,"));
        assert!(
            exported.contains("![Elsewhere](asset:missing)"),
            "an image the notebook lacks stays as it was"
        );

        let (imported, imported_assets) = adopt_inline_images(&exported);
        assert_eq!(imported, markdown);
        assert_eq!(imported_assets, vec![(source, bytes)]);
    }

    eprintln!("phase only_images_a_notebook_keeps_are_taken_in");
    {
        let (text, assets) = adopt_inline_images("![Script](data:image/svg+xml;base64,PHN2Zz4=)\n");
        assert_eq!(text, "![Script](data:image/svg+xml;base64,PHN2Zz4=)\n");
        assert!(assets.is_empty());
    }
}

#[allow(clippy::too_many_lines, reason = "merged cases, one per section")]
#[test]
fn collaborative_rebinds_keep_marks_targets_and_reveal_focus() {
    eprintln!("phase brokered_block_reveal_restores_exact_page_and_editor_focus");
    {
        let (mut app, welcome) = seeded();
        assert!(
            notes(&mut app).main.editor().blocks().len() > 1,
            "welcome has several blocks"
        );
        let block_id = notes(&mut app).workspace.bodies[&welcome][1].id.clone();
        let shown = current(&mut app);
        assert!(
            notes(&mut app)
                .open_block(&shown, welcome, Some("no-such-block".into()))
                .is_err(),
            "a block that is gone is not revealed"
        );
        notes(&mut app)
            .open_block(&shown, welcome, Some(block_id))
            .expect("the block's note opens");
        app.settle();

        assert_eq!(notes(&mut app).selected_for_test(), Some(welcome));
        assert_eq!(
            notes(&mut app).main.editor().focused_index(),
            Some(1),
            "the canonical block action reveals the exact editor block"
        );
    }

    eprintln!("phase canonical_inline_marks_rehydrate_after_a_collaborative_rebind");
    {
        let (mut app, welcome) = seeded();
        let block = notes(&mut app).workspace.bodies[&welcome][0].id.clone();
        let original = notes(&mut app)
            .main
            .editor()
            .blocks()
            .iter()
            .find(|candidate| candidate.id == block)
            .expect("search block belongs to the bound editor")
            .text()
            .to_string();
        assert!(original.len() >= 4);
        let marks = vec![crate::actions::InlineMarkSpan {
            start: 0,
            end: 4,
            mark: crate::actions::InlineMark::Underline,
        }];
        let action = crate::actions::set_mark_action(
            welcome,
            &block,
            0..4,
            &ui::widget::marks::Mark::Underline,
        );
        notes(&mut app).apply_page(&action).unwrap();

        // Simulate a remote structural refresh. The widget rebuilds plain
        // text/kinds, then Notes hydrates the native mark state back into the
        // live rich line rather than dropping formatting on rebind.
        notes(&mut app).bound_page = None;
        app.settle();
        let restored = notes(&mut app)
            .main
            .editor()
            .blocks()
            .iter()
            .find(|candidate| candidate.id == block)
            .expect("same durable block rebinds")
            .line
            .marks()
            .iter()
            .map(crate::actions::InlineMarkSpan::from_ui)
            .collect::<Vec<_>>();
        assert_eq!(restored, marks);
    }

    eprintln!("phase rebinding_the_open_note_keeps_the_caret_where_it_was");
    {
        let (mut app, _welcome) = seeded();
        let (index, block, end) = {
            let editor = notes(&mut app).main.editor();
            let index = editor.blocks().len().saturating_sub(1);
            let block = &editor.blocks()[index];
            (index, block.id.clone(), block.text().len())
        };
        assert!(end > 0);
        notes(&mut app)
            .main
            .editor_mut()
            .focus_block(index, ui::widget::CursorPlacement::End);
        notes(&mut app).bound_page = None;
        app.settle();
        let editor = notes(&mut app).main.editor();
        let focused = editor
            .focused_index()
            .and_then(|focused| editor.blocks().get(focused))
            .expect("the rebound editor keeps a focused block");
        assert_eq!(focused.id, block, "the caret stays in its block");
        assert_eq!(
            focused.line.cursor_pos(),
            end,
            "the next letter is typed after the last one, not at the start"
        );
    }

    eprintln!("phase mark_only_dirty_edge_dispatches_the_canonical_action");
    {
        let (mut app, welcome) = seeded();
        let (block, text_len) = {
            let first = &notes(&mut app).main.editor().blocks()[0];
            (first.id.clone(), first.text().len())
        };
        assert!(text_len >= 4);
        notes(&mut app).main.editor_mut().blocks_mut()[0]
            .line
            .set_marks(vec![ui::widget::marks::MarkSpan::new(
                0..4,
                ui::widget::marks::Mark::Underline,
            )]);
        // Formatting changes no plain text; the editor still writes it as
        // the page's own edit.
        let before = part_doc(&app, welcome, AutoCommit::base_heads);
        app.settle();
        let attributed = part_doc(&app, welcome, |doc| {
            doc.clone().get_changes(&before).iter().any(|change| {
                change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
                    .is_some()
            })
        });
        assert!(
            attributed,
            "the mark-only edit is an attributed, undoable Notes action"
        );
        assert_eq!(
            part_doc(&app, welcome, |doc| crate::read::page_block_marks(
                doc, welcome
            ))
            .into_iter()
            .find(|(id, _)| id == &block)
            .map(|(_, marks)| marks),
            Some(vec![crate::actions::InlineMarkSpan {
                start: 0,
                end: 4,
                mark: crate::actions::InlineMark::Underline,
            }])
        );
    }

    eprintln!("phase collaborative_identical_insert_preserves_the_original_block_target");
    {
        let (mut app, welcome) = seeded();
        let original = notes(&mut app).workspace.bodies[&welcome][1].clone();
        let block = original.id.clone();

        // A member inserts an identical-text block before the target.
        // Content and index cannot distinguish them; block identity can.
        let last = notes(&mut app).workspace.bodies[&welcome]
            .last()
            .unwrap()
            .id
            .clone();
        arrive(
            &mut app,
            &crate::actions::edit_body_action(
                welcome,
                tilo_document::RichTextAction::InsertParagraph {
                    after: last.clone(),
                    text: original.text.clone(),
                },
            ),
        );
        let inserted = part_doc(&app, welcome, |doc| {
            crate::read::page_block_snapshots(doc, welcome)
        })
        .into_iter()
        .skip_while(|(id, _)| *id != last)
        .nth(1)
        .unwrap()
        .0;
        arrive(
            &mut app,
            &crate::actions::edit_body_action(
                welcome,
                tilo_document::RichTextAction::MoveBlocks {
                    blocks: vec![inserted],
                    after: None,
                },
            ),
        );
        app.settle();

        let shown = current(&mut app);
        notes(&mut app)
            .open_block(&shown, welcome, Some(block))
            .expect("durable block identity still resolves");
        app.settle();
        assert_eq!(
            notes(&mut app).main.editor().focused_index(),
            Some(2),
            "the original block moved to index 2; the identical insertion at 0 is not revealed"
        );
    }
}
