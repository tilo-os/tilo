//! Every app the web page offers opens from the desk's search on the next frame the desk draws.
//!
//! The page lists the apps its build ships in its search — its catalogue with the ones that cannot
//! run in a browser left out — and a person opens one with the search chord, the app's name and
//! Enter. What they wait for is the app's window on the desk: a window on the desk they already
//! have, whose body the app's own code fills in when it arrives, so that code arriving never holds
//! the window back. That is one frame of the desk: past it the desk reads as not having heard the
//! person.
//!
//! This walks the page's own list through the desk's real search in one process, so a slow or
//! crowded-out app is named in seconds. It replaces a walk of the same list through a headless
//! browser, which opened the apps one after another on a software rasteriser and took minutes for
//! each. An app that loses the first row of its own name to something else, or whose window comes
//! later than the frame after Enter, is a finding about the product and fails this test by name.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test fixtures: a failed lookup is the assertion"
)]

mod support;

use std::time::{Duration, Instant};

use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::a11y::{AccessNode, Role};
use ui::widget::TileHost;

use support::{chrome_at, search_settled};

/// The desk a page draws at: a window on a laptop.
const DESK: (u32, u32) = (1280, 800);

/// An app the page's search offers, as its catalogue lists it.
#[derive(Clone)]
struct PageApp {
    /// The package id a launch names it by.
    id: String,
    /// The name the person reads, which is what they type.
    title: String,
}

/// Every app the page's search offers: the bundle's catalogue
/// (`crates/browser/tilo-web-desktop/apps.json`), with the ones that do not run in a browser left
/// out as the page leaves them out.
fn page_apps() -> Vec<PageApp> {
    let catalogue: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../browser/tilo-web-desktop/apps.json"
    )))
    .expect("the page's catalogue is JSON");
    catalogue["applications"]
        .as_array()
        .expect("a list of applications")
        .iter()
        .filter_map(|app| {
            let id = app["id"].as_str()?;
            let title = app["name"].as_str()?;
            tilo_desktop_search::availability::runs_on(id, &["web"]).then(|| PageApp {
                id: id.into(),
                title: title.into(),
            })
        })
        .collect()
}

/// The page's apps, as the desk's search offers them to open and as its launches open them.
struct WebApps(Vec<PageApp>);

impl TileSource for WebApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str) -> Option<Tab> {
        let title = self
            .0
            .iter()
            .find(|offered| offered.id == app)?
            .title
            .clone();
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
        self.0
            .iter()
            .map(|offered| tilo_desktop_search::providers::AppEntry {
                id: offered.id.clone(),
                title: offered.title.clone(),
                category: None,
                description: None,
            })
            .collect()
    }
}

/// A desk in a workspace of its own with no window open, offering `apps` in its search: every app is
/// opened from nothing, so the window that arrives is the launch's and its frame is the launch's
/// measure.
fn mount_page(apps: &[PageApp]) -> (ui::headless::FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tree = seed_tree(&mut minter, "Desk", Vec::new());
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(WebApps(apps.to_vec())));
    handle.set_viewport_width(f64::from(DESK.0));
    let mut desk = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        DESK,
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::UserId("tester".into()),
    );
    desk.set_caller_palette_enabled(false);
    desk.relayout();
    (desk, handle)
}

/// The open palette's card.
fn card(palette: &ui::headless::InspectionNode) -> &AccessNode {
    palette
        .semantics
        .as_ref()
        .expect("the open search's card says what it shows")
}

/// The rows the open search lists, first to last, in the names the person reads.
fn result_names(chrome: &ui::headless::FrameHarness) -> Vec<String> {
    fn find<'a>(
        node: &'a ui::headless::InspectionNode,
        widget: &str,
    ) -> Option<&'a ui::headless::InspectionNode> {
        if node.widget == widget {
            return Some(node);
        }
        node.children.iter().find_map(|child| find(child, widget))
    }
    fn rows(node: &AccessNode, out: &mut Vec<String>) {
        if node.role == Role::ListItem {
            out.push(node.name.clone().unwrap_or_default());
        }
        for child in &node.children {
            rows(child, out);
        }
    }
    let inspected = chrome.inspection();
    let palette = find(&inspected.root, "ActionPalette").expect("the open search's card");
    let mut out = Vec::new();
    for child in &card(palette).children {
        rows(child, &mut out);
    }
    out
}

