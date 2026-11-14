//! A workspace id carried as its UUID text, in every encoding.
//!
//! Delegations name their workspaces this way: the 0.1 releases signed and
//! stored them with the UUID as text, and a delegation is signed by its
//! person's root, which no host holds to sign it again. Kept as text, every
//! delegation a release stored decodes and verifies as it is.

use collab_types::WorkspaceId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

fn parse<E: serde::de::Error>(text: &str) -> Result<WorkspaceId, E> {
    WorkspaceId::parse(text).ok_or_else(|| E::custom("a workspace id is not a UUID"))
}

pub(crate) fn serialize<S: Serializer>(
    workspace: &WorkspaceId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    workspace.to_string().serialize(serializer)
}

pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<WorkspaceId, D::Error> {
    parse(&String::deserialize(deserializer)?)
}

/// A list of workspace ids, each as its text.
pub(crate) mod list {
    use super::{Deserialize, Deserializer, Serialize, Serializer, WorkspaceId, parse};

    pub(crate) fn serialize<S: Serializer>(
        workspaces: &[WorkspaceId],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        workspaces
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<WorkspaceId>, D::Error> {
        Vec::<String>::deserialize(deserializer)?
            .iter()
            .map(|text| parse(text))
            .collect()
    }
}
