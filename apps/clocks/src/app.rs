//! Responsive Clocks UI. All controls mutate the durable engine state directly.
use crate::native_timers::{ALARMS_URI, TIMERS_URI, timer_id as native_timer_id};
use crate::{Alarm, ClockState, DueEvent, ShellSnapshot, Timer, WeekdayKey, WorldClock};
use automerge::{AutoCommit, ObjId};
use chrono::{DateTime, NaiveTime, Offset, Utc};
use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};
use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::tokens::{Metrics, spacing::Spacing};
use ui::widget::toolbar::Orientation;
use ui::widget::{
    Actions, Banner, Button, ButtonRole, Card, CardGrid, Column, Container, ControlSize, Dropdown,
    Element, EmptyState, Field, LabelPlacement, ModalDialog, ModelView, NavItem, NavList,
    NavSection, NavShell, ProgressRing, Property, PropertyList, Row, Scrollable, Section,
    SectionHeader, Stack, Text, TextInput, TextInputKind, TimePicker, TimeValue, Toggle, Toolbar,
    Widget,
};
use ui::widget::{AppModel, ModelPath};
use ui::{TextKind, TextRole};
use ui::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use uuid::Uuid;
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// The current UTC time, read through the toolkit clock so captures show the
/// pinned time.
pub(crate) fn now_utc() -> DateTime<Utc> {
    ui::clock::local_now().with_timezone(&Utc)
}

/// The gap between the groups of one screen.
fn group_gap() -> f32 {
    Spacing::BASE.s6
}

fn text(label: &str, role: TextRole) -> Text {
    Text::new(label).with_role(role)
}

/// A secondary button that runs `f`.
fn button(id: &str, label: &str, f: impl FnMut() + 'static) -> Button {
    Button::new(id, label, None).on_effect(f)
}

/// An icon-only plain button that runs `f`.
fn icon_button(id: &str, icon: Icon, name: &str, f: impl FnMut() + 'static) -> Button {
    Button::icon_only(id, icon, name).on_effect(f)
}

pub(crate) fn duration(ms: u64) -> String {
    let seconds = ms / 1_000;
    if seconds >= 3_600 {
        format!(
            "{:02}:{:02}:{:02}",
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        )
    } else {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    }
}

fn remaining_fraction(remaining_ms: u64, duration_ms: u64) -> f32 {
    if duration_ms == 0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_precision_loss,
        reason = "a display fraction only needs f32 precision; timer durations are far below 2^24 ms granularity concerns"
    )]
    let fraction = remaining_ms as f32 / duration_ms as f32;
    fraction.clamp(0.0, 1.0)
}

fn timer_label(input: &str) -> String {
    let label = input.trim();
    if label.is_empty() {
        "Timer".into()
    } else {
        label.chars().take(80).collect()
    }
}

fn duration_component(input: &str, upper_bound: u64) -> Option<u64> {
    let input = input.trim();
    (!input.is_empty() && input.chars().all(|character| character.is_ascii_digit()))
        .then(|| input.parse::<u64>().ok())
        .flatten()
        .filter(|value| *value <= upper_bound)
}

fn custom_timer_duration_seconds(hours: &str, minutes: &str, seconds: &str) -> Option<u64> {
    let hours = duration_component(hours, 99)?;
    let minutes = duration_component(minutes, 59)?;
    let seconds = duration_component(seconds, 59)?;
    hours
        .checked_mul(3_600)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

fn alarm_label(input: &str) -> String {
    let label = input.trim();
    if label.is_empty() {
        "Alarm".into()
    } else {
        label.chars().take(80).collect()
    }
}

const WEEKDAYS: [(WeekdayKey, &str); 7] = [
    (WeekdayKey::Mon, "Mon"),
    (WeekdayKey::Tue, "Tue"),
    (WeekdayKey::Wed, "Wed"),
    (WeekdayKey::Thu, "Thu"),
    (WeekdayKey::Fri, "Fri"),
    (WeekdayKey::Sat, "Sat"),
    (WeekdayKey::Sun, "Sun"),
];

pub(crate) fn repeat_summary(days: &BTreeSet<WeekdayKey>) -> String {
    let weekdays: BTreeSet<WeekdayKey> = WEEKDAYS[..5].iter().map(|(day, _)| *day).collect();
    let weekend: BTreeSet<WeekdayKey> = WEEKDAYS[5..].iter().map(|(day, _)| *day).collect();
    if days.is_empty() {
        "Once".into()
    } else if days.len() == WEEKDAYS.len() {
        "Every day".into()
    } else if *days == weekdays {
        "Weekdays".into()
    } else if *days == weekend {
        "Weekends".into()
    } else {
        WEEKDAYS
            .iter()
            .filter_map(|(day, label)| days.contains(day).then_some(*label))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// The time in `zone`, and how it relates to the local day and time.
fn city_time(zone: &str) -> Option<(String, String)> {
    let tz = zone.parse::<chrono_tz::Tz>().ok()?;
    let here = ui::clock::local_now();
    let there = here.with_timezone(&tz);
    let day = match there.date_naive().cmp(&here.date_naive()) {
        std::cmp::Ordering::Equal => "Today",
        std::cmp::Ordering::Greater => "Tomorrow",
        std::cmp::Ordering::Less => "Yesterday",
    };
    let minutes = there
        .offset()
        .fix()
        .local_minus_utc()
        .saturating_sub(here.offset().local_minus_utc())
        / 60;
    let relation = if minutes == 0 {
        "same time".to_owned()
    } else {
        let direction = if minutes > 0 { "ahead" } else { "behind" };
        let magnitude = minutes.unsigned_abs();
        match (magnitude / 60, magnitude % 60) {
            (0, rest) => format!("{rest} min {direction}"),
            (hours, 0) => format!("{hours} h {direction}"),
            (hours, rest) => format!("{hours} h {rest} min {direction}"),
        }
    };
    Some((
        there.format("%H:%M").to_string(),
        format!("{day}, {relation}"),
    ))
}

/// A city's name from its zone id: `America/New_York` is "New York".
fn city_name(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// A zone's name in a list: "New York (America)".
fn timezone_label(zone: &str) -> String {
    let city = city_name(zone);
    let region = zone.split('/').next().unwrap_or_default();
    if region.is_empty() || region == zone {
        city
    } else {
        format!("{city} ({region})")
    }
}

/// Every zone a city can be added from, as (zone id, label), by label.
fn timezone_choices() -> Vec<(String, String)> {
    const REGIONS: [&str; 9] = [
        "Africa",
        "America",
        "Antarctica",
        "Asia",
        "Atlantic",
        "Australia",
        "Europe",
        "Indian",
        "Pacific",
    ];
    let mut choices: Vec<(String, String)> = chrono_tz::TZ_VARIANTS
        .iter()
        .map(ToString::to_string)
        .filter(|zone| {
            zone.split_once('/')
                .is_some_and(|(region, _)| REGIONS.contains(&region))
        })
        .map(|zone| {
            let label = timezone_label(&zone);
            (zone, label)
        })
        .collect();
    choices.sort_by(|a, b| a.1.cmp(&b.1));
    choices
}

/// The screens of the app, in sidebar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Screen {
    World,
    Alarms,
    Timers,
    Stopwatch,
}

impl Screen {
    const ALL: [Self; 4] = [Self::World, Self::Alarms, Self::Timers, Self::Stopwatch];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::World => "world",
            Self::Alarms => "alarms",
            Self::Timers => "timers",
            Self::Stopwatch => "stopwatch",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::World => "World clocks",
            Self::Alarms => "Alarms",
            Self::Timers => "Timers",
            Self::Stopwatch => "Stopwatch",
        }
    }

    fn from_id(id: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|screen| screen.id() == id)
            .unwrap_or(Self::Timers)
    }
}

/// The fields of a dialog, each label above its control so the controls line up.
fn fields(items: Vec<Element>) -> Column {
    items.into_iter().fold(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(Spacing::BASE.s4),
        Column::push,
    )
}

/// The weekday choices of the new alarm form, one toggle per day.
fn weekday_picker(days: &[ui::LocalState<bool>]) -> Element {
    let mut picker = Toolbar::new(Orientation::Horizontal);
    for ((_, label), state) in WEEKDAYS.iter().zip(days) {
        let day = state.clone();
        picker = picker.push_toggle(label, state.get(), move || day.update(|on| *on = !*on));
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(Spacing::BASE.s2)
            .push(Box::new(text("Repeat", TextRole::Label)))
            .push(Box::new(picker)),
    )
}

/// The current wall-clock second: what a live readout is keyed on.
fn second() -> i64 {
    ui::clock::local_now().timestamp()
}

/// The local time and date: the one hero of the screen.
fn local_time_block() -> Column {
    Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(Spacing::BASE.s1)
        .push(Box::new(text("Local time", TextRole::Caption).dimmed()))
        .push(Box::new(
            Text::dynamic(|| ui::clock::local_now().format("%H:%M").to_string())
                .with_role(TextRole::Display)
                .numeric(),
        ))
        .push(Box::new(
            Text::dynamic(|| ui::clock::local_now().format("%A, %-d %B").to_string())
                .with_role(TextRole::Body)
                .dimmed(),
        ))
}

