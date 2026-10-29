//! The live notifications as resources: one per notification, read as its
//! data, with the actions its banner offers, carried out exactly as the
//! banner would. Beside them, what outlives a notification: the deadlines
//! apps handed over and the person's priority contacts, each with the
//! actions that change them.

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::priority::{ContactProjection, PriorityContact};
use crate::scheduled::{AlarmRequest, Deadline, Owner, ScheduleRequest};
use crate::{INLINE_REPLY, Notification, Notifications};

/// The resource the live notifications are published under.
pub const NOTIFICATIONS_RESOURCE: &str = "notifications";
/// The kind of [`NOTIFICATIONS_RESOURCE`].
pub const NOTIFICATIONS_KIND: &str = "session.notifications";
/// The kind of each notification's resource.
pub const NOTIFICATION_KIND: &str = "session.notification";

/// What a notification's resource id starts with, before its id.
const NOTIFICATION_PREFIX: &str = "notification:";

/// The resource the deadlines apps handed over are published under.
pub const SCHEDULES_RESOURCE: &str = "notification-schedules";
/// The kind of [`SCHEDULES_RESOURCE`].
pub const SCHEDULES_KIND: &str = "session.notification-schedules";
/// The kind of each deadline's resource.
pub const SCHEDULE_KIND: &str = "session.notification-schedule";
const SCHEDULE_PREFIX: &str = "notification-schedule:";

/// The resource the priority contacts are published under.
pub const PRIORITY_CONTACTS_RESOURCE: &str = "priority-contacts";
/// The kind of [`PRIORITY_CONTACTS_RESOURCE`].
pub const PRIORITY_CONTACTS_KIND: &str = "session.priority-contacts";
/// The kind of each priority contact's resource.
pub const PRIORITY_CONTACT_KIND: &str = "session.priority-contact";
const PRIORITY_CONTACT_PREFIX: &str = "priority-contact:";

/// The resource id the deadline with `token` is published as.
#[must_use]
pub fn schedule_resource(token: &str) -> String {
    format!("{SCHEDULE_PREFIX}{token}")
}

/// The resource id priority contact `id` is published as.
#[must_use]
pub fn priority_contact_resource(id: &str) -> String {
    format!("{PRIORITY_CONTACT_PREFIX}{id}")
}

/// The resource id notification `id` is published as.
#[must_use]
pub fn notification_resource(id: u32) -> String {
    format!("{NOTIFICATION_PREFIX}{id}")
}

/// What a notification's resource offers: what its banner does.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notification")]
pub enum NotificationEffect {
    /// Close the notification
    #[effect(own_device, idempotent)]
    Close,
    /// Invoke one of the notification's actions
    #[effect(own_device)]
    Invoke {
        #[param("The key of one of the notification's actions")]
        action: String,
    },
    /// Reply to the notification
    #[effect(own_device)]
    Reply {
        #[param("The reply")]
        text: String,
    },
}

/// What the deadlines offer: a new one, for an app.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notification-schedules")]
pub enum SchedulesEffect {
    /// Schedule a notification
    #[effect(own_device)]
    Schedule {
        #[param("The app the notification is from, by its id")]
        app_id: String,
        #[param("The workspace the app runs in; left out, none")]
        workspace: Option<String>,
        #[param("Names it among the app's own; scheduling it again replaces it")]
        tag: String,
        #[param("The notification's title")]
        title: String,
        #[param("The notification's text")]
        body: Option<String>,
        #[param("The address the app opens at when the notification is chosen")]
        activation: Option<String>,
        #[param("When it is due, in milliseconds since 1970; for an alarm, its first time")]
        at_ms: i64,
        #[param("An alarm's hour, in local time; left out, it rings once at the time given")]
        alarm_hour: Option<u8>,
        #[param("An alarm's minute")]
        alarm_minute: Option<u8>,
        #[param(
            "The weekdays an alarm repeats on, Monday as 1, Tuesday as 2, Wednesday as 4 and so on; none rings once"
        )]
        alarm_weekdays: Option<u8>,
    },
}

/// What one deadline offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.notification-schedule")]
pub enum ScheduleEffect {
    /// Cancel the scheduled notification
    #[effect(own_device, idempotent)]
    Cancel,
    /// Dismiss the snoozed alarm
    #[effect(own_device, idempotent)]
    DismissSnooze,
}

