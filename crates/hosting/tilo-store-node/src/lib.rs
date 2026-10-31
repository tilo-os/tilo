//! A workspace's store device: what members publish while another member is
//! away, kept and handed back without ever opening it.
//!
//! A store holds sealed document frames ([`neighbour_wire::NeighbourMsg::AppDocument`]):
//! one signed op per frame, with the channel, epoch and retention class its
//! author declared in the clear and the document content sealed under the
//! channel's content keys. The store checks the author's signature — which
//! needs no key of its own — and keeps the frame. Opening it needs the
//! channel's content keys, which only the workspace's members hold, so a
//! store is a device that carries a workspace's history without being able
//! to read it.
//!
//! What the store does with a frame:
//!
//! * **retains** it in the lineage of its channel and author
//!   ([`Frame`], [`store::Store::retain`]);
//! * **compacts** a lineage: an op its author sealed as a
//!   [`ContentRetention::Checkpoint`] is the whole state of that lineage up
//!   to its sequence number, so the author's earlier frames there are
//!   retired;
//! * **retires by appetite** ([`Appetite`]): a store that keeps everything,
//!   or one bounded by bytes that drops the oldest frames first and every
//!   lineage's newest checkpoint last;
//! * **serves anti-entropy**: a member asks from a cursor per lineage and
//!   receives the frames after it, bounded per round, and learns where the
//!   store has nothing ([`store::Replay::floors`]).
//!
//! The device side is a protocol of its own on iroh ([`serve`], [`client`]):
//! the store answers on [`ALPN`], proves the dialling device with its
//! certificate over an endpoint whose id is that device's key, and asks its
//! embedder's [`Access`] whether that device may sync that workspace.

#![deny(rust_2018_idioms)]

pub mod client;
pub mod frame;
pub mod serve;
pub mod store;
pub mod wire;

pub use client::{Link, LinkError, advance, gaps};
pub use frame::{Frame, Lineage, Refusal};
pub use serve::{ALPN, Access, FixedAccess, StoreNode};
pub use store::{Appetite, Budget, Cursor, Floor, Replay, Retained, Store, StoreError};
pub use wire::{PROTOCOL_VERSION, StoreReply, StoreRequest};
