//! A window of Notes with nothing in it yet, hosted in a desktop tab as the
//! browser desk hosts it: the window must offer New note by name, the way it
//! does once a note exists, so a person (or an agent reading the desk's
//! semantics) can make the first one.

use crate::component_support::{self, NotebookFixture};
use std::cell::RefCell;
use std::rc::Rc;
use tilo_desktop_model::{IdMinter, Tab, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::widget::TileHost;

/// Answers with a notice until its component arrives, as the browser desk
/// does while a tab's record is still loading.
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

/// A desk showing one window of Notes over `f`'s notebook, the record having
/// arrived after the desk was already running.
fn desk(f: &NotebookFixture, width: u32) -> (FrameHarness, ui::record::RecordCheckout) {
    let checkout = component_support::notebook(f);
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
        view: String::new(),
        pinned: false,
    };
    let tab_id = tab.id;
    let tree = seed_tree(&mut minter, "Desk", vec![tab]);
    let arriving = Rc::new(RefCell::new(None));
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Arriving(arriving.clone())));
    handle.set_viewport_width(f64::from(width));
    let mut harness = FrameHarness::new_with_controlled_inputs(
        ui::State::new(),
        Box::new(surface),
        (width, 900),
        ui::Theme::dark(),
        component_support::renderer(),
        f.author.clone(),
    );
    let wakes = ui::owner_wake::OwnerWakeQueue::default();
    harness.attach_owner_wake_queue(wakes.clone()).unwrap();
    harness.relayout();

    let mut component = component_support::component(f, 7, component_support::existing(&checkout));
    component.mount_with_owner_queue(&wakes, |_| {}).unwrap();
    *arriving.borrow_mut() = Some(component);
    handle.rebuild(tab_id);
    for _ in 0..3 {
        harness.poll_frame().unwrap();
        harness.relayout();
    }
    (harness, checkout)
}

fn page_count(f: &NotebookFixture, checkout: &ui::record::RecordCheckout) -> usize {
    f.records
        .read(&checkout.id, |doc| notes::read::list_pages(doc).len())
        .unwrap()
}

/// The accessibility nodes named `name`: their id and where they stand.
fn nodes_named(
    harness: &FrameHarness,
    name: &str,
) -> Vec<(ui::accesskit::NodeId, ui::accesskit::Node)> {
    harness
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.label() == Some(name))
        .collect()
}

/// The band the window's own controls stand in: the bar, whatever holds it.
fn bar_band(harness: &FrameHarness) -> (f64, f64) {
    let close = nodes_named(harness, "Close")
        .into_iter()
        .find_map(|(_, node)| node.bounds())
        .expect("the window's own close control is drawn in its bar");
    (close.y0, close.y1)
}

/// A window of Notes with no note in it still offers New note, in the bar it
/// draws: the app declares the slot the moment its window is up, and the
/// window stops handing its bar to the app's own row for it, so the control
/// stands in the same row as the window's own controls, named for a screen
/// reader and an agent, and pressing it makes the note. Both widths a person
/// meets it at: a window of a desk, and a phone's.
#[test]
fn an_empty_notes_window_offers_new_note_in_its_bar_and_it_makes_a_note() {
    ui::i18n::I18nManager::init("en-US");
    for width in [1280, 390] {
        let f = NotebookFixture::empty();
        let (mut harness, checkout) = desk(&f, width);
        assert_eq!(
            page_count(&f, &checkout),
            0,
            "{width}px: the notebook is empty"
        );

        let (bar_y0, bar_y1) = bar_band(&harness);
        let offered = nodes_named(&harness, &ui::i18n!("notes-affordance-new-note"));
        assert!(
            !offered.is_empty(),
            "{width}px: an empty window of Notes offers New note"
        );
        let new_note = offered
            .into_iter()
            .find(|(_, node)| {
                node.bounds()
                    .is_some_and(|bounds| bounds.y0 >= bar_y0 - 1.0 && bounds.y1 <= bar_y1 + 1.0)
            })
            .map(|(id, _)| id);
        let Some(new_note) = new_note else {
            panic!("{width}px: New note stands in the window's bar, in the row its own controls do")
        };

        harness
            .perform_a11y_action(new_note, &ui::a11y::Action::Activate)
            .unwrap();
        for _ in 0..4 {
            harness.poll_frame().unwrap();
            harness.relayout();
        }
        assert_eq!(
            page_count(&f, &checkout),
            1,
            "{width}px: the bar's New note makes the notebook's first note"
        );
    }
}
