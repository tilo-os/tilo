use jiff::civil::Date;
use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::internals::timeline::test_support::*;
use tilo_ui_app::widget::internals::timeline::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;
tilo_test_env::scenario! {
    /// The grid as a page shows it: days a reader can find by their dates, events with their
    /// times, and a press on an event's element the press it makes when clicked.
    fn the_week_is_a_page_of_days_and_events_with_their_times;

    phase each_day_is_a_section_with_a_time_and_each_event_a_button_with_its_hours;
    {
        let anchor = Date::new(2026, 8, 31).unwrap();
        let mut timeline = Timeline::new(TimelineMode::Week, anchor, Vec::new());
        timeline.show_now = false;
        timeline.layout_for(area(), &Theme::dark());
        let days = timeline.test_days();
        timeline.set_items_for_test(fifty_items(&days));
        timeline.layout_for(area(), &Theme::dark());
        let mut host = tilo_ui_app::app::driver::Driver::for_root(
            tilo_ui_app::State::new(),
            timeline,
            (1280, 820),
            Theme::dark(),
        );
        host.relayout();
        let composition = host.composition();
        let grid = by_widget(&composition.root, "Timeline").expect("the grid is composed");
        let access = grid.semantics.as_ref().expect("the grid says what it is");

        let sections: Vec<_> = access
            .children
            .iter()
            .filter(|child| child.role == tilo_ui_base::a11y::Role::Group)
            .collect();
        assert_eq!(sections.len(), 7, "a week is seven day groups");
        let mut buttons = Vec::new();
        for (column, section) in sections.iter().enumerate() {
            let day = days[column];
            let iso = day.to_string();
            let heading = tilo_ui_base::format::day_heading(day, false);
            assert_eq!(section.value.as_deref(), Some(iso.as_str()));
            assert_eq!(section.name.as_deref(), Some(heading.as_str()));
            buttons.extend(
                section
                    .children
                    .iter()
                    .filter(|part| part.role == tilo_ui_base::a11y::Role::Button),
            );
        }
        assert!(buttons.len() > 10, "the events the viewport shows are buttons");
        for &button in &buttons {
            let value = button.value.as_deref().expect("an event carries its time");
            assert!(value.contains('T') && value.contains('/'), "{value}");
            assert!(button.description.is_some(), "an event says its hours");
            let target = button
                .action_target
                .as_deref()
                .expect("an event names what a press on it acts on");
            assert!(target.starts_with("tilo.key-action:timeline:"), "{target}");
        }

        // The page itself: sections a person's browser lays out, each headed by its day, the
        // long titles intact and each event's element acting as its own press.
        let html = tilo_ui_app::composition::page::Page::new().render(&composition);
        assert_eq!(html.matches("<section").count(), 7, "a day is a section: {html}");
        assert!(html.contains(&tilo_ui_app::composition::page::escape(
            &tilo_ui_base::format::day_heading(days[0], false)
        )));
        assert!(html.contains("a title long enough to run past the width of one day"));
        assert!(html.contains("data-a=\"activate\" data-t=\"tilo.key-action:timeline:"));
        // Each day and each event tells the browser the time it stands for.
        for day in &days {
            let heading = tilo_ui_app::composition::page::escape(&tilo_ui_base::format::day_heading(*day, false));
            assert!(
                html.contains(&format!("<time datetime=\"{day}\">{heading}</time>")),
                "{day} is a time on the page: {html}"
            );
        }
        for &button in &buttons {
            let value = button.value.as_deref().expect("an event carries its time");
            let hours = button.description.as_deref().expect("an event says its hours");
            assert!(
                html.contains(&format!(
                    "<time datetime=\"{value}\">{}</time>",
                    tilo_ui_app::composition::page::escape(hours)
                )),
                "the event at {value} is a time on the page: {html}"
            );
        }
    }

    phase activating_an_events_element_presses_what_a_click_on_it_presses;
    {
        let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
        let anchor = Date::new(2026, 8, 31).unwrap();
        let mut timeline = Timeline::new(TimelineMode::Week, anchor, Vec::new());
        timeline.show_now = false;
        timeline.layout_for(area(), &Theme::dark());
        let day = timeline.test_days()[2];
        timeline.set_items_for_test(vec![item(0, day, 9 * 60, 10 * 60)]);
        timeline.set_item_events_for_test(vec![item_events(&queue)]);
        let mut host = tilo_ui_app::app::driver::Driver::for_root(
            tilo_ui_app::State::new(),
            timeline,
            (1280, 820),
            Theme::dark(),
        );
        host.relayout();
        let composition = host.composition();
        let grid = by_widget(&composition.root, "Timeline").expect("the grid is composed");
        let button = grid
            .semantics
            .as_ref()
            .expect("the grid says what it is")
            .nodes()
            .into_iter()
            .find(|part| part.role == tilo_ui_base::a11y::Role::Button && part.action_target.is_some())
            .expect("the event is a button");
        let target = button.action_target.clone().expect("the button names its event");
        let bounds = button.bounds.expect("the event stands somewhere");
        let (x, y, width, height) =
            (bounds.x0, bounds.y0, bounds.x1 - bounds.x0, bounds.y1 - bounds.y0);
        host.click_at(x + width / 2.0, y + height / 2.0);
        let clicked = queue.take();
        assert_eq!(clicked.len(), 1, "the click pressed the event");
        assert_eq!(clicked[0].event, "pressed");
        assert_eq!(clicked[0].value, Some(Value::Resource("resource-0".into())));

        host.perform_semantic_named(
            "Timeline",
            Some(target.as_str()),
            &tilo_ui_base::a11y::Action::Activate,
        )
        .expect("the event's element takes the activation");
        let pressed = queue.take();
        assert_eq!(pressed.len(), 1);
        assert_eq!(pressed[0].event, clicked[0].event);
        assert_eq!(pressed[0].value, clicked[0].value);
    }

    phase the_month_shows_each_cell_as_the_day_it_is_with_its_events;
    {
        let day = Date::new(2026, 11, 3).unwrap();
        let mut timeline = Timeline::new(TimelineMode::Month, day, Vec::new());
        timeline.layout_for(Rect::new(0.0, 0.0, 980.0, 760.0), &Theme::dark());
        timeline.set_items_for_test(vec![item(0, day, 9 * 60, 10 * 60)]);
        timeline.layout_for(Rect::new(0.0, 0.0, 980.0, 760.0), &Theme::dark());
        let access = timeline.accessibility().expect("the month says what it is");
        assert_eq!(access.children.len(), 42, "six weeks of day cells");
        let heading = tilo_ui_base::format::day_heading(day, false);
        let cell = access
            .children
            .iter()
            .find(|cell| cell.value.as_deref() == Some("2026-11-03"))
            .expect("the day's own cell");
        assert_eq!(cell.role, tilo_ui_base::a11y::Role::Group);
        assert_eq!(cell.name.as_deref(), Some(heading.as_str()));
        assert!(cell.bounds.is_some(), "a cell stands somewhere");
        assert_eq!(cell.children.len(), 1, "the day's event");
        assert!(cell.children[0].bounds.is_some());
    }

    phase ten_thousand_events_are_as_many_nodes_as_the_viewport_shows;
    {
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        let days = timeline.test_days();
        let items = (0..10_000)
            .map(|index| {
                let start = (i64::try_from(index).unwrap_or(0) * 10) % (24 * 60);
                item(index, days[index % 7], start, start + 1)
            })
            .collect();
        timeline.set_items_for_test(items);
        timeline.layout_for(Rect::new(100.0, 40.0, 1300.0, 240.0), &Theme::dark());
        let access = timeline.accessibility().expect("the grid says what it is");
        let nodes = access.nodes().len();
        assert!(nodes > 7, "the days and what shows of them are nodes");
        assert!(
            nodes < 2_000,
            "only what the viewport shows is a node, not all 10k (was {nodes})"
        );
        for day in &access.children {
            for node in &day.children {
                assert!(node.bounds.is_some(), "a shown event stands somewhere");
            }
        }
    }
}
