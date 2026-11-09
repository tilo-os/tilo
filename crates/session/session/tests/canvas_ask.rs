//! Text typed on the canvas that asks (`docs/designs/app-language/shell.md` §8.1,
//! "Asking from the canvas").
//!
//! The canvas entry reads "Find something, or ask". A line that finds something
//! places it, as it always did (`arrangement_placing.rs`). A line that is a
//! question, or that names nothing, is not refused: the session posts it as the
//! person into their ask conversation with the agent the workspace keeps, and
//! that conversation stands where they typed, so the person sees what they asked
//! in the place that asked it.
//!
//! Every step here is the production path: the canvas entry's own
//! `place-found` action on `shell:arrangement`, the session's Chat record opened
//! through the same store every app of the workspace opens, and Chat's own
//! `create-channel-with-id` and `post` on it. Nothing here writes Chat's state by
//! hand; the record is read back from the store as a window would read it.
//!
//! What is stubbed: the agent itself. This file proves the ask reaches the
//! conversation — the channel exists, it takes the kept agent as a participant,
//! and it holds the person's message — which is exactly the turn
//! `conversations.rs` derives and the broker delivers.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

mod common;

use std::time::Duration;

use chat::Chat;
use tilo_compositor::InputEvent;
use tilo_service::Questions;
use tilo_session::{ActionPlane, Options, Session};

use common::{
    CANVAS_ENTRY, PERSON, arrangement, on_screen, private_home, show_preset, until, wait_for,
};

/// The agent the workspace keeps for [`PERSON`]: the id its actor works as, and the one the session
/// makes first when the workspace keeps none.
const AGENT: &str = "assistant";

/// Enter, the key that submits the canvas entry.
const KEY_ENTER: u32 = 28;

/// This computer as the person "Quinn Davis": a device of theirs, certified under the person's
/// root key, kept in an identity root of the test's own — so the session seats them by their root
/// key (64 hex characters, the id a real session's messages are authored by), not by a plain name.
/// The kernel's own login name is what [`crate::common::PERSON`] cannot be here: the page must
/// draw their display name, never the key.
fn this_computer(env: &mut tilo_test_env::ScopedEnv, display_name: &str) -> tempfile::TempDir {
    use tilo_user_identity::persist;

    let root = tempfile::tempdir().expect("a directory for this computer's identity");
    let paths = tilo_user_identity::IdentityPaths::under(root.path());
    std::fs::create_dir_all(paths.device_privkey.parent().expect("the key's directory"))
        .expect("this computer's key directory");
    let person = ed25519_dalek::SigningKey::from_bytes(&[0x31; 32]);
    let device = ed25519_dalek::SigningKey::from_bytes(&[0x32; 32]);
    persist::write_device_key(&paths.device_privkey, &device).expect("this computer's device key");
    let mut cert = tilo_user_identity::DeviceCert {
        user_public_key: person.verifying_key().to_bytes(),
        device_public_key: device.verifying_key().to_bytes(),
        device_label: "The laptop".to_owned(),
        capabilities: tilo_user_identity::DeviceCapabilities::member(),
        issued_at_ns: tilo_clock::unix_now_ns(),
        expires_at_ns: None,
        issuer: None,
        signature: [0; 64],
    };
    tilo_user_identity::sign_device_cert(&mut cert, &person).expect("a certified device");
    persist::write_device_cert(&paths.device_cert, &cert).expect("this computer's certificate");
    env.set("TILO_IDENTITY_ROOT", root.path());
    // The name they go by, which the desktop shows them by everywhere: never their key.
    ui::local_identity::install_source(Some(std::sync::Arc::new(
        ui::local_identity::FixedIdentity(ui::local_identity::LocalIdentity {
            group_public_key: cert.user_public_key,
            display_name: display_name.to_owned(),
            avatar_color: [40, 80, 120],
        }),
    )));
    root
}

/// A headless session with the shell as the whole screen and the apps a development session seeds,
/// the framed canvas in front.
fn session(root: &std::path::Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(common::seeded_apps())],
        questions: Some(Questions::new()),
        action_plane: ActionPlane::At(root.join("plane.sock")),
        agent_door: Some(root.join("agent.sock")),
        ..Options::default()
    });
    show_preset(&mut session, "Frames");
    until(&mut session, "the canvas", |session| {
        on_screen(session, CANVAS_ENTRY).is_some()
    });
    session
}

