//! The motion a person sees in the Tasks package with no view asking for any: checking a task
//! off fills its box over frames, and a row a person adds or clears arrives and leaves through
//! the list's own motion, at the pace the theme gives. The views say what a row is; the
//! platform plays the change.
//!
//! The frames are read one at a time: each frame the harness draws is rasterised, and a band of
//! it compared with another frame, so a motion is a series of values rather than a single
//! before and after. `the_frames_a_person_sees` writes those frames as PNGs; it is ignored by
//! default because it is there to look at, not to gate.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::arithmetic_side_effects,
    reason = "pixels of a small frame: coordinates and counts are tiny, and the arithmetic is the \
              comparison itself"
)]

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;
use ui::headless::InspectionNode;
use ui::time::testing::with_monotonic_time;

const DESKTOP: (u32, u32) = (1280, 800);

/// How far apart the test draws its frames: the pace a display presents them.
const FRAME: ui::time::Duration = ui::time::Duration::from_millis(16);

thread_local! {
    /// Where the test's clock stands: what the last frame it drew was at, and where the input
    /// it sends stands. A press and the frames after it share one clock, as a person's do.
    static AT: std::cell::RefCell<Option<ui::time::Instant>> =
        const { std::cell::RefCell::new(None) };
}

/// Set where the test's clock stands.
fn set_at(at: ui::time::Instant) {
    AT.with(|clock| *clock.borrow_mut() = Some(at));
}

/// The instant the test's clock stands at.
fn at_now() -> ui::time::Instant {
    AT.with(|clock| {
        *clock
            .borrow_mut()
            .get_or_insert_with(ui::time::Instant::now)
    })
}

/// The scene the scenario stages in a window a person drives: the same fixtures, with the
/// theme's motion playing as it plays live, at rest.
fn staged(scenario: &str) -> TestApp<TlApp> {
    crate::support::installed("tasks");
    let mut app = TestApp::<TlApp>::drive_at(scenario, DESKTOP).expect("the scenario stages");
    set_at(ui::time::Instant::now());
    settle(&mut app);
    app
}

/// How far ahead a settle follows a wake the window set for itself: a timer it wants a frame at.
/// A wake further out leaves the window at rest now.
const SETTLE_HORIZON: ui::time::Duration = ui::time::Duration::from_secs(1);

/// Run the frames the window asks for until it asks for none, then the idle pass that finishes
/// what they started: the scene at rest, as a person sees it a moment after it opened.
fn settle(app: &mut TestApp<TlApp>) {
    for _ in 0..600 {
        let now = at_now();
        let schedule = with_monotonic_time(now, || app.harness().frame_schedule());
        let at = if schedule.needs_animation_frame {
            now + FRAME
        } else if schedule.queued_work {
            now
        } else if let Some(deadline) = schedule
            .next_wake_deadline
            .filter(|deadline| *deadline <= now + SETTLE_HORIZON)
        {
            deadline.max(now)
        } else {
            with_monotonic_time(now, || app.idle());
            return;
        };
        set_at(at);
        with_monotonic_time(at, || {
            app.frame();
        });
    }
    panic!(
        "the window settles: it is asked for a frame by {:?}",
        app.harness().frame_requesters()
    );
}

/// Run the frame due at `at`: whether the window wanted one.
fn draw(app: &mut TestApp<TlApp>, at: ui::time::Instant) -> bool {
    set_at(at);
    with_monotonic_time(at, || app.frame())
}

/// Run the frame due at `at` and draw it: whether the window wanted a frame, and what it drew.
fn frame_at(app: &mut TestApp<TlApp>, at: ui::time::Instant) -> (bool, Vec<u8>) {
    set_at(at);
    with_monotonic_time(at, || {
        let drew = app.frame();
        let pixels = app.harness().render_rgba();
        (drew, pixels)
    })
}

fn walk<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

/// The rows the list shows, by the name each says, and where each is. A tree draws rows of its
/// own for the branches it holds, which the list does not list.
fn rows(app: &mut TestApp<TlApp>) -> Vec<(String, [f64; 4])> {
    fn collect(node: &InspectionNode, in_tree: bool, out: &mut Vec<(String, [f64; 4])>) {
        let in_tree = in_tree || node.widget == "TreeView";
        if !in_tree
            && (node.widget == "SwipeRow" || node.widget == "ListRow")
            && let Some(name) = node.semantics.as_ref().and_then(|it| it.name.clone())
        {
            out.push((name, node.rect));
        }
        for child in &node.children {
            collect(child, in_tree, out);
        }
    }
    let snapshot = app.harness().inspection();
    let mut found = Vec::new();
    collect(&snapshot.root, false, &mut found);
    found
}