/// What the priority contacts offer: another one.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.priority-contacts")]
pub enum PriorityContactsEffect {
    /// Mark as priority contact
    #[effect(own_device, idempotent)]
    Mark {
        #[param("The contact's id")]
        id: String,
        #[param("The contact's name")]
        name: String,
        #[param("The email addresses and phone numbers their messages come from")]
        endpoints: Vec<String>,
    },
}

/// What one priority contact offers.
#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "so.tilo.session.priority-contact")]
pub enum PriorityContactEffect {
    /// Unmark as priority contact
    #[effect(own_device, idempotent)]
    Unmark,
}

fn schedule(deadline: &Deadline) -> ResourceSpec {
    let mut published = offer::<ScheduleEffect>(ResourceSpec::new(
        schedule_resource(&deadline.token),
        SCHEDULE_KIND,
        deadline.title.clone(),
    ))
    .with_presentation(ResourcePresentation {
        detail: Some(bounded(&deadline.app_id, ResourcePresentation::MAX_DETAIL_BYTES).to_owned()),
        badge: None,
        preview: Some(deadline.body.as_str())
            .filter(|body| !body.is_empty())
            .map(|body| bounded(body, ResourcePresentation::MAX_PREVIEW_BYTES).to_owned()),
    });
    let snoozed = deadline.snoozed_until.is_some();
    published.retain_actions(|spec| spec.id != "dismiss-snooze" || snoozed);
    published
}

fn priority_contact(contact: &PriorityContact) -> ResourceSpec {
    let label = if contact.name.is_empty() {
        contact.id.clone()
    } else {
        contact.name.clone()
    };
    offer::<PriorityContactEffect>(ResourceSpec::new(
        priority_contact_resource(&contact.id),
        PRIORITY_CONTACT_KIND,
        label,
    ))
}

fn done() -> Started {
    Started::Done(ActionOutput::default())
}

/// The longest prefix of `text` within `max` bytes.
fn bounded(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    text.get(..end).unwrap_or_default()
}

/// The keys of `notification`'s actions a caller may invoke: all but the
/// inline reply, which takes its text through `reply`.
fn invocable(notification: &Notification) -> impl Iterator<Item = &str> {
    notification
        .actions
        .iter()
        .map(|(key, _)| key.as_str())
        .filter(|key| *key != INLINE_REPLY)
}

fn resource(notification: &Notification) -> ResourceSpec {
    let label = if notification.summary.is_empty() {
        notification.app_name.clone()
    } else {
        notification.summary.clone()
    };
    let presentation = ResourcePresentation {
        detail: Some(notification.app_name.as_str())
            .filter(|name| !name.is_empty())
            .map(|name| bounded(name, ResourcePresentation::MAX_DETAIL_BYTES).to_owned()),
        badge: None,
        preview: Some(notification.body.as_str())
            .filter(|body| !body.is_empty())
            .map(|body| bounded(body, ResourcePresentation::MAX_PREVIEW_BYTES).to_owned()),
    };
    let invokes = invocable(notification).next().is_some();
    let replies = notification.takes_reply();
    let mut published = offer::<NotificationEffect>(ResourceSpec::new(
        notification_resource(notification.id),
        NOTIFICATION_KIND,
        label,
    ))
    .with_presentation(presentation);
    published.retain_actions(|spec| match spec.id.as_str() {
        "invoke" => invokes,
        "reply" => replies,
        _ => true,
    });
    published
}

