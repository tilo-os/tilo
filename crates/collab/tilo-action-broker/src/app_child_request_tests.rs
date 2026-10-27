// What an app's effect asks of another app while it runs an invocation
// someone asked of it is that someone's: decided by their grants, delivered
// as theirs, and never widened by the app's own identity or by an
// invocation the app made of itself.

const NOTES: &str = "so.tilo.notes";
const NOTES_PEER: u64 = 908;
const MAIL: &str = "so.tilo.mail";
const MAIL_PEER: u64 = 909;

/// Notes, publishing its notebook's `create-page` on connection
/// [`NOTES_PEER`], and Mail its `send` on [`MAIL_PEER`]; what is delivered
/// to Notes arrives on the receiver.
fn publish_notes(
    rig: &Rig,
    env: &Environment,
) -> tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope> {
    let surface = |peer_id: u64, app_id: &str, resource: &str, action: &str| PublishedSurface {
        peer_id,
        app_id: app_id.into(),
        route: CrdtRoute::record(app_id, env.route.cluster_id.clone(), "record"),
        actions: vec![PublishedAction {
            semantic_identity: None,
            resource: resource.into(),
            action: action.into(),
            resource_kind: resource.into(),
            resource_label: resource.into(),
            action_label: action.into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            operation_kind: tilo_actions::OperationKind::Effect,
            params: Vec::new(),
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Undoable,
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..Default::default()
            },
            input: PublishedInput::None,
            view: None,
        }],
        live: true,
    };
    rig.state.replace_published_surfaces_for_test(vec![
        surface(NOTES_PEER, NOTES, "notes.notebook", "create-page"),
        surface(MAIL_PEER, MAIL, "mail.outbox", "send"),
    ]);
    drop(rig.state.capture_invocations_for_test(MAIL_PEER));
    rig.state.capture_invocations_for_test(NOTES_PEER)
}

/// `action` of `resource` on connection `peer`, continuing `on_behalf_of`.
fn call(
    peer: u64,
    resource: &str,
    action: &str,
    on_behalf_of: Option<tilo_actions::InvocationId>,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        on_behalf_of,
        target: tilo_actions::remote::RemoteActionTarget {
            provider: peer.to_string(),
            address: tilo_actions::ActionAddress::new(resource, action),
        },
        argument: tilo_actions::remote::InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

/// A new note in Notes' notebook, continuing `on_behalf_of`.
fn new_note(
    on_behalf_of: Option<tilo_actions::InvocationId>,
) -> tilo_actions::remote::InvokeRequest {
    call(NOTES_PEER, "notes.notebook", "create-page", on_behalf_of)
}

fn admitted(answer: &tilo_actions::remote::RemoteActionMessage) -> bool {
    matches!(
        answer,
        tilo_actions::remote::RemoteActionMessage::Receipt(
            tilo_actions::remote::InvocationReceipt {
                status: tilo_actions::remote::ReceiptStatus::Accepted
                    | tilo_actions::remote::ReceiptStatus::Completed { .. },
                ..
            }
        )
    )
}

#[tokio::test(flavor = "current_thread")]
async fn an_effects_call_of_another_apps_action_is_its_callers_by_their_grants() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::test_support::{Grants as _, KindSet, Subject};
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            {
                let workspace = &mut env.membership.write().workspaces[0];
                workspace.admit_agent("scribe", KindSet::READ);
                workspace.allow_app(Subject::agent("scribe"), &env.route.app_id, KindSet::ALL);
                workspace.allow_app(Subject::agent("scribe"), NOTES, KindSet::ALL);
            }
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let asked = id(0xc4a1);
            invoke(&agent.handle, &rig.binding, asked).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, "agent:scribe");

            // The person could send mail, and the app is theirs; the agent
            // may not, so its effect may not either.
            let beyond = app_asks(&app, call(MAIL_PEER, "mail.outbox", "send", Some(asked))).await;
            assert!(!admitted(&beyond), "{beyond:?}");

            let child = new_note(Some(asked));
            let made = app_asks(&app, child.clone()).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes runs the call");
            assert_eq!(note.action, "create-page");
            assert_eq!(note.actor, "agent:scribe", "delivered as the agent's own");
            let invoker = tilo_actions::ActionInvoker::delivered(&note.actor, note.context());
            assert_eq!(invoker.actor, "agent:scribe", "its history names the agent");
            assert!(notes.try_recv().is_err(), "one call, one delivery");

            // Kept under the agent's own custody: the agent follows the
            // child it asked for through its app, as its own invocation.
            let agent_caller = rig
                .state
                .continuing_caller(903, asked)
                .expect("the invocation still runs for the agent");
            assert!(
                rig.state
                    .action_observe(&agent_caller, &child.invocation)
                    .is_some(),
                "the child is the agent's"
            );

            // An invocation nobody delivered to the app, and a child reusing
            // the invocation it continues, are nobody's.
            let foreign = app_asks(&app, new_note(Some(id(0xc4a2)))).await;
            assert!(refused(&foreign), "{foreign:?}");
            let mut reused = new_note(Some(asked));
            reused.invocation = asked;
            let reused = app_asks(&app, reused).await;
            assert!(refused(&reused), "{reused:?}");
            assert!(notes.try_recv().is_err());
            drop(agent);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_call_continuing_what_an_app_asked_of_itself_is_the_apps_own() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let own = id(0xc4a3);
            invoke(&app, &rig.binding, own).await.unwrap();
            rig.delivered_effect(&env, own).await;

            // The person may make notes, but the app holds no grant to
            // Notes: naming its own invocation does not make it theirs.
            let laundered = app_asks(&app, new_note(Some(own))).await;
            assert!(refused(&laundered), "{laundered:?}");
            let unnamed = app_asks(&app, new_note(None)).await;
            assert!(refused(&unnamed), "{unnamed:?}");
            assert!(notes.try_recv().is_err(), "nothing reached Notes");
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_persons_invocation_lets_its_effect_call_another_app_as_them() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let person = rig.caller(&env, 906, None).await;
            let asked = id(0xc4a4);
            invoke(&person.handle, &rig.binding, asked).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();

            let made = app_asks(&app, new_note(Some(asked))).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes runs the call");
            assert_eq!(note.actor, delivered.actor, "the person who asked");
            drop(person);
            rig.stop().await;
        })
        .await;
}
