//! The grid's layout, the change each gesture lands, and 10k events on one week.

use super::widget::ItemEvents;
use super::*;
use crate::component::{Event, Fired, Intent, Value};
use crate::composition::Composed;
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Widget;
use jiff::civil::Date;
use vello::kurbo::{Affine, Point, Rect};

fn item(index: usize, d: Date, start_min: i64, end_min: i64) -> TimelineItem {
    TimelineItem {
        id: format!("item-{index}"),
        resource: Some(format!("resource-{index}")),
        title: format!("Event {index}"),
        location: String::new(),
        tone: TimelineTone::First,
        color: vello::peniko::Color::BLACK,
        stack: 0,
        day: d,
        end_day: d,
        start_min,
        end_min,
        clock_end_min: end_min,
        all_day: false,
    }
}

fn area() -> Rect {
    Rect::new(100.0, 40.0, 1300.0, 640.0)
}

fn tokens() -> TimelineTokens {
    TimelineTokens::default_dark()
}

fn geom() -> TimelineGeom {
    geometry(area(), &tokens(), 7, 0)
}

fn center_of(column: usize, minute: i64) -> Point {
    let geom = geom();
    let width = column_width(area(), 7, geom);
    Point::new(
        area().x0 + geom.gutter + width * (column as f64 + 0.5),
        geom.body_top + minute as f64 / 60.0 * geom.hour_h,
    )
}

fn grid_events(queue: &crate::Intents<Fired>) -> (Event, Event, Event) {
    (
        Event::View(Intent::new(queue, "timeline", "created")),
        Event::View(Intent::new(queue, "timeline", "open_day")),
        Event::View(Intent::new(queue, "timeline", "zoomed")),
    )
}

fn item_events(queue: &crate::Intents<Fired>) -> ItemEvents {
    ItemEvents {
        press: Event::View(Intent::new(queue, "item", "pressed")),
        moved: Event::View(Intent::new(queue, "item", "moved")),
        resized: Event::View(Intent::new(queue, "item", "resized")),
    }
}

fn week(anchor: Date) -> Timeline {
    let mut timeline = Timeline::new(TimelineMode::Week, anchor, Vec::new());
    timeline.layout_for(area(), &Theme::dark());
    timeline.set_offset_for_test(0.0);
    timeline.show_now = false;
    timeline
}