impl Service for Notifications {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new(NOTIFICATIONS_RESOURCE, NOTIFICATIONS_KIND, "Notifications")
                .with_children(self.live().iter().map(resource)),
            offer::<SchedulesEffect>(ResourceSpec::new(
                SCHEDULES_RESOURCE,
                SCHEDULES_KIND,
                "Scheduled notifications",
            ))
            .with_children(self.schedules().deadlines().iter().map(schedule)),
            offer::<PriorityContactsEffect>(ResourceSpec::new(
                PRIORITY_CONTACTS_RESOURCE,
                PRIORITY_CONTACTS_KIND,
                "Priority contacts",
            ))
            .with_children(
                self.priority_contacts()
                    .contacts()
                    .iter()
                    .map(priority_contact),
            ),
        ]
    }

    /// The root reads as every live notification, oldest first; each
    /// notification as its own data, without its pixels, which travel only
    /// in the drained [`crate::Arrival`]. The deadlines and the priority
    /// contacts read as their lists, each one as itself; a priority contact
    /// never with its endpoints.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == NOTIFICATIONS_RESOURCE {
            return Some(reading(&self.live()));
        }
        if resource == SCHEDULES_RESOURCE {
            return Some(reading(&self.schedules().deadlines()));
        }
        if resource == PRIORITY_CONTACTS_RESOURCE {
            return Some(reading(&self.priority_contacts().contacts()));
        }
        if let Some(token) = resource.strip_prefix(SCHEDULE_PREFIX) {
            return self
                .schedules()
                .deadlines()
                .into_iter()
                .find(|deadline| deadline.token == token)
                .map(|deadline| reading(&deadline));
        }
        if let Some(id) = resource.strip_prefix(PRIORITY_CONTACT_PREFIX) {
            return self
                .priority_contacts()
                .contacts()
                .into_iter()
                .find(|contact| contact.id == id)
                .map(|contact| reading(&contact));
        }
        let id = resource.strip_prefix(NOTIFICATION_PREFIX)?.parse().ok()?;
        self.notification(id)
            .map(|notification| reading(&notification))
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Started, DoorError> {
        let published = self.resources();
        let unknown = || DoorError::Unknown(ActionAddress::new(resource, action));
        let refused = |error: String| DoorError::InvalidArguments(error);
        if resource == SCHEDULES_RESOURCE {
            let SchedulesEffect::Schedule {
                app_id,
                workspace,
                tag,
                title,
                body,
                activation,
                at_ms,
                alarm_hour,
                alarm_minute,
                alarm_weekdays,
            } = decode(&published, resource, action, arguments)?;
            let owner = Owner::of(&crate::Caller { app_id, workspace });
            let alarm = alarm_hour.map(|hour| AlarmRequest {
                hour,
                minute: alarm_minute.unwrap_or(0),
                weekdays: alarm_weekdays.unwrap_or(0),
                enabled_at_ms: tilo_clock::unix_now_ms_i64(),
                last_rung: None,
            });
            self.schedules()
                .schedule(
                    &owner,
                    ScheduleRequest {
                        tag,
                        title,
                        body: body.unwrap_or_default(),
                        activation: activation.unwrap_or_default(),
                        at_ms,
                        alarm,
                    },
                )
                .map_err(refused)?;
            self.changed();
            return Ok(done());
        }
        if let Some(token) = resource.strip_prefix(SCHEDULE_PREFIX) {
            let effect: ScheduleEffect = decode(&published, resource, action, arguments)?;
            let found = match effect {
                ScheduleEffect::Cancel => self.schedules().cancel_token(token),
                ScheduleEffect::DismissSnooze => self
                    .schedules()
                    .dismiss_snooze(token, tilo_clock::unix_now_ms_i64()),
            }
            .map_err(refused)?;
            self.changed();
            return found.then(done).ok_or_else(unknown);
        }
        if resource == PRIORITY_CONTACTS_RESOURCE {
            let PriorityContactsEffect::Mark {
                id,
                name,
                endpoints,
            } = decode(&published, resource, action, arguments)?;
            self.priority_contacts()
                .mark(&ContactProjection {
                    id,
                    name,
                    endpoints,
                })
                .map_err(refused)?;
            self.changed();
            return Ok(done());
        }
        if let Some(id) = resource.strip_prefix(PRIORITY_CONTACT_PREFIX) {
            let PriorityContactEffect::Unmark = decode(&published, resource, action, arguments)?;
            let found = self.priority_contacts().unmark(id).map_err(refused)?;
            self.changed();
            return found.then(done).ok_or_else(unknown);
        }
        let effect: NotificationEffect = decode(&published, resource, action, arguments)?;
        let id: u32 = resource
            .strip_prefix(NOTIFICATION_PREFIX)
            .and_then(|id| id.parse().ok())
            .ok_or_else(unknown)?;
        let notification = self.notification(id).ok_or_else(unknown)?;
        match effect {
            NotificationEffect::Close => {}
            NotificationEffect::Invoke { action: key } => {
                if !invocable(&notification).any(|offered| offered == key) {
                    return Err(DoorError::InvalidArguments(key));
                }
                self.action_invoked(id, &key, self.mint());
            }
            NotificationEffect::Reply { text } => self.replied(id, &text),
        }
        // The banner closes once the person acted on it; the session takes
        // it away when it drains the close.
        if self.dismiss(id) {
            Ok(done())
        } else {
            Err(unknown())
        }
    }

    fn poll(&mut self) -> bool {
        self.take_changed()
    }
}
