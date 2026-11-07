//! The clock Clocks keeps floating above other windows, as its live
//! activity over the app's channel: the shell presents it as a card or a
//! pill, and hands back the controls pressed on it.

use serde::{Deserialize, Serialize};
use ui::live_activity::{Announcement, Control, ControlKind, Kind, Published, Summary, Tone};

/// How soon a timer has to be due for its card to take the pill from the
/// others: the same minute its pill reading is the most urgent one.
const DUE_SOON_SECS: i64 = 60;

/// The floating clock as Clocks reads it, before it is published as the
/// shell's summary.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "the activity the shell presents; each flag is independent"
)]
pub struct ShellSnapshot {
    pub active: bool,
    /// The timer shown; `None` is the stopwatch.
    #[serde(default)]
    pub activity_id: Option<String>,
    pub label: String,
    pub remaining_secs: u64,
    pub stopwatch: bool,
    #[serde(default)]
    pub paused: bool,
    pub overlay: bool,
    /// Stable identity for the latest due event: a due event is announced
    /// exactly once even if the shell takes it in after the timer has
    /// already left the active set.
    #[serde(default)]
    pub notification_id: Option<String>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

/// A control pressed on the shell's floating clock.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ShellCommand {
    pub command: String,
    /// The timer it names; `None` for the stopwatch.
    #[serde(default)]
    pub activity_id: Option<String>,
}

/// What Clocks shows now, as the shell's summary: the timer or stopwatch
/// running, its controls, and the due timer it wants announced.
#[must_use]
pub fn published(snapshot: &ShellSnapshot) -> Published {
    let announce = snapshot.notification_id.clone().map(|id| Announcement {
        id,
        title: snapshot.notification_title.clone(),
        body: snapshot.notification_body.clone(),
    });
    let mut published = snapshot
        .active
        .then(|| summary(snapshot))
        .map_or_else(Published::default, Published::of);
    published.overlay = snapshot.overlay;
    published.announce = announce;
    published
}

/// The activity Clocks shows: a timer's countdown or a stopwatch's run, with
/// pausing, a minute more (a lap) and its end.
fn summary(snapshot: &ShellSnapshot) -> Summary {
    let countdown =
        (!snapshot.stopwatch).then(|| i64::try_from(snapshot.remaining_secs).unwrap_or(i64::MAX));
    let tone = countdown.map_or(Tone::Neutral, |remaining| {
        if remaining <= 0 {
            Tone::Alert
        } else if remaining <= DUE_SOON_SECS {
            Tone::Accent
        } else {
            Tone::Neutral
        }
    });
    let (run, more) = if snapshot.stopwatch {
        ("toggle-stopwatch", "lap-stopwatch")
    } else {
        ("toggle", "add-minute")
    };
    let more_label = if snapshot.stopwatch {
        "Lap"
    } else {
        "Add a minute"
    };
    let mut summary = Summary::new(
        if snapshot.stopwatch {
            Kind::Stopwatch
        } else {
            Kind::Timer
        },
        snapshot.label.clone(),
        ui::format::timecode(snapshot.remaining_secs),
    )
    .with_icon(if snapshot.stopwatch { "clock" } else { "timer" })
    .with_tone(tone)
    .with_control(Control::new(
        run,
        if snapshot.paused { "Resume" } else { "Pause" },
        ControlKind::Primary,
    ))
    .with_control(Control::new(more, more_label, ControlKind::Secondary))
    // A timer is ended by the person, which the card offers as their close.
    .with_control(Control::new("close", "End", ControlKind::Close));
    if let Some(countdown) = countdown {
        summary = summary.with_countdown(countdown);
    }
    if let Some(id) = &snapshot.activity_id {
        summary = summary.with_id(id.clone());
    }
    summary
}

/// Show what Clocks shows now as its live activity, or end it when there is
/// nothing running and nothing to announce.
pub fn publish(snapshot: &ShellSnapshot) {
    let shown = (snapshot.active || snapshot.notification_id.is_some())
        .then(|| serde_json::to_value(published(snapshot)).ok())
        .flatten();
    ui::collaboration::channel_live_activity::show(shown);
}

/// The controls pressed on the floating clock since the last call.
#[must_use]
pub fn take_commands() -> Vec<ShellCommand> {
    ui::collaboration::channel_live_activity::take_controls()
        .into_iter()
        .filter_map(|control| serde_json::from_value(control).ok())
        .collect()
}
