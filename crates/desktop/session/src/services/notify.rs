//! Notifications from the session's apps, served on the socket their launcher opened for them.
//!
//! An app runs in a process with no bus of its own, so what it shows the person goes over this
//! connection instead: the session pushes it into the one notifications daemon every app's and
//! the desktop's own notices reach ([`tilo_notifications_service::Notifications`]), attributed
//! to the app the launcher named on the connection. What the person then does with a
//! notification — choosing it or one of its buttons, dismissing it — comes back on the same
//! connection ([`tilo_notify::service`]), so an app's notifications behave as the portal's once
//! did.
//!
//! A notification the app names by a tag keeps one id for its life, so showing the tag again
//! replaces it. The deadlines an app hands over are the session's own durable schedules
//! ([`tilo_notifications_service::Schedules`]), kept in the workspace, and only that app's are
//! ever read back to it.

use std::collections::HashMap;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use tilo_lock::MutexRecover as _;
use tilo_notifications_service::{
    Caller, ContactProjection, Notification, Notifications, Owner, ScheduleRequest,
};
use tilo_notify::service::{AlarmRequest, Channel, NotifySide, Refusal};
use tilo_notify::{CalendarAlarm, PriorityContact};
use tilo_session_core::desktop::notifications::open_action_label;

/// The daemon's key for a notification's default action, as the notifications service names it.
const DEFAULT_ACTION: &str = "default";
/// The key a notification's button at this index invokes, after this.
const BUTTON_ACTION: &str = "tilo-button-";
/// A notification's title, as long as an app's may be before the daemon keeps it.
const MAX_TITLE_CHARS: usize = 512;
/// A notification's body, bounded as the portal bounds one.
const MAX_BODY_CHARS: usize = 16 * 1024;
/// What an app may type into a notification's action key: the app's own URI, which the Rust side
/// of the wire bounded at [`tilo_notify`]'s 2048 bytes already.
const MAX_URI_BYTES: usize = 2048;

/// The notification service, served until the session ends.
pub(crate) struct Served;

/// One notification the daemon is showing, as the app that showed it knows it.
struct Open {
    tag: String,
    /// The URI each of its action keys hands back.
    routes: HashMap<String, String>,
    channel: Weak<Channel>,
}

/// Every notification shown now, by the daemon's id.
type Shown = Arc<Mutex<HashMap<u32, Open>>>;

/// Bind the notification socket under the launcher's services root and serve it.
///
/// # Errors
/// The services root or the socket cannot be made, or the acceptor cannot start.
pub(crate) fn serve(notifications: Notifications) -> Result<Served, String> {
    launcher::services::ensure_services_root()
        .map_err(|error| format!("create the services directory: {error}"))?;
    serve_at(
        &launcher::services::socket_for(launcher::services::NOTIFY_SERVICE),
        notifications,
    )
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the notification service accepts and serves on its own threads"
)]
fn serve_at(path: &Path, notifications: Notifications) -> Result<Served, String> {
    if path.exists() {
        std::fs::remove_file(path)
            .map_err(|error| format!("remove the stale notification socket: {error}"))?;
    }
    let listener = tilo_files::native::socket::bind(path)
        .map_err(|error| format!("bind the notification socket {}: {error}", path.display()))?;
    let shown: Shown = Arc::default();
    // What the person did with a notification reaches the app that showed it: only the connection
    // that showed it is told, and each tap carries the app's own tag and URI back.
    let tapped = Arc::clone(&shown);
    notifications.on_taken(move |id, taken| {
        let tilo_notifications_service::Taken::Action { key, .. } = taken else {
            // No app on this wire offers an inline reply: the Rust client cannot take one, and
            // an app meant to answer the person asks for their window instead.
            return;
        };
        let Some((route, tag, channel)) = tapped.lock_recover().get(&id).and_then(|open| {
            Some((
                open.routes.get(key)?.clone(),
                open.tag.clone(),
                open.channel.upgrade()?,
            ))
        }) else {
            return;
        };
        // A notification whose action the person took is read; the banner goes as the app asks
        // it to, or as the session ages it.
        tracing::debug!(%tag, %route, "a notification's action was taken");
        channel.activated(&route);
    });
    // A notification that went away is told to the app that showed it, unless a newer one under
    // the same tag replaced it.
    let closed = Arc::clone(&shown);
    notifications.on_closed(move |id, reason| {
        let Some(open) = closed.lock_recover().remove(&id) else {
            return;
        };
        let Some(channel) = open.channel.upgrade() else {
            return;
        };
        channel.closed(&open.tag, close_reason(reason));
    });
    let accepting = Arc::clone(&shown);
    std::thread::Builder::new()
        .name("tilo-notify-session".into())
        .spawn(move || accept(&listener, &accepting, &notifications))
        .map_err(|error| format!("start the notification service: {error}"))?;
    tracing::info!(path = %path.display(), "serving notifications to the apps");
    Ok(Served)
}

