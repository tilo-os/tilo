// A person's unsent editor reaches the Chat app only through the private
// command lane, for the invocation that adopts it, and never enters the
// room, its history or the invocation journal.
fn assert_private_editor_absent(bytes: &[u8], editor: &runtime_collab::private_draft::Editor) {
    for secret in [&editor.text, &editor.preedit] {
        assert!(!secret.is_empty(), "privacy canaries must not be empty");
        assert!(
            !bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "private editor content entered a public or durable artifact"
        );
    }
}

fn assert_private_editor_absent_from_store(
    path: &std::path::Path,
    editor: &runtime_collab::private_draft::Editor,
) {
    for entry in std::fs::read_dir(path).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            assert_private_editor_absent_from_store(&entry.path(), editor);
        } else {
            assert_private_editor_absent(&std::fs::read(entry.path()).unwrap(), editor);
        }
    }
}

/// The fixture directory at `revision`, its one action named `action`.
struct NamedActionDirectory {
    revision: &'static str,
    action: &'static str,
}
impl wire::DirectorySource for NamedActionDirectory {
    fn query(
        &self,
        _query: runtime_collab::ActionDirectoryQuery,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryPage> {
        Box::pin(std::future::ready(Err(
            "this directory only resolves".into()
        )))
    }

    fn resolve(
        &self,
        _: (&str, &str, &str),
        _local_document: bool,
        _max_bytes: u32,
    ) -> wire::Asked<runtime_collab::ActionDirectoryResolution> {
        let mut row = directory_state(self.revision).borrow().row();
        row.action = Some(self.action.into());
        row.descriptor.as_mut().unwrap().action = self.action.into();
        row.visibility.as_mut().unwrap().action = self.action.into();
        Box::pin(std::future::ready(Ok(
            runtime_collab::ActionDirectoryResolution::Found {
                row: Box::new(row),
                revision: None,
            },
        )))
    }
}

/// `directory`'s action on the rig's application, at its revision, with
/// `argument`: the request a caller sends.
async fn named_action_request(
    rig: &Rig,
    directory: NamedActionDirectory,
    argument: serde_json::Value,
) -> tilo_actions::remote::InvokeRequest {
    let mut request = local_host_request(true);
    request.revision = Some(directory.revision.into());
    request.target.address = tilo_actions::ActionAddress::new("workspace:atlas", directory.action);
    assert!(argument.is_object(), "an argument object");
    request.argument = tilo_actions::remote::InvokeArgument::Json(
        argument.as_object().cloned().unwrap_or_default(),
    );
    rig.worker
        .handle
        .publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(directory)))
        .await
        .unwrap();
    request
}

/// The person `caller` invokes `directory`'s action: the invocation the
/// rig's application is delivered.
async fn invoke_as_person(
    rig: &Rig,
    caller: &tilo_actions::remote::Caller,
    directory: NamedActionDirectory,
    argument: serde_json::Value,
) -> (tilo_actions::InvocationId, runtime_collab::Invocation) {
    let request = named_action_request(rig, directory, argument).await;
    let receipt = local_host_answer(rig.state.action_invoke_local_person(caller, &request))
        .await
        .unwrap();
    assert_eq!(receipt.invocation, request.invocation);
    let delivered = tilo_test_env::within(
        "the person's invocation is delivered",
        rig.stream.as_ref().unwrap().read(),
    )
    .await
    .unwrap();
    assert_eq!(delivered.invocation_uuid, *request.invocation.0.as_bytes());
    (request.invocation, delivered)
}

/// The person on the device whose Chat worker is open, and their
/// presentation.
fn chat_person(env: &Environment) -> (tilo_actions::remote::Caller, Vec<u8>) {
    (
        tilo_actions::remote::Caller::authenticated(
            env.cert.user_public_key,
            env.cert.device_public_key,
        )
        .in_room(&env.route.cluster_id)
        .in_new_execution(),
        presentation(&env.route.cluster_id, &env.cert, &env.key),
    )
}

