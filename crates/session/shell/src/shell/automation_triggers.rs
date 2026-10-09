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
//! - [`AppOpenedTriggers`] fires when an application starts publishing its
//!   actions to the broker.
//! - [`WorkspaceJoinedTriggers`] fires when this device joins a workspace.
//! - [`FileChangedTriggers`] fires on a change inside a folder a workspace
//!   capability shares with the saver ([`watched_folders`]).
//! - [`FocusModeTriggers`] fires when Do Not Disturb or the focus schedule
//!   switches on or off.
//!
//! What an event says (the app, the workspace, the file, the mode) is the
//! trigger's payload ([`TriggerSpec::payload`]); each value is handed to the
//! run as the shortcut's input of the same name and type.
//!
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
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{
    DateTime, Datelike as _, Duration as ChronoDuration, FixedOffset, Local, NaiveDate, NaiveTime,
    Offset as _, TimeZone as _, Utc,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use settings_store::hardware::KeyChord;
use sha2::{Digest as _, Sha256};
use tilo_actions::params::ParamType;
use tilo_automation::{
    CatchUp, FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, FocusTransition, RunId, Schedule, ScheduleClaim,
    Shortcut, TriggerSpec, Weekday,
};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER: ChronoDuration = ChronoDuration::seconds(90);

pub use tilo_action_broker::automation::{MIN_INTERVAL_SECONDS, readable_folder, validate_trigger};

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
    /// The applications publishing live actions to the broker now.
    Apps(&'a BTreeSet<String>),
    /// The workspaces this device is in now: room id to name.
    Workspaces(&'a BTreeMap<String, String>),
    /// Whether each focus mode is on now.
    Focus(FocusState),
    /// Files changed inside watched folders since the last look.
    Files(&'a [FolderChange]),
}

/// Whether each focus mode is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusState {
    pub do_not_disturb: bool,
    pub focus: bool,
}

impl FocusState {
    /// Whether `mode` ([`FOCUS_DO_NOT_DISTURB`], [`FOCUS_SCHEDULE`]) is on.
    #[must_use]
    pub fn is_on(self, mode: &str) -> Option<bool> {
        match mode {
            FOCUS_DO_NOT_DISTURB => Some(self.do_not_disturb),
            FOCUS_SCHEDULE => Some(self.focus),
            _ => None,
        }
    }
}

/// How a file in a watched folder changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChange {
    Created,
    Modified,
    Removed,
}

impl FileChange {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Removed => "removed",
        }
    }
}

/// A folder a file-changed trigger watches: the path the capability
/// `capability` of workspace `room` shares with the shortcut's saver.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchedFolder {
    pub room: String,
    pub capability: String,
    pub path: PathBuf,
}

/// One file that changed inside a watched folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderChange {
    pub room: String,
    pub capability: String,
    /// The file's path inside the folder.
    pub file: String,
    pub change: FileChange,
}

impl FolderChange {
    /// The change to `path` inside `folder`, when it is inside it.
    #[must_use]
    pub fn inside(folder: &WatchedFolder, path: &Path, change: FileChange) -> Option<Self> {
        let file = path.strip_prefix(&folder.path).ok()?;
        (!file.as_os_str().is_empty()).then(|| Self {
            room: folder.room.clone(),
            capability: folder.capability.clone(),
            file: file.to_string_lossy().into_owned(),
            change,
        })
    }
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
    /// The trigger's payload, as values for the shortcut's inputs.
    pub inputs: Map<String, Value>,
}

impl Firing {
    /// `trigger` of `armed` firing with `payload`: each value the shortcut
    /// declares an input of the same name and type for is handed to it.
    fn event(armed: &Armed<'_>, trigger: &TriggerSpec, payload: &[(&str, Value)]) -> Self {
        let inputs = payload
            .iter()
            .filter(|(name, value)| {
                armed.shortcut.inputs.iter().any(|input| {
                    input.name == *name
                        && match input.ty {
                            ParamType::Boolean => value.is_boolean(),
                            ParamType::Text => value.is_string(),
                            _ => false,
                        }
                })
            })
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        Self {
            room: armed.room.clone(),
            shortcut: armed.shortcut.id.clone(),
            trigger: trigger.clone(),
            run: RunId::new(),
            claim: None,
            inputs,
        }
    }
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
                        inputs: Map::new(),
                    })
                })
            })
            .collect()
    }
}

