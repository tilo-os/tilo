//! The Clocks window: a surface over the person's [`Clocks`]. It shows the
//! state it is presented and changes it only through the resource's
//! actions; what stays on this device (floating clocks, the delivery
//! ledger) goes through the app's device-local settings.
//!
//! A live window also rings what is due, hands running timers to the
//! platform's notification scheduler, and keeps the shell's floating clock
//! current. A capture or a past version does none of that.

use crate::model::{Clocks, WeekdayKey, bounded_label};
use crate::native_timers::{ALARMS_URI, TIMERS_URI, timer_id as native_timer_id};
use crate::resource::ClocksEffect;
use crate::schedule::{DueEvent, OnThisDevice, shell_activity};
use crate::transport::{ShellCommand, ShellSnapshot};
use chrono::Utc;
use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};
use taffy::{NodeId, TaffyTree};
use ui::framework::{Actions, Dispatcher, Setting};
use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Card, CardGrid, Column, Container, Element, ModelView, ProgressBar, Row, Scrollable,
    Spacer, Text, TextInput, TextInputKind, TimePicker, TimeValue, ToggleGroup, Widget,
};
use ui::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

/// What the window shows: the person's clocks and this device's choices.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Shown {
    pub(crate) clocks: Clocks,
    pub(crate) device: OnThisDevice,
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
    Box::new(Button::new(id, label, None).fit_content(44.0).on_effect(f))
}

