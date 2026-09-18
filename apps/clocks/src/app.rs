//! Responsive Clocks UI. All controls mutate the durable engine state directly.
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
use ui::widget::{
    Button, Card, Column, Container, Element, ModelView, ProgressBar, Row, Scrollable, Spacer,
    Text, TextInput, TextInputKind, TimePicker, TimeValue, ToggleGroup, Widget,
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
    (remaining_ms as f32 / duration_ms as f32).clamp(0.0, 1.0)
}

fn timer_label(input: &str) -> String {
    let label = input.trim();
    if label.is_empty() {
        "Timer".into()
    } else {
        label.chars().take(80).collect()
    }
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
    zone.parse::<chrono_tz::Tz>()
        .map(|z| {
            Utc::now()
                .with_timezone(&z)
                .format("%H:%M · %a %-d %b · %Z · UTC%:z")
                .to_string()
        })
        .unwrap_or_else(|_| "Time unavailable".into())
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
            .with_gap(14.0)
            .push(Box::new(
                Text::new("World clocks")
                    .with_size(24.0)
                    .with_heading_level(2),
            ))
            .push(Box::new(Card::titled(
                "Local time",
                Box::new(
                    Text::new(&chrono::Local::now().format("%H:%M, %A, %-d %B").to_string())
                        .with_size(32.0),
                ),
            )))
            .push(Box::new(list))
            .push(Box::new(Text::new("Add a city").with_heading_level(3)))
            .push(Box::new(
                TextInput::from_state(&city_query)
                    .with_id("clocks.world.search")
                    .with_kind(TextInputKind::Search)
                    .with_placeholder("Search cities or time zones"),
            ))
            .push(Box::new(choices)),
    )
}

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
                Text::new(
                    "No alarms. Create one and it will ring while Clocks is running, even when its window is hidden.",
                )
                .dimmed(),
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
                                a.enabled = !a.enabled;
                            }
                        })
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
            .with_gap(14.0)
            .push(Box::new(
                Text::new("Alarms").with_size(24.0).with_heading_level(2),
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
                        .push(Box::new(
                            TimePicker::from_state(&draft)
                                .with_id("clocks.alarm.time")
                                .with_accessible_name("Alarm time"),
                        ))
                        .push(Box::new(Text::new("Repeat").with_heading_level(3)))
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
                                })
                            });
                            add_label.set(String::new());
                            add_repeat.set(BTreeSet::new());
                        })),
                ),
            ))),
    )
}

