//! The live notifications as resources: one per notification, read as its
//! data, with the actions its banner offers, carried out exactly as the
//! banner would.

use serde_json::{Map, Value};
use tilo_actions::{ActionAddress, ActionOutput, DoorError, ResourcePresentation, ResourceSpec};
use tilo_resource::Effect;
use tilo_service::{Service, Started, decode, offer, reading};

use crate::{INLINE_REPLY, Notification, Notifications};

/// The resource the live notifications are published under.
pub const NOTIFICATIONS_RESOURCE: &str = "notifications";
/// The kind of [`NOTIFICATIONS_RESOURCE`].
pub const NOTIFICATIONS_KIND: &str = "session.notifications";
/// The kind of each notification's resource.
pub const NOTIFICATION_KIND: &str = "session.notification";

/// What a notification's resource id starts with, before its id.
const NOTIFICATION_PREFIX: &str = "notification:";

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
        ]
    }

    /// The root reads as every live notification, oldest first; each
    /// notification as its own data, without its pixels, which travel only
    /// in the drained [`crate::Arrival`].
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == NOTIFICATIONS_RESOURCE {
            return Some(reading(&self.live()));
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
        let effect: NotificationEffect = decode(&self.resources(), resource, action, arguments)?;
        let unknown = || DoorError::Unknown(ActionAddress::new(resource, action));
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
            Ok(Started::Done(ActionOutput::default()))
        } else {
            Err(unknown())
        }
    }

    fn poll(&mut self) -> bool {
        self.take_changed()
    }
}
