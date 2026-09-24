//! Durable, wall-clock-safe timekeeping engine shared by Clocks and shell chrome.
//! UI code supplies wall and monotonic time explicitly, which keeps suspend,
//! clock corrections, tests, and alarm delivery deterministic.

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub mod actions;
pub mod app;
#[cfg(windows)]
mod native_timers;
pub use app::ClocksApp;

#[must_use]
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    Box::new(ClocksApp::new())
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
    use std::fs;
    use std::io;
    use std::path::PathBuf;

    #[must_use]
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .map(|p| p.join("tilo/clocks.json"))
    }
    pub fn publish(snapshot: &ShellSnapshot) -> io::Result<()> {
        let path = path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR missing"))?;
        atomic_file::write_private(&path, &serde_json::to_vec(snapshot)?)
    }

    #[derive(serde::Deserialize)]
    struct Command {
        version: u8,
        command: String,
        #[serde(default)]
        activity_id: Option<uuid::Uuid>,
    }
    pub fn take_command() -> Option<(String, Option<uuid::Uuid>)> {
        let path = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)?
            .join("tilo/clocks-command.json");
        let bytes = fs::read(&path).ok()?;
        let _ = fs::remove_file(path);
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
            .and_then(|end| (end - now).to_std().ok())
            .unwrap_or_default()
    }
}

impl Stopwatch {
    #[must_use]
    pub fn elapsed(&self, now: DateTime<Utc>) -> Duration {
        let running = self
            .started_at_utc
            .and_then(|start| (now - start).to_std().ok())
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

fn state_path() -> Option<PathBuf> {
    state_path_with(|key| std::env::var_os(key))
}

fn state_path_with(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    let absolute = |key| env(key).map(PathBuf::from).filter(|p| p.is_absolute());
    #[cfg(windows)]
    let native = || {
        absolute("LOCALAPPDATA")
            .or_else(|| absolute("USERPROFILE").map(|p| p.join("AppData/Local")))
    };
    #[cfg(not(windows))]
    let native = || absolute("HOME").map(|p| p.join(".local/state"));
    absolute("XDG_STATE_HOME")
        .or_else(native)
        .map(|p| p.join("tilo/clocks.json"))
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
        state_path().map(|p| Self::load_at(&p)).unwrap_or_default()
    }

    fn load_at(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> io::Result<()> {
        let path = state_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no state directory"))?;
        self.save_at(&path)
    }

    fn save_at(&self, path: &Path) -> io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid state path"))?;
        private_fs::directory(dir)?;
        atomic_file::write_with_mode(path, &serde_json::to_vec_pretty(self)?, 0o600)
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
    next_alarm_in_timezone(alarm, after)
}

fn next_alarm_in_timezone<Tz: TimeZone>(
    alarm: &Alarm,
    after: DateTime<Tz>,
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
                if candidate > after {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_alarm_for_a_past_time_waits_until_tomorrow() {
        let now = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
        let mut state = ClockState::default();
        state.alarms.push(Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        });
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        let before = Local.with_ymd_and_hms(2026, 1, 16, 11, 59, 59).unwrap();
        assert!(state.tick(before.with_timezone(&Utc), before).is_empty());
        let deadline = Local.with_ymd_and_hms(2026, 1, 16, 12, 0, 0).unwrap();
        assert!(
            matches!(state.tick(deadline.with_timezone(&Utc), deadline).as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch")
        );
        assert!(!state.alarms[0].enabled);
        assert!(
            state
                .tick(deadline.with_timezone(&Utc), deadline)
                .is_empty()
        );
    }

    #[test]
    fn pending_alarm_survives_restart_on_another_day() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clocks.json");
        let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let mut state = ClockState::default();
        state.alarms.push(Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        });
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        state.save_at(&path).unwrap();
        let mut restored = ClockState::load_at(&path);
        let reopened = Local.with_ymd_and_hms(2026, 1, 16, 8, 0, 0).unwrap();
        assert!(
            matches!(restored.tick(reopened.with_timezone(&Utc), reopened).as_slice(), [DueEvent::Alarm { label, .. }] if label == "Lunch")
        );
        assert!(!restored.alarms[0].enabled);
        assert_eq!(
            restored.alarms[0].last_fired_local_date,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap())
        );
    }

