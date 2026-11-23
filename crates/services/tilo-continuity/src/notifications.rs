//! Notifications that continue across the person's own devices, published as [`PROVIDER`]: what
//! this device shows its others of what it was told, and what they told it.
//!
//! A notification an app sent on this device is projected to each of the person's devices that is
//! here, under an id this device minted ([`NotificationId`]): another device shows it as its own
//! banner and centre row, named after this device. The projection carries what the person's policy
//! lets leave (the body only with previews on) and the actions the app offered, and it stops being
//! shown once its window on the other device runs out or the person here is done with it, which
//! retires every copy.
//!
//! What the person does with a mirrored notification goes back to the device that sent it: being
//! done with it closes the app's notification there, and choosing one of its actions runs that
//! action there. A device's ids name only what it projected, so nothing here answers a command for
//! a notification it did not send. All of it is under the person's Continuity → Notifications
//! switch.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::json;
use tilo_actions::remote::{ActionCallError, ActionProvider};
use tilo_fabric::actions::{Asker, Provider, asking_device};
use tilo_fabric::{DeviceId, Fabric, nonzero, spend};
use tilo_lock::MutexRecover as _;

use crate::Switches;

/// The provider mirrored notifications are published under.
pub const PROVIDER: &str = "so.tilo.notification-mirror";
/// Longest a mirrored notification may stay up on another device.
pub const MAX_CONTINUITY_NOTIFICATION_TTL_MS: u64 = 15 * 60 * 1_000;
const MAX_CONTINUITY_ACTIONS: usize = 16;
/// The most projections one device keeps bookkeeping for: what fits on the other devices' sessions
/// and centres anyway.
const MAX_PROJECTED: usize = 128;

/// A notification as the device that projected it names it: its run's epoch and a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct NotificationId {
    pub epoch: [u8; 16],
    pub token: [u8; 16],
}

/// Whether `id` is a well-formed notification app id.
#[must_use]
pub fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// What another device shows of a notification projected to it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContinuityNotificationProjection {
    pub id: NotificationId,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub body_redacted: bool,
    pub actions: Vec<(String, String)>,
    pub urgency: u8,
    pub expires_unix_ms: u64,
}

impl ContinuityNotificationProjection {
    #[must_use]
    pub fn validate_at(&self, now_unix_ms: u64) -> bool {
        self.id.epoch != [0; 16]
            && self.id.token != [0; 16]
            && valid_app_id(&self.app_id)
            && !self.app_name.is_empty()
            && self.app_name.len() <= 160
            && self.title.len() <= 512
            && self.body.len() <= 4_096
            && self.urgency <= 2
            && self.actions.len() <= MAX_CONTINUITY_ACTIONS
            && self.expires_unix_ms > now_unix_ms
            && self.expires_unix_ms.saturating_sub(now_unix_ms)
                <= MAX_CONTINUITY_NOTIFICATION_TTL_MS
            && [&self.app_name, &self.title]
                .into_iter()
                .all(|value| !value.chars().any(char::is_control))
            && self
                .body
                .chars()
                .all(|character| !character.is_control() || matches!(character, '\n' | '\t'))
            && (!self.body_redacted || self.body.is_empty())
            && self.actions.iter().all(|(key, label)| {
                !key.is_empty()
                    && key.len() <= 128
                    && label.len() <= 256
                    && !key.chars().any(char::is_control)
                    && !label.chars().any(char::is_control)
            })
    }
}

/// `notifications#project`: a notification an app sent here, as the session shows it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    /// The id the app's notification has on this device's notification service.
    pub daemon_id: u32,
    /// The session's id for the row it shows.
    pub log_id: u64,
    pub app_id: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub actions: Vec<(String, String)>,
    /// 0 for information, 1 for a warning, 2 for an error.
    #[serde(default)]
    pub urgency: u8,
    /// When the banner here goes away.
    pub dismiss_after_ms: u64,
    /// Whether the person lets a notification's text leave this device.
    pub previews: bool,
}

