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
    send_to.open(None, &[]);
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
    assert_eq!(
        room.rect().x0,
        212.0 + f64::from(Theme::dark().tokens.spacing.s2)
    );
    assert_eq!(room.rect().y0, 120.0, "level with what opened it");

    // Near the window's right edge: it takes the leading side instead.
    let trailing = box_beside(Rect::new(560.0, 120.0, 792.0, 320.0), viewport);
    assert!(
        trailing.rect().x1 <= 560.0,
        "no room on the trailing side: the box stands on the leading one, {}",
        trailing.rect()
    );
    assert_eq!(trailing.rect().y0, 120.0);

    // A window too narrow for either side: it stands under the menu, clear of
    // what the menu is about, and inside the window.
    let narrow = box_beside(
        Rect::new(150.0, 120.0, 250.0, 320.0),
        Rect::new(0.0, 0.0, 400.0, 600.0),
    );
    assert!(
        narrow.rect().x0 >= 0.0 && narrow.rect().x1 <= 400.0,
        "inside the window: {}",
        narrow.rect()
    );
    assert!(
        narrow.rect().y0 >= 320.0 && narrow.rect().y1 <= 600.0,
        "under the menu, over nothing it is about: {}",
        narrow.rect()
    );

    // No room anywhere: it still stays inside the window, over what it is about.
    let tight = box_beside(
        Rect::new(150.0, 20.0, 250.0, 160.0),
        Rect::new(0.0, 0.0, 400.0, 200.0),
    );
    assert!(
        tight.rect().x0 >= 0.0 && tight.rect().x1 <= 400.0 && tight.rect().y0 >= 0.0,
        "inside the window: {}",
        tight.rect()
    );
    // A menu at the window's trailing edge with a little room before it: the
    // box covers as little of it as the window allows.
    let edge = box_beside(
        Rect::new(300.0, 100.0, 460.0, 560.0),
        Rect::new(0.0, 0.0, 480.0, 600.0),
    );
    assert!(
        (edge.rect().x0 - 0.0).abs() < f64::EPSILON,
        "it takes the side it covers least of the menu on: {}",
        edge.rect()
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
    assert!(
        send_to.rows().is_empty(),
        "an empty recipient field shows nobody to choose from"
    );
    assert!(
        crate::shared_selection::take_recipient_searches().is_empty(),
        "and asks the search nothing until the person types"
    );
    send_to.focus_first();
    type_text(&mut send_to, "a");
    let (request, _) = crate::shared_selection::take_recipient_searches()
        .pop()
        .expect("the box asked the search");
    crate::shared_selection::answer_recipients(
        request,
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
            "send-to:message",
            "send-to:send",
        ],
        "in the order the keyboard visits them; adding a subject stands there only while the window holds something to add"
    );
    let read: Vec<&str> = node
        .children
        .iter()
        .filter(|child| child.role == Role::StaticText)
        .filter_map(|child| child.name.as_deref())
        .collect();
    assert_eq!(
        read,
        [
            crate::i18n::I18nManager::lookup_plain("send-to-new-agent").as_str(),
            "Invoice from Ada"
        ],
        "whom it goes to and the subject are read where they stand"
    );
    let field = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("send-to:recipients"))
        .expect("the field that adds someone is read");
    assert_eq!(
        field.name.as_deref(),
        Some(crate::i18n::I18nManager::lookup_plain("send-to-recipients-name").as_str()),
        "named as it reads"
    );
    assert_eq!(
        field.value.as_deref(),
        Some("a"),
        "holding what is typed into it"
    );

    // Typing asks the one search for what matches, and the answer is what
    // the rows show.
    send_to.focus_first();
    type_text(&mut send_to, "d");
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

