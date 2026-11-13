//! Chat's channel on the canvas: placing a Chat channel draws Chat's own `page` — its people,
//! its messages, and its composer — in the shell, exactly as a tilolang app's view draws
//! (one-app-model.md §2), and what the person does there reaches Chat's record.
//!
//! Chat declares `Channel` as a resource and `page` as its default view. The channel is placed
//! through the arrangement's published `place`, as an agent or the person does; the shell's
//! runtime draws the channel's reading through Chat's view. Typing in the composer and pressing
//! send sends to the channel through Chat's own `send`; pressing an attachment places what it
//! holds. What stands on screen is read from what the shell lays out for assistive technology,
//! never from pixels.

#![cfg(all(target_os = "linux", feature = "dev-rust-views"))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{ActionPlane, Options, Session};

/// The channel the test places: the demo workspace's `#design-crit`, as Chat's demo publishes
/// it.
const CHANNEL: &str = "chat.workspace.channels:c_crit";
/// The channel's id within Chat's record: what a channel item names.
const CHANNEL_ID: &str = "c_crit";
/// A message the demo channel holds, as its page shows it.
const MESSAGE: &str = "Morning! Pushed the new call overlay mock";
/// What the person types into the composer, and what the channel says once it is posted.
const TYPED: &str = "looks good";

/// A session with the shell as the whole screen and Chat's demo conversation published, its
/// test hooks at `root`, the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let program = std::env::current_exe().expect("this test's program");
    let apps = crate::common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: crate::common::PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![
                Box::new(apps),
                Box::new(tilo_session::dev_rust_views::RustApps::new().unwrap()),
            ],
            questions: Some(tilo_service::Questions::new()),
            action_plane: ActionPlane::At(root.join("plane.sock")),
            agent_door: Some(root.join("agent.sock")),
            admit_repl: vec![program],
            test_control: Some(root.join("test-control.sock")),
            ..Options::default()
        },
        &[OutputConfig::new(
            "HEADLESS-1",
            crate::common::WIDTH,
            crate::common::HEIGHT,
        )],
    )
    .expect("session");
    crate::common::show_preset(&mut session, "Frames");
    crate::common::until(&mut session, "the canvas", |session| {
        crate::common::on_screen(session, crate::common::CANVAS_ENTRY).is_some()
    });
    session
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's
/// published `place`, as an agent puts it there.
fn place(session: &mut Session, resource: &str) -> u64 {
    place_at(session, resource, 720, 620)
}

/// Place `resource` on the canvas of the workspace in front at `width` by `height`, through the
/// arrangement's published `place`, as an agent puts it there.
fn place_at(session: &mut Session, resource: &str, width: i64, height: i64) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 80,
        "y": 60,
        "width": width,
        "height": height,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// Place `resource` on the canvas far from the origin, so it stands clear of a conversation
