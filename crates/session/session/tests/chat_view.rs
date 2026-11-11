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
fn a_place_620_to_700_tall_still_gives_its_transcript_its_room() {
    // A place 620 to 700 logical pixels tall leaves the channel page's window that room less its
    // bar, its head and the composer — and the transcript must still be given the height that is
    // left. At these sizes the transcript's scroll viewport is never zero: the page names the
    // channel under that room's top, and the composer stands under the room's foot.
    let _settled = ui::motion::testing::settled();
    for height in [620, 640, 660, 680, 700] {
        let root = common::private_home();
        let mut session = session(root.path());
        place_at(&mut session, CHANNEL, 720, height);
        common::until(&mut session, "the channel's page", |session| {
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
    // The place a person's conversation stands in, the live shell's own size: a transcript
    // longer than the room the header and the composer leave it.
    place(&mut session, CHANNEL);
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
fn a_reply_to_a_run_of_a_message_is_stored_quoted_where_the_page_shows_it() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    place(&mut session, CHANNEL);
    common::until(&mut session, "the channel's page", |session| {
        session.shell_says(MESSAGE)
    });
    // The message the person will answer, as the channel's reading holds it.
    let (answered, answered_text) = {
        let reading = session
            .read(CHANNEL)
            .unwrap_or_else(|| panic!("the session serves {CHANNEL}"));
        let message = reading["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|message| {
                message["text"]
                    .as_str()
                    .is_some_and(|text| text.starts_with(MESSAGE))
            })
            .expect("the demo message is in the channel");
        (
            message["id"].as_str().expect("its id").to_owned(),
            message["text"].as_str().expect("its text").to_owned(),
        )
    };
    // The run the reply answers: bytes chosen in the answered message, a slice of it rather than
    // the whole.
    let chosen = "Pushed the new call";
    let start = answered_text
        .find(chosen)
        .expect("the words are in the message");
    let end = start + chosen.len();
    // Post the reply through the channel's own `post`, the verb a place's composer runs and a
    // press sends, carrying the message it answers and the run it quotes (`start:end` bytes).
    let item = session
        .service_resources()
        .into_iter()
        .find(|spec| {
            spec.id.as_str().ends_with(&format!(":{CHANNEL_ID}"))
                || spec
                    .id
                    .as_str()
                    .ends_with(&format!("/channels/{CHANNEL_ID}"))
        })
        .filter(|spec| spec.actions.iter().any(|action| action.id == "post"))
        .map(|spec| spec.id.as_str().to_owned())
        .unwrap_or_else(|| {
            panic!(
                "the channel offers post, which its composer runs: {:?}",
                session
                    .service_resources()
                    .into_iter()
                    .map(|spec| spec.id.as_str().to_owned())
                    .collect::<Vec<_>>()
            )
        });
    let mut arguments = Map::new();
    arguments.insert("channel".to_owned(), json!(CHANNEL_ID));
    arguments.insert("text".to_owned(), json!(TYPED));
    arguments.insert("parent".to_owned(), json!(answered));
    arguments.insert("span".to_owned(), json!(format!("{start}:{end}")));
    session
        .invoke(&item, "post", &arguments)
        .unwrap_or_else(|error| panic!("the reply posts: {error:?}"));
    // The channel's live reading now holds a reply answering the message, whose quote is the
    // run: its words are those of the answered message between the offsets it kept, so pressing
    // the quote opens on exactly what it shows.
    let mut found: Option<Value> = None;
    common::until(&mut session, "the quoted reply in the channel", |session| {
        let Some(reading) = session.read(CHANNEL) else {
            return false;
        };
        found = reading["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|message| message["text"].as_str() == Some(TYPED))
            .and_then(|message| {
                let reply_to = message.get("reply_to")?.clone();
                (!reply_to.is_null()).then_some(reply_to)
            });
        found.is_some()
    });
    let quoted = found.expect("the reply carries the run it quotes");
    assert_eq!(
        quoted["excerpt"].as_str(),
        Some(chosen),
        "the quote draws the words the person chose"
    );
    // And the page the place draws carries the quote over the reply: the run, drawn where the
    // reply stands, so the person reads the words their answer was about.
    assert!(
        session.shell_says(chosen),
        "the page draws the quoted run over the reply: {:?}",
        session.accessible_labels()
    );
    shot(&mut session, root.path(), "span-reply.png");
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

/// The workspace in front, by its id: what the runs and Chat's record are read under.
fn front_workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front")
}

#[test]
fn a_resource_dropped_on_the_composer_rides_with_the_message_and_the_agents_turn_names_it() {
    // A real session, with the workspace's seeded tilolang apps: no development fixture stands
    // between the drop and Chat's record. A question typed on the canvas asks the workspace's
    // agent, and the conversation it opens is served by the session's own `ChatViewService` —
    // Chat's `page` drawn over the record. A resource dropped on that composer — here the root
    // of Tasks, which the workspace publishes by its address
    // (`tl:so.tilo.lang.tasks:root-…`) — rides with the message the person sends, named in both
    // the draft's chip and the message's by the title its app publishes.
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the canvas", |session| {
        common::on_screen(session, common::CANVAS_ENTRY).is_some()
    });
    // The person asks the workspace's agent a question: the session opens the conversation and
    // places it on the canvas.
    common::press(&mut session, common::CANVAS_ENTRY);
    common::type_text(&mut session, "what is left?");
    for pressed in [true, false] {
        session
            .compositor()
            .input(tilo_compositor::InputEvent::Key {
                keycode: 28,
                pressed,
            })
            .expect("a key");
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
    }
    let placed = common::wait_for(&mut session, "the conversation placed", |session| {
        common::places(session)
            .into_iter()
            .find(|place| place.record.starts_with("chat.workspace/channels/"))
    });
    let address = placed.record;
    let channel = address
        .trim_start_matches("chat.workspace/channels/")
        .to_owned();
    // The channel's own verb a drop runs: `attach`, published by the service on the channel's
    // item, taking the channel and the resource the drag carried.
    let item = session
        .service_resources()
        .into_iter()
        .find(|spec| {
            spec.id.as_str() == address && spec.actions.iter().any(|action| action.id == "attach")
        })
        .map(|spec| spec.id.as_str().to_owned())
        .unwrap_or_else(|| {
            panic!(
                "the channel offers attach, which a drop on the composer runs: {:?}",
                session
                    .service_resources()
                    .into_iter()
                    .map(|spec| spec.id.as_str().to_owned())
                    .collect::<Vec<_>>()
            )
        });
    // The resource dropped on the composer: the root of Tasks, which the workspace publishes by
    // its address, with the title its app publishes it under.
    let (dropped, title) = {
        let published = session
            .service_resources()
            .into_iter()
            .find(|spec| {
                spec.kind == "tl.so.tilo.lang.tasks.Tasks"
                    && spec.id.as_str().starts_with("tl:so.tilo.lang.tasks:")
            })
            .unwrap_or_else(|| {
                panic!(
                    "the workspace publishes Tasks' root: {:?}",
                    session
                        .service_resources()
                        .into_iter()
                        .map(|spec| spec.id.as_str().to_owned())
                        .collect::<Vec<_>>()
                )
            });
        (published.id.as_str().to_owned(), published.label.clone())
    };
    let mut arguments = serde_json::Map::new();
    arguments.insert("channel".into(), json!(channel));
    arguments.insert("resource".into(), json!(dropped));
    session
        .invoke(&item, "attach", &arguments)
        .unwrap_or_else(|error| panic!("the drop on the composer attaches: {error:?}"));
    // The draft's chip names the resource by the title its app publishes, never the address the
    // drag carried or a step of it (`root-fjqi4yze`): the chip a person reads over the composer.
    common::until(&mut session, "the draft's chip", |session| {
        session
            .read(&address)
            .and_then(|reading| {
                reading["draft"]["label"]
                    .as_str()
                    .filter(|label| !label.is_empty())
                    .map(str::to_owned)
            })
            .is_some()
    });
    let reading = session
        .read(&address)
        .unwrap_or_else(|| panic!("the session serves the channel's reading: {address}"));
    assert_eq!(
        reading["draft"]["label"].as_str(),
        Some(title.as_str()),
        "the draft's chip says the resource's title: {reading}"
    );
    assert_eq!(
        reading["draft"]["resource"]["id"].as_str(),
        Some(dropped.as_str()),
        "the chip holds the address it was dropped by: {reading}"
    );
    // The person types their words and sends, through the channel's own `post`: the verb the
    // composer's field and its Send button both run, routed by the session's door to the service
    // that publishes it.
    let mut post_arguments = serde_json::Map::new();
    post_arguments.insert("channel".into(), json!(channel));
    post_arguments.insert("text".into(), json!(TYPED));
    session
        .invoke(&address, "post", &post_arguments)
        .unwrap_or_else(|error| panic!("the channel's post runs: {error:?}"));
    let posted = |session: &Session| {
        session.read(&address).and_then(|reading| {
            reading["messages"].as_array().and_then(|messages| {
                messages
                    .iter()
                    .find(|message| message["text"].as_str() == Some(TYPED))
                    .cloned()
            })
        })
    };
    common::until(&mut session, "the posted message", |session| {
        posted(session).is_some()
    });
    let message = posted(&session).unwrap_or_else(|| {
        panic!(
            "the channel holds the sent message: {:?}; the shell refused {:?}",
            session.read(&address),
            session.shell_refusals()
        )
    });
    // The message carries the dropped resource as its attachment, named by the title its app
    // publishes: the chip the page draws beside the person's words.
    let sent_attachment = message["attachments"]
        .as_array()
        .and_then(|attachments| {
            attachments.iter().find(|attachment| {
                attachment["resource"]["id"].as_str() == Some(dropped.as_str())
                    && attachment["kind"].as_str() == Some("resource")
            })
        })
        .unwrap_or_else(|| panic!("the message carries the resource that was dropped: {message}"))
        .clone();
    assert_eq!(
        sent_attachment["label"].as_str(),
        Some(title.as_str()),
        "the message's chip says the resource's title, never its id: {message}"
    );
    // The agent's turn names it by that title: an agent answering this message is asked what
    // the person wrote and told which resource they handed it. The instruction a turn is asked
    // with is built from the message's own words and attachments (`instruction_of`), which is
    // what the pass hands the agent's run — read here from the page's attachment, which is the
    // record's own reference drawn.
    let sent = tilo_reference::Attached::resource(
        tilo_reference::ResourceAt {
            app: dropped
                .strip_prefix("tl:")
                .and_then(|rest| rest.split_once(':'))
                .map_or_else(|| "chat".to_owned(), |(app, _)| app.to_owned()),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                sent_attachment["resource"]["ty"]
                    .as_str()
                    .unwrap_or_default(),
                dropped.clone(),
                title.clone(),
            ),
        },
        None,
        title.clone(),
    );
    let instruction = tilo_session::conversations::instruction_of(&chat::record::Message {
        text: message["text"].as_str().unwrap_or_default().to_owned(),
        attachments: vec![sent],
        ..chat::record::Message::default()
    });
    assert!(
        instruction.contains(&title),
        "the turn names the resource by its title: {instruction}"
    );
    assert!(
        instruction.contains(&dropped),
        "the turn says where the resource is too: {instruction}"
    );
    assert!(
        instruction.starts_with(TYPED),
        "the turn asks what the person wrote first: {instruction}"
    );
}