/// Enter on a recipient picked from the search goes on to the message: the
/// words typed next are the message, not a new search for a person.
#[test]
fn picking_a_recipient_leaves_the_keyboard_in_the_message() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    send_to.focus_first();
    type_text(&mut send_to, "ada");
    let (request, _) = crate::shared_selection::take_recipient_searches()
        .pop()
        .expect("the box asked the search");
    crate::shared_selection::answer_recipients(
        request,
        vec![Recipient::found(
            RecipientKind::Person,
            "person:ada",
            "Ada Lovelace",
        )],
    );
    assert!(send_to.drain_answer());

    assert!(key(
        &mut send_to,
        Key::Named(NamedKey::Enter),
        Modifiers::default()
    ));
    assert_eq!(
        send_to.focus(),
        SendToFocus::Message,
        "a recipient picked is a message about to be written"
    );
    type_text(&mut send_to, "look");
    assert_eq!(send_to.text, "look", "the words typed are the message");
}

#[test]
fn a_recipient_chip_leads_with_what_it_is_and_carries_the_x_that_takes_it_off() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    assert_eq!(send_to.layout.chips.len(), 1, "the agent a message makes");
    assert!(
        send_to.layout.removes[0].is_none(),
        "the default recipient stays: the box always has somewhere to send"
    );

    // A name typed into the field makes an agent of that name, which is not
    // the default and comes off by its ×.
    send_to.focus_first();
    type_text(&mut send_to, "Ada's agent");
    let _ = crate::shared_selection::take_recipient_searches();
    send_to.pick_or_name();
    assert_eq!(send_to.recipients().len(), 1);
    assert_eq!(send_to.recipients()[0].kind, RecipientKind::NewAgent);
    assert_eq!(send_to.recipients()[0].name, "Ada's agent");
    let remove = send_to.layout.removes[0].expect("an agent made by name is taken off");
    assert!(send_to.press(remove.center()), "the × is in the box");
    assert!(
        send_to.recipients().is_empty(),
        "the chip is off the message, and the box asks whom it goes to"
    );
}

#[test]
fn a_subject_chip_reads_what_was_chosen_without_the_marks_of_a_list() {
    let one =
        SharedSelection::of_items([("row-1", "#1  Ship the release")]).in_kind("tasks.tracker");
    assert_eq!(
        subject_label(&one),
        "#1 Ship the release",
        "the chip reads the task, not the list's bullet"
    );
    assert_eq!(
        subject_glyph(&one),
        crate::icons::Icon::ListChecks,
        "and leads with the glyph of the app that holds it"
    );

    let many =
        SharedSelection::of_items([("row-1", "#1  Ship the release"), ("row-2", "#2  Tell Ada")])
            .in_kind("tasks.tracker");
    assert_eq!(
        subject_label(&many),
        "#1 Ship the release +1",
        "the rest of the selection is counted, not spelled out"
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

    send_to.text = "Can you look at this?".to_owned();
    send_to.send();
    let requests = crate::shared_selection::take_send_to();
    assert_eq!(requests.len(), 1, "one request, one action");
    assert_eq!(requests[0].to.len(), 1);
    assert_eq!(
        requests[0].to[0].name, "",
        "the new agent nobody named is named by the shell from what it is asked, never \
         every one \"New agent\""
    );
    assert_eq!(requests[0].about.len(), 1);
    assert_eq!(requests[0].text, "Can you look at this?");
    assert!(send_to.take_sent());
    assert!(!send_to.take_sent(), "sent once");

    // A message typed into the box, whether by the person's keys or the
    // menu's sentence, is one request per Send.
    send_to.text = "And this too".to_owned();
    send_to.send();
    assert_eq!(crate::shared_selection::take_send_to().len(), 1);
}

/// What a message sent with no words at all carries: the subject is the
/// message, so a task handed to someone goes without a sentence, and a box
/// with neither words nor a subject has nothing to send.
#[test]
fn a_subject_alone_is_something_to_send() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    assert_eq!(send_to.recipients().len(), 1, "the agent a message makes");
    assert_eq!(send_to.subjects().len(), 1, "about what the menu acted on");
    assert!(
        send_to.can_send(),
        "a task sent to someone is the message: the words are optional"
    );

    send_to.send();
    let requests = crate::shared_selection::take_send_to();
    assert_eq!(requests.len(), 1, "the subject alone sends");
    assert_eq!(requests[0].text, "", "with no words beside it");
    assert_eq!(requests[0].about.len(), 1, "and the task it is about");
    assert!(send_to.take_sent());

    send_to.subjects.clear();
    assert!(
        !send_to.can_send(),
        "a box with neither words nor a subject sends nothing"
    );
    send_to.send();
    assert!(crate::shared_selection::take_send_to().is_empty());
}

