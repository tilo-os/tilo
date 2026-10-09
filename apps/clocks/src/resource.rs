//! Clocks as a framework app: the [`Clocks`] resource, the window that
//! shows it, the effects that change only this device, and what the window
//! adds to the published tree.

use std::rc::Rc;

use chrono::Utc;
use ui::framework::{EffectAnswer, EffectCx, Effected};
use ui::prelude::*;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::items::item_resource_id;

use crate::app::{ClocksApp, Controls, Shown, duration, repeat_summary};
use crate::model::Clocks;
use crate::schedule::OnThisDevice;

/// What changes only this device: which clocks float above other windows
/// on its screen.
#[derive(Clone, Debug, Effect)]
#[effect(operation = "clocks.effects", device_only)]
pub enum ClocksEffect {
    /// Open this alarm on this device.
    OpenAlarm {
        #[param("The alarm's id")]
        alarm: String,
    },
    /// Dismiss the due alarm or timer notice on this device.
    DismissNotice,
    /// Float a timer above other windows.
    FloatTimer {
        #[param("The timer's id")]
        timer: String,
        #[param("Float it")]
        floating: bool,
    },
    /// Float the stopwatch above other windows.
    FloatStopwatch {
        #[param("Float it")]
        floating: bool,
    },
}

/// The Clocks app.
pub struct ClocksShell;

/// Every `--shots` scenario; a `-narrow` one is the same scene in a phone
/// window.
const SCENARIOS: &[&str] = &[
    "empty",
    "running",
    "ringing",
    "world",
    "alarms",
    "stopwatch",
    "timers",
    "running-narrow",
    "ringing-narrow",
    "world-narrow",
    "alarms-narrow",
    "stopwatch-narrow",
    "timers-narrow",
];

/// The moment a capture's wall clocks show, the same on every run.
pub(crate) struct ShotNow(pub(crate) chrono::DateTime<Utc>);

/// A Tuesday morning: the time a capture's clocks read.
fn shot_now() -> chrono::DateTime<Utc> {
    chrono::DateTime::from_timestamp(1_782_806_400, 0).unwrap_or_default()
}

/// The scene a scenario shows, without its window size.
fn scene(name: Option<&str>) -> &str {
    let name = name.unwrap_or_default();
    name.strip_suffix("-narrow").unwrap_or(name)
}

impl ResourceApp for ClocksShell {
    type Resource = Clocks;
    const ID: &'static str = "so.tilo.clocks";
    const TITLE: &'static str = "Clocks";
    const ICON: Option<&'static str> = Some("clocks");
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Clocks>) -> Element {
        crate::locales::install();
        let controls = Controls {
            actions: cx.actions(),
            dispatcher: cx.dispatcher(),
        };
        cx.surface(ClocksApp::new(
            controls,
            cx.settings::<OnThisDevice>(),
            cx.mode().is_live(),
        ))
    }

    fn launch_record(input: &LaunchInput<'_>) -> Option<ui::record::RecordId> {
        let uri = input.arguments().first()?;
        let path = uri.strip_prefix("tilo-clocks:record/")?;
        let record = path
            .split_once("/alarm/")
            .map_or(path, |(record, _)| record);
        ui::record::RecordId::parse(record)
    }

    fn launch(input: &LaunchInput<'_>, _created: bool) -> Vec<Invoke<Clocks>> {
        input
            .arguments()
            .first()
            .and_then(|uri| crate::native_timers::activation_alarm(uri))
            .map(|alarm| {
                vec![Invoke::effect(&ClocksEffect::OpenAlarm {
                    alarm: alarm.to_owned(),
                })]
            })
            .unwrap_or_default()
    }

    fn scenarios() -> &'static [&'static str] {
        SCENARIOS
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        if scenario.is_some_and(|name| name.ends_with("-narrow")) {
            (520, 820)
        } else {
            (1280, 800)
        }
    }

    fn fixture(fixture: &mut ui::framework::Fixture<Clocks>) {
        fixture.provide(ShotNow(shot_now()));
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Clocks>> {
        use crate::model::WeekdayKey::{Fri, Mon, Thu, Tue, Wed};
        match scene(name) {
            "running" => vec![Clocks::ADD_TIMER.with(("Tea".to_owned(), 272))],
            "world" => [
                ("Tokyo", "Asia/Tokyo"),
                ("London", "Europe/London"),
                ("New York", "America/New_York"),
            ]
            .into_iter()
            .map(|(city, zone)| Clocks::ADD_WORLD_CLOCK.with((city.to_owned(), zone.to_owned())))
            .collect(),
            "alarms" => vec![
                Clocks::ADD_ALARM.with(("Wake up".to_owned(), 7, 0, vec![Mon, Tue, Wed, Thu, Fri])),
                Clocks::ADD_ALARM.with(("Gym".to_owned(), 18, 30, Vec::new())),
            ],
            _ => Vec::new(),
        }
    }
}

impl ResourceSurface<Clocks, ClocksEffect> for ClocksApp {
    fn present(&mut self, state: &Rc<Clocks>) {
        self.show(state);
    }