/// Chooses the sidebar's place `label`, as a person does: what is due before today waits in
/// Today, and the inbox holds only what no day and no project sorts.
fn open_place(app: &mut TestApp<TlApp>, label: &str) {
    fn place<'a>(node: &'a ui::a11y::AccessNode, label: &str) -> Option<&'a ui::a11y::AccessNode> {
        if node.role == ui::a11y::Role::TreeItem
            && node
                .name
                .as_deref()
                .is_some_and(|name| name.starts_with(label))
        {
            return Some(node);
        }
        node.children.iter().find_map(|child| place(child, label))
    }
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let bounds = all
        .iter()
        .filter_map(|node| node.semantics.as_ref())
        .find_map(|access| place(access, label))
        .and_then(|access| access.bounds)
        .unwrap_or_else(|| panic!("the sidebar offers {label}"));
    with_monotonic_time(at_now(), || {
        app.harness().click_at(
            f64::midpoint(bounds.x0, bounds.x1),
            f64::midpoint(bounds.y0, bounds.y1),
        );
    });
    settle(app);
}

/// The space the leading checkbox of the row titled `title` takes: the start of the row, as
/// tall as its box.
fn row_box(app: &mut TestApp<TlApp>, title: &str) -> [f64; 4] {
    let [x, y, _, h] = rows(app)
        .into_iter()
        .find(|(name, _)| name == title)
        .unwrap_or_else(|| panic!("the list shows {title}"))
        .1;
    let size = 22.0;
    [x + 4.0, y + (h - size) / 2.0, size, size]
}

/// The space the list of rows takes: from the first row's top to the last one's bottom, as
/// wide as the widest row, and one row taller, since a row a person adds arrives under the
/// last one.
fn list_band(app: &mut TestApp<TlApp>) -> [f64; 4] {
    let rows = rows(app);
    let first = rows.first().expect("the list shows rows").1;
    let mut bottom = first[1] + first[3];
    let mut right = first[0] + first[2];
    for (_, rect) in &rows {
        bottom = bottom.max(rect[1] + rect[3]);
        right = right.max(rect[0] + rect[2]);
    }
    [
        first[0],
        first[1],
        right - first[0],
        bottom - first[1] + first[3],
    ]
}

/// Whether the window still has motion to play.
fn moving(app: &mut TestApp<TlApp>) -> bool {
    // Read at the instant the last frame was drawn at: a motion is measured against the clock
    // the test's frames run on, not the machine's, which lags the frames the test drew.
    with_monotonic_time(at_now(), || {
        app.harness().frame_schedule().needs_animation_frame
    })
}

/// The rect of the text field the window shows under the semantics name `name`: the entry at a
/// list's foot, which the window also offers as a button of its own.
fn field(app: &mut TestApp<TlApp>, name: &str) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .find(|node| {
            node.widget == "TextInput"
                && node
                    .semantics
                    .as_ref()
                    .and_then(|semantics| semantics.name.as_deref())
                    == Some(name)
        })
        .unwrap_or_else(|| panic!("the window shows a field named {name}"))
        .rect
}

/// The window's Delete command, as a person reaches it.
enum Delete {
    /// A button of the header's own, when the bar has room for it.
    Button([f64; 4]),
    /// The row of the header's menu the bar keeps it in, while the menu is open.
    Row([f64; 4]),
}

/// The button the window shows under `name`, which a person presses, or nothing when the bar
/// has no room for it and keeps it behind the header's menu instead.
fn shown_button(app: &mut TestApp<TlApp>, name: &str) -> Option<[f64; 4]> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .find(|node| {
            node.widget == "Button"
                && node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(name)
                && node.rect[2] > 0.0
                && node.rect[3] > 0.0
        })
        .map(|node| node.rect)
}

/// Bring the window's Delete command within reach: a button of its own, or the task's own
/// command on the row's menu, opened here so that only the press that follows is what the
/// frames measure.
fn reach_delete(app: &mut TestApp<TlApp>, title: &str) -> Delete {
    if let Some(rect) = shown_button(app, "Delete") {
        return Delete::Button(rect);
    }
    let [x, y, _, h] = rows(app)
        .into_iter()
        .find(|(name, _)| name == title)
        .unwrap_or_else(|| panic!("the list shows {title}: {:?}", rows(app)))
        .1;
    // The row's own edge, off the content it holds: a row opened into its item fills with the
    // fields the person edits, and the ways into the row itself stand beside them, as the
    // disclosure gutter of a tree's row does.
    let (x, y) = (x + 2.0, y + h / 2.0);
    with_monotonic_time(at_now(), || {
        let harness = app.harness();
        harness.send(&ui::synthetic_input::cursor_moved(x, y));
        for state in [
            ui::winit::event::ElementState::Pressed,
            ui::winit::event::ElementState::Released,
        ] {
            harness.send(&ui::synthetic_input::mouse_input(
                state,
                ui::winit::event::MouseButton::Right,
            ));
        }
    });
    settle(app);
    let row = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| {
            node.role() == ui::accesskit::Role::MenuItem && node.label() == Some("Delete")
        })
        .and_then(|(_, node)| node.bounds())
        .expect("the row's menu offers Delete");
    Delete::Row([row.x0, row.y0, row.x1 - row.x0, row.y1 - row.y0])
}

