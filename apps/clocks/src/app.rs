//! The Clocks window: a surface over the person's [`Clocks`]. It shows the
//! state it is presented and changes it only through the resource's
//! actions; what stays on this device (floating clocks, the delivery
//! ledger) goes through the app's device-local settings.
//!
//! A live window also rings what is due, hands running timers to the
//! platform's notification scheduler, and keeps the shell's floating clock
//! current. A capture or a past version does none of that.

use crate::model::{Clocks, WeekdayKey, bounded_label};
use crate::native_timers::{
    ALARMS_URI, TIMERS_URI, alarm_id as native_alarm_id, timer_id as native_timer_id,
};
use crate::resource::ClocksEffect;
use crate::schedule::{DueEvent, OnThisDevice, shell_activity};
use crate::transport::{ShellCommand, ShellSnapshot};
use chrono::{Offset, Utc};
use std::{collections::BTreeSet, time::Duration};
use ui::framework::{Actions, Dispatcher, Setting};
use ui::icons::Icon;
use ui::prelude::Id;
use ui::widget::primitives::{Fact, Facts};
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Card, CardGrid, Column, Container, Element, ModelView, ProgressBar, Row, Scrollable,
    Spacer, Text, TextInput, TextInputKind, TimePicker, TimeValue, Widget,
};

/// What the window shows: the person's clocks and this device's choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shown {
    pub(crate) clocks: Clocks,
    pub(crate) device: OnThisDevice,
    /// The moment wall clocks show, when a capture pins it; else the
    /// device's clock.
    pub(crate) pinned_now: Option<chrono::DateTime<Utc>>,
    pub(crate) selected_alarm: Option<Id>,
}

impl Shown {
    /// The present as wall clocks show it.
    fn now(&self) -> chrono::DateTime<Utc> {
        self.pinned_now.unwrap_or_else(Utc::now)
    }

    /// The local time of day the "Here" clock shows: a capture's pinned
    /// moment reads the same on every machine.
    fn here(&self) -> chrono::NaiveDateTime {
        self.pinned_now.map_or_else(
            || chrono::Local::now().naive_local(),
            |pinned| pinned.naive_utc(),
        )
    }
}

/// How the window's controls reach the resource and the device.
#[derive(Clone)]
pub(crate) struct Controls {
    pub(crate) actions: Actions<Clocks>,
    pub(crate) dispatcher: Dispatcher<Clocks>,
}

impl Controls {
    /// A button invoking `invoke` through the door, or disabled when the
    /// participant may not.
    fn button(&self, button: Button, invoke: ui::prelude::Invoke<Clocks>) -> Button {
        self.actions.bind_invocation(button, invoke)
    }

    /// A button invoking `effect` through the door.
    fn effect(&self, button: Button, effect: &ClocksEffect) -> Button {
        self.actions.bind_effect_invocation(button, effect)
    }
}

fn button(id: &str, label: &str, f: impl FnMut() + 'static) -> Element {
    Box::new(
        Button::new(id, label)
            .control_size(ui::widget::ControlSize::Large)
            .on_effect(f),
    )
}

fn primary_button(id: &str, label: &str, f: impl FnMut() + 'static) -> Element {
    Box::new(
        Button::new(id, label)
            .role(ui::widget::ButtonRole::Primary)
            .control_size(ui::widget::ControlSize::Large)
            .on_effect(f),
    )
}

pub(crate) fn duration(ms: u64) -> String {
    ui::format::timecode(ms / 1_000)
}

fn millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
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
    if days.is_empty() {
        return "Once".into();
    }
    WEEKDAYS
        .iter()
        .filter_map(|(day, label)| days.contains(day).then_some(*label))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A city's time of day, and beside it the facts that place it: its day
/// when that is not today here, and how far ahead or behind here it is.
pub(crate) fn city_time(zone: &str, now: chrono::DateTime<Utc>) -> (String, Vec<Fact>) {
    let Ok(zone) = zone.parse::<chrono_tz::Tz>() else {
        return ("Time unavailable".into(), Vec::new());
    };
    let there = now.with_timezone(&zone);
    let here = now.with_timezone(&chrono::Local);
    let mut facts = Vec::new();
    if there.date_naive() != here.date_naive() {
        facts.push(Fact::text(ui::format::day_heading_short(
            there.date_naive(),
        )));
    }
    let difference = there
        .offset()
        .fix()
        .local_minus_utc()
        .saturating_sub(here.offset().fix().local_minus_utc());
    facts.push(Fact::text(offset_from_here(difference)));
    (ui::clock::time_of_day(&there), facts)
}

/// How far a place is ahead of or behind here, in words.
fn offset_from_here(seconds: i32) -> String {
    if seconds == 0 {
        return "Same time as here".into();
    }
    let minutes = seconds.unsigned_abs() / 60;
    let (hours, minutes) = (minutes / 60, minutes % 60);
    let amount = match (hours, minutes) {
        (0, minutes) => format!("{minutes} min"),
        (1, 0) => "1 hour".into(),
        (hours, 0) => format!("{hours} hours"),
        (hours, minutes) => format!("{hours} h {minutes} min"),
    };
    if seconds > 0 {
        format!("{amount} ahead")
    } else {
        format!("{amount} behind")
    }
}

fn timezone_label(zone: &str) -> String {
    let city = zone.rsplit('/').next().unwrap_or(zone).replace('_', " ");
    let region = zone.split('/').next().unwrap_or_default();
    if region.is_empty() || region == city {
        city
    } else {
        format!("{city}, in {region}")
    }
}