/// A box the session opened to the one place a report goes: its chip stands
/// there on its own, nobody is searched for, the chip is not taken off, and
/// what the person writes goes to it — attached to what the window picked.
#[test]
fn a_box_opened_to_a_destination_keeps_it_and_searches_for_nobody() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let _ = crate::shared_selection::take_send_to();
    let _ = crate::shared_selection::take_recipient_searches();
    crate::shared_selection::set_send_host_available(true);
    let destination = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
    let mut send_to = SendToBox::new();
    send_to.set_viewport(Some(viewport));
    send_to.set_about(Some(SharedSelection::of_text("Invoice from Ada")));
    send_to.open(None, std::slice::from_ref(&destination));
    send_to.place(Rect::new(40.0, 120.0, 212.0, 320.0), Some(viewport));

    assert!(send_to.addressed());
    assert_eq!(send_to.recipients(), std::slice::from_ref(&destination));
    assert_eq!(
        send_to.focus(),
        SendToFocus::Message,
        "the words are all that is left to write"
    );
    assert!(
        crate::shared_selection::take_recipient_searches().is_empty(),
        "a box that knows where it sends asks the search nothing"
    );
    assert!(send_to.perform_semantic_action(
        "send-to:recipients",
        &crate::a11y::Action::SetValue("ada".to_owned())
    ));
    assert!(
        crate::shared_selection::take_recipient_searches().is_empty(),
        "and finds nobody when something is typed there"
    );
    send_to.focus_first();
    assert_eq!(send_to.focus(), SendToFocus::Message, "still the message");

    type_text(&mut send_to, "The invoice button does nothing");
    send_to.send();
    let requests = crate::shared_selection::take_send_to();
    assert_eq!(requests.len(), 1, "one request, one report");
    assert_eq!(requests[0].to, [destination], "to the one place it goes");
    assert_eq!(requests[0].text, "The invoice button does nothing");
    assert_eq!(requests[0].about.len(), 1, "with what the window picked");
    assert!(send_to.take_sent());
}

/// The message wraps at the box's width and the box makes room for it: a
/// sentence longer than the field is read whole, in lines, never run past the
/// card sideways.
#[test]
fn the_message_wraps_and_the_box_makes_room_for_it() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 900.0);
    let menu = Rect::new(40.0, 120.0, 212.0, 320.0);
    let mut send_to = box_beside(menu, viewport);
    let empty = send_to.rect().height();
    let short = send_to.layout.message.height();
    assert!(short > 0.0, "the field stands while it is empty");

    let words = "The invoice button does nothing the second time I press it, and the page then \
                 forgets which workspace I was in. It happened again when I reopened the same \
                 file a minute later.";
    assert!(send_to.perform_semantic_action(
        "send-to:message",
        &crate::a11y::Action::SetValue(words.to_owned()),
    ));
    send_to.place(menu, Some(viewport));
    let grown = send_to.layout.message.height();
    assert!(
        grown > short,
        "the words wrapped: the field is {grown} tall, it was {short}"
    );
    assert!(
        send_to.rect().height() > empty,
        "and the box made room for them"
    );

    // The field is as wide as the box leaves it, and the sentence is wider
    // than that: it has to wrap. What it takes wrapped fits the field, so no
    // line is cut off at the bottom either.
    let theme = &send_to.theme;
    let style = theme.text(TextRole::Body);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a finite widget width in CSS pixels fits the f32 shaping API"
    )]
    let width = (send_to.layout.message.width() - 2.0 * f64::from(theme.tokens.spacing.s2)) as f32;
    let (natural, one_line) = crate::text_render::with_layout_renderer(theme, |renderer| {
        (
            renderer.measure_style(words, None, &style),
            renderer.measure_style(words, Some(width), &style),
        )
    });
    assert!(
        f64::from(natural.0) > send_to.layout.message.width(),
        "the sentence is wider than the field, so it must wrap"
    );
    assert!(
        one_line.1 > natural.1 * 1.5,
        "wrapped, it is more than one line: {} against {}",
        one_line.1,
        natural.1
    );
    assert!(
        f64::from(one_line.1) <= grown + 0.5,
        "the field holds every wrapped line: {} written in {grown}",
        one_line.1
    );
}

