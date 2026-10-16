//! Six Notes windows in the desktop surface the browser desktop hosts apps
//! in, panned by coalesced trackpad wheels. A pan moves laid-out windows and
//! nothing else, so a wheel's own host tick is left to the frame, and that
//! frame, which nothing new reached, neither ticks, syncs nor publishes, nor
//! asks the windows' History again what the frame before it answered, nor
//! draws a window at rest again.

use crate::component_support::{self, NotebookFixture};
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tilo_desktop_model::{IdMinter, Tab, TabId, TileContent, Workspace};
use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
use ui::application::AppComponent;
use ui::headless::FrameHarness;
use ui::tilo_resource::profiling::{Sample, observe};
use ui::widget::TileHost;

type Arrived = Rc<RefCell<HashMap<TabId, AppComponent>>>;

/// Hands each tab its component once it arrives, as the browser desktop
/// does once a tab's record is loaded.
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

const WINDOWS: usize = 6;
/// Record admissions one window may make in a pan frame: its sync and its
/// undo status, with room to spare.
const RECORDS_PER_WINDOW: usize = 4;

fn six_notes(
    f: &NotebookFixture,
    checkout: &ui::record::RecordCheckout,
) -> (FrameHarness, DesktopHandle) {
    let mut minter = IdMinter::new(7);
    let tabs: Vec<Tab> = (0..WINDOWS)
        .map(|_| Tab {
            id: minter.mint(),
            title: "Notes".into(),
            tile: TileContent::LocalApp {
                app_id: "so.tilo.notes".into(),
                label: "Notes".into(),
                note: String::new(),
            },
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
        component.mount_with_owner_queue(&wakes, |_| {}).unwrap();
        arrived.borrow_mut().insert(tab, component);
        handle.rebuild(tab);
    }
    for _ in 0..4 {
        harness.poll_frame().unwrap();
        harness.relayout();
    }
    assert!(arrived.borrow().is_empty(), "every tab took its component");
    harness.set_coalesce_wheel_frames(true);
    (harness, handle)
}

fn wheel(harness: &mut FrameHarness, dx: f64) {
    harness.send_host_event(&ui::winit::event::WindowEvent::MouseWheel {
        device_id: ui::winit::event::DeviceId::dummy(),
        delta: ui::winit::event::MouseScrollDelta::PixelDelta(
            ui::winit::dpi::PhysicalPosition::new(dx, 0.0),
        ),
        phase: ui::winit::event::TouchPhase::Moved,
    });
}

fn pan_offset(handle: &DesktopHandle) -> f64 {
    handle.snapshot("g", vec![]).desktop_pan.unwrap().offset
}

fn calls(rows: &[Sample], phase: &str) -> usize {
    rows.iter()
        .find(|row| row.phase == phase)
        .map_or(0, |row| row.calls)
}

#[test]
fn a_pan_frame_over_six_notes_windows_leaves_undo_bookkeeping_to_actions() {
    ui::i18n::I18nManager::init("en-US");
    let f = NotebookFixture::rich();
    let checkout = component_support::notebook(&f);
    let (mut harness, handle) = six_notes(&f, &checkout);
    let max = handle.snapshot("g", vec![]).desktop_pan.unwrap().max;
    assert!(max > 0.0, "six windows overflow the strip");
    // One pan across the strip and back first, as the browser's
    // qualification does: each window is drawn once, which settles what a
    // first frame on screen does once.
    for (dx, end) in [(-120.0, max), (120.0, 0.0)] {
        for _ in 0..200 {
            if (pan_offset(&handle) - end).abs() < 0.5 {
                break;
            }
            wheel(&mut harness, dx);
            harness.poll_frame().unwrap();
        }
        assert!(
            (pan_offset(&handle) - end).abs() < 0.5,
            "the first pan reaches {end}"
        );
    }

    let frames: usize = 60;
    let start = pan_offset(&handle);
    let quiet = harness.quiet_frames();
    let settled = harness.settled_history_frames();
    let replayed = harness.replayed_window_draws();
    let ((), rows) = observe(|| {
        for index in 0..frames {
            let dx = if (index / 10) % 2 == 0 { -6.0 } else { 6.0 };
            wheel(&mut harness, dx);
            wheel(&mut harness, dx);
            assert!(harness.poll_frame().unwrap(), "each wheel pair is a frame");
            let stats = harness.last_relayout();
            assert!(
                !stats.rebuilt && !stats.measured,
                "a pan only re-applies positions: {stats:?}, asked by {:?}",
                harness.layout_measure_requesters()
            );
        }
    });
    assert!(
        (pan_offset(&handle) - start).abs() < 0.01,
        "every wheel panned the strip"
    );
    // A pan changes no document, focus or declaration, and a view-only
    // wheel calls nothing that could: every pan frame is quiet. It runs no
    // host tick (no interaction since the last frame left one pending), no
    // sync of any window, no undo publication and so no resource discovery.
    assert_eq!(
        harness.quiet_frames() - quiet,
        u64::try_from(frames).unwrap(),
        "every pan frame was quiet: {rows:?}"
    );
    // Nor does it ask each window's History again whether the app still
    // has the document owners it registered, or whether a committed
    // frontier moved: the frame before asked, and its witness proves that
    // nothing either reads has moved since. Each such question walks a
    // whole app.
    assert_eq!(
        harness.settled_history_frames() - settled,
        u64::try_from(frames).unwrap(),
        "every pan frame took its History answer from the frame before: {rows:?}"
    );
    for phase in [
        "ui.host_tick",
        "ui.frame_sync",
        "ui.publish_undo_status",
        "ui.bound_discovery_miss",
        "ui.history_root_validation",
        "ui.history_heads",
        "ui.settle_replayed_draws",
    ] {
        assert_eq!(
            calls(&rows, phase),
            0,
            "{phase} ran on a pan frame: {rows:?}"
        );
    }
    // Nor is any window's record opened: a sweep that opened the record of
    // every bound widget to look for a bracket would open it once per
    // widget, and each window has far more widgets than that.
    let records = calls(&rows, "ui.record_frame");
    let bound = bound_widgets(&harness.inspection().root, false);
    assert!(
        bound > RECORDS_PER_WINDOW * WINDOWS,
        "the bound below is meaningful: {bound} bound widgets"
    );
    assert!(
        records < frames.saturating_mul(RECORDS_PER_WINDOW * WINDOWS),
        "{records} record admissions over {frames} pan frames: the bracket sweep \
         opened records it found nothing in"
    );
    assert_pan_frames_replay(&mut harness, replayed, frames);

    // Any change still reaches its own frame: a click into a window syncs
    // the windows and publishes undo on the frame that shows it.
    let ((), rows) = observe(|| harness.click_at(300.0, 450.0));
    assert_eq!(
        calls(&rows, "ui.settle_replayed_draws"),
        1,
        "the replayed windows are drawn again before the click: {rows:?}"
    );
    assert!(
        calls(&rows, "ui.frame_sync") > 0 && calls(&rows, "ui.publish_undo_status") > 0,
        "a click syncs and publishes on its frame: {rows:?}"
    );
    assert!(
        calls(&rows, "ui.history_root_validation") >= WINDOWS,
        "a click's frame asks every window's History again: {rows:?}"
    );
}

/// A window at rest is not drawn again on a pan frame: the frame shows its
/// last drawing moved by the pan. Every one of `frames` pan frames since
/// `replayed` replayed at least one window, and the last shows what a frame
/// drawing every window shows. Panned once more, the next frame replays
/// again (the first frame after a full one may draw).
fn assert_pan_frames_replay(harness: &mut FrameHarness, replayed: u64, frames: usize) {
    let windows = harness.replayed_window_draws().saturating_sub(replayed);
    assert!(
        windows >= u64::try_from(frames).unwrap(),
        "{windows} windows replayed over {frames} pan frames"
    );
    let shown = harness.render_rgba();
    let before = harness.replayed_window_draws();
    harness.settle_replayed_draws();
    assert_eq!(
        harness.replayed_window_draws(),
        before,
        "settling draws every window"
    );
    let drawn = harness.render_rgba();
    assert_eq!(
        ui::offscreen::OffscreenRenderer::pixels_differing_beyond_aa(&shown, &drawn),
        0,
        "a pan frame that replayed windows shows what drawing them shows; they differ within {:?}",
        differing_bounds(&shown, &drawn, 1800)
    );
    for _ in 0..2 {
        wheel(harness, -6.0);
        assert!(harness.poll_frame().unwrap());
    }
    assert!(harness.replayed_window_draws() > before);
}

/// The pixel rectangle `(x0, y0, x1, y1)` holding every pixel of two
/// `width`-wide RGBA frames that differs by more than antialiasing, to say
/// where a failed comparison looked different.
fn differing_bounds(a: &[u8], b: &[u8], width: usize) -> Option<(usize, usize, usize, usize)> {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .enumerate()
        .filter(|(_, (x, y))| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 1))
        .map(|(index, _)| {
            (
                index.checked_rem(width).unwrap_or(0),
                index.checked_div(width).unwrap_or(0),
            )
        })
        .fold(None, |bounds, (x, y)| {
            Some(bounds.map_or(
                (x, y, x, y),
                |(x0, y0, x1, y1): (usize, usize, usize, usize)| {
                    (x0.min(x), y0.min(y), x1.max(x), y1.max(y))
                },
            ))
        })
}

/// How many widgets lie inside a hosted component.
fn bound_widgets(node: &ui::headless::InspectionNode, inside: bool) -> usize {
    let inside = inside || node.widget == "BoundComponent";
    node.children
        .iter()
        .map(|child| bound_widgets(child, inside))
        .fold(usize::from(inside), usize::saturating_add)
}
