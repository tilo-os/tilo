//! Deadlines apps hand the daemon to deliver as notifications, whether or
//! not the app runs then: a reminder at a time, or an alarm at a local wall
//! clock time, once or on chosen weekdays.
//!
//! Apps hand them over through the daemon (`XTiloScheduleNotification` and
//! its siblings, as `tilo-notify` calls them), the person through the
//! published resources. Each deadline belongs to the app and the workspace
//! that scheduled it: only they read, replace or cancel it. The ledger is
//! kept in the person's private state and every claim on it (a delivery,
//! a snooze) is written before it takes effect, so a deadline neither
//! repeats nor goes missing across restarts, suspend or clock changes.
//!
//! A delivered alarm offers Snooze and Dismiss. Snoozing rings it again
//! five minutes later, as an absolute instant; dismissing ends that
//! occurrence (a repeating alarm rings on its next day).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_notify::CalendarAlarm;

use crate::{Caller, Notification};

const MAX_ENTRIES: usize = 1024;
const MAX_PER_APP: usize = 64;
const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
/// How long a snooze puts an alarm off.
pub const SNOOZE_MS: i64 = 300_000;
/// The room of an app the session knows no workspace of.
pub const LOCAL_ROOM: &str = "local";
/// The action key of a ringing alarm's Snooze.
pub const SNOOZE: &str = "snooze";
/// The action key of a ringing alarm's Dismiss.
pub const DISMISS: &str = "dismiss";
/// How many delivered alarms keep their Snooze and Dismiss: what the
/// notification centre may still show.
const MAX_RUNG: usize = 256;

/// `(tag, (hour, minute, weekdays, enabled_at_ms), next_at_ms, last_at_ms,
/// last_rung)`: one alarm's state as `XTiloScheduledAlarms` answers it.
pub type AlarmStateWire = (String, (u8, u8, u8, i64), Vec<i64>, Vec<i64>, String);

/// Who a deadline belongs to: the app and the workspace (room) it runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub app: String,
    pub room: String,
}

impl Owner {
    /// The owner of what `caller` schedules.
    #[must_use]
    pub fn of(caller: &Caller) -> Self {
        Self {
            app: caller.app_id.clone(),
            room: caller
                .workspace
                .clone()
                .filter(|room| !room.is_empty())
                .unwrap_or_else(|| LOCAL_ROOM.to_owned()),
        }
    }
}

/// An alarm's local wall clock time, as an app hands it over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlarmRequest {
    pub hour: u8,
    pub minute: u8,
    /// Monday is bit zero; none rings once.
    pub weekdays: u8,
    /// When the person turned it on, as the app records it.
    pub enabled_at_ms: i64,
    /// The local date the app already rang it on, so the daemon does not
    /// ring it again that day.
    pub last_rung: Option<NaiveDate>,
}

/// One deadline as an app hands it over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScheduleRequest {
    /// Names the deadline among the app's own: scheduling under a tag again
    /// replaces it.
    pub tag: String,
    pub title: String,
    pub body: String,
    /// The URI the app is opened at when the person chooses the
    /// notification; empty for none.
    pub activation: String,
    /// When it falls due, in milliseconds since the epoch; for an alarm,
    /// its first occurrence.
    pub at_ms: i64,
    pub alarm: Option<AlarmRequest>,
}

/// `XTiloScheduleNotification`'s arguments: tag, title, body, activation,
/// `at_ms` and at most one `(hour, minute, weekdays, enabled_at_ms,
/// last_rung)` alarm.
pub type ScheduleArgs = (
    String,
    String,
    String,
    String,
    i64,
    Vec<(u8, u8, u8, i64, String)>,
);