fn private_draft_fixture(env: &Environment) -> runtime_collab::private_draft::Draft {
    runtime_collab::private_draft::Draft {
        workspace: env.route.cluster_id.clone(),
        target: runtime_collab::private_draft::Target::Person {
            public_key: "b".repeat(64),
        },
        editor: runtime_collab::private_draft::Editor {
            text: "private-body-canary-73d94-é".into(),
            anchor: 7,
            caret: 3,
            preedit: "private-preedit-canary-候補".into(),
            preedit_cursor: Some((0, 7)),
        },
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_private_draft_reaches_chat_only_for_its_adoption_and_stays_out_of_durable_state() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let rig = Rig::with_services(
                &env,
                HostServices {
                    local_person: Some(Arc::new(super::signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    ..env.membership.services()
                },
            )
            .await;
            let (caller, proof) = chat_person(&env);
            let draft = private_draft_fixture(&env);
            let token = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .unwrap();
            let (invocation, delivered) = invoke_as_person(
                &rig,
                &super::signed_worker::native_person(&env.route, &env.cert),
                NamedActionDirectory {
                    revision: "draft-r1",
                    action: "adopt-private-draft",
                },
                serde_json::json!({"token": token}),
            )
            .await;
            assert_eq!(delivered.action, "adopt-private-draft");
            let argument = delivered.argument.as_deref().unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(
                    argument
                        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                        .unwrap_or(argument)
                )
                .unwrap(),
                serde_json::json!({"token": token}),
                "the delivered action carries only the opaque token"
            );
            assert_private_editor_absent(delivered.context_json.as_bytes(), &draft.editor);
            let handle = &rig.worker.handle;
            let fetch = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: false,
            };
            assert!(handle.private_draft(&fetch, &[]).await.is_err());
            assert!(
                handle
                    .private_draft(&fetch, uuid::Uuid::new_v4().as_bytes())
                    .await
                    .is_err(),
                "only the adopting invocation reaches the draft"
            );
            let other = rig.caller(&env, 904, None).await;
            assert!(
                other
                    .handle
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .is_err(),
                "another connection never reaches the draft"
            );
            let fetched: runtime_collab::private_draft::Draft = serde_json::from_slice(
                &handle
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .unwrap(),
            )
            .unwrap();
            assert!(fetched.editor == draft.editor);
            assert!(fetched.target == draft.target);
            assert!(
                rig.state
                    .private_drafts
                    .lock()
                    .unwrap()
                    .contains_key(&token),
                "fetching is not consuming"
            );
            // Only an invocation still the person's own reaches the draft: a
            // run that took it over does not.
            let old_owner = {
                let mut pending = rig.state.pending_invocations.lock().unwrap();
                std::mem::replace(
                    &mut pending.get_mut(&delivered.id).unwrap().owner,
                    InvocationOwner::NativeTask {
                        task: "task:private".into(),
                        agent: "scribe".into(),
                        epoch: 1,
                    },
                )
            };
            assert!(
                handle
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .is_err(),
                "an invocation a task owns does not reach the person's draft"
            );
            rig.state
                .pending_invocations
                .lock()
                .unwrap()
                .get_mut(&delivered.id)
                .unwrap()
                .owner = old_owner;
            assert!(
                handle
                    .private_draft(
                        &runtime_collab::private_draft::Request {
                            private_draft: uuid::Uuid::new_v4(),
                            acknowledge: true,
                        },
                        invocation.0.as_bytes(),
                    )
                    .await
                    .is_err()
            );
            handle
                .private_draft(
                    &runtime_collab::private_draft::Request {
                        private_draft: token,
                        acknowledge: true,
                    },
                    invocation.0.as_bytes(),
                )
                .await
                .unwrap();
            let update = tilo_actions::InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            };
            handle
                .update_invocation(delivered.id, &serde_json::to_string(&update).unwrap())
                .await
                .unwrap();
            assert!(
                !rig.state
                    .private_drafts
                    .lock()
                    .unwrap()
                    .contains_key(&token)
            );
            assert!(
                handle
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .is_err()
            );
            // A staged draft expires on its own, and staging needs the
            // person's device to prove itself now.
            let expired = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .unwrap();
            rig.state
                .private_drafts
                .lock()
                .unwrap()
                .get_mut(&expired)
                .unwrap()
                .expires = std::time::Instant::now();
            rig.state.expire_stale_invocations();
            assert!(
                !rig.state
                    .private_drafts
                    .lock()
                    .unwrap()
                    .contains_key(&expired),
                "a draft past its bound is gone"
            );
            *env.authority.certificate.lock().unwrap() = None;
            assert!(
                rig.state
                    .stage_private_draft(&caller, &proof, draft.clone())
                    .is_err(),
                "staging without the device's certificate is refused"
            );
            assert_private_editor_absent_from_store(env.root.path(), &draft.editor);
            rig.stop().await;
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn private_drafts_are_bounded_per_person() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let rig = Rig::with_services(
                &env,
                HostServices {
                    local_person: Some(Arc::new(super::signed_worker::NativePerson(
                        env.authority.clone(),
                    ))),
                    ..env.membership.services()
                },
            )
            .await;
            let (caller, proof) = chat_person(&env);
            let draft = private_draft_fixture(&env);
            for _ in 0..16 {
                rig.state
                    .stage_private_draft(&caller, &proof, draft.clone())
                    .unwrap();
            }
            assert!(
                rig.state
                    .stage_private_draft(&caller, &proof, draft)
                    .is_err()
            );
            rig.stop().await;
        })
        .await;
}