    #[test]
    fn recurring_alarm_catches_up_once_and_persists_the_next_weekday() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("clocks.json");
        let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let mut state = ClockState::default();
        state.alarms.push(Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::from([WeekdayKey::Thu]),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        });
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        let reopened = Local.with_ymd_and_hms(2026, 1, 23, 8, 0, 0).unwrap();
        let events = state.tick(reopened.with_timezone(&Utc), reopened);
        assert!(
            matches!(events.as_slice(), [DueEvent::Alarm { scheduled_for, .. }]
            if *scheduled_for == Local.with_ymd_and_hms(2026, 1, 15, 12, 0, 0).unwrap().with_timezone(&Utc))
        );
        assert_eq!(
            state.alarms[0].next_due_utc,
            Some(
                Local
                    .with_ymd_and_hms(2026, 1, 29, 12, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        state.save_at(&path).unwrap();
        let mut restored = ClockState::load_at(&path);
        assert!(
            restored
                .tick(reopened.with_timezone(&Utc), reopened)
                .is_empty()
        );
        let next = Local.with_ymd_and_hms(2026, 1, 29, 12, 0, 0).unwrap();
        assert_eq!(restored.tick(next.with_timezone(&Utc), next).len(), 1);
        assert!(restored.tick(next.with_timezone(&Utc), next).is_empty());
    }

    #[test]
    fn disabling_and_reenabling_an_alarm_discards_the_old_deadline() {
        let now = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
        let mut state = ClockState::default();
        state.alarms.push(Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: Some(
                Local
                    .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc),
            ),
        });
        state.alarms[0].set_enabled(false);
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        state.alarms[0].set_enabled(true);
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        assert_eq!(
            state.alarms[0].next_due_utc,
            Some(
                Local
                    .with_ymd_and_hms(2026, 1, 16, 12, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
    }

    #[test]
    fn legacy_alarm_state_arms_without_losing_its_settings() {
        let serialized = r#"{"id":"f33f2200-0e21-44b8-a337-fc7ec3e5f625","label":"Lunch","local_time":"12:00:00","weekdays":["Thu"],"enabled":true,"last_fired_local_date":null}"#;
        let alarm: Alarm = serde_json::from_str(serialized).unwrap();
        let mut state = ClockState {
            alarms: vec![alarm],
            ..ClockState::default()
        };
        let now = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        assert!(state.tick(now.with_timezone(&Utc), now).is_empty());
        assert_eq!(
            state.alarms[0].next_due_utc,
            Some(
                Local
                    .with_ymd_and_hms(2026, 1, 15, 12, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
        assert_eq!(state.alarms[0].label, "Lunch");
        assert_eq!(state.alarms[0].weekdays, BTreeSet::from([WeekdayKey::Thu]));
    }

    #[test]
    fn next_alarm_keeps_the_requested_time_after_todays_occurrence() {
        let after = Local.with_ymd_and_hms(2026, 1, 15, 12, 1, 0).unwrap();
        let alarm = Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::new(),
            enabled: true,
            last_fired_local_date: None,
            next_due_utc: None,
        };
        assert_eq!(
            next_alarm_local(&alarm, after),
            Some(Local.with_ymd_and_hms(2026, 1, 16, 12, 0, 0).unwrap())
        );
    }

    #[test]
    fn next_alarm_does_not_repeat_a_delivered_date_after_clock_rollback() {
        let after = Local.with_ymd_and_hms(2026, 1, 15, 11, 0, 0).unwrap();
        let alarm = Alarm {
            id: Uuid::new_v4(),
            label: "Lunch".into(),
            local_time: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: BTreeSet::from([WeekdayKey::Thu]),
            enabled: true,
            last_fired_local_date: Some(after.date_naive()),
            next_due_utc: None,
        };
        assert_eq!(
            next_alarm_local(&alarm, after),
            Some(Local.with_ymd_and_hms(2026, 1, 22, 12, 0, 0).unwrap())
        );
    }

    #[test]
    fn next_alarm_resolves_dst_gaps_and_never_repeats_the_autumn_hour() {
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
            next_alarm_in_timezone(&alarm, spring),
            Some(zone.with_ymd_and_hms(2026, 3, 29, 3, 0, 0).unwrap())
        );
        let autumn = zone.with_ymd_and_hms(2026, 10, 25, 1, 0, 0).unwrap();
        let first = next_alarm_in_timezone(&alarm, autumn).unwrap();
        assert_eq!(
            first.with_timezone(&Utc),
            Utc.with_ymd_and_hms(2026, 10, 25, 0, 30, 0).unwrap()
        );
        assert_eq!(
            next_alarm_in_timezone(&alarm, first),
            Some(zone.with_ymd_and_hms(2026, 10, 26, 2, 30, 0).unwrap())
        );
    }

    #[test]
    fn next_alarm_honors_disabled_and_weekday_settings() {
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
        assert_eq!(next_alarm_in_timezone(&alarm, after), None);
        alarm.enabled = true;
        assert_eq!(
            next_alarm_in_timezone(&alarm, after),
            Some(Utc.with_ymd_and_hms(2026, 1, 17, 12, 0, 0).unwrap())
        );
    }

    #[test]
    fn native_timer_handoff_survives_restart_and_matches_only_its_deadline() {
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
        assert!(matches!(
            restored.tick(later, later.with_timezone(&Local)).as_slice(),
            [DueEvent::Timer {
                native_scheduled: true,
                ..
            }]
        ));
        assert_eq!(restored.timers[0].native_scheduled_end, None);
        assert!(restored.tick(later, later.with_timezone(&Local)).is_empty());

        let mut changed = ClockState::load_at(&path);
        changed.timers[0].pause(now + chrono::Duration::seconds(1));
        changed.timers[0].start(now + chrono::Duration::seconds(20));
        let later = now + chrono::Duration::seconds(60);
        assert!(matches!(
            changed.tick(later, later.with_timezone(&Local)).as_slice(),
            [DueEvent::Timer {
                native_scheduled: false,
                ..
            }]
        ));
    }

    #[test]
    fn saved_timer_survives_replacement_and_restart() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/clocks.json");
        let now = Utc::now();
        let mut state = ClockState::default();
        let mut timer = Timer::new("Tea — Grün", Duration::from_secs(60));
        timer.start(now);
        state.timers.push(timer);
        state.save_at(&path).unwrap();
        assert_eq!(ClockState::load_at(&path), state);

        state.timers[0].label = "Updated tea timer".into();
        state.save_at(&path).unwrap();
        let mut reopened = ClockState::load_at(&path);
        assert_eq!(
            reopened, state,
            "replacing the saved file preserves the complete state"
        );
        let later = now + chrono::Duration::seconds(61);
        assert_eq!(reopened.tick(later, later.with_timezone(&Local)).len(), 1);
        reopened.save_at(&path).unwrap();
        let mut after_delivery = ClockState::load_at(&path);
        assert!(
            after_delivery
                .tick(later, later.with_timezone(&Local))
                .is_empty()
        );
    }

    #[test]
    fn save_failure_is_reported_without_overwriting_a_blocker() {
        let directory = tempfile::tempdir().unwrap();
        let blocker = directory.path().join("not-a-directory");
        fs::write(&blocker, b"keep me").unwrap();
        assert!(
            ClockState::default()
                .save_at(&blocker.join("clocks.json"))
                .is_err()
        );
        assert_eq!(fs::read(blocker).unwrap(), b"keep me");
    }

    #[cfg(windows)]
    #[test]
    fn native_state_path_ignores_relative_overrides() {
        let env = |key: &str| match key {
            "XDG_STATE_HOME" => Some("relative".into()),
            "LOCALAPPDATA" => Some(r"C:\Users\Reader\AppData\Local".into()),
            _ => None,
        };
        assert_eq!(
            state_path_with(env),
            Some(PathBuf::from(
                r"C:\Users\Reader\AppData\Local\tilo\clocks.json"
            ))
        );
        assert_eq!(
            state_path_with(|key| (key == "USERPROFILE").then(|| r"C:\Users\Reader".into())),
            state_path_with(env)
        );
        assert!(state_path_with(|_| None).is_none());
    }

    #[test]
    fn absolute_state_override_is_used_for_the_real_store() {
        let directory = tempfile::tempdir().unwrap();
        let path = state_path_with(|key| {
            (key == "XDG_STATE_HOME").then(|| directory.path().as_os_str().to_owned())
        })
        .unwrap();
        let state = ClockState::default();
        state.save_at(&path).unwrap();
        assert_eq!(path, directory.path().join("tilo/clocks.json"));
        assert_eq!(ClockState::load_at(&path), state);
    }

    #[test]
    fn timer_survives_suspend_by_deadline() {
        let now = Utc::now();
        let mut t = Timer::new("Tea", Duration::from_secs(300));
        t.start(now);
        assert_eq!(
            t.remaining(now + chrono::Duration::seconds(120)),
            Duration::from_secs(180)
        );
    }
    #[test]
    fn timer_fires_once() {
        let now = Utc::now();
        let mut s = ClockState::default();
        let mut t = Timer::new("Tea", Duration::from_secs(1));
        t.start(now);
        s.timers.push(t);
        assert_eq!(
            s.tick(now + chrono::Duration::seconds(2), Local::now())
                .len(),
            1
        );
        assert!(
            s.tick(now + chrono::Duration::seconds(3), Local::now())
                .is_empty()
        );
    }
    #[test]
    fn paused_timer_does_not_decay() {
        let now = Utc::now();
        let t = Timer::new("", Duration::from_secs(9));
        assert_eq!(
            t.remaining(now + chrono::Duration::hours(1)),
            Duration::from_secs(9)
        );
    }

    #[test]
    fn paused_pinned_timer_remains_in_shell_and_is_marked_resumable() {
        let now = Utc::now();
        let mut state = ClockState::default();
        let mut timer = Timer::new("Tea", Duration::from_secs(90));
        timer.pinned_overlay = true;
        state.timers.push(timer);
        let activity = state.shell_activity(now).unwrap();
        assert_eq!(activity.remaining, Duration::from_secs(90));
        assert!(activity.overlay);
        assert!(activity.paused);
        assert!(!activity.stopwatch);
    }
    #[test]
    fn stopwatch_accumulates_across_pause_and_resume() {
        let now = Utc::now();
        let mut stopwatch = Stopwatch::default();
        stopwatch.start(now);
        stopwatch.pause(now + chrono::Duration::seconds(12));
        stopwatch.start(now + chrono::Duration::seconds(20));
        assert_eq!(
            stopwatch.elapsed(now + chrono::Duration::seconds(25)),
            Duration::from_secs(17)
        );
    }

    #[test]
    fn paused_pinned_stopwatch_remains_in_shell_and_is_marked_resumable() {
        let now = Utc::now();
        let mut state = ClockState::default();
        state.stopwatch.start(now);
        state.stopwatch.pause(now + chrono::Duration::seconds(12));
        state.stopwatch.pinned_overlay = true;
        let activity = state
            .shell_activity(now + chrono::Duration::minutes(1))
            .unwrap();
        assert_eq!(activity.remaining, Duration::from_secs(12));
        assert!(activity.paused);
        assert!(activity.stopwatch);
    }
    #[test]
    fn stopwatch_laps_are_cumulative_and_reset_clears_them() {
        let now = Utc::now();
        let mut stopwatch = Stopwatch::default();
        stopwatch.start(now);
        stopwatch.lap(now + chrono::Duration::seconds(3));
        stopwatch.lap(now + chrono::Duration::seconds(8));
        assert_eq!(stopwatch.laps_ms, vec![3_000, 8_000]);
        stopwatch.reset();
        assert_eq!(stopwatch, Stopwatch::default());
    }

    #[test]
    fn floating_stopwatch_commands_never_mutate_a_timer() {
        let now = Utc::now();
        let mut state = ClockState::default();
        let mut timer = Timer::new("Tea", Duration::from_secs(300));
        timer.start(now);
        timer.pinned_overlay = true;
        state.timers.push(timer.clone());
        state.stopwatch.start(now);
        state.stopwatch.pinned_overlay = true;

        state.apply_shell_command("lap-stopwatch", None, now + chrono::Duration::seconds(12));
        assert_eq!(state.stopwatch.laps_ms, vec![12_000]);
        assert_eq!(state.timers.first(), Some(&timer));

        state.apply_shell_command("close-stopwatch", None, now);
        assert!(!state.stopwatch.pinned_overlay);
        assert!(
            state
                .timers
                .first()
                .is_some_and(|timer| timer.pinned_overlay)
        );
    }

    #[test]
    fn shell_snapshot_tombstone_is_backward_compatible() {
        let legacy = r#"{"version":1,"label":"Tea","remaining_secs":30,"stopwatch":false,"overlay":true,"generation":4,"producer_pid":7}"#;
        let parsed: ShellSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(parsed.active, "older live snapshots remain active");
        assert!(parsed.notification_id.is_none());

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
        assert!(encoded.contains("\"active\":false"));
    }

    #[test]
    fn due_notice_survives_an_inactive_shell_snapshot() {
        let notification_id = Uuid::new_v4();
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
        assert!(!decoded.active);
        assert_eq!(decoded.notification_id, Some(notification_id));
        assert_eq!(decoded.notification_title, "Timer finished");
        assert_eq!(decoded.notification_body, "Tea");
    }

    #[test]
    fn floating_commands_mutate_the_timer_named_by_the_shell_snapshot() {
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
        assert_eq!(activity.activity_id, Some(sooner_id));
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

    #[test]
    fn explicitly_floated_timer_owns_overlay_over_unpinned_urgent_timer() {
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
        assert_eq!(activity.activity_id, Some(floated_id));
        assert_eq!(activity.label, "Focus");
        assert!(activity.overlay);
    }
}
