//! Chat's channel on the canvas: placing a Chat channel draws Chat's own `page` — its people,
//! its messages, and its composer — in the shell, exactly as a tilolang app's view draws
//! (one-app-model.md §2), and what the person does there reaches Chat's record.
//!
//! Chat declares `Channel` as a resource and `page` as its default view. The channel is placed
//! through the arrangement's published `place`, as an agent or the person does; the shell's
//! runtime draws the channel's reading through Chat's view. Typing in the composer and pressing
//! send posts to the channel through Chat's own `post`; pressing an attachment places what it
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

mod common;

use serde_json::{Map, Value, json};
use tilo_compositor::OutputConfig;
use tilo_session::{ActionPlane, Options, Session};

/// The channel the test places: the demo workspace's `#design-crit`, as Chat's demo publishes
/// it.
const CHANNEL: &str = "chat.workspace.channels:c_crit";
/// A message the demo channel holds, as its page shows it.
const MESSAGE: &str = "Morning! Pushed the new call overlay mock";
/// What the person types into the composer, and what the channel says once it is posted.
const TYPED: &str = "looks good";

/// A session with the shell as the whole screen and Chat's demo conversation published, its
/// test hooks at `root`, the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let program = std::env::current_exe().expect("this test's program");
    let apps = common::seeded_apps();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: common::PERSON.to_owned(),
            shell: true,
            collab: false,
            services: vec![
                Box::new(apps),
                Box::new(tilo_session::dev_rust_views::RustApps::new()),
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
            common::WIDTH,
            common::HEIGHT,
        )],
    )
    .expect("session");
    common::show_preset(&mut session, "Frames");
    common::until(&mut session, "the canvas", |session| {
        common::on_screen(session, common::CANVAS_ENTRY).is_some()
    });
    session
}

/// Place `resource` on the canvas of the workspace in front, through the arrangement's
/// published `place`, as an agent puts it there.
fn place(session: &mut Session, resource: &str) -> u64 {
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
        "width": 720,
        "height": 620,
    })
    .as_object()
    .unwrap()
    .clone();
    session
        .invoke("shell:arrangement", "place", &arguments)
        .unwrap_or_else(|error| panic!("place {resource}: {error:?}"));
    place
}

/// [`place`] of `resource` in a place `height` tall: a page too short to hold its transcript,
/// which has to scroll inside the room the header and composer leave it.
fn place_at(session: &mut Session, resource: &str, width: i32, height: i32) {
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
    let answer = common::control(
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
fn a_chat_channel_placed_on_the_canvas_draws_its_own_page() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    common::until(&mut session, "the channel's page", |session| {
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
    common::until(&mut session, "the page in the light theme", |session| {
        session.shell_says(MESSAGE)
    });
    shot(&mut session, root.path(), "channel-page-light.png");
}

#[test]
fn the_transcript_is_drawn_below_the_header_never_under_it() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    // A page too short for its transcript: the transcript must scroll inside the room the
    // header and the composer leave it, never grow past it and draw under the header. This is
    // the place a person sees a conversation in — smaller than its messages.
    place_at(&mut session, CHANNEL, 520, 300);
    common::until(&mut session, "the channel's page", |session| {
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
    assert!(clip_h > 0.0, "the transcript has room to draw in: {clip_h}");
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
    let mut drawn = 0_u32;
    for row in &rows {
        let Some((_, y, _, h)) = session.shell_drawn_text_bounds(row) else {
            // Scrolled out of the transcript's room: nothing of it paints.
            continue;
        };
        drawn = drawn.saturating_add(1);
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
    assert!(
        drawn > 0,
        "the page draws the transcript's rows: {:?}",
        session.accessible_labels()
    );
}

#[test]
fn typing_in_the_composer_and_pressing_send_posts_to_the_channel() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    common::until(&mut session, "the channel's page", |session| {
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
        common::control(
            &mut session,
            &socket,
            &json!({"op": "click_at", "x": cx, "y": cy})
        ),
        json!({"ok": {}}),
        "the click on the composer is taken"
    );
    assert_eq!(
        common::control(&mut session, &socket, &json!({"op": "type", "text": TYPED})),
        json!({"ok": {}}),
        "the text is typed"
    );
    // What was typed stands in the field before it is sent: the draft the button reads.
    common::until(&mut session, "the typed draft", |session| {
        session.shell_says(TYPED)
    });
    // Pressing send posts what was typed through Chat's own `post`: the channel's reading the
    // place draws then holds a message saying it, which is what the page shows again — the
    // page's own message, not the draft still standing in the field.
    let answer = common::control(
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
    common::until(&mut session, "the message in the channel", |session| {
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
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    // The small demo channel holds one message and a message sharing a sample video, whose
    // row is what the page draws for the attachment: both stand within a place's view.
    const SHARED: &str = "I rewrote my window compositor in Rust and it changed EVERYTHING!!!";
    place(&mut session, "chat.workspace.channels:c_general");
    common::until(&mut session, "the channel's page", |session| {
        session.shell_says(SHARED)
    });
    // The attachment's row opens what it holds: activating it places it beside the page, which
    // is the system's open (shell.md §3), never Chat's.
    let before: Vec<String> = common::places(&session)
        .into_iter()
        .map(|place| place.record)
        .collect();
    common::press(&mut session, SHARED);
    // The press places the video beside the page: the arrangement takes it as one step of the
    // person's, and the page stays where it was.
    for _ in 0..20 {
        session
            .step(std::time::Duration::from_millis(16))
            .expect("step");
        if common::places(&session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:"))
        {
            break;
        }
    }
    assert!(
        common::places(&session)
            .iter()
            .any(|place| place.record.starts_with("tube.video:")),
        "the attachment's press places what it holds; before: {before:?}, after: {:?}; the \
         shell refused {:?}",
        common::places(&session)
            .into_iter()
            .map(|place| place.record)
            .collect::<Vec<_>>(),
        session.shell_refusals()
    );
}
