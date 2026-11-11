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
use ui::a11y::Role;
use ui::framework::testing::TestApp;
use ui::headless::InspectionNode;

const DESKTOP: (u32, u32) = (1280, 800);
const PHONE: (u32, u32) = (420, 860);

/// The project board scenario staged, and the project's bar switched to its board: a lane of no
/// section and two sections with tasks.
fn staged(viewport: (u32, u32)) -> TestApp<TlApp> {
    support::installed("tasks");
    let mut app = TestApp::<TlApp>::capture_at(Some("a project board"), viewport)
        .expect("the scenario stages");
    settle(&mut app);
    show_board(&mut app);
    app
}

/// Switches the project's window from its list to its board, as a person does: the bar's switch
/// offers the views its type declares, the board the second.
fn show_board(app: &mut TestApp<TlApp>) {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let bar = all
        .iter()
        .filter_map(|node| node.semantics.as_ref())
        .find(|access| access.role == Role::TabBar && access.name.as_deref() == Some("Project"))
        .unwrap_or_else(|| panic!("the bar offers the views of the project: {snapshot:#?}"));
    let board = bar
        .children
        .iter()
        .find(|tab| {
            tab.role == Role::Tab
                && tab
                    .name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case("board"))
        })
        .and_then(|tab| tab.bounds)
        .unwrap_or_else(|| panic!("the bar offers the board: {snapshot:#?}"));
    app.harness().click_at(
        f64::midpoint(board.x0, board.x1),
        f64::midpoint(board.y0, board.y1),
    );
    settle(app);
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

/// The cards on the board, `(title, [x, y, width, height])`, in the order they are drawn: a
/// card is its row, which says what the card stands for.
fn cards(app: &mut TestApp<TlApp>) -> Vec<(String, [f64; 4])> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "ListRow")
        .filter_map(|node| {
            let name = node.semantics.as_ref()?.name.clone()?;
            Some((name, node.rect))
        })
        .collect()
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
    // A lane the window cuts off is reached where it shows the card: the board scrolls across,
    // and the pointer a person drags with stays in the window.
    let x = (to[0] + to[2] / 2.0).min(f64::from(DESKTOP.0) - 8.0);
    app.harness()
        .drag_at((from[0] + from[2] / 2.0, from[1] + from[3] / 2.0), (x, y));
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

/// The fields a board draws for a person to add from: each lane's entry, where it is drawn.
fn entries(app: &mut TestApp<TlApp>) -> Vec<ui::kurbo::Rect> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "TextInput")
        .filter(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some("New task")
        })
        .map(|node| {
            ui::kurbo::Rect::new(
                node.rect[0],
                node.rect[1],
                node.rect[0] + node.rect[2],
                node.rect[1] + node.rect[3],
            )
        })
        .collect()
}