/// What `notifications#project` answers: the projection's id, or none when it reaches no other
/// device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Projected {
    pub id: Option<NotificationId>,
}

/// `notifications#prune`: the rows the session shows now.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Prune {
    pub shown: BTreeSet<u64>,
}

/// `notifications#retire`: the app's notification the person here is done with.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Retire {
    pub daemon_id: u32,
}

/// `notifications#choose`: what the person did with another device's notification shown here as
/// `log_id`: chose `action`, or none to be done with it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Choose {
    pub log_id: u64,
    #[serde(default)]
    pub action: Option<String>,
}

/// `notifications#show`: a notification another device projected here.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Show {
    pub notification: ContinuityNotificationProjection,
}

/// `notifications#retired`: the projection another device took away.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Retired {
    pub id: NotificationId,
}

/// `notifications#answer`: what the person did on another device with a notification this one
/// projected.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Answer {
    pub id: NotificationId,
    #[serde(default)]
    pub action: Option<String>,
}

/// What the session answers `notifications#show-remote`: the row it shows the notification as.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ShownAs {
    pub log_id: u64,
}

/// Another device's notification, as this device shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Route {
    source: DeviceId,
    id: NotificationId,
}

/// One notification this device projected to the person's other devices.
struct Out {
    id: NotificationId,
    /// The session's id for the notification here.
    log_id: u64,
    /// The actions the app offered, which only those may be invoked on.
    action_keys: Vec<String>,
    /// When the projection stops being shown on the other devices.
    expires_unix_ms: u64,
}

struct State {
    /// This run's epoch: with it, an id minted now never names a notification an earlier run of
    /// this device projected.
    epoch: [u8; 16],
    /// What this device projected, by the daemon notification it came from.
    out: HashMap<u32, Out>,
    /// The daemon notification each projection came from.
    by_id: HashMap<NotificationId, u32>,
    /// What another of the person's devices projected here, by the session's id, so a choice on it
    /// is answered where it came from.
    shown: HashMap<u64, Route>,
}

/// Notification mirroring on this device. Cloning shares it.
#[derive(Clone)]
pub struct Mirror {
    fabric: Fabric,
    switches: Switches,
    state: Arc<Mutex<State>>,
}

impl Mirror {
    #[must_use]
    pub fn new(fabric: Fabric, switches: Switches) -> Self {
        Self {
            fabric,
            switches,
            state: Arc::new(Mutex::new(State {
                epoch: nonzero::<16>(),
                out: HashMap::new(),
                by_id: HashMap::new(),
                shown: HashMap::new(),
            })),
        }
    }

    fn permitted(&self) -> Result<(), ActionCallError> {
        self.switches
            .notifications_permitted()
            .then_some(())
            .ok_or_else(tilo_fabric::cant_now)
    }

    /// Send `action` with `input` to every one of the person's devices that is here.
    fn send(&self, action: &'static str, input: impl Serialize) {
        let input = serde_json::to_value(input).unwrap_or_default();
        for device in self.fabric.online() {
            spend(
                self.fabric
                    .call(&device, PROVIDER, "notifications", action, input.clone()),
                "a notification on another device",
            );
        }
    }