/// Press what `reach_delete` reached, at `at`.
fn press_delete(app: &mut TestApp<TlApp>, remove: &Delete, at: ui::time::Instant) {
    let [x, y, w, h] = match remove {
        Delete::Button(rect) | Delete::Row(rect) => *rect,
    };
    with_monotonic_time(at, || {
        app.harness().click_at(x + w / 2.0, y + h / 2.0);
    });
}

/// Whether the row titled `title` is drawn marked.
fn marked(app: &mut TestApp<TlApp>, title: &str) -> bool {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter()
        .filter(|node| node.widget == "SwipeRow" || node.widget == "ListRow")
        .filter(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some(title)
        })
        .any(|node| {
            node.semantics
                .as_ref()
                .is_some_and(|semantics| semantics.state.checked == Some(true))
        })
}

/// The pixels of `rect` in the frame `pixels`, which the harness draws in device pixels.
fn pixels_in(app: &mut TestApp<TlApp>, pixels: &[u8], rect: [f64; 4]) -> Vec<[u8; 4]> {
    let root = app.harness().inspection().root.rect;
    let width_px = app.harness().inspection().viewport[0];
    let scale = f64::from(width_px) / root[2];
    let width = width_px as usize;
    let [x, y, w, h] = rect;
    let x0 = ((x - root[0]) * scale).round().max(0.0) as usize;
    let y0 = ((y - root[1]) * scale).round().max(0.0) as usize;
    let x1 = (((x - root[0] + w) * scale).round() as usize).min(width);
    let y1 = ((y - root[1] + h) * scale).round() as usize;
    let mut out = Vec::new();
    for row in y0..y1.min(pixels.len() / 4 / width.max(1)) {
        for col in x0..x1 {
            let at = (row * width + col) * 4;
            out.push([pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]);
        }
    }
    out
}

/// How far the pixels of `rect` in `pixels` are from the ones in `settled`: the mean of the
/// distances between the colours, 0 for the same frame.
fn difference(app: &mut TestApp<TlApp>, pixels: &[u8], settled: &[[u8; 4]], rect: [f64; 4]) -> f64 {
    let now = pixels_in(app, pixels, rect);
    apart(&now, settled)
}

/// How far one crop of a rect is from another of the same rect: the mean distance between
/// their colours, 0 for the same pixels.
fn apart(now: &[[u8; 4]], settled: &[[u8; 4]]) -> f64 {
    let common = now.len().min(settled.len());
    if common == 0 {
        return 0.0;
    }
    let total: f64 = now
        .iter()
        .zip(settled)
        .take(common)
        .map(|(a, b)| {
            (f64::from(a[0].abs_diff(b[0]))
                + f64::from(a[1].abs_diff(b[1]))
                + f64::from(a[2].abs_diff(b[2])))
                / 3.0
        })
        .sum();
    total / common as f64
}

/// How many frames of the series show the change part way: neither all but started nor all
/// but finished.
fn partway(series: &[f64]) -> usize {
    let Some(settled) = series.last().copied() else {
        return 0;
    };
    series
        .iter()
        .filter(|at| **at > settled * 0.05 && **at < settled * 0.95)
        .count()
}

/// How many frames of the series show a change under way: not yet at rest, and enough above
/// where it ends for the difference to be the change rather than the list it left.
fn in_flight(series: &[f64]) -> usize {
    let Some(settled) = series.last().copied() else {
        return 0;
    };
    let Some(peak) = series.iter().copied().reduce(f64::max) else {
        return 0;
    };
    let span = (peak - settled).abs();
    if span < 0.5 {
        return 0;
    }
    series
        .iter()
        .filter(|at| (**at - settled).abs() > span * 0.05)
        .count()
}

/// Whether the last frames of the series hold still: the motion has ended.
fn settles(series: &[f64]) -> bool {
    let Some(settled) = series.last().copied() else {
        return false;
    };
    series[series.len().saturating_sub(3)..]
        .iter()
        .all(|at| (*at - settled).abs() < 1.0)
}

/// Open the task titled `title` from the inbox: its own window's box is the one a check-off
/// stays on, where a list row's closes the row out of the list.
fn open_task(app: &mut TestApp<TlApp>, title: &str) {
    let [x, y, w, h] = rows(app)
        .into_iter()
        .find(|(name, _)| name == title)
        .unwrap_or_else(|| panic!("the list shows {title}"))
        .1;
    with_monotonic_time(at_now(), || {
        app.harness().click_at(x + w / 2.0, y + h / 2.0);
    });
    settle(app);
}

/// The box of the task a person opened: the window's own checkbox, which the list's rows do
/// not have (a row draws its box), and whether it holds.
fn page_box(app: &mut TestApp<TlApp>) -> ([f64; 4], bool) {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.iter()
        .filter(|node| node.widget == "Checkbox")
        .filter(|node| node.rect[2] > 0.0 && node.rect[3] > 0.0)
        .find_map(|node| {
            let state = node.semantics.as_ref()?.state.checked?;
            Some((node.rect, state))
        })
        .expect("the task's window shows its box")
}

