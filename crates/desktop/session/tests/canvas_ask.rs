//! Text typed on the canvas that asks (`docs/designs/app-language/shell.md` §8.1,
//! "Asking from the canvas").
//!
//! A line said at the canvas's caret that finds something places it, as it always did
//! (`layout_placing.rs`). A line that is a question, or that names nothing, is the person's
//! send to the agent they keep in the workspace: `session:send-to#send`, the one send a send-to
//! box, an agent and a script run, so it lands in their conversation with the agent exactly as a
//! box's message does. The conversation does not open: the turn the message opens stands where
//! the person asked once Chat holds it (`canvas_point.rs`).
//!
//! Every step here is the production path: the shell's own `place-found` action on
//! `shell:layout`, pointed at and typed with the pointer and keyboard, and the send as the
//! session carries it out, read back from `session:send-to`'s own reading. Chat's process is not
//! part of this session, so the message landing in the conversation is proven where Chat runs (the
//! nested agent story).

#![cfg(target_os = "linux")]

use std::time::Duration;

use tilo_session::{AppSocket, Options, Session};

use crate::common::{CANVAS, PERSON, layout, on_screen, private_home, show_preset, until};

/// A headless session with the shell as the whole screen and the apps a development session seeds,
/// the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        services: crate::common::seeded_apps(),
        asks: Some(tilo_service::asks::channel()),
        app_socket: AppSocket::At(root.join("app_wire.sock")),
        mcp_server: Some(root.join("agent.sock")),
        ..Options::default()
    });
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS).is_some()
    });
    session
}

/// Point at the canvas's free ground and say `text` there, Enter sending it: the person asking,
/// through the shell's own action.
fn ask(session: &mut Session, text: &str) {
    let at = crate::common::free_point(session);
    crate::common::point_and_say(session, at, text);
}

/// A question said at the canvas's caret is the person's send of those words to the workspace's
/// agent: one where they keep none yet, named as the workspace names its agent. Nothing stands on
/// the canvas for it until Chat holds the message, and no conversation opens over the canvas.
///
/// The agent the first ask made is the one every later ask goes to: the workspace's record names it
/// as the person's, so a second question continues the same conversation rather than making a
/// second agent. Both asks are one person's on one canvas, so they are asked of one session: the
/// second is the ask after the first, as it is for the person.
#[test]
fn the_first_ask_on_the_canvas_makes_the_workspaces_agent_and_the_second_goes_to_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let places = layout(&session).places.len();

    // The first ask: the person keeps no agent yet.
    ask(&mut session, "what is on my task list?");

    let first = crate::common::sent(&mut session, 0);
    assert_eq!(first["text"], "what is on my task list?", "{first}");
    let to = first["to"].as_array().expect("whom it went to");
    assert_eq!(to.len(), 1, "one agent is asked: {first}");
    assert_eq!(
        to[0]["kind"], "new-agent",
        "the person kept no agent yet: {first}"
    );
    assert_eq!(to[0]["name"], "Assistant", "{first}");
    assert_eq!(
        layout(&session).places.len(),
        places,
        "the ask stands no place of its own on the canvas"
    );

    // The second ask: the agent the first one made is kept, and asked.
    ask(&mut session, "and what is due today?");

    let second = crate::common::sent(&mut session, 1);
    assert_eq!(second["text"], "and what is due today?", "{second}");
    let asked = second["to"].as_array().expect("whom the second went to");
    assert_eq!(asked.len(), 1, "one agent is asked again: {second}");
    let to = &second["to"][0];
    assert_eq!(to["kind"], "agent", "the agent kept now: {second}");
    assert_eq!(to["name"], "Assistant", "{second}");
    assert!(
        to["id"].as_str().is_some_and(|id| !id.is_empty()),
        "named by its id: {second}"
    );
    assert_eq!(
        layout(&session).places.len(),
        places,
        "the second ask stands no place of its own on the canvas either"
    );

    // Two asks are two sends, each carried out once, and the second leaves the first as it was
    // sent: the agent the first made is named in the second, not written back over the first.
    let reading = session
        .read("session:send-to")
        .expect("the sends' own reading");
    let sends = reading["sent"].as_array().expect("the sends carried out");
    assert_eq!(sends.len(), 2, "two asks are two sends: {reading}");
    assert_eq!(sends[0], first, "the first send reads as it was sent");
    assert_eq!(sends[1], second, "the second is the last carried out");
}

