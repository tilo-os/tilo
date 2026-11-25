//! A triggered turn's read is decided as the agent its certificate delegates, not as a caller
//! that only says it is an agent: the latter reads nothing anywhere.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use super::{CHAT, MESSAGE, Where, turn_caller};
use tilo_action_policy::{ActionKind, Answer, PersonId};
use tilo_actions::remote::Caller;

/// A workspace `id` owned by `person`, as the governance keeps it: the owner's role's grants,
/// and no grant naming any agent.
fn owned_workspace(id: &str, person: &str) -> tilo_desktop_model::Workspace {
    let mut workspace = tilo_desktop_model::Workspace::new(
        tilo_server_protocol::WorkspaceId::parse(id).expect("a workspace id"),
        "Chat",
    );
    workspace.policy.set_owner(person.to_owned());
    workspace
}

/// The agent a turn acts as reads the message that started it, as its certificate delegates it:
/// the same grants every call of its turn carries. A caller that only says it is an agent holds
/// no grant at all, and is refused the read.
#[test]
fn a_turns_read_is_decided_as_the_agents_delegation() {
    let id = tilo_server_protocol::WorkspaceId::from_u128(91).to_string();
    let person = hex::encode([7_u8; 32]);
    let workspace = owned_workspace(&id, &person);
    let place = Where {
        app: CHAT.to_owned(),
        kind: MESSAGE.to_owned(),
        item: "m-1".to_owned(),
        record: "general".to_owned(),
        change: "c1".to_owned(),
    };
    let message = tilo_actions::resource_node(&workspace.policy.node(), CHAT, MESSAGE, &place.item);
    let asked = PersonId::parse(&person).expect("a person");

    let bare = Caller::agent_turn(
        ("scout", asked.clone()),
        asked.clone(),
        tilo_action_policy::turn::Turn::new(place.conversation(), place.item.clone()),
    );
    assert_eq!(
        tilo_broker_core::BrokerActor::for_caller(&bare).answer_in(
            &workspace,
            &message,
            ActionKind::Read,
            0,
        ),
        Answer::Deny,
        "a caller that only says it is an agent reads nothing"
    );

    let caller = turn_caller(&id, "scout", &person, asked, &place);
    let actor = tilo_broker_core::BrokerActor::for_caller(&caller);
    assert!(
        actor.reads_in(&workspace, 0),
        "the turn takes part in the workspace"
    );
    assert_eq!(
        actor.answer_in(&workspace, &message, ActionKind::Read, 0),
        Answer::Allow,
        "the agent reads the message its certificate delegates"
    );
}
