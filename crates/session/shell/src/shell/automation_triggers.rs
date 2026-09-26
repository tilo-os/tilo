//! What starts a saved shortcut without anyone invoking it.
//!
//! A [`TriggerSource`] watches for one kind of event and turns it into
//! [`Firing`]s for the shortcuts that are armed on this device. Sources are
//! pure over what they are shown: the session hands them events (a pressed
//! chord, the passing of time read from an injected [`Clock`]) and the
//! shortcuts it has armed, and starts a run for every firing through the
//! action broker, which checks again that the shortcut still exists and that
//! its saver may still run it.
//!
//! Only shortcuts saved by the person this device acts for in their room are
//! armed ([`armed`]), so a collaborator's shortcut never fires on someone
//! else's behalf, and deleting a shortcut disarms it at the next event.
//!
//! - [`KeyboardTriggers`] fires on a chord in the shell's own notation.
//! - [`ScheduleTriggers`] fires at intervals or at local wall-clock times.
//!   Each schedule's next fire time is persisted before the firing is handed
//!   over, so a restart neither repeats a fire nor forgets one; a fire time
//!   missed while the device was off or asleep is skipped or run once,
//!   according to the trigger's [`CatchUp`] policy.
//!
//! A shortcut reaches every device of the person who saved it, and each of
//! them arms it. A schedule still runs once per fire time: the saver's
//! online devices ([`PersonalDevices`]) rank themselves for each fire time
//! by a hash of it, the first runs it at once and records a
//! [`ScheduleClaim`] in the workspace, which syncs like the shortcut, and
//! each other device waits [`CLAIM_GRACE`] per rank before taking a fire
//! time nobody has claimed — so an elected device that is asleep or offline
//! is covered by the next one.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{
    DateTime, Datelike as _, Duration as ChronoDuration, FixedOffset, Local, NaiveDate, NaiveTime,
    Offset as _, TimeZone as _, Utc,
};
use serde::{Deserialize, Serialize};
use settings_store::hardware::KeyChord;
use sha2::{Digest as _, Sha256};
use tilo_automation::{CatchUp, RunId, Schedule, ScheduleClaim, Shortcut, TriggerSpec, Weekday};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER: ChronoDuration = ChronoDuration::seconds(90);

/// The shortest interval a schedule may repeat at.
pub const MIN_INTERVAL_SECONDS: u64 = 60;

/// How long each device waits, per device ranked before it, for a fire
/// time to be claimed before it takes it.
pub const CLAIM_GRACE: ChronoDuration = ChronoDuration::seconds(45);

/// The person's certified devices, as this device sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalDevices {
    /// This device, or `None` when it has no certified identity and so
    /// shares its schedules with no other device.
    pub local: Option<String>,
    /// The person's devices that are online now; this one counts whether
    /// listed or not.
    pub online: BTreeSet<String>,
}

impl PersonalDevices {
    /// This device alone.
    #[must_use]
    pub fn alone(local: impl Into<String>) -> Self {
        Self {
            local: Some(local.into()),
            online: BTreeSet::new(),
        }
    }
}

/// Where trigger sources learn which of the person's devices are online.
pub trait DeviceDirectory: Send + Sync {
    fn personal_devices(&self) -> PersonalDevices;
}

/// A fixed set of devices; a device with no certified identity by default.
#[derive(Debug, Default)]
pub struct FixedDevices(Mutex<PersonalDevices>);

impl FixedDevices {
    #[must_use]
    pub fn new(devices: PersonalDevices) -> Self {
        Self(Mutex::new(devices))
    }

    pub fn set(&self, devices: PersonalDevices) {
        if let Ok(mut current) = self.0.lock() {
            *current = devices;
        }
    }
}

impl DeviceDirectory for FixedDevices {
    fn personal_devices(&self) -> PersonalDevices {
        self.0
            .lock()
            .map_or_else(|_| PersonalDevices::default(), |devices| devices.clone())
    }
}

/// Where trigger sources read the time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;

    /// The local wall-clock offset in effect at `at`.
    fn offset_at(&self, at: DateTime<Utc>) -> FixedOffset;
}

/// The system clock, in the session's local time zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }

    fn offset_at(&self, at: DateTime<Utc>) -> FixedOffset {
        Local.offset_from_utc_datetime(&at.naive_utc()).fix()
    }
}

/// A clock that moves only when told to, in a fixed offset.
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<DateTime<Utc>>,
    offset: FixedOffset,
}

