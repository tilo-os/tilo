//! Responsive Clocks UI. All controls mutate the durable engine state directly.
use crate::native_timers::{ALARMS_URI, TIMERS_URI, timer_id as native_timer_id};
use crate::{Alarm, ClockState, DueEvent, ShellSnapshot, Timer, WeekdayKey, WorldClock};
use automerge::{AutoCommit, ObjId};
use chrono::{NaiveTime, Utc};
use std::{
    collections::{BTreeSet, HashMap},
    time::Duration,
};
use taffy::{NodeId, TaffyTree};
use ui::icons::Icon;
use ui::widget::toggle_group::Segment;
use ui::widget::{AppModel, ModelPath};
use ui::widget::{
    Button, Card, CardGrid, Column, Container, Element, ModelView, ProgressBar, Row, Scrollable,
    Spacer, Text, TextInput, TextInputKind, TimePicker, TimeValue, ToggleGroup, Widget,
};
use ui::{presence::PresenceCtx, text_render::TextRenderer, theme::Theme};
use uuid::Uuid;
use vello::{
    Scene,
    kurbo::{Affine, Point, Rect},
};

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

fn quick_start_button(id: &str, label: &str, f: impl FnMut() + 'static) -> Element {
    // A common, generous width keeps the two-column grid legible at 200% text
    // while still fitting inside the phone content rail.
    Box::new(
        Button::new(id, label, None)
            .with_size(200.0, 44.0)
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
fn world_screen(state: &ui::LocalState<ClockState>) -> Element {
    let list_state = state.clone();
    let list = ModelView::from_state(state, move |s| {
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0);
        if s.world_clocks.is_empty() {
            col = col.push(Box::new(
                Text::new("No cities yet. Add one below to compare time at a glance.").dimmed(),
            ));
        }
        for clock in &s.world_clocks {
            let id = clock.id;
            let remove = list_state.clone();
            let row = Row::new()
                .push(Box::new(
                    Text::new(&city_time(&clock.timezone)).with_size(28.0),
                ))
                .push(Box::new(Spacer::new()))
                .push(button(
                    &format!("clocks.world.{id}.remove"),
                    "Remove",
                    move || remove.update(|s| s.world_clocks.retain(|c| c.id != id)),
                ));
            col = col.push(Box::new(Card::titled(&clock.city, Box::new(row))));
        }
        Box::new(col)
    });
    let city_query = ui::LocalState::new(String::new());
    let result_state = state.clone();
    let result_query = city_query.clone();
    let choices = ModelView::from_state(&city_query, move |query| {
        let mut choices = Row::new();
        for zone in timezone_results(query) {
            let st = result_state.clone();
            let clear = result_query.clone();
            let city = zone.rsplit('/').next().unwrap_or(&zone).replace('_', " ");
            let label = timezone_label(&zone);
            let id = zone.to_lowercase().replace(['/', '_'], "-");
            choices = choices.push(button(
                &format!("clocks.world.add.{id}"),
                &label,
                move || {
                    let city = city.clone();
                    let zone = zone.clone();
                    st.update(|s| {
                        if !s.world_clocks.iter().any(|clock| clock.timezone == zone) {
                            s.world_clocks.push(WorldClock {
                                id: Uuid::new_v4(),
                                city,
                                timezone: zone,
                            });
                        }
                    });
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
fn alarms_screen(state: &ui::LocalState<ClockState>) -> Element {
    let draft = ui::LocalState::new(TimeValue::new(7, 30));
    let draft_label = ui::LocalState::new(String::new());
    let draft_repeat = ui::LocalState::new(BTreeSet::<WeekdayKey>::new());
    let add_state = state.clone();
    let add_draft = draft.clone();
    let add_label = draft_label.clone();
    let add_repeat = draft_repeat.clone();
    let repeat_controls = draft_repeat.clone();
    let list_state = state.clone();
    let list = ModelView::from_state(state, move |s| {
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0);
        if s.alarms.is_empty() {
            col = col.push(Box::new(
                Text::new("No alarms yet. Set one for a future time.").dimmed(),
            ));
        }
        for alarm in &s.alarms {
            let id = alarm.id;
            let toggle = list_state.clone();
            let remove = list_state.clone();
            let repeat = repeat_summary(&alarm.weekdays);
            let row = Row::new()
                .push(Box::new(
                    Text::new(&alarm.local_time.format("%H:%M").to_string()).with_size(32.0),
                ))
                .push(Box::new(
                    Text::new(&format!(
                        "{repeat} · {}",
                        if alarm.enabled { "On" } else { "Off" }
                    ))
                    .dimmed(),
                ))
                .push(Box::new(Spacer::new()))
                .push(button(
                    &format!("clocks.alarm.{id}.toggle"),
                    if alarm.enabled { "Turn off" } else { "Turn on" },
                    move || {
                        toggle.update(|s| {
                            if let Some(a) = s.alarms.iter_mut().find(|a| a.id == id) {
                                a.set_enabled(!a.enabled);
                            }
                        });
                    },
                ))
                .push(button(
                    &format!("clocks.alarm.{id}.remove"),
                    "Remove",
                    move || remove.update(|s| s.alarms.retain(|a| a.id != id)),
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
                            let t = add_draft.get();
                            add_state.update(|s| {
                                s.alarms.push(Alarm {
                                    id: Uuid::new_v4(),
                                    label: alarm_label(&add_label.get()),
                                    local_time: NaiveTime::from_hms_opt(
                                        t.hours.into(),
                                        t.minutes.into(),
                                        0,
                                    )
                                    .unwrap_or_default(),
                                    weekdays: add_repeat.get(),
                                    enabled: true,
                                    last_fired_local_date: None,
                                    next_due_utc: None,
                                });
                            });
                            add_label.set(String::new());
                            add_repeat.set(BTreeSet::new());
                        })),
                ),
            ))),
    )
}

#[expect(
    clippy::too_many_lines,
    reason = "declarative UI builder that reads top-to-bottom; splitting it would scatter one screen across helpers"
)]
fn timers_screen(
    state: &ui::LocalState<ClockState>,
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
            let st = state.clone();
            row = row.push(quick_start_button(
                &format!("clocks.preset.{secs}"),
                button_label,
                move || {
                    let mut t = Timer::new(label, Duration::from_secs(secs));
                    t.start(Utc::now());
                    st.update(|s| s.timers.push(t));
                },
            ));
        }
        presets = presets.push(Box::new(row));
    }
    let list_state = state.clone();
    let list = ModelView::from_state(state, move |s| {
        let now = Utc::now();
        let mut col = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(10.0);
        if s.timers.is_empty() {
            return Box::new(Spacer::new());
        }
        for timer in &s.timers {
            let id = timer.id;
            let toggle = list_state.clone();
            let add_minute = list_state.clone();
            let pin = list_state.clone();
            let remove = list_state.clone();
            let running = timer.ends_at_utc.is_some();
            let remaining_ms = timer
                .remaining(now)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX);
            let progress = remaining_fraction(remaining_ms, timer.duration_ms);
            col = col.push(Box::new(Card::titled(
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
                                    Text::new(if running { "Running" } else { "Paused" }).dimmed(),
                                )),
                        ))
                        .push(Box::new(
                            Container::new().with_no_padding().with_child(Box::new(
                                ProgressBar::new(progress)
                                    .with_label(format!("{} remaining", timer.label)),
                            )),
                        ))
                        .push(Box::new(
                            Row::new()
                                .push(Box::new(
                                    Button::new(
                                        &format!("clocks.timer.{id}.add-minute"),
                                        "+1 min",
                                        None,
                                    )
                                    .with_accessible_name(&format!(
                                        "Add one minute to {}",
                                        timer.label
                                    ))
                                    .fit_content(44.0)
                                    .on_effect(move || {
                                        add_minute.update(|s| {
                                            if let Some(t) =
                                                s.timers.iter_mut().find(|t| t.id == id)
                                            {
                                                let now = Utc::now();
                                                let left = t
                                                    .remaining(now)
                                                    .saturating_add(Duration::from_secs(60));
                                                t.paused_remaining_ms = Some(
                                                    left.as_millis().try_into().unwrap_or(u64::MAX),
                                                );
                                                t.ends_at_utc = None;
                                                t.start(now);
                                            }
                                        });
                                    }),
                                ))
                                .push(Box::new(
                                    Button::new(
                                        &format!("clocks.timer.{id}.toggle"),
                                        if running { "Pause" } else { "Resume" },
                                        None,
                                    )
                                    .with_icon(if running { Icon::Pause } else { Icon::Play })
                                    .primary()
                                    .fit_content(44.0)
                                    .on_effect(move || {
                                        toggle.update(|s| {
                                            if let Some(t) =
                                                s.timers.iter_mut().find(|t| t.id == id)
                                            {
                                                if t.ends_at_utc.is_some() {
                                                    t.pause(Utc::now());
                                                } else {
                                                    t.start(Utc::now());
                                                }
                                            }
                                        });
                                    }),
                                ))
                                .push(Box::new(
                                    Button::new(
                                        &format!("clocks.timer.{id}.pin"),
                                        if timer.pinned_overlay {
                                            "Hide"
                                        } else {
                                            "Float"
                                        },
                                        None,
                                    )
                                    .with_icon(Icon::PictureInPicture2)
                                    .with_accessible_name(if timer.pinned_overlay {
                                        "Hide floating timer"
                                    } else {
                                        "Float timer above other windows"
                                    })
                                    .tooltip(if timer.pinned_overlay {
                                        "Hide floating timer"
                                    } else {
                                        "Float timer above other windows"
                                    })
                                    .fit_content(44.0)
                                    .on_effect(move || {
                                        pin.update(|s| {
                                            if let Some(t) =
                                                s.timers.iter_mut().find(|t| t.id == id)
                                            {
                                                t.pinned_overlay = !t.pinned_overlay;
                                            }
                                        });
                                    }),
                                ))
                                .push(Box::new(
                                    Button::new(&format!("clocks.timer.{id}.remove"), "", None)
                                        .with_icon(Icon::Trash2)
                                        .with_accessible_name("Remove timer")
                                        .tooltip("Remove timer")
                                        .danger()
                                        .fit_content(44.0)
                                        .on_effect(move || {
                                            remove.update(|s| s.timers.retain(|t| t.id != id));
                                        }),
                                )),
                        )),
                ),
            )));
        }
        Box::new(Card::titled("Active timers", Box::new(col)))
    });
    let add_state = state.clone();
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
                                let mut t = Timer::new(
                                    timer_label(&add_label.get()),
                                    Duration::from_secs(secs),
                                );
                                t.start(Utc::now());
                                add_state.update(|s| s.timers.push(t));
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

