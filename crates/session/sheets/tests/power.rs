//! The power menu as a person, a screen reader and an agent meet it: where
//! it hangs, what it offers, what a choice asks the shell for.

use tilo_sheets::SheetState as _;
use tilo_sheets::power::{Offers, PowerApp, PowerEffect, PowerState};
use ui::a11y::Action as A11yAction;
use ui::framework::testing::HeadlessApp;
use ui::vello::kurbo::Rect;

const VIEWPORT: (u32, u32) = (1280, 800);

fn staged(scenario: &str) -> HeadlessApp<PowerApp> {
    HeadlessApp::<PowerApp>::capture_at(Some(scenario), VIEWPORT).unwrap()
}

fn rows(app: &mut HeadlessApp<PowerApp>) -> Vec<(String, Rect)> {
    let mut rows: Vec<(String, Rect)> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::MenuItem)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            Some((
                node.label()?.to_owned(),
                Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
            ))
        })
        .collect();
    rows.sort_by(|a, b| a.1.y0.total_cmp(&b.1.y0));
    rows
}

/// A menu the shell put up, lent the state it reports to.
fn mounted(offers: Offers) -> (HeadlessApp<PowerApp>, PowerState) {
    let state = PowerState::default();
    state.set_offers(offers);
    state.presentation().open(false);
    let mut app = HeadlessApp::<PowerApp>::for_person_with_lent("person", state.clone()).unwrap();
    app.harness().resize(VIEWPORT).unwrap();
    app.settle();
    (app, state)
}

fn key(app: &mut HeadlessApp<PowerApp>, key: &str) {
    app.harness().key(key);
    app.settle();
}