/// A send goes to Chat in the pass that took it. The canvas's ask is taken as the shell is read
/// over the frame just drawn, after the pass's own settle; it still goes out before that pass
/// ends, never waiting for the loop's next wake and the frame that wake draws. After every step of
/// the ask, from the Enter on, no send is waiting.
#[test]
fn an_ask_goes_out_in_the_pass_that_took_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let at = crate::common::free_point(&mut session);
    crate::common::click(&mut session, at);
    until(
        &mut session,
        "the caret where the person pointed",
        |session| on_screen(session, crate::common::CANVAS_CARET).is_some(),
    );
    crate::common::type_text(&mut session, "what is on my task list?");
    let waiting = |session: &mut Session| {
        session.settle_services();
        let reading = session
            .read("session:send-to")
            .expect("the sends' own reading");
        (
            reading["waiting"].as_u64().expect("how many sends wait"),
            reading["sent"].as_array().map_or(0, Vec::len),
        )
    };
    for pressed in [true, false] {
        session
            .compositor()
            .input(tilo_compositor::InputEvent::Key {
                keycode: crate::common::KEY_ENTER,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
        assert_eq!(
            waiting(&mut session).0,
            0,
            "a pass ends with no send waiting"
        );
    }
    let deadline = std::time::Instant::now() + tilo_test_env::hang_bound();
    loop {
        let (waiting, sent) = waiting(&mut session);
        assert_eq!(waiting, 0, "a pass ends with no send waiting");
        if sent == 1 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the ask was never sent"
        );
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// The agent the steps below are attributed to, as an agent's turn names it.
/// The published root of the seeded app `app` (`so.tilo.mail`): what the agent's steps
/// show, and what they stand beside.
fn root_of(session: &Session, app: &str) -> String {
    let prefix = format!("tl:{app}:root-");
    session
        .resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with(&prefix))
        .map_or_else(
            || panic!("{app}'s root is published"),
            |spec| spec.id.as_str().to_owned(),
        )
}

/// The canvas point of the strip `place` stands in, and its workspace.
fn canvas_point(
    session: &Session,
    place: &tilo_session_core::layout::PlaceReading,
) -> (i64, i64, String) {
    let strip = crate::common::strip(session, crate::common::number(&place.strip))
        .expect("the place has its strip");
    (strip.canvas_x, strip.canvas_y, strip.workspace)
}

/// `arguments` placed on the canvas as the agent, and the place showing `record` once it stands.
fn placed_as_agent(
    session: &mut Session,
    record: &str,
    arguments: serde_json::Value,
) -> tilo_session_core::layout::PlaceReading {
    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "place",
            &arguments,
        )
        .unwrap_or_else(|error| panic!("the place runs: {error}"));
    crate::common::wait_for(session, format!("{record} placed"), |session| {
        crate::common::places(session)
            .into_iter()
            .find(|place| place.resource == record)
    })
}

/// An agent's `place` that names no point but something to stand beside stands what it shows in
/// the free space to that place's right, at its line — an agent's turn names the conversation it
/// was asked in, so what it places stands next to that conversation (`shell.md` §8.1). Placing it
/// again brings the place showing it forward rather than opening a second one.
#[test]
fn an_agents_place_without_a_point_stands_beside_what_it_names() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let (inbox_root, notes_root) = (
        root_of(&session, "so.tilo.mail"),
        root_of(&session, "so.tilo.notes"),
    );
    let (inbox_id, notes_id) = (inbox_root.as_str(), notes_root.as_str());
    let inbox = placed_as_agent(
        &mut session,
        inbox_id,
        serde_json::json!({"resource": inbox_id, "x": 240, "y": 160}),
    );
    let beside = serde_json::json!({"resource": notes_id, "beside": inbox_id});
    let answered = session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "place",
            &beside,
        )
        .unwrap_or_else(|error| panic!("the place runs: {error}"));
    let notes = crate::common::wait_for(&mut session, "the notes placed", |session| {
        crate::common::places(session)
            .into_iter()
            .find(|place| place.resource == notes_id)
    });
    // The place answers what it showed by the title it stands by, so the agent's step line names
    // it beside what the place declares it did ("Showed: Notes").
    let ui::host_operation::ResourceOperationStart::Completed(output) = answered else {
        panic!("a place completes at once: {answered:?}");
    };
    assert_eq!(
        output
            .value
            .as_ref()
            .and_then(|value| value.get("title"))
            .and_then(serde_json::Value::as_str),
        Some(notes.title.as_str()),
        "the place says the title it stands by: {output:?}"
    );
    assert!(!notes.title.is_empty(), "the notes' place has a title");

    let (x, y, workspace) = canvas_point(&session, &notes);
    let (anchor_x, anchor_y, anchor_in) = canvas_point(&session, &inbox);
    assert_eq!(workspace, anchor_in, "it stands in the anchor's workspace");
    assert_eq!(y, anchor_y, "it stands at the anchor's line");
    assert!(
        x > anchor_x,
        "it stands to the anchor's right: {x} against {anchor_x}"
    );
    // The step is the agent's: the layout's history keeps it as the agent's, and the person's own
    // steps to take back (`Layout::last_step`) are the person's alone, so none is read here.

    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            "shell:layout",
            "place",
            &beside,
        )
        .unwrap_or_else(|error| panic!("the second place runs: {error}"));
    session.step(Duration::from_millis(1)).expect("a frame");
    let shown = crate::common::places(&session)
        .into_iter()
        .filter(|place| place.resource == notes_id)
        .count();
    assert_eq!(shown, 1, "one place shows it");
}

