//! The box beside a menu: where it stands at the window's edges, the order
//! the keyboard and a screen reader visit it in, and that sending it makes
//! one request the session acts on.

use super::*;
use crate::widget::{Edits, EventCtx, TaffyTreeExt};
use winit::event::{Modifiers, WindowEvent};
use winit::keyboard::NamedKey;

/// A box opened about a Task, beside `menu` in `viewport`.
fn box_beside(menu: Rect, viewport: Rect) -> SendToBox {
    let _ = crate::shared_selection::take_send_to();
    let _ = crate::shared_selection::take_recipient_searches();
    crate::shared_selection::set_send_host_available(true);
    let mut send_to = SendToBox::new();
    send_to.set_viewport(Some(viewport));
    send_to.set_about(Some(
        SharedSelection::of_text("Invoice from Ada")
            .at(tilo_actions::SelectionPlace::item("row-3")),
    ));
    send_to.open(None);
    send_to.place(menu, Some(viewport));
    send_to
}

/// Give `send_to` an event, with no modifiers held.
fn send(send_to: &mut SendToBox, event: &WindowEvent) -> bool {
    send_held(send_to, event, Modifiers::default())
}

/// Give `send_to` a key with `modifiers` held.
fn key(send_to: &mut SendToBox, key: Key, modifiers: Modifiers) -> bool {
    let event = crate::synthetic_input::key_event(key, None, winit::event::ElementState::Pressed);
    send_held(
        send_to,
        &crate::synthetic_input::keyboard_input(event),
        modifiers,
    )
}

/// Give `send_to` an event with `modifiers` held.
fn send_held(send_to: &mut SendToBox, event: &WindowEvent, modifiers: Modifiers) -> bool {
    let mut taffy = taffy::TaffyTree::<()>::new();
    let node = taffy.leaf_node(taffy::Style::default());
    let mut doc = automerge::AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    send_to.handle_event(
        event,
        &mut EventCtx {
            scale_factor: 1.0,
            modifiers: &modifiers,
            state: Edits::lend(&mut doc),
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy: &taffy,
        },
    );
    handled
}

fn tab(send_to: &mut SendToBox, shift: bool) -> bool {
    let modifiers = if shift {
        Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
    } else {
        Modifiers::default()
    };
    key(send_to, Key::Named(NamedKey::Tab), modifiers)
}

/// Type `text` into the field the keyboard is in.
fn type_text(send_to: &mut SendToBox, text: &str) {
    for ch in text.chars() {
        send(send_to, &crate::synthetic_input::char_key(&ch.to_string()));
    }
}

#[test]
fn the_box_stands_beside_its_menu_and_never_leaves_the_window() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    // Room on its trailing side: it keeps the menu's top.
    let room = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    assert_eq!(room.rect().x0, 212.0 + f64::from(GAP));
    assert_eq!(room.rect().y0, 120.0, "level with what opened it");

    // Near the window's right edge: it takes the leading side instead.
    let trailing = box_beside(Rect::new(560.0, 120.0, 792.0, 320.0), viewport);
    assert!(
        trailing.rect().x1 <= 560.0,
        "no room on the trailing side: the box stands on the leading one, {}",
        trailing.rect()
    );
    assert_eq!(trailing.rect().y0, 120.0);

    // A window too narrow for either side: it stays inside, over the menu
    // where it must, rather than off the screen.
    let narrow = box_beside(
        Rect::new(150.0, 120.0, 250.0, 320.0),
        Rect::new(0.0, 0.0, 400.0, 600.0),
    );
    assert!(
        narrow.rect().x0 >= 0.0 && narrow.rect().x1 <= 400.0,
        "inside the window: {}",
        narrow.rect()
    );

    // At the bottom: it moves up to keep its whole height in the window.
    let low = box_beside(
        Rect::new(40.0, 520.0, 212.0, 596.0),
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
    assert!(
        low.rect().y1 <= 600.0 && low.rect().y0 >= 0.0,
        "the whole box stays in the window: {}",
        low.rect()
    );
}