/// Every trigger of the armed shortcuts that `matches`, with its payload.
fn fire_matching<'t>(
    armed: &[Armed<'_>],
    mut matches: impl FnMut(&Armed<'_>, &TriggerSpec) -> Option<Vec<(&'t str, Value)>>,
) -> Vec<Firing> {
    let mut firings = Vec::new();
    for armed in armed {
        for trigger in &armed.shortcut.triggers {
            if let Some(payload) = matches(armed, trigger) {
                firings.push(Firing::event(armed, trigger, &payload));
            }
        }
    }
    firings
}

/// Fires when an application starts publishing live actions. The first
/// look only learns what is already running.
#[derive(Debug, Default)]
pub struct AppOpenedTriggers {
    running: Option<BTreeSet<String>>,
}

impl TriggerSource for AppOpenedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Apps(running) = event else {
            return Vec::new();
        };
        let Some(before) = self.running.replace(running.clone()) else {
            return Vec::new();
        };
        running
            .difference(&before)
            .flat_map(|opened| {
                fire_matching(armed, |_, trigger| match trigger {
                    TriggerSpec::AppOpened { app_id } if app_id == opened => {
                        Some(vec![("app_id", Value::from(opened.as_str()))])
                    }
                    _ => None,
                })
            })
            .collect()
    }
}

/// Fires when this device joins a workspace. The first look only learns
/// the workspaces it is already in.
#[derive(Debug, Default)]
pub struct WorkspaceJoinedTriggers {
    joined: Option<BTreeSet<String>>,
}

impl TriggerSource for WorkspaceJoinedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Workspaces(workspaces) = event else {
            return Vec::new();
        };
        let now: BTreeSet<String> = workspaces.keys().cloned().collect();
        let Some(before) = self.joined.replace(now) else {
            return Vec::new();
        };
        workspaces
            .iter()
            .filter(|(room, _)| !before.contains(*room))
            .flat_map(|(room, name)| {
                fire_matching(armed, |_, trigger| match trigger {
                    TriggerSpec::WorkspaceJoined { room: wanted }
                        if wanted.as_ref().is_none_or(|wanted| wanted == room) =>
                    {
                        Some(vec![
                            ("workspace", Value::from(room.as_str())),
                            ("workspace_name", Value::from(name.as_str())),
                        ])
                    }
                    _ => None,
                })
            })
            .collect()
    }
}

/// Fires for each change inside a folder a shortcut's file-changed trigger
/// watches, in the shortcut's own workspace.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileChangedTriggers;

impl TriggerSource for FileChangedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Files(changes) = event else {
            return Vec::new();
        };
        changes
            .iter()
            .flat_map(|change| {
                fire_matching(armed, |armed, trigger| match trigger {
                    TriggerSpec::FileChanged { capability }
                        if *capability == change.capability && armed.room == change.room =>
                    {
                        Some(vec![
                            ("file", Value::from(change.file.as_str())),
                            ("change", Value::from(change.change.label())),
                        ])
                    }
                    _ => None,
                })
            })
            .collect()
    }
}

/// Fires when a focus mode switches on or off. The first look only learns
/// which modes are on.
#[derive(Debug, Default)]
pub struct FocusModeTriggers {
    last: Option<FocusState>,
}

impl TriggerSource for FocusModeTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed<'_>],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Focus(now) = event else {
            return Vec::new();
        };
        let Some(before) = self.last.replace(now) else {
            return Vec::new();
        };
        if before == now {
            return Vec::new();
        }
        fire_matching(armed, |_, trigger| {
            let TriggerSpec::FocusMode { mode, transition } = trigger else {
                return None;
            };
            let active = now.is_on(mode)?;
            let switched = before.is_on(mode)? != active;
            let wanted = match transition {
                FocusTransition::Enter => active,
                FocusTransition::Leave => !active,
            };
            (switched && wanted).then(|| {
                vec![
                    ("mode", Value::from(mode.as_str())),
                    ("active", Value::from(active)),
                ]
            })
        })
    }
}

