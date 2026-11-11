//! Applications opened from the desk's search: what a row of it opens, and
//! that the desk goes on drawing however many are opened in a row.

use super::*;

/// A catalogue of ten applications, as a workspace's own roster of them is:
/// what the desk's search offers and what a row of it opens.
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

/// A desk whose search offers [`CATALOGUE`]: each row opens that app's own
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

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
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
                description: None,
            })
            .collect()
    }
}

/// Every application the search offers, opened from it in a row: each Enter
/// leaves its window on the desk by the next frame, and the desk goes on
/// drawing after all of them. A desk that stopped presenting after some number
/// of opens would swallow the next person's Enter, and the page above it would
/// show a search that never closed.
#[test]
fn every_open_from_the_search_in_a_row_shows_its_window_and_the_desk_keeps_drawing() {
    let mut minter = IdMinter::new(7);
    // A window of an app the catalogue does not offer, so every row of it is a
    // new window: a row for an app already on the desk brings that one forward,
    // as it should, and adds none.
    let scratch = tab(&mut minter, "Scratch");
    let tree = seed_tree(&mut minter, "Desk", vec![scratch]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Catalogue));
    handle.set_viewport_width(1200.0);
    let mut driver = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        (1200, 800),
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
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
        // The desk finishes what the open began — it never waits for a frame
        // that no one asks for.
        settle_still(&mut driver);
    }
}
