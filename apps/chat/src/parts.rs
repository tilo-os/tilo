//! The chat's messages as a chain of parts.
//!
//! One resource cannot hold a chat that runs for months: a resource's snapshot
//! has a size limit, and a newcomer would be sent all of it. So the workspace's
//! chat resource ([`crate::chat::Chat`]) keeps categories, channels, people,
//! drafts and moderation, and its messages live in numbered part resources
//! ([`ChatMessages`]), each a resource of its own ([`part`]).
//!
//! - **Which part is current.** The part with the highest number a device
//!   holds is the chain's head. A workspace's host replays a joining device the
//!   chat resource and the head only; every earlier part is on-demand
//!   (`neighbour_wire::document_sync`): a device opens it, through the normal
//!   resource sync and the chat resource's grants, when someone scrolls back to
//!   it.
//! - **Where a post goes.** The framework places each message
//!   ([`ui::framework::Chained`]): a post with the newest, a reply, an edit,
//!   a reaction or a report with the message it belongs to.
//! - **Read marks** are positions in the chain ([`ReadMark`]), so they mean
//!   the same whether or not a device holds the earlier parts.
//! - **Counts of the parts before.** A part carries, for each part before
//!   it, how many top-level messages each channel has there
//!   ([`ChatMessages::earlier`]): whoever starts it writes them from the part
//!   before, which they hold. A reader's unread counts reach back to their
//!   mark through the head's, however many parts they were away for,
//!   without holding those parts.
//!
//! This module is the contract the model, the window and the workspace's hosts
//! (bots, seeding, bridges, moderation) share; change it only with all of
//! them.

use std::collections::BTreeMap;

use ui::resource::DocumentId;
use ui::tilo_resource::{self, Collection, Model};

use crate::chat::Message;

/// The chain the chat's message parts belong to.
#[must_use]
pub fn chain() -> DocumentId {
    DocumentId::derived("messages", &DocumentId::workspace_default())
}

/// Part `index` of the chat's messages.
#[must_use]
pub fn part(index: u64) -> DocumentId {
    chain().part(index)
}

/// The number of `record` when it is a part of the chat's messages.
#[must_use]
pub fn part_index(record: &DocumentId) -> Option<u64> {
    record
        .chain_part()
        .filter(|(base, _)| *base == chain())
        .map(|(_, index)| index)
}

/// One part of the chat's messages: every message posted while it was the
/// head, oldest first, and everything done to them since (edits, reactions,
/// replies in their threads, which stay with the message they answer).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ChatMessages {
    /// The messages, oldest first: made, ordered and removed only by the
    /// part's actions. A post takes its author from whoever signed it; a
    /// generic insert could name anyone as a message's author, and a
    /// replica replaying it would reproduce the forgery.
    #[model(readonly)]
    pub messages: Collection<Message>,
    /// For each part before this one (by its number, in decimal), how many
    /// top-level messages each channel (by its id) has there, as whoever
    /// started this part saw it (`crate::messages::ChatMessages::carry_counts`).
    /// A post that reached a part after the next one started is counted
    /// where that part is held: its own messages count, not this.
    #[model(readonly)]
    pub earlier: BTreeMap<String, BTreeMap<String, i64>>,
}

impl ChatMessages {
    /// How many top-level messages each channel has in this part, by
    /// channel id.
    #[must_use]
    pub fn counts(&self) -> BTreeMap<String, i64> {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for message in self
            .messages
            .values()
            .filter(|message| message.parent.is_none())
        {
            let count = counts.entry(message.channel.to_string()).or_default();
            *count = count.saturating_add(1);
        }
        counts
    }

    /// What a part started after this one, part `index`, carries of the
    /// parts before it: what this one carries, and this one's own counts.
    #[must_use]
    pub fn carried_after(&self, index: u64) -> BTreeMap<String, BTreeMap<String, i64>> {
        let mut earlier = self.earlier.clone();
        earlier.insert(index.to_string(), self.counts());
        earlier
    }
}

pub use chat_model::ReadMark;

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// Part ids and the order of read marks.
        /// Each case was a test of its own, on a fixture of its own; a failure
        /// names the cases that failed.
        fn parts_number_one_chain_and_posts_and_read_marks_move_along_it;

        phase parts_are_numbered_resources_of_one_chain;
        {
            let first = part(0);
            assert_eq!(part_index(&first), Some(0));
            assert_eq!(part_index(&part(41)), Some(41));
            assert_eq!(part_index(&chain()), None);
            assert_eq!(part_index(&DocumentId::workspace_default().part(3)), None);
            assert_eq!(
                neighbour_wire::document_sync(first.as_str()),
                neighbour_wire::DocumentSync::Part {
                    base: chain().as_str(),
                    index: 0
                }
            );
        }

        phase a_later_read_mark_is_further;
        {
            let early = ReadMark { part: 2, seen: 900 };
            let later = ReadMark { part: 3, seen: 1 };
            assert!(later > early);
            assert!(
                ReadMark { part: 2, seen: 901 } > early,
                "one message further in the same part"
            );
            assert_eq!(
                ReadMark { part: 2, seen: 900 },
                early,
                "the same place is no further"
            );
            assert!(
                ReadMark { part: 0, seen: 1 } > ReadMark::default(),
                "any reading is past none"
            );
        }
    }
}
