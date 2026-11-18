//! The person's answers about the devices of their machines: "Recorder may use
//! the camera on this laptop, always".
//!
//! An answer that outlasts the session is one document of the person's own store
//! ([`DEVICE_ANSWERS_KIND`]), one entry per device, app and capability, naming
//! the device it is about. It follows the person to every device of theirs,
//! keeps its history like any of their documents, and any of their devices may
//! take it back. It answers only on the device it names: an answer given on
//! the laptop never opens the phone's camera. An answer for the session stays
//! in this device's memory, and one for once is never kept.
//!
//! A workspace never answers here. What it may do is put a ceiling on the
//! apps in it ([`DeviceGrants::decide`]): refuse, or have the person asked
//! each time.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _};
use collab_types::DocumentMutation;
use collab_types::caps::DeviceCap;
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Answer, Lifetime};
use tilo_lock::MutexRecover as _;

use super::link::PersonalSync;
use super::{DEVICES_APP, PersonalStore};

/// The kind of the person's document that holds their kept device answers.
pub const DEVICE_ANSWERS_KIND: &str = "identity.device-answers";

/// One kept answer, as the person's document holds it.
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

impl DeviceAnswer {
    fn key(&self) -> String {
        entry_key(&self.device, &self.app, self.cap)
    }
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

fn entry_key(device: &str, app: &str, cap: DeviceCap) -> String {
    format!("{device}\u{1f}{app}\u{1f}{}", cap.name())
}

fn document() -> String {
    collab_types::person_document_id(DEVICE_ANSWERS_KIND)
}

/// The person's device answers, as one device reads and gives them.
#[derive(Clone)]
pub struct DeviceGrants {
    /// The person's store, where kept answers live, once it is open: until
    /// then none are kept.
    store: Arc<OnceLock<PersonalSync>>,
    /// The answers given for this session, by app and capability.
    session: Arc<Mutex<BTreeMap<(String, DeviceCap), Answer>>>,
}

impl DeviceGrants {
    /// Answers kept in the person's store `store`.
    #[must_use]
    pub fn over(store: PersonalSync) -> Self {
        let grants = Self::unkept();
        grants.keep_in(store);
        grants
    }

    /// Answers of a device without the person's store: none outlast the
    /// session until it is handed one ([`Self::keep_in`]).
    #[must_use]
    pub fn unkept() -> Self {
        Self {
            store: Arc::default(),
            session: Arc::default(),
        }
    }

    /// Keep answers in the person's store `store` from now on, in every clone:
    /// what a session does once it opened the store. The first store stands.
    pub fn keep_in(&self, store: PersonalSync) {
        let _ = self.store.set(store);
    }

    /// Ticks whenever the person's store changed, here or on another of their devices.
    #[must_use]
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.store.get().map(PersonalSync::changes)
    }

    fn this_device(store: &PersonalStore) -> String {
        hex::encode(store.device().device_public_key)
    }

    /// What `app` may do with `cap` on this device: the answer the person gave
    /// for this session or kept for this device, or [`Answer::Ask`] when they
    /// gave none. `ceiling` is the most the app's workspace allows:
    /// [`Answer::Deny`] refuses whatever the person answered, [`Answer::Ask`]
    /// asks again past what they kept.
    #[must_use]
    pub fn decide(&self, app: &str, cap: DeviceCap, ceiling: Answer) -> Answer {
        if ceiling == Answer::Deny {
            return Answer::Deny;
        }
        let session = self
            .session
            .lock_recover()
            .get(&(app.to_owned(), cap))
            .copied();
        if let Some(answer) = session {
            return answer;
        }
        if ceiling == Answer::Ask {
            return Answer::Ask;
        }
        let Some(store) = self.store.get() else {
            return Answer::Ask;
        };
        let now = Some(tilo_clock::unix_now_ms());
        store.read(|store| {
            let key = entry_key(&Self::this_device(store), app, cap);
            entries(store)
                .remove(&key)
                .filter(|kept| kept.lifetime.is_live_at(now))
                .map_or(Answer::Ask, |kept| kept.answer)
        })
    }