#[test]
fn checking_a_task_off_fills_its_box_over_frames() {
    let mut app = staged("a busy week");
    // A task nothing sorts into a project or a day: the inbox keeps it open in the list.
    open_place(&mut app, "Inbox");
    let title = "Sort the paper mail";
    assert!(!marked(&mut app, title), "the task starts open");
    open_task(&mut app, title);
    let (band, held) = page_box(&mut app);
    assert!(!held, "the task's box starts open");

    let start = at_now();
    let (_, rest) = frame_at(&mut app, start);
    let before = pixels_in(&mut app, &rest, band);

    let pressed = start + ui::time::Duration::from_millis(100);
    let (x, y) = (band[0] + band[2] / 2.0, band[1] + band[3] / 2.0);
    with_monotonic_time(pressed, || app.harness().click_at(x, y));

    let mut series = Vec::new();
    for step in 1..=14 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let (_, pixels) = frame_at(&mut app, at);
        series.push(difference(&mut app, &pixels, &before, band));
    }
    println!("check-off frames: {series:?}");

    let settled = *series.last().expect("frames were read");
    assert!(
        settled > 6.0,
        "the box a person checked is drawn filled: {series:?}"
    );
    assert!(
        partway(&series) >= 3,
        "the mark arrives over frames rather than at once: {series:?}"
    );
    assert!(
        settles(&series),
        "the mark settles instead of going on: {series:?}"
    );

    settle(&mut app);
    assert!(
        page_box(&mut app).1,
        "the task stays checked once the app holds it"
    );
    assert!(
        !moving(&mut app),
        "nothing is still playing once the mark settled"
    );
}

#[test]
fn a_task_a_person_adds_lands_in_the_list() {
    // A list moves for a change a person made by hand: a pointer's insert or removal, a drag.
    // A key's result lands (`motion::MotionSource::lands_at_once`), so a title typed into the
    // entry arrives by the next frame, and the list has nothing left to play afterwards.
    let mut app = staged("a busy week");
    // The inbox's entry adds without a day, so the row it adds stands in the list the window
    // shows.
    open_place(&mut app, "Inbox");
    let before_rows = rows(&mut app).len();
    let band = list_band(&mut app);
    let start = at_now();
    let (_, rest) = frame_at(&mut app, start);
    let before = pixels_in(&mut app, &rest, band);

    // The entry at the list's foot, as a person adds a task: a title typed, then Enter. What is
    // typed names no day, so the row lands under the list's "No date" heading, below the rows
    // the band reaches.
    let [x, y, w, h] = field(&mut app, "New task");
    let pressed = start + ui::time::Duration::from_millis(100);
    with_monotonic_time(pressed, || {
        app.harness().click_at(x + w / 2.0, y + h / 2.0);
        app.harness().type_text("Water the plants");
        app.harness().key("Enter");
    });

    let mut series = Vec::new();
    let mut first = None;
    for step in 1..=14 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let (_, pixels) = frame_at(&mut app, at);
        series.push(difference(&mut app, &pixels, &before, band));
        first.get_or_insert(pixels);
    }
    println!("insert frames: {series:?}");

    settle(&mut app);
    assert_eq!(
        rows(&mut app).len(),
        before_rows + 1,
        "the task a person added is in the list"
    );
    let row = rows(&mut app)
        .into_iter()
        .find(|(name, _)| name == "Water the plants")
        .unwrap_or_else(|| panic!("the list shows what was typed: {:?}", rows(&mut app)))
        .1;
    let drawn = apart(
        &pixels_in(&mut app, &first.expect("a frame after the key"), row),
        &pixels_in(&mut app, &rest, row),
    );
    assert!(
        drawn > 0.5,
        "the new row is drawn by the frame after the key, where it lands: {drawn}"
    );
    assert!(
        in_flight(&series) == 0,
        "a key's result lands rather than playing: {series:?}"
    );
}

