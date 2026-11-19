//! Applications opened from the shell's search: what a row of it opens, and
//! that the shell goes on drawing however many are opened in a row.

use super::*;

/// A catalogue of ten applications, as a workspace's own roster of them is:
/// what the shell's search offers and what a row of it opens.
const CATALOGUE: [&str; 10] = [
    "Browser",
    "Calculator",
    "Calendar",
    "Camera",
    "Canvas",
    "Chat",
    "Clocks",
    "Contacts",
    "Notes",
    "Sheets",
];

/// A shell whose search offers [`CATALOGUE`]: each row opens that app's own
/// window, as a workspace with those apps installed does.
#[derive(Default)]
struct Catalogue;

impl TileSource for Catalogue {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str, _: Option<u64>) -> Option<Tab> {
        let title = app.strip_prefix("so.tilo.")?.to_owned();
        Some(Tab {
            id,
            title: title.clone(),
            tile: TileContent::LocalApp {
                app_id: app.into(),
                label: title,
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        })
    }

    fn search_applications(&self) -> Vec<tilo_desktop_search::providers::AppEntry> {
        CATALOGUE
            .into_iter()
            .map(|title| tilo_desktop_search::providers::AppEntry {
                id: format!("so.tilo.{}", title.to_lowercase()),
                title: title.into(),
                category: Some("Applications".into()),
            })
            .collect()
    }
}

/// Every application the search offers, opened from it in a row: each Enter
/// leaves its window on the shell by the next frame, and the shell goes on
/// drawing after all of them. A shell that stopped presenting after some number
/// of opens would swallow the next person's Enter, and the page above it would
/// show a search that never closed.
#[test]
fn every_open_from_the_search_in_a_row_shows_its_window_and_the_shell_keeps_drawing() {
    let mut minter = IdMinter::new(7);
    // A window of an app the catalogue does not offer, so every row of it is a
    // new window: a row for an app already on the shell brings that one forward,
    // as it should, and adds none.
    let scratch = tab(&mut minter, "Scratch");
    let tree = seed_tree(&mut minter, "Office", vec![scratch]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Catalogue));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    let mut chrome = chrome_at(&handle, (1200, 800));
    for (index, app) in CATALOGUE.iter().enumerate() {
        let before = handle.snapshot("g", vec![]).entries.len();
        handle.toggle_search();
        driver.relayout();
        chrome.relayout();
        type_search(
            &mut driver,
            &mut chrome,
            &handle,
            app.to_lowercase().as_str(),
        );
        key_search(&mut driver, &mut chrome, &handle, "Enter");
        let after = handle.snapshot("g", vec![]);
        assert_eq!(
            after.entries.len(),
            before + 1,
            "opening {app} ({} of {}) left no window: {:?}",
            index + 1,
            CATALOGUE.len(),
            after
                .entries
                .iter()
                .map(|e| e.label.clone())
                .collect::<Vec<_>>()
        );
        assert!(
            after
                .entries
                .iter()
                .any(|entry| entry.label == app.to_lowercase()),
            "the window opened for {app} is not the app's"
        );
        // The shell finishes what the open began — it never waits for a frame
        // that no one asks for.
        settle_still(&mut driver);
    }
}

/// The person's chord is Search all Tilo: pressed while one window's own
/// search is up, it opens the shell's search over the workspace, and only the
/// press after that closes it.
#[test]
fn the_desktops_chord_opens_the_desktops_search_over_a_windows_own() {
    let (mut driver, handle, _) = mount();
    driver.relayout();

    // A window's own search, as a press in its field leaves it up.
    handle.open_search(
        Scope::InApp(ui::search::AppScope::of("so.tilo.notes")),
        None,
    );
    driver.relayout();
    assert!(handle.search_open(), "the window's search opened");
    assert_eq!(
        handle.search_scope().as_deref(),
        Some("app:so.tilo.notes"),
        "the search in the window is the window's"
    );

    handle.toggle_search();
    driver.relayout();
    assert!(
        handle.search_open(),
        "the chord kept a search up: it is Search all Tilo, not a close of the app's"
    );
    assert_eq!(
        handle.search_scope().as_deref(),
        Some("workspace"),
        "the chord opened the shell's search over the workspace"
    );

    handle.toggle_search();
    driver.relayout();
    assert!(
        !handle.search_open(),
        "a press over the shell's own search closes it"
    );
}

