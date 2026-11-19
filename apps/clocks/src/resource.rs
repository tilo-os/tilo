//! Clocks as a framework app: the [`Clocks`] resource, the window that
//! shows it, the effects that change only this device, and what the window
//! adds to the published tree.

use ui::framework::{EffectAnswer, Effected, Publishes};
use ui::prelude::*;
use ui::tilo_actions::ResourcePresentation;
use ui::tilo_resource::Id;

use crate::app::{ClocksApp, Controls, Shown, Store, duration, repeat_summary, wall_now};
use crate::model::Clocks;
use crate::schedule::DeviceChoices;

pub use clocks_model::ClocksEffect;
use clocks_model::StopwatchRow;

pub struct ClocksShell;

/// Every `--shots` scenario; a `-narrow` one is the same scene in a phone
/// window.
const SCENARIOS: &[&str] = &[
    "empty",
    "running",
    "ringing",
    "world",
    "alarms",
    "ringing-alarm",
    "stopwatch",
    "timers",
    "running-narrow",
    "ringing-narrow",
    "world-narrow",
    "alarms-narrow",
    "ringing-alarm-narrow",
    "stopwatch-narrow",
    "timers-narrow",
];

pub(crate) struct ShotNow(pub(crate) jiff::Timestamp);

fn shot_now() -> jiff::Timestamp {
    jiff::Timestamp::from_second(1_782_806_400)
        .ok()
        .unwrap_or_default()
}

fn scene(name: Option<&str>) -> &str {
    let name = name.unwrap_or_default();
    name.strip_suffix("-narrow").unwrap_or(name)
}

/// What the binary runs instead of opening a window: `--probe-timezone-engine`
/// runs the time-zone database's self-check, which proves Clocks loads the
/// engine library and runs it. It needs `runtime-engine`.
const COMMANDS: &[ui::framework::Command] = &[ui::framework::Command {
    option: "--probe-timezone-engine",
    run: probe_timezone_engine,
}];

#[cfg(feature = "runtime-engine")]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    let report = tilo_timezone_engine::self_check()?;
    println!("clocks --probe-timezone-engine: {report}");
    Ok(())
}

#[cfg(not(feature = "runtime-engine"))]
fn probe_timezone_engine(_: &str) -> Result<(), String> {
    Err("the time-zone engine library needs the `runtime-engine` feature".to_string())
}

impl ResourceApp for ClocksShell {
    type Resource = Clocks;
    const ID: &'static str = ui::apps::CLOCKS;
    const TITLE: &'static str = "Clocks";
    const TITLE_KEY: &'static str = "clocks-app-title";

    fn commands() -> &'static [ui::framework::Command] {
        COMMANDS
    }

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("clocks");
    const ARGUMENTS: usize = 1;

    fn view(cx: &mut ViewCx<Clocks>) -> Element {
        let controls = Controls {
            actions: cx.actions(),
            dispatcher: cx.dispatcher(),
        };
        let app = ClocksApp::new(
            controls,
            DeviceChoices::new(cx),
            cx.live().clone(),
            cx.mode().is_live(),
            (cx.document_id().cloned(), cx.opener()),
        );
        let store = app.store.clone();
        if let Some(now) = cx.fixture::<ShotNow>() {
            store.state.update(|shown| shown.pinned_now = Some(now.0));
        }
        {
            let store = store.clone();
            cx.effects(move |effect: ClocksEffect, cx| {
                store.present(&cx.live().get());
                store.answer(effect)
            });
        }
        {
            let (store, live) = (store.clone(), cx.live().clone());
            cx.on_stage(move |scenario| {
                store.present(&live.get());
                store.stage(scenario);
            });
        }
        let (rows, items, states) = (store.clone(), store.clone(), store);
        cx.publishing(
            Publishes::new()
                // Each clock's item says how it stands right now, and the
                // stopwatch, which is no item, is one row of its own.
                .item_presentation(move |field, id| {
                    items
                        .state
                        .with(|shown| item_presentation(shown, field, id))
                })
                .live_rows(move || vec![rows.state.with(stopwatch_row)])
                // A clock floats or it does not: its row offers that change.
                .row_status(move |id| states.state.with(|shown| row_status(shown, id))),
            Box::new(app),
        )
    }

    fn launch_resource(input: &LaunchInput<'_>) -> Option<ui::resource::DocumentId> {
        crate::native_timers::activation_resource(input.arguments().first()?)
    }

    /// An alarm's link (`tilo-clocks:alarm/<id>`, as its notice names it) opens that alarm.
    fn open_link(link: &str) -> Option<Vec<Invoke<Clocks>>> {
        let alarm = crate::native_timers::activation_alarm(link)?;
        Some(vec![Invoke::effect(&ClocksEffect::OpenAlarm {
            alarm: alarm.to_owned(),
        })])
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
            "ringing-alarm" => vec![Clocks::ADD_ALARM.with((
                "Wake up".to_owned(),
                7,
                0,
                vec![Mon, Tue, Wed, Thu, Fri],
            ))],
            _ => Vec::new(),
        }
    }
}