fn stopwatch_screen(state: &ui::LocalState<ClockState>) -> Element {
    let controls = state.clone();
    let view = ModelView::from_state(state, move |s| {
        let now = Utc::now();
        let running = s.stopwatch.started_at_utc.is_some();
        let mut laps = Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .with_gap(5.0);
        for (i, ms) in s.stopwatch.laps_ms.iter().enumerate().rev() {
            laps = laps.push(Box::new(Text::new(&format!(
                "Lap {}   {}",
                i.saturating_add(1),
                duration(*ms)
            ))));
        }
        let elapsed_ms: u64 = s
            .stopwatch
            .elapsed(now)
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX);
        let a = controls.clone();
        let b = controls.clone();
        let c = controls.clone();
        let d = controls.clone();
        Box::new(Card::new(Box::new(
            Column::new()
                .with_no_padding()
                .with_intrinsic_height()
                .with_gap(12.0)
                .push(Box::new(Text::new(&duration(elapsed_ms)).with_size(44.0)))
                .push(Box::new(
                    Row::new()
                        .push(Box::new(
                            Button::new(
                                "clocks.stopwatch.toggle",
                                if running { "Pause" } else { "Start" },
                                None,
                            )
                            .primary()
                            .with_icon(if running { Icon::Pause } else { Icon::Play })
                            .fit_content(44.0)
                            .on_effect(move || {
                                a.update(|s| {
                                    if s.stopwatch.started_at_utc.is_some() {
                                        s.stopwatch.pause(Utc::now());
                                    } else {
                                        s.stopwatch.start(Utc::now());
                                    }
                                });
                            }),
                        ))
                        .push(Box::new(
                            Button::new("clocks.stopwatch.lap", "Lap", None)
                                .fit_content(44.0)
                                .with_enabled(running)
                                .on_effect(move || b.update(|s| s.stopwatch.lap(Utc::now()))),
                        ))
                        .push(Box::new(
                            Button::new("clocks.stopwatch.reset", "Reset", None)
                                .fit_content(44.0)
                                .with_enabled(elapsed_ms != 0 || !s.stopwatch.laps_ms.is_empty())
                                .on_effect(move || c.update(|s| s.stopwatch.reset())),
                        )),
                ))
                .push(Box::new(
                    Button::new(
                        "clocks.stopwatch.pin",
                        if s.stopwatch.pinned_overlay {
                            "Stop floating"
                        } else {
                            "Keep stopwatch visible"
                        },
                        None,
                    )
                    .with_icon(Icon::PictureInPicture2)
                    .fit_content(44.0)
                    .on_effect(move || {
                        d.update(|s| s.stopwatch.pinned_overlay = !s.stopwatch.pinned_overlay);
                    }),
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

fn surface(
    mode: &ui::LocalState<usize>,
    state: &ui::LocalState<ClockState>,
    notice: &ui::LocalState<Option<(String, String)>>,
    validation_error: &ui::LocalState<Option<String>>,
) -> Element {
    let select = mode.clone();
    let st = state.clone();
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
    let page = ModelView::from_state(mode, move |i| match i {
        0 => world_screen(&st),
        1 => alarms_screen(&st),
        3 => stopwatch_screen(&st),
        _ => timers_screen(&st, &timer_validation_error),
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

pub struct ClocksApp {
    inner: Element,
    state: ui::LocalState<ClockState>,
    pub(crate) actions: crate::resource::ActionSink,
    hosted: bool,
    generation: u64,
    last_saved: String,
    last_notification: Option<(uuid::Uuid, String, String)>,
    notice: ui::LocalState<Option<(String, String)>>,
    persistent: bool,
    next_clock_tick: ui::time::Instant,
    /// The open section: World, Alarms, Timers, Stopwatch.
    mode: ui::LocalState<usize>,
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
impl ClocksApp {
    #[must_use]
    pub fn new() -> Self {
        let value = ClockState::load();
        // Reopen on the activity that most likely brought the user back,
        // while a quiet first launch begins with the useful at-a-glance view.
        let mode = if !value.timers.is_empty() {
            2
        } else if value.stopwatch.started_at_utc.is_some() || value.stopwatch.accumulated_ms > 0 {
            3
        } else {
            usize::from(!value.alarms.is_empty())
        };
        Self::with_state(value, mode, true)
    }
    #[must_use]
    pub fn running_demo() -> Self {
        Self::scenario("running")
    }
    #[must_use]
    pub fn scenario(name: &str) -> Self {
        Self::with_mode(name, false)
    }

    fn with_mode(name: &str, persistent: bool) -> Self {
        let mode = match name {
            "world" => 0,
            "alarms" => 1,
            "stopwatch" => 3,
            _ => 2,
        };
        let mut value = if persistent {
            ClockState::load()
        } else {
            ClockState::default()
        };
        if name == "running" {
            let mut t = Timer::new("Tea", Duration::from_secs(272));
            t.start(Utc::now());
            t.pinned_overlay = true;
            value.timers.push(t);
        }
        let mut app = Self::with_state(value, mode, persistent);
        if name == "ringing" {
            app.remember_due(DueEvent::Timer {
                id: Uuid::new_v4(),
                label: "Tea".into(),
                native_scheduled: false,
            });
        }
        app
    }

    fn with_state(value: ClockState, mode: usize, persistent: bool) -> Self {
        // Register earlier timers before the first UI event can delete them.
        let mut value = value;
        value
            .native_timer_owners
            .extend(value.timers.iter().map(|timer| timer.id));
        let state = ui::LocalState::new(value);
        let notice = ui::LocalState::new(None);
        let validation_error = ui::LocalState::new(None);
        let mode = ui::LocalState::new(mode);
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
            inner: surface(&mode, &state, &notice, &validation_error),
            state,
            actions: crate::resource::ActionSink::default(),
            hosted: false,
            generation: 0,
            last_saved: String::new(),
            last_notification: None,
            notice,
            persistent,
            next_clock_tick: ui::time::now(),
            mode,
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
                .map(|(_, title, body)| (title.clone(), body.clone())),
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
                TIMERS_URI => self.mode.set(2),
                ALARMS_URI => self.mode.set(1),
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
                t.start(Utc::now());
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
                        t.pause(Utc::now());
                    } else {
                        t.start(Utc::now());
                    }
                }
            }),
            crate::actions::ClockOperation::AddMinute { id } => self.state.update(|s| {
                if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                    let left = t
                        .remaining(Utc::now())
                        .saturating_add(Duration::from_secs(60));
                    t.paused_remaining_ms = Some(left.as_millis().try_into().unwrap_or(u64::MAX));
                    t.ends_at_utc = None;
                    t.start(Utc::now());
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
                    s.stopwatch.pause(Utc::now());
                } else {
                    s.stopwatch.start(Utc::now());
                }
            }),
            crate::actions::ClockOperation::ResetStopwatch => {
                self.state.update(|s| s.stopwatch.reset());
            }
            crate::actions::ClockOperation::Lap => {
                self.state.update(|s| s.stopwatch.lap(Utc::now()));
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
        let now = Utc::now();
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
            .update(|s| due = s.tick(now, chrono::Local::now()));
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

impl ClocksApp {
    /// Framework [`ResourceSurface::attach`] marks the surface hosted so
    /// `resources` does not republish `clocks.collection` beside ResourceRoot.
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
        ClocksApp, DueEvent, Timer, WEEKDAYS, alarm_label, custom_timer_duration_seconds,
        remaining_fraction, repeat_summary, timer_label, timezone_label, timezone_results,
    };
    use crate::ShellSnapshot;
    use chrono::Utc;
    use std::collections::BTreeSet;
    use std::time::Duration;
    use ui::{State, headless::FrameHarness, theme::Theme};

    #[test]
    fn compact_and_large_text_layouts_keep_full_disjoint_touch_targets() {
        // Case: the shared mode switcher keeps full touch targets on compact windows.
        fn find_toggle(node: &ui::headless::InspectionNode) -> Option<[f64; 4]> {
            if node.widget == "ToggleGroup" {
                return Some(node.rect);
            }
            node.children.iter().find_map(find_toggle)
        }

        for width in [360_u32, 520_u32] {
            let harness = FrameHarness::new(
                State::new(),
                Box::new(ClocksApp::scenario("timers")),
                (width, 820),
            );
            let [x, _y, nav_width, nav_height] =
                find_toggle(&harness.inspection().root).expect("shared segmented navigation");
            assert!(
                nav_height >= 44.0,
                "width {width}: tab targets became too short: {nav_height}"
            );
            assert!(
                x >= 0.0 && x + nav_width <= f64::from(width),
                "width {width}: switcher fits the window"
            );

            for label in ["World", "Alarms", "Timers", "Stopwatch"] {
                assert!(
                    harness.a11y_node_named(label).is_some(),
                    "width {width}: {label} must remain independently discoverable"
                );
            }
        }

        // Case: large-text custom duration controls have disjoint geometry.
        let harness = FrameHarness::new_with_theme(
            State::new(),
            Box::new(ClocksApp::scenario("timers")),
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

        // Case: world clock search covers the timezone catalogue without jargon.
        assert_eq!(
            timezone_label("America/Argentina/Buenos_Aires"),
            "Buenos Aires · America",
            "case timezone-label"
        );
        assert!(
            timezone_results("reykjavik")
                .iter()
                .any(|zone| zone == "Atlantic/Reykjavik"),
            "case search-reykjavik"
        );
        assert!(
            timezone_results("new york")
                .iter()
                .any(|zone| zone == "America/New_York"),
            "case search-new-york"
        );
        assert!(timezone_results("").len() >= 6, "case search-empty");
        assert!(timezone_results("a").len() <= 8, "case search-bounded");
        assert!(
            timezone_results("a")
                .iter()
                .all(|zone| !zone.starts_with("Etc/")),
            "case search-no-etc"
        );
    }
}
