//! The chat's messages as a chain of parts.
//!
//! One record cannot hold a chat that runs for months: a record's snapshot
//! has a size limit, and a newcomer would be sent all of it. So the room's
//! chat record ([`crate::record::Chat`]) keeps workspaces, channels, people,
//! drafts and moderation, and its messages live in numbered part records
//! ([`ChatPart`]), each a record of its own ([`part`]).
//!
//! - **Which part is current.** The part with the highest number a device
//!   holds is the chain's head. A room's host replays a joining device the
//!   chat record and the head only; every earlier part is on-demand
//!   (`neighbour_wire::record_sync`): a device opens it, through the normal
//!   record sync and the chat record's grants, when someone scrolls back to
//!   it.
//! - **Where a post goes.** To the head, unless the head is full
//!   ([`post_part`]): then to the next part. A part's id is the chain and its
//!   number, so two writers that both start part `n + 1` start the same
//!   record, and their posts meet there.
//! - **Where a change to a message goes.** To the part that holds the
//!   message: an edit, a reaction, a reply in its thread, a report.
//! - **Read marks** are positions in the chain ([`ReadMark`]), so they mean
//!   the same whether or not a device holds the earlier parts.
//!
//! This module is the contract the model, the window and the room's hosts
//! (bots, seeding, bridges, moderation) share; change it only with all of
//! them.

use ui::record::RecordId;
use ui::tilo_resource::{self, Collection, Model};

use crate::record::Message;

/// Posts a part holds before the next one starts.
pub const PART_MESSAGES: usize = 2000;
/// Message text, in bytes, a part holds before the next one starts: well
/// under what one record's snapshot may hold, so every part stays
/// compactable whatever its posts say.
pub const PART_TEXT_BYTES: usize = 1024 * 1024;

/// The chain the chat's message parts belong to.
#[must_use]
pub fn chain() -> RecordId {
    RecordId::derived("messages", &RecordId::room_default())
}

/// Part `index` of the chat's messages.
#[must_use]
pub fn part(index: u64) -> RecordId {
    chain().part(index)
}

/// The number of `record` when it is a part of the chat's messages.
#[must_use]
pub fn part_index(record: &RecordId) -> Option<u64> {
    record
        .chain_part()
        .filter(|(base, _)| *base == chain())
        .map(|(_, index)| index)
}

/// The part a new post goes to, given the head (the highest part held), how
/// many messages it holds and their text in bytes: the head, or the next
/// part once the head is full.
#[must_use]
pub const fn post_part(head: u64, head_messages: usize, head_text_bytes: usize) -> u64 {
    if head_messages >= PART_MESSAGES || head_text_bytes >= PART_TEXT_BYTES {
        head.saturating_add(1)
    } else {
        head
    }
}

/// One part of the chat's messages: every message posted while it was the
/// head, oldest first, and everything done to them since (edits, reactions,
/// replies in their threads, which stay with the message they answer).
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct ChatPart {
    /// The messages, oldest first.
    pub messages: Collection<Message>,
}

/// How far a person read a channel: every top-level message of the channel
/// in the parts before `part`, and the first `seen` of them in `part`.
#[derive(Model, Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadMark {
    /// The part the person read into.
    pub part: i64,
    /// Top-level messages of the channel they saw in that part.
    pub seen: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_are_numbered_records_of_one_chain() {
        let first = part(0);
        assert_eq!(part_index(&first), Some(0));
        assert_eq!(part_index(&part(41)), Some(41));
        assert_eq!(part_index(&chain()), None);
        assert_eq!(part_index(&RecordId::room_default().part(3)), None);
        // The host reads the same chain and number from the wire id.
        assert_eq!(
            neighbour_wire::record_sync(first.as_str()),
            neighbour_wire::RecordSync::Part {
                base: chain().as_str(),
                index: 0
            }
        );
    }

    #[test]
    fn a_post_goes_to_the_next_part_once_the_head_is_full() {
        assert_eq!(post_part(4, PART_MESSAGES - 1, 0), 4);
        assert_eq!(post_part(4, PART_MESSAGES, 0), 5);
        assert_eq!(post_part(4, 10, PART_TEXT_BYTES), 5);
    }

    #[test]
    fn a_later_read_mark_is_further() {
        let early = ReadMark { part: 2, seen: 900 };
        let later = ReadMark { part: 3, seen: 1 };
        assert!(later > early);
    }
}
