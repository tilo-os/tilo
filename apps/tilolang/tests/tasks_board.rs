//! The Tasks package as the UI host runs it, on a project shown as a board: a card dragged
//! within its lane or into another lands where it was dropped, through the lane's drop, the
//! window and the app's `place` action. And the Today list, which reads the person's day, takes
//! in the next day's tasks when their midnight passes: the window sleeps until then and wakes
//! for it.
//!
//! With `TILO_TASKS_SHOTS=<dir>` the test `shots` writes the board at desktop and phone sizes,
//! light and dark, before and after a drag.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

mod support;

use tilolang_app::app::TlApp;
use ui::framework::testing::TestApp;
use ui::headless::InspectionNode;

const DESKTOP: (u32, u32) = (1280, 800);
const PHONE: (u32, u32) = (420, 860);

/// The project board scenario staged: a lane of no section and two sections with tasks.
fn staged(viewport: (u32, u32)) -> TestApp<TlApp> {
    support::installed("tasks");
    let mut app = TestApp::<TlApp>::capture_at(Some("a project board"), viewport)
        .expect("the scenario stages");
    settle(&mut app);
    app
}

fn settle(app: &mut TestApp<TlApp>) {
    for _ in 0..8 {
        app.harness()
            .settle_for_capture(ui::time::Duration::from_millis(300));
        app.harness().relayout();
    }
}

fn walk<'a>(node: &'a InspectionNode, out: &mut Vec<&'a InspectionNode>) {
    out.push(node);
    for child in &node.children {
        walk(child, out);
    }
}

/// The cards on the board, `(title, [x, y, width, height])`, in the order they are drawn: each
/// card's rect, named by the row it holds.
fn cards(app: &mut TestApp<TlApp>) -> Vec<(String, [f64; 4])> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let mut out = Vec::new();
    let mut card = None;
    for node in all {
        if node.widget == "Card" {
            card = Some(node.rect);
        } else if node.widget == "ListRow"
            && let Some(rect) = card.take()
            && let Some(name) = node.semantics.as_ref().and_then(|s| s.name.clone())
        {
            out.push((name, rect));
        }
    }
    out
}

/// The titles of the cards whose left edge is at `x`, top to bottom: one lane.
fn lane(app: &mut TestApp<TlApp>, x: f64) -> Vec<String> {
    let mut lane: Vec<(String, [f64; 4])> = cards(app)
        .into_iter()
        .filter(|(_, rect)| (rect[0] - x).abs() < 1.0)
        .collect();
    lane.sort_by(|a, b| a.1[1].total_cmp(&b.1[1]));
    lane.into_iter().map(|(name, _)| name).collect()
}

fn rect_of(app: &mut TestApp<TlApp>, title: &str) -> [f64; 4] {
    cards(app)
        .into_iter()
        .find(|(name, _)| name.contains(title))
        .map_or_else(|| panic!("no card `{title}`: {:?}", cards(app)), |(_, r)| r)
}

/// Drag the card `title` to just inside the top of the card `onto`, or below it.
fn drag(app: &mut TestApp<TlApp>, title: &str, onto: &str, below: bool) {
    let from = rect_of(app, title);
    let to = rect_of(app, onto);
    let y = if below {
        to[1] + to[3] + 24.0
    } else {
        to[1] + 6.0
    };
    app.harness().drag_at(
        (from[0] + from[2] / 2.0, from[1] + from[3] / 2.0),
        (to[0] + to[2] / 2.0, y),
    );
    settle(app);
}

fn names(lane: &[String]) -> Vec<&str> {
    lane.iter()
        .map(|name| {
            [
                "Measure the new bedroom",
                "Ask Mia about the sofa",
                "Order boxes and tape",
                "Pack the kitchen",
                "Book the van",
                "Give notice to the electricity company",
            ]
            .into_iter()
            .find(|title| name.contains(title))
            .unwrap_or("?")
        })
        .collect()
}

#[test]
fn a_card_dragged_within_its_lane_and_into_another_lands_where_it_was_dropped() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let packing_x = rect_of(&mut app, "Order boxes and tape")[0];
    let booked_x = rect_of(&mut app, "Book the van")[0];
    assert_eq!(
        names(&lane(&mut app, packing_x)),
        ["Order boxes and tape", "Pack the kitchen"],
        "a lane shows its open tasks in order"
    );
    // Within its lane: the last card above the first.
    drag(&mut app, "Pack the kitchen", "Order boxes and tape", false);
    assert_eq!(
        names(&lane(&mut app, packing_x)),
        ["Pack the kitchen", "Order boxes and tape"]
    );
    // Into another lane, between its two cards.
    drag(&mut app, "Order boxes and tape", "Give notice", false);
    assert_eq!(
        names(&lane(&mut app, booked_x)),
        [
            "Book the van",
            "Order boxes and tape",
            "Give notice to the electricity company"
        ]
    );
    assert_eq!(names(&lane(&mut app, packing_x)), ["Pack the kitchen"]);
}

