//! Person-only native host capability. No request supplies a caller or a role.
use std::sync::Arc;

/// The host must read and commit through its actual joined-workspace owner.
pub trait PersonChatServices: Send + Sync {
    /// Freshly authorize the pair and return its private, body-free change lane.
    fn subscribe(
        &self,
        _workspace: &str,
        _caller: &tilo_user_identity::DeviceCert,
        _peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        Err("private conversation subscriptions unavailable".into())
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
    Contacts,
    /// The pairs this person is in, which the workspace proves.
    Pairs,
    Open {
        public_key: String,
    },
    Read {
        public_key: String,
    },
    Send {
        public_key: String,
        text: String,
        invocation: uuid::Uuid,
    },
    /// Announce the pair this device's person shares with `public_key`: the
    /// one write that makes their record the two people's.
    Announce {
        public_key: String,
    },
    /// One page of a conversation still held in the person's old private
    /// store, read so that pair records can import it. Never a write.
    Import {
        public_key: String,
        after_sequence: u64,
    },
}
// An internally tagged unit variant accepts extra fields even with
// deny_unknown_fields. Decode Contacts as an empty struct to close that door.
#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum ClosedPersonChatCommand {
    Contacts {},
    Pairs {},
    Open {
        public_key: String,
    },
    Read {
        public_key: String,
    },
    Send {
        public_key: String,
        text: String,
        invocation: uuid::Uuid,
    },
    Announce {
        public_key: String,
    },
    Import {
        public_key: String,
        after_sequence: u64,
    },
}

impl<'de> serde::Deserialize<'de> for PersonChatCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(
            match <ClosedPersonChatCommand as serde::Deserialize>::deserialize(deserializer)? {
                ClosedPersonChatCommand::Contacts {} => Self::Contacts,
                ClosedPersonChatCommand::Pairs {} => Self::Pairs,
                ClosedPersonChatCommand::Open { public_key } => Self::Open { public_key },
                ClosedPersonChatCommand::Read { public_key } => Self::Read { public_key },
                ClosedPersonChatCommand::Send {
                    public_key,
                    text,
                    invocation,
                } => Self::Send {
                    public_key,
                    text,
                    invocation,
                },
                ClosedPersonChatCommand::Announce { public_key } => Self::Announce { public_key },
                ClosedPersonChatCommand::Import {
                    public_key,
                    after_sequence,
                } => Self::Import {
                    public_key,
                    after_sequence,
                },
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
            Self::Contacts => "contacts",
            Self::Pairs => "pairs",
            Self::Open { .. } => "open-direct-conversation",
            Self::Read { .. } => "read-direct-conversation",
            Self::Send { .. } => "send-direct-message",
            Self::Announce { .. } => "announce-direct-pair",
            Self::Import { .. } => "import-direct-conversation",
        }
    }
}

pub type PersonChatService = Arc<dyn PersonChatServices>;