/// One Chat window holding two conversations of one workspace: the draft
/// belongs to the conversation its adopt action is delivered to, so that
/// record's channel fetches it and its sibling's does not.
#[tokio::test(flavor = "current_thread")]
async fn private_draft_belongs_to_the_record_its_adoption_targets() {
    tokio::task::LocalSet::new()
        .run_until(async {
            const CHAT_WINDOW: u64 = 906;
            let mut env = Environment::new();
            env.route.app_id = "so.tilo.chat".into();
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let mut services = env.membership.services();
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                room: env.route.cluster_id.clone(),
            }));
            services.local_person = Some(Arc::new(super::signed_worker::NativePerson(
                env.authority.clone(),
            )));
            let rig = Rig::with_services(&env, services).await;
            let (window, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                CHAT_WINDOW,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let proof = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let record =
                |id: &str| CrdtRoute::record("so.tilo.chat", env.route.cluster_id.clone(), id);
            let first = window
                .join_channel(&record("conversation-a").channel_key(), &proof)
                .await
                .unwrap();
            let second = window
                .join_channel(&record("conversation-b").channel_key(), &proof)
                .await
                .unwrap();
            let (caller, _) = chat_person(&env);
            let draft = private_draft_fixture(&env);
            let token = rig
                .state
                .stage_private_draft(&caller, &proof, draft.clone())
                .expect("a person with Chat open across several records may stage a draft");
            second
                .publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(
                    NamedActionDirectory {
                        revision: "draft-r1",
                        action: "adopt-private-draft",
                    },
                )))
                .await
                .unwrap();
            let stream = second.subscribe_stable_invocations().await.unwrap();
            // The window holds each conversation as its own route hold.
            let second_hold = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .holder_of(&record("conversation-b"))
                .unwrap();
            assert_eq!(rig.state.connection_of(second_hold), CHAT_WINDOW);
            let mut request = local_host_request(true);
            request.revision = Some("draft-r1".into());
            request.target.provider = second_hold.to_string();
            request.target.address =
                tilo_actions::ActionAddress::new("workspace:atlas", "adopt-private-draft");
            request.argument = tilo_actions::remote::InvokeArgument::Json(
                serde_json::json!({"token": token})
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            );
            let invocation = request.invocation;
            let receipt =
                local_host_answer(rig.state.action_invoke_local_person(&caller, &request))
                    .await
                    .unwrap();
            assert_eq!(receipt.invocation, invocation);
            let delivered = tilo_test_env::within("the conversation's own delivery", stream.read())
                .await
                .unwrap();
            assert_eq!(delivered.invocation_uuid, *invocation.0.as_bytes());
            assert_eq!(delivered.action, "adopt-private-draft");
            let fetch = runtime_collab::private_draft::Request {
                private_draft: token,
                acknowledge: false,
            };
            assert!(
                first
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .is_err(),
                "a sibling conversation's channel does not hold this draft"
            );
            let fetched: runtime_collab::private_draft::Draft = serde_json::from_slice(
                &second
                    .private_draft(&fetch, invocation.0.as_bytes())
                    .await
                    .expect("the conversation the adoption targets fetches its draft"),
            )
            .unwrap();
            assert!(fetched.editor == draft.editor);
            drop((first, second, window));
            rig.stop().await;
        })
        .await;
}