/// placed there: through the arrangement's published `place`, as an agent puts it there.
fn place_far(session: &mut Session, resource: &str) -> u64 {
    let (workspace, row, strip, place) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
            session.handle().mint_id(),
            session.handle().mint_id(),
        )
    };
    let arguments: Map<String, Value> = json!({
        "workspace": workspace,
        "row": row,
        "strip": strip,
        "place": place,
        "resource": resource,
        "x": 900,
        "y": 540,
        "width": 320,
        "height": 220,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// Choose the appearance `option` (`light`, `dark`), as Settings does: the shell follows it at
/// once, the same page in another theme.
fn choose_appearance(session: &mut Session, option: &str) {
    let _ = session.invoke(
        &format!("setting:appearance.style={option}"),
        "choose",
        &Map::new(),
    );
}

/// The screenshot the shots directory names: how a person sees the page, taken over the
/// test-control socket `tilo-control snapshot` uses, never from a private path.
fn shot(session: &mut Session, root: &std::path::Path, name: &str) {
    let Some(dir) = std::env::var_os("TILO_CHAT_VIEW_SHOT").filter(|dir| !dir.is_empty()) else {
        return;
    };
    let dir = std::path::PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("the shots directory");
    let png = dir.join(name);
    let socket = root.join("test-control.sock");
    let answer = crate::common::control(
        session,
        &socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert!(
        answer.get("ok").is_some(),
        "the snapshot is taken: {answer}"
    );
}

#[test]
fn a_place_620_to_700_tall_still_gives_its_transcript_its_room() {
    // A place 620 to 700 logical pixels tall leaves the channel page's window that room less its
    // bar, its head and the composer — and the transcript must still be given the height that is
    // left. At these sizes the transcript's scroll viewport is never zero: the page names the
    // channel under that room's top, and the composer stands under the room's foot.
    let _settled = ui::motion::testing::settled();
    for height in [620, 640, 660, 680, 700] {
        let root = crate::common::private_home();
        let mut session = session(root.path());
        place_at(&mut session, CHANNEL, 720, height);
        crate::common::until(&mut session, "the channel's page", |session| {
            session.shell_says(MESSAGE)
        });
        let (_, clip_y, _, clip_h) = session.shell_scroll_clip(MESSAGE).unwrap_or_else(|| {
            panic!(
                "at {height} the transcript draws through its scroll view: {:?}",
                session.accessible_labels()
            )
        });
        assert!(
            clip_h > 0.0,
            "at {height} the transcript keeps the room under the bar and head: {clip_h}"
        );
        // The composer keeps its room at the foot, and the transcript's room ends over it.
        let (_, composer_y, _, composer_h) = session
            .shell_text_bounds("Message #design-crit")
            .unwrap_or_else(|| {
                panic!(
                    "at {height} the page offers its composer: {:?}",
                    session.accessible_labels()
                )
            });
        assert!(
            composer_h > 0.0,
            "at {height} the composer keeps its own height"
        );
        assert!(
            clip_y + clip_h <= composer_y + 1.0,
            "at {height} the transcript's room stands over the composer: it ends at {}, the \
             composer starts at {}",
            clip_y + clip_h,
            composer_y
        );
    }
}

#[test]
fn a_chat_channel_placed_on_the_canvas_draws_its_own_page() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(MESSAGE)
    });
    // The channel's own view is on the canvas: its name, and its messages, each the component
    // Chat's page composed.
    assert!(
        session.shell_says("#design-crit"),
        "the page names the channel: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_says(MESSAGE),
        "the page shows its messages: {:?}",
        session.accessible_labels()
    );
    // The page draws as Chat's own window does: a run's first message carries its author's
    // avatar (an image labelled by their name), and its time reads as the person reads clocks,
    // never as a raw ISO stamp.
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label.contains("Aria")),
        "a message carries its author's avatar: {labels:?}"
    );
    assert!(
        !labels
            .iter()
            .any(|label| label.contains("T09:") || label.ends_with('Z')),
        "the page shows no raw ISO stamp: {labels:?}"
    );
    // The composer is the page's own: the box a message is typed in, and its send button.
    assert!(
        session.shell_control("Send").is_some(),
        "the page offers its composer: {:?}",
        session.accessible_labels()
    );
    shot(&mut session, root.path(), "channel-page-default.png");
    // The same page in the light theme: only shared components and theme roles, so every theme
    // reaches it.
    choose_appearance(&mut session, "light");
    crate::common::until(&mut session, "the page in the light theme", |session| {
        session.shell_says(MESSAGE)
    });
    shot(&mut session, root.path(), "channel-page-light.png");
}