/// A place moved beside another (`position` with `beside`, no point) stands in the free space next
/// to it: "put my notes next to the inbox". One that names neither a point nor anything to stand
/// beside is refused rather than sent to the canvas' origin.
#[test]
fn a_place_positioned_beside_another_stands_next_to_it() {
    let _settled = tilo_ui_base::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let (inbox_root, notes_root) = (
        root_of(&session, "so.tilo.mail"),
        root_of(&session, "so.tilo.notes"),
    );
    let (inbox_id, notes_id) = (inbox_root.as_str(), notes_root.as_str());
    let inbox = placed_as_agent(
        &mut session,
        inbox_id,
        serde_json::json!({"resource": inbox_id, "x": 240, "y": 160}),
    );
    let note = placed_as_agent(
        &mut session,
        notes_id,
        serde_json::json!({"resource": notes_id, "x": 6000, "y": 4000}),
    );
    let (ix, iy, _) = canvas_point(&session, &inbox);
    let (nx, ny, _) = canvas_point(&session, &note);
    assert!(
        nx >= 6000 && ny >= 4000,
        "the note stands far off first: {nx},{ny}"
    );

    let place = format!("shell:place:{}", note.key);
    let refused = session.invoke_with_actor(
        &crate::common::agent("assistant"),
        &place,
        "position",
        &serde_json::json!({}),
    );
    assert!(refused.is_err(), "no point and nothing beside is refused");
    // Nor is half a point a point, the place itself something to stand beside, or a place that is
    // not there; and a position that was refused moves nothing.
    for (what, arguments) in [
        ("an x without a y", serde_json::json!({"x": 1600})),
        ("a y without an x", serde_json::json!({"y": 0})),
        ("beside itself", serde_json::json!({"beside": place})),
        (
            "beside a place that is not there",
            serde_json::json!({"beside": "shell:place:999999"}),
        ),
    ] {
        let refused = session.invoke_with_actor(
            &crate::common::agent("assistant"),
            &place,
            "position",
            &arguments,
        );
        assert!(refused.is_err(), "{what} is refused: {arguments}");
    }
    session.step(Duration::from_millis(1)).expect("a frame");
    let stayed = crate::common::place(&session, crate::common::number(&note.key))
        .expect("the note's place stands");
    let (sx, sy, _) = canvas_point(&session, &stayed);
    assert_eq!(
        (sx, sy),
        (nx, ny),
        "a refused position leaves the note where it stood"
    );

    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            &place,
            "position",
            &serde_json::json!({"beside": format!("shell:place:{}", inbox.key)}),
        )
        .unwrap_or_else(|error| panic!("the position runs: {error}"));
    let moved = crate::common::wait_for(&mut session, "the note moved", |session| {
        let place = crate::common::places(session)
            .into_iter()
            .find(|place| place.resource == notes_id)?;
        let (x, y, _) = canvas_point(session, &place);
        (x < 6000).then_some((x, y))
    });
    assert_eq!(moved.1, iy, "it stands at the inbox's line: {moved:?}");
    assert!(moved.0 > ix, "to the inbox's right: {moved:?} against {ix}");
}

/// How wide the display of [`a_place_moved_to_a_point_is_drawn_there`] is: wide enough that the
/// point the place is moved to stands on it with the canvas where it opened.
const WIDE: i32 = 2560;

/// A session on a display [`WIDE`] wide, the framed canvas in front, the seeded apps published.
fn wide_session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless_on(
        &[tilo_compositor::OutputConfig::new(
            "HEADLESS-1",
            WIDE,
            crate::common::HEIGHT,
        )],
        Options {
            person: PERSON.to_owned(),
            collab: false,
            services: crate::common::seeded_apps(),
            asks: Some(tilo_service::asks::channel()),
            app_socket: AppSocket::At(root.join("app_wire.sock")),
            mcp_server: Some(root.join("agent.sock")),
            ..Options::default()
        },
    );
    show_preset(&mut session, "so.tilo.shell:frames");
    until(&mut session, "the canvas", |session| {
        session.shell_control(CANVAS).is_some()
    });
    session
}

/// How the place [`a_place_moved_to_a_point_is_drawn_there`] moves came to stand on the canvas.
#[derive(Clone, Copy, Debug)]
enum PlaceOrigin {
    /// The notes' root, placed at a point by an agent's `place`.
    PlacedRecord,
    /// The Notes app's window, opened beside another app's as `session.open_app` opens them.
    AppWindow,
}