fn timezone_results(query: &str) -> Vec<String> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return [
            "America/New_York",
            "Europe/London",
            "Europe/Paris",
            "Asia/Tokyo",
            "Asia/Kolkata",
            "Australia/Sydney",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
    }
    chrono_tz::TZ_VARIANTS
        .iter()
        .map(ToString::to_string)
        .filter(|zone| {
            !zone.starts_with("Etc/")
                && (zone.to_lowercase().contains(&query)
                    || timezone_label(zone).to_lowercase().contains(&query))
        })
        .take(8)
        .collect()
}

fn world_clock_list(state: &ui::LocalState<Shown>, controls: &Controls) -> ModelView<Shown> {
    let list_controls = controls.clone();
    ModelView::from_state(state, move |s| {
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        if s.clocks.world_clocks.is_empty() {
            col = col.push(Box::new(
                Text::new("No cities yet. Add one to compare times at a glance.").dimmed(),
            ));
        }
        for (id, clock) in s.clocks.world_clocks.iter() {
            let (time, facts) = city_time(&clock.timezone.0, s.now());
            let row = Row::new()
                .push(Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_rhythm(ui::tokens::Rhythm::Tight)
                        .push(Box::new(Text::new(&time).with_role(ui::TextRole::Title)))
                        .push(Box::new(Facts::new(facts))),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    list_controls.button(
                        Button::new(&format!("clocks.world.{id}.remove"), "Remove")
                            .control_size(ui::widget::ControlSize::Large),
                        Clocks::REMOVE_WORLD_CLOCK.with(id.clone()),
                    ),
                ));
            col = col.push(Box::new(clock_card(&clock.city, Box::new(row))));
        }
        Box::new(col)
    })
}

