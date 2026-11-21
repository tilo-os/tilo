//! What starts a saved shortcut without anyone invoking it. A [`TriggerSource`] watches for one
//! kind of event and turns it into [`Firing`]s for the shortcuts armed on this device; the session
//! starts each run through the action broker, which checks again that the shortcut still exists and
//! that its saver may still run it. Only shortcuts saved by the person this device acts for in
//! their workspace are armed ([`armed`]), so a member's shortcut never fires on someone
//! else's behalf. A shortcut reaches every device of its saver: they rank themselves for each fire
//! time or record change by a hash, the first runs it at once and records a [`ScheduleClaim`] that
//! syncs like the shortcut, and each other waits [`CLAIM_GRACE`] per rank, so an asleep device is
//! covered by the next. A schedule's next fire is persisted before it is handed over.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use jiff::SignedDuration;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use settings_store::hardware::KeyChord;
use sha2::{Digest as _, Sha256};
use tilo_actions::params::ParamType;
use tilo_automation::EVENT_INPUT;
use tilo_automation::dispatch::{AppEvent, DocumentEvent, app_event_object};
use tilo_automation::{
    AutomationInvocationId, CatchUp, FOCUS_DO_NOT_DISTURB, FOCUS_SCHEDULE, FocusTransition,
    Schedule, ScheduleClaim, Shortcut, TriggerSpec,
};

/// A fire time this far in the past counts as missed rather than on time.
pub const MISSED_AFTER: SignedDuration = tilo_automation::schedule::MISSED_AFTER;

pub use tilo_workspace_service::apps::automation::{readable_folder, validate_trigger};

/// How long each device waits, per device ranked before it, for a fire time to be claimed before it
/// takes it.
pub const CLAIM_GRACE: SignedDuration = SignedDuration::from_secs(45);

/// The person's certified devices, as this device sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PersonalDevices {
    /// This device, or `None` when it has no certified identity and so shares its schedules with no
    /// other device.
    pub local: Option<String>,
    /// The person's devices that are online now; this one counts whether listed or not.
    pub online: BTreeSet<String>,
}

#[cfg(test)]
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

/// A clock that moves only when told to, in a fixed zone.
#[cfg(test)]
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<jiff::Timestamp>,
    zone: TimeZone,
}

#[cfg(test)]
impl ManualClock {
    #[must_use]
    pub fn new(now: jiff::Timestamp, zone: TimeZone) -> Self {
        Self {
            now: Mutex::new(now),
            zone,
        }
    }

    pub fn set(&self, now: jiff::Timestamp) {
        if let Ok(mut current) = self.now.lock() {
            *current = now;
        }
    }
}

#[cfg(test)]
impl Clock for ManualClock {
    fn now(&self) -> jiff::Timestamp {
        self.now
            .lock()
            .map_or_else(|_| jiff::Timestamp::now(), |now| *now)
    }

    fn zone(&self) -> TimeZone {
        self.zone.clone()
    }
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
    fn now(&self) -> jiff::Timestamp;

    /// The zone the local wall clock reads in.
    fn zone(&self) -> TimeZone;
}

/// The system clock, in the session's local time zone.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> jiff::Timestamp {
        jiff::Timestamp::now()
    }

    fn zone(&self) -> TimeZone {
        TimeZone::system()
    }
}

/// A shortcut this device may start, with the workspace it is saved in, the person whose device
/// proved saving it, and the fire times of that workspace's schedules already claimed.
#[derive(Clone, Debug)]
pub struct Armed {
    pub workspace: String,
    pub shortcut: Shortcut,
    /// Who saved it, by their device's proof: its triggers fire on their behalf only.
    pub saver: String,
    /// The fire times of that workspace's schedules already claimed, which say when another of the
    /// person's devices took one.
    pub claims: Vec<ScheduleClaim>,
}

/// Every shortcut in `workspaces` with triggers that the person this device acts for in its
/// workspace (`local_person`) saved, by the proof of a device of theirs
/// ([`tilo_peer_state::workspace::proven_saver`]) that holds at `now_ns` against the workspace's
/// revocations (`revocations`, `None` for a workspace this device holds no document of). A
/// `saved_by` without such a proof, or one for another workspace, never arms.
pub fn armed(
    workspaces: &[tilo_desktop_model::Workspace],
    local_person: impl Fn(&tilo_desktop_model::Workspace) -> String,
    revocations: impl Fn(&tilo_desktop_model::Workspace) -> Option<tilo_user_identity::RevocationList>,
    now_ns: u64,
) -> Vec<Armed> {
    let mut armed = Vec::new();
    for workspace in workspaces {
        let mut triggered = workspace
            .shortcuts
            .iter()
            .filter_map(|saved| {
                saved
                    .decode()
                    .inspect_err(|error| {
                        tracing::warn!(
                            %error,
                            "a saved shortcut does not read back; it arms no trigger"
                        );
                    })
                    .ok()
            })
            .filter(|shortcut| !shortcut.triggers.is_empty())
            .peekable();
        if triggered.peek().is_none() {
            continue;
        }
        let Some(revocations) = revocations(workspace) else {
            continue;
        };
        let person = local_person(workspace);
        let workspace_id = workspace.id().to_string();
        for shortcut in triggered {
            if tilo_peer_state::workspace::proven_saver(
                &workspace_id,
                &shortcut,
                &revocations,
                now_ns,
            )
            .as_ref()
                == Some(&person)
            {
                armed.push(Armed {
                    workspace: workspace_id.clone(),
                    shortcut,
                    saver: person.clone(),
                    claims: workspace.schedule_claims.clone(),
                });
            }
        }
    }
    armed
}

