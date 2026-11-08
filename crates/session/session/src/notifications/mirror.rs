//! Notifications that continue across the person's own devices: what this device shows its others
//! of what it was told, and what they told it.
//! A notification an app sent on this device is projected to each of the person's devices that is
//! online, under an id this session minted ([`ContinuityNotificationId`]): another device shows it
//! as its own banner and centre row, named after this device. The projection carries what the
//! person's policy lets leave (the body only with previews on) and the actions the app offered, and
//! it stops being shown once its window on the other device runs out or the person here is done
//! with it, which retires every copy.
//! What the person does with a mirrored notification goes back to the device that sent it, through
//! the same route it came on: dismissing it closes the app's notification there, and choosing one
//! of its actions runs that action there. A device's ids name only what it projected, so nothing
//! here answers a command for a notification it did not send.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use tilo_desktop_surface::notifications::{
    ContinuityNotificationId, Notification, NotificationSeverity, RemoteNotificationRoute,
};
use tilo_device_fabric::link::{FabricLink as _, SharedLink};
use tilo_device_fabric::notification::{
    ContinuityNotificationProjection, MAX_CONTINUITY_NOTIFICATION_TTL_MS,
};
use tilo_device_fabric::{DeviceId, PersonalFabricAction};

/// The most projections one session keeps bookkeeping for: what fits on the other devices' desks
/// and centres anyway.
const MAX_PROJECTED: usize = 128;

/// One notification this device projected to the person's other devices.
struct Projected {
    id: ContinuityNotificationId,
    /// The desk's id for the notification here.
    log_id: u64,
    /// The actions the app offered, which only those may be invoked on.
    action_keys: Vec<String>,
    /// When the projection stops being shown on the other devices.
    expires_unix_ms: u64,
}

#[derive(Default)]
struct State {
    /// This session's epoch: with it, an id minted now never names a notification an earlier run of
    /// this device projected.
    epoch: [u8; 16],
    /// What this device projected, by the daemon notification it came from.
    out: HashMap<u32, Projected>,
    /// The daemon notification each projection came from.
    by_id: HashMap<ContinuityNotificationId, u32>,
    /// What another of the person's devices projected here, by the desk's id,
    /// so a choice on it is answered where it came from.
    shown: HashMap<u64, RemoteNotificationRoute>,
}

/// This device's side of the person's other devices' notifications.
pub(crate) struct Mirroring {
    /// The person's devices: where this device's notifications go, and the door what they do with
    /// one comes back through. `None` while this session is no certified device of theirs.
    link: RefCell<Option<SharedLink>>,
    state: RefCell<State>,
}

impl Mirroring {
    pub(crate) fn new() -> Rc<Self> {
        Rc::new(Self {
            link: RefCell::new(None),
            state: RefCell::new(State {
                epoch: tilo_device_fabric::random::nonzero::<16>(),
                ..State::default()
            }),
        })
    }

    /// Project this device's notifications to `link` 's devices from now on.
    pub(crate) fn attach(&self, link: SharedLink) {
        *self.link.borrow_mut() = Some(link);
    }

    /// The person's devices as this device holds them, when this session takes part as one of them
    /// and the person lets notifications continue.
    fn link(&self) -> Option<SharedLink> {
        let link = self.link.borrow().clone()?;
        link.notifications_permitted().then_some(link)
    }