    fn attach(&mut self, cx: ui::framework::SurfaceCx<Clocks>) {
        self.record = cx.record().cloned();
        self.opener = cx.opener();
        if let Some(now) = cx.fixture::<ShotNow>() {
            self.state.update(|shown| shown.pinned_now = Some(now.0));
        }
    }

    fn stage(&mut self, scenario: Option<&str>) {
        match scene(scenario) {
            "world" => self.set_mode(crate::app::WORLD),
            "alarms" => self.set_mode(crate::app::ALARMS),
            "stopwatch" => self.set_mode(crate::app::STOPWATCH),
            "running" => {
                self.set_mode(crate::app::TIMERS);
                let timers: Vec<String> = self.state.with(|shown| {
                    shown
                        .clocks
                        .timers
                        .ids()
                        .map(|id| id.as_str().to_owned())
                        .collect()
                });
                self.update_device(|device| device.floating_timers.extend(timers));
            }
            "ringing" => {
                self.set_mode(crate::app::TIMERS);
                self.notice
                    .set(Some(("Timer finished".to_owned(), "Tea".to_owned())));
            }
            _ => self.set_mode(crate::app::TIMERS),
        }
    }

    fn effect(&mut self, effect: ClocksEffect, _cx: &mut EffectCx<Clocks, Self>) -> EffectAnswer {
        match effect {
            ClocksEffect::OpenAlarm { alarm } => {
                let id = Id::from_raw(alarm.as_str());
                if !self.state.with(|shown| shown.clocks.alarms.contains(&id)) {
                    return Err(format!("there is no alarm `{alarm}`"));
                }
                self.state.update(|shown| shown.selected_alarm = Some(id));
                self.set_mode(crate::app::ALARMS);
            }
            ClocksEffect::DismissNotice => self.notice.set(None),
            ClocksEffect::FloatTimer { timer, floating } => {
                if !self
                    .state
                    .with(|shown| shown.clocks.timers.contains(&Id::from_raw(timer.as_str())))
                {
                    return Err(format!("there is no timer `{timer}`"));
                }
                self.update_device(|device| {
                    if floating {
                        device.floating_timers.insert(timer);
                    } else {
                        device.floating_timers.remove(&timer);
                    }
                });
            }
            ClocksEffect::FloatStopwatch { floating } => {
                self.update_device(|device| device.floating_stopwatch = floating);
            }
        }
        Ok(Effected::done())
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.state.with(live_rows)
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// How each clock stands right now, published on its item for the palette
/// and agents: what a timer has left and whether it floats here, when an
/// alarm rings, where a city is. Floating a timer is an operation of its
/// row; the stopwatch has a row of its own.
fn live_rows(shown: &Shown) -> Vec<LiveRow> {
    let now = Utc::now();
    let clocks = &shown.clocks;
    let mut rows: Vec<LiveRow> = clocks
        .timers
        .iter()
        .map(|(id, timer)| {
            timer_row(
                id,
                timer,
                shown.device.floating_timers.contains(id.as_str()),
                now,
            )
        })
        .collect();
    rows.extend(clocks.alarms.iter().map(|(id, alarm)| {
        LiveRow::new(
            item_resource_id::<Clocks>("alarms", id),
            "clocks.alarm",
            &alarm.label,
        )
        .presentation(ResourcePresentation {
            detail: Some(format!(
                "At {0}, repeating {1}.",
                alarm.local_time().format("%H:%M"),
                repeat_summary(&alarm.weekdays())
            )),
            badge: Some(if alarm.enabled { "On" } else { "Off" }.into()),
            preview: None,
        })
        .effect(&ClocksEffect::OpenAlarm {
            alarm: id.as_str().to_owned(),
        })
    }));
    rows.extend(clocks.world_clocks.iter().map(|(id, clock)| {
        LiveRow::new(
            item_resource_id::<Clocks>("world_clocks", id),
            "clocks.world-clock",
            &clock.city,
        )
        .presentation(ResourcePresentation {
            detail: Some(clock.timezone.0.clone()),
            badge: None,
            preview: None,
        })
    }));
    let floating = shown.device.floating_stopwatch;
    rows.push(
        LiveRow::new("clocks.stopwatch", "clocks.stopwatch", "Stopwatch")
            .presentation(ResourcePresentation {
                detail: Some(duration(millis(clocks.stopwatch.elapsed(now)))),
                badge: floating.then(|| "Floating".into()),
                preview: None,
            })
            .effect(&ClocksEffect::FloatStopwatch {
                floating: !floating,
            }),
    );
    rows
}

fn timer_row(
    id: &Id,
    timer: &crate::model::Timer,
    floating: bool,
    now: chrono::DateTime<Utc>,
) -> LiveRow {
    let status = if timer.is_running(now) {
        "Running"
    } else if timer.is_finished(now) {
        "Done"
    } else {
        "Paused"
    };
    let remaining = duration(millis(timer.remaining(now)));
    LiveRow::new(
        item_resource_id::<Clocks>("timers", id),
        "clocks.timer",
        &timer.label,
    )
    .presentation(ResourcePresentation {
        detail: Some(format!("{status}, with {remaining} remaining.")),
        badge: floating.then(|| "Floating".into()),
        preview: None,
    })
    .effect(&ClocksEffect::FloatTimer {
        timer: id.as_str().to_owned(),
        floating: !floating,
    })
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;
