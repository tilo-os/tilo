//! [`Timeline`] as a described component: what a view hands it, and the changes it reports back.

use tilo_ui_base::{component::Component, component::Described, component::Event, component::Moment, component::ResourceId};
use tilo_ui_base::theme::Theme;

use super::widget::{ItemEvents, Timeline};
use super::{TimelineItem, TimelineMode, TimelineTone, local_day_minute};

/// A day, week or month as a time grid: events at their hours laid side by side where they
/// overlap, an all-day lane, the line where now is, and a month of days with what stands on each.
/// Dragging an event moves it, dragging its edge resizes it, and dragging across empty hours makes
/// one that starts and ends where the drag did; each is a typed event, and the grid never changes
/// what it shows.
#[derive(Component)]
#[component(name = "Timeline", role = "Group", label = "label")]
pub struct TimelineProps {
    /// What the grid is, for assistive technology: "This week".
    pub label: Option<String>,
    /// The range shown: the day, the week of a day, or the month of a day.
    pub range: TimelineMode,
    /// The day the period stands in.
    #[component(required)]
    pub start: Moment,
    /// The minutes a drag snaps to: a quarter hour by default.
    pub slot: i64,
    /// Whether the line where now is shows, read from the frame's clock (views cannot read it).
    pub show_now: bool,
    /// How tall an hour is drawn against its legible minimum: 1 is the minimum, 2 twice it.
    pub zoom: f64,
    /// How many all-day rows the lane keeps, whatever the events need.
    pub rows: i64,
    /// What stands on the grid, in no particular order: each is placed by its own times.
    #[component(part(default))]
    pub items: Vec<TimelineItemProps>,
    /// Empty hours were dragged across: carries where the new event starts and ends.
    #[component(event(value = Moment, at = Moment))]
    pub created: Event,
    /// A day's head was pressed: carries that day.
    #[component(event(value = Moment))]
    pub open_day: Event,
    /// The person zoomed the hours: carries the new zoom.
    #[component(event(value = f64))]
    pub zoomed: Event,
}

impl Default for TimelineProps {
    fn default() -> Self {
        Self {
            label: None,
            range: TimelineMode::Week,
            start: Moment::default(),
            slot: super::SNAP_MIN,
            show_now: true,
            zoom: 1.0,
            rows: 0,
            items: Vec::new(),
            created: Event::None,
            open_day: Event::None,
            zoomed: Event::None,
        }
    }
}

/// One thing on the grid: a meeting in its hours, or something that takes whole days.
#[derive(Component, Default)]
#[component(name = "TimelineItem", role = "ListItem", presents = "item")]
pub struct TimelineItemProps {
    /// The item's stable key.
    #[component(key)]
    pub key: String,
    /// The resource it stands for: what a press, a move or a resize carries back.
    #[component(positional)]
    pub item: Option<ResourceId>,
    /// What it is called.
    pub title: Option<String>,
    /// The moment it starts: its day, and the time of day it is at.
    #[component(required)]
    pub start: Moment,
    /// The moment it ends; a day's end for something that takes whole days.
    pub end: Option<Moment>,
    /// Whether it takes the whole day, or the days between its start and end.
    pub all_day: bool,
    /// Where it happens.
    pub location: Option<String>,
    /// The colour it picks from the theme's series.
    pub tone: TimelineTone,
    /// Which column it takes among the events it overlaps, from 1; 0 lays it out by itself.
    pub stack: i64,
    /// It was pressed: carries the resource it stands for.
    #[component(event(value = ResourceId))]
    pub pressed: Event,
    /// It was dragged: carries it and the moment it now starts at, its length unchanged.
    #[component(event(value = ResourceId, at = Moment))]
    pub moved: Event,
    /// Its edge was dragged: carries it and the moment it now ends at.
    #[component(event(value = ResourceId, at = Moment))]
    pub resized: Event,
}

tilo_ui_base::part_type!(TimelineItemProps);

/// The grid's items and each item's own handlers, in order.
fn place(props: Vec<TimelineItemProps>, theme: &Theme) -> (Vec<TimelineItem>, Vec<ItemEvents>) {
    let mut items = Vec::with_capacity(props.len());
    let mut events = Vec::with_capacity(props.len());
    for each in props {
        events.push(ItemEvents {
            press: each.pressed.clone(),
            moved: each.moved.clone(),
            resized: each.resized.clone(),
        });
        items.push(item(each, theme));
    }
    (items, events)
}

fn item(props: TimelineItemProps, theme: &Theme) -> TimelineItem {
    let (start_day, start_min) = local_day_minute(props.start);
    let (end_day, end_min) = match props.end {
        Some(end) => {
            let (end_day, end_min) = local_day_minute(end);
            (
                end_day,
                if end_day == start_day {
                    end_min
                } else {
                    super::MINUTES_PER_DAY
                },
            )
        }
        None => (start_day, super::MINUTES_PER_DAY),
    };
    let (start_min, end_min) = if props.all_day {
        (0, super::MINUTES_PER_DAY)
    } else if end_day == start_day && end_min <= start_min {
        (start_min, (start_min + 15).min(super::MINUTES_PER_DAY))
    } else {
        (start_min, end_min)
    };
    TimelineItem {
        id: props.key,
        resource: props.item.map(|id| id.0),
        title: props.title.unwrap_or_default(),
        location: props.location.unwrap_or_default(),
        tone: props.tone,
        color: props.tone.color(theme),
        stack: props.stack,
        day: start_day,
        end_day: if props.all_day || end_day > start_day {
            end_day
        } else {
            start_day
        },
        start_min,
        end_min,
        clock_end_min: end_min,
        all_day: props.all_day,
    }
}

impl Described for Timeline {
    type Props = TimelineProps;

    fn from_props(key: &str, props: TimelineProps) -> Self {
        let _ = key;
        let theme = Theme::dark();
        let (anchor, _) = local_day_minute(props.start);
        let (items, events) = place(props.items, &theme);
        let mut timeline = Timeline::new(props.range, anchor, items);
        timeline.item_events = events;
        timeline.slot = props.slot.max(1);
        timeline.zoom = props.zoom.max(0.1);
        timeline.min_rows = usize::try_from(props.rows.max(0)).unwrap_or(0);
        timeline.allday_rows = timeline.allday_rows.max(timeline.min_rows);
        timeline.show_now = props.show_now;
        timeline.label = props.label;
        timeline.created = props.created;
        timeline.open_day = props.open_day;
        timeline.zoomed = props.zoomed;
        timeline
    }

    fn set_props(&mut self, props: TimelineProps) -> bool {
        let theme = self.theme_for_props().unwrap_or_else(Theme::dark);
        let (anchor, _) = local_day_minute(props.start);
        let (items, events) = place(props.items, &theme);
        let forward = anchor >= self.anchor;
        self.min_rows = usize::try_from(props.rows.max(0)).unwrap_or(0);
        let period_changed = props.range != self.mode || anchor != self.anchor;
        self.retarget(props.range, anchor, items, period_changed && forward);
        self.item_events = events;
        self.slot = props.slot.max(1);
        self.zoom = props.zoom.max(0.1);
        self.show_now = props.show_now;
        self.label = props.label;
        self.created = props.created;
        self.open_day = props.open_day;
        self.zoomed = props.zoomed;
        true
    }
}
