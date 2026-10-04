#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use crate::model::{Stopwatch, WeekdayKey};
use std::time::Duration;
use ui::prelude::{Replica, TestApp};

const T0: i64 = 1_800_000_000_000;

fn replica() -> Replica<Clocks> {
    let mut replica = Replica::<Clocks>::new();
    replica.set_now_ms(T0);
    replica
}

fn only_id<T>(items: &Collection<T>) -> Id {
    items.ids().next().expect("one item").clone()
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn every_edit_is_an_action_stamped_with_its_own_time() {
    // Phase: a timer counts down to an absolute deadline taken from the
    // action's time, so it replays to the same moment on every device.
    let mut device = replica();
    let mut other = device.fork();
    device
        .apply(Clocks::ADD_TIMER.with(("Tea".into(), 300)))
        .unwrap();
    let state = device.state().unwrap();
    let id = only_id(&state.timers);
    assert_eq!(
        state.timers.get(&id).unwrap().ends_at_ms,
        Some(T0 + 300_000)
    );

    device.set_now_ms(T0 + 100_000);
    device.apply(Clocks::PAUSE_TIMER.with(id.clone())).unwrap();
    let timer = device.state().unwrap().timers.get(&id).cloned().unwrap();
    assert_eq!(timer.ends_at_ms, None, "paused: no deadline");
    assert_eq!(
        timer.paused_remaining_ms,
        Some(200_000),
        "paused: what is left"
    );

    device.set_now_ms(T0 + 500_000);
    device.apply(Clocks::RESUME_TIMER.with(id.clone())).unwrap();
    device.apply(Clocks::ADD_MINUTE.with(id.clone())).unwrap();
    let timer = device.state().unwrap().timers.get(&id).cloned().unwrap();
    assert_eq!(
        timer.ends_at_ms,
        Some(T0 + 500_000 + 260_000),
        "resumed with what was left, plus a minute"
    );

    // Phase: another device replays the same actions to the same state.
    for envelope in device.log() {
        other.replay(envelope).unwrap();
    }
    assert_eq!(other.state().unwrap(), device.state().unwrap());

    // Phase: a finished timer starts over with its full duration.
    device.set_now_ms(T0 + 2_000_000);
    device.apply(Clocks::RESUME_TIMER.with(id.clone())).unwrap();
    assert_eq!(
        device.state().unwrap().timers.get(&id).unwrap().ends_at_ms,
        Some(T0 + 2_000_000 + 300_000)
    );
    device.apply(Clocks::REMOVE_TIMER.with(id.clone())).unwrap();
    assert!(device.state().unwrap().timers.is_empty());
    assert!(
        device.apply(Clocks::REMOVE_TIMER.with(id)).is_err(),
        "a removed timer cannot be removed again"
    );

    // Phase: the stopwatch accumulates across pauses and laps are totals.
    device.set_now_ms(T0);
    device.apply(Clocks::START_STOPWATCH.invoke()).unwrap();
    device.set_now_ms(T0 + 3_000);
    device.apply(Clocks::LAP.invoke()).unwrap();
    device.set_now_ms(T0 + 8_000);
    device.apply(Clocks::LAP.invoke()).unwrap();
    device.set_now_ms(T0 + 12_000);
    device.apply(Clocks::PAUSE_STOPWATCH.invoke()).unwrap();
    device.set_now_ms(T0 + 20_000);
    device.apply(Clocks::LAP.invoke()).unwrap();
    device.apply(Clocks::START_STOPWATCH.invoke()).unwrap();
    let stopwatch = device.state().unwrap().stopwatch;
    assert_eq!(stopwatch.laps_ms, vec![3_000, 8_000], "no lap while paused");
    assert_eq!(
        stopwatch.elapsed(crate::model::instant(T0 + 25_000)),
        Duration::from_secs(17)
    );
    device.apply(Clocks::RESET_STOPWATCH.invoke()).unwrap();
    assert_eq!(device.state().unwrap().stopwatch, Stopwatch::default());

    // Phase: alarms remember their repeat days and when they were turned on.
    device.set_now_ms(T0);
    device
        .apply(Clocks::ADD_ALARM.with((
            "  School run ".into(),
            7,
            45,
            vec![WeekdayKey::Wed, WeekdayKey::Mon],
        )))
        .unwrap();
    let state = device.state().unwrap();
    let alarm_id = only_id(&state.alarms);
    let alarm = state.alarms.get(&alarm_id).unwrap();
    assert_eq!(alarm.label, "School run");
    assert_eq!(
        alarm.weekdays(),
        [WeekdayKey::Mon, WeekdayKey::Wed].into_iter().collect()
    );
    assert_eq!(alarm.enabled_at_ms, T0);
    device
        .apply(Clocks::SET_ALARM_ENABLED.with((alarm_id.clone(), false)))
        .unwrap();
    device.set_now_ms(T0 + 60_000);
    device
        .apply(Clocks::SET_ALARM_ENABLED.with((alarm_id.clone(), true)))
        .unwrap();
    let alarm = device
        .state()
        .unwrap()
        .alarms
        .get(&alarm_id)
        .cloned()
        .unwrap();
    assert!(alarm.enabled);
    assert_eq!(alarm.enabled_at_ms, T0 + 60_000, "turned on again later");

    // Phase: a city is added once, under its time zone.
    for _ in 0..2 {
        device
            .apply(Clocks::ADD_WORLD_CLOCK.with(("Tokyo".into(), "Asia/Tokyo".into())))
            .unwrap();
    }
    let state = device.state().unwrap();
    assert_eq!(
        state.world_clocks.len(),
        1,
        "a city already shown stays once"
    );
    let city = only_id(&state.world_clocks);
    device.apply(Clocks::REMOVE_WORLD_CLOCK.with(city)).unwrap();
    assert!(device.state().unwrap().world_clocks.is_empty());
}

#[test]
fn edits_that_make_no_sense_are_refused() {
    let mut device = replica();
    for (case, result) in [
        (
            "zero timer",
            device
                .apply(Clocks::ADD_TIMER.with(("Tea".into(), 0)))
                .map(drop),
        ),
        (
            "alarm hour",
            device
                .apply(Clocks::ADD_ALARM.with(("Late".into(), 24, 0, Vec::new())))
                .map(drop),
        ),
        (
            "alarm minute",
            device
                .apply(Clocks::ADD_ALARM.with(("Late".into(), 23, 60, Vec::new())))
                .map(drop),
        ),
        (
            "time zone",
            device
                .apply(Clocks::ADD_WORLD_CLOCK.with(("Nowhere".into(), "Mars/Base".into())))
                .map(drop),
        ),
        (
            "unknown timer",
            device
                .apply(Clocks::PAUSE_TIMER.with(Id::from_raw("missing")))
                .map(drop),
        ),
    ] {
        assert!(result.is_err(), "case {case}: refused");
    }
    assert_eq!(device.state().unwrap(), Clocks::default());
}

/// The published tree, flattened to `(resource, action)` pairs.
fn published(app: &mut TestApp<ClocksShell>) -> Vec<(String, String)> {
    app.agent_surface()
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["resource"].as_str().unwrap().to_owned(),
                row["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

fn find(resources: &[ui::tilo_actions::Resource], id: &str) -> Option<ui::tilo_actions::Resource> {
    resources.iter().find_map(|resource| {
        if resource.id.as_str() == id {
            Some(resource.clone())
        } else {
            find(&resource.children, id)
        }
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn one_published_tree_offers_every_clock_and_floats_timers_on_this_device_only() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.invoke(Clocks::ADD_TIMER.with(("Pasta".into(), 600)))
        .unwrap();
    app.invoke(Clocks::ADD_ALARM.with(("Wake up".into(), 7, 30, Vec::new())))
        .unwrap();
    app.invoke(Clocks::ADD_WORLD_CLOCK.with(("Tokyo".into(), "Asia/Tokyo".into())))
        .unwrap();
    app.settle();
    let state = app.state();
    let timer = item_resource_id::<Clocks>("timers", &only_id(&state.timers));
    let alarm = item_resource_id::<Clocks>("alarms", &only_id(&state.alarms));
    let city = item_resource_id::<Clocks>("world_clocks", &only_id(&state.world_clocks));

    // Phase: every capability is one published action, on the clock it
    // acts on.
    let actions = published(&mut app);
    let has =
        |resource: &str, action: &str| actions.iter().any(|(r, a)| r == resource && a == action);
    for action in [
        "add-timer",
        "add-alarm",
        "add-world-clock",
        "start-stopwatch",
        "pause-stopwatch",
        "lap",
        "reset-stopwatch",
        "float-timer",
        "float-stopwatch",
    ] {
        assert!(has(Clocks::KIND, action), "the collection offers {action}");
    }
    for action in [
        "pause-timer",
        "resume-timer",
        "add-minute",
        "remove-timer",
        "set-floating",
    ] {
        assert!(has(&timer, action), "the timer offers {action}");
    }
    for action in ["set-alarm-enabled", "remove-alarm"] {
        assert!(has(&alarm, action), "the alarm offers {action}");
    }
    assert!(has(&city, "remove-world-clock"), "the city can be removed");
    assert!(
        has("clocks.stopwatch", "set-floating"),
        "the stopwatch floats"
    );

    // Phase: each clock says how it stands right now.
    let resources = app.harness().resources();
    let row = find(&resources, &timer).expect("timer row");
    let detail = row.presentation.as_ref().and_then(|p| p.detail.clone());
    assert!(
        detail
            .as_deref()
            .is_some_and(|d| d.starts_with("Running · ")),
        "{detail:?}"
    );
    let alarm_row = find(&resources, &alarm).expect("alarm row");
    assert_eq!(
        alarm_row.presentation.and_then(|p| p.detail).as_deref(),
        Some("07:30 · Once")
    );

    // Phase: floating a timer from its row changes this device's settings,
    // not the clocks everyone shares, and the row offers the way back.
    app.call_on(&timer, "set-floating", serde_json::json!({}))
        .unwrap();
    let device = app.settings::<OnThisDevice>().get();
    assert_eq!(
        device.floating_timers.into_iter().collect::<Vec<_>>(),
        vec![timer.rsplit('/').next().unwrap().to_owned()]
    );
    assert_eq!(app.state(), state, "floating is not an edit of the clocks");
    let row = find(&app.harness().resources(), &timer).unwrap();
    assert_eq!(
        row.presentation.and_then(|p| p.badge).as_deref(),
        Some("Floating")
    );
    assert!(
        row.actions
            .iter()
            .any(|action| action.id == "set-floating" && action.label == "Stop floating")
    );
    assert!(
        app.effect(&ClocksEffect::FloatTimer {
            timer: "missing".into(),
            floating: true,
        })
        .is_err(),
        "only a timer that exists floats"
    );
}

#[test]
fn every_scenario_stages_its_scene_through_actions_and_view_state() {
    for &name in SCENARIOS {
        let app = TestApp::<ClocksShell>::capture(Some(name))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let state = app.state();
        assert_eq!(
            state.timers.len(),
            usize::from(scene(Some(name)) == "running"),
            "{name}: timers"
        );
    }
    let mut ringing = TestApp::<ClocksShell>::capture(Some("ringing")).unwrap();
    assert!(
        ringing
            .harness()
            .a11y_node_named("Timer finished")
            .is_some(),
        "ringing shows the due notice"
    );
    let mut world = TestApp::<ClocksShell>::capture(Some("world-narrow")).unwrap();
    assert!(world.harness().a11y_node_named("World clocks").is_some());
    assert_eq!(ClocksShell::shot_viewport(Some("world-narrow")), (520, 820));
    assert_eq!(ClocksShell::shot_viewport(Some("world")), (1280, 800));
}