fn world_screen(state: &ui::LocalState<Shown>, controls: &Controls) -> Element {
    let list = world_clock_list(state, controls);
    let city_query = ui::LocalState::new(String::new());
    let result_query = city_query.clone();
    let add = controls.dispatcher.clone();
    let choices = ModelView::from_state(&city_query, move |query| {
        let mut choices = Row::new().with_padding_rhythm(None, None);
        for zone in timezone_results(query) {
            let add = add.clone();
            let clear = result_query.clone();
            let city = zone.rsplit('/').next().unwrap_or(&zone).replace('_', " ");
            let label = timezone_label(&zone);
            let id = zone.to_lowercase().replace(['/', '_'], "-");
            choices = choices.push(button(
                &format!("clocks.world.add.{id}"),
                &label,
                move || {
                    add.submit(Clocks::ADD_WORLD_CLOCK.with((city.clone(), zone.clone())));
                    clear.set(String::new());
                },
            ));
        }
        Box::new(choices)
    });
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .push(Box::new(
                Text::new("World clocks")
                    .with_role(ui::TextRole::LargeTitle)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Keep the people and places that matter in view.").dimmed(),
            ))
            .push(Box::new(
                CardGrid::new(vec![
                    Box::new(clock_card(
                        "Here",
                        Box::new(ModelView::from_state(state, |s| {
                            let here = s.here();
                            Box::new(
                                Column::new()
                                    .with_no_padding()
                                    .with_intrinsic_height()
                                    .with_rhythm(ui::tokens::Rhythm::Tight)
                                    .push(Box::new(
                                        Text::new(&ui::clock::time_of_day(&here))
                                            .with_role(ui::TextRole::Display),
                                    ))
                                    .push(Box::new(
                                        Text::new(&ui::format::day_heading(here.date(), false))
                                            .dimmed(),
                                    )),
                            ) as Element
                        })),
                    )),
                    Box::new(clock_card(
                        "Add a city",
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_rhythm(ui::tokens::Rhythm::Related)
                                .push(Box::new(
                                    TextInput::from_state(&city_query)
                                        .with_id("clocks.world.search")
                                        .with_kind(TextInputKind::Search)
                                        .with_placeholder("Search cities or time zones"),
                                ))
                                .push(Box::new(choices)),
                        ),
                    )),
                ])
                .with_card_width(ui::widget::CardWidth::Wide),
            ))
            .push(Box::new(list)),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "declarative UI builder that reads top-to-bottom; splitting it would scatter one screen across helpers"
)]
fn alarms_screen(state: &ui::LocalState<Shown>, controls: &Controls) -> Element {
    let draft = ui::LocalState::new(TimeValue::new(7, 30));
    let draft_label = ui::LocalState::new(String::new());
    let draft_repeat = ui::LocalState::new(BTreeSet::<WeekdayKey>::new());
    let add = controls.dispatcher.clone();
    let add_draft = draft.clone();
    let add_label = draft_label.clone();
    let add_repeat = draft_repeat.clone();
    let repeat_controls = draft_repeat.clone();
    let list_controls = controls.clone();
    let list = ModelView::from_state(state, move |s| {
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        if s.clocks.alarms.is_empty() {
            col = col.push(Box::new(
                Text::new("No alarms yet. Set one for a future time.").dimmed(),
            ));
        }
        let mut alarms: Vec<_> = s.clocks.alarms.iter().collect();
        alarms.sort_by_key(|(id, _)| s.selected_alarm.as_ref() != Some(id));
        for (id, alarm) in alarms {
            let repeat = repeat_summary(&alarm.weekdays());
            let row = Row::new()
                .push(Box::new(
                    Text::new(&ui::clock::time_of_day(&alarm.local_time()))
                        .with_role(ui::TextRole::LargeTitle),
                ))
                .push(Box::new(
                    Text::new(&format!(
                        "{repeat}, currently {}",
                        if alarm.enabled { "On" } else { "Off" }
                    ))
                    .dimmed(),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    list_controls.button(
                        Button::new(
                            &format!("clocks.alarm.{id}.toggle"),
                            if alarm.enabled { "Turn off" } else { "Turn on" },
                        )
                        .control_size(ui::widget::ControlSize::Large),
                        Clocks::SET_ALARM_ENABLED.with((id.clone(), !alarm.enabled)),
                    ),
                ))
                .push(Box::new(
                    list_controls.button(
                        Button::new(&format!("clocks.alarm.{id}.remove"), "Remove")
                            .control_size(ui::widget::ControlSize::Large),
                        Clocks::REMOVE_ALARM.with(id.clone()),
                    ),
                ));
            col = col.push(Box::new(
                clock_card(&alarm.label, Box::new(row))
                    .selected(s.selected_alarm.as_ref() == Some(id)),
            ));
        }
        Box::new(col)
    });
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .push(Box::new(
                Text::new("Alarms")
                    .with_role(ui::TextRole::LargeTitle)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("A reliable nudge, exactly when you need it.").dimmed(),
            ))
            .push(Box::new(list))
            .push(Box::new(clock_card(
                "New alarm",
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .push(Box::new(
                            TextInput::from_state(&draft_label)
                                .with_id("clocks.alarm.label")
                                .with_kind(TextInputKind::Name)
                                .with_placeholder("What is this alarm for?"),
                        ))
                        .push(Box::new(Text::new("Wake-up time").with_heading_level(3)))
                        .push(Box::new(
                            TimePicker::from_state(&draft)
                                .with_id("clocks.alarm.time")
                                .with_accessible_name("Alarm time"),
                        ))
                        .push(Box::new(
                            Text::new("Use the arrows, scroll, or type two digits.").dimmed(),
                        ))
                        .push(Box::new(Text::new("Repeat days").with_heading_level(3)))
                        .push(Box::new(ModelView::from_state(
                            &draft_repeat,
                            move |selected| {
                                let mut first_row = Row::new();
                                let mut second_row = Row::new();
                                for (index, (day, label)) in WEEKDAYS.into_iter().enumerate() {
                                    let selection = repeat_controls.clone();
                                    let mut day_button = Button::new(
                                        &format!("clocks.alarm.repeat.{}", label.to_lowercase()),
                                        label,
                                    )
                                    .control_size(ui::widget::ControlSize::Large)
                                    .with_accessible_name(&format!("Repeat on {label}"))
                                    .on_effect(move || {
                                        selection.update(|days| {
                                            if !days.remove(&day) {
                                                days.insert(day);
                                            }
                                        });
                                    });
                                    if selected.contains(&day) {
                                        day_button =
                                            day_button.role(ui::widget::ButtonRole::Primary);
                                    }
                                    if index < 4 {
                                        first_row = first_row.push(Box::new(day_button));
                                    } else {
                                        second_row = second_row.push(Box::new(day_button));
                                    }
                                }
                                Box::new(
                                    Column::new()
                                        .with_no_padding()
                                        .with_intrinsic_height()
                                        .with_rhythm(ui::tokens::Rhythm::Related)
                                        .push(Box::new(first_row))
                                        .push(Box::new(second_row)),
                                )
                            },
                        )))
                        .push(primary_button("clocks.alarm.add", "Add alarm", move || {
                            let time = add_draft.get();
                            add.submit(Clocks::ADD_ALARM.with((
                                bounded_label(&add_label.get(), "Alarm"),
                                time.hours,
                                time.minutes,
                                add_repeat.get().into_iter().collect(),
                            )));
                            add_label.set(String::new());
                            add_repeat.set(BTreeSet::new());
                        })),
                ),
            ))),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "declarative UI builder that reads top-to-bottom; splitting it would scatter one card across helpers"
)]
fn timer_card(
    id: &ui::prelude::Id,
    timer: &crate::model::Timer,
    floating: bool,
    controls: &Controls,
) -> Element {
    let now = Utc::now();
    let running = timer.is_running(now);
    let finished = timer.is_finished(now);
    let remaining_ms = millis(timer.remaining(now));
    let progress = remaining_fraction(remaining_ms, timer.duration_ms);
    let toggle = if running {
        Clocks::PAUSE_TIMER.with(id.clone())
    } else {
        Clocks::RESUME_TIMER.with(id.clone())
    };
    let float_label = if floating {
        "Hide floating timer"
    } else {
        "Float timer above other windows"
    };
    Box::new(clock_card(
        &timer.label,
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Text::new(&duration(remaining_ms)).with_role(ui::TextRole::Display),
                        ))
                        .push(Box::new(Spacer::new()))
                        .push(Box::new(
                            Text::new(if running {
                                "Running"
                            } else if finished {
                                "Done"
                            } else {
                                "Paused"
                            })
                            .dimmed(),
                        )),
                ))
                .push(Box::new(Container::new().with_no_padding().with_child(
                    Box::new(
                        ProgressBar::new(progress).with_label(format!("{} remaining", timer.label)),
                    ),
                )))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            controls.button(
                                Button::new(&format!("clocks.timer.{id}.add-minute"), "+1 min")
                                    .with_accessible_name(&format!(
                                        "Add one minute to {}",
                                        timer.label
                                    ))
                                    .control_size(ui::widget::ControlSize::Large),
                                Clocks::ADD_MINUTE.with(id.clone()),
                            ),
                        ))
                        .push(Box::new(
                            controls.button(
                                Button::new(
                                    &format!("clocks.timer.{id}.toggle"),
                                    if running {
                                        "Pause"
                                    } else if finished {
                                        "Restart"
                                    } else {
                                        "Resume"
                                    },
                                )
                                .with_icon(if running { Icon::Pause } else { Icon::Play })
                                .role(ui::widget::ButtonRole::Primary)
                                .control_size(ui::widget::ControlSize::Large),
                                toggle,
                            ),
                        ))
                        .push(Box::new(
                            controls.effect(
                                Button::new(
                                    &format!("clocks.timer.{id}.pin"),
                                    if floating { "Hide" } else { "Float" },
                                )
                                .with_icon(Icon::PictureInPicture2)
                                .with_accessible_name(float_label)
                                .tooltip(float_label)
                                .control_size(ui::widget::ControlSize::Large),
                                &ClocksEffect::FloatTimer {
                                    timer: id.as_str().to_owned(),
                                    floating: !floating,
                                },
                            ),
                        ))
                        .push(Box::new(
                            controls.button(
                                Button::new(&format!("clocks.timer.{id}.remove"), "")
                                    .with_icon(Icon::Trash2)
                                    .with_accessible_name("Remove timer")
                                    .tooltip("Remove timer")
                                    .role(ui::widget::ButtonRole::Destructive)
                                    .control_size(ui::widget::ControlSize::Large),
                                Clocks::REMOVE_TIMER.with(id.clone()),
                            ),
                        )),
                )),
        ),
    ))
}