/// Nanoseconds since the Unix epoch at `at` , for [`armed`].
#[must_use]
pub fn unix_ns(at: jiff::Timestamp) -> u64 {
    u64::try_from(at.as_nanosecond()).unwrap_or_default()
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
    /// The workspaces this device is in now: id to name.
    Workspaces(&'a BTreeMap<String, String>),
    /// Whether each focus mode is on now.
    Focus(FocusState),
    /// Files changed inside watched folders since the last look.
    Files(&'a [FolderChange]),
    /// Changes committed to records of this device's workspaces since the last look.
    Records(&'a [DocumentEvent]),
}

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
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
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

/// A folder a file-changed trigger watches: the path the capability `capability` of `workspace`
/// shares with the shortcut's saver.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct WatchedFolder {
    pub workspace: String,
    pub capability: String,
    pub path: PathBuf,
}

/// One file that changed inside a watched folder.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct FolderChange {
    pub workspace: String,
    pub capability: String,
    /// The file's path inside the folder.
    pub file: String,
    pub change: FileChange,
}

/// Which file of which workspace a [`FolderChange`] is about: its workspace,
/// its capability and the file's path. `Ord` puts files in the order a person
/// sees them, so one poll reports them the same way twice.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChangedFile {
    pub workspace: String,
    pub capability: String,
    pub file: String,
}

impl FolderChange {
    /// What identifies the file this reports on, so one file settling is one change however many
    /// kinds of change it made.
    #[must_use]
    pub fn same_file(&self) -> ChangedFile {
        ChangedFile {
            workspace: self.workspace.clone(),
            capability: self.capability.clone(),
            file: self.file.clone(),
        }
    }

    #[must_use]
    pub fn inside(folder: &WatchedFolder, path: &Path, change: FileChange) -> Option<Self> {
        let file = path.strip_prefix(&folder.path).ok()?;
        (!file.as_os_str().is_empty()).then(|| Self {
            workspace: folder.workspace.clone(),
            capability: folder.capability.clone(),
            file: file.to_string_lossy().into_owned(),
            change,
        })
    }
}

/// One trigger that fired: run `shortcut` in `workspace` , as run `run` .
#[derive(Clone, Debug, PartialEq)]
pub struct Firing {
    pub workspace: String,
    pub shortcut: String,
    pub trigger: TriggerSpec,
    pub invocation: AutomationInvocationId,
    /// The fire time this device takes, to record in the workspace
    /// ([`WorkspaceTree::claim_schedule`](tilo_desktop_model::WorkspaceTree::claim_schedule))
    /// before the run starts.
    pub claim: Option<ScheduleClaim>,
    /// The trigger's payload, as values for the shortcut's inputs.
    pub inputs: Map<String, Value>,
}

impl Firing {
    /// `trigger` of `armed` firing with `payload` : each value the shortcut declares an input of
    /// the same name and type for is handed to it.
    fn event(armed: &Armed, trigger: &TriggerSpec, payload: &[(&str, Value)]) -> Self {
        let inputs = payload
            .iter()
            .filter(|(name, value)| {
                armed.shortcut.inputs.iter().any(|input| {
                    input.name == *name
                        && match input.ty {
                            ParamType::Boolean => value.is_boolean(),
                            ParamType::Text => value.is_string(),
                            ParamType::Object => value.is_object(),
                            _ => false,
                        }
                })
            })
            .map(|(name, value)| ((*name).to_string(), value.clone()))
            .collect();
        Self {
            workspace: armed.workspace.clone(),
            shortcut: armed.shortcut.id.clone(),
            trigger: trigger.clone(),
            invocation: AutomationInvocationId::new(),
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
        armed: &[Armed],
        context: TriggerContext<'_>,
    ) -> Vec<Firing>;

    /// Runs another of the person's devices started; a source holding one for its turn drops it.
    fn forget_started(&mut self, _started: &BTreeSet<AutomationInvocationId>) {}

    /// Firings an earlier event produced that were waiting for something to reach the disk and may
    /// run now.
    fn take_settled(&mut self) -> Vec<Firing> {
        Vec::new()
    }
}

/// Fires a shortcut whose keyboard trigger names the pressed chord.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyboardTriggers;

impl TriggerSource for KeyboardTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
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
                    let parsed = tilo_failure::ignored!(
                        "a chord that does not parse matches no key",
                        KeyChord::parse(chord).ok()
                    );
                    (parsed.as_ref() == Some(pressed)).then(|| Firing {
                        workspace: armed.workspace.clone(),
                        shortcut: armed.shortcut.id.clone(),
                        trigger: trigger.clone(),
                        invocation: AutomationInvocationId::new(),
                        claim: None,
                        inputs: Map::new(),
                    })
                })
            })
            .collect()
    }
}

