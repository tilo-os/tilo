//! Chat with the person's agent, through the window a person uses: they open the conversation with
//! their agent from the sidebar, write in the composer and send it with the Send control. The
//! agent answers through Chat's published `send`, with the turn the dispatch records it as, and
//! the answer stands in the conversation under who wrote it. A view the agent wrote with
//! `send_view` is drawn in its message, and a step that waits on the person draws Allow, Always
//! and Deny: pressing Allow sends the dispatch the answer it waits for, and what the agent writes
//! after it stands in the conversation.
//!
//! The turn's record is the system dispatch's, not the app's: these tests stand in for the
//! dispatch's side of a turn (the invocation records and what the agent writes through the Action
//! API, as the workspace's agent does), and read what the window does with it.

use std::rc::Rc;

use collaboration::RouteSpec;
use serde_json::{Map, json};
use tilo_app_window::{UiNode, Value};
use tilo_permissions::policy::{self, MemberAccess};
use tilolang_process::testing::Bus;
use tilolang_tilo::runtime::{Invoked, ResourceRef, Value as RtValue};
use ui::tilo_actions::MemberId;

use super::{CHAT, Session, Window, find, spec_of};

/// The agent the person keeps in the workspace, as the policy and the member list name it.
const AGENT: &str = "helper";
/// What the member list calls the agent.
const AGENT_NAME: &str = "Helper";

/// Ada at Chat, in a workspace whose policy holds the agent `helper` for her and whose member
/// list names it.
fn chat_with_an_agent() -> (Session, String) {
    let bus = Rc::new(Bus::default());
    bus.member(MemberId::person("ada"), "Ada");
    bus.member(MemberId::agent(AGENT, "ada"), AGENT_NAME);
    let route = RouteSpec::app(CHAT, uuid::Uuid::new_v4());
    let workspace = route.cluster_id.to_string();
    let mut policy = policy::WorkspacePolicy::new(workspace.as_str(), "ada");
    // Ada's agent, admitted by her: an agent is the pair of its id and its person.
    policy.grants.push(policy::Grant::new(
        format!("agent-member:{AGENT}"),
        policy::Grantee::agent(AGENT, "ada"),
        policy::TreePath::workspace(&workspace),
        tilo_action_types::KindSet::READ_UNDOABLE,
        policy::Answer::Allow,
        policy::Grantee::person("ada"),
    ));
    bus.set_access(&route, "ada", MemberAccess::person(policy, "ada"));
    let spec = spec_of(&route, "ada");
    // A window tall enough to show a conversation's messages and the turn under them without
    // scrolling: what the test reads and presses is what the person sees.
    let mut session = Session::on_bus_window(&bus, &route, spec, |hosted| {
        Window::in_harness_sized(hosted, (1280, 1600))
    });
    session.settle();
    (session, workspace)
}

/// The person opens the conversation with their agent from the sidebar: Everyone lists who the
/// workspace holds, the agent among them as the member list names it, and the way to the
/// conversation with it opens that conversation.
fn open_the_agents_conversation(session: &mut Session) {
    session.window.press_named("Everyone");
    session.settle();
    let nodes = session.window.harness().a11y_nodes();
    let (_, row) = nodes
        .iter()
        .find_map(|(at, node)| (node.label() == Some(AGENT_NAME)).then_some((*at, node)))
        .unwrap_or_else(|| {
            let named = session
                .controls()
                .into_iter()
                .map(|control| control.name)
                .collect::<Vec<_>>();
            panic!("Everyone lists {AGENT_NAME:?}: {named:?}")
        });
    let row = row.bounds().expect("the agent's row is laid out");
    // The Message control in that row, past the agent's name.
    let message = nodes
        .iter()
        .filter_map(|(_, node)| {
            (node.label() == Some("Message"))
                .then(|| node.bounds())
                .flatten()
                .filter(|rect| rect.y0 >= row.y0 - 1.0 && rect.y1 <= row.y1 + 1.0)
        })
        .min_by(|one, two| one.x0.total_cmp(&two.x0))
        .expect("the agent's row offers Message");
    session.window.click_at(
        f64::midpoint(message.x0, message.x1),
        f64::midpoint(message.y0, message.y1),
    );
    session.settle();
}

