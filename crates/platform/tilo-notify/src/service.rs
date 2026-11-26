//! The notification service an app's session serves it: an app shows, replaces, withdraws and
//! schedules notifications over the connection its launcher opened for it, and what the person
//! does with one comes back on the same connection.
//!
//! An app runs in a process with no bus of its own, so the session is where notifications go: it
//! pushes them into the one notifications daemon every app's and the desktop's own notices reach,
//! attributed to the app the connection was opened for ([`crate::service::FD_ENV`]). The wire is
//! one length-prefixed JSON message per frame, the framing every socket in the tree uses: the app
//! sends [`Request`]s, each naming an id, and the session answers the id with an [`Answer`]; what
//! a notification's actions and dismissals mean for the app arrive as [`Request::Activated`]'s
//! counterpart, [`Message::Activated`] and [`Message::Closed`].
//!
//! [`NotifySide`] is whatever answers on the session's end: the session's notifications service.
//! [`ServiceNotifier`] is the app's end, and the [`Notifier`] a Rust app's process uses when its
//! launcher hands it a connection.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tilo_socket::Stream;
use wire_frame::Framing;

use crate::{
    AlarmSchedule, AlarmState, CalendarAlarm, CloseReason, Feature, Notification, Notifier,
    NotifyError, PriorityContact, Scheduled, SystemTime, Unsupported,
};

/// The descriptor the launcher hands an app it starts: the app's end of the connection served
/// here.
pub const FD_ENV: &str = "TILO_SVC_NOTIFY_FD";

/// One message: its JSON, behind the length prefix.
const FRAME: Framing = Framing::little_endian(1024 * 1024);

/// How long a call waits for the session's answer: the session pushes into the notifications
/// daemon, which never asks the person, so an answer that does not come is a failure rather than
/// a hang.
const WAIT: Duration = Duration::from_secs(30);

/// An alarm as an app hands it over: its local wall-clock time, when the person turned it on, and
/// the date it last rang, which the session reads as `YYYY-MM-DD`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmRequest {
    pub hour: u8,
    pub minute: u8,
    /// Monday is bit zero; none rings once.
    pub weekdays: u8,
    pub enabled_at_ms: i64,
    pub last_rung: Option<String>,
}

impl AlarmRequest {
    /// The request an app's [`AlarmSchedule`] makes; `None` when its delivered date is not one
    /// the session reads.
    #[must_use]
    pub fn of(schedule: &AlarmSchedule) -> Option<Self> {
        Some(Self {
            hour: schedule.alarm.hour,
            minute: schedule.alarm.minute,
            weekdays: schedule.alarm.weekdays,
            enabled_at_ms: schedule.enabled_at_ms,
            last_rung: schedule
                .last_rung
                .map(|date| date.to_string())
                .filter(|date| !date.is_empty()),
        })
    }

    /// This request as an app's own [`AlarmSchedule`].
    #[must_use]
    pub fn schedule(&self) -> AlarmSchedule {
        AlarmSchedule {
            alarm: CalendarAlarm {
                hour: self.hour,
                minute: self.minute,
                weekdays: self.weekdays,
            },
            enabled_at_ms: self.enabled_at_ms,
            last_rung: self
                .last_rung
                .as_deref()
                .and_then(|date| jiff::civil::Date::strptime("%Y-%m-%d", date).ok()),
        }
    }
}

/// One call an app makes. Each names an `id` its [`Answer`] comes back under; the id is the
/// app's own and means nothing to the session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Show {
        id: u64,
        notification: Notification,
    },
    Withdraw {
        id: u64,
        tag: String,
    },
    Schedule {
        id: u64,
        notification: Notification,
        at: SystemTime,
    },
    ScheduleAlarm {
        id: u64,
        notification: Notification,
        at: SystemTime,
        alarm: AlarmRequest,
    },
    Scheduled {
        id: u64,
    },
    AlarmStates {
        id: u64,
    },
    CancelScheduled {
        id: u64,
        tag: String,
    },
    /// Replace the people whose messages break through focus and Do Not Disturb with the ones
    /// this app knows of.
    PriorityContacts {
        id: u64,
        contacts: Vec<PriorityContact>,
    },
}