#[test]
fn the_rows_a_person_removes_leave_the_list() {
    let mut app = staged("a busy week");
    open_place(&mut app, "Today");
    let before_rows = rows(&mut app).len();
    assert!(before_rows > 2, "the place holds tasks");

    // A first removal teaches the list its rows' order: a task the person opens, then removes
    // from the pane it opened, takes the row it made. It stands at the list's foot, so nothing
    // below it has to make way.
    open_task(&mut app, "Reply to Mia about Sunday lunch");
    let remove = reach_delete(&mut app, "Reply to Mia about Sunday lunch");
    press_delete(&mut app, &remove, at_now());
    settle(&mut app);
    assert_eq!(
        rows(&mut app).len(),
        before_rows - 1,
        "the task a person removed left the row it made"
    );

    // The command reached first, so what is measured is the press and the rows it moves: the task
    // takes the step under it with it, and the rows below make way for the rest.
    open_task(&mut app, "Find the last service date");
    let remove = reach_delete(&mut app, "Find the last service date");
    let band = list_band(&mut app);
    let start = at_now();
    let (_, rest) = frame_at(&mut app, start);
    let before = pixels_in(&mut app, &rest, band);
    let pressed = start + ui::time::Duration::from_millis(100);
    press_delete(&mut app, &remove, pressed);

    let mut frames = Vec::new();
    for step in 1..=14 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        set_at(at);
        with_monotonic_time(at, || app.idle());
        let (_, pixels) = frame_at(&mut app, at);
        frames.push(pixels);
    }

    // Where the rows the removal leaves come to rest. They make way by about their own height,
    // so the frame before the press is a poor ruler for the move: it differs from the last
    // frame as much as any frame on the way does. The last frame is the one they end on, so
    // every frame read against it says how far the rows still are from where they land.
    let last = frames.last().expect("frames were read");
    let held = pixels_in(&mut app, last, band);
    let moved = apart(&before, &held);
    assert!(
        moved > 3.0,
        "the rows the removal leaves are drawn elsewhere: {moved}"
    );
    let mut series = Vec::new();
    for pixels in &frames {
        series.push(difference(&mut app, pixels, &held, band));
    }
    println!("removal frames: {series:?}");

    settle(&mut app);
    assert_eq!(
        rows(&mut app).len(),
        before_rows - 3,
        "the task a person removed took the step under it, and the row it made: {:?}",
        rows(&mut app)
            .into_iter()
            .map(|(n, _)| n)
            .collect::<Vec<_>>()
    );
    assert!(
        in_flight(&series) >= 3,
        "the rows below make way over frames rather than jumping: {series:?}"
    );
    assert!(settles(&series), "the list settles: {series:?}");
    assert!(
        !moving(&mut app),
        "nothing is still playing once the list settled"
    );
}

/// What one frame costs in work done: the CPU its thread ran it on (a loaded machine stretches
/// the wall clock of the same work, not this), and what it rebuilt and built.
struct FrameWork {
    cpu: std::time::Duration,
    drew: bool,
    nodes: u64,
    rebuilt: u64,
    marked_dirty: usize,
}

/// Run the frame due at `at` and draw it, timing the frame itself: the rasterisation that
/// follows is the harness drawing the window off screen, waits on the GPU, and is no work the
/// window's frame does.
fn frame_work(app: &mut TestApp<TlApp>, at: ui::time::Instant) -> (FrameWork, Vec<u8>) {
    let (nodes, rebuilt) = {
        let harness = app.harness();
        (harness.layout_nodes_built(), harness.rebuilt_frames())
    };
    let started = ui::rt::thread_cpu_time().expect("a thread's CPU clock");
    let mut cpu = std::time::Duration::ZERO;
    set_at(at);
    let (drew, pixels) = with_monotonic_time(at, || {
        let drew = app.harness().poll_frame().expect("a frame");
        cpu = ui::rt::thread_cpu_time()
            .expect("a thread's CPU clock")
            .saturating_sub(started);
        (drew, app.harness().render_rgba())
    });
    let (nodes, rebuilt, layout) = {
        let harness = app.harness();
        (
            harness.layout_nodes_built() - nodes,
            harness.rebuilt_frames() - rebuilt,
            harness.last_relayout(),
        )
    };
    (
        FrameWork {
            cpu,
            drew,
            nodes,
            rebuilt,
            marked_dirty: layout.marked_dirty,
        },
        pixels,
    )
}

/// The boundaries the window's views have rendered since the window opened: a mark's frames
/// render none of them again. The visit lays the window out twice, so a test reads it between
/// frames and never between the frames it costs.
fn renders(app: &mut TestApp<TlApp>) -> u64 {
    use tilo_render_protocol::patch::Lane;
    use ui::framework::testing::with_surface;
    with_surface::<TlApp, tilolang_app::window::Window, _>(app.harness(), |window| {
        let instance = window.instance();
        [Lane::Root, Lane::Detail]
            .into_iter()
            .filter_map(|lane| instance.view(lane))
            .map(|view| view.borrow().stats().renders)
            .sum()
    })
    .expect("the tasks window")
}

/// The box of the opened task's done toggle: the change mark a person sees fill.
fn the_mark_box(app: &mut TestApp<TlApp>) -> [f64; 4] {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .find(|node| node.widget == "Checkbox")
        .map(|node| node.rect)
        .expect("the opened task's box")
}

