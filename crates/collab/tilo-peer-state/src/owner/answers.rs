//! The writes a device held that its server had not taken, by signature: the
//! record layout an earlier build wrote, read so its journal replays.
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
