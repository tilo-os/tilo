//! The person's answers about the devices of their machines: "Recorder may use
//! the camera on this laptop, always".
//!
//! An answer that outlasts the session is an entry of a typed resource of the
//! person's own workspace ([`AnswersRecord`]), one per device, app and
//! capability, naming the device it is about. It follows the person to every
//! device of theirs, keeps its history like any of their resources, and any of
//! their devices may take it back. It answers only on the device it names: an
//! answer given on the laptop never opens the phone's camera. An answer for the
//! session stays in this device's memory, and one for once is never kept.
//!
//! A workspace never answers here. What it may do is refuse a device to the
//! apps in it ([`DeviceGrants::refuse_in`]), whatever the person answered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use collab_types::caps::DeviceCap;
use serde::{Deserialize, Serialize};
use tilo_action_policy::Answer;
use tilo_action_types::Lifetime;
use tilo_lock::MutexRecover as _;
use tilo_user_identity::DeviceCert;

/// One kept answer, as the person's record of them holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceAnswer {
    /// The device it answers on: its key, in hex.
    pub device: String,
    /// The app it answers for.
    pub app: String,
    /// What the app may use.
    pub cap: DeviceCap,
    /// Allowed or refused.
    pub answer: Answer,
    /// How long it stands.
    pub lifetime: Lifetime,
}

/// A kept answer as a list of them shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Kept {
    #[serde(flatten)]
    pub answer: DeviceAnswer,
    /// The name of the device it answers on, as its certificate names it.
    pub device_label: String,
    /// Whether that device is this one.
    pub this_device: bool,
}

/// The person's record of their kept answers, as whoever keeps this device's answers follows and
/// changes it by its typed actions: the daemon over the workspaces it holds, a session over the
/// daemon.
pub struct AnswersRecord {
    /// This device's certificate: the device its answers are on.
    pub cert: DeviceCert,
    /// Every answer the record keeps now, on any of the person's devices.
    pub kept: Arc<dyn Fn() -> Vec<DeviceAnswer> + Send + Sync>,
    /// Moves with every change of the record.
    pub changes: tokio::sync::watch::Receiver<u64>,
    /// The person's devices whose certificates hold now: what names the device each answer is on.
    pub devices: Arc<dyn Fn() -> Vec<DeviceCert> + Send + Sync>,
    /// Keep an answer, now or once this device holds the person's workspace.
    pub answer: Arc<dyn Fn(DeviceAnswer) + Send + Sync>,
    /// Take back what `(device, app, cap)` was answered, now or once this device holds it.
    pub take_back: Arc<TakeBack>,
}

/// What takes back the answer `(device, app, cap)` was given.
pub type TakeBack = dyn Fn(&str, &str, DeviceCap) + Send + Sync;

/// The person's device answers, as one device reads and gives them.
#[derive(Clone)]
pub struct DeviceGrants {
    /// The person's record of kept answers, once whoever keeps these follows it:
    /// until then none are kept.
    store: Arc<OnceLock<AnswersRecord>>,
    /// The answers given for this session, by app and capability.
    session: Arc<Mutex<BTreeMap<(String, DeviceCap), Answer>>>,
    /// What the workspaces refuse their apps, as `(workspace, app, device)`.
    refused: Arc<Mutex<BTreeSet<Refusal>>>,
    /// What this device's apps were allowed at the last look ([`Self::taken_back`]).
    seen: Arc<Mutex<BTreeSet<(String, DeviceCap)>>>,
}

/// A workspace refusing one of its apps a device: `(workspace, app, device)`.
pub type Refusal = (String, String, DeviceCap);

impl DeviceGrants {
    /// Answers of a device without the person's own workspace: none outlast the
    /// session until it is handed one ([`Self::keep_in`]).
    #[must_use]
    pub fn unkept() -> Self {
        Self {
            store: Arc::default(),
            session: Arc::default(),
            refused: Arc::default(),
            seen: Arc::default(),
        }
    }

    /// Keep answers in the person's record `store` from now on, in every clone.
    /// The first record stands.
    pub fn keep_in(&self, store: AnswersRecord) {
        let _ = self.store.set(store);
    }