impl Request {
    /// The call's id, which its answer names.
    #[must_use]
    pub const fn id(&self) -> u64 {
        match self {
            Self::Show { id, .. }
            | Self::Withdraw { id, .. }
            | Self::Schedule { id, .. }
            | Self::ScheduleAlarm { id, .. }
            | Self::Scheduled { id }
            | Self::AlarmStates { id }
            | Self::CancelScheduled { id, .. }
            | Self::PriorityContacts { id, .. } => *id,
        }
    }
}

/// One alarm as the session reports it: its state and what it rang.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlarmStateWire {
    pub tag: String,
    pub alarm: AlarmRequest,
    pub next_at: Option<SystemTime>,
    pub last_at: Option<SystemTime>,
}

impl AlarmStateWire {
    /// The state an [`AlarmState`] reports.
    #[must_use]
    pub fn of(state: &AlarmState) -> Self {
        Self {
            tag: state.tag.clone(),
            alarm: AlarmRequest {
                hour: state.alarm.hour,
                minute: state.alarm.minute,
                weekdays: state.alarm.weekdays,
                enabled_at_ms: state.enabled_at_ms,
                last_rung: state.last_rung.map(|date| date.to_string()),
            },
            next_at: state.next_at,
            last_at: state.last_at,
        }
    }

    /// This wire state as an [`AlarmState`].
    #[must_use]
    pub fn state(&self) -> AlarmState {
        AlarmState {
            tag: self.tag.clone(),
            alarm: self.alarm.schedule().alarm,
            enabled_at_ms: self.alarm.enabled_at_ms,
            next_at: self.next_at,
            last_at: self.last_at,
            last_rung: self
                .alarm
                .last_rung
                .as_deref()
                .and_then(|date| jiff::civil::Date::strptime("%Y-%m-%d", date).ok()),
        }
    }
}

/// What the session answered a call with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Answer {
    Done,
    Scheduled(Vec<Scheduled>),
    Alarms(Vec<AlarmStateWire>),
    /// The session would not do it, and why.
    Refused(Refusal),
}

/// Why the session refused a call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// This host cannot do it: scheduling, the one feature the session may lack.
    Unsupported,
    Invalid(String),
    Failed(String),
    /// This connection reaches no notification service.
    Unavailable,
}

impl Refusal {
    /// The refusal as the error an app's [`Notifier`] reports.
    #[must_use]
    pub fn error(&self) -> NotifyError {
        match self {
            Self::Unsupported => NotifyError::Unsupported(Unsupported::of(Feature::Schedule)),
            Self::Invalid(reason) | Self::Failed(reason) => NotifyError::Failed(reason.clone()),
            Self::Unavailable => NotifyError::Unavailable,
        }
    }
}

/// One message on its way to the app: the answer to a call, and what the person did with a
/// notification the app showed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Message {
    Answer {
        id: u64,
        answer: Answer,
    },
    /// The person chose the notification, or one of its buttons: the URI it was shown with.
    Activated {
        uri: String,
    },
    /// The notification is gone, and why: the person dismissed it, it expired, or it was closed.
    Closed {
        tag: String,
        reason: CloseReason,
    },
}

/// What answers on the session's end of an app's notification connection: the session's
/// notifications daemon, attributed to the app the connection is for.
pub trait NotifySide: Send + Sync {
    /// The side of this connection that tells the app what happened to its notifications, before
    /// the first request. The connection closes with the app, and so does the side.
    fn attached(&self, channel: Arc<Channel>) {
        drop(channel);
    }

    /// Show `notification`, or replace the app's notification with its tag.
    fn show(&self, notification: &Notification) -> Result<(), Refusal>;

    /// Withdraw the app's notification with `tag`.
    fn withdraw(&self, tag: &str) -> Result<(), Refusal>;

    /// Hand `notification` over for `at`, with `alarm` for one that rings at a wall-clock time.
    fn schedule(
        &self,
        notification: &Notification,
        at: SystemTime,
        alarm: Option<&AlarmRequest>,
    ) -> Result<(), Refusal>;