    /// Show the notification app `app` sent here on the person's other devices, `notification`
    /// being the one the desk shows: its id, what its actions are, and when its banner goes.
    /// `previews` is whether this person lets a notification's text leave this device.
    /// Returns the projection's id; `None` when this session reaches no other device of theirs.
    pub(crate) fn project(
        &self,
        daemon_id: u32,
        app: (&str, &str),
        notification: &Notification,
        previews: bool,
        now_unix_ms: u64,
    ) -> Option<ContinuityNotificationId> {
        let link = self.link()?;
        let (app_id, app_name) = app;
        let mut state = self.state.borrow_mut();
        // A notification replaced under the id the app sent it with keeps the projection it had, so
        // the other devices see the update in place.
        let id = match state.out.get(&daemon_id) {
            Some(projected) => projected.id,
            None if state.out.len() >= MAX_PROJECTED => return None,
            None => ContinuityNotificationId {
                epoch: state.epoch,
                token: tilo_device_fabric::random::nonzero::<16>(),
            },
        };
        let redact = !previews;
        let expires_unix_ms = now_unix_ms
            .saturating_add(
                u64::try_from(notification.dismiss_after.as_millis())
                    .unwrap_or(MAX_CONTINUITY_NOTIFICATION_TTL_MS),
            )
            .min(now_unix_ms.saturating_add(MAX_CONTINUITY_NOTIFICATION_TTL_MS));
        let projection = ContinuityNotificationProjection {
            id,
            app_id: app_id.to_owned(),
            app_name: app_name.to_owned(),
            title: notification.title.clone(),
            body: if redact {
                String::new()
            } else {
                notification.body.clone()
            },
            body_redacted: redact,
            actions: notification.actions.clone(),
            urgency: match notification.severity {
                NotificationSeverity::Error => 2,
                NotificationSeverity::Warning => 1,
                NotificationSeverity::Info => 0,
            },
            expires_unix_ms,
        };
        if !projection.validate_at(now_unix_ms) {
            return None;
        }
        let action_keys = projection
            .actions
            .iter()
            .map(|(key, _)| key.clone())
            .collect();
        state.by_id.insert(id, daemon_id);
        state.out.insert(
            daemon_id,
            Projected {
                id,
                log_id: notification.id,
                action_keys,
                expires_unix_ms,
            },
        );
        drop(state);
        send(
            &link,
            &PersonalFabricAction::NotificationProject {
                notification: projection,
            },
        );
        Some(id)
    }

    /// Everything this device projected that is no longer shown here: the other devices take those
    /// copies away. Projections whose window ran out are forgotten without a word, and the rows of
    /// theirs the desk no longer shows are forgotten too.
    pub(crate) fn prune(&self, shown_log_ids: &BTreeSet<u64>, now_unix_ms: u64) {
        let mut state = self.state.borrow_mut();
        state
            .shown
            .retain(|log_id, _| shown_log_ids.contains(log_id));
        let Some(link) = self.link() else {
            return;
        };
        let gone: Vec<u32> = state
            .out
            .iter()
            .filter(|(_, projected)| {
                projected.expires_unix_ms <= now_unix_ms
                    || !shown_log_ids.contains(&projected.log_id)
            })
            .map(|(daemon_id, _)| *daemon_id)
            .collect();
        let mut retired = Vec::new();
        for daemon_id in gone {
            let Some(projected) = state.out.remove(&daemon_id) else {
                continue;
            };
            state.by_id.remove(&projected.id);
            if shown_log_ids.contains(&projected.log_id) {
                // Its window ran out on the other devices as well.
                continue;
            }
            retired.push(projected);
        }
        drop(state);
        for projected in retired {
            send(
                &link,
                &PersonalFabricAction::NotificationRetire { id: projected.id },
            );
        }
    }

    /// The person here is done with the notification the app sent under `daemon_id` : the other
    /// devices take its copy away.
    pub(crate) fn retire(&self, daemon_id: u32) {
        let projected = {
            let mut state = self.state.borrow_mut();
            let Some(projected) = state.out.remove(&daemon_id) else {
                return;
            };
            state.by_id.remove(&projected.id);
            projected
        };
        if let Some(link) = self.link() {
            send(
                &link,
                &PersonalFabricAction::NotificationRetire { id: projected.id },
            );
        }
    }