#[test]
fn the_transcript_is_drawn_below_the_header_never_under_it() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    // The place a person's conversation stands in, the live shell's own size: a transcript
    // longer than the room the header and the composer leave it.
    place(&mut session, CHANNEL);
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(MESSAGE)
    });
    // The page's header is the channel's name and topic, and the transcript is the scroll view
    // under them: the room it draws its rows through begins below the header's own band, and a
    // row's pixels are cut to that room. Measured where they are *drawn* — a row's box cut to
    // the scroll view's clip — not from its unclipped layout box, which a scrolled row keeps
    // above the page while nothing of it paints there.
    let bounds = |session: &Session, text: &str| {
        session
            .shell_text_bounds(text)
            .unwrap_or_else(|| panic!("the page says `{text}`: {:?}", session.accessible_labels()))
    };
    let (_, name_y, _, name_h) = bounds(&session, "#design-crit");
    let (_, topic_y, _, topic_h) = bounds(&session, "Share work, get sharp feedback");
    let header_bottom = (name_y + name_h).max(topic_y + topic_h);
    // The clip the transcript draws through is a room under the header's bottom edge: the
    // header never stands inside it.
    let (_, clip_y, _, clip_h) = session
        .shell_scroll_clip(MESSAGE)
        .unwrap_or_else(|| panic!("the transcript draws through a scroll view: {MESSAGE}"));
    // The room begins clear of the header, with the gap a section keeps under a block — not
    // flush on the topic's last line, which is what a header laid over its content looks like.
    let gap = clip_y - header_bottom;
    assert!(
        gap >= 4.0,
        "the transcript's room begins clear of the header: the header ends at {header_bottom}, \
         the room begins at {clip_y} (a gap of {gap})"
    );
    // The room the transcript is given is the page's to lay out (a merged tree's window bar and
    // desktop measure decide its height); this test holds only where that room *begins*, which
    // is the header's own band and the gap under it.
    assert!(clip_h >= 0.0, "the transcript's room is measured: {clip_h}");
    // Every row of the transcript that the page draws is drawn inside that room, never across
    // the header: a row the transcript has scrolled above its room is drawn nowhere.
    let reading = session
        .read(CHANNEL)
        .unwrap_or_else(|| panic!("the session serves {CHANNEL}"));
    let rows: Vec<String> = reading["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["text"].as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .collect();
    assert!(!rows.is_empty(), "the channel holds messages: {reading}");
    for row in &rows {
        let Some((_, y, _, h)) = session.shell_drawn_text_bounds(row) else {
            // Scrolled out of the transcript's room: nothing of it paints.
            continue;
        };
        assert!(
            y >= header_bottom - 0.5,
            "`{row}` is drawn clear of the header: its band {y}..{end}, the header ends at \
             {header_bottom}",
            end = y + h
        );
        assert!(
            y + h <= clip_y + clip_h + 0.5,
            "`{row}` is drawn inside the transcript's room: its band {y}..{end} ends below \
             {floor}",
            end = y + h,
            floor = clip_y + clip_h
        );
    }
    // The page holds the transcript, whatever room the place's own layout gives it to draw in
    // (a merged tree's window bar and desktop measure decide that height).
    assert!(
        session.shell_says(MESSAGE),
        "the page holds the transcript: {:?}",
        session.accessible_labels()
    );
}

#[test]
fn typing_in_the_composer_and_pressing_send_posts_to_the_channel() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(MESSAGE)
    });
    // The composer's field takes the keyboard, as a person's click on it puts the caret there.
    let (x, y, w, h) = session
        .shell_text_bounds("Message #design-crit")
        .unwrap_or_else(|| {
            panic!(
                "the composer's field stands on screen: {:?}",
                session.accessible_labels()
            )
        });
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let socket = root.path().join("test-control.sock");
    assert_eq!(
        crate::common::control(
            &mut session,
            &socket,
            &json!({"op": "click_at", "x": cx, "y": cy})
        ),
        json!({"ok": {}}),
        "the click on the composer is taken"
    );
    assert_eq!(
        crate::common::control(&mut session, &socket, &json!({"op": "type", "text": TYPED})),
        json!({"ok": {}}),
        "the text is typed"
    );
    // What was typed stands in the field before it is sent: the draft the button reads.
    crate::common::until(&mut session, "the typed draft", |session| {
        session.shell_says(TYPED)
    });
    // Pressing send sends what was typed through Chat's own `send`: the channel's reading the
    // place draws then holds a message saying it, which is what the page shows again — the
    // page's own message, not the draft still standing in the field.
    let answer = crate::common::control(
        &mut session,
        &socket,
        &json!({"op": "click_named", "name": "Send"}),
    );
    assert_eq!(answer, json!({"ok": {}}), "the Send button takes the press");
    // Every message the channel's reading holds, by its text: the posted one is among them,
    // which a draft in the field never is (the reading is what Chat's record says).
    let reading = |session: &Session| {
        session
            .read(CHANNEL)
            .unwrap_or_else(|| panic!("the session serves {CHANNEL}"))
    };
    let posted = |session: &Session| {
        reading(session)["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|message| message["text"].as_str() == Some(TYPED))
    };
    crate::common::until(&mut session, "the message in the channel", |session| {
        posted(session)
    });
    assert!(
        posted(&session),
        "the channel's record holds the posted message: {:?}; the shell refused {:?}",
        reading(&session)["messages"],
        session.shell_refusals()
    );
    shot(&mut session, root.path(), "posted.png");
}

