//! Durable app/workspace-scoped deadlines owned by the notification service.
//! Apps hand off work here; no app process or network is needed at delivery.
use super::{IncomingNotification, NotificationBridge, PortalCaller, ScheduledAction};
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
struct Occurrence {
    at_ms: i64,
    action: Option<ScheduledAction>,
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
    #[serde(default)]
    token: String,
    #[serde(default)]
    active: Option<Occurrence>,
    #[serde(default)]
    snooze_at_ms: Option<i64>,
}
impl Entry {
    fn owned(&self, caller: &Owner) -> bool {
        self.room == caller.room && self.app == caller.caller.app_id
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
}

/// The snoozed alarms of the last committed ledger, every room's. The
/// compositor's loop reads these to draw pending snoozes; it must not take
/// the store's lock, which a commit holds across its disk write.
pub(super) type SnoozedView = std::sync::Arc<std::sync::Mutex<Vec<super::SnoozedAlarm>>>;

pub(super) struct ScheduleStore {
    entries: Vec<Entry>,
    path: Option<PathBuf>,
    snoozed: SnoozedView,
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
        let store = Self {
            entries,
            path,
            snoozed: SnoozedView::default(),
        };
        store.publish_snoozed();
        Ok(store)
    }

    /// The committed snoozes, readable without this store's lock.
    pub fn snoozed_view(&self) -> SnoozedView {
        std::sync::Arc::clone(&self.snoozed)
    }