impl Store {
    pub(crate) fn stage(&self, scenario: Option<&str>) {
        match scene(scenario) {
            "world" => self.set_mode(crate::app::WORLD),
            "alarms" => self.set_mode(crate::app::ALARMS),
            "ringing-alarm" => {
                self.set_mode(crate::app::ALARMS);
                let alarm = self
                    .state
                    .with(|shown| shown.clocks.alarms.ids().next().cloned());
                self.notice
                    .set(Some(("Alarm".to_owned(), "Wake up".to_owned(), alarm)));
            }
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
                    .set(Some(("Timer finished".to_owned(), "Tea".to_owned(), None)));
            }
            _ => self.set_mode(crate::app::TIMERS),
        }
    }

    fn answer(&self, effect: ClocksEffect) -> EffectAnswer {
        match effect {
            ClocksEffect::OpenAlarm { alarm } => {
                let id = Id::from_raw(alarm.as_str());
                if !self.state.with(|shown| shown.clocks.alarms.contains(&id)) {
                    return Err(format!("there is no alarm `{alarm}`"));
                }
                self.state.update(|shown| shown.selected_alarm = Some(id));
                self.set_mode(crate::app::ALARMS);
            }
            ClocksEffect::Show { section } => self.set_mode(match section.as_str() {
                "world" => crate::app::WORLD,
                "alarms" => crate::app::ALARMS,
                "timers" => crate::app::TIMERS,
                "stopwatch" => crate::app::STOPWATCH,
                other => {
                    return Err(format!(
                        "there is no section `{other}`: world, alarms, timers or stopwatch"
                    ));
                }
            }),
            ClocksEffect::DismissNotice => self.notice.set(None),
            ClocksEffect::SnoozeAlarm { alarm, minutes } => {
                let id = Id::from_raw(alarm.as_str());
                let now_ms = wall_now().as_millisecond();
                let mut delivery = self.device.get().delivery;
                self.state.with(|shown| {
                    let alarm = shown
                        .clocks
                        .alarms
                        .get(&id)
                        .ok_or_else(|| format!("there is no alarm `{alarm}`"))?;
                    crate::schedule::snooze(&mut delivery, &id, alarm, now_ms, minutes)
                })?;
                self.update_device(|device| device.delivery = delivery);
                self.notice.set(None);
            }
            ClocksEffect::FloatTimer { timer } => {
                if !self
                    .state
                    .with(|shown| shown.clocks.timers.contains(&Id::from_raw(timer.as_str())))
                {
                    return Err(format!("there is no timer `{timer}`"));
                }
                self.update_device(|device| {
                    device.floating_timers.insert(timer);
                });
            }
            ClocksEffect::UnfloatTimer { timer } => {
                if !self
                    .state
                    .with(|shown| shown.clocks.timers.contains(&Id::from_raw(timer.as_str())))
                {
                    return Err(format!("there is no timer `{timer}`"));
                }
                self.update_device(|device| {
                    device.floating_timers.remove(&timer);
                });
            }
            ClocksEffect::FloatStopwatch => {
                self.update_device(|device| device.floating_stopwatch = true);
            }
            ClocksEffect::UnfloatStopwatch => {
                self.update_device(|device| device.floating_stopwatch = false);
            }
        }
        Ok(Effected::done())
    }
}

