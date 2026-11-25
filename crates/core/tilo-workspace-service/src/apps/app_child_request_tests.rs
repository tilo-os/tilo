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
        did: None,
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
/// (a read), `clear-done` (destructive) and `empty-trash` (which asks its caller
/// first) on connection [`NOTES_PEER`], and Mail its `send` on
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
                {
                    let mut clear = published(notebook, "clear-done", Undoable, &[]);
                    clear.contract.risk = tilo_actions::RiskLevel::Destructive;
                    clear
                },
                {
                    let mut empty = published(notebook, "empty-trash", Undoable, &[]);
                    empty.contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                    empty
                },
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
        signed: None,
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
                    error: tilo_actions::remote::ActionCallError::ConfirmationRequired { .. },
                },
                ..
            }
        )
    )
}

/// `request`, carrying a person's approve of an asked record it never was kept as.
fn approved(
    mut request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::InvokeRequest {
    request.confirmation = Some(tilo_actions::remote::Confirmation::Asked(
        tilo_actions::remote::asked::Approval::new(
            tilo_actions::remote::asked::RecordRef {
                workspace: "w1".to_owned(),
                record: format!("tilo.invocations/{}", request.invocation.0),
                device: None,
            },
            None,
        ),
    ));
    request
}

/// What `answer` waits for the person with: the facts they are shown.
fn asked(
    answer: &tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::asked::AskedFacts {
    match answer {
        tilo_actions::remote::RemoteActionMessage::Receipt(
            tilo_actions::remote::InvocationReceipt {
                status:
                    tilo_actions::remote::ReceiptStatus::Rejected {
                        error: tilo_actions::remote::ActionCallError::ConfirmationRequired(facts),
                    },
                ..
            },
        ) => (**facts).clone(),
        other => panic!("not waiting for the person: {other:?}"),
    }
}

/// That `answer` admitted `request`, and Notes ran it for the agent: the
/// invocation it continues from there.
fn ran(
    notes: &mut tokio::sync::mpsc::UnboundedReceiver<InvocationEnvelope>,
    request: &tilo_actions::remote::InvokeRequest,
    answer: &tilo_actions::remote::RemoteActionMessage,
    scribe: tilo_actions::MemberId,
) -> tilo_actions::InvocationId {
    assert!(admitted(answer), "{answer:?}");
    let delivered = notes.try_recv().expect("Notes runs the call");
    assert_eq!(delivered.invocation, request.invocation);
    assert_eq!(delivered.actor, tilo_actions::Actor::Member(scribe));
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
            use crate::apps::test_support::KindSet;
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            env.agent_may("scribe", KindSet::READ, &[&env.route.app_id, NOTES]);
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let asked = id(0xc4a1);
            invoke(&agent.handle, asked).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, tilo_actions::Actor::Member(env.scribe()));

            // The person could send mail, and the app is theirs; the agent
            // may not, so its effect may not either.
            let beyond = app_asks(&app, call(MAIL_PEER, "mail.outbox", "send", Some(asked))).await;
            assert!(!admitted(&beyond), "{beyond:?}");

            let child = new_note(Some(asked));
            let made = app_asks(&app, child.clone()).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes runs the call");
            assert_eq!(note.action, "create-page");
            assert_eq!(
                note.actor,
                tilo_actions::Actor::Member(env.scribe()),
                "delivered as the agent's own"
            );
            let invoker = tilo_actions::ActionInvoker::delivered(&note.actor, note.context());
            assert_eq!(
                invoker.acting.by,
                tilo_actions::Actor::member(env.scribe()),
                "its history names the agent"
            );
            assert!(notes.try_recv().is_err(), "one call, one delivery");

            // Kept under the agent's own ownership: the agent follows the
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
async fn a_call_continuing_the_persons_press_is_theirs_and_an_app_cannot_make_one() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let (rig, _launcher) = pressed_opens_rig(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let delivered = |id: tilo_actions::InvocationId| {
                let stream = rig.stream.as_ref().unwrap();
                async move {
                    let delivered = stream.read().await.unwrap();
                    assert_eq!(delivered.invocation_uuid, *id.0.as_bytes());
                    delivered
                }
            };

            // The person presses in the app's window: the app's call of its own action is
            // theirs, and so is the call its code makes of another app while it runs.
            rig.state.note_press(WINDOW, 7, None);
            let press = id(0xc4c1);
            {
                use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
                let frame =
                    RemoteActionFrame::new(RemoteActionMessage::Invoke(durable_request(press)))
                        .pressed(Some(7))
                        .encode()
                        .unwrap();
                app.remote_action(&frame).await.unwrap();
            }
            let pressed = delivered(press).await;
            let child = new_note(Some(press));
            let made = app_asks(&app, child.clone()).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes runs the call");
            assert_eq!(note.invocation, child.invocation);
            assert_eq!(
                note.actor, pressed.actor,
                "decided and delivered as the person"
            );

            // One press makes one call: the app's next call of its own action, which nobody
            // pressed for, is the app's own, and the app holds no grant to Notes.
            let unpressed = id(0xc4c2);
            invoke(&app, unpressed).await.unwrap();
            delivered(unpressed).await;
            let forged = app_asks(&app, new_note(Some(unpressed))).await;
            assert!(refused(&forged), "{forged:?}");
            assert!(notes.try_recv().is_err(), "nothing more reached Notes");
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_child_reaches_no_further_than_what_it_continues() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::apps::test_support::KindSet;
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            env.agent_may("scribe", KindSet::READ, &[&env.route.app_id, NOTES]);
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            // Notes runs on a connection of its own, whose effects continue
            // what was delivered to it.
            let notes_app = rig.app_caller(&env, NOTES_PEER, NOTES, None).await;
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let begun = id(0xc4b1);
            invoke(&agent.handle, begun).await.unwrap();
            rig.stream.as_ref().unwrap().read().await.unwrap();
            let notes = &mut notes;
            let scribe = || env.scribe();

            // An edit continuing an edit, and a read, stay within it.
            let edit = new_note(Some(begun));
            let edit = ran(notes, &edit, &app_asks(&app, edit.clone()).await, scribe());
            let read = notebook("read-page", begun);
            let read = ran(notes, &read, &app_asks(&app, read.clone()).await, scribe());

            // The agent may edit Notes, but what continues its read reads:
            // an edit is put to the person, and nobody here answers.
            let beyond = app_asks(&notes_app.handle, notebook("create-page", read)).await;
            assert!(unconfirmed(&beyond), "{beyond:?}");
            let within = notebook("read-page", read);
            let within = ran(
                notes,
                &within,
                &app_asks(&notes_app.handle, within.clone()).await,
                scribe(),
            );
            let after_edit = notebook("create-page", edit);
            ran(
                notes,
                &after_edit,
                &app_asks(&notes_app.handle, after_edit.clone()).await,
                scribe(),
            );

            // Down a chain: what continues the read reaches no further than it.
            let beyond = app_asks(&notes_app.handle, notebook("create-page", within)).await;
            assert!(unconfirmed(&beyond), "{beyond:?}");
            // Naming a confirmation of its own answers nothing.
            let mut claimed = notebook("create-page", read);
            claimed.confirmation = Some(tilo_actions::remote::Confirmation::Confirmed);
            let claimed = app_asks(&notes_app.handle, claimed).await;
            assert!(!admitted(&claimed), "{claimed:?}");
            assert!(notes.try_recv().is_err(), "nothing beyond reached Notes");

            // It waits for the person as its asked record, showing them Notes' action; an approve
            // the app sends itself answers nothing: only the dispatch carries one.
            let waiting = app_asks(&notes_app.handle, notebook("create-page", read)).await;
            assert_eq!(asked(&waiting).verb, "create-page");
            let forged = app_asks(&notes_app.handle, approved(notebook("create-page", read))).await;
            assert!(!admitted(&forged) && !unconfirmed(&forged), "{forged:?}");
            assert!(notes.try_recv().is_err(), "nothing ran unanswered");

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

/// What a person's invocation calls of another app's action it does not declare is no part of
/// what their yes to it covered: it is decided as their own call of that action would be. One no
/// rule asks of them runs as them; one whose rule asks them first waits for them, standing over
/// the window the invocation was delivered to.
#[tokio::test(flavor = "current_thread")]
async fn a_persons_child_call_asks_them_only_where_its_own_rule_does() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = native_person_rig(&env).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();
            let parent = id(0xc4d1);
            let delivered = person_asks(&rig, &env, parent).await;

            // No rule asks the person before they make a page: it runs, as them.
            let made = app_asks(&app, new_note(Some(parent))).await;
            assert!(admitted(&made), "{made:?}");
            let note = notes.try_recv().expect("Notes makes the page");
            assert_eq!(note.actor, delivered.actor, "the person who asked");

            // Emptying the trash asks its caller first: their yes to the invocation does not
            // answer it, and nothing runs while it waits.
            let empty = call(NOTES_PEER, "notes.notebook", "empty-trash", Some(parent));
            let facts = asked(&app_asks(&app, empty.clone()).await);
            assert_eq!(facts.verb, "empty-trash");
            assert!(
                notes.try_recv().is_err(),
                "nothing runs while the person decides"
            );

            // Kept for them, the question stands over the window the invocation was delivered
            // to, as one a press there asked.
            rig.state.bind_window_instance_for_test(903, 41);
            let device = env.cert.device_public_key;
            assert_eq!(
                rig.state.parent_view(903, &device, &empty),
                Some(tilo_actions::remote::asked::Pressed {
                    device: hex::encode(env.cert.device_public_key),
                    app: env.route.app_id.clone(),
                    instance: "41".to_owned(),
                    resource: "workspace:atlas".to_owned(),
                    at: None,
                })
            );
            assert_eq!(
                rig.state.parent_view(903, &device, &new_note(None)),
                None,
                "a call that continues nothing asks over no window of its own"
            );
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

/// The rig with the host's native device, the API the person's own palette
/// and sheets invoke through.
async fn native_person_rig(env: &Environment) -> Rig {
    Rig::with_services(
        env,
        ProcessServices {
            local_person: Some(Arc::new(super::signed_worker::NativePerson(
                env.authority.clone(),
            ))),
            ..env.membership.services()
        },
    )
    .await
}

/// The device of a host whose identity a login is still writing: reading it
/// answers that the registry has not named the person yet, and the ask the
/// device's first action makes waits for it, after which the identity is there.
struct Arriving {
    host: Arc<CurrentHost>,
    named: std::sync::atomic::AtomicBool,
}

impl crate::apps::services::LocalPersonServices for Arriving {
    fn certificate(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if !self.named.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("this device's identity is not ready yet".into());
        }
        crate::apps::services::LocalPersonServices::certificate(
            &super::signed_worker::NativePerson(Arc::clone(&self.host)),
            workspace,
            person,
        )
    }

    fn certificate_when_named(
        &self,
        workspace: &str,
        person: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        // What the wait ends on: the registry names the person, and the identity is there from
        // then on — the effect's own re-check asks again.
        self.named.store(true, std::sync::atomic::Ordering::SeqCst);
        self.certificate(workspace, person)
    }
}

/// A person's first action of a login is answered by the identity, not
/// refused before the registry names them.
#[tokio::test(flavor = "current_thread")]
async fn a_persons_first_action_is_answered_once_the_identity_names_them() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::with_services(
                &env,
                ProcessServices {
                    local_person: Some(Arc::new(Arriving {
                        host: env.authority.clone(),
                        named: std::sync::atomic::AtomicBool::new(false),
                    })),
                    ..env.membership.services()
                },
            )
            .await;
            let _notes = publish_notes(&rig, &env);
            let asked = id(0xc4c1);
            // The receipt is accepted, as `person_asks` holds: an identity that is being written
            // is waited for, never refused.
            person_asks(&rig, &env, asked).await;
            rig.stop().await;
        })
        .await;
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