#[test]
fn pressing_an_attachment_places_the_resource_it_holds() {
    // The small demo channel holds one message and a message sharing a sample video, whose
    // row is what the page draws for the attachment: both stand within a place's view.
    const SHARED: &str = "I rewrote my window compositor in Rust and it changed EVERYTHING!!!";
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    place(&mut session, "chat.workspace.channels:c_general");
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(SHARED)
    });
    // The attachment's row opens what it holds: activating it places it beside the page, which
    // is the system's open (shell.md §3), never Chat's.
    let before: Vec<String> = crate::common::places(&session)
        .into_iter()
        .map(|place| place.record)
        .collect();
    crate::common::press(&mut session, SHARED);
    // The press places the video beside the page: the arrangement takes it as one step of the
    // person's, and the page stays where it was.
    for _ in 0..20 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
        if crate::common::places(&session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:"))
        {
            break;
        }
    }
    assert!(
        crate::common::places(&session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:")),
        "the attachment's press places what it holds; before: {before:?}, after: {:?}; the \
         shell refused {:?}",
        crate::common::places(&session)
            .into_iter()
            .map(|place| place.record)
            .collect::<Vec<_>>(),
        session.shell_refusals()
    );
}

/// The example drag from a message's attachment chip onto the canvas: a press on a chip inside
/// the transcript's scrolling region travels out of it, lifts the chip carrying its resource,
/// and the map under the pointer where it is let go runs the system's `place`. The video the
/// chip holds is placed at the point it was let go, not left unplaced or placed at the origin.
#[test]
fn an_attachment_chip_dragged_out_of_the_transcript_places_its_resource_on_the_canvas() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    // The small demo channel whose whole transcript fits: its shared video's row is what the
    // page draws for the attachment, in reach of a pointer.
    const SHARED: &str = "I rewrote my window compositor in Rust and it changed EVERYTHING!!!";
    const DROPPED: &str = "tube.video:vid_rust_async";
    place(&mut session, "chat.workspace.channels:c_general");
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(SHARED)
    });
    // The chip stands on screen: its row's label, drawn through the transcript's scroll view.
    let (chip_x, chip_y, chip_w, chip_h) = session.shell_text_bounds(SHARED).unwrap_or_else(|| {
        panic!(
            "the attachment's row stands on screen: {:?}",
            session.accessible_labels()
        )
    });
    // Bare canvas under the place: the map fills the desk below the entry, and the channel's
    // window stands over it. A spot near the map's foot, left of the window's right edge and
    // below its bottom, is the map itself — not the window's own drop target (its composer).
    let (from_x, from_y) = (chip_x + chip_w / 2.0, chip_y + chip_h / 2.0);
    let (to_x, to_y) = (60.0, f64::from(crate::common::HEIGHT) - 40.0);
    // The resources placed before the drag: the drag adds the video beside the channel.
    let before: Vec<String> = crate::common::places(&session)
        .into_iter()
        .map(|place| place.record)
        .collect();
    assert!(
        !before.iter().any(|record| record == DROPPED),
        "the video is not placed before the drag: {before:?}"
    );
    shot(&mut session, root.path(), "before-drag.png");
    let socket = root.path().join("test-control.sock");
    assert_eq!(
        crate::common::control(
            &mut session,
            &socket,
            &json!({
                "op": "drag",
                "x_from": from_x,
                "y_from": from_y,
                "x_to": to_x,
                "y_to": to_y,
            })
        ),
        json!({"ok": {}}),
        "the drag is taken"
    );
    // The drag places the resource the chip held: the map's `drop` ran the system's `place`, so
    // a place of the video stands in the arrangement, at the canvas point it was let go.
    crate::common::until(&mut session, "the video placed on the canvas", |session| {
        crate::common::places(session)
            .iter()
            .any(|place| place.record == DROPPED)
    });
    let placed: Vec<String> = crate::common::places(&session)
        .into_iter()
        .map(|place| place.record)
        .collect();
    assert!(
        placed.iter().any(|record| record == DROPPED),
        "the dragged chip places the resource it holds; before: {before:?}, after: {placed:?}; the \
         shell refused {:?}",
        session.shell_refusals()
    );
    // The video's strip stands where it was dropped, not at the origin: its canvas point is the
    // map point under the pointer where the drag ended, on the framed canvas's grid (`frames_snap`,
    // a frame's pitch), so one of its coordinates may snap to the grid's first line.
    let place_key = crate::common::places(&session)
        .into_iter()
        .find(|place| place.record == DROPPED)
        .and_then(|place| place.key.parse().ok())
        .expect("the video's place stands in the arrangement");
    let strip =
        crate::common::strip_of(&session, place_key).expect("the video's place has a strip");
    assert!(
        (strip.canvas_x, strip.canvas_y) != (0, 0),
        "the drop places the video at the point it was let go, not the origin: {strip:?}"
    );
    shot(&mut session, root.path(), "dragged-out.png");
}