/// One city, as a card with its time and how far it is from here.
fn city_card(state: &ui::LocalState<ClockState>, clock: &WorldClock) -> Element {
    let id = clock.id;
    let remove = state.clone();
    let (time_zone, relation_zone) = (clock.timezone.clone(), clock.timezone.clone());
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(Spacing::BASE.s1)
        .push(Box::new(
            Text::dynamic(move || {
                city_time(&time_zone).map_or_else(|| "--:--".into(), |(time, _)| time)
            })
            .with_role(TextRole::Title)
            .numeric(),
        ))
        .push(Box::new(
            Text::dynamic(move || {
                city_time(&relation_zone)
                    .map_or_else(|| "Time unavailable".into(), |(_, relation)| relation)
            })
            .with_role(TextRole::Caption)
            .dimmed(),
        ));
    let name = format!("Remove {}", clock.city);
    Box::new(
        Card::new(Box::new(body)).with_title_action(
            &clock.city,
            Box::new(
                icon_button(
                    &format!("clocks.world.{id}.remove"),
                    Icon::Trash2,
                    &name,
                    move || remove.update(|s| s.world_clocks.retain(|c| c.id != id)),
                )
                .tooltip(name.clone()),
            ),
        ),
    )
}

/// Picks a time zone to add as a city; the field filters as the person types.
fn add_city(state: &ui::LocalState<ClockState>) -> Dropdown {
    let picked = ui::LocalState::new(String::new());
    let add_state = state.clone();
    let add_picked = picked.clone();
    let mut add = Dropdown::from_state(Vec::new(), &picked)
        .with_id("clocks.world.add")
        .with_accessible_name("Add city")
        .with_placeholder("Add city")
        .on_change(move |zone| {
            if zone.is_empty() {
                return;
            }
            let zone = zone.to_owned();
            add_state.update(|s| {
                if !s.world_clocks.iter().any(|clock| clock.timezone == zone) {
                    s.world_clocks.push(WorldClock {
                        id: Uuid::new_v4(),
                        city: city_name(&zone),
                        timezone: zone.clone(),
                    });
                }
            });
            add_picked.set(String::new());
        });
    add.set_value_options(timezone_choices());
    add
}

fn world_screen(state: &ui::LocalState<ClockState>) -> Element {
    let list_state = state.clone();
    let cities = ModelView::from_state_by(
        state,
        |s| s.world_clocks.clone(),
        move |s| {
            if s.world_clocks.is_empty() {
                return Box::new(
                    EmptyState::new("Add a city to compare times").with_icon(Icon::Globe),
                ) as Element;
            }
            let cards: Vec<Element> = s
                .world_clocks
                .iter()
                .map(|clock| city_card(&list_state, clock))
                .collect();
            Box::new(Section::titled(
                "Cities",
                vec![Box::new(CardGrid::new(cards))],
            ))
        },
    );
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(group_gap())
            .push(Box::new(
                Row::new()
                    .with_justify(taffy::JustifyContent::SpaceBetween)
                    .push(Box::new(local_time_block()))
                    .push(Box::new(
                        Container::new()
                            .with_no_padding()
                            .with_width(Metrics::BASE.panel_width_utility)
                            .with_child(Box::new(add_city(state))),
                    )),
            ))
            .push(Box::new(cities)),
    )
}

/// What the person has typed into the forms. It outlives the screens, which
/// are rebuilt when a notice appears or goes.
#[derive(Clone)]
struct Drafts {
    alarm_time: ui::LocalState<TimeValue>,
    alarm_label: ui::LocalState<String>,
    alarm_days: Vec<ui::LocalState<bool>>,
    timer_label: ui::LocalState<String>,
    timer_hours: ui::LocalState<String>,
    timer_minutes: ui::LocalState<String>,
    timer_seconds: ui::LocalState<String>,
}

impl Drafts {
    fn new() -> Self {
        Self {
            alarm_time: ui::LocalState::new(TimeValue::new(7, 30)),
            alarm_label: ui::LocalState::new(String::new()),
            alarm_days: WEEKDAYS
                .iter()
                .map(|_| ui::LocalState::new(false))
                .collect(),
            timer_label: ui::LocalState::new(String::new()),
            timer_hours: ui::LocalState::new("00".to_string()),
            timer_minutes: ui::LocalState::new("10".to_string()),
            timer_seconds: ui::LocalState::new("00".to_string()),
        }
    }
}

/// One alarm, as a card with its on/off switch.
fn alarm_card(state: &ui::LocalState<ClockState>, alarm: &Alarm) -> Element {
    let id = alarm.id;
    let toggle = state.clone();
    let remove = state.clone();
    let enabled = ui::LocalState::new(alarm.enabled);
    let body = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(Spacing::BASE.s1)
        .push(Box::new(
            text(
                &alarm.local_time.format("%H:%M").to_string(),
                TextRole::Title,
            )
            .numeric(),
        ))
        .push(Box::new(
            text(&repeat_summary(&alarm.weekdays), TextRole::Caption).dimmed(),
        ));
    let switch = Toggle::from_state(&enabled)
        .with_id(&format!("clocks.alarm.{id}.toggle"))
        .with_accessible_name(&format!("{} on", alarm.label))
        .on_change(move |on| {
            toggle.update(|s| {
                if let Some(a) = s.alarms.iter_mut().find(|a| a.id == id) {
                    a.set_enabled(on);
                }
            });
        });
    Box::new(
        Card::new(Box::new(body))
            .with_title_action(&alarm.label, Box::new(switch))
            .with_footer(Actions::new(
                button(
                    &format!("clocks.alarm.{id}.remove"),
                    "Remove alarm",
                    move || remove.update(|s| s.alarms.retain(|a| a.id != id)),
                )
                .role(ButtonRole::Destructive)
                .control_size(ControlSize::Small),
            )),
    )
}

/// The dialog that adds an alarm.
fn new_alarm_dialog(
    state: &ui::LocalState<ClockState>,
    drafts: &Drafts,
    dialog: &ui::LocalState<Option<Dialog>>,
) -> Element {
    let add_state = state.clone();
    let add_time = drafts.alarm_time.clone();
    let add_label = drafts.alarm_label.clone();
    let add_days = drafts.alarm_days.clone();
    let add_dialog = dialog.clone();
    let cancel = dialog.clone();
    let dismiss = dialog.clone();
    let form = fields(vec![
        Box::new(
            Field::new(
                "Label",
                TextInput::from_state(&drafts.alarm_label)
                    .with_id("clocks.alarm.label")
                    .with_kind(TextInputKind::Name)
                    .with_placeholder("Wake up"),
            )
            .with_label_placement(Some(LabelPlacement::Above)),
        ),
        Box::new(
            Field::new(
                "Time",
                TimePicker::from_state(&drafts.alarm_time)
                    .with_id("clocks.alarm.time")
                    .with_accessible_name("Alarm time"),
            )
            .with_label_placement(Some(LabelPlacement::Above)),
        ),
        weekday_picker(&drafts.alarm_days),
    ]);
    Box::new(
        ModalDialog::new("New alarm", form)
            .open(true)
            .primary_action("Add alarm", move || {
                let t = add_time.get();
                let weekdays: BTreeSet<WeekdayKey> = WEEKDAYS
                    .iter()
                    .zip(&add_days)
                    .filter_map(|((day, _), on)| on.get().then_some(*day))
                    .collect();
                add_state.update(|s| {
                    s.alarms.push(Alarm {
                        id: Uuid::new_v4(),
                        label: alarm_label(&add_label.get()),
                        local_time: NaiveTime::from_hms_opt(t.hours.into(), t.minutes.into(), 0)
                            .unwrap_or_default(),
                        weekdays,
                        enabled: true,
                        last_fired_local_date: None,
                        next_due_utc: None,
                    });
                });
                add_label.set(String::new());
                for day in &add_days {
                    day.set(false);
                }
                add_dialog.set(None);
            })
            .secondary_action("Cancel", move || cancel.set(None))
            .on_dismiss(move || dismiss.set(None)),
    )
}

fn alarms_screen(state: &ui::LocalState<ClockState>) -> Element {
    let list_state = state.clone();
    let list = ModelView::from_state(state, move |s| {
        if s.alarms.is_empty() {
            return Box::new(EmptyState::new("No alarms set").with_icon(Icon::Bell)) as Element;
        }
        let cards: Vec<Element> = s
            .alarms
            .iter()
            .map(|alarm| alarm_card(&list_state, alarm))
            .collect();
        Box::new(Section::new(vec![Box::new(CardGrid::new(cards))]))
    });
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(list)),
    )
}

/// A timer's controls: pause or resume, one more minute, remove.
fn timer_actions(state: &ui::LocalState<ClockState>, timer: &Timer, finished: bool) -> Actions {
    let id = timer.id;
    let running = timer.ends_at_utc.is_some();
    let toggle = state.clone();
    let add_minute = state.clone();
    let remove = state.clone();
    Actions::new(
        button(
            &format!("clocks.timer.{id}.toggle"),
            if running {
                "Pause"
            } else if finished {
                "Restart"
            } else {
                "Resume"
            },
            move || {
                toggle.update(|s| {
                    if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                        if t.ends_at_utc.is_some() {
                            t.pause(now_utc());
                        } else {
                            if finished {
                                t.paused_remaining_ms = None;
                            }
                            t.start(now_utc());
                        }
                    }
                });
            },
        )
        .with_icon(if running { Icon::Pause } else { Icon::Play })
        .control_size(ControlSize::Small),
    )
    .secondary(
        button(
            &format!("clocks.timer.{id}.add-minute"),
            "Add 1 min",
            move || {
                add_minute.update(|s| {
                    if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                        let now = now_utc();
                        let left = t.remaining(now).saturating_add(Duration::from_secs(60));
                        t.paused_remaining_ms =
                            Some(left.as_millis().try_into().unwrap_or(u64::MAX));
                        t.ends_at_utc = None;
                        t.start(now);
                    }
                });
            },
        )
        .with_accessible_name(&format!("Add one minute to {}", timer.label))
        .control_size(ControlSize::Small),
    )
    .secondary(
        button(&format!("clocks.timer.{id}.remove"), "Remove", move || {
            remove.update(|s| s.timers.retain(|t| t.id != id));
        })
        .role(ButtonRole::Destructive)
        .with_accessible_name(&format!("Remove {}", timer.label))
        .control_size(ControlSize::Small),
    )
}