impl ManualClock {
    #[must_use]
    pub fn new(now: DateTime<Utc>, offset: FixedOffset) -> Self {
        Self {
            now: Mutex::new(now),
            offset,
        }
    }

    pub fn set(&self, now: DateTime<Utc>) {
        if let Ok(mut current) = self.now.lock() {
            *current = now;
        }
    }

    pub fn advance(&self, by: ChronoDuration) {
        if let Ok(mut current) = self.now.lock() {
            *current = current.checked_add_signed(by).unwrap_or(*current);
        }
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        self.now.lock().map_or_else(|_| Utc::now(), |now| *now)
    }

    fn offset_at(&self, _at: DateTime<Utc>) -> FixedOffset {
        self.offset
    }
}

/// A shortcut this device may start, with the room it is saved in and the
/// fire times of that room's schedules already claimed.
#[derive(Clone, Debug)]
pub struct Armed<'a> {
    pub room: String,
    pub shortcut: &'a Shortcut,
    pub claims: &'a [ScheduleClaim],
}

/// Every shortcut in `workspaces` with triggers that was saved by the person
/// this device acts for in its room (`local_person`).
pub fn armed(
    workspaces: &[crate::shell::state::Workspace],
    local_person: impl Fn(&crate::shell::state::Workspace) -> String,
) -> Vec<Armed<'_>> {
    workspaces
        .iter()
        .flat_map(|workspace| {
            let person = local_person(workspace);
            let room = workspace.room.to_string();
            let claims = workspace.schedule_claims.as_slice();
            workspace
                .shortcuts
                .iter()
                .filter(move |shortcut| {
                    !shortcut.triggers.is_empty()
                        && shortcut.saved_by.as_deref() == Some(person.as_str())
                })
                .map(move |shortcut| Armed {
                    room: room.clone(),
                    shortcut,
                    claims,
                })
        })
        .collect()
}

/// What a source is shown.
#[derive(Clone, Copy, Debug)]
pub enum TriggerEvent<'a> {
    /// Time passed; read the clock.
    Tick,
    /// A chord was pressed that nothing else in the shell took.
    Key(&'a KeyChord),
}

/// One trigger that fired: run `shortcut` in `room`, as run `run`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firing {
    pub room: String,
    pub shortcut: String,
    pub trigger: TriggerSpec,
    pub run: RunId,
    /// The fire time this device takes, to record in the room
    /// ([`WorkspaceTree::claim_schedule`](crate::shell::state::WorkspaceTree::claim_schedule))
    /// before the run starts.
    pub claim: Option<ScheduleClaim>,
}

/// What every source reads besides the event.
#[derive(Clone, Copy)]
pub struct TriggerContext<'a> {
    pub clock: &'a dyn Clock,
    pub devices: &'a PersonalDevices,
}

/// Turns one kind of event into firings.
pub trait TriggerSource: Send {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        context: TriggerContext<'_>,
    ) -> Vec<Firing>;
}

/// Fires a shortcut whose keyboard trigger names the pressed chord.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardTriggers;

impl TriggerSource for KeyboardTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Key(pressed) = event else {
            return Vec::new();
        };
        armed
            .iter()
            .flat_map(|armed| {
                armed.shortcut.triggers.iter().filter_map(move |trigger| {
                    let TriggerSpec::Keyboard { chord } = trigger else {
                        return None;
                    };
                    (KeyChord::parse(chord).ok().as_ref() == Some(pressed)).then(|| Firing {
                        room: armed.room.clone(),
                        shortcut: armed.shortcut.id.clone(),
                        trigger: trigger.clone(),
                        run: RunId::new(),
                        claim: None,
                    })
                })
            })
            .collect()
    }
}

/// One schedule's persisted state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LedgerEntry {
    /// The trigger as it was when armed; a changed schedule starts over.
    trigger: TriggerSpec,
    /// When it fires next, in milliseconds since the Unix epoch.
    next_fire_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ScheduleLedger {
    /// Keyed by `room/shortcut/trigger index`.
    #[serde(default)]
    schedules: BTreeMap<String, LedgerEntry>,
}

/// Fires scheduled triggers when their time comes, exactly once per fire
/// time, across restarts.
#[derive(Debug)]
pub struct ScheduleTriggers {
    ledger: ScheduleLedger,
    path: Option<PathBuf>,
}

