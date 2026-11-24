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
use tilolang_process::testing::Bus;
use tilolang_tilo::runtime::{Invoked, ResourceRef, Value as RtValue};
use ui::permissions::policy::{self, MemberAccess};
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
    bus.member(MemberId::agent(AGENT), AGENT_NAME);
    let route = RouteSpec::app(CHAT, uuid::Uuid::new_v4());
    let workspace = route.cluster_id.to_string();
    let mut policy = policy::WorkspacePolicy::new(workspace.as_str(), "ada");
    policy.agents.insert(
        AGENT.to_owned(),
        policy::AgentMember {
            for_person: "ada".to_owned(),
        },
    );
    let spec = spec_of(&route, "ada").access(MemberAccess::person(policy, "ada"));
    // A window tall enough to show a conversation's messages and the turn under them without
    // scrolling: what the test reads and presses is what the person sees.
    let mut session = Session::on_bus_window(&bus, &route, spec, |hosted| {
        Window::in_harness_sized(hosted, (1280, 1600))
    });
    session.settle();
    (session, workspace)
}

/// The person opens the conversation with their agent from the sidebar: the agent stands under
/// Direct messages as the member list names it, and choosing it opens the conversation.
fn open_the_agents_conversation(session: &mut Session) {
    let named = session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .collect::<Vec<_>>();
    let node = session
        .window
        .harness()
        .a11y_node_named(AGENT_NAME)
        .unwrap_or_else(|| panic!("the sidebar offers {AGENT_NAME:?}: {named:?}"));
    session
        .window
        .harness()
        .perform_a11y_action(node, &ui::a11y::Action::Activate)
        .unwrap_or_else(|why| panic!("choosing {AGENT_NAME} opens the conversation: {why}"));
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
    let invoker = ui::tilo_actions::ActionInvoker::agent(AGENT).acting_for("ada");
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
fn dispatched(session: &Session, id: &str, record: Invoked) {
    session
        .instance_host()
        .runtime()
        .set_invoked(id, Some(&record));
}

/// Record a turn and its steps on the dispatch, in the order the window reads them: the turn
/// first, so the view reads its steps, then each step. A record set before the window read it is
/// the dispatch's to read again, and the window's reader of invocations clears what it has not
/// read yet.
fn dispatch_a_turn(session: &mut Session, turn: Invoked, steps: &[Invoked]) {
    let turn_id = turn.id.clone();
    dispatched(session, &turn_id, turn);
    session.settle();
    for step in steps {
        dispatched(session, &step.id, step.clone());
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
        turn_in(&workspace, turn, "ended", vec![step.to_owned()]),
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
        turn_in(&workspace, turn, "waiting", vec![step.to_owned()]),
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
        turn_in(&workspace, turn, "ended", vec![step.to_owned()]),
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

/// Whether the window sent the dispatch an answer of `approval` to the invocation `step`.
fn resolve_asked(session: &Session, step: &str, approval: &str) -> bool {
    use ui::tilo_actions::remote::{InvokeArgument, RemoteActionMessage};
    // The dispatch names an invocation record by the id its app stores it under: its item.
    let item = step.rsplit('/').next().unwrap_or(step);
    session.sent.borrow().iter().any(|request| {
        let collaboration::CollabRequest::RemoteAction {
            message: RemoteActionMessage::Invoke(request),
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
