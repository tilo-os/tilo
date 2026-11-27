//! Reactions as a reader sees them: who answered something with which emoji.
//! The record a thing keeps its reactions in is the resource model's; what a
//! reaction strip shows is plain data any view can draw.

use tilo_member_types::MemberId;

/// One person (or outside account) among those who reacted with an emoji.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reactor {
    /// The member who invoked the reaction.
    pub by: MemberId,
    /// The outside account a bot reacted for.
    pub persona: Option<String>,
    /// The name that account shows.
    pub name: Option<String>,
}

/// Everyone who reacted with one emoji, first reaction first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReactionGroup {
    pub emoji: String,
    pub reactors: Vec<Reactor>,
    /// When the first of them reacted.
    pub first_at: i64,
}

impl ReactionGroup {
    #[must_use]
    pub fn count(&self) -> usize {
        self.reactors.len()
    }

    /// Whether `member` is among them themselves.
    #[must_use]
    pub fn includes(&self, member: &MemberId) -> bool {
        self.reactors
            .iter()
            .any(|reactor| reactor.persona.is_none() && reactor.by == *member)
    }
}
