//! What the runtime invokes is decided by the workspace's one policy: the person's access, the
//! answers they gave their agents, and the authors of the code, the minimum winning.

use crate::fixtures::Workspace;
use serde_json::json;
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{ActionKind, Answer, Grantee, MemberAccess, WorkspacePolicy};
use tilolang_runtime::{Actor, AgentBlock};
use tilolang_tilo::meta::MetaExt as _;
use tilolang_tilo::publish::{Caller, PlanRequest};

use crate::fixtures::{Replica, TASKS_SET};

const LETTERS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.tl");
const LETTERS_IDS: &str = include_str!("../../../lang/tilolang/tests/fixtures/letters/app.ids");
const APP: &str = "so.tilo.tasks";
const WORKSPACE: &str = "workspace";

fn agent() -> Caller {
    Caller {
        actor: tilolang_tilo::runtime::ids::MemberId::agent("task-1", "ada").into(),
        agent: true,
        on_behalf_of: Some("ada".to_owned()),
        handed: false,
        app: None,
    }
}

/// Ada's access in a workspace she owns and Grace edits, with `answers` she gave her agents.
fn access(answers: Vec<tilo_action_policy::Grant>) -> MemberAccess {
    let mut policy = WorkspacePolicy::new(WORKSPACE, "ada");
    policy
        .members
        .insert("grace".to_owned(), tilo_action_policy::Role::Editor);
    policy.grants.extend(answers);
    MemberAccess::person(policy, "ada")
}

/// The block that adds `title` to the todo list.
fn add_block(replica: &Replica, title: &str) -> String {
    let list = replica.host.root_of(APP).unwrap();
    format!(
        "tasks.new_task(@tasks.Tasks:{}, title: \"{title}\")",
        list.id_string()
    )
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
        WORKSPACE,
        ("task-1", "ada"),
        (&AnswerScope::app(APP), ActionKind::Undoable),
        Answer::Deny,
        Grantee::person("ada"),
    );
    let (ada, packages) = Replica::access(
        "ada",
        Workspace::new(),
        TASKS_SET,
        Some(access(vec![answer])),
    );
    let meta = &packages.last().unwrap().meta;
    let list = ada.host.root_of(APP).unwrap();
    ada.commit();
    let add = meta.actions.iter().find(|a| a.name == "new_task").unwrap();
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
            from: None,
        })
    };
    let refused = plan(Some(&agent())).unwrap_err();
    assert!(refused.contains("not allowed"), "{refused}");
    // The person herself, and the app's own code for her, may.
    assert!(plan(None).is_ok());
    // What the agent writes in a block is denied the same.
    ada.host.admit_agent(&agent());
    let block = ada
        .host
        .runtime()
        .run_agent_block(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(
                "task-1", "ada",
            )),
            &add_block(&ada, "milk"),
        )
        .unwrap();
    assert_eq!(state(block), "denied");
}

tilo_test_env::scenario! {
    fn untrusted_and_agents_code_asks;

    phase untrusted_code_asks_and_the_persons_agents_code_runs;
    {
        let (ada, _) = Replica::access(
            "ada",
            Workspace::new(),
            TASKS_SET,
            Some(access(Vec::new())),
        );
        ada.host.root_of(APP).unwrap();
        ada.commit();
        // Code Grace wrote, which nobody here trusts, asks; she may edit the workspace herself.
        let untrusted = ada
            .host
            .runtime()
            .run_agent_block(&Actor::from(tilolang_tilo::runtime::ids::MemberId::person("grace")), &add_block(&ada, "milk"))
            .unwrap();
        assert_eq!(state(untrusted), "asked");
        // Someone the workspace does not know is refused outright.
        let stranger = ada
            .host
            .runtime()
            .run_agent_block(&Actor::from(tilolang_tilo::runtime::ids::MemberId::person("stranger")), &add_block(&ada, "milk"))
            .unwrap();
        assert_eq!(state(stranger), "denied");
        ada.host.admit_agent(&agent());
        let admitted = ada
            .host
            .runtime()
            .run_agent_block(&Actor::from(tilolang_tilo::runtime::ids::MemberId::agent("task-1", "stranger")), &add_block(&ada, "eggs"))
            .unwrap();
        assert_eq!(state(admitted), "done");
    }

    phase reaching_outside_from_untrusted_or_an_agents_code_asks;
    {
        let (ada, packages) = Replica::access(
            "ada",
            Workspace::new(),
            &[(LETTERS, LETTERS_IDS)],
            Some(access(Vec::new())),
        );
        let meta = &packages.last().unwrap().meta;
        let letters = ada.host.root_of("test.letters").unwrap();
        ada.commit();
        let new_draft = meta.actions.iter().find(|a| a.name == "new_draft").unwrap();
        ada.host
            .runtime()
            .invoke_action(
                &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
                (
                    new_draft.id.as_str(),
                    vec![tilolang_runtime::Value::resource(&letters)],
                ),
            )
            .unwrap();
        ada.commit();
        let tilolang_runtime::Value::List(drafts) = ada.host.runtime().read(&letters, "drafts").unwrap()
        else {
            panic!("the letters app lists its drafts");
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
                .run_agent_block(
                    &if who == "task-1" {
                        Actor::from(tilolang_tilo::runtime::ids::MemberId::agent(who, "ada"))
                    } else {
                        Actor::from(tilolang_tilo::runtime::ids::MemberId::person(who))
                    },
                    &send,
                )
                .unwrap();
            assert_eq!(state(block), "asked", "{who}");
        }
    }
}