/// What a still window costs a frame: what its frames do in work, not how long a machine busy
/// with other work takes to run them. It lays out nothing: no rebuild, no node built.
fn still_frames(
    app: &mut TestApp<TlApp>,
    start: ui::time::Instant,
) -> (Vec<std::time::Duration>, usize, Vec<u8>) {
    let mut still = Vec::new();
    let mut dirtied = 0;
    let mut rest = Vec::new();
    for step in 0..8 {
        let at = start + ui::time::Duration::from_millis(16 * step);
        // A still frame is one the window draws with nothing of the app moving: a widget asks for
        // it, and the window paints what it already shows.
        with_monotonic_time(at, || {
            ui::motion::want_frames_until(at + ui::time::Duration::from_millis(1));
        });
        let (work, pixels) = frame_work(app, at);
        assert!(work.drew, "a still window draws the frame it is asked for");
        assert_eq!(work.rebuilt, 0, "a still frame rebuilds no layout");
        assert_eq!(work.nodes, 0, "a still frame builds no layout nodes");
        dirtied = dirtied.max(work.marked_dirty);
        still.push(work.cpu);
        rest = pixels;
    }
    (still, dirtied, rest)
}

#[test]
fn a_frame_of_the_mark_costs_what_a_still_frame_costs() {
    let mut app = staged("a busy week");
    let mark = the_mark_box(&mut app);
    assert!(!moving(&mut app), "the window rests before the mark moves");

    let start = at_now();
    let (still, still_dirty, rest) = still_frames(&mut app, start);
    let before = pixels_in(&mut app, &rest, mark);

    // Tick the opened task off: its box fills over the frames that follow.
    let (x, y) = (mark[0] + mark[2] / 2.0, mark[1] + mark[3] / 2.0);
    let pressed = start + ui::time::Duration::from_millis(120);
    with_monotonic_time(pressed, || app.harness().click_at(x, y));
    let mut works = Vec::new();
    let mut series = Vec::new();
    let mut playing = Vec::new();
    let mut step = 0;
    // Where the press lands: the app has answered it, so the frame rebuilt no layout and the
    // views it answers with rendered nothing new.
    let mut rendered = with_monotonic_time(pressed, || renders(&mut app));
    loop {
        step += 1;
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let (work, pixels) = frame_work(&mut app, at);
        series.push(difference(&mut app, &pixels, &before, mark));
        playing.push(moving(&mut app));
        let landed = work.rebuilt == 0;
        works.push(work);
        let now = with_monotonic_time(at, || renders(&mut app));
        let answered = now == rendered;
        rendered = now;
        if landed && answered {
            break;
        }
        assert!(step <= 3, "the press lands within a few frames");
    }
    assert_eq!(
        playing.last(),
        Some(&true),
        "the mark is on its way where the press lands"
    );
    // The frames from here on are the mark's own: the press has landed, the app answered it.
    let landed = works.len();

    // The mark's own frames, until it settles: they render none of the views again.
    while playing.last() == Some(&true) {
        step += 1;
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let (work, pixels) = frame_work(&mut app, at);
        series.push(difference(&mut app, &pixels, &before, mark));
        playing.push(moving(&mut app));
        works.push(work);
        assert!(
            with_monotonic_time(at, || renders(&mut app)) == rendered,
            "frame {step} of the mark renders no boundary of the view again"
        );
        assert!(step <= 16, "the mark settles");
    }

    // What the mark's own frames cost: a repaint. They rebuild no layout, build no node,
    // mark no more widgets for a measure than a still frame, and render none of the view.
    let mut moving = Vec::new();
    for (index, work) in works.iter().enumerate().skip(landed) {
        assert_eq!(
            work.rebuilt,
            0,
            "frame {} of the mark rebuilds no layout",
            index + 1
        );
        assert_eq!(
            work.nodes,
            0,
            "frame {} of the mark builds no layout nodes",
            index + 1
        );
        assert!(
            work.marked_dirty <= still_dirty,
            "frame {} of the mark marks {count} widgets for a measure, no more than a still \
             frame's {still_dirty}",
            index + 1,
            count = work.marked_dirty
        );
        moving.push(work.cpu);
    }

    let still_ms = median_ms(&still);
    let moving_ms = median_ms(&moving);
    println!("a frame: {still_ms:.2} ms CPU still, {moving_ms:.2} ms CPU with the mark on its way");
    println!("the mark over frames: {series:?}");
    assert!(
        partway(&series) >= 3,
        "the mark is on its way over the frames it is costed on: {series:?}"
    );
    assert!(settles(&series), "the mark settles: {series:?}");
    assert!(
        moving_ms < still_ms * 1.5 + 1.0,
        "a frame of the mark costs what a still frame costs: {still_ms:.2} ms still, \
         {moving_ms:.2} ms moving"
    );
}

/// Half a second of the window's idle ticks, as a live window runs them: how many frames it
/// asked for, and the CPU they cost.
fn idle_ticks(app: &mut TestApp<TlApp>, start: ui::time::Instant) -> (usize, std::time::Duration) {
    let mut produced = 0;
    let mut cpu = std::time::Duration::ZERO;
    for step in 0..30 {
        let at = start + ui::time::Duration::from_millis(16 * step);
        let began = ui::rt::thread_cpu_time().expect("a thread's CPU clock");
        if draw(app, at) {
            produced += 1;
        }
        cpu += ui::rt::thread_cpu_time()
            .expect("a thread's CPU clock")
            .saturating_sub(began);
    }
    (produced, cpu)
}