/// What a timer's ring shows changes every second while it runs: the ring is
/// rebuilt on its own, so the card's buttons keep their state.
fn timer_ring(state: &ui::LocalState<ClockState>, id: Uuid) -> ModelView<RingKey> {
    ModelView::from_state_by(
        state,
        move |s| {
            let timer = s.timers.iter().find(|t| t.id == id);
            (
                second(),
                timer.map(|t| (t.paused_remaining_ms, t.ends_at_utc)),
            )
        },
        move |s| {
            let Some(timer) = s.timers.iter().find(|t| t.id == id) else {
                return Box::new(Row::new()) as Element;
            };
            let remaining_ms = timer
                .remaining(now_utc())
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            let ring = ProgressRing::new(f64::from(remaining_fraction(
                remaining_ms,
                timer.duration_ms,
            )))
            .value(duration(remaining_ms))
            .caption(match timer_phase(timer) {
                TimerPhase::Running => "Running",
                TimerPhase::Finished => "Finished",
                TimerPhase::Paused => "Paused",
            });
            Box::new(
                Row::new()
                    .with_justify(taffy::JustifyContent::Center)
                    .push(Box::new(ring)),
            )
        },
    )
}

/// What a ring's key holds: the second, and the timer's countdown fields.
type RingKey = (i64, Option<(Option<u64>, Option<DateTime<Utc>>)>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerPhase {
    Running,
    Paused,
    Finished,
}

fn timer_phase(timer: &Timer) -> TimerPhase {
    if timer.ends_at_utc.is_some() {
        TimerPhase::Running
    } else if timer.paused_remaining_ms == Some(0) {
        TimerPhase::Finished
    } else {
        TimerPhase::Paused
    }
}

/// One running or paused timer, as a card with its controls.
fn timer_card(state: &ui::LocalState<ClockState>, timer: &Timer) -> Element {
    let id = timer.id;
    let pin = state.clone();
    let float_name = if timer.pinned_overlay {
        "Stop floating timer"
    } else {
        "Float timer above other windows"
    };
    let pin_button = icon_button(
        &format!("clocks.timer.{id}.pin"),
        Icon::PictureInPicture2,
        float_name,
        move || {
            pin.update(|s| {
                if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                    t.pinned_overlay = !t.pinned_overlay;
                }
            });
        },
    )
    .tooltip(float_name);
    let finished = timer_phase(timer) == TimerPhase::Finished;
    Box::new(
        Card::new(Box::new(timer_ring(state, id)))
            .with_title_action(&timer.label, Box::new(pin_button))
            .with_footer(timer_actions(state, timer, finished)),
    )
}

/// The dialog that starts a timer of any length.
fn new_timer_dialog(
    state: &ui::LocalState<ClockState>,
    drafts: &Drafts,
    validation_error: &ui::LocalState<Option<String>>,
    dialog: &ui::LocalState<Option<Dialog>>,
) -> Element {
    // Keep the components as distinct fields. A spinner row looked compact at
    // default size but allowed its values and arrow targets to collide at 200%
    // text, which made setting a duration unreliable with touch or a screen
    // magnifier.
    let add_state = state.clone();
    let add_hours = drafts.timer_hours.clone();
    let add_minutes = drafts.timer_minutes.clone();
    let add_seconds = drafts.timer_seconds.clone();
    let add_label = drafts.timer_label.clone();
    let add_error = validation_error.clone();
    let add_dialog = dialog.clone();
    let cancel = dialog.clone();
    let dismiss = dialog.clone();
    let duration_field = |label: &str, id: &str, name: &str, state: &ui::LocalState<String>| {
        Field::new(
            label,
            TextInput::from_state(state)
                .with_id(id)
                .with_kind(TextInputKind::Number)
                .with_accessible_name(name)
                .with_placeholder("00"),
        )
        .with_label_placement(Some(LabelPlacement::Above))
    };
    let error = ModelView::from_state(validation_error, |message| {
        message.as_ref().map_or_else(
            || Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element,
            |message| {
                Box::new(Banner::error("Timer needs a duration").with_message(message.clone()))
            },
        )
    });
    let form = fields(vec![
        Box::new(
            Field::new(
                "Label",
                TextInput::from_state(&drafts.timer_label)
                    .with_id("clocks.timer.custom.label")
                    .with_kind(TextInputKind::Name)
                    .with_accessible_name("Timer label")
                    .with_placeholder("Tea"),
            )
            .with_label_placement(Some(LabelPlacement::Above)),
        ),
        Box::new(duration_field(
            "Hours",
            "clocks.timer.custom.hours",
            "Timer duration hours",
            &drafts.timer_hours,
        )),
        Box::new(duration_field(
            "Minutes",
            "clocks.timer.custom.minutes",
            "Timer duration minutes",
            &drafts.timer_minutes,
        )),
        Box::new(duration_field(
            "Seconds",
            "clocks.timer.custom.seconds",
            "Timer duration seconds",
            &drafts.timer_seconds,
        )),
        Box::new(error),
    ]);
    Box::new(
        ModalDialog::new("New timer", form)
            .open(true)
            .primary_action("Start timer", move || {
                let Some(secs) = custom_timer_duration_seconds(
                    &add_hours.get(),
                    &add_minutes.get(),
                    &add_seconds.get(),
                ) else {
                    add_error.set(Some(
                        "Use whole numbers: hours 0–99 and minutes and seconds 0–59.".into(),
                    ));
                    return;
                };
                if secs == 0 {
                    add_error.set(Some(
                        "Choose a duration greater than zero before starting a timer.".into(),
                    ));
                    return;
                }
                let mut t = Timer::new(timer_label(&add_label.get()), Duration::from_secs(secs));
                t.start(now_utc());
                add_state.update(|s| s.timers.push(t));
                add_label.set(String::new());
                add_error.set(None);
                add_dialog.set(None);
            })
            .secondary_action("Cancel", move || cancel.set(None))
            .on_dismiss(move || dismiss.set(None)),
    )
}

/// One-tap timers for common jobs.
fn quick_starts(state: &ui::LocalState<ClockState>) -> Row {
    let mut presets = Row::new();
    for (label, button_label, secs) in [
        ("Tea", "Tea, 5 min", 300),
        ("Break", "Break, 10 min", 600),
        ("Focus", "Focus, 25 min", 1500),
        ("Laundry", "Laundry, 45 min", 2700),
    ] {
        let st = state.clone();
        presets = presets.push(Box::new(button(
            &format!("clocks.preset.{secs}"),
            button_label,
            move || {
                let mut t = Timer::new(label, Duration::from_secs(secs));
                t.start(now_utc());
                st.update(|s| s.timers.push(t));
            },
        )));
    }
    presets
}

/// What shapes the timers screen: each timer's name and how it can be driven.
type TimersKey = Vec<(Uuid, String, TimerPhase, bool)>;

fn timers_key(state: &ClockState) -> TimersKey {
    state
        .timers
        .iter()
        .map(|t| (t.id, t.label.clone(), timer_phase(t), t.pinned_overlay))
        .collect()
}

fn timers_screen(state: &ui::LocalState<ClockState>) -> Element {
    let page_state = state.clone();
    Box::new(ModelView::from_state_by(state, timers_key, move |s| {
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(group_gap());
        if !s.timers.is_empty() {
            let cards: Vec<Element> = s
                .timers
                .iter()
                .map(|timer| timer_card(&page_state, timer))
                .collect();
            page = page.push(Box::new(
                Section::new(vec![Box::new(CardGrid::new(cards))])
                    .with_header(SectionHeader::heading("Active timers").count(s.timers.len())),
            ));
        }
        Box::new(page.push(Box::new(Section::titled(
            "Quick starts",
            vec![Box::new(quick_starts(&page_state))],
        ))))
    }))
}