/// The example drag from a message's attachment chip onto the canvas: a press on a chip inside
/// the transcript's scrolling region travels out of it, lifts the chip carrying its resource,
/// and the map under the pointer where it is let go runs the system's `place`. The video the
/// chip holds is placed at the point it was let go, not left unplaced or placed at the origin.
#[test]
fn an_attachment_chip_dragged_out_of_the_transcript_places_its_resource_on_the_canvas() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = session(root.path());
    // The small demo channel whose whole transcript fits: its shared video's row is what the
    // page draws for the attachment, in reach of a pointer.
    const SHARED: &str = "I rewrote my window compositor in Rust and it changed EVERYTHING!!!";
    const DROPPED: &str = "tube.video:vid_rust_async";
    place(&mut session, "chat.workspace.channels:c_general");
    common::until(&mut session, "the channel's page", |session| {
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
    let (to_x, to_y) = (60.0, f64::from(common::HEIGHT) - 40.0);
    // The resources placed before the drag: the drag adds the video beside the channel.
    let before: Vec<String> = common::places(&session)
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
        common::control(
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
    common::until(&mut session, "the video placed on the canvas", |session| {
        common::places(session)
            .iter()
            .any(|place| place.record == DROPPED)
    });
    let placed: Vec<String> = common::places(&session)
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
    // map point under the pointer where the drag ended.
    let place_key = common::places(&session)
        .into_iter()
        .find(|place| place.record == DROPPED)
        .and_then(|place| place.key.parse().ok())
        .expect("the video's place stands in the arrangement");
    let strip = common::strip_of(&session, place_key).expect("the video's place has a strip");
    assert!(
        strip.canvas_x > 0 && strip.canvas_y > 0,
        "the drop places the video at the point it was let go, not the origin: {strip:?}"
    );
    shot(&mut session, root.path(), "dragged-out.png");
}

/// Press Enter on the keyboard, as the person does.
fn press_enter(session: &mut Session) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(tilo_compositor::InputEvent::Key {
                keycode: 28,
                pressed,
            })
            .expect("a key");
        session
            .step(std::time::Duration::from_millis(1))
            .expect("a frame");
    }
}

