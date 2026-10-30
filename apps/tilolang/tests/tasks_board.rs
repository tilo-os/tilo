//! The Tasks package as the UI host runs it, on a project shown as a board: a card dragged
//! within its lane or into another lands where it was dropped, through the lane's drop, the
//! window and the app's `place` action.
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