fn primary_button(id: &str, label: &str, f: impl FnMut() + 'static) -> Element {
    Box::new(
        Button::new(id, label, None)
            .primary()
            .fit_content(44.0)
            .on_effect(f),
    )
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

fn city_time(zone: &str) -> String {
    zone.parse::<chrono_tz::Tz>().map_or_else(
        |_| "Time unavailable".into(),
        |z| {
            Utc::now()
                .with_timezone(&z)
                .format("%H:%M · %a %-d %b · %Z · UTC%:z")
                .to_string()
        },
    )
}

fn timezone_label(zone: &str) -> String {
    let city = zone.rsplit('/').next().unwrap_or(zone).replace('_', " ");
    let region = zone.split('/').next().unwrap_or_default();
    if region.is_empty() || region == city {
        city
    } else {
        format!("{city} · {region}")
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

#[expect(
    clippy::too_many_lines,
    reason = "declarative UI builder that reads top-to-bottom; splitting it would scatter one screen across helpers"
)]
fn world_screen(state: &ui::LocalState<Shown>, controls: &Controls) -> Element {
    let list_controls = controls.clone();
    let list = ModelView::from_state(state, move |s| {
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0);
        if s.clocks.world_clocks.is_empty() {
            col = col.push(Box::new(
                Text::new("No cities yet. Add one below to compare time at a glance.").dimmed(),
            ));
        }
        for (id, clock) in s.clocks.world_clocks.iter() {
            let row = Row::new()
                .push(Box::new(
                    Text::new(&city_time(&clock.timezone.0)).with_size(28.0),
                ))
                .push(Box::new(Spacer::new()))
                .push(Box::new(
                    list_controls.button(
                        Button::new(&format!("clocks.world.{id}.remove"), "Remove", None)
                            .fit_content(44.0),
                        Clocks::REMOVE_WORLD_CLOCK.with(id.clone()),
                    ),
                ));
            col = col.push(Box::new(Card::titled(&clock.city, Box::new(row))));
        }
        Box::new(col)
    });
    let city_query = ui::LocalState::new(String::new());
    let result_query = city_query.clone();
    let add = controls.dispatcher.clone();
    let choices = ModelView::from_state(&city_query, move |query| {
        let mut choices = Row::new();
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
                    add.invoke(Clocks::ADD_WORLD_CLOCK.with((city.clone(), zone.clone())));
                    clear.set(String::new());
                },
            ));
        }
        Box::new(choices)
    });
    Box::new(
        Column::new()
            .with_gap(18.0)
            .push(Box::new(
                Text::new("World clocks")
                    .with_size(32.0)
                    .with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Keep the people and places that matter in view.").dimmed(),
            ))
            .push(Box::new(
                CardGrid::content_sized(vec![
                    Box::new(Card::titled(
                        "Here",
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(6.0)
                                .push(Box::new(
                                    Text::new(&chrono::Local::now().format("%H:%M").to_string())
                                        .with_size(52.0),
                                ))
                                .push(Box::new(
                                    Text::new(
                                        &chrono::Local::now().format("%A, %-d %B").to_string(),
                                    )
                                    .dimmed(),
                                )),
                        ),
                    )),
                    Box::new(Card::titled(
                        "Add a city",
                        Box::new(
                            Column::new()
                                .with_no_padding()
                                .with_intrinsic_height()
                                .with_gap(10.0)
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
                .with_min_card_width(390.0),
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
            .with_gap(10.0);
        if s.clocks.alarms.is_empty() {
            col = col.push(Box::new(
                Text::new("No alarms yet. Set one for a future time.").dimmed(),
            ));
        }
        for (id, alarm) in s.clocks.alarms.iter() {
            let repeat = repeat_summary(&alarm.weekdays());
            let row = Row::new()
                .push(Box::new(
                    Text::new(&alarm.local_time().format("%H:%M").to_string()).with_size(32.0),
                ))
                .push(Box::new(
                    Text::new(&format!(
                        "{repeat} · {}",
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
                            None,
                        )
                        .fit_content(44.0),
                        Clocks::SET_ALARM_ENABLED.with((id.clone(), !alarm.enabled)),
                    ),
                ))
                .push(Box::new(
                    list_controls.button(
                        Button::new(&format!("clocks.alarm.{id}.remove"), "Remove", None)
                            .fit_content(44.0),
                        Clocks::REMOVE_ALARM.with(id.clone()),
                    ),
                ));
            col = col.push(Box::new(Card::titled(&alarm.label, Box::new(row))));
        }
        Box::new(col)
    });
    Box::new(
        Column::new()
            .with_gap(18.0)
            .push(Box::new(
                Text::new("Alarms").with_size(32.0).with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("A reliable nudge, exactly when you need it.").dimmed(),
            ))
            .push(Box::new(list))
            .push(Box::new(Card::titled(
                "New alarm",
                Box::new(
                    Column::new()
                        .with_no_padding()
                        .with_intrinsic_height()
                        .with_gap(12.0)
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
                                        None,
                                    )
                                    .with_size(44.0, 44.0)
                                    .with_accessible_name(&format!("Repeat on {label}"))
                                    .on_effect(move || {
                                        selection.update(|days| {
                                            if !days.remove(&day) {
                                                days.insert(day);
                                            }
                                        });
                                    });
                                    if selected.contains(&day) {
                                        day_button = day_button.primary();
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
                                        .with_gap(8.0)
                                        .push(Box::new(first_row))
                                        .push(Box::new(second_row)),
                                )
                            },
                        )))
                        .push(primary_button("clocks.alarm.add", "Add alarm", move || {
                            let time = add_draft.get();
                            add.invoke(Clocks::ADD_ALARM.with((
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
    Box::new(Card::titled(
        &timer.label,
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(12.0)
                .push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(&duration(remaining_ms)).with_size(64.0)))
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
                                Button::new(
                                    &format!("clocks.timer.{id}.add-minute"),
                                    "+1 min",
                                    None,
                                )
                                .with_accessible_name(&format!("Add one minute to {}", timer.label))
                                .fit_content(44.0),
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
                                    None,
                                )
                                .with_icon(if running { Icon::Pause } else { Icon::Play })
                                .primary()
                                .fit_content(44.0),
                                toggle,
                            ),
                        ))
                        .push(Box::new(
                            controls.effect(
                                Button::new(
                                    &format!("clocks.timer.{id}.pin"),
                                    if floating { "Hide" } else { "Float" },
                                    None,
                                )
                                .with_icon(Icon::PictureInPicture2)
                                .with_accessible_name(float_label)
                                .tooltip(float_label)
                                .fit_content(44.0),
                                &ClocksEffect::FloatTimer {
                                    timer: id.as_str().to_owned(),
                                    floating: !floating,
                                },
                            ),
                        ))
                        .push(Box::new(
                            controls.button(
                                Button::new(&format!("clocks.timer.{id}.remove"), "", None)
                                    .with_icon(Icon::Trash2)
                                    .with_accessible_name("Remove timer")
                                    .tooltip("Remove timer")
                                    .danger()
                                    .fit_content(44.0),
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
    let custom_hours = ui::LocalState::new("00".to_string());
    let custom_minutes = ui::LocalState::new("10".to_string());
    let custom_seconds = ui::LocalState::new("00".to_string());
    let custom_label = ui::LocalState::new(String::new());
    let preset_specs = [
        ("Tea", "Tea · 5m", 300),
        ("Break", "Break · 10m", 600),
        ("Focus", "Focus · 25m", 1500),
        ("Laundry", "Laundry · 45m", 2700),
    ];
    let mut presets = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(8.0);
    for preset_row in preset_specs.chunks(2) {
        let mut row = Row::new();
        for &(label, button_label, secs) in preset_row {
            // A common, generous width keeps the two-column grid legible at
            // 200% text while still fitting inside the phone content rail.
            row = row.push(Box::new(
                controls.button(
                    Button::new(&format!("clocks.preset.{secs}"), button_label, None)
                        .with_size(200.0, 44.0),
                    Clocks::ADD_TIMER.with((label.to_owned(), secs)),
                ),
            ));
        }
        presets = presets.push(Box::new(row));
    }
    let list_controls = controls.clone();
    let list = ModelView::from_state(state, move |s| {
        if s.clocks.timers.is_empty() {
            return Box::new(Spacer::new());
        }
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0);
        for (id, timer) in s.clocks.timers.iter() {
            let floating = s.device.floating_timers.contains(id.as_str());
            col = col.push(timer_card(id, timer, floating, &list_controls));
        }
        Box::new(Card::titled("Active timers", Box::new(col)))
    });
    let add = controls.dispatcher.clone();
    let add_hours = custom_hours.clone();
    let add_minutes = custom_minutes.clone();
    let add_seconds = custom_seconds.clone();
    let add_label = custom_label.clone();
    let add_error = validation_error.clone();
    let error = ModelView::from_state(validation_error, |message| {
        message.as_ref().map_or_else(
            || Box::new(Spacer::new()) as Element,
            |message| {
                Box::new(Card::titled(
                    "Timer needs a duration",
                    Box::new(Text::new(message)),
                ))
            },
        )
    });
    // The grid resolves to two columns where there is room and one where
    // there is not. Unlike the former three-spinner row, it never shrinks a
    // control below a readable, independently targetable width.
    let duration_fields = CardGrid::content_sized(vec![
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(6.0)
                .push(Box::new(Text::new("Hours").with_heading_level(4)))
                .push(Box::new(
                    TextInput::from_state(&custom_hours)
                        .with_id("clocks.timer.custom.hours")
                        .with_kind(TextInputKind::Number)
                        .with_accessible_name("Timer duration hours")
                        .with_placeholder("00"),
                )),
        ),
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(6.0)
                .push(Box::new(Text::new("Minutes").with_heading_level(4)))
                .push(Box::new(
                    TextInput::from_state(&custom_minutes)
                        .with_id("clocks.timer.custom.minutes")
                        .with_kind(TextInputKind::Number)
                        .with_accessible_name("Timer duration minutes")
                        .with_placeholder("10"),
                )),
        ),
        Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(6.0)
                .push(Box::new(Text::new("Seconds").with_heading_level(4)))
                .push(Box::new(
                    TextInput::from_state(&custom_seconds)
                        .with_id("clocks.timer.custom.seconds")
                        .with_kind(TextInputKind::Number)
                        .with_accessible_name("Timer duration seconds")
                        .with_placeholder("00"),
                )),
        ),
    ])
    .with_min_card_width(200.0)
    .with_gap(12.0);
    let setup = CardGrid::content_sized(vec![
        Box::new(Card::titled(
            "Quick starts",
            Box::new(
                Column::new()
                    .with_no_padding()
                    .with_intrinsic_height()
                    .with_gap(10.0)
                    .push(Box::new(
                        Text::new("A focused timer is one tap away.").dimmed(),
                    ))
                    .push(Box::new(presets)),
            ),
        )),
        Box::new(Card::titled(
            "Custom timer",
            Box::new(
                Column::new()
                    .with_gap(12.0)
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
                                &add_hours.get(),
                                &add_minutes.get(),
                                &add_seconds.get(),
                            ) else {
                                add_error.set(Some(
                                    "Use whole numbers: hours 0–99 and minutes and seconds 0–59."
                                        .into(),
                                ));
                                return;
                            };
                            if secs > 0 {
                                add.invoke(
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
    .with_min_card_width(390.0);
    Box::new(
        Column::new()
            .with_gap(18.0)
            .push(Box::new(
                Text::new("Timers").with_size(32.0).with_heading_level(2),
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
            .with_gap(5.0);
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
                .with_gap(12.0)
                .push(Box::new(Text::new(&duration(elapsed_ms)).with_size(44.0)))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            controls.button(
                                Button::new(
                                    "clocks.stopwatch.toggle",
                                    if running { "Pause" } else { "Start" },
                                    None,
                                )
                                .primary()
                                .with_icon(if running { Icon::Pause } else { Icon::Play })
                                .fit_content(44.0),
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
                                    Button::new("clocks.stopwatch.lap", "Lap", None)
                                        .fit_content(44.0),
                                    Clocks::LAP.invoke(),
                                )
                                .with_enabled(running),
                        ))
                        .push(Box::new(
                            controls
                                .button(
                                    Button::new("clocks.stopwatch.reset", "Reset", None)
                                        .fit_content(44.0),
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
                            None,
                        )
                        .with_icon(Icon::PictureInPicture2)
                        .fit_content(44.0),
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
            .with_gap(14.0)
            .push(Box::new(
                Text::new("Stopwatch").with_size(24.0).with_heading_level(2),
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
    state: &ui::LocalState<Shown>,
    controls: &Controls,
    notice: &ui::LocalState<Option<(String, String)>>,
    validation_error: &ui::LocalState<Option<String>>,
) -> Element {
    let select = mode.clone();
    let st = state.clone();
    let page_controls = controls.clone();
    let timer_validation_error = validation_error.clone();
    let nav = ToggleGroup::new(
        vec![
            Segment::new("World"),
            Segment::new("Alarms"),
            Segment::new("Timers"),
            Segment::new("Stopwatch"),
        ],
        mode.get(),
    )
    .with_flexible_width()
    .with_style(ui::widget::toggle_group::SegmentedStyle::Pill)
    .with_id("clocks.mode")
    .on_view("clocks.mode.select", move |i| select.set(i));
    let nav = Row::new()
        .push(Box::new(Spacer::new()))
        .push(Box::new(
            Container::new()
                .with_background()
                .with_padding(8.0)
                .with_max_width(760.0)
                .with_child(Box::new(nav)),
        ))
        .push(Box::new(Spacer::new()));
    let page = ModelView::from_state(mode, move |i| match *i {
        WORLD => world_screen(&st, &page_controls),
        ALARMS => alarms_screen(&st, &page_controls),
        STOPWATCH => stopwatch_screen(&st, &page_controls),
        _ => timers_screen(&st, &page_controls, &timer_validation_error),
    });
    let dismiss_notice = notice.clone();
    let due_notice = ModelView::from_state(notice, move |notice| {
        notice.as_ref().map_or_else(
            || Box::new(Spacer::new()) as Element,
            |(title, body)| {
                Box::new(Card::titled(
                    title,
                    Box::new(
                        Row::new()
                            .push(Box::new(Text::new(body)))
                            .push(Box::new(Spacer::new()))
                            .push(button("clocks.notice.dismiss", "Dismiss", {
                                let dismiss_notice = dismiss_notice.clone();
                                move || dismiss_notice.set(None)
                            })),
                    ),
                ))
            },
        )
    });
    let header = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(6.0)
        .push(Box::new(
            Text::new("Clocks").with_size(40.0).with_heading_level(1),
        ))
        .push(Box::new(
            Text::new("World time, alarms, timers and focus tools.").dimmed(),
        ));
    let content = Column::new()
        .with_gap(16.0)
        .push(Box::new(header))
        .push(Box::new(due_notice))
        .push(Box::new(nav))
        .push(Box::new(
            Scrollable::new_vertical()
                .fill_parent_height()
                .with_child(Box::new(page)),
        ));
    Box::new(
        Row::new()
            .push(Box::new(Spacer::new()))
            .push(Box::new(
                Container::new()
                    .with_no_padding()
                    .with_max_width(1120.0)
                    .filling()
                    .with_child(Box::new(content)),
            ))
            .push(Box::new(Spacer::new())),
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
    /// Whether the section was chosen from the first state presented.
    opened: bool,
    generation: u64,
    pub(crate) last_notification: Option<(String, String, String)>,
    pub(crate) notice: ui::LocalState<Option<(String, String)>>,
    next_clock_tick: ui::time::Instant,
    /// The open section: World, Alarms, Timers, Stopwatch.
    pub(crate) mode: ui::LocalState<usize>,
    /// The host's notifications, resolved on first use.
    notifications: Option<std::sync::Arc<dyn ui::notify::Notifier>>,
    pub(crate) pending_notifications: Vec<PendingNotification>,
    next_notification_attempt: ui::time::Instant,
    last_scheduled_timers: Option<Vec<(String, Option<i64>)>>,
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
        });
        let notice = ui::LocalState::new(None);
        let validation_error = ui::LocalState::new(None);
        let mode = ui::LocalState::new(WORLD);
        let activations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        if live {
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
            inner: surface(&mode, &state, &controls, &notice, &validation_error),
            state,
            dispatcher: controls.dispatcher,
            device,
            live,
            opened: false,
            generation: 0,
            last_notification: None,
            notice,
            next_clock_tick: ui::time::now(),
            mode,
            notifications: None,
            pending_notifications: Vec::new(),
            next_notification_attempt: ui::time::now(),
            last_scheduled_timers: None,
            next_schedule_attempt: ui::time::now(),
            activations,
        }
    }

    /// Show `clocks`. The first time, open the section that most likely
    /// brought the person back; a quiet first launch begins with the
    /// at-a-glance world view.
    pub(crate) fn show(&mut self, clocks: &Clocks) {
        if !self.opened {
            self.opened = true;
            self.mode.set(if !clocks.timers.is_empty() {
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
            } if ui::host_notifications::SUPPORTED => {
                Some((native_timer_id(id.as_str()), *native_scheduled))
            }
            DueEvent::Timer { .. } | DueEvent::Alarm { .. } => None,
        };
        let (title, body, activation) = match event {
            DueEvent::Alarm { label, .. } => ("Alarm".to_owned(), label, ALARMS_URI),
            DueEvent::Timer { label, .. } => ("Timer finished".to_owned(), label, TIMERS_URI),
        };
        let notification_id = uuid::Uuid::new_v4().simple().to_string();
        if self.live {
            let (tag, scheduled) =
                native.unwrap_or_else(|| (notification_id.chars().take(16).collect(), false));
            if !scheduled {
                self.pending_notifications
                    .push((tag, title.clone(), body.clone(), activation));
            }
        }
        self.notice.set(Some((title.clone(), body.clone())));
        self.last_notification = Some((notification_id, title, body));
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
        if !schedules_notifications() || ui::time::now() < self.next_schedule_attempt {
            return;
        }
        let timers = self.state.with(|shown| {
            shown
                .clocks
                .timers
                .iter()
                .map(|(id, timer)| (id.as_str().to_owned(), timer.ends_at_ms))
                .collect::<Vec<_>>()
        });
        if self.last_scheduled_timers.as_ref() == Some(&timers) {
            return;
        }
        let host = self.notifier();
        let clocks = self.state.with(|shown| shown.clocks.clone());
        let mut delivery = self.device.get().delivery;
        let device = self.device.clone();
        let result = crate::native_timers::reconcile(&clocks, &mut delivery, &*host, now, |d| {
            device
                .update(|device| device.delivery = d.clone())
                .map_err(|error| error.to_string())
        });
        self.refresh_device();
        match result {
            Ok(()) => self.last_scheduled_timers = Some(timers),
            Err(error) => {
                tracing::warn!(%error, "Clocks timer scheduling");
                self.next_schedule_attempt = retry_at();
            }
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
                .invoke(Clocks::SET_ALARM_ENABLED.with((alarm, false)));
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
                self.dispatcher.invoke(invoke);
            }
            ("lap-stopwatch", _) => self.dispatcher.invoke(Clocks::LAP.invoke()),
            ("close-stopwatch", _) => self.update_device(|device| {
                device.floating_stopwatch = false;
            }),
            ("toggle", Some(id)) => {
                let invoke = if self.state.with(|shown| running(shown, &id)) {
                    Clocks::PAUSE_TIMER.with(id)
                } else {
                    Clocks::RESUME_TIMER.with(id)
                };
                self.dispatcher.invoke(invoke);
            }
            ("add-minute", Some(id)) => self.dispatcher.invoke(Clocks::ADD_MINUTE.with(id)),
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
        let producer_pid = tilo_process::id().unwrap_or(0);
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
                producer_pid,
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
                producer_pid,
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
    fn apply_activations(&self) {
        let uris = self
            .activations
            .lock()
            .map(|mut pending| std::mem::take(&mut *pending))
            .unwrap_or_default();
        for uri in uris {
            match uri.as_str() {
                TIMERS_URI => self.mode.set(TIMERS),
                ALARMS_URI => self.mode.set(ALARMS),
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

impl Widget for ClocksApp {
    fn sync(&mut self, d: &mut AppModel, p: &ModelPath) {
        let tick_base = ui::time::now();
        self.next_clock_tick = tick_base
            .checked_add(Duration::from_secs(1))
            .unwrap_or(tick_base);
        self.refresh_device();
        self.apply_activations();
        if self.live {
            let now = Utc::now();
            if let Some(command) = crate::transport::take_command() {
                self.apply_shell_command(command);
            }
            self.schedule_native_timers(now);
            self.ring_due(now);
            self.deliver_native_notifications();
            self.generation = self.generation.saturating_add(1);
            if let Err(error) = crate::transport::publish(&self.shell_snapshot(now)) {
                tracing::debug!(%error, "Clocks could not update the shell's floating clock");
            }
        }
        // Countdowns move every second without any change to the clocks.
        self.state.update(|_| {});
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
        vec![self.inner.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.inner.as_mut()]
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

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
