//! The question about a display change as a person, a screen reader and an
//! agent meet it: a card on every display, Keep with the keyboard, and the
//! answer going back to the shell.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::display_confirm::{ConfirmEffect, ConfirmSeat, DisplayConfirmApp, Question};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<DisplayConfirmApp> {
    HeadlessApp::<DisplayConfirmApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// Every button, by its label, where it stands.
fn buttons(app: &mut HeadlessApp<DisplayConfirmApp>) -> Vec<(String, Rect)> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect()
}

fn question(tool: Option<&str>) -> Question {
    Question {
        tool: tool.map(str::to_owned),
        status: "Reverting in 9 s".to_owned(),
        answering: false,
        displays: vec![Rect::new(0.0, 0.0, 1280.0, 800.0)],
    }
}

fn seated(question: Question) -> (HeadlessApp<DisplayConfirmApp>, ConfirmSeat) {
    let seat = ConfirmSeat::default();
    seat.ask(question);
    let mut app =
        HeadlessApp::<DisplayConfirmApp>::for_person_with_lent("person", seat.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, seat)
}

#[test]
fn every_display_shows_the_question_in_its_middle() {
    let mut app = staged("two-displays");
    let keeps: Vec<Rect> = buttons(&mut app)
        .into_iter()
        .filter(|(label, _)| label == "Keep")
        .map(|(_, rect)| rect)
        .collect();
    assert_eq!(
        keeps.len(),
        1,
        "the keyboard and a reader meet it once: {keeps:?}"
    );
    assert!(keeps[0].center().x < 640.0);
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert!(alpha(320, 400) > 0, "a card on the first display");
    assert!(alpha(960, 400) > 0, "and one on the second");
}

#[test]
fn only_a_known_tool_can_be_trusted() {
    let mut app = staged("known-tool");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert!(
        labels.iter().any(|label| label.contains("kanshi")),
        "{labels:?}"
    );
    let mut app = staged("unknown-tool");
    let labels: Vec<String> = buttons(&mut app)
        .into_iter()
        .map(|(label, _)| label)
        .collect();
    assert_eq!(labels.len(), 2, "Keep and Revert only: {labels:?}");
}

#[test]
fn the_question_paints_only_its_cards_over_the_desktop() {
    let mut app = staged("known-tool");
    let pixels = app.harness().render_rgba_transparent();
    let alpha = |x: u32, y: u32| pixels[usize::try_from((y * VIEWPORT.0 + x) * 4 + 3).unwrap()];
    assert_eq!(alpha(10, 10), 0, "the desktop shows around the card");
    assert!(alpha(640, 400) > 0, "the card is painted in the middle");
}

#[test]
fn a_screen_reader_meets_a_modal_dialog_naming_the_tool() {
    let mut app = staged("known-tool");
    let nodes = app.harness().a11y_nodes();
    let dialog = nodes
        .iter()
        .map(|(_, node)| node)
        .find(|node| node.role() == ui::accesskit::Role::Dialog)
        .expect("the question is a dialog");
    assert!(dialog.label().is_some_and(|name| name.contains("kanshi")));
    assert!(dialog.is_modal());
    assert!(
        dialog
            .description()
            .is_some_and(|text| text.contains("Reverting in 12 s"))
    );
}

#[test]
fn enter_keeps_and_escape_reverts() {
    let (mut app, seat) = seated(question(Some("kanshi")));
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_asked(), [ConfirmEffect::Keep]);
    app.harness().key("Escape");
    app.settle();
    assert_eq!(seat.take_asked(), [ConfirmEffect::Revert]);
}

#[test]
fn a_press_on_revert_answers_the_shell() {
    let (mut app, seat) = seated(question(None));
    let revert = buttons(&mut app)
        .into_iter()
        .find(|(label, _)| label == "Revert")
        .unwrap()
        .1;
    app.harness().click_at(revert.center().x, revert.center().y);
    app.settle();
    assert_eq!(seat.take_asked(), [ConfirmEffect::Revert]);
}

#[test]
fn an_agent_reverts_at_once_but_keeping_is_the_persons_answer() {
    use ui::tilo_resource::Effect as _;
    let (mut app, seat) = seated(question(Some("kanshi")));
    app.effect(&ConfirmEffect::Revert).unwrap();
    app.settle();
    assert_eq!(seat.take_asked(), [ConfirmEffect::Revert]);
    for declaration in ConfirmEffect::declarations() {
        assert_eq!(
            declaration.attended,
            declaration.id != "revert",
            "only Revert needs nobody attending: {}",
            declaration.id
        );
    }
}

#[test]
fn the_shell_updates_the_countdown_in_place() {
    let (mut app, seat) = seated(question(Some("kanshi")));
    seat.ask(Question {
        status: "Reverting in 3 s".to_owned(),
        ..question(Some("kanshi"))
    });
    app.settle();
    let said = app.harness().a11y_nodes().into_iter().any(|(_, node)| {
        node.label()
            .is_some_and(|text| text.contains("Reverting in 3 s"))
            || node
                .description()
                .is_some_and(|text| text.contains("Reverting in 3 s"))
    });
    assert!(said);
}

#[test]
fn while_the_answer_is_recorded_the_buttons_wait() {
    let mut app = staged("answering");
    app.harness()
        .perform_named("display-confirm-keep-0", &A11yAction::Activate)
        .ok();
    let nodes = app.harness().a11y_nodes();
    assert!(
        nodes
            .iter()
            .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
            .all(|(_, node)| node.is_disabled())
    );
}

#[test]
fn tab_walks_keep_revert_and_the_trust_answer_and_enter_gives_it() {
    let (mut app, seat) = seated(question(Some("kanshi")));
    app.harness().key("Tab");
    app.harness().key("Tab");
    app.settle();
    app.harness().key("Enter");
    app.settle();
    assert_eq!(seat.take_asked(), [ConfirmEffect::KeepAndTrust]);
}
