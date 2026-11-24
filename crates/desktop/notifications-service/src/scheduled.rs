//! Deadlines apps hand the daemon to deliver as notifications, whether or
//! not the app runs then: a reminder at a time, or an alarm at a local wall
//! clock time, once or on chosen weekdays.
//!
//! Apps hand them over through the daemon (`XTiloScheduleNotification` and
//! its siblings, as `tilo-notify` calls them), the person through the
//! published resources. Each deadline belongs to the app and the workspace
//! that scheduled it: only they read, replace or cancel it. The deadlines are
//! the person's records in the workspace (one [`RECORD_KIND`] record per app,
//! which only they read), so a deadline and every claim on it (a delivery, a
//! snooze) neither repeats nor goes missing across restarts, suspend or clock
//! changes. Until the session reaches the workspace they are held here, and
//! written there once it does.
//!
//! A delivered alarm offers Snooze and Dismiss. Snoozing rings it again
//! five minutes later, as an absolute instant; dismissing ends that
//! occurrence (a repeating alarm rings on its next day).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};

use collab_types::store::wire::{Command, Reply};
use jiff::civil::Date;
use serde::{Deserialize, Serialize};
use tilo_lock::MutexRecover as _;
use tilo_notify::CalendarAlarm;

use crate::{Caller, Notification};

const MAX_ENTRIES: usize = 1024;
const MAX_PER_APP: usize = 64;
/// How long a snooze puts an alarm off.
pub const SNOOZE_MS: i64 = 300_000;
/// The workspace of an app the session knows none of.
pub const LOCAL_WORKSPACE: &str = "local";
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

/// Who a deadline belongs to: the app and the workspace it runs in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Owner {
    pub app: String,
    pub workspace: String,
}

