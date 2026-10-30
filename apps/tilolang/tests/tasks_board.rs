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

#[allow(
    dead_code,
    reason = "each test binary reads what it needs of the change marks"
)]
mod change_marks;
#[allow(
    dead_code,
    reason = "each test binary reads what it needs of a window's history"
)]
mod history_window;
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

/// What an agent's run did in Tasks is marked where the person sees it: the tasks it added and
/// the one it checked off carry a mark in its colour on their rows, with no code of the app's,
/// and the window's header counts them. With `TILO_LEGIBILITY_SHOTS=<dir>` the window is
/// written, light and dark.
#[test]
fn what_a_run_did_in_tasks_is_marked_on_its_rows() {
    use change_marks::{RUN, action_id, chroma_in, marks, published_with, run, settle};
    let mut app =
        ui::framework::testing::HostedApp::open(support::installed("tasks"), "ada").unwrap();
    settle(&mut app);
    let root = app
        .read(|doc| tilolang_tilo::action::resource_ids(doc, "so.tilo.lang.tasks"))
        .remove(0);
    let tasks = format!("tl:so.tilo.lang.tasks:{root}");
    let add = action_id(&mut app, &tasks, "Add");
    let titled = |title: &str| {
        let mut args = serde_json::Map::new();
        args.insert("title".to_owned(), title.into());
        args
    };
    // Ada's own task first: nothing to mark.
    app.invoke(None, None, &tasks, &add, &titled("Water the plants"), false)
        .unwrap();
    settle(&mut app);
    assert!(marks(&mut app).is_empty());
    let (agent, ambient) = run();
    for title in ["Call the landlord", "Book the dentist"] {
        app.invoke(
            Some(agent.clone()),
            Some(&ambient),
            &tasks,
            &add,
            &titled(title),
            false,
        )
        .unwrap();
    }
    // It checks off Ada's task too.
    let task = published_with(&mut app, &tasks, "Complete");
    let complete = action_id(&mut app, &task, "Complete");
    app.invoke(
        Some(agent),
        Some(&ambient),
        &task,
        &complete,
        &serde_json::Map::new(),
        false,
    )
    .unwrap();
    settle(&mut app);
    let shown = marks(&mut app);
    assert!(shown.len() >= 2, "{shown:?}");
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    for (name, rect) in &shown {
        assert!(
            chroma_in(&mut app, *rect) > ground.saturating_add(24),
            "{name} is drawn in the agent's colour"
        );
    }
    if let Some(dir) = change_marks::shots_dir() {
        for (shade, theme) in [("light", ui::Theme::light()), ("dark", ui::Theme::dark())] {
            app.harness().set_theme(theme);
            settle(&mut app);
            app.harness()
                .render_png(&dir.join(format!("tasks-marks-{shade}.png")));
        }
    }
    let _ = RUN;
}

/// A task's history in its window: who set its due day and when, the day it was due before and
/// the day it is due now, each shown as a day, and an agent's run as one entry. With
/// `TILO_HISTORY_SHOTS=<dir>` the window is written, light and dark, the run's entry closed and
/// open.
#[test]
fn a_tasks_history_shows_its_due_day_before_and_after_and_who_set_it() {
    use change_marks::{action_id, named, published_with, run, settle};
    use history_window::{after, shoot, times};
    let _settled = ui::motion::testing::settled();
    let mut app =
        ui::framework::testing::HostedApp::open(support::installed("tasks"), "ada").unwrap();
    settle(&mut app);
    let root = app
        .read(|doc| tilolang_tilo::action::resource_ids(doc, "so.tilo.lang.tasks"))
        .remove(0);
    let tasks = format!("tl:so.tilo.lang.tasks:{root}");
    let add = action_id(&mut app, &tasks, "Add");
    let mut titled = serde_json::Map::new();
    titled.insert("title".to_owned(), "Call the landlord".into());
    app.invoke(None, None, &tasks, &add, &titled, false)
        .unwrap();
    settle(&mut app);
    let task = published_with(&mut app, &tasks, "Set due date");
    let schedule = action_id(&mut app, &task, "Set due date");
    let due = |day: &str| {
        let mut args = serde_json::Map::new();
        args.insert("due".to_owned(), day.into());
        args
    };
    app.invoke(None, None, &task, &schedule, &due("2026-11-02"), false)
        .unwrap();
    // The agent's run puts it off twice: one entry of two changes.
    let (agent, ambient) = run();
    for day in ["2026-11-04", "2026-11-05"] {
        app.invoke(
            Some(agent.clone()),
            Some(&ambient),
            &task,
            &schedule,
            &due(day),
            false,
        )
        .unwrap();
    }
    let done = ui::time::SystemTime::now();
    settle(&mut app);
    ui::time::testing::with_wall(after(done, 1), || {
        let row = named(&mut app, "SwipeRow", "Call the landlord");
        app.harness()
            .click_at(row[0] + row[2] / 4.0, row[1] + row[3] / 2.0);
        settle(&mut app);
        assert_eq!(
            times(&mut app).len(),
            2,
            "the run's entry and Ada's own, each saying when"
        );
        shoot(&mut app, "tasks-history", settle);
        let show = named(&mut app, "Button", "Show changes");
        app.harness()
            .click_at(show[0] + show[2] / 2.0, show[1] + show[3] / 2.0);
        settle(&mut app);
        assert_eq!(
            times(&mut app).len(),
            4,
            "opened, each of the run's changes says when"
        );
        shoot(&mut app, "tasks-history-open", settle);
    });
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
        // The window sleeps until what it shows next reads differently, and not before: the
        // moment the staged task was made, in its history, stops reading as just now before
        // the person's midnight, a minute away, comes.
        let deadline = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .expect("a window showing today waits for midnight");
        let wait = deadline.saturating_duration_since(ui::time::now());
        let expected = wakes_after(before, before, local(7, 1, (0, 0, 0)));
        assert!(
            wait.abs_diff(expected) < ui::time::Duration::from_secs(1)
                && wait <= ui::time::Duration::from_secs(60),
            "the window wakes when the history's moment reads anew, before midnight: \
             {wait:?}, not {expected:?}"
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
        // And the window waits again, for the next midnight or the history's moment reading
        // anew, whichever comes first.
        let wait = app
            .harness()
            .frame_schedule()
            .next_wake_deadline
            .map(|deadline| deadline.saturating_duration_since(ui::time::now()));
        let expected = wakes_after(before, after, local(7, 2, (0, 0, 0)));
        assert!(
            wait.is_some_and(|wait| wait.abs_diff(expected) < ui::time::Duration::from_secs(1)),
            "the window waits for what it shows to read differently: {wait:?}, not {expected:?}"
        );
    });
}

/// How long after `now` a window showing a moment made at `made` and a day ending at
/// `midnight` next shows something different: when the moment reads anew as the toolkit reads
/// it, or midnight, whichever is first.
fn wakes_after(
    made: ui::time::SystemTime,
    now: ui::time::SystemTime,
    midnight: ui::time::SystemTime,
) -> ui::time::Duration {
    let secs = |at: ui::time::SystemTime| {
        i64::try_from(at.duration_since(ui::time::UNIX_EPOCH).unwrap().as_secs()).unwrap()
    };
    let midnight = secs(midnight);
    let at = ui::widget::next_change(secs(made), secs(now))
        .map_or(midnight, |change| change.min(midnight));
    ui::time::Duration::from_secs(u64::try_from(at.checked_sub(secs(now)).unwrap()).unwrap())
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