/// The reproduction of the live composer, as a person met it: a real session whose conversation
/// was placed by the canvas's own ask (so the page is drawn from the session's `ChatViewService`
/// over Chat's record, never the development fixture), real typed keys into the composer, and
/// Enter. What was typed draws once — erasing it returns the field to the empty field it was —
/// and Enter posts the message into the channel's own record.
#[test]
fn typing_in_the_ask_conversations_composer_posts_on_enter_and_draws_the_text_once() {
    let _settled = ui::motion::testing::settled();
    let root = common::private_home();
    let mut session = common::seeded_session(root.path());
    common::until(&mut session, "the canvas", |session| {
        common::on_screen(session, common::CANVAS_ENTRY).is_some()
    });
    common::press(&mut session, common::CANVAS_ENTRY);
    common::type_text(&mut session, "what is left?");
    press_enter(&mut session);
    let address = common::wait_for(&mut session, "the conversation placed", |session| {
        common::places(session)
            .into_iter()
            .find(|place| place.record.starts_with("chat.workspace/channels/"))
            .map(|place| place.record)
    });
    let name = common::wait_for(&mut session, "the conversation's reading", |session| {
        session
            .read(&address)
            .and_then(|reading| reading["name"].as_str().map(str::to_owned))
    });
    // The composer's field takes the keyboard, as a person's click on it does.
    let placeholder = format!("Message #{name}");
    let (x, y, w, h) = common::wait_for(&mut session, "the composer's field", |session| {
        session.shell_text_bounds(&placeholder)
    });
    let socket = root.path().join("test-control.sock");
    assert_eq!(
        common::control(
            &mut session,
            &socket,
            &json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0})
        ),
        json!({"ok": {}}),
        "the click on the composer is taken"
    );
    let empty = common::shot(&mut session, "ask-composer-empty.png");
    common::type_text(&mut session, "hello there");
    common::until(&mut session, "the typed draft", |session| {
        session.shell_says("hello there")
    });
    let typed = common::shot(&mut session, "ask-composer-typed.png");
    // Erase it again: the field shows the empty field it showed before, never the remains of
    // the erased glyphs drawn over.
    for _ in 0.."hello there".len() {
        for pressed in [true, false] {
            session
                .compositor()
                .input(tilo_compositor::InputEvent::Key {
                    keycode: 14,
                    pressed,
                })
                .expect("backspace");
            session
                .step(std::time::Duration::from_millis(1))
                .expect("a frame");
        }
    }
    common::until(&mut session, "the field empty again", |session| {
        !session.shell_says("hello there")
    });
    let erased = common::shot(&mut session, "ask-composer-erased.png");
    // The field draws what was typed once: the screen after erasing is the screen before
    // typing, up to the caret's blink.
    let changed = common::changed(&empty, &erased);
    assert!(
        changed < 600,
        "the field draws the text once: {changed} pixels still differ after erasing it"
    );
    assert_ne!(
        common::changed(&empty, &typed),
        0,
        "the typed text is drawn at all"
    );
    // The words sent: typing them again and pressing Enter posts the message through the
    // channel's own `post`, which the session's record holds.
    common::type_text(&mut session, "hello there");
    common::until(&mut session, "the draft again", |session| {
        session.shell_says("hello there")
    });
    press_enter(&mut session);
    let posted = |session: &Session| {
        session.read(&address).and_then(|reading| {
            reading["messages"]
                .as_array()
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|message| message["text"].as_str() == Some("hello there"))
                })
                .then_some(())
        })
    };
    common::until(&mut session, "the message in the channel", |session| {
        posted(session).is_some()
    });
    assert!(
        posted(&session).is_some(),
        "Enter posts what the field holds: {:?}; the shell refused {:?}",
        session.read(&address),
        session.shell_refusals()
    );
    // The field let go of the message it sent: it draws its prompt again, and what the person
    // types next is a new message, never the sent words with more typed after them — the stale
    // draft the live composer kept when its send was refused.
    common::until(&mut session, "the field empty after sending", |session| {
        session.shell_text_bounds(&placeholder).is_some()
    });
    common::shot(&mut session, "ask-composer-posted.png");
    common::type_text(&mut session, "and then");
    // The field shows its prompt no longer (the empty field's prompt is gone), and it says what
    // was typed into it now: the draft is the person's new words alone.
    common::until(
        &mut session,
        "the second draft is the new words",
        |session| {
            session.shell_text_bounds(&placeholder).is_none() && session.shell_says("and then")
        },
    );
    press_enter(&mut session);
    let second = |session: &Session| {
        session.read(&address).and_then(|reading| {
            reading["messages"]
                .as_array()
                .is_some_and(|messages| {
                    messages
                        .iter()
                        .any(|message| message["text"].as_str() == Some("and then"))
                })
                .then_some(())
        })
    };
    common::until(
        &mut session,
        "the second message in the channel",
        |session| second(session).is_some(),
    );
    // The second message is the new words alone: no message holds the sent words with more
    // after them, which is what typing into a stale draft would post.
    let reading = session.read(&address).unwrap_or_default();
    let stale = reading["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["text"]
                .as_str()
                .is_some_and(|text| text.contains("hello there") && text.contains("and then"))
        })
    });
    assert!(
        !stale,
        "the sent words never ride with the next message: {reading}"
    );
    // The field let go of the second message too.
    common::until(&mut session, "the field empty again", |session| {
        session.shell_text_bounds(&placeholder).is_some()
    });
    common::shot(&mut session, "ask-composer-second.png");
}
