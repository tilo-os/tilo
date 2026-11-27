//! An agent working in its session takes a step its person asks first about
//! ([`super::agent_connection`]): the step waits for them, stating the agent by
//! the session's name, and their approve of it lets that one step go on.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each scenario holds its session's api to the end, as a step of the session does"
)]

use super::super::agent_connection::AgentConnection;
use super::*;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    ActionProvider as _, InvokeArgument, InvokeRequest, RemoteActionTarget,
};

/// What the person reads the session by.
const NAME: &str = "Claude Code #1";

/// The agent `scribe`, listed in the workspace and working in its session with a key of its own,
/// for the fixture's person, whose admitted connection its steps come through; its person asks
/// first before it changes Notes.
fn agent_in_session(f: &Fixture) -> (AgentConnection, Caller) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let workspace = workspace_id().to_string();
    let person = hex::encode(f.native.certificate.user_public_key);
    f.list_scribe();
    f.native
        .sessions
        .lock()
        .unwrap()
        .push((SESSION.into(), crate::services::GoingSession::default()));
    put_grant(
        f,
        agent_answers::grant(
            &workspace,
            ("scribe", &person),
            (
                &AnswerScope::app("so.tilo.notes"),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Grantee::person(person.clone()),
        ),
    );
    let mut admitted = f
        .state
        .verify_action_caller(&workspace, &f.signed_presentation())
        .unwrap()
        .admitted;
    let kept = crate::test_support::KeptAgents::default().listing(&workspace, ("scribe", &person));
    admit_listed_agent(&kept, &workspace, &mut admitted, "scribe").unwrap();
    let agent_key = SigningKey::from_bytes(&[57; 32]).verifying_key().to_bytes();
    let caller = Caller::delegated_agent("scribe", f.native.certificate.user_public_key, agent_key)
        .in_workspace(workspace.clone())
        .in_new_execution()
        .named(NAME)
        .in_agent_session(SESSION);
    let api = f
        .state
        .agent_session_api(
            &caller,
            InvocationCaller::Local {
                route: CrdtRoute::record(tilo_action_services::task_programs::APP, workspace, ""),
                time: f.state.registered_policy_time().unwrap(),
                admitted: Box::new(admitted),
                pressed: false,
            },
        )
        .expect("a caller in an agent session");
    (api, caller)
}

fn append_request(invocation: u128) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId(uuid::Uuid::from_u128(invocation)),
        target: RemoteActionTarget {
            provider: "200".into(),
            address: ActionAddress::new("notes:release", "append"),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        appearance: None,
        wait_ms: None,
        signed: None,
    }
}

/// `pending`'s answer, taking the broker's turn each pass, which is where an application's
/// invocation is delivered.
async fn answered<T>(f: &Fixture, mut pending: tilo_actions::remote::Pending<T>) -> T {
    tilo_test_env::within("the broker answers", async {
        loop {
            f.state.take_turn(&f.directories);
            match pending.poll_ready() {
                tilo_actions::remote::PendingPoll::Ready(value) => break value,
                tilo_actions::remote::PendingPoll::Abandoned => panic!("nothing answered"),
                tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
            }
        }
    })
    .await
}

/// `request` as the dispatch sends it again once its person approved the call's asked record.
fn approved(request: &InvokeRequest) -> InvokeRequest {
    use tilo_actions::remote::asked::{Approval, RecordRef};
    InvokeRequest {
        confirmation: Some(tilo_actions::remote::Confirmation::Asked(Approval::new(
            RecordRef {
                workspace: workspace_id().to_string(),
                record: format!("invocations/{}", request.invocation.0),
                device: None,
            },
            None,
        ))),
        ..request.clone()
    }
}

/// A step the person asks first about waits for them, stating the agent by the session's name:
/// their approve lets exactly that step go on, as the agent in the session's group, and the next
/// step asks again.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_sessions_step_waits_for_its_person_by_the_sessions_name() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let (api, caller) = agent_in_session(&f);
            let (_target, stream) = notes_target(&f).await;
            let refused = answered(&f, api.invoke(&caller, append_request(71)))
                .await
                .unwrap_err();
            let ActionCallError::ConfirmationRequired(facts) = &refused else {
                panic!("the step waits for its person: {refused:?}");
            };
            assert!(
                matches!(&facts.by, tilo_actions::remote::asked::AskedBy::Agent(named)
                    if named.id == "scribe" && named.name == NAME),
                "{facts:?}"
            );
            answered(&f, api.invoke(&caller, approved(&append_request(71))))
                .await
                .unwrap();
            let wire = stream.next().await.unwrap();
            assert_eq!(
                wire.actor,
                tilo_actions::MemberId::agent(
                    "scribe",
                    person_id_for_root(f.native.certificate.user_public_key)
                )
                .into()
            );
            let context = wire.context().cloned().expect("the session's context");
            assert_eq!(context.group.expect("the session's group").id, SESSION);
            let again = answered(&f, api.invoke(&caller, append_request(72)))
                .await
                .unwrap_err();
            assert!(
                matches!(again, ActionCallError::ConfirmationRequired(_)),
                "once is once: {again:?}"
            );
            assert_eq!(unread_invocations(200), 0, "nothing more reached the app");
            drop(f);
        })
        .await;
}