#[expect(
    clippy::too_many_lines,
    reason = "declarative UI builder that reads top-to-bottom; splitting it would scatter one screen across helpers"
)]
fn timers_screen(
    state: &ui::LocalState<Shown>,
    controls: &Controls,
    validation_error: &ui::LocalState<Option<String>>,
) -> Element {
    // Keep the components as distinct fields. A spinner row looked compact at
    // default size but allowed its values and arrow targets to collide at 200%
    // text, which made setting a duration unreliable with touch or a screen
    // magnifier.
    let custom_hours = ui::LocalState::new(0.0_f64);
    let custom_minutes = ui::LocalState::new(10.0_f64);
    let custom_seconds = ui::LocalState::new(0.0_f64);
    let custom_label = ui::LocalState::new(String::new());
    let preset_specs = [
        ("Tea", "Tea for 5 minutes", 300),
        ("Break", "Break for 10 minutes", 600),
        ("Focus", "Focus for 25 minutes", 1500),
        ("Laundry", "Laundry for 45 minutes", 2700),
    ];
    let mut presets = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Related);
    for preset_row in preset_specs.chunks(2) {
        let mut row = Row::new().with_padding_rhythm(None, None);
        for &(label, button_label, secs) in preset_row {
            // A common, generous width keeps the two-column grid legible at
            // 200% text while still fitting inside the phone content rail.
            row = row.push(Box::new(
                controls.button(
                    Button::new(
                        &format!("clocks.preset.{secs}"),
                        &format!("{label} ({} min)", secs / 60),
                    )
                    .with_accessible_name(button_label)
                    .control_size(ui::widget::ControlSize::Large),
                    Clocks::ADD_TIMER.with((label.to_owned(), secs)),
                ),
            ));
        }
        presets = presets.push(Box::new(row));
    }
    let list_controls = controls.clone();
    let list = ModelView::from_state(state, move |s| {
        if s.clocks.timers.is_empty() {
            return Box::new(Column::new().with_no_padding().with_intrinsic_height());
        }
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Related);
        for (id, timer) in s.clocks.timers.iter() {
            let floating = s.device.floating_timers.contains(id.as_str());
            col = col.push(timer_card(id, timer, floating, &list_controls));
        }
        Box::new(clock_card("Active timers", Box::new(col)))
    });
    let add = controls.dispatcher.clone();
    let add_hours = custom_hours.clone();
    let add_minutes = custom_minutes.clone();
    let add_seconds = custom_seconds.clone();
    let add_label = custom_label.clone();
    let add_error = validation_error.clone();
    let error = ModelView::from_state(validation_error, |message| {
        message.as_ref().map_or_else(
            || Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element,
            |message| {
                Box::new(clock_card(
                    "Timer needs a duration",
                    Box::new(Text::new(message)),
                ))
            },
        )
    });
    // Duration is a related cluster, not three content cards. Shared
    // steppers measure their digits and touch targets; the row wraps whole
    // fields at large text rather than squeezing their editable values.
    let mut duration_fields = Row::new()
        .with_padding_rhythm(None, None)
        .with_rhythm(ui::tokens::Rhythm::Related)
        .with_align_items(taffy::AlignItems::Start);
    for (label, id, state, maximum) in [
        ("Hours", "hours", &custom_hours, 99.0),
        ("Minutes", "minutes", &custom_minutes, 59.0),
        ("Seconds", "seconds", &custom_seconds, 59.0),
    ] {
        duration_fields = duration_fields.push(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_intrinsic_width()
                .with_rhythm(ui::tokens::Rhythm::Tight)
                .push(Box::new(Text::new(label).with_role(ui::TextRole::Caption)))
                .push(Box::new(
                    ui::widget::NumberInput::from_state(state)
                        .with_id(&format!("clocks.timer.custom.{id}"))
                        .with_range(0.0, maximum)
                        .with_step(1.0)
                        .with_precision(0)
                        .control_size(ui::widget::ControlSize::Large)
                        .with_accessible_name(format!("Timer duration {id}")),
                )),
        ));
    }
    let setup = CardGrid::new(vec![
        Box::new(clock_card(
            "Quick starts",
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_rhythm(ui::tokens::Rhythm::Related)
                    .push(Box::new(
                        Text::new("A focused timer is one tap away.").dimmed(),
                    ))
                    .push(Box::new(presets)),
            ),
        )),
        Box::new(clock_card(
            "Custom timer",
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .push(Box::new(
                        TextInput::from_state(&custom_label)
                            .with_id("clocks.timer.custom.label")
                            .with_kind(TextInputKind::Name)
                            .with_accessible_name("Timer label")
                            .with_placeholder("What is this timer for?"),
                    ))
                    .push(Box::new(duration_fields))
                    .push(primary_button(
                        "clocks.timer.custom.add",
                        "Start timer",
                        move || {
                            let Some(secs) = custom_timer_duration_seconds(
                                &add_hours.get().to_string(),
                                &add_minutes.get().to_string(),
                                &add_seconds.get().to_string(),
                            ) else {
                                add_error.set(Some(
                                    "Use whole numbers: hours 0–99 and minutes and seconds 0–59."
                                        .into(),
                                ));
                                return;
                            };
                            if secs > 0 {
                                add.submit(
                                    Clocks::ADD_TIMER
                                        .with((bounded_label(&add_label.get(), "Timer"), secs)),
                                );
                                add_label.set(String::new());
                                add_error.set(None);
                            } else {
                                add_error.set(Some(
                                    "Choose a duration greater than zero before starting a timer."
                                        .into(),
                                ));
                            }
                        },
                    ))
                    .push(Box::new(error)),
            ),
        )),
    ])
    .with_card_width(ui::widget::CardWidth::Wide);
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Section)
            .push(Box::new(
                Text::new("Timers")
                    .with_role(ui::TextRole::LargeTitle)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Keep pace with your day, without losing your place.").dimmed(),
            ))
            .push(Box::new(list))
            .push(Box::new(setup)),
    )
}