/// How long the stopwatch has run, in milliseconds.
fn stopwatch_elapsed_ms(state: &ClockState) -> u64 {
    state
        .stopwatch
        .elapsed(now_utc())
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

/// What shapes the stopwatch screen: whether it runs, floats or has anything
/// to reset, and the laps. The readout ticks on its own.
type StopwatchKey = (bool, bool, bool, Vec<u64>);

fn stopwatch_key(state: &ClockState) -> StopwatchKey {
    (
        state.stopwatch.started_at_utc.is_some(),
        state.stopwatch.pinned_overlay,
        state.stopwatch.accumulated_ms > 0,
        state.stopwatch.laps_ms.clone(),
    )
}

/// Start or pause, lap, reset, and float.
fn stopwatch_controls(controls: &ui::LocalState<ClockState>, s: &ClockState) -> Row {
    let running = s.stopwatch.started_at_utc.is_some();
    let (start, lap, reset, float) = (
        controls.clone(),
        controls.clone(),
        controls.clone(),
        controls.clone(),
    );
    Row::new()
        .push(Box::new(
            button(
                "clocks.stopwatch.toggle",
                if running { "Pause" } else { "Start" },
                move || {
                    start.update(|s| {
                        if s.stopwatch.started_at_utc.is_some() {
                            s.stopwatch.pause(now_utc());
                        } else {
                            s.stopwatch.start(now_utc());
                        }
                    });
                },
            )
            .role(ButtonRole::Primary)
            .with_icon(if running { Icon::Pause } else { Icon::Play }),
        ))
        .push(Box::new(
            button("clocks.stopwatch.lap", "Lap", move || {
                lap.update(|s| s.stopwatch.lap(now_utc()));
            })
            .with_enabled(running),
        ))
        .push(Box::new(
            button("clocks.stopwatch.reset", "Reset", move || {
                reset.update(|s| s.stopwatch.reset());
            })
            .with_enabled(stopwatch_elapsed_ms(s) != 0 || !s.stopwatch.laps_ms.is_empty()),
        ))
        .push(Box::new(
            button(
                "clocks.stopwatch.pin",
                if s.stopwatch.pinned_overlay {
                    "Stop floating"
                } else {
                    "Float above other windows"
                },
                move || {
                    float.update(|s| s.stopwatch.pinned_overlay = !s.stopwatch.pinned_overlay);
                },
            )
            .role(ButtonRole::Plain)
            .with_icon(Icon::PictureInPicture2),
        ))
}

/// The laps, newest first, each with how long it lasted.
fn lap_list(s: &ClockState) -> Section {
    // Each lap is stored as the time on the clock when it was taken; the
    // list shows how long each one lasted.
    let mut previous = 0;
    let mut laps: Vec<Property> = s
        .stopwatch
        .laps_ms
        .iter()
        .enumerate()
        .map(|(i, ms)| {
            let lap = ms.saturating_sub(previous);
            previous = *ms;
            Property::new(format!("Lap {}", i.saturating_add(1)), duration(lap))
                .with_kind(TextKind::Numeric)
        })
        .collect();
    laps.reverse();
    Section::new(vec![Box::new(PropertyList::new(laps))])
        .with_header(SectionHeader::heading("Laps").count(s.stopwatch.laps_ms.len()))
}

fn stopwatch_screen(state: &ui::LocalState<ClockState>) -> Element {
    let controls = state.clone();
    let readout = state.clone();
    Box::new(ModelView::from_state_by(state, stopwatch_key, move |s| {
        let readout = readout.clone();
        let mut page = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(group_gap())
            .push(Box::new(
                Text::dynamic(move || readout.with(|s| duration(stopwatch_elapsed_ms(s))))
                    .with_role(TextRole::Display)
                    .numeric(),
            ))
            .push(Box::new(stopwatch_controls(&controls, s)));
        if !s.stopwatch.laps_ms.is_empty() {
            page = page.push(Box::new(lap_list(s)));
        }
        Box::new(page)
    }))
}

/// A finished timer or a ringing alarm, shown until it is dismissed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Notice {
    pub(crate) title: String,
    pub(crate) body: String,
    /// The finished timer this notice can restart.
    pub(crate) restart: Option<Uuid>,
}

fn notice_banner(
    notice: &Notice,
    notices: &ui::LocalState<Option<Notice>>,
    state: &ui::LocalState<ClockState>,
) -> Element {
    let dismiss = notices.clone();
    let mut actions = Actions::new(button("clocks.notice.dismiss", "Dismiss", move || {
        dismiss.set(None);
    }));
    if let Some(id) = notice.restart {
        let notices = notices.clone();
        let state = state.clone();
        actions = actions.secondary(button(
            "clocks.notice.restart",
            "Restart timer",
            move || {
                state.update(|s| {
                    if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                        t.paused_remaining_ms = None;
                        t.start(now_utc());
                    }
                });
                notices.set(None);
            },
        ));
    }
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(Spacing::BASE.s2)
            .push(Box::new(
                Banner::warning(notice.title.clone()).with_message(notice.body.clone()),
            ))
            .push(Box::new(actions)),
    )
}

fn page(screen: Screen, state: &ui::LocalState<ClockState>) -> Element {
    match screen {
        Screen::World => world_screen(state),
        Screen::Alarms => alarms_screen(state),
        Screen::Timers => timers_screen(state),
        Screen::Stopwatch => stopwatch_screen(state),
    }
}

/// What the open screen shows: which screen, and the notice above it.
type ViewKey = (Option<String>, Option<Notice>);

/// The dialogs the screens open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Dialog {
    NewAlarm,
    NewTimer,
}

fn screen_of(selection: Option<&str>) -> Screen {
    selection.map_or(Screen::Timers, Screen::from_id)
}

/// What the open screen adds to the window header: the way to make a new
/// alarm or timer.
fn header_action(
    selection: &ui::LocalState<Option<String>>,
    dialog: &ui::LocalState<Option<Dialog>>,
) -> Element {
    let dialog = dialog.clone();
    Box::new(ModelView::from_state(selection, move |selected| {
        let open = dialog.clone();
        match screen_of(selected.as_deref()) {
            Screen::Alarms => Box::new(
                button("clocks.alarm.new", "New alarm", move || {
                    open.set(Some(Dialog::NewAlarm));
                })
                .with_icon(Icon::Plus)
                .role(ButtonRole::Primary),
            ) as Element,
            Screen::Timers => Box::new(
                button("clocks.timer.new", "New timer", move || {
                    open.set(Some(Dialog::NewTimer));
                })
                .with_icon(Icon::Plus)
                .role(ButtonRole::Primary),
            ),
            Screen::World | Screen::Stopwatch => Box::new(Row::new()),
        }
    }))
}

/// The window: the screens in the sidebar, the open one beside it.
fn surface(
    selection: &ui::LocalState<Option<String>>,
    state: &ui::LocalState<ClockState>,
    notice: &ui::LocalState<Option<Notice>>,
    validation_error: &ui::LocalState<Option<String>>,
    dialog: &ui::LocalState<Option<Dialog>>,
    view: &ui::LocalState<ViewKey>,
    drafts: &Drafts,
) -> NavShell {
    let sidebar = NavList::new(vec![NavSection::untitled(
        "screens",
        Screen::ALL
            .into_iter()
            .map(|screen| NavItem::new(screen.id(), screen.title()))
            .collect(),
    )])
    .with_id("clocks.screens")
    .with_accessible_name("Clocks")
    .bind_selection(selection);
    let (pages_state, banner_notice) = (state.clone(), notice.clone());
    // The banner sits above the open screen while a notice is showing, so the
    // screen is built again when either changes; the scroll area stays.
    let pages = ModelView::from_state(view, move |(selected, notice)| {
        let mut column = Column::new().with_intrinsic_height();
        if let Some(notice) = notice {
            column = column
                .push(notice_banner(notice, &banner_notice, &pages_state))
                .with_gap(group_gap());
        }
        Box::new(column.push(page(screen_of(selected.as_deref()), &pages_state)))
    });
    let body = Column::new().with_no_padding().push(Box::new(
        Scrollable::new_vertical()
            .fill_parent_height()
            .with_child(Box::new(pages)),
    ));
    let (dialog_state, dialog_error) = (state.clone(), validation_error.clone());
    let dialog_drafts = drafts.clone();
    let dialog_open = dialog.clone();
    let dialogs = ModelView::from_state(dialog, move |open| match open {
        Some(Dialog::NewAlarm) => new_alarm_dialog(&dialog_state, &dialog_drafts, &dialog_open),
        Some(Dialog::NewTimer) => {
            new_timer_dialog(&dialog_state, &dialog_drafts, &dialog_error, &dialog_open)
        }
        None => Box::new(Row::new()),
    });
    let screen = screen_of(selection.get().as_deref());
    NavShell::new()
        .sidebar(Box::new(sidebar))
        .content(Box::new(
            Stack::new().push(Box::new(body)).push(Box::new(dialogs)),
        ))
        .content_header(
            screen.title(),
            vec![
                header_action(selection, dialog),
                Box::new(ui::widget::history_button()),
            ],
        )
}