/// The direct conversation `name` of the window's workspace.
fn conversation(session: &Session, name: &str) -> ResourceRef {
    let host = session.instance_host();
    let all = host.root_of(CHAT).expect("the conversations");
    let RtValue::List(items) = host.runtime().read(&all, "channels").expect("the channels") else {
        panic!("the conversations list their channels");
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            RtValue::Ref(at) => Some((*at.target).clone()),
            _ => None,
        })
        .find(|at| host.runtime().read(at, "name").ok() == Some(RtValue::Text(name.to_owned())))
        .unwrap_or_else(|| panic!("the conversation with {name}"))
}

/// The messages of `direct`, oldest first.
fn messages(session: &Session, direct: &ResourceRef) -> Vec<ResourceRef> {
    let host = session.instance_host();
    let RtValue::List(items) = host
        .runtime()
        .read(direct, "messages")
        .expect("the messages")
    else {
        panic!("a conversation lists its messages");
    };
    items
        .into_iter()
        .filter_map(|item| match item {
            RtValue::Ref(at) => Some((*at.target).clone()),
            _ => None,
        })
        .collect()
}

/// What the message `m` says.
fn said(session: &Session, m: &ResourceRef) -> String {
    match session
        .instance_host()
        .runtime()
        .read(m, "body")
        .expect("a message says something")
    {
        RtValue::Rich(rich) => rich.written_text(),
        RtValue::Text(text) => text,
        other => panic!("a message's body reads as text: {other:?}"),
    }
}

/// A resource as the Action API's arguments name one: its id, `document/item`, as the runtime's
/// references are written.
fn resource_argument(session: &Session, ty: &str, at: &ResourceRef) -> serde_json::Value {
    json!({
        "app": CHAT,
        "kind": session.kind(ty),
        "id": at.id_string(),
        "label": "",
    })
}

/// The agent invokes Chat's published `action`, acting for the person it works for, as the
/// workspace's agent does: the invoker the broker admits carries the person with it.
fn agent_invokes(session: &mut Session, action: &str, arguments: &Map<String, serde_json::Value>) {
    let identity = session.published_doing(&format!("tl.{CHAT}"), action);
    let address = ui::tilo_actions::ActionAddress::parse(&identity)
        .unwrap_or_else(|| panic!("{identity} names a resource and an action"));
    let invoker = ui::tilo_actions::ActionInvoker::agent(AGENT, "ada").acting_for("ada");
    let ambient = ui::tilo_actions::ActionContext::default();
    session
        .invoke(
            Some(invoker),
            Some(&ambient),
            &address.resource,
            &address.action,
            arguments,
        )
        .unwrap_or_else(|why| panic!("the agent's {action} ran: {why}"));
}

/// The agent writes `body` in `direct`, answering `reply_to` when it names a message, as the
/// workspace's agent writes its answer: through Chat's published `send`.
fn agent_says(
    session: &mut Session,
    direct: &ResourceRef,
    body: &str,
    reply_to: Option<&ResourceRef>,
    turn: &str,
) {
    let mut arguments = json!({
        "to": resource_argument(session, "Direct", direct),
        "body": body,
        "turn": turn,
    });
    if let Some(reply_to) = reply_to {
        arguments["reply_to"] = resource_argument(session, "Message", reply_to);
    }
    agent_invokes(
        session,
        "send",
        &arguments.as_object().cloned().unwrap_or_default(),
    );
}

/// The agent sends the view `definition`, called `title`, through Chat's published `send_view`.
fn agent_sends_view(session: &mut Session, direct: &ResourceRef, title: &str, definition: &str) {
    let arguments = json!({
        "to": resource_argument(session, "Direct", direct),
        "title": title,
        "definition": definition,
    });
    agent_invokes(
        session,
        "send_view",
        &arguments.as_object().cloned().unwrap_or_default(),
    );
}

/// What the dispatch records of the invocation `id`: the window reads it with `invoked(id)`.
fn dispatched(session: &Session, id: &str, record: &Invoked) {
    session
        .instance_host()
        .runtime()
        .set_invoked(id, Some(record));
}

/// Record a turn and its steps on the dispatch, in the order the window reads them: the turn
/// first, so the view reads its steps, then each step. A record set before the window read it is
/// the dispatch's to read again, and the window's reader of invocations clears what it has not
/// read yet.
fn dispatch_a_turn(session: &mut Session, turn: &Invoked, steps: &[Invoked]) {
    dispatched(session, &turn.id, turn);
    session.settle();
    for step in steps {
        dispatched(session, &step.id, step);
    }
    session.settle();
}