fn stopwatch_screen(state: &ui::LocalState<Shown>, controls: &Controls) -> Element {
    let controls = controls.clone();
    let view = ModelView::from_state(state, move |s| {
        let now = Utc::now();
        let stopwatch = &s.clocks.stopwatch;
        let running = stopwatch.is_running();
        let floating = s.device.floating_stopwatch;
        let mut laps = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_rhythm(ui::tokens::Rhythm::Tight);
        for (i, ms) in stopwatch.laps_ms.iter().enumerate().rev() {
            laps = laps.push(Box::new(Text::new(&format!(
                "Lap {}   {}",
                i.saturating_add(1),
                duration(*ms)
            ))));
        }
        let elapsed_ms = millis(stopwatch.elapsed(now));
        Box::new(Card::new(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .push(Box::new(
                    Text::new(&duration(elapsed_ms)).with_role(ui::TextRole::Display),
                ))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            controls.button(
                                Button::new(
                                    "clocks.stopwatch.toggle",
                                    if running { "Pause" } else { "Start" },
                                )
                                .role(ui::widget::ButtonRole::Primary)
                                .with_icon(if running { Icon::Pause } else { Icon::Play })
                                .control_size(ui::widget::ControlSize::Large),
                                if running {
                                    Clocks::PAUSE_STOPWATCH.invoke()
                                } else {
                                    Clocks::START_STOPWATCH.invoke()
                                },
                            ),
                        ))
                        .push(Box::new(
                            controls
                                .button(
                                    Button::new("clocks.stopwatch.lap", "Lap")
                                        .control_size(ui::widget::ControlSize::Large),
                                    Clocks::LAP.invoke(),
                                )
                                .with_enabled(running),
                        ))
                        .push(Box::new(
                            controls
                                .button(
                                    Button::new("clocks.stopwatch.reset", "Reset")
                                        .control_size(ui::widget::ControlSize::Large),
                                    Clocks::RESET_STOPWATCH.invoke(),
                                )
                                .with_enabled(elapsed_ms != 0 || !stopwatch.laps_ms.is_empty()),
                        )),
                ))
                .push(Box::new(
                    controls.effect(
                        Button::new(
                            "clocks.stopwatch.pin",
                            if floating {
                                "Stop floating"
                            } else {
                                "Keep stopwatch visible"
                            },
                        )
                        .with_icon(Icon::PictureInPicture2)
                        .control_size(ui::widget::ControlSize::Large),
                        &ClocksEffect::FloatStopwatch {
                            floating: !floating,
                        },
                    ),
                ))
                .push(Box::new(laps)),
        )))
    });
    Box::new(
        Column::new()
            .with_rhythm(ui::tokens::Rhythm::Group)
            .push(Box::new(
                Text::new("Stopwatch")
                    .with_role(ui::TextRole::Title)
                    .with_heading_level(2),
            ))
            .push(Box::new(view)),
    )
}

/// The open section.
pub(crate) const WORLD: usize = 0;
pub(crate) const ALARMS: usize = 1;
pub(crate) const TIMERS: usize = 2;
pub(crate) const STOPWATCH: usize = 3;

