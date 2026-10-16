//! Person-only native host capability. No request supplies a caller or a role.
use std::sync::Arc;

/// The host must read and commit through its actual joined-room owner.
pub trait PersonChatServices: Send + Sync {
    /// Freshly authorize the pair and return its private, body-free change lane.
    ///
    /// # Errors
    /// Returns an error if subscriptions are unavailable or current pair authorization fails.
    fn subscribe(
        &self,
        _room: &str,
        _caller: &tilo_user_identity::DeviceCert,
        _peer: &str,
    ) -> Result<tokio::sync::watch::Receiver<u64>, String> {
        Err("private conversation subscriptions unavailable".into())
    }

    /// Execute a closed person-chat command with transport-certified authority.
    ///
    /// # Errors
    /// Returns an error if the command is invalid, authorization fails, or the owner is unavailable.
    fn command(
        &self,
        room: &str,
        caller: &tilo_user_identity::DeviceCert,
        command: &[u8],
    ) -> Result<Vec<u8>, String>;
}

/// Closed wire vocabulary, independent of the model-callable action registry.
#[derive(Clone, serde::Serialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PersonChatCommand {
    Contacts,
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
}
// An internally tagged unit variant accepts extra fields even with
// deny_unknown_fields. Decode Contacts as an empty struct to close that door.
#[derive(serde::Deserialize)]
#[serde(tag = "operation", rename_all = "kebab-case", deny_unknown_fields)]
enum ClosedPersonChatCommand {
    Contacts {},
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
}

impl<'de> serde::Deserialize<'de> for PersonChatCommand {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(
            match <ClosedPersonChatCommand as serde::Deserialize>::deserialize(deserializer)? {
                ClosedPersonChatCommand::Contacts {} => Self::Contacts,
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
            Self::Open { .. } => "open-direct-conversation",
            Self::Read { .. } => "read-direct-conversation",
            Self::Send { .. } => "send-direct-message",
        }
    }
}

pub type PersonChatService = Arc<dyn PersonChatServices>;