    /// The daemon notification and the action, when `action` from `source` is a command for a
    /// notification this device projected and that action is one the app offered.
    pub(crate) fn answered(&self, action: &PersonalFabricAction) -> Option<(u32, Option<String>)> {
        let state = self.state.borrow();
        match action {
            PersonalFabricAction::NotificationDismiss { id } => Some((*state.by_id.get(id)?, None)),
            PersonalFabricAction::NotificationInvoke { id, action_key } => {
                let offered = state.out.values().find(|projected| {
                    projected.id == *id && projected.action_keys.contains(action_key)
                })?;
                let daemon_id = *state.by_id.get(&offered.id)?;
                Some((daemon_id, Some(action_key.clone())))
            }
            _ => None,
        }
    }

    /// The device a projection that the desk shows as `log_id` came from, if it did: what a choice
    /// on it is answered to.
    pub(crate) fn route_of(&self, log_id: u64) -> Option<RemoteNotificationRoute> {
        self.state.borrow().shown.get(&log_id).cloned()
    }

    /// Remember that the desk's notification `log_id` is another device's `route` .
    pub(crate) fn shown(&self, log_id: u64, route: RemoteNotificationRoute) {
        self.state.borrow_mut().shown.insert(log_id, route);
    }

    /// Forget the desk's notification `log_id` : it left the desk, so nothing more is answered to
    /// it.
    pub(crate) fn gone(&self, log_id: u64) {
        self.state.borrow_mut().shown.remove(&log_id);
    }

    /// Answer the device the desk's notification `log_id` came from with `command` , then forget
    /// it: the person is done with it here.
    pub(crate) fn answer(
        &self,
        log_id: u64,
        command: impl FnOnce(&DeviceId, ContinuityNotificationId) -> PersonalFabricAction,
    ) {
        let Some(route) = self.route_of(log_id) else {
            return;
        };
        self.gone(log_id);
        let Ok(source) = DeviceId::new(route.source_device.clone()) else {
            return;
        };
        self.deliver(&source, command(&source, route.notification_id));
    }

    /// Tell the device `route` shows the notification here what the person did with it: run
    /// `action` when they chose one, else be done with it.
    pub(crate) fn answer_route(&self, route: &RemoteNotificationRoute, action: Option<&str>) {
        self.gone_route(route);
        let Ok(source) = DeviceId::new(route.source_device.clone()) else {
            return;
        };
        let action = action.map_or_else(
            || PersonalFabricAction::NotificationDismiss {
                id: route.notification_id,
            },
            |key| PersonalFabricAction::NotificationInvoke {
                id: route.notification_id,
                action_key: key.to_owned(),
            },
        );
        self.deliver(&source, action);
    }

    /// A retirement arrived from `source` : the desk forgets every copy of the projection it names.
    pub(crate) fn retire_shown(&self, source: &DeviceId, id: ContinuityNotificationId) {
        let source = source.as_str();
        self.state
            .borrow_mut()
            .shown
            .retain(|_, route| !(route.source_device == source && route.notification_id == id));
    }

    /// Forget the desk's notification `route` shows here.
    fn gone_route(&self, route: &RemoteNotificationRoute) {
        self.state
            .borrow_mut()
            .shown
            .retain(|_, shown| shown != route);
    }

    fn deliver(&self, source: &DeviceId, action: PersonalFabricAction) {
        let Some(link) = self.link() else {
            return;
        };
        if let Err(error) = link.try_send_personal_fabric_action(source, action) {
            tracing::warn!(
                device = source.as_str(),
                %error,
                "a choice on another device's notification did not reach it"
            );
        }
    }
}

/// Send `action` to every one of the person's devices that is online.
fn send(link: &SharedLink, action: &PersonalFabricAction) {
    let Some((local, _)) = link.local_device_fabric_identity() else {
        return;
    };
    for device in link.online_device_ids() {
        if device == local {
            continue;
        }
        if let Err(error) = link.try_send_personal_fabric_action(&device, action.clone()) {
            tracing::warn!(
                device = device.as_str(),
                %error,
                "a notification is not shown on another device"
            );
        }
    }
}
