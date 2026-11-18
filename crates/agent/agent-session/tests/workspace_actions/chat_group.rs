//! A member below moderator is offered the chat's `group` and `apply`, and a
//! batch of posts lands as one step.
//!
//! The chain that keeps the chat's messages offers them to whoever may take
//! the least kind any of its verbs asks for, at no field: what each
//! invocation writes is decided from its own arguments. One verb's field
//! (`remove-message` writes `moderation`) is that verb's, not the batch's,
//! and reading it as the batch's hid both from every member the role keeps
//! from moderating, so a chat could only be posted to one call at a time.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_actions::api::{ActionAddress, ActionDescriptor};
use tilo_agent_session::{Replica, ReplicaActor, Seed};
use ui::permissions::Role;
use ui::tilo_resource::MemberId;

/// How many posts one batch carries.
const BATCH: usize = 500;

/// The actions the chain that keeps the chat's messages offers `member`.
fn offered(member: &Replica) -> Vec<ActionDescriptor> {
    let chain = ui::resource::chain_resource_id(&chat::parts::chain());
    member
        .survey()
        .into_iter()
        .filter(|action| action.address.resource == chain)
        .collect()
}

/// One call's arguments for `action`, from the parameters it publishes: what
/// an SDK builds a call from. The values come in the order the declaration
/// names them.
fn arguments(
    action: &ActionDescriptor,
    values: Vec<serde_json::Value>,
) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    for (param, value) in action.params.iter().zip(values) {
        arguments.insert(param.name.to_string(), value);
    }
    arguments
}

#[test]
fn a_member_below_moderator_is_offered_the_batch_and_a_batch_is_one_step() {
    let access = ui::permissions::PermissionContext::member_of(
        crate::common::WORKSPACE,
        &MemberId::person("bo"),
        Role::Editor,
    )
    .access()
    .clone();
    let mut member = crate::common::open(
        "so.tilo.chat",
        &MemberId::person("bo"),
        ReplicaActor::Member {
            access,
            sandbox: Vec::new(),
            unix_time: 1_900_000_000,
            agent: None,
            agent_name: None,
            agent_session: None,
            device: None,
        },
        Seed::New,
    )
    .unwrap();
    let offered = offered(&member);
    let named = |id: &str| offered.iter().find(|action| action.address.action == id);
    for id in ["group", "apply"] {
        let action = named(id).unwrap_or_else(|| {
            panic!(
                "an editor is offered `{id}`: {:?}",
                offered
                    .iter()
                    .map(|action| action.address.action.clone())
                    .collect::<Vec<_>>()
            )
        });
        assert!(
            action.contract.field.is_none(),
            "`{id}` is offered at no field, not one verb's: {:?}",
            action.contract.field
        );
    }
    // The verb whose field that is stays the moderator's: offering the batch
    // must not offer what no role but theirs may do.
    assert!(
        named("remove-message").is_none(),
        "an editor moderates nothing: {:?}",
        offered
            .iter()
            .map(|action| action.address.action.clone())
            .collect::<Vec<_>>()
    );

    // A batch of posts, through the group the chain publishes.
    let post = named("post").expect("an editor posts");
    let mut posts = Vec::with_capacity(BATCH);
    for at in 0..BATCH {
        let words = arguments(
            post,
            vec![
                serde_json::json!("c_chat"),
                serde_json::json!(format!("bulk {at}")),
            ],
        );
        posts.push(serde_json::json!({"action": post.address.action, "args": words}));
    }
    let mut words = serde_json::Map::new();
    words.insert("actions".into(), serde_json::Value::Array(posts));
    member
        .invoke(&named("group").unwrap().address, &words, false)
        .expect("the batch is applied");
    let changes = member.take_document_mutations().unwrap();
    assert_eq!(
        changes.len(),
        1,
        "the batch writes the part once: {:?}",
        changes
            .iter()
            .map(|(document, _)| document.as_str().to_owned())
            .collect::<Vec<_>>()
    );
    let part = chat::parts::part(0);
    let held = member
        .read_resource(&part, chat::messages::read_part)
        .expect("the member holds the part");
    assert_eq!(
        held.messages.len(),
        BATCH,
        "every post of the batch is kept"
    );

    // One step: one undo takes the whole batch back.
    let mut undo = serde_json::Map::new();
    undo.insert("part".into(), serde_json::json!(0));
    member
        .invoke(
            &ActionAddress::new(
                ui::resource::chain_resource_id(&chat::parts::chain()),
                "undo",
            ),
            &undo,
            false,
        )
        .expect("the undo is applied");
    let held = member
        .read_resource(&part, chat::messages::read_part)
        .expect("the member still holds the part");
    assert_eq!(held.messages.len(), 0, "one undo takes the batch back");
}
