//! The native desktop's notification sources and its adapter onto the shared
//! banner stack and notification centre.
//!
//! Freedesktop `Notify` calls and notifications mirrored from another of the
//! person's devices become the shared [`Notification`] model here; the chrome
//! holds them in its [`NotificationLog`] and answers the shared widgets
//! through [`ChromeNotifications`].

use std::cell::Ref;
use std::time::Duration;

use tilo_desktop::notifications::{
    DEFAULT_DISMISS_AFTER, HistoryActivation, Notification, NotificationHost, NotificationLog,
    NotificationPerson, NotificationSeverity, PERSISTENT_DISMISS, RemoteNotificationRoute,
};

use crate::notifications::{
    ContinuityCommandEgress, ContinuityNotificationCommand, IncomingNotification,
    RemoteContinuityNotification,
};
use crate::shell::chrome_state::{Chrome, ChromeState};
use crate::shell::i18n::t_args;
use crate::shell::room_activity::RoomActivityPane;
use crate::shell::state::WorkspaceAuthority as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NotificationLaunch {
    pub app_id: String,
    pub target: tilo_desktop::notifications::ScheduledNotificationTarget,
}

pub(crate) fn scheduled_launch(notification: &Notification) -> Option<NotificationLaunch> {
    if notification.remote_route.is_some() {
        return None;
    }
    let target = notification
        .scheduled_target
        .clone()
        .filter(tilo_desktop::notifications::ScheduledNotificationTarget::valid)?;
    Some(NotificationLaunch {
        app_id: notification.app_id.clone()?,
        target,
    })
}

pub(crate) fn launch_scheduled_notification(
    request: NotificationLaunch,
    tree: &crate::shell::state::WorkspaceTreeHandle,
) -> Result<(), String> {
    let workspace = tree
        .read()
        .desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == request.target.room)
        .cloned()
        .ok_or("notification's original room is unavailable")?;
    crate::shell::launcher::queue_package_target_in_workspace(
        request.app_id,
        &workspace,
        request.target.uri,
    )
    .map_err(|error| error.to_string())
}

pub(crate) fn alarm_action_address(
    id: &str,
) -> Option<(u64, crate::notifications::ScheduledAction)> {
    let (notice, action) = id.strip_prefix("notification.")?.split_once('.')?;
    let notice = notice.parse::<u64>().ok().filter(|id| *id != 0)?;
    let action = match action {
        "snooze" => crate::notifications::ScheduledAction::Snooze,
        "dismiss" => crate::notifications::ScheduledAction::Dismiss,
        _ => return None,
    };
    Some((notice, action))
}

pub(crate) fn snoozed_alarm_dismiss_id(
    target: &tilo_desktop::notifications::ScheduledNotificationTarget,
) -> String {
    format!(
        "snoozed-alarm.{}.{}.dismiss",
        target.token, target.occurrence_ms
    )
}

pub(crate) fn is_snoozed_alarm_action(id: &str) -> bool {
    id.strip_prefix("snoozed-alarm.")
        .and_then(|id| id.strip_suffix(".dismiss"))
        .and_then(|id| id.split_once('.'))
        .is_some_and(|(token, at)| {
            token.len() == 32
                && token.bytes().all(|byte| byte.is_ascii_hexdigit())
                && at.parse::<i64>().is_ok()
        })
}

fn pending_snoozes_for_active_room(state: &ChromeState) -> Vec<crate::notifications::SnoozedAlarm> {
    // Do not hold a tree guard while reading the scheduler: delivery reads
    // its room mapping while holding the scheduler's own lock.
    let room = state
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room.to_string());
    room.map_or_else(Vec::new, |room| state.snoozed_alarms(&room))
}

fn snoozed_alarm_rows(
    state: &ChromeState,
    workspace: &crate::shell::state::Workspace,
    sources: Vec<crate::notifications::SnoozedAlarm>,
) -> Vec<crate::shell::search::SearchResult> {
    use crate::shell::search::action_api::ActionError;
    use crate::shell::search::{ResourceKind, SearchResult};
    let actor = workspace.actor_for_person(&state.local_user_for_room(&workspace.room));
    sources
        .into_iter()
        .filter_map(|source| {
            let id = snoozed_alarm_dismiss_id(&source.target);
            if source.target.room != workspace.room.to_string()
                || !crate::shell::search::providers::actions::shell_action_authorized_for(
                    workspace, &actor, &id,
                )
            {
                return None;
            }
            let already_reachable = state
                .notification_log()
                .active()
                .iter()
                .chain(state.notification_log().history())
                .any(|notice| {
                    scheduled_launch(notice).is_some_and(|request| {
                        request.target.calendar_alarm
                            && request.app_id == source.app_id
                            && request.target.room == source.target.room
                            && request.target.token == source.target.token
                            && request.target.uri == source.target.uri
                            && request.target.occurrence_ms == source.target.occurrence_ms
                    })
                });
            if already_reachable {
                return None;
            }
            let expected_room = source.target.room.clone();
            let address = id.clone();
            let label = tilo_desktop::notifications::alarm_action_labels()
                .into_iter()
                .find(|(key, _)| key == "dismiss")?
                .1;
            Some(SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: "actions".into(),
                id: id.clone(),
                title: tilo_desktop::notifications::alarm_action_title(&label, &source.title),
                subtitle: Some(tilo_desktop::notifications::alarm_snoozed_label()),
                badge: None,
                score: 0,
                params: Vec::new(),
                contract: Some(
                    crate::shell::search::providers::actions::shell_action_contract(&id),
                ),
                on_open: std::rc::Rc::new(|_, _, _, _| {
                    Err(ActionError::Refused(
                        "alarm actions require lifecycle dispatch".into(),
                    ))
                }),
                on_invoke: None,
                on_begin_invoke: Some(std::rc::Rc::new(move |_, _, chrome, _| {
                    begin_alarm_action(chrome, &address, &expected_room, Some(&source))
                })),
            })
        })
        .collect()
}

pub(crate) fn alarm_action_rows(state: &ChromeState) -> Vec<crate::shell::search::SearchResult> {
    use crate::shell::search::action_api::{ActionArgs, ActionError};
    use crate::shell::search::{ResourceKind, SearchResult};
    let pending = pending_snoozes_for_active_room(state);
    let tree = state.tree();
    let Some(workspace) = tree.desktop.active_workspace() else {
        return Vec::new();
    };
    if !state.alarm_actions_available() {
        return Vec::new();
    }
    let room = workspace.room.to_string();
    let actor = workspace.actor_for_person(&state.local_user_for_room(&workspace.room));
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for notice in state
        .notification_log()
        .active()
        .iter()
        .chain(state.notification_log().history())
    {
        if !seen.insert(notice.id) {
            continue;
        }
        let Some(request) = scheduled_launch(notice)
            .filter(|request| request.target.calendar_alarm && request.target.room == room)
        else {
            continue;
        };
        for (key, label) in tilo_desktop::notifications::alarm_action_labels() {
            if key == "snooze" && request.target.snoozed {
                continue;
            }
            let id = format!("notification.{}.{key}", notice.id);
            if !crate::shell::search::providers::actions::shell_action_authorized_for(
                workspace, &actor, &id,
            ) {
                continue;
            }
            let expected_room = request.target.room.clone();
            let action_id = id.clone();
            rows.push(SearchResult {
                facts: Vec::new(),
                shortcut: None,
                kind: ResourceKind::Action,
                operation_kind: tilo_actions::OperationKind::Effect,
                provider_id: "actions".into(),
                id: id.clone(),
                title: tilo_desktop::notifications::alarm_action_title(&label, &notice.title),
                subtitle: Some(tilo_desktop::notifications::alarm_action_subtitle()),
                badge: None,
                score: 0,
                params: Vec::new(),
                contract: Some(
                    crate::shell::search::providers::actions::shell_action_contract(&id),
                ),
                on_open: std::rc::Rc::new(|_, _, _, _| {
                    Err(ActionError::Refused(
                        "alarm actions require lifecycle dispatch".into(),
                    ))
                }),
                on_invoke: None,
                on_begin_invoke: Some(std::rc::Rc::new(
                    move |_, _, chrome: &Chrome, _: &ActionArgs| {
                        begin_alarm_action(chrome, &action_id, &expected_room, None)
                    },
                )),
            });
        }
    }
    rows.extend(snoozed_alarm_rows(state, workspace, pending));
    drop(tree);
    rows
}