pub struct ClocksApp {
    inner: NavShell,
    state: ui::LocalState<ClockState>,
    pub(crate) actions: crate::resource::ActionSink,
    hosted: bool,
    generation: u64,
    last_saved: String,
    last_notification: Option<(uuid::Uuid, String, String)>,
    notice: ui::LocalState<Option<Notice>>,
    persistent: bool,
    next_clock_tick: ui::time::Instant,
    /// The open screen's id, bound to the sidebar list.
    selection: ui::LocalState<Option<String>>,
    /// The screen the window header currently names.
    shown: Screen,
    /// The dialog that is open over the window.
    dialog: ui::LocalState<Option<Dialog>>,
    /// The open screen and notice the page was last built for.
    view: ui::LocalState<ViewKey>,
    /// What has been typed into the dialogs.
    drafts: Drafts,
    /// The host's notifications, resolved on first use.
    notifications: Option<std::sync::Arc<dyn ui::notify::Notifier>>,
    /// Due alerts not yet shown: tag, title, body, activation URI.
    pending_notifications: Vec<(String, String, String, &'static str)>,
    next_notification_attempt: ui::time::Instant,
    last_scheduled_timers: String,
    next_schedule_attempt: ui::time::Instant,
    /// Activation URIs of clicked notifications, from any thread.
    activations: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

/// The state a named screenshot scenario shows: the data, the open screen and
/// the finished timer to announce, if any.
fn scenario_fixture(name: &str) -> (ClockState, Screen, Option<Uuid>) {
    let now = now_utc();
    let local = |hours, minutes| NaiveTime::from_hms_opt(hours, minutes, 0).unwrap_or_default();
    let mut value = ClockState::default();
    let mut finished = None;
    let screen = match name {
        "world" => {
            for (city, timezone) in [
                ("New York", "America/New_York"),
                ("London", "Europe/London"),
                ("Tokyo", "Asia/Tokyo"),
                ("Sydney", "Australia/Sydney"),
            ] {
                value.world_clocks.push(WorldClock {
                    id: Uuid::new_v4(),
                    city: city.into(),
                    timezone: timezone.into(),
                });
            }
            Screen::World
        }
        "alarms" | "new-alarm" => {
            use WeekdayKey::{Fri, Mon, Sat, Sun, Thu, Tue, Wed};
            for (label, time, days, enabled) in [
                ("Wake up", local(7, 30), vec![Mon, Tue, Wed, Thu, Fri], true),
                ("Gym", local(18, 15), vec![Tue, Thu], false),
                (
                    "Take medication",
                    local(21, 0),
                    vec![Mon, Tue, Wed, Thu, Fri, Sat, Sun],
                    true,
                ),
                ("Farmers market", local(9, 0), vec![Sat], true),
            ] {
                value.alarms.push(Alarm {
                    id: Uuid::new_v4(),
                    label: label.into(),
                    local_time: time,
                    weekdays: days.into_iter().collect(),
                    enabled,
                    last_fired_local_date: None,
                    next_due_utc: None,
                });
            }
            Screen::Alarms
        }
        "stopwatch" => {
            value.stopwatch.started_at_utc =
                now.checked_sub_signed(chrono::Duration::milliseconds(125_000));
            value.stopwatch.laps_ms = vec![41_000, 83_000, 118_000];
            Screen::Stopwatch
        }
        "running" | "timers" | "new-timer" => {
            let mut tea = Timer::new("Tea", Duration::from_secs(300));
            tea.paused_remaining_ms = Some(272_000);
            tea.start(now);
            tea.pinned_overlay = name == "running";
            value.timers.push(tea);
            if name == "timers" {
                let mut pasta = Timer::new("Pasta", Duration::from_secs(720));
                pasta.paused_remaining_ms = Some(495_000);
                value.timers.push(pasta);
            }
            Screen::Timers
        }
        "ringing" => {
            let mut tea = Timer::new("Tea", Duration::from_secs(300));
            tea.paused_remaining_ms = Some(0);
            finished = Some(tea.id);
            value.timers.push(tea);
            let mut laundry = Timer::new("Laundry", Duration::from_secs(2_700));
            laundry.paused_remaining_ms = Some(1_410_000);
            laundry.start(now);
            value.timers.push(laundry);
            Screen::Timers
        }
        "empty-world" => Screen::World,
        "empty-alarms" => Screen::Alarms,
        _ => Screen::Timers,
    };
    (value, screen, finished)
}

impl ClocksApp {
    #[must_use]
    pub fn new() -> Self {
        // A capture runs on a private profile and never reads or writes the
        // person's saved clocks; its scenario stages the state.
        if ui::time::wall_frozen() {
            return Self::with_state(ClockState::default(), Screen::Timers, false);
        }
        let value = ClockState::load();
        // Reopen on the activity that most likely brought the user back,
        // while a quiet first launch begins with the useful at-a-glance view.
        let screen = if !value.timers.is_empty() {
            Screen::Timers
        } else if value.stopwatch.started_at_utc.is_some() || value.stopwatch.accumulated_ms > 0 {
            Screen::Stopwatch
        } else if value.alarms.is_empty() {
            Screen::World
        } else {
            Screen::Alarms
        };
        Self::with_state(value, screen, true)
    }
    #[must_use]
    pub fn running_demo() -> Self {
        Self::scenario("running")
    }
    #[must_use]
    pub fn scenario(name: &str) -> Self {
        let mut app = Self::with_state(ClockState::default(), Screen::Timers, false);
        app.stage_scenario(name);
        app
    }

    /// Replace the state with what the named scenario shows. The app stops
    /// persisting: a scenario is a fixture, not the person's clocks.
    pub(crate) fn stage_scenario(&mut self, name: &str) {
        let name = name.strip_suffix("-narrow").unwrap_or(name);
        let (value, screen, finished) = scenario_fixture(name);
        if name == "new-alarm" {
            for ((day, _), on) in WEEKDAYS.iter().zip(&self.drafts.alarm_days) {
                on.set(!matches!(day, WeekdayKey::Sat | WeekdayKey::Sun));
            }
        }
        self.dialog.set(match name {
            "new-alarm" => Some(Dialog::NewAlarm),
            "new-timer" => Some(Dialog::NewTimer),
            _ => None,
        });
        self.persistent = false;
        self.state.set(value);
        self.selection.set(Some(screen.id().to_owned()));
        self.notice.set(None);
        if let Some(id) = finished {
            self.remember_due(DueEvent::Timer {
                id,
                label: "Tea".into(),
                native_scheduled: false,
            });
        }
    }

    fn with_state(value: ClockState, screen: Screen, persistent: bool) -> Self {
        // Register earlier timers before the first UI event can delete them.
        let mut value = value;
        value
            .native_timer_owners
            .extend(value.timers.iter().map(|timer| timer.id));
        let state = ui::LocalState::new(value);
        let notice = ui::LocalState::new(None);
        let validation_error = ui::LocalState::new(None);
        let selection = ui::LocalState::new(Some(screen.id().to_owned()));
        let dialog = ui::LocalState::new(None);
        let view = ui::LocalState::new((Some(screen.id().to_owned()), None));
        let drafts = Drafts::new();
        let activations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        if persistent {
            let sink = activations.clone();
            let wake = ui::wake::WakeHandle::new();
            ui::notify::set_activation_handler(move |uri| {
                if let Ok(mut pending) = sink.lock() {
                    pending.push(uri.to_owned());
                }
                wake.wake();
            });
        }
        Self {
            inner: surface(
                &selection,
                &state,
                &notice,
                &validation_error,
                &dialog,
                &view,
                &drafts,
            ),
            state,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
            generation: 0,
            last_saved: String::new(),
            last_notification: None,
            notice,
            persistent,
            next_clock_tick: ui::time::now(),
            selection,
            shown: screen,
            dialog,
            view,
            drafts,
            notifications: None,
            pending_notifications: Vec::new(),
            next_notification_attempt: ui::time::now(),
            last_scheduled_timers: String::new(),
            next_schedule_attempt: ui::time::now(),
            activations,
        }
    }

    /// Retain the latest due notice in every subsequent transport snapshot.
    ///
    /// The shell polls a replace-in-place state file, so a one-frame zero
    /// countdown can be overwritten by the inactive tombstone before the
    /// shell observes it. A stable notification ID lets the producer keep the
    /// notice attached to later tombstones while the shell deduplicates it.
    fn remember_due(&mut self, event: DueEvent) {
        let native = match &event {
            DueEvent::Timer {
                id,
                native_scheduled,
                ..
            } if ui::host_notifications::SUPPORTED => {
                Some((native_timer_id(*id), *native_scheduled))
            }
            DueEvent::Timer { .. } | DueEvent::Alarm { .. } => None,
        };
        let restart = match &event {
            DueEvent::Timer { id, .. } => Some(*id),
            DueEvent::Alarm { .. } => None,
        };
        let (title, body, activation) = match event {
            DueEvent::Alarm { label, .. } => ("Alarm".to_owned(), label, ALARMS_URI),
            DueEvent::Timer { label, .. } => ("Timer finished".to_owned(), label, TIMERS_URI),
        };
        let notice = (uuid::Uuid::new_v4(), title, body);
        if self.persistent {
            let (tag, scheduled) = native.unwrap_or_else(|| {
                (
                    notice.0.simple().to_string().chars().take(16).collect(),
                    false,
                )
            });
            if !scheduled {
                self.pending_notifications.push((
                    tag,
                    notice.1.clone(),
                    notice.2.clone(),
                    activation,
                ));
            }
        }
        self.last_notification = Some(notice);
        self.notice.set(
            self.last_notification
                .as_ref()
                .map(|(_, title, body)| Notice {
                    title: title.clone(),
                    body: body.clone(),
                    restart,
                }),
        );
    }

    /// The host's notifications, resolved once.
    fn notifier(&mut self) -> std::sync::Arc<dyn ui::notify::Notifier> {
        self.notifications
            .get_or_insert_with(|| ui::host::current().notifications)
            .clone()
    }

    /// Show due alerts the platform did not deliver itself. A host without
    /// notifications drops them (the in-app notice and the shell still show
    /// them); a service that failed is tried again shortly.
    fn deliver_native_notifications(&mut self) {
        if self.pending_notifications.is_empty() || ui::time::now() < self.next_notification_attempt
        {
            return;
        }
        self.next_notification_attempt = retry_at();
        let host = self.notifier();
        self.pending_notifications
            .retain(|(tag, title, body, activation)| {
                let notification =
                    ui::notify::Notification::new(tag.as_str(), title.as_str(), body.as_str())
                        .with_activation(*activation);
                match host.show(&notification) {
                    Ok(()) => false,
                    Err(ui::notify::NotifyError::Failed(error)) => {
                        tracing::warn!(%error, "Clocks notification delivery");
                        true
                    }
                    Err(error) => {
                        tracing::debug!(%error, "Clocks notifications unavailable");
                        false
                    }
                }
            });
    }

    /// Hand running timers to the platform's scheduler, where it keeps
    /// notifications for an app that is not running.
    fn schedule_native_timers(&mut self, now: chrono::DateTime<Utc>) {
        if !self.persistent
            || !schedules_notifications()
            || ui::time::now() < self.next_schedule_attempt
        {
            return;
        }
        let encoded = self
            .state
            .with(|s| serde_json::to_string(&s.timers).unwrap_or_default());
        if encoded == self.last_scheduled_timers {
            return;
        }
        match self.reconcile_native_timers(now) {
            Ok(()) => {
                self.last_scheduled_timers = self
                    .state
                    .with(|s| serde_json::to_string(&s.timers).unwrap_or_default());
            }
            Err(error) => {
                tracing::warn!(%error, "Clocks timer scheduling");
                self.next_schedule_attempt = retry_at();
            }
        }
    }

    fn reconcile_native_timers(&mut self, now: chrono::DateTime<Utc>) -> Result<(), String> {
        let host = self.notifier();
        let mut result = Ok(());
        self.state.update(|state| {
            result = crate::native_timers::reconcile(state, &*host, now, |state| {
                state.save().map_err(|error| error.to_string())
            });
        });
        result
    }

    /// Open the section a clicked notification names.
    fn apply_activations(&self) {
        let uris = self
            .activations
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for uri in uris {
            match uri.as_str() {
                TIMERS_URI => self.selection.set(Some(Screen::Timers.id().to_owned())),
                ALARMS_URI => self.selection.set(Some(Screen::Alarms.id().to_owned())),
                _ => {}
            }
        }
    }
}

/// When a notification service that failed is tried again.
fn retry_at() -> ui::time::Instant {
    let now = ui::time::now();
    now.checked_add(Duration::from_secs(5)).unwrap_or(now)
}

/// Whether the platform keeps scheduled notifications for an app that is
/// not running.
fn schedules_notifications() -> bool {
    !matches!(
        ui::notify::availability(ui::notify::Feature::Schedule),
        ui::notify::Availability::Unsupported(_)
    )
}

impl Default for ClocksApp {
    fn default() -> Self {
        Self::new()
    }
}
impl ClocksApp {
    #[expect(
        clippy::too_many_lines,
        reason = "one flat match arm per Clocks operation; each arm is a short state update"
    )]
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        ty: &str,
        payload: &[u8],
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        if ty != crate::actions::OPERATION_TYPE {
            return self
                .inner
                .start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        kind,
                        ty,
                        payload,
                    ),
                    doc,
                    path,
                )
                .map(|started| started.is_some());
        }
        let Some(op) = crate::actions::decode(ty, payload)? else {
            return Ok(false);
        };
        match op {
            crate::actions::ClockOperation::NewTimer { seconds, label } => {
                let mut t = Timer::new(label, Duration::from_secs(seconds));
                t.start(now_utc());
                self.state.update(|s| s.timers.push(t));
            }
            crate::actions::ClockOperation::NewAlarm {
                hours,
                minutes,
                label,
            } => self.state.update(|s| {
                if let Some(local_time) = NaiveTime::from_hms_opt(hours.into(), minutes.into(), 0) {
                    s.alarms.push(Alarm {
                        id: Uuid::new_v4(),
                        label: alarm_label(&label),
                        local_time,
                        weekdays: BTreeSet::new(),
                        enabled: true,
                        last_fired_local_date: None,
                        next_due_utc: None,
                    });
                }
            }),
            crate::actions::ClockOperation::AddWorldClock { city, timezone } => {
                self.state.update(|s| {
                    if timezone.parse::<chrono_tz::Tz>().is_ok()
                        && !s
                            .world_clocks
                            .iter()
                            .any(|clock| clock.timezone == timezone)
                    {
                        s.world_clocks.push(WorldClock {
                            id: Uuid::new_v4(),
                            city,
                            timezone,
                        });
                    }
                });
            }
            crate::actions::ClockOperation::ToggleTimer { id } => self.state.update(|s| {
                if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                    if t.ends_at_utc.is_some() {
                        t.pause(now_utc());
                    } else {
                        t.start(now_utc());
                    }
                }
            }),
            crate::actions::ClockOperation::AddMinute { id } => self.state.update(|s| {
                if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                    let left = t
                        .remaining(now_utc())
                        .saturating_add(Duration::from_secs(60));
                    t.paused_remaining_ms = Some(left.as_millis().try_into().unwrap_or(u64::MAX));
                    t.ends_at_utc = None;
                    t.start(now_utc());
                }
            }),
            crate::actions::ClockOperation::SetTimerOverlay { id, pinned } => {
                self.state.update(|s| {
                    if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                        t.pinned_overlay = pinned;
                    }
                });
            }
            crate::actions::ClockOperation::RemoveTimer { id } => {
                self.state.update(|s| s.timers.retain(|t| t.id != id));
            }
            crate::actions::ClockOperation::ToggleAlarm { id } => self.state.update(|s| {
                if let Some(alarm) = s.alarms.iter_mut().find(|alarm| alarm.id == id) {
                    alarm.set_enabled(!alarm.enabled);
                }
            }),
            crate::actions::ClockOperation::RemoveAlarm { id } => self
                .state
                .update(|s| s.alarms.retain(|alarm| alarm.id != id)),
            crate::actions::ClockOperation::RemoveWorldClock { id } => self
                .state
                .update(|s| s.world_clocks.retain(|clock| clock.id != id)),
            crate::actions::ClockOperation::ToggleStopwatch => self.state.update(|s| {
                if s.stopwatch.started_at_utc.is_some() {
                    s.stopwatch.pause(now_utc());
                } else {
                    s.stopwatch.start(now_utc());
                }
            }),
            crate::actions::ClockOperation::ResetStopwatch => {
                self.state.update(|s| s.stopwatch.reset());
            }
            crate::actions::ClockOperation::Lap => {
                self.state.update(|s| s.stopwatch.lap(now_utc()));
            }
            crate::actions::ClockOperation::SetStopwatchOverlay { pinned } => {
                self.state.update(|s| s.stopwatch.pinned_overlay = pinned);
            }
        }
        Ok(true)
    }
}

