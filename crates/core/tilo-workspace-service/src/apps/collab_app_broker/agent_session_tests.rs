//! An agent working in its session takes a step its person asks first about
//! ([`super::agent_connection`]): the question is put to them on the host's
//! sheet by the session's name, and their answer decides that one step.
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

/// The shell's sheet, as a test answers it.
#[derive(Default)]
struct Shell {
    asked: Mutex<Vec<crate::apps::app_actions::AppActionConsentRequest>>,
    answers: Mutex<Vec<tokio::sync::oneshot::Sender<crate::apps::app_actions::AppActionAnswer>>>,
}

impl crate::apps::app_actions::AppActionConsentPrompter for Shell {
    fn ask(
        &self,
        request: crate::apps::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::apps::app_actions::AppActionAnswer>> {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.asked.lock().unwrap().push(request);
        self.answers.lock().unwrap().push(answer);
        Some(answered)
    }

    fn withdraw(&self, _: &str) {}
}

impl Shell {
    /// The `count`th question the sheet shows, once it is up.
    async fn shown(&self, count: usize) -> crate::apps::app_actions::AppActionConsentRequest {
        tilo_test_env::within("the sheet asks", async {
            loop {
                if let Some(asked) = self.asked.lock().unwrap().get(count.saturating_sub(1)) {
                    break asked.clone();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
    }

    /// The person answers the question up last: allowed for a lifetime, or declined for `None`.
    fn answer(&self, lifetime: Option<tilo_action_policy::Lifetime>) {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop()
            .expect("a question is up");
        let _ = answer.send(crate::apps::app_actions::AppActionAnswer::unedited(
            lifetime,
        ));
    }
}

/// The agent `scribe`, listed in the workspace and working in its session with a key of its own,
/// for the fixture's person, whose admitted connection its steps come through; its person asks
/// first before it changes Notes.
fn agent_in_session(f: &Fixture) -> (AgentConnection, Caller, Arc<Shell>) {
    use tilo_action_policy::agent_answers::{self, AnswerScope};
    let workspace = workspace_id().to_string();
    let person = hex::encode(f.native.certificate.user_public_key);
    f.list_scribe();
    f.native.sessions.lock().unwrap().push((
        SESSION.into(),
        crate::apps::services::GoingSession::default(),
    ));
    let shell = Arc::new(Shell::default());
    f.state.set_app_action_consent(shell.clone());
    put_grant(
        f,
        agent_answers::grant(
            &workspace,
            "scribe",
            (
                &AnswerScope::app("so.tilo.notes"),
                tilo_action_policy::ActionKind::Undoable,
            ),
            tilo_action_policy::Answer::Ask,
            tilo_action_policy::Grantee::person(person),
        ),
    );
    let mut admitted = f
        .state
        .verify_action_caller(&workspace, &f.signed_presentation())
        .unwrap()
        .admitted;
    let kept = crate::apps::test_support::KeptAgents::default().listing(&workspace, "scribe");
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
                route: CrdtRoute::record(tilo_actions::remote::task_programs::APP, workspace, ""),
                time: f.state.registered_policy_time().unwrap(),
                admitted: Box::new(admitted),
            },
        )
        .expect("a caller in an agent session");
    (api, caller, shell)
}

fn append_request(invocation: u128) -> InvokeRequest {
    InvokeRequest {
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

/// A step the person asks first about is put to them on the sheet by the session's name: the
/// person allowing it once lets exactly that step go on, as the agent in the session's group, and
/// declining the next refuses it in the words a declined step is refused in.
#[tokio::test(flavor = "current_thread")]
async fn an_agent_sessions_question_is_asked_on_the_sheet_and_answered_there() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let (api, caller, shell) = agent_in_session(&f);
            let (_target, stream) = notes_target(&f).await;
            assert_eq!(
                answered(&f, api.invoke(&caller, append_request(71)))
                    .await
                    .unwrap_err(),
                ActionCallError::confirmation_required()
            );
            let asking = f.state.request_approval(&caller, None, &append_request(71));
            let shown = shell.shown(1).await;
            assert!(shown.confirming, "{shown:?}");
            assert_eq!(shown.scope.as_deref(), Some(NAME), "{shown:?}");
            shell.answer(Some(tilo_action_policy::Lifetime::Once));
            let mut again = append_request(71);
            answered(&f, asking).await.unwrap().apply(&mut again);
            answered(&f, api.invoke(&caller, again)).await.unwrap();
            let wire = stream.next().await.unwrap();
            assert_eq!(wire.actor, tilo_actions::MemberId::agent("scribe").into());
            let context = wire.context().cloned().expect("the session's context");
            assert_eq!(context.group.expect("the session's group").id, SESSION);
            assert_eq!(
                answered(&f, api.invoke(&caller, append_request(72)))
                    .await
                    .unwrap_err(),
                ActionCallError::confirmation_required(),
                "once is once"
            );

            let asking = f.state.request_approval(&caller, None, &append_request(72));
            shell.shown(2).await;
            shell.answer(None);
            let refused = answered(&f, asking).await.unwrap_err();
            assert!(
                matches!(&refused, ActionCallError::Refused { reason }
                    if reason.contains(tilo_actions::draft::DECLINED)),
                "{refused:?}"
            );
            assert_eq!(unread_invocations(200), 0, "nothing more reached the app");
            drop(f);
        })
        .await;
}
