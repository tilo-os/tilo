//! A window as narrow as a phone's: one column in front at a time, which a person's hand slides in
//! and an open nobody's hand made puts in front at once.

use super::{CHAT, NOTES, Session, shown_at};

/// A person at Chat in a window 320 px wide (a phone's), with a channel `design` holding
/// `messages` messages `bo` wrote while they were away: nothing of it is theirs, so all of it
/// waits to be read. The channel is open, at its newest message.
fn chat_at_a_phones_width(messages: usize) -> Session {
    let mut session = Session::in_window(CHAT, "ada", (320, 568));
    let new_channel = session.identity_doing("Conversations", "new_channel");
    let channel = match session.invoke_doing(&new_channel, &serde_json::json!({ "name": "design" }))
    {
        ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => output
            .resources
            .into_iter()
            .next()
            .expect("the channel it made"),
        other => panic!("the channel was made: {other:?}"),
    };
    // Sending is the app's own: an action of no one document, given the conversation it writes in.
    let send = session.published_doing(&format!("tl.{CHAT}"), "send");
    let to = serde_json::json!({
        "app": CHAT,
        "kind": session.kind("TextChannel"),
        "id": channel,
        "label": "",
    });
    for said in 0..messages {
        let written = session.invoke_as(
            &ui::tilo_actions::MemberId::person("bo").into(),
            &send,
            &serde_json::json!({ "to": to, "body": format!("Message {said}") }),
        );
        assert!(
            matches!(
                written,
                ui::tilo_actions::InvocationUpdate::Succeeded { .. }
            ),
            "bo's message was written: {written:?}"
        );
    }
    let (x, y) = shown_at(&mut session, "design");
    session.window.click_at(x, y);
    session.settle();
    session
}

/// What the way back over a conversation is in a window drawn at a phone's width: a control
/// saying the words the pill shows, where the pill is drawn, which a reader, an agent or
/// a keyboard takes to the first message the person has not seen.
#[test]
fn a_conversations_way_back_is_a_control_that_goes_to_the_first_message_unread() {
    use ui::widget::jump::Jump;

    let _settled = ui::motion::testing::settled();
    let mut session = chat_at_a_phones_width(40);
    let words = Jump::Unread.label();
    let pill = session
        .window
        .harness()
        .a11y_node_named(&words)
        .unwrap_or_else(|| {
            panic!(
                "the window offers the way back: {:?}",
                session
                    .controls()
                    .into_iter()
                    .map(|control| control.name)
                    .collect::<Vec<_>>()
            )
        });
    let shown = session
        .window
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(id, _)| *id == pill)
        .and_then(|(_, node)| node.bounds())
        .expect("the way back stands where it is drawn");
    assert!(
        shown.x0 >= 0.0 && shown.x1 <= 320.0 && shown.y0 > 0.0 && shown.y1 <= 568.0,
        "the way back stands inside the window: {shown:?}"
    );
    assert!(
        shown.x1 - shown.x0 > 80.0,
        "its words are whole at 320 px: {shown:?}"
    );
    assert!(
        session
            .controls()
            .iter()
            .any(|control| control.name == words),
        "the window's controls name the way back: {:?}",
        session
            .controls()
            .into_iter()
            .map(|control| control.name)
            .collect::<Vec<_>>()
    );

    // An agent presses it by its node, as a reader takes it, and the person is where reading
    // starts again.
    session
        .window
        .harness()
        .perform_a11y_action(pill, &ui::a11y::Action::Activate)
        .expect("the way back takes the press");
    session.settle();
    assert!(
        session.window.harness().a11y_node_named(&words).is_none(),
        "taken, the way back offers nothing more"
    );
    let first = session
        .window
        .harness()
        .a11y_nodes()
        .into_iter()
        .find_map(|(_, node)| {
            let name = node.label()?;
            name.contains("Message 0").then(|| node.bounds()).flatten()
        })
        .expect("the first message is read in the window");
    assert!(
        first.y0 >= 0.0 && first.y1 <= 568.0,
        "the way back went to the first message unread: {first:?}"
    );
}