    /// The app's deadlines still to fall due.
    fn scheduled(&self) -> Result<Vec<Scheduled>, Refusal>;

    /// The app's alarms, and what the session delivered of them.
    fn alarm_states(&self) -> Result<Vec<AlarmState>, Refusal>;

    /// Cancel the app's deadline with `tag`.
    fn cancel_scheduled(&self, tag: &str) -> Result<(), Refusal>;

    /// Replace the people whose messages may break through focus and Do Not Disturb with
    /// `contacts`, as this app knows them.
    /// A side that keeps no priority contacts.
    fn priority_contacts(&self, _contacts: &[PriorityContact]) -> Result<(), Refusal> {
        Err(Refusal::Unsupported)
    }
}

/// The session's end of one app's connection, for what the person did with its notifications.
pub struct Channel {
    writer: Mutex<Stream>,
}

impl Channel {
    /// The person chose a notification: `uri` is what the app showed it with.
    pub fn activated(&self, uri: &str) {
        self.send(&Message::Activated {
            uri: uri.to_owned(),
        });
    }

    /// The app's notification with `tag` is gone, for `reason`.
    pub fn closed(&self, tag: &str, reason: CloseReason) {
        self.send(&Message::Closed {
            tag: tag.to_owned(),
            reason,
        });
    }

    fn answer(&self, id: u64, answer: Answer) {
        self.send(&Message::Answer { id, answer });
    }

    fn send(&self, message: &Message) {
        let Ok(bytes) = serde_json::to_vec(message) else {
            return;
        };
        let Ok(mut writer) = self.writer.lock() else {
            return;
        };
        if FRAME.write(&mut *writer, &bytes).is_ok() {
            let _ = writer.flush();
        }
    }
}

/// Serve an app's notification calls on `stream` until it closes the connection.
pub fn serve(stream: Stream, side: &dyn NotifySide) {
    let Ok(writer) = stream.try_clone() else {
        return;
    };
    let channel = Arc::new(Channel {
        writer: Mutex::new(writer),
    });
    side.attached(Arc::clone(&channel));
    let mut reader = stream;
    while let Ok(Some(frame)) = FRAME.read(&mut reader) {
        let Ok(request) = serde_json::from_slice::<Request>(&frame) else {
            return;
        };
        let id = request.id();
        channel.answer(id, answer(side, request));
    }
}

/// What `side` answers `request` with.
fn answer(side: &dyn NotifySide, request: Request) -> Answer {
    let done = match request {
        Request::Show { notification, .. } => crate::validate(&notification)
            .map_err(refused)
            .and_then(|()| side.show(&notification)),
        Request::Withdraw { tag, .. } => crate::validate_tag(&tag)
            .map_err(refused)
            .and_then(|()| side.withdraw(&tag)),
        Request::Schedule {
            notification, at, ..
        } => crate::validate(&notification)
            .map_err(refused)
            .and_then(|()| side.schedule(&notification, at, None)),
        Request::ScheduleAlarm {
            notification,
            at,
            alarm,
            ..
        } => crate::validate(&notification)
            .map_err(refused)
            .and_then(|()| side.schedule(&notification, at, Some(&alarm))),
        Request::Scheduled { .. } => {
            return match side.scheduled() {
                Ok(rows) => Answer::Scheduled(rows),
                Err(refusal) => Answer::Refused(refusal),
            };
        }
        Request::AlarmStates { .. } => {
            return match side.alarm_states() {
                Ok(states) => Answer::Alarms(states.iter().map(AlarmStateWire::of).collect()),
                Err(refusal) => Answer::Refused(refusal),
            };
        }
        Request::CancelScheduled { tag, .. } => crate::validate_tag(&tag)
            .map_err(refused)
            .and_then(|()| side.cancel_scheduled(&tag)),
        Request::PriorityContacts { contacts, .. } => side.priority_contacts(&contacts),
    };
    match done {
        Ok(()) => Answer::Done,
        Err(refusal) => Answer::Refused(refusal),
    }
}

