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

/// How much of each placed window's rect on screen the last frame painted:
/// the share of its pixels that differ from the empty canvas beside them
/// (`None` for a window wholly off screen).
/// Layout alone can place every window and draw none of them; this reads
/// the frame the compositor submitted.
fn painted_shares(desktop: &Desktop, state: &Value, groups: &[u64]) -> Vec<(u64, Option<f64>)> {
    let path = desktop
        .state_path()
        .with_file_name(format!("canvas-frame-{}.png", std::process::id()));
    if path.exists() {
        std::fs::remove_file(&path).expect("remove an earlier snapshot");
    }
    let answer = desktop.call(&json!({"op": "snapshot", "path": path}));
    assert!(
        answer.get("err").is_none(),
        "the snapshot was refused: {answer}"
    );
    let frame = image::open(&path)
        .unwrap_or_else(|error| panic!("the snapshot {} decodes: {error}", path.display()))
        .to_rgba8();
    std::fs::remove_file(&path).expect("remove the snapshot");
    let (width, height) = frame.dimensions();
    let canvas_frame = four(&state["frame"]).expect("the canvas has a frame");
    let scale = f64::from(width) / f64::from(SCREEN.0);
    let pixel = |x: f64, y: f64| {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped into the frame first"
        )]
        let at =
            |v: f64, side: u32| (v * scale).clamp(0.0, f64::from(side.saturating_sub(1))) as u32;
        *frame.get_pixel(at(x, width), at(y, height))
    };
    // The empty canvas: the first point of the deck, scanning up from its
    // bottom-right corner, that no window stands over (with a margin for
    // shadows).
    let windows: Vec<[f64; 4]> = state["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| four(&w["screen"]))
        .collect();
    let empty = (0..40)
        .flat_map(|i| (0..40).map(move |j| (i, j)))
        .map(|(i, j)| {
            (
                canvas_frame[0] + canvas_frame[2] - 4.0 - f64::from(i) * canvas_frame[2] / 40.0,
                canvas_frame[1] + canvas_frame[3] - 4.0 - f64::from(j) * canvas_frame[3] / 40.0,
            )
        })
        .find(|(x, y)| {
            windows.iter().all(|w| {
                *x < w[0] - 24.0
                    || *x > w[0] + w[2] + 24.0
                    || *y < w[1] - 24.0
                    || *y > w[1] + w[3] + 24.0
            })
        })
        .expect("some of the canvas is empty");
    let backdrop = pixel(empty.0, empty.1);
    let differs = |p: image::Rgba<u8>| {
        p.0.iter()
            .zip(backdrop.0.iter())
            .take(3)
            .any(|(a, b)| a.abs_diff(*b) > 6)
    };
    groups
        .iter()
        .map(|group| {
            let Some((_, screen)) = window(state, *group) else {
                return (*group, Some(0.0));
            };
            let mut painted = 0_u32;
            let mut seen = 0_u32;
            for i in 1..20 {
                for j in 1..20 {
                    let x = screen[0] + screen[2] * f64::from(i) / 20.0;
                    let y = screen[1] + screen[3] * f64::from(j) / 20.0;
                    if x < canvas_frame[0]
                        || y < canvas_frame[1]
                        || x >= canvas_frame[0] + canvas_frame[2]
                        || y >= canvas_frame[1] + canvas_frame[3]
                    {
                        continue;
                    }
                    seen = seen.checked_add(1).expect("361 samples at most");
                    if differs(pixel(x, y)) {
                        painted = painted.checked_add(1).expect("361 samples at most");
                    }
                }
            }
            (
                *group,
                (seen > 0).then(|| f64::from(painted) / f64::from(seen)),
            )
        })
        .collect()
}

