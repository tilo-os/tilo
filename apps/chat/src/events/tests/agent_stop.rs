//! The stop of an agent's work: who is offered it where the box sends and in the reply's own
//! menu, what pressing it invokes, and what the reply says after.

use super::*;
use ui::Widget;

/// The reply the demo's agent works on.
fn reply() -> String {
    crate::testing::agent_reply()
}

/// A window of the demo's person on the conversation in which their agent is at work, drawn.
fn working() -> (ChatApp, crate::testing::Workspace) {
    let (mut app, mut doc) = app_with_doc();
    stage_agent_work(&mut app, &mut doc);
    read_again(&mut app, &mut doc);
    (app, doc)
}

/// Read what the conversation holds now, and draw it.
fn read_again(app: &mut ChatApp, doc: &mut crate::testing::Workspace) {
    app.land(doc);
    app.sync(&doc.chat, &automerge::ROOT);
    render_frame(app, 1600.0, 900.0);
}

/// Where the control `id` stands in the frame last drawn.
fn control(app: &ChatApp, id: &str) -> Option<vello::kurbo::Rect> {
    app.semantic_hits()
        .into_iter()
        .find_map(|(rect, hit, ..)| (hit.semantic_id() == id).then_some(rect))
}

/// The node called `name` in what assistive technology is told.
fn told(app: &ChatApp, name: &str) -> Option<ui::a11y::AccessNode> {
    fn within(node: ui::a11y::AccessNode, name: &str) -> Option<ui::a11y::AccessNode> {
        if node.name.as_deref() == Some(name) {
            return Some(node);
        }
        node.children
            .into_iter()
            .find_map(|child| within(child, name))
    }
    within(app.accessibility()?, name)
}

/// What the agent's reply says of its work, as assistive technology reads it.
fn said_of_the_work(app: &ChatApp) -> String {
    told(app, "Tide: ")
        .expect("the agent's reply is told")
        .description
        .unwrap_or_default()
}

/// The place of the row called `label` in the menu that is open.
fn menu_row(app: &ChatApp, label: &str) -> Option<usize> {
    let menu = app.menu.as_ref().expect("the menu is open");
    menu.widget
        .items()
        .iter()
        .position(|row| row.shown_label().as_deref() == Some(label))
}

/// Chat's `stop-agent` on `message`.
fn stop_of(message: &str) -> ui::tilo_resource::Invoke<crate::parts::ChatPart> {
    crate::parts::ChatPart::STOP_AGENT.with(crate::messages::chat_part::StopAgent {
        message: ui::tilo_resource::Id::from_raw(message),
    })
}

/// Whether the last action this window invoked on a message is Chat's `stop-agent` on `message`.
fn stop_invoked_on(app: &ChatApp, message: &str) -> bool {
    matches!(
        app.dispatched_parts.borrow().last(),
        Some(crate::messages::ChatPartAction::StopAgent(
            crate::messages::chat_part::StopAgent { message: named }
        )) if named.as_str() == message
    )
}

/// The stop the message that asked holds, as the conversation's parts keep it.
fn stop_mark(doc: &crate::testing::Workspace) -> Option<crate::summons::Stopped> {
    doc.messages()
        .iter()
        .find(|(id, _)| id.as_str() == crate::resource::DEMO_SUMMONS)
        .and_then(|(_, message)| message.stopped.clone())
}

/// While their agent is at work and nothing is typed, the person it works for is offered its
/// stop where the box offers send; what they type instead is sent.
#[test]
fn the_box_offers_its_person_the_stop_where_it_offers_send() {
    let (mut app, mut doc) = working();
    let stop =
        control(&app, "chat.stop").expect("the person their agent works for is offered its stop");
    assert_eq!(
        control(&app, "chat.send"),
        None,
        "it stands where the send would"
    );
    assert!(
        control(&app, "chat.thread-stop").is_some(),
        "the open thread's box offers it too"
    );
    assert!(
        told(&app, "Stop the answer").is_some(),
        "and says what it does"
    );

    app.focus = Focus::Main;
    app.set_composer_focus();
    type_str(&mut app, &mut doc, "wait");
    render_frame(&mut app, 1600.0, 900.0);
    assert_eq!(
        control(&app, "chat.stop"),
        None,
        "with words typed the box sends them"
    );
    assert_eq!(
        control(&app, "chat.send"),
        Some(stop),
        "from the same place"
    );
}

