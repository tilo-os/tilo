//! Saying something about a thing.
//!
//! Elsewhere a reference degrades into prose somewhere between the selection
//! and the message, and whoever reads it — a colleague, a search index, an
//! agent — has to guess what "this" meant. Here objects already have addresses,
//! because apps publish a tree of [`Resource`](tilo_actions::Resource)s, so a
//! selection in any app becomes a [`ResourceRef`] and a [`Remark`] carries that
//! address beside the text rather than inside it.
//!
//! The property worth protecting: an instruction reaches its reader as
//! `(text, address, destination)`. [`read_chat_action`] is the test of it — if
//! it ever has to parse [`Remark::text`] to recover the address, this is gone.
//!
//! A [`ResourceRef`] names something in the *document* — a block id, a path —
//! not an entry in a resource tree, which `tilo-actions` defines as per-frame
//! presentation that never crosses a boundary. That is what lets a reader on
//! another machine resolve it in a later session.
//!
//! [`Remark::to_chat_action`] fails closed: a delivery the chat action cannot
//! represent is an [`Undeliverable`] error, because a private remark silently
//! written into a channel row is a disclosure bug.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod delivery;
mod direct;
mod reference;
mod remark;

pub use delivery::{
    Delivery, Destination, DestinationDisplay, Id, MentionRef, RoomMember, Target, deliver,
    resolve_mentions,
};
pub use direct::DirectRoom;
pub use reference::{Referent, ResourceRef, Span, SpanUnit};
pub use remark::{
    CHAT_ACTION_TYPE, ROOM_CONVERSATION, ReceivedRemark, Remark, Undeliverable, read_chat_action,
};