/// An invocation of the turn that answered the conversation.
fn turn_in(workspace: &str, id: &str, state: &str, calls: Vec<String>) -> Invoked {
    Invoked {
        workspace: workspace.to_owned(),
        id: id.to_owned(),
        state: state.to_owned(),
        action: "so.tilo.agent#answer".to_owned(),
        title: "Answer the conversation".to_owned(),
        summary: String::new(),
        failure: String::new(),
        calls,
        provider: "anthropic".to_owned(),
        model: "claude-sonnet-5-5".to_owned(),
        cost: 0.001_2,
    }
}

/// One step of that turn.
fn step_in(workspace: &str, id: &str, state: &str, title: &str) -> Invoked {
    Invoked {
        workspace: workspace.to_owned(),
        id: id.to_owned(),
        state: state.to_owned(),
        action: "so.tilo.chat#send".to_owned(),
        title: title.to_owned(),
        summary: String::new(),
        failure: String::new(),
        calls: Vec::new(),
        provider: String::new(),
        model: String::new(),
        cost: 0.0,
    }
}

/// One message the window draws: who it is by, what it says, and the row itself.
struct Shown {
    author: Option<String>,
    text: String,
    row: UiNode,
}

/// The messages the transcript draws now, oldest first.
fn transcript(session: &mut Session) -> Vec<Shown> {
    let view = session.hosted().view().expect("the window is connected");
    let mut shown = Vec::new();
    for list in view.lists() {
        let Some(rows) = view.rows_of(&list) else {
            continue;
        };
        let rows = rows.borrow();
        for key in rows.rendered() {
            let Some(row) = rows.row(&key) else {
                continue;
            };
            let Some(item) = find(row, &|node| node.kind == "ui.ListItem") else {
                continue;
            };
            let author = match item.props.get("author") {
                Some(Value::Actor(who)) => Some(who.name.clone()),
                _ => None,
            };
            let mut text = String::new();
            texts(row, &mut text);
            shown.push(Shown {
                author,
                text,
                row: row.clone(),
            });
        }
    }
    shown
}

/// Every word a node and its children show, rich text as written.
fn texts(node: &UiNode, out: &mut String) {
    for value in node.props.values() {
        match value {
            Value::Text(text) => {
                out.push_str(text);
                out.push(' ');
            }
            Value::Actor(who) => {
                out.push_str(&who.name);
                out.push(' ');
            }
            Value::Rich(rich) => {
                for block in &rich.blocks {
                    out.push_str(&block.text);
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
    for child in &node.children {
        texts(child, out);
    }
}

/// The person types `said` into the composer and sends it with the Send control, as a person does.
fn send_with_the_control(session: &mut Session, said: &str) {
    let (x, y) = crate::interaction::composer(session.window.harness());
    session.window.click_at(x, y);
    session.window.type_text(said);
    session.window.harness().relayout();
    let (x, y) = crate::interaction::send_button(session.window.harness());
    session.window.click_at(x, y);
    session.settle();
}

/// The person opens Chat, opens the conversation with their agent, writes a message and sends it
/// with the Send control: the message is in the conversation, by them.
#[test]
fn a_person_writes_a_message_in_a_conversation_with_their_agent_and_sends_it() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "Plan the release with me");

    let direct = conversation(&session, AGENT_NAME);
    let messages = messages(&session, &direct);
    assert_eq!(messages.len(), 1, "one message was sent");
    assert_eq!(said(&session, &messages[0]), "Plan the release with me");

    // It stands in the window, by the person who sent it.
    let shown = transcript(&mut session);
    let sent = shown.last().expect("the transcript draws the message");
    assert!(
        sent.text.contains("Plan the release with me"),
        "the message shows in the conversation: {sent:?}",
        sent = sent.text
    );
    assert_eq!(
        sent.author.as_deref(),
        Some("Ada"),
        "the message is by the person who sent it"
    );
}

/// The agent answers: the answer stands in the conversation by the agent, with the turn that wrote
/// it, the model it asked and the step it took.
#[test]
fn an_agents_answer_stands_with_its_turn_and_author() {
    let _settled = ui::motion::testing::settled();
    let (mut session, workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "Plan the release with me");

    let direct = conversation(&session, AGENT_NAME);
    let asked = messages(&session, &direct)
        .into_iter()
        .next()
        .expect("the person's message");
    let turn = "agents.agent:helper#answer:1";
    let step = "agents.agent:helper#answer:1/step-1";
    agent_says(
        &mut session,
        &direct,
        "Here is the plan: design on Monday, release on Friday.",
        Some(&asked),
        turn,
    );
    dispatch_a_turn(
        &mut session,
        &turn_in(&workspace, turn, "ended", vec![step.to_owned()]),
        &[step_in(&workspace, step, "ended", "Wrote the plan")],
    );

    let shown = transcript(&mut session);
    let answer = shown.last().expect("the transcript draws the answer");
    assert!(
        answer
            .text
            .contains("Here is the plan: design on Monday, release on Friday."),
        "the answer shows in the conversation: {}",
        answer.text
    );
    assert_eq!(
        answer.author.as_deref(),
        Some(AGENT_NAME),
        "the answer is by the agent"
    );
    assert!(
        answer.text.contains("Done"),
        "the turn stands with the answer: {}",
        answer.text
    );
    assert!(
        answer.text.contains("anthropic/claude-sonnet-5-5"),
        "the turn names the model it asked: {}",
        answer.text
    );
    assert!(
        answer.text.contains("Wrote the plan"),
        "the turn's step stands with it: {}",
        answer.text
    );
}

/// The agent answers with a view it wrote: the view is drawn in its message, natively, not named
/// in words.
#[test]
fn an_agents_written_view_is_drawn_inside_its_message() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "How many of each?");

    let direct = conversation(&session, AGENT_NAME);
    let definition = "view numbers() {\n\
        stack(gap: .tight) {\n\
            ui.ListRow(title: \"Apples\", secondary: \"3\")\n\
            ui.ListRow(title: \"Pears\", secondary: \"5\")\n\
        }\n\
    }";
    agent_sends_view(&mut session, &direct, "Apples and pears", definition);
    session.settle();

    let shown = transcript(&mut session);
    let view = shown.last().expect("the transcript draws the sent view");
    let rows = view.row.of_kind("ui.ListRow");
    assert_eq!(
        rows.len(),
        2,
        "the view's rows are drawn in the message: {}",
        view.text
    );
    for word in ["Apples", "3", "Pears", "5"] {
        assert!(
            view.text.contains(word),
            "the view shows {word:?} in the message: {}",
            view.text
        );
    }
}