impl ScheduleTriggers {
    /// Schedules persisted at `path`, or kept in memory only for `None`.
    #[must_use]
    pub fn open(path: Option<PathBuf>) -> Self {
        let ledger = path
            .as_ref()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self { ledger, path }
    }

    /// When the schedule `key` fires next, when it is armed.
    #[must_use]
    pub fn next_fire(&self, room: &str, shortcut: &str, index: usize) -> Option<DateTime<Utc>> {
        self.ledger
            .schedules
            .get(&ledger_key(room, shortcut, index))
            .and_then(|entry| DateTime::from_timestamp_millis(entry.next_fire_ms))
    }

    fn persist(&self) {
        let Some(path) = &self.path else {
            return;
        };
        let written = serde_json::to_vec_pretty(&self.ledger)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let staged = path.with_extension("json.tmp");
                std::fs::write(&staged, bytes)?;
                std::fs::rename(&staged, path)
            });
        if let Err(error) = written {
            tracing::warn!(%error, path = %path.display(), "shortcut schedules were not saved");
        }
    }
}

impl TriggerSource for ScheduleTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Tick = event else {
            return Vec::new();
        };
        let clock = context.clock;
        let now = clock.now();
        let mut firings = Vec::new();
        let mut live = BTreeSet::new();
        let mut changed = false;
        for armed in armed {
            'triggers: for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::Schedule { schedule, catch_up } = trigger else {
                    continue;
                };
                let key = ledger_key(&armed.room, &armed.shortcut.id, index);
                live.insert(key.clone());
                let current = self
                    .ledger
                    .schedules
                    .get(&key)
                    .filter(|entry| entry.trigger == *trigger)
                    .and_then(|entry| DateTime::from_timestamp_millis(entry.next_fire_ms));
                let next = match current {
                    Some(due) if now < due => continue,
                    Some(due) => 'due: {
                        let Some(wait) = turn(&key, armed, index, due, context.devices) else {
                            // Another of the person's devices took it.
                            break 'due next_fire_time(schedule, now, clock);
                        };
                        let Some(start) = due.checked_add_signed(wait) else {
                            continue 'triggers;
                        };
                        if now < start {
                            continue 'triggers;
                        }
                        let on_time = now.signed_duration_since(start) <= MISSED_AFTER;
                        if on_time || *catch_up == CatchUp::RunOnce {
                            firings.push(Firing {
                                room: armed.room.clone(),
                                shortcut: armed.shortcut.id.clone(),
                                trigger: trigger.clone(),
                                run: scheduled_run(&key, due),
                                claim: context.devices.local.as_ref().map(|device| ScheduleClaim {
                                    shortcut: armed.shortcut.id.clone(),
                                    trigger: index,
                                    due_ms: due.timestamp_millis(),
                                    device: device.clone(),
                                }),
                            });
                        }
                        next_fire_time(schedule, now, clock)
                    }
                    None => next_fire_time(schedule, now, clock),
                };
                let Some(next) = next else {
                    continue;
                };
                self.ledger.schedules.insert(
                    key,
                    LedgerEntry {
                        trigger: trigger.clone(),
                        next_fire_ms: next.timestamp_millis(),
                    },
                );
                changed = true;
            }
        }
        let before = self.ledger.schedules.len();
        self.ledger.schedules.retain(|key, _| live.contains(key));
        changed |= self.ledger.schedules.len() != before;
        // Persisted before anything runs: a crash after this point loses at
        // most the run, never repeats it.
        if changed {
            self.persist();
        }
        firings
    }
}

fn ledger_key(room: &str, shortcut: &str, index: usize) -> String {
    format!("{room}/{shortcut}/{index}")
}

/// How long after `due` this device takes the fire time of schedule `key`
/// (trigger `index` of `armed`), or `None` when one of the person's devices
/// already claimed it. Every device ranks the person's online devices the
/// same way for a fire time, so they agree on who goes first without
/// talking; ranking by fire time spreads schedules across them.
fn turn(
    key: &str,
    armed: &Armed<'_>,
    index: usize,
    due: DateTime<Utc>,
    devices: &PersonalDevices,
) -> Option<ChronoDuration> {
    let Some(local) = devices.local.as_deref() else {
        return Some(ChronoDuration::zero());
    };
    let due_ms = due.timestamp_millis();
    let claimed = armed.claims.iter().any(|claim| {
        claim.shortcut == armed.shortcut.id && claim.trigger == index && claim.due_ms >= due_ms
    });
    if claimed {
        return None;
    }
    let local_rank = runner_rank(key, due_ms, local);
    let ahead = devices
        .online
        .iter()
        .filter(|device| device.as_str() != local)
        .filter(|device| runner_rank(key, due_ms, device) < local_rank)
        .count();
    CLAIM_GRACE.checked_mul(i32::try_from(ahead).ok()?)
}