fn timers_screen(
    state: &ui::LocalState<ClockState>,
    validation_error: &ui::LocalState<Option<String>>,
) -> Element {
    let custom = ui::LocalState::new(TimeValue::new(0, 10).with_seconds(0));
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
            col = col.push(Box::new(
                Text::new("No timers running. Choose a preset or set an exact duration.").dimmed(),
            ));
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
                                .push(Box::new(Text::new(&duration(remaining_ms)).with_size(48.0)))
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
                                        })
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
                                                    t.pause(Utc::now())
                                                } else {
                                                    t.start(Utc::now())
                                                }
                                            }
                                        })
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
                                        })
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
        Box::new(col)
    });
    let add_state = state.clone();
    let add_custom = custom.clone();
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
    Box::new(
        Column::new()
            .with_gap(18.0)
            .push(Box::new(
                Text::new("Timers").with_size(28.0).with_heading_level(2),
            ))
            .push(Box::new(
                Text::new("Start quickly, or dial in an exact duration.").dimmed(),
            ))
            .push(Box::new(Text::new("Quick starts").with_heading_level(3)))
            .push(Box::new(presets))
            .push(Box::new(list))
            .push(Box::new(Card::titled(
                "Custom timer",
                Box::new(
                    Column::new()
                        .with_gap(12.0)
                        .push(Box::new(
                            TextInput::from_state(&custom_label)
                                .with_id("clocks.timer.custom.label")
                                .with_kind(TextInputKind::Name)
                                .with_placeholder("What is this timer for?"),
                        ))
                        .push(Box::new(Text::new("Hours, minutes, seconds").dimmed()))
                        .push(Box::new(
                            TimePicker::from_state(&custom)
                                .with_id("clocks.timer.custom.duration")
                                .with_accessible_name("Timer duration"),
                        ))
                        .push(primary_button(
                            "clocks.timer.custom.add",
                            "Start timer",
                            move || {
                                let v = add_custom.get();
                                let secs = u64::from(v.hours) * 3600
                                    + u64::from(v.minutes) * 60
                                    + u64::from(v.seconds.unwrap_or(0));
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
            ))),
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
                i + 1,
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
                                        s.stopwatch.pause(Utc::now())
                                    } else {
                                        s.stopwatch.start(Utc::now())
                                    }
                                })
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
                        d.update(|s| s.stopwatch.pinned_overlay = !s.stopwatch.pinned_overlay)
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
    .with_style(ui::widget::toggle_group::SegmentedStyle::Underline)
    .with_id("clocks.mode")
    .on_view("clocks.mode.select", move |i| select.set(i));
    let nav = Row::new()
        .push(Box::new(Spacer::new()))
        .push(Box::new(
            Container::new()
                .with_no_padding()
                .with_max_width(620.0)
                .filling()
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
        .with_gap(4.0)
        .push(Box::new(
            Text::new("Clocks").with_size(34.0).with_heading_level(1),
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
                    .with_max_width(880.0)
                    .filling()
                    .with_child(Box::new(content)),
            ))
            .push(Box::new(Spacer::new())),
    )
}

pub struct ClocksApp {
    inner: Element,
    state: ui::LocalState<ClockState>,
    generation: u64,
    last_saved: String,
    last_notification: Option<(uuid::Uuid, String, String)>,
    notice: ui::LocalState<Option<(String, String)>>,
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
        } else if !value.alarms.is_empty() {
            1
        } else {
            0
        };
        Self::with_state(value, mode)
    }
    #[must_use]
    pub fn running_demo() -> Self {
        Self::scenario("running")
    }
    #[must_use]
    pub fn scenario(name: &str) -> Self {
        let mode = match name {
            "world" => 0,
            "alarms" => 1,
            "stopwatch" => 3,
            _ => 2,
        };
        let mut value = if matches!(
            name,
            "empty" | "running" | "ringing" | "world" | "alarms" | "stopwatch" | "timers"
        ) {
            ClockState::default()
        } else {
            ClockState::load()
        };
        if name == "running" {
            let mut t = Timer::new("Tea", Duration::from_secs(272));
            t.start(Utc::now());
            t.pinned_overlay = true;
            value.timers.push(t);
        }
        let mut app = Self::with_state(value, mode);
        if name == "ringing" {
            app.remember_due(DueEvent::Timer {
                id: Uuid::new_v4(),
                label: "Tea".into(),
            });
        }
        app
    }

    fn with_state(value: ClockState, mode: usize) -> Self {
        let state = ui::LocalState::new(value);
        let notice = ui::LocalState::new(None);
        let validation_error = ui::LocalState::new(None);
        Self {
            inner: surface(
                &ui::LocalState::new(mode),
                &state,
                &notice,
                &validation_error,
            ),
            state,
            generation: 0,
            last_saved: String::new(),
            last_notification: None,
            notice,
        }
    }

    /// Retain the latest due notice in every subsequent transport snapshot.
    ///
    /// The shell polls a replace-in-place state file, so a one-frame zero
    /// countdown can be overwritten by the inactive tombstone before the
    /// shell observes it. A stable notification ID lets the producer keep the
    /// notice attached to later tombstones while the shell deduplicates it.
    fn remember_due(&mut self, event: DueEvent) {
        let (title, body) = match event {
            DueEvent::Alarm { label, .. } => ("Alarm".to_owned(), label),
            DueEvent::Timer { label, .. } => ("Timer finished".to_owned(), label),
        };
        self.last_notification = Some((uuid::Uuid::new_v4(), title, body));
        self.notice.set(
            self.last_notification
                .as_ref()
                .map(|(_, title, body)| (title.clone(), body.clone())),
        );
    }
}
impl Default for ClocksApp {
    fn default() -> Self {
        Self::new()
    }
}
impl Widget for ClocksApp {
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }
    fn invoke_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        ty: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<bool, String> {
        if ty != crate::actions::OPERATION_TYPE {
            return self
                .inner
                .invoke_resource_operation(kind, ty, payload, doc, path);
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
                })
            }
            crate::actions::ClockOperation::ToggleTimer { id } => self.state.update(|s| {
                if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                    if t.ends_at_utc.is_some() {
                        t.pause(Utc::now())
                    } else {
                        t.start(Utc::now())
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
                    t.start(Utc::now())
                }
            }),
            crate::actions::ClockOperation::SetTimerOverlay { id, pinned } => {
                self.state.update(|s| {
                    if let Some(t) = s.timers.iter_mut().find(|t| t.id == id) {
                        t.pinned_overlay = pinned;
                    }
                })
            }
            crate::actions::ClockOperation::RemoveTimer { id } => {
                self.state.update(|s| s.timers.retain(|t| t.id != id))
            }
            crate::actions::ClockOperation::ToggleAlarm { id } => self.state.update(|s| {
                if let Some(alarm) = s.alarms.iter_mut().find(|alarm| alarm.id == id) {
                    alarm.enabled = !alarm.enabled;
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
                self.state.update(|s| s.stopwatch.reset())
            }
            crate::actions::ClockOperation::Lap => {
                self.state.update(|s| s.stopwatch.lap(Utc::now()))
            }
            crate::actions::ClockOperation::SetStopwatchOverlay { pinned } => {
                self.state.update(|s| s.stopwatch.pinned_overlay = pinned)
            }
        };
        Ok(true)
    }
    fn resources(&self, doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let _ = doc;
        self.state.with(crate::actions::provide_resources)
    }
    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
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
        self.state
            .with(|state| find(crate::actions::provide_resources(state), id))
    }
    fn sync(&mut self, d: &mut AutoCommit, p: &ObjId) {
        let now = Utc::now();
        if let Some((cmd, activity_id)) = crate::transport::take_command() {
            self.state
                .update(|state| state.apply_shell_command(&cmd, activity_id, now));
        }
        let mut due = Vec::new();
        self.state
            .update(|s| due = s.tick(now, chrono::Local::now()));
        for event in due {
            self.remember_due(event);
        }
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
                    producer_pid: std::process::id(),
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
                    producer_pid: std::process::id(),
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
            let _ = crate::transport::publish(&snapshot);
        });
        let encoded = self
            .state
            .with(|s| serde_json::to_string(s).unwrap_or_default());
        if encoded != self.last_saved {
            self.state.with(|s| {
                let _ = s.save();
            });
            self.last_saved = encoded;
        }
        self.state.update(|_| {});
        self.inner.sync(d, p)
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
        self.inner.apply_layout(t, n, o)
    }
    fn draw(
        &mut self,
        s: &mut Scene,
        tf: Affine,
        tr: &mut TextRenderer,
        th: &Theme,
        p: &PresenceCtx,
    ) {
        self.inner.draw(s, tf, tr, th, p)
    }
    fn handle_event(
        &mut self,
        e: &winit::event::WindowEvent,
        sf: f64,
        mo: &winit::event::Modifiers,
        d: &mut AutoCommit,
        p: &ObjId,
        h: &mut bool,
        f: &mut Option<NodeId>,
        n: NodeId,
        t: &TaffyTree<()>,
    ) {
        self.inner.handle_event(e, sf, mo, d, p, h, f, n, t)
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
    fn needs_idle_sync(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ClocksApp, DueEvent, Timer, WEEKDAYS, alarm_label, remaining_fraction, repeat_summary,
        timer_label, timezone_label, timezone_results,
    };
    use chrono::Utc;
    use std::collections::BTreeSet;
    use std::time::Duration;

    #[test]
    fn countdown_progress_visualizes_time_remaining() {
        assert!((remaining_fraction(300_000, 300_000) - 1.0).abs() < f32::EPSILON);
        assert!((remaining_fraction(150_000, 300_000) - 0.5).abs() < f32::EPSILON);
        assert!(remaining_fraction(0, 300_000).abs() < f32::EPSILON);
        assert!(remaining_fraction(20, 0).abs() < f32::EPSILON);
        assert!((remaining_fraction(400_000, 300_000) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn completed_timer_notice_survives_the_inactive_tombstone() {
        let now = Utc::now();
        let mut timer = Timer::new("Tea", Duration::from_secs(1));
        timer.start(now);
        let mut state = crate::ClockState::default();
        state.timers.push(timer);

        let due = state.tick(now + chrono::Duration::seconds(2), chrono::Local::now());
        assert!(
            state
                .shell_activity(now + chrono::Duration::seconds(2))
                .is_none()
        );
        assert!(matches!(due.as_slice(), [DueEvent::Timer { label, .. }] if label == "Tea"));

        let mut app = ClocksApp::scenario("empty");
        for event in due {
            app.remember_due(event);
        }
        let notice = app.last_notification.clone().expect("due notice retained");
        assert_eq!(notice.1, "Timer finished");
        assert_eq!(notice.2, "Tea");

        // Later inactive frames reuse this identity, allowing a polling shell
        // to observe it without announcing the same completion twice.
        assert_eq!(app.last_notification.as_ref(), Some(&notice));
    }

    #[test]
    fn custom_timer_labels_are_useful_bounded_and_have_a_calm_default() {
        assert_eq!(timer_label("  Pasta  "), "Pasta");
        assert_eq!(timer_label("   "), "Timer");
        assert_eq!(timer_label(&"x".repeat(120)).chars().count(), 80);
    }

    #[test]
    fn alarm_schedule_summaries_are_specific_and_labels_are_bounded() {
        assert_eq!(alarm_label("  School run "), "School run");
        assert_eq!(alarm_label(" "), "Alarm");
        let selected = [WEEKDAYS[0].0, WEEKDAYS[2].0, WEEKDAYS[4].0]
            .into_iter()
            .collect::<BTreeSet<_>>();
        assert_eq!(repeat_summary(&selected), "Mon, Wed, Fri");
        assert_eq!(repeat_summary(&BTreeSet::new()), "Once");
    }

    #[test]
    fn world_clock_search_covers_the_timezone_catalogue_without_jargon() {
        assert_eq!(
            timezone_label("America/Argentina/Buenos_Aires"),
            "Buenos Aires · America"
        );
        assert!(
            timezone_results("reykjavik")
                .iter()
                .any(|zone| zone == "Atlantic/Reykjavik")
        );
        assert!(
            timezone_results("new york")
                .iter()
                .any(|zone| zone == "America/New_York")
        );
        assert!(timezone_results("").len() >= 6);
        assert!(timezone_results("a").len() <= 8);
        assert!(
            timezone_results("a")
                .iter()
                .all(|zone| !zone.starts_with("Etc/"))
        );
    }
}