/// Every placed window on screen is drawn, not only laid out: most of the
/// rect it stands in holds its pixels.
#[track_caller]
fn assert_windows_painted(desktop: &Desktop, what: &str, groups: &[u64]) {
    let state = canvas(desktop);
    let shares = painted_shares(desktop, &state, groups);
    assert!(
        shares.iter().any(|(_, share)| share.is_some())
            && shares
                .iter()
                .all(|(_, share)| share.is_none_or(|share| share > 0.5)),
        "{what}: the canvas places its windows but does not draw them: painted share per group {shares:?}\ncanvas: {state}"
    );
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
    assert_windows_painted(&desktop, "on the canvas", &[FIRST, SECOND, THIRD]);
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
    // Enter where no agent can be made for the task (the test session runs
    // no agent runtime) says so at the caret, and keeps what was typed;
    // typing again takes the reason away.
    desktop.key("Enter");
    wait(
        &desktop,
        "the caret says no agent can be asked here",
        || {
            let caret = &canvas(&desktop)["caret"];
            caret["refused"] == json!("agent-quick-blocked-no-runtime")
                && caret["text"] == json!("show me this week's release work")
        },
    );
    desktop.type_text("!");
    wait(&desktop, "typing again takes the reason away", || {
        canvas(&desktop)["caret"]["refused"].is_null()
    });
    desktop.key("Escape");
    wait(&desktop, "Escape puts the caret away", || {
        canvas(&desktop)["caret"].is_null()
    });
    // Enter with nothing typed says what to do, too.
    desktop.click_at(empty.0, empty.1);
    desktop.key("Enter");
    wait(&desktop, "the empty caret says to type first", || {
        canvas(&desktop)["caret"]["refused"] == json!("canvas-ask-nothing-typed")
    });
    desktop.key("Escape");
    // The strips' ribbon and rail never show over a canvas.
    assert_eq!(canvas(&desktop)["strip_indicators"], json!(false));

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
    assert_windows_painted(&desktop, "panned and zoomed", &[FIRST, SECOND, THIRD]);

    // The strips come back, and the canvas finds everything where it was.
    desktop.invoke_action("actions", "workspace.show-strips");
    wait(&desktop, "the strips show, with their ribbon", || {
        let state = canvas(&desktop);
        state["shown"] == json!(false) && state["strip_indicators"] == json!(true)
    });
    desktop.invoke_action("actions", "workspace.show-canvas");
    wait(&desktop, "the canvas shows again", || {
        let state = canvas(&desktop);
        state["shown"] == json!(true) && drawn_at_places(&state)
    });
    assert_windows_painted(
        &desktop,
        "back on the canvas from the strips",
        &[FIRST, SECOND, THIRD],
    );

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

/// A window opened while the canvas shows is placed where the person sees
/// it, and drawn there with every other window: beside the window in front
/// when that is on screen, else wherever is; with no room on screen, this
/// device's lens zooms out to take every window in.
#[test]
fn a_window_opened_on_the_canvas_is_drawn_on_screen_with_the_others() {
    let desktop = Desktop::start(SCREEN, &canvas_tree());
    wait(&desktop, "the canvas shows its windows", || {
        let state = canvas(&desktop);
        state["shown"] == json!(true) && drawn_at_places(&state)
    });
    desktop.invoke_action("actions", "feedback.open");
    let opened = || {
        let state = canvas(&desktop);
        let frame = four(&state["frame"])?;
        let windows = state["windows"].as_array()?;
        let on_screen = |entry: &Value| {
            four(&entry["place"]).is_some()
                && four(&entry["screen"]).is_some_and(|screen| {
                    screen[0] >= frame[0] - 0.5
                        && screen[1] >= frame[1] - 0.5
                        && screen[0] + screen[2] <= frame[0] + frame[2] + 0.5
                        && screen[1] + screen[3] <= frame[1] + frame[3] + 0.5
                })
        };
        let new = windows
            .iter()
            .find(|w| ![FIRST, SECOND, THIRD].contains(&w["group"].as_u64().unwrap_or(FIRST)))?;
        (windows.len() == 4 && windows.iter().all(on_screen)).then(|| new["group"].as_u64())?
    };
    wait(
        &desktop,
        "the opened window and every other is placed wholly on screen",
        || opened().is_some(),
    );
    let group = opened().expect("the opened window");
    assert_windows_painted(
        &desktop,
        "the window opened on the canvas",
        &[FIRST, SECOND, THIRD, group],
    );
}