tilo_test_env::scenario! {
    /// The power menu: where it hangs, what it offers and publishes, and what a choice asks the shell.
    fn the_power_menu_offers_what_the_session_may_do_and_asks_the_shell;

    phase the_menu_hangs_under_the_bar_against_the_trailing_edge;
    {
        let mut app = staged("open");
        let rows = rows(&mut app);
        let names: Vec<&str> = rows.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "Lock",
                "Log out",
                "Switch user",
                "Suspend",
                "Restart",
                "Shut down"
            ],
            "every action, in the order a person expects them"
        );
        for (name, rect) in &rows {
            assert!(rect.y0 >= 40.0, "{name} hangs below the bar: {rect:?}");
            assert!(
                rect.x1 <= f64::from(VIEWPORT.0) && rect.x0 > f64::from(VIEWPORT.0) / 2.0,
                "{name} stands at the trailing edge: {rect:?}"
            );
            assert!(
                rect.height() >= 24.0,
                "{name} is a pointer target: {rect:?}"
            );
        }
        for pair in rows.windows(2) {
            assert!(
                pair[0].1.y1 <= pair[1].1.y0,
                "rows do not overlap: {pair:?}"
            );
        }
    }

    phase a_session_that_does_not_own_the_machine_offers_a_shut_down_that_ends_it;
    {
        let mut app = staged("detached");
        let names: Vec<String> = rows(&mut app).into_iter().map(|(name, _)| name).collect();
        // Shut down stays: in a session started from another desktop it ends
        // that session, it does not power the machine off.
        assert_eq!(names, ["Lock", "Log out", "Shut down"]);
        let mut app = staged("no-switching");
        let names: Vec<String> = rows(&mut app).into_iter().map(|(name, _)| name).collect();
        assert!(!names.iter().any(|name| name == "Switch user"));
        assert!(names.iter().any(|name| name == "Shut down"));
    }

    phase a_screen_reader_meets_a_named_modal_dialog_of_described_rows;
    {
        let mut app = staged("open");
        let nodes = app.harness().a11y_nodes();
        let dialog = nodes
            .iter()
            .map(|(_, node)| node)
            .find(|node| node.role() == ui::accesskit::Role::Dialog)
            .expect("the menu is a dialog");
        assert_eq!(dialog.label(), Some("Power"));
        assert!(dialog.is_modal());
        assert!(
            dialog
                .description()
                .is_some_and(|text| text.contains("switch user"))
        );
        let items: Vec<_> = nodes
            .iter()
            .map(|(_, node)| node)
            .filter(|node| node.role() == ui::accesskit::Role::MenuItem)
            .collect();
        assert_eq!(items.len(), 6);
        let logout = items
            .iter()
            .find(|node| node.label() == Some("Log out"))
            .unwrap();
        assert_eq!(logout.description(), Some("Open apps close"));
    }

    phase every_action_is_published_and_those_not_offered_say_why;
    {
        let (mut app, _state) = mounted(Offers {
            host_power: false,
            ..Offers::default()
        });
        let surface = app.agent_surface().to_string();
        for id in [
            "lock",
            "log-out",
            "switch-user",
            "suspend",
            "restart",
            "shut-down",
        ] {
            assert!(surface.contains(id), "{id} is published: {surface}");
        }
        assert!(
            app.effect(&PowerEffect::ShutDown).is_err(),
            "a session that does not own the machine does not shut it down"
        );
    }

    phase a_press_runs_the_rows_action_which_asks_first_then_asks_the_shell;
    {
        let (mut app, state) = mounted(Offers::default());
        app.harness()
            .perform_named("power_shut_down", &A11yAction::Activate)
            .unwrap();
        app.settle();
        assert!(state.take_asked().is_empty(), "shutting down asks first");
        assert!(!state.presentation().is_open(), "the menu took itself down");
        assert!(app.harness().asking().is_some(), "and its question stands");
        assert!(app.harness().confirm_asked().is_some());
        app.settle();
        assert_eq!(state.take_asked(), [PowerEffect::ShutDown]);
        let (mut app, state) = mounted(Offers::default());
        app.harness()
            .perform_named("power_lock", &A11yAction::Activate)
            .unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerEffect::Lock], "locking asks nothing");
    }

    phase an_agent_invoking_an_action_reaches_the_same_api;
    {
        let (mut app, state) = mounted(Offers::default());
        app.effect(&PowerEffect::Lock).unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerEffect::Lock]);
        // Restart takes everything down: whoever acts for the person is asked.
        let (mut app, state) = mounted(Offers::default());
        assert!(app.effect(&PowerEffect::Restart).is_err() || state.take_asked().is_empty());
        app.effect_confirmed(&PowerEffect::Restart).unwrap();
        app.settle();
        assert_eq!(state.take_asked(), [PowerEffect::Restart]);
    }

    phase the_keyboard_reaches_every_row_and_escape_leaves;
    {
        let (mut app, state) = mounted(Offers::default());
        state.presentation().open(true);
        app.settle();
        key(&mut app, "ArrowDown");
        key(&mut app, "Enter");
        assert_eq!(
            app.harness().asking().map(|(address, _)| address.action),
            Some("log-out".to_owned()),
            "from the first row, one step down is Log out, which asks first"
        );
        key(&mut app, "Escape");
        assert!(state.take_asked().is_empty(), "Escape answers no");

        state.presentation().open(true);
        app.settle();
        key(&mut app, "Escape");
        assert!(state.take_asked().is_empty());
        assert!(!state.presentation().is_open(), "Escape takes the menu down");
    }

    phase the_shell_taking_the_sheet_down_closes_the_menu;
    {
        let (mut app, state) = mounted(Offers::default());
        assert!(!rows(&mut app).is_empty());
        assert!(!state.rows().is_empty(), "the state knows where the rows are");
        state.presentation().close();
        app.settle();
        assert!(rows(&mut app).is_empty());
        assert!(state.rows().is_empty());
    }

    phase every_scenario_renders;
    {
        for scenario in ["open", "keyboard", "detached", "no-switching"] {
            let mut app = staged(scenario);
            assert!(!rows(&mut app).is_empty(), "{scenario}");
        }
    }
}