/// A step of the turn waits on the person: the window draws Allow, Always and Deny, the person
/// presses Allow, and the answer goes to the dispatch the turn waits on. What the agent writes
/// after it stands in the conversation.
#[test]
fn an_agents_ask_shows_and_the_persons_answer_goes_to_the_dispatch() {
    let _settled = ui::motion::testing::settled();
    let (mut session, workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "Send the plan to the team");

    let direct = conversation(&session, AGENT_NAME);
    let asked = messages(&session, &direct)
        .into_iter()
        .next()
        .expect("the person's message");
    let turn = "agents.agent:helper#answer:2";
    let step = "agents.agent:helper#answer:2/step-1";
    agent_says(
        &mut session,
        &direct,
        "I am ready to send it.",
        Some(&asked),
        turn,
    );
    dispatch_a_turn(
        &mut session,
        &turn_in(&workspace, turn, "waiting", vec![step.to_owned()]),
        &[step_in(&workspace, step, "waiting", "Send the plan")],
    );

    let shown = session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect::<Vec<_>>();
    for (control, why) in [
        ("Allow", "the ask is answerable"),
        ("Always", "the person may stop being asked"),
        ("Deny", "the person may refuse"),
    ] {
        assert!(
            shown.iter().any(|name| name == control),
            "{control} stands where the turn waits ({why}): {shown:?}"
        );
    }

    session.forget_sent();
    // Press the button where the window laid it out, read from the frame's accessibility tree:
    // the one node that is the button, not the words it shows.
    let snapshot = session.window.harness().inspection();
    let allow = snapshot
        .root
        .nodes()
        .into_iter()
        .filter_map(|node| node.semantics.as_ref())
        .flat_map(ui::a11y::AccessNode::nodes)
        .find(|node| node.role == ui::a11y::Role::Button && node.answers_to("Allow"))
        .and_then(|node| node.bounds)
        .expect("Allow stands where the step waits");
    session.window.click_at(
        f64::midpoint(allow.x0, allow.x1),
        f64::midpoint(allow.y0, allow.y1),
    );
    session.settle();

    assert!(
        resolve_asked(&session, step, "approve"),
        "pressing Allow answers the waiting step: {:?}",
        session
            .sent
            .borrow()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect::<Vec<_>>()
    );

    // The dispatch takes the answer: the step ends, and what the agent writes after it stands in
    // the conversation, with nothing left to answer.
    dispatch_a_turn(
        &mut session,
        &turn_in(&workspace, turn, "ended", vec![step.to_owned()]),
        &[step_in(&workspace, step, "ended", "Sent the plan")],
    );
    agent_says(
        &mut session,
        &direct,
        "Sent. The team has it now.",
        Some(&asked),
        turn,
    );
    session.settle();
    let shown = transcript(&mut session);
    let after = shown.last().expect("the agent's message after the answer");
    assert!(
        after.text.contains("Sent. The team has it now."),
        "the agent goes on in the conversation: {}",
        after.text
    );
    let allowed = session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect::<Vec<_>>();
    assert!(
        !allowed.iter().any(|name| name == "Allow"),
        "the ask is answered and offers nothing more: {allowed:?}"
    );
}