/// The workspace in front, as its id.
fn workspace(session: &Session) -> String {
    session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front")
}

/// Chat's record of the workspace in front, as this session holds it open: the same record the pass
/// derives turns from, so a test reads it without opening a second writer on the store the session
/// writes through.
fn chat(session: &Session) -> Option<Chat> {
    session.chat_of(&workspace(session))
}

/// The channels Chat's record holds, as `(id, participant keys)` .
fn channels(chat: &Chat) -> Vec<(String, Vec<String>)> {
    chat.channels
        .iter()
        .map(|(id, channel)| {
            (
                id.as_str().to_owned(),
                channel
                    .participants
                    .iter()
                    .map(|key| key.0.clone())
                    .collect(),
            )
        })
        .collect()
}

/// Type `text` in the canvas entry and press Enter: the person asking, through the shell's own
/// action.
fn ask(session: &mut Session, text: &str) {
    common::press(session, CANVAS_ENTRY);
    common::type_text(session, text);
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: KEY_ENTER,
                pressed,
            })
            .expect("a key");
        session.step(Duration::from_millis(1)).expect("a frame");
    }
}

/// A question typed into the canvas entry posts into the person's ask conversation with the agent
/// the workspace keeps, and that conversation stands on the canvas where they typed.
#[test]
fn a_question_typed_on_the_canvas_asks_the_kept_agent() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    let before: Vec<String> = chat(&session)
        .map(|chat| channels(&chat).into_iter().map(|(id, _)| id).collect())
        .unwrap_or_default();

    ask(&mut session, "what is on my task list?");

    // A place stands that was not there, showing a channel of Chat's record: the
    // ask conversation. The arrangement is read from the session's own desk,
    // which is never a store read, so this wait never touches the store the
    // session is writing through.
    until(
        &mut session,
        "the ask conversation on the canvas",
        |session| {
            arrangement(session)
                .places
                .into_iter()
                .any(|place| place.record.contains("/channels/"))
        },
    );
    let placed = arrangement(&session)
        .places
        .into_iter()
        .find(|place| place.record.contains("/channels/"))
        .expect("a place shows a channel");
    let channel = placed
        .record
        .rsplit('/')
        .next()
        .expect("the place shows a channel")
        .to_owned();
    assert!(
        !before.contains(&channel),
        "the conversation was not there before the ask: {before:?}"
    );

    // Chat's record holds it, with the agent a participant: the turn the broker
    // delivers to that agent is the person's message in this channel.
    let (_, participants) = channels(&chat(&session).expect("Chat's record"))
        .into_iter()
        .find(|(id, _)| *id == channel)
        .expect("the channel is Chat's");
    assert!(
        participants.contains(&format!("agent:{AGENT}")),
        "the agent takes part in the conversation it is asked in: {participants:?}"
    );
    assert!(
        participants.contains(&format!("person:{PERSON}")),
        "the person the conversation is with is a participant: {participants:?}"
    );
}

/// The ask is the person's own arranging step, attributed to them.
#[test]
fn the_ask_is_the_persons_own_step() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");

    let (who, verb) = wait_for(&mut session, "the arranging step", |session| {
        session.arrangement().borrow_mut().last_step()
    });
    assert_eq!(who, PERSON, "the step is the person's own");
    assert_eq!(verb, "place", "asking is one arranging step");
}

/// What the person asked gives the agent the workspace keeps a turn: the run the broker hands it,
/// read from the same source the broker reads (shell.md §8.1). The turn is the person's message,
/// with the agent's id and the conversation's address, so the agent answering is answering this ask.
#[test]
fn the_ask_gives_the_kept_agent_a_turn() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let workspace = workspace(&session);
    let runs = session.conversation_runs();

    let turns = wait_for(&mut session, "the agent's turn", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        (!turns.is_empty()).then_some(turns)
    });
    assert_eq!(turns.len(), 1, "one ask is one turn: {turns:?}");
    let turn = &turns[0];
    assert_eq!(turn.agent, AGENT, "the agent the ask made is the one");
    assert_eq!(
        turn.instruction, "what is on my task list?",
        "the turn is what the person asked"
    );
    let (address, message) = turn
        .issue
        .rsplit_once("/messages/")
        .expect("the issue names the conversation's message");
    assert!(
        address.starts_with("chat.workspace/channels/"),
        "the turn is in the conversation Chat holds: {address}"
    );
    assert_eq!(
        turn.turn.as_ref().map(|turn| turn.message.as_str()),
        Some(message),
        "the turn answers that message"
    );
    assert!(turn.active, "the person's message waits for its answer");
}

