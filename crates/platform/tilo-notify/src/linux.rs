//! The Linux backend on the session bus: the notification portal
//! (`org.freedesktop.portal.Notification`, which the Tilo shell and
//! xdg-desktop-portal implement), else the freedesktop notification service
//! (`org.freedesktop.Notifications`) on a desktop without a portal.
//!
//! Every call runs on one bus thread and is bounded, so a stuck service can
//! delay the caller by at most [`CALL_BUDGET`] and never by a D-Bus timeout.
//!
//! A notification with an activation URI carries a default action (the
//! portal's `default-action`, the freedesktop `default` action); the first
//! such notification starts listeners for both services' `ActionInvoked`
//! signals on the same connection, which hand the URI to the app while it
//! runs.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use zbus::blocking::Connection;
use zbus::zvariant::Value;

use crate::{
    AlarmSchedule, AlarmState, CalendarAlarm, Notification, Notifier, NotifyError, PriorityContact,
    Scheduled, SystemTime,
};

type AlarmStateWire = (String, (u8, u8, u8, i64), Vec<i64>, Vec<i64>, String);

const CALL_BUDGET: Duration = Duration::from_secs(3);

const NOT_TILO: &str = "Only an app Tilo launched may name priority contacts.";

const PORTAL: (&str, &str, &str) = (
    "org.freedesktop.portal.Desktop",
    "/org/freedesktop/portal/desktop",
    "org.freedesktop.portal.Notification",
);
const FREEDESKTOP: (&str, &str, &str) = (
    "org.freedesktop.Notifications",
    "/org/freedesktop/Notifications",
    "org.freedesktop.Notifications",
);

type Job = Box<dyn FnOnce(Option<&Connection>) + Send>;

/// The bus thread's queue. `None` when the thread could not start.
fn bus() -> Option<&'static Mutex<mpsc::Sender<Job>>> {
    static BUS: OnceLock<Option<Mutex<mpsc::Sender<Job>>>> = OnceLock::new();
    BUS.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Job>();
        tilo_rt::spawn_thread("tilo-notify-bus", move || {
            let connection = Connection::session()
                .inspect_err(|error| tracing::debug!(%error, "no session bus for notifications"))
                .ok();
            while let Ok(job) = rx.recv() {
                job(connection.as_ref());
            }
        })
        .ok()?;
        Some(Mutex::new(tx))
    })
    .as_ref()
}

/// Run `call` on the bus thread and wait at most [`CALL_BUDGET`].
fn on_bus<T: Send + 'static>(
    call: impl FnOnce(&Connection) -> Result<T, NotifyError> + Send + 'static,
) -> Result<T, NotifyError> {
    let (reply, answer) = mpsc::channel();
    let job: Job = Box::new(move |connection| {
        let _ = reply.send(connection.map_or(Err(NotifyError::Unavailable), call));
    });
    bus()
        .and_then(|bus| bus.lock().ok()?.send(job).ok())
        .ok_or(NotifyError::Unavailable)?;
    answer.recv_timeout(CALL_BUDGET).unwrap_or_else(|_| {
        Err(NotifyError::Failed(
            "the notification service did not answer".into(),
        ))
    })
}

/// Whether `error` says nothing on the bus implements the call, as opposed to
/// the service refusing it.
fn missing(error: &zbus::Error) -> bool {
    let zbus::Error::MethodError(name, _, _) = error else {
        return false;
    };
    matches!(
        name.as_str(),
        "org.freedesktop.DBus.Error.ServiceUnknown"
            | "org.freedesktop.DBus.Error.UnknownInterface"
            | "org.freedesktop.DBus.Error.UnknownMethod"
            | "org.freedesktop.DBus.Error.UnknownObject"
            | "org.freedesktop.DBus.Error.NameHasNoOwner"
    )
}

#[allow(clippy::needless_pass_by_value, reason = "a `map_err` adapter")]
fn failed(error: zbus::Error) -> NotifyError {
    NotifyError::Failed(error.to_string())
}

/// Which service answered first; later calls go straight to it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Service {
    Portal,
    Freedesktop,
}

pub(crate) struct SessionNotifier {
    app_id: String,
}

/// Freedesktop ids by tag, so a tag replaces and withdraws its own
/// notification. Shared by every notifier in the process: the service hands
/// out ids per connection, and there is one.
fn ids() -> &'static Mutex<HashMap<String, u32>> {
    static IDS: OnceLock<Mutex<HashMap<String, u32>>> = OnceLock::new();
    IDS.get_or_init(Mutex::default)
}