/// The person writes `body` in `direct` through Chat's published `send`, as their own press does.
fn person_says(session: &mut Session, direct: &ResourceRef, body: &str) {
    let identity = session.published_doing(&format!("tl.{CHAT}"), "send");
    let address = ui::tilo_actions::ActionAddress::parse(&identity)
        .unwrap_or_else(|| panic!("{identity} names a resource and an action"));
    let arguments = json!({
        "to": resource_argument(session, "Direct", direct),
        "body": body,
    });
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    session
        .invoke(None, None, &address.resource, &address.action, &arguments)
        .unwrap_or_else(|why| panic!("the person's send ran: {why}"));
}

/// Whether the window sent the dispatch an answer of `approval` to the invocation `step`.
fn resolve_asked(session: &Session, step: &str, approval: &str) -> bool {
    use ui::tilo_actions::remote::{InvokeArgument, RemoteActionMessage};
    // The dispatch names an invocation record by the id its app stores it under: its item.
    let item = step.rsplit('/').next().unwrap_or(step);
    session.sent.borrow().iter().any(|request| {
        let collaboration::CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(request),
            ..
        } = request
        else {
            return false;
        };
        if request.target.address.action != "resolve" || request.target.address.resource != item {
            return false;
        }
        let InvokeArgument::Json(arguments) = &request.argument else {
            return false;
        };
        arguments.get("resolution").and_then(|it| it.as_str()) == Some(approval)
    })
}

/// Run the wheel's glide to where it rests, as the window draws it away.
fn glide(session: &mut Session) {
    use ui::time::testing::with_monotonic_time;
    let mut at = ui::time::Instant::now();
    for _ in 0..400 {
        with_monotonic_time(at, || {
            session.settle_host();
            session.window.frame();
        });
        if !session
            .window
            .harness()
            .frame_schedule()
            .needs_animation_frame
        {
            return;
        }
        at += ui::time::Duration::from_millis(16);
    }
    panic!("the list's glide never came to rest");
}

/// A turn that runs: the window offers Stop on it, and pressing Stop cancels the invocation the
/// turn is, so the dispatch ends it and what it wrote so far stays.
#[test]
fn a_running_turn_is_stopped_from_its_message() {
    use ui::tilo_actions::remote::{InvokeArgument, RemoteActionMessage};
    let _settled = ui::motion::testing::settled();
    let (mut session, workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "Plan the release with me");

    let direct = conversation(&session, AGENT_NAME);
    let asked = messages(&session, &direct)
        .into_iter()
        .next()
        .expect("the person's message");
    let turn = "agents.agent:helper#answer:7";
    agent_says(
        &mut session,
        &direct,
        "Design on Monday.",
        Some(&asked),
        turn,
    );
    dispatch_a_turn(
        &mut session,
        &turn_in(&workspace, turn, "running", Vec::new()),
        &[],
    );

    let shown = session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect::<Vec<_>>();
    assert!(
        shown.iter().any(|name| name == "Stop"),
        "a running turn offers Stop: {shown:?}"
    );

    session.forget_sent();
    session.window.press_named("Stop");
    session.settle();

    let cancelled = session.sent.borrow().iter().any(|request| {
        let collaboration::CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(request),
            ..
        } = request
        else {
            return false;
        };
        request.target.address.action == "cancel"
            && matches!(request.argument, InvokeArgument::Json(_))
    });
    assert!(
        cancelled,
        "pressing Stop cancels the turn's invocation: {:?}",
        session
            .sent
            .borrow()
            .iter()
            .map(|request| format!("{request:?}"))
            .collect::<Vec<_>>()
    );
}