/// Every trigger of the armed shortcuts that `matches` , with its payload.
fn fire_matching<'t>(
    armed: &[Armed],
    mut matches: impl FnMut(&Armed, &TriggerSpec) -> Option<Vec<(&'t str, Value)>>,
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

/// Fires when an application starts publishing live actions. The first look only learns what is
/// already running.
#[derive(Debug, Default)]
pub struct AppOpenedTriggers {
    running: Option<BTreeSet<String>>,
}

impl TriggerSource for AppOpenedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
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

/// Fires when this device joins a workspace. The first look only learns the workspaces it is
/// already in.
#[derive(Debug, Default)]
pub struct WorkspaceJoinedTriggers {
    joined: Option<BTreeSet<String>>,
}

impl TriggerSource for WorkspaceJoinedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
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
            .filter(|(workspace, _)| !before.contains(*workspace))
            .flat_map(|(workspace, name)| {
                fire_matching(armed, |_, trigger| match trigger {
                    TriggerSpec::WorkspaceJoined { workspace: wanted }
                        if wanted.as_ref().is_none_or(|wanted| wanted == workspace) =>
                    {
                        Some(vec![
                            ("workspace", Value::from(workspace.as_str())),
                            ("workspace_name", Value::from(name.as_str())),
                        ])
                    }
                    _ => None,
                })
            })
            .collect()
    }
}

/// Fires for each change inside a folder a shortcut's file-changed trigger watches, in the
/// shortcut's own workspace.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileChangedTriggers;

impl TriggerSource for FileChangedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
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
                        if *capability == change.capability
                            && armed.workspace == change.workspace =>
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

/// Fires when a focus mode switches on or off. The first look only learns which modes are on.
#[derive(Debug, Default)]
pub struct FocusModeTriggers {
    last: Option<FocusState>,
}

impl TriggerSource for FocusModeTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
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