/// A lane adds at its foot, in its own place: each lane ends in its entry rather than carrying an
/// add in its heading, and what is typed in one makes a card of that lane.
#[test]
fn a_lanes_foot_adds_a_card_to_the_lane_it_is_in() {
    let _settled = ui::motion::testing::settled();
    let mut app = staged(DESKTOP);
    let packing_x = rect_of(&mut app, "Pack the kitchen")[0];
    let booked_x = rect_of(&mut app, "Book the van")[0];
    let bedroom_x = rect_of(&mut app, "Measure the new bedroom")[0];
    // One entry per lane, at the lane's foot under its cards.
    let found: Vec<ui::kurbo::Rect> = entries(&mut app)
        .into_iter()
        .filter(|rect| {
            [packing_x, booked_x, bedroom_x]
                .iter()
                .any(|x| (rect.x0 - x).abs() < 1.0)
        })
        .collect();
    assert_eq!(
        found.len(),
        3,
        "the three lanes each end in their entry: {found:?}"
    );
    for x in [packing_x, booked_x, bedroom_x] {
        let lowest = cards(&mut app)
            .into_iter()
            .filter(|(_, rect)| (rect[0] - x).abs() < 1.0)
            .map(|(_, rect)| rect[1] + rect[3])
            .fold(0.0, f64::max);
        assert!(
            found
                .iter()
                .any(|rect| (rect.x0 - x).abs() < 1.0 && rect.y0 > lowest),
            "the lane at {x} ends in its entry, under its cards: {found:?}"
        );
    }
    // The window's bar carries the one add of the window, above the board: no lane's heading
    // carries one of its own.
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let adds: Vec<[f64; 4]> = all
        .into_iter()
        .filter(|node| node.widget == "Button")
        .filter(|node| {
            node.semantics
                .as_ref()
                .and_then(|semantics| semantics.name.as_deref())
                == Some("New task")
        })
        .map(|node| node.rect)
        .collect();
    assert_eq!(
        adds.len(),
        1,
        "the window has one add, in its bar: {adds:?}"
    );
    let top = cards(&mut app)
        .into_iter()
        .map(|(_, rect)| rect[1])
        .fold(f64::INFINITY, f64::min);
    assert!(
        adds[0][1] + adds[0][3] <= top,
        "the add is the window's, above the board rather than in a lane: {adds:?}"
    );

    // Typed into the Packing lane's entry, the card lands in Packing.
    let entry = found
        .iter()
        .find(|rect| (rect.x0 - packing_x).abs() < 1.0)
        .expect("the Packing lane ends in its entry");
    app.harness().click_at(
        f64::midpoint(entry.x0, entry.x1),
        f64::midpoint(entry.y0, entry.y1),
    );
    app.harness().type_text("Buy tape");
    app.harness().key("Enter");
    settle(&mut app);
    // The card joins the lane in place: the board stays where it is.
    let added = cards(&mut app)
        .into_iter()
        .find(|(name, _)| name.contains("Buy tape"))
        .unwrap_or_else(|| {
            let names: Vec<String> = accessible(&mut app)
                .into_iter()
                .filter_map(|node| node.name)
                .collect();
            panic!("the lane shows the card that was typed: {names:?}")
        });
    assert!(
        (added.1[0] - packing_x).abs() < 1.0,
        "the card is in the lane typed in: {added:?}"
    );
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
    // What a person adds in the list's entry waits in the inbox, and the window shows it there.
    change_marks::choose(&mut app, "Inbox");
    let root = app
        .read(|doc| tilolang_tilo::action::resource_ids(doc, "so.tilo.lang.tasks"))
        .remove(0);
    let tasks = format!("tl:so.tilo.lang.tasks:{root}");
    let add = action_id(&mut app, &tasks, "New task");
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
    settle(&mut app);
    // It checks off Ada's task too.
    let task = published_with(&mut app, &tasks, "Complete");
    let complete = action_id(&mut app, &task, "Complete");
    app.invoke(
        Some(agent.clone()),
        Some(&ambient),
        &task,
        &complete,
        &serde_json::Map::new(),
        false,
    )
    .unwrap();
    // And removes one it added.
    let remove = action_id(&mut app, &tasks, "Delete…");
    let mut which = serde_json::Map::new();
    which.insert(
        "t".to_owned(),
        change_marks::reference("so.tilo.lang.tasks", &mut app, "Book the dentist"),
    );
    app.invoke(Some(agent), Some(&ambient), &tasks, &remove, &which, false)
        .unwrap();
    settle(&mut app);
    let ground = chroma_in(&mut app, [0.0, 0.0, 8.0, 8.0]);
    // Every row the run touched that the inbox still holds is marked: the one it removed
    // stays as a struck row, all at the row's own height, with the agent's face leading the
    // line it says under the title. The one it checked off left the inbox for Done.
    let listed = change_marks::rows(&mut app);
    assert!(
        listed.len() == 2 && listed.iter().all(|(_, _, marked)| *marked),
        "{listed:?}"
    );
    assert_eq!(
        change_marks::removed_rows(&mut app).len(),
        1,
        "the removed task stands as a struck row"
    );
    for (name, rect, _) in &listed {
        assert!(
            (rect[3] - listed[0].1[3]).abs() < 0.5,
            "{name} keeps the row's height"
        );
        // What it says leads the line under the title: the face stands at the row's left, where
        // the words start, and not on the trailing edge.
        let leading = [rect[0], rect[1], rect[2] / 2.0, rect[3]];
        assert!(
            chroma_in(&mut app, leading) > ground.saturating_add(24),
            "{name} shows the agent's face where its words start"
        );
        let trailing = [rect[0] + rect[2] - 24.0, rect[1], 24.0, rect[3]];
        assert!(
            chroma_in(&mut app, trailing) <= ground.saturating_add(24),
            "{name} keeps its trailing edge for the row's own controls"
        );
    }
    // The header counts them, once.
    assert_eq!(marks(&mut app).len(), 1);
    // What the run checked off is read where it went, marked there too.
    change_marks::choose(&mut app, "Done");
    let done = change_marks::rows(&mut app);
    let (_, _, marked) = done
        .iter()
        .find(|(name, _, _)| name == "Water the plants")
        .unwrap_or_else(|| panic!("Done lists the task the run checked off: {done:?}"));
    assert!(marked, "the row it checked off is marked in Done: {done:?}");
    change_marks::choose(&mut app, "Inbox");
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
    use change_marks::{action_id, choose, named, published_with, run, settle};
    use history_window::{after, shoot, times};
    let _settled = ui::motion::testing::settled();
    let mut app =
        ui::framework::testing::HostedApp::open(support::installed("tasks"), "ada").unwrap();
    settle(&mut app);
    // The task a person adds waits in the inbox, which the window shows it in.
    choose(&mut app, "Inbox");
    let root = app
        .read(|doc| tilolang_tilo::action::resource_ids(doc, "so.tilo.lang.tasks"))
        .remove(0);
    let tasks = format!("tl:so.tilo.lang.tasks:{root}");
    let add = action_id(&mut app, &tasks, "New task");
    let mut titled = serde_json::Map::new();
    titled.insert("title".to_owned(), "Call the landlord".into());
    app.invoke(None, None, &tasks, &add, &titled, false)
        .unwrap();
    settle(&mut app);
    // The row opens where it stands, and stays open while the days the task waits at move it
    // out of the inbox, into Today and on to Upcoming.
    let row = named(&mut app, "SwipeRow", "Call the landlord");
    app.harness()
        .click_at(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    settle(&mut app);
    let task = published_with(&mut app, &tasks, "Pick date…");
    let schedule = action_id(&mut app, &task, "Pick date…");
    let due = |day: &str| {
        let mut args = serde_json::Map::new();
        args.insert("due".to_owned(), day.into());
        args
    };
    app.invoke(None, None, &task, &schedule, &due("2026-11-02"), false)
        .unwrap();
    settle(&mut app);
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
        assert_eq!(
            times(&mut app).len(),
            2,
            "the run's entry and Ada's own, each saying when"
        );
        shoot(&mut app, "tasks-history", settle);
        // The task's own fields stand above its history, so the pane scrolls to the history's
        // rows before the run's entry is opened, as a person reaches them.
        app.harness()
            .send(&ui::synthetic_input::cursor_moved(700.0, 400.0));
        settle(&mut app);
        for _ in 0..6 {
            app.harness().scroll(0.0, -1.0);
            settle(&mut app);
        }
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

/// Click the sidebar's item whose accessible name begins with `label` (its count after). A
/// list's heading over a day is named the same way and folds its group rather than choosing
/// the place, so the item clicked is the one the sidebar holds: the tree's.
fn click_item(app: &mut TestApp<TlApp>, label: &str) {
    let all = accessible(app);
    let found = all.iter().find_map(|node| {
        node.name
            .as_deref()
            .filter(|name| name.starts_with(label))
            .filter(|_| node.role == ui::a11y::Role::TreeItem)
            .and(node.bounds)
    });
    let Some(b) = found else {
        let named: Vec<(&str, &ui::a11y::Role)> = all
            .iter()
            .filter_map(|node| Some((node.name.as_deref()?, &node.role)))
            .collect();
        panic!("no sidebar item named `{label}`: {named:?}");
    };
    app.harness()
        .click_at(f64::midpoint(b.x0, b.x1), f64::midpoint(b.y0, b.y1));
    settle(app);
}

#[test]
fn the_home_list_shows_the_inbox_tasks_of_a_busy_week() {
    let _settled = ui::motion::testing::settled();
    // A day into the week the scenario stages: what is due before it is overdue and waits in
    // Today, what is due after it in Upcoming.
    ui::time::testing::with_wall(local(7, 1, (9, 0, 0)), || {
        support::installed("tasks");
        let mut app = TestApp::<TlApp>::capture_at(Some("a busy week"), DESKTOP)
            .expect("the scenario stages");
        settle(&mut app);
        click_item(&mut app, "Inbox");
        let names: Vec<Option<String>> = accessible(&mut app)
            .into_iter()
            .map(|node| node.name)
            .collect();
        // The inbox is what no one has sorted yet: no project, no day.
        assert!(
            shows(&mut app, "Sort the paper mail"),
            "the inbox lists what no day and no project sorts: {names:?}"
        );
        assert!(
            !shows(&mut app, "No tasks here"),
            "the inbox has tasks in it: {names:?}"
        );
        for title in [
            "Call the landlord about the boiler",
            "Book a haircut",
            "Reply to Mia about Sunday lunch",
        ] {
            assert!(
                !shows(&mut app, title),
                "a task due a day waits in Today, not the inbox: `{title}`: {names:?}"
            );
        }
        assert!(
            !shows(&mut app, "Order boxes and tape"),
            "a task filed in a project is not in the inbox: {names:?}"
        );
        // What the inbox leaves out is what Today holds: every task due today or earlier.
        click_item(&mut app, "Today");
        let names: Vec<Option<String>> = accessible(&mut app)
            .into_iter()
            .map(|node| node.name)
            .collect();
        for title in [
            "Call the landlord about the boiler",
            "Book a haircut",
            "Reply to Mia about Sunday lunch",
        ] {
            assert!(shows(&mut app, title), "Today lists `{title}`: {names:?}");
        }
        assert!(
            !shows(&mut app, "Sort the paper mail"),
            "a task with no day waits in the inbox: {names:?}"
        );
        // What is due after today waits in Upcoming, whichever project it is of.
        click_item(&mut app, "Upcoming");
        let names: Vec<Option<String>> = accessible(&mut app)
            .into_iter()
            .map(|node| node.name)
            .collect();
        assert!(
            shows(&mut app, "Order boxes and tape"),
            "Upcoming lists what is due later: {names:?}"
        );
        assert!(
            !shows(&mut app, "Call the landlord about the boiler"),
            "a task due today is not in Upcoming: {names:?}"
        );
    });
}

/// The box of the row the keyboard is on: moving through a list focuses a row without
/// choosing it, so the row the keyboard is on is the focused one.
fn keyboard_row(app: &mut TestApp<TlApp>) -> Option<[f64; 4]> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "SwipeRow")
        .find(|node| {
            node.semantics
                .as_ref()
                .is_some_and(|semantics| semantics.state.focused)
        })
        .map(|node| node.rect)
}

