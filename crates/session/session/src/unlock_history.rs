//! The person's own record of what their devices answered when one of them asked to
//! be unlocked after a reboot.
//! An answer is a fact the answering device signs and publishes in its own part of
//! the person's device fabric ([`crate::platform::fabric::unlock`]); every one of their
//! devices carries it. What the person reads is their record of kind `device.unlock`, one
//! document whose changes are those answers, so the activity history lists them beside
//! everything else that happened.
//! The record is a projection of the fabric: each device writes the answers it
//! hears under the answering device's actor, at the time it decided.
//! Nothing is invented: an answer enters only if it validates
//! ([`UnlockDecisionEntry::validate`]), and a document this build cannot read is
//! never written over.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::platform::fabric::PersonDevices;
use crate::platform::fabric::{DecisionOutcome, UnlockDecisionEntry};
use crate::platform::fabric::{FabricLink as _, SharedLink};
use automerge::transaction::{CommitOptions, Transactable as _};
use automerge::{ActorId, AutoCommit, ChangeHash, ObjType, OnPartialLoad, ROOT, ReadDoc as _};
use tilo_actions::{ActionApp, ActionContext, ActionFacet, ActionItem, ActionKey, Did, Scalar};
use tilo_session_core::desktop::activity::DetachedRecordHistory;
use ui::resource::{DocumentCheckout, DocumentId, ResourceType};

use crate::i18n::t;
use crate::network::slot_name;

/// The package the answers are listed under in the activity history: this session's own, not one an
/// app is. The words of a row come from the session's catalogue (`unlock-approved`, `unlock-denied`
/// ).
const APP: &str = "so.tilo.unlock";

/// The kind of the person's own record of those answers.
const KIND: &str = crate::platform::fabric::DEVICE_UNLOCK_ACTION;

/// The document `kept` holds, and the writer that keeps it: none for one this build did not open,
/// which is then never written over.
fn open(kept: crate::kept::Kept) -> (AutoCommit, Option<Saver>) {
    let bytes = match kept.read() {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (AutoCommit::new(), Some(Saver::new(kept, Vec::new(), 0))),
        Err(error) => {
            tracing::warn!(%error, "the unlock history is not read");
            return (AutoCommit::new(), None);
        }
    };
    let options = automerge::LoadOptions::new().on_partial_load(OnPartialLoad::Ignore);
    match AutoCommit::load_with_options(&bytes, options) {
        Ok(mut doc) => {
            let heads = doc.get_heads();
            (doc, Some(Saver::new(kept, heads, bytes.len())))
        }
        Err(error) => {
            tracing::warn!(%error, "the unlock history is not opened");
            (AutoCommit::new(), None)
        }
    }
}

/// Keeps the document: each answer appends the changes since the last save, and the document is
/// written whole again once what was appended outgrows it.
struct Saver {
    kept: crate::kept::Kept,
    heads: Vec<ChangeHash>,
    plan: app_state::SavePlan,
}

impl Saver {
    fn new(kept: crate::kept::Kept, heads: Vec<ChangeHash>, whole: usize) -> Self {
        Self {
            kept,
            heads,
            plan: app_state::SavePlan::new(whole, 0),
        }
    }

    fn save(&mut self, doc: &mut AutoCommit) {
        let written = match self.plan.next() {
            app_state::Save::Full => {
                let bytes = doc.save();
                let len = bytes.len();
                self.kept
                    .replace(bytes)
                    .map(|_| self.plan.stored_whole(len))
            }
            app_state::Save::Append => {
                let bytes = doc.save_after(&self.heads);
                let len = bytes.len();
                self.kept.append(bytes).map(|_| self.plan.appended(len))
            }
        };
        if written.is_some() {
            self.heads = doc.get_heads();
        }
    }
}

pub(crate) struct UnlockHistory {
    doc: RefCell<AutoCommit>,
    /// What keeps the document: `None` on a session that keeps no state, and on one that did not
    /// open the document it kept.
    saver: RefCell<Option<Saver>>,
    /// The person's own record the answers belong to. `None` on a session with no certified person:
    /// it is on no fabric, so nothing answers.
    record: Option<DocumentCheckout>,
    /// The person's fabric, while this session takes part in one.
    link: RefCell<Option<SharedLink>>,
    /// The answers already in the document, by the key each is kept under.
    kept: RefCell<BTreeSet<String>>,
}

