//! The framework-hosted app reports the durability the host measures: a note
//! written to the record's storage reads "Saved", never the session-only
//! warning, and is still there after the app is closed and reopened.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

fn save_labels(app: &mut TestApp<NotesShell>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .filter(|label| ["Saved", "Saving…", "Save failed", "Not saved"].contains(&label.as_str()))
        .collect()
}

#[test]
fn a_hosted_note_reports_saved_and_survives_a_reopen() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.invoke(Invoke::apply(&create_page_action(PageId::new_v4(), None, "Groceries")).unwrap())
        .unwrap();
    app.flush().unwrap();
    app.settle();
    assert_eq!(
        save_labels(&mut app),
        ["Saved"],
        "the status follows the host"
    );

    let mut app = app.reopen().unwrap();
    let titles = app.read_document(|doc| {
        read::list_pages(doc)
            .into_iter()
            .map(|page| page.title)
            .collect::<Vec<_>>()
    });
    assert_eq!(titles, ["Groceries"], "the note is durable");
    assert_eq!(save_labels(&mut app), ["Saved"]);
}

/// Run frames only when the live host would: for queued work, an animation
/// or a due wake deadline, each followed by the idle pass that hands edits to
/// the record's writer. Returns once `until` holds or `budget` runs out.
fn idle(
    app: &mut TestApp<NotesShell>,
    budget: std::time::Duration,
    mut until: impl FnMut(&mut TestApp<NotesShell>) -> bool,
) -> bool {
    let started = std::time::Instant::now();
    loop {
        if until(app) {
            return true;
        }
        if started.elapsed() >= budget {
            return false;
        }
        let schedule = app.harness().frame_schedule();
        let woken = schedule.queued_work
            || schedule.needs_animation_frame
            || schedule
                .next_wake_deadline
                .is_some_and(|deadline| deadline <= ui::time::now());
        if woken {
            app.harness().relayout();
            app.harness().persist_tick();
        } else {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// Keys arrive one by one, slower than the status bubble lingers and faster
/// than the writer's debounce, so the status reads "Saving…" from the first
/// key to the last. The write then lands on the store's own thread with no
/// further input, and the status still moves to "Saved".
#[test]
fn a_note_typed_key_by_key_reads_saved_once_its_write_lands() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::new().unwrap();
    app.harness().click_named("notes.first_note").unwrap();
    for key in "Written on this device, one key at a time.".chars() {
        app.harness().type_text(&key.to_string());
        app.harness().persist_tick();
        idle(&mut app, std::time::Duration::from_millis(80), |_| false);
    }
    let saved = idle(&mut app, std::time::Duration::from_secs(10), |app| {
        save_labels(app) == ["Saved"]
    });
    assert!(
        saved,
        "the note's write landed, yet with no input the app still shows {:?}",
        save_labels(&mut app)
    );
}
