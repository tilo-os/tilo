// What an app's effect asks of another app while it runs an invocation
// someone asked of it is that someone's: decided by their grants, delivered
// as theirs, and never widened by the app's own identity or by an
// invocation the app made of itself.

const NOTES: &str = "so.tilo.notes";
const NOTES_PEER: u64 = 908;
const MAIL: &str = "so.tilo.mail";
const MAIL_PEER: u64 = 909;

/// `action` on `resource`, published as a background job of `kind`,
/// tagged `tags`.
fn published(
    resource: &str,
    action: &str,
    kind: tilo_actions::ActionKind,
    tags: &[&str],
) -> PublishedAction {
    PublishedAction {
        state: tilo_actions::ActionState::default(),
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
            kind,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            result: tilo_actions::ResultSpec::Job,
            tags: tags.iter().map(|tag| (*tag).to_owned()).collect(),
            ..Default::default()
        },
        input: PublishedInput::None,
        view: None,
    }
}

/// Notes, publishing its notebook's `create-page` (an edit), `read-page`
/// (a read) and `retitle-mine` (an edit of the person's own record, which
/// needs only reading) on connection [`NOTES_PEER`], and Mail its `send` on
/// [`MAIL_PEER`]; what is delivered to Notes arrives on the receiver.
fn publish_notes(
    rig: &Rig,
    env: &Environment,
) -> tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope> {
    use tilo_actions::ActionKind::{Read, Undoable};
    let surface = |peer_id: u64, app_id: &str, actions: Vec<PublishedAction>| PublishedSurface {
        peer_id,
        app_id: app_id.into(),
        route: CrdtRoute::record(app_id, env.route.cluster_id.clone(), "record"),
        actions,
        live: true,
    };
    let notebook = "notes.notebook";
    rig.state.replace_published_surfaces_for_test(vec![
        surface(
            NOTES_PEER,
            NOTES,
            vec![
                published(notebook, "create-page", Undoable, &[]),
                published(notebook, "read-page", Read, &[]),
                published(
                    notebook,
                    "retitle-mine",
                    Undoable,
                    &[tilo_actions::OWN_RECORD_SCOPE],
                ),
            ],
        ),
        surface(
            MAIL_PEER,
            MAIL,
            vec![published("mail.outbox", "send", Undoable, &[])],
        ),
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

/// Notes' `action` on its notebook, continuing `on_behalf_of`.
fn notebook(
    action: &str,
    on_behalf_of: tilo_actions::InvocationId,
) -> tilo_actions::remote::InvokeRequest {
    call(NOTES_PEER, "notes.notebook", action, Some(on_behalf_of))
}

/// Whether `answer` waits for the person's confirmation nobody gave.
fn unconfirmed(answer: &tilo_actions::remote::RemoteActionMessage) -> bool {
    matches!(
        answer,
        tilo_actions::remote::RemoteActionMessage::Receipt(
            tilo_actions::remote::InvocationReceipt {
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionHostError::ConfirmationRequired,
                },
                ..
            }
        )
    )
}

/// The host's confirmation sheet, handing each question to the test.
struct PersonSheet(
    tokio::sync::mpsc::UnboundedSender<(
        crate::app_actions::AppActionConsentRequest,
        tokio::sync::oneshot::Sender<crate::app_actions::AppActionAnswer>,
    )>,
);
impl crate::app_actions::AppActionConsentPrompter for PersonSheet {
    fn ask(
        &self,
        request: crate::app_actions::AppActionConsentRequest,
    ) -> Option<tokio::sync::oneshot::Receiver<crate::app_actions::AppActionAnswer>> {
        let (answer, receive) = tokio::sync::oneshot::channel();
        self.0.send((request, answer)).ok()?;
        Some(receive)
    }
}

/// That `answer` admitted `request`, and Notes ran it for the agent: the
/// invocation it continues from there.
fn ran(
    notes: &mut tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
    request: &tilo_actions::remote::InvokeRequest,
    answer: &tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::InvocationId {
    assert!(admitted(answer), "{answer:?}");
    let delivered = notes.try_recv().expect("Notes runs the call");
    assert_eq!(delivered.invocation, request.invocation);
    assert_eq!(delivered.actor, "agent:scribe");
    request.invocation
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
            invoke(&agent.handle, asked).await.unwrap();
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
            invoke(&app, own).await.unwrap();
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
async fn a_child_reaches_no_further_than_what_it_continues() {
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
            // Notes runs on a connection of its own, whose effects continue
            // what was delivered to it.
            let notes_app = rig.app_caller(&env, NOTES_PEER, NOTES, None).await;
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let asked = id(0xc4b1);
            invoke(&agent.handle, asked).await.unwrap();
            rig.stream.as_ref().unwrap().read().await.unwrap();
            let notes = &mut notes;

            // An edit continuing an edit, and a read, stay within it.
            let edit = new_note(Some(asked));
            let edit = ran(notes, &edit, &app_asks(&app, edit.clone()).await);
            let read = notebook("read-page", asked);
            let read = ran(notes, &read, &app_asks(&app, read.clone()).await);

            // The agent may edit Notes, but what continues its read reads:
            // an edit is put to the person, and nobody here answers.
            let beyond = app_asks(&notes_app.handle, notebook("create-page", read)).await;
            assert!(unconfirmed(&beyond), "{beyond:?}");
            let within = notebook("read-page", read);
            let within = ran(
                notes,
                &within,
                &app_asks(&notes_app.handle, within.clone()).await,
            );
            let after_edit = notebook("create-page", edit);
            ran(
                notes,
                &after_edit,
                &app_asks(&notes_app.handle, after_edit.clone()).await,
            );

            // Down a chain: an edit of the person's own record needs only
            // reading, so it runs under the read, and reaches no further
            // than it.
            let own = notebook("retitle-mine", read);
            let own = ran(notes, &own, &app_asks(&notes_app.handle, own.clone()).await);
            for parent in [own, within] {
                let beyond = app_asks(&notes_app.handle, notebook("create-page", parent)).await;
                assert!(unconfirmed(&beyond), "{beyond:?}");
            }
            // Naming a confirmation of its own answers nothing.
            let mut claimed = notebook("create-page", read);
            claimed.confirmation = Some(tilo_actions::remote::Confirmation::Confirmed);
            let claimed = app_asks(&notes_app.handle, claimed).await;
            assert!(!admitted(&claimed), "{claimed:?}");
            assert!(notes.try_recv().is_err(), "nothing beyond reached Notes");

            // The person answers it on their own device as its own request,
            // and then it runs.
            let (sheet, mut sheets) = tokio::sync::mpsc::unbounded_channel();
            rig.state
                .set_app_action_consent(Arc::new(PersonSheet(sheet)));
            let confirmed = notebook("create-page", read);
            let notes_handle = notes_app.handle.clone();
            let asking = app_asks(&notes_handle, confirmed.clone());
            tokio::pin!(asking);
            let (question, answer) = tokio::select! {
                answer = &mut asking => panic!("ran unasked: {answer:?}"),
                sheet = sheets.recv() => sheet.unwrap(),
            };
            assert_eq!(
                (question.target_app.as_str(), question.action.as_str()),
                (NOTES, "create-page")
            );
            assert!(
                notes.try_recv().is_err(),
                "nothing runs while the person decides"
            );
            answer
                .send(crate::app_actions::AppActionConsent::Once.into())
                .unwrap();
            let answered = asking.await;
            ran(notes, &confirmed, &answered);

            drop((agent, notes_app));
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_persons_invocation_lets_its_effect_call_another_app_as_them() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = native_person_rig(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let asked = id(0xc4a4);
            let delivered = person_asks(&rig, &env, asked).await;

            let made = app_asks(&app, new_note(Some(asked))).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes runs the call");
            assert_eq!(note.actor, delivered.actor, "the person who asked");
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_persons_read_does_not_make_their_edit_unasked() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = native_person_rig(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let notes_app = rig.app_caller(&env, NOTES_PEER, NOTES, None).await;
            let asked = id(0xc4b2);
            person_asks(&rig, &env, asked).await;
            let read = notebook("read-page", asked);
            assert!(admitted(&app_asks(&app, read.clone()).await));
            notes.try_recv().expect("Notes reads");

            // The person may edit; their read's effect may not, even when it
            // claims their confirmation.
            for confirmation in [None, Some(tilo_actions::remote::Confirmation::Confirmed)] {
                let mut edit = notebook("create-page", read.invocation);
                edit.confirmation = confirmation;
                let edit = app_asks(&notes_app.handle, edit).await;
                assert!(!admitted(&edit), "{edit:?}");
            }
            assert!(notes.try_recv().is_err(), "no edit reached Notes");
            drop(notes_app);
            rig.stop().await;
        })
        .await;
}

/// The rig with the host's native seat, the door the person's own palette
/// and sheets invoke through.
async fn native_person_rig(env: &Environment) -> Rig {
    Rig::with_services(
        env,
        HostServices {
            local_person: Some(Arc::new(super::signed_worker::NativePerson(
                env.authority.clone(),
            ))),
            ..env.membership.services()
        },
    )
    .await
}

/// The person asks the rig's application for its action as `asked`; the
/// invocation it is handed.
async fn person_asks(
    rig: &Rig,
    env: &Environment,
    asked: tilo_actions::InvocationId,
) -> runtime_collab::Invocation {
    let mut request = local_host_request(true);
    request.invocation = asked;
    let receipt = local_host_answer(rig.state.action_invoke_local_person(
        &super::signed_worker::native_person(&env.route, &env.cert),
        &request,
    ))
    .await
    .unwrap();
    assert!(
        matches!(
            receipt.status,
            tilo_actions::remote::ReceiptStatus::Accepted
        ),
        "{receipt:?}"
    );
    rig.stream.as_ref().unwrap().read().await.unwrap()
}
