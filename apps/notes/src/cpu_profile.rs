//! Test-only observations around the actual Notes projection functions.
//! No timing budgets: the canonical mutation assertions establish the subject.

use ui::tilo_resource::profiling::Sample;

/// Run `operation` and return its wall time with every scope it ran.
fn observe(operation: impl FnOnce()) -> (u128, Vec<Sample>) {
    ui::tilo_resource::profiling::observe(|| {
        let start = ui::time::now();
        operation();
        start.elapsed().as_nanos()
    })
}

pub(super) fn fixture(
    pages: usize,
) -> (
    ui::framework::testing::TestApp<crate::NotesShell>,
    crate::PageId,
) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = ui::framework::testing::TestApp::<crate::NotesShell>::new().unwrap();
    let target = crate::PageId::new_v4();
    // Author the corpus through the actions a person's edits use, with no
    // frame between them: setup does not render 255 intermediate frames.
    let mut corpus = Vec::new();
    for index in 0..pages {
        let page = if index == 0 {
            target
        } else {
            crate::PageId::new_v4()
        };
        corpus.push(crate::actions::create_page_action(
            page,
            None,
            &format!("CPU fixture {index}"),
        ));
        if index != 0 {
            let body = format!("Authored fixture {index}. {}", "notebook text ".repeat(160));
            let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
            corpus.push(crate::actions::write_body_action(page, &blocks));
        }
    }
    author(&mut app, corpus);
    app.settle();
    assert_eq!(listed(&app).len(), pages);
    app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(target))
        .unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted editor");
    app.harness().click_at(rect.x0 + 20.0, rect.y0 + 10.0);
    (app, target)
}

type App = ui::framework::testing::TestApp<crate::NotesShell>;

/// Write `actions` into the notes as the window's person does, through the
/// notes' chain, with no frame between them.
pub(super) fn author(app: &mut App, actions: impl IntoIterator<Item = crate::actions::Action>) {
    let chain = app
        .surface(|notes: &mut crate::NotesApp| notes.pages_chain())
        .unwrap()
        .expect("the notes are held");
    for action in actions {
        chain.apply(&action).unwrap();
    }
}

/// Apply a note's `action` through the notes' published chain, as a control
/// or an agent addresses it.
pub(super) fn invoke_page(app: &mut App, action: &crate::actions::Action) {
    app.invoke_child::<crate::NoteList>(
        &crate::note_list::chain_resource(),
        ui::tilo_resource::Invoke::apply(action).unwrap(),
    )
    .unwrap();
}

/// A note's `action` from another member, arriving in the part keeping its
/// note as sync delivers it; no frame is drawn.
pub(super) fn arrives(app: &mut App, action: &crate::actions::Action) {
    let parts = app.chain_parts::<crate::NoteList>();
    let mut action = action.clone();
    crate::note_list::prepare(
        std::slice::from_mut(&mut action),
        &app.state(),
        &parts,
        None,
    )
    .unwrap();
    let index = ui::framework::placed::<crate::NoteList>(&action, &parts);
    app.replica_arrives(&crate::note_list::chain().part(index), |doc| {
        tilo_actions::apply_local_by(
            doc,
            &action,
            crate::actions::encode(&action),
            Some(b"member"),
        )
        .unwrap();
    })
    .unwrap();
}

/// What the notebook and every part of the notes hold: what any write
/// changes.
fn stored(app: &App) -> Vec<(Vec<automerge::ChangeHash>, usize)> {
    use automerge::transaction::Transactable as _;
    let read = |doc: &ui::tilo_resource::Doc| (doc.base_heads(), doc.pending_ops());
    let mut held = vec![app.read_document(read)];
    for (index, _) in app.chain_parts::<crate::NoteList>() {
        held.push(
            app.read_document_of(&crate::note_list::chain().part(index), read)
                .unwrap(),
        );
    }
    held
}

/// Every note, in notebook order.
fn listed(app: &App) -> Vec<crate::model::PageMeta> {
    crate::read::workspace(&app.state(), &app.chain_parts::<crate::NoteList>())
        .data
        .pages
}

#[test]
fn published_resources_hold_still_across_language_changes_and_remote_notes() {
    fn contains(resources: &[tilo_actions::ResourceSpec], id: &str) -> bool {
        resources
            .iter()
            .any(|resource| resource.id.as_str() == id || contains(&resource.children, id))
    }
    fn labels(resources: &[tilo_actions::ResourceSpec]) -> Vec<String> {
        let mut shown = Vec::new();
        for resource in resources {
            shown.push(resource.label.clone());
            shown.extend(labels(&resource.children));
        }
        shown
    }

    let (mut app, _) = fixture(16);
    let original = app.harness().resources();
    assert!(contains(&original, "notes.notebook"));
    let before = stored(&app);
    for _ in 0..10 {
        let resources = app.harness().resources();
        assert!(contains(&resources, "notes.notebook"));
        assert_eq!(labels(&resources), labels(&original), "nothing changed");
    }
    assert_eq!(stored(&app), before);

    let locale = ui::i18n::I18nManager::pseudo_locale();
    ui::i18n::I18nManager::set_pseudo_locale(ui::i18n::PseudoLocale::Expanded);
    let translated = app.harness().resources();
    ui::i18n::I18nManager::set_pseudo_locale(locale);
    assert!(
        contains(&translated, "notes.notebook"),
        "the catalogue reads in the new language"
    );
    assert_ne!(
        labels(&translated),
        labels(&original),
        "the labels a person hears follow their language"
    );
    assert_eq!(stored(&app), before);

    let page = crate::PageId::new_v4();
    arrives(
        &mut app,
        &crate::actions::create_page_action(page, None, "Remote creation"),
    );
    app.harness().relayout();
    let resources = app.harness().resources();
    assert!(
        contains(&resources, &crate::actions::page_resource_id(page)),
        "another member's note shows in the published catalogue"
    );
}