/// A long conversation: the window opens at its newest message, and scrolling back reaches the
/// first one, so a person reads a conversation of any length from its start.
#[test]
fn a_long_conversation_is_read_back_to_its_first_message() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    let direct = conversation(&session, AGENT_NAME);
    for n in 0..40 {
        person_says(&mut session, &direct, &format!("Message number {n}"));
    }
    session.settle();

    let newest = transcript(&mut session);
    let last = newest
        .last()
        .expect("the transcript draws the newest message");
    assert!(
        last.text.contains("Message number 39"),
        "the window opens at the newest message: {}",
        last.text
    );

    // The person turns the wheel over the conversation until its first message stands in the
    // window.
    // The pointer stands over the conversation, where a person's wheel turns it.
    session.window.move_to(640.0, 700.0);
    session.settle();
    let mut found = false;
    for _ in 0..100 {
        let shown = transcript(&mut session);
        found = shown.iter().any(|shown| {
            shown.text.split(' ').any(|word| word == "number")
                && shown.text.contains("Message number 0")
        });
        if found {
            break;
        }
        session.window.scroll(0.0, 20.0);
        glide(&mut session);
    }
    assert!(found, "the conversation reads back to its first message");
    assert_eq!(
        messages(&session, &direct).len(),
        40,
        "every message the person wrote is in the conversation"
    );
}

/// A person replies to a message: Reply on its row puts it above the composer, and the message
/// then sent quotes it above its own words, in the conversation and in what Chat keeps.
#[test]
fn a_person_replies_to_a_message_and_the_window_quotes_what_it_answers() {
    let _settled = ui::motion::testing::settled();
    let (mut session, _workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);
    send_with_the_control(&mut session, "Plan the release with me");

    let direct = conversation(&session, AGENT_NAME);
    let asked = messages(&session, &direct)
        .into_iter()
        .next()
        .expect("the person's message");
    agent_says(
        &mut session,
        &direct,
        "Friday suits the team.",
        Some(&asked),
        "agents.agent:helper#answer:3",
    );
    session.settle();
    let answered = messages(&session, &direct)
        .into_iter()
        .last()
        .expect("the agent's answer");

    // The person points at the agent's answer and presses Reply where it stands.
    let nodes = session.window.harness().a11y_nodes();
    let answer_at = nodes
        .iter()
        .find_map(|(_, node)| {
            node.label()
                .is_some_and(|label| label.contains("Friday suits the team."))
                .then(|| node.bounds())
                .flatten()
        })
        .expect("the answer stands in the conversation");
    // The pointer stands on the answer, where its toolbar shows.
    session.window.move_to(
        f64::midpoint(answer_at.x0, answer_at.x1),
        f64::midpoint(answer_at.y0, answer_at.y1),
    );
    session.settle();
    let reply_at = session
        .window
        .harness()
        .a11y_nodes()
        .iter()
        .filter_map(|(_, node)| {
            (node.label() == Some("Reply"))
                .then(|| node.bounds())
                .flatten()
                .filter(|rect| rect.y1 <= answer_at.y0 + 1.0)
        })
        .max_by(|one, two| one.y1.total_cmp(&two.y1))
        .expect("Reply stands over the answer");
    session.window.click_at(
        f64::midpoint(reply_at.x0, reply_at.x1),
        f64::midpoint(reply_at.y0, reply_at.y1),
    );
    session.settle();
    send_with_the_control(&mut session, "Then Friday it is.");

    let sent = messages(&session, &direct);
    assert_eq!(sent.len(), 3, "the reply is a message of the conversation");
    let reply = sent.last().expect("the reply");
    let host = session.instance_host();
    let answering = host
        .runtime()
        .read(reply, "reply_to")
        .expect("the reply says what it answers");
    let id = answered.id_string();
    let item = id.rsplit('/').next().unwrap_or(&id);
    assert!(
        format!("{answering:?}").contains(item),
        "the reply answers the agent's message ({item}): {answering:?}"
    );

    let shown = transcript(&mut session);
    let answer = shown.last().expect("the transcript draws the reply");
    assert!(
        answer.text.contains("Then Friday it is."),
        "the person's words stand in the reply: {}",
        answer.text
    );
    assert!(
        answer.text.contains("Friday suits the team."),
        "the reply quotes what it answers above its words: {}",
        answer.text
    );
}

