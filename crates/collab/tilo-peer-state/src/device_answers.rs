//! The person's answers about the devices of their machines: "Recorder may use
//! the camera on this laptop, always".
//!
//! An answer that outlasts the session is one document of the person's own
//! workspace ([`DEVICE_ANSWERS_KIND`]), one entry per device, app and
//! capability, naming the device it is about. It follows the person to every
//! device of theirs, keeps its history like any of their documents, and any of
//! their devices may take it back. It answers only on the device it names: an
//! answer given on the laptop never opens the phone's camera. An answer for the
//! session stays in this device's memory, and one for once is never kept.
//!
//! A workspace never answers here. What it may do is refuse a device to the
//! apps in it ([`DeviceGrants::refuse_in`]), whatever the person answered.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, OnceLock};

use automerge::transaction::Transactable as _;
use automerge::{AutoCommit, ReadDoc as _};
use collab_types::caps::DeviceCap;
use serde::{Deserialize, Serialize};
use tilo_action_policy::{Answer, Lifetime};
use tilo_lock::MutexRecover as _;

use crate::person_records::PersonDocuments;

/// The app whose documents the person's own records of themselves are: their
/// devices and everything they answered about them.
pub const PERSON_RECORDS_APP: &str = "so.tilo.identity";

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
    /// The person's own workspace, where kept answers live, once this session
    /// opened it: until then none are kept.
    store: Arc<OnceLock<Arc<dyn PersonDocuments>>>,
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
    /// Answers kept in the person's own workspace `store`.
    #[must_use]
    pub fn over(store: Arc<dyn PersonDocuments>) -> Self {
        let grants = Self::unkept();
        grants.keep_in(store);
        grants
    }

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

    /// Keep answers in the person's own workspace `store` from now on, in
    /// every clone: what a session does once it holds the workspace. The first
    /// store stands.
    pub fn keep_in(&self, store: Arc<dyn PersonDocuments>) {
        let _ = self.store.set(store);
    }

    /// Ticks whenever the person's own workspace changed, here or on another of
    /// their devices.
    #[must_use]
    pub fn changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.store.get().map(|store| store.changes())
    }

    fn this_device(store: &dyn PersonDocuments) -> String {
        hex::encode(store.device())
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
        let key = entry_key(&Self::this_device(&**store), app, cap);
        entries(&**store)
            .remove(&key)
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
                let kept = DeviceAnswer {
                    device: Self::this_device(&**store),
                    app: app.to_owned(),
                    cap,
                    answer,
                    lifetime,
                };
                let Ok(value) = serde_json::to_string(&kept) else {
                    return;
                };
                write(store, move |doc| {
                    doc.put(automerge::ROOT, kept.key(), value.clone())
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
            .is_none_or(|store| Self::this_device(&**store) == device);
        let session = this
            && self
                .session
                .lock_recover()
                .remove(&(app.to_owned(), cap))
                .is_some();
        let Some(store) = self.store.get() else {
            return session;
        };
        let key = entry_key(device, app, cap);
        if !entries(&**store).contains_key(&key) {
            return session;
        }
        write(store, move |doc| doc.delete(automerge::ROOT, key.as_str()));
        true
    }

    /// Take back what `app` was answered of `cap` on this device.
    pub fn revoke_here(&self, app: &str, cap: DeviceCap) -> bool {
        let device = self
            .store
            .get()
            .map(|store| Self::this_device(&**store))
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
        let this = Self::this_device(&**store);
        let labels: BTreeMap<String, String> = store
            .devices()
            .into_iter()
            .map(|cert| (hex::encode(cert.device_public_key), cert.device_label))
            .collect();
        entries(&**store)
            .into_values()
            .filter(|kept| kept.lifetime.is_live_at(now))
            .map(|answer| Kept {
                device_label: labels.get(&answer.device).cloned().unwrap_or_default(),
                this_device: answer.device == this,
                answer,
            })
            .collect()
    }
}

/// The kept answers the person's document holds, by entry.
fn entries(store: &dyn PersonDocuments) -> BTreeMap<String, DeviceAnswer> {
    let Some(doc) = store
        .document(PERSON_RECORDS_APP, &document())
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

/// Change the person's document through `edit`, named in its history, and send it to their
/// other devices.
fn write(
    store: &Arc<dyn PersonDocuments>,
    edit: impl Fn(&mut AutoCommit) -> Result<(), automerge::AutomergeError> + Send + Sync + 'static,
) {
    // The answers are the person's: each change names them.
    let person = store.person();
    store.edit(
        PERSON_RECORDS_APP,
        &document(),
        Box::new(move |doc| {
            use collab_types::CommitNamed as _;
            edit(doc).map_err(|error| error.to_string())?;
            doc.commit_edit(DEVICE_ANSWERS_KIND, "answers", &person);
            Ok(())
        }),
    );
}
