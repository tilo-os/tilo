//! Every app the web page offers opens from the shell's search on the next frame the shell draws.
//!
//! The page lists the apps its build ships in its search — its catalogue with the ones that cannot
//! run in a browser left out — and a person opens one with the search chord, the app's name and
//! Enter. What they wait for is the app's window on the shell: a window on the shell they already
//! have, whose body the app's own code fills in when it arrives, so that code arriving never holds
//! the window back. That is one frame of the shell: past it the shell reads as not having heard the
//! person.
//!
//! This walks the page's own list through the shell's real search in one process, so a slow or
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

use crate::support;

use std::time::{Duration, Instant};

use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::a11y::{AccessNode, Role};
use ui::widget::TileHost;

use support::{chrome_at, search_settled};

/// The size a page's window is drawn at: a laptop's.
const WINDOW: (u32, u32) = (1280, 800);

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

/// The page's apps, as the shell's search offers them to open and as its launches open them.
struct WebApps(Vec<PageApp>);

impl TileSource for WebApps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str, _: Option<u64>) -> Option<Tab> {
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
            })
            .collect()
    }
}

/// A shell in a workspace of its own with no window open, offering `apps` in its search: every app is
/// opened from nothing, so the window that arrives is the launch's and its frame is the launch's
/// measure.
fn mount_page(apps: &[PageApp]) -> (ui::headless::FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tree = seed_tree(&mut minter, "Office", Vec::new());
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(WebApps(apps.to_vec())));
    handle.set_viewport_width(f64::from(WINDOW.0));
    let mut shell = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        WINDOW,
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    shell.set_caller_palette_enabled(false);
    shell.relayout();
    (shell, handle)
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

/// Close the search a failed app left up, so the next one starts from the shell as a person's would:
/// the host closes wherever it stands, whatever was typed.
fn close_search(
    shell: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
) -> Result<(), String> {
    if !handle.search_open() {
        return Ok(());
    }
    handle.close_search();
    shell.relayout();
    chrome.relayout();
    (!handle.search_open())
        .then_some(())
        .ok_or_else(|| "the host closed the search and the shell kept it up".to_owned())
}

