//! The person's own record of what their devices answered when one of them asked to
//! be unlocked after a reboot.
//! An answer is a fact the answering device signs and publishes in its own part of
//! the person's device fabric ([`tilo_device_fabric::unlock`]); every one of their
//! devices carries it. What the person reads is `member:<person>:device.unlock`,
//! one document whose changes are those answers, so the activity history lists them
//! beside everything else that happened.
//! The record is a projection of the fabric: each device writes the answers it
//! hears under the answering device's actor, at the time it decided.
//! Nothing is invented: an answer enters only if it validates
//! ([`UnlockDecisionRecord::validate`]), and a document this build cannot read is
//! never written over.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{ActorId, AutoCommit, ChangeHash, ObjType, OnPartialLoad, ROOT, ReadDoc as _};
use tilo_actions::{ActionApp, ActionContext, ActionFacet, ActionItem, ActionKey, Did, Scalar};
use tilo_desktop_surface::activity::DetachedRecordHistory;
use tilo_device_fabric::DeviceFabric;
use tilo_device_fabric::link::{FabricLink as _, SharedLink};
use tilo_device_fabric::unlock::{DecisionOutcome, UnlockDecisionRecord};
use ui::record::{RecordCheckout, RecordId, RecordKind};

use crate::i18n::t;
use crate::network::slot_name;

/// The document beside the rest of this device's state.
const FILE: &str = "device-unlock.automerge";

/// The package the answers are listed under in the activity history: this
/// session's own, not one an app is. The words of a row come from the
/// session's catalogue (`unlock-approved`, `unlock-denied`).
const APP: &str = "so.tilo.unlock";

/// The kind of the person's own record of those answers.
const KIND: &str = tilo_device_fabric::unlock::DEVICE_UNLOCK_ACTION;

/// Where the document lives under `state`, this device's state directory.
fn path(state: &Path) -> PathBuf {
    state.join("tilo").join(FILE)
}

/// The document kept at `at`, and where it is kept: `None` there for one
/// this build did not open, which is then never written over.
fn open(at: &Path) -> (AutoCommit, Option<PathBuf>) {
    let bytes = match std::fs::read(at) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (AutoCommit::new(), Some(at.to_path_buf()));
        }
        Err(error) => {
            tracing::warn!(path = %at.display(), %error, "the unlock record is not read");
            return (AutoCommit::new(), None);
        }
    };
    let options = automerge::LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&bytes, options) {
        Ok(doc) => (doc, Some(at.to_path_buf())),
        Err(error) => {
            tracing::warn!(path = %at.display(), %error, "the unlock record is not opened");
            (AutoCommit::new(), None)
        }
    }
}

pub(crate) struct UnlockHistory {
    doc: RefCell<AutoCommit>,
    /// Where the document is kept: `None` on a session that keeps no state,
    /// and on one that did not open the document on disk.
    path: Option<PathBuf>,
    /// The person's own record the answers belong to. `None` on a session
    /// with no certified person: it is on no fabric, so nothing answers.
    record: Option<RecordCheckout>,
    /// The person's fabric, while this session takes part in one.
    link: RefCell<Option<SharedLink>>,
    /// The answers already in the document, by the key each is kept under.
    kept: RefCell<BTreeSet<String>>,
}

impl UnlockHistory {
    /// The record under `state`, read into from what an earlier session kept
    /// there, and holding nothing on a device that answered nothing yet.
    /// `person` is who sits here: the record is theirs, on every one of
    /// their devices.
    pub(crate) fn new(state: Option<&Path>, person: &str) -> Self {
        let record = RecordId::of_member(person, KIND)
            .map(|id| RecordCheckout::new(id, RecordKind::new(KIND)));
        let (doc, path) = match state {
            Some(state) => open(&path(state)),
            None => (AutoCommit::new(), None),
        };
        let kept = doc.keys(ROOT).collect();
        Self {
            doc: RefCell::new(doc),
            path,
            record,
            link: RefCell::default(),
            kept: RefCell::new(kept),
        }
    }