tilo_test_env::scenario! {
    /// Overlapping events share a day's width, and each gesture lands the one typed change.
    fn the_grid_lays_overlapping_events_side_by_side_and_each_gesture_lands_its_change;

    phase overlapping_events_take_their_own_column;
    {
        let split = |index, of| Split { index, of };
        assert_eq!(
            splits(&[(540, 600), (570, 660), (600, 630), (720, 780)]),
            vec![split(0, 2), split(1, 2), split(0, 2), split(0, 1)]
        );
        assert_eq!(
            splits(&[(540, 600), (540, 600), (540, 600)]),
            vec![split(0, 3), split(1, 3), split(2, 3)]
        );
        assert_eq!(
            splits(&[(540, 600), (600, 660)]),
            vec![split(0, 1), split(0, 1)]
        );
    }

    phase dragging_empty_hours_creates_an_event_at_the_snapped_times;
    {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        timeline.set_events_for_test(grid_events(&queue));
        let target = timeline.days[2];
        let from = center_of(2, 9 * 60 + 7);
        assert!(timeline.begin_for_test(from));
        timeline.update_for_test(Point::new(from.x + 2.0, from.y + 2.0));
        timeline.update_for_test(center_of(2, 10 * 60 + 53));
        timeline.finish_for_test(center_of(2, 10 * 60 + 53));
        let fired = queue.take();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event, "created");
        assert_eq!(
            fired[0].value,
            Some(Value::List(vec![
                Value::Text(format!("{target}T09:00:00")),
                Value::Text(format!("{target}T11:00:00")),
            ]))
        );
    }

    phase dragging_an_event_moves_it_to_the_day_and_time_it_lands_on;
    {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        timeline.set_events_for_test(grid_events(&queue));
        let (from_day, to_day) = (timeline.days[1], timeline.days[4]);
        timeline.set_items_for_test(vec![item(0, from_day, 10 * 60, 11 * 60)]);
        timeline.set_item_events_for_test(vec![item_events(&queue)]);
        let rect = timeline.rect_for_test(0).unwrap();
        // A quarter into the block: fifteen minutes past its start, as a hand grabs it.
        let grab = Point::new(rect.center().x, rect.y0 + rect.height() * 0.25);
        assert!(timeline.begin_for_test(grab));
        timeline.update_for_test(center_of(4, 14 * 60 + 15));
        timeline.finish_for_test(center_of(4, 14 * 60 + 15));
        let fired = queue.take();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event, "moved");
        assert_eq!(
            fired[0].value,
            Some(Value::List(vec![
                Value::Resource("resource-0".into()),
                Value::Text(format!("{to_day}T14:00:00")),
            ]))
        );
    }

    phase dragging_an_events_bottom_edge_resizes_only_its_end;
    {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        timeline.set_events_for_test(grid_events(&queue));
        let day = timeline.days[2];
        timeline.set_items_for_test(vec![item(0, day, 9 * 60, 10 * 60)]);
        timeline.set_item_events_for_test(vec![item_events(&queue)]);
        let rect = timeline.rect_for_test(0).unwrap();
        let grip = Point::new(rect.center().x, rect.y1 - 1.0);
        assert!(timeline.begin_for_test(grip));
        timeline.update_for_test(center_of(5, 11 * 60 + 40));
        timeline.finish_for_test(center_of(5, 11 * 60 + 40));
        let fired = queue.take();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event, "resized");
        assert_eq!(
            fired[0].value,
            Some(Value::List(vec![
                Value::Resource("resource-0".into()),
                Value::Text(format!("{day}T11:45:00")),
            ]))
        );
    }

    phase pressing_an_event_reports_it_and_pressing_empty_hours_opens_the_day;
    {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        timeline.set_events_for_test(grid_events(&queue));
        let first = timeline.days[0];
        let day = timeline.days[3];
        timeline.set_items_for_test(vec![item(0, day, 3 * 60, 4 * 60)]);
        timeline.set_item_events_for_test(vec![item_events(&queue)]);
        let inside = timeline.rect_for_test(0).unwrap().center();
        assert!(timeline.begin_for_test(inside));
        timeline.finish_for_test(inside);
        let fired = queue.take();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event, "pressed");
        assert_eq!(fired[0].value, Some(Value::Resource("resource-0".into())));

        let empty = center_of(0, 3 * 60);
        assert!(timeline.begin_for_test(empty));
        timeline.finish_for_test(empty);
        let fired = queue.take();
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].event, "open_day");
        assert_eq!(fired[0].value, Some(Value::Text(first.to_string())));
    }

    phase ten_thousand_events_lay_out_and_only_the_viewport_is_painted;
    {
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        let days = timeline.days.clone();
        let items = (0..10_000)
            .map(|index| {
                let start = (i64::try_from(index).unwrap_or(0) * 10) % (24 * 60);
                item(index, days[index % 7], start, start + 1)
            })
            .collect();
        timeline.set_items_for_test(items);
        timeline.layout_for(Rect::new(100.0, 40.0, 1300.0, 240.0), &Theme::dark());
        let theme = Theme::dark();
        let mut scene = vello::Scene::new();
        let mut text = TextRenderer::new();
        timeline.draw(&mut crate::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme, crate::presence::PresenceCtx::empty().draw_env()));
        assert!(timeline.painted() > 0, "the week shows its events");
        assert!(
            timeline.painted() < 2_000,
            "only the hours on screen are painted, not all 10k (was {})",
            timeline.painted()
        );
    }

    phase the_month_draws_a_multi_day_event_as_one_bar_and_takes_the_rest_to_more;
    {
        let day = Date::new(2026, 11, 3).unwrap();
        let mut items = vec![TimelineItem {
            id: "span".into(),
            resource: Some("span".into()),
            title: "Conference".into(),
            location: String::new(),
            tone: TimelineTone::First,
            color: vello::peniko::Color::BLACK,
            stack: 0,
            day: Date::new(2026, 11, 2).unwrap(),
            end_day: Date::new(2026, 11, 5).unwrap(),
            start_min: 0,
            end_min: 24 * 60,
            clock_end_min: 24 * 60,
            all_day: true,
        }];
        for index in 0..5 {
            let offset = i64::try_from(index).unwrap_or(0);
            items.push(item(index, day, 9 * 60 + offset, 10 * 60 + offset));
        }
        let theme = Theme::dark();
        let mut scene = vello::Scene::new();
        let mut text = TextRenderer::new();
        let mut ctx = PaintCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme);
        let face = MonthFace {
            anchor: day,
            today: day,
            items: &items,
            untitled: "",
        };
        let report = face.paint(&mut ctx, Rect::new(0.0, 0.0, 700.0, 600.0), None);
        let bars: Vec<Rect> = report
            .hits
            .iter()
            .filter(|(_, target)| matches!(target, MonthTarget::OpenEvent(id) if id == "span"))
            .map(|(rect, _)| *rect)
            .collect();
        assert_eq!(bars.len(), 1, "one event spans days as one bar");
        assert!(
            bars[0].width() > 700.0 / 7.0 * 2.0,
            "the bar crosses the days it covers, was {} wide",
            bars[0].width()
        );
        let chips = report
            .hits
            .iter()
            .filter(|(_, target)| matches!(target, MonthTarget::OpenEvent(id) if id != "span"))
            .count();
        assert!(chips >= 1, "the day's events show");
        assert!(chips < 5, "only the slots a cell has show, the rest is more");
    }
}

