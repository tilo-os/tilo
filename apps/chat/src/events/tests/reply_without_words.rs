//! The reply a turn leaves when what its person wrote next takes the turn along. It ends
//! complete, without a word and without work, and a conversation shows nothing of it: no row
//! among the messages, no thread under the message it answered, nothing read out.

use super::*;
use crate::agent_stream::AgentFinish;
use crate::model::{AgentStep, AgentStepOutcome, AgentWork, AgentWorkState};

/// The person's agent.
fn agent() -> ui::tilo_resource::MemberId {
    crate::resource::demo_agent()
}

/// Work that ended complete, as a turn on a server ends the reply it leaves.
fn complete() -> AgentWork {
    AgentWork::new(
        AgentWorkState::ProviderFinished {
            outcome: AgentFinish::Complete,
        },
        crate::demo::NOW,
    )
}

/// The agent notes on its reply to `message` where its work stands, through Chat's own action
/// as an agent on a server does, with the reply's words where it has some.
fn notes(doc: &mut crate::testing::Workspace, message: &str, work: &AgentWork, say: Option<&str>) {
    doc.apply_part_as(
        &agent(),
        crate::resource::work_noted(
            &ui::tilo_resource::Id::from_raw(message),
            crate::testing::work_update(work),
            say.map(str::to_owned),
        ),
    )
    .unwrap();
}

/// The agent's reply to `message`.
fn reply_to(message: &str) -> String {
    crate::chat::reply_id(&ui::tilo_resource::Id::from_raw(message), &agent()).to_string()
}

/// Read what the conversation holds now, and draw it.
fn draw(app: &mut ChatApp, doc: &mut crate::testing::Workspace) {
    app.land(doc);
    app.sync(&doc.chat, &automerge::ROOT);
    render_frame(app, 1600.0, 900.0);
}

/// The messages assistive technology is told of, each by its name.
fn told(app: &ChatApp) -> Vec<String> {
    app.accessibility()
        .expect("the window is told")
        .children
        .into_iter()
        .find(|node| node.name.as_deref() == Some("Messages"))
        .expect("the conversation's list")
        .children
        .into_iter()
        .filter_map(|node| node.name)
        .collect()
}

/// Whether a message is told that has no words after who wrote it.
fn tells_a_message_without_words(app: &ChatApp) -> bool {
    told(app).iter().any(|name| name.trim_end().ends_with(':'))
}

/// A window on the conversation between its person and their agent, nothing said in it yet.
fn with_agent() -> (ChatApp, crate::testing::Workspace, String) {
    let (mut app, doc) = app_with_doc();
    let conversation = crate::chat::Chat::direct_id([app.me.id.clone(), agent()]).to_string();
    app.active_channel.clone_from(&conversation);
    app.reselect();
    (app, doc, conversation)
}

/// The person wrote again while their agent worked: their first message was taken up, their
/// second came, the turn left its first reply as a server's turn does, complete and with no
/// word of its own, and answered under their second. The window, drawn, and in order their
/// first message, the reply left, their second message and the one answer.
fn written_again() -> (ChatApp, [String; 4]) {
    let (mut app, mut doc, conversation) = with_agent();
    let me = app.me.id.clone();
    let first = doc
        .say(&me, &conversation, "Draft a note to Maya about Friday.")
        .unwrap();
    notes(
        &mut doc,
        &first,
        &AgentWork::new(AgentWorkState::Taken, crate::demo::NOW),
        None,
    );
    let second = doc
        .say(&me, &conversation, "And say that I bring the numbers.")
        .unwrap();
    notes(&mut doc, &first, &complete(), None);
    notes(
        &mut doc,
        &second,
        &complete(),
        Some("The note is drafted, with the numbers."),
    );
    draw(&mut app, &mut doc);
    let (left, answer) = (reply_to(&first), reply_to(&second));
    (app, [first, left, second, answer])
}

/// In the conversation with their agent the reply the turn left has no row: the person's
/// second message follows their first with nothing between, and the one answer follows it.
#[test]
fn the_reply_a_turn_left_without_words_or_work_has_no_row() {
    let (app, [first, left, second, answer]) = written_again();
    let row = |id: &str| crate::app::painted_row(&app.hits, id);
    assert_eq!(
        row(&left),
        None,
        "nothing is drawn for the reply the turn left"
    );
    assert_eq!(
        app.timeline.borrow().shown(),
        [first.clone(), second.clone(), answer.clone()],
        "the conversation shows their two messages and the one answer"
    );
    let (first, second, answer) = (
        row(&first).expect("their first message is drawn"),
        row(&second).expect("their second message is drawn"),
        row(&answer).expect("the one answer is drawn"),
    );
    assert!(
        (second.y0 - first.y1).abs() < 0.5,
        "their second message follows their first with nothing between: {first:?} {second:?}"
    );
    assert!(
        (answer.y0 - second.y1).abs() < 0.5,
        "and the answer follows it: {second:?} {answer:?}"
    );
}