    /// Show the notification an app sent here on the person's other devices.
    fn project(&self, asked: Project, now_unix_ms: u64) -> Option<NotificationId> {
        if !self.switches.notifications_permitted() {
            return None;
        }
        let mut state = self.state.lock_recover();
        // A notification replaced under the id the app sent it with keeps the projection it had, so
        // the other devices see the update in place.
        let id = match state.out.get(&asked.daemon_id) {
            Some(projected) => projected.id,
            None if state.out.len() >= MAX_PROJECTED => return None,
            None => NotificationId {
                epoch: state.epoch,
                token: nonzero::<16>(),
            },
        };
        let redact = !asked.previews;
        let expires_unix_ms = now_unix_ms
            .saturating_add(asked.dismiss_after_ms)
            .min(now_unix_ms.saturating_add(MAX_CONTINUITY_NOTIFICATION_TTL_MS));
        let projection = ContinuityNotificationProjection {
            id,
            app_id: asked.app_id,
            app_name: asked.app_name,
            title: asked.title,
            body: if redact { String::new() } else { asked.body },
            body_redacted: redact,
            actions: asked.actions,
            urgency: asked.urgency,
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
        state.by_id.insert(id, asked.daemon_id);
        state.out.insert(
            asked.daemon_id,
            Out {
                id,
                log_id: asked.log_id,
                action_keys,
                expires_unix_ms,
            },
        );
        drop(state);
        self.send(
            "show",
            Show {
                notification: projection,
            },
        );
        Some(id)
    }

    /// Everything this device projected that is no longer shown here: the other devices take those
    /// copies away. Projections whose window ran out are forgotten without a word, and the rows of
    /// theirs the session no longer shows are forgotten too.
    fn prune(&self, shown_log_ids: &BTreeSet<u64>, now_unix_ms: u64) {
        let mut state = self.state.lock_recover();
        state
            .shown
            .retain(|log_id, _| shown_log_ids.contains(log_id));
        if !self.switches.notifications_permitted() {
            return;
        }
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
            retired.push(projected.id);
        }
        drop(state);
        for id in retired {
            self.send("retired", Retired { id });
        }
    }

    /// The person here is done with the notification the app sent under `daemon_id`: the other
    /// devices take its copy away.
    fn retire(&self, daemon_id: u32) {
        let mut state = self.state.lock_recover();
        let Some(projected) = state.out.remove(&daemon_id) else {
            return;
        };
        state.by_id.remove(&projected.id);
        drop(state);
        let id = projected.id;
        if self.switches.notifications_permitted() {
            self.send("retired", Retired { id });
        }
    }

    /// The daemon notification and the action, when `action` is a choice on a notification this
    /// device projected and one the app offered.
    fn answered(&self, id: &NotificationId, action: Option<&str>) -> Option<(u32, Option<String>)> {
        let state = self.state.lock_recover();
        let daemon_id = *state.by_id.get(id)?;
        let offered = action.map(|key| {
            state
                .out
                .get(&daemon_id)
                .is_some_and(|out| out.action_keys.iter().any(|offered| offered == key))
        });
        drop(state);
        match (action, offered) {
            (Some(key), Some(true)) => Some((daemon_id, Some(key.to_owned()))),
            (None, _) => Some((daemon_id, None)),
            _ => None,
        }
    }

    /// The person chose `action` on another device's notification shown here as `log_id`, or was
    /// done with it: the device it came from is told, and it is forgotten here.
    fn choose(&self, log_id: u64, action: Option<String>) -> Result<(), ActionCallError> {
        let route = self
            .state
            .lock_recover()
            .shown
            .remove(&log_id)
            .ok_or_else(|| ActionCallError::Refused {
                reason: "that notification is not another device's".to_owned(),
            })?;
        self.permitted()?;
        let source = route.source.clone();
        self.fabric
            .call(
                &route.source,
                PROVIDER,
                "notifications",
                "answer",
                Answer {
                    id: route.id,
                    action,
                },
            )
            .then(move |answer| {
                if !matches!(answer, Some(Ok(_))) {
                    // A choice that did not reach its device is only logged.
                    tracing::warn!(
                        device = source.as_str(),
                        "a choice on another device's notification did not reach it"
                    );
                }
            });
        Ok(())
    }

