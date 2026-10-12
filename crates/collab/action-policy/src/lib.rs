//! Who may invoke which action.
//!
//! Authority is granted over the published tree of rooms, apps, resources,
//! items and actions. A [`Grant`] names a [`Subject`], a node ([`TreePath`]),
//! the kinds of action it covers ([`KindSet`]), an [`Answer`] and a
//! [`Lifetime`]. [`decide`] is the one function every door, host and receiver
//! uses: the most specific covering node wins, at the same node Deny beats Ask
//! beats Allow, and no grant means Deny. Someone acting for a person (an agent,
//! a task, a Shortcut, an app) is bounded by both: [`Authority::decide`] takes
//! the minimum.
//!
//! Roles are presets of grants on a room ([`roles`]); membership, roles and
//! grants change only through the room's governance actions ([`RoomPolicy`]).
//! [`describe`] renders a grant set as sentences people and agents can talk
//! through.

mod decide;
pub mod describe;
pub mod edits;
mod grant;
mod kind;
mod path;
pub mod roles;
mod room;
mod standing;

pub use decide::{Authority, Basis, Decision, Party, Verdict, decide};
pub use describe::{Ids, Labels, describe, describe_node};
pub use grant::{Answer, Context, Grant, GrantId, Lifetime, Role, Subject};
pub use kind::{ActionKind, KindSet};
pub use path::{Level, PathError, Segment, Specificity, TreePath};
pub use roles::{Preset, PresetError, Rule};
pub use room::{Change, Refusal, RoomPolicy};
pub use standing::{Bound, Standing};