/// Nobody else is offered it: another member of the conversation sees the send as ever, Chat
/// refuses an invocation of theirs, and the turn goes on. Once its person stopped it, the reply
/// tells them who did.
#[test]
fn another_member_is_offered_no_stop_and_is_refused() {
    let (mut app, mut doc) = working();
    app.me = crate::identity::LocalIdentity {
        id: MemberId::person("u_aria"),
        name: "Aria".into(),
        initials: "A".into(),
    };
    app.former_ids.clear();
    render_frame(&mut app, 1600.0, 900.0);
    assert_eq!(control(&app, "chat.stop"), None, "the conversation's box");
    assert_eq!(control(&app, "chat.thread-stop"), None, "the thread's box");
    assert!(control(&app, "chat.send").is_some(), "they send as ever");
    app.open_message_menu(reply(), Point::new(120.0, 120.0));
    assert_eq!(
        menu_row(&app, "Stop the answer"),
        None,
        "the reply's menu offers them none"
    );
    app.menu = None;

    assert!(
        doc.apply_part_as(&MemberId::person("u_aria"), stop_of(&reply()))
            .is_err(),
        "Chat refuses another member's invocation"
    );
    read_again(&mut app, &mut doc);
    assert_eq!(stop_mark(&doc), None, "and the turn goes on");
    assert!(
        said_of_the_work(&app).contains("Add function: Revenue curve"),
        "{}",
        said_of_the_work(&app)
    );

    doc.apply_part_as(&MemberId::person(model::ME), stop_of(&reply()))
        .expect("its person stops it");
    read_again(&mut app, &mut doc);
    let said = said_of_the_work(&app);
    assert!(
        said.starts_with("Stopped by ") && !said.contains("Stopped by you"),
        "the reply tells another member who stopped it: {said}"
    );
}

/// A reply nobody is at work on offers no stop: a conversation without an agent's turn, and a
/// turn that ended by itself. A stop invoked after that end is taken and changes nothing a
/// reader meets.
#[test]
fn a_reply_that_is_not_going_offers_no_stop() {
    let (mut plain, _doc) = app_with_doc();
    render_frame(&mut plain, 1600.0, 900.0);
    assert_eq!(
        control(&plain, "chat.stop"),
        None,
        "no agent is at work here"
    );

    let (mut app, mut doc) = working();
    assert!(
        control(&app, "chat.stop").is_some(),
        "while the agent works its person is offered the stop"
    );
    doc.apply_part_as(
        &crate::resource::demo_agent(),
        crate::testing::record_work(
            crate::resource::DEMO_SUMMONS,
            &crate::model::AgentWork::new(
                crate::model::AgentWorkState::Done {
                    did: "Revenue curve added".into(),
                },
                crate::demo::NOW,
            ),
        ),
    )
    .expect("the agent ends its work");
    read_again(&mut app, &mut doc);
    assert_eq!(control(&app, "chat.stop"), None, "the turn ended by itself");
    assert_eq!(control(&app, "chat.thread-stop"), None);
    assert!(control(&app, "chat.send").is_some());
    app.open_message_menu(reply(), Point::new(120.0, 120.0));
    assert_eq!(menu_row(&app, "Stop the answer"), None);
    app.menu = None;
    let before = said_of_the_work(&app);

    doc.apply_part_as(&MemberId::person(model::ME), stop_of(&reply()))
        .expect("a stop that comes after the end is taken");
    read_again(&mut app, &mut doc);
    assert_eq!(
        said_of_the_work(&app),
        before,
        "and the reply says what it said"
    );
    assert_eq!(control(&app, "chat.stop"), None);
}

/// The reply's own menu offers its person the stop too, and choosing it invokes Chat's
/// `stop-agent` on that reply.
#[test]
fn the_replys_menu_offers_its_person_the_stop() {
    let (mut app, mut doc) = working();
    app.open_message_menu(reply(), Point::new(120.0, 120.0));
    let row =
        menu_row(&app, "Stop the answer").expect("the reply's menu offers its person the stop");
    assert!(ui::Widget::perform_semantic_action(
        &mut app,
        &format!("context-menu:item:{row}"),
        &ui::a11y::Action::Activate,
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
    ));
    assert!(
        stop_invoked_on(&app, &reply()),
        "choosing it invokes stop-agent on the reply"
    );
    read_again(&mut app, &mut doc);
    let said = said_of_the_work(&app);
    assert!(said.contains("Stopped by you"), "{said}");

    // The message that asked is its person's own: its menu holds no stop of anything.
    app.open_message_menu(
        crate::resource::DEMO_SUMMONS.into(),
        Point::new(120.0, 120.0),
    );
    assert_eq!(menu_row(&app, "Stop the answer"), None);
}

