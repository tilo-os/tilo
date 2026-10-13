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

/// A history a hundred times shorter: what opening costs when the history
/// hardly matters, measured beside the long one on the same machine.
const FEW_KEYSTROKES: usize = KEYSTROKES / 100;

/// Opening the notebook, from reading its record to a settled first frame,
/// in an unoptimised test build on an idle runner. Describing the typed
/// history alone took seconds per thousand changes here, so a launch that
/// does it again fails by a wide margin.
const OPEN_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// How many times the short history's cost the long one may take. Work that
/// grows with the history takes about a hundred times as long; a busy runner
/// slows both alike.
const GROWTH: u32 = 10;

/// The budget for an operation that took `short` over the short history:
/// [`OPEN_BUDGET`] on an idle runner, more when the runner is busy.
fn budget(short: std::time::Duration) -> std::time::Duration {
    OPEN_BUDGET.max(short.saturating_mul(GROWTH))
}

fn typed_notebook(directory: &std::path::Path) {
    typed_notebook_of(directory, KEYSTROKES);
}

fn typed_notebook_of(directory: &std::path::Path, keystrokes: usize) {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<NotesShell>::open_data_dir(directory).unwrap();
    let page = PageId::new_v4();
    app.invoke(Invoke::apply(&create_page_action(page, None, "Bugs")).unwrap())
        .unwrap();
    let mut peer = app.peer_document();
    let block = read::page_block_snapshots(&peer, page)[0].0.clone();
    let line = "the sidebar flickers when a note is renamed twice ";
    for (at, ch) in line.chars().cycle().take(keystrokes).enumerate() {
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
    assert!(app.versions().len() >= keystrokes);
}

/// How long opening the notebook in `directory` takes.
fn open_time(directory: &std::path::Path) -> std::time::Duration {
    let started = std::time::Instant::now();
    let mut app = TestApp::<NotesShell>::open_data_dir(directory).unwrap();
    let opened = started.elapsed();
    assert!(app.harness().targets().contains_key("block_editor"));
    opened
}

/// How long opening History over the notebook in `directory` takes.
fn history_open_time(directory: &std::path::Path) -> std::time::Duration {
    let mut app = TestApp::<NotesShell>::open_data_dir(directory).unwrap();
    let harness = app.harness();
    let started = std::time::Instant::now();
    harness
        .open_history()
        .expect("the published history action opens the canonical timeline");
    let opened = started.elapsed();
    assert!(harness.targets().contains_key("activity_timeline"));
    opened
}

#[test]
fn a_notebook_with_a_long_typing_history_opens_within_budget() {
    let short = tempfile::tempdir().unwrap();
    typed_notebook_of(short.path(), FEW_KEYSTROKES);
    let directory = tempfile::tempdir().unwrap();
    typed_notebook(directory.path());

    let budget = budget(open_time(short.path()));
    let opened = open_time(directory.path());
    eprintln!("opened a notebook with {KEYSTROKES} typed changes in {opened:?}");
    assert!(
        opened < budget,
        "opening a notebook with {KEYSTROKES} typed changes took {opened:?} (budget {budget:?})"
    );
}

#[test]
fn history_of_a_long_typing_history_opens_at_its_latest_page() {
    let short = tempfile::tempdir().unwrap();
    typed_notebook_of(short.path(), FEW_KEYSTROKES);
    let directory = tempfile::tempdir().unwrap();
    typed_notebook(directory.path());

    let short_opened = history_open_time(short.path());
    let mut app = TestApp::<NotesShell>::open_data_dir(directory.path()).unwrap();
    let harness = app.harness();

    let budget = budget(short_opened);
    let started = std::time::Instant::now();
    harness
        .open_history()
        .expect("the published history action opens the canonical timeline");
    let opened = started.elapsed();
    eprintln!("opened History over {KEYSTROKES} typed changes in {opened:?}");
    assert!(harness.targets().contains_key("activity_timeline"));
    assert!(
        opened < budget,
        "opening History over {KEYSTROKES} typed changes took {opened:?} (budget {budget:?})"
    );

    // The rest of the history is a page at a time away.
    assert!(harness.targets().contains_key("history-earlier"));
    let started = std::time::Instant::now();
    harness.click_named("history-earlier").unwrap();
    let paged = started.elapsed();
    eprintln!("listed an earlier page in {paged:?}");
    assert!(
        paged < budget,
        "listing an earlier page took {paged:?} (budget {budget:?})"
    );
}