fn open_menu(app: &mut TestApp<TlApp>) -> Option<[f64; 4]> {
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    all.into_iter()
        .filter(|node| node.widget == "ContextMenu")
        .find(|node| {
            node.semantics
                .as_ref()
                .is_some_and(|semantics| semantics.state.expanded == Some(true))
        })
        .map(|node| node.rect)
}

/// The menu the keyboard opens on a row stands against that row: as Menu/Shift+F10 opens it on
/// every platform, right under the row's own edge rather than somewhere in the list.
#[test]
fn a_row_menu_opened_from_the_keyboard_stands_under_that_row() {
    let _settled = ui::motion::testing::settled();
    support::installed("tasks");
    let mut app =
        TestApp::<TlApp>::capture_at(Some("a busy week"), DESKTOP).expect("the scenario stages");
    settle(&mut app);
    // Choosing a place leaves the task the scenario showed open, as a person does.
    click_item(&mut app, "Today");
    // The list takes the keyboard, and Down puts it on a row.
    app.harness().key("Tab");
    settle(&mut app);
    app.harness().key("Down");
    settle(&mut app);
    let row = keyboard_row(&mut app).unwrap_or_else(|| {
        let snapshot = app.harness().inspection();
        let mut all = Vec::new();
        walk(&snapshot.root, &mut all);
        let focused: Vec<_> = all
            .iter()
            .filter(|node| {
                node.semantics
                    .as_ref()
                    .is_some_and(|semantics| semantics.state.focused)
            })
            .map(|node| node.widget.clone())
            .collect();
        panic!("the keyboard is on a row, not on {focused:?}");
    });
    app.harness().key("Shift+F10");
    settle(&mut app);
    let menu = open_menu(&mut app).expect("Shift+F10 opens the focused row's menu");
    let row_bottom = row[1] + row[3];
    // Under the row, one step of air off it, and not at the list's foot.
    assert!(
        menu[1] >= row_bottom - 1.0 && menu[1] - row_bottom <= 8.0,
        "the menu stands under the row it opened from: menu at {menu:?}, row {row:?}"
    );
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
        // Choosing a place leaves the task the scenario showed open.
        click_item(&mut app, "Today");
        // The task the day is about, opened where it stands: its history reads its age.
        click_item(&mut app, "Call the landlord");
        assert!(
            shows(&mut app, "Call the landlord"),
            "today's task is listed"
        );
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

/// What an agent's action looks like to a person watching the window, as the broker shows an
/// agent at work: `action` of the published `resource`, applying, by the run's agent.
fn agent_acting(
    app: &mut ui::framework::testing::HostedApp,
    resource: &str,
    action: &str,
) -> ui::tilo_actions::SemanticInteraction {
    let resources = app.harness().resources();
    let descriptor = ui::tilo_actions::door::survey(&resources)
        .into_iter()
        .find(|descriptor| {
            descriptor.address.resource == resource && descriptor.address.action == action
        })
        .unwrap_or_else(|| panic!("the window publishes {action} of {resource}"));
    let (agent, _) = change_marks::run();
    ui::tilo_actions::SemanticInteraction {
        version: ui::tilo_actions::INTERACTION_VERSION,
        invocation: ui::tilo_actions::InvocationId::new(),
        sequence: 1,
        actor: ui::tilo_actions::InteractionActor {
            id: agent.actor,
            kind: ui::tilo_actions::ActorKind::Agent,
            for_person: None,
        },
        phase: ui::tilo_actions::InteractionPhase::Applying,
        action: ui::tilo_actions::InteractionAction {
            id: descriptor.address.action.clone(),
            label: descriptor.action_label.clone(),
            app: None,
        },
        resource: ui::tilo_actions::ActionResource {
            id: descriptor.address.resource.clone(),
            kind: descriptor.resource_kind.clone(),
            label: descriptor.resource_label,
        },
        anchor: ui::tilo_actions::InteractionAnchor::Resource,
        group: None,
    }
}

/// Shows `event` in the window and lets the agent's mark come to rest, as a frame does.
fn show_acting(
    app: &mut ui::framework::testing::HostedApp,
    event: ui::tilo_actions::SemanticInteraction,
) {
    let (agent, _) = change_marks::run();
    let shown = app.harness().update_presence(|presence| {
        presence.names.insert(
            ui::permissions::UserId(agent.actor.clone()),
            "Helper".into(),
        );
        presence
            .apply_interaction(event)
            .expect("a well-formed interaction applies")
    });
    assert!(shown, "the agent's action shows");
    assert!(
        app.harness()
            .settle_for_capture(ui::time::Duration::from_secs(30)),
        "the agent's mark comes to rest"
    );
    app.harness().poll_frame().expect("a frame is drawn");
    app.harness().relayout();
    let faults = ui::presence::tag_faults(&app.harness().inspection().root);
    assert!(faults.is_empty(), "the agent's tag: {faults:?}");
}

/// The id a task's title is kept under: what an edit of it and a caret in it name.
fn title_field() -> String {
    let ids = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../tl/tasks/app.ids"),
    )
    .expect("the package's ids");
    ids.lines()
        .find_map(|line| line.strip_prefix("Task.title = "))
        .expect("the title's id")
        .trim()
        .to_owned()
}

