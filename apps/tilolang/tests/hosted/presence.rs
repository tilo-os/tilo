//! Who else is in a conversation and who of them is writing, as the people at two devices see it:
//! each device's instance runs in its own host, on a connection to the same record, and the broker
//! relays what each says of its person to the other.

use super::*;
use tilolang_host::testing::Bus;
use tilolang_tilo::chunk::Chunks;

/// `person` at a Chat window of the record `route` names, held as `chunks`, on a connection of
/// `bus`: what they say of themselves reaches the bus's other connections on the route.
fn chat_on(bus: &Rc<Bus>, route: &RouteSpec, person: &str, chunks: Chunks) -> Session {
    let broker = Broker::on(bus, route.clone(), person);
    let (sent, inbox, routed) = (broker.sent(), broker.inbox(), broker.routed());
    let manager = CollaborationManager::with_peer_transport(broker);
    let wire = (Box::new((inbox, routed)) as Box<dyn std::any::Any>, sent);
    let spec = Spec::new(CHAT, route.cluster_id.to_string(), 1, person).in_language("en-US");
    let mut session = Session::launch(manager, route, spec, chunks, None, wire, Window::in_harness);
    session.settle();
    session
}

/// Whether the window says someone is typing.
fn says_typing(session: &mut Session) -> Option<String> {
    session
        .shown()
        .into_iter()
        .find(|text| text.contains("typing"))
}

/// The person's composer in the conversation the window shows.
fn composer(session: &mut Session) -> (f64, f64) {
    let root = session.hosted().view().expect("the window is connected");
    let tree = root
        .detail_tree()
        .expect("the window shows the conversation");
    let key = find(&tree, &|node| {
        matches!(node.props.get("placeholder"), Some(tilo_ui_host::Value::Text(p)) if p == "Message #design")
    })
    .expect("the conversation has its composer")
    .key
    .clone();
    let key = format!("{}{key}", tilo_ui_host::shell::DETAIL);
    let rect = widget_by_id(session.window.root(), &key)
        .expect("the composer is laid out")
        .get_rect();
    (rect.center().x, rect.center().y)
}

#[test]
fn a_person_writing_in_a_conversation_reads_as_typing_to_the_other_person_there_until_they_send() {
    let bus = Rc::new(Bus::default());
    let route = RouteSpec::app(CHAT, uuid::Uuid::new_v4());
    let mut ada = chat_on(
        &bus,
        &route,
        "ada",
        Chunks::of(automerge::AutoCommit::new()),
    );
    let new_channel = ada.identity_doing("Conversations", "new_channel");
    let made = ada.invoke_doing(&new_channel, &serde_json::json!({ "name": "design" }));
    assert!(
        matches!(made, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the channel was made: {made:?}"
    );
    ada.settle();
    // Bo's device holds the workspace's record as Ada's does.
    let mut bo = chat_on(&bus, &route, "bo", record(&ada));
    // Bo's window knows Ada by her name.
    bo.window.harness().update_presence(|presence| {
        presence
            .names
            .insert(ui::permissions::UserId("ada".to_owned()), "Ada".to_owned());
        true
    });
    for session in [&mut ada, &mut bo] {
        let (x, y) = shown_at(session, "design");
        session.window.click_at(x, y);
        session.settle();
    }
    assert_eq!(says_typing(&mut bo), None, "nobody writes yet");

    let (x, y) = composer(&mut ada);
    ada.window.click_at(x, y);
    ada.window.type_text("Shall we");
    ada.settle();
    bo.settle();
    assert_eq!(
        says_typing(&mut bo).as_deref(),
        Some("Ada is typing…"),
        "Bo reads Ada writing in the conversation: {:?}",
        bo.shown()
    );
    assert_eq!(
        says_typing(&mut ada),
        None,
        "Ada is not told she is typing herself"
    );

    // Ada sends: the draft is gone, and so is the typing.
    ada.window.press_key(ui::winit::keyboard::NamedKey::Enter);
    ada.settle();
    bo.settle();
    assert_eq!(
        says_typing(&mut bo),
        None,
        "once Ada sent, Bo no longer reads her typing: {:?}",
        bo.shown()
    );
    assert!(
        bo.shown().iter().any(|text| text.contains("Shall we")),
        "Bo reads what Ada sent: {:?}",
        bo.shown()
    );
}
