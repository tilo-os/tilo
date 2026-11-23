//! Content-free CPU observations of mounted production Settings interactions.
//! Scope durations nest and are not additive. No latency budget is asserted.

use crate::{SettingsShell, categories::Category};
use ui::framework::testing::TestApp;

use ui::synthetic_input::cursor_moved;

fn sidebar_toggle(app: &mut TestApp<SettingsShell>) -> ui::vello::kurbo::Rect {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            (node.role() == ui::accesskit::Role::Button && node.label() == Some("Hide sidebar"))
                .then(|| node.bounds())
                .flatten()
        })
        .map(|rect| ui::vello::kurbo::Rect::new(rect.x0, rect.y0, rect.x1, rect.y1))
        .expect("the mounted shared sidebar toggle is accessible")
}

/// Frames [`settle`] allows before it calls the window hung. The slide runs
/// on the toolkit clock, so a loaded machine draws it in fewer frames, never
/// more; what this bounds is a window that keeps asking for frames, which is
/// what a hang looks like. Counting frames keeps that independent of how
/// fast the machine paints them.
const SETTLE_FRAMES: usize = 600;

/// Run frames until the window's layout stops moving: a sidebar opening
/// slides its rows in, so a row's bounds are only where a click lands once
/// the slide is done.
fn settle(app: &mut TestApp<SettingsShell>) {
    for _ in 0..SETTLE_FRAMES {
        if !app.harness().frame_schedule().needs_animation_frame {
            return;
        }
        app.harness().poll_frame().unwrap();
    }
    assert!(
        !app.harness().frame_schedule().needs_animation_frame,
        "the window kept animating for {SETTLE_FRAMES} frames"
    );
}

