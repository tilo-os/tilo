//! The workspace's membership: a log of signed operations, and the membership
//! every device derives from it.
//!
//! A workspace's members, their roles, its grants, bans and invitations are
//! not chosen by whoever's row was written last. They are the effect of the
//! operations the workspace's devices signed, applied in one order that every
//! device reaches on its own ([`replay`]): a device that was sent an operation
//! keeps it, a device whose author had lost the grant it needed keeps it with
//! no effect, and two devices holding the same operations hold the same
//! membership.
//!
//! The document's own member rows remain, projected from the operations, so a
//! store, an older build and anything that reads a workspace document without
//! its operations still sees who is in it.
pub mod legacy;
pub mod op;
pub(crate) mod replay;

#[cfg(test)]
mod tests;

pub use op::{MEMBERS_KEY, Membership, MembershipOpCommand, OpId, SignedMembershipOp};
pub use replay::{genesis, growth, heads, materialise, ops, replay, settle, starts, steps};
