//! Who acts in a workspace, as plain data: a person ([`PersonId`]), a member ([`MemberId`]),
//! an actor ([`Actor`]) and the chain of who acted for whom ([`Acting`]), and the app one acts
//! in ([`AppId`]). The workspace's policy decides what each may do; these only name them.

mod app_id;
mod member;
mod person;

pub use app_id::{
    AppId, Grammar, InvalidAppId, MAX_APP_ID_BYTES, is_valid_lowercase_segment, is_valid_segment,
};
pub use member::{
    Acting, Actor, AgentSessionId, AutomationId, DIRECT_CONVERSATION_PREFIX, MemberId, ResourceId,
    identity_color, key_agent_id, key_of_agent,
};
pub use person::{PersonId, PersonIdError};