/// A malformed request, as the refusal an app is told.
fn refused(error: NotifyError) -> Refusal {
    match error {
        NotifyError::Invalid(reason) => Refusal::Invalid(reason.to_owned()),
        NotifyError::Unsupported(_) => Refusal::Unsupported,
        NotifyError::Failed(reason) => Refusal::Failed(reason),
        NotifyError::Unavailable => Refusal::Unavailable,
    }
}

/// This process's connection to its session's notification service, taken once: what the system
/// notifier and an app's priority contacts both use. `None` when the launcher handed none, or it
/// is already closed.
fn inherited() -> Option<&'static Arc<Connection>> {
    static INHERITED: std::sync::OnceLock<Option<Arc<Connection>>> = std::sync::OnceLock::new();
    INHERITED
        .get_or_init(|| {
            let fd = std::env::var(FD_ENV).ok()?.parse::<i32>().ok()?;
            if fd < 0 {
                return None;
            }
            // SAFETY: the launcher passes this descriptor to this process, and it is this
            // process's from here: nothing else in the app holds the number.
            let stream = Stream::from(unsafe {
                <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd)
            });
            Connection::start(stream)
        })
        .as_ref()
}

/// Whether the launcher handed this process a notification service.
#[must_use]
pub fn inherited_service() -> bool {
    inherited().is_some()
}

/// Replace the people whose messages may break through focus and Do Not Disturb with `contacts`,
/// as this app knows them, over the connection its session opened for it.
///
/// # Errors
/// This process was handed no notification service, or the session refused the list.
pub fn publish_priority_contacts(contacts: &[PriorityContact]) -> Result<(), NotifyError> {
    let Some(connection) = inherited() else {
        return Err(NotifyError::Unavailable);
    };
    let notifier = ServiceNotifier {
        connection: Arc::clone(connection),
    };
    let contacts = contacts.to_vec();
    ServiceNotifier::did(notifier.ask(move |id| Request::PriorityContacts { id, contacts })?)
}

/// An app's end of its notification connection: a Rust app's [`Notifier`], the same service a
/// tilolang instance reaches.
pub struct ServiceNotifier {
    connection: Arc<Connection>,
}

struct Connection {
    writer: Mutex<Stream>,
    /// The calls waiting for their answers, by the id they named.
    waiting: Mutex<HashMap<u64, Sender<Answer>>>,
    next: AtomicU64,
}

impl ServiceNotifier {
    /// The notifier over the connection the launcher opened for this app, named by [`FD_ENV`];
    /// `None` when this process was handed none.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        Some(Self {
            connection: Arc::clone(inherited()?),
        })
    }

    /// The notifier over `stream`, the connection the session opened for the app; `None` when it
    /// cannot be read and written beside this end.
    #[must_use]
    pub fn connect(stream: Stream) -> Option<Self> {
        Some(Self {
            connection: Connection::start(stream)?,
        })
    }

    /// Ask the session `request`, built with the id it answers under.
    fn ask(&self, request: impl FnOnce(u64) -> Request) -> Result<Answer, NotifyError> {
        let id = self.connection.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = mpsc::channel();
        self.connection
            .waiting
            .lock()
            .map_err(|_| NotifyError::Unavailable)?
            .insert(id, reply);
        let sent = serde_json::to_vec(&request(id)).ok().and_then(|bytes| {
            let mut writer = self.connection.writer.lock().ok()?;
            FRAME.write(&mut *writer, &bytes).ok()?;
            writer.flush().ok()
        });
        if sent.is_none() {
            if let Ok(mut waiting) = self.connection.waiting.lock() {
                waiting.remove(&id);
            }
            return Err(NotifyError::Unavailable);
        }
        answer.recv_timeout(WAIT).map_err(|_| {
            if let Ok(mut waiting) = self.connection.waiting.lock() {
                waiting.remove(&id);
            }
            NotifyError::Failed("the notification service did not answer".into())
        })
    }

    /// `answer` as a result of doing it, or the refusal.
    fn did(answer: Answer) -> Result<(), NotifyError> {
        match answer {
            Answer::Done => Ok(()),
            Answer::Refused(refusal) => Err(refusal.error()),
            Answer::Scheduled(_) | Answer::Alarms(_) => Err(NotifyError::Failed(
                "the notification service answered the wrong call".into(),
            )),
        }
    }
}

