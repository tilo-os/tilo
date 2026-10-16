//! Addresses of things in documents.
//!
//! Elsewhere a reference degrades into prose somewhere between the selection
//! and the message, and whoever reads it — a colleague, a search index, an
//! agent — has to guess what "this" meant. Here objects already have addresses,
//! because apps publish a tree of [`Resource`](tilo_actions::Resource)s, so a
//! selection in any app becomes a [`ResourceRef`] carried beside the text
//! rather than inside it.
//!
//! A [`ResourceRef`] names something in the *document* — a block id, a path —
//! not an entry in a resource tree, which `tilo-actions` defines as per-frame
//! presentation that never crosses a boundary. That is what lets a reader on
//! another machine resolve it in a later session.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

mod reference;

pub mod link;

pub use reference::{Referent, ResourceRef, Span, SpanUnit};