fn millis(duration: std::time::Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

/// How the clock `id` of `field` stands right now, beside its item for the
/// palette and agents: what its document does not hold.
fn item_presentation(shown: &Shown, field: &str, id: &str) -> Option<ResourcePresentation> {
    let now = crate::app::wall_now();
    let clocks = &shown.clocks;
    match field {
        "timers" => {
            let timer = clocks.timers.get(&Id::from_raw(id))?;
            let status = if timer.is_running(now) {
                "Running"
            } else if timer.is_finished(now) {
                "Done"
            } else {
                "Paused"
            };
            let remaining = duration(millis(timer.remaining(now)));
            Some(ResourcePresentation {
                detail: Some(format!("{status}, with {remaining} remaining.")),
                badge: shown
                    .device
                    .floating_timers
                    .contains(id)
                    .then(|| "Floating".into()),
                preview: None,
            })
        }
        "alarms" => {
            let alarm = clocks.alarms.get(&Id::from_raw(id))?;
            Some(ResourcePresentation {
                detail: Some(format!(
                    "At {0}, repeating {1}.",
                    ui::clock::time_of_day(alarm.local_time()),
                    repeat_summary(&alarm.weekdays())
                )),
                badge: Some(if alarm.enabled { "On" } else { "Off" }.into()),
                preview: None,
            })
        }
        "world_clocks" => {
            let clock = clocks.world_clocks.get(&Id::from_raw(id))?;
            Some(ResourcePresentation {
                detail: Some(clock.timezone.0.clone()),
                badge: None,
                preview: None,
            })
        }
        _ => None,
    }
}

/// The stopwatch, the one clock that is no document item: one row of the
/// `clocks.stopwatch` type, which offers floating it.
fn stopwatch_row(shown: &Shown) -> LiveRow {
    LiveRow::typed::<StopwatchRow>("clocks.stopwatch", "Stopwatch").presentation(
        ResourcePresentation {
            detail: Some(duration(millis(
                shown.clocks.stopwatch.elapsed(crate::app::wall_now()),
            ))),
            badge: shown.device.floating_stopwatch.then(|| "Floating".into()),
            preview: None,
        },
    )
}

/// What a timer or the stopwatch is like now on this device: it floats or
/// it does not, so its row offers only the change that applies.
fn row_status(shown: &Shown, id: &str) -> Vec<ui::framework::EffectStatus> {
    use ui::framework::EffectStatus;
    const FLOATS: &str = "It already floats above other windows";
    const SET: &str = "It is not floating above other windows";
    if id == "clocks.stopwatch" {
        return if shown.device.floating_stopwatch {
            vec![EffectStatus::unavailable(
                ClocksEffect::FLOAT_STOPWATCH,
                "already-floating",
                FLOATS,
            )]
        } else {
            vec![EffectStatus::unavailable(
                ClocksEffect::UNFLOAT_STOPWATCH,
                "not-floating",
                SET,
            )]
        };
    }
    let Some((field, timer)) = ui::tilo_resource::items::item_of_resource::<Clocks>(id) else {
        return Vec::new();
    };
    if field != "timers" {
        return Vec::new();
    }
    if shown.device.floating_timers.contains(timer.as_str()) {
        vec![EffectStatus::unavailable(
            ClocksEffect::FLOAT_TIMER,
            "already-floating",
            FLOATS,
        )]
    } else {
        vec![EffectStatus::unavailable(
            ClocksEffect::UNFLOAT_TIMER,
            "not-floating",
            SET,
        )]
    }
}

#[cfg(test)]
#[path = "resource_tests.rs"]
mod tests;

ui::reach_tests!(ClocksShell);
