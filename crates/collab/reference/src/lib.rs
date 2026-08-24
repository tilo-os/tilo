//! **Saying something about a thing.**
//!
//! Every other system makes a person *describe* what they are talking about.
//! You can paste a link to a document; you cannot point at paragraph three. The
//! reference degrades into prose somewhere between the selection and the
//! message, and whoever reads it — a colleague, a search index, an agent — has
//! to guess what "this" meant.
//!
//! In qOS objects already have addresses: an app publishes a tree of
//! [`Resource`](qos_actions::Resource)s through `qos-actions`. So a selection in
//! *any* app can become a [`ResourceRef`] — a durable, machine-readable address
//! — and a [`Remark`] carries that address as a first-class field beside the
//! text, never inside it.
//!
//! The property this crate exists to protect: **an instruction addressed to an
//! agent arrives as `(text, resource address, destination)`.** The agent does
//! not parse prose to find out what it is looking at, and never needs screen
//! coordinates. [`read_chat_action`] is the test of that property — if it ever
//! has to read [`Remark::text`] to recover the address, the idea has been lost.
//!
//! ## The three pieces
//!
//! * [`ResourceRef`] — *what is being talked about*. A stable `(kind, id)` plus
//!   an optional [`Span`] into it, and a human `label` that is **never** the
//!   address (see [`ResourceRef`]'s own docs for why that separation matters).
//! * [`Destination`] — *where the remark goes*. Chosen explicitly by the person
//!   and shown on screen next to the compose field, so routing is visible
//!   rather than inferred.
//! * [`Remark`] — the two together, plus the body and its [`MentionRef`]s.
//!
//! ## Why the reference is not a palette pointer
//!
//! `qos-actions` is explicit that a `Resource` is *presentation*: a local,
//! per-frame description that "never crosses a boundary". A message outlives
//! the frame it was authored in, so persisting a palette entry would be wrong.
//! A [`ResourceRef`] instead names something that lives in the **document** — a
//! block id, a file path — which is what lets a reader on another machine, in a
//! later session, resolve it at all. It agrees with the ids apps publish, so it
//! converts to a [`ResourceId`](qos_actions::ResourceId) for free
//! ([`ResourceRef::resource_id`]) and lands in the same vocabulary the door
//! uses, without depending on a tree being open right now.
//!
//! ## Delivery fails closed
//!
//! [`Remark::to_chat_action`] refuses to encode a delivery the chat action
//! cannot faithfully represent, rather than downgrading it. A private remark
//! silently written into a channel row would be a disclosure bug, so an
//! unrepresentable delivery is an [`Undeliverable`] error and nothing is sent.
//! See [`Remark::to_chat_action`].

mod delivery;
mod reference;
mod remark;

pub use delivery::{
    Delivery, Destination, DestinationDisplay, Id, MentionRef, Target, deliver,
};
pub use reference::{Referent, ResourceRef, Span, SpanUnit};
pub use remark::{
    CHAT_ACTION_TYPE, ReceivedRemark, Remark, Undeliverable, read_chat_action,
};
