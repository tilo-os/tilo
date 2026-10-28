//! New note creates a note whether Notes is the window's root component
//! (the sidebar's "+") or one tab of a desktop surface, the way the browser
//! desktop hosts every app (the tab's window bar). Its click travels as a registered control invocation, so
//! a tile the desktop builds after the harness bound the tree's controls must
//! still deliver what its buttons emit.

use crate::component_support::{self, NotebookFixture};
use std::{cell::RefCell, rc::Rc};
use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::widget::TileHost;

fn page_count(f: &NotebookFixture, checkout: &ui::record::RecordCheckout) -> usize {
    f.records
        .read(&checkout.id, |doc| notes::read::list_pages(doc).len())
        .unwrap()
}

/// Answers with a notice until its component arrives, as the browser
/// desktop does while a tab's record is still loading.
struct Arriving(Rc<RefCell<Option<AppComponent>>>);
impl TileSource for Arriving {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        match self.0.borrow_mut().take() {
            Some(component) => TileHost::from_component(component),
            None => TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: "Loading".into(),
            }),
        }
    }
}

/// Pressing New note (`press`: the sidebar's "+", or the window bar's New
/// note where the bar hosts the window) creates a page.
fn assert_plus_creates_a_page(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    harness: &mut FrameHarness,
    press: impl FnOnce(&mut FrameHarness),
) {
    let before = page_count(f, checkout);
    press(harness);
    for _ in 0..4 {
        harness.poll_frame().unwrap();
    }
    assert!(
        harness.control_failures().is_empty(),
        "{:?}",
        harness.control_failures()
    );
    assert_eq!(
        page_count(f, checkout),
        before.checked_add(1).unwrap(),
        "the + button creates a page"
    );
}

#[test]
fn hosted_header_plus_creates_a_page_as_root_component() {
    ui::i18n::I18nManager::init("en-US");
    let f = NotebookFixture::rich();
    let checkout = component_support::notebook(&f);
    let component = component_support::component(&f, 7, component_support::existing(&checkout));
    let mut harness = component_support::component_driver(component);
    harness.relayout();
    assert_plus_creates_a_page(&f, &checkout, &mut harness, |harness| {
        harness.click_named("new-note").unwrap();
    });
}

#[test]
fn hosted_header_plus_creates_a_page_in_a_desktop_tab_built_late() {
    ui::i18n::I18nManager::init("en-US");
    let f = NotebookFixture::rich();
    let checkout = component_support::notebook(&f);
    let mut minter = IdMinter::new(7);
    let tab = Tab {
        id: minter.mint(),
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: "so.tilo.notes".into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
    };
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Desk", vec![tab]);
    let arriving = Rc::new(RefCell::new(None));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Arriving(arriving.clone())));
    handle.set_viewport_width(1800.0);
    let mut harness = FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(surface),
        (1800, 900),
        ui::Theme::dark(),
        component_support::renderer(),
        f.author.clone(),
    );
    let wakes = ui::owner_wake::OwnerWakeQueue::default();
    harness.attach_owner_wake_queue(wakes.clone()).unwrap();
    harness.relayout();

    // The record arrives after the desktop is already running.
    let mut component = component_support::component(&f, 7, component_support::existing(&checkout));
    component.mount_with_owner_queue(&wakes, |_| {}).unwrap();
    *arriving.borrow_mut() = Some(component);
    handle.rebuild(tab_id);
    for _ in 0..3 {
        harness.poll_frame().unwrap();
        harness.relayout();
    }
    assert!(arriving.borrow().is_none(), "the desktop rebuilt the tab");

    // The desktop draws the tab's bar: New note is its action there.
    assert_plus_creates_a_page(&f, &checkout, &mut harness, |harness| {
        let new_note = harness
            .a11y_node_named("New note")
            .expect("the tab's bar offers New note");
        harness
            .perform_a11y_action(new_note, &ui::a11y::Action::Activate)
            .unwrap();
    });
}