/// Where `device` stands for one fire time of a schedule; lowest first.
fn runner_rank(key: &str, due_ms: i64, device: &str) -> ([u8; 32], String) {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.shortcut.runner\0");
    hasher.update(key.as_bytes());
    hasher.update(due_ms.to_be_bytes());
    hasher.update(device.as_bytes());
    (hasher.finalize().into(), device.to_string())
}

/// The run a scheduled fire starts: the same for the same fire time, so a
/// fire that is somehow handed over twice reuses every invocation id.
fn scheduled_run(key: &str, due: DateTime<Utc>) -> RunId {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.shortcut.schedule\0");
    hasher.update(key.as_bytes());
    hasher.update(due.timestamp_millis().to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *digested;
    }
    RunId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

/// The first fire time of `schedule` strictly after `now`. An interval
/// counts from the Unix epoch, so every device computes the same fire times
/// whenever it armed the schedule.
#[must_use]
pub fn next_fire_time(
    schedule: &Schedule,
    now: DateTime<Utc>,
    clock: &dyn Clock,
) -> Option<DateTime<Utc>> {
    match schedule {
        Schedule::Interval { seconds } => {
            let period = i64::try_from((*seconds).max(MIN_INTERVAL_SECONDS)).ok()?;
            let periods = now.timestamp().div_euclid(period).checked_add(1)?;
            DateTime::from_timestamp(period.checked_mul(periods)?, 0)
        }
        Schedule::Calendar { at, weekdays } => {
            let time = NaiveTime::from_hms_opt(u32::from(at.hour), u32::from(at.minute), 0)?;
            let today = now.with_timezone(&clock.offset_at(now)).date_naive();
            (0..=8_i64)
                .filter_map(|day| today.checked_add_signed(ChronoDuration::days(day)))
                .filter(|date| weekdays.is_empty() || weekdays.contains(&weekday_of(*date)))
                .filter_map(|date| local_instant(date, time, clock))
                .find(|candidate| *candidate > now)
        }
    }
}

/// `date` at `time` on the local wall clock, as an instant.
fn local_instant(date: NaiveDate, time: NaiveTime, clock: &dyn Clock) -> Option<DateTime<Utc>> {
    let naive = date.and_time(time);
    // Read the offset at a first guess of the instant, so a change of
    // offset between now and then (daylight saving) is taken into account.
    let first_offset = clock.offset_at(naive.and_utc()).local_minus_utc();
    let guess = naive
        .and_utc()
        .checked_sub_signed(ChronoDuration::seconds(i64::from(first_offset)))?;
    let offset = clock.offset_at(guess);
    offset
        .from_local_datetime(&naive)
        .single()
        .map(|local| local.with_timezone(&Utc))
}

fn weekday_of(date: NaiveDate) -> Weekday {
    match date.weekday() {
        chrono::Weekday::Mon => Weekday::Monday,
        chrono::Weekday::Tue => Weekday::Tuesday,
        chrono::Weekday::Wed => Weekday::Wednesday,
        chrono::Weekday::Thu => Weekday::Thursday,
        chrono::Weekday::Fri => Weekday::Friday,
        chrono::Weekday::Sat => Weekday::Saturday,
        chrono::Weekday::Sun => Weekday::Sunday,
    }
}

/// Why a trigger cannot be saved on a shortcut, when it cannot.
///
/// # Errors
///
/// The reason, for the person saving it.
pub fn validate_trigger(trigger: &TriggerSpec) -> Result<(), String> {
    match trigger {
        TriggerSpec::Keyboard { chord } => KeyChord::parse(chord)
            .map(drop)
            .map_err(|reason| format!("keyboard trigger `{chord}`: {reason}")),
        TriggerSpec::Schedule {
            schedule: Schedule::Interval { seconds },
            ..
        } => {
            if *seconds < MIN_INTERVAL_SECONDS {
                Err(format!(
                    "a schedule repeats at most once every {MIN_INTERVAL_SECONDS} seconds"
                ))
            } else {
                Ok(())
            }
        }
        TriggerSpec::Schedule {
            schedule: Schedule::Calendar { at, .. },
            ..
        } => {
            if at.hour < 24 && at.minute < 60 {
                Ok(())
            } else {
                Err(format!(
                    "{:02}:{:02} is not a time of day",
                    at.hour, at.minute
                ))
            }
        }
        other => Err(format!(
            "`{}` triggers cannot be saved yet; keyboard and schedule triggers can",
            other.kind()
        )),
    }
}

/// Every trigger source a session runs, reading one clock and one view of
/// the person's devices.
pub struct TriggerHost {
    clock: Arc<dyn Clock>,
    devices: Arc<dyn DeviceDirectory>,
    sources: Vec<Box<dyn TriggerSource>>,
}

impl TriggerHost {
    /// Keyboard and schedule triggers on `clock`, with schedules persisted
    /// at `schedules` (in memory only for `None`).
    #[must_use]
    pub fn new(clock: Arc<dyn Clock>, schedules: Option<PathBuf>) -> Self {
        Self {
            clock,
            devices: Arc::new(FixedDevices::default()),
            sources: vec![
                Box::new(KeyboardTriggers),
                Box::new(ScheduleTriggers::open(schedules)),
            ],
        }
    }

    /// The session's triggers: the system clock, schedules persisted under
    /// the launcher-provided state root.
    #[must_use]
    pub fn for_session() -> Self {
        let schedules = std::env::var_os("XDG_STATE_HOME")
            .map(|state| PathBuf::from(state).join("tilo/automation/schedules.json"));
        Self::new(Arc::new(SystemClock), schedules)
    }

    /// Read the person's devices from `devices`, so their schedules run
    /// once across them.
    pub fn set_devices(&mut self, devices: Arc<dyn DeviceDirectory>) {
        self.devices = devices;
    }

    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Show `event` to every source; every firing, in source order.
    pub fn observe(&mut self, event: TriggerEvent<'_>, armed: &[Armed<'_>]) -> Vec<Firing> {
        let clock = Arc::clone(&self.clock);
        let devices = self.devices.personal_devices();
        let context = TriggerContext {
            clock: clock.as_ref(),
            devices: &devices,
        };
        self.sources
            .iter_mut()
            .flat_map(|source| source.observe(event, armed, context))
            .collect()
    }
}