/// What keeps its row: a reply while its turn works, a reply that ended without words but
/// did something, and a reply whose model failed.
#[test]
fn a_reply_at_work_or_with_work_to_show_keeps_its_row() {
    let (mut app, mut doc, conversation) = with_agent();
    let me = app.me.id.clone();
    let asked = doc
        .say(&me, &conversation, "Draft a note to Maya about Friday.")
        .unwrap();
    let reply = reply_to(&asked);
    let taken = AgentWork::new(AgentWorkState::Taken, crate::demo::NOW);
    notes(&mut doc, &asked, &taken, None);
    draw(&mut app, &mut doc);
    assert!(
        crate::app::painted_row(&app.hits, &reply).is_some(),
        "a reply its turn works on says so"
    );
    let step = AgentStep {
        id: "00000000-0000-4000-8000-00000000000a".into(),
        sequence: 0,
        resource: "tasks.workspace".into(),
        label: "Add task".into(),
        outcome: AgentStepOutcome::Applied,
    };
    notes(&mut doc, &asked, &complete().with_trace(vec![step]), None);
    draw(&mut app, &mut doc);
    assert!(
        crate::app::painted_row(&app.hits, &reply).is_some(),
        "a reply that ended without words shows what it did"
    );
    let again = doc.say(&me, &conversation, "And the numbers?").unwrap();
    let failed = AgentWork::new(
        AgentWorkState::ProviderFinished {
            outcome: AgentFinish::Failed,
        },
        crate::demo::NOW,
    );
    notes(&mut doc, &again, &failed, None);
    draw(&mut app, &mut doc);
    assert!(
        crate::app::painted_row(&app.hits, &reply_to(&again)).is_some(),
        "a reply whose model failed says so"
    );
}

/// In a channel a reply stands in its message's thread. The message offers a thread while
/// the reply's turn works, and none for the reply the turn left; an open thread tells
/// nothing of it.
#[test]
fn a_channel_message_offers_no_thread_for_the_reply_a_turn_left() {
    let (mut app, mut doc) = app_with_doc();
    let me = app.me.id.clone();
    let channel = app
        .channels
        .iter()
        .find(|channel| !channel.direct && channel.kind == crate::model::ChannelKind::Text)
        .expect("a text channel")
        .id
        .clone();
    let asked = doc
        .say(&me, &channel, "Tide, draft a note to Maya.")
        .unwrap();
    app.active_channel = channel;
    app.reselect();
    let offers_thread = |app: &ChatApp| {
        app.hits
            .iter()
            .any(|(_, hit)| matches!(hit, Hit::OpenThread(id) if *id == asked))
    };
    let taken = AgentWork::new(AgentWorkState::Taken, crate::demo::NOW);
    notes(&mut doc, &asked, &taken, None);
    draw(&mut app, &mut doc);
    assert!(
        offers_thread(&app),
        "while its turn works the reply is the message's thread"
    );
    notes(&mut doc, &asked, &complete(), None);
    draw(&mut app, &mut doc);
    assert!(!offers_thread(&app), "the reply the turn left is no thread");
    app.thread_open = Some(asked.clone());
    app.right_flap.snap(true);
    draw(&mut app, &mut doc);
    assert!(
        !tells_a_message_without_words(&app),
        "the open thread tells nothing of it: {:?}",
        told(&app)
    );
}

/// The conversation the pictures show is the case: under the person's first message stands
/// the reply its turn left, the same as the one an agent's own notes leave.
#[test]
fn the_pictured_conversation_holds_the_reply_its_turn_left() {
    use crate::testing::MountedChat as _;
    let mut shown =
        ui::framework::testing::TestApp::<crate::ChatShell>::capture(Some("agent-went-on"))
            .unwrap();
    let conversation = crate::resource::went_on_conversation().to_string();
    assert_eq!(shown.chat().active_channel, conversation);
    let rows = crate::testing::mounted_rows(&mut shown, &conversation);
    let left = reply_to(crate::resource::WENT_ON_FIRST);
    let answer = reply_to(crate::resource::WENT_ON_SECOND);
    assert_eq!(
        rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>(),
        [
            crate::resource::WENT_ON_FIRST,
            left.as_str(),
            crate::resource::WENT_ON_SECOND,
            answer.as_str()
        ]
    );
    let (app, [first, noted, ..]) = written_again();
    let noted = app
        .message(&noted)
        .expect("the reply the agent's notes left");
    let pictured = rows.get(1).expect("the reply the picture holds");
    let of = |reply: &crate::model::Message| {
        (
            reply.text.clone(),
            reply.work.as_ref().map(|work| work.state.clone()),
            reply.work.as_ref().map(|work| work.trace.len()),
            reply.parent.is_some(),
        )
    };
    assert_eq!(of(pictured), of(noted));
    assert_eq!(noted.parent.as_deref(), Some(first.as_str()));
    assert_eq!(
        pictured.parent.as_deref(),
        Some(crate::resource::WENT_ON_FIRST)
    );
}

/// Pictures for a review, written only where `TILO_UI_SCREENSHOT_DIR` names a folder: the
/// conversation with the person's agent after they wrote again while it worked, in both
/// themes, in a wide window and a narrow one.
#[test]
fn pictures_of_a_conversation_written_into_while_the_agent_worked() {
    let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for (size, viewport) in [("wide", (1280, 800)), ("narrow", (420, 800))] {
        for (name, theme) in [
            ("dark", ui::theme::Theme::dark()),
            ("light", ui::theme::Theme::light()),
        ] {
            let mut shown = ui::framework::testing::TestApp::<crate::ChatShell>::capture_at(
                Some("agent-went-on"),
                viewport,
            )
            .unwrap();
            shown.harness().set_theme(theme);
            shown
                .harness()
                .render_png(&directory.join(format!("agent-went-on-{name}-{size}.png")));
        }
    }
}
