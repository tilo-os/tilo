//! A notebook someone has typed in for a while opens as fast as a new one.
//! Every keystroke is a change in the record's history, so a launch that
//! replayed or described that history would grow with every word ever
//! typed; the launch reads the current state and leaves the history to the
//! History view, which describes it when opened, a page at a time.

use notes::actions::create_page_action;
use notes::read;
use notes::{NotesShell, PageId};
use tilo_document::RichTextAction;
use tilo_document::action::RegionKind;
use ui::framework::testing::TestApp;
use ui::tilo_resource::Invoke;

/// About the history of a thirty-line note typed one character at a time.
const KEYSTROKES: usize = 4_000;

/// Opening the notebook, from reading its record to a settled first frame,
/// in an unoptimised test build on a shared CI runner. Describing the typed
/// history alone took seconds per thousand changes here, so a launch that
/// does it again fails by a wide margin.
const OPEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

fn typed_notebook(directory: &std::path::Path) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::open_data_dir(directory).unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "Bugs")).unwrap())
        .unwrap();
    let mut peer = app.peer_document();
    let block = read::page_block_snapshots(&peer, page)[0].0.clone();
    let line = "the sidebar flickers when a note is renamed twice ";
    for (at, ch) in line.chars().cycle().take(KEYSTROKES).enumerate() {
        crate::component_support::edit_page_body(
            &mut peer,
            page,
            &RichTextAction::SpliceText {
                region: RegionKind::Body,
                block: block.clone(),
                at,
                del: 0,
                ins: ch.to_string(),
            },
        )
        .unwrap();
        ui::tilo_resource::history::commit_stamped(&mut peer);
    }
    app.merge_peer(&mut peer).unwrap();
    app.flush().unwrap();
    assert!(app.versions().len() >= KEYSTROKES);
}

#[test]
fn a_notebook_with_a_long_typing_history_opens_within_budget() {
    let directory = tempfile::tempdir().unwrap();
    typed_notebook(directory.path());

    let started = std::time::Instant::now();
    let mut app = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
    let opened = started.elapsed();
    eprintln!("opened a notebook with {KEYSTROKES} typed changes in {opened:?}");
    assert!(app.harness().targets().contains_key("block_editor"));
    assert!(
        opened < OPEN_BUDGET,
        "opening a notebook with {KEYSTROKES} typed changes took {opened:?} (budget {OPEN_BUDGET:?})"
    );
}

#[test]
fn history_of_a_long_typing_history_opens_at_its_latest_page() {
    let directory = tempfile::tempdir().unwrap();
    typed_notebook(directory.path());
    let mut app = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
    let harness = app.harness();

    let started = std::time::Instant::now();
    harness
        .open_history()
        .expect("the published history action opens the canonical timeline");
    let opened = started.elapsed();
    eprintln!("opened History over {KEYSTROKES} typed changes in {opened:?}");
    assert!(harness.targets().contains_key("activity_timeline"));
    assert!(
        opened < OPEN_BUDGET,
        "opening History over {KEYSTROKES} typed changes took {opened:?} (budget {OPEN_BUDGET:?})"
    );

    // The rest of the history is a page at a time away.
    assert!(harness.targets().contains_key("history-earlier"));
    let started = std::time::Instant::now();
    harness.click_named("history-earlier").unwrap();
    let paged = started.elapsed();
    eprintln!("listed an earlier page in {paged:?}");
    assert!(
        paged < OPEN_BUDGET,
        "listing an earlier page took {paged:?} (budget {OPEN_BUDGET:?})"
    );
}