/// A box opened to the one place reports go says so: its header and the name
/// assistive technology reads are the report's, not a message's.
#[test]
fn a_box_opened_to_the_report_destination_says_report_a_problem() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 900.0);
    let _ = crate::shared_selection::take_send_to();
    crate::shared_selection::set_send_host_available(true);
    let destination = Recipient::found(RecipientKind::Feedback, "c_feedback", "Tilo feedback");
    let mut send_to = SendToBox::new();
    send_to.set_viewport(Some(viewport));
    send_to.open(None, std::slice::from_ref(&destination));
    send_to.place(Rect::new(40.0, 120.0, 212.0, 320.0), Some(viewport));

    assert_eq!(
        send_to.name(),
        crate::i18n::I18nManager::lookup_plain("send-to-report-name"),
        "a report is what the box is doing"
    );
    let node = send_to.accessibility().expect("an open box is read");
    assert_eq!(node.name.as_deref(), Some(send_to.name().as_str()));

    let _ = crate::shared_selection::take_send_to();
    let message = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    assert_eq!(
        message.name(),
        crate::i18n::I18nManager::lookup_plain("send-to-name"),
        "and a message it sends anywhere is a message"
    );
}

/// The field that searches for whom the message goes to shows its hint whole:
/// it is drawn as wide as the hint, so a window too narrow to hold it is a
/// window the box does not fit, never a hint the person reads half of.
#[test]
fn the_field_that_searches_for_a_recipient_shows_its_hint_whole() {
    crate::shared_selection::set_focused_selection(None);
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    let node = send_to.accessibility().expect("an open box is read");
    let field = node
        .children
        .iter()
        .find(|child| child.action_target.as_deref() == Some("send-to:recipients"))
        .expect("the field that searches is read");
    let drawn = field
        .bounds
        .map(crate::a11y::AccessBounds::to_rect)
        .expect("where it is drawn");
    let hint = crate::i18n::I18nManager::lookup_plain("send-to-search-placeholder");
    let mut text = TextRenderer::new();
    let (hint_width, _) = text.measure_style(&hint, None, &Theme::dark().text(TextRole::Body));
    // The hint is set in from the field's edge as a chip's mark is, so the
    // room it needs is its own width plus that inset.
    let inset = send_to.mark_inset();
    assert!(
        drawn.width() + 0.5 >= f64::from(hint_width) + inset,
        "the field shows {} px for a {hint_width} px hint {hint:?} set {inset} px in",
        drawn.width()
    );
}