/// The folders the file-changed triggers of `armed` watch. A trigger names a capability of its
/// shortcut's workspace; it watches that capability's folder only while the capability is a
/// filesystem grant the saver may still read through, so a revoked or narrowed grant stops the
/// watch.
#[must_use]
pub fn watched_folders(
    workspaces: &[tilo_desktop_model::Workspace],
    armed: &[Armed],
) -> Vec<WatchedFolder> {
    let mut folders = Vec::new();
    for armed in armed {
        let Some(workspace) = workspaces
            .iter()
            .find(|workspace| workspace.id().to_string() == armed.workspace)
        else {
            continue;
        };
        let saver = tilo_actions::MemberId::person(armed.saver.clone());
        for trigger in &armed.shortcut.triggers {
            let TriggerSpec::FileChanged { capability } = trigger else {
                continue;
            };
            if let Ok(path) = readable_folder(workspace, capability, &saver) {
                folders.push(WatchedFolder {
                    workspace: armed.workspace.clone(),
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct LogEntry {
    /// The trigger as it was when armed; a changed schedule starts over.
    trigger: TriggerSpec,
    /// When it fires next, in milliseconds since the Unix epoch.
    next_fire_ms: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct ScheduleLog {
    /// Keyed by `workspace/shortcut/trigger index` .
    #[serde(default)]
    schedules: BTreeMap<String, LogEntry>,
}

/// Fires scheduled triggers when their time comes, at most once per fire time, across restarts.
/// The log of next fire times is a document of the device's store ([`crate::kept`]), which commits
/// it on its own thread, because [`TriggerEvent::Tick`] arrives on the compositor's loop and the
/// write's fsyncs can take seconds under I/O pressure. The firings of the tick that wrote a log wait
/// under that write; once the store says it committed (or failed, which is logged and runs the
/// firings as before) the store wakes the loop and a later tick hands them over. A newer log that
/// replaces a queued older one commits both. A crash before the write lands leaves the previous log
/// and loses no invocation: none has started.
pub struct ScheduleTriggers {
    log: ScheduleLog,
    kept: Option<crate::kept::Kept>,
    /// Firings waiting for the write of the log they moved past, `None` for one that waits for
    /// nothing.
    held: Vec<(Option<durable_store::Revision>, Firing)>,
    /// Wakes the session's loop once the log is written, so the firings it holds are handed over
    /// then rather than a tick later.
    wake: crate::automation::Wake,
}

impl std::fmt::Debug for ScheduleTriggers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScheduleTriggers")
            .field("log", &self.log)
            .field("kept", &self.kept)
            .field("held", &self.held.len())
            .finish_non_exhaustive()
    }
}

impl ScheduleTriggers {
    /// Schedules kept in `kept`, or in memory only for `None`.
    #[must_use]
    pub(crate) fn open(kept: Option<crate::kept::Kept>, wake: crate::automation::Wake) -> Self {
        let log = kept
            .as_ref()
            .and_then(|kept| match kept.read() {
                Ok(bytes) => bytes,
                Err(error) => {
                    tracing::warn!(%error, "the log of fired triggers could not be read");
                    None
                }
            })
            .and_then(|bytes| {
                serde_json::from_slice(&bytes)
                    .inspect_err(|error| {
                        tracing::warn!(%error, "the log of fired triggers does not read back");
                    })
                    .ok()
            })
            .unwrap_or_default();
        Self {
            log,
            kept,
            held: Vec::new(),
            wake,
        }
    }

    /// Write the log and hold `due` until it is written. Kept in memory only, or when the log
    /// cannot be encoded or written, `due` runs at once.
    fn persist(&mut self, due: Vec<Firing>) {
        let Some(kept) = &self.kept else {
            self.held.extend(due.into_iter().map(|firing| (None, firing)));
            return;
        };
        let revision = match serde_json::to_vec_pretty(&self.log) {
            Ok(bytes) => kept.replace(bytes),
            Err(error) => {
                tracing::warn!(%error, "shortcut schedules were not saved");
                None
            }
        };
        self.held
            .extend(due.into_iter().map(|firing| (revision, firing)));
        if revision.is_some() {
            // Made on the loop's thread, so it wakes the loop that holds the firings; a no-op where
            // no loop installed a wake.
            kept.wake_when_settled(crate::automation::Wake::clone(&self.wake));
        }
    }
}

impl TriggerSource for ScheduleTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
        context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Tick = event else {
            return Vec::new();
        };
        let clock = context.clock;
        let now = clock.now();
        let mut due_now = Vec::new();
        let mut live = BTreeSet::new();
        let mut changed = false;
        for armed in armed {
            'triggers: for (index, trigger) in armed.shortcut.triggers.iter().enumerate() {
                let TriggerSpec::Schedule { schedule, catch_up } = trigger else {
                    continue;
                };
                let key = log_key(&armed.workspace, &armed.shortcut.id, index);
                live.insert(key.clone());

                let current = self
                    .log
                    .schedules
                    .get(&key)
                    .filter(|entry| entry.trigger == *trigger)
                    .and_then(|entry| jiff::Timestamp::from_millisecond(entry.next_fire_ms).ok());
                let next = match current {
                    Some(due) if now < due => continue,
                    Some(due) => 'due: {
                        let Some(wait) = turn(&key, armed, index, due, context.devices) else {
                            // Another of the person's devices took it.
                            break 'due next_fire(schedule, now, clock);
                        };
                        let Some(start) = due.checked_add(wait).ok() else {
                            continue 'triggers;
                        };
                        if now < start {
                            continue 'triggers;
                        }
                        let on_time = now.duration_since(start) <= MISSED_AFTER;
                        if on_time || *catch_up == CatchUp::RunOnce {
                            due_now.push(Firing {
                                workspace: armed.workspace.clone(),
                                shortcut: armed.shortcut.id.clone(),
                                trigger: trigger.clone(),
                                invocation: scheduled_invocation(&key, due),
                                inputs: Map::new(),
                                claim: context.devices.local.as_ref().map(|device| ScheduleClaim {
                                    shortcut: armed.shortcut.id.clone(),
                                    trigger: index,
                                    due_ms: due.as_millisecond(),
                                    device: device.clone(),
                                }),
                            });
                        }
                        next_fire(schedule, now, clock)
                    }
                    None => next_fire(schedule, now, clock),
                };
                let Some(next) = next else {
                    continue;
                };
                self.log.schedules.insert(
                    key,
                    LogEntry {
                        trigger: trigger.clone(),
                        next_fire_ms: next.as_millisecond(),
                    },
                );
                changed = true;
            }
        }
        let before = self.log.schedules.len();
        self.log.schedules.retain(|key, _| live.contains(key));
        changed |= self.log.schedules.len() != before;
        // Persisted before anything runs: a firing is handed over only once the log that moved
        // past its fire time is written, so a crash loses at most the run, never repeats it.
        if changed {
            self.persist(due_now);
        }
        self.take_settled()
    }

    fn take_settled(&mut self) -> Vec<Firing> {
        if self.held.is_empty() {
            return Vec::new();
        }
        let kept = self.kept.as_ref();
        let (ready, waiting): (Vec<_>, Vec<_>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|(revision, _)| match (revision, kept) {
                (Some(revision), Some(kept)) => match kept.durability(*revision) {
                    durable_store::Durability::Pending => false,
                    durable_store::Durability::Durable => true,
                    durable_store::Durability::Failed(error) => {
                        tracing::warn!(%error, "shortcut schedules were not saved");
                        true
                    }
                },
                _ => true,
            });
        self.held = waiting;
        ready.into_iter().map(|(_, firing)| firing).collect()
    }
}

fn log_key(workspace: &str, shortcut: &str, index: usize) -> String {
    format!("{workspace}/{shortcut}/{index}")
}

/// How long after `due` this device takes the fire time of schedule `key` (trigger `index` of
/// `armed` ), or `None` when one of the person's devices already claimed it. Every device ranks the
/// person's online devices the same way for a fire time, so they agree on who goes first without
/// talking; ranking by fire time spreads schedules across them.
fn turn(
    key: &str,
    armed: &Armed,
    index: usize,
    due: jiff::Timestamp,
    devices: &PersonalDevices,
) -> Option<SignedDuration> {
    let Some(local) = devices.local.as_deref() else {
        return Some(SignedDuration::ZERO);
    };
    let due_ms = due.as_millisecond();
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

/// The invocation a scheduled fire starts: the same for the same fire time, so a
/// fire that is somehow
/// handed over twice reuses every invocation id.
fn scheduled_invocation(key: &str, due: jiff::Timestamp) -> AutomationInvocationId {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.shortcut.schedule\0");
    hasher.update(key.as_bytes());
    hasher.update(due.as_millisecond().to_be_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    for (byte, digested) in bytes.iter_mut().zip(digest.iter()) {
        *byte = *digested;
    }
    AutomationInvocationId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

/// The first fire time of `schedule` strictly after `now`, in the wall-clock time `clock` reads.
pub(crate) fn next_fire(
    schedule: &Schedule,
    now: jiff::Timestamp,
    clock: &dyn Clock,
) -> Option<jiff::Timestamp> {
    tilo_automation::schedule::next_fire_time(schedule, now, &clock.zone())
}

/// A record firing this device holds back while devices ranked before it get their turn.
#[derive(Clone, Debug)]
struct Held {
    firing: Firing,
    start: jiff::Timestamp,
}

/// Where `device` stands for a change; lowest first. Every device of the person ranks the same way,
/// so they agree without talking.
fn change_device_rank(change: &str, device: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(b"tilo.shortcut.change-runner\0");
    hasher.update(change.as_bytes());
    hasher.update([0]);
    hasher.update(device.as_bytes());
    hasher.finalize().into()
}

/// How many online devices rank before the local one for `change` .
fn change_rank(change: &str, devices: &PersonalDevices) -> usize {
    let Some(local) = devices.local.as_deref() else {
        return 0;
    };
    let mine = change_device_rank(change, local);
    devices
        .online
        .iter()
        .filter(|device| device.as_str() != local)
        .filter(|device| change_device_rank(change, device) < mine)
        .count()
}

/// Shared by both record sources: hold each firing for its turn and hand it over when the turn
/// comes. The run's own first change is the claim: a held firing whose run already started
/// elsewhere is dropped ([`TriggerSource::forget_started`]).
#[derive(Debug, Default)]
struct RecordElection {
    held: Vec<Held>,
}

impl RecordElection {
    fn offer(
        &mut self,
        firings: Vec<Firing>,
        change: &str,
        context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let rank = change_rank(change, context.devices);
        if rank == 0 {
            return firings;
        }
        let wait = CLAIM_GRACE
            .checked_mul(i32::try_from(rank).unwrap_or(i32::MAX))
            .unwrap_or(SignedDuration::MAX);
        let start = context
            .clock
            .now()
            .checked_add(wait)
            .unwrap_or(jiff::Timestamp::MAX);
        self.held
            .extend(firings.into_iter().map(|firing| Held { firing, start }));
        Vec::new()
    }

    fn due(&mut self, now: jiff::Timestamp) -> Vec<Firing> {
        let (due, waiting): (Vec<Held>, Vec<Held>) = std::mem::take(&mut self.held)
            .into_iter()
            .partition(|held| held.start <= now);
        self.held = waiting;
        due.into_iter().map(|held| held.firing).collect()
    }

    fn forget_started(&mut self, started: &BTreeSet<AutomationInvocationId>) {
        self.held
            .retain(|held| !started.contains(&held.firing.invocation));
    }

    /// Due firings, then the firings of `event` 's changes whose trigger `wanted` accepts, those
    /// this device runs now.
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
        context: TriggerContext<'_>,
        wanted: fn(&TriggerSpec) -> bool,
    ) -> Vec<Firing> {
        let mut ready = self.due(context.clock.now());
        if let TriggerEvent::Records(events) = event {
            for change in events {
                let firings = record_firings(armed, change, wanted);
                ready.extend(self.offer(firings, &change.change, context));
            }
        }
        ready
    }
}

/// What `event` fires among `armed` in its workspace, through the dispatcher a bot's host uses too,
/// so both fire on a change the same way.
/// A change a shortcut's run made fires nothing here, in any workspace: a pair of shortcuts that
/// watch each other's kind and write it would otherwise run each other for as long as the session
/// lasts. The dispatcher says why ([`Skipped::Loop`]), and the change is passed on to nobody else
/// on this device.
fn record_firings(
    armed: &[Armed],
    event: &DocumentEvent,
    wanted: fn(&TriggerSpec) -> bool,
) -> Vec<Firing> {
    use tilo_automation::dispatch::{self, Armed as DispatchArmed, Event, Firer};
    let mut firings = Vec::new();
    for armed in armed
        .iter()
        .filter(|armed| armed.workspace == event.workspace)
    {
        let saver = armed.saver.as_str();
        let dispatched = dispatch::dispatch(
            &[DispatchArmed {
                firer: Firer::Shortcut {
                    shortcut: &armed.shortcut.id,
                    saver,
                },
                shortcut: &armed.shortcut,
            }],
            Event::Record(event),
        );
        for fired in dispatched
            .firings
            .into_iter()
            .filter(|fired| wanted(&fired.trigger))
        {
            firings.push(Firing {
                workspace: armed.workspace.clone(),
                shortcut: armed.shortcut.id.clone(),
                trigger: fired.trigger,
                invocation: fired.invocation,
                claim: None,
                inputs: fired.inputs,
            });
        }
    }
    firings
}

/// Fires a shortcut's record-changed triggers on matching changes, on one of the saver's devices.
#[derive(Debug, Default)]
pub struct RecordChangedTriggers {
    election: RecordElection,
}

impl TriggerSource for RecordChangedTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
        context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        self.election.observe(event, armed, context, |trigger| {
            matches!(trigger, TriggerSpec::ResourceChanged { .. })
        })
    }

    fn forget_started(&mut self, started: &BTreeSet<AutomationInvocationId>) {
        self.election.forget_started(started);
    }
}

/// Fires a shortcut's app-event triggers on the events applications declared,
/// whatever the application is: an event is a change to a record of its own
/// kind ([`AppEvent::of_change`]), so nothing here names an app.
#[derive(Clone, Copy, Debug, Default)]
pub struct AppEventTriggers;

impl TriggerSource for AppEventTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
        _context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        let TriggerEvent::Records(changes) = event else {
            return Vec::new();
        };
        changes
            .iter()
            .filter_map(AppEvent::of_change)
            .flat_map(|declared| {
                fire_matching(armed, |armed, trigger| match trigger {
                    TriggerSpec::AppEvent { app_id, kind }
                        if *app_id == declared.app_id
                            && *kind == declared.kind
                            && armed.workspace == declared.workspace =>
                    {
                        Some(vec![(
                            EVENT_INPUT,
                            Value::Object(app_event_object(&declared)),
                        )])
                    }
                    _ => None,
                })
            })
            .collect()
    }
}

