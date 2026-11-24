//! Who may invoke which action.
//!
//! Authority is granted over the published tree of workspaces, apps,
//! resources, items and actions. A [`Grant`] names a [`Grantee`], a node ([`TreePath`]),
//! the kinds of action it covers ([`KindSet`]), an [`Answer`] and a
//! [`Lifetime`]. [`decide`] is the one function every action API, host and receiver
//! uses: the most specific covering node wins, then a personal override takes
//! precedence over a role rule. Otherwise Deny beats Ask beats Allow, and no
//! grant means Deny.
//!
//! [`Acting`] is who made a change and who they acted for, as a tree: an
//! automation a person set up, run by an agent, for two people. An action is
//! allowed only when every node of the tree allows it
//! ([`Authority::of_acting`]), and [`admit_invocation`] is the one admission of an
//! invocation every route that runs actions takes: the desktop's broker and a
//! host running a hosted workspace's documents alike.
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

mod acting;
mod admission;
pub mod agent_answers;
pub mod authorship;
pub mod content;
mod decide;
pub mod describe;
pub mod earlier;
pub mod edits;
pub mod entry;
mod grant;
mod kind;
mod member;
mod member_access;
mod path;
mod person;
mod policy_edit;
mod readers;
pub mod reading;
pub mod roles;
pub mod share;
pub mod turn;
mod workspace;

pub use acting::{Invocation, admit_invocation, answer_at, asker_of, granted_to};
pub use admission::{Admission, Asker, Invoked, Reach, Refused, admit};
pub use content::{ContentLimit, ContentPolicy, KeptLimits, Raises};
pub use decide::{Authority, Basis, Cap, Decision, Effective, Party, Refuser, Verdict, decide};
pub use describe::{Ids, Labels, describe, describe_node, sentences};
pub use earlier::EarlierIds;
pub use entry::{EntryError, EntryPolicy, ListingPolicy, VISITOR_ENTRY_ACTIONS, VISITOR_ENTRY_APP};
/// The id prefix of the answers a person gives an agent session: "Allow for this agent" records
/// one, deciding that session's later steps and no other session's ([`is_session_answer`]).
pub const SESSION_ANSWER_PREFIX: &str = "session-answer:";

/// Whether `grant` is an answer a person gave one agent session.
#[must_use]
pub fn is_session_answer(grant: &Grant) -> bool {
    grant.id.as_str().starts_with(SESSION_ANSWER_PREFIX)
}

/// The answers a person gave the agent session `session`.
pub fn session_answers<'g>(
    grants: &'g [Grant],
    session: &str,
) -> impl Iterator<Item = &'g Grant> + 'g {
    let prefix = format!("{SESSION_ANSWER_PREFIX}{session}:");
    grants
        .iter()
        .filter(move |grant| grant.id.as_str().starts_with(&prefix))
}

pub use grant::{Answer, Context, Grant, GrantId, Grantee, Lifetime, OFFERED_SPAN_MS, Role};
pub use kind::{ActionKind, KindSet};
pub use member::{
    Acting, Actor, AgentSessionId, AutomationId, DIRECT_CONVERSATION_PREFIX, MemberId, ResourceId,
    key_agent_id, key_of_agent,
};
pub use member_access::MemberAccess;
pub use path::{Level, PathError, Segment, Specificity, TreePath};
pub use person::{PersonId, PersonIdError};
pub use policy_edit::{PolicyEdit, PresetMutationError, sort_presets, until_secs};
pub use readers::ReaderRule;
pub use roles::{Preset, PresetError, Rule};
pub use workspace::{AgentMember, Change, PersonGrants, Refusal, WorkspacePolicy};