/// How many of the controls the window shows are bound to an action the app publishes, as the
/// actions' own controls are (an invocation's is the press that runs it).
fn bound_controls(app: &mut TestApp<TlApp>) -> usize {
    ui::widget::controls::control_action_bindings(app.harness().root_widget())
        .into_iter()
        .filter(|binding| {
            matches!(
                &binding.kind,
                ui::widget::ControlBindingKind::Action(address) if address.action != "invoke"
            )
        })
        .count()
}

/// How many times the observed frame ran the scope `phase`.
fn calls(samples: &[ui::tilo_resource::profiling::Sample], phase: &str) -> usize {
    samples
        .iter()
        .filter(|sample| sample.phase == phase)
        .map(|sample| sample.calls)
        .sum()
}

/// A frame of a window whose controls are bound to the actions it publishes reads each control's
/// action again, and runs none of them: binding a control checks what its action takes, it never
/// plans the action. Planning one is a trial run of it, and a frame that planned the action of
/// every row it reads took ten times as long as the frame itself.
#[test]
fn a_frame_of_bound_controls_plans_no_action() {
    let mut app = staged("a busy week");
    let bound = bound_controls(&mut app);
    assert!(
        bound > 0,
        "the window's controls are bound to the actions it publishes"
    );
    let ((), samples) = ui::tilo_resource::profiling::observe(|| app.harness().relayout());
    assert_eq!(
        calls(&samples, "ui.frame_sync"),
        1,
        "the frame syncs: {samples:?}"
    );
    assert_eq!(
        calls(&samples, "tl.plan"),
        0,
        "a frame plans no action of the {bound} controls it shows"
    );
}

/// A frame reads every row and menu item the window shows again, and binds none of their
/// controls: what a row's controls were bound to when it was read last is what they stay bound
/// to, until the app publishes other actions. Finding the action a control runs searched the
/// published ones, and validating it read the action's arguments, so a frame that bound the
/// controls of every row it read spent most of the frame at that.
#[test]
fn a_still_frame_builds_no_action_bindings() {
    let mut app = staged("a busy week");
    let bound = bound_controls(&mut app);
    assert!(
        bound > 0,
        "the window's controls are bound to the actions it publishes"
    );
    // The frame the window settled on read its rows against what the app publishes now.
    app.harness().relayout();
    let ((), samples) = ui::tilo_resource::profiling::observe(|| app.harness().relayout());
    assert_eq!(
        calls(&samples, "ui.frame_sync"),
        1,
        "the frame syncs: {samples:?}"
    );
    let calls = |phase: &str| calls(&samples, phase);
    assert_eq!(
        calls("ui.bind"),
        0,
        "a still frame builds no binding of the {bound} controls it shows: {samples:?}"
    );
}

/// A still window is not a frame source: left alone, it produces no frames at all, and a frame
/// something outside it forces re-measures nothing, syncs nothing and asks no History surface
/// anything, because nothing it shows moved.
#[test]
fn a_still_window_produces_no_frames() {
    let mut app = staged("a busy week");
    // The live window's own tick: take up what the app's workers woke it for.
    app.frame();
    // The frame that settles the window left the witness the next frame is read against.
    app.harness().relayout();
    assert!(
        !moving(&mut app),
        "the window rests before it is left alone"
    );

    let start = at_now();
    let (produced, cpu) = idle_ticks(&mut app, start);
    assert_eq!(
        produced, 0,
        "an idle window produces no frame: what it would draw, it already shows"
    );
    assert!(
        cpu < std::time::Duration::from_millis(50),
        "half a second of idle ticks costs {:.2} ms CPU, not a frame's worth per tick",
        cpu.as_secs_f64() * 1000.0
    );
    assert!(
        app.harness().queued_work_sources().is_empty(),
        "an idle window queues no work: {:?}",
        app.harness().queued_work_sources()
    );
    assert_eq!(
        app.harness().frame_requesters(),
        Vec::new(),
        "no widget asks an idle window for a frame"
    );
    assert_eq!(
        app.harness().redraw_sources(),
        Vec::<String>::new(),
        "no widget asks an idle window to repaint it"
    );

    // A frame the window itself asks for while nothing of the app changed — a mark
    // filling in — costs the paint of what moved: no measure, no sync, no History
    // surface asked anything.
    let mark = the_mark_box(&mut app);
    let (x, y) = (mark[0] + mark[2] / 2.0, mark[1] + mark[3] / 2.0);
    let pressed = start + ui::time::Duration::from_millis(200);
    with_monotonic_time(pressed, || app.harness().click_at(x, y));
    let (samples, drew, layout, quiet) = {
        let mut step = 0;
        loop {
            step += 1;
            assert!(
                step <= 20,
                "the mark settles: the window is asked for a frame by {:?}",
                app.harness().frame_requesters()
            );
            let at = pressed + ui::time::Duration::from_millis(16 * step);
            let quiet_before = app.harness().quiet_frames();
            let (drew, samples) = ui::tilo_resource::profiling::observe(|| draw(&mut app, at));
            let quiet = app.harness().quiet_frames() > quiet_before;
            let layout = app.harness().last_relayout();
            if !moving(&mut app) {
                break (samples, drew, layout, quiet);
            }
        }
    };
    assert!(
        drew,
        "the frame that ends the mark is one the window asked for"
    );
    assert!(
        quiet,
        "a still frame syncs and publishes nothing: {samples:?}"
    );
    assert_eq!(
        layout.marked_dirty, 0,
        "a still frame marks nothing for a measure"
    );
    assert!(!layout.rebuilt, "a still frame rebuilds no layout");
    let calls = |phase: &str| {
        samples
            .iter()
            .filter(|sample| sample.phase == phase)
            .map(|sample| sample.calls)
            .sum::<usize>()
    };
    assert_eq!(
        calls("ui.frame_sync"),
        0,
        "a still frame syncs no app: {samples:?}"
    );
    assert_eq!(
        calls("ui.history_root_validation"),
        0,
        "a still frame asks History nothing: {samples:?}"
    );
    let loud: Vec<&str> = samples
        .iter()
        .map(|sample| sample.phase)
        .filter(|phase| phase.starts_with("ui.loud."))
        .collect();
    assert!(loud.is_empty(), "a still frame is quiet, not {loud:?}");
}