    /// Take part in `link`'s fabric: the answers it carries are written into
    /// the person's record.
    pub(crate) fn watch(&self, link: SharedLink) {
        *self.link.borrow_mut() = Some(link);
    }

    /// Write what the person's fabric holds now into the record: every answer
    /// that validates against the device that published it and is not in the
    /// document yet, as one change by the device that answered and at the
    /// time it did. Whether the record holds an answer it did not before.
    pub(crate) fn absorb(&self) -> bool {
        if self.record.is_none() {
            return false;
        }
        let Some(link) = self.link.borrow().clone() else {
            return false;
        };
        let mut wrote = false;
        let fabric = link.device_fabric();
        for (owner, snapshot) in fabric.snapshots() {
            for answer in &snapshot.unlock.decisions {
                // Only the device whose snapshot carries it signed it: another
                // device's answer is in that device's own snapshot.
                if answer.validate(owner).is_err() {
                    continue;
                }
                let key = key_of(answer);
                if self.kept.borrow().contains(&key) {
                    continue;
                }
                let boot = boot_of(&fabric, answer);
                let Some(message) = context_of(answer, &boot).change_message() else {
                    continue;
                };
                let mut doc = self.doc.borrow_mut();
                doc.set_actor(ActorId::from(&answer.decided_by[..]));
                if !write_entry(&mut doc, &key, answer, &boot) {
                    doc.rollback();
                    continue;
                }
                let options = CommitOptions::default()
                    .with_time(seconds(answer.decided_at_ns))
                    .with_message(message);
                if doc.commit_with(options).is_none() {
                    continue;
                }
                if let Some(at) = &self.path {
                    tilo_durable_writer::keep(at.clone(), doc.save());
                }
                drop(doc);
                self.kept.borrow_mut().insert(key);
                wrote = true;
            }
        }
        wrote
    }

    /// The person's record, for a reader that lists what happened: `None`
    /// until one of their devices answered something, and on a session with
    /// no certified person.
    pub(crate) fn history(&self) -> Option<DetachedRecordHistory> {
        let checkout = self.record.clone()?;
        let doc = self.doc.borrow();
        (!doc.base_heads().is_empty()).then(|| DetachedRecordHistory {
            app: APP.to_owned(),
            checkout,
            document: Some(doc.clone()),
        })
    }

    /// The changes the record is at, so a reader can tell it moved.
    pub(crate) fn heads(&self) -> Vec<ChangeHash> {
        self.doc.borrow().base_heads()
    }

    /// How the devices that answered are known: each by the name its own
    /// snapshot gives it.
    pub(crate) fn authors(&self) -> BTreeMap<String, String> {
        let Some(link) = self.link.borrow().clone() else {
            return BTreeMap::new();
        };
        let fabric = link.device_fabric();
        fabric
            .snapshots()
            .iter()
            .filter(|(_, snapshot)| !snapshot.unlock.decisions.is_empty())
            .map(|(owner, snapshot)| (owner.as_str().to_owned(), snapshot.presence.label.clone()))
            .collect()
    }

    /// This device, as its own answers are written here: `None` until the
    /// session takes part in a fabric as a certified device.
    pub(crate) fn local_actor(&self) -> Option<String> {
        let link = self.link.borrow().clone()?;
        link.local_device_fabric_identity()
            .map(|(device, _)| device.as_str().to_owned())
    }

    /// Whether `id` names the person's record, which no app opens.
    pub(crate) fn holds(&self, id: &str) -> bool {
        self.record
            .as_ref()
            .is_some_and(|record| record.id.as_str() == id)
    }
}

/// The device a boot belongs to, as the person's fabric names it.
struct Boot {
    /// The enrolled device's key (hex), empty while no device of the
    /// person's publishes the enrolment any more.
    device: String,
    label: String,
}

