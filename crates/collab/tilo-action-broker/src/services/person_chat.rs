//! Person-only native host capability. No request supplies a caller or a role.
use std::sync::Arc;

/// The host must read and commit through its actual joined-workspace owner.
pub trait PersonChatServices: Send + Sync {
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
    /// One page of a conversation still held in the person's old private
    /// store, read so that the conversations they belong to can import it.
    /// Never a write.
    Import {
        public_key: String,
        after_sequence: u64,
    },
    /// Empty the person's old private store: the conversations it held are in
    /// their records, and nothing reads or writes it again.
    Clear,
}
// An internally tagged unit variant accepts extra fields even with
// deny_unknown_fields. Decode unit commands as empty structs to close that door.
#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum ClosedPersonChatCommand {
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
            Self::Import { .. } => "import-direct-conversation",
            Self::Clear => "clear-private-archive",
        }
    }
}

pub type PersonChatService = Arc<dyn PersonChatServices>;