/// A second ask brings the conversation's place forward instead of standing a second place of it
/// (`shell.md` §8.1).
#[test]
fn a_second_ask_brings_the_conversation_forward() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let first = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    let count = arrangement(&session).places.len();

    ask(&mut session, "and what is due today?");

    let places = arrangement(&session).places;
    assert_eq!(
        places.len(),
        count,
        "the second ask added no place: {places:?}"
    );
    assert!(
        places.iter().any(|place| place.key == first.key),
        "the conversation's own place is the one it stands in"
    );
}

/// The conversation the canvas places is usable in a real session, not only in a development one:
/// the channel the ask made is published by the session's own `ChatViewService`, and its `post`
/// verb — the one the page's composer runs — writes a message the reading the place draws then
/// holds. No `dev-rust-views` fixture takes part: this is the path every person's session takes.
#[test]
fn the_conversations_page_posts_through_the_sessions_own_channel() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let place = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    // The channel the ask placed, as the session publishes it (`chat.workspace/channels/<id>`).
    let address = place.record;
    assert!(
        address.starts_with("chat.workspace/channels/"),
        "the place shows the conversation Chat holds: {address}"
    );
    // The service serves the channel's reading before anything is typed: the page a place draws
    // is the ask conversation, holding the person's question.
    let before = session
        .read(&address)
        .unwrap_or_else(|| panic!("the session serves the channel's reading: {address}"));
    assert!(
        before.to_string().contains("what is on my task list?"),
        "the reading holds the person's question: {before}"
    );
    // Post through the page's own verb, as pressing its Send does: the channel's `post`, with the
    // words the composer holds. The session's door routes it to the service that publishes it.
    let mut arguments = serde_json::Map::new();
    let channel = address
        .trim_start_matches("chat.workspace/channels/")
        .to_owned();
    arguments.insert("channel".into(), serde_json::Value::String(channel));
    arguments.insert(
        "text".into(),
        serde_json::Value::String("looks good".into()),
    );
    session
        .invoke(&address, "post", &arguments)
        .unwrap_or_else(|error| panic!("the channel's post runs: {error:?}"));
    // The reading the place draws now holds the message the composer posted: a message of the
    // reading, written by the person seated here, whose id was not there before. This is the
    // thing the page's composer must get right — a message, attributed to the person, that a
    // second read serves — not merely bytes somewhere in the reading.
    let after = session
        .read(&address)
        .unwrap_or_else(|| panic!("the session still serves the channel's reading"));
    let message_ids = |reading: &serde_json::Value| -> Vec<String> {
        reading["messages"]
            .as_array()
            .map(|messages| {
                messages
                    .iter()
                    .filter_map(|message| message["id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let was = message_ids(&before);
    let posted = after["messages"]
        .as_array()
        .and_then(|messages| {
            messages.iter().find(|message| {
                message["text"].as_str() == Some("looks good")
                    && message["author"].as_str() == Some(PERSON)
            })
        })
        .unwrap_or_else(|| {
            panic!("the page holds the person's posted message: {after}");
        });
    let posted_id = posted["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the posted message has an id: {posted}"));
    assert!(
        !was.iter().any(|id| id == posted_id),
        "the posted message is new since the reading before it: {posted_id} in {was:?}"
    );
}

/// The ask places the conversation as the resource Chat publishes: its channel, shown through
/// Chat's own default `page` view. A place that named no view would stand the app's window —
/// a blank frame — instead of the conversation the person asked in (shell.md §8.1, "The view").
#[test]
fn the_ask_places_the_channels_page_not_the_apps_window() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let place = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    assert_eq!(
        place.shown_as, "page",
        "the place shows the channel's own page, drawn by Chat's view: {place:?}"
    );
    assert_eq!(
        place.shows, "chat",
        "the place is Chat's channel, never the app's window: {place:?}"
    );
}

/// The place the ask makes draws Chat's own page, not an empty frame: the words the person
/// typed stand in the shell's laid-out tree, drawn from the reading the session's own
/// `ChatViewService` publishes (shell.md §8.1, "The view")./// typed stand in the shell's laid-out tree, drawn from the reading the session's own
/// `ChatViewService` publishes (shell.md §8.1, "The view").
///
/// The arrangement's own fields are not enough to prove this — `shown_as` and `shows` may name
/// the page while the runtime's `resource(p.record)` lookup finds no reading to draw it, and the
/// frame then shows nothing (a `WindowWell`) behind its pin and close. A reading's key holds its
/// id in one canonical form (the path's document and last item, `chat.workspace/<id>`); the place
/// names it by the path the session publishes it at (`chat.workspace/channels/<id>`), and the
/// lookup must find the same reading by either, as it does now. No development fixture takes
/// part: this is the path every person's session takes.
#[test]
fn the_conversations_place_draws_the_page_from_the_sessions_own_service() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let place = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    until(&mut session, "the page's transcript", |session| {
        session.shell_says("what is on my task list?")
    });
    assert!(
        session.shell_says("what is on my task list?"),
        "the place draws the conversation's page, holding what the person asked: {:?}; the \
         shell says {:?}",
        place.record,
        session.accessible_labels()
    );
}

/// A real session's ask conversation names its people by who they are, never by a key: the
/// person who asked by their display name, the agent that answers by the name the workspace
/// keeps for it. No development fixture takes part — this is the path every person's session
/// takes, with the person seated by their root key's 64 hex characters, as a device is.
#[test]
fn the_conversations_page_names_its_people_never_by_a_key() {
    let _settled = ui::motion::testing::settled();
    let mut env = tilo_test_env::ScopedEnv::new();
    let _identity = this_computer(&mut env, "Quinn Davis");
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let place = wait_for(&mut session, "the conversation placed", |session| {
        arrangement(session)
            .places
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    until(&mut session, "the page's transcript", |session| {
        session.shell_says("what is on my task list?")
    });
    // The person who asked is named by their display name, the one this device shows them by
    // everywhere (the same name the top bar's face wears): never by their routing key.
    until(&mut session, "the person named", |session| {
        session.shell_says("Quinn Davis")
    });
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label.contains("Quinn Davis")),
        "the person's byline is their display name, never their key: {labels:?}; the place \
         shows {:?}",
        place.record
    );
    // No routing key reaches the page: a 64-character hex run is a person's root key, and none
    // stands among the page's people. (The shell's own bar is read separately: this test is the
    // conversation's page, and nothing else on the desk.)
    let hex_run = |text: &str| {
        text.split(|c: char| !c.is_ascii_hexdigit())
            .any(|run| run.len() >= 32)
    };
    let reading = session
        .read(&place.record)
        .unwrap_or_else(|| panic!("the session serves {}", place.record));
    let people = reading["people"].as_array().cloned().unwrap_or_default();
    assert!(!people.is_empty(), "the page names its people: {reading}");
    for person in &people {
        let name = person["name"].as_str().unwrap_or_default();
        let id = person["id"].as_str().unwrap_or_default();
        assert!(
            !name.trim().is_empty() && !hex_run(name) && name != id,
            "a person is named, never shown their key: {person}"
        );
    }
    // Every message's author is one of those named people: the byline never falls back to an id.
    for message in reading["messages"].as_array().cloned().unwrap_or_default() {
        let author = message["author"].as_str().unwrap_or_default();
        assert!(
            people
                .iter()
                .any(|person| person["id"].as_str() == Some(author)),
            "every author is named: {message} in {reading}"
        );
    }
    let person = people.iter().find(|person| person["name"] == "Quinn Davis");
    assert!(
        person.is_some(),
        "the reading names the person who asked: {reading}"
    );
}

/// The turn's reply is Chat's own verb offered from the moment the turn exists: the session's
/// Chat service publishes the summons's message item with Chat's `record-work`, so the kept
/// agent — whose tools are the broker's catalogue — is offered the address its answer goes to
/// before any reply has been written.
///
/// This is the narrowest real seam the missing reply hangs on: with Chat's window closed no
/// `chat.messages` item is published at all, so the agent's `record-work` on the turn's reply was
/// never in its catalogue and the person's ask went unanswered (`conversation.rs`'s "the
/// conversation publishes no `record-work` on the turn's reply"). Nothing here is a scripted
/// model: the item is what the service publishes, and invoking it is Chat's own verb applied to
/// Chat's own record, as the agent that answered.
#[test]
fn the_turn_reply_is_offered_chats_record_work_before_it_exists() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let workspace = workspace(&session);
    let runs = session.conversation_runs();
    let turns = wait_for(&mut session, "the agent's turn", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        (!turns.is_empty()).then_some(turns)
    });
    let turn = &turns[0];
    let (address, message) = turn
        .issue
        .rsplit_once("/messages/")
        .expect("the issue names the conversation's message");
    // The summons's item, as the session's Chat service publishes it — the resource an agent
    // names to record what it did with the message. Its `record-work` is Chat's own declaration
    // (`ChatPart::RECORD_WORK`), its action type Chat's.
    let item = format!("chat.messages/messages/{message}");
    let offered = session
        .service_resources()
        .into_iter()
        .find(|resource| resource.id.as_str() == item)
        .unwrap_or_else(|| panic!("the session publishes the summons's item {item}"));
    let record_work = offered
        .actions
        .iter()
        .find(|action| action.id == "record-work")
        .unwrap_or_else(|| {
            let ids: Vec<&str> = offered
                .actions
                .iter()
                .map(|action| action.id.as_str())
                .collect();
            panic!("the item offers Chat's record-work, not {ids:?}")
        });
    assert!(
        record_work
            .params()
            .iter()
            .any(|param| param.name.as_ref() == "message" && param.required),
        "the item's record-work takes the message whose work it records, as Chat's window's does"
    );
    assert!(
        record_work.bound().is_empty(),
        "it is unbound: the agent names the message, exactly as it does on any message's item"
    );
    // The answer the agent records lands on the reply Chat derives from the summons and the
    // agent — the very address the turn's answer is — written as the agent, before the reply
    // existed to be published. The step runs exactly as the session's own `DoorAsk::Step` runs a
    // step no surface door publishes: through the services, as `agent:<id>`.
    let agent = format!("agent:{}", turn.agent);
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "work".into(),
        serde_json::json!({"state": "doing", "did": "Reading the tasks"}),
    );
    arguments.insert(
        "say".into(),
        serde_json::json!("Looking at your task list now."),
    );
    session
        .invoke_service_step_as(&agent, &item, "record-work", &arguments)
        .unwrap_or_else(|error| panic!("Chat records the work: {error}"));
    let channel = address.rsplit('/').next().unwrap_or_default().to_owned();
    let reply = chat::record::reply_id(
        &tilo_resource::Id::from_raw(message),
        &format!("agent:{}", turn.agent),
    );
    // The reading the page draws: Chat's own `#[view(default)]` over the channel, served by the
    // session — the same reading a place of the conversation shows.
    let page = session
        .read(&format!("chat.workspace/channels/{channel}"))
        .unwrap_or_else(|| panic!("the session serves the channel {channel}"));
    let reply_row = page["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|row| row["id"].as_str() == Some(reply.as_str()))
        })
        .unwrap_or_else(|| panic!("the reply exists: {} in {page}", reply.as_str()));
    assert_eq!(
        reply_row["author"].as_str(),
        Some(agent.as_str()),
        "the reply is the agent's"
    );
    assert_eq!(
        reply_row["text"].as_str(),
        Some("Looking at your task list now."),
        "the reply says the agent's words"
    );
    // The page names the agent the workspace keeps by the name its keeper gave it, "Assistant",
    // never the slug its actor works as ("assistant"): the two are the record's display name and
    // its id, and the byline reads the first.
    let people = page["people"].as_array().cloned().unwrap_or_default();
    let named = people
        .iter()
        .find(|person| person["id"].as_str() == Some(agent.as_str()))
        .unwrap_or_else(|| panic!("the page names the agent {agent}: {page}"));
    assert_eq!(
        named["name"].as_str(),
        Some("Assistant"),
        "the agent's byline is the name its keeper gave it, never its id: {named}"
    );
    // The turn is still going — the reply is `doing`, not finished — so the item keeps its
    // `record-work`: the agent's next record (what it did, and then its words at the end) has the
    // address it works from, exactly as the first did. An item that vanished with the reply's
    // first record is why an ask whose plan took several steps went unanswered halfway.
    let still_offered = session.service_resources().into_iter().any(|resource| {
        resource.id.as_str() == item
            && resource
                .actions
                .iter()
                .any(|action| action.id == "record-work")
    });
    assert!(
        still_offered,
        "the summons keeps its record-work while the turn is going: {item}"
    );
}