/// A source that answers on a worker once the test lets it: what a source reading a disk or a
/// service is, whose answer comes after the pass that asked it.
struct Held(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl tilo_desktop_search::Source for Held {
    fn id(&self) -> &'static str {
        "plan"
    }

    fn query(&self, needle: &str) -> Vec<tilo_desktop_search::Hit> {
        let (answers, opened) = &*self.0;
        drop(
            opened
                .wait_while(
                    answers
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner),
                    |answers| !*answers,
                )
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        if !needle.contains("quarterly") {
            return Vec::new();
        }
        vec![tilo_desktop_search::Hit {
            row: None,
            preview: None,
            faces: Vec::new(),
            outcome: None,
            kind: ui::search::system::ResourceKind::Note,
            provider: "plan".into(),
            id: "quarterly-plan".into(),
            title: "Quarterly plan".into(),
            subtitle: None,
            facts: Vec::new(),
            score: ui::search::system::MAX_PROVIDER_SCORE,
            icon: None,
            target: tilo_desktop_search::Target::Window { tab: 0, app: None },
        }]
    }
}

/// What adds [`Held`] to the shell's search.
struct HeldSearch(std::sync::Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>);

impl tilo_desktop_surface::SearchHost for HeldSearch {
    fn feed(&self, _: &str, _: &Scope) -> tilo_desktop_surface::SearchHostFeed {
        tilo_desktop_surface::SearchHostFeed {
            sources: vec![tilo_desktop_search::Held::new(Held(std::sync::Arc::clone(
                &self.0,
            )))],
            ..Default::default()
        }
    }

    fn performs(&self, _: &tilo_desktop_search::Target) -> bool {
        false
    }
}

/// A source that answers after the pass that asked it asks for the shell's next pass, with no
/// pass drawing: a page that draws only when asked shows the row then, rather than whenever
/// something else happens to draw.
#[test]
fn a_search_answer_after_the_pass_asks_for_the_next_pass() {
    let held = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
    let mut minter = IdMinter::new(7);
    let scratch = tab(&mut minter, "Scratch");
    let tree = seed_tree(&mut minter, "Office", vec![scratch]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Catalogue));
    handle.set_viewport_width(1200.0);
    let wakes = std::sync::Arc::new((std::sync::Mutex::new(0_usize), std::sync::Condvar::new()));
    handle.set_wake({
        let wakes = std::sync::Arc::clone(&wakes);
        std::sync::Arc::new(move || {
            let (count, woken) = &*wakes;
            *count
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
            woken.notify_all();
        })
    });
    handle.set_search_host(Rc::new(HeldSearch(std::sync::Arc::clone(&held))));
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    driver.set_caller_palette_enabled(false);
    driver.relayout();
    let mut chrome = chrome_at(&handle, (1200, 800));
    handle.toggle_search();
    driver.relayout();
    chrome.relayout();
    chrome.type_text("quarterly");
    chrome.relayout();
    driver.relayout();
    chrome.relayout();
    // The passes asked the source, which has not answered: the wakes so far are theirs.
    let (count, woken) = &*wakes;
    let before = *count
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let (answers, opened) = &*held;
    *answers
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
    opened.notify_all();
    let (after, _) = woken
        .wait_timeout_while(
            count
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            support::SEARCH_STEP,
            |count| *count == before,
        )
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    assert!(
        *after > before,
        "the source answered and nothing asked for the pass that shows it"
    );
    drop(after);
    settle_search(&mut driver, &mut chrome, &handle);
    assert!(
        handle
            .search_rows()
            .1
            .iter()
            .any(|row| row.title == "Quarterly plan"),
        "the pass the answer asked for shows its row: {:?}",
        handle.search_rows()
    );
}