    /// Keep what the person answered for `app` and `cap` on this device for
    /// `lifetime`: nothing for once, this device's memory for the session, the
    /// person's store for longer.
    ///
    /// # Errors
    /// The person's store refused the write; the answer stands for this session then.
    pub fn answer(
        &self,
        app: &str,
        cap: DeviceCap,
        answer: Answer,
        lifetime: Lifetime,
    ) -> Result<(), String> {
        match lifetime {
            Lifetime::Once => Ok(()),
            Lifetime::Session => {
                self.session
                    .lock_recover()
                    .insert((app.to_owned(), cap), answer);
                Ok(())
            }
            Lifetime::Until(_) | Lifetime::UntilRevoked => {
                let Some(store) = self.store.get() else {
                    self.session
                        .lock_recover()
                        .insert((app.to_owned(), cap), answer);
                    return Ok(());
                };
                let device = store.read(Self::this_device);
                let kept = DeviceAnswer {
                    device,
                    app: app.to_owned(),
                    cap,
                    answer,
                    lifetime,
                };
                let value = serde_json::to_string(&kept).map_err(|error| error.to_string())?;
                write(store, "answer", |doc| {
                    doc.put(automerge::ROOT, kept.key(), value)
                })
            }
        }
    }

    /// Take back what `app` was answered of `cap` on `device` (in hex): kept,
    /// and for this session when it is this device. `false` when there was nothing.
    ///
    /// # Errors
    /// The person's store refused the write; the answer stands then.
    pub fn revoke(&self, device: &str, app: &str, cap: DeviceCap) -> Result<bool, String> {
        let this = self
            .store
            .get()
            .is_none_or(|store| store.read(Self::this_device) == device);
        let session = this
            && self
                .session
                .lock_recover()
                .remove(&(app.to_owned(), cap))
                .is_some();
        let Some(store) = self.store.get() else {
            return Ok(session);
        };
        let key = entry_key(device, app, cap);
        if !store.read(|store| entries(store).contains_key(&key)) {
            return Ok(session);
        }
        write(store, "revoke", |doc| {
            doc.delete(automerge::ROOT, key.as_str())
        })?;
        Ok(true)
    }

    /// Take back what `app` was answered of `cap` on this device.
    ///
    /// # Errors
    /// The person's store refused the write; the answer stands then.
    pub fn revoke_here(&self, app: &str, cap: DeviceCap) -> Result<bool, String> {
        let device = self
            .store
            .get()
            .map(|store| store.read(Self::this_device))
            .unwrap_or_default();
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

    /// Every answer the person kept that still stands, on any of their devices.
    #[must_use]
    pub fn kept(&self) -> Vec<Kept> {
        let Some(store) = self.store.get() else {
            return Vec::new();
        };
        let now = Some(tilo_clock::unix_now_ms());
        store.read(|store| {
            let this = Self::this_device(store);
            let labels: BTreeMap<String, String> = store
                .devices()
                .into_iter()
                .map(|cert| (hex::encode(cert.device_public_key), cert.device_label))
                .collect();
            entries(store)
                .into_values()
                .filter(|kept| kept.lifetime.is_live_at(now))
                .map(|answer| Kept {
                    device_label: labels.get(&answer.device).cloned().unwrap_or_default(),
                    this_device: answer.device == this,
                    answer,
                })
                .collect()
        })
    }
}

/// The kept answers the person's document holds, by entry.
fn entries(store: &PersonalStore) -> BTreeMap<String, DeviceAnswer> {
    let Some(doc) = store
        .document(DEVICES_APP, &document())
        .and_then(|saved| AutoCommit::load(&saved).ok())
    else {
        return BTreeMap::new();
    };
    doc.map_range(automerge::ROOT, ..)
        .filter_map(|item| {
            let automerge::ValueRef::Scalar(automerge::ScalarValueRef::Str(text)) = item.value
            else {
                return None;
            };
            let answer: DeviceAnswer = serde_json::from_str(&text).ok()?;
            (answer.key() == *item.key).then(|| (item.key.to_string(), answer))
        })
        .collect()
}

/// Change the person's document through `edit`, named `what` in its history, and
/// send it to their other devices.
fn write(
    store: &PersonalSync,
    what: &str,
    edit: impl FnOnce(&mut AutoCommit) -> Result<(), automerge::AutomergeError>,
) -> Result<(), String> {
    use collab_types::CommitNamed as _;
    let id = document();
    store
        .publish(|personal, key, now| {
            let mut doc = match personal.document(DEVICES_APP, &id) {
                Some(saved) => AutoCommit::load(&saved).map_err(|error| error.to_string())?,
                None => AutoCommit::new(),
            };
            edit(&mut doc).map_err(|error| error.to_string())?;
            doc.commit_edit(DEVICE_ANSWERS_KIND, what);
            personal.publish(
                DEVICES_APP,
                &id,
                &DocumentMutation::Snapshot(doc.save()),
                key,
                now,
            )
        })
        .map(drop)
}