impl std::fmt::Debug for TriggerHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TriggerHost")
            .field("sources", &self.sources.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_automation::TimeOfDay;

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .expect("an RFC 3339 instant")
            .with_timezone(&Utc)
    }

    fn shortcut(trigger: &TriggerSpec) -> Shortcut {
        serde_json::from_value(serde_json::json!({
            "id": "tidy",
            "name": "Tidy",
            "steps": [],
            "triggers": [trigger],
            "saved_by": "person:me"
        }))
        .expect("shortcut JSON")
    }

    #[test]
    fn a_calendar_schedule_fires_at_the_next_listed_local_time() {
        // 2026-09-25 is a Friday; the local clock is two hours ahead.
        let clock = ManualClock::new(
            at("2026-09-25T10:00:00Z"),
            FixedOffset::east_opt(2 * 3600).expect("offset"),
        );
        let weekday_mornings = Schedule::Calendar {
            at: TimeOfDay {
                hour: 9,
                minute: 30,
            },
            weekdays: vec![Weekday::Monday, Weekday::Friday],
        };
        assert_eq!(
            next_fire_time(&weekday_mornings, clock.now(), &clock),
            Some(at("2026-09-28T07:30:00Z")),
            "Friday 09:30 local has passed, so Monday's is next"
        );
        let daily_evening = Schedule::Calendar {
            at: TimeOfDay {
                hour: 18,
                minute: 0,
            },
            weekdays: Vec::new(),
        };
        assert_eq!(
            next_fire_time(&daily_evening, clock.now(), &clock),
            Some(at("2026-09-25T16:00:00Z"))
        );
    }

    #[test]
    fn an_interval_fires_on_the_same_times_wherever_it_was_armed() {
        let clock = ManualClock::new(
            at("2026-09-25T10:00:00Z"),
            FixedOffset::east_opt(0).expect("offset"),
        );
        let hourly = Schedule::Interval { seconds: 3600 };
        assert_eq!(
            next_fire_time(&hourly, clock.now(), &clock),
            Some(at("2026-09-25T11:00:00Z"))
        );
        assert_eq!(
            next_fire_time(&hourly, at("2026-09-25T09:30:12Z"), &clock),
            Some(at("2026-09-25T10:00:00Z")),
            "armed half way through a period, it still fires on the hour"
        );
    }

    #[test]
    fn a_keyboard_trigger_fires_on_its_chord_only() {
        let saved = shortcut(&TriggerSpec::Keyboard {
            chord: "Super+Shift+N".into(),
        });
        let armed = [Armed {
            room: "room".into(),
            shortcut: &saved,
            claims: &[],
        }];
        let clock = SystemClock;
        let devices = PersonalDevices::default();
        let context = TriggerContext {
            clock: &clock,
            devices: &devices,
        };
        let mut keys = KeyboardTriggers;
        let chord = KeyChord::parse("super+shift+n").expect("chord");
        let fired = keys.observe(TriggerEvent::Key(&chord), &armed, context);
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].shortcut, "tidy");
        let other = KeyChord::parse("Super+Shift+M").expect("chord");
        assert!(
            keys.observe(TriggerEvent::Key(&other), &armed, context)
                .is_empty()
        );
        assert!(keys.observe(TriggerEvent::Tick, &armed, context).is_empty());
    }

    /// One of the person's devices: its own copy of the personal
    /// workspace, its own trigger host and its own view of which devices
    /// are online, on a clock all devices share.
    struct Device {
        tree: crate::shell::state::WorkspaceTree,
        triggers: TriggerHost,
        ran: Vec<Firing>,
    }

    const PERSONAL: &str = "5c07c075-0000-4000-8000-000000000042";

    fn personal_room() -> crate::shell::state::RoomId {
        crate::shell::state::RoomId::parse(PERSONAL).expect("room id")
    }

    impl Device {
        fn new(name: &str, online: &[&str], clock: &Arc<ManualClock>) -> Self {
            let mut tree = crate::shell::state::WorkspaceTree::default();
            let workspace = tree.append_empty_workspace("Personal");
            assert!(tree.set_room(workspace, personal_room()));
            let mut triggers = TriggerHost::new(Arc::clone(clock) as Arc<dyn Clock>, None);
            triggers.set_devices(Arc::new(FixedDevices::new(PersonalDevices {
                local: Some(name.to_string()),
                online: online.iter().map(ToString::to_string).collect(),
            })));
            Self {
                tree,
                triggers,
                ran: Vec::new(),
            }
        }

        /// Show the device the passing time, and take what fired the way
        /// the session does: claim it in the room, then run it.
        fn tick(&mut self) {
            let fired = {
                let armed = armed(&self.tree.desktop.workspaces, |_| "person:me".into());
                self.triggers.observe(TriggerEvent::Tick, &armed)
            };
            for firing in fired {
                if let Some(claim) = &firing.claim {
                    self.tree.claim_schedule(personal_room(), claim);
                }
                self.ran.push(firing);
            }
        }
    }

    /// Carry `from`'s copy of the personal room to `to`.
    fn sync(from: &mut Device, to: &mut Device) {
        let changes = from
            .tree
            .export_room_changes_since(personal_room(), &[])
            .expect("the device is in the room");
        to.tree
            .apply_room_changes(
                personal_room(),
                &changes,
                &crate::shell::state::PeerOrigin::hub(1),
            )
            .expect("the room's changes apply");
    }

    fn laptop_and_desktop(clock: &Arc<ManualClock>) -> (Device, Device) {
        let both = ["laptop", "desktop"];
        let mut laptop = Device::new("laptop", &both, clock);
        let mut desktop = Device::new("desktop", &both, clock);
        let workspace = laptop.tree.desktop.workspaces[0].id;
        assert!(laptop.tree.save_workspace_shortcut(
            workspace,
            &shortcut(&TriggerSpec::Schedule {
                schedule: Schedule::Interval { seconds: 3600 },
                catch_up: CatchUp::Skip,
            })
        ));
        sync(&mut laptop, &mut desktop);
        assert_eq!(
            desktop.tree.desktop.workspaces[0].shortcuts.len(),
            1,
            "the shortcut reached the other device"
        );
        (laptop, desktop)
    }

    fn hourly_key() -> String {
        ledger_key(PERSONAL, "tidy", 0)
    }

    /// Which of `devices` goes first for the hourly schedule's fire time `due`.
    fn elected<'a>(devices: &[&'a str], due: DateTime<Utc>) -> &'a str {
        devices
            .iter()
            .copied()
            .min_by_key(|device| runner_rank(&hourly_key(), due.timestamp_millis(), device))
            .expect("a device")
    }

    #[test]
    fn a_personal_schedule_runs_once_per_fire_time_across_two_devices() {
        let clock = Arc::new(ManualClock::new(
            at("2026-09-25T08:00:10Z"),
            FixedOffset::east_opt(0).expect("offset"),
        ));
        let (mut laptop, mut desktop) = laptop_and_desktop(&clock);
        let mut runners = BTreeSet::new();
        for hour in 0..6_i64 {
            let due = at("2026-09-25T09:00:00Z") + ChronoDuration::hours(hour);
            // Both devices look every quarter minute until five minutes past
            // each fire time, exchanging the room after each look.
            clock.set(due - ChronoDuration::seconds(15));
            for _ in 0..21 {
                laptop.tick();
                sync(&mut laptop, &mut desktop);
                desktop.tick();
                sync(&mut desktop, &mut laptop);
                clock.advance(ChronoDuration::seconds(15));
            }
            let runs: Vec<&Firing> = laptop.ran.iter().chain(&desktop.ran).collect();
            assert_eq!(
                runs.len(),
                usize::try_from(hour + 1).expect("hours fit"),
                "one run per fire time: {runs:?}"
            );
            let this_run = scheduled_run(&hourly_key(), due);
            let runner = if laptop.ran.iter().any(|run| run.run == this_run) {
                "laptop"
            } else {
                "desktop"
            };
            assert_eq!(runner, elected(&["laptop", "desktop"], due));
            runners.insert(runner);
        }
        assert_eq!(
            desktop.tree.desktop.workspaces[0].schedule_claims.len(),
            1,
            "one claim per schedule, the latest fire time's"
        );
        assert_eq!(runners.len(), 2, "fire times are spread over both devices");
    }

    #[test]
    fn when_the_elected_device_is_asleep_another_runs_the_fire_time_once() {
        let clock = Arc::new(ManualClock::new(
            at("2026-09-25T08:00:10Z"),
            FixedOffset::east_opt(0).expect("offset"),
        ));
        let (mut laptop, mut desktop) = laptop_and_desktop(&clock);
        laptop.tick();
        desktop.tick();
        let due = at("2026-09-25T09:00:00Z");
        let (asleep, awake) = if elected(&["laptop", "desktop"], due) == "laptop" {
            (&mut laptop, &mut desktop)
        } else {
            (&mut desktop, &mut laptop)
        };

        // The elected device still looks online to the other, but neither
        // looks at the time nor syncs.
        clock.set(due);
        awake.tick();
        clock.set(due + CLAIM_GRACE - ChronoDuration::seconds(1));
        awake.tick();
        assert!(awake.ran.is_empty(), "the other device waits its turn");
        clock.set(due + CLAIM_GRACE);
        awake.tick();
        assert_eq!(awake.ran.len(), 1, "then takes the unclaimed fire time");
        for _ in 0..10 {
            clock.advance(ChronoDuration::seconds(15));
            awake.tick();
        }
        assert_eq!(awake.ran.len(), 1, "and runs it once");

        // The elected device wakes and learns the fire time was taken.
        sync(awake, asleep);
        asleep.tick();
        assert!(
            asleep.ran.is_empty(),
            "the woken device does not run it again"
        );
    }

    #[test]
    fn only_keyboard_and_schedule_triggers_can_be_saved() {
        assert!(
            validate_trigger(&TriggerSpec::Keyboard {
                chord: "Super+Shift+N".into()
            })
            .is_ok()
        );
        assert!(validate_trigger(&TriggerSpec::Keyboard { chord: "n".into() }).is_err());
        assert!(
            validate_trigger(&TriggerSpec::Schedule {
                schedule: Schedule::Interval { seconds: 5 },
                catch_up: CatchUp::Skip,
            })
            .is_err()
        );
        assert!(
            validate_trigger(&TriggerSpec::Schedule {
                schedule: Schedule::Calendar {
                    at: TimeOfDay {
                        hour: 24,
                        minute: 0
                    },
                    weekdays: Vec::new(),
                },
                catch_up: CatchUp::Skip,
            })
            .is_err()
        );
        assert!(
            validate_trigger(&TriggerSpec::AppOpened {
                app_id: "so.tilo.notes".into()
            })
            .is_err()
        );
    }
}
