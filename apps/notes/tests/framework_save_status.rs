//! The framework-hosted app reports the durability the host measures: a note
//! written to the record's storage reads "Saved", never the session-only
//! warning, and is still there after the app is closed and reopened.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

/// The save states the window announces on its own, as live regions.
fn announced_save_labels(app: &mut TestApp<NotesShell>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| {
            node.live()
                .is_some_and(|live| live != ui::accesskit::Live::Off)
        })
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .filter(|label| ["Saved", "Saving…", "Save failed", "Not saved"].contains(&label.as_str()))
        .collect()
}

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
    // Bytes that were only read count as durable once the store's thread has
    // synced them, so a reopened note can read "Saving…" for a moment. It
    // must turn "Saved" by itself: run frames only as the live host would
    // (the store's wake once its thread is done) until it does.
    let saved = idle(&mut app, tilo_test_env::hang_bound(), |app| {
        save_labels(app) == ["Saved"]
    });
    assert!(
        saved,
        "a reopened note reads Saved without input: {:?}",
        save_labels(&mut app)
    );
}

/// Run frames only when the live host would: for queued work (a wake the
/// record's store raised once a write landed among it), an animation or a
/// due wake deadline, each followed by the idle pass that hands edits to the
/// record's writer. Returns once `until` holds or `budget` runs out.
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

/// Keys arrive one by one, faster than the writer's debounce, so the status reads "Saving…" from the first
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
        assert_eq!(
            announced_save_labels(&mut app),
            Vec::<String>::new(),
            "autosave announces nothing"
        );
    }
    let saved = idle(&mut app, tilo_test_env::hang_bound(), |app| {
        save_labels(app) == ["Saved"]
    });
    assert!(
        saved,
        "the note's write landed, yet with no input the app still shows {:?}",
        save_labels(&mut app)
    );
}

/// Assistive technology hears what the screen shows: autosave cycling
/// between "Saving…" and "Saved" is silent, a failed save is said in the
/// window's bar (its notice is a polite live region), and so is the save
/// that ends it, once. The note's own tree announces nothing.
#[test]
fn only_a_failed_save_and_its_recovery_are_announced() {
    use ui::record::RecordSaveStatus;
    use ui::widget::Widget;
    fn live(node: &ui::a11y::AccessNode, out: &mut Vec<String>) {
        if node.live.is_some() {
            out.push(node.name.clone().unwrap_or_default());
        }
        for child in &node.children {
            live(child, out);
        }
    }
    ui::i18n::I18nManager::init("en-US");
    ui::window_toolbar::notify(None);
    let mut doc = automerge::AutoCommit::new();
    read::initialize(&mut doc);
    doc.commit();
    let mut app =
        notes::NotesApp::from_document(&doc, &ui::permissions::UserId("me".into())).unwrap();
    let status = ui::LocalState::new(RecordSaveStatus::Saved);
    app.follow_save_status(status.clone());
    let mut said = None;
    // What the bar newly says after the save state changes.
    let mut hear = |state: RecordSaveStatus| {
        status.set(state);
        app.sync(&doc, &automerge::ROOT);
        let mut own = Vec::new();
        live(&app.accessibility().unwrap(), &mut own);
        assert_eq!(
            own,
            Vec::<String>::new(),
            "the note announces nothing itself"
        );
        let now = ui::window_toolbar::notice().map(|notice| notice.text);
        let news = (now != said).then(|| now.clone()).flatten();
        said = now;
        news.into_iter().collect::<Vec<_>>()
    };
    for state in
        std::iter::repeat_n([RecordSaveStatus::Saving, RecordSaveStatus::Saved], 3).flatten()
    {
        assert_eq!(hear(state), Vec::<String>::new(), "autosave is silent");
    }
    let failed = hear(RecordSaveStatus::Failed("disk failure".into()));
    assert!(
        matches!(failed.as_slice(), [text] if text.starts_with("Save failed")),
        "a failed save is announced: {failed:?}"
    );
    assert_eq!(
        hear(RecordSaveStatus::Saving),
        Vec::<String>::new(),
        "the retry is quiet"
    );
    assert_eq!(
        hear(RecordSaveStatus::Saved),
        ["Saved"],
        "the recovery is announced"
    );
    assert_eq!(
        hear(RecordSaveStatus::Saving),
        Vec::<String>::new(),
        "and quiet after"
    );
}
