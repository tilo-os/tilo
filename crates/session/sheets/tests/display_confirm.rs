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

tilo_test_env::scenario! {
    /// The question about a display change: what it shows, and each answer it gives the shell.
    fn the_display_question_names_its_tool_and_answers_the_shell;

    phase a_question_on_every_display_is_met_once_by_the_keyboard_and_a_reader;
    // A card stands on every display (their pixels are read in `painted.rs`),
    // and the question is still one question.
    {
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
    }

    phase only_a_known_tool_can_be_trusted;
    {
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

    phase a_screen_reader_meets_a_modal_dialog_naming_the_tool;
    {
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

    phase enter_keeps_and_escape_reverts;
    {
        let (mut app, seat) = seated(question(Some("kanshi")));
        app.harness().key("Enter");
        app.settle();
        assert_eq!(seat.take_asked(), [ConfirmEffect::Keep]);
        app.harness().key("Escape");
        app.settle();
        assert_eq!(seat.take_asked(), [ConfirmEffect::Revert]);
    }

    phase a_press_on_revert_answers_the_shell;
    {
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

    phase an_agent_reverts_at_once_but_keeping_is_the_persons_answer;
    {
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

    phase the_shell_updates_the_countdown_in_place;
    {
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

    phase while_the_answer_is_recorded_the_buttons_wait;
    {
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

    phase tab_walks_keep_revert_and_the_trust_answer_and_enter_gives_it;
    {
        let (mut app, seat) = seated(question(Some("kanshi")));
        app.harness().key("Tab");
        app.harness().key("Tab");
        app.settle();
        app.harness().key("Enter");
        app.settle();
        assert_eq!(seat.take_asked(), [ConfirmEffect::KeepAndTrust]);
    }
}