    fn publish_snoozed(&self) {
        let snoozed = self.snoozed_alarms();
        if let Ok(mut view) = self.snoozed.lock() {
            *view = snoozed;
        }
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
        self.publish_snoozed();
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
            token: hex::encode(crate::random::nonzero::<16>()),
            active: None,
            snooze_at_ms: None,
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
                    .position(|e| e.next_at_ms().is_none() && (!app_full || e.owned(caller)))
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
            .filter_map(|e| Some((e.tag.clone(), e.next_at_ms()?)))
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

    fn snoozed_alarms(&self) -> Vec<super::SnoozedAlarm> {
        self.entries
            .iter()
            .filter_map(|entry| {
                let active = entry.active.as_ref()?;
                if entry.calendar.is_none()
                    || entry.snooze_at_ms.is_none()
                    || active.action != Some(ScheduledAction::Snooze)
                {
                    return None;
                }
                Some(super::SnoozedAlarm {
                    app_id: entry.app.clone(),
                    title: entry.title.clone(),
                    target: tilo_desktop::notifications::ScheduledNotificationTarget {
                        room: entry.room.clone(),
                        token: entry.token.clone(),
                        occurrence_ms: active.at_ms,
                        calendar_alarm: true,
                        snoozed: true,
                        uri: entry.activation.clone(),
                    },
                })
            })
            .collect()
    }

    pub fn next_at_ms(&self) -> Option<i64> {
        self.entries.iter().filter_map(Entry::next_at_ms).min()
    }

    pub fn act(
        &mut self,
        request: &super::ScheduledActionRequest,
        now_ms: i64,
    ) -> Result<(), String> {
        let mut entries = self.entries.clone();
        let target = &request.target;
        if !target.valid() || !target.calendar_alarm {
            return Err("this notification is not a calendar alarm".into());
        }
        let app_id = request.app_id.as_str();
        let entry = entries.iter_mut().find(|entry| {
            entry.app == app_id
                && entry.room == target.room
                && entry.token == target.token
                && entry.activation == target.uri
        });
        let Some(entry) = entry else {
            return if request.action == ScheduledAction::Dismiss {
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
            // Dismissal of an old card retires only that card. It never
            // changes a newer ring, or resurrects a canceled schedule.
            return if request.action == ScheduledAction::Dismiss {
                Ok(())
            } else {
                Err("scheduled occurrence is stale".into())
            };
        };
        if active.action == Some(request.action) {
            return Ok(());
        }
        if active.action == Some(ScheduledAction::Dismiss) {
            return Err("scheduled occurrence was already dismissed".into());
        }
        entry.snooze_at_ms = match request.action {
            ScheduledAction::Snooze => Some(
                now_ms
                    .checked_add(300_000)
                    .filter(|at| DateTime::<Utc>::from_timestamp_millis(*at).is_some())
                    .ok_or("invalid snooze deadline")?,
            ),
            ScheduledAction::Dismiss => None,
        };
        active.action = Some(request.action);
        self.commit(entries)
    }

    pub fn service_step<Tz: TimeZone>(
        &mut self,
        bridge: &NotificationBridge,
        now: &DateTime<Tz>,
        workspaces: Option<&crate::portal::GrantStoreHandle>,
    ) -> Result<(), String> {
        for request in bridge.drain_scheduled_actions() {
            let result = if workspaces.is_some_and(|workspaces| {
                workspaces
                    .caller_in_room(&request.target.room, &request.app_id)
                    .is_none()
            }) {
                Err("alarm's original application workspace is unavailable".into())
            } else {
                self.act(&request, now.timestamp_millis())
            };
            bridge.finish_scheduled_action(request.id, result);
        }
        self.deliver_due(bridge, now, workspaces)
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
            if let Some(calendar) = &entry.calendar
                && entry
                    .next_at_ms
                    .is_some_and(|at| at > now.timestamp_millis())
            {
                // Calendar deadlines follow timezone changes; snooze is an
                // absolute instant and never shifts with the local timezone.
                entry.next_at_ms = calendar
                    .alarm()
                    .next_in(entry.last_rung, now)
                    .map(|date| date.timestamp_millis());
            }
            let calendar_due = entry.next_at_ms.filter(|at| *at <= now.timestamp_millis());
            let snooze_due = entry
                .snooze_at_ms
                .filter(|at| *at <= now.timestamp_millis());
            let Some(at_ms) = calendar_due.into_iter().chain(snooze_due).max() else {
                continue;
            };
            let caller = match workspaces {
                Some(workspaces) => {
                    let Some(caller) = workspaces.caller_in_room(&entry.room, &entry.app) else {
                        continue;
                    };
                    caller
                }
                None => PortalCaller::new(entry.workspace, entry.app.clone()),
            };
            if entry.token.is_empty() {
                entry.token = hex::encode(crate::random::nonzero::<16>());
            }
            let target = tilo_desktop::notifications::ScheduledNotificationTarget {
                room: entry.room.clone(),
                token: entry.token.clone(),
                occurrence_ms: at_ms,
                calendar_alarm: entry.calendar.is_some(),
                snoozed: false,
                uri: entry.activation.clone(),
            };
            due.push((
                IncomingNotification {
                    dbus_id: bridge.alloc_id(),
                    app_name: entry.app.clone(),
                    app_icon: None,
                    desktop_entry: None,
                    image_data: None,
                    summary: entry.title.clone(),
                    body: entry.body.clone(),
                    actions: Vec::new(),
                    expire_timeout: 0,
                    urgency: 1,
                    caller: Some(caller),
                    badge_count: None,
                    contact_endpoint: None,
                    sender_name: None,
                    sound: Some(settings_store::system_sounds::SystemSoundEvent::Notification),
                    incoming_call: false,
                    machine_wide: false,
                },
                target,
            ));
            entry.active = entry.calendar.as_ref().map(|_| Occurrence {
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
            // A new calendar occurrence supersedes a pending snooze of the
            // preceding one. When both are overdue, deliver only one catch-up.
            if snooze_due.is_some() || calendar_due.is_some() {
                entry.snooze_at_ms = None;
            }
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
        for (notice, target) in due {
            bridge.push_scheduled(notice, target);
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
                if let Err(error) = store.service_step(&bridge, &Local::now(), workspaces.as_ref())
                {
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
                .saturating_sub(tilo_clock::unix_now_ms_i64())
                .clamp(1000, 30_000);
            let delay = std::time::Duration::from_millis(u64::try_from(millis).unwrap_or(1000));
            tokio::select! { () = tokio::time::sleep(delay) => {}, () = changed.wait() => {} }
        } else {
            changed.wait().await;
        }
    }
}

#[cfg(test)]
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
    fn closed_alarm_target_survives_real_history_reload_and_routes_once() {
        let temp = tempfile::tempdir().expect("private fixture");
        let due = at("2026-04-14T12:00:00+00:00");
        let caller = owner(1, "so.tilo.clocks");
        let mut store =
            ScheduleStore::open(Some(temp.path().join("schedules.json"))).expect("store");
        let mut args = alarm_args("alarm", due, 0);
        args.3 = "tilo-clocks:alarm/exact-alarm-id".into();
        store.schedule(&caller, args).expect("handoff");
        let bridge = NotificationBridge::new();
        store
            .deliver_due(&bridge, &due, None)
            .expect("delivery with no app process");
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_dnd(true);
        chrome.borrow_mut().set_notification_bridge(bridge);
        chrome.borrow_mut().pump_dbus_notifications();
        let history = crate::shell::notification_history::NotificationHistoryStore::at(
            temp.path().join("history.json"),
        );
        history
            .save(chrome.borrow().notification_history())
            .expect("persist real native history");
        drop(chrome);
        drop(store);
        let restored = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        restored.borrow_mut().set_dnd(true);
        let notification = history.load().pop().expect("restored alarm");
        assert!(
            notification.sender_id.is_none(),
            "process ids cannot survive restart"
        );
        let id = restored.borrow_mut().push_notification_full(notification);
        assert_eq!(
            restored.borrow_mut().activate_history_notification(id),
            tilo_desktop::notifications::HistoryActivation::Invoked
        );
        let requests = restored.borrow_mut().take_notification_launch_requests();
        assert_eq!(requests.len(), 1);
        let request = requests.first().expect("one verified launch");
        assert_eq!(request.app_id, "so.tilo.clocks");
        assert_eq!(request.target.room, caller.room);
        assert_eq!(request.target.uri, "tilo-clocks:alarm/exact-alarm-id");
        assert_eq!(request.target.occurrence_ms, due.timestamp_millis());
        assert_eq!(
            restored.borrow_mut().activate_history_notification(id),
            tilo_desktop::notifications::HistoryActivation::Missing
        );
        assert!(
            restored
                .borrow_mut()
                .take_notification_launch_requests()
                .is_empty()
        );
    }

    #[test]
    fn live_alarm_activation_routes_the_same_exact_target_once() {
        let caller = owner(1, "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut store = ScheduleStore::open(None).expect("store");
        let mut args = alarm_args("alarm", due, 0);
        args.3 = "tilo-clocks:alarm/live-alarm-id".into();
        store.schedule(&caller, args).expect("handoff");
        let bridge = NotificationBridge::new();
        store.deliver_due(&bridge, &due, None).expect("delivery");
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_notification_bridge(bridge);
        chrome.borrow_mut().pump_dbus_notifications();
        let id = chrome
            .borrow()
            .notifications()
            .first()
            .expect("live alarm")
            .id;
        chrome.borrow_mut().activate_notification(id);
        let requests = chrome.borrow_mut().take_notification_launch_requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].target.uri, "tilo-clocks:alarm/live-alarm-id");
        assert_eq!(requests[0].target.room, caller.room);
        chrome.borrow_mut().activate_notification(id);
        assert!(
            chrome
                .borrow_mut()
                .take_notification_launch_requests()
                .is_empty()
        );
    }

    #[test]
    fn wire_notification_cannot_borrow_another_apps_durable_target() {
        let caller = owner(1, "so.tilo.clocks");
        let due = at("2026-04-14T12:00:00+00:00");
        let mut store = ScheduleStore::open(None).expect("store");
        store
            .schedule(&caller, alarm_args("alarm", due, 0))
            .expect("handoff");
        let bridge = NotificationBridge::new();
        store.deliver_due(&bridge, &due, None).expect("delivery");
        let mut notice = bridge.drain_incoming().pop().expect("alarm notice");
        notice.caller = Some(PortalCaller::new(WorkspaceId(1), "another.app"));
        bridge.push_incoming(notice);
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_notification_bridge(bridge);
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(
            chrome
                .borrow()
                .notifications()
                .first()
                .expect("wire notice")
                .scheduled_target
                .is_none()
        );
    }

    struct AlarmFixture {
        caller: Owner,
        store: std::sync::Arc<std::sync::Mutex<ScheduleStore>>,
        bridge: NotificationBridge,
        wake: crate::idle_wake::IdleWake,
        chrome: crate::shell::chrome_state::Chrome,
        id: u64,
        target: tilo_desktop::notifications::ScheduledNotificationTarget,
    }

    fn alarm_fixture(
        due: DateTime<FixedOffset>,
        repeat: u8,
        path: Option<PathBuf>,
    ) -> AlarmFixture {
        let mut tree = crate::shell::state::WorkspaceTree::default();
        let mut workspace = crate::shell::state::Workspace::new(WorkspaceId(1), "Alarm fixture");
        workspace.set_owner("shell-fixture");
        tree.desktop.active = workspace.id;
        tree.desktop.workspaces.push(workspace);
        let workspace = tree.desktop.active_workspace().expect("synthetic room");
        let caller = Owner {
            caller: PortalCaller::new(workspace.id, "so.tilo.clocks"),
            room: workspace.room().to_string(),
        };
        let mut store = ScheduleStore::open(path).expect("private store");
        let mut args = alarm_args("alarm", due, repeat);
        args.3 = "tilo-clocks:alarm/exact-id".into();
        store.schedule(&caller, args).expect("handoff");
        let store = std::sync::Arc::new(std::sync::Mutex::new(store));
        let bridge = NotificationBridge::new();
        let wake = crate::idle_wake::IdleWake::default();
        bridge.attach_schedule_controller(&store, wake.clone());
        store
            .lock()
            .expect("store")
            .service_step(&bridge, &due, None)
            .expect("ring");
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            tree.shared(),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        chrome.borrow_mut().pump_dbus_notifications();
        let notice = chrome
            .borrow()
            .notifications()
            .first()
            .expect("ringing card")
            .clone();
        let target = notice.scheduled_target.expect("verified target");
        assert!(target.calendar_alarm);
        AlarmFixture {
            caller,
            store,
            bridge,
            wake,
            chrome,
            id: notice.id,
            target,
        }
    }

    fn action_request(
        fixture: &AlarmFixture,
        action: ScheduledAction,
    ) -> super::super::ScheduledActionRequest {
        super::super::ScheduledActionRequest {
            id: 1,
            app_id: fixture.caller.caller.app_id.clone(),
            target: fixture.target.clone(),
            action,
        }
    }

    #[test]
    fn semantic_snooze_commits_before_ack_survives_restart_and_rings_once() {
        use crate::shell::search::{
            InvocationDisposition, ResourceKind, SearchContext, SearchProvider,
        };
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 127, Some(path.clone()));
        let mut document = automerge::AutoCommit::new();
        let id = format!("notification.{}.snooze", fixture.id);
        let row = crate::shell::search::providers::ActionsProvider::new()
            .resolve(
                &id,
                SearchContext {
                    query_scope: None,
                    include_programmatic: true,
                    needle: "",
                    wanted: &[ResourceKind::Action],
                    chrome: &fixture.chrome.borrow(),
                    doc: &document,
                    doc_root: &automerge::ROOT,
                },
            )
            .expect("actual semantic snooze action");
        assert_eq!(
            row.contract.as_ref().expect("contract").retry,
            tilo_actions::RetryPolicy::Idempotent
        );
        let disposition = row.on_begin_invoke.as_ref().expect("lifecycle action")(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            &fixture.chrome,
            &crate::shell::search::action_api::ActionArgs::none(),
        )
        .expect("begin");
        let InvocationDisposition::Pending(ticket) = disposition else {
            panic!("must wait for durable commit");
        };
        assert!(ticket().expect("pending").is_none());
        assert_eq!(fixture.chrome.borrow().notifications().len(), 1);
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("worker step");
        assert!(ticket().expect("not pumped yet").is_none());
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(ticket().expect("durable receipt").is_some());
        assert!(fixture.chrome.borrow().notifications().is_empty());
        let stale = action_request(&fixture, ScheduledAction::Snooze);
        let original_calendar = fixture
            .store
            .lock()
            .expect("store")
            .alarm_states(&fixture.caller);
        drop(fixture.store);
        let mut restarted = ScheduleStore::open(Some(path)).expect("restart");
        let bridge = NotificationBridge::new();
        let snoozed = due + chrono::Duration::minutes(5);
        restarted
            .service_step(&bridge, &(snoozed - chrono::Duration::seconds(1)), None)
            .expect("before deadline");
        assert!(bridge.drain_incoming().is_empty());
        restarted
            .service_step(&bridge, &snoozed, None)
            .expect("snoozed ring without an app");
        assert_eq!(bridge.drain_incoming().len(), 1);
        restarted
            .service_step(&bridge, &(snoozed + chrono::Duration::hours(1)), None)
            .expect("no duplicate");
        assert!(bridge.drain_incoming().is_empty());
        assert_eq!(
            restarted.alarm_states(&fixture.caller),
            original_calendar,
            "snooze does not rewrite calendar delivery history"
        );
        assert!(
            restarted.act(&stale, snoozed.timestamp_millis()).is_err(),
            "old card cannot snooze the new ring"
        );
    }

    #[test]
    fn duplicate_snooze_and_dismiss_are_durable_and_cancel_pending_snooze() {
        let due = at("2026-04-14T12:00:00+00:00");
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let fixture = alarm_fixture(due, 0, Some(path.clone()));
        let snooze = action_request(&fixture, ScheduledAction::Snooze);
        let mut store = fixture.store.lock().expect("store");
        store.act(&snooze, due.timestamp_millis()).expect("snooze");
        let deadline = store.next_at_ms();
        store
            .act(
                &snooze,
                (due + chrono::Duration::minutes(2)).timestamp_millis(),
            )
            .expect("idempotent retry");
        assert_eq!(store.next_at_ms(), deadline, "retry cannot extend snooze");
        let dismiss = action_request(&fixture, ScheduledAction::Dismiss);
        store
            .act(&dismiss, due.timestamp_millis())
            .expect("dismiss pending snooze");
        store
            .act(&dismiss, due.timestamp_millis())
            .expect("idempotent dismiss");
        assert!(store.next_at_ms().is_none());
        assert!(store.act(&snooze, due.timestamp_millis()).is_err());
        drop(store);
        let mut restarted = ScheduleStore::open(Some(path)).expect("restart after dismissal");
        assert!(restarted.next_at_ms().is_none());
        assert!(restarted.act(&snooze, due.timestamp_millis()).is_err());
        restarted
            .service_step(&fixture.bridge, &(due + chrono::Duration::hours(1)), None)
            .expect("after canceled snooze");
        assert!(fixture.bridge.drain_incoming().is_empty());
    }

    #[test]
    fn retained_history_snooze_works_after_service_and_compositor_restart() {
        use tilo_desktop::notifications::NotificationHost;
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(path.clone()));
        let tree = fixture.chrome.borrow().tree_handle();
        fixture
            .chrome
            .borrow_mut()
            .activate_notification(fixture.id);
        let history = crate::shell::notification_history::NotificationHistoryStore::at(
            temp.path().join("history.json"),
        );
        history
            .save(fixture.chrome.borrow().notification_history())
            .expect("persist native history");
        drop(fixture);
        let store = std::sync::Arc::new(std::sync::Mutex::new(
            ScheduleStore::open(Some(path)).expect("service restart"),
        ));
        let bridge = NotificationBridge::new();
        bridge.attach_schedule_controller(&store, crate::idle_wake::IdleWake::default());
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            tree,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        chrome.borrow_mut().set_dnd(true);
        let restored = history.load().pop().expect("history reload");
        assert!(restored.sender_id.is_none());
        let id = chrome.borrow_mut().push_notification_full(restored);
        assert_eq!(
            crate::shell::notification::ChromeNotifications::new(&chrome)
                .actions_for(
                    chrome
                        .borrow()
                        .notification_log()
                        .history_entry(id)
                        .expect("retained alarm")
                )
                .len(),
            2
        );
        crate::shell::notification::ChromeNotifications::new(&chrome).invoke_action(id, "snooze");
        assert!(
            chrome
                .borrow()
                .notification_log()
                .history_entry(id)
                .is_some(),
            "card stays until durable commit"
        );
        store
            .lock()
            .expect("store")
            .service_step(&bridge, &due, None)
            .expect("worker commit");
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(
            chrome
                .borrow()
                .notification_log()
                .history_entry(id)
                .is_some_and(|notice| notice
                    .scheduled_target
                    .as_ref()
                    .is_some_and(|target| target.snoozed))
        );
        store
            .lock()
            .expect("store")
            .service_step(&bridge, &(due + chrono::Duration::minutes(5)), None)
            .expect("snoozed ring");
        chrome.borrow_mut().pump_dbus_notifications();
        assert_eq!(chrome.borrow().notification_history().len(), 2);
        assert_eq!(
            chrome
                .borrow()
                .notification_history()
                .last()
                .expect("new snoozed ring")
                .scheduled_target
                .as_ref()
                .expect("same exact resource")
                .occurrence_ms,
            (due + chrono::Duration::minutes(5)).timestamp_millis()
        );
    }

    fn invoke_fixture_alarm_action(
        chrome: &crate::shell::chrome_state::Chrome,
        id: &str,
    ) -> crate::shell::search::PendingInvocation {
        use crate::shell::search::{
            InvocationDisposition, ResourceKind, SearchContext, SearchProvider,
        };
        let mut document = automerge::AutoCommit::new();
        let row = crate::shell::search::providers::ActionsProvider::new()
            .resolve(
                id,
                SearchContext {
                    query_scope: None,
                    include_programmatic: true,
                    needle: "",
                    wanted: &[ResourceKind::Action],
                    chrome: &chrome.borrow(),
                    doc: &document,
                    doc_root: &automerge::ROOT,
                },
            )
            .expect("actual published alarm action");
        let disposition = row.on_begin_invoke.as_ref().expect("lifecycle action")(
            &mut ui::widget::Edits::lend(&mut document),
            &automerge::ROOT,
            chrome,
            &crate::shell::search::action_api::ActionArgs::none(),
        )
        .expect("begin action");
        let InvocationDisposition::Pending(ticket) = disposition else {
            panic!("must wait for durable receipt");
        };
        ticket
    }

    fn reload_fixture_history(
        fixture: &AlarmFixture,
        path: std::path::PathBuf,
    ) -> (crate::shell::chrome_state::Chrome, u64) {
        let history = crate::shell::notification_history::NotificationHistoryStore::at(path);
        history
            .save(fixture.chrome.borrow().notification_history())
            .expect("persist snoozed history");
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            fixture.chrome.borrow().tree_handle(),
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome
            .borrow_mut()
            .set_notification_bridge(fixture.bridge.clone());
        chrome.borrow_mut().set_dnd(true);
        let restored = history.load().pop().expect("snoozed history reload");
        assert!(restored.scheduled_target.as_ref().expect("target").snoozed);
        let id = chrome.borrow_mut().push_notification_full(restored);
        (chrome, id)
    }

    #[test]
    fn snoozed_history_dismiss_cancels_before_ringing_through_the_public_action() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(path.clone()));
        let snooze = invoke_fixture_alarm_action(
            &fixture.chrome,
            &format!("notification.{}.snooze", fixture.id),
        );
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("snooze commit");
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(snooze().expect("snooze receipt").is_some());
        assert!(fixture.chrome.borrow().notifications().is_empty());
        assert!(
            fixture
                .chrome
                .borrow()
                .notification_log()
                .history_entry(fixture.id)
                .expect("reachable snooze")
                .scheduled_target
                .as_ref()
                .expect("state")
                .snoozed
        );
        let (chrome, id) = reload_fixture_history(&fixture, temp.path().join("history.json"));
        let dismiss = invoke_fixture_alarm_action(&chrome, &format!("notification.{id}.dismiss"));
        assert!(dismiss().expect("pending").is_none());
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &(due + chrono::Duration::minutes(1)), None)
            .expect("dismiss commit");
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(dismiss().expect("dismiss receipt").is_some());
        assert!(
            chrome
                .borrow()
                .notification_log()
                .history_entry(id)
                .is_none()
        );
        drop(fixture.store);
        let mut restarted = ScheduleStore::open(Some(path)).expect("restart");
        restarted
            .service_step(&fixture.bridge, &(due + chrono::Duration::minutes(6)), None)
            .expect("after canceled deadline");
        assert!(fixture.bridge.drain_incoming().is_empty());
        assert!(restarted.next_at_ms().is_none());
    }

    #[test]
    fn durable_snooze_cancellation_is_published_after_restart_without_notification_history() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(path.clone()));
        let snooze = invoke_fixture_alarm_action(
            &fixture.chrome,
            &format!("notification.{}.snooze", fixture.id),
        );
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("snooze commit");
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(snooze().expect("receipt").is_some());
        let address = crate::shell::notification::snoozed_alarm_dismiss_id(&fixture.target);
        let tree = fixture.chrome.borrow().tree_handle();
        drop(fixture);
        let store = std::sync::Arc::new(std::sync::Mutex::new(
            ScheduleStore::open(Some(path.clone())).expect("service restart"),
        ));
        let bridge = NotificationBridge::new();
        bridge.attach_schedule_controller(&store, crate::idle_wake::IdleWake::default());
        let chrome = crate::shell::chrome_state::ChromeState::new_with_fixture_handle(
            tree,
            crate::shell::hosted_inputs::HostedTileInputs::default(),
        );
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome.borrow_mut().set_notification_bridge(bridge.clone());
        assert!(chrome.borrow().notification_history().is_empty());
        let dismiss = invoke_fixture_alarm_action(&chrome, &address);
        assert!(dismiss().expect("pending").is_none());
        store
            .lock()
            .expect("store")
            .service_step(&bridge, &(due + chrono::Duration::minutes(1)), None)
            .expect("commit cancellation");
        chrome.borrow_mut().pump_dbus_notifications();
        assert!(dismiss().expect("durable receipt").is_some());
        drop(store);
        let mut restarted = ScheduleStore::open(Some(path)).expect("second service restart");
        restarted
            .service_step(&bridge, &(due + chrono::Duration::minutes(6)), None)
            .expect("past deadline");
        assert!(bridge.drain_incoming().is_empty());
        assert!(restarted.next_at_ms().is_none());
    }

    /// The compositor's loop draws pending snoozes while the scheduler may
    /// hold its store across a commit's disk write; it reads the committed
    /// view and never waits for that lock.
    #[test]
    fn pending_snoozes_are_read_without_the_store_lock() {
        let temp = tempfile::tempdir().expect("private fixture");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(temp.path().join("schedules.json")));
        let snooze = invoke_fixture_alarm_action(
            &fixture.chrome,
            &format!("notification.{}.snooze", fixture.id),
        );
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("snooze commit");
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(snooze().expect("receipt").is_some());

        let held = fixture.store.lock().expect("a commit holds the store");
        let (read_tx, read) = std::sync::mpsc::channel();
        let bridge = fixture.bridge.clone();
        let room = fixture.target.room.clone();
        std::thread::spawn(move || {
            let _ = read_tx.send(bridge.snoozed_alarms(&room));
        });
        let snoozed = read.recv_timeout(tilo_test_env::hang_bound());
        drop(held);
        let snoozed = snoozed.expect("reading pending snoozes waited for the store lock");
        assert_eq!(snoozed.len(), 1, "the committed snooze is pending");
        assert_eq!(snoozed[0].target.token, fixture.target.token);
        assert!(
            fixture.bridge.snoozed_alarms("another-room").is_empty(),
            "snoozes are scoped to their room"
        );
    }

    #[test]
    fn canceling_a_snoozed_alarm_survives_restart_and_stale_dismiss_cannot_rearm_it() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(path.clone()));
        let snooze = action_request(&fixture, ScheduledAction::Snooze);
        let dismiss = action_request(&fixture, ScheduledAction::Dismiss);
        fixture
            .store
            .lock()
            .expect("store")
            .act(&snooze, due.timestamp_millis())
            .expect("snooze");
        fixture
            .store
            .lock()
            .expect("store")
            .cancel(&fixture.caller, "alarm")
            .expect("cancel existing schedule");
        drop(fixture.store);
        let mut restarted = ScheduleStore::open(Some(path)).expect("restart");
        assert!(restarted.act(&snooze, due.timestamp_millis()).is_err());
        restarted
            .act(&dismiss, due.timestamp_millis())
            .expect("stale card dismissal");
        restarted
            .service_step(&fixture.bridge, &(due + chrono::Duration::hours(1)), None)
            .expect("past canceled deadline");
        assert!(restarted.pending(&fixture.caller).is_empty());
        assert!(fixture.bridge.drain_incoming().is_empty());
    }

    #[test]
    fn retained_alarm_controls_refuse_a_changed_workspace_owner() {
        use tilo_desktop::notifications::NotificationHost;
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, None);
        let notice = fixture.chrome.borrow().notifications()[0].clone();
        assert_eq!(
            crate::shell::notification::ChromeNotifications::new(&fixture.chrome)
                .actions_for(&notice)
                .len(),
            2
        );
        fixture
            .chrome
            .borrow()
            .tree_handle()
            .write()
            .desktop
            .workspaces[0]
            .set_owner("another-fixture-owner");
        assert!(
            crate::shell::notification::ChromeNotifications::new(&fixture.chrome)
                .actions_for(&notice)
                .is_empty()
        );
        assert!(
            fixture
                .chrome
                .borrow_mut()
                .request_alarm_action(fixture.id, ScheduledAction::Snooze, true)
                .is_err()
        );
        assert!(fixture.bridge.drain_scheduled_actions().is_empty());
        assert!(fixture.store.lock().expect("store").next_at_ms().is_none());
        assert_eq!(fixture.chrome.borrow().notifications().len(), 1);
    }

    #[test]
    fn failed_snooze_storage_keeps_the_card_and_can_be_retried() {
        let temp = tempfile::tempdir().expect("private fixture");
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 0, Some(temp.path().join("schedules.json")));
        std::fs::write(temp.path().join("blocker"), b"file, not a directory").expect("blocker");
        fixture.store.lock().expect("store").path =
            Some(temp.path().join("blocker/schedules.json"));
        let request = fixture
            .chrome
            .borrow_mut()
            .request_alarm_action(fixture.id, ScheduledAction::Snooze, true)
            .expect("queue");
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("worker step reports action failure");
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(
            fixture
                .chrome
                .borrow_mut()
                .take_alarm_action_result(request)
                .expect("failed receipt")
                .is_err()
        );
        assert_eq!(fixture.chrome.borrow().notifications().len(), 1);
        assert!(fixture.store.lock().expect("store").next_at_ms().is_none());
        fixture.store.lock().expect("store").path = Some(temp.path().join("schedules.json"));
        let retry = fixture
            .chrome
            .borrow_mut()
            .request_alarm_action(fixture.id, ScheduledAction::Snooze, true)
            .expect("retry");
        fixture
            .store
            .lock()
            .expect("store")
            .service_step(&fixture.bridge, &due, None)
            .expect("commit");
        fixture.chrome.borrow_mut().pump_dbus_notifications();
        assert!(
            fixture
                .chrome
                .borrow_mut()
                .take_alarm_action_result(retry)
                .expect("success")
                .is_ok()
        );
        assert!(fixture.chrome.borrow().notifications().is_empty());
    }

    #[test]
    fn foreign_app_room_token_and_stale_occurrence_cannot_change_an_alarm() {
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 127, None);
        let original = fixture
            .store
            .lock()
            .expect("store")
            .alarm_states(&fixture.caller);
        for changed in 0..4 {
            let mut request = action_request(&fixture, ScheduledAction::Snooze);
            match changed {
                0 => request.app_id = "another.app".into(),
                1 => request.target.room = "another-room".into(),
                2 => request.target.token = "0123456789abcdef0123456789abcdef".into(),
                _ => request.target.occurrence_ms -= 1,
            }
            assert!(
                fixture
                    .store
                    .lock()
                    .expect("store")
                    .act(&request, due.timestamp_millis())
                    .is_err()
            );
        }
        assert_eq!(
            fixture
                .store
                .lock()
                .expect("store")
                .alarm_states(&fixture.caller),
            original
        );
    }

    #[test]
    fn snooze_stays_in_utc_across_timezone_and_wall_clock_changes() {
        use tilo_desktop::notifications::NotificationHost as _;
        let due = at("2026-04-14T12:00:00+00:00");
        let fixture = alarm_fixture(due, 127, None);
        crate::shell::notification::ChromeNotifications::new(&fixture.chrome)
            .invoke_action(fixture.id, "snooze");
        let mut store = fixture.store.lock().expect("store");
        store
            .service_step(&fixture.bridge, &due, None)
            .expect("host operation");
        let expected = (due + chrono::Duration::minutes(5)).timestamp_millis();
        assert_eq!(store.next_at_ms(), Some(expected));
        store
            .service_step(&fixture.bridge, &at("2026-04-14T21:03:00+09:00"), None)
            .expect("timezone change");
        assert_eq!(store.next_at_ms(), Some(expected));
        assert!(fixture.bridge.drain_incoming().is_empty());
        store
            .service_step(&fixture.bridge, &at("2026-04-14T20:03:00+09:00"), None)
            .expect("clock moved backward");
        assert_eq!(store.next_at_ms(), Some(expected));
        assert!(fixture.bridge.drain_incoming().is_empty());
        let forward = due + chrono::Duration::minutes(7);
        store
            .service_step(&fixture.bridge, &forward, None)
            .expect("clock forward catches up once");
        assert_eq!(fixture.bridge.drain_incoming().len(), 1);
        store
            .service_step(&fixture.bridge, &forward, None)
            .expect("same clock");
        drop(store);
        assert!(fixture.bridge.drain_incoming().is_empty());
    }

    #[tokio::test]
    async fn worker_returns_a_durable_snooze_receipt_without_a_future_calendar_deadline() {
        let temp = tempfile::tempdir().expect("private fixture");
        let path = temp.path().join("schedules.json");
        let fixture = alarm_fixture(at("2026-04-14T12:00:00+00:00"), 0, Some(path.clone()));
        assert!(fixture.store.lock().expect("store").next_at_ms().is_none());
        let worker = tokio::spawn(drive(
            std::sync::Arc::downgrade(&fixture.store),
            fixture.bridge.clone(),
            fixture.wake.clone(),
            None,
        ));
        tokio::task::yield_now().await;
        let request = fixture
            .bridge
            .request_scheduled_action(
                fixture.caller.caller.app_id.clone(),
                fixture.target.clone(),
                ScheduledAction::Snooze,
            )
            .expect("host request");
        // The ledger holds no deadline, so only the request can wake the
        // parked worker.
        let result = tilo_test_env::within("the worker's snooze receipt", async {
            loop {
                if let Some(result) = fixture.bridge.take_scheduled_action_result(request) {
                    break result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await;
        worker.abort();
        let _ = worker.await;
        result.expect("durable commit");
        let persisted = ScheduleStore::open(Some(path)).expect("actual stored ledger");
        assert_eq!(
            persisted.next_at_ms(),
            fixture.store.lock().expect("store").next_at_ms()
        );
        assert!(
            persisted.next_at_ms().is_some(),
            "ack cannot precede durable snooze"
        );
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
        let room = workspace.room().to_string();
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
