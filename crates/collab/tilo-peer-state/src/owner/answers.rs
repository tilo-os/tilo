//! The writes a device holds that its server has not taken, by signature: what a compaction of
//! a document's history never covers.
use super::publication::StoredPublication;
use super::stored;
use collab_types::SignedOp;
use serde::{Deserialize, Serialize};

/// A write's signature, as base64 text like the file's other opaque bytes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct AnswerSignature(#[serde(with = "crate::owner::stored::base64_bytes")] Vec<u8>);

/// The writes a device holds that its server has not taken, by signature.
pub(crate) type Unanswered = crate::ordered::Ordered<AnswerSignature, ()>;

/// [`Unanswered`] as the list of signatures the workspace's file holds.
pub(super) mod unanswered_list {
    use super::{AnswerSignature, Unanswered};
    use serde::{Deserialize, Deserializer, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        unanswered: &Unanswered,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(unanswered.keys())
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Unanswered, D::Error> {
        Ok(Vec::<AnswerSignature>::deserialize(deserializer)?
            .into_iter()
            .map(|signature| (signature, ()))
            .collect())
    }
}
impl AnswerSignature {
    pub(super) fn of(write: &SignedOp) -> Self {
        Self(write.signature.0.to_vec())
    }
}

impl StoredPublication {
    /// Keep `write`, this device's, as one its server has not taken.
    pub(super) fn unanswered_write(&mut self, write: &SignedOp) {
        self.unanswered.insert(AnswerSignature::of(write), ());
    }

    /// Take `write`, this device's, out of what its server has not taken.
    pub(super) fn answered_write(&mut self, write: &SignedOp) {
        self.unanswered.remove(&AnswerSignature::of(write));
    }

    /// Whether `write`, this device's, waits for its server to take it.
    pub(super) fn unanswered(&self, write: &SignedOp) -> bool {
        self.unanswered.contains_key(&AnswerSignature::of(write))
    }
}

/// Keep `write`, this device's, as one its server has not taken yet.
pub(crate) fn note_unanswered(
    view: &mut stored::StoredWorkspace,
    write: &SignedOp,
) -> Result<(), String> {
    view.publication
        .as_mut()
        .ok_or("publication absent")?
        .unanswered_write(write);
    Ok(())
}
