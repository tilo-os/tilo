//! Durable, wall-clock-safe timekeeping engine shared by Clocks and shell chrome.
//! UI code supplies wall and monotonic time explicitly, which keeps suspend,
//! clock corrections, tests, and alarm delivery deterministic.

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;

pub mod actions;
pub mod app;
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
    pub stopwatch: Stopwatch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DueEvent {
    Alarm { id: Uuid, label: String },
    Timer { id: Uuid, label: String },
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
}

fn snapshot_active_default() -> bool {
    true
}

/// Session-private app→shell transport. The runtime directory is already
/// per-login; owner/mode checks on the shell side prevent forged status.
pub mod transport {
    use super::ShellSnapshot;
    use std::fs::{self, OpenOptions};
    use std::io::{self, Write};
    #[cfg(unix)]
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    use std::path::PathBuf;

    #[must_use]
    pub fn path() -> Option<PathBuf> {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .map(|p| p.join("qos/clocks.json"))
    }
    pub fn publish(snapshot: &ShellSnapshot) -> io::Result<()> {
        let path = path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "XDG_RUNTIME_DIR missing"))?;
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid clocks path"))?;
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let tmp = path.with_extension("json.new");
        let mut opts = OpenOptions::new();
        opts.create(true).truncate(true).write(true);
        #[cfg(unix)]
        opts.mode(0o600);
        let mut file = opts.open(&tmp)?;
        serde_json::to_writer(&mut file, snapshot)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(tmp, path)
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
            .join("qos/clocks-command.json");
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
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".local/state"))
        })
        .map(|p| p.join("qos/clocks.json"))
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
        state_path()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
    pub fn save(&self) -> io::Result<()> {
        let path = state_path()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no state directory"))?;
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid state path"))?;
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let tmp = path.with_extension("json.new");
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&tmp)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.flush()?;
        file.sync_all()?;
        fs::rename(tmp, path)
    }
}

impl ClockState {
    /// Returns due events exactly once and updates durable delivery markers.
    pub fn tick(&mut self, now_utc: DateTime<Utc>, now_local: DateTime<Local>) -> Vec<DueEvent> {
        let mut due = Vec::new();
        for alarm in &mut self.alarms {
            let today = now_local.date_naive();
            let repeats_today =
                alarm.weekdays.is_empty() || alarm.weekdays.contains(&now_local.weekday().into());
            if alarm.enabled
                && repeats_today
                && alarm.last_fired_local_date != Some(today)
                && now_local.time() >= alarm.local_time
            {
                due.push(DueEvent::Alarm {
                    id: alarm.id,
                    label: alarm.label.clone(),
                });
                alarm.last_fired_local_date = Some(today);
                if alarm.weekdays.is_empty() {
                    alarm.enabled = false;
                }
            }
        }
        for timer in &mut self.timers {
            if timer.ends_at_utc.is_some_and(|end| end <= now_utc) {
                timer.ends_at_utc = None;
                timer.paused_remaining_ms = Some(0);
                due.push(DueEvent::Timer {
                    id: timer.id,
                    label: timer.label.clone(),
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
    if !alarm.enabled {
        return None;
    }
    for day in 0..=8_i64 {
        let date = after
            .date_naive()
            .checked_add_signed(chrono::Duration::days(day))?;
        if !alarm.weekdays.is_empty() && !alarm.weekdays.contains(&date.weekday().into()) {
            continue;
        }
        let mut naive = date.and_time(alarm.local_time);
        for _ in 0..180 {
            if let Some(candidate) = Local.from_local_datetime(&naive).earliest() {
                if candidate > after {
                    return Some(candidate);
                }
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
        };
        let encoded = serde_json::to_string(&inactive).unwrap();
        assert!(encoded.contains("\"active\":false"));
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
