// An app's own connection opens an application only for the person's press
// in it or for an invocation someone else asked of it: a person at one of
// the host's apis, or an agent within its own grants.

/// A launcher that opens what it is asked and records what it opened.
struct PressedOpens {
    opened: Mutex<Vec<String>>,
}

impl PressedOpens {
    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl crate::services::LauncherServices for PressedOpens {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        [
            "so.tilo.word-processor",
            "so.tilo.chat",
            "so.tilo.notes",
            "so.tilo.tasks",
        ]
        .into_iter()
        .map(|app| crate::services::LaunchableApplication {
            capabilities: Vec::new(),
            package_id: app.into(),
            display_name: app.into(),
            unconfined: false,
            offers: Vec::new(),
            surface: None,
        })
        .collect()
    }
    fn launch(
        &self,
        app: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        self.opened.lock().unwrap().push(app.into());
        Ok(())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
}

async fn pressed_opens_rig(env: &Environment) -> (Rig, Arc<PressedOpens>) {
    let launcher = Arc::new(PressedOpens {
        opened: Mutex::new(Vec::new()),
    });
    let rig = Rig::with_services(
        env,
        ProcessServices {
            launcher: Some(launcher.clone()),
            ..env.membership.services()
        },
    )
    .await;
    // The app's connection is window `WINDOW` of this desktop: the compositor issues its presses
    // there, and a call naming one of them proves it.
    rig.state.bind_window_instance_for_test(APP_PEER, WINDOW);
    (rig, launcher)
}

fn open_app_request(
    workspace: &str,
    app: &str,
    on_behalf_of: Option<tilo_actions::InvocationId>,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        on_behalf_of,
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(workspace),
            address: tilo_actions::ActionAddress::new(
                format!("{}{app}", tilo_actions::remote::APPLICATION_RESOURCE_PREFIX),
                tilo_actions::remote::APPLICATION_OPEN_ACTION,
            ),
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

/// The app connection the rig's worker is, and the window it is: the compositor issues a press's
/// serial to that window.
const APP_PEER: u64 = 903;
const WINDOW: u64 = 11;

async fn app_asks(
    handle: &runtime_collab::RecordHandle,
    request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::RemoteActionMessage {
    app_asks_pressed(handle, request, None).await
}

/// [`app_asks`], naming the input serial of the person's press the call follows, if any.
async fn app_asks_pressed(
    handle: &runtime_collab::RecordHandle,
    request: tilo_actions::remote::InvokeRequest,
    press: Option<u32>,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
        .pressed(press)
        .encode()
        .unwrap();
    RemoteActionFrame::decode(&handle.remote_action(&frame).await.unwrap())
        .unwrap()
        .message
}

fn refused(answer: &tilo_actions::remote::RemoteActionMessage) -> bool {
    matches!(
        answer,
        tilo_actions::remote::RemoteActionMessage::Receipt(
            tilo_actions::remote::InvocationReceipt {
                status: tilo_actions::remote::ReceiptStatus::Rejected {
                    error: tilo_actions::remote::ActionCallError::NotPermitted,
                },
                ..
            }
        )
    )
}

#[tokio::test(flavor = "current_thread")]
async fn an_app_opens_an_app_only_for_the_persons_press_in_it() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new(true);
            let (rig, launcher) = pressed_opens_rig(&env).await;
            let workspace = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();

            // A call naming no press is the app's own: it opens nothing.
            let unasked = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                None,
            )
            .await;
            assert!(refused(&unasked), "{unasked:?}");
            assert!(launcher.opened().is_empty());

            // The compositor issued a serial to another window of the desktop: it proves nothing
            // for this app's own window.
            rig.state.note_press(WINDOW + 1, 1, None);
            let elsewhere = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                Some(1),
            )
            .await;
            assert!(
                refused(&elsewhere),
                "a press in another window proves nothing here"
            );