/// Open `app` from the search as the page does: the search chord, its name typed, its row first,
/// Enter. `Ok` carries how long the open took — from Enter to the window laid out on the shell's
/// next frame — and `Err` names the step that did not happen.
fn open_from_search(
    shell: &mut ui::headless::FrameHarness,
    chrome: &mut ui::headless::FrameHarness,
    handle: &DesktopHandle,
    app: &PageApp,
) -> Result<Duration, String> {
    close_search(shell, chrome, handle)?;

    chrome.set_modifiers(&["ctrl".into()]);
    chrome.key("k");
    chrome.set_modifiers(&[]);
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    if !handle.search_open() {
        return Err("the search chord opened no search".into());
    }

    let query = app.title.to_lowercase();
    chrome.type_text(&query);
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    if !search_settled(shell, chrome, handle) {
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

    // Enter, then the shell's own frame: the window it opens is on the shell on that frame, and its
    // app's code filling it in later is what no person waits for.
    let pressed = Instant::now();
    chrome.key("Enter");
    chrome.relayout();
    shell.relayout();
    let open = pressed.elapsed();

    let snapshot = handle.snapshot("g", Vec::new());
    let Some(entry) = snapshot
        .entries
        .iter()
        .find(|entry| entry.app.as_deref() == Some(app.id.as_str()))
    else {
        return Err(format!(
            "the shell drew no window of it on the next frame; it listed {:?}",
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
        rect.width() > 0.0 && rect.x0 >= -0.5 && rect.x1 <= f64::from(WINDOW.0) + 0.5
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
    let (mut shell, handle) = mount_page(&apps);
    let mut chrome = chrome_at(&handle, WINDOW);
    chrome.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));

    let started = Instant::now();
    let mut missed = Vec::new();
    for app in &apps {
        match open_from_search(&mut shell, &mut chrome, &handle, app) {
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

/// An app's window open under another name — a sheet window called "Sheet1",
/// as the workbook names it — still leaves the app's own row first for its
/// name: the window is where the person already was, and the app is what they
/// typed.
#[test]
fn an_open_window_under_another_name_does_not_take_the_apps_row_away() {
    let apps = page_apps();
    let Some(sheets) = apps.iter().find(|app| app.title == "Sheets").cloned() else {
        panic!("the page's catalogue lists Sheets");
    };
    let mut minter = IdMinter::new(7);
    let tab = Tab {
        id: minter.mint(),
        title: "Sheet1".into(),
        tile: TileContent::LocalApp {
            app_id: sheets.id.clone(),
            label: "Sheet1".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let tree = seed_tree(&mut minter, "Office", vec![tab]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(WebApps(apps)));
    handle.set_viewport_width(f64::from(WINDOW.0));
    let mut shell = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        WINDOW,
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    shell.set_caller_palette_enabled(false);
    shell.relayout();
    let mut chrome = chrome_at(&handle, WINDOW);
    chrome.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));

    chrome.set_modifiers(&["ctrl".into()]);
    chrome.key("k");
    chrome.set_modifiers(&[]);
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    assert!(handle.search_open(), "the search chord opened a search");
    chrome.type_text("sheets");
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    assert!(
        search_settled(&mut shell, &mut chrome, &handle),
        "the search for \"sheets\" settled"
    );
    let rows = result_names(&chrome);
    assert_eq!(
        rows.first().map(String::as_str),
        Some(sheets.title.as_str()),
        "the app's row leads for its name, with its window open under another: {rows:?}"
    );
}

/// A window is found by the app it shows as well as by what it shows: a sheet window named after
/// its workbook ("Sheet1") is a Sheets window, and a person typing "sheets" finds it under the
/// app's own row.
#[test]
fn a_sheet_window_is_found_by_its_apps_name() {
    let apps = page_apps();
    let Some(sheets) = apps.iter().find(|app| app.title == "Sheets").cloned() else {
        panic!("the page's catalogue lists Sheets");
    };
    let mut minter = IdMinter::new(7);
    let tab = Tab {
        id: minter.mint(),
        title: "Sheet1".into(),
        tile: TileContent::LocalApp {
            app_id: sheets.id.clone(),
            label: "Sheet1".into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    };
    let tree = seed_tree(&mut minter, "Office", vec![tab]);
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(WebApps(apps)));
    handle.set_viewport_width(f64::from(WINDOW.0));
    let mut shell = ui::headless::FrameHarness::new_with_controlled_inputs(
        ui::state::State::new(),
        Box::new(surface),
        WINDOW,
        ui::Theme::dark(),
        ui::TextRenderer::new(),
        ui::permissions::MemberId::person("tester"),
    );
    shell.set_caller_palette_enabled(false);
    shell.relayout();
    let mut chrome = chrome_at(&handle, WINDOW);
    chrome.set_platform_conventions(ui::platform::PlatformConventions::new(
        ui::platform::Platform::Linux,
    ));

    chrome.set_modifiers(&["ctrl".into()]);
    chrome.key("k");
    chrome.set_modifiers(&[]);
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    assert!(handle.search_open(), "the search chord opened a search");
    chrome.type_text("sheets");
    chrome.relayout();
    shell.relayout();
    chrome.relayout();
    assert!(
        search_settled(&mut shell, &mut chrome, &handle),
        "the search for \"sheets\" settled"
    );
    let rows = result_names(&chrome);
    assert_eq!(
        rows.first().map(String::as_str),
        Some(sheets.title.as_str()),
        "the app's row leads for its name: {rows:?}"
    );
    assert!(
        rows.iter().any(|row| row == "Sheet1 — Sheets"),
        "the window on the workbook is found by its app's name: {rows:?}"
    );
}