impl ScheduleRequest {
    /// The request `XTiloScheduleNotification`'s arguments make.
    ///
    /// # Errors
    /// More than one alarm, or a malformed delivered date.
    pub fn from_wire(args: ScheduleArgs) -> Result<Self, String> {
        let (tag, title, body, activation, at_ms, alarms) = args;
        let alarm = match alarms.as_slice() {
            [] => None,
            [(hour, minute, weekdays, enabled_at_ms, last_rung)] => Some(AlarmRequest {
                hour: *hour,
                minute: *minute,
                weekdays: *weekdays,
                enabled_at_ms: *enabled_at_ms,
                last_rung: if last_rung.is_empty() {
                    None
                } else {
                    Some(
                        NaiveDate::parse_from_str(last_rung, "%Y-%m-%d")
                            .map_err(|_| "invalid delivered alarm date".to_owned())?,
                    )
                },
            }),
            _ => return Err("expected at most one calendar alarm".into()),
        };
        Ok(Self {
            tag,
            title,
            body,
            activation,
            at_ms,
            alarm,
        })
    }
}

/// The deadline a delivered notification rings for: which, and which of
/// its occurrences. It survives restarts, so the person may still act on
/// it from the notification centre.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Occurrence {
    /// The workspace the deadline belongs to.
    pub room: String,
    /// The deadline's own token, never reused.
    pub token: String,
    /// The occurrence, in milliseconds since the epoch.
    pub occurrence_ms: i64,
    /// It is an alarm, which offers Snooze and Dismiss.
    pub calendar_alarm: bool,
    /// The URI the app is opened at when chosen.
    pub uri: String,
}

/// What the person did with a ringing alarm.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AlarmAction {
    Snooze,
    Dismiss,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Calendar {
    hour: u8,
    minute: u8,
    weekdays: u8,
    enabled_at_ms: i64,
}