fn dirty_leaves(widget: &dyn ui::Widget, out: &mut Vec<serde_json::Value>) {
    let children = widget.children();
    if children.is_empty() && widget.layout_needs_measure() {
        out.push(serde_json::json!({
            "widget": widget.concrete_type_name(), "test_id": widget.test_id(),
        }));
    }
    for child in children {
        dirty_leaves(child, out);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Publication {
    /// It changes what the resource holds or offers: the framework publishes
    /// the resource again, so that work belongs to the measurement.
    Changed,
    /// It changes only how the frame is painted: the published tree is
    /// reused, and the measurement covers the painting.
    Unchanged,
}

fn measure(
    app: &mut TestApp<SettingsShell>,
    category: Category,
    operation: &str,
    sample: usize,
    publication: Publication,
    act: impl FnOnce(&mut TestApp<SettingsShell>),
) {
    app.harness().clear_profile_samples();
    let ((elapsed, drain_elapsed, frames), phases) = ui::tilo_resource::profiling::observe(|| {
        let started = std::time::Instant::now();
        act(app);
        let elapsed = started.elapsed().as_nanos();
        let drain = std::time::Instant::now();
        let frames = app.harness().take_profile_samples();
        (elapsed, drain.elapsed().as_nanos(), frames)
    });
    assert!(
        !frames.is_empty(),
        "the measured interaction paints real frames"
    );
    assert!(frames.iter().any(|frame| frame.draw_us > 0));
    assert_eq!(
        app.harness().native_input_snapshot().unwrap()["selected_category"],
        category.slug(),
        "the mounted canonical pane is the measured subject"
    );
    if publication == Publication::Changed {
        assert!(
            phases
                .iter()
                .any(|scope| scope.phase == "framework.publication"),
            "the interaction changes the resource, so the framework publishes it"
        );
    }
    {
        let layout = app.harness().last_relayout();
        if category == Category::Keyboard && operation == "pointer-hover" && sample >= 2 {
            assert_eq!(
                layout.marked_leaves, 0,
                "the collapsed keyboard inspection is an empty container, not a changing intrinsic-size leaf"
            );
        }
        let mut leaves = Vec::new();
        dirty_leaves(app.harness().root_widget(), &mut leaves);
        eprintln!(
            "settings-cpu-interaction {}",
            serde_json::json!({
                "architecture": std::env::consts::ARCH,
                "profile": if cfg!(debug_assertions) { "test-debug" } else { "optimized" },
                "pane": category.slug(), "operation": operation, "sample": sample,
                "warmup": sample < 2,
                "last_layout_rebuilt": layout.rebuilt,
                "last_layout_measured": layout.measured,
                "last_layout_marked_dirty": layout.marked_dirty,
                "last_layout_marked_leaves": layout.marked_leaves,
                "event_elapsed_ns": elapsed, "profile_drain_elapsed_ns": drain_elapsed,
                "phases": phases, "frames": frames, "dirty_leaves_after_draw": leaves,
                "includes_cpu_layout_and_scene_draw": true,
                "includes_gpu_or_display": false,
                "execution_host": "mounted headless framework; no GPU or device presentation",
            })
        );
    }
}

#[test]
fn mounted_settings_cpu_interactions_observe_canonical_panes_and_shared_controls() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts normally");
    app.harness().resize((1472, 920)).unwrap();
    app.call(
        "open",
        serde_json::json!({ "anchor": crate::effects::pane_row_id(Category::Displays) }),
    )
    .expect("initial ordinary pane selection");
    app.harness().begin_profiling();
    for sample in 0..9 {
        for category in [
            Category::Appearance,
            Category::Network,
            Category::Keyboard,
            Category::Displays,
        ] {
            let bounds = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    (node.role() == ui::accesskit::Role::TreeItem
                        && node.label() == Some(category.label().as_str()))
                    .then(|| node.bounds())
                    .flatten()
                })
                .expect("the actual visible navigation row is accessible");
            assert!(bounds.y0 >= 0.0 && bounds.y1 <= 920.0);
            measure(
                &mut app,
                category,
                "pointer-pane-navigation",
                sample,
                Publication::Changed,
                |app| {
                    app.harness().click_at(
                        f64::midpoint(bounds.x0, bounds.x1),
                        f64::midpoint(bounds.y0, bounds.y1),
                    );
                },
            );
            let toggle = sidebar_toggle(&mut app);
            // Hovering the shared toggle repaints it and leaves the resource
            // as it was, so the measured frames are the painting's.
            measure(
                &mut app,
                category,
                "pointer-hover",
                sample,
                Publication::Unchanged,
                |app| {
                    app.harness().send(&cursor_moved(1190.0, 890.0));
                    app.harness()
                        .send(&cursor_moved(toggle.center().x, toggle.center().y));
                },
            );
            measure(
                &mut app,
                category,
                "sidebar-control-click",
                sample,
                Publication::Changed,
                |app| {
                    app.harness().click_at(toggle.center().x, toggle.center().y);
                },
            );
            assert_eq!(
                app.harness().native_input_snapshot().unwrap()["sidebar_open"],
                false
            );
            app.harness().key("Escape");
            // Restore through the ordinary shared control; inspection does not
            // manufacture open state or bypass the production layout path.
            let bounds = app
                .harness()
                .a11y_nodes()
                .into_iter()
                .find_map(|(_, node)| {
                    (node.role() == ui::accesskit::Role::Button
                        && node.label() == Some("Show sidebar"))
                    .then(|| node.bounds())
                    .flatten()
                })
                .expect("the ordinary sidebar reopen control stays visible");
            app.harness().click_at(
                f64::midpoint(bounds.x0, bounds.x1),
                f64::midpoint(bounds.y0, bounds.y1),
            );
            assert_eq!(
                app.harness().native_input_snapshot().unwrap()["sidebar_open"],
                true
            );
            settle(&mut app);
        }
    }
}

