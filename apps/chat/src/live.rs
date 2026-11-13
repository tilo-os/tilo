//! The call as the shell's live activity: a card or a pill the person keeps
//! in view while they work in another workspace, and the controls they press
//! there.
//!
//! The shell decides whether the call's card stands on the desk: while Chat's
//! own window is on screen the call is drawn there, and the card waits. What
//! this module publishes is the reading the pill and the card's fallback show,
//! and the activity id the card's window is presented under.

use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};
use ui::time::Instant;

use crate::call::CallState;

/// The activity id a call in `channel` runs as.
#[must_use]
pub(crate) fn activity(channel: &str) -> String {
    format!("chat-call:{channel}")
}

/// What Chat publishes now: the running call, or nothing at all.
#[must_use]
pub(crate) fn published(call: &CallState, title: &str, subtitle: &str, now: Instant) -> Published {
    if !call.active || call.channel.is_empty() {
        return Published::default();
    }
    let mut published = Published::of(summary(call, title, subtitle, now));
    // A call asks for its card without the person pulling it: they are in
    // the call, and it should be in reach wherever they work.
    published.overlay = true;
    published
}

/// The call's reading: who, for how long, and the controls the person presses
/// where the card shows it.
fn summary(call: &CallState, title: &str, subtitle: &str, now: Instant) -> Summary {
    let muted = call.muted || call.deafened;
    Summary::new(Kind::Call, title, elapsed(call, now))
        .with_id(activity(&call.channel))
        .with_icon("phone")
        .with_subtitle(subtitle)
        .with_control(Control::new(
            "mute",
            crate::i18n::t(if muted {
                "chat-call-unmute"
            } else {
                "chat-call-mute"
            }),
            ControlKind::Primary,
        ))
        .with_control(Control::new(
            "leave",
            crate::i18n::t("chat-call-leave"),
            ControlKind::Close,
        ))
}

/// How long the call has run, as a card reads it.
pub(crate) fn elapsed(call: &CallState, now: Instant) -> String {
    let Some(started) = call.started else {
        return String::new();
    };
    ui::format::timecode(now.saturating_duration_since(started).as_secs())
}

/// A control pressed on the call's card or pill, where the person is not in
/// Chat's own window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Command {
    /// Mute or unmute the microphone.
    Mute,
    /// Leave the call.
    Leave,
}

/// The controls pressed on the call since the last call of this, as what
/// they ask.
#[must_use]
pub(crate) fn take_commands() -> Vec<Command> {
    ui::collaboration::channel_live_activity::take_controls()
        .into_iter()
        .filter_map(|control| {
            let command = control.get("command")?.as_str()?;
            match command {
                "mute" => Some(Command::Mute),
                "leave" => Some(Command::Leave),
                _ => None,
            }
        })
        .collect()
}

/// Show what Chat shows now as its live activity, or end it.
pub(crate) fn publish(published: &Published) {
    let shown = (published.summary.is_some())
        .then(|| serde_json::to_value(published).ok())
        .flatten();
    ui::collaboration::channel_live_activity::show(shown);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn started() -> Instant {
        ui::time::now()
    }

    /// A running call reads as who it is with, how long it has run, and the
    /// two things the person does from the card.
    fn a_call_reads_as_who_and_for_how_long() {
        let call = CallState {
            active: true,
            channel: "voice".into(),
            started: Some(started()),
            ..CallState::default()
        };
        let shown = published(&call, "Ada and Grace", "2 people", started());
        assert!(shown.overlay, "a call asks for its card");
        let summary = shown.summary.expect("a summary");
        assert_eq!(summary.kind, Kind::Call);
        assert_eq!(summary.id, "chat-call:voice");
        assert_eq!(summary.title, "Ada and Grace");
        assert_eq!(summary.subtitle, "2 people");
        assert_eq!(summary.value, "0:00");
        assert_eq!(summary.icon, "phone");
        let controls: Vec<_> = summary.controls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(controls, ["mute", "leave"]);
        assert_eq!(
            summary.close_control().map(|c| c.id.as_str()),
            Some("leave")
        );

        // A muted call offers to unmute instead.
        let muted = CallState {
            muted: true,
            ..call
        };
        let summary = published(&muted, "Ada and Grace", "2 people", started())
            .summary
            .expect("a summary");
        assert_eq!(summary.controls[0].label, "Unmute");
    }

    /// A call that is not running shows nothing at all.
    fn no_call_is_no_activity() {
        let shown = published(&CallState::default(), "Ada", "", started());
        assert!(shown.summary.is_none());
        assert!(!shown.overlay);
    }

    /// The elapsed time reads as a clock past the hour.
    fn a_long_call_reads_in_hours() {
        let long_ago = started()
            .checked_sub(ui::time::Duration::from_secs(3725))
            .expect("a platform clock that has run for an hour");
        let call = CallState {
            active: true,
            channel: "voice".into(),
            started: Some(long_ago),
            ..CallState::default()
        };
        let summary = published(&call, "Ada", "", started())
            .summary
            .expect("a summary");
        assert_eq!(summary.value, "1:02:05");

        // The clock turns over at the minute and at the hour, and a call
        // started after the moment it is read at has run for no time.
        let now = started();
        let hour = ui::time::Duration::from_secs(3600);
        let read_at = |started: Option<Instant>| {
            let call = CallState {
                active: true,
                channel: "voice".into(),
                started,
                ..CallState::default()
            };
            published(&call, "Ada", "", now)
                .summary
                .expect("a summary")
                .value
        };
        for (secs, reads) in [
            (59, "0:59"),
            (60, "1:00"),
            (3599, "59:59"),
            (3600, "1:00:00"),
        ] {
            let ago = ui::time::Duration::from_secs(secs);
            assert_eq!(read_at(now.checked_sub(ago)), reads, "after {secs} s");
        }
        assert_eq!(read_at(now.checked_add(hour)), "0:00", "not started yet");
    }

    /// What the call's live activity says.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    #[test]
    fn a_call_publishes_its_card_and_no_call_publishes_nothing() {
        crate::testing::cases!(
            a_call_reads_as_who_and_for_how_long,
            no_call_is_no_activity,
            a_long_call_reads_in_hours,
        );
    }
}