    /// Another device projected a notification here: the session shows it, named after that
    /// device.
    fn show(
        &self,
        from: &DeviceId,
        notification: &ContinuityNotificationProjection,
    ) -> Result<(), ActionCallError> {
        self.permitted()?;
        if !notification.validate_at(tilo_clock::unix_now_ms()) {
            return Err(ActionCallError::InvalidRequest {
                reason: "not a notification another device could show".to_owned(),
            });
        }
        let route = Route {
            source: from.clone(),
            id: notification.id,
        };
        let state = self.state.clone();
        self.fabric
            .seat()
            .call(
                "notifications",
                "show-remote",
                json!({
                    "source": from,
                    "device": self.fabric.label_of(from),
                    "notification": notification,
                }),
            )
            .then(move |answer| {
                let Some(Ok(value)) = answer else {
                    return;
                };
                if let Ok(shown) = serde_json::from_value::<ShownAs>(value) {
                    state.lock_recover().shown.insert(shown.log_id, route);
                }
            });
        Ok(())
    }

    /// Another device took a projection away: the session forgets every copy of it here.
    fn retired(&self, from: &DeviceId, id: NotificationId) {
        self.state
            .lock_recover()
            .shown
            .retain(|_, route| !(&route.source == from && route.id == id));
        let answer = self.fabric.seat().call(
            "notifications",
            "retire-remote",
            json!({ "source": from, "id": id }),
        );
        spend(answer, "a mirrored notification here");
    }

    /// The person chose on another device on a notification this device projected: the app here
    /// is told.
    fn answer(&self, id: &NotificationId, action: Option<&str>) -> Result<(), ActionCallError> {
        let (daemon_id, action) =
            self.answered(id, action)
                .ok_or_else(|| ActionCallError::Refused {
                    reason: "this device projected no such notification".to_owned(),
                })?;
        spend(
            self.fabric.seat().call(
                "notifications",
                "answered",
                json!({ "daemon_id": daemon_id, "action": action }),
            ),
            "a choice made on another device",
        );
        Ok(())
    }

    /// Mirroring's resources and actions, as the device's action host mounts them under
    /// [`PROVIDER`].
    #[must_use]
    pub fn provider(&self) -> Arc<dyn ActionProvider + Send + Sync> {
        let at = || self.clone();
        Provider::new(PROVIDER, self.fabric.local().clone(), *self.fabric.person())
            .act(
                "notifications",
                "project",
                "Show a notification on the person's other devices",
                Asker::Here,
                {
                    let mirror = at();
                    move |_, asked: Project| {
                        Ok(Projected {
                            id: mirror.project(asked, tilo_clock::unix_now_ms()),
                        })
                    }
                },
            )
            .act(
                "notifications",
                "prune",
                "Take away the copies of notifications no longer shown here",
                Asker::Here,
                {
                    let mirror = at();
                    move |_, prune: Prune| {
                        mirror.prune(&prune.shown, tilo_clock::unix_now_ms());
                        Ok(())
                    }
                },
            )
            .act(
                "notifications",
                "retire",
                "Take away the copies of a notification the person is done with",
                Asker::Here,
                {
                    let mirror = at();
                    move |_, retire: Retire| {
                        mirror.retire(retire.daemon_id);
                        Ok(())
                    }
                },
            )
            .act(
                "notifications",
                "choose",
                "Answer another device's notification",
                Asker::Here,
                {
                    let mirror = at();
                    move |_, choose: Choose| mirror.choose(choose.log_id, choose.action)
                },
            )
            .act(
                "notifications",
                "show",
                "Show another device's notification",
                Asker::Device,
                {
                    let mirror = at();
                    move |who, show: Show| mirror.show(asking_device(who)?, &show.notification)
                },
            )
            .act(
                "notifications",
                "retired",
                "Another device took its notification away",
                Asker::Device,
                {
                    let mirror = at();
                    move |who, retired: Retired| {
                        mirror.retired(asking_device(who)?, retired.id);
                        Ok(())
                    }
                },
            )
            .act(
                "notifications",
                "answer",
                "The person answered this device's notification elsewhere",
                Asker::Device,
                {
                    let mirror = at();
                    move |_, answer: Answer| mirror.answer(&answer.id, answer.action.as_deref())
                },
            )
            .shared()
    }
}