/// Their press invokes Chat's `stop-agent` on the reply. The reply stays, says its person
/// stopped it and holds what it held; the box is back at send; a second stop changes nothing.
#[test]
fn a_press_stops_the_work_and_the_reply_says_its_person_stopped_it() {
    let (mut app, mut doc) = working();
    let stop =
        control(&app, "chat.stop").expect("the person their agent works for is offered its stop");
    let held = said_of_the_work(&app);
    assert!(held.contains("Requested: Add function"), "{held}");

    click_point(&mut app, &mut doc, stop.center());
    assert!(
        stop_invoked_on(&app, &reply()),
        "the press invokes stop-agent on the reply"
    );
    read_again(&mut app, &mut doc);
    assert_eq!(control(&app, "chat.stop"), None, "the box is back at send");
    assert_eq!(control(&app, "chat.send"), Some(stop), "in the same place");
    assert_eq!(control(&app, "chat.thread-stop"), None);
    let said = said_of_the_work(&app);
    assert!(
        said.contains("Stopped by you"),
        "the reply says its person stopped it: {said}"
    );
    assert!(
        said.contains("Requested: Add function"),
        "and holds what it held: {said}"
    );
    let stood = stop_mark(&doc);
    assert!(stood.is_some(), "the stop stands on the message that asked");

    // Stopped again, by a press that comes late or by another invocation: nothing changes.
    click_point(&mut app, &mut doc, stop.center());
    doc.apply_part_as(&MemberId::person(model::ME), stop_of(&reply()))
        .expect("a second stop is taken");
    read_again(&mut app, &mut doc);
    assert_eq!(stop_mark(&doc), stood, "a second stop changes nothing");
    assert_eq!(said_of_the_work(&app), said);
    assert_eq!(control(&app, "chat.stop"), None);
}

/// What is offered is read from the conversation, not kept by a window: a second window of the
/// same person offers the stop while the first does and loses it when the first stops the work.
#[test]
fn a_second_window_of_the_person_offers_and_loses_the_stop_with_the_first() {
    let (mut app, mut doc) = working();
    let mut second = ChatApp::new_ephemeral();
    second.attach_workspace(&doc);
    second.reload(&doc);
    second.stage_scenario("agent-work");
    render_frame(&mut second, 1600.0, 900.0);
    assert!(
        control(&second, "chat.stop").is_some(),
        "the second window offers it too"
    );

    let stop = control(&app, "chat.stop").expect("the first window offers it");
    click_point(&mut app, &mut doc, stop.center());
    read_again(&mut app, &mut doc);
    second.reload(&doc);
    render_frame(&mut second, 1600.0, 900.0);
    assert_eq!(
        control(&second, "chat.stop"),
        None,
        "and loses it when the first stops the work"
    );
    assert!(control(&second, "chat.send").is_some());
    assert!(
        said_of_the_work(&second).contains("Stopped by you"),
        "{}",
        said_of_the_work(&second)
    );
}

/// A stop the workspace does not take is said in words on the control it was pressed at, and
/// the turn goes on.
#[test]
fn a_refused_stop_is_said_where_it_was_pressed() {
    let (mut app, mut doc) = working();
    let refused = app
        .chain
        .as_ref()
        .expect("the window holds the messages")
        .dispatcher()
        .refused();
    refused.set(Some(ui::framework::Refused {
        action: "stop-agent".into(),
        item: None,
        reason: "only the person an agent works for stops its work".into(),
        by_workspace: true,
    }));
    read_again(&mut app, &mut doc);
    assert!(control(&app, "chat.stop").is_some(), "the turn goes on");
    let said = told(&app, "Stop the answer")
        .expect("the stop is still offered")
        .description
        .unwrap_or_default();
    assert_eq!(
        said,
        "Not stopped: only the person an agent works for stops its work"
    );
}

/// Pictures for a review, written only where `TILO_UI_SCREENSHOT_DIR` names a folder: the
/// conversation while the person's agent works and after they stopped it, in both themes, in a
/// wide window and a narrow one.
#[test]
fn pictures_of_the_stop_offered_and_taken() {
    let Some(directory) = std::env::var_os("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for scenario in ["agent-work", "agent-stopped"] {
        for (size, viewport) in [("wide", (1280, 800)), ("narrow", (420, 800))] {
            for (name, theme) in [
                ("dark", ui::theme::Theme::dark()),
                ("light", ui::theme::Theme::light()),
            ] {
                let mut shown = ui::framework::testing::TestApp::<crate::ChatShell>::capture_at(
                    Some(scenario),
                    viewport,
                )
                .unwrap();
                shown.harness().set_theme(theme);
                shown
                    .harness()
                    .render_png(&directory.join(format!("{scenario}-{name}-{size}.png")));
            }
        }
    }
}