/// The wall clock at `hour:minute:second` on `day` of 2026, in the zone this process reads as
/// the person's.
fn local(month: u32, day: u32, (hour, minute, second): (u32, u32, u32)) -> ui::time::SystemTime {
    use chrono::TimeZone as _;
    let at = chrono::Local
        .with_ymd_and_hms(2026, month, day, hour, minute, second)
        .earliest()
        .expect("the time exists where the person is");
    let ms = u64::try_from(at.timestamp_millis()).expect("after 1970");
    ui::time::UNIX_EPOCH
        .checked_add(ui::time::Duration::from_millis(ms))
        .expect("the clock reaches it")
}

/// Whether some accessible node of the window is named with `text`.
fn shows(app: &mut TestApp<TlApp>, text: &str) -> bool {
    accessible(app)
        .iter()
        .any(|node| node.name.as_deref().is_some_and(|name| name.contains(text)))
}

fn access_walk<'a>(node: &'a ui::a11y::AccessNode, out: &mut Vec<&'a ui::a11y::AccessNode>) {
    out.push(node);
    for child in &node.children {
        access_walk(child, out);
    }
}

/// Every accessible node of the window.
fn accessible(app: &mut TestApp<TlApp>) -> Vec<ui::a11y::AccessNode> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let mut out = Vec::new();
    for node in all {
        if let Some(access) = &node.semantics {
            let mut nodes = Vec::new();
            access_walk(access, &mut nodes);
            out.extend(nodes.into_iter().cloned());
        }
    }
    out
}

/// Click the node whose accessible name begins with `label` (a sidebar item, its count after).
fn click_item(app: &mut TestApp<TlApp>, label: &str) {
    let all = accessible(app);
    let found = all.iter().find_map(|node| {
        node.name
            .as_deref()
            .filter(|name| name.starts_with(label))
            .and(node.bounds)
    });
    let Some(b) = found else {
        let names: Vec<&str> = all.iter().filter_map(|node| node.name.as_deref()).collect();
        panic!("nothing named `{label}`: {names:?}");
    };
    app.harness()
        .click_at(f64::midpoint(b.x0, b.x1), f64::midpoint(b.y0, b.y1));
}

#[test]
fn the_today_list_takes_in_tomorrows_task_when_the_persons_midnight_passes() {
    use ui::time::testing::with_wall;
    let _settled = ui::motion::testing::settled();
    let before = local(6, 30, (23, 59, 0));
    let after = local(7, 1, (0, 0, 30));
    // 23:59 on the scenario's day: "Reply to Mia" is due tomorrow, so Today leaves it out.
    let mut app = with_wall(before, || {
        support::installed("tasks");
        let mut app = TestApp::<TlApp>::capture_at(Some("a busy week"), DESKTOP)
            .expect("the scenario stages");
        settle(&mut app);
        click_item(&mut app, "Today");
        settle(&mut app);
        assert!(shows(&mut app, "Buy oat milk"), "today's task is listed");
        assert!(
            !shows(&mut app, "Reply to Mia"),
            "tomorrow's task is not listed today"
        );
        // The window sleeps until the person's midnight, and not before.
        let deadline = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .expect("a window showing today waits for midnight");
        let wait = deadline.saturating_duration_since(ui::time::now());
        assert!(
            wait > ui::time::Duration::from_secs(50) && wait <= ui::time::Duration::from_secs(60),
            "the window wakes at midnight, a minute away: {wait:?}"
        );
        app
    });
    // Past midnight the deadline is due; the frame it wakes for shows the new day's list.
    with_wall(after, || {
        let due = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .is_some_and(|deadline| deadline <= ui::time::now());
        assert!(due, "the window's deadline is due past midnight");
        app.frame();
        assert!(
            shows(&mut app, "Reply to Mia"),
            "the task due on the new day is listed once midnight passed"
        );
        // And the window waits for the next midnight.
        let wait = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .map(|deadline| deadline.saturating_duration_since(ui::time::now()));
        assert!(
            wait.is_some_and(|wait| wait > ui::time::Duration::from_secs(23 * 3600)),
            "the window waits for the next midnight: {wait:?}"
        );
    });
}

#[test]
fn shots() {
    let Some(dir) = std::env::var_os("TILO_TASKS_SHOTS") else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).unwrap();
    let _settled = ui::motion::testing::settled();
    for (size, viewport) in [("desktop", DESKTOP), ("phone", PHONE)] {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            let mut app = staged(viewport);
            app.harness().set_theme(theme);
            settle(&mut app);
            let name = |state: &str| dir.join(format!("board-{state}-{size}-{shade}.png"));
            app.harness().render_png(&name("rest"));
            drag(&mut app, "Pack the kitchen", "Order boxes and tape", false);
            app.harness().render_png(&name("moved"));
        }
    }
}