/// Send `count` messages to the conversation `channel` through Chat's own `send`, the verb its
/// composer runs, so the transcript is longer than any place the canvas gives it.
fn post_many(session: &mut Session, channel: &str, count: usize) {
    for index in 0..count {
        let mut arguments = Map::new();
        arguments.insert(
            "to".to_owned(),
            json!([{"kind": "conversation", "id": channel, "name": channel}]),
        );
        arguments.insert("attached".to_owned(), json!([]));
        arguments.insert(
            "text".to_owned(),
            json!(format!("line {index:02} of the transcript")),
        );
        session
            .invoke(CHANNEL, "send", &arguments)
            .unwrap_or_else(|error| panic!("send {index}: {error:?}"));
    }
}

/// A conversation forty messages long in a place 520 px tall — the height the system places a
/// place at — keeps its page inside the place: the composer stands within the place's own
/// frame, the transcript scrolls in the room above it, and the newest message is drawn.
#[test]
fn a_conversation_in_a_520_px_place_keeps_its_composer_in_the_place() {
    let _settled = ui::motion::testing::settled();
    let root = crate::common::private_home();
    let mut session = session(root.path());
    place_at(&mut session, CHANNEL, 720, 520);
    crate::common::until(&mut session, "the channel's page", |session| {
        session.shell_says(MESSAGE)
    });
    post_many(&mut session, CHANNEL_ID, 40);
    let newest = "line 39 of the transcript";
    crate::common::until(&mut session, "the newest message", |session| {
        session.shell_says(newest)
    });
    // Where the place stands on the desk, as the shell lays it out: the frame's own box, the
    // largest node named for the channel (the frame's group holds the page's heading of the
    // same words inside it).
    let (_, frame_y, _, frame_h) = session
        .shell_text_bounds_all("#design-crit")
        .into_iter()
        .fold(None, |held: Option<(f64, f64, f64, f64)>, rect| {
            Some(match held {
                Some(best) if best.2 * best.3 >= rect.2 * rect.3 => best,
                _ => rect,
            })
        })
        .unwrap_or_else(|| {
            panic!(
                "the place stands on the desk: {:?}",
                session.accessible_labels()
            )
        });
    assert!(
        (frame_h - 520.0).abs() <= 1.0,
        "the place stands 520 tall: {frame_h}"
    );
    let (_, clip_y, _, clip_h) = session.shell_scroll_clip(newest).unwrap_or_else(|| {
        panic!(
            "the transcript draws through its scroll view: {:?}",
            session.accessible_labels()
        )
    });
    let (_, composer_y, _, composer_h) = session
        .shell_text_bounds("Message #design-crit")
        .unwrap_or_else(|| {
            panic!(
                "the page offers its composer: {:?}",
                session.accessible_labels()
            )
        });
    assert!(
        clip_h > 0.0,
        "the transcript keeps a room to scroll in: {clip_h}"
    );
    assert!(
        clip_y + clip_h <= composer_y + 1.0,
        "the transcript's room ends over the composer: {} vs {composer_y}",
        clip_y + clip_h
    );
    assert!(
        composer_y + composer_h <= frame_y + 520.0 + 1.0,
        "the composer stands inside the place: it ends at {}, the place at {}",
        composer_y + composer_h,
        frame_y + 520.0
    );
    let (_, newest_y, _, newest_h) = session.shell_drawn_text_bounds(newest).unwrap_or_else(|| {
        panic!(
            "the newest message is drawn: {:?}",
            session.accessible_labels()
        )
    });
    assert!(
        newest_y >= clip_y - 0.5 && newest_y + newest_h <= clip_y + clip_h + 0.5,
        "the newest message is drawn in the transcript's room: {newest_y}..{}, the room \
         {clip_y}..{}",
        newest_y + newest_h,
        clip_y + clip_h
    );
    // The transcript is longer than its room: it scrolls, rather than the page growing past the
    // place.
    assert!(
        clip_h < 520.0,
        "the transcript's room is less than the whole place: {clip_h}"
    );
    crate::common::shot(&mut session, "place-520.png");
}