/// Move the place `key` to `(1600, 0)` with its own `position`, as the agent, and wait until it is
/// drawn there: its frame as far right of where it was drawn as 1600 is of the strip's point
/// before, and at the canvas' top. `row` is how the place came to stand, which every failure names.
fn moved_and_drawn(session: &mut Session, key: &str, row: PlaceOrigin) {
    let (name, title) = crate::common::wait_for(
        session,
        format_args!("{row:?}: the place's name"),
        |session| {
            crate::common::place(session, crate::common::number(key))
                .map(|place| (place.name, place.title))
                .filter(|(name, _)| !name.is_empty())
        },
    );
    // The place on the canvas: a window's frame by the name every surface calls it, a place of
    // content by its own title on its handle; the one on the canvas, above the dock that lists it.
    let shown = |session: &Session| {
        [&title, &name]
            .into_iter()
            .filter(|said| !said.is_empty())
            .flat_map(|said| session.shell_controls(said))
            .filter(|&rect| {
                crate::common::on_display(rect, WIDE, crate::common::HEIGHT)
                    && rect.1 < f64::from(crate::common::HEIGHT) - 120.0
            })
            .max_by(|a, b| (a.2 * a.3).total_cmp(&(b.2 * b.3)))
    };
    let (x0, y0, _, _) = crate::common::wait_for(
        session,
        format_args!("{row:?}: the place drawn"),
        |session| shown(session),
    );
    let before = crate::common::place(session, crate::common::number(key))
        .unwrap_or_else(|| panic!("{row:?}: the place"));
    let (bx, by, _) = canvas_point(session, &before);

    session
        .invoke_with_actor(
            &crate::common::agent("assistant"),
            &format!("shell:place:{key}"),
            "position",
            &serde_json::json!({"x": 1600, "y": 0}),
        )
        .unwrap_or_else(|error| panic!("{row:?}: the position runs: {error}"));
    let (x, y, _) = crate::common::wait_for(
        session,
        format_args!("{row:?}: the place moved"),
        |session| {
            let place = crate::common::place(session, crate::common::number(key))?;
            let point = canvas_point(session, &place);
            (point.0 == 1600).then_some(point)
        },
    );
    assert_eq!((x, y), (1600, 0), "{row:?}: the layout holds the point");
    #[allow(clippy::cast_precision_loss, reason = "canvas points are small")]
    let (dx, dy) = ((1600 - bx) as f64, (0 - by) as f64);
    crate::common::wait_for(
        session,
        format_args!("{row:?}: the place drawn at its point, {dx},{dy} from {x0},{y0}"),
        |session| {
            let (nx, ny, _, _) = shown(session)?;
            ((nx - x0 - dx).abs() < 2.0 && (ny - y0 - dy).abs() < 2.0).then_some(())
        },
    );
}

/// A place moved to a point on the canvas (`position` with `x` and `y`, as the person's drag and an
/// agent's step move it) is drawn there: its frame stands at that point, as far from where it
/// stood as the point is. So is an app's window opened on the canvas, as `session.open_app` opens
/// one beside the others: each row is a way a place comes to stand, moved the one way.
///
/// Every row has a session of its own, so the place a row moves stands on a canvas no earlier row
/// stood anything on; the rows share only the app store this process built its apps into.
#[test]
fn a_place_moved_to_a_point_is_drawn_there() {
    let _settled = tilo_ui_base::motion::testing::settled();
    for row in [PlaceOrigin::PlacedRecord, PlaceOrigin::AppWindow] {
        let root = private_home();
        let mut session = wide_session(root.path());
        let origin = crate::common::places(&session);
        assert!(
            origin.is_empty(),
            "{row:?}: nothing stands on the canvas the row starts on: {origin:?}"
        );
        let key = match row {
            PlaceOrigin::PlacedRecord => {
                let notes_root = root_of(&session, "so.tilo.notes");
                placed_as_agent(
                    &mut session,
                    &notes_root,
                    serde_json::json!({"resource": notes_root, "x": 240, "y": 160}),
                )
                .key
            }
            PlaceOrigin::AppWindow => {
                for app in ["so.tilo.tasks", "so.tilo.notes"] {
                    session
                        .invoke(
                            tilo_session_core::desktop::verbs::CHROME_RESOURCE,
                            "open-app",
                            serde_json::json!({"app": app})
                                .as_object()
                                .expect("an object"),
                        )
                        .unwrap_or_else(|error| panic!("{row:?}: {app} opens: {error}"));
                }
                crate::common::wait_for(
                    &mut session,
                    format_args!("{row:?}: the Notes window's place"),
                    |session| {
                        crate::common::places(session)
                            .into_iter()
                            .find(|place| place.shows == "so.tilo.notes")
                    },
                )
                .key
            }
        };
        moved_and_drawn(&mut session, &key, row);
    }
}