fn close_reason(reason: tilo_notifications_service::CloseReason) -> tilo_notify::CloseReason {
    match reason {
        tilo_notifications_service::CloseReason::Expired => tilo_notify::CloseReason::Expired,
        tilo_notifications_service::CloseReason::Dismissed => tilo_notify::CloseReason::Dismissed,
        tilo_notifications_service::CloseReason::Closed => tilo_notify::CloseReason::Closed,
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "session helper: the notification service accepts and serves on its own threads"
)]
fn accept(listener: &UnixListener, shown: &Shown, notifications: &Notifications) {
    for stream in listener.incoming() {
        match stream {
            Ok(mut stream) => {
                // The launcher names the app before the app can write: everything it shows is
                // that app's, and nothing on the connection can say otherwise.
                let Ok(app_id) = runtime_portal::session_services::read_app_id(&mut stream) else {
                    tracing::warn!("a notification connection named no app");
                    continue;
                };
                let side = Arc::new(Side {
                    caller: Caller {
                        app_id,
                        workspace: None,
                    },
                    channel: Mutex::default(),
                    tagged: Mutex::default(),
                    shown: Arc::clone(shown),
                    notifications: notifications.clone(),
                });
                if let Err(error) = std::thread::Builder::new()
                    .name("tilo-notify-app".into())
                    .spawn(move || tilo_notify::service::serve(stream, &*side))
                {
                    tracing::warn!(%error, "a notification connection could not be served");
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => {
                tracing::warn!(%error, "the notification service's accept failed");
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

/// One app's connection: the notifications it shows, and the way back to it.
struct Side {
    caller: Caller,
    channel: Mutex<Option<Arc<Channel>>>,
    /// The daemon's id for each of this app's tags, so a tag shown again replaces its
    /// notification.
    tagged: Mutex<HashMap<String, u32>>,
    shown: Shown,
    notifications: Notifications,
}

impl NotifySide for Side {
    fn attached(&self, channel: Arc<Channel>) {
        *self.channel.lock_recover() = Some(channel);
    }

    fn show(&self, notification: &tilo_notify::Notification) -> Result<(), Refusal> {
        let Some(channel) = self.channel.lock_recover().clone() else {
            return Err(Refusal::Unavailable);
        };
        let (id, routes) = self.tagged_for(notification);
        self.shown.lock_recover().insert(
            id,
            Open {
                tag: notification.tag.clone(),
                routes,
                channel: Arc::downgrade(&channel),
            },
        );
        self.notifications
            .push(shown(id, &self.caller, notification));
        Ok(())
    }

    fn withdraw(&self, tag: &str) -> Result<(), Refusal> {
        let Some(id) = self.tagged.lock_recover().remove(tag) else {
            return Ok(());
        };
        self.shown.lock_recover().remove(&id);
        // The session takes its banner away. The app is not told: it withdrew it, and it already
        // knows.
        self.notifications.request_close(id);
        Ok(())
    }

    fn schedule(
        &self,
        notification: &tilo_notify::Notification,
        at: tilo_notify::SystemTime,
        alarm: Option<&AlarmRequest>,
    ) -> Result<(), Refusal> {
        let at_ms = at
            .duration_since(tilo_notify::SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|since| i64::try_from(since.as_millis()).ok())
            .ok_or_else(|| Refusal::Invalid("invalid notification deadline".into()))?;
        let alarm = alarm.map(service_alarm).transpose()?;
        self.notifications
            .schedules()
            .schedule(
                &Owner::of(&self.caller),
                ScheduleRequest {
                    tag: notification.tag.clone(),
                    title: truncate(&notification.title, MAX_TITLE_CHARS),
                    body: truncate(&notification.body, MAX_BODY_CHARS),
                    activation: notification.activation.clone().unwrap_or_default(),
                    at_ms,
                    alarm,
                },
            )
            .map_err(Refusal::Failed)
    }

    fn scheduled(&self) -> Result<Vec<tilo_notify::Scheduled>, Refusal> {
        Ok(self
            .notifications
            .schedules()
            .pending(&Owner::of(&self.caller))
            .into_iter()
            .map(|(tag, at_ms)| tilo_notify::Scheduled {
                tag,
                at: at_of(at_ms).unwrap_or(tilo_notify::SystemTime::UNIX_EPOCH),
            })
            .collect())
    }

    fn alarm_states(&self) -> Result<Vec<tilo_notify::AlarmState>, Refusal> {
        Ok(self
            .notifications
            .schedules()
            .alarm_states(&Owner::of(&self.caller))
            .into_iter()
            .map(
                |(tag, (hour, minute, weekdays, enabled_at_ms), next, last, date)| {
                    let last_rung = if date.is_empty() {
                        None
                    } else {
                        jiff::civil::Date::strptime("%Y-%m-%d", &date).ok()
                    };
                    tilo_notify::AlarmState {
                        tag,
                        alarm: CalendarAlarm {
                            hour,
                            minute,
                            weekdays,
                        },
                        enabled_at_ms,
                        next_at: next.first().and_then(|at| at_of(*at)),
                        last_at: last.first().and_then(|at| at_of(*at)),
                        last_rung,
                    }
                },
            )
            .collect())
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), Refusal> {
        self.notifications
            .schedules()
            .cancel(&Owner::of(&self.caller), tag);
        Ok(())
    }

    fn priority_contacts(&self, contacts: &[PriorityContact]) -> Result<(), Refusal> {
        let contacts: Vec<ContactProjection> = contacts
            .iter()
            .map(|contact| ContactProjection {
                id: contact.id.clone(),
                name: contact.name.clone(),
                endpoints: contact.endpoints.clone(),
            })
            .collect();
        self.notifications
            .priority_contacts()
            .replace_verified(&contacts)
            .map(drop)
            .map_err(Refusal::Failed)
    }
}

impl Side {
    /// The daemon's id for `notification`, kept for its tag, and the URI each of its action keys
    /// hands back.
    fn tagged_for(
        &self,
        notification: &tilo_notify::Notification,
    ) -> (u32, HashMap<String, String>) {
        let id = {
            let mut tagged = self.tagged.lock_recover();
            *tagged
                .entry(notification.tag.clone())
                .or_insert_with(|| self.notifications.alloc_id())
        };
        let mut routes = HashMap::new();
        if let Some(uri) = &notification.activation {
            routes.insert(DEFAULT_ACTION.to_owned(), uri.clone());
        }
        for (index, button) in notification.buttons.iter().enumerate() {
            routes.insert(format!("{BUTTON_ACTION}{index}"), button.activation.clone());
        }
        routes.retain(|_, uri| uri.len() <= MAX_URI_BYTES);
        (id, routes)
    }
}

/// The daemon's notification for what an app showed, attributed to it.
fn shown(id: u32, caller: &Caller, notification: &tilo_notify::Notification) -> Notification {
    let mut actions: Vec<(String, String)> = Vec::new();
    if notification.activation.is_some() {
        actions.push((
            DEFAULT_ACTION.to_owned(),
            notification
                .action
                .clone()
                .unwrap_or_else(open_action_label),
        ));
    }
    for (index, button) in notification.buttons.iter().enumerate() {
        actions.push((format!("{BUTTON_ACTION}{index}"), button.label.clone()));
    }
    Notification {
        summary: truncate(&notification.title, MAX_TITLE_CHARS),
        body: truncate(&notification.body, MAX_BODY_CHARS),
        actions,
        urgency: match notification.severity {
            tilo_notify::Severity::Error => 2,
            tilo_notify::Severity::Info | tilo_notify::Severity::Warning => 1,
        },
        caller: Some(caller.clone()),
        suppress_sound: notification.silent,
        ..Notification::new(id)
    }
}

/// `alarm` as the wall-clock alarm the notifications service keeps.
fn service_alarm(
    alarm: &AlarmRequest,
) -> Result<tilo_notifications_service::AlarmRequest, Refusal> {
    let last_rung = alarm
        .last_rung
        .as_deref()
        .map(|date| {
            jiff::civil::Date::strptime("%Y-%m-%d", date)
                .map_err(|_| Refusal::Invalid("invalid delivered alarm date".into()))
        })
        .transpose()?;
    Ok(tilo_notifications_service::AlarmRequest {
        hour: alarm.hour,
        minute: alarm.minute,
        weekdays: alarm.weekdays,
        enabled_at_ms: alarm.enabled_at_ms,
        last_rung,
    })
}

/// `at_ms` as a system time, when it is one.
fn at_of(at_ms: i64) -> Option<tilo_notify::SystemTime> {
    let millis = u64::try_from(at_ms).ok()?;
    tilo_notify::SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(millis))
}

fn truncate(value: &str, max: usize) -> String {
    value.chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use tilo_notifications_service::Arrival;
    use tilo_notify::Notification as AppNotification;
    use tilo_notify::Notifier as _;
    use tilo_notify::service::ServiceNotifier;

    /// The service at a socket of its own, and an app's connection to it that the launcher
    /// named `app`.
    fn session(app: &str) -> (Notifications, ServiceNotifier, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let path = directory.path().join("notify.sock");
        let notifications = Notifications::new();
        serve_at(&path, notifications.clone()).expect("the service serves");
        let mut stream = tilo_socket::connect(&path).expect("the socket is there");
        runtime_portal::session_services::write_app_id(&mut stream, app).expect("the handshake");
        let client = ServiceNotifier::connect(stream).expect("the connection is usable");
        (notifications, client, directory)
    }

    fn shown(notifications: &Notifications) -> Vec<tilo_notifications_service::Notification> {
        notifications
            .drain()
            .into_iter()
            .filter_map(|arrival| match arrival {
                Arrival::Notify(notification) => Some(notification),
                Arrival::Close(_) => None,
            })
            .collect()
    }

    #[test]
    fn a_notification_an_app_shows_arrives_attributed_to_it() {
        let (notifications, client, _directory) = session("so.tilo.mail");
        client
            .show(
                &AppNotification::new("new-mail", "Ada wrote", "See you at nine")
                    .with_activation("tilo-mail:thread/1")
                    .with_button("Reply", "tilo-mail:reply/1")
                    .with_severity(tilo_notify::Severity::Warning)
                    .quietly(),
            )
            .expect("the session took it");
        let arrived = shown(&notifications);
        assert_eq!(arrived.len(), 1);
        assert_eq!(arrived[0].summary, "Ada wrote");
        assert_eq!(arrived[0].body, "See you at nine");
        assert_eq!(
            arrived[0].caller,
            Some(Caller {
                app_id: "so.tilo.mail".to_owned(),
                workspace: None,
            })
        );
        assert_eq!(
            arrived[0].actions,
            [
                ("default".to_owned(), "Open".to_owned()),
                ("tilo-button-0".to_owned(), "Reply".to_owned()),
            ]
        );
        assert!(
            arrived[0].suppress_sound,
            "a quiet notification stays quiet"
        );
        assert_eq!(arrived[0].urgency, 1);
    }

    #[test]
    fn a_tag_shown_again_replaces_its_notification_under_the_same_id() {
        let (notifications, client, _directory) = session("so.tilo.mail");
        client
            .show(&AppNotification::new("download", "Downloading", "40%"))
            .expect("the first");
        client
            .show(&AppNotification::new("download", "Downloading", "80%"))
            .expect("the second");
        let arrived = shown(&notifications);
        assert_eq!(arrived.len(), 2);
        assert_eq!(arrived[0].id, arrived[1].id, "one notification, replaced");
        assert_eq!(notifications.live().len(), 1);
        assert_eq!(notifications.live()[0].body, "80%");
    }

    #[test]
    fn what_the_person_chooses_reaches_the_app_and_its_own_uri() {
        let (notifications, client, _directory) = session("so.tilo.mail");
        let (activated, chosen) = mpsc::channel();
        tilo_notify::set_activation_handler("tilo-session-notify-test", move |uri| {
            let _ = activated.send(uri.to_owned());
        });
        client
            .show(
                &AppNotification::new("thread", "Ada wrote", "")
                    .with_activation("tilo-session-notify-test:thread/1")
                    .with_button("Reply", "tilo-session-notify-test:reply/1"),
            )
            .expect("the session took it");
        let id = notifications.live()[0].id;
        notifications.action_invoked(id, "default", None);
        assert_eq!(
            chosen.recv_timeout(Duration::from_secs(5)),
            Ok("tilo-session-notify-test:thread/1".to_owned()),
            "the default action hands back what the app showed it with"
        );
        notifications.action_invoked(id, "tilo-button-0", None);
        assert_eq!(
            chosen.recv_timeout(Duration::from_secs(5)),
            Ok("tilo-session-notify-test:reply/1".to_owned()),
            "and a button hands back its own"
        );
    }

    #[test]
    fn a_dismissal_reaches_the_app_under_its_tag_and_reason() {
        let (notifications, client, _directory) = session("so.tilo.mail");
        let (closed, gone) = mpsc::channel();
        tilo_notify::set_close_handler("tilo-session-close-test:", move |tag, reason| {
            let _ = closed.send((tag.to_owned(), reason));
        });
        client
            .show(&AppNotification::new(
                "tilo-session-close-test:1",
                "Ada wrote",
                "",
            ))
            .expect("the session took it");
        let id = notifications.live()[0].id;
        notifications.closed(id, tilo_notifications_service::CloseReason::Dismissed);
        assert_eq!(
            gone.recv_timeout(Duration::from_secs(5)),
            Ok((
                "tilo-session-close-test:1".to_owned(),
                tilo_notify::CloseReason::Dismissed
            ))
        );
    }

    #[test]
    fn an_app_withdraws_only_its_own_notification() {
        let (notifications, client, _directory) = session("so.tilo.mail");
        client
            .show(&AppNotification::new("one", "Ada wrote", ""))
            .expect("the first");
        client
            .show(&AppNotification::new("two", "Bo wrote", ""))
            .expect("the second");
        let _ = shown(&notifications);
        client.withdraw("one").expect("withdrawn");
        assert_eq!(
            notifications
                .live()
                .iter()
                .map(|notification| notification.summary.as_str())
                .collect::<Vec<_>>(),
            ["Bo wrote"],
            "only the tag it named went"
        );
        assert!(
            notifications
                .drain()
                .iter()
                .any(|arrival| matches!(arrival, Arrival::Close(_))),
            "the session takes its banner away"
        );
    }

    #[test]
    fn a_deadline_is_kept_for_the_app_that_handed_it_over() {
        let (notifications, client, _directory) = session("so.tilo.clock");
        let at = tilo_notify::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        client
            .schedule(&AppNotification::new("timer", "Tea", "It is ready"), at)
            .expect("the session took the deadline");
        let owner = Owner {
            app: "so.tilo.clock".to_owned(),
            workspace: "local".to_owned(),
        };
        assert_eq!(
            notifications.schedules().pending(&owner),
            [("timer".to_owned(), 1_800_000_000_000)]
        );
        let other = Owner {
            app: "so.tilo.mail".to_owned(),
            workspace: "local".to_owned(),
        };
        assert!(
            notifications.schedules().pending(&other).is_empty(),
            "another app reads none of it"
        );
        assert_eq!(
            client.scheduled().expect("its own deadlines"),
            [tilo_notify::Scheduled {
                tag: "timer".to_owned(),
                at
            }]
        );
        client.cancel_scheduled("timer").expect("cancelled");
        assert!(notifications.schedules().pending(&owner).is_empty());
    }

    #[test]
    fn a_connection_that_names_no_app_is_not_served() {
        let directory = tempfile::tempdir().expect("a scratch directory");
        let path = directory.path().join("notify.sock");
        let notifications = Notifications::new();
        serve_at(&path, notifications.clone()).expect("the service serves");
        let mut stream = tilo_socket::connect(&path).expect("the socket is there");
        let frame = serde_json::to_vec(&serde_json::json!({ "protocol": 1 })).expect("the frame");
        runtime_portal::session_services::APP_ID_FRAMING
            .write(&mut stream, &frame)
            .expect("a handshake naming no app");
        // The session drops the connection rather than serve an app it cannot name.
        let mut reader = stream.try_clone().expect("the connection");
        let mut byte = [0_u8; 1];
        assert_eq!(
            std::io::Read::read(&mut reader, &mut byte).unwrap_or(0),
            0,
            "the connection is closed"
        );
        assert!(notifications.live().is_empty());
    }
}