#[test]
fn the_keyboard_visits_the_recipients_the_subjects_the_message_and_send() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    crate::shared_selection::answer_recipients(
        send_to.request,
        vec![
            Recipient::found(RecipientKind::Agent, "agent:7", "Ada").with_detail("agent"),
            Recipient::found(RecipientKind::Person, "person:ada", "Ada Lovelace"),
        ],
    );
    assert!(send_to.drain_answer(), "the search answered this box");
    assert_eq!(send_to.rows().len(), 2);
    assert_eq!(
        send_to.hovered_row(),
        Some(0),
        "the first row is the keyboard's"
    );

    let node = send_to.accessibility().expect("an open box is read");
    assert_eq!(node.role, Role::Dialog);
    assert_eq!(
        node.name.as_deref(),
        Some(crate::i18n::I18nManager::lookup_plain("send-to-name").as_str()),
        "the box is named for what it does"
    );
    let targets: Vec<&str> = node
        .children
        .iter()
        .filter_map(|child| child.action_target.as_deref())
        .collect();
    assert_eq!(
        targets,
        [
            "send-to:recipients",
            "send-to:recipient:0",
            "send-to:recipient:1",
            "send-to:add-subject",
            "send-to:message",
            "send-to:send",
        ],
        "in the order the keyboard visits them"
    );
    assert_eq!(
        node.children
            .iter()
            .find(|child| child.role == Role::StaticText)
            .and_then(|child| child.name.clone()),
        Some("Invoice from Ada".to_owned()),
        "the subject is read where it stands"
    );

    // Typing asks the one search for what matches, and the answer is what
    // the rows show.
    send_to.focus_first();
    type_text(&mut send_to, "ad");
    assert!(
        send_to.rows().is_empty(),
        "what was found for an earlier query is not what this one finds"
    );
    let (request, query) = crate::shared_selection::take_recipient_searches()
        .pop()
        .expect("the box asked the search");
    assert_eq!(query, "ad");
    crate::shared_selection::answer_recipients(
        request,
        vec![Recipient::found(RecipientKind::Agent, "agent:4", "Adrien")],
    );
    assert!(send_to.drain_answer());
    assert_eq!(send_to.hovered_row(), Some(0), "Enter would pick it");

    // Tab: the recipient field, the message, then the menu's own stops.
    assert_eq!(send_to.focus(), SendToFocus::Recipients);
    assert!(tab(&mut send_to, false));
    assert_eq!(send_to.focus(), SendToFocus::Message);
    assert!(tab(&mut send_to, false));
    assert!(!send_to.is_focused(), "past the last stop the menu has it");
    send_to.focus_first();
    assert!(tab(&mut send_to, true));
    assert_eq!(
        send_to.focus(),
        SendToFocus::Message,
        "Shift with Tab steps back to the message"
    );
}

#[test]
fn send_makes_one_request_as_the_person_with_what_the_box_holds() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    assert_eq!(
        send_to.recipients().len(),
        1,
        "a box opens on the agent it makes"
    );
    assert_eq!(send_to.recipients()[0].kind, RecipientKind::NewAgent);
    assert_eq!(send_to.subjects().len(), 1, "about what the menu acted on");

    // Nothing typed: Send makes no request.
    send_to.send();
    assert!(crate::shared_selection::take_send_to().is_empty());

    send_to.text = "Can you look at this?".to_owned();
    send_to.send();
    let requests = crate::shared_selection::take_send_to();
    assert_eq!(requests.len(), 1, "one request, one action");
    assert_eq!(requests[0].to.len(), 1);
    assert_eq!(requests[0].about.len(), 1);
    assert_eq!(requests[0].text, "Can you look at this?");
    assert_eq!(
        requests[0].bounds,
        Some(crate::floating_surface::protocol_rect_array(send_to.rect())),
        "the conversation opens where the box stood"
    );
    assert!(send_to.take_sent());
    assert!(!send_to.take_sent(), "sent once");

    // A message typed into the box, whether by the person's keys or the
    // menu's sentence, is one request per Send.
    send_to.text = "And this too".to_owned();
    send_to.send();
    assert_eq!(crate::shared_selection::take_send_to().len(), 1);
}