/// Where the test's workspace records the grants a person gives.
struct TreeGrants(crate::apps::test_support::WorkspaceTreeHandle);
impl crate::apps::services::WorkspaceGrants for TreeGrants {
    fn spend(&self, _workspace: &str, grant: &str) -> bool {
        self.0
            .write()
            .workspaces
            .first_mut()
            .unwrap()
            .policy
            .revoke_grant(&tilo_action_policy::GrantId::new(grant))
    }
}

#[tokio::test(flavor = "current_thread")]
async fn an_apps_own_change_of_another_app_waits_for_the_person_until_a_grant_names_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let mut services = env.membership.services();
            // Where a grant given once is taken back: the workspace's grants.
            services.workspace_grants = Some(Arc::new(TreeGrants(env.membership.clone())));
            // The app declares that it takes other apps' actions.
            services.app_action_access = Some(Arc::new(|_: &str| {
                crate::apps::app_actions::AppActionAccess {
                    query: false,
                    invoke: true,
                }
            }));
            let rig = Rig::with_services(&env, services).await;
            let mut notes = publish_notes(&rig, &env);
            let app = rig.worker.handle.clone();

            // The app holds no grant to Notes: the call waits for the person, naming the app as
            // who asks and Notes' action as what. Asked of the app by its id, as one app asks
            // another's api.
            let mut clear = call(NOTES_PEER, "notes.notebook", "clear-done", None);
            clear.target = tilo_actions::remote::application_action_target(
                &env.route.cluster_id,
                NOTES,
                "notes.notebook",
                "clear-done",
            );
            let facts = asked(&app_asks(&app, clear.clone()).await);
            assert!(
                matches!(
                    &facts.by,
                    tilo_actions::remote::asked::AskedBy::App { app, .. } if *app == env.route.app_id
                ),
                "{facts:?}"
            );
            assert_eq!(facts.verb, "clear-done");
            assert!(facts.effect.is_some(), "a grant can be given with the answer");
            assert!(notes.try_recv().is_err(), "nothing runs while the person decides");
            // An approve the app sends itself answers nothing.
            let forged = app_asks(&app, approved(clear.clone())).await;
            assert!(!admitted(&forged) && !unconfirmed(&forged), "{forged:?}");
            assert!(notes.try_recv().is_err(), "nothing runs on the app's own word");

            // A grant naming the app, as approving it for good gives, lets it run unasked.
            let granted = {
                use crate::apps::test_support::{Grants as _, KindSet};
                env.membership.write().workspaces[0].allow_app(
                    tilo_action_policy::Grantee::app(env.route.app_id.clone()),
                    NOTES,
                    KindSet::ALL,
                )
            };
            let unasked = app_asks(&app, call(NOTES_PEER, "notes.notebook", "create-page", None)).await;
            assert!(admitted(&unasked), "{unasked:?}");
            notes.try_recv().expect("Notes makes the page");

            // Revoked, as Privacy & Security does, the app waits for the person again.
            assert!(
                env.membership.write().workspaces[0]
                    .policy
                    .revoke_grant(&granted),
                "the grant is revoked"
            );
            let again = app_asks(&app, call(NOTES_PEER, "notes.notebook", "create-page", None)).await;
            assert!(unconfirmed(&again), "{again:?}");
            assert!(notes.try_recv().is_err(), "Notes is handed nothing once revoked");
            rig.stop().await;
        })
        .await;
}
