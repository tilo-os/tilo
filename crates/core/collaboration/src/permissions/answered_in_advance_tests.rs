//! A window admits what someone acting for a person may do as the workspace's own admission does:
//! an agent's send waits for its person, unless that person already answered it for that very
//! action ("Always allow this").

use super::*;
use tilo_actions::{ActionSpec, OperationKind, ResourceSpec};

const APP: &str = "so.tilo.notes";

fn turn() -> Vec<policy::Grant> {
    let agent = policy::Grantee::agent("scribe", "editor");
    let workspace = TreePath::workspace(LOCAL_WORKSPACE);
    vec![
        policy::Grant::new(
            "turn:scribe:0",
            agent.clone(),
            workspace.clone(),
            KindSet::READ_UNDOABLE,
            Answer::Allow,
            policy::Grantee::person("editor"),
        ),
        policy::Grant::new(
            "turn:scribe:1",
            agent,
            workspace,
            KindSet::OUTSIDE,
            Answer::Ask,
            policy::Grantee::person("editor"),
        ),
    ]
}

fn answer_for(action: &str) -> policy::Grant {
    policy::agent_answers::grant(
        LOCAL_WORKSPACE,
        ("scribe", "editor"),
        (
            &policy::agent_answers::AnswerScope::action(APP, "note", action),
            ActionKind::Outside,
        ),
        Answer::Allow,
        policy::Grantee::person("editor"),
    )
}

/// What the window says of the agent's send, carrying `grants`.
fn sends(grants: Vec<policy::Grant>) -> Result<(), tilo_actions::ApiError> {
    let send =
        ActionSpec::operation("send", "Send", OperationKind::Effect, "notes.send", &()).unwrap();
    assert_eq!(send.contract.need().kind, ActionKind::Outside);
    let note = ResourceSpec::new("n1", "note", "Note").with_action(send.clone());
    let window =
        PermissionContext::member_of(LOCAL_WORKSPACE, &MemberId::person("editor"), Role::Editor)
            .acting_as(
                tilo_member_types::Actor::Member(tilo_member_types::MemberId::agent(
                    "scribe", "editor",
                )),
                grants,
            )
            .reaching_person();
    let answer = window.invocation_admission(APP, &note, &send, None, None);
    admit_in_window(answer, &send, window.asker())
}

#[test]
fn an_agents_send_its_person_answered_in_advance_runs_unasked_in_the_window() {
    assert_eq!(
        sends(turn()),
        Err(tilo_actions::ApiError::ConfirmationRequired),
        "an agent's send waits for its person"
    );

    let mut answered = turn();
    answered.push(answer_for("send"));
    assert_eq!(
        sends(answered),
        Ok(()),
        "what its person answered in advance for that very action runs unasked"
    );

    let mut elsewhere = turn();
    elsewhere.push(answer_for("archive"));
    assert_eq!(
        sends(elsewhere),
        Err(tilo_actions::ApiError::ConfirmationRequired),
        "an answer for another action lifts nothing"
    );
}
