//! Durable app/workspace-scoped deadlines owned by the notification service.
//! Apps hand off work here; no app process or network is needed at delivery.
use super::{IncomingNotification, NotificationBridge, PortalCaller};
use crate::shell::state::WorkspaceId;
use chrono::{DateTime, Local, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const MAX_ENTRIES: usize = 1024;
const MAX_PER_APP: usize = 64;
const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;

pub(super) type AlarmStateWire = (String, (u8, u8, u8, i64), Vec<i64>, Vec<i64>, String);

pub(super) type ScheduleArgs = (
    String,
    String,
    String,
    String,
    i64,
    Vec<(u8, u8, u8, i64, String)>,
);

/// The exact server-owned room key is separate from the local display id:
/// launcher identities carry room UUIDs, never caller-supplied workspace ids.
pub(super) struct Owner {
    pub caller: PortalCaller,
    pub room: String,
}
impl Owner {
    pub fn mapped(caller: PortalCaller) -> Self {
        Self {
            room: format!("local:{}", caller.workspace_id.0),
            caller,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Calendar {
    hour: u8,
    minute: u8,
    weekdays: u8,
    enabled_at_ms: i64,
}
impl Calendar {
    fn alarm(&self) -> ui::notify::CalendarAlarm {
        ui::notify::CalendarAlarm {
            hour: self.hour,
            minute: self.minute,
            weekdays: self.weekdays,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    workspace: WorkspaceId,
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
}
impl Entry {
    fn owned(&self, caller: &Owner) -> bool {
        self.room == caller.room && self.app == caller.caller.app_id
    }
    fn same_request(&self, other: &Self) -> bool {
        self.title == other.title
            && self.body == other.body
            && self.activation == other.activation
            && self.calendar == other.calendar
            && (self.calendar.is_some() || self.original_at_ms == other.original_at_ms)
    }
}

pub(super) struct ScheduleStore {
    entries: Vec<Entry>,
    path: Option<PathBuf>,
}
impl ScheduleStore {
    pub fn open(path: Option<PathBuf>) -> Result<Self, String> {
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

    fn commit(&mut self, entries: Vec<Entry>) -> Result<(), String> {
        if let Some(path) = &self.path {
            let bytes = serde_json::to_vec(&entries).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_STORE_BYTES {
                return Err("notification schedule store exceeds its limit".into());
            }
            tilo_files::native::atomic::write_private(path, &bytes).map_err(|e| e.to_string())?;
        }
        self.entries = entries;
        Ok(())
    }

    pub fn schedule(&mut self, caller: &Owner, args: ScheduleArgs) -> Result<(), String> {
        let (tag, title, body, activation, at_ms, calendar) = args;
        let (calendar, last_rung) = match calendar.as_slice() {
            [] => (None, None),
            [(hour, minute, weekdays, enabled_at_ms, last_rung)] => {
                let c = Calendar {
                    hour: *hour,
                    minute: *minute,
                    weekdays: *weekdays,
                    enabled_at_ms: *enabled_at_ms,
                };
                if !c.alarm().valid() {
                    return Err("invalid alarm time or weekdays".into());
                }
                let last_rung = if last_rung.is_empty() {
                    None
                } else {
                    Some(
                        NaiveDate::parse_from_str(last_rung, "%Y-%m-%d")
                            .map_err(|_| "invalid delivered alarm date".to_owned())?,
                    )
                };
                (Some(c), last_rung)
            }
            _ => return Err("expected at most one calendar alarm".into()),
        };
        if tag.is_empty()
            || tag.len() > 128
            || tag.chars().any(char::is_control)
            || title.trim().is_empty()
            || title.len() > 512
            || body.len() > 4096
            || activation.len() > 2048
            || activation.chars().any(char::is_control)
            || DateTime::<Utc>::from_timestamp_millis(at_ms).is_none()
        {
            return Err("invalid scheduled notification".into());
        }
        let entry = Entry {
            workspace: caller.caller.workspace_id,
            room: caller.room.clone(),
            app: caller.caller.app_id.clone(),
            tag,
            title,
            body,
            activation,
            original_at_ms: at_ms,
            next_at_ms: Some(at_ms),
            calendar,
            last_rung,
            last_at_ms: None,
        };
        let mut entries = self.entries.clone();
        if let Some(old) = entries
            .iter_mut()
            .find(|old| old.owned(caller) && old.tag == entry.tag)
        {
            // Reopening an app must not rearm an occurrence the service already delivered.
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
            let app_full = entries.iter().filter(|e| e.owned(caller)).count() >= MAX_PER_APP;
            if entries.len() >= MAX_ENTRIES || app_full {
                if let Some(index) = entries
                    .iter()
                    .position(|e| e.next_at_ms.is_none() && (!app_full || e.owned(caller)))
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

    pub fn cancel(&mut self, caller: &Owner, tag: &str) -> Result<(), String> {
        let mut entries = self.entries.clone();
        entries.retain(|entry| !(entry.owned(caller) && entry.tag == tag));
        self.commit(entries)
    }

    pub fn pending(&self, caller: &Owner) -> Vec<(String, i64)> {
        self.entries
            .iter()
            .filter(|e| e.owned(caller))
            .filter_map(|e| Some((e.tag.clone(), e.next_at_ms?)))
            .collect()
    }

    pub fn alarm_states(&self, caller: &Owner) -> Vec<AlarmStateWire> {
        self.entries
            .iter()
            .filter(|entry| entry.owned(caller))
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

    pub fn next_at_ms(&self) -> Option<i64> {
        self.entries.iter().filter_map(|e| e.next_at_ms).min()
    }

    pub fn deliver_due<Tz: TimeZone>(
        &mut self,
        bridge: &NotificationBridge,
        now: &DateTime<Tz>,
        workspaces: Option<&crate::portal::GrantStoreHandle>,
    ) -> Result<(), String> {
        let mut entries = self.entries.clone();
        let mut due = Vec::new();
        for entry in &mut entries {
            let Some(mut at_ms) = entry.next_at_ms else {
                continue;
            };
            if let Some(calendar) = &entry.calendar
                && at_ms > now.timestamp_millis()
            {
                // Upcoming local alarms follow timezone changes; overdue
                // deadlines remain due after suspend or service restart.
                let next = calendar
                    .alarm()
                    .next_in(entry.last_rung, now)
                    .map(|d| d.timestamp_millis());
                entry.next_at_ms = next;
                let Some(next) = next else {
                    continue;
                };
                at_ms = next;
            }
            if at_ms > now.timestamp_millis() {
                continue;
            }
            let caller = match workspaces {
                Some(workspaces) => {
                    let Some(caller) = workspaces.caller_in_room(&entry.room, &entry.app) else {
                        continue;
                    };
                    caller
                }
                None => PortalCaller::new(entry.workspace, entry.app.clone()),
            };
            due.push(IncomingNotification {
                dbus_id: bridge.alloc_id(),
                app_name: entry.app.clone(),
                summary: entry.title.clone(),
                body: entry.body.clone(),
                actions: Vec::new(),
                expire_timeout: 0,
                urgency: 1,
                caller: Some(caller),
                badge_count: None,
                contact_endpoint: None,
                sound: Some(settings_store::system_sounds::SystemSoundEvent::Notification),
            });
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
        // Claims become durable before handoff, as with system automation.
        // A storage failure neither consumes nor enqueues a deadline.
        if !due.is_empty()
            || self
                .entries
                .iter()
                .zip(&entries)
                .any(|(a, b)| a.next_at_ms != b.next_at_ms)
        {
            self.commit(entries)?;
        }
        for notice in due {
            bridge.push_incoming(notice);
        }
        Ok(())
    }
}

pub(super) async fn drive(
    schedules: std::sync::Weak<std::sync::Mutex<ScheduleStore>>,
    bridge: NotificationBridge,
    changed: crate::idle_wake::IdleWake,
    workspaces: Option<crate::portal::GrantStoreHandle>,
) {
    while let Some(store) = schedules.upgrade() {
        let next = match store.lock() {
            Ok(mut store) => {
                if let Err(error) = store.deliver_due(&bridge, &Local::now(), workspaces.as_ref()) {
                    tracing::warn!(%error, "scheduled notification delivery failed");
                }
                store.next_at_ms()
            }
            Err(_) => break,
        };
        drop(store);
        if let Some(next) = next {
            // Recheck the wall clock to observe suspend and timezone/clock
            // corrections. Empty ledgers park without polling.
            let millis = next
                .saturating_sub(i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX))
                .clamp(1000, 30_000);
            let delay = std::time::Duration::from_millis(u64::try_from(millis).unwrap_or(1000));
            tokio::select! { () = tokio::time::sleep(delay) => {}, () = changed.wait() => {} }
        } else {
            changed.wait().await;
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "fixture construction and persisted-state round trips must fail the test"
)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(value: &str) -> DateTime<FixedOffset> {
        DateTime::parse_from_rfc3339(value).expect("valid test instant")
    }
    fn owner(workspace: u64, app: &str) -> Owner {
        Owner::mapped(PortalCaller::new(WorkspaceId(workspace), app))
    }
    fn alarm_args(tag: &str, due: DateTime<FixedOffset>, repeat: u8) -> ScheduleArgs {
        (
            tag.into(),
            "Alarm".into(),
            "Wake up".into(),
            "tilo-clocks:alarms".into(),
            due.timestamp_millis(),
            vec![(12, 0, repeat, 1, String::new())],
        )
    }

    #[test]
    fn initial_handoff_preserves_the_apps_already_delivered_local_date() {
        let now = at("2026-04-14T11:00:00+00:00");
        let tomorrow = ui::notify::CalendarAlarm {
            hour: 12,
            minute: 0,
            weekdays: 0,
        }
        .next_in(Some(now.date_naive()), &now)
        .expect("next unconsumed occurrence");
        let caller = owner(1, "so.tilo.clocks");
        let mut store = ScheduleStore::open(None).expect("memory store");
        let mut args = alarm_args("reopened", tomorrow, 0);
        args.5.first_mut().expect("calendar").4 = now.date_naive().to_string();
        store.schedule(&caller, args).expect("handoff");
        store
            .deliver_due(&NotificationBridge::new(), &now, None)
            .expect("service tick");
        assert_eq!(
            store.pending(&caller),
            vec![("reopened".into(), tomorrow.timestamp_millis())],
            "host recomputation must not move an already-consumed date back into the schedule"
        );
    }

    #[test]
    fn deadlines_survive_close_restart_and_do_not_repeat_after_dismissal() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let caller = owner(1, "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let args = alarm_args("alarm", due, 0);
        let mut store = ScheduleStore::open(Some(path.clone())).expect("new store");
        store.schedule(&caller, args.clone()).expect("handoff");
        drop(store); // The app and service can both exit before the deadline.
        let mut restarted = ScheduleStore::open(Some(path.clone())).expect("restart");
        let bridge = NotificationBridge::new();
        restarted.deliver_due(&bridge, &due, None).expect("deliver");
        let notices = bridge.drain_incoming();
        assert_eq!(notices.len(), 1);
        assert_eq!(
            notices.first().and_then(|n| n.caller.as_ref()),
            Some(&caller.caller)
        );
        // Closing a toast never rearms its deadline, including after restart.
        restarted
            .schedule(&caller, args)
            .expect("idempotent app reopen");
        drop(restarted);
        let mut restarted = ScheduleStore::open(Some(path)).expect("second restart");
        restarted
            .deliver_due(&bridge, &due, None)
            .expect("second tick");
        assert!(bridge.drain_incoming().is_empty());
        assert!(restarted.pending(&caller).is_empty());
    }

    #[test]
    fn cancellation_and_tags_are_scoped_to_the_verified_workspace_and_app() {
        let mut store = ScheduleStore::open(None).expect("memory store");
        let bridge = NotificationBridge::new();
        let due = at("2026-04-14T12:00:00+00:00");
        let a = owner(1, "so.tilo.clocks");
        let b = owner(2, "so.tilo.clocks");
        let c = owner(1, "so.tilo.other");
        for caller in [&a, &b, &c] {
            store
                .schedule(caller, alarm_args("same-tag", due, 0))
                .expect("schedule");
        }
        store.cancel(&a, "same-tag").expect("cancel own alarm");
        assert!(store.pending(&a).is_empty());
        assert_eq!(store.pending(&b).len(), 1);
        assert_eq!(store.pending(&c).len(), 1);
        store
            .deliver_due(&bridge, &due, None)
            .expect("deliver others");
        let notices = bridge.drain_incoming();
        assert_eq!(notices.len(), 2);
        assert!(notices.iter().all(|n| n.caller.as_ref() != Some(&a.caller)));
    }

    #[test]
    fn repeating_alarm_catches_up_once_and_follows_local_time_without_an_app() {
        let mut store = ScheduleStore::open(None).expect("memory store");
        let bridge = NotificationBridge::new();
        let caller = owner(1, "so.tilo.clocks");
        store
            .schedule(
                &caller,
                alarm_args("daily", at("2026-04-14T12:00:00+00:00"), 127),
            )
            .expect("schedule");
        let late = at("2026-04-17T15:00:00+00:00");
        store
            .deliver_due(&bridge, &late, None)
            .expect("catch up after offline/suspend");
        assert_eq!(
            bridge.drain_incoming().len(),
            1,
            "missed days are not replayed"
        );
        store
            .deliver_due(&bridge, &late, None)
            .expect("repeat tick");
        assert!(bridge.drain_incoming().is_empty());
        let changed_zone = at("2026-04-18T10:00:00+02:00");
        store
            .deliver_due(&bridge, &changed_zone, None)
            .expect("timezone change");
        assert_eq!(
            store.pending(&caller),
            vec![(
                "daily".into(),
                at("2026-04-18T12:00:00+02:00").timestamp_millis()
            )]
        );
        let local_noon = at("2026-04-18T12:00:00+02:00");
        store
            .deliver_due(&bridge, &local_noon, None)
            .expect("next local noon");
        assert_eq!(bridge.drain_incoming().len(), 1);
        store
            .deliver_due(&bridge, &at("2026-04-18T11:00:00+02:00"), None)
            .expect("clock moves backwards");
        store
            .deliver_due(&bridge, &local_noon, None)
            .expect("same local date again");
        assert!(
            bridge.drain_incoming().is_empty(),
            "clock rollback must not duplicate an alarm"
        );
    }

    #[test]
    fn delivery_uses_the_current_room_mapping_and_parks_deleted_rooms() {
        let workspace = crate::shell::state::Workspace::new(WorkspaceId(7), "Alarm room");
        let room = workspace.room.to_string();
        let (workspaces, _) = crate::portal::GrantStoreHandle::test_with_workspace(workspace);
        let owner = Owner {
            caller: PortalCaller::new(WorkspaceId(99), "so.tilo.clocks"),
            room,
        };
        let due = at("2026-04-14T12:00:00+00:00");
        let mut store = ScheduleStore::open(None).expect("memory store");
        store
            .schedule(&owner, alarm_args("alarm", due, 0))
            .expect("schedule");
        let bridge = NotificationBridge::new();
        store
            .deliver_due(&bridge, &due, Some(&workspaces))
            .expect("mapped delivery");
        assert_eq!(
            bridge
                .drain_incoming()
                .first()
                .and_then(|n| n.caller.as_ref())
                .map(|c| c.workspace_id),
            Some(WorkspaceId(7))
        );
        store
            .schedule(&owner, alarm_args("second", due, 0))
            .expect("another deadline");
        workspaces.tree_handle().write().desktop.workspaces.clear();
        store
            .deliver_due(&bridge, &due, Some(&workspaces))
            .expect("deleted room");
        assert!(
            bridge.drain_incoming().is_empty(),
            "a missing room never becomes a default workspace"
        );
        assert_eq!(
            store.pending(&owner).len(),
            1,
            "user alarm data is retained"
        );
    }

    #[test]
    fn persistence_failure_preserves_deadlines_and_does_not_deliver() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let caller = owner(1, "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut store = ScheduleStore::open(Some(path.clone())).expect("store");
        store
            .schedule(&caller, alarm_args("alarm", due, 0))
            .expect("schedule");
        std::fs::remove_file(&path).expect("remove fixture file");
        std::fs::create_dir(&path).expect("block replacement with a directory");
        let bridge = NotificationBridge::new();
        assert!(store.deliver_due(&bridge, &due, None).is_err());
        assert!(bridge.drain_incoming().is_empty());
        assert_eq!(store.pending(&caller).len(), 1);
        std::fs::remove_dir(&path).expect("restore writable fixture");
        store.deliver_due(&bridge, &due, None).expect("retry");
        assert_eq!(bridge.drain_incoming().len(), 1);
    }
}
