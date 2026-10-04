#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::{
    WEEKDAYS, custom_timer_duration_seconds, remaining_fraction, repeat_summary, timezone_label,
    timezone_results,
};
use crate::model::{Clocks, bounded_label};
use crate::{ClocksShell, OnThisDevice, ShellSnapshot};
use std::collections::BTreeSet;
use std::path::Path;
use ui::prelude::TestApp;
use ui::theme::Theme;

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
        let mut app = TestApp::<ClocksShell>::capture_at(Some("timers"), (width, 820)).unwrap();
        let harness = app.harness();
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

fn snapshot(runtime: &Path) -> ShellSnapshot {
    serde_json::from_slice(&std::fs::read(runtime.join("tilo/clocks.json")).unwrap()).unwrap()
}

fn command(runtime: &Path, command: &str, activity_id: Option<&str>) {
    std::fs::write(
        runtime.join("tilo/clocks-command.json"),
        serde_json::json!({
            "version": 1,
            "command": command,
            "activity_id": activity_id,
        })
        .to_string(),
    )
    .unwrap();
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, one labelled phase per merged case"
)]
fn a_live_window_keeps_the_shell_floating_clock_and_answers_its_controls() {
    let home = tilo_test_env::TestHome::new().unwrap();
    let runtime = home.runtime().to_path_buf();
    let mut app = TestApp::<ClocksShell>::new().unwrap();

    // Phase: nothing runs, so the shell shows nothing.
    app.settle();
    assert!(!snapshot(&runtime).active, "idle: inactive tombstone");

    // Phase: a running timer is what the shell shows, by its id.
    app.invoke(Clocks::ADD_TIMER.with(("Laundry".into(), 2_700)))
        .unwrap();
    app.settle();
    let id = app.state().timers.ids().next().unwrap().as_str().to_owned();
    let shown = snapshot(&runtime);
    assert!(shown.active && !shown.paused && !shown.overlay, "{shown:?}");
    assert_eq!(shown.activity_id.as_deref(), Some(id.as_str()));
    assert_eq!(shown.label, "Laundry");

    // Phase: the shell's controls change exactly the timer they name.
    command(&runtime, "toggle", Some(&id));
    app.settle();
    app.settle();
    assert!(
        !runtime.join("tilo/clocks-command.json").exists(),
        "the command is taken once"
    );
    let timer = app.state().timers.values().next().cloned().unwrap();
    assert_eq!(timer.ends_at_ms, None, "toggle pauses the running timer");
    command(&runtime, "add-minute", Some(&id));
    app.settle();
    app.settle();
    let timer = app.state().timers.values().next().cloned().unwrap();
    assert!(
        timer.ends_at_ms.is_some() && timer.remaining(chrono::Utc::now()).as_secs() > 2_700,
        "a minute more, running again: {timer:?}"
    );

    // Phase: floating is this device's choice, and the shell closes it.
    app.call_on(
        &ui::tilo_resource::items::item_resource_id::<Clocks>(
            "timers",
            &ui::prelude::Id::from_raw(id.as_str()),
        ),
        "set-floating",
        serde_json::json!({}),
    )
    .unwrap();
    app.settle();
    assert!(snapshot(&runtime).overlay, "floating timer");
    command(&runtime, "close", Some(&id));
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
    command(&runtime, "toggle-stopwatch", None);
    app.settle();
    app.settle();
    assert!(app.state().stopwatch.is_running(), "stopwatch started");
    command(&runtime, "lap-stopwatch", None);
    app.settle();
    app.settle();
    assert_eq!(app.state().stopwatch.laps_ms.len(), 1, "one lap");
    assert_eq!(app.state().timers, before, "timers untouched");

    // Phase: a finished timer is announced once, and the notice outlives
    // the countdown in every later snapshot.
    app.invoke(Clocks::REMOVE_TIMER.with(ui::prelude::Id::from_raw(id.as_str())))
        .unwrap();
    app.invoke(Clocks::RESET_STOPWATCH.invoke()).unwrap();
    app.invoke(Clocks::ADD_TIMER.with(("Tea".into(), 1)))
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1_100));
    app.settle();
    assert!(
        snapshot(&runtime).notification_id.is_some(),
        "the timer rang"
    );
    let first = snapshot(&runtime);
    assert_eq!(first.notification_title, "Timer finished");
    assert_eq!(first.notification_body, "Tea");
    assert!(
        app.harness().a11y_node_named("Timer finished").is_some(),
        "the window shows the notice"
    );
    app.settle();
    let later = snapshot(&runtime);
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