impl ui::undo::CommittedActions for ClocksApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        // Hosted edits belong to the framework dispatcher. Standalone stays
        // on the local host, which has no document action yet.
        if self.actions.is_hosted() {
            return None;
        }
        self.actions.take_committed()
    }
}

impl Widget for ClocksApp {
    ui::provides!(
        dyn ui::undo::CommittedActions,
        dyn ui::resources::PublishesResources
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }
    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        let Some(request) = op.unscoped() else {
            return Ok(None);
        };
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }
    fn sync(&mut self, d: &mut AppModel, p: &ModelPath) {
        let tick_base = ui::time::now();
        self.next_clock_tick = tick_base
            .checked_add(Duration::from_secs(1))
            .unwrap_or(tick_base);
        let now = now_utc();
        if let Some((cmd, activity_id)) = self
            .persistent
            .then(crate::transport::take_command)
            .flatten()
        {
            self.state
                .update(|state| state.apply_shell_command(&cmd, activity_id, now));
        }
        self.apply_activations();
        let mut due = Vec::new();
        self.schedule_native_timers(now);
        self.state
            .update(|s| due = s.tick(now, ui::clock::local_now()));
        for event in due {
            self.remember_due(event);
        }
        self.deliver_native_notifications();
        self.generation = self.generation.saturating_add(1);
        self.state.with(|s| {
            let activity = s.shell_activity(now);
            let mut snapshot = activity.map_or_else(
                || ShellSnapshot {
                    version: 1,
                    active: false,
                    activity_id: None,
                    label: String::new(),
                    remaining_secs: 0,
                    stopwatch: false,
                    paused: false,
                    overlay: false,
                    generation: self.generation,
                    producer_pid: tilo_process::id().unwrap_or(0),
                    notification_id: None,
                    notification_title: String::new(),
                    notification_body: String::new(),
                },
                |a| ShellSnapshot {
                    version: 1,
                    active: true,
                    activity_id: a.activity_id,
                    label: a.label,
                    remaining_secs: a.remaining.as_secs(),
                    stopwatch: a.stopwatch,
                    paused: a.paused,
                    overlay: a.overlay,
                    generation: self.generation,
                    producer_pid: tilo_process::id().unwrap_or(0),
                    notification_id: None,
                    notification_title: String::new(),
                    notification_body: String::new(),
                },
            );
            if let Some((id, title, body)) = &self.last_notification {
                snapshot.notification_id = Some(*id);
                snapshot.notification_title.clone_from(title);
                snapshot.notification_body.clone_from(body);
            }
            if self.persistent {
                let _ = crate::transport::publish(&snapshot);
            }
        });
        let encoded = self
            .state
            .with(|s| serde_json::to_string(s).unwrap_or_default());
        if self.persistent
            && encoded != self.last_saved
            && self.state.with(ClockState::save).is_ok()
        {
            self.last_saved = encoded;
        }
        self.state.update(|_| {});
        let key = (self.selection.get(), self.notice.get());
        if self.view.get() != key {
            self.view.set(key);
        }
        let screen = self
            .selection
            .get()
            .as_deref()
            .map_or(Screen::Timers, Screen::from_id);
        if screen != self.shown {
            self.shown = screen;
            self.inner.set_content_title(screen.title());
        }
        self.inner.sync(d, p);
    }
    fn build_layout(
        &mut self,
        t: &mut TaffyTree<()>,
        th: &Theme,
        m: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner.build_layout(t, th, m)
    }
    fn apply_layout(&mut self, t: &TaffyTree<()>, n: NodeId, o: Point) {
        self.inner.apply_layout(t, n, o);
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        th: &Theme,
        p: &PresenceCtx,
    ) {
        self.inner.draw(s, tf, tr, th, p);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.inner.handle_event(event, ctx);
    }
    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.inner]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.inner]
    }
    fn name(&self) -> &'static str {
        "ClocksApp"
    }
    fn get_rect(&self) -> Rect {
        self.inner.get_rect()
    }
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        Some(
            self.inner
                .next_wake_deadline()
                .map_or(self.next_clock_tick, |inner| {
                    inner.min(self.next_clock_tick)
                }),
        )
    }
    fn take_structure_dirty(&mut self) -> bool {
        self.inner.take_structure_dirty()
    }
    fn take_focus_reset(&mut self) -> bool {
        self.inner.take_focus_reset()
    }
}