impl Calendar {
    fn alarm(&self) -> CalendarAlarm {
        CalendarAlarm {
            hour: self.hour,
            minute: self.minute,
            weekdays: self.weekdays,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Rung {
    at_ms: i64,
    action: Option<AlarmAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    room: String,
    app: String,
    tag: String,
    title: String,
    body: String,
    activation: String,
    original_at_ms: i64,
    next_at_ms: Option<i64>,
    calendar: Option<Calendar>,
    last_rung: Option<NaiveDate>,
    #[serde(default)]
    last_at_ms: Option<i64>,
    #[serde(default)]
    token: String,
    #[serde(default)]
    active: Option<Rung>,
    #[serde(default)]
    snooze_at_ms: Option<i64>,
}

impl Entry {
    fn owned(&self, owner: &Owner) -> bool {
        self.app == owner.app && self.room == owner.room
    }

    fn next_at_ms(&self) -> Option<i64> {
        self.next_at_ms.into_iter().chain(self.snooze_at_ms).min()
    }

    fn same_request(&self, other: &Self) -> bool {
        self.title == other.title
            && self.body == other.body
            && self.activation == other.activation
            && self.calendar == other.calendar
            && (self.calendar.is_some() || self.original_at_ms == other.original_at_ms)
    }

    fn snoozed_until(&self) -> Option<i64> {
        let active = self.active.as_ref()?;
        (self.calendar.is_some() && active.action == Some(AlarmAction::Snooze))
            .then_some(self.snooze_at_ms)
            .flatten()
    }
}

/// One deadline as the daemon publishes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deadline {
    pub token: String,
    pub app_id: String,
    pub room: String,
    pub tag: String,
    pub title: String,
    pub body: String,
    /// When it next falls due, if it does.
    pub next_at_ms: Option<i64>,
    /// An alarm's local time: `(hour, minute, weekdays)`.
    pub alarm: Option<(u8, u8, u8)>,
    /// A snoozed alarm rings again then.
    pub snoozed_until: Option<i64>,
}

fn token() -> String {
    let mut bytes = [0_u8; 16];
    loop {
        if let Err(error) = getrandom::fill(&mut bytes) {
            tracing::warn!(target: "notifications", %error, "no randomness for a deadline's token");
        }
        if bytes != [0; 16] {
            return hex::encode(bytes);
        }
        // All zero names no deadline; the generator failed or was very
        // unlucky: count up from it rather than spin.
        bytes[15] = 1;
    }
}

fn valid_token(token: &str) -> bool {
    token.len() == 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The ledger: every deadline, as last written.
struct Ledger {
    entries: Vec<Entry>,
    path: Option<PathBuf>,
}

impl Ledger {
    fn open(path: Option<PathBuf>) -> Result<Self, String> {
        let entries = match &path {
            Some(path) => match std::fs::metadata(path) {
                Ok(metadata) if metadata.len() <= MAX_STORE_BYTES => {
                    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
                    serde_json::from_slice::<Vec<Entry>>(&bytes).map_err(|e| e.to_string())?
                }
                Ok(_) => return Err("notification schedule store exceeds its limit".into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
                Err(e) => return Err(e.to_string()),
            },
            None => Vec::new(),
        };
        if entries.len() > MAX_ENTRIES {
            return Err("too many stored notification schedules".into());
        }
        Ok(Self { entries, path })
    }

    /// Write `entries`, then take them as the ledger. A failed write
    /// changes nothing.
    fn commit(&mut self, entries: Vec<Entry>) -> Result<(), String> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&entries).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err("notification schedule store exceeds its limit".into());
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            tilo_files::native::atomic::write_private(path, &bytes).map_err(|e| e.to_string())?;
        }
        self.entries = entries;
        Ok(())
    }

    fn schedule(&mut self, owner: &Owner, request: ScheduleRequest) -> Result<(), String> {
        let ScheduleRequest {
            tag,
            title,
            body,
            activation,
            at_ms,
            alarm,
        } = request;
        let (calendar, last_rung) = match alarm {
            None => (None, None),
            Some(alarm) => {
                let calendar = Calendar {
                    hour: alarm.hour,
                    minute: alarm.minute,
                    weekdays: alarm.weekdays,
                    enabled_at_ms: alarm.enabled_at_ms,
                };
                if !calendar.alarm().valid() {
                    return Err("invalid alarm time or weekdays".into());
                }
                (Some(calendar), alarm.last_rung)
            }
        };
        if tag.is_empty()
            || tag.len() > 128
            || tag.chars().any(char::is_control)
            || title.trim().is_empty()
            || title.len() > 512
            || body.len() > 4096
            || activation.len() > 2048
            || activation.chars().any(char::is_control)
            || (!activation.is_empty() && !tilo_notify::valid_activation(&activation))
            || DateTime::<Utc>::from_timestamp_millis(at_ms).is_none()
        {
            return Err("invalid scheduled notification".into());
        }
        let entry = Entry {
            room: owner.room.clone(),
            app: owner.app.clone(),
            tag,
            title,
            body,
            activation,
            original_at_ms: at_ms,
            next_at_ms: Some(at_ms),
            calendar,
            last_rung,
            last_at_ms: None,
            token: token(),
            active: None,
            snooze_at_ms: None,
        };
        let mut entries = self.entries.clone();
        if let Some(old) = entries
            .iter_mut()
            .find(|old| old.owned(owner) && old.tag == entry.tag)
        {
            // Reopening an app must not rearm an occurrence already delivered.
            if old.same_request(&entry) {
                return Ok(());
            }
            if old.calendar == entry.calendar
                && (entry.calendar.is_some() || old.original_at_ms == entry.original_at_ms)
            {
                old.title = entry.title;
                old.body = entry.body;
                old.activation = entry.activation;
            } else {
                *old = entry;
            }
        } else {
            let app_full = entries.iter().filter(|e| e.owned(owner)).count() >= MAX_PER_APP;
            if entries.len() >= MAX_ENTRIES || app_full {
                if let Some(index) = entries
                    .iter()
                    .position(|e| e.next_at_ms().is_none() && (!app_full || e.owned(owner)))
                {
                    entries.remove(index);
                } else {
                    return Err("notification schedule limit reached".into());
                }
            }
            entries.push(entry);
        }
        self.commit(entries)
    }

    fn cancel(&mut self, owner: &Owner, tag: &str) -> Result<(), String> {
        let mut entries = self.entries.clone();
        entries.retain(|entry| !(entry.owned(owner) && entry.tag == tag));
        self.commit(entries)
    }

    fn pending(&self, owner: &Owner) -> Vec<(String, i64)> {
        self.entries
            .iter()
            .filter(|e| e.owned(owner))
            .filter_map(|e| Some((e.tag.clone(), e.next_at_ms()?)))
            .collect()
    }

    fn alarm_states(&self, owner: &Owner) -> Vec<AlarmStateWire> {
        self.entries
            .iter()
            .filter(|entry| entry.owned(owner))
            .filter_map(|entry| {
                let c = entry.calendar.as_ref()?;
                Some((
                    entry.tag.clone(),
                    (c.hour, c.minute, c.weekdays, c.enabled_at_ms),
                    entry.next_at_ms.into_iter().collect(),
                    entry.last_at_ms.into_iter().collect(),
                    entry.last_rung.map(|d| d.to_string()).unwrap_or_default(),
                ))
            })
            .collect()
    }

    fn next_at_ms(&self) -> Option<i64> {
        self.entries.iter().filter_map(Entry::next_at_ms).min()
    }

    /// Snooze or dismiss occurrence `target` of app `app_id`'s alarm.
    fn act(
        &mut self,
        app_id: &str,
        target: &Occurrence,
        action: AlarmAction,
        now_ms: i64,
    ) -> Result<(), String> {
        let mut entries = self.entries.clone();
        if !target.calendar_alarm || !valid_token(&target.token) {
            return Err("this notification is not a calendar alarm".into());
        }
        let entry = entries.iter_mut().find(|entry| {
            entry.app == app_id
                && entry.room == target.room
                && entry.token == target.token
                && entry.activation == target.uri
        });
        let Some(entry) = entry else {
            return if action == AlarmAction::Dismiss {
                Ok(())
            } else {
                Err("scheduled occurrence is no longer available".into())
            };
        };
        if entry.calendar.is_none() {
            return Err("this notification is not a calendar alarm".into());
        }
        let active = entry
            .active
            .as_mut()
            .filter(|active| active.at_ms == target.occurrence_ms);
        let Some(active) = active else {
            // Dismissing an old card retires only that card: it never
            // changes a newer ring or resurrects a cancelled schedule.
            return if action == AlarmAction::Dismiss {
                Ok(())
            } else {
                Err("scheduled occurrence is stale".into())
            };
        };
        if active.action == Some(action) {
            return Ok(());
        }
        if active.action == Some(AlarmAction::Dismiss) {
            return Err("scheduled occurrence was already dismissed".into());
        }
        entry.snooze_at_ms = match action {
            AlarmAction::Snooze => Some(
                now_ms
                    .checked_add(SNOOZE_MS)
                    .filter(|at| DateTime::<Utc>::from_timestamp_millis(*at).is_some())
                    .ok_or("invalid snooze deadline")?,
            ),
            AlarmAction::Dismiss => None,
        };
        active.action = Some(action);
        self.commit(entries)
    }

    /// Claim every deadline due at `now`: written before it is handed
    /// over, so a failed write delivers nothing and loses nothing. Each
    /// due deadline as the notification it rings as, made with `alloc`'s
    /// ids.
    fn take_due<Tz: TimeZone>(
        &mut self,
        now: &DateTime<Tz>,
        mut alloc: impl FnMut() -> u32,
    ) -> Result<Vec<Notification>, String> {
        let now_ms = now.timestamp_millis();
        let mut entries = self.entries.clone();
        let mut due = Vec::new();
        for entry in &mut entries {
            if let Some(calendar) = &entry.calendar
                && entry.next_at_ms.is_some_and(|at| at > now_ms)
            {
                // Calendar deadlines follow time zone changes; a snooze is
                // an absolute instant and never shifts with the zone.
                entry.next_at_ms = calendar
                    .alarm()
                    .next_in(entry.last_rung, now)
                    .map(|date| date.timestamp_millis());
            }
            let calendar_due = entry.next_at_ms.filter(|at| *at <= now_ms);
            let snooze_due = entry.snooze_at_ms.filter(|at| *at <= now_ms);
            let Some(at_ms) = calendar_due.into_iter().chain(snooze_due).max() else {
                continue;
            };
            if entry.token.is_empty() {
                entry.token = token();
            }
            due.push(ringing(alloc(), entry, at_ms));
            entry.active = entry.calendar.as_ref().map(|_| Rung {
                at_ms,
                action: None,
            });
            if let Some(at_ms) = calendar_due {
                entry.last_at_ms = Some(at_ms);
                entry.last_rung = DateTime::<Utc>::from_timestamp_millis(at_ms)
                    .map(|date| date.with_timezone(&now.timezone()).date_naive());
                entry.next_at_ms = entry
                    .calendar
                    .as_ref()
                    .filter(|c| c.weekdays != 0)
                    .and_then(|c| c.alarm().next_in(entry.last_rung, now))
                    .map(|d| d.timestamp_millis());
            }
            // A new occurrence supersedes a pending snooze of the one
            // before; when both are overdue only one catch-up rings.
            entry.snooze_at_ms = None;
        }
        if !due.is_empty()
            || self
                .entries
                .iter()
                .zip(&entries)
                .any(|(a, b)| a.next_at_ms != b.next_at_ms)
        {
            self.commit(entries)?;
        }
        Ok(due)
    }

    fn deadlines(&self) -> Vec<Deadline> {
        self.entries
            .iter()
            .map(|entry| Deadline {
                token: entry.token.clone(),
                app_id: entry.app.clone(),
                room: entry.room.clone(),
                tag: entry.tag.clone(),
                title: entry.title.clone(),
                body: entry.body.clone(),
                next_at_ms: entry.next_at_ms(),
                alarm: entry
                    .calendar
                    .as_ref()
                    .map(|c| (c.hour, c.minute, c.weekdays)),
                snoozed_until: entry.snoozed_until(),
            })
            .collect()
    }
}

/// The notification deadline `entry` rings as at `at_ms`.
fn ringing(id: u32, entry: &Entry, at_ms: i64) -> Notification {
    let calendar_alarm = entry.calendar.is_some();
    Notification {
        app_name: entry.app.clone(),
        summary: entry.title.clone(),
        body: entry.body.clone(),
        // Persistent: it waits for the person, who may be away.
        expire_timeout: 0,
        category: calendar_alarm.then(|| "alarm.ringing".to_owned()),
        caller: Some(Caller {
            app_id: entry.app.clone(),
            workspace: (entry.room != LOCAL_ROOM).then(|| entry.room.clone()),
        }),
        // The labels are the session's to say in the person's language.
        actions: if calendar_alarm {
            vec![
                (DISMISS.to_owned(), "Dismiss".to_owned()),
                (SNOOZE.to_owned(), "Snooze".to_owned()),
            ]
        } else {
            Vec::new()
        },
        scheduled: Some(Occurrence {
            room: entry.room.clone(),
            token: entry.token.clone(),
            occurrence_ms: at_ms,
            calendar_alarm,
            uri: entry.activation.clone(),
        }),
        ..Notification::new(id)
    }
}

/// The deadlines, shared by the daemon's D-Bus face, its delivery and its
/// resources. Cloning shares them.
#[derive(Clone)]
pub struct Schedules {
    /// `None` when the ledger could not be opened: scheduling is refused
    /// rather than kept where it would be lost.
    ledger: Arc<Mutex<Option<Ledger>>>,
    /// Wakes the delivery when a deadline was added or moved.
    wake: Arc<tokio::sync::Notify>,
    /// The alarms delivered, by notification id: what their Snooze and
    /// Dismiss act on, also once the banner is gone.
    rung: Arc<Mutex<BTreeMap<u32, (String, Occurrence)>>>,
}

impl std::fmt::Debug for Schedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Schedules")
            .field("available", &self.ledger.lock_recover().is_some())
            .finish_non_exhaustive()
    }
}

