//! Who else is in a conversation and who of them is writing, as the people at two devices see it:
//! each device's instance runs in its own host, on a connection to the same document, the broker
//! relays what each says of its person to the other, and names everyone by the workspace's member
//! list it hands each connection.

use super::*;
use tilolang_process::testing::Bus;

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
        matches!(node.props.get("placeholder"), Some(tilo_app_window::Value::Text(p)) if p == "Message #design")
    })
    .expect("the conversation has its composer")
    .key
    .clone();
    let key = format!("{}{key}", tilo_app_window::shell::DETAIL);
    let rect = widget_by_id(session.window.root(), &key)
        .expect("the composer is laid out")
        .get_rect();
    (rect.center().x, rect.center().y)
}

/// Ada and Bo, members of one workspace, each at a Chat window on its document, both in the
/// conversation #design, which Ada made.
fn ada_and_bo_in_design() -> (Session, Session) {
    let bus = Rc::new(Bus::default());
    bus.member(ui::tilo_actions::MemberId::person("ada"), "Ada");
    bus.member(ui::tilo_actions::MemberId::person("bo"), "Bo");
    let route = RouteSpec::app(CHAT, uuid::Uuid::new_v4());
    let mut ada = Session::on_bus(&bus, &route, spec_of(&route, "ada"));
    let new_channel = ada.identity_doing("Conversations", "new_channel");
    let made = ada.invoke_doing(&new_channel, &serde_json::json!({ "name": "design" }));
    assert!(
        matches!(made, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the channel was made: {made:?}"
    );
    ada.settle();
    // Bo's window reads and writes the workspace's one store, as Ada's does.
    let mut bo = Session::on_bus(&bus, &route, spec_of(&route, "bo"));
    for session in [&mut ada, &mut bo] {
        let (x, y) = shown_at(session, "design");
        session.window.click_at(x, y);
        session.settle();
    }
    (ada, bo)
}

#[test]
fn a_person_writing_in_a_conversation_reads_as_typing_to_the_other_person_there_until_they_send() {
    let (mut ada, mut bo) = ada_and_bo_in_design();
    assert_eq!(says_typing(&mut bo), None, "nobody writes yet");

    // Nobody told Bo's window what Ada is called: his instance's connection heard it from the
    // workspace's member list.
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

#[test]
fn a_writer_whose_window_vanished_stops_reading_as_typing_when_they_go_quiet_with_nothing_else_happening()
 {
    let (mut ada, mut bo) = ada_and_bo_in_design();
    let (x, y) = composer(&mut ada);
    ada.window.click_at(x, y);
    ada.window.type_text("Shall we");
    ada.settle();
    bo.settle();
    assert_eq!(says_typing(&mut bo).as_deref(), Some("Ada is typing…"));
    // Ada's window is gone without a word: she never says she stopped. Bo's host wakes by itself
    // when she has been quiet long enough.
    drop(ada);
    let wait = bo
        .host
        .wakes_in()
        .expect("Bo's host wakes when Ada's typing runs out");
    assert!(
        wait <= tilo_ui_base::presence::COMPOSING_LINGER,
        "it wakes within the time a quiet writer lingers: {wait:?}"
    );
    let woke_at = ui::time::now() + wait;
    ui::time::testing::with_monotonic_time(woke_at, || bo.settle());
    assert_eq!(
        ui::time::testing::with_monotonic_time(woke_at, || says_typing(&mut bo)),
        None,
        "at that wake Bo no longer reads Ada typing: {:?}",
        bo.shown()
    );
}