fn surface(
    mode: &ui::LocalState<usize>,
    selection: &ui::LocalState<Option<String>>,
    state: &ui::LocalState<Shown>,
    controls: &Controls,
    notice: &ui::LocalState<Option<(String, String)>>,
    validation_error: &ui::LocalState<Option<String>>,
) -> Element {
    let st = state.clone();
    let page_controls = controls.clone();
    let timer_validation_error = validation_error.clone();
    let page = ModelView::from_state(mode, move |i| match *i {
        WORLD => world_screen(&st, &page_controls),
        ALARMS => alarms_screen(&st, &page_controls),
        STOPWATCH => stopwatch_screen(&st, &page_controls),
        _ => timers_screen(&st, &page_controls, &timer_validation_error),
    });
    let notice_controls = controls.clone();
    let due_notice = ModelView::from_state(notice, move |notice| {
        notice.as_ref().map_or_else(
            || Box::new(Column::new().with_no_padding().with_intrinsic_height()) as Element,
            |(title, body)| {
                Box::new(
                    Column::new()
                        .with_intrinsic_height()
                        .push(Box::new(clock_card(
                            title,
                            Box::new(
                                Row::new()
                                    .with_padding_rhythm(None, None)
                                    .push(Box::new(Text::new(body)))
                                    .push(Box::new(Spacer::new()))
                                    .push(Box::new(
                                        notice_controls.effect(
                                            Button::new("clocks.notice.dismiss", "Dismiss")
                                                .control_size(ui::widget::ControlSize::Large),
                                            &ClocksEffect::DismissNotice,
                                        ),
                                    )),
                            ),
                        ))),
                )
            },
        )
    });
    // Each page already owns its one content gutter. An empty notice
    // must not add another gutter and a large section gap above its title.
    let page = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_rhythm(ui::tokens::Rhythm::Tight)
        .push(Box::new(due_notice))
        .push(Box::new(page));
    let mode_state = mode.clone();
    let sections = [
        (WORLD, "World", Icon::Globe),
        (ALARMS, "Alarms", Icon::Bell),
        (TIMERS, "Timers", Icon::Clock),
        (STOPWATCH, "Stopwatch", Icon::Clock),
    ];
    let sidebar = ui::widget::NavList::new(vec![ui::widget::NavSection::untitled(
        "screens",
        sections
            .iter()
            .map(|(id, label, icon)| ui::widget::NavItem::new(id.to_string(), *label).icon(*icon))
            .collect(),
    )])
    .with_id("clocks.screens")
    .with_accessible_name("Clocks")
    .bind_selection(selection)
    .on_select(move |id| {
        if let Ok(mode) = id.parse::<usize>() {
            mode_state.set(mode);
        }
    });
    Box::new(
        ui::widget::NavShell::new()
            .content_kind(ui::widget::ContentKind::Page)
            .with_id("clocks.shell")
            .mode(ui::widget::NavMode::Push)
            .with_pane_state(&ui::LocalState::new(ui::widget::NavPane::Content))
            .sidebar(Box::new(sidebar))
            .sidebar_header("Clocks", Vec::new())
            .content(Box::new(
                Scrollable::new_vertical()
                    .fill_parent_height()
                    .with_child(Box::new(page)),
            )),
    )
}

