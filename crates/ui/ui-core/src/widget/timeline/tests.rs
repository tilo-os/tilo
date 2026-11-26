//! The grid's layout, the change each gesture lands, and 10k events on one week.
use super::test_support::*;

use super::*;
use jiff::civil::Date;
use tilo_ui_base::paint::PaintCtx;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{component::Fired, component::Value};
use vello::kurbo::{Affine, Point, Rect};

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
        let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
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
        let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
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
        let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
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
        let queue: tilo_ui_base::Intents<Fired> = tilo_ui_base::Intents::default();
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
        timeline.draw(&mut tilo_ui_base::draw::DrawCtx::new(&mut scene, Affine::IDENTITY, &mut text, &theme, tilo_ui_base::presence::PresenceCtx::empty().draw_env()));
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