/// A turn whose answer failed says so where it happened, and its Retry asks the agent again on
/// the same message: the page shows the failure row with a working Retry, and running it makes
/// the person's one question a turn again — no second message is posted.
///
/// Every step is the production path: Chat's own `record-work` writes the failed end as the
/// agent, the page is Chat's own `#[view(default)]` served by the session, and the Retry is the
/// channel's published `retry` verb — the one the row runs and the REPL calls. No model is
/// started here (nothing is scripted either): this proves the conversation's side of the turn,
/// which is what the row and its Retry are.
#[test]
fn a_failed_turn_offers_a_retry_that_asks_the_agent_again_on_the_same_message() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    let workspace = workspace(&session);
    let runs = session.conversation_runs();
    let turns = wait_for(&mut session, "the agent's turn", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        (!turns.is_empty()).then_some(turns)
    });
    let turn = &turns[0];
    let (address, message) = turn
        .issue
        .rsplit_once("/messages/")
        .expect("the issue names the conversation's message");
    let agent = format!("agent:{}", turn.agent);
    let item = format!("chat.messages/messages/{message}");
    // The run ends without an answer: the provider failed, and its account is the reason.
    let mut failed = serde_json::Map::new();
    failed.insert(
        "work".into(),
        serde_json::json!({"state": "provider_failed", "why": "The model connection closed"}),
    );
    failed.insert(
        "say".into(),
        serde_json::json!("The model connection closed"),
    );
    session
        .invoke_service_step_as(&agent, &item, "record-work", &failed)
        .unwrap_or_else(|error| panic!("Chat records the failed turn: {error}"));
    let channel = address.rsplit('/').next().unwrap_or_default().to_owned();
    let reply = chat::record::reply_id(
        &tilo_resource::Id::from_raw(message),
        &format!("agent:{}", turn.agent),
    );
    // The page the place draws: Chat's own reading of the channel, with the failure row the person
    // sees and the Retry it offers.
    let channel_resource = format!("chat.workspace/channels/{channel}");
    let page = session
        .read(&channel_resource)
        .unwrap_or_else(|| panic!("the session serves the channel {channel}"));
    let reply_row = page["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|row| row["id"].as_str() == Some(reply.as_str()))
        })
        .unwrap_or_else(|| panic!("the reply exists: {} in {page}", reply.as_str()));
    assert_eq!(
        reply_row["work"]["state"].as_str(),
        Some("provider_failed"),
        "the row says the turn failed: {reply_row}"
    );
    assert_eq!(
        reply_row["retry"].as_bool(),
        Some(true),
        "the failed reply is the one a Retry asks again: {reply_row}"
    );
    assert_eq!(
        reply_row["retry_summons"].as_str(),
        Some(message),
        "the Retry names the message the reply answered, never a new one: {reply_row}"
    );
    // The Retry is the channel's own verb, published as the page's Retry runs it.
    let offers_retry = session.service_resources().into_iter().any(|resource| {
        resource.id.as_str() == channel_resource
            && resource.actions.iter().any(|action| action.id == "retry")
    });
    assert!(
        offers_retry,
        "the channel offers its retry: {channel_resource}"
    );
    let before = session.person_messages_in(&channel);
    let mut arguments = serde_json::Map::new();
    arguments.insert("channel".into(), serde_json::json!(channel));
    arguments.insert("reply".into(), serde_json::json!(reply.as_str()));
    arguments.insert("summons".into(), serde_json::json!(message));
    session
        .invoke(&channel_resource, "retry", &arguments)
        .unwrap_or_else(|error| panic!("the retry runs: {error:?}"));
    // Nothing is posted: the person's words stay the one message they wrote.
    assert_eq!(
        session.person_messages_in(&channel),
        before,
        "a retry asks again on the same message, never posting the question twice"
    );
    // The same message is a turn again, with the agent the conversation keeps.
    let again = wait_for(&mut session, "the turn again", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        turns
            .into_iter()
            .find(|turn| turn.issue.ends_with(&format!("/messages/{message}")))
    });
    assert_eq!(
        again.instruction, "what is on my task list?",
        "the turn is the person's own question, asked again"
    );
    // And the page no longer draws the stale Retry: the reply it named was asked again.
    let page = session
        .read(&channel_resource)
        .unwrap_or_else(|| panic!("the session still serves the channel {channel}"));
    let reply_row = page["messages"]
        .as_array()
        .and_then(|messages| {
            messages
                .iter()
                .find(|row| row["id"].as_str() == Some(reply.as_str()))
        })
        .unwrap_or_else(|| panic!("the reply is still there: {page}"));
    assert_eq!(
        reply_row["retry"].as_bool(),
        Some(false),
        "a reply already asked again offers no Retry: {reply_row}"
    );
}

