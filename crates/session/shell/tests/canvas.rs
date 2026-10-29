//! The canvas, driven the way a person drives it in a headless session: the
//! workspace's windows stand where its arrangement places them, a click on
//! empty canvas puts a caret there and typing goes to it, a band selects the
//! windows it crosses, the wheel pans and Ctrl+wheel zooms, a window carried
//! by its bar stays where it is let go, and a restart finds every window
//! where it was (`docs/designs/app-language/canvas.md`).

use serde_json::{Value, json};
use shell::shell::state::{DesktopTree, Tab, WindowGroup, WindowWidth, WorkspaceTree};
use tilo_desktop_model::{Arrangement, ArrangementKind, CanvasPlace};

use crate::support::desktop::Desktop;

const SCREEN: (i32, i32) = (1280, 800);
const FIRST: u64 = 920_001;
const SECOND: u64 = 920_002;
const THIRD: u64 = 920_003;

/// The demo's three windows on a canvas: two side by side at the top, one
/// below the first.
fn canvas_tree() -> WorkspaceTree {
    let mut tree = WorkspaceTree::bootstrap_demo();
    let workspace = &mut tree.desktop.workspaces[0];
    workspace.name = "Canvas desktop".into();
    let strip = &mut workspace.strips[0];
    let original = strip.window_groups.remove(0);
    let mut tabs = original.tabs.into_iter();
    let (notes, files) = (tabs.next().unwrap(), tabs.next().unwrap());
    let rest: Vec<Tab> = tabs.collect();
    let group = |id: u64, tabs: Vec<Tab>| WindowGroup {
        id,
        active_tab: tabs[0].id,
        tabs,
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    strip.window_groups = vec![
        group(FIRST, vec![notes]),
        group(SECOND, vec![files]),
        group(THIRD, rest),
    ];
    let strip_id = strip.id;
    workspace.arrangement = Arrangement {
        kind: ArrangementKind::Canvas,
        places: [
            (FIRST, CanvasPlace::new(40, 40, 400, 300)),
            (SECOND, CanvasPlace::new(480, 40, 400, 300)),
            (THIRD, CanvasPlace::new(40, 380, 400, 260)),
        ]
        .into_iter()
        .collect(),
    };
    let DesktopTree { active_group, .. } = &mut tree.desktop;
    active_group.insert(strip_id, FIRST);
    tree
}

/// Wait for `condition`, saying what the canvas showed when it never held.
#[track_caller]
fn wait(desktop: &Desktop, what: &str, condition: impl FnMut() -> bool) {
    let context = || {
        let dump = desktop.ok(&json!({"op": "state_dump"}));
        let tree = &dump["state"]["workspace_tree"];
        format!(
            "canvas: {}\nonboarding: {}\narrangements: {}\nwindows: {:?}",
            canvas(desktop),
            dump["state"]["chrome"]["input_capture"]["onboarding"],
            tree["workspaces"]
                .as_array()
                .map(|ws| ws
                    .iter()
                    .map(|w| w["arrangement"].clone())
                    .collect::<Vec<_>>())
                .unwrap_or_default()
                .len(),
            desktop.windows().len(),
        ) + &format!(
            "\nfirst workspace arrangement: {}",
            tree["workspaces"][0]["arrangement"]
        )
    };
    desktop.wait_for_with(what, context, condition);
}

fn canvas(desktop: &Desktop) -> Value {
    desktop.chrome()["canvas"].clone()
}

fn four(value: &Value) -> Option<[f64; 4]> {
    let list = value.as_array()?;
    Some([
        list.first()?.as_f64()?,
        list.get(1)?.as_f64()?,
        list.get(2)?.as_f64()?,
        list.get(3)?.as_f64()?,
    ])
}

fn window(state: &Value, group: u64) -> Option<(Option<[f64; 4]>, [f64; 4])> {
    let entry = state["windows"]
        .as_array()?
        .iter()
        .find(|w| w["group"].as_u64() == Some(group))?;
    Some((four(&entry["place"]), four(&entry["screen"])?))
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1.0
}

/// Every window is drawn where its place is, through the lens.
fn drawn_at_places(state: &Value) -> bool {
    let (Some(frame), Some(zoom)) = (four(&state["frame"]), state["zoom"].as_f64()) else {
        return false;
    };
    let Some(origin) = four(&json!([state["origin"][0], state["origin"][1], 0.0, 0.0])) else {
        return false;
    };
    [FIRST, SECOND, THIRD].iter().all(|group| {
        let Some((Some(place), screen)) = window(state, *group) else {
            return false;
        };
        close(screen[0], frame[0] + (place[0] - origin[0]) * zoom)
            && close(screen[1], frame[1] + (place[1] - origin[1]) * zoom)
            && close(screen[2], place[2] * zoom)
            && close(screen[3], place[3] * zoom)
    })
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one session driven through every gesture in order, then restarted"
)]
fn point_at_empty_canvas_and_type_select_pan_zoom_and_keep_places_across_a_restart() {
    let desktop = Desktop::start(SCREEN, &canvas_tree());
    wait(
        &desktop,
        "the canvas shows its windows at their places",
        || {
            let state = canvas(&desktop);
            state["shown"] == json!(true) && drawn_at_places(&state)
        },
    );
    let frame = four(&canvas(&desktop)["frame"]).expect("the canvas has a frame");

    // Free space: a click puts the caret there, and typing goes to it.
    let empty = (frame[0] + 1000.0, frame[1] + 600.0);
    desktop.click_at(empty.0, empty.1);
    wait(&desktop, "a caret where the canvas was clicked", || {
        let caret = &canvas(&desktop)["caret"];
        caret["at"][0].as_f64().is_some_and(|x| close(x, 1000.0))
            && caret["at"][1].as_f64().is_some_and(|y| close(y, 600.0))
    });
    desktop.type_text("show me this week's release work");
    wait(&desktop, "what is typed is at the caret", || {
        canvas(&desktop)["caret"]["text"] == json!("show me this week's release work")
    });
    assert_eq!(
        desktop.chrome()["input_capture"]["chrome"],
        json!(true),
        "the caret has the keyboard"
    );
    desktop.key("Escape");
    wait(&desktop, "Escape puts the caret away", || {
        canvas(&desktop)["caret"].is_null()
    });

    // A band over the first window selects it, and typing about it starts
    // under it.
    desktop.ok(&json!({
        "op": "drag",
        "x_from": frame[0] + 20.0, "y_from": frame[1] + 20.0,
        "x_to": frame[0] + 200.0, "y_to": frame[1] + 200.0,
        "steps": 8,
    }));
    wait(&desktop, "the band selected the window it crossed", || {
        let state = canvas(&desktop);
        state["selection"] == json!([FIRST]) && !state["caret"].is_null()
    });
    desktop.key("Escape");

    // The wheel pans the canvas; every window moves with it.
    desktop.ok(&json!({"op": "pointer_move", "x": empty.0, "y": empty.1}));
    desktop.ok(&json!({"op": "scroll", "dx": 0.0, "dy": -2.0}));
    wait(&desktop, "the wheel panned the canvas", || {
        let state = canvas(&desktop);
        state["origin"][1].as_f64().is_some_and(|y| y.abs() > 1.0) && drawn_at_places(&state)
    });
    // Ctrl and the wheel zoom it about the pointer.
    desktop
        .ok(&json!({"op": "modifiers", "ctrl": true, "shift": false, "alt": false, "logo": false}));
    desktop.ok(&json!({"op": "scroll", "dx": 0.0, "dy": -3.0}));
    desktop.ok(
        &json!({"op": "modifiers", "ctrl": false, "shift": false, "alt": false, "logo": false}),
    );
    wait(&desktop, "Ctrl and the wheel zoomed the canvas", || {
        let state = canvas(&desktop);
        state["zoom"]
            .as_f64()
            .is_some_and(|z| (z - 1.0).abs() > 0.05)
            && drawn_at_places(&state)
    });

    // The strips come back, and the canvas finds everything where it was.
    desktop.invoke_action("actions", "workspace.show-strips");
    wait(&desktop, "the strips show", || {
        canvas(&desktop)["shown"] == json!(false)
    });
    desktop.invoke_action("actions", "workspace.show-canvas");
    wait(&desktop, "the canvas shows again", || {
        let state = canvas(&desktop);
        state["shown"] == json!(true) && drawn_at_places(&state)
    });

    // A window carried by its bar stays where it is let go.
    let (_, second) = window(&canvas(&desktop), SECOND).expect("the second window is drawn");
    let grab = (second[0] + second[2] / 2.0, second[1] + 12.0);
    desktop.ok(&json!({
        "op": "drag",
        "x_from": grab.0, "y_from": grab.1,
        "x_to": grab.0 + 120.0, "y_to": grab.1 + 160.0,
        "steps": 12,
    }));
    let moved = |state: &Value| {
        window(state, SECOND)
            .and_then(|(place, _)| place)
            .is_some_and(|place| place[1] > 41.0)
    };
    wait(
        &desktop,
        "the carried window stays where it was let go",
        || moved(&canvas(&desktop)),
    );
    let carried = window(&canvas(&desktop), SECOND)
        .and_then(|(place, _)| place)
        .expect("the carried window has a place");

    // A restart finds every window where it was.
    let state_path = desktop.state_path();
    wait(&desktop, "the moved place is saved", || {
        shell::shell::persist::load_tree_from(&state_path).is_ok_and(|tree| {
            tree.desktop.workspaces[0]
                .arrangement
                .place_of(SECOND)
                .is_some_and(|place| close(f64::from(place.y), carried[1]))
        })
    });
    let saved = shell::shell::persist::load_tree_from(&state_path).expect("the desktop was kept");
    // The compositor stops; its runtime directory, the saved desktop with it, goes.
    drop(desktop);
    assert!(saved.desktop.workspaces[0].arrangement.is_canvas());
    let restarted = Desktop::start(SCREEN, &saved);
    wait(
        &restarted,
        "after a restart every window is where it was",
        || {
            let state = canvas(&restarted);
            state["shown"] == json!(true)
                && window(&state, SECOND)
                    .and_then(|(place, _)| place)
                    .is_some_and(|place| close(place[0], carried[0]) && close(place[1], carried[1]))
                && window(&state, FIRST)
                    .and_then(|(place, _)| place)
                    .is_some_and(|place| close(place[0], 40.0) && close(place[1], 40.0))
        },
    );
}
