//! What typing into a note costs the layout tree when Notes runs in windows
//! of the desktop surface the browser desktop hosts apps in: a keystroke
//! rebuilds no layout tree at all, re-measures the editor it edits and its
//! ancestors, not every window's widgets, and does so in one frame at most;
//! a focused editor at rest measures nothing.

use crate::component_support::{self, NotebookFixture};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::widget::TileHost;

type Arrived = Rc<RefCell<HashMap<TabId, AppComponent>>>;

struct Components(Arrived);
impl TileSource for Components {
    fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
        match self.0.borrow_mut().remove(&tab.id) {
            Some(component) => TileHost::from_component(component),
            None => TileHost::new(ui::widget::TileContent::Notice {
                title: tab.title.clone(),
                detail: "Loading".into(),
            }),
        }
    }
}

/// `windows` Notes windows on one desktop, each on the fixture's notebook.
fn notes_windows(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    windows: usize,
) -> FrameHarness {
    notes_windows_with(f, checkout, windows, |_| {})
}

fn notes_windows_with(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
    windows: usize,
    sink: impl Fn(ui::WakeEvent) + Clone + Send + Sync + 'static,
) -> FrameHarness {
    let mut minter = IdMinter::new(7);
    let tabs: Vec<Tab> = (0..windows)
        .map(|_| Tab {
            id: minter.mint(),
            title: "Notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
            app_title: None,
        })
        .collect();
    let ids: Vec<TabId> = tabs.iter().map(|tab| tab.id).collect();
    let tree = seed_tree(&mut minter, "Desk", tabs);
    let arrived = Arrived::default();
    let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Components(Rc::clone(&arrived))));
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
    for (instance, &tab) in (1..).zip(&ids) {
        let mut component =
            component_support::component(f, instance, component_support::existing(checkout));
        component
            .mount_with_owner_queue(&wakes, sink.clone())
            .unwrap();
        arrived.borrow_mut().insert(tab, component);
        handle.rebuild(tab);
    }
    for _ in 0..4 {
        harness.poll_frame().unwrap();
        harness.relayout();
    }
    assert!(arrived.borrow().is_empty(), "every tab took its component");
    harness.set_coalesce_input_frames(true);
    harness
}

/// The frames a host draws after an input, until it is at rest or `limit`;
/// how many of them solved the layout.
fn settle(harness: &mut FrameHarness, limit: usize) -> usize {
    let mut measured = 0_usize;
    for _ in 0..limit {
        if !harness.poll_frame().unwrap() {
            break;
        }
        measured = measured.saturating_add(usize::from(harness.last_relayout().measured));
    }
    measured
}

#[test]
fn a_keystroke_in_a_note_rebuilds_no_layout_tree() {
    keystrokes_rebuild_nothing(None);
}

#[test]
fn a_keystroke_in_an_empty_note_rebuilds_no_layout_tree() {
    keystrokes_rebuild_nothing(Some(|f: &NotebookFixture| f.empty));
}

/// Type into the open note, or first open the one `open` names.
fn keystrokes_rebuild_nothing(open: Option<fn(&NotebookFixture) -> notes::PageId>) {
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        ui::i18n::I18nManager::init("en-US");
        let f = NotebookFixture::rich();
        let checkout = component_support::notebook(&f);
        // A window opening a note is wide enough to show its notebook.
        let mut harness = notes_windows(&f, &checkout, if open.is_some() { 1 } else { 2 });
        if let Some(open) = open {
            let row = component_support::page_rect(&harness, open(&f));
            harness.click_at(row.x0 + 12.0, f64::midpoint(row.y0, row.y1));
            settle(&mut harness, 6);
        }
        let editor = harness
            .targets()
            .into_iter()
            .find(|(key, _)| key == "block_editor")
            .map(|(_, rect)| rect)
            .expect("a note's editor");
        harness.click_at(editor.x0 + 20.0, editor.y0 + 10.0);
        // The first key may settle what the click left (focus, the caret).
        harness.key("x");
        settle(&mut harness, 4);
        let rebuilt = harness.rebuilt_frames();
        for key in "typing".chars() {
            // The key's own frame, if it drew one, then those it asks for.
            let frames = harness.relayout_frames();
            harness.key(&key.to_string());
            let mut measured = usize::from(
                harness.relayout_frames() != frames && harness.last_relayout().measured,
            );
            let stats = harness.last_relayout();
            measured = measured.saturating_add(settle(&mut harness, 10));
            assert!(
                measured <= 1,
                "a keystroke solved the layout in {measured} frames, asked by {:?}",
                harness.layout_measure_requesters()
            );
            assert!(
                !stats.rebuilt,
                "a keystroke rebuilt the layout tree: {stats:?}"
            );
            // The editor's own leaf and its ancestors to the root: one
            // window's path, not every window's widgets.
            assert!(
                stats.marked_leaves <= 1 && stats.marked_dirty <= 16,
                "a keystroke re-measured more than the editor it edits: {stats:?}, asked by {:?}",
                harness.layout_measure_requesters()
            );
        }
        assert_eq!(
            harness.rebuilt_frames(),
            rebuilt,
            "no frame of six keystrokes rebuilt the layout tree"
        );
        // At rest with the caret in the editor, the frames that still come
        // (a caret blink, a timer) repaint at most; none of them measures.
        let start = ui::time::now();
        for step in 1..=8u64 {
            let at = start
                .checked_add(ui::time::Duration::from_millis(step.saturating_mul(400)))
                .unwrap();
            ui::time::testing::with_monotonic_time(at, || {
                let measured = settle(&mut harness, 4);
                assert_eq!(
                    measured,
                    0,
                    "a focused editor at rest solved the layout, asked by {:?}",
                    harness.layout_measure_requesters()
                );
            });
        }
    });
}