impl ClocksApp {
    /// Framework [`ResourceSurface::attach`] marks the surface hosted so
    /// `resources` does not republish `clocks.collection` beside `ResourceRoot`.
    pub(crate) fn mark_framework_hosted(&mut self) {
        self.hosted = true;
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.hosted
    }

    /// Test-only: mark hosted without constructing a framework dispatcher.
    #[cfg(test)]
    pub(crate) fn mark_hosted_for_test(&mut self) {
        self.hosted = true;
        self.actions.mark_hosted_for_test();
    }

    #[cfg(test)]
    pub(crate) fn sink_is_hosted(&self) -> bool {
        self.actions.is_hosted()
    }

    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn timer_count(&self) -> usize {
        self.state.with(|state| state.timers.len())
    }
}

impl ui::resources::PublishesResources for ClocksApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        if self.hosted {
            return self.state.with(crate::actions::live_clock_extras);
        }
        self.state.with(|state| {
            let mut root =
                ui::tilo_resource::publish::<crate::resource::Clocks>("clocks", "Clocks", state);
            root.children
                .extend(crate::actions::live_clock_extras(state));
            vec![root]
        })
    }
    fn resource(&self, doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        fn find(
            resources: Vec<tilo_actions::Resource>,
            id: &str,
        ) -> Option<tilo_actions::Resource> {
            for resource in resources {
                if resource.id.as_str() == id {
                    return Some(resource);
                }
                if let Some(found) = find(resource.children.clone(), id) {
                    return Some(found);
                }
            }
            None
        }
        find(self.resources(doc), id)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClocksApp, DueEvent, Timer, WEEKDAYS, alarm_label, city_time,
        custom_timer_duration_seconds, remaining_fraction, repeat_summary, timer_label,
        timezone_choices, timezone_label,
    };
    use crate::ShellSnapshot;
    use chrono::Utc;
    use std::collections::BTreeSet;
    use std::time::Duration;
    use ui::{State, headless::FrameHarness, theme::Theme};

    #[test]
    fn screens_are_sidebar_destinations_and_dialog_fields_keep_disjoint_touch_targets() {
        // Case: every screen is a destination of the sidebar list.
        let harness = FrameHarness::new(
            State::new(),
            Box::new(ClocksApp::scenario("timers")),
            (1280, 800),
        );
        for label in ["World clocks", "Alarms", "Timers", "Stopwatch"] {
            assert!(
                harness.a11y_node_named(label).is_some(),
                "{label} must be discoverable"
            );
        }

        // Case: the sidebar collapses instead of squeezing the screen in a
        // narrow window, and the screen keeps every control inside it.
        for width in [360_u32, 520_u32] {
            let harness = FrameHarness::new(
                State::new(),
                Box::new(ClocksApp::scenario("timers")),
                (width, 820),
            );
            let targets = harness.targets();
            for id in ["clocks.timer.new", "clocks.preset.300"] {
                let rect = targets
                    .get(id)
                    .copied()
                    .unwrap_or_else(|| panic!("width {width}: missing {id}"));
                assert!(
                    rect.x0 >= 0.0 && rect.x1 <= f64::from(width),
                    "width {width}: {id} fits the window: {rect:?}"
                );
            }
        }

        // Case: large-text duration fields have disjoint geometry.
        let harness = FrameHarness::new_with_theme(
            State::new(),
            Box::new(ClocksApp::scenario("new-timer")),
            (880, 800),
            Theme::dark().with_text_scale(2.0),
        );
        let targets = harness.targets();
        let fields = [
            "clocks.timer.custom.hours",
            "clocks.timer.custom.minutes",
            "clocks.timer.custom.seconds",
        ]
        .map(|id| {
            targets
                .get(id)
                .copied()
                .unwrap_or_else(|| panic!("missing {id}"))
        });

        for (index, earlier) in fields.iter().enumerate() {
            for later in &fields[index + 1..] {
                assert!(
                    !(earlier.x0 < later.x1
                        && later.x0 < earlier.x1
                        && earlier.y0 < later.y1
                        && later.y0 < earlier.y1),
                    "duration controls must remain disjoint touch targets: {earlier:?} and {later:?}"
                );
            }
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn due_notices_are_announced_once_and_survive_the_inactive_tombstone() {
        // Case: a completed timer's notice survives the inactive tombstone.
        {
            let now = Utc::now();
            let mut timer = Timer::new("Tea", Duration::from_secs(1));
            timer.start(now);
            let mut state = crate::ClockState::default();
            state.timers.push(timer);

            let due = state.tick(now + chrono::Duration::seconds(2), chrono::Local::now());
            assert!(
                state
                    .shell_activity(now + chrono::Duration::seconds(2))
                    .is_none(),
                "case completed-timer: no longer an active shell activity"
            );
            assert!(
                matches!(due.as_slice(), [DueEvent::Timer { label, .. }] if label == "Tea"),
                "case completed-timer: due event"
            );

            let mut app = ClocksApp::scenario("empty");
            for event in due {
                app.remember_due(event);
            }
            let notice = app.last_notification.clone().expect("due notice retained");
            assert_eq!(notice.1, "Timer finished", "case completed-timer: title");
            assert_eq!(notice.2, "Tea", "case completed-timer: body");

            // Later inactive frames reuse this identity, allowing a polling shell
            // to observe it without announcing the same completion twice.
            assert_eq!(
                app.last_notification.as_ref(),
                Some(&notice),
                "case completed-timer: identity reused"
            );
        }

        // Case: the due notice survives an inactive shell snapshot on the wire.
        {
            let notification_id = uuid::Uuid::new_v4();
            let snapshot = ShellSnapshot {
                version: 1,
                active: false,
                activity_id: None,
                label: String::new(),
                remaining_secs: 0,
                stopwatch: false,
                paused: false,
                overlay: false,
                generation: 12,
                producer_pid: 7,
                notification_id: Some(notification_id),
                notification_title: "Timer finished".into(),
                notification_body: "Tea".into(),
            };

            let encoded = serde_json::to_string(&snapshot).unwrap();
            let decoded: ShellSnapshot = serde_json::from_str(&encoded).unwrap();
            assert!(!decoded.active, "case wire-notice: inactive");
            assert_eq!(
                decoded.notification_id,
                Some(notification_id),
                "case wire-notice: id"
            );
            assert_eq!(
                decoded.notification_title, "Timer finished",
                "case wire-notice: title"
            );
            assert_eq!(decoded.notification_body, "Tea", "case wire-notice: body");
        }

        // Case: the shell snapshot tombstone is backward compatible.
        {
            let legacy = r#"{"version":1,"label":"Tea","remaining_secs":30,"stopwatch":false,"overlay":true,"generation":4,"producer_pid":7}"#;
            let parsed: ShellSnapshot = serde_json::from_str(legacy).unwrap();
            assert!(parsed.active, "older live snapshots remain active");
            assert!(
                parsed.notification_id.is_none(),
                "case legacy-snapshot: no notification"
            );

            let inactive = ShellSnapshot {
                version: 1,
                active: false,
                activity_id: None,
                label: String::new(),
                remaining_secs: 0,
                stopwatch: false,
                paused: false,
                overlay: false,
                generation: 5,
                producer_pid: 7,
                notification_id: None,
                notification_title: String::new(),
                notification_body: String::new(),
            };
            let encoded = serde_json::to_string(&inactive).unwrap();
            assert!(
                encoded.contains("\"active\":false"),
                "case legacy-snapshot: tombstone encodes active=false"
            );
        }

        // Case (Windows only): a native handoff does not queue a second live alert.
        #[cfg(windows)]
        {
            let mut app = ClocksApp::scenario("empty");
            app.persistent = true;
            let id = uuid::Uuid::new_v4();
            app.remember_due(DueEvent::Timer {
                id,
                label: "Tea".into(),
                native_scheduled: true,
            });
            assert!(
                app.pending_notifications.is_empty(),
                "case native-handoff: scheduled natively, nothing queued"
            );
            app.remember_due(DueEvent::Timer {
                id,
                label: "Tea".into(),
                native_scheduled: false,
            });
            assert_eq!(
                app.pending_notifications.len(),
                1,
                "case native-handoff: unscheduled queues one"
            );
            assert_eq!(
                app.pending_notifications[0].0,
                super::native_timer_id(id),
                "case native-handoff: tag"
            );
        }
    }

    #[test]
    fn timer_changes_request_layout_rebuild_and_wake_without_collaboration() {
        use ui::widget::Widget;
        let mut app = ClocksApp::scenario("empty");
        let mut doc = automerge::AutoCommit::new();
        app.sync(&mut doc, &automerge::ROOT);
        let mut taffy = taffy::TaffyTree::new();
        app.build_layout(
            &mut taffy,
            &ui::Theme::default(),
            &mut std::collections::HashMap::new(),
        );
        let _ = app.take_structure_dirty();
        app.state.update(|state| {
            state
                .timers
                .push(Timer::new("Tea", Duration::from_secs(60)));
        });
        app.sync(&mut doc, &automerge::ROOT);
        assert!(
            app.take_structure_dirty(),
            "replaced timer controls must invalidate host pointers"
        );
        let deadline = app
            .next_wake_deadline()
            .expect("clock must wake with no peer or input");
        assert!(deadline > ui::time::now());
        assert!(deadline <= ui::time::now() + Duration::from_secs(1));
    }

    #[test]
    fn live_start_restores_saved_timers_and_shots_leave_them_alone() {
        const CHILD: &str = "TILO_CLOCKS_RESTORE_TEST_CHILD";
        if std::env::var_os(CHILD).is_some() {
            use ui::widget::Widget;
            let app = ClocksApp::new();
            assert!(app.persistent);
            app.state.with(|state| {
                assert_eq!(state.timers.len(), 1);
                assert_eq!(state.timers[0].label, "Saved timer");
            });
            let mut shot = ClocksApp::scenario("running");
            shot.sync(&mut automerge::AutoCommit::new(), &automerge::ROOT);
            let mut unknown_shot = ClocksApp::scenario("live");
            assert!(!unknown_shot.persistent);
            unknown_shot.sync(&mut automerge::AutoCommit::new(), &automerge::ROOT);
            assert_eq!(crate::ClockState::load().timers[0].label, "Saved timer");
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let mut saved = crate::ClockState::default();
        saved
            .timers
            .push(Timer::new("Saved timer", Duration::from_secs(60)));
        saved
            .save_at(&directory.path().join("tilo/clocks.json"))
            .unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "app::tests::live_start_restores_saved_timers_and_shots_leave_them_alone",
                "--test-threads=1",
            ])
            .env(CHILD, "1")
            .env("XDG_STATE_HOME", directory.path())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        // libtest exits 0 when `--exact` selects nothing; require the child ran.
        assert!(
            stdout.contains("test result: ok. 1 passed"),
            "the child ran no test: {stdout}"
        );
    }

    #[test]
    fn semantic_alarm_toggle_clears_the_previous_occurrence() {
        use ui::widget::Widget;

        let mut app = ClocksApp::scenario("empty");
        let mut doc = automerge::AutoCommit::new();
        let create = crate::actions::ClockOperation::NewAlarm {
            hours: 12,
            minutes: 0,
            label: "Lunch".into(),
        };
        assert!(
            app.start_operation(
                &ui::widget::ResourceOperation::local(
                    ui::tilo_actions::InvocationId::new(),
                    tilo_actions::OperationKind::Effect,
                    crate::actions::OPERATION_TYPE,
                    &serde_json::to_vec(&create).unwrap()
                ),
                &mut doc,
                &automerge::ROOT
            )
            .map(|started| started.is_some())
            .unwrap()
        );
        let id = app.state.with(|state| state.alarms[0].id);
        let toggle =
            serde_json::to_vec(&crate::actions::ClockOperation::ToggleAlarm { id }).unwrap();
        for enabled in [false, true] {
            app.state
                .update(|state| state.alarms[0].next_due_utc = Some(Utc::now()));
            assert!(
                app.start_operation(
                    &ui::widget::ResourceOperation::local(
                        ui::tilo_actions::InvocationId::new(),
                        tilo_actions::OperationKind::Effect,
                        crate::actions::OPERATION_TYPE,
                        &toggle
                    ),
                    &mut doc,
                    &automerge::ROOT
                )
                .map(|started| started.is_some())
                .unwrap()
            );
            app.state.with(|state| {
                assert_eq!(state.alarms[0].enabled, enabled);
                assert_eq!(state.alarms[0].next_due_utc, None);
            });
        }
    }

    #[test]
    fn timer_alarm_and_world_clock_inputs_are_bounded_and_readable() {
        // Case: countdown progress visualizes time remaining.
        assert!(
            (remaining_fraction(300_000, 300_000) - 1.0).abs() < f32::EPSILON,
            "case progress-full"
        );
        assert!(
            (remaining_fraction(150_000, 300_000) - 0.5).abs() < f32::EPSILON,
            "case progress-half"
        );
        assert!(
            remaining_fraction(0, 300_000).abs() < f32::EPSILON,
            "case progress-empty"
        );
        assert!(
            remaining_fraction(20, 0).abs() < f32::EPSILON,
            "case progress-zero-total"
        );
        assert!(
            (remaining_fraction(400_000, 300_000) - 1.0).abs() < f32::EPSILON,
            "case progress-overflow-clamped"
        );

        // Case: custom timer labels are useful, bounded and have a calm default.
        assert_eq!(timer_label("  Pasta  "), "Pasta", "case timer-label-trim");
        assert_eq!(timer_label("   "), "Timer", "case timer-label-default");
        assert_eq!(
            timer_label(&"x".repeat(120)).chars().count(),
            80,
            "case timer-label-bounded"
        );

        // Case: custom timer durations require bounded whole-number fields, incl. edges.
        for (hours, minutes, seconds, want) in [
            ("01", "02", "03", Some(3_723)),
            ("", "10", "00", None),
            ("100", "10", "00", None),
            ("00", "60", "00", None),
            ("00", "10", "-1", None),
            ("0", "0", "0", Some(0)),
            ("99", "59", "59", Some(99 * 3_600 + 59 * 60 + 59)),
            ("18446744073709551616", "0", "0", None),
        ] {
            assert_eq!(
                custom_timer_duration_seconds(hours, minutes, seconds),
                want,
                "case duration {hours:?}:{minutes:?}:{seconds:?}"
            );
        }

        // Case: alarm schedule summaries are specific and labels are bounded.
        assert_eq!(
            alarm_label("  School run "),
            "School run",
            "case alarm-label-trim"
        );
        assert_eq!(alarm_label(" "), "Alarm", "case alarm-label-default");
        let selected = [WEEKDAYS[0].0, WEEKDAYS[2].0, WEEKDAYS[4].0]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            repeat_summary(&selected),
            "Mon, Wed, Fri",
            "case repeat-selected"
        );
        assert_eq!(repeat_summary(&BTreeSet::new()), "Once", "case repeat-none");

        let weekdays = WEEKDAYS[..5]
            .iter()
            .map(|(day, _)| *day)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            repeat_summary(&weekdays),
            "Weekdays",
            "case repeat-weekdays"
        );
        let weekend = WEEKDAYS[5..]
            .iter()
            .map(|(day, _)| *day)
            .collect::<BTreeSet<_>>();
        assert_eq!(repeat_summary(&weekend), "Weekends", "case repeat-weekend");
        let every_day = WEEKDAYS
            .iter()
            .map(|(day, _)| *day)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            repeat_summary(&every_day),
            "Every day",
            "case repeat-every-day"
        );
    }

    #[test]
    fn city_picker_and_city_times_are_readable() {
        // Case: the city picker covers the timezone catalogue without jargon.
        assert_eq!(
            timezone_label("America/Argentina/Buenos_Aires"),
            "Buenos Aires (America)",
            "case timezone-label"
        );
        let choices = timezone_choices();
        for zone in ["Atlantic/Reykjavik", "America/New_York", "Asia/Kolkata"] {
            assert!(
                choices.iter().any(|(id, _)| id == zone),
                "case picker-has-{zone}"
            );
        }
        assert!(
            choices.iter().all(|(id, _)| !id.starts_with("Etc/")),
            "case picker-no-etc"
        );
        assert!(
            choices.windows(2).all(|pair| pair[0].1 <= pair[1].1),
            "case picker-sorted"
        );

        // Case: a city's time says how far it is from here.
        let (time, relation) = city_time("Pacific/Kiritimati").expect("a known zone");
        assert_eq!(time.len(), 5, "case city-time-clock");
        assert!(
            ["Today", "Tomorrow", "Yesterday"]
                .iter()
                .any(|day| relation.starts_with(day)),
            "case city-time-day: {relation}"
        );
        assert!(
            city_time("Nowhere/Land").is_none(),
            "case city-time-unknown"
        );
    }
}
