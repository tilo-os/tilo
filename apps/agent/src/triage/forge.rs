//! Where an approved issue is filed, and how filing survives a restart.
//!
//! The triage agent never holds a forge token or reaches the network. It
//! names the forge account the team connected in Settings and hands the
//! approved draft to the accounts service over its launcher-issued
//! descriptor ([`tilo_account_service`]); custody files it with the token
//! sealed for that account.
//!
//! Filing is idempotent. Before it files, the agent writes a durable intent
//! ([`Intents`]): the approved draft it is about to file. The filed issue
//! carries the draft's marker, and the service returns an issue that
//! already carries it instead of filing another. An agent that stopped
//! between filing and saying so in the thread finds its intent, finds the
//! issue by its marker, and only replies.

use std::collections::BTreeSet;
use std::path::PathBuf;

use chat::feedback::IssueDraft;
pub use tilo_account_service::FiledIssue;
use tilo_account_service::{Client, ForgeIssue};

/// The marker a draft's issue carries: the room and the draft's message,
/// so the same draft is one issue however often it is filed.
#[must_use]
pub fn marker(room: &str, draft_message: &str) -> String {
    let room: String = room
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect();
    format!("{room}:{draft_message}")
}

/// Something that files an approved issue.
pub trait Forge: Send {
    /// The issue already filed for `marker`, if any.
    ///
    /// # Errors
    ///
    /// Why the forge could not be asked, in words for the log.
    fn find(&self, marker: &str) -> Result<Option<FiledIssue>, String>;

    /// File `draft` marked `marker`, or return the issue already filed for
    /// that marker.
    ///
    /// # Errors
    ///
    /// Why it was not filed, in words for the log.
    fn file(&self, draft: &IssueDraft, marker: &str) -> Result<FiledIssue, String>;
}

type Connect = Box<dyn Fn() -> Option<Client> + Send>;

/// The team's forge account, through the accounts service.
pub struct AccountService {
    account: String,
    connect: Connect,
}

impl AccountService {
    /// The accounts service the launcher handed this process.
    #[must_use]
    pub fn from_env(account: impl Into<String>) -> Self {
        Self::with_connector(account, Box::new(Client::from_env))
    }

    /// The accounts service reached through `connect`, for a host that
    /// connects it itself (a test harness).
    #[must_use]
    pub fn with_connector(account: impl Into<String>, connect: Connect) -> Self {
        Self {
            account: account.into(),
            connect,
        }
    }

    fn client(&self) -> Result<Client, String> {
        (self.connect)().ok_or_else(|| "the accounts service is not available".to_string())
    }
}

impl Forge for AccountService {
    fn find(&self, marker: &str) -> Result<Option<FiledIssue>, String> {
        self.client()?
            .find_forge_issue(&self.account, marker)
            .map_err(|error| error.to_string())
    }

    fn file(&self, draft: &IssueDraft, marker: &str) -> Result<FiledIssue, String> {
        self.client()?
            .file_forge_issue(
                &self.account,
                &ForgeIssue {
                    title: draft.title.clone(),
                    body: draft.body.clone(),
                    marker: marker.to_string(),
                },
            )
            .map_err(|error| error.to_string())
    }
}

/// No forge account connected: drafts can be approved, and filing waits.
#[derive(Clone, Copy, Debug, Default)]
pub struct Unconnected;

impl Forge for Unconnected {
    fn find(&self, _marker: &str) -> Result<Option<FiledIssue>, String> {
        Ok(None)
    }

    fn file(&self, _draft: &IssueDraft, _marker: &str) -> Result<FiledIssue, String> {
        Err("no forge account is connected for triage".into())
    }
}

/// The drafts the agent set out to file and has not yet seen answered in
/// the room, kept on disk so they outlive the process.
#[derive(Debug, Default)]
pub struct Intents {
    path: Option<PathBuf>,
    pending: BTreeSet<String>,
}

impl Intents {
    /// Intents kept in `path`, reading what an earlier run left there.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        let pending = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Self {
            path: Some(path),
            pending,
        }
    }

    /// Intents kept only in memory, for a run with nowhere to write.
    #[must_use]
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// Where the agent keeps its intents for `room`: its own state
    /// directory, which the launcher scopes to this app in this room.
    #[must_use]
    pub fn for_room(room: &str) -> Self {
        let base = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state"))
            });
        match base {
            Some(base) => Self::at(base.join("tilo-agent/triage").join(format!("{room}.json"))),
            None => Self::in_memory(),
        }
    }

    #[must_use]
    pub fn contains(&self, draft_message: &str) -> bool {
        self.pending.contains(draft_message)
    }

    /// Record the intent to file `draft_message`, durably, before filing.
    ///
    /// # Errors
    ///
    /// When the intent cannot be written; the agent then does not file.
    pub fn record(&mut self, draft_message: &str) -> std::io::Result<()> {
        if self.pending.insert(draft_message.to_string()) {
            self.save()?;
        }
        Ok(())
    }

    /// Forget intents the room shows answered.
    pub fn settle(&mut self, answered: &dyn Fn(&str) -> bool) {
        let before = self.pending.len();
        self.pending.retain(|draft| !answered(draft));
        if self.pending.len() != before
            && let Err(error) = self.save()
        {
            tracing::warn!(%error, "triage: could not forget settled filings");
        }
    }

    fn save(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(&self.pending).map_err(std::io::Error::other)?;
        atomic_file::write_creating_parent(path, &bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_intent_outlives_the_process_until_it_is_settled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.json");
        let mut intents = Intents::at(path.clone());
        intents.record("m_draft").unwrap();
        let reread = Intents::at(path.clone());
        assert!(reread.contains("m_draft"));
        let mut reread = reread;
        reread.settle(&|draft| draft == "m_draft");
        assert!(!Intents::at(path).contains("m_draft"));
        assert_eq!(marker("7a1a-9e00", "m_draft"), "7a1a-9e00:m_draft");
    }
}