/// Close the search a failed app left up, so the next one starts from the desk as a person's would:
/// the host closes wherever it stands, whatever was typed.
fn close_search(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
) -> Result<(), String> {
    if !handle.search_open() {
        return Ok(());
    }
    handle.close_search();
    desk.relayout();
    chrome.relayout();
    (!handle.search_open())
        .then_some(())
        .ok_or_else(|| "the host closed the search and the desk kept it up".to_owned())
}

/// Open `app` from the search as the page does: the search chord, its name typed, its row first,
/// Enter. `Ok` carries how long the open took — from Enter to the window laid out on the desk's
/// next frame — and `Err` names the step that did not happen.
fn open_from_search(
    desk: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    app: &PageApp,
) -> Result<Duration, String> {
    close_search(desk, chrome, handle)?;

    chrome.set_modifiers(&["ctrl".into()]);
    chrome.key("k");
    chrome.set_modifiers(&[]);
    chrome.relayout();
    desk.relayout();
    chrome.relayout();
    if !handle.search_open() {
        return Err("the search chord opened no search".into());
    }

    let query = app.title.to_lowercase();
    chrome.type_text(&query);
    chrome.relayout();
    desk.relayout();
    chrome.relayout();
    if !search_settled(desk, chrome, handle) {
        return Err(format!(
            "the search for {query:?} still expected rows {} s after they were typed",
            support::SEARCH_STEP.as_secs()
        ));
    }

    let rows = result_names(chrome);
    match rows.first() {
        Some(first) if *first == app.title => {}
        Some(first) => {
            return Err(format!(
                "the first row for {query:?} was {first:?}, not {:?}; the card listed {rows:?}",
                app.title
            ));
        }
        None => return Err(format!("the search for {query:?} listed no row")),
    }

    // Enter, then the desk's own frame: the window it opens is on the desk on that frame, and its
    // app's code filling it in later is what no person waits for.
    let pressed = Instant::now();
    chrome.key("Enter");
    chrome.relayout();
    desk.relayout();
    let open = pressed.elapsed();

    let snapshot = handle.snapshot("g", Vec::new());
    let Some(entry) = snapshot
        .entries
        .iter()
        .find(|entry| entry.app.as_deref() == Some(app.id.as_str()))
    else {
        return Err(format!(
            "the desk drew no window of it on the next frame; it listed {:?}",
            snapshot
                .entries
                .iter()
                .map(|entry| entry.label.as_str())
                .collect::<Vec<_>>()
        ));
    };
    if snapshot.active.as_ref() != Some(&entry.route) {
        return Err("its window opened, but not as the focused one".into());
    }
    let tab = entry
        .route
        .ids()
        .map_err(|why| format!("its window's route is not a place: {why}"))?
        .tab;
    let drawn = handle
        .shown_tabs()
        .into_iter()
        .find(|(shown, _)| *shown == tab)
        .map(|(_, rect)| rect);
    let on_screen = drawn.is_some_and(|rect| {
        rect.width() > 0.0 && rect.x0 >= -0.5 && rect.x1 <= f64::from(DESK.0) + 0.5
    });
    if !on_screen {
        return Err(format!(
            "its window was not laid out on screen on the next frame ({drawn:?})"
        ));
    }
    if handle.search_open() {
        return Err("the search stayed up after its row was chosen".into());
    }
    Ok(open)
}

#[test]
fn every_app_the_page_offers_opens_from_search_on_the_next_frame() {
    let apps = page_apps();
    assert!(
        apps.len() > 20,
        "the page's catalogue lists the apps its build ships: {}",
        apps.len()
    );
    let (mut desk, handle) = mount_page(&apps);
    let mut chrome = chrome_at(&handle, DESK);
    chrome.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));

    let started = Instant::now();
    let mut missed = Vec::new();
    for app in &apps {
        match open_from_search(&mut desk, &mut chrome, &handle, app) {
            Ok(open) => println!("TIMING app={} ms={}", app.id, open.as_millis()),
            Err(step) => missed.push(format!("{}: {step}", app.id)),
        }
    }
    println!(
        "TIMING total ms={} apps={}",
        started.elapsed().as_millis(),
        apps.len()
    );
    assert!(
        missed.is_empty(),
        "{} of the {} apps the page offers did not open from search on the next frame:\n  {}",
        missed.len(),
        apps.len(),
        missed.join("\n  ")
    );
}
