//! Who may invoke which action.
//!
//! Authority is granted over the published tree of workspaces, apps,
//! resources, items and actions. A [`Grant`] names a [`Subject`], a node ([`TreePath`]),
//! the kinds of action it covers ([`KindSet`]), an [`Answer`] and a
//! [`Lifetime`]. [`decide`] is the one function every door, host and receiver
//! uses: the most specific covering node wins, at the same node Deny beats Ask
//! beats Allow, and no grant means Deny. Someone acting for a person (an agent,
//! a task, a Shortcut, an app) is bounded by both: [`Authority::decide`] takes
//! the minimum.
//!
//! [`Invoker`] is who an invocation is decided for apart from the policy, and
//! [`admit_invocation`] the one admission of an invocation every route that
//! runs actions takes: the desktop's broker and a host running a hosted
//! workspace's records alike.
//!
//! Roles are presets of grants on a workspace ([`roles`]); membership, roles
//! and grants change only through the workspace's governance actions
//! ([`WorkspacePolicy`]).
//! [`describe()`] renders a grant set by node, for a log; [`sentences()`] says
//! it the way a person reads it, one sentence per grant. [`agent_answers`] are
//! the Allow, Ask first and Deny a person sets for one of their agents, per
//! application, kind of resource or action. [`authorship`] makes the code
//! between a person and an action (a tilolang program) one more bounding
//! party.

mod admission;
pub mod agent_answers;
pub mod authorship;
pub mod bot;
mod decide;
pub mod describe;
pub mod door;
pub mod earlier;
pub mod edits;
mod grant;
mod invoker;
mod kind;
mod member_record;
mod path;
pub mod roles;
pub mod share;
mod standing;
pub mod turn;
mod workspace;

pub use admission::{Admission, Asker, Invoked, Reach, Refused, admit};
pub use decide::{Authority, Basis, Decision, Party, Verdict, decide};
pub use describe::{Ids, Labels, describe, describe_node, sentences};
pub use door::{DoorError, DoorPolicy, ListingPolicy};
pub use earlier::EarlierIds;
pub use grant::{Answer, Context, Grant, GrantId, Lifetime, Role, Subject};
pub use invoker::{Invocation, Invoker, admit_invocation, answer_at, asker_of};
pub use kind::{ActionKind, KindSet};
pub use member_record::{member_record, member_record_id};
pub use path::{Level, PathError, Segment, Specificity, TreePath};
pub use roles::{Preset, PresetError, Rule};
pub use standing::{Bound, Standing};
pub use workspace::{Change, Refusal, WorkspacePolicy};
