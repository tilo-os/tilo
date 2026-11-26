//! Notifications that continue across the person's own devices, through the record of their
//! devices ([`SharedNotice`]).
//! A notification an app sent on this device is kept in the record while the shell shows it, under
//! an id this session drew: the person's other devices read it there and show it as their own
//! banner and centre row, named after this device. The record carries what the person's policy
//! lets leave (the text only with previews on) and the actions the app offered, and the
//! notification leaves it once its time there runs out or the person here is done with it, which
//! takes every copy away.
//! What the person does with one shown here from another device is said in the record: dismissing
//! it or choosing one of its actions answers it there, and the device it was sent on reads the
//! answer, closes the app's notification or runs that action, and takes it out of the record.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap};

use tilo_resource::Invoke;
use tilo_session_core::desktop::notifications::{
    ContinuityNotificationId, Notification, NotificationSeverity, RemoteNotificationRoute,
};
use tilo_workspace_service::workspaces::person_workspace::devices::{
    Devices, MAX_NOTICE_SHOWN_MS, NoticeAction, SharedNotice,
};

use crate::followed_resource::FollowedResource;
use crate::platform::fabric::Continuity;

/// One notification of this device the record keeps.
struct Projected {
    /// Its id in the record, hex.
    id: String,
    /// The shell's id for it here.
    log_id: u64,
    /// When its time in the record runs out, ms since the epoch.
    until_ms: i64,
}

#[derive(Default)]
struct State {
    /// What this device keeps in the record, by the daemon notification it came from.
    out: HashMap<u32, Projected>,
    /// The daemon notification each one in the record came from, by its id there.
    by_id: HashMap<String, u32>,
    /// What another of the person's devices keeps in the record that the shell shows here, by its
    /// id there: the shell's id, and the notification as it was shown.
    shown: BTreeMap<String, (u64, SharedNotice)>,
    /// How far the record moved when this device last read it.
    read: Option<u64>,
}

/// The person's devices as this session reads and writes their record.
struct Record {
    attached: Attached,
    continuity: Continuity,
}

/// The record, and who this device is in it.
#[derive(Clone)]
struct Attached {
    devices: FollowedResource<Devices>,
    /// This device's person's root key.
    person: [u8; 32],
    /// This device's own key.
    local: [u8; 32],
}

impl Attached {
    /// Make `invoke` on the record at the device's daemon, without waiting for it.
    fn change(&self, invoke: &Invoke<Devices>) {
        let action = invoke.id;
        self.devices.invoke_unwaited(invoke, move |made| {
            if let Err(error) = made {
                tracing::warn!(%error, action, "a notification's place in the record of the person's devices was not changed");
            }
        });
    }
}

/// What the record says this session has to do since it was last read.
#[derive(Default)]
pub(crate) struct Followed {
    /// The person answered a notification of this device elsewhere: the daemon notification, and
    /// the action they chose, `None` when they dismissed it.
    pub(crate) answered: Vec<(u32, Option<String>)>,
    /// Another device's notification to show here, or show again as it changed: the route back to
    /// it, and what it says.
    pub(crate) show: Vec<(RemoteNotificationRoute, SharedNotice)>,
    /// What the shell shows here of another device that is no longer shown there.
    pub(crate) gone: Vec<(String, ContinuityNotificationId)>,
}

/// This device's side of the notifications the person's devices show each other.
pub(crate) struct Across {
    /// The person's devices: `None` while this session reaches no record of theirs.
    record: RefCell<Option<Record>>,
    state: RefCell<State>,
}

impl Across {
    pub(crate) fn new() -> Self {
        Self {
            record: RefCell::new(None),
            state: RefCell::default(),
        }
    }

    /// Keep this device's notifications in the record of the person's devices followed as
    /// `devices` from now on, as the device `cert` names, while the person's `continuity` lets
    /// them continue.
    pub(crate) fn attach(
        &self,
        devices: FollowedResource<Devices>,
        cert: &tilo_user_identity::DeviceCert,
        continuity: Continuity,
    ) {
        *self.record.borrow_mut() = Some(Record {
            attached: Attached {
                devices,
                person: cert.user_public_key,
                local: cert.device_public_key,
            },
            continuity,
        });
    }

    /// The record and who this device is in it, while the person lets notifications continue.
    fn record(&self) -> Option<Attached> {
        let record = self.record.borrow();
        let record = record.as_ref()?;
        record
            .continuity
            .notifications_permitted()
            .then(|| record.attached.clone())
    }