/// A note a search hit names, opened in a window as narrow as a phone's, comes in front at once:
/// the hit's `open` reaches the instance, and nobody's hand moved the window, so no push plays.
#[test]
fn a_note_opened_from_search_in_a_narrow_window_lands_in_front_at_once() {
    let mut session = Session::in_window(NOTES, "ada", (420, 860));
    let file = session.identity_doing("Notebook", "file_note");
    let filed = session.invoke_doing(
        &file,
        &serde_json::json!({ "title": "Groceries", "tag": "home" }),
    );
    assert!(
        matches!(filed, ui::tilo_actions::InvocationUpdate::Succeeded { .. }),
        "the note was filed: {filed:?}"
    );
    session.settle();
    assert!(
        !session
            .window
            .harness()
            .targets()
            .contains_key("nav-shell-back"),
        "the list is in front"
    );
    // What a hit of the note names, as search reads the workspace's store: the window itself
    // publishes nothing.
    session.assert_no_index_delta();
    let anchor = tilolang_tilo::search::whole(&session.instance_host())
        .into_iter()
        .find(|entry| entry.title.contains("Groceries"))
        .map(|entry| entry.resource)
        .expect("search finds the note");
    session.instance_mut().reveal(anchor);
    // The first frame that shows the note shows it where it rests: Back stands in its place,
    // and the frame after it lays out that last pose and asks for no more.
    let mut frames = 0;
    let back = loop {
        frames += 1;
        assert!(frames <= 8, "the hit's note comes in front");
        session.settle_host();
        session.window.harness().poll_frame().expect("a frame");
        if let Some(back) = session.window.harness().targets().get("nav-shell-back") {
            break *back;
        }
    };
    session.window.harness().poll_frame().expect("a frame");
    assert_eq!(
        session.window.harness().targets().get("nav-shell-back"),
        Some(&back),
        "nobody's hand moved it, so it lands at once"
    );
    assert!(
        !session
            .window
            .harness()
            .frame_schedule()
            .needs_animation_frame,
        "the note stands in front at rest: {:?}, asked by {:?}",
        session.window.harness().frame_schedule(),
        session.window.harness().frame_requesters()
    );
}

// USER REQUIREMENT: a note opens through its own `open` — the resource path a search hit, a
// link or an agent takes — and Back shows the tree it was opened from again.
#[test]
fn a_note_opens_by_its_own_open_and_back_shows_the_tree_again() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::in_window(NOTES, "ada", (420, 860));
    let file = session.identity_doing("Notebook", "file_note");
    let filed = session.invoke_doing(
        &file,
        &serde_json::json!({ "title": "Groceries", "tag": "home" }),
    );
    let note = match filed {
        ui::tilo_actions::InvocationUpdate::Succeeded { output, .. } => output
            .resources
            .into_iter()
            .next()
            .expect("the note it filed"),
        other => panic!("the note was filed: {other:?}"),
    };
    session.settle();
    assert!(
        !session
            .window
            .harness()
            .targets()
            .contains_key("nav-shell-back"),
        "the tree is in front, with nothing over it"
    );

    // Its own `open`, through the action API on the resource the app publishes: the note
    // stands in front.
    let opened = session.invoke(None, None, &note, "open", &serde_json::Map::new());
    assert!(opened.is_ok(), "the note's open answers: {opened:?}");
    session.settle();
    assert!(
        session
            .window
            .harness()
            .targets()
            .contains_key("nav-shell-back"),
        "the note stands in front of the tree, with Back to it"
    );

    // Back shows the tree again, the note's row in it.
    session
        .window
        .harness()
        .click_named("nav-shell-back")
        .expect("Back is shown");
    session.settle();
    assert!(
        !session
            .window
            .harness()
            .targets()
            .contains_key("nav-shell-back"),
        "the tree is in front again"
    );
    assert!(
        session
            .window
            .harness()
            .a11y_nodes()
            .into_iter()
            .any(|(_, node)| node
                .label()
                .is_some_and(|label| label.contains("Groceries"))),
        "the tree lists the note it was opened from"
    );
}