/// Fires a shortcut's incoming-message triggers on a message someone else received in the saver's
/// workspaces, on one of the saver's devices.
#[derive(Debug, Default)]
pub struct IncomingMessageTriggers {
    election: RecordElection,
}

impl TriggerSource for IncomingMessageTriggers {
    fn observe(
        &mut self,
        event: TriggerEvent<'_>,
        armed: &[Armed],
        context: TriggerContext<'_>,
    ) -> Vec<Firing> {
        self.election.observe(event, armed, context, |trigger| {
            matches!(trigger, TriggerSpec::IncomingMessage { .. })
        })
    }

    fn forget_started(&mut self, started: &BTreeSet<AutomationInvocationId>) {
        self.election.forget_started(started);
    }
}

/// Every trigger source a session runs, reading one clock and one view of the person's devices.
pub struct TriggerHost {
    clock: Arc<dyn Clock>,
    devices: Arc<dyn DeviceDirectory>,
    sources: Vec<Box<dyn TriggerSource>>,
}

impl TriggerHost {
    /// Keyboard and schedule triggers on `clock` , with schedules kept in `schedules` (in memory
    /// only for `None` ).
    #[must_use]
    pub(crate) fn new(
        clock: Arc<dyn Clock>,
        schedules: Option<crate::kept::Kept>,
        wake: crate::automation::Wake,
    ) -> Self {
        Self {
            clock,
            devices: Arc::new(FixedDevices::default()),
            sources: vec![
                Box::new(KeyboardTriggers),
                Box::new(ScheduleTriggers::open(schedules, wake)),
                Box::new(AppOpenedTriggers::default()),
                Box::new(WorkspaceJoinedTriggers::default()),
                Box::new(FileChangedTriggers),
                Box::new(FocusModeTriggers::default()),
                Box::new(AppEventTriggers),
                Box::new(RecordChangedTriggers::default()),
                Box::new(IncomingMessageTriggers::default()),
            ],
        }
    }