/// The composed node of the widget named `widget`, depth first.
fn by_widget<'a>(node: &'a Composed, widget: &str) -> Option<&'a Composed> {
    if node.widget == widget {
        return Some(node);
    }
    node.children
        .iter()
        .find_map(|child| by_widget(child, widget))
}

/// A week of fifty events, some of them titled past one column's width.
fn fifty_items(days: &[Date]) -> Vec<TimelineItem> {
    (0..50)
        .map(|index| {
            let day = days[index % days.len()];
            let start = 7 * 60 + i64::try_from(index % 12).unwrap_or(0) * 45;
            let title = if index % 5 == 0 {
                format!(
                    "Event {index}: a title long enough to run past the width of one day's column"
                )
            } else {
                format!("Event {index}")
            };
            let mut each = item(index, day, start, start + 40);
            each.title = title;
            each
        })
        .collect()
}

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
        let days = timeline.days.clone();
        timeline.set_items_for_test(fifty_items(&days));
        timeline.layout_for(area(), &Theme::dark());
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
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
            .filter(|child| child.role == crate::a11y::Role::Group)
            .collect();
        assert_eq!(sections.len(), 7, "a week is seven day groups");
        let mut buttons = Vec::new();
        for (column, section) in sections.iter().enumerate() {
            let day = days[column];
            let iso = day.to_string();
            let heading = crate::format::day_heading(day, false);
            assert_eq!(section.value.as_deref(), Some(iso.as_str()));
            assert_eq!(section.name.as_deref(), Some(heading.as_str()));
            buttons.extend(
                section
                    .children
                    .iter()
                    .filter(|part| part.role == crate::a11y::Role::Button),
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
        let html = crate::composition::page::Page::new().render(&composition);
        assert_eq!(html.matches("<section").count(), 7, "a day is a section: {html}");
        assert!(html.contains(&crate::composition::page::escape(
            &crate::format::day_heading(days[0], false)
        )));
        assert!(html.contains("a title long enough to run past the width of one day"));
        assert!(html.contains("data-a=\"activate\" data-t=\"tilo.key-action:timeline:"));
        // Each day and each event tells the browser the time it stands for.
        for day in &days {
            let heading = crate::composition::page::escape(&crate::format::day_heading(*day, false));
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
                    crate::composition::page::escape(hours)
                )),
                "the event at {value} is a time on the page: {html}"
            );
        }
    }

    phase activating_an_events_element_presses_what_a_click_on_it_presses;
    {
        let queue: crate::Intents<Fired> = crate::Intents::default();
        let anchor = Date::new(2026, 8, 31).unwrap();
        let mut timeline = Timeline::new(TimelineMode::Week, anchor, Vec::new());
        timeline.show_now = false;
        timeline.layout_for(area(), &Theme::dark());
        let day = timeline.days[2];
        timeline.set_items_for_test(vec![item(0, day, 9 * 60, 10 * 60)]);
        timeline.set_item_events_for_test(vec![item_events(&queue)]);
        let mut host = crate::headless::FrameHarness::for_root(
            crate::State::new(),
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
            .find(|part| part.role == crate::a11y::Role::Button && part.action_target.is_some())
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
            &crate::a11y::Action::Activate,
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
        let heading = crate::format::day_heading(day, false);
        let cell = access
            .children
            .iter()
            .find(|cell| cell.value.as_deref() == Some("2026-11-03"))
            .expect("the day's own cell");
        assert_eq!(cell.role, crate::a11y::Role::Group);
        assert_eq!(cell.name.as_deref(), Some(heading.as_str()));
        assert!(cell.bounds.is_some(), "a cell stands somewhere");
        assert_eq!(cell.children.len(), 1, "the day's event");
        assert!(cell.children[0].bounds.is_some());
    }

    phase ten_thousand_events_are_as_many_nodes_as_the_viewport_shows;
    {
        let mut timeline = week(Date::new(2026, 8, 31).unwrap());
        let days = timeline.days.clone();
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