/// The window's tasks, opened: Ada's own task in her inbox, and the published tasks list.
fn a_task_of_adas(title: &str) -> (ui::framework::testing::HostedApp, String) {
    use change_marks::{action_id, settle};
    let mut app =
        ui::framework::testing::HostedApp::open(support::installed("tasks"), "ada").unwrap();
    settle(&mut app);
    change_marks::choose(&mut app, "Inbox");
    let root = app
        .read(|doc| tilolang_tilo::action::resource_ids(doc, "so.tilo.lang.tasks"))
        .remove(0);
    let tasks = format!("tl:so.tilo.lang.tasks:{root}");
    let add = action_id(&mut app, &tasks, "New task");
    let mut args = serde_json::Map::new();
    args.insert("title".to_owned(), title.into());
    app.invoke(None, None, &tasks, &add, &args, false).unwrap();
    settle(&mut app);
    let task = change_marks::published_with(&mut app, &tasks, "Complete");
    (app, task)
}

/// An agent checking off a task is seen at the check box a person would press: its pointer
/// rests on the row's box, in the agent's colour, and on no other row's.
#[test]
fn an_agent_completing_a_task_is_seen_at_its_check_box() {
    let (mut app, task) = a_task_of_adas("Buy oat milk");
    let complete = change_marks::action_id(&mut app, &task, "Complete");
    let event = agent_acting(&mut app, &task, &complete);
    show_acting(&mut app, event);
    let (_, row, _) = change_marks::rows(&mut app)
        .into_iter()
        .find(|(name, _, _)| name.starts_with("Buy oat milk"))
        .expect("the inbox lists the task");
    let row = ui::kurbo::Rect::new(row[0], row[1], row[0] + row[2], row[1] + row[3]);
    let check = app
        .harness()
        .controls()
        .iter()
        .filter(|shown| shown.role == ui::accesskit::Role::CheckBox)
        .filter_map(|shown| shown.bounds)
        .find(|bounds| row.contains(bounds.center()))
        .expect("the row holds its check box");
    let [marker] = app.harness().drawn_action_markers() else {
        panic!(
            "the frame paints one mark of the agent: {:?}",
            app.harness().drawn_action_markers()
        );
    };
    let [x0, y0, x1, y1] = marker.locus;
    let locus = ui::kurbo::Rect::new(x0, y0, x1, y1);
    assert!(
        row.contains(locus.center()),
        "the mark rests on the task's row: {locus:?} in {row:?}"
    );
    let [x0, y0, x1, y1] = marker.marker;
    let pointer = ui::kurbo::Rect::new(x0, y0, x1, y1);
    assert!(
        pointer.inflate(4.0, 4.0).contains(check.center())
            || check.contains(pointer.origin())
            || locus.contains(check.center()),
        "the agent's pointer rests on the task's check box: {pointer:?}, {check:?}"
    );
    if let Some(dir) = change_marks::shots_dir() {
        app.harness()
            .render_png(&dir.join("tasks-agent-completes.png"));
    }
    // The person's press of the box runs the same action, through the same door.
    app.harness().click_at(check.center().x, check.center().y);
    change_marks::settle(&mut app);
    assert!(
        !change_marks::rows(&mut app)
            .iter()
            .any(|(name, _, _)| name.starts_with("Buy oat milk")),
        "pressing the box completes the task: {:?}",
        change_marks::rows(&mut app)
    );
}