    /// The session's triggers: the system clock, schedules kept in the device's store under the
    /// state root the session keeps its shell in.
    #[must_use]
    pub fn for_session(state: Option<&Path>, wake: crate::automation::Wake) -> Self {
        let schedules = state.map(|state| crate::kept::Kept::open(state, crate::kept::SCHEDULES));
        Self::new(Arc::new(SystemClock), schedules, wake)
    }

    /// Read the person's devices from `devices` , so their schedules run once across them.
    pub fn set_devices(&mut self, devices: Arc<dyn DeviceDirectory>) {
        self.devices = devices;
    }

    #[must_use]
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// Show `event` to every source; every firing, in source order.
    pub fn observe(&mut self, event: TriggerEvent<'_>, armed: &[Armed]) -> Vec<Firing> {
        let clock = Arc::clone(&self.clock);
        // Only schedules read the device list, and asking for it is a lock plus a copy of the
        // personal fabric: a keystroke must not pay for it.
        let devices = if matches!(event, TriggerEvent::Key(_)) {
            PersonalDevices::default()
        } else {
            self.devices.personal_devices()
        };
        let context = TriggerContext {
            clock: clock.as_ref(),
            devices: &devices,
        };
        self.sources
            .iter_mut()
            .flat_map(|source| source.observe(event, armed, context))
            .collect()
    }