fn chosen() -> &'static Mutex<Option<Service>> {
    static CHOSEN: OnceLock<Mutex<Option<Service>>> = OnceLock::new();
    CHOSEN.get_or_init(Mutex::default)
}

impl SessionNotifier {
    pub(crate) fn new(app_id: &str) -> Self {
        Self {
            app_id: app_id.to_owned(),
        }
    }
}

/// The portal action a click invokes; its target is the activation URI.
const PORTAL_ACTION: &str = "tilo-activate";
/// The freedesktop action key a click (on the body) invokes.
const DEFAULT_ACTION: &str = "default";

/// Activation URIs by freedesktop notification id, for its `ActionInvoked`.
fn activation_ids() -> &'static Mutex<HashMap<u32, String>> {
    static IDS: OnceLock<Mutex<HashMap<u32, String>>> = OnceLock::new();
    IDS.get_or_init(Mutex::default)
}

/// Listen for both services' `ActionInvoked` on `connection`, once.
fn listen_for_clicks(connection: &Connection) {
    static LISTENING: std::sync::Once = std::sync::Once::new();
    LISTENING.call_once(|| {
        let portal = connection.clone();
        let spawned = tilo_rt::spawn_thread("tilo-notify-portal-clicks", move || {
            let (dest, path, iface) = PORTAL;
            let Ok(proxy) = zbus::blocking::Proxy::new(&portal, dest, path, iface) else {
                return;
            };
            let Ok(signals) = proxy.receive_signal("ActionInvoked") else {
                return;
            };
            for signal in signals {
                let Ok((_app, _id, action, parameter)) =
                    signal
                        .body()
                        .deserialize::<(String, String, String, Vec<zbus::zvariant::OwnedValue>)>()
                else {
                    continue;
                };
                let uri = parameter
                    .first()
                    .and_then(|value| String::try_from(value.try_clone().ok()?).ok());
                if action == PORTAL_ACTION
                    && let Some(uri) = uri
                {
                    crate::activated(&uri);
                }
            }
        });
        let freedesktop = connection.clone();
        let spawned = spawned.and_then(|_| {
            tilo_rt::spawn_thread("tilo-notify-clicks", move || {
                let (dest, path, iface) = FREEDESKTOP;
                let Ok(proxy) = zbus::blocking::Proxy::new(&freedesktop, dest, path, iface) else {
                    return;
                };
                let Ok(signals) = proxy.receive_signal("ActionInvoked") else {
                    return;
                };
                for signal in signals {
                    let Ok((id, action)) = signal.body().deserialize::<(u32, String)>() else {
                        continue;
                    };
                    let uri = activation_ids()
                        .lock()
                        .ok()
                        .and_then(|ids| ids.get(&id).cloned());
                    if action == DEFAULT_ACTION
                        && let Some(uri) = uri
                    {
                        crate::activated(&uri);
                    }
                }
            })
        });
        if let Err(error) = spawned {
            tracing::debug!(%error, "cannot listen for notification clicks");
        }
    });
}

fn portal_add(connection: &Connection, n: &Notification) -> zbus::Result<()> {
    let mut content: HashMap<&str, Value<'_>> = HashMap::new();
    content.insert("title", Value::from(n.title.as_str()));
    content.insert("body", Value::from(n.body.as_str()));
    if let Some(uri) = &n.activation {
        content.insert("default-action", Value::from(PORTAL_ACTION));
        content.insert("default-action-target", Value::from(uri.as_str()));
    }
    let (dest, path, iface) = PORTAL;
    connection
        .call_method(
            Some(dest),
            path,
            Some(iface),
            "AddNotification",
            &(n.tag.as_str(), content),
        )
        .map(drop)
}

fn portal_remove(connection: &Connection, tag: &str) -> zbus::Result<()> {
    let (dest, path, iface) = PORTAL;
    connection
        .call_method(Some(dest), path, Some(iface), "RemoveNotification", &(tag,))
        .map(drop)
}