    /// Show the notification app `app` sent here on the person's other devices, `notification`
    /// being the one the shell shows: its id, what its actions are, and when its banner goes.
    /// `previews` is whether this person lets a notification's text leave this device. Returns its
    /// id; `None` when this session reaches no record of their devices.
    pub(crate) fn project(
        &self,
        daemon_id: u32,
        app: (&str, &str),
        notification: &Notification,
        previews: bool,
        now_ms: i64,
    ) -> Option<ContinuityNotificationId> {
        let record = self.record()?;
        let local = record.local;
        let (app_id, app_name) = app;
        let mut state = self.state.borrow_mut();
        // A notification replaced under the id the app sent it with keeps the id it had, so the
        // other devices show the update in place.
        let id = match state.out.get(&daemon_id) {
            Some(projected) => id_of(&projected.id)?,
            None => ContinuityNotificationId {
                epoch: crate::platform::fabric::nonzero::<16>(),
                token: crate::platform::fabric::nonzero::<16>(),
            },
        };
        let shown_for = i64::try_from(notification.dismiss_after.as_millis())
            .unwrap_or(MAX_NOTICE_SHOWN_MS)
            .clamp(1, MAX_NOTICE_SHOWN_MS);
        let notice = SharedNotice {
            from: hex::encode(local),
            app_id: app_id.to_owned(),
            app_name: app_name.to_owned(),
            title: notification.title.clone(),
            body: if previews {
                notification.body.clone()
            } else {
                String::new()
            },
            redacted: !previews,
            actions: notification
                .actions
                .iter()
                .map(|(key, label)| NoticeAction {
                    key: key.clone(),
                    label: label.clone(),
                })
                .collect(),
            urgency: match notification.severity {
                NotificationSeverity::Error => 2,
                NotificationSeverity::Warning => 1,
                NotificationSeverity::Info => 0,
            },
            shown_ms: now_ms,
            until_ms: now_ms.saturating_add(shown_for),
            ..SharedNotice::default()
        };
        let key = hex_of(id);
        state.by_id.insert(key.clone(), daemon_id);
        state.out.insert(
            daemon_id,
            Projected {
                id: key.clone(),
                log_id: notification.id,
                until_ms: notice.until_ms,
            },
        );
        drop(state);
        record.change(&Devices::showing_notice(&key, &notice));
        Some(id)
    }

    /// Everything of this device the record keeps that is no longer shown here leaves it, so the
    /// other devices take their copies away. What ran out is left to run out there too, and the
    /// rows of theirs the shell no longer shows are forgotten.
    pub(crate) fn prune(&self, shown_log_ids: &BTreeSet<u64>, now_ms: i64) {
        let mut state = self.state.borrow_mut();
        state
            .shown
            .retain(|_, (log_id, _)| shown_log_ids.contains(log_id));
        let gone: Vec<u32> = state
            .out
            .iter()
            .filter(|(_, projected)| {
                projected.until_ms <= now_ms || !shown_log_ids.contains(&projected.log_id)
            })
            .map(|(daemon_id, _)| *daemon_id)
            .collect();
        let mut retired = Vec::new();
        for daemon_id in gone {
            let Some(projected) = state.out.remove(&daemon_id) else {
                continue;
            };
            state.by_id.remove(&projected.id);
            if projected.until_ms > now_ms {
                retired.push(projected.id);
            }
        }
        drop(state);
        self.retire_ids(retired);
    }

    /// The person here is done with the notification the app sent under `daemon_id`: the other
    /// devices take their copies away.
    pub(crate) fn retire(&self, daemon_id: u32) {
        let projected = {
            let mut state = self.state.borrow_mut();
            let Some(projected) = state.out.remove(&daemon_id) else {
                return;
            };
            state.by_id.remove(&projected.id);
            projected
        };
        self.retire_ids(vec![projected.id]);
    }

    fn retire_ids(&self, ids: Vec<String>) {
        if ids.is_empty() {
            return;
        }
        let Some(record) = self.record() else {
            return;
        };
        for id in ids {
            record.change(&Devices::retiring_notice(&id));
        }
    }

