use super::{
    WEEKDAYS, custom_timer_duration_seconds, remaining_fraction, repeat_summary, timezone_label,
    timezone_results,
};
use crate::model::{Clocks, bounded_label};
use crate::{ClocksShell, OnThisDevice, ShellSnapshot};
use std::collections::BTreeSet;
use ui::prelude::TestApp;
use ui::theme::Theme;

#[test]
fn closed_alarm_activation_reopens_the_same_record_at_the_exact_alarm() {
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.invoke(Clocks::ADD_TIMER.with(("Distractor".to_owned(), 600)))
        .unwrap();
    app.invoke(Clocks::ADD_ALARM.with(("First".to_owned(), 10, 0, Vec::new())))
        .unwrap();
    app.invoke(Clocks::ADD_ALARM.with(("Requested".to_owned(), 11, 0, Vec::new())))
        .unwrap();
    let before = app.state();
    let id = before
        .alarms
        .iter()
        .find(|(_, alarm)| alarm.label == "Requested")
        .unwrap()
        .0
        .clone();
    let source_record = app.record().clone();
    let uri = crate::native_timers::alarm_activation(Some(&source_record), id.as_str());
    let other_record = ui::record::RecordId::minted();
    app.open_here(&[&format!("tilo-clocks:record/{other_record}")])
        .unwrap();
    assert_eq!(app.record(), &other_record);
    app.invoke(Clocks::ADD_ALARM.with(("Other record".to_owned(), 12, 0, Vec::new())))
        .unwrap();
    let mut reopened = app.relaunch(&[&uri]).unwrap();
    assert_eq!(reopened.record(), &source_record);
    assert_eq!(
        reopened.state(),
        before,
        "opening an alarm is not a document edit"
    );
    reopened
        .with_surface::<super::ClocksApp, _>(|surface| {
            assert_eq!(
                surface.mode.get(),
                super::ALARMS,
                "a running timer must not redirect the alarm activation"
            );
            assert_eq!(
                surface.state.with(|shown| shown.selected_alarm.clone()),
                Some(id)
            );
        })
        .unwrap();
}

#[test]
fn live_alarm_activation_returns_from_another_record_to_the_source_alarm() {
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.invoke(Clocks::ADD_ALARM.with(("Source".to_owned(), 11, 0, Vec::new())))
        .unwrap();
    let source = app.record().clone();
    let alarm = app.state().alarms.ids().next().unwrap().clone();
    let uri = crate::native_timers::alarm_activation(Some(&source), alarm.as_str());
    let other = ui::record::RecordId::minted();
    app.open_here(&[&format!("tilo-clocks:record/{other}")])
        .unwrap();
    assert_eq!(app.record(), &other);
    app.with_surface::<super::ClocksApp, _>(|surface| surface.activate_uri(&uri))
        .unwrap();
    app.harness().relayout();
    assert_eq!(app.record(), &source);
    app.with_surface::<super::ClocksApp, _>(|surface| {
        assert_eq!(surface.mode.get(), super::ALARMS);
        assert_eq!(
            surface.state.with(|shown| shown.selected_alarm.clone()),
            Some(alarm)
        );
    })
    .unwrap();
}

#[test]
fn semantic_alarm_open_rejects_missing_targets_without_changing_selection() {
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.invoke(Clocks::ADD_ALARM.with(("Requested".to_owned(), 11, 0, Vec::new())))
        .unwrap();
    let id = app.state().alarms.ids().next().unwrap().clone();
    app.effect(&crate::resource::ClocksEffect::OpenAlarm {
        alarm: id.to_string(),
    })
    .unwrap();
    assert!(
        app.effect(&crate::resource::ClocksEffect::OpenAlarm {
            alarm: "missing".into()
        })
        .is_err()
    );
    app.with_surface::<super::ClocksApp, _>(|surface| {
        assert_eq!(
            surface.state.with(|shown| shown.selected_alarm.clone()),
            Some(id)
        );
    })
    .unwrap();
}

