//! Who a message sent from a window goes to: a person, an agent the workspace keeps, an agent the
//! send makes, a conversation, or the one place a report goes.
//!
//! It is an action argument ([`crate::Recipient`] travels in a typed effect's parameters), so it
//! lives with the other argument values an app publishes, and the window that shows the chip and
//! the shell that delivers the message read the one definition.

use serde::{Deserialize, Serialize};

/// What a recipient of a sent message is: someone the search found, or the agent the person is
/// making by sending.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecipientKind {
    /// An agent this send makes, kept by the person sending.
    NewAgent,
    /// An agent the workspace keeps and the search found.
    Agent,
    /// A person the workspace knows, by their root identity.
    Person,
    /// A conversation, opened where it is and posted in.
    Conversation,
    /// The one place a report about Tilo itself goes, which the session names
    /// when it opens the box there: the search never finds it, and it never
    /// sits beside another recipient.
    Feedback,
}

impl RecipientKind {
    /// The word a row of the box shows for it, when it needs one.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::NewAgent => "New agent",
            Self::Agent => "Agent",
            Self::Person => "Person",
            Self::Conversation => "Conversation",
            Self::Feedback => "Feedback",
        }
    }
}

/// One recipient of a message sent from a window: who it goes to, and what the person reads on its
/// chip.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recipient {
    pub kind: RecipientKind,
    /// The agent's id, the person's root identity or the conversation's id; empty for
    /// [`RecipientKind::NewAgent`], which the shell makes.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    /// The name the chip shows: the agent's or the person's, the conversation's, or the name the new
    /// agent takes — empty for a new agent nobody named, which the shell names by what it is asked.
    pub name: String,
    /// A line under the name in the search's rows (who keeps an agent, who
    /// is in a conversation); never on the chip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl Recipient {
    /// The agent this send makes, named `name`.
    #[must_use]
    pub fn new_agent(name: impl Into<String>) -> Self {
        Self {
            kind: RecipientKind::NewAgent,
            id: String::new(),
            name: name.into(),
            detail: None,
        }
    }

    /// Someone or something the search found.
    #[must_use]
    pub fn found(kind: RecipientKind, id: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
            name: name.into(),
            detail: None,
        }
    }

    /// The same recipient with a line under its name.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        let detail = detail.into();
        self.detail = (!detail.trim().is_empty()).then_some(detail);
        self
    }
}

/// The recipients of `all` that `query` names, best first: what is called exactly that, then what
/// a name starts with it, then a word of a name, then the conversations whose people hold it, then
/// the rest. Among equals `all`'s own order (the most recent first) stands. An empty `query` names
/// none.
#[must_use]
pub fn recipients_matching(all: &[Recipient], query: &str) -> Vec<Recipient> {
    let needle = query.trim().to_lowercase();
    let rank = |recipient: &Recipient| {
        let name = recipient.name.to_lowercase();
        if name == needle {
            return Some(4);
        }
        if name.starts_with(&needle) {
            return Some(3);
        }
        if name
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| word.starts_with(&needle))
        {
            return Some(2);
        }
        let detail = recipient
            .detail
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        needle
            .split_whitespace()
            .all(|word| name.contains(word) || detail.contains(word))
            .then_some(u8::from(recipient.kind == RecipientKind::Conversation))
    };
    if needle.is_empty() {
        return Vec::new();
    }
    let mut found: Vec<(u8, &Recipient)> = all
        .iter()
        .filter_map(|recipient| Some((rank(recipient)?, recipient)))
        .collect();
    found.sort_by_key(|(rank, _)| std::cmp::Reverse(*rank));
    found
        .into_iter()
        .take(12)
        .map(|(_, recipient)| recipient.clone())
        .collect()
}