impl Connection {
    /// The connection over `stream`, with the thread that takes what the session says on it.
    fn start(stream: Stream) -> Option<Arc<Self>> {
        let reader = stream.try_clone().ok()?;
        let connection = Arc::new(Self {
            writer: Mutex::new(stream),
            waiting: Mutex::default(),
            next: AtomicU64::new(1),
        });
        let following = Arc::clone(&connection);
        std::thread::Builder::new()
            .name("tilo-notify-service".into())
            .spawn(move || following.listen(reader))
            .ok()?;
        Some(connection)
    }

    /// Take the session's answers and what happened to the app's notifications, until the
    /// connection closes.
    fn listen(&self, mut reader: Stream) {
        while let Ok(Some(frame)) = FRAME.read(&mut reader) {
            let Ok(message) = serde_json::from_slice::<Message>(&frame) else {
                return;
            };
            match message {
                Message::Answer { id, answer } => {
                    let waiting = self
                        .waiting
                        .lock()
                        .ok()
                        .and_then(|mut waiting| waiting.remove(&id));
                    if let Some(reply) = waiting {
                        let _ = reply.send(answer);
                    }
                }
                Message::Activated { uri } => crate::activated(&uri),
                Message::Closed { tag, reason } => crate::closed(&tag, reason),
            }
        }
    }
}

impl Notifier for ServiceNotifier {
    fn show(&self, notification: &Notification) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        let notification = notification.clone();
        Self::did(self.ask(move |id| Request::Show { id, notification })?)
    }

    fn withdraw(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        let tag = tag.to_owned();
        Self::did(self.ask(move |id| Request::Withdraw { id, tag })?)
    }

    fn schedule(&self, notification: &Notification, at: SystemTime) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        let notification = notification.clone();
        Self::did(self.ask(move |id| Request::Schedule {
            id,
            notification,
            at,
        })?)
    }

    fn schedule_alarm(
        &self,
        notification: &Notification,
        at: SystemTime,
        schedule: AlarmSchedule,
    ) -> Result<(), NotifyError> {
        crate::validate(notification)?;
        if !schedule.alarm.valid() {
            return Err(NotifyError::Invalid("invalid alarm time or weekdays"));
        }
        let alarm = AlarmRequest::of(&schedule)
            .ok_or(NotifyError::Invalid("invalid delivered alarm date"))?;
        let notification = notification.clone();
        Self::did(self.ask(move |id| Request::ScheduleAlarm {
            id,
            notification,
            at,
            alarm,
        })?)
    }

    fn scheduled(&self) -> Result<Vec<Scheduled>, NotifyError> {
        match self.ask(|id| Request::Scheduled { id })? {
            Answer::Scheduled(rows) => Ok(rows),
            Answer::Refused(refusal) => Err(refusal.error()),
            Answer::Done | Answer::Alarms(_) => Err(NotifyError::Failed(
                "the notification service answered the wrong call".into(),
            )),
        }
    }

    fn alarm_states(&self) -> Result<Vec<AlarmState>, NotifyError> {
        match self.ask(|id| Request::AlarmStates { id })? {
            Answer::Alarms(states) => Ok(states.iter().map(AlarmStateWire::state).collect()),
            Answer::Refused(refusal) => Err(refusal.error()),
            Answer::Done | Answer::Scheduled(_) => Err(NotifyError::Failed(
                "the notification service answered the wrong call".into(),
            )),
        }
    }

    fn cancel_scheduled(&self, tag: &str) -> Result<(), NotifyError> {
        crate::validate_tag(tag)?;
        let tag = tag.to_owned();
        Self::did(self.ask(move |id| Request::CancelScheduled { id, tag })?)
    }
}
