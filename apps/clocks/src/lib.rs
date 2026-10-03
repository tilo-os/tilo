//! Durable, wall-clock-safe timekeeping engine shared by Clocks and shell chrome.
//! UI code supplies wall and monotonic time explicitly, which keeps suspend,
//! clock corrections, tests, and alarm delivery deterministic.

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::io;
use std::time::Duration;
use tilo_files::{Dir, VPath, blocking};
use tilo_paths::AppPaths;
use uuid::Uuid;

pub mod actions;
pub mod app;
mod native_timers;
pub mod resource;
pub use app::ClocksApp;

#[must_use]
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(ClocksApp::new())
}

/// The app as every host mounts it: the live root.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::launch::LaunchSpec::new("so.tilo.clocks", app_root).as_definition("Clocks")
}

#[must_use]
pub fn app_root_for_shot(scenario: &str) -> Box<dyn ui::widget::Widget> {
    Box::new(ClocksApp::scenario(scenario))
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorldClock {
    pub id: Uuid,
    pub city: String,
    pub timezone: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alarm {
    pub id: Uuid,
    pub label: String,
    pub local_time: NaiveTime,
    pub weekdays: BTreeSet<WeekdayKey>,
    pub enabled: bool,
    /// Prevents duplicate delivery when wall time moves backwards.
    pub last_fired_local_date: Option<chrono::NaiveDate>,
    /// Persist the occurrence, not just its time of day: reopening after
    /// midnight must still complete the alarm that was armed yesterday.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_utc: Option<DateTime<Utc>>,
}

impl Alarm {
    pub fn set_enabled(&mut self, enabled: bool) {
        self.enabled = enabled;
        // A newly enabled alarm is armed at the next tick. Never revive an
        // expired deadline left over from before the user disabled it.
        self.next_due_utc = None;
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum WeekdayKey {
    Mon,
    Tue,
    Wed,
    Thu,
    Fri,
    Sat,
    Sun,
}

impl From<Weekday> for WeekdayKey {
    fn from(day: Weekday) -> Self {
        match day {
            Weekday::Mon => Self::Mon,
            Weekday::Tue => Self::Tue,
            Weekday::Wed => Self::Wed,
            Weekday::Thu => Self::Thu,
            Weekday::Fri => Self::Fri,
            Weekday::Sat => Self::Sat,
            Weekday::Sun => Self::Sun,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Timer {
    pub id: Uuid,
    pub label: String,
    pub duration_ms: u64,
    pub ends_at_utc: Option<DateTime<Utc>>,
    pub paused_remaining_ms: Option<u64>,
    pub pinned_overlay: bool,
    /// The native host accepted responsibility for this exact deadline.
    /// Kept across restarts so a delivered alert is not submitted again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_scheduled_end: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Stopwatch {
    pub accumulated_ms: u64,
    pub started_at_utc: Option<DateTime<Utc>>,
    pub laps_ms: Vec<u64>,
    pub pinned_overlay: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClockState {
    pub world_clocks: Vec<WorldClock>,
    pub alarms: Vec<Alarm>,
    pub timers: Vec<Timer>,
    /// Native schedules owned by this profile, retained until cancellation or
    /// delivery is confirmed even when the corresponding timer was deleted.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub native_timer_owners: BTreeSet<Uuid>,
    pub stopwatch: Stopwatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DueEvent {
    Alarm {
        id: Uuid,
        label: String,
        scheduled_for: DateTime<Utc>,
    },
    Timer {
        id: Uuid,
        label: String,
        native_scheduled: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShellActivity {
    /// Stable timer identity. `None` identifies the singleton stopwatch.
    pub activity_id: Option<Uuid>,
    pub label: String,
    pub remaining: Duration,
    pub overlay: bool,
    pub stopwatch: bool,
    pub paused: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "mirrors the JSON wire format the shell reads; each flag is independent"
)]
pub struct ShellSnapshot {
    pub version: u8,
    #[serde(default = "snapshot_active_default")]
    pub active: bool,
    #[serde(default)]
    pub activity_id: Option<Uuid>,
    pub label: String,
    pub remaining_secs: u64,
    pub stopwatch: bool,
    #[serde(default)]
    pub paused: bool,
    pub overlay: bool,
    pub generation: u64,
    pub producer_pid: u32,
    /// Stable identity for the latest due event. It is deliberately separate
    /// from `generation`: live snapshots are refreshed often, while a due
    /// event must be announced exactly once even if the shell observes it
    /// after the timer has already left the active set.
    #[serde(default)]
    pub notification_id: Option<Uuid>,
    #[serde(default)]
    pub notification_title: String,
    #[serde(default)]
    pub notification_body: String,
}

fn snapshot_active_default() -> bool {
    true
}

/// Session-private app→shell transport. The runtime directory is already
/// per-login; owner/mode checks on the shell side prevent forged status.
pub mod transport {
    use super::ShellSnapshot;
    use std::io;
    use tilo_files::{Dir, VPath, blocking};

    /// `$XDG_RUNTIME_DIR/tilo`, which the shell reads and writes too.
    fn runtime() -> io::Result<Dir> {
        let location = tilo_paths::AppPaths::for_app("tilo")
            .runtime()
            .map_err(|error| io::Error::new(io::ErrorKind::NotFound, error))?;
        Dir::open(location).map_err(io::Error::other)
    }

    /// Atomically writes `snapshot` to the private transport file.
    ///
    /// # Errors
    ///
    /// Fails when the session has no runtime directory, serialization
    /// fails, or the file cannot be written.
    pub fn publish(snapshot: &ShellSnapshot) -> io::Result<()> {
        let name = VPath::new("clocks.json").map_err(io::Error::other)?;
        blocking::write_private(&runtime()?, &name, &serde_json::to_vec(snapshot)?)
            .map_err(io::Error::other)
    }

    #[derive(serde::Deserialize)]
    struct Command {
        version: u8,
        #[expect(
            clippy::struct_field_names,
            reason = "the field name is the JSON key the shell writes"
        )]
        command: String,
        #[serde(default)]
        activity_id: Option<uuid::Uuid>,
    }
    pub fn take_command() -> Option<(String, Option<uuid::Uuid>)> {
        let dir = runtime().ok()?;
        let name = VPath::new("clocks-command.json").ok()?;
        let bytes = blocking::read(&dir, &name).ok()?;
        let _ = blocking::remove(&dir, &name);
        if bytes.len() > 1024 {
            return None;
        }
        let command: Command = serde_json::from_slice(&bytes).ok()?;
        (command.version == 1).then_some((command.command, command.activity_id))
    }
}

impl Timer {
    #[must_use]
    pub fn new(label: impl Into<String>, duration: Duration) -> Self {
        let ms = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        Self {
            id: Uuid::new_v4(),
            label: label.into(),
            duration_ms: ms,
            ends_at_utc: None,
            paused_remaining_ms: Some(ms),
            pinned_overlay: false,
            native_scheduled_end: None,
        }
    }
    pub fn start(&mut self, now: DateTime<Utc>) {
        let left = self.paused_remaining_ms.take().unwrap_or(self.duration_ms);
        let delta = chrono::Duration::milliseconds(i64::try_from(left).unwrap_or(i64::MAX));
        self.ends_at_utc = now.checked_add_signed(delta);
    }
    pub fn pause(&mut self, now: DateTime<Utc>) {
        self.paused_remaining_ms = Some(
            self.remaining(now)
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        );
        self.ends_at_utc = None;
    }
    #[must_use]
    pub fn remaining(&self, now: DateTime<Utc>) -> Duration {
        if let Some(ms) = self.paused_remaining_ms {
            return Duration::from_millis(ms);
        }
        self.ends_at_utc
            .and_then(|end| end.signed_duration_since(now).to_std().ok())
            .unwrap_or_default()
    }
}

impl Stopwatch {
    #[must_use]
    pub fn elapsed(&self, now: DateTime<Utc>) -> Duration {
        let running = self
            .started_at_utc
            .and_then(|start| now.signed_duration_since(start).to_std().ok())
            .unwrap_or_default();
        Duration::from_millis(self.accumulated_ms).saturating_add(running)
    }
    pub fn start(&mut self, now: DateTime<Utc>) {
        if self.started_at_utc.is_none() {
            self.started_at_utc = Some(now);
        }
    }
    pub fn pause(&mut self, now: DateTime<Utc>) {
        self.accumulated_ms = self.elapsed(now).as_millis().try_into().unwrap_or(u64::MAX);
        self.started_at_utc = None;
    }
    pub fn reset(&mut self) {
        self.accumulated_ms = 0;
        self.started_at_utc = None;
        self.laps_ms.clear();
    }
    pub fn lap(&mut self, now: DateTime<Utc>) {
        if self.started_at_utc.is_some() {
            self.laps_ms
                .push(self.elapsed(now).as_millis().try_into().unwrap_or(u64::MAX));
        }
    }
}

const STATE_FILE: &str = "clocks.json";

/// The saved state: `tilo/clocks.json` in the user's state directory
/// (`$XDG_STATE_HOME` on Linux), in the page's profile on the web.
fn state_store() -> Option<(Dir, VPath)> {
    let dir = Dir::open(AppPaths::for_app("tilo").state()).ok()?;
    Some((dir, VPath::new(STATE_FILE).ok()?))
}

fn command_timer(timers: &mut [Timer], activity_id: Option<Uuid>) -> Option<&mut Timer> {
    match activity_id {
        Some(id) => timers.iter_mut().find(|timer| timer.id == id),
        // Compatibility for commands emitted by an older shell.
        None => timers.first_mut(),
    }
}

impl ClockState {
    /// Apply a command emitted by the shell's floating activity controller.
    /// Stopwatch-specific commands cannot fall through to a timer.
    pub fn apply_shell_command(
        &mut self,
        command: &str,
        activity_id: Option<Uuid>,
        now: DateTime<Utc>,
    ) {
        match command {
            "toggle-stopwatch" => {
                if self.stopwatch.started_at_utc.is_some() {
                    self.stopwatch.pause(now);
                } else {
                    self.stopwatch.start(now);
                }
            }
            "lap-stopwatch" => self.stopwatch.lap(now),
            "close-stopwatch" => self.stopwatch.pinned_overlay = false,
            "toggle" => {
                if let Some(timer) = command_timer(&mut self.timers, activity_id) {
                    if timer.ends_at_utc.is_some() {
                        timer.pause(now);
                    } else {
                        timer.start(now);
                    }
                }
            }
            "add-minute" => {
                if let Some(timer) = command_timer(&mut self.timers, activity_id) {
                    let left = timer.remaining(now).saturating_add(Duration::from_secs(60));
                    timer.paused_remaining_ms =
                        Some(left.as_millis().try_into().unwrap_or(u64::MAX));
                    timer.ends_at_utc = None;
                    timer.start(now);
                }
            }
            "close" => {
                if let Some(timer) = command_timer(&mut self.timers, activity_id) {
                    timer.pinned_overlay = false;
                }
            }
            _ => {}
        }
    }

    #[must_use]
    pub fn load() -> Self {
        state_store()
            .map(|(dir, name)| Self::load_from(&dir, &name))
            .unwrap_or_default()
    }

    fn load_from(dir: &Dir, name: &VPath) -> Self {
        blocking::read(dir, name)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    #[cfg(test)]
    fn load_at(path: &std::path::Path) -> Self {
        tilo_files::file_at(path)
            .map(|(dir, name)| Self::load_from(&dir, &name))
            .unwrap_or_default()
    }

    /// Persists the state to the per-user state file with private permissions.
    ///
    /// # Errors
    ///
    /// Fails when no state directory is available, the directory cannot be
    /// created, or the file cannot be written.
    pub fn save(&self) -> io::Result<()> {
        let (dir, name) = state_store()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no state directory"))?;
        self.save_to(&dir, &name)
    }

    fn save_to(&self, dir: &Dir, name: &VPath) -> io::Result<()> {
        blocking::write_private(dir, name, &serde_json::to_vec_pretty(self)?)
            .map_err(io::Error::other)
    }

    #[cfg(test)]
    fn save_at(&self, path: &std::path::Path) -> io::Result<()> {
        let (dir, name) = tilo_files::file_at(path).map_err(io::Error::other)?;
        self.save_to(&dir, &name)
    }
}

impl ClockState {
    /// Returns due events exactly once and updates durable delivery markers.
    pub fn tick(&mut self, now_utc: DateTime<Utc>, now_local: DateTime<Local>) -> Vec<DueEvent> {
        let mut due = Vec::new();
        for alarm in &mut self.alarms {
            if !alarm.enabled {
                alarm.next_due_utc = None;
                continue;
            }
            if alarm.next_due_utc.is_none_or(|deadline| deadline > now_utc) {
                // Resolve in local time, so future deadlines follow timezone
                // changes. An elapsed persisted deadline remains due, even
                // when the application was closed for several days.
                alarm.next_due_utc =
                    next_alarm_local(alarm, now_local).map(|deadline| deadline.with_timezone(&Utc));
            }
            if let Some(deadline) = alarm.next_due_utc.filter(|deadline| *deadline <= now_utc) {
                due.push(DueEvent::Alarm {
                    id: alarm.id,
                    label: alarm.label.clone(),
                    scheduled_for: deadline,
                });
                alarm.last_fired_local_date = Some(deadline.with_timezone(&Local).date_naive());
                alarm.next_due_utc = None;
                if alarm.weekdays.is_empty() {
                    alarm.enabled = false;
                } else {
                    // Catch up once after suspend/exit, then arm the next
                    // future occurrence instead of replaying missed days.
                    alarm.next_due_utc = next_alarm_local(alarm, now_local)
                        .map(|deadline| deadline.with_timezone(&Utc));
                }
            }
        }
        for timer in &mut self.timers {
            if timer.ends_at_utc.is_some_and(|end| end <= now_utc) {
                let native_scheduled = timer.native_scheduled_end == timer.ends_at_utc;
                timer.ends_at_utc = None;
                timer.native_scheduled_end = None;
                timer.paused_remaining_ms = Some(0);
                due.push(DueEvent::Timer {
                    id: timer.id,
                    label: timer.label.clone(),
                    native_scheduled,
                });
            }
        }
        due
    }

    #[must_use]
    pub fn shell_activity(&self, now: DateTime<Utc>) -> Option<ShellActivity> {
        if self.stopwatch.pinned_overlay
            && (self.stopwatch.started_at_utc.is_some() || self.stopwatch.accumulated_ms > 0)
        {
            return Some(ShellActivity {
                activity_id: None,
                label: "Stopwatch".into(),
                remaining: self.stopwatch.elapsed(now),
                overlay: true,
                stopwatch: true,
                paused: self.stopwatch.started_at_utc.is_none(),
            });
        }
        self.timers
            .iter()
            .filter(|t| t.ends_at_utc.is_some() || t.pinned_overlay)
            // A timer explicitly floated by the user owns the controller,
            // even if some unrelated background timer ends sooner. Within
            // each class the most urgent timer still wins.
            .min_by_key(|t| (!t.pinned_overlay, t.remaining(now)))
            .map(|t| ShellActivity {
                activity_id: Some(t.id),
                label: if t.label.is_empty() {
                    "Timer".into()
                } else {
                    t.label.clone()
                },
                remaining: t.remaining(now),
                overlay: t.pinned_overlay,
                stopwatch: false,
                paused: t.ends_at_utc.is_none(),
            })
    }
}

/// DST-aware next alarm occurrence using the system timezone. Missing spring
/// times advance minute-by-minute; ambiguous autumn times select the first.
#[must_use]
pub fn next_alarm_local(alarm: &Alarm, after: DateTime<Local>) -> Option<DateTime<Local>> {
    next_alarm_in_timezone(alarm, &after)
}

fn next_alarm_in_timezone<Tz: TimeZone>(
    alarm: &Alarm,
    after: &DateTime<Tz>,
) -> Option<DateTime<Tz>> {
    if !alarm.enabled {
        return None;
    }
    for day in 0..=8_i64 {
        let date = after
            .date_naive()
            .checked_add_signed(chrono::Duration::days(day))?;
        if alarm.last_fired_local_date == Some(date)
            || (!alarm.weekdays.is_empty() && !alarm.weekdays.contains(&date.weekday().into()))
        {
            continue;
        }
        let mut naive = date.and_time(alarm.local_time);
        for _ in 0..180 {
            if let Some(candidate) = after.timezone().from_local_datetime(&naive).earliest() {
                if candidate > *after {
                    return Some(candidate);
                }
                // A valid occurrence that has passed belongs to another day.
                // Only nonexistent spring-forward times advance by a minute.
                break;
            }
            naive = naive.checked_add_signed(chrono::Duration::minutes(1))?;
        }
    }
    None
}

pub use resource::{Clocks, ClocksShell};

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A one-shot 12:00 "Lunch" alarm; cases override the fields they probe.
    fn lunch_alarm(weekdays: BTreeSet<WeekdayKey>) -> Alarm {
        Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays,
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        }
    }

    fn local_utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn alarm_tick_arms_delivers_once_and_persists_across_restarts() {
        // Phase: a new alarm for a past time waits until tomorrow.
        {
            let now = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
            let mut state = ClockState::default();
            state.alarms.push(lunch_alarm(BTreeSet::new()));
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "past time: nothing due at arming"
            );
            let before = Local.with_ymd_and_hms(2026, 1, 16, 11, 59, 59).unwrap();
            assert!(
                state.tick(before.with_timezone(&Utc), before).is_empty(),
                "past time: nothing due a second before tomorrow's deadline"
            );
            let deadline = Local.with_ymd_and_hms(2026, 1, 16, 12, 0, 0).unwrap();
            assert!(
                matches!(state.tick(deadline.with_timezone(&Utc), deadline).as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch"),
                "past time: fires at tomorrow's deadline"
            );
            assert!(!state.alarms[0].enabled, "past time: one-shot disables");
            assert!(
                state
                    .tick(deadline.with_timezone(&Utc), deadline)
                    .is_empty(),
                "past time: fires only once"
            );
        }

        // Phase: a pending alarm survives a restart on another day.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("clocks.json");
            let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
            let mut state = ClockState::default();
            state.alarms.push(lunch_alarm(BTreeSet::new()));
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "restart: nothing due at arming"
            );
            state.save_at(&path).unwrap();
            let mut restored = ClockState::load_at(&path);
            let reopened = Local.with_ymd_and_hms(2026, 1, 16, 8, 0, 0).unwrap();
            assert!(
                matches!(restored.tick(reopened.with_timezone(&Utc), reopened).as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch"),
                "restart: missed alarm delivered after reopening"
            );
            assert!(!restored.alarms[0].enabled, "restart: one-shot disables");
            assert_eq!(
                restored.alarms[0].last_fired_local_date,
                Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()),
                "restart: records the scheduled date"
            );
        }

        // Phase: a recurring alarm catches up once and persists the next weekday.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("clocks.json");
            let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
            let mut state = ClockState::default();
            state
                .alarms
                .push(lunch_alarm(BTreeSet::from([WeekdayKey::Thu])));
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "recurring: nothing due at arming"
            );
            let reopened = Local.with_ymd_and_hms(2026, 1, 23, 8, 0, 0).unwrap();
            let events = state.tick(reopened.with_timezone(&Utc), reopened);
            assert!(
                matches!(events.as_slice(), [DueEvent::Alarm { scheduled_for, .. }]
                if *scheduled_for == local_utc(2026, 1, 15, 12, 0, 0)),
                "recurring: exactly one catch-up for the first missed occurrence"
            );
            assert_eq!(
                state.alarms[0].next_due_utc,
                Some(local_utc(2026, 1, 29, 12, 0, 0)),
                "recurring: next Thursday after the reopen"
            );
            state.save_at(&path).unwrap();
            let mut restored = ClockState::load_at(&path);
            assert!(
                restored
                    .tick(reopened.with_timezone(&Utc), reopened)
                    .is_empty(),
                "recurring: restored state does not repeat the catch-up"
            );
            let next = Local.with_ymd_and_hms(2026, 1, 29, 12, 0, 0).unwrap();
            assert_eq!(
                restored.tick(next.with_timezone(&Utc), next).len(),
                1,
                "recurring: fires on the persisted weekday"
            );
            assert!(
                restored.tick(next.with_timezone(&Utc), next).is_empty(),
                "recurring: fires only once on that weekday"
            );
        }

        // Phase: disabling and re-enabling an alarm discards the old deadline.
        {
            let now = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
            let mut state = ClockState::default();
            let mut alarm = lunch_alarm(BTreeSet::new());
            alarm.next_due_utc = Some(local_utc(2026, 1, 15, 12, 0, 0));
            state.alarms.push(alarm);
            state.alarms[0].set_enabled(false);
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "toggle: disabled alarm does not fire"
            );
            state.alarms[0].set_enabled(true);
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "toggle: re-enabled alarm does not fire the stale deadline"
            );
            assert_eq!(
                state.alarms[0].next_due_utc,
                Some(local_utc(2026, 1, 16, 12, 0, 0)),
                "toggle: re-armed for tomorrow"
            );
        }

        // Phase: legacy alarm state (no next_due_utc) arms without losing settings.
        {
            let serialized = r#"{"id":"f33f2200-0e21-44b8-a337-fc7ec3e5f625","label":"Lunch","local_time":"12:00:00","weekdays":["Thu"],"enabled":true,"last_fired_local_date":null}"#;
            let alarm: Alarm = serde_json::from_str(serialized).unwrap();
            let mut state = ClockState {
                alarms: vec![alarm],
                ..ClockState::default()
            };
            let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
            assert!(
                state.tick(now.with_timezone(&Utc), now).is_empty(),
                "legacy: nothing due at arming"
            );
            assert_eq!(
                state.alarms[0].next_due_utc,
                Some(local_utc(2026, 1, 15, 12, 0, 0)),
                "legacy: armed for today"
            );
            assert_eq!(state.alarms[0].label, "Lunch", "legacy: label kept");
            assert_eq!(
                state.alarms[0].weekdays,
                BTreeSet::from([WeekdayKey::Thu]),
                "legacy: weekdays kept"
            );
        }
    }

    #[test]
    fn next_alarm_occurrence_honors_time_weekdays_history_and_dst() {
        // Case: keeps the requested time after today's occurrence.
        let after = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
        assert_eq!(
            next_alarm_local(&lunch_alarm(BTreeSet::new()), after),
            Some(Local.with_ymd_and_hms(2026, 1, 16, 12, 0, 0).unwrap()),
            "case after-todays-occurrence"
        );

        // Case: does not repeat a delivered date after a clock rollback.
        let after = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let mut alarm = lunch_alarm(BTreeSet::from([WeekdayKey::Thu]));
        alarm.last_fired_local_date = Some(after.date_naive());
        assert_eq!(
            next_alarm_local(&alarm, after),
            Some(Local.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap()),
            "case clock-rollback"
        );

        // Case: resolves DST gaps and never repeats the autumn hour.
        let zone = chrono_tz::Europe::Berlin;
        let alarm = Alarm {
            id: Uuid::new_v4(),
            label: "Wake".into(),
            local_time: NaiveTime::from_hms_opt(2, 30, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        };
        let spring = zone.with_ymd_and_hms(2026, 3, 29, 1, 0, 0).unwrap();
        assert_eq!(
            next_alarm_in_timezone(&alarm, &spring),
            Some(zone.with_ymd_and_hms(2026, 3, 29, 3, 0, 0).unwrap()),
            "case dst-spring-gap"
        );
        let autumn = zone.with_ymd_and_hms(2026, 10, 25, 1, 0, 0).unwrap();
        let first = next_alarm_in_timezone(&alarm, &autumn).unwrap();
        assert_eq!(
            first.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap(),
            "case dst-autumn-first-occurrence"
        );
        assert_eq!(
            next_alarm_in_timezone(&alarm, &first),
            Some(zone.with_ymd_and_hms(2026, 10, 26, 2, 30, 0).unwrap()),
            "case dst-autumn-no-repeat"
        );

        // Case: honors disabled and weekday settings.
        let after = Utc.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let mut alarm = Alarm {
            id: Uuid::new_v4(),
            label: "Weekend".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::from([WeekdayKey::Sat]),
            enabled: false,
            last_fired_local_date: None,
            next_due_utc: None,
        };
        assert_eq!(
            next_alarm_in_timezone(&alarm, &after),
            None,
            "case disabled"
        );
        alarm.enabled = true;
        assert_eq!(
            next_alarm_in_timezone(&alarm, &after),
            Some(Utc.with_ymd_and_hms(2026, 1, 17, 12, 0, 0).unwrap()),
            "case weekday-only"
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn timer_deadline_delivers_once_across_suspend_restart_and_native_handoff() {
        // Case: a running timer counts down by deadline, so suspend cannot stall it.
        {
            let now = Utc::now();
            let mut t = Timer::new("Tea", Duration::from_secs(300));
            t.start(now);
            assert_eq!(
                t.remaining(now + chrono::Duration::seconds(120)),
                Duration::from_secs(180),
                "case suspend-by-deadline"
            );
        }

        // Case: a paused (never started) timer does not decay.
        {
            let now = Utc::now();
            let t = Timer::new("", Duration::from_secs(9));
            assert_eq!(
                t.remaining(now + chrono::Duration::hours(1)),
                Duration::from_secs(9),
                "case paused-no-decay"
            );
        }

        // Case: a due timer fires once.
        {
            let now = Utc::now();
            let mut s = ClockState::default();
            let mut t = Timer::new("Tea", Duration::from_secs(1));
            t.start(now);
            s.timers.push(t);
            assert_eq!(
                s.tick(now + chrono::Duration::seconds(2), Local::now())
                    .len(),
                1,
                "case fires-once: first tick"
            );
            assert!(
                s.tick(now + chrono::Duration::seconds(3), Local::now())
                    .is_empty(),
                "case fires-once: second tick"
            );
        }

        // Case: native timer handoff survives restart and matches only its deadline.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("clocks.json");
            let now = Utc::now();
            let mut timer = Timer::new("Tea", Duration::from_secs(30));
            timer.start(now);
            timer.native_scheduled_end = timer.ends_at_utc;
            let mut state = ClockState::default();
            state.timers.push(timer);
            state.save_at(&path).unwrap();
            let mut restored = ClockState::load_at(&path);
            let later = now + chrono::Duration::seconds(31);
            assert!(
                matches!(
                    restored.tick(later, later.with_timezone(&Local)).as_slice(),
                    [DueEvent::Timer {
                        native_scheduled: true,
                        ..
                    }]
                ),
                "case native-handoff: restored deadline is natively scheduled"
            );
            assert_eq!(
                restored.timers[0].native_scheduled_end, None,
                "case native-handoff: handoff consumed"
            );
            assert!(
                restored.tick(later, later.with_timezone(&Local)).is_empty(),
                "case native-handoff: fires only once"
            );

            let mut changed = ClockState::load_at(&path);
            changed.timers[0].pause(now + chrono::Duration::seconds(1));
            changed.timers[0].start(now + chrono::Duration::seconds(20));
            let later = now + chrono::Duration::seconds(60);
            assert!(
                matches!(
                    changed.tick(later, later.with_timezone(&Local)).as_slice(),
                    [DueEvent::Timer {
                        native_scheduled: false,
                        ..
                    }]
                ),
                "case native-handoff: a changed deadline is not the scheduled one"
            );
        }

        // Case: a saved timer survives file replacement and restart.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("private/clocks.json");
            let now = Utc::now();
            let mut state = ClockState::default();
            let mut timer = Timer::new("Tea — Grün", Duration::from_secs(60));
            timer.start(now);
            state.timers.push(timer);
            state.save_at(&path).unwrap();
            assert_eq!(
                ClockState::load_at(&path),
                state,
                "case saved-timer: first save round-trips"
            );

            state.timers[0].label = "Updated tea timer".into();
            state.save_at(&path).unwrap();
            let mut reopened = ClockState::load_at(&path);
            assert_eq!(
                reopened, state,
                "replacing the saved file preserves the complete state"
            );
            let later = now + chrono::Duration::seconds(61);
            assert_eq!(
                reopened.tick(later, later.with_timezone(&Local)).len(),
                1,
                "case saved-timer: fires after reopening"
            );
            reopened.save_at(&path).unwrap();
            let mut after_delivery = ClockState::load_at(&path);
            assert!(
                after_delivery
                    .tick(later, later.with_timezone(&Local))
                    .is_empty(),
                "case saved-timer: delivery persisted"
            );
        }
    }

    #[test]
    fn state_store_round_trips_and_never_clobbers_a_blocker() {
        // Case: a saved state reads back from its file.
        {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join(STATE_FILE);
            let state = ClockState::default();
            state.save_at(&path).unwrap();
            assert_eq!(ClockState::load_at(&path), state, "case round trip");
        }

        // Case: a save failure is reported without overwriting a blocker.
        {
            let directory = tempfile::tempdir().unwrap();
            let blocker = directory.path().join("not-a-directory");
            fs::write(&blocker, b"keep me").unwrap();
            assert!(
                ClockState::default()
                    .save_at(&blocker.join("clocks.json"))
                    .is_err(),
                "case blocker: save reports failure"
            );
            assert_eq!(
                fs::read(blocker).unwrap(),
                b"keep me",
                "case blocker: blocker untouched"
            );
        }
    }

    #[test]
    fn stopwatch_accumulates_across_pause_and_laps_until_reset() {
        // Case: accumulates across pause and resume.
        {
            let now = Utc::now();
            let mut stopwatch = Stopwatch::default();
            stopwatch.start(now);
            stopwatch.pause(now + chrono::Duration::seconds(12));
            stopwatch.start(now + chrono::Duration::seconds(20));
            assert_eq!(
                stopwatch.elapsed(now + chrono::Duration::seconds(25)),
                Duration::from_secs(17),
                "case pause-resume"
            );
        }

        // Case: laps are cumulative and reset clears them.
        {
            let now = Utc::now();
            let mut stopwatch = Stopwatch::default();
            stopwatch.start(now);
            stopwatch.lap(now + chrono::Duration::seconds(3));
            stopwatch.lap(now + chrono::Duration::seconds(8));
            assert_eq!(stopwatch.laps_ms, vec![3_000, 8_000], "case laps");
            stopwatch.reset();
            assert_eq!(stopwatch, Stopwatch::default(), "case reset");
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn shell_activity_shows_and_commands_only_the_floated_clock() {
        // Case: a paused pinned timer remains in the shell and is marked resumable.
        {
            let now = Utc::now();
            let mut state = ClockState::default();
            let mut timer = Timer::new("Tea", Duration::from_secs(90));
            timer.pinned_overlay = true;
            state.timers.push(timer);
            let activity = state.shell_activity(now).unwrap();
            assert_eq!(
                activity.remaining,
                Duration::from_secs(90),
                "case paused-timer: remaining"
            );
            assert!(activity.overlay, "case paused-timer: overlay");
            assert!(activity.paused, "case paused-timer: paused");
            assert!(!activity.stopwatch, "case paused-timer: not stopwatch");
        }

        // Case: a paused pinned stopwatch remains in the shell and is marked resumable.
        {
            let now = Utc::now();
            let mut state = ClockState::default();
            state.stopwatch.start(now);
            state.stopwatch.pause(now + chrono::Duration::seconds(12));
            state.stopwatch.pinned_overlay = true;
            let activity = state
                .shell_activity(now + chrono::Duration::minutes(1))
                .unwrap();
            assert_eq!(
                activity.remaining,
                Duration::from_secs(12),
                "case paused-stopwatch: elapsed"
            );
            assert!(activity.paused, "case paused-stopwatch: paused");
            assert!(activity.stopwatch, "case paused-stopwatch: stopwatch");
        }

        // Case: an explicitly floated timer owns the overlay over an unpinned urgent timer.
        {
            let now = Utc::now();
            let mut state = ClockState::default();
            let mut urgent = Timer::new("Eggs", Duration::from_secs(30));
            urgent.start(now);
            let mut floated = Timer::new("Focus", Duration::from_secs(1_500));
            floated.start(now);
            floated.pinned_overlay = true;
            let floated_id = floated.id;
            state.timers.extend([urgent, floated]);

            let activity = state.shell_activity(now).unwrap();
            assert_eq!(
                activity.activity_id,
                Some(floated_id),
                "case floated-over-urgent: id"
            );
            assert_eq!(activity.label, "Focus", "case floated-over-urgent: label");
            assert!(activity.overlay, "case floated-over-urgent: overlay");
        }

        // Case: floating stopwatch commands never mutate a timer.
        {
            let now = Utc::now();
            let mut state = ClockState::default();
            let mut timer = Timer::new("Tea", Duration::from_secs(300));
            timer.start(now);
            timer.pinned_overlay = true;
            state.timers.push(timer.clone());
            state.stopwatch.start(now);
            state.stopwatch.pinned_overlay = true;

            state.apply_shell_command("lap-stopwatch", None, now + chrono::Duration::seconds(12));
            assert_eq!(
                state.stopwatch.laps_ms,
                vec![12_000],
                "case stopwatch-commands: lap"
            );
            assert_eq!(
                state.timers.first(),
                Some(&timer),
                "case stopwatch-commands: timer untouched by lap"
            );

            state.apply_shell_command("close-stopwatch", None, now);
            assert!(
                !state.stopwatch.pinned_overlay,
                "case stopwatch-commands: close unpins stopwatch"
            );
            assert!(
                state
                    .timers
                    .first()
                    .is_some_and(|timer| timer.pinned_overlay),
                "case stopwatch-commands: timer stays pinned"
            );
        }

        // Case: floating commands mutate the timer named by the shell snapshot.
        {
            let now = Utc::now();
            let mut state = ClockState::default();
            let mut later = Timer::new("Laundry", Duration::from_secs(900));
            later.start(now);
            let mut sooner = Timer::new("Tea", Duration::from_secs(120));
            sooner.start(now);
            sooner.pinned_overlay = true;
            let later_id = later.id;
            let sooner_id = sooner.id;
            state.timers.extend([later, sooner]);

            let activity = state.shell_activity(now).unwrap();
            assert_eq!(
                activity.activity_id,
                Some(sooner_id),
                "case named-timer: shell shows the pinned timer"
            );
            state.apply_shell_command("toggle", activity.activity_id, now);

            assert!(
                state
                    .timers
                    .iter()
                    .find(|timer| timer.id == later_id)
                    .unwrap()
                    .ends_at_utc
                    .is_some(),
                "a shell control must not mutate a different timer"
            );
            assert!(
                state
                    .timers
                    .iter()
                    .find(|timer| timer.id == sooner_id)
                    .unwrap()
                    .ends_at_utc
                    .is_none(),
                "the timer visible in the shell is the one that pauses"
            );
        }
    }
}