fn begin_alarm_action(
    chrome: &Chrome,
    address: &str,
    expected_room: &str,
    source: Option<&crate::notifications::SnoozedAlarm>,
) -> Result<
    crate::shell::search::InvocationDisposition,
    crate::shell::search::action_api::ActionError,
> {
    use crate::shell::search::action_api::ActionError;
    use crate::shell::search::{ActionReceipt, InvocationDisposition};
    let request = {
        let mut state = chrome.borrow_mut();
        let allowed = {
            let tree = state.tree();
            tree.desktop.active_workspace().is_some_and(|workspace| {
                let actor = workspace.actor_for_person(&state.local_user_for_room(&workspace.room));
                workspace.room.to_string() == expected_room
                    && crate::shell::search::providers::actions::shell_action_authorized_for(
                        workspace, &actor, address,
                    )
            })
        };
        if !allowed {
            return Err(ActionError::Refused(
                "alarm's original workspace authority changed".into(),
            ));
        }
        if let Some(source) = source {
            state
                .request_snoozed_alarm_dismiss(source)
                .map_err(ActionError::Refused)?
        } else {
            let (notice_id, action) = alarm_action_address(address)
                .ok_or_else(|| ActionError::Refused("unknown alarm action".into()))?;
            state
                .request_alarm_action(notice_id, action, true)
                .map_err(ActionError::Refused)?
        }
    };
    let chrome = chrome.clone();
    let ticket = std::rc::Rc::new(move || {
        chrome
            .borrow_mut()
            .take_alarm_action_result(request)
            .map_or(Ok(None), |result| {
                result
                    .map(|()| Some(ActionReceipt::completed()))
                    .map_err(ActionError::Refused)
            })
    });
    Ok(InvocationDisposition::Pending(ticket))
}

/// Put a startup refusal on the screen and leave it there. The compositor
/// falling back to a bootstrapped tree means the person is looking at a
/// desktop that isn't theirs, so `Error` severity + [`PERSISTENT_DISMISS`].
pub fn announce_state_notice(chrome: &Chrome, notice: &crate::shell::persist::StateNotice) {
    chrome.borrow_mut().push_notification_full(
        Notification::new(notice.title(), notice.body())
            .with_severity(NotificationSeverity::Error)
            .with_dismiss_after(PERSISTENT_DISMISS),
    );
}

/// A notification from a D-Bus `Notify` call. `urgency` 2 (critical) is
/// forced persistent regardless of `expire_timeout` — the spec says critical
/// must not auto-expire; otherwise `-1` → [`DEFAULT_DISMISS_AFTER`], `0` →
/// [`PERSISTENT_DISMISS`], `> 0` → that many milliseconds.
pub fn from_incoming(inc: IncomingNotification) -> Notification {
    let app_id = inc.caller.as_ref().map(|caller| caller.app_id.clone());
    let mut notification = Notification::new(inc.summary, inc.body);
    notification.severity = match inc.urgency {
        2 => NotificationSeverity::Error,
        _ => NotificationSeverity::Info,
    };
    notification.dismiss_after = if inc.urgency == 2 {
        PERSISTENT_DISMISS
    } else {
        match inc.expire_timeout {
            t if t < 0 => DEFAULT_DISMISS_AFTER,
            0 => PERSISTENT_DISMISS,
            // Safe: the two arms above already took every `t <= 0`.
            ms => Duration::from_millis(u64::try_from(ms).unwrap_or(0)),
        }
    };
    notification.sender_id = Some(inc.dbus_id);
    // `app_name` is sender-controlled in the freedesktop protocol. Only show
    // the label derived from launcher attribution.
    notification.app_name = app_id.as_ref().map(|id| {
        crate::shell::launcher::display_name_for_app_id(id).unwrap_or_else(|| id.clone())
    });
    notification.app_id = app_id;
    notification.source_workspace = inc.caller.as_ref().map(|c| c.workspace_id.0);
    // A communications app that names who a message is from, by display
    // name and contact handle, gets a banner led by that person. The name is
    // what the person is called; the handle only picks their colour and is
    // never shown. The bridge passes both on only from a launcher-verified
    // Tilo app.
    notification.from = inc
        .sender_name
        .clone()
        .zip(inc.contact_endpoint.clone())
        .map(|(name, identity)| NotificationPerson { name, identity });
    // A call rings for the person it is from; a category alone is only the
    // app's word, so it counts only beside a verified sender.
    if inc.incoming_call && notification.from.is_some() {
        notification.addressed = Some(tilo_desktop::notifications::Addressed::IncomingCall);
    }
    notification.machine_wide = inc.machine_wide;
    notification.actions = inc.actions;
    // Never from the wire: an app that could ask to pierce Do-Not-Disturb
    // would ask every time.
    notification.bypass_dnd = false;
    notification.show_on_lock_screen = false;
    notification
}

/// A validated, ephemeral projection from another device as an ordinary
/// notification, keeping its authenticated provenance and privacy ceiling.
/// It never gets a local D-Bus route.
pub fn from_remote_continuity(
    remote: RemoteContinuityNotification,
    now_unix_ms: u64,
) -> Option<Notification> {
    if !remote.projection.validate_at(now_unix_ms) {
        return None;
    }
    let projection = remote.projection;
    let body_redacted = projection.body_redacted;
    let source_caption = t_args(
        "notification-source-caption",
        &[
            ("app", projection.app_name.as_str().into()),
            ("device", remote.source_label.as_str().into()),
        ],
    );
    let body = if body_redacted {
        String::new()
    } else {
        projection.body
    };
    let mut notification = Notification::new(projection.title, body);
    if projection.urgency == 2 {
        notification.severity = NotificationSeverity::Error;
    }
    notification.dismiss_after =
        Duration::from_millis(projection.expires_unix_ms.saturating_sub(now_unix_ms));
    notification.remote_route = Some(RemoteNotificationRoute {
        source_device: remote.source_device,
        source_label: remote.source_label,
        notification_id: projection.id,
    });
    notification.app_name = Some(source_caption);
    notification.app_id = Some(projection.app_id);
    notification.actions = projection.actions;
    // A remote device's notification never expands the local lock screen
    // disclosure surface.
    notification.show_on_lock_screen = false;
    notification.show_preview = !body_redacted;
    Some(notification)
}

/// The command that dismisses a mirrored notification on its origin device.
pub fn remote_dismiss_command(notification: &Notification) -> Option<ContinuityCommandEgress> {
    let route = notification.remote_route.as_ref()?;
    Some(ContinuityCommandEgress {
        target_device: route.source_device.clone(),
        id: route.notification_id,
        command: ContinuityNotificationCommand::Dismiss,
    })
}