/// The person's chooser at the window: it answers a contents pick with the files `files` names,
/// as a person choosing them in the system's chooser does, and keeps what it was asked.
struct Picks {
    files: Vec<(&'static str, &'static [u8])>,
    asked: std::sync::Mutex<Vec<tilo_dialogs::portal::PickRequest>>,
}

impl Picks {
    /// A chooser the person answers with `files`, in that order.
    fn several(files: Vec<(&'static str, &'static [u8])>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            files,
            asked: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// What it was asked, oldest first.
    fn asked(&self) -> Vec<tilo_dialogs::portal::PickRequest> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A channel no test reaches for: the picks a window never makes here.
fn unanswered<T>() -> std::sync::mpsc::Receiver<T> {
    std::sync::mpsc::channel().1
}

impl tilo_dialogs::FilePicker for Picks {
    fn pick(&self, _: tilo_dialogs::portal::PickRequest) -> tilo_dialogs::portal::PortalChannel {
        unanswered()
    }

    fn launch(
        &self,
        _: tilo_dialogs::portal::LaunchRequest,
    ) -> tilo_dialogs::portal::LaunchChannel {
        unanswered()
    }

    fn pick_multiple(
        &self,
        request: tilo_dialogs::portal::PickRequest,
    ) -> tilo_dialogs::portal::MultiChannel {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(request);
        let (sender, receiver) = std::sync::mpsc::channel();
        let references = self
            .files
            .iter()
            .map(|(name, _)| std::path::PathBuf::from(name))
            .collect();
        let _ = sender.send(tilo_dialogs::portal::MultiOutcome::Selected(references));
        receiver
    }

    /// Each file it chose opens as the bytes the person chose, by the reference it answered.
    fn reopen(
        &self,
        reference: &std::path::Path,
        _write: bool,
    ) -> tilo_dialogs::portal::OpenFileChannel {
        let (sender, receiver) = std::sync::mpsc::channel();
        let answer = self
            .files
            .iter()
            .find(|(name, _)| std::path::Path::new(name) == reference)
            .map_or_else(
                || tilo_dialogs::portal::OpenFileOutcome::Failed("no such choice".to_owned()),
                |(name, bytes)| {
                    tilo_dialogs::portal::OpenFileOutcome::Opened(
                        tilo_dialogs::portal::OpenedFile::from_bytes(
                            bytes.to_vec(),
                            *name,
                            reference.to_path_buf(),
                        ),
                    )
                },
            );
        let _ = sender.send(answer);
        receiver
    }
}

/// The session's tools, as a test serves them: it answers each request with the next of
/// `answers`, in the order given, and keeps what it was asked.
struct Tools {
    asked: std::sync::Arc<std::sync::Mutex<Vec<runtime_interface::tools::ToolIntent>>>,
    answers: std::sync::Arc<std::sync::Mutex<Vec<runtime_interface::tools::ToolReply>>>,
}

impl Tools {
    /// The session's end of a tools connection, and the stub answering the other end.
    fn serving(
        answers: Vec<runtime_interface::tools::ToolReply>,
    ) -> (std::sync::Arc<runtime_interface::tools::Service>, Self) {
        let stub = Self {
            asked: std::sync::Arc::default(),
            answers: std::sync::Arc::new(std::sync::Mutex::new(
                answers.into_iter().rev().collect(),
            )),
        };
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().expect("the tools connection");
        stub.serve(ours);
        let service =
            runtime_interface::tools::Service::connect(theirs, "the window's workspace".to_owned());
        (std::sync::Arc::new(service), stub)
    }

    /// Answer `stream`'s requests until it closes, on a thread of its own: what the session's end
    /// of an app's connection does.
    fn serve(&self, stream: std::os::unix::net::UnixStream) {
        let asked = std::sync::Arc::clone(&self.asked);
        let answers = std::sync::Arc::clone(&self.answers);
        std::thread::spawn(move || {
            use std::io::{BufRead as _, BufReader, Write as _};
            let Ok(mut writing) = stream.try_clone() else {
                return;
            };
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { return };
                let Ok(request) =
                    serde_json::from_str::<runtime_interface::tools::ToolRequest>(&line)
                else {
                    return;
                };
                asked
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(request.intent);
                let reply = answers
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .pop()
                    .unwrap_or_else(|| runtime_interface::tools::ToolReply {
                        accepted: false,
                        message: "the test service has no more answers".to_owned(),
                        ..runtime_interface::tools::ToolReply::default()
                    });
                let Ok(mut encoded) = serde_json::to_vec(&reply) else {
                    return;
                };
                encoded.push(b'\n');
                if writing.write_all(&encoded).is_err() {
                    return;
                }
            }
        });
    }

    /// What it was asked, oldest first.
    fn asked(&self) -> Vec<runtime_interface::tools::ToolIntent> {
        self.asked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// A listing that offers the app `app` for a file of the kind asked.
fn listing(app: &str) -> runtime_interface::tools::ToolReply {
    runtime_interface::tools::ToolReply {
        accepted: true,
        message: "listed".to_owned(),
        documents: vec![runtime_interface::tools::DocumentApp {
            id: app.to_owned(),
            name: app.to_owned(),
            is_default: true,
            icon: None,
            source: Some(runtime_interface::tools::AppKind::Tilo),
            artwork: None,
        }],
        ..runtime_interface::tools::ToolReply::default()
    }
}

/// The session opening the file it was asked to.
fn opened() -> runtime_interface::tools::ToolReply {
    runtime_interface::tools::ToolReply {
        accepted: true,
        message: "opened".to_owned(),
        ..runtime_interface::tools::ToolReply::default()
    }
}

/// The person adds two files from the workspace to a message and sends it: the window asks the
/// system's chooser for them, the message carries both and shows them, and pressing one opens it
/// in the app that opens its kind, through the system's open action.
#[test]
fn a_person_adds_two_files_to_a_message_and_opens_one() {
    let _settled = ui::motion::testing::settled();
    let picks = Picks::several(vec![
        ("Launch plan.pdf", b"%PDF-1.4 the launch plan"),
        ("notes.md", b"# Notes\n"),
    ]);
    let chooser: std::sync::Arc<dyn tilo_dialogs::FilePicker> = picks.clone();
    Session::choosing_files_with(&chooser);
    let (tools, served) = Tools::serving(vec![listing("so.tilo.notes"), opened()]);
    Session::opening_files_through(&tools);
    let (mut session, _workspace) = chat_with_an_agent();
    open_the_agents_conversation(&mut session);

    // The person writes the message, adds the files with the composer's Attach, and sends it.
    let (x, y) = crate::interaction::composer(session.window.harness());
    session.window.click_at(x, y);
    session.window.type_text("Here is the plan and the notes");
    session.window.harness().relayout();
    session.window.press_named("Attach");
    session.settle();
    let (x, y) = crate::interaction::send_button(session.window.harness());
    session.window.click_at(x, y);
    session.settle();

    // The chooser was asked for several files at once, and the message carries both of them.
    let asked = picks.asked();
    assert_eq!(asked.len(), 1, "the chooser was asked once");
    assert!(
        asked[0].is_multiple(),
        "the message takes more than one file: {:?}",
        asked[0]
    );
    let direct = conversation(&session, AGENT_NAME);
    let sent = messages(&session, &direct)
        .into_iter()
        .last()
        .expect("the message the person sent");
    let RtValue::List(files) = session
        .instance_host()
        .runtime()
        .read(&sent, "attachments")
        .expect("the message says what it carries")
    else {
        panic!("a message lists the files it carries");
    };
    let names = files
        .iter()
        .map(|file| match file {
            RtValue::Ref(at) => session
                .instance_host()
                .runtime()
                .read(&at.target, "name")
                .expect("an attachment says its name"),
            other => panic!("a message carries files, not {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            RtValue::Text("Launch plan.pdf".to_owned()),
            RtValue::Text("notes.md".to_owned())
        ],
        "the message carries both files, in the order they were chosen"
    );

    // The window shows both with the message.
    let shown = transcript(&mut session);
    let message = shown.last().expect("the transcript draws the message");
    for name in ["Launch plan.pdf", "notes.md"] {
        assert!(
            message.text.contains(name),
            "the message shows {name:?}: {}",
            message.text
        );
    }

    // Pressing one opens it in the app that opens its kind, through the system's open action.
    session.window.press_named("Launch plan.pdf");
    session.settle();
    let open = served
        .asked()
        .into_iter()
        .find_map(|intent| match intent {
            runtime_interface::tools::ToolIntent::OpenContent { name, app, .. } => {
                Some((name, app))
            }
            _ => None,
        })
        .expect("the file reaches the system's open action");
    assert_eq!(open.0, "Launch plan.pdf", "the file the person pressed");
    assert_eq!(open.1, "so.tilo.notes", "the app that opens its kind");
}