/// A box that says nothing about what it sends holds no line for subject chips:
/// the message stands at the box's own spacing under the recipients, while a
/// box with a subject gives that chip its own line.
#[test]
fn a_box_with_nothing_to_say_keeps_no_line_held_open_between_its_fields() {
    crate::shared_selection::set_focused_selection(None);
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    let beside = Rect::new(40.0, 120.0, 212.0, 320.0);
    let _ = crate::shared_selection::take_send_to();
    let _ = crate::shared_selection::take_recipient_searches();
    crate::shared_selection::set_send_host_available(true);
    let mut plain = SendToBox::new();
    plain.set_viewport(Some(viewport));
    plain.open(None, &[]);
    plain.place(beside, Some(viewport));
    let lines = |send_to: &SendToBox| {
        let node = send_to.accessibility().expect("an open box is read");
        let at = |target: &str| {
            node.children
                .iter()
                .find(|child| child.action_target.as_deref() == Some(target))
                .and_then(|child| child.bounds.map(crate::a11y::AccessBounds::to_rect))
                .expect("where it is drawn")
        };
        (at("send-to:recipients"), at("send-to:message"))
    };
    let (field, message) = lines(&plain);
    assert_eq!(
        message.y0,
        field.y1 + plain.gap() / 2.0,
        "the message stands at the box's spacing under the field, with no empty line between"
    );
    let node = plain.accessibility().expect("an open box is read");
    let at = |target: &str| {
        node.children
            .iter()
            .find(|child| child.action_target.as_deref() == Some(target))
            .and_then(|child| child.bounds.map(crate::a11y::AccessBounds::to_rect))
            .expect("where it is drawn")
    };
    assert_eq!(
        node.bounds
            .map(crate::a11y::AccessBounds::to_rect)
            .expect("the box's own bounds")
            .y1,
        at("send-to:send").y1 + plain.padding(),
        "the box is as tall as what it holds: its Send row ends one padding above its edge"
    );
    let (_, about) = lines(&box_beside(beside, viewport));
    assert!(
        about.y0 > message.y0,
        "a subject chip holds a line of its own: {} under {}",
        about.y0,
        message.y0
    );
}

/// The button that adds what is selected now says what it adds, and is not
/// drawn at all while the window holds nothing to add: a lone sign under the
/// subjects reads as a control nobody can name.
#[test]
fn adding_a_subject_is_named_and_stands_only_while_there_is_one_to_add() {
    let viewport = Rect::new(0.0, 0.0, 800.0, 600.0);
    crate::shared_selection::set_focused_selection(None);
    let mut send_to = box_beside(Rect::new(40.0, 120.0, 212.0, 320.0), viewport);
    let target = |send_to: &SendToBox| {
        send_to
            .accessibility()
            .expect("an open box is read")
            .children
            .iter()
            .find(|child| child.action_target.as_deref() == Some("send-to:add-subject"))
            .map(|child| {
                (
                    child.name.clone(),
                    child.bounds.map(crate::a11y::AccessBounds::to_rect),
                )
            })
    };
    assert!(
        target(&send_to).is_none(),
        "nothing selected: no button to add it"
    );

    let selected = SharedSelection::of_text("Ship the launch notes")
        .at(tilo_actions::SelectionPlace::item("issue-2"));
    crate::shared_selection::set_focused_selection(Some(selected.clone()));
    send_to.layout = send_to.build_layout();
    let (name, bounds) = target(&send_to).expect("something selected: the button that adds it");
    assert_eq!(
        name.as_deref(),
        Some(crate::i18n::I18nManager::lookup_plain("send-to-add-subject").as_str()),
        "it is read as what it adds"
    );
    let bounds = bounds.expect("where it is drawn");
    assert!(
        bounds.width() + 0.5 >= send_to.chip_ask(&send_to.add_label(), false) - 0.5,
        "the button is as wide as its words: {} for {:?}",
        bounds.width(),
        send_to.add_label()
    );

    send_to.press(bounds.center());
    assert_eq!(
        send_to.subjects(),
        &[send_to.subjects()[0].clone(), selected],
        "the subject it added is on the message"
    );
    assert!(
        target(&send_to).is_none(),
        "what is already on the message is not offered again"
    );
    crate::shared_selection::set_focused_selection(None);
}
