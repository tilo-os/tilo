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
pub mod op;
pub(crate) mod replay;

#[cfg(test)]
mod tests;

pub use op::{MEMBERS_KEY, MembershipOpCommand, OpId, SignedMembershipOp, put_op};
pub use replay::{HeldOps, growth, heads, heads_of, ops, people, refused, replay, replay_declared};