            // The person's press in this app's window, which the call names, opens one app.
            rig.state.note_press(WINDOW, 2, None);
            let pressed = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                Some(2),
            )
            .await;
            assert!(!refused(&pressed), "{pressed:?}");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);

            // One press proves one call: the same serial again is the app's own.
            let again = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                Some(2),
            )
            .await;
            assert!(refused(&again), "one press proves one open");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);

            // A serial older than the window proves nothing either, and neither does one the
            // compositor never issued.
            rig.state.note_stale_press(WINDOW, 3);
            let stale = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                Some(3),
            )
            .await;
            assert!(refused(&stale), "an old press proves nothing");
            let synthetic = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", None),
                Some(99),
            )
            .await;
            assert!(refused(&synthetic), "a serial nobody issued proves nothing");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_invocation_an_app_made_of_itself_opens_nothing_without_a_press() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new(true);
            let (rig, launcher) = pressed_opens_rig(&env).await;
            let workspace = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();
            let own = id(0xa0d1);
            invoke(&app, own).await.unwrap();
            rig.delivered_effect(&env, own).await;

            let laundered = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", Some(own)),
                None,
            )
            .await;
            assert!(refused(&laundered), "{laundered:?}");

            // The child call the pressed invocation makes runs as the person: it opens what the
            // app itself may not, because the press named is one the person made in its window.
            rig.state.note_press(WINDOW, 7, None);
            let pressed = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", Some(own)),
                Some(7),
            )
            .await;
            assert!(!refused(&pressed), "{pressed:?}");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_agents_invocation_opens_what_the_agent_may_open() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::test_support::KindSet;
            let env = Environment::new(true);
            let (rig, launcher) = pressed_opens_rig(&env).await;
            env.agent_may(
                "scribe",
                KindSet::READ,
                &[&env.route.app_id, "so.tilo.chat"],
            );
            let workspace = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let asked = id(0xa0d2);
            invoke(&agent.handle, asked).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, tilo_actions::Actor::Member(env.scribe()));

            let beyond = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.notes", Some(asked)),
                None,
            )
            .await;
            assert!(refused(&beyond), "the agent may not open Notes: {beyond:?}");
            let allowed = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", Some(asked)),
                None,
            )
            .await;
            assert!(!refused(&allowed), "{allowed:?}");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);

            let foreign = app_asks_pressed(
                &app,
                open_app_request(&workspace, "so.tilo.chat", Some(id(0xa0d3))),
                None,
            )
            .await;
            assert!(
                refused(&foreign),
                "an invocation nobody asked of it is no authority"
            );
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);
            drop(agent);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_approve_an_app_sends_on_its_own_connection_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new(true);
            let (rig, launcher) = pressed_opens_rig(&env).await;
            let workspace = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();

            // Pressed, so the open would go on: only the approve it carries refuses it.
            rig.state.note_press(WINDOW, 7, None);
            let mut forged = open_app_request(&workspace, "so.tilo.chat", None);
            forged.confirmation = Some(tilo_actions::remote::Confirmation::Asked(
                tilo_actions::remote::asked::Approval::new(
                    tilo_actions::remote::asked::RecordRef {
                        workspace: workspace.clone(),
                        record: forged.invocation.0.to_string(),
                        device: None,
                    },
                    None,
                ),
            ));
            let answer = app_asks_pressed(&app, forged, Some(7)).await;
            assert!(
                matches!(
                    &answer,
                    tilo_actions::remote::RemoteActionMessage::Receipt(
                        tilo_actions::remote::InvocationReceipt {
                            status: tilo_actions::remote::ReceiptStatus::Rejected {
                                error: tilo_actions::remote::ActionCallError::Refused { reason },
                            },
                            ..
                        }
                    ) if reason == tilo_actions::remote::asked::FORGED
                ),
                "{answer:?}"
            );
            assert!(launcher.opened().is_empty());
            rig.stop().await;
        })
        .await;
}