/// An exact-origin command, only for an action advertised on this live
/// projection. The origin bridge independently checks its own live allowlist
/// before emitting anything to the application.
pub fn remote_action_command(
    notification: &Notification,
    action_key: &str,
) -> Option<ContinuityCommandEgress> {
    if !notification
        .actions
        .iter()
        .any(|(key, _)| key == action_key)
    {
        return None;
    }
    let route = notification.remote_route.as_ref()?;
    Some(ContinuityCommandEgress {
        target_device: route.source_device.clone(),
        id: route.notification_id,
        command: ContinuityNotificationCommand::Invoke {
            action_key: action_key.to_string(),
        },
    })
}

/// The chrome as the shared stack's and centre's [`NotificationHost`]. With
/// the Task Center's broker attached, a retained notification can be handed
/// to a live workspace agent.
pub struct ChromeNotifications<'a> {
    chrome: &'a Chrome,
    agents: Option<&'a RoomActivityPane>,
}

impl<'a> ChromeNotifications<'a> {
    pub fn new(chrome: &'a Chrome) -> Self {
        Self {
            chrome,
            agents: None,
        }
    }

    #[must_use]
    pub fn with_agents(mut self, agents: &'a RoomActivityPane) -> Self {
        self.agents = Some(agents);
        self
    }

    /// The workspace, room and agent a notification may go to, when that
    /// agent is live right now.
    fn live_agent(
        &self,
        notification: &Notification,
    ) -> Option<(crate::shell::state::WorkspaceId, String, String)> {
        let agents = self.agents?;
        let (workspace, room, agent) = self
            .chrome
            .borrow()
            .notification_agent_candidate(notification)?;
        let room = room.to_string();
        agents
            .agent_is_present_in(&room, &agent)
            .then_some((workspace, room, agent))
    }
}

