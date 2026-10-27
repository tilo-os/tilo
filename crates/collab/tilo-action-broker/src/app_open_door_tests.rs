// An app's own connection opens an application only for the person's press
// in it or for an invocation someone else asked of it: a person at one of
// the host's doors, or an agent within its own grants.

/// A launcher that opens what it is asked and answers whether the person
/// pressed in the asking app, each press proving one open.
struct PressedOpens {
    presses: Mutex<Vec<String>>,
    opened: Mutex<Vec<String>>,
}

impl PressedOpens {
    fn press(&self, app: &str) {
        self.presses.lock().unwrap().push(app.into());
    }
    fn opened(&self) -> Vec<String> {
        self.opened.lock().unwrap().clone()
    }
}

impl crate::services::LauncherServices for PressedOpens {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        ["so.tilo.word-processor", "so.tilo.chat", "so.tilo.notes"]
            .into_iter()
            .map(|app| crate::services::LaunchableApplication {
                capabilities: Vec::new(),
                package_id: app.into(),
                display_name: app.into(),
                unconfined: false,
                offers: Vec::new(),
            })
            .collect()
    }
    fn person_initiated(&self, app_id: &str) -> bool {
        let mut presses = self.presses.lock().unwrap();
        let before = presses.len();
        if let Some(at) = presses.iter().position(|app| app == app_id) {
            presses.remove(at);
        }
        presses.len() != before
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
        presses: Mutex::new(Vec::new()),
        opened: Mutex::new(Vec::new()),
    });
    let rig = Rig::with_services(
        env,
        HostServices {
            launcher: Some(launcher.clone()),
            ..env.membership.services()
        },
    )
    .await;
    (rig, launcher)
}

fn open_app_request(
    room: &str,
    app: &str,
    on_behalf_of: Option<tilo_actions::InvocationId>,
) -> tilo_actions::remote::InvokeRequest {
    tilo_actions::remote::InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        on_behalf_of,
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(room),
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
    }
}

async fn app_asks(
    handle: &runtime_collab::ChannelHandle,
    request: tilo_actions::remote::InvokeRequest,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
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
                    error: tilo_actions::remote::ActionHostError::NotPermitted,
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
            let env = Environment::new();
            let (rig, launcher) = pressed_opens_rig(&env).await;
            let room = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();

            let unasked = app_asks(&app, open_app_request(&room, "so.tilo.chat", None)).await;
            assert!(refused(&unasked), "{unasked:?}");
            assert!(launcher.opened().is_empty());

            launcher.press("so.tilo.chat");
            let elsewhere = app_asks(&app, open_app_request(&room, "so.tilo.chat", None)).await;
            assert!(
                refused(&elsewhere),
                "a press in another app proves nothing here"
            );

            launcher.press(&env.route.app_id);
            let pressed = app_asks(&app, open_app_request(&room, "so.tilo.chat", None)).await;
            assert!(!refused(&pressed), "{pressed:?}");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);

            let again = app_asks(&app, open_app_request(&room, "so.tilo.chat", None)).await;
            assert!(refused(&again), "one press proves one open");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn an_invocation_an_app_made_of_itself_opens_nothing_without_a_press() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let (rig, launcher) = pressed_opens_rig(&env).await;
            let room = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();
            let own = id(0xa0d1);
            invoke(&app, &rig.binding, own).await.unwrap();
            rig.delivered_effect(&env, own).await;

            let laundered =
                app_asks(&app, open_app_request(&room, "so.tilo.chat", Some(own))).await;
            assert!(refused(&laundered), "{laundered:?}");
            let launch = app
                .launch_installed_application_for(
                    "so.tilo.chat",
                    None,
                    None,
                    Some(*own.0.as_bytes()),
                )
                .await;
            assert!(launch.is_err(), "{launch:?}");
            assert!(launcher.opened().is_empty());

            launcher.press(&env.route.app_id);
            let pressed = app_asks(&app, open_app_request(&room, "so.tilo.chat", Some(own))).await;
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
            use crate::test_support::{Grants as _, KindSet, Subject};
            let env = Environment::new();
            let (rig, launcher) = pressed_opens_rig(&env).await;
            {
                let workspace = &mut env.membership.write().workspaces[0];
                workspace.admit_agent("scribe", KindSet::READ);
                workspace.allow_app(Subject::agent("scribe"), &env.route.app_id, KindSet::ALL);
                workspace.allow_app(Subject::agent("scribe"), "so.tilo.chat", KindSet::ALL);
            }
            let room = env.route.cluster_id.clone();
            let app = rig.worker.handle.clone();
            let agent = rig.caller(&env, 905, Some("scribe")).await;
            let asked = id(0xa0d2);
            invoke(&agent.handle, &rig.binding, asked).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(delivered.actor, "agent:scribe");

            let beyond =
                app_asks(&app, open_app_request(&room, "so.tilo.notes", Some(asked))).await;
            assert!(refused(&beyond), "the agent may not open Notes: {beyond:?}");
            let allowed =
                app_asks(&app, open_app_request(&room, "so.tilo.chat", Some(asked))).await;
            assert!(!refused(&allowed), "{allowed:?}");
            assert_eq!(launcher.opened(), ["so.tilo.chat"]);

            let foreign = app_asks(
                &app,
                open_app_request(&room, "so.tilo.chat", Some(id(0xa0d3))),
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
async fn an_app_launches_an_installed_app_only_for_a_press_or_an_invocation_it_runs() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let (started, started_rx) = std::sync::mpsc::channel();
            let (adopt, receiver) = tokio::sync::oneshot::channel();
            let launcher = Arc::new(ObservedLauncher {
                started,
                receiver: Mutex::new(Some(receiver)),
                requests: Mutex::new(Vec::new()),
                adoption: Mutex::new(None),
                original: Mutex::new(None),
                pressed: false.into(),
            });
            let rig = Rig::with_services(
                &env,
                HostServices {
                    launcher: Some(launcher.clone()),
                    ..env.membership.services()
                },
            )
            .await;
            let app = rig.worker.handle.clone();
            let unasked = app
                .launch_installed_application("so.tilo.word-processor", None, None)
                .await;
            assert!(unasked.is_err(), "{unasked:?}");
            assert!(launcher.requests.lock().unwrap().is_empty());
            assert!(started_rx.try_recv().is_err());

            launcher
                .pressed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let pending = tokio::task::spawn_local(async move {
                app.launch_installed_application("so.tilo.word-processor", None, None)
                    .await
            });
            certified_launch_wait(&started_rx).await;
            adopt.send(Ok(())).unwrap();
            let (_, json) = pending.await.unwrap().unwrap();
            assert!(matches!(
                serde_json::from_str::<tilo_actions::InvocationUpdate>(&json).unwrap(),
                tilo_actions::InvocationUpdate::Succeeded { .. }
            ));
            rig.stop().await;
        })
        .await;
}