fn freedesktop_notify(connection: &Connection, app: &str, n: &Notification) -> zbus::Result<()> {
    let replaces = ids()
        .lock()
        .ok()
        .and_then(|ids| ids.get(&n.tag).copied())
        .unwrap_or(0);
    let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
    // The specification has no warning level; only an error is critical.
    if n.severity == crate::Severity::Error {
        hints.insert("urgency", Value::U8(2));
    }
    let actions: Vec<&str> = if n.activation.is_some() {
        vec![DEFAULT_ACTION, n.action.as_deref().unwrap_or("Open")]
    } else {
        Vec::new()
    };
    let (dest, path, iface) = FREEDESKTOP;
    let reply = connection.call_method(
        Some(dest),
        path,
        Some(iface),
        "Notify",
        &(
            app,
            replaces,
            "",
            n.title.as_str(),
            n.body.as_str(),
            actions,
            hints,
            -1_i32,
        ),
    )?;
    let id: u32 = reply.body().deserialize()?;
    if let Ok(mut ids) = ids().lock() {
        ids.insert(n.tag.clone(), id);
    }
    if let (Some(uri), Ok(mut activations)) = (&n.activation, activation_ids().lock()) {
        activations.insert(id, uri.clone());
    }
    Ok(())
}

fn freedesktop_close(connection: &Connection, tag: &str) -> zbus::Result<()> {
    let Some(id) = ids().lock().ok().and_then(|mut ids| ids.remove(tag)) else {
        return Ok(());
    };
    let (dest, path, iface) = FREEDESKTOP;
    connection
        .call_method(Some(dest), path, Some(iface), "CloseNotification", &(id,))
        .map(drop)
}

/// Try the portal, else the freedesktop service, remembering which answered.
fn either(
    connection: &Connection,
    portal: impl FnOnce(&Connection) -> zbus::Result<()>,
    freedesktop: impl FnOnce(&Connection) -> zbus::Result<()>,
) -> Result<(), NotifyError> {
    let known = chosen().lock().ok().and_then(|chosen| *chosen);
    if known != Some(Service::Freedesktop) {
        match portal(connection) {
            Ok(()) => {
                remember(Service::Portal);
                return Ok(());
            }
            Err(error) if known.is_none() && missing(&error) => {}
            Err(error) => return Err(failed(error)),
        }
    }
    match freedesktop(connection) {
        Ok(()) => {
            remember(Service::Freedesktop);
            Ok(())
        }
        Err(error) if missing(&error) => Err(NotifyError::Unavailable),
        Err(error) => Err(failed(error)),
    }
}

fn remember(service: Service) {
    if let Ok(mut chosen) = chosen().lock() {
        *chosen = Some(service);
    }
}

fn schedule_notification(
    notification: &Notification,
    at: SystemTime,
    calendar: Vec<(u8, u8, u8, i64, String)>,
) -> Result<(), NotifyError> {
    crate::validate(notification)?;
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .ok_or(NotifyError::Invalid("invalid notification deadline"))?;
    let args = (
        notification.tag.clone(),
        notification.title.clone(),
        notification.body.clone(),
        notification.activation.clone().unwrap_or_default(),
        millis,
        calendar,
    );
    on_bus(move |connection| {
        let (dest, path, iface) = FREEDESKTOP;
        connection
            .call_method(
                Some(dest),
                path,
                Some(iface),
                "XTiloScheduleNotification",
                &args,
            )
            .map(drop)
            .map_err(schedule_error)
    })
}

fn decode_deadline(values: &[i64]) -> Result<Option<SystemTime>, NotifyError> {
    match values {
        [] => Ok(None),
        [millis] => {
            let millis = u64::try_from(*millis)
                .map_err(|_| NotifyError::Invalid("invalid alarm deadline"))?;
            SystemTime::UNIX_EPOCH
                .checked_add(Duration::from_millis(millis))
                .map(Some)
                .ok_or(NotifyError::Invalid("invalid alarm deadline"))
        }
        _ => Err(NotifyError::Invalid("expected at most one alarm deadline")),
    }
}

fn schedule_error(error: zbus::Error) -> NotifyError {
    if missing(&error) {
        NotifyError::Unsupported(crate::Unsupported {
            feature: crate::Feature::Schedule,
            reason: "This desktop does not provide Tilo's durable notification scheduler.",
        })
    } else {
        failed(error)
    }
}

