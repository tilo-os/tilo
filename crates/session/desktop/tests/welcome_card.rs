//! The welcome card on an empty desk: one press opens one window, on the pass the desk asks its
//! host for. A host draws only when something asks it to, so a desk that answered the press in its
//! own pass and left the host idle would open the window whenever an unrelated frame came — and a
//! person who pressed again meanwhile would have two windows open.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use tilo_desktop::desktop_host::{DeskLayer, DesktopAsk};
use tilo_desktop::launchpad::CollaborationLaunchpad;
use tilo_desktop_model::{DesktopTree, Strip, StripId, Tab, TileContent, Workspace, WorkspaceSlot};
use tilo_desktop_surface::navigation::ChromeEffect;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource};
use ui::headless::FrameHarness;
use ui::kurbo::Point;
use ui::widget::TileHost;

/// A desk with a workspace, a strip and no window: the welcome card stands on it.
fn empty_desk() -> DesktopTree {
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip::new(StripId(2), "Main")];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
    tree
}

/// The host's tabs: a window for whatever a launch names, showing a notice.
struct Apps;

impl TileSource for Apps {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }

    fn launch(&mut self, _: &Workspace, id: u64, app: &str, _: Option<u64>) -> Option<Tab> {
        Some(Tab {
            id,
            title: app.to_owned(),
            app_title: None,
            tile: TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            view: String::new(),
            pinned: false,
        })
    }
}

/// An empty desk and the host holding it: what it shows, what it asks of the host, and how many
/// times it asked for the pass that answers the ask.
struct Desk {
    frame: FrameHarness,
    handle: DesktopHandle,
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    woken: Rc<Cell<u64>>,
}

impl Desk {
    /// The desk as its host mounts it: the host wakes, counted in `woken`, when the desk asks for a
    /// pass.
    fn new() -> Self {
        let (surface, handle) = DesktopSurface::new(empty_desk(), 7, Box::new(Apps));
        handle.set_viewport_width(1280.0);
        let asks: Rc<RefCell<Vec<DesktopAsk>>> = Rc::default();
        let woken = Rc::new(Cell::new(0u64));
        handle.set_wake({
            let woken = Rc::clone(&woken);
            Rc::new(move || woken.set(woken.get().wrapping_add(1)))
        });
        let layer = DeskLayer::new(
            Box::new(surface),
            handle.clone(),
            CollaborationLaunchpad::new(),
            Rc::clone(&asks),
            Vec::new(),
        );
        let mut frame = FrameHarness::for_root(
            ui::state::State::new(),
            layer,
            (1280, 800),
            ui::Theme::dark(),
        );
        frame.relayout();
        Self {
            frame,
            handle,
            asks,
            woken,
        }
    }

    /// What the desk asked of its host since the last call.
    fn asked(&self) -> Vec<DesktopAsk> {
        std::mem::take(&mut *self.asks.borrow_mut())
    }
}

/// Where the welcome card `label` stands, as the reader is told: the card a person presses.
fn card(frame: &FrameHarness, label: &str) -> Point {
    let node = frame
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.label() == Some(label))
                .then(|| node.bounds())
                .flatten()
        })
        .expect("the welcome card names its place on the empty desk");
    Point::new(node.x0.midpoint(node.x1), node.y0.midpoint(node.y1))
}

/// The apps of the windows on the desk, in the order they stand.
fn windows(handle: &DesktopHandle) -> Vec<String> {
    handle.read(|tree| {
        tree.tabs()
            .filter_map(|tab| tilo_desktop_model::apps::canonical_app_id(&tab.tile))
            .map(String::from)
            .collect()
    })
}

/// One press on the welcome card's Notes asks its host once, and asks it for the pass that opens
/// the window: the window is there on that pass, with no wait on anything else.
#[test]
fn one_press_on_the_welcome_card_opens_one_window_on_the_pass_it_asks_for() {
    let mut desk = Desk::new();
    assert!(
        windows(&desk.handle).is_empty(),
        "an empty desk has no window"
    );
    let notes = card(&desk.frame, "Notes");

    desk.frame.click_at(notes.x, notes.y);
    assert_eq!(
        desk.asked(),
        [DesktopAsk::HostAction("launchpad-notes".to_owned())],
        "one press asks for Notes once"
    );
    assert!(
        desk.woken.get() > 0,
        "the desk asked its host for the pass that opens it, rather than leaving the choice \
         waiting for whatever frame came next"
    );

    // The host carries the choice out as a host does, and the pass it asks for draws the window.
    let woken_before = desk.woken.get();
    desk.handle
        .dispatch(ChromeEffect::OpenApp {
            app: "so.tilo.notes".to_owned(),
            beside: None,
            background: None,
            float: None,
        })
        .expect("Notes opens on the desk");
    assert!(
        desk.woken.get() > woken_before,
        "a queued open asks for the pass that applies it"
    );
    desk.frame.relayout();
    assert_eq!(
        windows(&desk.handle),
        ["so.tilo.notes"],
        "one window, and nothing else"
    );

    // The press that follows finds the window there, so a person who presses again while the
    // application's code is still arriving gets no second copy of it.
    desk.frame.click_at(notes.x, notes.y);
    desk.frame.relayout();
    assert_eq!(
        windows(&desk.handle),
        ["so.tilo.notes"],
        "a second press does not open a second window"
    );
}