impl UnlockHistory {
    /// The record under `state` , read into from what an earlier session kept there, and holding
    /// nothing on a device that answered nothing yet. `person` is who sits here: the record is
    /// theirs, on every one of their devices.
    pub(crate) fn new(state: Option<&Path>, person: &str) -> Self {
        let record = DocumentId::of_member(person, KIND)
            .map(|id| DocumentCheckout::new(id, ResourceType::new(KIND)));
        let (doc, saver) = match state {
            Some(state) => open(crate::kept::Kept::open(state, crate::kept::UNLOCK)),
            None => (AutoCommit::new(), None),
        };
        let kept = doc.keys(ROOT).collect();
        Self {
            doc: RefCell::new(doc),
            saver: RefCell::new(saver),
            record,
            link: RefCell::default(),
            kept: RefCell::new(kept),
        }
    }

    /// Take part in `link` 's fabric: the answers it carries are written into the person's record.
    pub(crate) fn watch(&self, link: SharedLink) {
        *self.link.borrow_mut() = Some(link);
    }

    /// Write what the person's fabric holds now into the record: every answer that validates
    /// against the device that published it and is not in the document yet, as one change by the
    /// device that answered and at the time it did. Whether the record holds an answer it did not
    /// before.
    pub(crate) fn absorb(&self) -> bool {
        if self.record.is_none() {
            return false;
        }
        let Some(link) = self.link.borrow().clone() else {
            return false;
        };
        let mut wrote = false;
        let fabric = link.devices();
        for (owner, device) in fabric.devices() {
            for answer in &device.unlock.decisions {
                // Only the device whose part carries it signed it: another device's answer is in
                // that device's own part of the person's store.
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
                if let Some(saver) = self.saver.borrow_mut().as_mut() {
                    saver.save(&mut doc);
                }
                drop(doc);
                self.kept.borrow_mut().insert(key);
                wrote = true;
            }
        }
        wrote
    }

    /// The person's record, for a reader that lists what happened: `None` until one of their
    /// devices answered something, and on a session with no certified person.
    pub(crate) fn history(&self) -> Option<DetachedRecordHistory> {
        let checkout = self.record.clone()?;
        let doc = self.doc.borrow();
        (!doc.base_heads().is_empty()).then(|| DetachedRecordHistory {
            app: APP.to_owned(),
            checkout,
            tree_chunks: Vec::new(),
            document: Some(doc.clone()),
        })
    }

    /// The changes the record is at, so a reader can tell it moved.
    pub(crate) fn heads(&self) -> Vec<ChangeHash> {
        self.doc.borrow().base_heads()
    }

    /// How the devices that answered are known: each by the name it gives itself in the person's
    /// store.
    pub(crate) fn authors(&self) -> BTreeMap<String, String> {
        let Some(link) = self.link.borrow().clone() else {
            return BTreeMap::new();
        };
        let fabric = link.devices();
        fabric
            .devices()
            .iter()
            .filter(|(_, device)| !device.unlock.decisions.is_empty())
            .map(|(owner, device)| (owner.as_str().to_owned(), device.about.label.clone()))
            .collect()
    }

    /// This device, as its own answers are written here: `None` until the session takes part in a
    /// fabric as a certified device.
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
    /// The enrolled device's key (hex), empty while no device of the person's publishes the
    /// enrolment any more.
    device: String,
    label: String,
}

/// The device `answer` 's boot belongs to, by the enrolment it names.
fn boot_of(fabric: &PersonDevices, answer: &UnlockDecisionEntry) -> Boot {
    for known in fabric.devices().values() {
        for enrolment in &known.unlock.enrolments {
            if enrolment.id() != Some(answer.boot_cert_digest) {
                continue;
            }
            let device = hex::encode(enrolment.device());
            let label = fabric
                .devices()
                .iter()
                .find(|(id, _)| id.as_str() == device)
                .map_or_else(
                    || t("unlock-history-unknown-device"),
                    |(_, known)| known.about.label.clone(),
                );
            return Boot { device, label };
        }
    }
    Boot {
        device: String::new(),
        label: t("unlock-history-unknown-device"),
    }
}

/// The key an answer is kept under: when it was made, which device made it and which boot it
/// answers. The same device answering the same boot again — after its answer ran out — is another
/// answer, and another entry.
fn key_of(answer: &UnlockDecisionEntry) -> String {
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

/// How the change `answer` makes reads in the history: "Disk unlock: Approved unlocking The phone",
/// by the device that answered, about the boot that asked and the disk it asked for.
fn context_of(answer: &UnlockDecisionEntry, boot: &Boot) -> ActionContext {
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

/// Write `answer` into `doc` as the entry `key` names: the facts a reader of the person's record
/// asks for, as plain values. Whether it was written.
fn write_entry(doc: &mut AutoCommit, key: &str, answer: &UnlockDecisionEntry, boot: &Boot) -> bool {
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