impl NotificationHost for ChromeNotifications<'_> {
    fn actions_for(&self, notification: &Notification) -> Vec<(String, String)> {
        let state = self.chrome.borrow();
        if let Some(actions) = state.device_asking_actions(notification.id) {
            return actions;
        }
        let available = state.alarm_actions_available();
        if available && state.alarm_action_authorized(notification) {
            let snoozed = notification
                .scheduled_target
                .as_ref()
                .is_some_and(|target| target.snoozed);
            tilo_desktop::notifications::alarm_action_labels()
                .into_iter()
                .filter(|(key, _)| !snoozed || key != "snooze")
                .collect()
        } else {
            Vec::new()
        }
    }

    fn invoke_action(&self, id: u64, key: &str) {
        if matches!(key, "approve" | "decline")
            && self
                .chrome
                .borrow_mut()
                .answer_device_asking(id, key == "approve")
        {
            return;
        }
        let action = match key {
            "snooze" => crate::notifications::ScheduledAction::Snooze,
            "dismiss" => crate::notifications::ScheduledAction::Dismiss,
            _ => return,
        };
        let mut state = self.chrome.borrow_mut();
        if let Err(error) = state.request_alarm_action(id, action, false) {
            tracing::warn!(%error, "alarm notification action refused");
            state.push_warning_notification(
                tilo_desktop::notifications::alarm_action_failed_label(),
                tilo_desktop::notifications::alarm_action_retry_label(),
            );
        }
    }

    fn log(&self) -> Ref<'_, NotificationLog> {
        Ref::map(self.chrome.borrow(), ChromeState::notification_log)
    }

    fn present(&self, now: std::time::Instant, on_screen: &[u64], held: bool) {
        self.chrome
            .borrow_mut()
            .present_notifications(now, on_screen, held);
    }

    fn open_center(&self) {
        self.chrome
            .borrow_mut()
            .open_quick_settings_level(crate::shell::quick_settings::QsLevel::Notifications);
    }

    fn session_locked(&self) -> bool {
        self.chrome.borrow().session_locked()
    }

    fn quiet_status(&self) -> Option<String> {
        self.chrome.borrow().quiet_status_now()
    }

    fn dismiss(&self, id: u64) {
        let alarm = self
            .chrome
            .borrow()
            .notifications()
            .iter()
            .find(|notice| notice.id == id)
            .is_some_and(|notice| !self.actions_for(notice).is_empty());
        if alarm {
            self.invoke_action(id, "dismiss");
        } else {
            self.chrome.borrow_mut().dismiss_notification(id);
        }
    }

    fn activate(&self, id: u64) {
        self.chrome.borrow_mut().activate_notification(id);
    }

    fn activate_history(&self, id: u64) -> HistoryActivation {
        self.chrome.borrow_mut().activate_history_notification(id)
    }

    fn dismiss_history(&self, id: u64) {
        let alarm = self
            .chrome
            .borrow()
            .notification_log()
            .history_entry(id)
            .is_some_and(|notice| !self.actions_for(notice).is_empty());
        if alarm {
            self.invoke_action(id, "dismiss");
        } else {
            self.chrome.borrow_mut().dismiss_history_notification(id);
        }
    }

    fn dismiss_history_group(&self, key: &str) {
        let ids = self.chrome.borrow().notification_log().group_ids(key);
        for id in ids {
            self.dismiss_history(id);
        }
    }

    fn clear_history(&self) {
        self.chrome.borrow_mut().clear_notification_history();
    }

    fn undo_clear(&self) {
        self.chrome.borrow_mut().undo_notification_history_clear();
    }

    fn agent_for(&self, notification: &Notification) -> Option<String> {
        self.live_agent(notification).map(|(_, _, agent)| agent)
    }

    fn hand_to_agent(&self, notification: &Notification) {
        let (Some(agents), Some((workspace, room, agent))) =
            (self.agents, self.live_agent(notification))
        else {
            return;
        };
        let task = self.chrome.borrow_mut().hand_history_notification_to_agent(
            notification.id,
            workspace,
            &agent,
        );
        let Some(task) = task else {
            return;
        };
        if let Err(error) = agents.dispatch_task_in(&room, &task) {
            tracing::warn!(%task, "handing a notification to {agent} failed: {error}");
        }
    }

    fn changed(&self) {
        self.chrome.borrow_mut().bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::WorkspaceTree;
    use std::time::Instant;
    use tilo_desktop::notifications::{MAX_HISTORY, MAX_NOTIFICATIONS, NotificationStack};
    use ui::text_render::TextRenderer;
    use ui::theme::Theme;
    use vello::Scene;
    use vello::kurbo::{Affine, Point, Rect};

    fn new_chrome() -> Chrome {
        let chrome = ChromeState::new(WorkspaceTree::default());
        chrome.borrow_mut().set_notification_policy_for_tests(
            settings_store::notification_policy::NotificationPolicy::default(),
        );
        chrome
    }

    /// Every live banner came on screen, unheld, at `at`, as the banner
    /// stack reports it: only then does a banner's time run.
    fn shown(chrome: &Chrome, at: Instant) {
        let ids: Vec<u64> = chrome
            .borrow()
            .notifications()
            .iter()
            .map(|n| n.id)
            .collect();
        chrome.borrow_mut().present_notifications(at, &ids, false);
    }

    #[test]
    fn toast_queue_pushes_caps_dismisses_and_expires() {
        fn push_appends_to_chrome_queue() {
            let chrome = new_chrome();
            chrome.borrow_mut().push_notification("Hello", "World");
            let ch = chrome.borrow();
            assert_eq!(ch.notifications().len(), 1);
            assert_eq!(ch.notifications()[0].title, "Hello");
            assert_eq!(ch.notifications()[0].body, "World");
            assert_eq!(ch.notifications()[0].severity, NotificationSeverity::Info);
        }

        fn cap_enforces_fifo_drop_at_max() {
            let chrome = new_chrome();
            for i in 0..(MAX_NOTIFICATIONS + 3) {
                chrome.borrow_mut().push_notification(format!("n{i}"), "");
            }
            assert_eq!(chrome.borrow().notifications().len(), MAX_NOTIFICATIONS);
            let first_title = chrome.borrow().notifications()[0].title.clone();
            assert_eq!(first_title, "n3");
        }

        fn dismiss_removes_by_id() {
            let chrome = new_chrome();
            let id_a = chrome.borrow_mut().push_notification("A", "");
            let _id_b = chrome.borrow_mut().push_notification("B", "");
            let id_c = chrome.borrow_mut().push_notification("C", "");
            assert_eq!(chrome.borrow().notifications().len(), 3);
            chrome.borrow_mut().dismiss_notification(id_a);
            assert_eq!(chrome.borrow().notifications().len(), 2);
            let titles: Vec<_> = chrome
                .borrow()
                .notifications()
                .iter()
                .map(|n| n.title.clone())
                .collect();
            assert_eq!(titles, vec!["B".to_string(), "C".to_string()]);
            chrome.borrow_mut().dismiss_notification(id_c);
            assert_eq!(chrome.borrow().notifications().len(), 1);
            assert_eq!(chrome.borrow().notifications()[0].title, "B");
        }

        fn expiry_drops_old_notifications_in_update_lifecycle() {
            let chrome = new_chrome();
            let id = chrome.borrow_mut().push_notification_full(
                Notification::new("Quick", "fades fast")
                    .with_dismiss_after(Duration::from_millis(50)),
            );
            assert_eq!(chrome.borrow().notifications().len(), 1);
            let future = Instant::now() + Duration::from_millis(200);
            assert_eq!(
                chrome.borrow_mut().update_notification_lifecycle(future),
                0,
                "a banner never on screen keeps its time"
            );
            shown(&chrome, Instant::now());
            let dropped = chrome.borrow_mut().update_notification_lifecycle(future);
            assert_eq!(dropped, 1);
            assert!(chrome.borrow().notifications().is_empty());
            // Re-dismissing a dropped id is a no-op, not a panic.
            chrome.borrow_mut().dismiss_notification(id);
        }

        fn switch_workspace_does_not_push_a_notification() {
            // `bootstrap_demo` gives ≥2 workspaces so a switch is meaningful;
            // `default()` builds an empty tree that can't exercise it.
            let chrome = ChromeState::new(WorkspaceTree::bootstrap_demo());
            let switched = chrome.borrow_mut().switch_workspace(1);
            assert!(switched, "expected a workspace switch");
            assert!(
                chrome.borrow().notifications().is_empty(),
                "a workspace switch must not raise a banner, got {:?}",
                chrome
                    .borrow()
                    .notifications()
                    .iter()
                    .map(|n| n.title.clone())
                    .collect::<Vec<_>>()
            );
            assert!(
                chrome.borrow().notification_history().is_empty(),
                "a workspace switch must not be recorded in history"
            );
        }

        push_appends_to_chrome_queue();
        cap_enforces_fifo_drop_at_max();
        dismiss_removes_by_id();
        expiry_drops_old_notifications_in_update_lifecycle();
        switch_workspace_does_not_push_a_notification();
    }

    #[test]
    fn toast_stack_layout_motion_and_accessibility() {
        /// Review fixtures: the ux-bench cannot raise a notification on macOS,
        /// so the stack is photographed from its paint path — a live column
        /// with an error toast, Do Not Disturb holding items (which puts no
        /// card on screen), and the locked-session card — at 1× and 200 %
        /// text, dark and light.
        #[expect(
            clippy::many_single_char_names,
            reason = "viewport size and sampled colour channels read best as w/h and r/g/b/e"
        )]
        fn notification_stack_review_fixtures_are_written() {
            // Motion reads at its end: this test checks the resting frame.
            let _settled = ui::motion::testing::settled();
            let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../target/ui-review/s2-r2/notifications");
            let (w, h) = (640_u32, 400_u32);
            let viewport = Rect::new(0.0, 0.0, f64::from(w), f64::from(h));
            let mut renderer = crate::shell::osd::review_offscreen::OffscreenRenderer::new(w, h);
            let variants = [
                ("dark-1x", Theme::dark(), 1.0_f32),
                ("dark-2x", Theme::dark().with_text_scale(2.0), 2.0),
                ("light-1x", Theme::light(), 1.0),
            ];
            for (name, mut theme, scale) in variants {
                // A still cannot show the entry slide; photograph the rest state.
                theme.reduce_motion = true;
                for state in ["live", "dnd", "locked"] {
                    let chrome = new_chrome();
                    {
                        let mut ch = chrome.borrow_mut();
                        match state {
                            "dnd" => {
                                ch.set_dnd(true);
                                ch.push_notification("Held", "while quiet");
                                ch.push_notification("Also held", "for later");
                            }
                            "locked" => {
                                ch.push_notification("Private", "never shown while locked");
                                ch.push_notification("Second", "count only");
                                ch.set_session_locked(true);
                            }
                            _ => {
                                ch.push_notification("Saved", "Report.pdf is in Documents");
                                ch.push_notification_full(
                                    Notification::new(
                                        "Backup failed",
                                        "The drive is not connected",
                                    )
                                    .with_severity(NotificationSeverity::Error),
                                );
                            }
                        }
                    }
                    let host = ChromeNotifications::new(&chrome);
                    let mut stack = NotificationStack::new();
                    stack.set_viewport(viewport);
                    let mut tr = TextRenderer::new();
                    tr.set_text_scale(scale);
                    let mut scene = Scene::new();
                    stack.draw(&host, &mut scene, Affine::IDENTITY, &mut tr, &theme);
                    assert_eq!(
                        tr.text_scale(),
                        scale,
                        "the pass restores the renderer's scale"
                    );
                    let ground = theme.tokens.colors.surfaces.base;
                    let path = out.join(format!("{name}-{state}.png"));
                    renderer.render_png(&scene, ground, &path);
                    assert!(
                        path.is_file(),
                        "notification fixture missing: {}",
                        path.display()
                    );
                    if state == "dnd" {
                        assert!(stack.rect().is_zero_area(), "{name}: no quiet card");
                        continue;
                    }
                    // The rendered pixels, not just the file: the first surface
                    // in the column (locked card or toast card) is the
                    // card ground at its plain bottom-right interior — the
                    // toolkit toast and the two shell cards share one material.
                    let first = stack.rect();
                    let first_h = if state == "live" {
                        // The shared toast card's height.
                        64.0
                    } else {
                        first.height()
                    };
                    let probe = Point::new(first.x1 - 12.0, first.y0 + first_h - 8.0);
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        reason = "probe points are inside a positive viewport"
                    )]
                    let (r, g, b, _) =
                        crate::shell::osd::review_offscreen::OffscreenRenderer::sample(
                            &renderer.render_resilient(&scene, ground),
                            w,
                            probe.x as u32,
                            probe.y as u32,
                        );
                    let e = ui::widget::toast::card_ground(&theme).0.to_rgba8();
                    assert!(
                        r.abs_diff(e.r) <= 2 && g.abs_diff(e.g) <= 2 && b.abs_diff(e.b) <= 2,
                        "{name}-{state}: surface interior ({r},{g},{b}) is the card ground ({},{},{})",
                        e.r,
                        e.g,
                        e.b
                    );
                }
            }
        }

        notification_stack_review_fixtures_are_written();
    }

    // ---------- D-Bus bridge wiring ----------

    use crate::notifications::{CloseReason, IncomingNotification, NotificationBridge};
    use crate::portal::PortalCaller;
    use crate::shell::state::WorkspaceId;

    fn incoming(dbus_id: u32, urgency: u8, expire: i32) -> IncomingNotification {
        IncomingNotification {
            dbus_id,
            app_name: "Mailer".into(),
            summary: "New mail".into(),
            body: "secret subject".into(),
            actions: vec![("default".into(), "Open".into())],
            expire_timeout: expire,
            urgency,
            caller: Some(PortalCaller::new(WorkspaceId(9), "org.mail")),
            badge_count: None,
            contact_endpoint: None,
            sender_name: None,
            sound: Some(settings_store::system_sounds::SystemSoundEvent::Notification),
            incoming_call: false,
            machine_wide: false,
        }
    }

    #[test]
    fn dbus_notifications_map_fields_and_replace_by_id() {
        fn from_incoming_maps_fields_and_urgency() {
            let n = from_incoming(incoming(3, 2, -1));
            assert_eq!(n.title, "New mail");
            assert_eq!(n.body, "secret subject");
            assert_eq!(n.sender_id, Some(3));
            assert_eq!(n.app_name.as_deref(), Some("org.mail"));
            assert_eq!(n.source_workspace, Some(9));
            // Critical urgency → Error severity + persistent dwell.
            assert_eq!(n.severity, NotificationSeverity::Error);
            assert_eq!(n.dismiss_after, PERSISTENT_DISMISS);
            assert_eq!(n.default_action_key().as_deref(), Some("default"));
            // An app's own notice is from no one in particular...
            assert_eq!(n.from, None);
            // ...a handle alone names no one: it is never shown as a name...
            let mut message = incoming(4, 1, -1);
            message.contact_endpoint = Some("mailto:ada@example.org".into());
            assert_eq!(from_incoming(message.clone()).from, None);
            // ...a message naming its sender is from that person.
            message.sender_name = Some("Ada".into());
            assert_eq!(
                from_incoming(message).from,
                Some(NotificationPerson {
                    name: "Ada".into(),
                    identity: "mailto:ada@example.org".into(),
                })
            );
        }

        fn from_incoming_expire_timeout_mapping() {
            // -1 = server default.
            assert_eq!(
                from_incoming(incoming(1, 1, -1)).dismiss_after,
                DEFAULT_DISMISS_AFTER
            );
            // 0 = never expire (modelled as PERSISTENT_DISMISS).
            assert_eq!(
                from_incoming(incoming(1, 1, 0)).dismiss_after,
                PERSISTENT_DISMISS
            );
            // >0 = milliseconds.
            assert_eq!(
                from_incoming(incoming(1, 1, 2500)).dismiss_after,
                Duration::from_millis(2500)
            );
        }

        fn pump_dbus_notifications_creates_attributed_toast() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            bridge.push_incoming(incoming(11, 1, 5000));
            chrome.borrow_mut().pump_dbus_notifications();
            let ch = chrome.borrow();
            assert_eq!(ch.notifications().len(), 1);
            let toast = &ch.notifications()[0];
            assert_eq!(toast.sender_id, Some(11));
            assert_eq!(toast.source_workspace, Some(9));
            assert_eq!(toast.app_name.as_deref(), Some("org.mail"));
        }

        fn replaces_id_supersedes_existing_toast_via_pump() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            bridge.push_incoming(incoming(5, 1, 5000));
            chrome.borrow_mut().pump_dbus_notifications();
            // Same dbus_id replaces, not appends.
            let mut second = incoming(5, 1, 5000);
            second.summary = "Updated".into();
            bridge.push_incoming(second);
            chrome.borrow_mut().pump_dbus_notifications();
            let ch = chrome.borrow();
            assert_eq!(ch.notifications().len(), 1, "replaces_id must supersede");
            assert_eq!(ch.notifications()[0].title, "Updated");
        }

        from_incoming_maps_fields_and_urgency();
        from_incoming_expire_timeout_mapping();
        pump_dbus_notifications_creates_attributed_toast();
        replaces_id_supersedes_existing_toast_via_pump();
    }

    fn remote_notification(
        source: &str,
        token: u8,
        title: &str,
        redacted: bool,
    ) -> crate::notifications::RemoteContinuityNotification {
        crate::notifications::RemoteContinuityNotification {
            source_device: source.into(),
            source_label: "Quinten's phone".into(),
            projection: crate::notifications::ContinuityNotificationProjection {
                id: tilo_desktop::notifications::ContinuityNotificationId {
                    epoch: [7; 16],
                    token: [token; 16],
                },
                app_id: "so.tilo.mail".into(),
                app_name: "Mail".into(),
                title: title.into(),
                body: if redacted {
                    String::new()
                } else {
                    "Secret preview".into()
                },
                body_redacted: redacted,
                actions: vec![("reply".into(), "Reply".into())],
                urgency: 1,
                expires_unix_ms: 12_000,
            },
        }
    }

    #[test]
    fn remote_projections_are_attributed_redacted_and_never_outlive_expiry() {
        fn remote_projection_becomes_attributed_non_dbus_toast() {
            let notification =
                from_remote_continuity(remote_notification("phone", 1, "New mail", false), 2_000)
                    .unwrap();
            assert_eq!(notification.title, "New mail");
            assert_eq!(notification.body, "Secret preview");
            assert_eq!(
                notification.app_name.as_deref(),
                Some("Mail · From Quinten's phone")
            );
            assert_eq!(notification.sender_id, None);
            assert_eq!(notification.source_workspace, None);
            assert_eq!(notification.actions, vec![("reply".into(), "Reply".into())]);
            assert_eq!(notification.dismiss_after, Duration::from_secs(10));
            assert!(!notification.show_on_lock_screen);
            assert_eq!(
                notification
                    .remote_route
                    .as_ref()
                    .map(|route| route.source_device.as_str()),
                Some("phone")
            );
        }

        fn remote_redaction_cannot_be_reintroduced_by_ui_adapter() {
            let notification =
                from_remote_continuity(remote_notification("phone", 2, "Private", true), 2_000)
                    .unwrap();
            assert!(notification.body.is_empty());
            assert!(!notification.show_preview);
        }

        fn remote_replacement_preserves_ui_identity_but_not_across_sources() {
            let mut log = NotificationLog::default();
            let first =
                from_remote_continuity(remote_notification("phone", 3, "First", false), 2_000)
                    .unwrap();
            let ui_id = first.id;
            let _ = log.show(first);
            let updated =
                from_remote_continuity(remote_notification("phone", 3, "Updated", false), 2_001)
                    .unwrap();
            assert!(log.replace_remote(&updated));
            assert_eq!(log.active()[0].id, ui_id);
            assert_eq!(log.active()[0].title, "Updated");
            let spoofed =
                from_remote_continuity(remote_notification("tablet", 3, "Spoofed", false), 2_002)
                    .unwrap();
            assert!(!log.replace_remote(&spoofed));
            assert_eq!(log.active()[0].title, "Updated");
        }

        fn expired_remote_projection_is_not_adapted() {
            assert!(
                from_remote_continuity(remote_notification("phone", 5, "Stale", false), 12_000,)
                    .is_none()
            );
        }

        fn remote_inbox_replaces_in_place_and_never_reprojects() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let first = live_remote_projection(6, "First");
            let mut replacement = first.clone();
            replacement.title = "Updated".into();
            let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            assert!(bridge.receive_continuity_projection("phone", "Phone", first, now));
            assert!(bridge.receive_continuity_projection("phone", "Phone", replacement, now));
            chrome.borrow_mut().pump_dbus_notifications();
            let state = chrome.borrow();
            assert_eq!(state.notifications().len(), 1);
            assert_eq!(state.notifications()[0].title, "Updated");
            drop(state);
            assert!(bridge.drain_continuity_outbound().is_empty());
        }

        fn expired_remote_projection_leaves_no_toast_or_persisted_state() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            let mut projection = live_remote_projection(8, "Brief");
            projection.expires_unix_ms = now.saturating_add(10);
            let id = projection.id;
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection, now));
            chrome.borrow_mut().pump_dbus_notifications();
            assert_eq!(chrome.borrow().notifications().len(), 1);
            bridge.expire_remote_continuity(now.saturating_add(11));
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(chrome.borrow().notifications().is_empty());
            assert!(chrome.borrow().notification_history().is_empty());
            assert!(!bridge.retire_remote_continuity("phone", id));
        }

        remote_projection_becomes_attributed_non_dbus_toast();
        remote_redaction_cannot_be_reintroduced_by_ui_adapter();
        remote_replacement_preserves_ui_identity_but_not_across_sources();
        expired_remote_projection_is_not_adapted();
        remote_inbox_replaces_in_place_and_never_reprojects();
        expired_remote_projection_leaves_no_toast_or_persisted_state();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn remote_notification_actions_route_to_their_exact_origin() {
        fn remote_actions_return_exact_origin_commands_without_local_dbus_route() {
            let notification =
                from_remote_continuity(remote_notification("phone", 4, "Action", false), 2_000)
                    .unwrap();
            assert!(notification.sender_id.is_none());
            assert!(remote_action_command(&notification, "archive").is_none());
            let action = remote_action_command(&notification, "reply").unwrap();
            assert_eq!(action.target_device, "phone");
            assert_eq!(
                action.command,
                crate::notifications::ContinuityNotificationCommand::Invoke {
                    action_key: "reply".into(),
                }
            );
            let dismiss = remote_dismiss_command(&notification).unwrap();
            assert_eq!(dismiss.target_device, "phone");
            assert_eq!(
                dismiss.command,
                crate::notifications::ContinuityNotificationCommand::Dismiss
            );
        }

        fn remote_activation_routes_command_and_origin_retire_removes_history() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let projection = live_remote_projection(7, "Action");
            let remote_id = projection.id;
            let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection, now));
            chrome.borrow_mut().pump_dbus_notifications();
            let ui_id = chrome.borrow().notifications()[0].id;
            assert!(chrome.borrow_mut().activate_notification(ui_id));
            let commands = bridge.drain_continuity_command_egress();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].target_device, "phone");
            assert_eq!(
                commands[0].command,
                crate::notifications::ContinuityNotificationCommand::Invoke {
                    action_key: "reply".into(),
                }
            );
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(chrome.borrow().notification_history().is_empty());
            assert!(!bridge.retire_remote_continuity("phone", remote_id));
            assert!(bridge.drain_action_events().is_empty());
            assert!(bridge.drain_closed_events().is_empty());
        }

        fn a_retained_mirrored_notification_routes_its_action_back_to_its_device() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let projection = live_remote_projection(8, "Reply later");
            let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            assert!(bridge.receive_continuity_projection("phone", "Phone", projection, now));
            chrome.borrow_mut().pump_dbus_notifications();
            let ui_id = chrome.borrow().notifications()[0].id;
            // The banner times out locally; the origin still holds it.
            shown(&chrome, Instant::now());
            let later = Instant::now() + Duration::from_secs(120);
            chrome.borrow_mut().update_notification_lifecycle(later);
            assert!(bridge.drain_continuity_command_egress().is_empty());
            assert_eq!(chrome.borrow().notification_history()[0].id, ui_id);

            let outcome = chrome.borrow_mut().activate_history_notification(ui_id);
            assert_eq!(
                outcome,
                tilo_desktop::notifications::HistoryActivation::Invoked
            );
            let commands = bridge.drain_continuity_command_egress();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].target_device, "phone");
            assert_eq!(
                commands[0].command,
                crate::notifications::ContinuityNotificationCommand::Invoke {
                    action_key: "reply".into(),
                }
            );
            assert!(
                bridge.drain_action_events().is_empty(),
                "no local D-Bus route"
            );
            assert!(chrome.borrow().notification_history().is_empty());
        }

        fn origin_remote_command_invokes_live_app_closes_toast_and_retires_siblings() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            bridge.push_incoming(incoming(92, 1, 5_000));
            chrome.borrow_mut().pump_dbus_notifications();
            let events = bridge.drain_continuity_outbound();
            let crate::notifications::ContinuityOutboundEvent::Project(projection) = &events[0]
            else {
                panic!("expected local projection");
            };
            let projection_id = projection.id;
            let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
            assert!(bridge.apply_continuity_command(
                projection_id,
                &crate::notifications::ContinuityNotificationCommand::Invoke {
                    action_key: "default".into(),
                },
                now,
            ));
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            assert_eq!(bridge.drain_action_events(), vec![(92, "default".into())]);
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(92, CloseReason::Dismissed)]
            );
            assert_eq!(
                bridge.drain_continuity_outbound(),
                vec![crate::notifications::ContinuityOutboundEvent::Retire(
                    projection_id
                )]
            );
        }

        remote_actions_return_exact_origin_commands_without_local_dbus_route();
        remote_activation_routes_command_and_origin_retire_removes_history();
        a_retained_mirrored_notification_routes_its_action_back_to_its_device();
        origin_remote_command_invokes_live_app_closes_toast_and_retires_siblings();
    }

    #[test]
    fn scheduled_activation_never_falls_back_to_another_room() {
        let tree = WorkspaceTree::bootstrap_demo().shared();
        assert!(!tree.read().desktop.workspaces.is_empty());
        let request = NotificationLaunch {
            app_id: "so.tilo.clocks".into(),
            target: tilo_desktop::notifications::ScheduledNotificationTarget {
                room: "missing-original-room".into(),
                token: "0123456789abcdef0123456789abcdef".into(),
                occurrence_ms: 1,
                calendar_alarm: true,
                snoozed: false,
                uri: "tilo-clocks:alarm/exact-alarm-id".into(),
            },
        };
        assert_eq!(
            launch_scheduled_notification(request, &tree),
            Err("notification's original room is unavailable".into())
        );
    }

    #[test]
    fn a_restored_notification_opens_its_verified_app() {
        let chrome = new_chrome();
        let mut restored = Notification::new("Invoice", "");
        restored.app_id = Some("org.mail".into());
        restored.app_name = Some("Mail".into());
        chrome.borrow_mut().set_dnd(true);
        let id = chrome.borrow_mut().push_notification_full(restored);
        assert_eq!(
            chrome.borrow_mut().activate_history_notification(id),
            tilo_desktop::notifications::HistoryActivation::OpenApp {
                app_id: "org.mail".into(),
                label: "Mail".into(),
            }
        );
        let system = chrome.borrow_mut().push_notification("Backup", "");
        assert_eq!(
            chrome.borrow_mut().activate_history_notification(system),
            tilo_desktop::notifications::HistoryActivation::Unavailable,
            "a shell notification with nothing to act on stays retained"
        );
        assert_eq!(chrome.borrow().notification_history().len(), 1);
    }

    #[test]
    fn only_verified_senders_project_badges_and_follow_their_settings() {
        fn only_verified_apps_project_and_preview_policy_redacts() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());

            let mut unverified = incoming(90, 1, 5_000);
            unverified.caller = None;
            bridge.push_incoming(unverified);
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(bridge.drain_continuity_outbound().is_empty());

            let policy = settings_store::notification_policy::NotificationPolicy {
                show_previews: false,
                ..Default::default()
            };
            chrome
                .borrow_mut()
                .set_notification_policy_for_tests(policy);
            bridge.push_incoming(incoming(91, 1, 5_000));
            chrome.borrow_mut().pump_dbus_notifications();
            let events = bridge.drain_continuity_outbound();
            let crate::notifications::ContinuityOutboundEvent::Project(projection) = &events[0]
            else {
                panic!("verified app should project");
            };
            assert!(projection.body_redacted);
            assert!(projection.body.is_empty());
        }

        fn only_verified_senders_can_update_shell_badges() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());

            let mut unverified = incoming(21, 1, 5000);
            unverified.caller = None;
            unverified.badge_count = Some(88);
            bridge.push_incoming(unverified);
            chrome.borrow_mut().pump_dbus_notifications();
            assert_eq!(chrome.borrow().notification_badge_total(), 0);

            let mut verified = incoming(22, 1, 5000);
            verified.badge_count = Some(12);
            bridge.push_incoming(verified);
            chrome.borrow_mut().pump_dbus_notifications();
            assert_eq!(chrome.borrow().notification_badge_for("org.mail"), 12);
            assert_eq!(chrome.borrow().notification_badge_total(), 12);
        }

        fn verified_foreign_identity_obeys_its_settings_rule() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let mut policy = settings_store::notification_policy::NotificationPolicy::default();
            policy.per_app.insert(
                "org.example.Chat".into(),
                settings_store::notification_policy::AppNotificationRule {
                    banners: Some(false),
                    ..Default::default()
                },
            );
            chrome
                .borrow_mut()
                .set_notification_policy_for_tests(policy);

            let mut foreign = incoming(23, 1, 5000);
            // This is the identity the notification service derived from a
            // Flatpak sender's PID + cgroup, not its Notify.app_name string.
            foreign.app_name = "Pretend Settings".into();
            foreign.caller = Some(PortalCaller::new(
                WorkspaceId::default(),
                "org.example.Chat",
            ));
            bridge.push_incoming(foreign);
            chrome.borrow_mut().pump_dbus_notifications();

            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            assert_eq!(
                chrome.borrow().notification_history()[0].app_id.as_deref(),
                Some("org.example.Chat")
            );
        }

        only_verified_apps_project_and_preview_policy_redacts();
        only_verified_senders_can_update_shell_badges();
        verified_foreign_identity_obeys_its_settings_rule();
    }

    fn live_remote_projection(
        token: u8,
        title: &str,
    ) -> crate::notifications::ContinuityNotificationProjection {
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap_or(0);
        let mut remote = remote_notification("phone", token, title, false).projection;
        remote.expires_unix_ms = now.saturating_add(60_000);
        remote
    }

    #[test]
    fn dbus_toasts_emit_closed_signals_and_internal_ones_never_do() {
        fn expire_emits_notification_closed_event() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            chrome.borrow_mut().push_notification_full(
                from_incoming(incoming(21, 1, 5000)).with_dismiss_after(Duration::from_millis(10)),
            );
            shown(&chrome, Instant::now());
            let future = Instant::now() + Duration::from_millis(200);
            chrome.borrow_mut().update_notification_lifecycle(future);
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(21, CloseReason::Expired)],
                "an expired D-Bus toast must emit NotificationClosed(reason=Expired)"
            );
        }

        fn dismiss_emits_closed_event() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let id = chrome
                .borrow_mut()
                .push_notification_full(from_incoming(incoming(31, 1, 5000)));
            chrome.borrow_mut().dismiss_notification(id);
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(31, CloseReason::Dismissed)]
            );
        }

        fn activate_invokes_default_action_then_closes() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let id = chrome
                .borrow_mut()
                .push_notification_full(from_incoming(incoming(41, 1, 5000)));
            assert!(chrome.borrow_mut().activate_notification(id));
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(
                bridge.drain_action_events(),
                vec![(41, "default".to_string())]
            );
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(41, CloseReason::Dismissed)]
            );
        }

        fn close_request_pump_removes_and_emits_closed() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            bridge.push_incoming(incoming(51, 1, 5000));
            chrome.borrow_mut().pump_dbus_notifications();
            assert_eq!(chrome.borrow().notifications().len(), 1);
            bridge.request_close(51);
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(
                bridge.drain_closed_events(),
                vec![(51, CloseReason::Closed)]
            );
        }

        fn internal_notifications_never_emit_dbus_signals() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            let id = chrome.borrow_mut().push_notification("internal", "");
            chrome.borrow_mut().dismiss_notification(id);
            assert!(bridge.drain_closed_events().is_empty());
        }

        expire_emits_notification_closed_event();
        dismiss_emits_closed_event();
        activate_invokes_default_action_then_closes();
        close_request_pump_removes_and_emits_closed();
        internal_notifications_never_emit_dbus_signals();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn do_not_disturb_suppresses_toasts_but_keeps_attributed_history() {
        fn session_lock_suppression_label() {
            let chrome = new_chrome();
            chrome.borrow_mut().push_notification("a", "b");
            chrome.borrow_mut().push_notification("c", "d");
            chrome.borrow_mut().set_session_locked(true);
            assert!(chrome.borrow().session_locked());
            assert_eq!(
                tilo_desktop::notifications::lock_suppressed_label(
                    chrome.borrow().notification_count()
                ),
                "2 notifications"
            );
        }

        fn dnd_suppresses_active_but_keeps_history() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_dnd(true);
            assert!(chrome.borrow().dnd());
            let id = chrome.borrow_mut().push_notification("Held", "while DND");
            let ch = chrome.borrow();
            assert!(
                ch.notifications().is_empty(),
                "DND must keep the active banner stack empty"
            );
            assert_eq!(ch.notification_history().len(), 1);
            assert_eq!(ch.notification_history()[0].title, "Held");
            assert_eq!(ch.notification_history()[0].id, id);
        }

        fn dnd_suppressed_dbus_notification_preserves_attribution_in_history() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            chrome.borrow_mut().set_notification_policy_for_tests(
                settings_store::notification_policy::NotificationPolicy {
                    do_not_disturb: true,
                    ..settings_store::notification_policy::NotificationPolicy::default()
                },
            );
            bridge.push_incoming(incoming(77, 1, 5000));
            chrome.borrow_mut().pump_dbus_notifications();
            let ch = chrome.borrow();
            assert!(ch.notifications().is_empty(), "DND suppresses the banner");
            assert_eq!(ch.notification_history().len(), 1);
            let h = &ch.notification_history()[0];
            assert_eq!(h.sender_id, Some(77));
            assert_eq!(h.app_name.as_deref(), Some("org.mail"));
            assert_eq!(h.source_workspace, Some(9));
        }

        fn toggle_dnd_flips_state_and_bumps_revision() {
            let chrome = new_chrome();
            let r0 = chrome.borrow().revision;
            assert!(!chrome.borrow().dnd());
            assert!(chrome.borrow_mut().toggle_dnd());
            assert!(chrome.borrow().dnd());
            assert!(chrome.borrow().revision > r0, "toggle must bump revision");
            assert!(!chrome.borrow_mut().toggle_dnd());
            assert!(!chrome.borrow().dnd());
        }

        fn set_dnd_is_idempotent() {
            let chrome = new_chrome();
            let r0 = chrome.borrow().revision;
            chrome.borrow_mut().set_dnd(false);
            assert_eq!(
                chrome.borrow().revision,
                r0,
                "a no-op set must not bump the revision"
            );
            chrome.borrow_mut().set_dnd(true);
            assert!(chrome.borrow().revision > r0);
        }

        fn dnd_is_the_persisted_policy_and_the_tile_mirrors_it() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_dnd(true);
            {
                let ch = chrome.borrow();
                assert!(ch.dnd());
                assert!(ch.quick_settings.do_not_disturb);
                assert_eq!(
                    ch.quiet_status(12 * 60, 0).as_deref(),
                    Some("Do Not Disturb")
                );
            }
            // Personal Settings writes the same policy; the shell adopts it.
            chrome.borrow_mut().set_notification_policy_for_tests(
                settings_store::notification_policy::NotificationPolicy::default(),
            );
            let ch = chrome.borrow();
            assert!(!ch.dnd());
            assert!(!ch.quick_settings.do_not_disturb);
            assert_eq!(ch.quiet_status(12 * 60, 0), None);
        }

        fn a_focus_schedule_names_when_it_ends() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_notification_policy_for_tests(
                settings_store::notification_policy::NotificationPolicy {
                    focus: settings_store::notification_policy::FocusSchedule {
                        enabled: true,
                        start_minute: 22 * 60,
                        end_minute: 7 * 60 + 30,
                        ..settings_store::notification_policy::FocusSchedule::default()
                    },
                    ..settings_store::notification_policy::NotificationPolicy::default()
                },
            );
            let ch = chrome.borrow();
            // Until a clock is chosen, the time reads as the desktop's
            // language reads clocks: English on a 12-hour one.
            assert_eq!(
                ch.quiet_status(23 * 60, 2).as_deref(),
                Some("Focus until 7:30 AM")
            );
            assert_eq!(ch.quiet_status(12 * 60, 2), None);
        }

        fn presenting_holds_banners_without_changing_the_persisted_switch() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_presenting(true);
            chrome
                .borrow_mut()
                .push_notification("Private", "not for the room");
            {
                let ch = chrome.borrow();
                assert!(!ch.dnd(), "presenting never rewrites the person's choice");
                assert!(ch.notifications().is_empty());
                assert_eq!(ch.notification_history().len(), 1);
                assert_eq!(
                    ch.quiet_status(12 * 60, 0).as_deref(),
                    Some("Do Not Disturb while presenting")
                );
            }
            chrome.borrow_mut().set_presenting(false);
            chrome.borrow_mut().push_notification("Back", "shown again");
            assert_eq!(chrome.borrow().notifications().len(), 1);
        }

        fn sharing_a_surface_holds_banners() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_active_exposures(vec![
                crate::shell::chrome_state::ActiveExposure {
                    peer: 7,
                    surface: 3,
                    can_control: false,
                },
            ]);
            chrome
                .borrow_mut()
                .push_notification("Private", "not for peers");
            let ch = chrome.borrow();
            assert!(ch.notifications().is_empty());
            assert_eq!(
                ch.quiet_status(12 * 60, 0).as_deref(),
                Some("Do Not Disturb while sharing")
            );
        }

        fn leaving_dnd_does_not_replay_held_notifications() {
            let chrome = new_chrome();
            chrome.borrow_mut().set_dnd(true);
            chrome.borrow_mut().push_notification("held", "");
            chrome.borrow_mut().set_dnd(false);
            assert!(
                chrome.borrow().notifications().is_empty(),
                "held notifications are not replayed as banners"
            );
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            // A fresh post now shows again.
            chrome.borrow_mut().push_notification("live", "");
            assert_eq!(chrome.borrow().notifications().len(), 1);
        }

        session_lock_suppression_label();
        dnd_suppresses_active_but_keeps_history();
        dnd_suppressed_dbus_notification_preserves_attribution_in_history();
        toggle_dnd_flips_state_and_bumps_revision();
        set_dnd_is_idempotent();
        dnd_is_the_persisted_policy_and_the_tile_mirrors_it();
        a_focus_schedule_names_when_it_ends();
        presenting_holds_banners_without_changing_the_persisted_switch();
        sharing_a_surface_holds_banners();
        leaving_dnd_does_not_replay_held_notifications();
    }

    // ---------- Do-Not-Disturb + history ----------

    #[test]
    fn retired_notifications_move_to_a_bounded_clearable_history() {
        fn active_notification_moves_to_history_on_dismiss() {
            let chrome = new_chrome();
            let id = chrome
                .borrow_mut()
                .push_notification("Shown", "then dismissed");
            assert_eq!(chrome.borrow().notifications().len(), 1);
            assert_eq!(chrome.borrow().notification_history().len(), 0);
            chrome.borrow_mut().dismiss_notification(id);
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            assert_eq!(chrome.borrow().notification_history()[0].title, "Shown");
        }

        fn expiry_moves_notification_to_history() {
            let chrome = new_chrome();
            chrome.borrow_mut().push_notification_full(
                Notification::new("Quick", "fades").with_dismiss_after(Duration::from_millis(10)),
            );
            assert_eq!(chrome.borrow().notification_history().len(), 0);
            shown(&chrome, Instant::now());
            let future = Instant::now() + Duration::from_millis(200);
            let dropped = chrome.borrow_mut().update_notification_lifecycle(future);
            assert_eq!(dropped, 1);
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(
                chrome.borrow().notification_history().len(),
                1,
                "an expired toast must be retained in history"
            );
        }

        fn close_by_dbus_id_moves_to_history() {
            let chrome = new_chrome();
            let bridge = NotificationBridge::new();
            chrome.borrow_mut().set_notification_bridge(bridge.clone());
            bridge.push_incoming(incoming(88, 1, 5000));
            chrome.borrow_mut().pump_dbus_notifications();
            assert_eq!(chrome.borrow().notifications().len(), 1);
            assert_eq!(chrome.borrow().notification_history().len(), 0);
            bridge.request_close(88);
            chrome.borrow_mut().pump_dbus_notifications();
            assert!(chrome.borrow().notifications().is_empty());
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            assert_eq!(
                chrome.borrow().notification_history()[0].sender_id,
                Some(88)
            );
        }

        fn cap_eviction_retires_oldest_to_history() {
            let chrome = new_chrome();
            for i in 0..(MAX_NOTIFICATIONS + 3) {
                chrome.borrow_mut().push_notification(format!("n{i}"), "");
            }
            assert_eq!(chrome.borrow().notifications().len(), MAX_NOTIFICATIONS);
            // The 3 evicted from the front are in history, oldest-first.
            let ch = chrome.borrow();
            assert_eq!(ch.notification_history().len(), 3);
            let titles: Vec<_> = ch
                .notification_history()
                .iter()
                .map(|n| n.title.clone())
                .collect();
            assert_eq!(titles, vec!["n0", "n1", "n2"]);
        }

        fn history_is_bounded_to_max() {
            let chrome = new_chrome();
            for i in 0..(MAX_HISTORY + 10) {
                let id = chrome.borrow_mut().push_notification(format!("h{i}"), "");
                chrome.borrow_mut().dismiss_notification(id);
            }
            let ch = chrome.borrow();
            assert_eq!(ch.notification_history().len(), MAX_HISTORY);
            let last = ch.notification_history().last().unwrap().title.clone();
            assert_eq!(last, format!("h{}", MAX_HISTORY + 10 - 1));
            assert!(
                ch.notification_history().iter().all(|n| n.title != "h0"),
                "the oldest entry must have been evicted"
            );
        }

        fn clear_history_empties_the_log() {
            let chrome = new_chrome();
            let id = chrome.borrow_mut().push_notification("x", "y");
            chrome.borrow_mut().dismiss_notification(id);
            assert_eq!(chrome.borrow().notification_history().len(), 1);
            chrome.borrow_mut().clear_notification_history();
            assert_eq!(chrome.borrow().notification_history().len(), 0);
        }

        active_notification_moves_to_history_on_dismiss();
        expiry_moves_notification_to_history();
        close_by_dbus_id_moves_to_history();
        cap_eviction_retires_oldest_to_history();
        history_is_bounded_to_max();
        clear_history_empties_the_log();
    }
}
