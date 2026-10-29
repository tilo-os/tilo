//! What the runtime invokes is decided by the room's one policy: the person's standing, the
//! answers they gave their agents, and the authors of the code, the minimum winning.

use automerge::AutoCommit;
use serde_json::json;
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, RoomPolicy, Standing, Subject};
use tilolang_runtime::{AgentBlock, Principal};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{Caller, PlanRequest};

use crate::fixtures::Replica;

const TODO: &str = include_str!("../../../../apps/tl/tasks/app.tl");
const TODO_IDS: &str = include_str!("../../../../apps/tl/tasks/app.ids");
const LETTERS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.ids");
const APP: &str = "so.tilo.lang.tasks";
const ROOM: &str = "room";

fn agent() -> Caller {
    Caller {
        actor: "task-1".to_owned(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
    }
}

/// Ada's standing in a room she owns and Grace edits, with `answers` she gave her agents.
fn standing(answers: Vec<tilo_action_policy::Grant>) -> Standing {
    let mut policy = RoomPolicy::new(ROOM, "ada");
    policy
        .members
        .insert("grace".to_owned(), tilo_action_policy::Role::Editor);
    policy.grants.extend(answers);
    Standing::person(policy, "ada")
}

/// The block that adds `title` to the todo list.
fn add_block(replica: &Replica, title: &str) -> String {
    let list = replica.host.root_of(APP).unwrap();
    format!("tasks.add(@tasks.Tasks:{}, \"{title}\")", list.id_string())
}

fn state(block: AgentBlock) -> String {
    match block {
        AgentBlock::Invocation { record, .. } => record.state,
        other => panic!("not an invocation: {other:?}"),
    }
}

#[test]
fn an_agent_the_person_denied_the_app_is_denied() {
    let answer = agent_answers::grant(
        ROOM,
        "task-1",
        (&AnswerScope::app(APP), ActionKind::Undoable),
        Answer::Deny,
        Subject::person("ada"),
    );
    let (mut ada, packages) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(TODO, TODO_IDS)],
        Some(standing(vec![answer])),
    );
    let meta = &packages[0].meta;
    let list = ada.host.root_of(APP).unwrap();
    ada.commit();
    let add = meta.actions.iter().find(|a| a.name == "add").unwrap();
    let kind = meta.kind_of("Tasks");
    let args = json!({"title": "Take the bins out"});
    let plan = |caller: Option<&Caller>| {
        ada.host.plan(&PlanRequest {
            app: APP,
            kind: &kind,
            action_id: &add.id,
            resource: Some(&list.doc),
            args: args.as_object().unwrap(),
            caller,
        })
    };
    let refused = plan(Some(&agent())).unwrap_err();
    assert!(refused.contains("not allowed"), "{refused}");
    // The person herself, and the app's own code for her, may.
    assert!(!plan(None).unwrap().edits.is_empty());
    // What the agent writes in a block is denied the same.
    ada.host.admit_agent(&agent());
    let block = ada
        .host
        .runtime()
        .run_agent_block(&Principal::new("task-1"), &add_block(&ada, "milk"))
        .unwrap();
    assert_eq!(state(block), "denied");
}

#[test]
fn untrusted_code_asks_and_the_persons_agents_code_runs() {
    let (mut ada, _) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(TODO, TODO_IDS)],
        Some(standing(Vec::new())),
    );
    ada.host.root_of(APP).unwrap();
    ada.commit();
    // Code Grace wrote, which nobody here trusts, asks; she may edit the room herself.
    let untrusted = ada
        .host
        .runtime()
        .run_agent_block(&Principal::new("grace"), &add_block(&ada, "milk"))
        .unwrap();
    assert_eq!(state(untrusted), "suggested");
    // Someone the room does not know is refused outright.
    let stranger = ada
        .host
        .runtime()
        .run_agent_block(&Principal::new("stranger"), &add_block(&ada, "milk"))
        .unwrap();
    assert_eq!(state(stranger), "denied");
    ada.host.admit_agent(&agent());
    let admitted = ada
        .host
        .runtime()
        .run_agent_block(&Principal::new("task-1"), &add_block(&ada, "eggs"))
        .unwrap();
    assert_eq!(state(admitted), "done");
}

#[test]
fn reaching_outside_from_untrusted_or_an_agents_code_asks() {
    let (mut ada, packages) = Replica::standing(
        "ada",
        AutoCommit::new(),
        &[(LETTERS, LETTERS_IDS)],
        Some(standing(Vec::new())),
    );
    let meta = &packages[0].meta;
    let desk = ada.host.root_of("test.letters").unwrap();
    ada.commit();
    let new_draft = meta.actions.iter().find(|a| a.name == "new_draft").unwrap();
    ada.host
        .runtime()
        .invoke_action(
            &Principal::new("ada"),
            (
                new_draft.id.as_str(),
                vec![tilolang_runtime::Value::resource(&desk)],
            ),
        )
        .unwrap();
    ada.commit();
    let tilolang_runtime::Value::List(drafts) = ada.host.runtime().read(&desk, "drafts").unwrap()
    else {
        panic!("the desk lists its drafts");
    };
    let tilolang_runtime::Value::Ref(draft) = &drafts[0] else {
        panic!("a draft is a reference");
    };
    let send = format!("letters.send(@letters.Draft:{})", draft.target.id_string());
    for who in ["grace", "task-1"] {
        if who == "task-1" {
            ada.host.admit_agent(&agent());
        }
        let block = ada
            .host
            .runtime()
            .run_agent_block(&Principal::new(who), &send)
            .unwrap();
        assert_eq!(state(block), "suggested", "{who}");
    }
}