impl Owner {
    /// The owner of what `caller` schedules.
    #[must_use]
    pub fn of(caller: &Caller) -> Self {
        Self {
            app: caller.app_id.clone(),
            workspace: caller
                .workspace
                .clone()
                .filter(|workspace| !workspace.is_empty())
                .unwrap_or_else(|| LOCAL_WORKSPACE.to_owned()),
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
    pub last_rung: Option<Date>,
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
                        Date::strptime("%Y-%m-%d", last_rung)
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
    pub workspace: String,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Rung {
    at_ms: i64,
    action: Option<AlarmAction>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Entry {
    workspace: String,
    app: String,
    tag: String,
    title: String,
    body: String,
    activation: String,
    original_at_ms: i64,
    next_at_ms: Option<i64>,
    calendar: Option<Calendar>,
    last_rung: Option<Date>,
    last_at_ms: Option<i64>,
    token: String,
    active: Option<Rung>,
    snooze_at_ms: Option<i64>,
}

impl Entry {
    fn owned(&self, owner: &Owner) -> bool {
        self.app == owner.app && self.workspace == owner.workspace
    }

    fn owner(&self) -> Owner {
        Owner {
            app: self.app.clone(),
            workspace: self.workspace.clone(),
        }
    }

    /// The same deadline as `other`: of the same owner, under the same tag.
    fn same_deadline(&self, other: &Self) -> bool {
        self.app == other.app && self.workspace == other.workspace && self.tag == other.tag
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
    pub workspace: String,
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

/// The kind of the workspace records the deadlines are kept in: one per app, under the app's id,
/// holding every deadline of that app.
pub const RECORD_KIND: &str = "notifications.schedule";

/// The app the records are kept as: the session's own, which runs the daemon.
const RECORD_APP: &str = "so.tilo.session";

/// The field of a schedule record holding its app's deadlines, as JSON.
const ENTRIES_FIELD: &str = "entries";

/// The log: every deadline, as last written, and what of it the workspace does not hold yet.
#[derive(Default)]
struct Log {
    entries: Vec<Entry>,
    /// The apps whose deadlines changed since their record was last written to the workspace.
    unwritten: BTreeSet<String>,
    /// Whether what the workspace holds was taken in: until then the log holds only what was
    /// scheduled since the daemon started.
    kept: bool,
    /// The person the records are kept for, who alone reads them.
    person: Option<String>,
    /// Why the workspace last refused to keep the deadlines, while it still does.
    refusal: Option<String>,
}

impl Log {
    /// Take `entries` as the log, noting each app whose deadlines changed.
    fn commit(&mut self, entries: Vec<Entry>) {
        let changed: BTreeSet<String> = self
            .entries
            .iter()
            .chain(&entries)
            .map(|entry| entry.app.clone())
            .filter(|app| of_app(&self.entries, app).ne(of_app(&entries, app)))
            .collect();
        self.unwritten.extend(changed);
        self.entries = entries;
    }

    /// Keep the log in the workspace `store` answers: the first time, take in the deadlines it
    /// holds, what was scheduled here meanwhile standing over them; every time, write the record
    /// of each app whose deadlines changed. Whether the deadlines were taken in now.
    fn keep_in(&mut self, store: &dyn tilo_service::WorkspaceStore) -> Result<bool, String> {
        let person = self
            .person
            .clone()
            .ok_or("the deadlines have no person to be kept for")?;
        let taken = !self.kept;
        if taken {
            let mut held = held_entries(store)?;
            held.retain(|old| !self.entries.iter().any(|new| new.same_deadline(old)));
            held.append(&mut self.entries);
            self.entries = held;
            self.kept = true;
        }
        while let Some(app) = self.unwritten.pop_first() {
            let entries: Vec<&Entry> = of_app(&self.entries, &app).collect();
            if let Err(error) = write_record(store, &app, &entries, &person) {
                self.unwritten.insert(app);
                return Err(error);
            }
        }
        Ok(taken)
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
            || jiff::Timestamp::from_millisecond(at_ms).is_err()
        {
            return Err("invalid scheduled notification".into());
        }
        let entry = Entry {
            workspace: owner.workspace.clone(),
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
        self.commit(entries);
        Ok(())
    }

    fn cancel(&mut self, owner: &Owner, tag: &str) {
        let mut entries = self.entries.clone();
        entries.retain(|entry| !(entry.owned(owner) && entry.tag == tag));
        self.commit(entries);
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
                && entry.workspace == target.workspace
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
                    .filter(|at| jiff::Timestamp::from_millisecond(*at).is_ok())
                    .ok_or("invalid snooze deadline")?,
            ),
            AlarmAction::Dismiss => None,
        };
        active.action = Some(action);
        self.commit(entries);
        Ok(())
    }

    /// Claim every deadline due at `now`, so it rings once. Each due
    /// deadline as the notification it rings as, made with `alloc`'s ids.
    fn take_due(&mut self, now: &jiff::Zoned, mut alloc: impl FnMut() -> u32) -> Vec<Notification> {
        let now_ms = now.timestamp().as_millisecond();
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
                    .map(|date| date.timestamp().as_millisecond());
            }
            let calendar_due = entry.next_at_ms.filter(|at| *at <= now_ms);
            let snooze_due = entry.snooze_at_ms.filter(|at| *at <= now_ms);
            let Some(at_ms) = calendar_due.into_iter().chain(snooze_due).max() else {
                continue;
            };
            due.push(ringing(alloc(), entry, at_ms));
            entry.active = entry.calendar.as_ref().map(|_| Rung {
                at_ms,
                action: None,
            });
            if let Some(at_ms) = calendar_due {
                entry.last_at_ms = Some(at_ms);
                entry.last_rung = jiff::Timestamp::from_millisecond(at_ms)
                    .ok()
                    .map(|date| date.to_zoned(now.time_zone().clone()).date());
                entry.next_at_ms = entry
                    .calendar
                    .as_ref()
                    .filter(|c| c.weekdays != 0)
                    .and_then(|c| c.alarm().next_in(entry.last_rung, now))
                    .map(|d| d.timestamp().as_millisecond());
            }
            // A new occurrence supersedes a pending snooze of the one
            // before; when both are overdue only one catch-up rings.
            entry.snooze_at_ms = None;
        }
        self.commit(entries);
        due
    }

    fn deadlines(&self) -> Vec<Deadline> {
        self.entries
            .iter()
            .map(|entry| Deadline {
                token: entry.token.clone(),
                app_id: entry.app.clone(),
                workspace: entry.workspace.clone(),
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

/// The deadlines of `app` among `entries`.
fn of_app<'a>(entries: &'a [Entry], app: &'a str) -> impl Iterator<Item = &'a Entry> {
    entries.iter().filter(move |entry| entry.app == app)
}

/// Carry out `command` in the workspace `store` answers.
fn command(store: &dyn tilo_service::WorkspaceStore, command: &Command) -> Result<Reply, String> {
    let bytes = serde_json::to_vec(command).map_err(|error| error.to_string())?;
    let reply = store.command(RECORD_APP, &bytes)?;
    serde_json::from_slice(&reply).map_err(|error| error.to_string())
}

/// Every deadline the workspace `store` answers holds, read from each app's record.
fn held_entries(store: &dyn tilo_service::WorkspaceStore) -> Result<Vec<Entry>, String> {
    let mut query = collab_types::store::Query::of(RECORD_KIND, MAX_ENTRIES);
    let mut held = Vec::new();
    loop {
        let Reply::Page(page) = command(store, &Command::Query(query.clone()))? else {
            return Err("the workspace answered a query of the deadlines with no page".into());
        };
        for (app, record) in page.resources {
            let entries = match record.fields.get(ENTRIES_FIELD) {
                Some(collab_types::store::Cell::One(collab_types::store::Value::Text(json))) => {
                    serde_json::from_str::<Vec<Entry>>(json)
                        .map_err(|error| format!("the deadlines of {app} do not read: {error}"))?
                }
                _ => return Err(format!("the record of {app}'s deadlines holds none")),
            };
            if entries.iter().any(|entry| entry.app != app) {
                return Err(format!(
                    "the record of {app}'s deadlines holds another app's"
                ));
            }
            held.extend(entries);
        }
        if held.len() > MAX_ENTRIES {
            return Err("the workspace holds too many deadlines".into());
        }
        match page.next {
            Some(next) => query.after = Some(next),
            None => return Ok(held),
        }
    }
}

/// Write `entries` as the record of `app`'s deadlines, which only `person` reads.
fn write_record(
    store: &dyn tilo_service::WorkspaceStore,
    app: &str,
    entries: &[&Entry],
    person: &str,
) -> Result<(), String> {
    let json = serde_json::to_string(entries).map_err(|error| error.to_string())?;
    let resource = collab_types::store::Resource {
        readers: person.to_owned(),
        parent: None,
        order: None,
        fields: BTreeMap::from([(
            ENTRIES_FIELD.to_owned(),
            collab_types::store::Cell::One(collab_types::store::Value::Text(json)),
        )]),
    };
    let stamp = tilo_resource_store::Stamp {
        verb: "schedule".to_owned(),
        invocation: token(),
        ..tilo_resource_store::Stamp::default()
    };
    match command(
        store,
        &Command::Put {
            kind: RECORD_KIND.to_owned(),
            id: app.to_owned(),
            resource,
            stamp,
        },
    )? {
        Reply::Written => Ok(()),
        other => Err(format!(
            "the workspace answered a write of the deadlines with {other:?}"
        )),
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
            workspace: (entry.workspace != LOCAL_WORKSPACE).then(|| entry.workspace.clone()),
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
            workspace: entry.workspace.clone(),
            token: entry.token.clone(),
            occurrence_ms: at_ms,
            calendar_alarm,
            uri: entry.activation.clone(),
        }),
        ..Notification::new(id)
    }
}

/// Told whenever the deadlines changed, from whichever thread changed them.
type OnChange = Arc<dyn Fn() + Send + Sync>;

/// The deadlines, shared by the daemon's D-Bus face, its delivery and its
/// resources. Cloning shares them.
#[derive(Clone, Default)]
pub struct Schedules {
    log: Arc<Mutex<Log>>,
    /// Wakes the delivery when a deadline was added or moved.
    wake: Arc<tokio::sync::Notify>,
    /// Wakes whoever writes the deadlines to the workspace when they changed.
    written: Arc<Mutex<Option<OnChange>>>,
    /// The alarms delivered, by notification id: what their Snooze and
    /// Dismiss act on, also once the banner is gone.
    rung: Arc<Mutex<BTreeMap<u32, (String, Occurrence)>>>,
}

impl std::fmt::Debug for Schedules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let log = self.log.lock_recover();
        f.debug_struct("Schedules")
            .field("deadlines", &log.entries.len())
            .field("kept", &log.kept)
            .finish_non_exhaustive()
    }
}

impl Schedules {
    /// Deadlines held here until they are kept in a workspace ([`Self::keep_in`]).
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Keep the records for `person`, who alone reads them.
    pub fn own_person(&self, person: &str) {
        self.log.lock_recover().person = Some(person.to_owned());
    }

    /// Call `wake` whenever the deadlines changed, from whichever thread changed them: what
    /// keeps them in the workspace writes them on its next pass.
    pub fn on_change(&self, wake: OnChange) {
        *self.written.lock_recover() = Some(wake);
    }

    /// Keep the deadlines in the workspace `store` answers: the first time, take in those it
    /// holds; every time, write those that changed since. Whether they were taken in now. A
    /// write the workspace refused is made again on the next call.
    ///
    /// # Errors
    /// The workspace's records do not read, or it refused a write; a refusal told already is
    /// not told again while it stands.
    pub fn keep_in(&self, store: &dyn tilo_service::WorkspaceStore) -> Result<bool, String> {
        let mut log = self.log.lock_recover();
        let kept = log.keep_in(store);
        let refusal = kept.as_ref().err().cloned();
        if refusal.is_some() && refusal == log.refusal {
            return Ok(false);
        }
        log.refusal = refusal;
        drop(log);
        let taken = kept?;
        if taken {
            self.wake.notify_one();
        }
        Ok(taken)
    }

    fn with<T>(&self, f: impl FnOnce(&mut Log) -> T) -> T {
        f(&mut self.log.lock_recover())
    }

    /// The deadlines changed: the delivery looks at them again, and they are written to the
    /// workspace.
    fn moved(&self) {
        self.wake.notify_one();
        let written = self.written.lock_recover().clone();
        if let Some(written) = written {
            written();
        }
    }

    /// Schedule `request` for `owner`, replacing its deadline of the same
    /// tag.
    ///
    /// # Errors
    /// The request is malformed, or the limits are reached.
    pub fn schedule(&self, owner: &Owner, request: ScheduleRequest) -> Result<(), String> {
        self.with(|log| log.schedule(owner, request))?;
        self.moved();
        Ok(())
    }

    /// Cancel `owner`'s deadline `tag`.
    pub fn cancel(&self, owner: &Owner, tag: &str) {
        self.with(|log| log.cancel(owner, tag));
        self.moved();
    }

    /// Cancel the deadline with `token`, whoever it belongs to: the person
    /// cancelling it. Whether there was one.
    pub fn cancel_token(&self, token: &str) -> bool {
        let owner = self.with(|log| {
            log.entries
                .iter()
                .find(|entry| entry.token == token)
                .map(|entry| (entry.owner(), entry.tag.clone()))
        });
        let Some((owner, tag)) = owner else {
            return false;
        };
        self.cancel(&owner, &tag);
        true
    }

    /// Dismiss the snoozed occurrence of the alarm with `token`: it does
    /// not ring again until its next day. Whether one was snoozed.
    ///
    /// # Errors
    /// The occurrence could not be dismissed.
    pub fn dismiss_snooze(&self, token: &str, now_ms: i64) -> Result<bool, String> {
        let target = self.with(|log| {
            log.entries
                .iter()
                .find(|entry| entry.token == token)
                .and_then(|entry| {
                    entry.snoozed_until()?;
                    Some((
                        entry.app.clone(),
                        Occurrence {
                            workspace: entry.workspace.clone(),
                            token: entry.token.clone(),
                            occurrence_ms: entry.active.as_ref()?.at_ms,
                            calendar_alarm: true,
                            uri: entry.activation.clone(),
                        },
                    ))
                })
        });
        let Some((app, target)) = target else {
            return Ok(false);
        };
        self.act(&app, &target, AlarmAction::Dismiss, now_ms)?;
        Ok(true)
    }

    /// `owner`'s deadlines still to fall due: `(tag, at_ms)`.
    #[must_use]
    pub fn pending(&self, owner: &Owner) -> Vec<(String, i64)> {
        self.with(|log| log.pending(owner))
    }

    /// `owner`'s alarms and what the daemon delivered of them.
    #[must_use]
    pub fn alarm_states(&self, owner: &Owner) -> Vec<AlarmStateWire> {
        self.with(|log| log.alarm_states(owner))
    }

    /// Every deadline, as published.
    #[must_use]
    pub fn deadlines(&self) -> Vec<Deadline> {
        self.with(|log| log.deadlines())
    }

    /// When the next deadline falls due, if any.
    #[must_use]
    pub fn next_at_ms(&self) -> Option<i64> {
        self.with(|log| log.next_at_ms())
    }

    /// Snooze or dismiss occurrence `target` of `app_id`'s alarm, at
    /// `now_ms`.
    ///
    /// # Errors
    /// It is no alarm of that app, or the occurrence is stale.
    pub fn act(
        &self,
        app_id: &str,
        target: &Occurrence,
        action: AlarmAction,
        now_ms: i64,
    ) -> Result<(), String> {
        self.with(|log| log.act(app_id, target, action, now_ms))?;
        self.moved();
        Ok(())
    }

    /// Claim what is due at `now` and hand it over as notifications, with
    /// ids from `alloc`.
    pub fn take_due(&self, now: &jiff::Zoned, alloc: impl FnMut() -> u32) -> Vec<Notification> {
        let due = self.with(|log| log.take_due(now, alloc));
        if !due.is_empty() {
            self.moved();
        }
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
        due
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
pub(crate) mod tests;
