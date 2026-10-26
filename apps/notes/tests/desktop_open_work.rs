//! What opening an app costs the desktop's layout tree when other windows
//! are open, on the desktop surface the browser desktop hosts apps in: a
//! window opens holding the app's place, then the app arrives in it. The
//! app is laid out under its window's retained node, not by rebuilding the
//! desktop's tree.

use crate::component_support::{self, NotebookFixture};
use std::{cell::RefCell, collections::HashMap, collections::HashSet, rc::Rc};
use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::navigation::Command;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::time::{Duration, Instant};
use ui::widget::TileHost;

const NOTES: &str = "so.tilo.notes";

/// The time between two frames the test draws.
const FRAME: Duration = Duration::from_millis(16);

type Arrived = Rc<RefCell<HashMap<TabId, AppComponent>>>;

struct Components(Arrived);
impl TileSource for Components {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        match self.0.borrow_mut().remove(&tab.id) {
            Some(component) => TileHost::from_component(component),
            // The app's code or record is still coming: the window holds
            // its place.
            None => TileHost::new(ui::widget::TileContent::Arriving),
        }
    }

    fn launch(&mut self, _: &Workspace, id: TabId, app: &str) -> Option<Tab> {
        (app == NOTES).then(|| notes_tab(id))
    }
}

fn notes_tab(id: TabId) -> Tab {
    Tab {
        id,
        title: "Notes".into(),
        tile: TileContent::LocalApp {
            app_id: NOTES.into(),
            label: "Notes".into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// What a step cost the layout: the trees it rebuilt, and the layout nodes
/// it built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Step {
    rebuilt: u64,
    nodes_built: u64,
}

impl Step {
    fn since(self, before: Self) -> Self {
        Self {
            rebuilt: self.rebuilt - before.rebuilt,
            nodes_built: self.nodes_built - before.nodes_built,
        }
    }
}

struct Desktop {
    harness: FrameHarness,
    handle: DesktopHandle,
    arrived: Arrived,
    wakes: ui::owner_wake::OwnerWakeQueue,
    instance: u64,
    /// The time the next frame is drawn at.
    now: Instant,
}

impl Desktop {
    /// `windows` Notes windows, every app arrived and drawn.
    fn with_windows(
        f: &NotebookFixture,
        checkout: &ui::record::RecordCheckout,
        windows: usize,
    ) -> Self {
        let mut minter = IdMinter::new(7);
        let tabs: Vec<Tab> = (0..windows).map(|_| notes_tab(minter.mint())).collect();
        let ids: Vec<TabId> = tabs.iter().map(|tab| tab.id).collect();
        let tree = seed_tree(&mut minter, "Desk", tabs);
        let arrived = Arrived::default();
        let (surface, handle) =
            DesktopSurface::new(tree, 7, Box::new(Components(Rc::clone(&arrived))));
        handle.set_viewport_width(1800.0);
        let now = ui::time::now();
        let mut harness = ui::time::testing::with_monotonic_time(now, || {
            FrameHarness::new_with_controlled_inputs(
                ui::State::new(),
                Box::new(surface),
                (1800, 900),
                ui::Theme::dark(),
                component_support::renderer(),
                f.author.clone(),
            )
        });
        let wakes = ui::owner_wake::OwnerWakeQueue::default();
        harness.attach_owner_wake_queue(wakes.clone()).unwrap();
        let mut desktop = Self {
            harness,
            handle,
            arrived,
            wakes,
            instance: 0,
            now,
        };
        desktop.settle(240);
        for tab in ids {
            desktop.arrive(f, checkout, tab);
        }
        desktop.settle(240);
        assert!(
            desktop.arrived.borrow().is_empty(),
            "every tab took its component"
        );
        desktop
    }

    /// The app's component for `tab` is ready: the window shows it.
    fn arrive(&mut self, f: &NotebookFixture, checkout: &ui::record::RecordCheckout, tab: TabId) {
        self.instance += 1;
        let mut component =
            component_support::component(f, self.instance, component_support::existing(checkout));
        component
            .mount_with_owner_queue(&self.wakes, |_| {})
            .unwrap();
        self.arrived.borrow_mut().insert(tab, component);
        self.handle.rebuild(tab);
    }

    /// The frames the host draws a frame apart until it is at rest, or
    /// `limit`; how many it drew.
    fn settle(&mut self, limit: usize) -> usize {
        for frame in 0..limit {
            self.now += FRAME;
            let harness = &mut self.harness;
            let drew =
                ui::time::testing::with_monotonic_time(self.now, || harness.poll_frame().unwrap());
            if !drew {
                return frame;
            }
        }
        limit
    }

    /// What the layout cost so far.
    fn spent(&self) -> Step {
        Step {
            rebuilt: self.harness.rebuilt_frames(),
            nodes_built: self.harness.layout_nodes_built(),
        }
    }

    fn shown(&self) -> HashSet<TabId> {
        self.handle
            .shown_tabs()
            .into_iter()
            .map(|(tab, _)| tab)
            .collect()
    }

    fn launch(&self) {
        let command = Command::Launch {
            generation: "open".into(),
            app: NOTES.into(),
        };
        self.handle
            .dispatch(&command, "open")
            .expect("the desktop launches Notes");
    }
}

/// What one more Notes window cost the layout once it had opened holding
/// the app's place: the app arriving in it.
fn open_with(others: usize) -> Step {
    ui::i18n::I18nManager::init("en-US");
    let f = NotebookFixture::rich();
    let checkout = component_support::notebook(&f);
    let mut desktop = Desktop::with_windows(&f, &checkout, others);
    let before = desktop.shown();
    desktop.launch();
    desktop.settle(240);
    let tab = desktop
        .shown()
        .into_iter()
        .find(|tab| !before.contains(tab))
        .expect("a new window opened");
    let start = desktop.spent();
    let wall = std::time::Instant::now();
    desktop.arrive(&f, &checkout, tab);
    // The frames that take the app in and lay it out.
    desktop.settle(4);
    eprintln!(
        "perf open beside {others}: the app arriving took {}us over its first frames",
        wall.elapsed().as_micros()
    );
    desktop.settle(240);
    let arrived = desktop.spent().since(start);
    assert!(
        desktop.arrived.borrow().is_empty(),
        "the new window took its app"
    );
    arrived
}

#[test]
fn an_app_arriving_rebuilds_only_its_window() {
    for others in [1, 6] {
        let cost = open_with(others);
        // The app itself is laid out under its window's node, and what it
        // replaces of its own widgets after arriving too. The one rebuild
        // left is the window's own new width when its app arrives, which
        // the row still lays out by building its tree again.
        assert!(
            cost.rebuilt <= 1,
            "an app arriving in its window beside {others} others rebuilt the desktop's layout tree more than for the window's width: {cost:?}"
        );
    }
}