    /// Read what moved in the record since the last read: what the person answered elsewhere of
    /// this device's notifications, and what the other devices show now. `None` when it did not
    /// move, or this session reaches no record.
    pub(crate) fn follow(&self, now_ms: i64) -> Option<Followed> {
        let attached = self.record()?;
        let moved = attached.devices.revision();
        let mut state = self.state.borrow_mut();
        if state.read == Some(moved) {
            return None;
        }
        state.read = Some(moved);
        let record = attached.devices.get();
        let local = hex::encode(attached.local);
        let person = attached.person;
        let labels: BTreeMap<String, String> = record
            .listed(&person)
            .into_iter()
            .map(|listed| {
                let label = listed.device.called(&listed.cert);
                (listed.key, label)
            })
            .collect();
        let mut followed = Followed::default();
        let mut there = BTreeSet::new();
        for (id, notice) in record.notices.iter() {
            let id = id.as_str();
            if notice.from == local {
                if !notice.answered_on.is_empty()
                    && let Some(daemon_id) = state.by_id.get(id)
                {
                    followed.answered.push((
                        *daemon_id,
                        (!notice.chosen.is_empty()).then(|| notice.chosen.clone()),
                    ));
                }
                // One this session projected is shown here already; one this device's daemon
                // kept for the person, as a refused device at another peer version is, is shown
                // like another device's.
                if state.by_id.contains_key(id) {
                    continue;
                }
            }
            if !notice.shown_at(now_ms) {
                continue;
            }
            let Some(notification_id) = id_of(id) else {
                continue;
            };
            there.insert(id.to_owned());
            if state
                .shown
                .get(id)
                .is_some_and(|(_, shown)| shown == notice)
            {
                continue;
            }
            followed.show.push((
                RemoteNotificationRoute {
                    source_label: labels
                        .get(&notice.from)
                        .cloned()
                        .unwrap_or_else(|| notice.from.clone()),
                    source_device: notice.from.clone(),
                    notification_id,
                },
                notice.clone(),
            ));
        }
        let gone: Vec<String> = state
            .shown
            .keys()
            .filter(|id| !there.contains(*id))
            .cloned()
            .collect();
        for id in gone {
            if let Some((_, notice)) = state.shown.remove(&id)
                && let Some(notification_id) = id_of(&id)
            {
                followed.gone.push((notice.from, notification_id));
            }
        }
        Some(followed)
    }

    /// Remember that the shell shows another device's `notice` under `route` as `log_id`.
    pub(crate) fn shown(&self, log_id: u64, route: &RemoteNotificationRoute, notice: SharedNotice) {
        self.state
            .borrow_mut()
            .shown
            .insert(hex_of(route.notification_id), (log_id, notice));
    }

    /// The person dismissed the shell's notification `log_id` here: when another device keeps it
    /// in the record, they answered it there.
    pub(crate) fn dismissed(&self, log_id: u64) {
        let id = self
            .state
            .borrow()
            .shown
            .iter()
            .find(|(_, (shown, _))| *shown == log_id)
            .map(|(id, _)| id.clone());
        if let Some(id) = id {
            self.answer(&id, "");
        }
    }

    /// The person answered another device's notification here, the one `route` names: `action`
    /// the one they chose, `None` when they only opened it. What the shell still shows of it goes
    /// once the record no longer shows it.
    pub(crate) fn answer_route(&self, route: &RemoteNotificationRoute, action: Option<&str>) {
        self.answer(&hex_of(route.notification_id), action.unwrap_or_default());
    }

    fn answer(&self, id: &str, chosen: &str) {
        let Some(record) = self.record() else {
            return;
        };
        record.change(&Devices::answering_notice(id, &record.local, chosen));
    }
}

/// `id` as the record keeps it: its 32 bytes in hex.
fn hex_of(id: ContinuityNotificationId) -> String {
    let mut bytes = [0; 32];
    bytes[..16].copy_from_slice(&id.epoch);
    bytes[16..].copy_from_slice(&id.token);
    hex::encode(bytes)
}

/// The id the record keeps as `hex`.
fn id_of(hex: &str) -> Option<ContinuityNotificationId> {
    let mut bytes = [0; 32];
    hex::decode_to_slice(hex, &mut bytes).ok()?;
    let mut id = ContinuityNotificationId {
        epoch: [0; 16],
        token: [0; 16],
    };
    id.epoch.copy_from_slice(&bytes[..16]);
    id.token.copy_from_slice(&bytes[16..]);
    Some(id)
}
