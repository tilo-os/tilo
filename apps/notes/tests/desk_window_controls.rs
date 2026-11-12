//! A window of Notes on the desk, alone in its window and among other tabs, at a desk's width, a
//! narrow window's and a phone's: the window's own maximize and close stand in its bar, and the
//! tab in front of a window of several offers its own close, each where a person and an agent
//! find it by name.

use crate::component_support::{self, NotebookFixture};
use std::cell::RefCell;
use std::rc::Rc;
use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::widget::TileHost;

/// Notes for the tab that shows it, a notice for any other.
struct Source(Rc<RefCell<Option<AppComponent>>>);
impl TileSource for Source {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        if let TileContent::LocalApp { app_id, .. } = &tab.tile
            && app_id == "so.tilo.notes"
            && let Some(component) = self.0.borrow_mut().take()
        {
            return TileHost::from_component(component);
        }
        TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

fn tab(minter: &mut IdMinter, app: &str, label: &str) -> Tab {
    Tab {
        id: minter.mint(),
        title: label.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: label.into(),
            note: String::new(),
        },
        app_title: None,
        view: String::new(),
        pinned: false,
    }
}

/// A desk `width` wide of one window whose tab in front is Notes over `f`'s notebook, with
/// `others` more tabs after it.
fn desk(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    width: u32,
    others: &[(&str, &str)],
) -> FrameHarness {
    let mut minter = IdMinter::new(7);
    let notes = tab(&mut minter, "so.tilo.notes", "Notes");
    let notes_id = notes.id;
    let mut tree = seed_tree(&mut minter, "Desk", vec![notes]);
    for &(app, label) in others {
        let more = tab(&mut minter, app, label);
        tree.workspaces[0].strips[0].window_groups[0]
            .tabs
            .push(more);
    }
    let source = Rc::new(RefCell::new(None));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Source(source.clone())));
    handle.set_viewport_width(f64::from(width));
    let mut harness = FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(surface),
        (width, 800),
        ui::Theme::dark(),
        component_support::renderer(),
        f.author.clone(),
    );
    let wakes = ui::owner_wake::OwnerWakeQueue::default();
    harness.attach_owner_wake_queue(wakes.clone()).unwrap();
    harness.relayout();
    let mut component = component_support::component(f, 7, component_support::existing(checkout));
    component.mount_with_owner_queue(&wakes, |_| {}).unwrap();
    *source.borrow_mut() = Some(component);
    handle.rebuild(notes_id);
    for _ in 0..8 {
        harness.poll_frame().unwrap();
        harness.relayout();
    }
    harness
}

/// Whether a control named `name` is drawn somewhere in the window's bar.
fn drawn_in_bar(harness: &FrameHarness, name: &str) -> bool {
    harness.a11y_nodes().into_iter().any(|(_, node)| {
        node.label() == Some(name)
            && node
                .bounds()
                .is_some_and(|b| b.x1 > b.x0 && b.y1 > b.y0 && b.y1 <= 64.0)
    })
}

/// Every window keeps its maximize and close, alone and among other tabs, at every width a
/// person meets it at; and the tab in front of a window of several keeps its own close, which a
/// narrow window used to squeeze away with the rest of its tabs.
#[test]
fn a_desk_window_keeps_its_close_and_maximize_and_its_tab_its_close() {
    ui::i18n::I18nManager::init("en-US");
    let others = [("so.tilo.sheets", "Sheets"), ("so.tilo.files", "Files")];
    let mut missing = Vec::new();
    for width in [1280, 700, 480, 390] {
        for others in [&[][..], &others[..]] {
            let f = NotebookFixture::rich();
            let checkout = component_support::notebook(&f);
            let harness = desk(&f, &checkout, width, others);
            let case = format!("{width} px, {} tabs", others.len() + 1);
            for name in ["Maximize", "Close"] {
                if !drawn_in_bar(&harness, name) {
                    missing.push(format!("{case}: the window's {name}"));
                }
            }
            // The tab in front names the note it shows.
            if !others.is_empty() && !drawn_in_bar(&harness, "Close Projects") {
                missing.push(format!("{case}: the close of the tab in front"));
            }
        }
    }
    assert!(missing.is_empty(), "missing from the bar: {missing:#?}");
}