/// Typing into a note's heading changes the note and the outline beside
/// it, and nothing else: the frames a keystroke draws repaint those, not
/// the windows they are in. The outline draws into the same damage part as
/// the page tree; while its damage went unsaid, the part could not say where
/// it changed and every key repainted each window whole.
#[test]
fn a_keystroke_in_a_heading_repaints_where_it_changed_not_the_windows() {
    use ui::retained_raster::{Repaint, RetainedFrames, TargetKey};
    const SCREEN: f64 = 1800.0 * 900.0;
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        ui::i18n::I18nManager::init("en-US");
        let f = NotebookFixture::rich();
        let checkout = component_support::notebook(&f);
        let mut harness = notes_windows(&f, &checkout, 2);
        harness.track_scene_parts(true);
        let editor = harness
            .targets()
            .into_iter()
            .find(|(key, _)| key == "block_editor")
            .map(|(_, rect)| rect)
            .expect("a note's editor");
        // The note's first block is a heading, which the outline lists.
        harness.click_at(editor.x0 + 20.0, editor.y0 + 10.0);
        harness.key("x");
        settle(&mut harness, 4);
        let target = TargetKey {
            width: 1800,
            height: 900,
            to_device: ui::kurbo::Affine::IDENTITY,
            base_color: ui::vello::peniko::Color::BLACK,
            generation: 0,
        };
        let mut shown = RetainedFrames::default();
        shown.presented(harness.scene(), harness.scene_parts(), target);
        for key in "typing".chars() {
            harness.key(&key.to_string());
            let mut repainted = 0.0;
            for _ in 0..6 {
                // As the browser does when a component's model wakes.
                if harness.has_pending_owner_models() {
                    harness.service_pending_owner_models().unwrap();
                }
                if !harness.poll_frame().unwrap() {
                    break;
                }
                match shown.repaint(harness.scene(), harness.scene_parts(), target) {
                    Repaint::Full(why) => panic!("typing {key:?} repainted the screen: {why:?}"),
                    Repaint::Regions(regions) => {
                        repainted += regions.iter().map(ui::kurbo::Rect::area).sum::<f64>();
                    }
                }
                shown.presented(harness.scene(), harness.scene_parts(), target);
            }
            assert!(
                repainted > 0.0 && repainted < SCREEN * 0.15,
                "typing {key:?} repainted {:.0}% of the screen",
                100.0 * repainted / SCREEN
            );
        }
    });
}

/// A keystroke in a note draws one frame, and its owner is serviced once:
/// the key's frame shows the edit, and the store's write of it landing
/// tells the owner its save status alone. That save landed in time, which
/// the window shows nothing of, so neither its wake nor its service asks
/// the host for a second frame. The host here behaves as the browser's:
/// a frame whenever queued work or a wake asks for one, the owner's
/// service whenever a wake is queued for it.
#[test]
fn a_keystroke_draws_one_frame_and_services_its_owner_once() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    ui::time::testing::with_monotonic_time(ui::time::now(), || {
        ui::i18n::I18nManager::init("en-US");
        let f = NotebookFixture::rich();
        let checkout = component_support::notebook(&f);
        // The wakes that ask the host for a frame, as the browser's sink
        // takes them.
        let frame_wakes = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&frame_wakes);
        let mut harness = notes_windows_with(&f, &checkout, 1, move |event| {
            if event.asks_for_frame() {
                count.fetch_add(1, Ordering::Relaxed);
            }
        });
        let editor = harness
            .targets()
            .into_iter()
            .find(|(key, _)| key == "block_editor")
            .map(|(_, rect)| rect)
            .expect("a note's editor");
        harness.click_at(editor.x0 + 20.0, editor.y0 + 10.0);
        // The first key may settle what the click left (focus, the caret).
        harness.key("x");
        for _ in 0..6 {
            let _ = f.records.try_flush();
            if harness.has_pending_owner_models() {
                harness.service_pending_owner_models().unwrap();
            }
            if !harness.poll_frame().unwrap() {
                break;
            }
        }
        for key in "typing".chars() {
            frame_wakes.store(0, Ordering::Relaxed);
            harness.key(&key.to_string());
            let (mut frames, mut services) = (0_usize, 0_usize);
            for _ in 0..8 {
                let due = harness.frame_schedule().queued_work
                    || frame_wakes.swap(0, Ordering::Relaxed) > 0;
                if due {
                    let sources = harness.queued_work_sources();
                    assert!(harness.poll_frame().unwrap(), "a due frame draws");
                    frames += 1;
                    assert!(
                        frames <= 1,
                        "typing {key:?} drew a second frame, for {sources:?}"
                    );
                }
                if harness.has_pending_owner_models() {
                    let report = harness.service_pending_owner_models_report();
                    assert!(
                        !report.attempted && !report.presented,
                        "a landed save ran the owner's model or showed something: {report:?}"
                    );
                    report.result.unwrap();
                    services += 1;
                } else if !due {
                    break;
                }
                // The store lands the key's write, as the browser's host
                // acknowledges it after the key's frame: its wake waits for
                // the host's next turn, where a frame may come first.
                let _ = f.records.try_flush();
            }
            assert_eq!(frames, 1, "typing {key:?} drew {frames} frames");
            assert_eq!(
                services, 1,
                "typing {key:?} serviced its owner {services} times"
            );
        }
    });
}