#[test]
fn compact_and_large_text_layouts_keep_full_disjoint_touch_targets() {
    // The shared navigation is a grouped list. Back exposes it on a phone;
    // every screen retains a full, independent pointer target.
    for width in [360_u32, 520_u32] {
        let mut app = TestApp::<ClocksShell>::capture_at(Some("timers"), (width, 820)).unwrap();
        let harness = app.harness();
        harness.set_interaction_environment(ui::InteractionEnvironment::TOUCH);
        harness
            .perform_named("nav-shell-back", &ui::a11y::Action::Activate)
            .unwrap();
        assert!(harness.settle_for_capture(tilo_test_env::hang_bound()));
        for label in ["World", "Alarms", "Timers", "Stopwatch"] {
            let (_, row) = harness
                .a11y_nodes()
                .into_iter()
                .find(|(_, node)| {
                    node.label() == Some(label) && node.role() == ui::accesskit::Role::TreeItem
                })
                .unwrap_or_else(|| {
                    panic!("width {width}: {label} remains independently discoverable")
                });
            let bounds = row
                .bounds()
                .expect("the navigation row publishes pointer bounds");
            assert!(
                bounds.height() >= 44.0,
                "width {width}: {label} target too short: {bounds:?}"
            );
            assert!(
                bounds.x0 >= 0.0 && bounds.x1 <= f64::from(width),
                "width {width}: {label} fits the window: {bounds:?}"
            );
        }
    }

    // Case: large-text custom duration controls have disjoint geometry.
    let mut app = TestApp::<ClocksShell>::capture_at(Some("timers"), (880, 800)).unwrap();
    let harness = app.harness();
    harness.set_theme(Theme::dark().with_text_scale(2.0));
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
fn resource_selected_timer_pane_keeps_navigation_in_sync() {
    let mut app = TestApp::<ClocksShell>::capture_at(Some("timers"), (1280, 800)).unwrap();
    let harness = app.harness();
    let nodes = harness.a11y_nodes();
    for (label, selected) in [("Timers", true), ("World", false)] {
        let row = nodes
            .iter()
            .find(|(_, node)| {
                node.label() == Some(label) && node.role() == ui::accesskit::Role::TreeItem
            })
            .unwrap_or_else(|| panic!("the {label} destination is published"));
        assert_eq!(row.1.is_selected(), Some(selected), "{label} selection");
    }
    assert!(
        harness.targets()["clocks.timer.custom.add"].y0 < 800.0,
        "the timer setup remains reachable in the desktop viewport"
    );
}

/// What the shell presents of Clocks' live activity now; an ended one is
/// shown as inactive.
fn snapshot() -> ShellSnapshot {
    ui::collaboration::channel_live_activity::shown().map_or_else(
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
        |shown| serde_json::from_value(shown).unwrap(),
    )
}

/// Press `command` on the floating clock, as the shell does.
fn command(command: &str, activity_id: Option<&str>) {
    ui::collaboration::channel_live_activity::press(serde_json::json!({
        "command": command,
        "activity_id": activity_id,
    }));
}

#[test]
fn a_live_window_keeps_the_shell_floating_clock_and_answers_its_controls() {
    let mut app = TestApp::<ClocksShell>::new().unwrap();

    // Phase: nothing runs, so the shell shows nothing.
    app.settle();
    assert!(!snapshot().active, "idle: nothing shown");

    // Phase: a running timer is what the shell shows, by its id.
    app.invoke(Clocks::ADD_TIMER.with(("Laundry".into(), 2_700)))
        .unwrap();
    app.settle();
    let id = app.state().timers.ids().next().unwrap().as_str().to_owned();
    let shown = snapshot();
    assert!(shown.active && !shown.paused && !shown.overlay, "{shown:?}");
    assert_eq!(shown.activity_id.as_deref(), Some(id.as_str()));
    assert_eq!(shown.label, "Laundry");

    // Phase: the shell's controls change exactly the timer they name.
    command("toggle", Some(&id));
    app.settle();
    app.settle();
    assert!(
        ui::collaboration::channel_live_activity::take_controls().is_empty(),
        "the command is taken once"
    );
    let timer = app.state().timers.values().next().cloned().unwrap();
    assert_eq!(timer.ends_at_ms, None, "toggle pauses the running timer");
    let paused = timer
        .paused_remaining_ms
        .expect("a paused timer keeps what is left");
    command("add-minute", Some(&id));
    app.settle();
    app.settle();
    let timer = app.state().timers.values().next().cloned().unwrap();
    assert!(
        timer.ends_at_ms.is_none() && timer.paused_remaining_ms == Some(paused + 60_000),
        "a minute more, still paused: {timer:?}"
    );

    // Phase: floating is this device's choice, and the shell closes it.
    app.call_on(
        &ui::tilo_resource::items::item_resource_id::<Clocks>(
            "timers",
            &ui::prelude::Id::from_raw(id.as_str()),
        ),
        "float-timer",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    assert!(snapshot().overlay, "floating timer");
    command("close", Some(&id));
    app.settle();
    app.settle();
    assert!(
        app.settings::<OnThisDevice>()
            .get()
            .floating_timers
            .is_empty(),
        "close stops floating"
    );

    // Phase: stopwatch controls never reach a timer.
    let before = app.state().timers;
    command("toggle-stopwatch", None);
    app.settle();
    app.settle();
    assert!(app.state().stopwatch.is_running(), "stopwatch started");
    command("lap-stopwatch", None);
    app.settle();
    app.settle();
    assert_eq!(app.state().stopwatch.laps_ms.len(), 1, "one lap");
    assert_eq!(app.state().timers, before, "timers untouched");

    // Phase: a finished timer is announced once, and the notice outlives
    // the countdown in every later snapshot.
    app.invoke(Clocks::REMOVE_TIMER.with(ui::prelude::Id::from_raw(id.as_str())))
        .unwrap();
    app.invoke(Clocks::RESET_STOPWATCH.invoke()).unwrap();
    // The wall clock is held, then moved past the timer's end.
    let added = ui::time::wall();
    ui::time::freeze_wall(added);
    app.invoke(Clocks::ADD_TIMER.with(("Tea".into(), 1)))
        .unwrap();
    ui::time::freeze_wall(added + std::time::Duration::from_secs(1));
    app.settle();
    assert!(snapshot().notification_id.is_some(), "the timer rang");
    let first = snapshot();
    assert_eq!(first.notification_title, "Timer finished");
    assert_eq!(first.notification_body, "Tea");
    assert!(
        app.harness().a11y_node_named("Timer finished").is_some(),
        "the window shows the notice"
    );
    app.settle();
    let later = snapshot();
    assert!(!later.active, "nothing runs any more");
    assert_eq!(
        later.notification_id, first.notification_id,
        "the same notice, not a second one"
    );
    assert_eq!(
        app.settings::<OnThisDevice>()
            .get()
            .delivery
            .timers_rung
            .len(),
        1,
        "this device remembers it rang"
    );
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

    // Case: labels are useful, bounded and have a calm default.
    assert_eq!(bounded_label("  Pasta  ", "Timer"), "Pasta");
    assert_eq!(bounded_label("   ", "Timer"), "Timer");
    assert_eq!(bounded_label(&"x".repeat(120), "Timer").chars().count(), 80);
    assert_eq!(bounded_label("  School run ", "Alarm"), "School run");
    assert_eq!(bounded_label(" ", "Alarm"), "Alarm");

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

    // Case: alarm schedule summaries are specific.
    let selected = [WEEKDAYS[0].0, WEEKDAYS[2].0, WEEKDAYS[4].0]
        .into_iter()
        .collect::<BTreeSet<_>>();
    assert_eq!(repeat_summary(&selected), "Mon, Wed, Fri");
    assert_eq!(repeat_summary(&BTreeSet::new()), "Once");

    // Case: world clock search covers the timezone catalogue without jargon.
    assert_eq!(
        timezone_label("America/Argentina/Buenos_Aires"),
        "Buenos Aires, in America"
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

#[test]
fn staged_navigation_selection_changes_before_the_next_widget_sync() {
    use ui::framework::ResourceSurface;
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.with_surface::<super::ClocksApp, _>(|surface| {
        for (scenario, mode) in [
            ("timers", super::TIMERS),
            ("world", super::WORLD),
            ("alarms", super::ALARMS),
            ("stopwatch", super::STOPWATCH),
        ] {
            surface.stage(Some(scenario));
            assert_eq!(surface.mode.get(), mode);
            assert_eq!(
                surface.selection.get(),
                Some(mode.to_string()),
                "{scenario} must publish one selection immediately"
            );
        }
    })
    .unwrap();
}

#[test]
fn narrow_custom_timer_fields_fit_and_start_through_real_controls() {
    let mut app = TestApp::<ClocksShell>::capture_at(Some("timers"), (390, 844)).unwrap();
    let harness = app.harness();
    harness.set_interaction_environment(ui::InteractionEnvironment::TOUCH);
    let targets = harness.targets();
    let start = targets["clocks.timer.custom.add"];
    assert!(
        start.y1 <= 844.0,
        "custom timer starts without an initially cropped form: {start:?}"
    );
    for (field, value) in [("hours", "2"), ("minutes", "3"), ("seconds", "4")] {
        let id = format!("clocks.timer.custom.{field}");
        let rect = targets[&id];
        assert!(rect.width() > 0.0 && rect.height() >= 44.0);
        assert!(rect.x0 >= 0.0 && rect.x1 <= 390.0);
        harness
            .perform_named(&id, &ui::a11y::Action::SetValue(value.into()))
            .unwrap();
    }
    harness
        .perform_named("clocks.timer.custom.add", &ui::a11y::Action::Activate)
        .unwrap();
    app.settle();
    let timers = app.state();
    assert_eq!(timers.timers.len(), 1);
    assert_eq!(
        timers.timers.values().next().unwrap().duration_ms,
        7_384_000
    );
}

#[test]
fn native_alarm_handoff_does_not_emit_a_second_shell_notice() {
    let mut app = TestApp::<ClocksShell>::new().unwrap();
    app.with_surface::<super::ClocksApp, _>(|surface| {
        surface.remember_due(crate::DueEvent::Alarm {
            id: ui::prelude::Id::from_raw("owned-alarm"),
            label: "Wake up".into(),
            scheduled_for: chrono::Utc::now(),
            native_scheduled: true,
        });
        assert!(
            surface.notice.get().is_some(),
            "the app still explains the due alarm"
        );
        assert!(
            surface
                .shell_snapshot(chrono::Utc::now())
                .notification_id
                .is_none(),
            "the shell must not notify again after the scheduler's delivery"
        );
    })
    .unwrap();
    app.effect(&crate::resource::ClocksEffect::DismissNotice)
        .unwrap();
    app.with_surface::<super::ClocksApp, _>(|surface| {
        assert!(
            surface.notice.get().is_none(),
            "semantic dismissal clears the device's notice"
        );
    })
    .unwrap();
}