fn median_ms(times: &[std::time::Duration]) -> f64 {
    let mut sorted: Vec<f64> = times.iter().map(|t| t.as_secs_f64() * 1000.0).collect();
    sorted.sort_by(f64::total_cmp);
    sorted.get(sorted.len() / 2).copied().unwrap_or_default()
}

#[test]
#[ignore = "writes the frames it reads as PNGs, to look at"]
fn the_frames_a_person_sees() {
    // A task checked off in its row.
    let mut app = staged("a busy week");
    let band = row_box(&mut app, "Buy oat milk and eggs");
    let start = at_now();
    let (_, _) = frame_at(&mut app, start);
    shot(&mut app, "check-off-00-rest");
    let pressed = start + ui::time::Duration::from_millis(100);
    let (x, y) = (band[0] + band[2] / 2.0, band[1] + band[3] / 2.0);
    with_monotonic_time(pressed, || app.harness().click_at(x, y));
    for step in 1..=10 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let _ = frame_at(&mut app, at);
        if step % 2 == 0 {
            shot(&mut app, &format!("check-off-{step:02}"));
        }
    }

    // A task added at the list's foot.
    let mut app = staged("a busy week");
    let start = at_now();
    let _ = frame_at(&mut app, start);
    let [x, y, w, h] = field(&mut app, "New task");
    let pressed = start + ui::time::Duration::from_millis(100);
    with_monotonic_time(pressed, || {
        app.harness().click_at(x + w / 2.0, y + h / 2.0);
        app.harness().type_text("Water the plants");
        app.harness().key("Enter");
    });
    for step in 1..=10 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let _ = frame_at(&mut app, at);
        if step % 2 == 0 {
            shot(&mut app, &format!("insert-{step:02}"));
        }
    }

    // Two removals from the pane: the first reads the list's order, the second glides.
    let mut app = staged("a busy week");
    let start = at_now();
    let _ = frame_at(&mut app, start);
    shot(&mut app, "remove-00-rest");
    let remove = reach_delete(&mut app, "Book a haircut");
    press_delete(
        &mut app,
        &remove,
        start + ui::time::Duration::from_millis(100),
    );
    settle(&mut app);
    let [x, y, w, h] = rows(&mut app)
        .into_iter()
        .find(|(name, _)| name == "Book a haircut")
        .expect("the list shows the task")
        .1;
    with_monotonic_time(at_now(), || {
        app.harness().click_at(x + w / 2.0, y + h / 2.0);
    });
    settle(&mut app);
    let (_, _) = frame_at(&mut app, at_now());
    shot(&mut app, "remove-01-opened");
    let remove = reach_delete(&mut app, "Book a haircut");
    let pressed = at_now() + ui::time::Duration::from_millis(100);
    press_delete(&mut app, &remove, pressed);
    for step in 1..=10 {
        let at = pressed + ui::time::Duration::from_millis(16 * step);
        let _ = frame_at(&mut app, at);
        if step % 2 == 0 {
            shot(&mut app, &format!("remove-{step:02}"));
        }
    }
}

/// Write the frame the window holds now, and print it as hex so a run away from the machine
/// it was drawn on can carry it back.
fn shot(app: &mut TestApp<TlApp>, name: &str) {
    let path = shot_path(name);
    app.harness().render_png(&path);
    let bytes = std::fs::read(&path).expect("the frame it wrote");
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in &bytes {
        use std::fmt::Write as _;
        write!(hex, "{byte:02x}").expect("a String takes every write");
    }
    println!("SHOT {name} {hex}");
}

/// Where a frame is written: beside the test binary, so a run away from this machine can carry
/// it back (each is printed by the test as it is written).
fn shot_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}.png"))
}
