//! Person-only native host capability. No request supplies a caller or a role.
use std::sync::Arc;

/// The host must read and commit through its actual joined-workspace owner.
pub trait PersonChatServices: Send + Sync {
    /// The pairs `caller`'s person is in, in `workspace`
    /// ([`collab_types::direct_pair_id`]): the records their devices read and
    /// change, whether or not one of them is open here. What the app is told
    /// when the workspace's governance changes.
    fn pairs(
        &self,
        _workspace: &str,
        _caller: &tilo_user_identity::DeviceCert,
    ) -> Result<Vec<String>, String> {
        Err("private pair list unavailable".into())
    }

    /// Execute a closed person-chat command with transport-certified authority.
    fn command(
        &self,
        workspace: &str,
        caller: &tilo_user_identity::DeviceCert,
        command: &[u8],
    ) -> Result<Vec<u8>, String>;
}

/// Closed wire vocabulary, independent of the model-callable action registry.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PersonChatCommand {
    /// The pairs this person is in, which the workspace proves.
    Pairs,
    /// Announce the pair this device's person shares with `public_key`: the
    /// one write that makes their record the two people's.
    Announce { public_key: String },
    /// One page of a conversation still held in the person's old private
    /// store, read so that pair records can import it. Never a write.
    Import {
        public_key: String,
        after_sequence: u64,
    },
    /// Empty the person's old private store: the conversations it held are in
    /// the pair records, and nothing reads or writes it again.
    Clear,
}
// An internally tagged unit variant accepts extra fields even with
// deny_unknown_fields. Decode unit commands as empty structs to close that door.
#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum ClosedPersonChatCommand {
    Pairs {},
    Announce {
        public_key: String,
    },
    Import {
        public_key: String,
        after_sequence: u64,
    },
    Clear {},
}

impl<'de> serde::Deserialize<'de> for PersonChatCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(
            match <ClosedPersonChatCommand as serde::Deserialize>::deserialize(deserializer)? {
                ClosedPersonChatCommand::Pairs {} => Self::Pairs,
                ClosedPersonChatCommand::Announce { public_key } => Self::Announce { public_key },
                ClosedPersonChatCommand::Import {
                    public_key,
                    after_sequence,
                } => Self::Import {
                    public_key,
                    after_sequence,
                },
                ClosedPersonChatCommand::Clear {} => Self::Clear,
            },
        )
    }
}

impl std::fmt::Debug for PersonChatCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonChatCommand")
            .field("operation", &self.action())
            .finish_non_exhaustive()
    }
}

impl PersonChatCommand {
    pub fn action(&self) -> &'static str {
        match self {
            Self::Pairs => "pairs",
            Self::Announce { .. } => "announce-direct-pair",
            Self::Import { .. } => "import-direct-conversation",
            Self::Clear => "clear-private-archive",
        }
    }
}

pub type PersonChatService = Arc<dyn PersonChatServices>;