/// A due alert not yet shown by the platform: tag, title, body and what a
/// click opens.
type PendingNotification = (String, String, String, &'static str);

/// The Clocks surface.
pub struct ClocksApp {
    inner: Element,
    pub(crate) state: ui::LocalState<Shown>,
    pub(crate) dispatcher: Dispatcher<Clocks>,
    pub(crate) device: Setting<OnThisDevice>,
    /// Whether this window is one someone uses, which rings, schedules and
    /// talks to the shell.
    live: bool,
    pub(crate) record: Option<ui::record::RecordId>,
    pub(crate) opener: Option<ui::framework::OpenHere>,
    /// Whether the section was chosen from the first state presented.
    opened: bool,
    pub(crate) last_notification: Option<(String, String, String)>,
    pub(crate) notice: ui::LocalState<Option<(String, String)>>,
    next_clock_tick: ui::time::Instant,
    /// The open section: World, Alarms, Timers, Stopwatch.
    pub(crate) mode: ui::LocalState<usize>,
    /// The retained navigation projects the same mode as the content.
    selection: ui::LocalState<Option<String>>,
    /// The host's notifications, resolved on first use.
    notifications: Option<std::sync::Arc<dyn ui::notify::Notifier>>,
    pub(crate) pending_notifications: Vec<PendingNotification>,
    next_notification_attempt: ui::time::Instant,
    last_scheduled_clocks: Option<(Clocks, crate::schedule::Delivery, i32)>,
    next_schedule_attempt: ui::time::Instant,
    /// Activation URIs of clicked notifications, from any thread.
    activations: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl ClocksApp {
    /// The surface over the clocks `controls` edit, keeping this device's
    /// choices in `device`. Only a `live` one rings and talks to the shell.
    pub(crate) fn new(controls: Controls, device: Setting<OnThisDevice>, live: bool) -> Self {
        let state = ui::LocalState::new(Shown {
            clocks: Clocks::default(),
            device: device.get(),
            pinned_now: None,
            selected_alarm: None,
        });
        let notice = ui::LocalState::new(None);
        let validation_error = ui::LocalState::new(None);
        let mode = ui::LocalState::new(WORLD);
        let selection = ui::LocalState::new(Some(WORLD.to_string()));
        let activations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        if live {
            let sink = activations.clone();
            let wake = ui::wake::WakeHandle::new();
            ui::notify::set_activation_handler("tilo-clocks", move |uri| {
                if let Ok(mut pending) = sink.lock() {
                    pending.push(uri.to_owned());
                }
                wake.wake();
            });
        }
        Self {
            inner: surface(
                &mode,
                &selection,
                &state,
                &controls,
                &notice,
                &validation_error,
            ),
            state,
            dispatcher: controls.dispatcher,
            device,
            live,
            record: None,
            opener: None,
            opened: false,
            last_notification: None,
            notice,
            next_clock_tick: ui::time::now(),
            mode,
            selection,
            notifications: None,
            pending_notifications: Vec::new(),
            next_notification_attempt: ui::time::now(),
            last_scheduled_clocks: None,
            next_schedule_attempt: ui::time::now(),
            activations,
        }
    }

    pub(crate) fn set_mode(&self, mode: usize) {
        self.mode.set(mode);
        self.selection.set(Some(mode.to_string()));
    }

    /// Show `clocks`. The first time, open the section that most likely
    /// brought the person back; a quiet first launch begins with the
    /// at-a-glance world view.
    pub(crate) fn show(&mut self, clocks: &Clocks) {
        if !self.opened {
            self.opened = true;
            self.set_mode(if !clocks.timers.is_empty() {
                TIMERS
            } else if clocks.stopwatch.is_running() || clocks.stopwatch.accumulated_ms > 0 {
                STOPWATCH
            } else if !clocks.alarms.is_empty() {
                ALARMS
            } else {
                WORLD
            });
        }
        if self.state.with(|shown| shown.clocks != *clocks) {
            self.state.update(|shown| shown.clocks = clocks.clone());
        }
    }

    /// Change this device's choices and show them at once.
    pub(crate) fn update_device(&self, change: impl FnOnce(&mut OnThisDevice)) {
        if let Err(error) = self.device.update(change) {
            tracing::warn!(%error, "Clocks could not keep its device settings");
        }
        self.refresh_device();
    }

    /// Follow a write of this device's choices from anywhere.
    fn refresh_device(&self) {
        let device = self.device.get();
        if self.state.with(|shown| shown.device != device) {
            self.state.update(|shown| shown.device = device);
        }
    }

    /// Retain the latest due notice in every subsequent transport snapshot.
    ///
    /// The shell polls a replace-in-place state file, so a one-frame zero
    /// countdown can be overwritten by the inactive tombstone before the
    /// shell observes it. A stable notification ID lets the producer keep the
    /// notice attached to later tombstones while the shell deduplicates it.
    pub(crate) fn remember_due(&mut self, event: DueEvent) {
        let native = match &event {
            DueEvent::Timer {
                id,
                native_scheduled,
                ..
            } => (native_timer_id(id.as_str()), *native_scheduled),
            DueEvent::Alarm {
                id,
                native_scheduled,
                ..
            } => (native_alarm_id(id.as_str()), *native_scheduled),
        };
        let (title, body, activation) = match event {
            DueEvent::Alarm { label, .. } => ("Alarm".to_owned(), label, ALARMS_URI),
            DueEvent::Timer { label, .. } => ("Timer finished".to_owned(), label, TIMERS_URI),
        };
        let notification_id = uuid::Uuid::new_v4().simple().to_string();
        if self.live {
            let (tag, scheduled) = native.clone();
            if !scheduled {
                self.pending_notifications
                    .push((tag, title.clone(), body.clone(), activation));
            }
        }
        self.notice.set(Some((title.clone(), body.clone())));
        if !native.1 {
            self.last_notification = Some((notification_id, title, body));
        }
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
    fn schedule_native_notifications(&mut self, now: chrono::DateTime<Utc>) {
        if !schedules_notifications() || ui::time::now() < self.next_schedule_attempt {
            return;
        }
        let clocks = self.state.with(|shown| shown.clocks.clone());
        let mut delivery = self.device.get().delivery;
        let offset = chrono::Local::now().offset().local_minus_utc();
        if self.last_scheduled_clocks.as_ref() == Some(&(clocks.clone(), delivery.clone(), offset))
        {
            return;
        }
        let host = self.notifier();
        let device = self.device.clone();
        let result = crate::native_timers::reconcile(
            self.record.as_ref(),
            &clocks,
            &mut delivery,
            &*host,
            now,
            |d| {
                device
                    .update(|device| device.delivery = d.clone())
                    .map_err(|error| error.to_string())
            },
        );
        self.refresh_device();
        match result {
            Ok(()) => self.last_scheduled_clocks = Some((clocks, delivery, offset)),
            Err(error) => {
                tracing::warn!(%error, "Clocks notification scheduling");
                self.next_schedule_attempt = retry_at();
            }
        }
    }

    fn restore_native_alarms(&mut self) {
        if !schedules_notifications() || ui::time::now() < self.next_schedule_attempt {
            return;
        }
        let offset = chrono::Local::now().offset().local_minus_utc();
        if self
            .last_scheduled_clocks
            .as_ref()
            .is_some_and(|(_, _, previous)| *previous == offset)
        {
            return;
        }
        let Ok(states) = self.notifier().alarm_states() else {
            return;
        };
        if states.is_empty() {
            return;
        }
        let clocks = self.state.with(|shown| shown.clocks.clone());
        let mut delivery = self.device.get().delivery;
        crate::native_timers::restore_alarms(&clocks, &mut delivery, &states);
        if delivery != self.device.get().delivery {
            self.update_device(|device| device.delivery = delivery);
        }
    }

    /// Ring what is due, and turn off the one-shot alarms that rang.
    fn ring_due(&mut self, now: chrono::DateTime<Utc>) {
        let mut delivery = self.device.get().delivery;
        let tick = self.state.with(|shown| {
            crate::schedule::tick(&shown.clocks, &mut delivery, now, chrono::Local::now())
        });
        if delivery != self.device.get().delivery {
            self.update_device(|device| device.delivery = delivery);
        }
        for alarm in tick.finished_alarms {
            self.dispatcher
                .submit(Clocks::SET_ALARM_ENABLED.with((alarm, false)));
        }
        for event in tick.due {
            self.remember_due(event);
        }
    }

    /// Answer a control pressed on the shell's floating clock. Stopwatch
    /// controls never fall through to a timer.
    pub(crate) fn apply_shell_command(&self, command: ShellCommand) {
        let now = Utc::now();
        let timer = command.activity_id.map(ui::prelude::Id::from_raw);
        let running = |shown: &Shown, id: &ui::prelude::Id| {
            shown
                .clocks
                .timers
                .get(id)
                .is_some_and(|timer| timer.is_running(now))
        };
        match (command.command.as_str(), timer) {
            ("toggle-stopwatch", _) => {
                let invoke = if self.state.with(|shown| shown.clocks.stopwatch.is_running()) {
                    Clocks::PAUSE_STOPWATCH.invoke()
                } else {
                    Clocks::START_STOPWATCH.invoke()
                };
                self.dispatcher.submit(invoke);
            }
            ("lap-stopwatch", _) => self.dispatcher.submit(Clocks::LAP.invoke()),
            ("close-stopwatch", _) => self.update_device(|device| {
                device.floating_stopwatch = false;
            }),
            ("toggle", Some(id)) => {
                let invoke = if self.state.with(|shown| running(shown, &id)) {
                    Clocks::PAUSE_TIMER.with(id)
                } else {
                    Clocks::RESUME_TIMER.with(id)
                };
                self.dispatcher.submit(invoke);
            }
            ("add-minute", Some(id)) => self.dispatcher.submit(Clocks::ADD_MINUTE.with(id)),
            ("close", Some(id)) => self.update_device(|device| {
                device.floating_timers.remove(id.as_str());
            }),
            _ => {}
        }
    }

    /// The floating clock as the shell reads it.
    fn shell_snapshot(&self, now: chrono::DateTime<Utc>) -> ShellSnapshot {
        let activity = self
            .state
            .with(|shown| shell_activity(&shown.clocks, &shown.device, now));
        let mut snapshot = activity.map_or_else(
            || ShellSnapshot {
                active: false,
                activity_id: None,
                label: String::new(),
                remaining_secs: 0,
                stopwatch: false,
                paused: false,
                overlay: false,
                notification_id: None,
                notification_title: String::new(),
                notification_body: String::new(),
            },
            |a| ShellSnapshot {
                active: true,
                activity_id: a.activity_id,
                label: a.label,
                remaining_secs: a.remaining.as_secs(),
                stopwatch: a.stopwatch,
                paused: a.paused,
                overlay: a.overlay,
                notification_id: None,
                notification_title: String::new(),
                notification_body: String::new(),
            },
        );
        if let Some((id, title, body)) = &self.last_notification {
            snapshot.notification_id = Some(id.clone());
            snapshot.notification_title.clone_from(title);
            snapshot.notification_body.clone_from(body);
        }
        snapshot
    }

    /// Open the section a clicked notification names.
    pub(crate) fn activate_uri(&self, uri: &str) {
        if let Some(record) = crate::native_timers::activation_record(uri)
            && self.record.as_ref() != Some(&record)
        {
            if let Some(opener) = &self.opener {
                opener.open(&[uri.to_owned()]);
            }
            return;
        }
        if let Some(id) = crate::native_timers::activation_alarm(uri) {
            self.dispatcher.effect(&ClocksEffect::OpenAlarm {
                alarm: id.to_owned(),
            });
        } else {
            match uri {
                TIMERS_URI => self.set_mode(TIMERS),
                ALARMS_URI => self.set_mode(ALARMS),
                _ => {}
            }
        }
    }

    fn apply_activations(&self) {
        let uris = self
            .activations
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for uri in uris {
            self.activate_uri(&uri);
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

impl ui::widget::Decorator for ClocksApp {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.inner.as_ref()
    }
    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.inner.as_mut()
    }
    fn sync(&mut self, d: &AppModel, p: &ModelPath) {
        let tick_base = ui::time::now();
        self.next_clock_tick = tick_base
            .checked_add(Duration::from_secs(1))
            .unwrap_or(tick_base);
        self.refresh_device();
        self.apply_activations();
        if self.live {
            let now = Utc::now();
            for command in crate::transport::take_commands() {
                self.apply_shell_command(command);
            }
            self.restore_native_alarms();
            self.ring_due(now);
            self.schedule_native_notifications(now);
            self.deliver_native_notifications();
            crate::transport::publish(&self.shell_snapshot(now));
        }
        // Countdowns move every second without any change to the clocks.
        self.state.update(|_| {});
        let selected = Some(self.mode.get().to_string());
        if self.selection.get() != selected {
            self.selection.set(selected);
        }
        self.inner.sync(d, p);
    }
    fn name(&self) -> &'static str {
        "ClocksApp"
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
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;

fn clock_card(title: &str, body: Element) -> Card {
    Card::new(body).with_title(title)
}