impl Notifier for SessionNotifier {
    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        schedule_notification(notification, at, Vec::new())
    }

    fn schedule_alarm(
        &self,
        notification: &Notification,
        at: SystemTime,
        schedule: AlarmSchedule,
    ) -> Result<(), NotifyError> {
        let alarm = schedule.alarm;
        if !alarm.valid() {
            return Err(NotifyError::Invalid("invalid alarm time or weekdays"));
        }
        schedule_notification(
            notification,
            at,
            vec![(
                alarm.hour,
                alarm.minute,
                alarm.weekdays,
                schedule.enabled_at_ms,
                schedule
                    .last_rung
                    .map(|date| date.to_string())
                    .unwrap_or_default(),
            )],
        )
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        on_bus(|connection| {
            let (dest, path, iface) = FREEDESKTOP;
            let response = connection
                .call_method(
                    Some(dest),
                    path,
                    Some(iface),
                    "XTiloScheduledNotifications",
                    &(),
                )
                .map_err(schedule_error)?;
            let rows: Vec<(String, i64)> = response.body().deserialize().map_err(failed)?;
            rows.into_iter()
                .map(|(tag, at)| {
                    let millis = u64::try_from(at)
                        .map_err(|_| NotifyError::Invalid("invalid stored deadline"))?;
                    let at = SystemTime::UNIX_EPOCH
                        .checked_add(Duration::from_millis(millis))
                        .ok_or(NotifyError::Invalid("invalid stored deadline"))?;
                    Ok(Scheduled { tag, at })
                })
                .collect()
        })
    }

    fn alarm_states(&self) -> Result<Vec<AlarmState>, NotifyError> {
        on_bus(|connection| {
            let (dest, path, iface) = FREEDESKTOP;
            let response = connection
                .call_method(Some(dest), path, Some(iface), "XTiloScheduledAlarms", &())
                .map_err(schedule_error)?;
            let rows: Vec<AlarmStateWire> = response.body().deserialize().map_err(failed)?;
            rows.into_iter()
                .map(
                    |(tag, (hour, minute, weekdays, enabled_at_ms), next, last, date)| {
                        let last_rung = if date.is_empty() {
                            None
                        } else {
                            Some(
                                chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(
                                    |_| NotifyError::Invalid("invalid delivered alarm date"),
                                )?,
                            )
                        };
                        Ok(AlarmState {
                            tag,
                            alarm: CalendarAlarm {
                                hour,
                                minute,
                                weekdays,
                            },
                            enabled_at_ms,
                            next_at: decode_deadline(&next)?,
                            last_at: decode_deadline(&last)?,
                            last_rung,
                        })
                    },
                )
                .collect()
        })
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        let tag = tag.to_owned();
        on_bus(move |connection| {
            let (dest, path, iface) = FREEDESKTOP;
            connection
                .call_method(
                    Some(dest),
                    path,
                    Some(iface),
                    "XTiloCancelScheduledNotification",
                    &(tag,),
                )
                .map(drop)
                .map_err(schedule_error)
        })
    }

    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        let notification = notification.clone();
        let app = self.app_id.clone();
        on_bus(move |connection| {
            if notification.activation.is_some() {
                listen_for_clicks(connection);
            }
            either(
                connection,
                |c| portal_add(c, &notification),
                |c| freedesktop_notify(c, &app, &notification),
            )
        })
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        let tag = tag.to_owned();
        on_bus(move |connection| {
            either(
                connection,
                |c| portal_remove(c, &tag),
                |c| freedesktop_close(c, &tag),
            )
        })
    }
}

/// The Tilo shell's extension of the notification service: it verifies the
/// sender's process ancestry, so only the app it launched is believed.
pub(crate) fn publish_priority_contacts(
    app_id: &str,
    contacts: &[PriorityContact],
) -> Result<(), NotifyError> {
    if std::env::var("TILO_PACKAGE_ID").as_deref() != Ok(app_id) {
        return Err(NotifyError::Unsupported(crate::Unsupported {
            feature: crate::Feature::PriorityContacts,
            reason: NOT_TILO,
        }));
    }
    let wire: Vec<(String, String, Vec<String>)> = contacts
        .iter()
        .map(|c| (c.id.clone(), c.name.clone(), c.endpoints.clone()))
        .collect();
    on_bus(move |connection| {
        let (dest, path, iface) = FREEDESKTOP;
        connection
            .call_method(
                Some(dest),
                path,
                Some(iface),
                "XTiloReplacePriorityContacts",
                &(wire,),
            )
            .map(drop)
            .map_err(|error| {
                if missing(&error) {
                    NotifyError::Unavailable
                } else {
                    failed(error)
                }
            })
    })
}