/// A served resource's own `open` is the system's `open`: it stands the resource beside the
/// conversation of the turn that asked, attributed to the agent, and the record is not changed
/// (`docs/designs/app-language/one-app-model.md` §3, "`open(r)` places r with its default view,
/// beside where it was asked, for every app"; shell.md §8, "The canvas composes views; it opens
/// nothing"). This is the `open` every tilolang resource publishes — the one an agent that read
/// the catalogue kept invoking, and was refused — run as the turn's step.
#[test]
fn a_served_resources_open_stands_it_beside_the_conversation_that_asked() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let mut session = session(root.path());
    ask(&mut session, "what is on my task list?");
    // The conversation the ask placed: the place a resource the turn opens stands beside.
    let conversation = wait_for(&mut session, "the ask placed", |session| {
        common::places(session)
            .into_iter()
            .find(|place| place.record.contains("/channels/"))
    });
    let workspace = workspace(&session);
    let runs = session.conversation_runs();
    let turns = wait_for(&mut session, "the agent's turn", |_session| {
        let turns = runs.runs(&workspace).ok()?;
        (!turns.is_empty()).then_some(turns)
    });
    let agent = format!("agent:{}", turns[0].agent);
    // A served tilolang resource the session publishes, with its own `open`: the notes app's
    // root, which the record service keeps.
    let resource = session
        .service_resources()
        .into_iter()
        .find(|spec| spec.id.as_str().starts_with("tl:so.tilo.lang.notes:root-"))
        .map(|spec| spec.id.as_str().to_owned())
        .expect("the notes app's root is published");
    assert!(
        !common::places(&session)
            .iter()
            .any(|place| place.record == resource),
        "the resource is not shown before anyone opens it"
    );
    assert!(
        session.service_resources().into_iter().any(|spec| {
            spec.id.as_str() == resource && spec.actions.iter().any(|action| action.id == "open")
        }),
        "the resource publishes its own `open`"
    );

    // The agent invokes the resource's `open` as the turn's step, beside the conversation.
    session
        .invoke_service_step_beside_as(
            &agent,
            &resource,
            "open",
            &serde_json::Map::new(),
            &conversation.record,
        )
        .unwrap_or_else(|error| panic!("the open runs: {error}"));

    let placed = wait_for(&mut session, "the resource placed", |session| {
        common::places(session)
            .into_iter()
            .find(|place| place.record == resource)
    });
    // It stands through its own primary view, as the system `open` does.
    assert!(
        !placed.shown_as.is_empty(),
        "it stands through the app's own view: {placed:?}"
    );
    assert_eq!(
        placed.shows, "so.tilo.lang.notes",
        "it shows the app whose root it names: {placed:?}"
    );
    // Beside the conversation it was asked in, in the same workspace and at its own line, the
    // place standing to the conversation's right — the free space nearest it — never off at the
    // canvas' origin.
    let strip_of = |place: &tilo_session::arrangement::PlaceReading, session: &Session| {
        common::strip(session, common::number(&place.strip)).expect("the place has its strip")
    };
    let (placed_strip, asked_in) = (
        strip_of(&placed, &session),
        strip_of(&conversation, &session),
    );
    assert_eq!(
        placed_strip.workspace, asked_in.workspace,
        "it stands in the conversation's workspace"
    );
    assert_eq!(
        placed_strip.canvas_y, asked_in.canvas_y,
        "it stands at the conversation's line"
    );
    assert!(
        placed_strip.canvas_x > asked_in.canvas_x,
        "it stands beside the conversation, to its right: {placed_strip:?} against {asked_in:?}"
    );
    // And it is one arranging step of the agent's, not the record's change.
    let (who, verb) = session
        .arrangement()
        .borrow_mut()
        .last_step()
        .expect("a step of the history");
    assert_eq!(who, agent, "attributed to the agent that asked");
    assert_eq!(verb, "place", "opening is one arranging step");
}