#[test]
fn mounted_text_note_view_only_scroll_contract_respects_title_editing() {
    let (mut app, target) = fixture(1);
    let body = "Ordinary text-note scrolling words.\n\n".repeat(32);
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    invoke_page(
        &mut app,
        &crate::actions::write_body_action(target, &blocks),
    );
    app.harness().relayout_at((1051, 240)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted canonical editor");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 20.0,
        ));
    let event = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    assert!(
        app.harness().root_widget().view_only_scroll(&event),
        "mounted canonical text-note wheel opts in through its bound owner"
    );
    app.surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().begin_title_edit(false))
        .unwrap();
    assert!(
        !app.harness().root_widget().view_only_scroll(&event),
        "title editing keeps the full input path"
    );
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::keyboard_input(
            winit::event::KeyEvent::synthetic(
                winit::event::RawKeyEvent {
                    physical_key: winit::keyboard::PhysicalKey::Code(
                        winit::keyboard::KeyCode::Escape,
                    ),
                    state: winit::event::ElementState::Pressed,
                },
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
                None,
                winit::keyboard::KeyLocation::Standard,
                false,
                None,
                winit::keyboard::Key::Named(winit::keyboard::NamedKey::Escape),
            ),
        ));
    assert!(
        app.harness().root_widget().view_only_scroll(&event),
        "cancelled title resumes text-note scrolling"
    );

    let before = stored(&app);
    assert!(
        app.harness().dispatch_to_root(&event),
        "real text-note wheel is handled"
    );
    let scrolled = app
        .surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
        .unwrap();
    assert!(scrolled > 0.0, "real note body scrolls");
    assert_eq!(stored(&app), before);
}

#[test]
fn mounted_text_note_wheel_avoids_editor_projection_and_still_edits() {
    use std::fmt::Write as _;
    let (mut app, target) = fixture(1);
    let mut body = String::new();
    let paragraph = "Measured scrolling text with ordinary words. ".repeat(6);
    for index in 0..180 {
        write!(&mut body, "Paragraph {index}: {paragraph}\n\n").unwrap();
    }
    let blocks = crate::MainPanel::seed_blocks(&body, |_| None);
    invoke_page(
        &mut app,
        &crate::actions::write_body_action(target, &blocks),
    );
    app.harness().relayout_at((1051, 952)).unwrap();
    let rect = app
        .harness()
        .targets()
        .get("block_editor")
        .copied()
        .expect("mounted canonical editor");
    app.harness()
        .dispatch_to_root(&ui::synthetic_input::cursor_moved(
            rect.x0 + 40.0,
            rect.y0 + 20.0,
        ));
    let before = stored(&app);
    let event = ui::synthetic_input::mouse_wheel(0.0, -1.0);
    let (elapsed, rows) = observe(|| {
        for _ in 0..100 {
            assert!(
                app.harness().dispatch_to_root(&event),
                "real note wheel handled"
            );
        }
    });
    let editor_calls = rows
        .iter()
        .filter(|row| row.phase == "rich_text_input")
        .map(|row| row.calls)
        .sum::<usize>();
    eprintln!(
        "notes-wheel-editor-boundary {}",
        serde_json::json!({
            "paragraphs":180,"markdown_bytes":body.len(),"viewport":[1051,952],"wheel_events":100,
            "elapsed_ns":elapsed,"editor_input_calls":editor_calls,"phases":rows,
            "canonical_notes_framework":true,"native_window_runner":false,"gpu_or_scanout":false,
            "architecture":std::env::consts::ARCH,"profile":if cfg!(debug_assertions) {"test-debug"} else {"optimized"},
        })
    );
    assert_eq!(
        editor_calls, 0,
        "handled scrolling must not enter the editor's document-projection path"
    );
    assert_eq!(
        stored(&app),
        before,
        "view scrolling leaves the canonical note untouched"
    );
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.main_panel_mut().scroll_y_for_test())
            .unwrap()
            > 0.0
    );
    app.harness().relayout();
    app.harness().click_at(rect.x0 + 40.0, rect.y0 + 20.0);
    let (_, editing) = observe(|| app.harness().type_text("verified "));
    assert!(
        editing
            .iter()
            .any(|row| row.phase == "rich_text_input" && row.calls > 0),
        "a real text edit must still reach the instrumented editor"
    );
    assert_ne!(
        stored(&app),
        before,
        "the real edit reaches the canonical resource"
    );
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.editor_text_for_test())
            .unwrap()
            .contains("verified ")
    );
}

#[test]
fn choosing_a_note_reads_no_notebook_and_shows_it() {
    let (mut app, _) = fixture(16);
    let other = listed(&app)
        .into_iter()
        .find(|page| page.title == "CPU fixture 5")
        .expect("fixture note")
        .id;
    let ((), rows) = ui::tilo_resource::profiling::observe(|| {
        app.surface(|notes: &mut crate::NotesApp| notes.navigate_to(other))
            .unwrap();
        app.harness().relayout();
    });
    assert!(
        app.surface(|notes: &mut crate::NotesApp| notes.editor_text_for_test())
            .unwrap()
            .contains("Authored fixture 5"),
        "the chosen note is shown"
    );
    let reads: Vec<_> = rows
        .iter()
        .filter(|row| row.phase == "read_notebook")
        .collect();
    assert!(
        reads.is_empty(),
        "choosing a note read the notebook: {reads:?}"
    );
}