/// The device `answer`'s boot belongs to, by the enrolment it names.
fn boot_of(fabric: &DeviceFabric, answer: &UnlockDecisionRecord) -> Boot {
    for snapshot in fabric.snapshots().values() {
        for enrolment in &snapshot.unlock.enrolments {
            if enrolment.id() != Some(answer.boot_cert_digest) {
                continue;
            }
            let device = hex::encode(enrolment.cert.device_public_key);
            let label = fabric
                .snapshots()
                .values()
                .find(|snapshot| snapshot.owner.as_str() == device)
                .map_or_else(
                    || t("unlock-history-unknown-device"),
                    |snapshot| snapshot.presence.label.clone(),
                );
            return Boot { device, label };
        }
    }
    Boot {
        device: String::new(),
        label: t("unlock-history-unknown-device"),
    }
}

/// The key an answer is kept under: when it was made, which device made it
/// and which boot it answers. The same device answering the same boot again —
/// after its answer ran out — is another answer, and another entry.
fn key_of(answer: &UnlockDecisionRecord) -> String {
    format!(
        "{}-{}-{}",
        answer.decided_at_ns,
        hex::encode(answer.decided_by),
        hex::encode(answer.boot_id.0),
    )
}

/// What the person answered, as the action and the row's words name it.
const fn outcome(answer: DecisionOutcome) -> &'static str {
    match answer {
        DecisionOutcome::Approve => "approved",
        DecisionOutcome::Deny => "denied",
    }
}

/// How the change `answer` makes reads in the history: "Disk unlock:
/// Approved unlocking The phone", by the device that answered, about the boot
/// that asked and the disk it asked for.
fn context_of(answer: &UnlockDecisionRecord, boot: &Boot) -> ActionContext {
    let outcome = outcome(answer.outcome);
    ActionContext {
        app: Some(ActionApp {
            id: APP.to_owned(),
            label: t("unlock-history-app"),
        }),
        item: Some(ActionItem {
            id: if boot.device.is_empty() {
                hex::encode(answer.boot_cert_digest)
            } else {
                boot.device.clone()
            },
            label: boot.label.clone(),
        }),
        what: Some(Did::verb(
            ActionKey::new(APP, KIND, outcome),
            std::iter::empty::<(String, Scalar)>(),
        )),
        facets: vec![
            ActionFacet::new("device", hex::encode(answer.decided_by)),
            ActionFacet::new("boot", hex::encode(answer.boot_id.0)),
            ActionFacet::new("slot", slot_name(&answer.slot)),
            ActionFacet::new("outcome", outcome),
        ],
        ..ActionContext::default()
    }
}

/// Write `answer` into `doc` as the entry `key` names: the facts a reader of
/// the person's record asks for, as plain values. Whether it was written.
fn write_entry(
    doc: &mut AutoCommit,
    key: &str,
    answer: &UnlockDecisionRecord,
    boot: &Boot,
) -> bool {
    let Ok(entry) = doc.put_object(ROOT, key, ObjType::Map) else {
        return false;
    };
    let facts = [
        ("device", hex::encode(answer.decided_by)),
        ("boot", hex::encode(answer.boot_id.0)),
        ("boot-cert", hex::encode(answer.boot_cert_digest)),
        ("boot-device", boot.device.clone()),
        ("slot", slot_name(&answer.slot)),
        ("outcome", outcome(answer.outcome).to_owned()),
    ];
    facts
        .into_iter()
        .all(|(name, value)| doc.put(&entry, name, value).is_ok())
        && doc
            .put(&entry, "at-ns", nanos(answer.decided_at_ns))
            .is_ok()
        && doc
            .put(&entry, "until-ns", nanos(answer.valid_until_ns))
            .is_ok()
}

/// `value` as a change of a document holds a number.
fn nanos(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// The time `answer` was made, as a change of a document names it.
fn seconds(value: u64) -> i64 {
    i64::try_from(value / 1_000_000_000).unwrap_or(i64::MAX)
}