/// An agent renaming a task is seen writing in its title, as a person typing there would be:
/// what it wrote stands in its colour and its caret, named with what it is doing, ends it.
#[test]
fn an_agent_editing_a_title_is_seen_writing_in_it() {
    use change_marks::{action_id, chroma_in, settle, text_field};
    let (mut app, task) = a_task_of_adas("Buy oat milk");
    let (_, row, _) = change_marks::rows(&mut app)
        .into_iter()
        .find(|(name, _, _)| name.starts_with("Buy oat milk"))
        .expect("the inbox lists the task");
    app.harness()
        .click_at(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    settle(&mut app);
    let edit = action_id(&mut app, &task, "Edit a text field");
    let (agent, ambient) = change_marks::run();
    let mut args = serde_json::Map::new();
    args.insert("field".to_owned(), title_field().into());
    args.insert("value".to_owned(), "Buy oat milk and bread".into());
    let event = agent_acting(&mut app, &task, &edit);
    app.invoke(
        Some(agent.clone()),
        Some(&ambient),
        &task,
        &edit,
        &args,
        false,
    )
    .unwrap();
    settle(&mut app);
    show_acting(&mut app, event);
    if let Some(dir) = change_marks::shots_dir() {
        app.harness()
            .render_png(&dir.join("tasks-agent-writes.png"));
    }
    let field = text_field(&mut app, "Buy oat milk and bread");
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let caret = all
        .iter()
        .find(|node| node.path == format!("presence-mark-{}", agent.actor))
        .unwrap_or_else(|| panic!("the agent's caret is drawn in the title"));
    let named = caret
        .semantics
        .as_ref()
        .and_then(|access| access.name.clone())
        .unwrap_or_default();
    assert!(
        named.starts_with("Helper: editing Title"),
        "a reader is told who writes there and what: {named}"
    );
    let [cx, cy, _, ch] = caret.rect;
    assert!(
        cx > field[0] + field[2] / 3.0
            && cx < field[0] + field[2]
            && cy >= field[1] - 1.0
            && cy + ch <= field[1] + field[3] + 1.0,
        "the caret ends what the agent wrote, inside the title: {:?} in {field:?}",
        caret.rect
    );
    // What it wrote stands in its colour beside the words that were there before.
    // Inside the line, clear of the field's edge.
    let line = [cy + 4.0, (ch - 8.0).max(1.0)];
    let kept = chroma_in(&mut app, [field[0] + 12.0, line[0], 24.0, line[1]]);
    let written = chroma_in(&mut app, [cx - 30.0, line[0], 24.0, line[1]]);
    assert!(
        written > kept.saturating_add(8),
        "the words the agent wrote stand in its colour: {written} vs {kept}"
    );
}

/// Someone else writing in a task's title is seen there: their caret, with their name, at the
/// place in the words their own window says they are.
#[test]
fn a_person_writing_in_a_title_is_seen_there() {
    use change_marks::{settle, text_field};
    let (mut app, task) = a_task_of_adas("Buy oat milk");
    let (_, row, _) = change_marks::rows(&mut app)
        .into_iter()
        .find(|(name, _, _)| name.starts_with("Buy oat milk"))
        .expect("the inbox lists the task");
    app.harness()
        .click_at(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    settle(&mut app);
    // Where Bo's window keys the title: the task and the field it is bound to.
    let target = task
        .strip_prefix("tl:so.tilo.lang.tasks:")
        .expect("a tasks resource");
    let key = ui::presence::PresenceKey::Widget {
        path: ["tilolang", target, title_field().as_str()]
            .map(|segment| ui::presence::PathSegment::Field(segment.to_owned()))
            .to_vec(),
    };
    app.harness().update_presence(|presence| {
        let bo = ui::permissions::UserId("bo".into());
        presence.names.insert(bo.clone(), "Bo".into());
        presence.set_caret(&key, bo, "Buy oat".len());
        true
    });
    settle(&mut app);
    let field = text_field(&mut app, "Buy oat milk");
    let snapshot = app.harness().inspection();
    let mut all = Vec::new();
    walk(&snapshot.root, &mut all);
    let caret = all
        .iter()
        .find(|node| node.path == "presence-mark-bo")
        .unwrap_or_else(|| panic!("Bo's caret is drawn in the title"));
    let [cx, cy, _, _] = caret.rect;
    assert!(
        cx > field[0] && cx < field[0] + field[2] * 0.8 && cy >= field[1] - 1.0,
        "Bo's caret stands in the title's words: {:?} in {field:?}",
        caret.rect
    );
    if let Some(dir) = change_marks::shots_dir() {
        app.harness()
            .render_png(&dir.join("tasks-person-writes.png"));
    }
}