    /// Runs another of the person's devices already started: no source starts them here.
    pub fn forget_started(&mut self, started: &BTreeSet<AutomationInvocationId>) {
        for source in &mut self.sources {
            source.forget_started(started);
        }
    }

    /// Firings an earlier tick held until their schedule log was written,
    /// now that it is. Reads no clock, device list or disk, so the loop can
    /// ask on every iteration.
    pub fn take_settled_firings(&mut self) -> Vec<Firing> {
        self.sources
            .iter_mut()
            .flat_map(|source| source.take_settled())
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
    use serde_json::json;
    use tilo_automation::Weekday;

    use super::*;

    fn at(text: &str) -> jiff::Timestamp {
        text.parse().expect("an RFC 3339 instant")
    }

    fn shortcut(trigger: &TriggerSpec) -> Shortcut {
        serde_json::from_value(json!({
            "id": "tidy",
            "name": "Tidy",
            "steps": [],
            "triggers": [trigger],
        }))
        .expect("shortcut JSON")
    }

    fn utc_clock(now: &str) -> Arc<ManualClock> {
        Arc::new(ManualClock::new(at(now), TimeZone::UTC))
    }

    tilo_test_env::scenario! {
        fn a_schedule_fires_at_its_own_times_once_each;

        phase a_calendar_schedule_fires_at_the_next_listed_local_time;
        {
            // 2026-09-25 is a Friday; the local clock is two hours ahead.
            let clock = ManualClock::new(
                at("2026-09-25T10:00:00Z"),
                TimeZone::fixed(jiff::tz::offset(2)),
            );
            let weekday_mornings = Schedule::Calendar {
                at: tilo_automation::TimeOfDay {
                    hour: 9,
                    minute: 30,
                },
                weekdays: vec![Weekday::Monday, Weekday::Friday],
            };
            assert_eq!(
                next_fire(&weekday_mornings, clock.now(), &clock),
                Some(at("2026-09-28T07:30:00Z")),
                "Friday 09:30 local has passed, so Monday's is next"
            );
            let daily_evening = Schedule::Calendar {
                at: tilo_automation::TimeOfDay {
                    hour: 18,
                    minute: 0,
                },
                weekdays: Vec::new(),
            };
            assert_eq!(
                next_fire(&daily_evening, clock.now(), &clock),
                Some(at("2026-09-25T16:00:00Z"))
            );
        }

        phase an_interval_fires_on_the_same_times_wherever_it_was_armed;
        {
            let clock = utc_clock("2026-09-25T10:00:00Z");
            let hourly = Schedule::Interval { seconds: 3600 };
            assert_eq!(
                next_fire(&hourly, clock.now(), clock.as_ref()),
                Some(at("2026-09-25T11:00:00Z"))
            );
            assert_eq!(
                next_fire(&hourly, at("2026-09-25T09:30:12Z"), clock.as_ref()),
                Some(at("2026-09-25T10:00:00Z")),
                "armed half way through a period, it still fires on the hour"
            );
        }

        phase an_hourly_schedule_fires_once_for_each_of_its_fire_times;
        {
            let clock = utc_clock("2026-09-25T08:00:10Z");
            let devices = PersonalDevices::alone("laptop");
            let context = TriggerContext {
                clock: clock.as_ref(),
                devices: &devices,
            };
            let mut schedules = ScheduleTriggers::open(None, Arc::new(|| ()));
            let armed = [Armed {
                workspace: "workspace".into(),
                shortcut: shortcut(&TriggerSpec::Schedule {
                    schedule: Schedule::Interval { seconds: 3600 },
                    catch_up: CatchUp::Skip,
                }),
                saver: "person".into(),
                claims: Vec::new(),
            }];
            assert!(
                schedules
                    .observe(TriggerEvent::Tick, &armed, context)
                    .is_empty(),
                "nothing is due before the hour"
            );
            clock.set(at("2026-09-25T09:00:05Z"));
            let fired = schedules.observe(TriggerEvent::Tick, &armed, context);
            assert_eq!(fired.len(), 1, "the fire time runs");
            assert_eq!(
                fired[0].invocation,
                scheduled_invocation(
                    &log_key("workspace", "tidy", 0),
                    at("2026-09-25T09:00:00Z")
                ),
                "the run is the fire time's, so the person's devices agree on it"
            );
            assert_eq!(
                fired[0].claim.as_ref().map(|claim| claim.device.as_str()),
                Some("laptop"),
                "this device claims the fire time it took"
            );
            for _ in 0..5 {
                assert!(
                    schedules
                        .observe(TriggerEvent::Tick, &armed, context)
                        .is_empty(),
                    "the same fire time never runs twice"
                );
            }
        }
    }

    tilo_test_env::scenario! {
        fn an_event_trigger_fires_on_the_event_it_names_and_no_other;

        phase a_keyboard_trigger_fires_on_its_chord_only;
        {
            let clock = utc_clock("2026-09-25T10:00:00Z");
            let mut host =
                TriggerHost::new(Arc::clone(&clock) as Arc<dyn Clock>, None, Arc::new(|| ()));
            let armed = [Armed {
                workspace: "workspace".into(),
                shortcut: shortcut(&TriggerSpec::Keyboard {
                    chord: "Super+Shift+N".into(),
                }),
                saver: "person".into(),
                claims: Vec::new(),
            }];
            let chord = KeyChord::parse("Super+Shift+N").expect("a chord");
            assert_eq!(
                host.observe(TriggerEvent::Key(&chord), &armed).len(),
                1,
                "the trigger's own chord starts it"
            );
            let other = KeyChord::parse("Super+Shift+M").expect("a chord");
            assert!(
                host.observe(TriggerEvent::Key(&other), &armed).is_empty(),
                "another chord starts nothing"
            );
        }

        phase an_app_opening_fires_once_it_starts_publishing;
        {
            let clock = utc_clock("2026-09-25T10:00:00Z");
            let mut host =
                TriggerHost::new(Arc::clone(&clock) as Arc<dyn Clock>, None, Arc::new(|| ()));
            let armed = [Armed {
                workspace: "workspace".into(),
                shortcut: shortcut(&TriggerSpec::AppOpened {
                    app_id: "so.tilo.notes".into(),
                }),
                saver: "person".into(),
                claims: Vec::new(),
            }];
            let mut running = BTreeSet::new();
            assert!(
                host.observe(TriggerEvent::Apps(&running), &armed)
                    .is_empty(),
                "the first look only learns what already runs"
            );
            running.insert("so.tilo.notes".to_owned());
            let fired = host.observe(TriggerEvent::Apps(&running), &armed);
            assert_eq!(fired.len(), 1, "the app opening starts the shortcut");
            assert!(
                host.observe(TriggerEvent::Apps(&running), &armed)
                    .is_empty(),
                "an app that stays running starts nothing again"
            );
        }

        phase a_focus_mode_fires_on_the_transition_it_names;
        {
            let clock = utc_clock("2026-09-25T10:00:00Z");
            let mut host =
                TriggerHost::new(Arc::clone(&clock) as Arc<dyn Clock>, None, Arc::new(|| ()));
            let armed = [Armed {
                workspace: "workspace".into(),
                shortcut: shortcut(&TriggerSpec::FocusMode {
                    mode: tilo_automation::FOCUS_DO_NOT_DISTURB.into(),
                    transition: tilo_automation::FocusTransition::Enter,
                }),
                saver: "person".into(),
                claims: Vec::new(),
            }];
            let off = FocusState::default();
            assert!(host.observe(TriggerEvent::Focus(off), &armed).is_empty());
            let on = FocusState {
                do_not_disturb: true,
                focus: false,
            };
            assert_eq!(
                host.observe(TriggerEvent::Focus(on), &armed).len(),
                1,
                "entering the mode starts the shortcut"
            );
            assert!(
                host.observe(TriggerEvent::Focus(on), &armed).is_empty(),
                "staying on is no transition"
            );
            assert_eq!(
                host.observe(TriggerEvent::Focus(off), &armed).len(),
                0,
                "leaving is another transition than the one it names"
            );
        }

        phase a_change_in_a_watched_folder_fires_the_trigger_naming_its_capability;
        {
            let clock = utc_clock("2026-09-25T10:00:00Z");
            let mut host =
                TriggerHost::new(Arc::clone(&clock) as Arc<dyn Clock>, None, Arc::new(|| ()));
            let armed = [Armed {
                workspace: "workspace".into(),
                shortcut: shortcut(&TriggerSpec::FileChanged {
                    capability: "cap-1".into(),
                }),
                saver: "person".into(),
                claims: Vec::new(),
            }];
            let watched = FolderChange {
                workspace: "workspace".into(),
                capability: "cap-1".into(),
                file: "Invoices/July.pdf".into(),
                change: FileChange::Created,
            };
            assert_eq!(
                host.observe(TriggerEvent::Files(&[watched]), &armed).len(),
                1,
                "the file appearing starts the shortcut"
            );
            let elsewhere = FolderChange {
                workspace: "workspace".into(),
                capability: "cap-2".into(),
                file: "Notes.md".into(),
                change: FileChange::Modified,
            };
            assert!(
                host.observe(TriggerEvent::Files(&[elsewhere]), &armed)
                    .is_empty(),
                "a folder the trigger does not name starts nothing"
            );
        }
    }
}