/// An idle window paints nothing: on the pane it opens on, after its first
/// reads and arrivals, no widget asks for a frame and no deadline is due —
/// and the clock advancing asks for none either. A regression names the
/// widget that does, so the frame it paints can be traced to its cause.
#[test]
fn an_idle_settings_window_asks_for_no_frames() {
    ui::i18n::I18nManager::init("en-US");
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts normally");
    app.harness().resize((1280, 800)).unwrap();
    settle(&mut app);
    let requesters = app.harness().frame_requesters();
    assert!(
        requesters.is_empty(),
        "idle Settings keeps asking for frames: {requesters:?}"
    );
    let start = ui::time::now();
    for step in 1..=20u64 {
        let at = start + std::time::Duration::from_millis(step * 100);
        let requesters =
            ui::time::testing::with_monotonic_time(at, || app.harness().frame_requesters());
        assert!(
            requesters.is_empty(),
            "at +{}ms idle Settings keeps asking for frames: {requesters:?}",
            step * 100
        );
    }
}

/// A read on a pane the person is not looking at that never lands, with the
/// deadline it keeps asking to be tried again: the Network page's scan still
/// on its way after they moved on to Appearance.
struct StuckRead {
    polls: std::rc::Rc<std::cell::Cell<usize>>,
}

impl crate::area::PaneArea for StuckRead {
    fn serving(&self) -> &'static [Category] {
        &[Category::Network]
    }

    fn poll(&self) {
        self.polls.set(self.polls.get() + 1);
    }

    fn first_read(&self) -> bool {
        true
    }

    fn pending_visual_work(&self) -> bool {
        true
    }

    fn next_wake(&self) -> Option<ui::time::Instant> {
        Some(ui::time::now())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// A read that never lands on a pane nobody shows, with the deadline it
/// keeps asking for, leaves the pane the person reads resting: the window
/// paints no frame for it and takes in none of its work, beyond the rows it
/// publishes. The device path reads its areas one at a time, as a live
/// window does.
#[test]
fn a_read_on_a_pane_nobody_shows_asks_for_no_frames_or_polls() {
    ui::i18n::I18nManager::init("en-US");
    crate::effects::defer_area_reads_in_tests(true);
    let polls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    {
        let polls = std::rc::Rc::clone(&polls);
        crate::area::replace_in_next_window(move |_| StuckRead { polls });
    }
    let mut app = TestApp::<SettingsShell>::new().expect("Settings mounts normally");
    crate::effects::defer_area_reads_in_tests(false);
    app.harness().resize((1280, 800)).unwrap();
    crate::effects::show_pane(&mut app, Category::Appearance);
    app.settle();
    // The first reads of the device path land one per sync; the window is
    // measured once they are all in.
    let mut pumped = 0;
    while app
        .with_widget::<crate::window::Inspected, _>(|settings| settings.area_reads().deferred())
        .unwrap_or(false)
    {
        pumped += 1;
        assert!(pumped < 400, "the window never finished reading its areas");
        app.harness()
            .wait_until(ui::time::Duration::from_millis(10), |_| false);
    }
    settle(&mut app);

    assert!(
        app.harness().pending_visual_work().is_empty(),
        "the window waits for a read on a pane nobody shows: {:?}",
        app.harness().pending_visual_work()
    );
    assert!(
        app.harness().frame_requesters().is_empty(),
        "a read on another pane asks for frames: {:?}",
        app.harness().frame_requesters()
    );
    let start = ui::time::now();
    for step in 1..=20u64 {
        let at = start + std::time::Duration::from_millis(step * 100);
        ui::time::testing::with_monotonic_time(at, || {
            let drew = app
                .harness()
                .poll_frame()
                .expect("the window either draws a frame or rests");
            assert!(
                !drew,
                "an idle Settings painted a frame over two seconds for a read on another pane: \
                 {:?}",
                app.harness().frame_requesters()
            );
        });
    }
    assert_eq!(
        polls.get(),
        0,
        "the window took in the work of a pane nobody shows {} times",
        polls.get()
    );
}
