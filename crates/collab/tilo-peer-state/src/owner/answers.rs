//! The writes a device holds that its server has not taken, by signature: what a compaction of
//! a document's history never covers.
use super::publication::StoredPublication;
use super::stored;
use collab_types::SignedOp;
use serde::{Deserialize, Serialize};

/// A write's signature, as base64 text like the file's other opaque bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct AnswerSignature(#[serde(with = "crate::owner::stored::base64_bytes")] Vec<u8>);
impl AnswerSignature {
    pub(super) fn of(write: &SignedOp) -> Self {
        Self(write.signature.0.to_vec())
    }
}

impl StoredPublication {
    /// Keep `write`, this device's, as one its server has not taken.
    pub(super) fn unanswered_write(&mut self, write: &SignedOp) {
        let signature = AnswerSignature::of(write);
        if !self.unanswered.contains(&signature) {
            self.unanswered.push(signature);
        }
    }

    /// Take `write`, this device's, out of what its server has not taken.
    pub(super) fn answered_write(&mut self, write: &SignedOp) {
        let signature = AnswerSignature::of(write);
        self.unanswered.retain(|kept| *kept != signature);
    }

    /// Whether `write`, this device's, waits for its server to take it.
    pub(super) fn unanswered(&self, write: &SignedOp) -> bool {
        self.unanswered.contains(&AnswerSignature::of(write))
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
