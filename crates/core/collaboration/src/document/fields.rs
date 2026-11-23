//! A resource document's own fields, read failing closed: the one reader the toolkit's store,
//! this crate's readers and a mount writing a workspace's tree all share
//! ([`collab_types::document::fields`]).

pub use collab_types::document::fields::*;
