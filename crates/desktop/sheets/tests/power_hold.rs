//! The question before logging out or shutting down while an app holds work that is not saved,
//! as a person, a screen reader and an agent meet it: the apps named in their own words, going
//! ahead, and staying.

use tilo_app_test_host::TestApp;
use tilo_sheets::SheetState as _;
use tilo_sheets::power_hold::{PowerHoldApp, PowerHoldEffect, PowerHoldState, Question};

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> TestApp<PowerHoldApp> {
    TestApp::<PowerHoldApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

/// A question about `effect`, with two apps saying what they hold.
fn question(effect: &str) -> Question {
    Question {
        effect: effect.to_owned(),
        apps: vec![
            "Notes: 2 notes are not saved".to_owned(),
            "Writer: an article is not saved".to_owned(),
        ],
    }
}

fn mounted(effect: &str) -> (TestApp<PowerHoldApp>, PowerHoldState) {
    let state = PowerHoldState::default();
    state.ask(question(effect));
    state.presentation().open(false);
    let mut app = TestApp::<PowerHoldApp>::for_person_with_lent("person", state.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, state)
}

fn says(app: &mut TestApp<PowerHoldApp>, text: &str) -> bool {
    app.harness().a11y_nodes().iter().any(|(_, node)| {
        node.label().is_some_and(|label| label.contains(text))
            || node.description().is_some_and(|said| said.contains(text))
            || node.value().is_some_and(|value| value.contains(text))
    })
}

fn buttons(app: &mut TestApp<PowerHoldApp>) -> Vec<String> {
    app.harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::Button)
        .filter_map(|(_, node)| node.label().map(str::to_owned))
        .collect()
}

tilo_test_env::scenario! {
    /// The question before logging out or shutting down while an app holds unsaved work.
    fn the_question_before_leaving_names_the_apps_and_answers_the_shell;

    phase the_apps_are_named_and_the_answers_are_the_persons;
    {
        let mut app = staged("log-out");
        for text in [
            "Log out?",
            "Notes: 2 notes are not saved",
            "Writer: an article is not saved",
        ] {
            assert!(says(&mut app, text), "{text:?} is shown");
        }
        let labels = buttons(&mut app);
        assert!(labels.iter().any(|label| label == "Log out anyway"), "{labels:?}");
        assert!(labels.iter().any(|label| label == "Stay"), "{labels:?}");
    }

    phase each_effect_says_its_own_words;
    {
        let mut app = staged("shut-down");
        assert!(says(&mut app, "Shut down?"));
        assert!(
            buttons(&mut app).iter().any(|label| label == "Shut down anyway"),
            "{:?}",
            buttons(&mut app)
        );
    }

    phase going_ahead_and_staying_reach_the_shell;
    {
        let (mut app, state) = mounted("log-out");
        app.harness()
            .perform_named("power-hold.primary", &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerHoldEffect::Carry]);
        let (mut app, state) = mounted("shut-down");
        app.harness()
            .perform_named("power-hold.secondary", &ui::a11y::Action::Activate)
            .unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerHoldEffect::Cancel]);
    }

    phase escape_stays_and_the_question_goes;
    {
        let (mut app, state) = mounted("log-out");
        app.harness().key("Escape");
        app.settle();
        assert_eq!(state.take_asked(), [PowerHoldEffect::Cancel]);
        assert!(!state.presentation().is_open(), "the question is taken down");
    }

    phase a_screen_reader_meets_a_modal_dialog_naming_the_apps;
    {
        let mut app = staged("log-out");
        let nodes = app.harness().a11y_nodes();
        let dialog = nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.role() == ui::accesskit::Role::Dialog)
            .expect("the question is a dialog");
        assert!(dialog.label().is_some_and(|name| name == "Log out?"));
        assert!(dialog.is_modal());
        assert!(
            dialog
                .description()
                .is_some_and(|text| text.contains("Notes: 2 notes are not saved")),
            "{:?}",
            dialog.description()
        );
    }

    phase an_agent_goes_ahead_at_once;
    {
        let (mut app, state) = mounted("log-out");
        app.effect(&PowerHoldEffect::Carry).unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerHoldEffect::Carry]);
    }
}