    /// Moves with every change of the person's record of kept answers, once kept.
    #[must_use]
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.store.get().map(|store| store.changes.clone())
    }

    fn this_device(store: &AnswersRecord) -> String {
        hex::encode(store.cert.device_public_key)
    }

    /// The devices the workspaces refuse their apps from now on, replacing what they refused before.
    pub fn refuse_in(&self, refused: BTreeSet<Refusal>) {
        *self.refused.lock_recover() = refused;
    }

    /// What `app`, running in `workspace`, may do with `cap` on this device:
    /// [`Answer::Deny`] when the workspace refuses it, else the answer the
    /// person gave for this session or kept for this device, else
    /// [`Answer::Ask`].
    #[must_use]
    pub fn decide(&self, app: &str, workspace: Option<&str>, cap: DeviceCap) -> Answer {
        let refused = workspace.is_some_and(|workspace| {
            self.refused
                .lock_recover()
                .contains(&(workspace.to_owned(), app.to_owned(), cap))
        });
        if refused {
            return Answer::Deny;
        }
        self.given(app, cap)
            .map_or(Answer::Ask, |(answer, _)| answer)
    }

    /// The answer the person gave `app` for `cap` on this device that still
    /// stands, and how long it lasts: one for the session, else one kept.
    #[must_use]
    pub fn given(&self, app: &str, cap: DeviceCap) -> Option<(Answer, Lifetime)> {
        let session = self
            .session
            .lock_recover()
            .get(&(app.to_owned(), cap))
            .copied();
        if let Some(answer) = session {
            return Some((answer, Lifetime::Session));
        }
        let store = self.store.get()?;
        let now = Some(tilo_clock::unix_now_ms());
        let this = Self::this_device(store);
        (store.kept)()
            .into_iter()
            .find(|kept| kept.device == this && kept.app == app && kept.cap == cap)
            .filter(|kept| kept.lifetime.is_live_at(now))
            .map(|kept| (kept.answer, kept.lifetime))
    }

    /// Keep what the person answered for `app` and `cap` on this device for
    /// `lifetime`: nothing for once, this device's memory for the session, the
    /// person's own workspace for longer.
    pub fn answer(&self, app: &str, cap: DeviceCap, answer: Answer, lifetime: Lifetime) {
        match lifetime {
            Lifetime::Once => {}
            Lifetime::Session => {
                self.session
                    .lock_recover()
                    .insert((app.to_owned(), cap), answer);
            }
            Lifetime::Until(_) | Lifetime::UntilRevoked => {
                let Some(store) = self.store.get() else {
                    self.session
                        .lock_recover()
                        .insert((app.to_owned(), cap), answer);
                    return;
                };
                (store.answer)(DeviceAnswer {
                    device: Self::this_device(store),
                    app: app.to_owned(),
                    cap,
                    answer,
                    lifetime,
                });
            }
        }
    }

    /// Take back what `app` was answered of `cap` on `device` (in hex): kept,
    /// and for this session when it is this device. `false` when there was nothing.
    pub fn revoke(&self, device: &str, app: &str, cap: DeviceCap) -> bool {
        let this = self
            .store
            .get()
            .is_none_or(|store| Self::this_device(store) == device);
        let session = this
            && self
                .session
                .lock_recover()
                .remove(&(app.to_owned(), cap))
                .is_some();
        let Some(store) = self.store.get() else {
            return session;
        };
        let kept = (store.kept)()
            .iter()
            .any(|kept| kept.device == device && kept.app == app && kept.cap == cap);
        if !kept {
            return session;
        }
        (store.take_back)(device, app, cap);
        true
    }

    /// Take back what `app` was answered of `cap` on this device.
    pub fn revoke_here(&self, app: &str, cap: DeviceCap) -> bool {
        let device = self.store.get().map(Self::this_device).unwrap_or_default();
        self.revoke(&device, app, cap)
    }

    /// What the person allowed on this device, for this session or kept, as
    /// `(app, device)`.
    #[must_use]
    pub fn allowed_here(&self) -> Vec<(String, DeviceCap)> {
        let mut allowed: Vec<(String, DeviceCap)> = self
            .session
            .lock_recover()
            .iter()
            .filter(|(_, answer)| **answer == Answer::Allow)
            .map(|(key, _)| key.clone())
            .collect();
        allowed.extend(
            self.kept()
                .into_iter()
                .filter(|kept| kept.this_device && kept.answer.answer == Answer::Allow)
                .map(|kept| (kept.answer.app, kept.answer.cap)),
        );
        allowed.sort();
        allowed.dedup();
        allowed
    }

    /// What this device's apps were allowed at the last look and no longer are, as `(app, device)`:
    /// taken back here or on another of the person's devices, or lapsed. A capture on such an
    /// answer stops.
    #[must_use]
    pub fn taken_back(&self) -> BTreeSet<(String, DeviceCap)> {
        let now: BTreeSet<(String, DeviceCap)> = self.allowed_here().into_iter().collect();
        let mut seen = self.seen.lock_recover();
        let taken = seen.difference(&now).cloned().collect();
        *seen = now;
        taken
    }

    /// Every answer the person kept that still stands, on any of their devices.
    #[must_use]
    pub fn kept(&self) -> Vec<Kept> {
        let Some(store) = self.store.get() else {
            return Vec::new();
        };
        let now = Some(tilo_clock::unix_now_ms());
        let this = Self::this_device(store);
        let labels: BTreeMap<String, String> = (store.devices)()
            .into_iter()
            .map(|cert| (hex::encode(cert.device_public_key), cert.device_label))
            .collect();
        (store.kept)()
            .into_iter()
            .filter(|kept| kept.lifetime.is_live_at(now))
            .map(|answer| Kept {
                device_label: labels.get(&answer.device).cloned().unwrap_or_default(),
                this_device: answer.device == this,
                answer,
            })
            .collect()
    }
}