/// The folders the file-changed triggers of `armed` watch. A trigger names
/// a capability of its shortcut's workspace; it watches that capability's
/// folder only while the capability is a filesystem grant the saver may
/// still read through, so a revoked or narrowed grant stops the watch.
#[must_use]
pub fn watched_folders(
    workspaces: &[crate::shell::state::Workspace],
    armed: &[Armed<'_>],
) -> Vec<WatchedFolder> {
    let mut folders = Vec::new();
    for armed in armed {
        let Some(workspace) = workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == armed.room)
        else {
            continue;
        };
        let Some(saver) = armed.shortcut.saved_by.as_deref() else {
            continue;
        };
        for trigger in &armed.shortcut.triggers {
            let TriggerSpec::FileChanged { capability } = trigger else {
                continue;
            };
            if let Ok(path) = readable_folder(workspace, capability, saver) {
                folders.push(WatchedFolder {
                    room: armed.room.clone(),
                    capability: capability.clone(),
                    path,
                });
            }
        }
    }
    folders.sort();
    folders.dedup();
    folders
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
            .and_then(|bytes| tilo_files::native::atomic::write_creating_parent(path, &bytes));
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
                                inputs: Map::new(),
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
                Box::new(AppOpenedTriggers::default()),
                Box::new(WorkspaceJoinedTriggers::default()),
                Box::new(FileChangedTriggers),
                Box::new(FocusModeTriggers::default()),
            ],
        }
    }

    /// The session's triggers: the system clock, schedules persisted under
    /// the launcher-provided state root.
    #[must_use]
    pub fn for_session() -> Self {
        let schedules = tilo_paths::env_dir("XDG_STATE_HOME")
            .map(|state| state.join("tilo/automation/schedules.json"));
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
    fn only_triggers_the_session_watches_can_be_saved() {
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
            .is_ok()
        );
        assert!(validate_trigger(&TriggerSpec::AppOpened { app_id: " ".into() }).is_err());
        assert!(validate_trigger(&TriggerSpec::WorkspaceJoined { room: None }).is_ok());
        assert!(
            validate_trigger(&TriggerSpec::FileChanged {
                capability: String::new()
            })
            .is_err()
        );
        assert!(
            validate_trigger(&TriggerSpec::FocusMode {
                mode: "sleep".into(),
                transition: FocusTransition::Enter,
            })
            .is_err()
        );
        assert!(
            validate_trigger(&TriggerSpec::NetworkChanged {
                network: None,
                connected: None,
            })
            .is_err()
        );
    }

    /// A shortcut saved by `person:me` with `trigger` and the inputs `inputs`
    /// (name and type).
    fn event_shortcut(trigger: &TriggerSpec, inputs: &[(&str, &str)]) -> Shortcut {
        let inputs: Vec<serde_json::Value> = inputs
            .iter()
            .map(|(name, ty)| {
                serde_json::json!({
                    "name": name, "ty": ty, "required": false, "description": name
                })
            })
            .collect();
        serde_json::from_value(serde_json::json!({
            "id": "on-event",
            "name": "On event",
            "inputs": inputs,
            "steps": [],
            "triggers": [trigger],
            "saved_by": "person:me"
        }))
        .expect("shortcut JSON")
    }

    fn observe_once(
        source: &mut dyn TriggerSource,
        event: TriggerEvent<'_>,
        saved: &Shortcut,
        room: &str,
    ) -> Vec<Firing> {
        let armed = [Armed {
            room: room.into(),
            shortcut: saved,
            claims: &[],
        }];
        let clock = SystemClock;
        let devices = PersonalDevices::default();
        source.observe(
            event,
            &armed,
            TriggerContext {
                clock: &clock,
                devices: &devices,
            },
        )
    }

    #[test]
    fn an_app_opening_fires_with_its_id_once_it_starts_publishing() {
        let saved = event_shortcut(
            &TriggerSpec::AppOpened {
                app_id: "so.tilo.notes".into(),
            },
            &[("app_id", "text")],
        );
        let mut source = AppOpenedTriggers::default();
        let running =
            |apps: &[&str]| -> BTreeSet<String> { apps.iter().map(ToString::to_string).collect() };
        let already = running(&["so.tilo.notes"]);
        assert!(
            observe_once(&mut source, TriggerEvent::Apps(&already), &saved, "room").is_empty(),
            "an app running when the session starts did not just open"
        );
        let closed = running(&[]);
        assert!(observe_once(&mut source, TriggerEvent::Apps(&closed), &saved, "room").is_empty());
        let other = running(&["so.tilo.files"]);
        assert!(observe_once(&mut source, TriggerEvent::Apps(&other), &saved, "room").is_empty());
        let reopened = running(&["so.tilo.files", "so.tilo.notes"]);
        let fired = observe_once(&mut source, TriggerEvent::Apps(&reopened), &saved, "room");
        assert_eq!(fired.len(), 1);
        assert_eq!(
            fired[0].inputs,
            serde_json::json!({ "app_id": "so.tilo.notes" })
                .as_object()
                .cloned()
                .expect("an object")
        );
    }

    #[test]
    fn joining_a_workspace_fires_with_its_id_and_name() {
        let any = event_shortcut(
            &TriggerSpec::WorkspaceJoined { room: None },
            &[("workspace_name", "text"), ("workspace", "integer")],
        );
        let mut source = WorkspaceJoinedTriggers::default();
        let mut rooms = BTreeMap::from([("personal".to_string(), "Home".to_string())]);
        assert!(
            observe_once(
                &mut source,
                TriggerEvent::Workspaces(&rooms),
                &any,
                "personal"
            )
            .is_empty(),
            "the workspaces a device starts in were not just joined"
        );
        rooms.insert("design".into(), "Design review".into());
        let fired = observe_once(
            &mut source,
            TriggerEvent::Workspaces(&rooms),
            &any,
            "personal",
        );
        assert_eq!(fired.len(), 1);
        assert_eq!(
            fired[0].inputs.get("workspace_name"),
            Some(&serde_json::json!("Design review"))
        );
        assert!(
            !fired[0].inputs.contains_key("workspace"),
            "an input of another type is not given the workspace id"
        );

        let only_design = event_shortcut(
            &TriggerSpec::WorkspaceJoined {
                room: Some("design".into()),
            },
            &[],
        );
        let mut source = WorkspaceJoinedTriggers::default();
        rooms.remove("design");
        observe_once(
            &mut source,
            TriggerEvent::Workspaces(&rooms),
            &only_design,
            "personal",
        );
        rooms.insert("standup".into(), "Standup".into());
        assert!(
            observe_once(
                &mut source,
                TriggerEvent::Workspaces(&rooms),
                &only_design,
                "personal"
            )
            .is_empty()
        );
        rooms.insert("design".into(), "Design review".into());
        assert_eq!(
            observe_once(
                &mut source,
                TriggerEvent::Workspaces(&rooms),
                &only_design,
                "personal"
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_change_in_a_watched_folder_fires_the_trigger_naming_its_capability() {
        let saved = event_shortcut(
            &TriggerSpec::FileChanged {
                capability: "inbox-read".into(),
            },
            &[("file", "text"), ("change", "text")],
        );
        let inbox = WatchedFolder {
            room: "room".into(),
            capability: "inbox-read".into(),
            path: PathBuf::from("/var/workspace/inbox"),
        };
        let changes = [
            FolderChange::inside(
                &inbox,
                Path::new("/var/workspace/inbox/scans/receipt.pdf"),
                FileChange::Created,
            )
            .expect("inside the folder"),
            FolderChange {
                capability: "outbox-read".into(),
                ..FolderChange::inside(
                    &inbox,
                    Path::new("/var/workspace/inbox/other.txt"),
                    FileChange::Modified,
                )
                .expect("inside the folder")
            },
        ];
        assert!(
            FolderChange::inside(
                &inbox,
                Path::new("/var/workspace/inboxes/x"),
                FileChange::Created
            )
            .is_none(),
            "a sibling folder is not inside"
        );
        let fired = observe_once(
            &mut FileChangedTriggers,
            TriggerEvent::Files(&changes),
            &saved,
            "room",
        );
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(
            fired[0].inputs,
            serde_json::json!({ "file": "scans/receipt.pdf", "change": "created" })
                .as_object()
                .cloned()
                .expect("an object")
        );
        assert!(
            observe_once(
                &mut FileChangedTriggers,
                TriggerEvent::Files(&changes),
                &saved,
                "another-room"
            )
            .is_empty(),
            "a capability id is only meaningful in its own workspace"
        );
    }

    #[test]
    fn a_focus_mode_fires_on_the_transition_it_names() {
        let entering_dnd = event_shortcut(
            &TriggerSpec::FocusMode {
                mode: FOCUS_DO_NOT_DISTURB.into(),
                transition: FocusTransition::Enter,
            },
            &[("active", "boolean"), ("mode", "text")],
        );
        let leaving_focus = event_shortcut(
            &TriggerSpec::FocusMode {
                mode: FOCUS_SCHEDULE.into(),
                transition: FocusTransition::Leave,
            },
            &[],
        );
        let mut dnd = FocusModeTriggers::default();
        let mut focus = FocusModeTriggers::default();
        let quiet = FocusState {
            do_not_disturb: true,
            focus: true,
        };
        let calm = FocusState::default();
        let dnd_only = FocusState {
            do_not_disturb: true,
            focus: false,
        };
        assert!(
            observe_once(&mut dnd, TriggerEvent::Focus(calm), &entering_dnd, "room").is_empty(),
            "the first look only learns the state"
        );
        let fired = observe_once(&mut dnd, TriggerEvent::Focus(quiet), &entering_dnd, "room");
        assert_eq!(fired.len(), 1);
        assert_eq!(
            fired[0].inputs,
            serde_json::json!({ "mode": "do_not_disturb", "active": true })
                .as_object()
                .cloned()
                .expect("an object")
        );
        assert!(
            observe_once(
                &mut dnd,
                TriggerEvent::Focus(dnd_only),
                &entering_dnd,
                "room"
            )
            .is_empty(),
            "another mode switching is not this one"
        );
        assert!(
            observe_once(&mut dnd, TriggerEvent::Focus(calm), &entering_dnd, "room").is_empty(),
            "leaving is not entering"
        );

        observe_once(
            &mut focus,
            TriggerEvent::Focus(quiet),
            &leaving_focus,
            "room",
        );
        assert_eq!(
            observe_once(
                &mut focus,
                TriggerEvent::Focus(dnd_only),
                &leaving_focus,
                "room"
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_folder_is_watched_only_while_its_capability_lets_the_saver_read_it() {
        use crate::shell::state::{Workspace, WorkspaceAuthority as _, WorkspaceId};
        use collab_types::{Access, CapOverride, Scope, Subject};

        let owner = "person:owner".to_string();
        let mut workspace = Workspace::new(WorkspaceId(1), "Scans");
        workspace.set_room(personal_room());
        workspace.set_owner(owner.clone());
        workspace
            .govern_as_at(
                &owner,
                tilo_action_policy::Change::AddMember {
                    person: "person:me".into(),
                    role: tilo_action_policy::Role::Viewer,
                },
                tilo_clock::unix_now(),
            )
            .expect("the owner admits the saver");
        workspace
            .grant_as(
                &owner,
                CapOverride::grant(
                    Subject::person("person:me"),
                    Scope::FsPath("/var/workspace/inbox".into()),
                    Access::Read,
                )
                .with_id("inbox-read"),
            )
            .expect("the owner shares the folder");
        workspace.shortcuts = vec![event_shortcut(
            &TriggerSpec::FileChanged {
                capability: "inbox-read".into(),
            },
            &[],
        )];
        let folders = |workspaces: &[Workspace]| {
            let armed = armed(workspaces, |_| "person:me".into());
            watched_folders(workspaces, &armed)
        };
        let watched = folders(std::slice::from_ref(&workspace));
        assert_eq!(
            watched,
            [WatchedFolder {
                room: PERSONAL.into(),
                capability: "inbox-read".into(),
                path: PathBuf::from("/var/workspace/inbox"),
            }]
        );
        assert!(
            readable_folder(&workspace, "me-membership", "person:me")
                .expect_err("a membership is no folder")
                .contains("does not share a folder")
        );

        assert!(workspace.revoke_as(&owner, "inbox-read"));
        assert!(
            folders(std::slice::from_ref(&workspace)).is_empty(),
            "a revoked capability stops the watch"
        );
    }
}