impl Schedules {
    /// The deadlines kept in the person's private state.
    #[must_use]
    pub fn open_default() -> Self {
        let path = tilo_paths::xdg::state_home()
            .map(|root| root.join("tilo/notifications/schedules.json"));
        if path.is_none() {
            tracing::warn!(target: "notifications", "no state directory: notification schedules unavailable");
            return Self::unavailable();
        }
        Self::open(path)
    }

    /// Deadlines kept in memory only.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::open(None)
    }

    /// Deadlines kept at `path`, when there is one.
    #[must_use]
    pub fn open(path: Option<PathBuf>) -> Self {
        match Ledger::open(path) {
            Ok(ledger) => Self::over(Some(ledger)),
            Err(error) => {
                tracing::warn!(target: "notifications", %error, "notification schedules unavailable");
                Self::unavailable()
            }
        }
    }

    fn unavailable() -> Self {
        Self::over(None)
    }

    fn over(ledger: Option<Ledger>) -> Self {
        Self {
            ledger: Arc::new(Mutex::new(ledger)),
            wake: Arc::default(),
            rung: Arc::default(),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Ledger) -> Result<T, String>) -> Result<T, String> {
        self.ledger
            .lock_recover()
            .as_mut()
            .ok_or_else(|| "durable notification scheduler unavailable".to_owned())
            .and_then(f)
    }

    /// Schedule `request` for `owner`, replacing its deadline of the same
    /// tag.
    ///
    /// # Errors
    /// The request is malformed, the limits are reached, or the ledger
    /// could not be written.
    pub fn schedule(&self, owner: &Owner, request: ScheduleRequest) -> Result<(), String> {
        self.with(|ledger| ledger.schedule(owner, request))?;
        self.wake.notify_one();
        Ok(())
    }

    /// Cancel `owner`'s deadline `tag`.
    ///
    /// # Errors
    /// The ledger could not be written.
    pub fn cancel(&self, owner: &Owner, tag: &str) -> Result<(), String> {
        self.with(|ledger| ledger.cancel(owner, tag))?;
        self.wake.notify_one();
        Ok(())
    }

    /// Cancel the deadline with `token`, whoever it belongs to: the person
    /// cancelling it. Whether there was one.
    ///
    /// # Errors
    /// The ledger could not be written.
    pub fn cancel_token(&self, token: &str) -> Result<bool, String> {
        let owner = self.with(|ledger| {
            Ok(ledger
                .entries
                .iter()
                .find(|entry| entry.token == token)
                .map(|entry| {
                    (
                        Owner {
                            app: entry.app.clone(),
                            room: entry.room.clone(),
                        },
                        entry.tag.clone(),
                    )
                }))
        })?;
        let Some((owner, tag)) = owner else {
            return Ok(false);
        };
        self.cancel(&owner, &tag)?;
        Ok(true)
    }

    /// Dismiss the snoozed occurrence of the alarm with `token`: it does
    /// not ring again until its next day. Whether one was snoozed.
    ///
    /// # Errors
    /// The ledger could not be written.
    pub fn dismiss_snooze(&self, token: &str, now_ms: i64) -> Result<bool, String> {
        let target = self.with(|ledger| {
            Ok(ledger
                .entries
                .iter()
                .find(|entry| entry.token == token)
                .and_then(|entry| {
                    entry.snoozed_until()?;
                    Some((
                        entry.app.clone(),
                        Occurrence {
                            room: entry.room.clone(),
                            token: entry.token.clone(),
                            occurrence_ms: entry.active.as_ref()?.at_ms,
                            calendar_alarm: true,
                            uri: entry.activation.clone(),
                        },
                    ))
                }))
        })?;
        let Some((app, target)) = target else {
            return Ok(false);
        };
        self.act(&app, &target, AlarmAction::Dismiss, now_ms)?;
        Ok(true)
    }

    /// `owner`'s deadlines still to fall due: `(tag, at_ms)`.
    ///
    /// # Errors
    /// The ledger is unavailable.
    pub fn pending(&self, owner: &Owner) -> Result<Vec<(String, i64)>, String> {
        self.with(|ledger| Ok(ledger.pending(owner)))
    }

    /// `owner`'s alarms and what the daemon delivered of them.
    ///
    /// # Errors
    /// The ledger is unavailable.
    pub fn alarm_states(&self, owner: &Owner) -> Result<Vec<AlarmStateWire>, String> {
        self.with(|ledger| Ok(ledger.alarm_states(owner)))
    }

    /// Every deadline, as published.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.with(|ledger| Ok(ledger.deadlines()))
            .unwrap_or_default()
    }

    /// When the next deadline falls due, if any.
    #[must_use]
    pub fn next_at_ms(&self) -> Option<i64> {
        self.with(|ledger| Ok(ledger.next_at_ms())).ok().flatten()
    }

    /// Snooze or dismiss occurrence `target` of `app_id`'s alarm, at
    /// `now_ms`.
    ///
    /// # Errors
    /// It is no alarm of that app, the occurrence is stale, or the ledger
    /// could not be written.
    pub fn act(
        &self,
        app_id: &str,
        target: &Occurrence,
        action: AlarmAction,
        now_ms: i64,
    ) -> Result<(), String> {
        self.with(|ledger| ledger.act(app_id, target, action, now_ms))?;
        self.wake.notify_one();
        Ok(())
    }

    /// Claim what is due at `now` and hand it over as notifications, with
    /// ids from `alloc`.
    ///
    /// # Errors
    /// The ledger is unavailable or could not be written; nothing was
    /// claimed.
    pub fn take_due<Tz: TimeZone>(
        &self,
        now: &DateTime<Tz>,
        alloc: impl FnMut() -> u32,
    ) -> Result<Vec<Notification>, String> {
        let due = self.with(|ledger| ledger.take_due(now, alloc))?;
        let mut rung = self.rung.lock_recover();
        for notification in &due {
            if let Some(occurrence) = notification
                .scheduled
                .clone()
                .filter(|occurrence| occurrence.calendar_alarm)
            {
                let app = notification
                    .caller
                    .as_ref()
                    .map(|caller| caller.app_id.clone())
                    .unwrap_or_default();
                rung.insert(notification.id, (app, occurrence));
                while rung.len() > MAX_RUNG {
                    rung.pop_first();
                }
            }
        }
        drop(rung);
        Ok(due)
    }

    /// The person chose action `key` on notification `id`: when it is a
    /// delivered alarm's Snooze or Dismiss, carry it out. Whether it was.
    pub(crate) fn alarm_action(&self, id: u32, key: &str) -> bool {
        let action = match key {
            SNOOZE => AlarmAction::Snooze,
            DISMISS => AlarmAction::Dismiss,
            _ => return false,
        };
        let Some((app, target)) = self.rung.lock_recover().get(&id).cloned() else {
            return false;
        };
        match self.act(&app, &target, action, tilo_clock::unix_now_ms_i64()) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(target: "notifications", %error, "alarm action not carried out");
                false
            }
        }
    }

    /// Wait until a deadline was added, moved or acted on.
    pub async fn changed(&self) {
        self.wake.notified().await;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "a fixture that fails fails the test")]
mod tests;
